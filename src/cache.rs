//! Display-only previews. Never substitute these lossy copies for exact frames.
use crate::{
    decode::{self, Frame},
    model::{Mode, PhotoId, Settings},
    services::{render_image, Mailbox},
    streaming::Request,
};
use anyhow::{ensure, Context, Result};
use crossbeam_channel::{bounded, Receiver, Sender};
use gpui_kit::RenderImage;
use lru::LruCache;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::VecDeque,
    io::Write,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, SystemTime},
};

pub const EDGE: u32 = 2560;
pub const MIN_PIXELS: u64 = 10_000_000;
const MIB: u64 = 1024 * 1024;
const OWNER: &[u8] = b"Scrutivue display preview cache v1\n";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CacheSettings {
    pub ram_mib: u64,
    pub prefetch: bool,
    pub disk_path: PathBuf,
    pub disk_mib: u64,
    pub build_all: bool,
}
impl Default for CacheSettings {
    fn default() -> Self {
        Self {
            ram_mib: 2048,
            prefetch: false,
            disk_path: std::env::temp_dir().join("Scrutivue"),
            disk_mib: 1024,
            build_all: false,
        }
    }
}
impl CacheSettings {
    pub fn validate(&mut self) {
        self.ram_mib = self.ram_mib.clamp(64, 1_048_576);
        self.disk_mib = self.disk_mib.min(1_048_576);
        if !self.disk_path.is_absolute() {
            self.disk_path = Self::default().disk_path;
        }
    }
    pub fn full_bytes(&self) -> usize {
        (self.ram_mib * MIB / 2).min(usize::MAX as u64) as usize
    }
    pub fn preview_bytes(&self) -> usize {
        self.full_bytes()
    }
    pub fn disk_bytes(&self) -> u64 {
        self.disk_mib * MIB
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Source {
    path: PathBuf,
    bytes: u64,
    modified: SystemTime,
    created: Option<SystemTime>,
}
impl Source {
    fn read(path: &Path) -> Result<Self> {
        let path = path.canonicalize()?;
        let stat = path.metadata()?;
        ensure!(stat.is_file(), "Preview source is not a file");
        Ok(Self {
            path,
            bytes: stat.len(),
            modified: stat.modified()?,
            created: stat.created().ok(),
        })
    }
    fn digest(&self) -> String {
        self.digest_for_policy(crate::jxl::PREVIEW_DISTANCE, crate::jxl::PREVIEW_EFFORT)
    }
    fn digest_for_policy(&self, distance: f32, effort: i64) -> String {
        let mut hash = Sha256::new();
        hash.update(b"Scrutivue-preview-v1-sRGB16\0");
        hash.update(EDGE.to_le_bytes());
        hash.update(distance.to_bits().to_le_bytes());
        hash.update(effort.to_le_bytes());
        hash.update(include_bytes!("../config/libraw.json"));
        hash.update(self.path.as_os_str().as_encoded_bytes());
        hash.update(self.bytes.to_le_bytes());
        for time in [Some(self.modified), self.created] {
            hash.update(format!("{time:?}").as_bytes());
        }
        format!("{:x}", hash.finalize())
    }
}

pub struct Preview {
    pub frame: Arc<Frame>,
    pub source_size: [u32; 2],
}
#[derive(Serialize, Deserialize)]
struct Header {
    version: u32,
    key: String,
    source_size: [u32; 2],
    size: [u32; 2],
    sha256: String,
}

fn linked(path: &Path) -> bool {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return false;
    };
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return true;
        }
    }
    metadata.file_type().is_symlink()
}

