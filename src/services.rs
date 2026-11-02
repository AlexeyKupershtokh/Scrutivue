use crate::{
    decode::{self, Frame},
    gpu::Gpu,
    model::{self, Mode, Photo, PhotoId, PhotoMetadata, Settings},
    processing::Cpu,
};
use crossbeam_channel::{bounded, unbounded, Receiver, Sender};
use gpui_kit::RenderImage;
use lru::LruCache;
use std::{
    collections::{hash_map::DefaultHasher, HashMap, HashSet, VecDeque},
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Condvar, Mutex,
    },
    thread,
    time::{Duration, Instant, SystemTime},
};

pub enum Event {
    Backend(String),
    Files {
        epoch: u64,
        scan: u64,
        files: Vec<ScannedPhoto>,
    },
    Roots {
        epoch: u64,
        roots: Vec<PathBuf>,
    },
    ScanDone {
        epoch: u64,
        scan: u64,
        skipped: usize,
    },
    Refreshed {
        epoch: u64,
        files: Vec<ScannedPhoto>,
        complete_roots: HashSet<PathBuf>,
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
        settings: Settings,
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
    retire: Sender<T>,
}
impl<T: Send + 'static> Mailbox<T> {
    pub fn new() -> Self {
        let (retire, obsolete) = unbounded();
        thread::Builder::new()
            .name("obsolete-jobs".into())
            .spawn(move || {
                // A busy compute worker must not retain an ever-growing list of
                // superseded source frames. Nor may the UI producer free them.
                for job in obsolete {
                    drop(job);
                }
            })
            .expect("obsolete job release thread");
        Self {
            value: Mutex::new(None),
            ready: Condvar::new(),
            retire,
        }
    }
    pub fn put(&self, value: T) {
        let mut state = self.value.lock().unwrap();
        let old = state.replace(value);
        drop(state);
        if let Some(old) = old {
            let _ = self.retire.send(old);
        }
        self.ready.notify_one();
    }
    pub fn take(&self) -> T {
        let mut value = self.value.lock().unwrap();
        while value.is_none() {
            value = self.ready.wait(value).unwrap();
        }
        value.take().unwrap()
    }
    #[cfg(feature = "experimental-nef-progressive")]
    pub fn try_take(&self) -> Option<T> {
        self.value.lock().unwrap().take()
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
    #[cfg(feature = "experimental-nef-progressive")]
    pub nef_preview: crate::nef_preview::Worker,
    pub events: Receiver<Event>,
    sender: Sender<Event>,
    pub renders: Arc<Mailbox<RenderJob>>,
    decode_jobs: DecodeJobs,
    decode_cache: Arc<DecodeCache>,
    thumbnails: Sender<(u64, PhotoId, PathBuf)>,
    scans: Sender<ScanJob>,
    next_scan: AtomicU64,
    retire: Sender<Garbage>,
    epoch: Arc<AtomicU64>,
}

#[derive(Default)]
struct DecodeQueue {
    pending: VecDeque<PathBuf>,
    wanted: HashMap<PathBuf, DecodeTarget>,
    active: HashMap<PathBuf, Arc<AtomicBool>>,
}
#[derive(Clone)]
struct DecodeTarget {
    epoch: u64,
    ids: Vec<PhotoId>,
    selected: bool,
}
impl DecodeQueue {
    // Scanner paths are canonical. Replacing the desired set coalesces repeated
    // requests and promotes an active prefetch instead of decoding it again.
    fn replace(&mut self, epoch: u64, jobs: Vec<(PhotoId, PathBuf)>, selected: usize) {
        self.pending.clear();
        self.wanted.clear();
        for (index, (id, path)) in jobs.into_iter().enumerate() {
            let target = self.wanted.entry(path.clone()).or_insert_with(|| {
                self.pending.push_back(path);
                DecodeTarget {
                    epoch,
                    ids: Vec::new(),
                    selected: index < selected,
                }
            });
            target.selected |= index < selected;
            if !target.ids.contains(&id) {
                target.ids.push(id);
            }
        }
        let urgent = self.wanted.values().any(|target| target.selected);
        for (path, cancel) in &self.active {
            let keep = self
                .wanted
                .get(path)
                .is_some_and(|target| target.epoch == epoch && (target.selected || !urgent));
            if !keep {
                cancel.store(true, Ordering::Release);
            }
        }
    }
    fn take(&mut self) -> Option<(PathBuf, Arc<AtomicBool>)> {
        let urgent = self.wanted.values().any(|target| target.selected);
        // No speculative work competes with a still-loading selected pair.
        // At most one speculative decode may run when the pair is ready.
        let index = self.pending.iter().position(|path| {
            !self.active.contains_key(path)
                && self
                    .wanted
                    .get(path)
                    .is_some_and(|target| target.selected || (!urgent && self.active.is_empty()))
        })?;
        let path = self.pending.remove(index).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        self.active.insert(path.clone(), cancel.clone());
        Some((path, cancel))
    }
    fn complete(&mut self, path: &Path, cancel: &Arc<AtomicBool>) -> Option<DecodeTarget> {
        let active = self.active.get(path)?;
        if !Arc::ptr_eq(active, cancel) {
            return None;
        }
        self.active.remove(path);
        if cancel.load(Ordering::Acquire) {
            return None;
        }
        self.pending.retain(|pending| pending != path);
        self.wanted.remove(path)
    }
    fn clear(&mut self) {
        self.pending.clear();
        self.wanted.clear();
        for cancel in self.active.values() {
            cancel.store(true, Ordering::Release);
        }
    }
}
type DecodeJobs = Arc<(Mutex<DecodeQueue>, Condvar)>;

/// Library IDs are display/GPU revision tokens, not source identities. All
/// filesystem calls for this key happen on a background worker.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct SourceKey {
    path: PathBuf,
    bytes: u64,
    modified: Option<SystemTime>,
    created: Option<SystemTime>,
}
impl SourceKey {
    fn read(path: &Path) -> anyhow::Result<Self> {
        let path = path.canonicalize()?;
        let stat = path.metadata()?;
        anyhow::ensure!(stat.is_file(), "Source is not a file");
        Ok(Self {
            path,
            bytes: stat.len(),
            modified: stat.modified().ok(),
            created: stat.created().ok(),
        })
    }
}

