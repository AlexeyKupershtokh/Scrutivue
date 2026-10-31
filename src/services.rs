use crate::{
    decode::{self, Frame},
    gpu::Gpu,
    model::{self, Mode, PhotoId, Settings},
    processing::Cpu,
};
use crossbeam_channel::{bounded, Receiver, Sender};
use gpui_kit::RenderImage;
use lru::LruCache;
use std::{
    collections::{hash_map::DefaultHasher, VecDeque},
    hash::{Hash, Hasher},
    num::NonZeroUsize,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Condvar, Mutex,
    },
    thread,
    time::{Duration, Instant, SystemTime},
};

pub enum Event {
    Backend(String),
    Files {
        epoch: u64,
        paths: Vec<PathBuf>,
    },
    ScanDone {
        epoch: u64,
        skipped: usize,
    },
    PathsChosen {
        epoch: u64,
        paths: Vec<PathBuf>,
    },
    Thumbnail {
        id: PhotoId,
        result: Result<Arc<RenderImage>, String>,
    },
    Decoded {
        id: PhotoId,
        result: Result<Arc<Frame>, String>,
    },
    Rendered {
        generation: u64,
        primary: Arc<RenderImage>,
        alternate: Option<Arc<RenderImage>>,
        elapsed: Duration,
        backend: String,
    },
    Error(String),
}

pub struct Mailbox<T> {
    value: Mutex<Option<T>>,
    ready: Condvar,
}
impl<T> Mailbox<T> {
    pub fn new() -> Self {
        Self {
            value: Mutex::new(None),
            ready: Condvar::new(),
        }
    }
    pub fn put(&self, value: T) {
        *self.value.lock().unwrap() = Some(value);
        self.ready.notify_one();
    }
    pub fn take(&self) -> T {
        let mut value = self.value.lock().unwrap();
        while value.is_none() {
            value = self.ready.wait(value).unwrap();
        }
        value.take().unwrap()
    }
}
pub enum RenderJob {
    Frame(RenderRequest),
    Clear,
}
pub struct RenderRequest {
    pub generation: u64,
    pub a: (PhotoId, Arc<Frame>),
    pub b: (PhotoId, Arc<Frame>),
    pub settings: Settings,
}
pub struct Services {
    pub events: Receiver<Event>,
    sender: Sender<Event>,
    pub renders: Arc<Mailbox<RenderJob>>,
    decode_jobs: Arc<(Mutex<VecDeque<(u64, PhotoId, PathBuf)>>, Condvar)>,
    decode_cache: Arc<Mutex<LruCache<PathBuf, Arc<Frame>>>>,
    thumbnails: Sender<(u64, PhotoId, PathBuf)>,
    epoch: Arc<AtomicU64>,
}

pub fn render_image(width: u32, height: u32, pixels: Vec<u8>) -> Arc<RenderImage> {
    let buffer = image::RgbaImage::from_raw(width, height, pixels).expect("render dimensions");
    Arc::new(RenderImage::new(vec![image::Frame::new(buffer)]))
}

fn render_plan(settings: &Settings) -> (Settings, bool) {
    let pair = settings.mode.prepared_pair();
    let mut settings = settings.clone();
    if pair {
        settings.mode = Mode::Single;
        settings.single_b = false;
        settings.split = 0.5;
    }
    (settings, pair)
}