/// The selected location is a parent, never itself a deletion target.
struct Disk {
    root: PathBuf,
}
impl Disk {
    fn open(parent: &Path) -> Result<Self> {
        let root = parent.join("scrutivue-previews-v1");
        ensure!(
            !linked(&root),
            "Preview cache directory cannot be a symbolic link or junction"
        );
        std::fs::create_dir_all(&root)?;
        let marker = root.join(".cache-owner");
        ensure!(!linked(&marker), "Invalid preview cache owner marker");
        if std::fs::read_dir(&root)?.next().is_none() {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&marker)?;
            file.write_all(OWNER)?;
            file.sync_all()?;
        }
        ensure!(
            std::fs::read(&marker)? == OWNER,
            "Refusing to use an unowned preview cache directory"
        );
        Ok(Self { root })
    }
    fn file(&self, key: &str, extension: &str) -> Result<PathBuf> {
        ensure!(
            key.len() == 64 && key.bytes().all(|c| c.is_ascii_hexdigit()),
            "Invalid preview cache key"
        );
        let a = self.root.join(&key[..2]);
        let b = a.join(&key[2..4]);
        ensure!(!linked(&a) && !linked(&b), "Linked preview cache shard");
        Ok(b.join(format!("{key}.{extension}")))
    }
    fn contains(&self, source: &Source) -> bool {
        let key = source.digest();
        ["json", "jxl"].iter().all(|extension| {
            self.file(&key, extension)
                .is_ok_and(|path| path.is_file() && !linked(&path))
        })
    }
    fn read(
        &self,
        source: &Source,
        library: Arc<crate::jxl::Library>,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Option<Arc<Preview>>> {
        let key = source.digest();
        let metadata = self.file(&key, "json")?;
        let pixels = self.file(&key, "jxl")?;
        if !metadata.is_file() || !pixels.is_file() {
            return Ok(None);
        }
        ensure!(
            !linked(&metadata) && !linked(&pixels),
            "Linked preview cache file"
        );
        ensure!(
            metadata.metadata()?.len() <= 8192 && pixels.metadata()?.len() <= 64 * MIB,
            "Invalid cached preview size"
        );
        let header: Header = serde_json::from_slice(&std::fs::read(&metadata)?)?;
        ensure!(
            header.version == 1 && header.key == key && eligible(header.source_size),
            "Invalid cached preview identity"
        );
        ensure!(
            header.size == preview_size(header.source_size),
            "Invalid cached preview dimensions"
        );
        let payload = std::fs::read(&pixels)?;
        ensure!(
            format!("{:x}", Sha256::digest(&payload)) == header.sha256,
            "Cached preview checksum mismatch"
        );
        let mut session = crate::jxl::Session::new(library, payload, [EDGE; 2])?;
        session.advance(1, cancelled)?;
        ensure!(
            session.size == header.size && Source::read(&source.path)? == *source,
            "Preview source changed"
        );
        // Updating only application-owned files makes pruning least-recently-used.
        for path in [&metadata, &pixels] {
            if let Ok(file) = std::fs::OpenOptions::new().write(true).open(path) {
                let _ = file.set_times(std::fs::FileTimes::new().set_modified(SystemTime::now()));
            }
        }
        Ok(Some(Arc::new(Preview {
            source_size: header.source_size,
            frame: Arc::new(Frame {
                width: session.size[0],
                height: session.size[1],
                rgb: std::mem::take(&mut session.pixels),
            }),
        })))
    }
    fn write(
        &self,
        source: &Source,
        preview: &Preview,
        payload: &[u8],
        limit: u64,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<()> {
        let key = source.digest();
        let header = Header {
            version: 1,
            key: key.clone(),
            source_size: preview.source_size,
            size: preview.frame.size(),
            sha256: format!("{:x}", Sha256::digest(payload)),
        };
        let header = serde_json::to_vec(&header)?;
        if payload.len() as u64 + header.len() as u64 > limit {
            return Ok(());
        }
        ensure!(
            !cancelled() && Source::read(&source.path)? == *source,
            "Cache preview write cancelled or source changed"
        );
        let pixels = self.file(&key, "jxl")?;
        let metadata = self.file(&key, "json")?;
        std::fs::create_dir_all(pixels.parent().unwrap())?;
        // Recheck shard links after creation, before any write.
        self.file(&key, "jxl")?;
        atomic_write(&pixels, payload, cancelled)?;
        atomic_write(&metadata, &header, cancelled)?;
        self.prune(limit)?;
        Ok(())
    }
    fn entries(&self) -> Vec<(PathBuf, u64, SystemTime)> {
        walkdir::WalkDir::new(&self.root)
            .follow_links(false)
            .min_depth(3)
            .max_depth(3)
            .into_iter()
            .filter_entry(|entry| !linked(entry.path()))
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_type().is_file())
            .filter_map(|entry| {
                let path = entry.path();
                let name = path.file_name()?.to_str()?;
                let extension = path.extension()?.to_str()?;
                let parts: Vec<_> = name.split('.').collect();
                let stem = *parts.first()?;
                let valid = matches!(extension, "jxl" | "json") && parts.len() == 2
                    || extension == "tmp"
                        && parts.len() == 4
                        && parts[1..3].iter().all(|part| {
                            !part.is_empty()
                                && part.len() <= 39
                                && part.bytes().all(|c| c.is_ascii_digit())
                        });
                if !valid || stem.len() != 64 || !stem.bytes().all(|c| c.is_ascii_hexdigit()) {
                    return None;
                }
                if self.file(stem, "jxl").ok()?.parent()? != path.parent()? {
                    return None;
                }
                let stat = entry.metadata().ok()?;
                Some((
                    path.to_owned(),
                    stat.len(),
                    stat.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                ))
            })
            .collect()
    }
    fn prune(&self, limit: u64) -> Result<()> {
        let mut entries = self.entries();
        let mut total: u64 = entries.iter().map(|e| e.1).sum();
        entries.sort_by_key(|e| e.2);
        for (path, bytes, _) in entries {
            if total <= limit {
                break;
            }
            std::fs::remove_file(path)?;
            total = total.saturating_sub(bytes);
        }
        Ok(())
    }
    fn clear(&self) -> Result<()> {
        self.prune(0)
    }
}
fn atomic_write(path: &Path, payload: &[u8], cancelled: &dyn Fn() -> bool) -> Result<()> {
    ensure!(!linked(path), "Linked cache destination");
    let temporary = path.with_extension(format!(
        "{}.{}.tmp",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)?
            .as_nanos()
    ));
    let result = (|| -> Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(payload)?;
        file.sync_all()?;
        drop(file);
        ensure!(!cancelled(), "Cache write cancelled");
        std::fs::rename(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}
pub fn eligible(size: [u32; 2]) -> bool {
    u64::from(size[0]) * u64::from(size[1]) >= MIN_PIXELS
}
fn preview_size(size: [u32; 2]) -> [u32; 2] {
    let edge = size[0].max(size[1]).max(1);
    size.map(|v| {
        ((u64::from(v) * u64::from(EDGE) + u64::from(edge) / 2) / u64::from(edge)).max(1) as u32
    })
}
fn reduce(frame: &Frame) -> Result<Arc<Preview>> {
    ensure!(
        eligible(frame.size()),
        "Source is below the 10 MP preview threshold"
    );
    let size = preview_size(frame.size());
    let image = image::ImageBuffer::<image::Rgb<u16>, _>::from_raw(
        frame.width,
        frame.height,
        frame.rgb.as_slice(),
    )
    .context("Invalid RGB16 source")?;
    let image = image::imageops::resize(
        &image,
        size[0],
        size[1],
        image::imageops::FilterType::Triangle,
    );
    Ok(Arc::new(Preview {
        source_size: frame.size(),
        frame: Arc::new(Frame {
            width: size[0],
            height: size[1],
            rgb: image.into_raw(),
        }),
    }))
}

pub struct Snapshot {
    pub generation: u64,
    pub pair: [PhotoId; 2],
    pub settings: Settings,
    pub size: [u32; 2],
    pub primary: Arc<RenderImage>,
    pub alternate: Option<Arc<RenderImage>>,
}
pub enum Event {
    Snapshot(Snapshot),
    Built { id: PhotoId, complete: bool },
    Cleared(Result<(), String>),
    Error(String),
}
type Loader = Arc<dyn Fn(&Path, &(dyn Fn() -> bool + Sync)) -> Result<Arc<Frame>> + Send + Sync>;
struct Shared {
    paths: Mutex<Vec<PathBuf>>,
    config: Mutex<CacheSettings>,
    revision: AtomicU64,
    focus: AtomicU64,
    busy: AtomicBool,
    clear: AtomicBool,
    closed: AtomicBool,
}
struct Offered {
    path: PathBuf,
    frame: Arc<Frame>,
    source: Source,
    revision: u64,
}
pub struct Worker {
    shared: Arc<Shared>,
    requests: Arc<Mailbox<Option<Request>>>,
    frames: Sender<Offered>,
    builds: Sender<(u64, PhotoId, PathBuf)>,
    pub events: Receiver<Event>,
}
impl Worker {
    pub fn new(config: CacheSettings, loader: Loader) -> Self {
        let shared = Arc::new(Shared {
            paths: Mutex::new(Vec::new()),
            config: Mutex::new(config),
            revision: AtomicU64::new(0),
            focus: AtomicU64::new(0),
            busy: AtomicBool::new(false),
            clear: AtomicBool::new(false),
            closed: AtomicBool::new(false),
        });
        let requests = Arc::new(Mailbox::new());
        let (frames, rx) = bounded(2);
        let (builds, build_rx) = bounded(2);
        let (tx, events) = bounded(8);
        let (control, jobs) = (shared.clone(), requests.clone());
        thread::Builder::new()
            .name("preview-cache".into())
            .spawn(move || run(control, jobs, rx, build_rx, tx, loader))
            .expect("preview cache worker");
        Self {
            shared,
            requests,
            frames,
            builds,
            events,
        }
    }
    pub fn configure(&self, mut config: CacheSettings) {
        config.validate();
        *self.shared.config.lock().unwrap() = config;
        self.shared.revision.fetch_add(1, Ordering::AcqRel);
    }
    pub fn request(&self, job: Option<Request>, busy: bool) {
        *self.shared.paths.lock().unwrap() = job
            .as_ref()
            .map(|job| job.pair.iter().map(|(_, path)| path.clone()).collect())
            .unwrap_or_default();
        self.shared.busy.store(busy, Ordering::Release);
        self.shared.focus.fetch_add(1, Ordering::AcqRel);
        self.requests.put(job);
    }
    /// Called on decode/release workers only; rejected large frames drop there.
    pub fn offer(&self, path: PathBuf, expected: &crate::model::PhotoMetadata, frame: Arc<Frame>) {
        if eligible(frame.size()) {
            if let Ok(source) = Source::read(&path) {
                if source.bytes == expected.bytes
                    && Some(source.modified) == expected.modified
                    && source.created == expected.created
                {
                    let _ = self.frames.try_send(Offered {
                        path,
                        frame,
                        source,
                        revision: self.shared.revision.load(Ordering::Acquire),
                    });
                }
            }
        }
    }
    pub fn build(&self, id: PhotoId, path: PathBuf) -> bool {
        self.builds
            .try_send((self.shared.revision.load(Ordering::Acquire), id, path))
            .is_ok()
    }
    pub fn clear_disk(&self) {
        self.shared.clear.store(true, Ordering::Release);
        self.shared.revision.fetch_add(1, Ordering::AcqRel);
    }
    pub fn cancel(&self) {
        self.shared.revision.fetch_add(1, Ordering::AcqRel);
        self.request(None, false);
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.shared.closed.store(true, Ordering::Release);
        self.shared.revision.fetch_add(1, Ordering::AcqRel);
    }
}

fn trim(cache: &mut LruCache<Source, Arc<Preview>>, budget: usize) {
    let mut bytes: usize = cache.iter().map(|(_, p)| p.frame.bytes()).sum();
    while bytes > budget {
        let Some((_, entry)) = cache.pop_lru() else {
            break;
        };
        bytes = bytes.saturating_sub(entry.frame.bytes());
    }
}
fn from_disk(
    disk: &Option<Disk>,
    library: &mut Option<Result<Arc<crate::jxl::Library>>>,
    source: &Source,
    pool: &rayon::ThreadPool,
    cancelled: &(dyn Fn() -> bool + Sync),
) -> Option<Arc<Preview>> {
    let store = disk.as_ref()?;
    if !store.contains(source) {
        return None;
    }
    let Ok(runtime) = library.get_or_insert_with(crate::jxl::Library::installed) else {
        return None;
    };
    pool.install(|| store.read(source, runtime.clone(), cancelled))
        .ok()
        .flatten()
}
fn already_cached(
    cache: &mut LruCache<Source, Arc<Preview>>,
    source: &Source,
    disk: &Option<Disk>,
) -> bool {
    cache.get(source).is_some() && disk.as_ref().is_none_or(|store| store.contains(source))
}

fn run(
    shared: Arc<Shared>,
    requests: Arc<Mailbox<Option<Request>>>,
    frames: Receiver<Offered>,
    builds: Receiver<(u64, PhotoId, PathBuf)>,
    tx: Sender<Event>,
    loader: Loader,
) {
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(2)
        .build()
        .expect("preview cache pool");
    let mut library = None;
    let mut cache = LruCache::<Source, Arc<Preview>>::unbounded();
    let mut config = shared.config.lock().unwrap().clone();
    let mut revision = 0;
    let mut disk: Option<Disk> = None;
    let mut disk_attempted = false;
    let mut job: Option<Request> = None;
    let mut offered = VecDeque::new();
    let mut last_error = String::new();
    while !shared.closed.load(Ordering::Acquire) {
        let next = shared.revision.load(Ordering::Acquire);
        if next != revision {
            revision = next;
            config = shared.config.lock().unwrap().clone();
            disk = None;
            disk_attempted = false;
            trim(&mut cache, config.preview_bytes());
        }
        if shared.clear.swap(false, Ordering::AcqRel) {
            cache.clear();
            offered.clear();
            let result = Disk::open(&config.disk_path)
                .and_then(|store| store.clear())
                .map_err(|e| format!("Could not clear preview cache: {e:#}"));
            let _ = tx.send(Event::Cleared(result));
        }
        if !disk_attempted && config.disk_bytes() > 0 {
            disk_attempted = true;
            match Disk::open(&config.disk_path).and_then(|store| {
                store.prune(config.disk_bytes())?;
                Ok(store)
            }) {
                Ok(store) => disk = Some(store),
                Err(error) => {
                    let text = format!("Preview disk cache unavailable: {error:#}");
                    if text != last_error {
                        last_error = text.clone();
                        let _ = tx.try_send(Event::Error(text));
                    }
                }
            }
        }
        let mut dirty = false;
        if let Some(request) = requests.try_take() {
            job = request;
            dirty = true;
        }
        if dirty {
            if let Some(request) = &job {
                let focus = shared.focus.load(Ordering::Acquire);
                for side in 0..2 {
                    if request.ready[side].is_some() {
                        continue;
                    }
                    if let Ok(source) = Source::read(&request.pair[side].1) {
                        if cache.get(&source).is_none() {
                            let path = &request.pair[side].1;
                            let cancelled = || {
                                shared.revision.load(Ordering::Acquire) != revision
                                    || shared.closed.load(Ordering::Acquire)
                                    || !shared.paths.lock().unwrap().contains(path)
                            };
                            // Corrupt/incomplete entries are misses; normal decoding is unaffected.
                            if let Some(preview) =
                                from_disk(&disk, &mut library, &source, &pool, &cancelled)
                            {
                                cache.put(source, preview);
                            }
                        }
                    }
                }
                if shared.focus.load(Ordering::Acquire) == focus {
                    if let Some(snapshot) = pool.install(|| snapshot(request, &mut cache, &pool)) {
                        let _ = tx.try_send(Event::Snapshot(snapshot));
                    }
                }
                trim(&mut cache, config.preview_bytes());
            }
        }
        offered.extend(frames.try_iter());
        while offered.len() > 2 {
            offered.pop_front();
        }
        if !shared.busy.load(Ordering::Acquire) {
            let work = offered.pop_front();
            let build = if work.is_none() {
                builds.try_recv().ok()
            } else {
                None
            };
            if work.as_ref().is_some_and(|work| work.revision != revision)
                || build
                    .as_ref()
                    .is_some_and(|(generation, _, _)| *generation != revision)
            {
                if let Some((_, id, _)) = build {
                    let _ = tx.send(Event::Built {
                        id,
                        complete: false,
                    });
                }
                continue;
            }
            if work.is_some() || build.is_some() {
                let focus = shared.focus.load(Ordering::Acquire);
                let cancelled = || {
                    shared.closed.load(Ordering::Acquire)
                        || shared.revision.load(Ordering::Acquire) != revision
                        || (shared.busy.load(Ordering::Acquire)
                            && shared.focus.load(Ordering::Acquire) != focus)
                };
                let result = (|| -> Result<()> {
                    let (source, frame) = if let Some(work) = work {
                        ensure!(
                            work.revision == revision && Source::read(&work.path)? == work.source,
                            "Obsolete preview source"
                        );
                        (work.source, work.frame)
                    } else {
                        let (_, _, path) = build.as_ref().unwrap();
                        let source = Source::read(path)?;
                        let size = decode::dimensions(path)?;
                        if !eligible(size) {
                            return Ok(());
                        }
                        if already_cached(&mut cache, &source, &disk) {
                            return Ok(());
                        }
                        if let Some(preview) =
                            from_disk(&disk, &mut library, &source, &pool, &cancelled)
                        {
                            cache.put(source, preview);
                            trim(&mut cache, config.preview_bytes());
                            return Ok(());
                        }
                        let frame = loader(path, &cancelled)?;
                        (source, frame)
                    };
                    if already_cached(&mut cache, &source, &disk) {
                        return Ok(());
                    }
                    if let Some(preview) =
                        from_disk(&disk, &mut library, &source, &pool, &cancelled)
                    {
                        cache.put(source, preview);
                        trim(&mut cache, config.preview_bytes());
                        return Ok(());
                    }
                    ensure!(!cancelled(), "Preview cache job cancelled");
                    if !eligible(frame.size()) {
                        return Ok(());
                    }
                    let preview = pool.install(|| reduce(&frame))?;
                    drop(frame);
                    ensure!(
                        !cancelled() && Source::read(&source.path)? == source,
                        "Preview source changed"
                    );
                    cache.put(source.clone(), preview.clone());
                    trim(&mut cache, config.preview_bytes());
                    if let Some(store) = &disk {
                        let runtime = library
                            .get_or_insert_with(crate::jxl::Library::installed)
                            .as_ref()
                            .map_err(|error| {
                                anyhow::anyhow!(
                                    "Pinned JPEG XL cache encoder unavailable: {error:#}"
                                )
                            })?;
                        let payload = pool.install(|| {
                            crate::jxl::encode_preview(runtime, &preview.frame, &cancelled)
                        })?;
                        ensure!(!cancelled(), "Preview cache job cancelled");
                        store.write(
                            &source,
                            &preview,
                            &payload,
                            config.disk_bytes(),
                            &cancelled,
                        )?;
                    }
                    cache.put(source, preview);
                    trim(&mut cache, config.preview_bytes());
                    Ok(())
                })();
                if let Some((_, id, _)) = build {
                    let _ = tx.send(Event::Built {
                        id,
                        complete: !cancelled(),
                    });
                }
                if let Err(error) = result {
                    if !cancelled() {
                        let text = format!("Preview cache: {error:#}");
                        if text != last_error {
                            last_error = text.clone();
                            let _ = tx.try_send(Event::Error(text));
                        }
                    }
                }
            }
        }
        thread::sleep(Duration::from_millis(8));
    }
}

fn snapshot(
    job: &Request,
    cache: &mut LruCache<Source, Arc<Preview>>,
    pool: &rayon::ThreadPool,
) -> Option<Snapshot> {
    if job.settings.highpass || job.settings.mode == Mode::Diff {
        return None;
    }
    let mut previews = [None, None];
    for (side, preview) in previews.iter_mut().enumerate() {
        if job.ready[side].is_none() {
            *preview = cache.get(&Source::read(&job.pair[side].1).ok()?).cloned();
        }
    }
    let frame = |side: usize| {
        job.ready[side]
            .as_ref()
            .or_else(|| previews[side].as_ref().map(|p| &p.frame))
    };
    let a = frame(0)?;
    let b = frame(1)?;
    let source_size = previews[0]
        .as_ref()
        .map(|p| p.source_size)
        .unwrap_or_else(|| a.size());
    let [w, h] = job.settings.viewport;
    if w == 0 || h == 0 {
        return None;
    }
    // Send a bounded source texture, not a CPU-rendered viewport. The old
    // per-screen-pixel interpolation cost more than decoding the cached JXL.
    let primary = display_texture(a, pool)?;
    let alternate = if job.pair[0].0 != job.pair[1].0 {
        Some(display_texture(b, pool)?)
    } else {
        None
    };
    Some(Snapshot {
        generation: job.generation,
        pair: job.pair.each_ref().map(|(id, _)| *id),
        settings: job.settings.clone(),
        size: source_size,
        primary,
        alternate,
    })
}

fn display_texture(frame: &Frame, pool: &rayon::ThreadPool) -> Option<Arc<RenderImage>> {
    if frame.width == 0 || frame.height == 0 {
        return None;
    }
    let reduced;
    let frame = if frame.width.max(frame.height) > EDGE {
        let size = preview_size(frame.size());
        let image = image::ImageBuffer::<image::Rgb<u16>, _>::from_raw(
            frame.width,
            frame.height,
            frame.rgb.as_slice(),
        )?;
        let image = pool.install(|| {
            image::imageops::resize(
                &image,
                size[0],
                size[1],
                image::imageops::FilterType::Triangle,
            )
        });
        reduced = Frame {
            width: size[0],
            height: size[1],
            rgb: image.into_raw(),
        };
        &reduced
    } else {
        frame
    };
    if frame.rgb.len() != frame.width as usize * frame.height as usize * 3 {
        return None;
    }
    let mut pixels = vec![0; frame.width as usize * frame.height as usize * 4];
    pool.install(|| {
        pixels
            .par_chunks_mut(4)
            .zip(frame.rgb.par_chunks_exact(3))
            .for_each(|(out, rgb)| {
                // RGB16 sRGB to BGRA8; integer rounding matches the display conversion.
                out[0] = ((u32::from(rgb[2]) + 128) / 257) as u8;
                out[1] = ((u32::from(rgb[1]) + 128) / 257) as u8;
                out[2] = ((u32::from(rgb[0]) + 128) / 257) as u8;
                out[3] = 255;
            })
    });
    Some(render_image(frame.width, frame.height, pixels))
}

#[cfg(test)]
fn preview_pixels(
    frame: &Frame,
    source_size: [u32; 2],
    settings: &Settings,
    pool: &rayon::ThreadPool,
) -> Vec<u8> {
    let [w, h] = settings.viewport;
    let scale = settings.camera.scale(source_size, settings.viewport);
    let zoom = [
        scale * source_size[0] as f32 / frame.width as f32,
        scale * source_size[1] as f32 / frame.height as f32,
    ];
    let mut output = vec![0; w as usize * h as usize * 4];
    pool.install(|| {
        output
            .par_chunks_mut(w as usize * 4)
            .enumerate()
            .for_each(|(y, row)| {
                for x in 0..w as usize {
                    let point = [
                        settings.camera.center[0]
                            + (x as f32 + 0.5 - w as f32 * 0.5) / (scale * source_size[0] as f32),
                        settings.camera.center[1]
                            + (y as f32 + 0.5 - h as f32 * 0.5) / (scale * source_size[1] as f32),
                    ];
                    let color = if point.iter().all(|p| (0. ..1.).contains(p)) {
                        crate::processing::sample(
                            frame,
                            None,
                            point[0] * frame.width as f32 - 0.5,
                            point[1] * frame.height as f32 - 0.5,
                            zoom,
                        )
                    } else {
                        [if (x / 16 + y / 16) & 1 == 1 {
                            0.075
                        } else {
                            0.085
                        }; 3]
                    };
                    for c in 0..3 {
                        row[x * 4 + 2 - c] =
                            (color[c].clamp(0., 1.) * 255.).round_ties_even() as u8;
                    }
                    row[x * 4 + 3] = 255;
                }
            })
    });
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    fn directory(name: &str) -> PathBuf {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/preview-cache-tests")
            .join(format!(
                "{}-{name}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }
    fn source(root: &Path) -> Source {
        let path = root.join("source.bin");
        std::fs::write(&path, b"source image must remain unchanged").unwrap();
        Source::read(&path).unwrap()
    }
    #[test]
    fn settings_split_budget_and_reject_relative_locations() {
        let mut settings = CacheSettings::default();
        assert_eq!(settings.full_bytes(), 1024 * 1024 * 1024);
        assert_eq!(settings.preview_bytes(), settings.full_bytes());
        assert_eq!(settings.disk_bytes(), 1024 * 1024 * 1024);
        settings.ram_mib = 1;
        settings.disk_mib = u64::MAX;
        settings.disk_path = "relative".into();
        settings.validate();
        assert_eq!(settings.ram_mib, 64);
        assert_eq!(settings.disk_mib, 1_048_576);
        assert!(settings.disk_path.is_absolute());
    }
    #[test]
    fn threshold_is_area_not_long_edge() {
        assert!(!eligible([10_000, 999]));
        assert!(eligible([3200, 3125]));
        assert_eq!(preview_size([5520, 8280]), [1707, 2560]);
        assert_eq!(preview_size([8280, 5520]), [2560, 1707]);
        assert_eq!(preview_size([50_000, 200]), [2560, 10]);
        let small = Frame {
            width: 10,
            height: 10,
            rgb: vec![0; 300],
        };
        assert!(reduce(&small).is_err());
    }
    #[test]
    fn source_keys_are_canonical_revision_and_pipeline_aware() {
        let root = directory("keys");
        let first = source(&root);
        let alias = Source::read(&root.join(".").join("source.bin")).unwrap();
        assert_eq!(first.digest(), alias.digest());
        assert_eq!(crate::jxl::PREVIEW_DISTANCE, 0.05);
        assert_eq!(first.digest(), first.digest_for_policy(0.05, 3));
        assert_ne!(first.digest(), first.digest_for_policy(0.03, 3));
        assert_ne!(first.digest(), first.digest_for_policy(0.05, 4));
        std::fs::write(&first.path, b"different source revision").unwrap();
        assert_ne!(first.digest(), Source::read(&first.path).unwrap().digest());
        let disk = Disk::open(&root).unwrap();
        let key = first.digest();
        let file = disk.file(&key, "jxl").unwrap();
        assert_eq!(file.parent().unwrap().file_name().unwrap(), &key[2..4]);
        assert!(disk.file("../../source", "jxl").is_err());
    }
    #[test]
    fn nonempty_unmarked_directories_and_foreign_files_are_preserved() {
        let root = directory("foreign");
        let owned = root.join("scrutivue-previews-v1");
        std::fs::create_dir_all(&owned).unwrap();
        let photo = owned.join("photograph.jxl");
        std::fs::write(&photo, b"do not delete").unwrap();
        assert!(Disk::open(&root).is_err());
        assert_eq!(std::fs::read(photo).unwrap(), b"do not delete");
        let other = directory("marked");
        let disk = Disk::open(&other).unwrap();
        let foreign = disk.root.join("user-photo.jpg");
        std::fs::write(&foreign, b"keep").unwrap();
        disk.clear().unwrap();
        assert_eq!(std::fs::read(foreign).unwrap(), b"keep");
        assert_eq!(
            std::fs::read(disk.root.join(".cache-owner")).unwrap(),
            OWNER
        );
    }
    #[test]
    fn budget_and_reset_only_touch_generated_hash_tree_entries() {
        let root = directory("prune");
        let disk = Disk::open(&root).unwrap();
        let source = source(&root);
        for i in 0..3 {
            let key = format!("{i:064x}");
            let path = disk.file(&key, "jxl").unwrap();
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            atomic_write(&path, &[0; 100], &|| false).unwrap();
        }
        let fake = disk.root.join("not-a-hash.jxl");
        std::fs::write(&fake, b"foreign").unwrap();
        let key = format!("{:064x}", 99);
        let orphan = disk
            .file(&key, "jxl")
            .unwrap()
            .with_extension("123.456.tmp");
        std::fs::create_dir_all(orphan.parent().unwrap()).unwrap();
        std::fs::write(&orphan, [0; 100]).unwrap();
        assert_eq!(disk.entries().len(), 4);
        disk.prune(150).unwrap();
        assert!(disk.entries().iter().map(|entry| entry.1).sum::<u64>() <= 150);
        disk.clear().unwrap();
        assert!(disk.entries().is_empty());
        assert_eq!(std::fs::read(fake).unwrap(), b"foreign");
        assert_eq!(Source::read(&source.path).unwrap(), source);
        assert!(!orphan.exists());
    }
    #[test]
    fn cancelled_atomic_writes_leave_no_partial_file() {
        let root = directory("atomic");
        let file = root.join("preview.jxl");
        assert!(atomic_write(&file, b"payload", &|| true).is_err());
        assert!(!file.exists());
        assert_eq!(std::fs::read_dir(root).unwrap().count(), 0);
    }
    #[test]
    fn ram_eviction_is_byte_bounded_without_a_six_entry_cap() {
        let root = directory("ram");
        let source = source(&root);
        let mut cache = LruCache::unbounded();
        for index in 0..20 {
            let mut key = source.clone();
            key.bytes = index;
            cache.put(
                key,
                Arc::new(Preview {
                    source_size: [4000, 3000],
                    frame: Arc::new(Frame {
                        width: 1,
                        height: 1,
                        rgb: vec![index as u16; 3],
                    }),
                }),
            );
        }
        trim(&mut cache, 60);
        assert_eq!(cache.len(), 10);
        trim(&mut cache, 0);
        assert!(cache.is_empty());
    }
    #[test]
    fn ram_hits_do_not_skip_persistence_after_a_disk_location_change() {
        let root = directory("relocate");
        let source = source(&root);
        let mut cache = LruCache::unbounded();
        cache.put(
            source.clone(),
            Arc::new(Preview {
                source_size: [4000, 3000],
                frame: Arc::new(Frame {
                    width: 1,
                    height: 1,
                    rgb: vec![0; 3],
                }),
            }),
        );
        assert!(already_cached(&mut cache, &source, &None));
        let disk = Disk::open(&root).unwrap();
        for extension in ["jxl", "json"] {
            let path = disk.file(&source.digest(), extension).unwrap();
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            atomic_write(&path, b"existing", &|| false).unwrap();
        }
        assert!(already_cached(&mut cache, &source, &Some(disk)));
        let other = Disk::open(&directory("relocate-new")).unwrap();
        assert!(!already_cached(&mut cache, &source, &Some(other)));
    }
    #[test]
    fn previews_never_render_diff_or_highpass() {
        let root = directory("exact");
        let source = source(&root);
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .unwrap();
        let mut cache = LruCache::unbounded();
        let mut job = Request {
            generation: 1,
            pair: [(1, source.path.clone()), (1, source.path)],
            settings: Settings::default(),
            ready: [None, None],
        };
        job.settings.mode = Mode::Diff;
        assert!(snapshot(&job, &mut cache, &pool).is_none());
        job.settings.mode = Mode::Click;
        job.settings.highpass = true;
        assert!(snapshot(&job, &mut cache, &pool).is_none());
    }
    #[test]
    fn cached_viewport_uses_original_geometry_even_with_rounded_preview_dimensions() {
        use crate::model::{Camera, Zoom};
        let frame = Frame {
            width: 17,
            height: 25,
            rgb: vec![40000; 17 * 25 * 3],
        };
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .unwrap();
        for zoom in [
            Zoom::Fit,
            Zoom::Fixed(0.5),
            Zoom::Fixed(1.),
            Zoom::Fixed(8.),
        ] {
            let settings = Settings {
                viewport: [41, 37],
                camera: Camera {
                    zoom,
                    center: [0.2, 0.7],
                },
                ..Default::default()
            };
            let source_size = [5520, 8280];
            let output = preview_pixels(&frame, source_size, &settings, &pool);
            let scale = settings.camera.scale(source_size, settings.viewport);
            for y in 0..37 {
                for x in 0..41 {
                    let p = [
                        settings.camera.center[0] + (x as f32 + 0.5 - 20.5) / (scale * 5520.),
                        settings.camera.center[1] + (y as f32 + 0.5 - 18.5) / (scale * 8280.),
                    ];
                    let byte = output[(y * 41 + x) * 4];
                    assert_eq!(
                        byte > 100,
                        p.iter().all(|v| (0. ..1.).contains(v)),
                        "zoom {zoom:?}, pixel {x},{y}"
                    );
                }
            }
        }
    }
    #[test]
    fn lazy_worker_skips_small_images_and_cancelled_queued_builds() {
        let root = directory("worker");
        let path = root.join("small.png");
        image::RgbImage::new(12, 10).save(&path).unwrap();
        let worker = Worker::new(
            CacheSettings {
                disk_mib: 0,
                disk_path: root.clone(),
                ..Default::default()
            },
            Arc::new(|_, _| {
                panic!("Small images must never request a full decode for preview caching")
            }),
        );
        assert!(worker.build(7, path.clone()));
        loop {
            match worker.events.recv_timeout(Duration::from_secs(5)).unwrap() {
                Event::Built { id, complete } => {
                    assert_eq!(id, 7);
                    assert!(complete);
                    break;
                }
                Event::Error(error) => panic!("{error}"),
                _ => {}
            }
        }
        worker.request(None, true);
        thread::sleep(Duration::from_millis(30));
        assert!(worker.build(8, path));
        worker.cancel();
        loop {
            match worker.events.recv_timeout(Duration::from_secs(5)).unwrap() {
                Event::Built { id, complete } => {
                    assert_eq!(id, 8);
                    assert!(!complete);
                    break;
                }
                Event::Error(error) => panic!("{error}"),
                _ => {}
            }
        }
        assert!(!root.join("scrutivue-previews-v1").exists());
    }
    #[test]
    fn source_textures_preserve_bgra_quantization_and_do_not_upscale_small_frames() {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .unwrap();
        let frame = Frame {
            width: 256,
            height: 256,
            rgb: (0..=u16::MAX).flat_map(|v| [v, 0, u16::MAX - v]).collect(),
        };
        let image = display_texture(&frame, &pool).unwrap();
        assert_eq!(u32::from(image.size(0).width), 256);
        assert_eq!(u32::from(image.size(0).height), 256);
        for (index, pixel) in image.as_bytes(0).unwrap().chunks_exact(4).enumerate() {
            let quantize = |v: u16| (v as f32 / 65535. * 255.).round_ties_even() as u8;
            assert_eq!(
                pixel,
                [
                    quantize(u16::MAX - index as u16),
                    0,
                    quantize(index as u16),
                    255
                ]
            );
        }
    }
    #[test]
    fn cached_snapshots_have_bounded_source_textures_instead_of_viewport_sized_buffers() {
        let root = directory("texture");
        let source = source(&root);
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .unwrap();
        let mut cache = LruCache::unbounded();
        cache.put(
            source.clone(),
            Arc::new(Preview {
                source_size: [5520, 8280],
                frame: Arc::new(Frame {
                    width: 17,
                    height: 25,
                    rgb: vec![32000; 17 * 25 * 3],
                }),
            }),
        );
        let mut job = Request {
            generation: 1,
            pair: [(1, source.path.clone()), (1, source.path)],
            settings: Settings {
                viewport: [100_000, 100_000],
                ..Default::default()
            },
            ready: [None, None],
        };
        let first = snapshot(&job, &mut cache, &pool).unwrap();
        assert_eq!(first.size, [5520, 8280]);
        assert_eq!(first.primary.as_bytes(0).unwrap().len(), 17 * 25 * 4);
        assert!(first.alternate.is_none());
        job.pair[1].0 = 2;
        job.ready[1] = Some(Arc::new(Frame {
            width: EDGE + 1,
            height: 1,
            rgb: vec![40000; (EDGE + 1) as usize * 3],
        }));
        let paired = snapshot(&job, &mut cache, &pool).unwrap();
        assert_eq!(
            paired.alternate.unwrap().as_bytes(0).unwrap().len(),
            EDGE as usize * 4
        );
        assert_eq!(job.ready[1].as_ref().unwrap().width, EDGE + 1);
    }
    #[test]
    #[ignore = "requires SCRUTIVUE_CACHE_PROFILE_SOURCE/PARENT/OUTPUT and an existing native preview"]
    fn native_cached_preview_stage_profile() {
        use std::time::Instant;
        let path = PathBuf::from(std::env::var_os("SCRUTIVUE_CACHE_PROFILE_SOURCE").unwrap());
        let parent = PathBuf::from(std::env::var_os("SCRUTIVUE_CACHE_PROFILE_PARENT").unwrap());
        let output = PathBuf::from(std::env::var_os("SCRUTIVUE_CACHE_PROFILE_OUTPUT").unwrap());
        let source = Source::read(&path).unwrap();
        let disk = Disk::open(&parent).unwrap();
        assert!(disk.contains(&source));
        let start = Instant::now();
        let library = Arc::new(
            crate::jxl::Library::load(
                &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/release/libjxl/jxl.dll"),
            )
            .unwrap(),
        );
        let library_ms = start.elapsed().as_secs_f64() * 1000.;
        let mut records = Vec::new();
        for threads in [2, 4, 6, 8] {
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .build()
                .unwrap();
            for repeat in 0..3 {
                let start = Instant::now();
                let payload = std::fs::read(disk.file(&source.digest(), "jxl").unwrap()).unwrap();
                let file_ms = start.elapsed().as_secs_f64() * 1000.;
                let start = Instant::now();
                let digest = format!("{:x}", Sha256::digest(&payload));
                let checksum_ms = start.elapsed().as_secs_f64() * 1000.;
                let start = Instant::now();
                let mut session = pool
                    .install(|| crate::jxl::Session::new(library.clone(), payload, [EDGE; 2]))
                    .unwrap();
                pool.install(|| session.advance(1, &|| false)).unwrap();
                let decode_ms = start.elapsed().as_secs_f64() * 1000.;
                let frame = Frame {
                    width: session.size[0],
                    height: session.size[1],
                    rgb: std::mem::take(&mut session.pixels),
                };
                let source_size = decode::dimensions(&path).unwrap();
                let settings = Settings {
                    viewport: [3276, 2019],
                    ..Default::default()
                };
                let start = Instant::now();
                let pixels = preview_pixels(&frame, source_size, &settings, &pool);
                let render_ms = start.elapsed().as_secs_f64() * 1000.;
                std::hint::black_box(pixels);
                let start = Instant::now();
                let texture = display_texture(&frame, &pool).unwrap();
                let texture_ms = start.elapsed().as_secs_f64() * 1000.;
                std::hint::black_box(texture);
                records.push(serde_json::json!({"threads":threads,"repeat":repeat,"file_ms":file_ms,"checksum_ms":checksum_ms,"jxl_decode_ms":decode_ms,"legacy_viewport_render_ms":render_ms,"source_texture_prepare_ms":texture_ms,"payload_sha256":digest,"preview_size":frame.size()}));
            }
        }
        let report = serde_json::json!({"source":path,"library_ms":library_ms,"records":records});
        std::fs::write(output, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
        println!("{report}");
    }
    #[test]
    #[ignore = "requires the pinned native JPEG XL runtime; exercises the actual distance/effort encoder"]
    fn native_jxl_disk_roundtrip_corruption_and_source_invalidation() {
        let root = directory("native");
        let source = source(&root);
        let disk = Disk::open(&root).unwrap();
        let library = Arc::new(
            crate::jxl::Library::load(
                &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/release/libjxl/jxl.dll"),
            )
            .unwrap(),
        );
        let [w, h] = [3200, 3125];
        let mut rgb = Vec::with_capacity(w as usize * h as usize * 3);
        for y in 0..h {
            for x in 0..w {
                rgb.extend_from_slice(&[
                    (x * 65535 / (w - 1)) as u16,
                    (y * 65535 / (h - 1)) as u16,
                    ((x + y) * 32767 / (w + h - 2)) as u16,
                ]);
            }
        }
        let full = Frame {
            width: w,
            height: h,
            rgb,
        };
        let preview = reduce(&full).unwrap();
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .unwrap();
        let payload = pool
            .install(|| crate::jxl::encode_preview(&library, &preview.frame, &|| false))
            .unwrap();
        disk.write(&source, &preview, &payload, 32 * MIB, &|| false)
            .unwrap();
        let decoded = pool
            .install(|| disk.read(&source, library.clone(), &|| false))
            .unwrap()
            .unwrap();
        assert_eq!(decoded.frame.size(), preview.frame.size());
        assert_eq!(decoded.source_size, [w, h]);
        let error: f64 = decoded
            .frame
            .rgb
            .iter()
            .zip(&preview.frame.rgb)
            .map(|(a, b)| a.abs_diff(*b) as f64)
            .sum::<f64>()
            / preview.frame.rgb.len() as f64;
        assert!(error < 256., "sRGB16 cache roundtrip error {error}");
        let path = disk.file(&source.digest(), "jxl").unwrap();
        let mut corrupt = payload;
        corrupt[8] ^= 1;
        std::fs::write(path, corrupt).unwrap();
        assert!(disk.read(&source, library.clone(), &|| false).is_err());
        std::fs::write(&source.path, b"changed").unwrap();
        let changed = Source::read(&source.path).unwrap();
        assert!(disk.read(&changed, library, &|| false).unwrap().is_none());
        assert!(disk
            .write(&source, &preview, b"obsolete", 32 * MIB, &|| false)
            .is_err());
        assert_eq!(std::fs::read(&source.path).unwrap(), b"changed");
    }
}
