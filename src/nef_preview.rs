//! Bounded, display-only NEF bands. Never enter the full-frame/analysis cache.
pub mod trace;
use crate::{
    decode::{progressive::Patch, Frame},
    model::{Mode, PhotoId, Settings},
    services::{render_image, Mailbox},
    streaming::Request,
};
use crossbeam_channel::{bounded, Receiver, Sender};
use gpui_kit::RenderImage;
use rayon::prelude::*;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

pub struct Snapshot {
    pub generation: u64,
    pub pair: [PhotoId; 2],
    pub settings: Settings,
    pub size: [u32; 2],
    pub primary: Arc<RenderImage>,
    pub alternate: Option<Arc<RenderImage>>,
    pub covered_pixels: [usize; 2],
    flights: Vec<Arc<AtomicBool>>,
}
impl Snapshot {
    pub fn is_valid(&self) -> bool {
        self.flights
            .iter()
            .all(|cancel| !cancel.load(Ordering::Acquire))
    }
}
struct Update {
    photo: PhotoId,
    path: PathBuf,
    cancelled: Arc<AtomicBool>,
    patch: Patch,
}
#[derive(Clone)]
struct Focus {
    pair: [(PhotoId, PathBuf); 2],
    center: [f32; 2],
}
#[derive(Clone)]
pub struct Publisher {
    updates: Sender<Update>,
    focus: Arc<Mutex<Option<Focus>>>,
    closed: Arc<AtomicBool>,
}
impl Publisher {
    pub fn target(&self, path: &Path) -> Option<(PhotoId, [f32; 2])> {
        self.focus.lock().unwrap().as_ref().and_then(|focus| {
            focus
                .pair
                .iter()
                .find(|(_, candidate)| candidate == path)
                .map(|(id, _)| (*id, focus.center))
        })
    }
    pub fn patch(&self, photo: PhotoId, path: PathBuf, cancelled: Arc<AtomicBool>, patch: Patch) {
        if !cancelled.load(Ordering::Acquire)
            && !self.closed.load(Ordering::Acquire)
            && self.target(&path).is_some_and(|(id, _)| id == photo)
        {
            // A slow renderer must never stall decoding or retain an unbounded
            // queue. Missing preview bands are filled by the authoritative frame.
            let _ = self.updates.try_send(Update {
                photo,
                path,
                cancelled,
                patch,
            });
        }
    }
}
pub struct Worker {
    publisher: Publisher,
    requests: Arc<Mailbox<(u64, Option<Request>)>>,
    revision: Arc<AtomicU64>,
    pub events: Receiver<Snapshot>,
}
impl Worker {
    pub fn new() -> Self {
        let (updates, data) = bounded(8);
        let (tx, events) = bounded(2);
        let publisher = Publisher {
            updates,
            focus: Arc::new(Mutex::new(None)),
            closed: Arc::new(AtomicBool::new(false)),
        };
        let requests = Arc::new(Mailbox::new());
        let revision = Arc::new(AtomicU64::new(0));
        let (jobs, current, stopped) =
            (requests.clone(), revision.clone(), publisher.closed.clone());
        thread::Builder::new()
            .name("nef-preview".into())
            .spawn(move || run(jobs, data, tx, current, stopped))
            .expect("NEF preview worker");
        Self {
            publisher,
            requests,
            revision,
            events,
        }
    }
    pub fn publisher(&self) -> Publisher {
        self.publisher.clone()
    }
    pub fn request(&self, request: Option<Request>) {
        trace::record(
            "preview_request",
            serde_json::json!({"generation":request.as_ref().map(|job|job.generation),
            "pair":request.as_ref().map(|job|job.pair.each_ref().map(|(id,_)|*id))}),
        );
        *self.publisher.focus.lock().unwrap() = request.as_ref().map(|job| Focus {
            pair: job.pair.clone(),
            center: job.settings.camera.center,
        });
        let revision = self.revision.fetch_add(1, Ordering::AcqRel) + 1;
        // Mailbox retires superseded ready-frame references on a worker, not UI.
        self.requests.put((revision, request));
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.publisher.closed.store(true, Ordering::Release);
    }
}