#[derive(Default)]
struct DecodeCounters {
    hits: AtomicU64,
    decodes: AtomicU64,
    invalidations: AtomicU64,
    cancelled: AtomicU64,
}
struct DecodeCache {
    frames: Mutex<LruCache<SourceKey, Arc<Frame>>>,
    protected: Mutex<HashSet<PathBuf>>,
    loading: Mutex<HashSet<SourceKey>>,
    ready: Condvar,
    counters: DecodeCounters,
}
impl DecodeCache {
    fn new() -> Self {
        Self {
            // Evict explicitly so speculative neighbors cannot evict A/B.
            frames: Mutex::new(LruCache::unbounded()),
            protected: Mutex::new(HashSet::new()),
            loading: Mutex::new(HashSet::new()),
            ready: Condvar::new(),
            counters: DecodeCounters::default(),
        }
    }
    fn lookup(&self, path: &Path) -> Option<Arc<Frame>> {
        let key = SourceKey::read(path).ok()?;
        key.modified?;
        let frame = self.frames.lock().unwrap().get(&key).cloned()?;
        if SourceKey::read(path).ok()? != key {
            return None;
        }
        self.counters.hits.fetch_add(1, Ordering::Relaxed);
        Some(frame)
    }
    fn remember(&self, path: &Path, expected: &PhotoMetadata, frame: Arc<Frame>) {
        let Ok(key) = SourceKey::read(path) else {
            return;
        };
        if key.modified.is_none()
            || key.bytes != expected.bytes
            || key.modified != expected.modified
            || key.created != expected.created
        {
            return;
        }
        let mut evicted = Vec::new();
        let mut cache = self.frames.lock().unwrap();
        let obsolete: Vec<_> = cache
            .iter()
            .filter(|(old, _)| old.path == key.path && **old != key)
            .map(|(old, _)| old.clone())
            .collect();
        for old in obsolete {
            if let Some(frame) = cache.pop(&old) {
                evicted.push(frame);
                self.counters.invalidations.fetch_add(1, Ordering::Relaxed);
            }
        }
        if let Some((_, old)) = cache.push(key, frame) {
            evicted.push(old);
        }
        Self::trim(
            &mut cache,
            &self.protected.lock().unwrap(),
            1024 * 1024 * 1024,
            &mut evicted,
        );
        drop(cache);
        drop(evicted);
    }
    fn load(
        &self,
        path: &Path,
        cancelled: &(dyn Fn() -> bool + Sync),
        decode: impl FnOnce(&Path) -> anyhow::Result<Arc<Frame>>,
    ) -> anyhow::Result<Arc<Frame>> {
        let key = SourceKey::read(path)?;
        // Single flight also protects against aliases reaching different queue
        // slots. A selected request can reuse the same completed source frame.
        let mut loading = self.loading.lock().unwrap();
        while loading.contains(&key) {
            anyhow::ensure!(!cancelled(), "Image decode cancelled");
            loading = self
                .ready
                .wait_timeout(loading, Duration::from_millis(20))
                .unwrap()
                .0;
        }
        loading.insert(key.clone());
        drop(loading);
        struct Flight<'a>(&'a DecodeCache, SourceKey);
        impl Drop for Flight<'_> {
            fn drop(&mut self) {
                self.0.loading.lock().unwrap().remove(&self.1);
                self.0.ready.notify_all();
            }
        }
        let _flight = Flight(self, key.clone());
        anyhow::ensure!(!cancelled(), "Image decode cancelled");
        let cached = if key.modified.is_some() {
            self.frames.lock().unwrap().get(&key).cloned()
        } else {
            None // Without a modification timestamp, do not risk a false hit.
        };
        if let Some(frame) = cached {
            anyhow::ensure!(
                SourceKey::read(path)? == key,
                "Source changed while reading the decode cache; refresh and try again"
            );
            self.counters.hits.fetch_add(1, Ordering::Relaxed);
            return Ok(frame);
        }
        self.counters.decodes.fetch_add(1, Ordering::Relaxed);
        let result = decode(&key.path);
        if cancelled() {
            self.counters.cancelled.fetch_add(1, Ordering::Relaxed);
            anyhow::bail!("Image decode cancelled");
        }
        let frame = result?;
        anyhow::ensure!(
            SourceKey::read(path)? == key,
            "Source changed while decoding; refresh and try again"
        );
        let mut evicted = Vec::new();
        if key.modified.is_some() {
            let mut cache = self.frames.lock().unwrap();
            let obsolete: Vec<_> = cache
                .iter()
                .filter(|(old, _)| old.path == key.path && **old != key)
                .map(|(old, _)| old.clone())
                .collect();
            for old in obsolete {
                if let Some(frame) = cache.pop(&old) {
                    evicted.push(frame);
                    self.counters.invalidations.fetch_add(1, Ordering::Relaxed);
                }
            }
            if let Some((_, old)) = cache.push(key, frame.clone()) {
                evicted.push(old);
            }
            Self::trim(
                &mut cache,
                &self.protected.lock().unwrap(),
                1024 * 1024 * 1024,
                &mut evicted,
            );
        }
        // Never release large pixel buffers under the cache lock or on the UI.
        drop(evicted);
        Ok(frame)
    }
    fn trim(
        cache: &mut LruCache<SourceKey, Arc<Frame>>,
        protected: &HashSet<PathBuf>,
        budget: usize,
        evicted: &mut Vec<Arc<Frame>>,
    ) {
        while cache.len() > 6
            || (cache.len() > 2
                && cache.iter().map(|(_, frame)| frame.bytes()).sum::<usize>() > budget)
        {
            let oldest = cache
                .iter()
                .rev()
                .find(|(key, _)| !protected.contains(&key.path))
                .map(|(key, _)| key.clone());
            let Some(oldest) = oldest else { break };
            if let Some(frame) = cache.pop(&oldest) {
                evicted.push(frame);
            }
        }
    }
}

enum ScanJob {
    Add {
        epoch: u64,
        scan: u64,
        paths: Vec<PathBuf>,
        recursive: bool,
    },
    Refresh {
        epoch: u64,
        roots: Vec<PathBuf>,
        recursive: bool,
    },
}
enum Garbage {
    Frames(Vec<Arc<Frame>>),
    Images(Vec<Arc<RenderImage>>),
    Event(Event),
    CacheFrame(PathBuf, PhotoMetadata, Arc<Frame>),
    LookupFrame(u64, PhotoId, PathBuf),
}

#[derive(Debug, Clone)]
pub struct ScannedPhoto {
    pub path: PathBuf,
    pub metadata: PhotoMetadata,
    pub root: Option<PathBuf>,
}