impl Services {
    pub fn new(force_cpu: bool, cache_dir: PathBuf) -> Self {
        let (sender, events) = bounded(64);
        let renders = Arc::new(Mailbox::<RenderJob>::new());
        let epoch = Arc::new(AtomicU64::new(0));
        let mailbox = renders.clone();
        let tx = sender.clone();
        thread::Builder::new()
            .name("gpu-render".into())
            .spawn(move || {
                let mut gpu = if force_cpu {
                    None
                } else {
                    match Gpu::new() {
                        Ok(gpu) => Some(gpu),
                        Err(e) => {
                            let _ = tx
                                .send(Event::Error(format!("GPU unavailable: {e}. CPU fallback.")));
                            None
                        }
                    }
                };
                let mut cpu = Cpu::new();
                let _ = tx.send(Event::Backend(
                    gpu.as_ref()
                        .map(|g| g.name.clone())
                        .unwrap_or_else(|| "CPU · Rayon".into()),
                ));
                loop {
                    let job = match mailbox.take() {
                        RenderJob::Clear => {
                            if let Some(gpu) = &mut gpu {
                                gpu.clear();
                            }
                            cpu = Cpu::new();
                            continue;
                        }
                        RenderJob::Frame(job) => job,
                    };
                    let start = Instant::now();
                    let a = (job.a.0, &*job.a.1);
                    let b = (job.b.0, &*job.b.1);
                    let mut backend = "CPU · Rayon".to_owned();
                    let (render_settings, pair) = render_plan(&job.settings);
                    let mut run = |show_b: bool| {
                        if let Some(device) = &mut gpu {
                            match device.render(a, b, &render_settings, show_b) {
                                Ok(pixels) => {
                                    backend = device.name.clone();
                                    return pixels;
                                }
                                Err(e) => {
                                    backend = format!("CPU fallback · {e}");
                                    let _ = tx.send(Event::Error(backend.clone()));
                                    gpu = None;
                                }
                            }
                        }
                        cpu.render(a, b, &render_settings, show_b)
                    };
                    let pixels = run(false);
                    // GPUI clips/switches these immutable views without asking compute.
                    let alternate = if pair && a.0 != b.0 {
                        Some(run(true))
                    } else {
                        None
                    };
                    let [w, h] = job.settings.viewport;
                    let primary = render_image(w, h, pixels);
                    let alternate = if pair {
                        Some(
                            alternate
                                .map(|p| render_image(w, h, p))
                                .unwrap_or_else(|| primary.clone()),
                        )
                    } else {
                        None
                    };
                    if tx
                        .send(Event::Rendered {
                            generation: job.generation,
                            primary,
                            alternate,
                            elapsed: start.elapsed(),
                            backend,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .expect("render thread");

        let decode_jobs = Arc::new((
            Mutex::new(VecDeque::<(u64, PhotoId, PathBuf)>::new()),
            Condvar::new(),
        ));
        let cache = Arc::new(Mutex::new(LruCache::<PathBuf, Arc<Frame>>::new(
            NonZeroUsize::new(6).unwrap(),
        )));
        for index in 0..2 {
            let jobs = decode_jobs.clone();
            let tx = sender.clone();
            let cache = cache.clone();
            let epoch = epoch.clone();
            thread::Builder::new()
                .name(format!("decode-{index}"))
                .spawn(move || loop {
                    let (job_epoch, id, path) = {
                        let mut queue = jobs.0.lock().unwrap();
                        while queue.is_empty() {
                            queue = jobs.1.wait(queue).unwrap();
                        }
                        queue.pop_front().unwrap()
                    };
                    if epoch.load(Ordering::Acquire) != job_epoch {
                        continue;
                    }
                    let cached = cache.lock().unwrap().get(&path).cloned();
                    let result = cached.map(Ok).unwrap_or_else(|| {
                        decode::decode(&path).map_err(|e| format!("{}: {e:#}", path.display()))
                    });
                    if epoch.load(Ordering::Acquire) != job_epoch {
                        continue;
                    }
                    if let Ok(frame) = &result {
                        let mut cache = cache.lock().unwrap();
                        if epoch.load(Ordering::Acquire) != job_epoch {
                            continue;
                        }
                        cache.put(path, frame.clone());
                        while cache.len() > 2
                            && cache.iter().map(|(_, f)| f.bytes()).sum::<usize>()
                                > 1024 * 1024 * 1024
                        {
                            cache.pop_lru();
                        }
                    }
                    if tx.send(Event::Decoded { id, result }).is_err() {
                        break;
                    }
                })
                .expect("decode thread");
        }
        let (thumbnails, jobs) = bounded::<(u64, PhotoId, PathBuf)>(4);
        let disk = Arc::new(ThumbnailCache::new(cache_dir));
        let prune = disk.clone();
        thread::spawn(move || prune.prune());
        for index in 0..2 {
            let jobs = jobs.clone();
            let tx = sender.clone();
            let disk = disk.clone();
            let epoch = epoch.clone();
            thread::Builder::new()
                .name(format!("thumbnail-{index}"))
                .spawn(move || {
                    let mut written = 0;
                    while let Ok((job_epoch, id, path)) = jobs.recv() {
                        if epoch.load(Ordering::Acquire) != job_epoch {
                            continue;
                        }
                        let result = disk
                            .get(&path)
                            .map(|image| {
                                let (width, height) = image.dimensions();
                                let mut pixels = image.into_raw();
                                for pixel in pixels.chunks_exact_mut(4) {
                                    pixel.swap(0, 2);
                                }
                                render_image(width, height, pixels)
                            })
                            .map_err(|e| format!("{e:#}"));
                        if epoch.load(Ordering::Acquire) != job_epoch {
                            continue;
                        }
                        if tx.send(Event::Thumbnail { id, result }).is_err() {
                            break;
                        }
                        written += 1;
                        if written % 256 == 0 {
                            disk.prune();
                        }
                    }
                })
                .expect("thumbnail thread");
        }
        Self {
            events,
            sender,
            renders,
            decode_jobs,
            decode_cache: cache,
            thumbnails,
            epoch,
        }
    }
    pub fn decode(&self, jobs: Vec<(PhotoId, PathBuf)>) {
        *self.decode_jobs.0.lock().unwrap() = jobs
            .into_iter()
            .map(|(id, path)| (self.epoch(), id, path))
            .collect();
        self.decode_jobs.1.notify_all();
    }
    pub fn thumbnail(&self, id: PhotoId, path: PathBuf) -> bool {
        self.thumbnails.try_send((self.epoch(), id, path)).is_ok()
    }
    pub fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::Acquire)
    }
    pub fn clear(&self) {
        self.epoch.fetch_add(1, Ordering::AcqRel);
        self.decode_jobs.0.lock().unwrap().clear();
        self.decode_cache.lock().unwrap().clear();
        self.renders.put(RenderJob::Clear);
    }
    pub fn choose(&self, directory: bool) {
        let tx = self.sender.clone();
        let epoch = self.epoch();
        thread::spawn(move || {
            let paths = if directory {
                rfd::FileDialog::new()
                    .set_title("Добавить папку")
                    .pick_folder()
                    .into_iter()
                    .collect()
            } else {
                rfd::FileDialog::new()
                    .set_title("Добавить изображения")
                    .add_filter(
                        "Images and RAW",
                        &[
                            "jpg", "jpeg", "png", "tif", "tiff", "webp", "bmp", "exr", "nef",
                            "nrw", "arw", "cr2", "cr3", "dng", "raf", "rw2", "orf", "pef", "3fr",
                            "fff", "iiq",
                        ],
                    )
                    .pick_files()
                    .unwrap_or_default()
            };
            let _ = tx.send(Event::PathsChosen { epoch, paths });
        });
    }
    pub fn scan(&self, paths: Vec<PathBuf>, recursive: bool) {
        let tx = self.sender.clone();
        let current = self.epoch.clone();
        let epoch = self.epoch();
        thread::spawn(move || {
            let mut batch = Vec::with_capacity(256);
            let mut skipped = 0;
            for path in paths {
                if current.load(Ordering::Acquire) != epoch {
                    return;
                }
                let path = path.canonicalize().unwrap_or(path);
                if path.is_dir() {
                    let walk = directory_walk(path, recursive);
                    for entry in walk {
                        if current.load(Ordering::Acquire) != epoch {
                            return;
                        }
                        match entry {
                            Ok(entry)
                                if entry.file_type().is_file()
                                    && model::supported(entry.path()) =>
                            {
                                batch.push(entry.into_path())
                            }
                            Err(_) => skipped += 1,
                            _ => {}
                        }
                        if batch.len() == 256
                            && tx
                                .send(Event::Files {
                                    epoch,
                                    paths: std::mem::take(&mut batch),
                                })
                                .is_err()
                        {
                            return;
                        }
                    }
                } else if model::supported(&path) && path.is_file() {
                    batch.push(path);
                } else {
                    skipped += 1;
                }
            }
            if !batch.is_empty() {
                let _ = tx.send(Event::Files {
                    epoch,
                    paths: batch,
                });
            }
            let _ = tx.send(Event::ScanDone { epoch, skipped });
        });
    }
}

fn directory_walk(path: PathBuf, recursive: bool) -> walkdir::WalkDir {
    walkdir::WalkDir::new(path)
        .follow_links(false)
        .max_depth(if recursive { usize::MAX } else { 1 })
        .sort_by_file_name()
}

struct ThumbnailCache {
    root: PathBuf,
    owned: bool,
}
impl ThumbnailCache {
    fn new(parent: PathBuf) -> Self {
        use std::io::Write;
        let root = parent.join("lumen-diff-v1");
        let marker = root.join(".cache-owner");
        let signature = b"Lumen Diff thumbnail cache v1\n";
        let _ = std::fs::create_dir_all(&root);
        if std::fs::read_dir(&root).is_ok_and(|mut entries| entries.next().is_none()) {
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&marker)
            {
                let _ = file.write_all(signature);
            }
        }
        let owned = std::fs::read(marker).is_ok_and(|bytes| bytes == signature);
        Self { root, owned }
    }
    fn key(&self, path: &Path) -> anyhow::Result<PathBuf> {
        let metadata = path.metadata()?;
        let mut hash = DefaultHasher::new();
        "thumbnail-v1".hash(&mut hash);
        path.hash(&mut hash);
        metadata.len().hash(&mut hash);
        metadata
            .modified()?
            .duration_since(SystemTime::UNIX_EPOCH)?
            .as_nanos()
            .hash(&mut hash);
        Ok(self.root.join(format!("{:016x}.png", hash.finish())))
    }
    fn get(&self, path: &Path) -> anyhow::Result<image::RgbaImage> {
        if !self.owned {
            return decode::thumbnail(path);
        }
        let key = self.key(path)?;
        if let Ok(image) = image::open(&key) {
            return Ok(image.into_rgba8());
        }
        let image = decode::thumbnail(path)?;
        // Unique temporary names let simultaneous opens share the cache safely.
        let temporary = key.with_extension(format!("{:?}.tmp", thread::current().id()));
        if image
            .save_with_format(&temporary, image::ImageFormat::Png)
            .is_ok()
        {
            if std::fs::rename(&temporary, &key).is_err() {
                let _ = std::fs::remove_file(&temporary);
            }
        }
        Ok(image)
    }
    fn prune(&self) {
        if !self.owned {
            return;
        }
        let Ok(entries) = std::fs::read_dir(&self.root) else {
            return;
        };
        let mut files: Vec<_> = entries
            .filter_map(Result::ok)
            .filter(|e| cache_filename(&e.file_name().to_string_lossy()))
            .filter_map(|e| {
                e.metadata().ok().map(|m| {
                    (
                        e.path(),
                        m.len(),
                        m.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                    )
                })
            })
            .collect();
        let mut total: u64 = files.iter().map(|x| x.1).sum();
        if total < 512 * 1024 * 1024 {
            return;
        }
        files.sort_by_key(|x| x.2);
        for (path, bytes, _) in files {
            if total <= 448 * 1024 * 1024 {
                break;
            }
            if std::fs::remove_file(path).is_ok() {
                total = total.saturating_sub(bytes);
            }
        }
    }
}

fn cache_filename(name: &str) -> bool {
    name.strip_suffix(".png")
        .is_some_and(|stem| stem.len() == 16 && stem.bytes().all(|c| c.is_ascii_hexdigit()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recursive_addition_finds_nested_images_without_reading_pixels() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(format!("target/scan-tests/{}", std::process::id()));
        std::fs::create_dir_all(root.join("one/two")).unwrap();
        for name in [
            "root.png",
            "one/child.NEF",
            "one/two/deep.jpg",
            "one/two/ignored.txt",
        ] {
            std::fs::write(root.join(name), b"not decoded by scanner").unwrap();
        }
        let count = |recursive| {
            directory_walk(root.clone(), recursive)
                .into_iter()
                .filter_map(Result::ok)
                .filter(|e| e.file_type().is_file() && model::supported(e.path()))
                .count()
        };
        assert_eq!(count(false), 1);
        assert_eq!(count(true), 3);
    }
    #[test]
    fn slider_and_click_prepare_whole_views_independent_of_divider() {
        for mode in [Mode::Single, Mode::Slider, Mode::Click] {
            for split in [0.0, 0.3, 1.0] {
                let (plan, pair) = render_plan(&Settings {
                    mode,
                    split,
                    ..Settings::default()
                });
                assert!(pair);
                assert_eq!(plan.mode, Mode::Single);
                assert_eq!(plan.split, 0.5);
            }
        }
        assert!(
            !render_plan(&Settings {
                mode: Mode::Diff,
                ..Settings::default()
            })
            .1
        );
    }
    #[test]
    fn render_mailbox_coalesces_interactions() {
        let mailbox = Mailbox::new();
        for i in 0..1000 {
            mailbox.put(i);
        }
        assert_eq!(mailbox.take(), 999);
    }
    #[test]
    fn cache_cleanup_only_accepts_our_hashed_names() {
        assert!(cache_filename("0123456789abcdef.png"));
        assert!(!cache_filename("photograph.png"));
        assert!(!cache_filename("0123456789abcdef.tmp"));
    }
    #[test]
    fn nonempty_unmarked_cache_directory_is_never_owned() {
        let parent = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(format!("target/cache-tests/{}", std::process::id()));
        let root = parent.join("lumen-diff-v1");
        std::fs::create_dir_all(&root).unwrap();
        let photo = root.join("user-photo.png");
        std::fs::write(&photo, b"untouched").unwrap();
        let cache = ThumbnailCache::new(parent);
        assert!(!cache.owned);
        cache.prune();
        assert_eq!(std::fs::read(photo).unwrap(), b"untouched");
    }
}