struct Image {
    photo: PhotoId,
    cancelled: Arc<AtomicBool>,
    size: [u32; 2],
    grid: [u32; 2],
    ratio: u32,
    rgb: Vec<u16>,
    ready: Vec<bool>,
    covered: usize,
}
impl Image {
    fn new(photo: PhotoId, update: &Update) -> Option<Self> {
        let patch = &update.patch;
        if patch.ratio != 4 || patch.image_size.contains(&0) {
            return None;
        }
        let grid = patch.image_size.map(|v| v.div_ceil(patch.ratio));
        let count = usize::try_from(u64::from(grid[0]) * u64::from(grid[1])).ok()?;
        if count > 8 * 1024 * 1024 {
            return None;
        }
        Some(Self {
            photo,
            cancelled: update.cancelled.clone(),
            size: patch.image_size,
            grid,
            ratio: patch.ratio,
            rgb: vec![0; count * 3],
            ready: vec![false; count],
            covered: 0,
        })
    }
    fn apply(&mut self, patch: &Patch) -> bool {
        if patch.image_size != self.size
            || patch.ratio != self.ratio
            || patch.size.contains(&0)
            || u64::from(patch.size[0]) * u64::from(patch.size[1]) > 8 * 1024 * 1024
            || patch.rgb.len() != patch.size[0] as usize * patch.size[1] as usize * 3
            || u64::from(patch.origin[0]) + u64::from(patch.size[0]) > u64::from(self.grid[0])
            || u64::from(patch.origin[1]) + u64::from(patch.size[1]) > u64::from(self.grid[1])
        {
            return false;
        }
        for y in 0..patch.size[1] as usize {
            let from = y * patch.size[0] as usize;
            let to =
                (y + patch.origin[1] as usize) * self.grid[0] as usize + patch.origin[0] as usize;
            let count = patch.size[0] as usize;
            self.rgb[to * 3..(to + count) * 3]
                .copy_from_slice(&patch.rgb[from * 3..(from + count) * 3]);
            for value in &mut self.ready[to..to + count] {
                if !*value {
                    self.covered += 1;
                    *value = true;
                }
            }
        }
        true
    }
    fn sample(&self, point: [u32; 2]) -> Option<[u16; 3]> {
        if self.cancelled.load(Ordering::Acquire)
            || point[0] >= self.size[0]
            || point[1] >= self.size[1]
        {
            return None;
        }
        let p = (point[1] / self.ratio * self.grid[0] + point[0] / self.ratio) as usize;
        self.ready
            .get(p)
            .copied()
            .unwrap_or(false)
            .then(|| self.rgb[p * 3..p * 3 + 3].try_into().unwrap())
    }
}