#[derive(Default)]
pub struct LibraryChange {
    pub replaced: Vec<(PhotoId, PhotoId)>,
    pub removed: Vec<PhotoId>,
}
impl LibraryChange {
    pub fn remap_selection(
        &self,
        selected: &mut [Option<PhotoId>; 2],
        focused: &mut Option<PhotoId>,
    ) {
        let had_a = selected[0].is_some();
        for (old, new) in &self.replaced {
            for id in selected.iter_mut().chain(std::iter::once(&mut *focused)) {
                if *id == Some(*old) {
                    *id = Some(*new);
                }
            }
        }
        for id in selected.iter_mut().chain(std::iter::once(focused)) {
            if id.is_some_and(|id| self.removed.contains(&id)) {
                *id = None;
            }
        }
        if had_a && selected[0].is_none() {
            selected[0] = selected[1].take();
        }
    }
}

/// Keep library order stable; a new ID is also the decode/GPU revision token.
pub fn merge_files(
    photos: &mut Vec<Photo>,
    next_id: &mut PhotoId,
    files: Vec<ScannedPhoto>,
) -> LibraryChange {
    let mut change = LibraryChange::default();
    let mut indices: HashMap<_, _> = photos
        .iter()
        .enumerate()
        .map(|(i, p)| (p.path.clone(), i))
        .collect();
    for file in files {
        let index = if let Some(&index) = indices.get(&file.path) {
            let photo = &mut photos[index];
            if !photo.metadata.same_revision(&file.metadata) {
                let old = photo.id;
                photo.id = *next_id;
                *next_id += 1;
                photo.metadata = file.metadata.clone();
                change.replaced.push((old, photo.id));
            } else if file.metadata.dimensions.is_some() {
                photo.metadata.dimensions = file.metadata.dimensions;
            }
            index
        } else {
            let id = *next_id;
            *next_id += 1;
            let name = file
                .path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let index = photos.len();
            indices.insert(file.path.clone(), index);
            photos.push(Photo {
                id,
                path: file.path,
                name,
                metadata: file.metadata,
                explicit: false,
                roots: HashSet::new(),
            });
            index
        };
        if let Some(root) = file.root {
            photos[index].roots.insert(root);
        } else {
            photos[index].explicit = true;
        }
    }
    change
}

pub fn refresh_files(
    photos: &mut Vec<Photo>,
    next_id: &mut PhotoId,
    files: Vec<ScannedPhoto>,
    complete_roots: &HashSet<PathBuf>,
) -> LibraryChange {
    for photo in photos.iter_mut() {
        photo.roots.retain(|root| !complete_roots.contains(root));
    }
    let mut change = merge_files(photos, next_id, files);
    photos.retain(|photo| {
        let keep = photo.explicit || !photo.roots.is_empty();
        if !keep {
            change.removed.push(photo.id);
        }
        keep
    });
    change
}

fn heavy_pool(name: &str, divisor: usize, cap: usize) -> Arc<rayon::ThreadPool> {
    let available = thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(2);
    let threads = (available / divisor).clamp(1, cap);
    // An ignored developer measurement can reproduce the previous two-thread
    // budget without adding a production environment-variable override.
    #[cfg(test)]
    let threads = if name == "decode-cpu" {
        std::env::var("SCRUTIVUE_PROFILE_DECODE_THREADS")
            .ok()
            .and_then(|n| n.parse::<usize>().ok())
            .filter(|&n| (1..=6).contains(&n))
            .unwrap_or(threads)
    } else {
        threads
    };
    Arc::new(
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .thread_name({
                let name = name.to_owned();
                move |i| format!("{name}-{i}")
            })
            .build()
            .expect("bounded service pool"),
    )
}

pub fn render_image(width: u32, height: u32, pixels: Vec<u8>) -> Arc<RenderImage> {
    let buffer = image::RgbaImage::from_raw(width, height, pixels).expect("render dimensions");
    Arc::new(RenderImage::new(vec![image::Frame::new(buffer)]))
}

fn render_plan(settings: &Settings) -> (Settings, bool) {
    let pair = settings.mode.prepared_pair();
    let mut settings = settings.clone();
    if pair {
        settings.mode = Mode::Click;
        settings.split = 0.5;
    }
    (settings, pair)
}