/// Developer probe through the actual asynchronous decode and preview workers.
pub fn probe(path: &Path, force_cpu: bool) -> anyhow::Result<serde_json::Value> {
    use crate::services::{Event, RenderJob, RenderRequest, Services};
    use sha2::{Digest, Sha256};
    let path = path.canonicalize()?;
    let services = Services::new(
        force_cpu,
        std::env::current_dir()?.join("target/nef-preview-probe-unused-thumbnails"),
    );
    let startup = Instant::now();
    let backend = loop {
        match services.events.recv_timeout(Duration::from_secs(60))? {
            Event::Backend(backend) => break backend,
            Event::Error(error) => anyhow::bail!("{error}"),
            event => services.discard(event),
        }
    };
    let startup_ms = startup.elapsed().as_secs_f64() * 1000.;
    let settings = Settings {
        viewport: [1280, 720],
        mode: Mode::Click,
        ..Default::default()
    };
    let started = Instant::now();
    services.nef_preview.request(Some(Request {
        generation: 1,
        pair: [(1, path.clone()), (1, path.clone())],
        settings: settings.clone(),
        ready: [None, None],
    }));
    services.decode(vec![(1, path.clone())], 1, vec![path.clone()]);
    let mut observations = Vec::new();
    let mut frame = None;
    let mut full_decode_ms = None;
    let ready_ms = loop {
        anyhow::ensure!(
            started.elapsed() < Duration::from_secs(60),
            "NEF preview probe timed out"
        );
        crossbeam_channel::select! {
            recv(services.nef_preview.events) -> event => {
                let snapshot = event?;
                if snapshot.generation == 1 && frame.is_none() {
                    observations.push(serde_json::json!({"ms":started.elapsed().as_secs_f64()*1000.,"covered_sampled_pixels":snapshot.covered_pixels}));
                }
                services.retire_images(std::iter::once(snapshot.primary).chain(snapshot.alternate).collect());
            },
            recv(services.events) -> event => match event? {
                Event::Decoded { id: 1, result } => {
                    let decoded = result.map_err(|error| anyhow::anyhow!(error))?;
                    full_decode_ms = Some(started.elapsed().as_secs_f64()*1000.);
                    services.nef_preview.request(None);
                    services.renders.put(RenderJob::Frame(RenderRequest { generation: 1, a: (1, decoded.clone()), b: (1, decoded.clone()), settings: settings.clone() }));
                    frame = Some(decoded);
                },
                Event::Rendered { primary, alternate, .. } => {
                    services.retire_images(std::iter::once(primary).chain(alternate).collect());
                    break started.elapsed().as_secs_f64()*1000.;
                },
                Event::Error(error) => anyhow::bail!("{error}"),
                event => services.discard(event),
            },
            default(Duration::from_secs(1)) => {},
        }
    };
    let frame = frame.ok_or_else(|| anyhow::anyhow!("Missing full NEF frame"))?;
    Ok(
        serde_json::json!({"success":true,"source":path,"backend":backend,"startup_ms":startup_ms,
        "first_preview_ms":observations.first().map(|row|row["ms"].clone()),"snapshots":observations,
        "full_decode_ms":full_decode_ms,"full_render_ready_ms":ready_ms,"dimensions":frame.size(),
        "full_rgb16_sha256":format!("{:x}",Sha256::digest(bytemuck::cast_slice(&frame.rgb))),
        "scope":"Actual asynchronous Services, six shared decode workers and two bounded preview workers, 1280x720 Single Fit. Timers exclude GPU startup, GPUI upload and screen presentation; source cache not flushed. Preview samples are exact 1:4 samples, never analysis frames."}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn update() -> Update {
        Update {
            photo: 7,
            path: PathBuf::from("test.nef"),
            cancelled: Arc::new(AtomicBool::new(false)),
            patch: Patch {
                image_size: [16, 16],
                origin: [1, 1],
                size: [2, 1],
                ratio: 4,
                rgb: vec![1000, 2000, 3000, 4000, 5000, 6000],
            },
        }
    }
    #[test]
    fn sampled_bands_are_bounded_masked_and_never_complete_frames() {
        let mut update = update();
        let mut image = Image::new(7, &update).unwrap();
        assert!(image.apply(&update.patch));
        assert_eq!(image.covered, 2);
        assert_eq!(image.sample([4, 4]), Some([1000, 2000, 3000]));
        assert_eq!(image.sample([7, 7]), Some([1000, 2000, 3000]));
        assert_eq!(image.sample([0, 0]), None);
        assert!(image.apply(&update.patch));
        assert_eq!(image.covered, 2);
        update.patch.origin = [4, 1];
        assert!(!image.apply(&update.patch));
        update.patch.origin = [1, 1];
        update.patch.rgb.pop();
        assert!(!image.apply(&update.patch));
        update.cancelled.store(true, Ordering::Release);
        assert_eq!(image.sample([4, 4]), None);
    }
    #[test]
    fn preview_composition_preserves_letterboxing_masks_and_rejects_diff_and_highpass() {
        let update = update();
        let mut image = Image::new(7, &update).unwrap();
        image.apply(&update.patch);
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .unwrap();
        let settings = Settings {
            viewport: [16, 16],
            ..Default::default()
        };
        let pixels = pixels(&Samples::Partial(&image), [16, 16], &settings, &pool);
        let p = (4 * 16 + 4) * 4;
        assert_eq!(&pixels[p..p + 4], &[11, 7, 3, 255]);
        assert_eq!(&pixels[..4], &[30, 30, 30, 255]);
        let mut sources = HashMap::new();
        sources.insert(update.path.clone(), image);
        let mut job = Request {
            generation: 1,
            pair: [(7, update.path.clone()), (7, update.path)],
            settings,
            ready: [None, None],
        };
        assert!(render(&job, &sources, &pool).is_some());
        job.settings.mode = Mode::Diff;
        assert!(render(&job, &sources, &pool).is_none());
        job.settings.mode = Mode::Click;
        job.settings.highpass = true;
        assert!(render(&job, &sources, &pool).is_none());
        job.settings.highpass = false;
        job.settings.viewport = [0, 16];
        assert!(render(&job, &sources, &pool).is_none());
    }
    #[test]
    fn requests_bound_the_mailbox_and_cancelled_or_other_sources_cannot_publish() {
        let worker = Worker::new();
        let publisher = worker.publisher();
        assert!(publisher.target(Path::new("test.nef")).is_none());
        let job = Request {
            generation: 9,
            pair: std::array::from_fn(|_| (7, PathBuf::from("test.nef"))),
            settings: Settings {
                viewport: [16, 16],
                ..Default::default()
            },
            ready: [None, None],
        };
        worker.request(Some(job));
        assert_eq!(
            publisher.target(Path::new("test.nef")),
            Some((7, [0.5, 0.5]))
        );
        let data = update();
        publisher.patch(data.photo, data.path, data.cancelled, data.patch);
        let snapshot = worker.events.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(snapshot.generation, 9);
        assert_eq!(snapshot.pair, [7, 7]);
        assert_eq!(snapshot.covered_pixels, [2, 2]);
        worker.request(None);
        assert!(publisher.target(Path::new("test.nef")).is_none());
        let data = update();
        publisher.patch(data.photo, data.path, data.cancelled, data.patch);
        assert!(worker
            .events
            .recv_timeout(Duration::from_millis(60))
            .is_err());
    }

    #[test]
    fn stale_photos_cancelled_flights_and_completed_sources_never_replace_current_samples() {
        let mut job = Request {
            generation: 1,
            pair: std::array::from_fn(|_| (7, PathBuf::from("test.nef"))),
            settings: Settings {
                viewport: [16, 16],
                ..Default::default()
            },
            ready: [None, None],
        };
        let mut sources = HashMap::new();
        let first = update();
        let old_flight = first.cancelled.clone();
        assert!(apply_update(&job, &mut sources, first));
        old_flight.store(true, Ordering::Release);
        let mut cancelled = update();
        cancelled.cancelled = old_flight;
        assert!(!apply_update(&job, &mut sources, cancelled));
        // Refresh changes the photo's revision token even when its path is equal.
        job.pair.iter_mut().for_each(|pair| pair.0 = 8);
        assert!(!apply_update(&job, &mut sources, update()));
        let mut fresh = update();
        fresh.photo = 8;
        fresh.patch.rgb.fill(50000);
        assert!(apply_update(&job, &mut sources, fresh));
        assert_eq!(
            sources[Path::new("test.nef")].sample([4, 4]),
            Some([50000; 3])
        );
        let full = Arc::new(Frame {
            width: 16,
            height: 16,
            rgb: vec![10; 16 * 16 * 3],
        });
        job.ready = [Some(full.clone()), Some(full)];
        let mut late = update();
        late.photo = 8;
        assert!(!apply_update(&job, &mut sources, late));
    }

    #[test]
    fn ready_a_partial_b_keep_independent_geometry_and_snapshot_revocation() {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .unwrap();
        let full = Arc::new(Frame {
            width: 8,
            height: 16,
            rgb: vec![40000; 8 * 16 * 3],
        });
        let job = Request {
            generation: 12,
            pair: [
                (3, PathBuf::from("ready.jpg")),
                (7, PathBuf::from("test.nef")),
            ],
            settings: Settings {
                viewport: [32, 16],
                mode: Mode::Slider,
                ..Default::default()
            },
            ready: [Some(full), None],
        };
        let mut sources = HashMap::new();
        let partial = update();
        let cancel = partial.cancelled.clone();
        assert!(apply_update(&job, &mut sources, partial));
        let snapshot = render(&job, &sources, &pool).unwrap();
        assert_eq!(snapshot.size, [8, 16]);
        assert_eq!(snapshot.pair, [3, 7]);
        assert_eq!(snapshot.covered_pixels, [0, 2]);
        assert!(snapshot.alternate.is_some());
        assert!(snapshot.is_valid());
        let a = Samples::Full(job.ready[0].as_ref().unwrap());
        let b = Samples::Partial(&sources[Path::new("test.nef")]);
        assert_eq!(a.sample([0.3, 0.3]), Some([40000; 3]));
        assert_eq!(b.sample([0.3, 0.3]), Some([1000, 2000, 3000]));
        let view = pixels(&a, [8, 16], &job.settings, &pool);
        assert_eq!(&view[..4], &[24, 24, 24, 255]);
        cancel.store(true, Ordering::Release);
        assert!(!snapshot.is_valid());
    }

    #[test]
    fn sampled_allocations_and_malformed_patches_have_hard_limits() {
        let mut data = update();
        data.patch.image_size = [100000, 100000];
        assert!(Image::new(7, &data).is_none());
        data.patch.image_size = [16, 16];
        let mut source = Image::new(7, &data).unwrap();
        data.patch.size = [u32::MAX, u32::MAX];
        assert!(!source.apply(&data.patch));
        data.patch.size = [2, 1];
        data.patch.origin = [u32::MAX, 0];
        assert!(!source.apply(&data.patch));
        assert_eq!(source.sample([u32::MAX, u32::MAX]), None);
        assert_eq!(source.covered, 0);
    }
}
enum Samples<'a> {
    Full(&'a Frame),
    Partial(&'a Image),
    Missing,
}
impl Samples<'_> {
    fn size(&self) -> Option<[u32; 2]> {
        match self {
            Self::Full(frame) => Some(frame.size()),
            Self::Partial(image) => Some(image.size),
            Self::Missing => None,
        }
    }
    fn sample(&self, normalized: [f32; 2]) -> Option<[u16; 3]> {
        let size = self.size()?;
        let p: [u32; 2] =
            std::array::from_fn(|i| ((normalized[i] * size[i] as f32) as u32).min(size[i] - 1));
        match self {
            Self::Full(frame) => {
                let index = (p[1] * frame.width + p[0]) as usize * 3;
                Some(frame.rgb[index..index + 3].try_into().unwrap())
            }
            Self::Partial(image) => image.sample(p),
            Self::Missing => None,
        }
    }
}
fn pixels(
    samples: &Samples<'_>,
    reference: [u32; 2],
    settings: &Settings,
    pool: &rayon::ThreadPool,
) -> Vec<u8> {
    let [w, h] = settings.viewport;
    let scale = settings.camera.scale(reference, settings.viewport);
    let mut pixels = vec![0; w as usize * h as usize * 4];
    pool.install(|| {
        pixels
            .par_chunks_mut(w as usize * 4)
            .enumerate()
            .for_each(|(y, row)| {
                for x in 0..w {
                    let point = [
                        settings.camera.center[0]
                            + (x as f32 + 0.5 - w as f32 * 0.5) / (scale * reference[0] as f32),
                        settings.camera.center[1]
                            + (y as f32 + 0.5 - h as f32 * 0.5) / (scale * reference[1] as f32),
                    ];
                    let inside = point.iter().all(|&p| (0. ..1.).contains(&p));
                    let rgb = inside.then(|| samples.sample(point)).flatten();
                    let index = x as usize * 4;
                    if let Some(rgb) = rgb {
                        for c in 0..3 {
                            row[index + c] = (u32::from(rgb[2 - c]) * 255 / 65535) as u8;
                        }
                    } else {
                        row[index..index + 3].fill(if inside {
                            30 + (((x / 8 + y as u32 / 8) & 1) * 8) as u8
                        } else {
                            24
                        });
                    }
                    row[index + 3] = 255;
                }
            })
    });
    pixels
}
fn render(
    job: &Request,
    sources: &HashMap<PathBuf, Image>,
    pool: &rayon::ThreadPool,
) -> Option<Snapshot> {
    if job.settings.highpass
        || job.settings.mode == Mode::Diff
        || job.settings.viewport.contains(&0)
        || u64::from(job.settings.viewport[0]) * u64::from(job.settings.viewport[1])
            > 16 * 1024 * 1024
    {
        return None;
    }
    let sample = |side: usize| {
        if let Some(frame) = &job.ready[side] {
            Samples::Full(frame)
        } else if let Some(image) = sources.get(&job.pair[side].1).filter(|image| {
            image.photo == job.pair[side].0 && !image.cancelled.load(Ordering::Acquire)
        }) {
            Samples::Partial(image)
        } else {
            Samples::Missing
        }
    };
    let a = sample(0);
    let b = sample(1);
    let size = a.size()?;
    let [w, h] = job.settings.viewport;
    let primary = render_image(w, h, pixels(&a, size, &job.settings, pool));
    let alternate = (job.pair[0].0 != job.pair[1].0)
        .then(|| render_image(w, h, pixels(&b, size, &job.settings, pool)));
    let covered = |samples: &Samples<'_>| match samples {
        Samples::Partial(image) => image.covered,
        _ => 0,
    };
    let flights = [&a, &b]
        .into_iter()
        .filter_map(|samples| match samples {
            Samples::Partial(image) => Some(image.cancelled.clone()),
            _ => None,
        })
        .collect();
    Some(Snapshot {
        generation: job.generation,
        pair: job.pair.each_ref().map(|(id, _)| *id),
        settings: job.settings.clone(),
        size,
        primary,
        alternate,
        covered_pixels: [covered(&a), covered(&b)],
        flights,
    })
}
fn run(
    requests: Arc<Mailbox<(u64, Option<Request>)>>,
    updates: Receiver<Update>,
    tx: Sender<Snapshot>,
    revision: Arc<AtomicU64>,
    closed: Arc<AtomicBool>,
) {
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(2)
        .thread_name(|i| format!("nef-preview-{i}"))
        .build()
        .expect("NEF preview render pool");
    let mut job: Option<Request> = None;
    let mut job_revision = 0;
    let mut sources = HashMap::<PathBuf, Image>::new();
    let mut dirty = false;
    let mut rendered = Instant::now() - Duration::from_secs(1);
    while !closed.load(Ordering::Acquire) {
        if let Some((request_revision, request)) = requests.try_take() {
            job_revision = request_revision;
            job = request;
            sources.retain(|path, image| {
                job.as_ref().is_some_and(|job| {
                    job.pair.iter().enumerate().any(|(side, (id, candidate))| {
                        candidate == path && *id == image.photo && job.ready[side].is_none()
                    }) && !image.cancelled.load(Ordering::Acquire)
                })
            });
            dirty = true;
        }
        let first = updates.recv_timeout(Duration::from_millis(4)).ok();
        for update in first.into_iter().chain(updates.try_iter()) {
            if let Some(job) = &job {
                dirty |= apply_update(job, &mut sources, update);
            }
        }
        if dirty && rendered.elapsed() >= Duration::from_millis(24) {
            if let Some(job) = &job {
                if let Some(snapshot) = render(job, &sources, &pool) {
                    if revision.load(Ordering::Acquire) == job_revision && snapshot.is_valid() {
                        let _ = tx.try_send(snapshot);
                    }
                    rendered = Instant::now();
                }
            }
            dirty = false;
        }
    }
}

fn apply_update(job: &Request, sources: &mut HashMap<PathBuf, Image>, update: Update) -> bool {
    if update.cancelled.load(Ordering::Acquire)
        || !job.pair.iter().enumerate().any(|(side, (id, path))| {
            *path == update.path && *id == update.photo && job.ready[side].is_none()
        })
    {
        return false;
    }
    if sources.get(&update.path).is_none_or(|source| {
        source.photo != update.photo || !Arc::ptr_eq(&source.cancelled, &update.cancelled)
    }) {
        let Some(mut source) = Image::new(update.photo, &update) else {
            return false;
        };
        if !source.apply(&update.patch) {
            return false;
        }
        sources.insert(update.path, source);
        return true;
    }
    sources.get_mut(&update.path).unwrap().apply(&update.patch)
}