impl Services {
    pub fn new(force_cpu: bool, cache_dir: PathBuf) -> Self {
        #[cfg(feature = "experimental-nef-progressive")]
        let nef_preview = crate::nef_preview::Worker::new();
        let (sender, events) = bounded(64);
        let renders = Arc::new(Mailbox::<RenderJob>::new());
        let epoch = Arc::new(AtomicU64::new(0));
        let render_pool = heavy_pool("render-cpu", 3, 4);
        // One shared budget for BOTH full-frame workers, not six threads each.
        // Leave half the logical CPUs available for input, rendering and IO.
        let decode_pool = heavy_pool("decode-cpu", 2, 6);
        let thumbnail_pool = heavy_pool("thumbnail-cpu", usize::MAX, 1);
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
                        render_pool.install(|| cpu.render(a, b, &render_settings, show_b))
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
                            settings: job.settings.clone(),
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

        let decode_jobs = Arc::new((Mutex::new(DecodeQueue::default()), Condvar::new()));
        let cache = Arc::new(DecodeCache::new());
        let (retire, garbage) = unbounded();
        let retiring_cache = cache.clone();
        let cache_events = sender.clone();
        let cache_epoch = epoch.clone();
        thread::Builder::new()
            .name("image-release".into())
            .spawn(move || {
                while let Ok(garbage) = garbage.recv() {
                    match garbage {
                        Garbage::Frames(frames) => drop(frames),
                        Garbage::Images(images) => drop(images),
                        Garbage::Event(event) => drop(event),
                        Garbage::CacheFrame(path, metadata, frame) => {
                            retiring_cache.remember(&path, &metadata, frame)
                        }
                        Garbage::LookupFrame(epoch, id, path) => {
                            if cache_epoch.load(Ordering::Acquire) == epoch {
                                if let Some(frame) = retiring_cache.lookup(&path) {
                                    let _ = cache_events.send(Event::Decoded {
                                        id,
                                        result: Ok(frame),
                                    });
                                }
                            }
                        }
                    }
                }
            })
            .expect("image release thread");
        for index in 0..2 {
            let jobs = decode_jobs.clone();
            let tx = sender.clone();
            let cache = cache.clone();
            let epoch = epoch.clone();
            let pool = decode_pool.clone();
            #[cfg(feature = "experimental-nef-progressive")]
            let preview = nef_preview.publisher();
            thread::Builder::new()
                .name(format!("decode-{index}"))
                .spawn(move || loop {
                    let (path, cancel) = {
                        let mut queue = jobs.0.lock().unwrap();
                        loop {
                            if let Some(job) = queue.take() {
                                break job;
                            }
                            queue = jobs.1.wait(queue).unwrap();
                        }
                    };
                    let cancelled = || cancel.load(Ordering::Acquire);
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        cache.load(&path, &cancelled, |source| {
                            #[cfg(feature = "experimental-nef-progressive")]
                            if path
                                .extension()
                                .is_some_and(|extension| extension.eq_ignore_ascii_case("nef"))
                            {
                                if let Some((photo, center)) = preview.target(&path) {
                                    let identity = SourceKey::read(source)?;
                                    return pool.install(|| {
                                        decode::progressive::decode(
                                            source,
                                            &cancelled,
                                            &decode::progressive::Progress {
                                                center,
                                                emit: &|patch| {
                                                    if !cancelled()
                                                        && SourceKey::read(source).ok().as_ref()
                                                            == Some(&identity)
                                                    {
                                                        preview.patch(
                                                            photo,
                                                            path.clone(),
                                                            cancel.clone(),
                                                            patch,
                                                        );
                                                    }
                                                },
                                            },
                                        )
                                    });
                                }
                            }
                            pool.install(|| decode::decode_cancellable(source, &cancelled))
                        })
                    }))
                    .unwrap_or_else(|_| Err(anyhow::anyhow!("Image decoder failed unexpectedly")))
                    .map_err(|e| format!("{}: {e:#}", path.display()));
                    let target = jobs.0.lock().unwrap().complete(&path, &cancel);
                    #[cfg(feature = "experimental-nef-progressive")]
                    if result.is_err() {
                        // Revoke queued previews after complete() has retained
                        // the authoritative decode error for the UI.
                        cancel.store(true, Ordering::Release);
                    }
                    jobs.1.notify_all();
                    if let Some(target) = target {
                        if epoch.load(Ordering::Acquire) != target.epoch {
                            continue;
                        }
                        for id in target.ids {
                            if tx
                                .send(Event::Decoded {
                                    id,
                                    result: result.clone(),
                                })
                                .is_err()
                            {
                                return;
                            }
                        }
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
            let pool = thumbnail_pool.clone();
            thread::Builder::new()
                .name(format!("thumbnail-{index}"))
                .spawn(move || {
                    let mut written = 0;
                    while let Ok((job_epoch, id, path)) = jobs.recv() {
                        if epoch.load(Ordering::Acquire) != job_epoch {
                            continue;
                        }
                        let result = pool
                            .install(|| disk.get(&path))
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
        let (scans, scan_jobs) = unbounded();
        let tx = sender.clone();
        let current = epoch.clone();
        thread::Builder::new()
            .name("library-scan".into())
            .spawn(move || {
                while let Ok(job) = scan_jobs.recv() {
                    run_scan(job, &tx, &current);
                }
            })
            .expect("library scan thread");
        Self {
            events,
            #[cfg(feature = "experimental-nef-progressive")]
            nef_preview,
            sender,
            renders,
            decode_jobs,
            decode_cache: cache,
            thumbnails,
            scans,
            next_scan: AtomicU64::new(1),
            retire,
            epoch,
        }
    }
    pub fn decode(&self, jobs: Vec<(PhotoId, PathBuf)>, selected: usize, protected: Vec<PathBuf>) {
        *self.decode_cache.protected.lock().unwrap() = protected.into_iter().take(2).collect();
        let mut queue = self.decode_jobs.0.lock().unwrap();
        queue.replace(self.epoch(), jobs, selected);
        drop(queue);
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
        self.decode_cache.protected.lock().unwrap().clear();
        self.decode_jobs.1.notify_all();
        // Clearing the list does not invalidate immutable source revisions.
        // The bounded worker-owned decode cache survives clear/re-add/new IDs.
        self.renders.put(RenderJob::Clear);
    }
    pub fn retire_frames(&self, frames: Vec<Arc<Frame>>) {
        if !frames.is_empty() {
            let _ = self.retire.send(Garbage::Frames(frames));
        }
    }
    pub fn remember_frame(&self, path: PathBuf, metadata: PhotoMetadata, frame: Arc<Frame>) {
        let _ = self.retire.send(Garbage::CacheFrame(path, metadata, frame));
    }
    pub fn lookup_frame(&self, id: PhotoId, path: PathBuf) {
        let _ = self
            .retire
            .send(Garbage::LookupFrame(self.epoch(), id, path));
    }
    pub fn retire_images(&self, images: Vec<Arc<RenderImage>>) {
        if !images.is_empty() {
            let _ = self.retire.send(Garbage::Images(images));
        }
    }
    pub fn discard(&self, event: Event) {
        let _ = self.retire.send(Garbage::Event(event));
    }
    pub fn choose(&self, directory: bool) {
        let tx = self.sender.clone();
        let epoch = self.epoch();
        thread::spawn(move || {
            let paths = if directory {
                rfd::FileDialog::new()
                    .set_title("Add folder")
                    .pick_folder()
                    .into_iter()
                    .collect()
            } else {
                rfd::FileDialog::new()
                    .set_title("Add images")
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
    pub fn scan(&self, paths: Vec<PathBuf>, recursive: bool) -> u64 {
        let scan = self.next_scan.fetch_add(1, Ordering::Relaxed);
        let _ = self.scans.send(ScanJob::Add {
            epoch: self.epoch(),
            scan,
            paths,
            recursive,
        });
        scan
    }
    pub fn refresh(&self, roots: Vec<PathBuf>, recursive: bool) {
        let _ = self.scans.send(ScanJob::Refresh {
            epoch: self.epoch(),
            roots,
            recursive,
        });
    }
    pub fn open_file(&self, path: PathBuf, choose_application: bool) {
        let tx = self.sender.clone();
        thread::spawn(move || {
            if let Err(error) = shell_open(&path, choose_application) {
                let _ = tx.send(Event::Error(format!("{}: {error}", path.display())));
            }
        });
    }
}

fn photo_metadata(path: &Path) -> std::io::Result<PhotoMetadata> {
    let stat = path.metadata()?;
    // Read headers on the scanner, never in render_rows or any UI callback.
    let dimensions = decode::dimensions(path).ok();
    Ok(PhotoMetadata {
        dimensions,
        bytes: stat.len(),
        modified: stat.modified().ok(),
        created: stat.created().ok(),
    })
}

fn run_scan(job: ScanJob, tx: &Sender<Event>, current: &AtomicU64) {
    let (epoch, scan, paths, recursive, refreshing) = match job {
        ScanJob::Add {
            epoch,
            scan,
            paths,
            recursive,
        } => (epoch, scan, paths, recursive, false),
        ScanJob::Refresh {
            epoch,
            roots,
            recursive,
        } => (epoch, 0, roots, recursive, true),
    };
    let active = || current.load(Ordering::Acquire) == epoch;
    if !active() {
        return;
    }
    let mut batch = Vec::new();
    let mut roots = Vec::new();
    let mut complete_roots = HashSet::new();
    let mut skipped = 0;
    let mut last_batch = Instant::now();
    for path in paths {
        if !active() {
            return;
        }
        let path = path.canonicalize().unwrap_or(path);
        let is_root = refreshing || path.is_dir();
        if is_root {
            roots.push(path.clone());
            let before = skipped;
            // A removed directory is a successful empty snapshot; permission/IO
            // failures are not evidence that its photos disappeared.
            match path.metadata() {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    complete_roots.insert(path);
                    continue;
                }
                Err(_) => {
                    skipped += 1;
                    continue;
                }
                Ok(metadata) if !metadata.is_dir() => {
                    complete_roots.insert(path);
                    continue;
                }
                _ => {}
            }
            for entry in directory_walk(path.clone(), recursive) {
                if !active() {
                    return;
                }
                match entry {
                    Ok(entry) if entry.file_type().is_file() && model::supported(entry.path()) => {
                        let file = entry.into_path();
                        let file = file.canonicalize().unwrap_or(file);
                        match photo_metadata(&file) {
                            Ok(metadata) => batch.push(ScannedPhoto {
                                path: file,
                                metadata,
                                root: Some(path.clone()),
                            }),
                            Err(_) => skipped += 1,
                        }
                    }
                    Err(_) => skipped += 1,
                    _ => {}
                }
                if !refreshing
                    && !batch.is_empty()
                    && (batch.len() >= 128 || last_batch.elapsed() >= Duration::from_millis(25))
                {
                    if tx
                        .send(Event::Files {
                            epoch,
                            scan,
                            files: std::mem::take(&mut batch),
                        })
                        .is_err()
                    {
                        return;
                    }
                    last_batch = Instant::now();
                }
            }
            if skipped == before {
                complete_roots.insert(path);
            }
        } else if model::supported(&path) && path.is_file() {
            match photo_metadata(&path) {
                Ok(metadata) => batch.push(ScannedPhoto {
                    path,
                    metadata,
                    root: None,
                }),
                Err(_) => skipped += 1,
            }
        } else {
            skipped += 1;
        }
        if !refreshing
            && !batch.is_empty()
            && (batch.len() >= 128 || last_batch.elapsed() >= Duration::from_millis(25))
        {
            if tx
                .send(Event::Files {
                    epoch,
                    scan,
                    files: std::mem::take(&mut batch),
                })
                .is_err()
            {
                return;
            }
            last_batch = Instant::now();
        }
    }
    if !active() {
        return;
    }
    if refreshing {
        let _ = tx.send(Event::Refreshed {
            epoch,
            files: batch,
            complete_roots,
            skipped,
        });
    } else {
        if !roots.is_empty() {
            let _ = tx.send(Event::Roots { epoch, roots });
        }
        if !batch.is_empty() {
            let _ = tx.send(Event::Files {
                epoch,
                scan,
                files: batch,
            });
        }
        let _ = tx.send(Event::ScanDone {
            epoch,
            scan,
            skipped,
        });
    }
}

#[cfg(target_os = "windows")]
fn shell_open(path: &Path, choose_application: bool) -> Result<(), String> {
    use std::{ffi::c_void, os::windows::ffi::OsStrExt, ptr};
    #[repr(C)]
    struct OpenAsInfo {
        file: *const u16,
        class: *const u16,
        flags: u32,
    }
    #[link(name = "shell32")]
    unsafe extern "system" {
        fn ShellExecuteW(
            window: *mut c_void,
            operation: *const u16,
            file: *const u16,
            parameters: *const u16,
            directory: *const u16,
            show: i32,
        ) -> isize;
        fn SHOpenWithDialog(window: *mut c_void, info: *const OpenAsInfo) -> i32;
    }
    #[link(name = "ole32")]
    unsafe extern "system" {
        fn CoInitializeEx(reserved: *mut c_void, flags: u32) -> i32;
        fn CoUninitialize();
    }
    let file: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    unsafe {
        let initialized = CoInitializeEx(ptr::null_mut(), 2) >= 0;
        let result = if choose_application {
            let status = SHOpenWithDialog(
                ptr::null_mut(),
                &OpenAsInfo {
                    file: file.as_ptr(),
                    class: ptr::null(),
                    flags: 4,
                },
            );
            // Cancelling the native chooser is not an application error.
            if status >= 0 || status as u32 == 0x800704c7 {
                Ok(())
            } else {
                Err(format!("Open with failed (0x{:08x})", status as u32))
            }
        } else {
            let operation: Vec<u16> = "open\0".encode_utf16().collect();
            let status = ShellExecuteW(
                ptr::null_mut(),
                operation.as_ptr(),
                file.as_ptr(),
                ptr::null(),
                ptr::null(),
                1,
            );
            if status > 32 {
                Ok(())
            } else {
                Err(format!("Open file failed (shell error {status})"))
            }
        };
        if initialized {
            CoUninitialize();
        }
        result
    }
}

#[cfg(not(target_os = "windows"))]
fn shell_open(path: &Path, choose_application: bool) -> Result<(), String> {
    if choose_application {
        return Err("Open with: native application chooser is supported only on Windows; use your file manager.".into());
    }
    let command = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let status = std::process::Command::new(command)
        .arg(path)
        .status()
        .map_err(|e| format!("Open file: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("Open file: {command} exited with {status}"))
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
        let root = parent.join("scrutivue-v1");
        let marker = root.join(".cache-owner");
        let signature = b"Scrutivue thumbnail cache v1\n";
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
        "thumbnail-v2".hash(&mut hash);
        path.hash(&mut hash);
        metadata.len().hash(&mut hash);
        metadata.created().ok().hash(&mut hash);
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
    #[ignore = "read-only 45 MP pair timing; set SCRUTIVUE_PROFILE_PAIR_A and SCRUTIVUE_PROFILE_PAIR_B"]
    fn profile_dng_pair_through_services() {
        let paths: Vec<_> = ["SCRUTIVUE_PROFILE_PAIR_A", "SCRUTIVUE_PROFILE_PAIR_B"]
            .iter()
            .map(|name| PathBuf::from(std::env::var_os(name).expect("Set both exact pair paths")))
            .collect();
        let startup = Instant::now();
        let services = Services::new(
            false,
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/loading-profile/thumbnails"),
        );
        let backend = loop {
            match services
                .events
                .recv_timeout(Duration::from_secs(120))
                .unwrap()
            {
                Event::Backend(backend) => break backend,
                Event::Error(error) => eprintln!("Profile backend warning: {error}"),
                event => services.discard(event),
            }
        };
        println!(
            "services_startup_ms={:.3} backend={backend}",
            startup.elapsed().as_secs_f64() * 1000.
        );
        let start = Instant::now();
        services.decode(
            vec![(1, paths[0].clone()), (2, paths[1].clone())],
            2,
            paths.clone(),
        );
        let mut frames = HashMap::new();
        while frames.len() < 2 {
            match services
                .events
                .recv_timeout(Duration::from_secs(120))
                .unwrap()
            {
                Event::Decoded { id, result } => {
                    let frame = result.unwrap();
                    println!(
                        "services_decoded_id={id} elapsed_ms={:.3} dimensions={:?}",
                        start.elapsed().as_secs_f64() * 1000.,
                        frame.size()
                    );
                    frames.insert(id, frame);
                }
                event => services.discard(event),
            }
        }
        let decoded_ms = start.elapsed().as_secs_f64() * 1000.;
        services.renders.put(RenderJob::Frame(RenderRequest {
            generation: 1,
            a: (1, frames[&1].clone()),
            b: (2, frames[&2].clone()),
            settings: Settings {
                viewport: [1920, 1080],
                ..Settings::default()
            },
        }));
        loop {
            match services
                .events
                .recv_timeout(Duration::from_secs(120))
                .unwrap()
            {
                Event::Rendered {
                    elapsed,
                    primary,
                    alternate,
                    ..
                } => {
                    println!("services_pair_decode_ms={decoded_ms:.3} first_render_after_decode_ms={:.3} selection_to_render_ready_ms={:.3} render_worker_ms={:.3} excludes_gpui_upload_and_presentation=true", start.elapsed().as_secs_f64()*1000.-decoded_ms, start.elapsed().as_secs_f64()*1000., elapsed.as_secs_f64()*1000.);
                    services.retire_images(std::iter::once(primary).chain(alternate).collect());
                    break;
                }
                Event::Error(error) => eprintln!("Profile render warning: {error}"),
                event => services.discard(event),
            }
        }
        // This is the actual public clear/re-add path, with NEW library IDs.
        services.clear();
        let start = Instant::now();
        services.decode(
            vec![(101, paths[0].clone()), (102, paths[1].clone())],
            2,
            paths.clone(),
        );
        let mut hits = 0;
        while hits < 2 {
            match services
                .events
                .recv_timeout(Duration::from_secs(120))
                .unwrap()
            {
                Event::Decoded { id, result } => {
                    let frame = result.unwrap();
                    assert!(Arc::ptr_eq(&frame, &frames[&(id - 100)]));
                    hits += 1;
                }
                event => services.discard(event),
            }
        }
        println!(
            "services_clear_readd_pair_cache_hit_ms={:.3}",
            start.elapsed().as_secs_f64() * 1000.
        );
    }
    fn cache_source(name: &str) -> PathBuf {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(format!("target/decode-cache-tests/{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join(name);
        image::RgbImage::new(5, 3).save(&path).unwrap();
        path.canonicalize().unwrap()
    }
    #[test]
    fn decode_cache_hits_across_new_ids_clear_and_readd_without_redecoding() {
        let path = cache_source("readd.png");
        let cache = DecodeCache::new();
        let mut queue = DecodeQueue::default();
        queue.replace(0, vec![(1, path.clone())], 1);
        let (source, cancel) = queue.take().unwrap();
        let first = cache.load(&source, &|| false, decode::decode).unwrap();
        assert_eq!(queue.complete(&source, &cancel).unwrap().ids, [1]);
        queue.clear();
        queue.replace(1, vec![(91, path.clone())], 1);
        let (source, cancel) = queue.take().unwrap();
        let hit = cache
            .load(&source, &|| false, |_| {
                panic!("cache hit must not decode pixels")
            })
            .unwrap();
        assert!(Arc::ptr_eq(&first, &hit));
        assert_eq!(queue.complete(&source, &cancel).unwrap().ids, [91]);
        assert_eq!(cache.counters.decodes.load(Ordering::Relaxed), 1);
        assert_eq!(cache.counters.hits.load(Ordering::Relaxed), 1);
    }
    #[test]
    fn completed_streamed_frames_reuse_the_revision_cache_but_changed_sources_do_not() {
        let path = cache_source("stream-completed.png");
        let cache = DecodeCache::new();
        let frame = decode::decode(&path).unwrap();
        let metadata = photo_metadata(&path).unwrap();
        assert!(cache.lookup(&path).is_none());
        cache.remember(&path, &metadata, frame.clone());
        assert!(Arc::ptr_eq(&frame, &cache.lookup(&path).unwrap()));
        let full = cache
            .load(&path, &|| false, |_| {
                panic!("a completed streamed frame must not decode again")
            })
            .unwrap();
        assert!(Arc::ptr_eq(&frame, &full));
        image::RgbImage::new(9, 7).save(&path).unwrap();
        assert!(cache.lookup(&path).is_none());
        cache.remember(&path, &metadata, frame);
        assert!(cache.lookup(&path).is_none());
        let new = decode::decode(&path).unwrap();
        cache.remember(&path, &photo_metadata(&path).unwrap(), new.clone());
        assert!(Arc::ptr_eq(&new, &cache.lookup(&path).unwrap()));
        assert_eq!(cache.frames.lock().unwrap().len(), 1);
    }
    #[test]
    fn changed_size_mtime_and_created_are_distinct_cache_revisions() {
        let path = cache_source("revision.png");
        let cache = DecodeCache::new();
        let first = cache.load(&path, &|| false, decode::decode).unwrap();
        let key = SourceKey::read(&path).unwrap();
        let mut changed = key.clone();
        changed.created = Some(SystemTime::UNIX_EPOCH);
        assert_ne!(key, changed);
        // A same-size overwrite is still a new revision when mtime changes.
        let modified = key.modified.unwrap() + Duration::from_secs(10);
        let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.set_times(std::fs::FileTimes::new().set_modified(modified))
            .unwrap();
        let second = cache.load(&path, &|| false, decode::decode).unwrap();
        assert!(!Arc::ptr_eq(&first, &second));
        image::RgbImage::new(9, 7).save(&path).unwrap();
        let third = cache.load(&path, &|| false, decode::decode).unwrap();
        assert_eq!(third.size(), [9, 7]);
        assert_eq!(cache.counters.decodes.load(Ordering::Relaxed), 3);
        assert_eq!(cache.counters.invalidations.load(Ordering::Relaxed), 2);
        assert_eq!(cache.frames.lock().unwrap().len(), 1);
    }
    #[test]
    fn source_changed_during_decode_is_never_published_or_cached() {
        let path = cache_source("during.png");
        let cache = DecodeCache::new();
        let error = cache
            .load(&path, &|| false, |source| {
                let frame = decode::decode(source)?;
                image::RgbImage::new(13, 11).save(source)?;
                Ok(frame)
            })
            .unwrap_err();
        assert!(error.to_string().contains("Source changed while decoding"));
        assert!(cache.frames.lock().unwrap().is_empty());
    }
    #[test]
    fn repeated_paths_share_one_job_and_selected_pair_blocks_prefetch() {
        let mut queue = DecodeQueue::default();
        queue.replace(
            0,
            vec![
                (1, "a".into()),
                (2, "b".into()),
                (3, "a".into()),
                (4, "prefetch".into()),
            ],
            2,
        );
        let (a, ca) = queue.take().unwrap();
        let (b, cb) = queue.take().unwrap();
        assert_eq!(a, PathBuf::from("a"));
        assert_eq!(b, PathBuf::from("b"));
        assert!(queue.take().is_none());
        assert_eq!(queue.complete(&a, &ca).unwrap().ids, [1, 3]);
        assert!(queue.take().is_none());
        queue.complete(&b, &cb).unwrap();
        assert_eq!(queue.take().unwrap().0, PathBuf::from("prefetch"));
    }
    #[test]
    fn selected_job_preempts_old_prefetch_and_promotes_matching_active_work() {
        let mut queue = DecodeQueue::default();
        queue.replace(0, vec![(1, "neighbor".into())], 0);
        let (neighbor, cancel) = queue.take().unwrap();
        queue.replace(0, vec![(2, "neighbor".into()), (3, "other".into())], 2);
        assert!(!cancel.load(Ordering::Acquire));
        assert_eq!(queue.take().unwrap().0, PathBuf::from("other"));
        assert_eq!(queue.complete(&neighbor, &cancel).unwrap().ids, [2]);

        let mut queue = DecodeQueue::default();
        queue.replace(0, vec![(1, "old-prefetch".into())], 0);
        let (old, cancel) = queue.take().unwrap();
        queue.replace(0, vec![(2, "selected".into()), (1, old.clone())], 1);
        assert!(cancel.load(Ordering::Acquire));
        assert_eq!(queue.take().unwrap().0, PathBuf::from("selected"));
        assert!(queue.complete(&old, &cancel).is_none());
    }
    #[test]
    fn clear_cancels_stale_completion_and_readd_waits_for_old_flight() {
        let mut queue = DecodeQueue::default();
        queue.replace(7, vec![(1, "a".into())], 1);
        let (path, old) = queue.take().unwrap();
        queue.clear();
        queue.replace(8, vec![(9, path.clone())], 1);
        assert!(old.load(Ordering::Acquire));
        assert!(queue.take().is_none());
        assert!(queue.complete(&path, &old).is_none());
        let (path, current) = queue.take().unwrap();
        let completion = queue.complete(&path, &current).unwrap();
        assert_eq!(completion.epoch, 8);
        assert_eq!(completion.ids, [9]);
    }
    #[test]
    fn cancelled_decode_never_enters_the_cache() {
        let path = cache_source("cancelled.png");
        let cache = DecodeCache::new();
        let cancel = AtomicBool::new(false);
        let result = cache.load(&path, &|| cancel.load(Ordering::Acquire), |source| {
            let frame = decode::decode(source)?;
            cancel.store(true, Ordering::Release);
            Ok(frame)
        });
        assert!(result.is_err());
        assert!(cache.frames.lock().unwrap().is_empty());
        assert_eq!(cache.counters.cancelled.load(Ordering::Relaxed), 1);
    }
    #[test]
    fn canonical_aliases_share_a_cache_entry() {
        let path = cache_source("canonical.png");
        let alias = path
            .parent()
            .unwrap()
            .join(".")
            .join(path.file_name().unwrap());
        let cache = DecodeCache::new();
        let first = cache.load(&path, &|| false, decode::decode).unwrap();
        let second = cache
            .load(&alias, &|| false, |_| panic!("canonical cache hit"))
            .unwrap();
        assert!(Arc::ptr_eq(&first, &second));
    }
    #[test]
    fn prefetch_eviction_never_discards_protected_selected_frames() {
        let mut cache = LruCache::unbounded();
        let mut protected = HashSet::new();
        for i in 0..8 {
            let path = PathBuf::from(format!("image-{i}"));
            if i < 2 {
                protected.insert(path.clone());
            }
            cache.put(
                SourceKey {
                    path,
                    bytes: 10,
                    modified: Some(SystemTime::UNIX_EPOCH),
                    created: None,
                },
                Arc::new(Frame {
                    width: 1,
                    height: 1,
                    rgb: vec![i; 3],
                }),
            );
        }
        let mut evicted = Vec::new();
        DecodeCache::trim(&mut cache, &protected, 18, &mut evicted);
        assert_eq!(cache.len(), 3);
        assert_eq!(evicted.len(), 5);
        assert!(protected
            .iter()
            .all(|path| cache.iter().any(|(key, _)| &key.path == path)));
    }
    #[test]
    fn concurrent_cache_requests_decode_one_canonical_revision_only_once() {
        let path = cache_source("single-flight.png");
        let cache = DecodeCache::new();
        let decode_count = AtomicU64::new(0);
        let barrier = std::sync::Barrier::new(2);
        let frames = thread::scope(|scope| {
            let handles: Vec<_> = (0..2)
                .map(|_| {
                    let (cache, path, count, barrier) = (&cache, &path, &decode_count, &barrier);
                    scope.spawn(move || {
                        barrier.wait();
                        cache
                            .load(path, &|| false, |path| {
                                count.fetch_add(1, Ordering::Relaxed);
                                thread::sleep(Duration::from_millis(20));
                                decode::decode(path)
                            })
                            .unwrap()
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert!(Arc::ptr_eq(&frames[0], &frames[1]));
        assert_eq!(decode_count.load(Ordering::Relaxed), 1);
        assert_eq!(cache.counters.hits.load(Ordering::Relaxed), 1);
    }
    fn scanned(name: &str, root: Option<&str>, bytes: u64) -> ScannedPhoto {
        ScannedPhoto {
            path: PathBuf::from(name),
            root: root.map(PathBuf::from),
            metadata: PhotoMetadata {
                bytes,
                modified: Some(SystemTime::UNIX_EPOCH),
                ..PhotoMetadata::default()
            },
        }
    }
    #[test]
    fn refresh_preserves_explicit_additions_order_and_unchanged_ids() {
        let mut photos = Vec::new();
        let mut next = 1;
        merge_files(
            &mut photos,
            &mut next,
            vec![
                scanned("root/a.png", Some("root"), 10),
                scanned("root/b.png", Some("root"), 20),
                scanned("independent.png", None, 30),
                // Adding a discovered file explicitly makes it independent, too.
                scanned("root/a.png", None, 10),
            ],
        );
        photos.swap(0, 2);
        let unchanged = photos[0].id;
        let change = refresh_files(
            &mut photos,
            &mut next,
            vec![scanned("root/new.png", Some("root"), 40)],
            &HashSet::from([PathBuf::from("root")]),
        );
        assert_eq!(
            photos
                .iter()
                .map(|p| p.path.to_str().unwrap())
                .collect::<Vec<_>>(),
            ["independent.png", "root/a.png", "root/new.png"]
        );
        assert_eq!(photos[0].id, unchanged);
        assert_eq!(change.removed, [2]);
        assert!(change.replaced.is_empty());
    }
    #[test]
    fn changed_file_gets_new_revision_but_keeps_its_library_position() {
        let mut photos = Vec::new();
        let mut next = 1;
        merge_files(
            &mut photos,
            &mut next,
            vec![
                scanned("root/a.png", Some("root"), 10),
                scanned("root/b.png", Some("root"), 20),
            ],
        );
        let change = refresh_files(
            &mut photos,
            &mut next,
            vec![
                scanned("root/b.png", Some("root"), 20),
                scanned("root/a.png", Some("root"), 11),
            ],
            &HashSet::from([PathBuf::from("root")]),
        );
        assert_eq!(photos[0].path, PathBuf::from("root/a.png"));
        assert_eq!(photos[0].metadata.bytes, 11);
        assert_eq!(photos[1].id, 2);
        assert_eq!(change.replaced, [(1, 3)]);
        assert!(change.removed.is_empty());
        let mut selected = [Some(1), Some(2)];
        let mut focused = Some(1);
        change.remap_selection(&mut selected, &mut focused);
        assert_eq!(selected, [Some(3), Some(2)]);
        assert_eq!(focused, Some(3));
    }
    #[test]
    fn refresh_remaps_revisions_and_only_keeps_valid_selected_ids_without_auto_filling() {
        let mut selected = [Some(1), Some(2)];
        let mut focused = Some(1);
        LibraryChange {
            replaced: vec![(2, 3), (3, 4)],
            removed: vec![1],
        }
        .remap_selection(&mut selected, &mut focused);
        assert_eq!(selected, [Some(4), None]);
        assert_eq!(focused, None);
        let mut empty = [None; 2];
        LibraryChange::default().remap_selection(&mut empty, &mut focused);
        assert_eq!(empty, [None; 2]);
        LibraryChange {
            replaced: vec![],
            removed: vec![4],
        }
        .remap_selection(&mut selected, &mut focused);
        assert_eq!(selected, [None; 2]);
    }
    #[test]
    fn incomplete_or_other_root_does_not_remove_existing_photos() {
        let mut photos = Vec::new();
        let mut next = 1;
        merge_files(
            &mut photos,
            &mut next,
            vec![
                scanned("a/image.png", Some("a"), 10),
                scanned("b/image.png", Some("b"), 10),
            ],
        );
        let change = refresh_files(
            &mut photos,
            &mut next,
            vec![],
            &HashSet::from([PathBuf::from("a")]),
        );
        assert_eq!(change.removed, [1]);
        assert_eq!(photos[0].path, PathBuf::from("b/image.png"));
    }
    #[test]
    fn worker_metadata_contains_pixel_dimensions_and_exact_file_bytes() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(format!("target/metadata-tests/{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("size.png");
        image::RgbImage::new(17, 9).save(&path).unwrap();
        let metadata = photo_metadata(&path).unwrap();
        assert_eq!(metadata.dimensions, Some([17, 9]));
        assert_eq!(metadata.bytes, path.metadata().unwrap().len());
        assert!(metadata.label().contains("17 × 9"));
    }
    #[test]
    fn scanner_missing_root_is_an_empty_complete_snapshot() {
        let (tx, rx) = bounded(4);
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/nonexistent-refresh-root-0123456789");
        assert!(!root.exists());
        run_scan(
            ScanJob::Refresh {
                epoch: 7,
                roots: vec![root.clone()],
                recursive: true,
            },
            &tx,
            &AtomicU64::new(7),
        );
        match rx.recv().unwrap() {
            Event::Refreshed {
                files,
                complete_roots,
                skipped,
                ..
            } => {
                assert!(files.is_empty());
                assert!(complete_roots.contains(&root));
                assert_eq!(skipped, 0);
            }
            _ => panic!("expected refresh"),
        }
    }
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
        for mode in [Mode::Slider, Mode::Click] {
            for split in [0.0, 0.3, 1.0] {
                let (plan, pair) = render_plan(&Settings {
                    mode,
                    split,
                    ..Settings::default()
                });
                assert!(pair);
                assert_eq!(plan.mode, Mode::Click);
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
    fn replacing_a_mailbox_job_never_releases_it_on_the_producer() {
        struct CountDrop(Sender<thread::ThreadId>);
        impl Drop for CountDrop {
            fn drop(&mut self) {
                let _ = self.0.send(thread::current().id());
            }
        }
        let (dropped, released) = unbounded();
        let producer = thread::current().id();
        let mailbox = Mailbox::new();
        mailbox.put(CountDrop(dropped.clone()));
        mailbox.put(CountDrop(dropped.clone()));
        // Released even when the compute consumer is busy and never calls take.
        assert_ne!(
            released.recv_timeout(Duration::from_secs(2)).unwrap(),
            producer
        );
        let latest = mailbox.take();
        drop(latest);
        assert_eq!(
            released.recv_timeout(Duration::from_secs(2)).unwrap(),
            producer
        );
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
        let root = parent.join("scrutivue-v1");
        std::fs::create_dir_all(&root).unwrap();
        let photo = root.join("user-photo.png");
        std::fs::write(&photo, b"untouched").unwrap();
        let cache = ThumbnailCache::new(parent);
        assert!(!cache.owned);
        cache.prune();
        assert_eq!(std::fs::read(photo).unwrap(), b"untouched");
    }
}
