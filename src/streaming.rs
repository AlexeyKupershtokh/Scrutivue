//! Visible-first DNG loading. State, mappings, decoding, development, rendering,
//! source validation and large buffer destruction all stay on this worker.
use crate::{
    decode::Frame,
    dng::tiles::{Source, Tile},
    jxl,
    model::{Mode, PhotoId, Settings},
    services::render_image,
};
use crossbeam_channel::{bounded, unbounded, Receiver, Sender};
use gpui_kit::RenderImage;
use rayon::prelude::*;
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};
mod difference;
#[cfg(test)]
mod profile;
mod retention;
#[cfg(test)]
mod single_profile;
#[cfg(test)]
mod single_tests;

#[derive(Clone)]
pub struct Request {
    pub generation: u64,
    pub pair: [(PhotoId, PathBuf); 2],
    pub settings: Settings,
    pub ready: [Option<Arc<Frame>>; 2],
}
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct Progress {
    pub done: [usize; 2],
    pub total: [usize; 2],
    pub difference_pixels: usize,
}
pub enum Event {
    Snapshot {
        generation: u64,
        pair: [PhotoId; 2],
        settings: Settings,
        size: [u32; 2],
        primary: Arc<RenderImage>,
        alternate: Option<Arc<RenderImage>>,
        status: String,
        progress: Progress,
    },
    Complete {
        id: PhotoId,
        frame: Arc<Frame>,
    },
    Progress {
        generation: u64,
        status: String,
    },
    Unsupported {
        ids: Vec<PhotoId>,
        reason: String,
    },
}
pub struct Worker {
    sender: Sender<(u64, Command)>,
    revision: Arc<AtomicU64>,
    pub events: Receiver<Event>,
}
enum Command {
    Load(Request),
    Pause,
    Clear,
}
impl Worker {
    pub fn new() -> Self {
        let (sender, jobs) = unbounded();
        let (tx, events) = bounded(8);
        let revision = Arc::new(AtomicU64::new(0));
        let current = revision.clone();
        thread::Builder::new()
            .name("dng-visible-tiles".into())
            .spawn(move || run(jobs, tx, current))
            .expect("DNG tile worker");
        Self {
            sender,
            revision,
            events,
        }
    }
    pub fn request(&self, request: Request) {
        let revision = self.revision.fetch_add(1, Ordering::AcqRel) + 1;
        let _ = self.sender.send((revision, Command::Load(request)));
    }
    pub fn pause(&self) {
        let revision = self.revision.fetch_add(1, Ordering::AcqRel) + 1;
        let _ = self.sender.send((revision, Command::Pause));
    }
    pub fn clear(&self) {
        let revision = self.revision.fetch_add(1, Ordering::AcqRel) + 1;
        let _ = self.sender.send((revision, Command::Clear));
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.revision.fetch_add(1, Ordering::AcqRel);
    }
}
struct State {
    source: Source,
    full: Option<Arc<Frame>>,
    pending: Option<(usize, jxl::Session)>,
}
impl State {
    fn tile_bytes(&self) -> usize {
        self.source
            .tiles
            .iter()
            .flatten()
            .map(|tile| tile.raw.capacity() * std::mem::size_of::<u16>() + tile.display.bytes())
            .sum()
    }
    fn samples(&self) -> Samples<'_> {
        Samples {
            source: &self.source,
            full: self.full.as_deref(),
        }
    }
    fn step(
        &mut self,
        index: usize,
        library: Option<&Arc<jxl::Library>>,
        coarse: bool,
        cancelled: &(dyn Fn() -> bool + Sync),
    ) -> anyhow::Result<()> {
        anyhow::ensure!(!cancelled(), "Image decode cancelled");
        if self.pending.as_ref().is_some_and(|(i, _)| *i == index) {
            let (_, session) = self.pending.as_mut().unwrap();
            session.advance(1, &|| cancelled())?;
            let display = self
                .source
                .develop(&session.pixels, session.size, 1, cancelled)?;
            let (_, session) = self.pending.take().unwrap();
            self.source.tiles[index] = Some(Tile {
                raw: session.pixels.clone(),
                raw_size: session.size,
                display,
                ratio: 1,
            });
        } else if let Some(library) = library {
            // At most one live progressive decoder per selected source. No
            // hundreds-of-session memory amplification for a whole-image ladder.
            if self.pending.is_some() {
                return Ok(());
            }
            self.pending = Some((index, self.source.session(index, library.clone())?));
            let (_, session) = self.pending.as_mut().unwrap();
            let ratio = session.advance(if coarse { 8 } else { 1 }, &|| cancelled())? as u32;
            let display = self
                .source
                .develop(&session.pixels, session.size, ratio, cancelled)?;
            let tile = Tile {
                raw: if ratio == 1 {
                    session.pixels.clone()
                } else {
                    Vec::new()
                },
                raw_size: session.size,
                display,
                ratio,
            };
            self.source.tiles[index] = Some(tile);
            if session.complete {
                self.pending = None;
            }
        } else {
            let (raw, size) = self.source.decode_oxide(index)?;
            anyhow::ensure!(!cancelled(), "Image decode cancelled");
            let display = self.source.develop(&raw, size, 1, cancelled)?;
            self.source.tiles[index] = Some(Tile {
                raw,
                raw_size: size,
                display,
                ratio: 1,
            });
        }
        Ok(())
    }
}

/// Read-only render data deliberately excludes the exclusively owned, non-Sync
/// progressive decoder. Sharing pixels must never share a live decoder handle.
#[derive(Clone, Copy)]
struct Samples<'a> {
    source: &'a Source,
    full: Option<&'a Frame>,
}
impl Samples<'_> {
    fn sample_exact(&self, x: u32, y: u32) -> Option<[u16; 3]> {
        self.sample_with_policy(x, y, true)
    }
    fn has_exact_pixels(&self) -> bool {
        self.full.is_some()
            || self
                .source
                .tiles
                .iter()
                .flatten()
                .any(|tile| tile.ratio == 1)
    }
    fn rect_ready(&self, rect: [u32; 4]) -> bool {
        if self.full.is_some() {
            return true;
        }
        let [x0, y0, x1, y1] = rect;
        let points = [
            self.source.raw_point(x0, y0),
            self.source.raw_point(x1, y0),
            self.source.raw_point(x0, y1),
            self.source.raw_point(x1, y1),
        ];
        let lo: [u32; 2] = std::array::from_fn(|axis| {
            points.iter().map(|p| p[axis]).min().unwrap() / self.source.tile_size[axis]
        });
        let hi: [u32; 2] = std::array::from_fn(|axis| {
            points.iter().map(|p| p[axis]).max().unwrap() / self.source.tile_size[axis]
        });
        let nx = self.source.raw_size[0].div_ceil(self.source.tile_size[0]);
        (lo[1]..=hi[1]).all(|y| {
            (lo[0]..=hi[0]).all(|x| {
                self.source.tiles[(y * nx + x) as usize]
                    .as_ref()
                    .is_some_and(|tile| tile.ratio == 1)
            })
        })
    }
    fn sample(&self, x: u32, y: u32) -> Option<[u16; 3]> {
        self.sample_with_policy(x, y, false)
    }
    fn sample_with_policy(&self, x: u32, y: u32, exact: bool) -> Option<[u16; 3]> {
        if let Some(frame) = &self.full {
            let p = ((y * frame.width + x) * 3) as usize;
            return Some(frame.rgb[p..p + 3].try_into().unwrap());
        }
        let point = self.source.raw_point(x, y);
        let index = self.source.index_at(point);
        let tile = self.source.tiles[index].as_ref()?;
        if exact && tile.ratio != 1 {
            return None;
        }
        let origin = self.source.origin(index);
        let x = (point[0] - origin[0]) / tile.ratio;
        let y = (point[1] - origin[1]) / tile.ratio;
        let p = ((y.min(tile.display.height - 1) * tile.display.width
            + x.min(tile.display.width - 1))
            * 3) as usize;
        Some(tile.display.rgb[p..p + 3].try_into().unwrap())
    }
}

/// Conservative display-to-raw visible tile set, including interpolation context.
pub fn visible_tiles(source: &Source, settings: &Settings, reference: [u32; 2]) -> Vec<usize> {
    priority_tiles(source, settings, reference, None, true)
}

fn priority_tiles(
    source: &Source,
    settings: &Settings,
    reference: [u32; 2],
    side: Option<usize>,
    visible: bool,
) -> Vec<usize> {
    let scale = settings.camera.scale(reference, settings.viewport);
    let [vw, vh] = settings.viewport.map(|v| v as f32);
    let divider = settings.split.clamp(0., 1.) * vw;
    let slider = settings.mode == Mode::Slider && side.is_some();
    let clip = if slider {
        if side == Some(0) {
            [0., divider]
        } else {
            [divider, vw]
        }
    } else {
        [0., vw]
    };
    if visible && clip[0] >= clip[1] {
        return Vec::new();
    }
    // Diff mip interpolation may need the neighboring coarse sample, not just
    // the nearest raw pixel. Plan its entire footprint before idle work starts.
    let padding = if settings.mode == Mode::Diff {
        let factor = 2f32.powf((1. / scale).log2().max(0.).ceil());
        (factor * 2. + 1.) * scale
    } else {
        scale
    };
    let mut ranked = Vec::new();
    for index in 0..source.tiles.len() {
        let Some(rect) = source.display_bounds(index) else {
            continue;
        };
        let lo: [f32; 2] = std::array::from_fn(|a| {
            (rect[a] as f32 / source.size[a] as f32 - settings.camera.center[a])
                * reference[a] as f32
                * scale
                + settings.viewport[a] as f32 * 0.5
        });
        let hi: [f32; 2] = std::array::from_fn(|a| {
            lo[a] + rect[a + 2] as f32 / source.size[a] as f32 * reference[a] as f32 * scale
        });
        if visible
            && (hi[0] <= clip[0] - padding
                || lo[0] >= clip[1] + padding
                || hi[1] <= -padding
                || lo[1] >= vh + padding)
        {
            continue;
        }
        let center = [(lo[0] + hi[0]) * 0.5, (lo[1] + hi[1]) * 0.5];
        let (hidden, distance, vertical) = if slider {
            let left = side == Some(0);
            let hidden = if left {
                lo[0] >= divider
            } else {
                hi[0] <= divider
            };
            let dx = if left {
                (divider - hi[0]).max(0.)
            } else {
                (lo[0] - divider).max(0.)
            };
            (u8::from(hidden), dx, (center[1] - vh * 0.5).abs())
        } else {
            let dx = center[0] - vw * 0.5;
            let dy = center[1] - vh * 0.5;
            (0, dx * dx + dy * dy, dy.abs())
        };
        ranked.push((index, hidden, distance, vertical));
    }
    ranked.sort_by(|a, b| {
        a.1.cmp(&b.1)
            .then_with(|| a.2.total_cmp(&b.2))
            .then_with(|| a.3.total_cmp(&b.3))
            .then_with(|| a.0.cmp(&b.0))
    });
    let mut indices: Vec<_> = ranked.into_iter().map(|r| r.0).collect();
    // Physically present tiles outside ActiveArea still belong to exact full
    // reconstruction, but must never preempt any displayable tile.
    if !visible {
        indices.extend((0..source.tiles.len()).filter(|&i| source.display_bounds(i).is_none()));
    }
    indices
}
fn pixels(state: &State, reference: [u32; 2], settings: &Settings) -> Vec<u8> {
    pixels_from_samples(state.samples(), reference, settings, false)
}
fn pixels_from_samples(
    state: Samples<'_>,
    reference: [u32; 2],
    settings: &Settings,
    parallel: bool,
) -> Vec<u8> {
    let [w, h] = settings.viewport;
    let scale = settings.camera.scale(reference, settings.viewport);
    let mut pixels = vec![0; w as usize * h as usize * 4];
    if w == 0 || h == 0 {
        return pixels;
    }
    let row = |(y, row): (usize, &mut [u8])| {
        let y = y as u32;
        for x in 0..w {
            let sx = settings.camera.center[0] * reference[0] as f32
                + (x as f32 + 0.5 - w as f32 / 2.) / scale;
            let sy = settings.camera.center[1] * reference[1] as f32
                + (y as f32 + 0.5 - h as f32 / 2.) / scale;
            let p = x as usize * 4;
            row[p + 3] = 255;
            let inside =
                sx >= 0. && sy >= 0. && sx < reference[0] as f32 && sy < reference[1] as f32;
            let value = if inside {
                state.sample(
                    ((sx / reference[0] as f32 * state.source.size[0] as f32) as u32)
                        .min(state.source.size[0] - 1),
                    ((sy / reference[1] as f32 * state.source.size[1] as f32) as u32)
                        .min(state.source.size[1] - 1),
                )
            } else {
                None
            };
            if let Some(rgb) = value {
                row[p] = (rgb[2] as u32 * 255 / 65535) as u8;
                row[p + 1] = (rgb[1] as u32 * 255 / 65535) as u8;
                row[p + 2] = (rgb[0] as u32 * 255 / 65535) as u8;
            } else {
                row[p..p + 3].fill(if inside {
                    30
                } else if (x / 16 + y / 16) & 1 == 1 {
                    19
                } else {
                    22
                });
            }
        }
    };
    if parallel {
        pixels
            .par_chunks_mut(w as usize * 4)
            .enumerate()
            .for_each(row);
    } else {
        pixels.chunks_mut(w as usize * 4).enumerate().for_each(row);
    }
    pixels
}
fn publish(
    tx: &Sender<Event>,
    job: &Request,
    states: &HashMap<PhotoId, State>,
    difference: &mut difference::Preview,
    pool: &rayon::ThreadPool,
) {
    let ids = job.pair.each_ref().map(|p| p.0);
    let a = &states[&ids[0]];
    let b = &states[&ids[1]];
    let [w, h] = job.settings.viewport;
    let (primary, difference_pixels) = if job.settings.mode == Mode::Diff {
        let (a, b) = (a.samples(), b.samples());
        let (pixels, ready) = pool.install(|| difference.render_samples(ids, a, b, &job.settings));
        (render_image(w, h, pixels), ready)
    } else {
        difference.clear();
        let primary = if cfg!(feature = "experimental-single-parallel") && ids[0] == ids[1] {
            let samples = a.samples();
            let reference = a.source.size;
            // Only immutable pixels enter the bounded pool, never a live session.
            pool.install(|| pixels_from_samples(samples, reference, &job.settings, true))
        } else {
            pixels(a, a.source.size, &job.settings)
        };
        (render_image(w, h, primary), 0)
    };
    let alternate = if ids[0] == ids[1] || job.settings.mode == Mode::Diff {
        None
    } else {
        Some(render_image(w, h, pixels(b, a.source.size, &job.settings)))
    };
    let (progress, status) = progress_status(job, states, difference_pixels);
    let published = tx.try_send(Event::Snapshot {
        generation: job.generation,
        pair: ids,
        settings: job.settings.clone(),
        size: a.source.size,
        primary,
        alternate,
        status,
        progress,
    });
    // Qualification must not postpone the first visible tile. Warm its lazy
    // header check only AFTER publishing, not while opening either source.
    if published.is_ok() {
        for state in [a, b]
            .into_iter()
            .take(if ids[0] == ids[1] { 1 } else { 2 })
        {
            // A cached full frame needs no qualification. An empty partner must
            // get its first visible tile before a header-copy optimization runs.
            if state.full.is_none() && state.source.tiles.iter().any(Option::is_some) {
                state.source.prepare_completion_policy();
            }
        }
    }
}
fn progress_status(
    job: &Request,
    states: &HashMap<PhotoId, State>,
    difference_pixels: usize,
) -> (Progress, String) {
    let a = &states[&job.pair[0].0];
    let b = &states[&job.pair[1].0];
    let total = [a.source.tiles.len(), b.source.tiles.len()];
    let done = [a, b].map(|s| {
        if s.full.is_some() {
            s.source.tiles.len()
        } else {
            s.source
                .tiles
                .iter()
                .filter(|t| t.as_ref().is_some_and(|t| t.ratio == 1))
                .count()
        }
    });
    let progress = Progress {
        done,
        total,
        difference_pixels,
    };
    let status = format!(
        "DNG tiles A {}/{} · B {}/{} · {}preview; {}",
        done[0],
        total[0],
        done[1],
        total[1],
        if job.settings.mode == Mode::Diff {
            "paired-tile difference · "
        } else {
            ""
        },
        if done == total {
            "preparing full-resolution tools"
        } else {
            "exact tools wait for full development"
        }
    );
    (progress, status)
}
fn publish_progress(tx: &Sender<Event>, job: &Request, states: &HashMap<PhotoId, State>) {
    let (_, status) = progress_status(job, states, 0);
    let _ = tx.try_send(Event::Progress {
        generation: job.generation,
        status,
    });
}
fn batch(
    state: &mut State,
    indices: &[usize],
    pool: &rayon::ThreadPool,
    library: Option<&Arc<jxl::Library>>,
    cancelled: &(dyn Fn() -> bool + Sync),
) -> anyhow::Result<()> {
    let source = &state.source;
    let decoded: Vec<_> = pool.install(|| {
        indices
            .par_iter()
            .map(|&i| (i, source.decode_display(i, library, cancelled)))
            .collect()
    });
    commit_batch(state, decoded)
}
fn commit_batch(
    state: &mut State,
    decoded: Vec<(usize, anyhow::Result<Tile>)>,
) -> anyhow::Result<()> {
    let mut error = None;
    for (i, result) in decoded {
        match result {
            Ok(tile) => state.source.tiles[i] = Some(tile),
            Err(failure) if error.is_none() => error = Some(failure),
            Err(_) => {}
        }
    }
    error.map_or(Ok(()), Err)
}
#[derive(Debug)]
enum Work {
    Step { index: usize, coarse: bool },
    Batch(Vec<usize>),
}
#[derive(Debug)]
struct Planned {
    id: PhotoId,
    work: Work,
    visible: bool,
}
fn next_round(
    states: &HashMap<PhotoId, State>,
    job: &Request,
    idle: bool,
    threads: usize,
    native: bool,
) -> Vec<Planned> {
    let reference = states[&job.pair[0].0].source.size;
    let mut candidates = Vec::new();
    for side in 0..2 {
        let id = job.pair[side].0;
        if side == 1 && id == job.pair[0].0 {
            continue;
        }
        let state = &states[&id];
        if state.full.is_some() {
            continue;
        }
        let indices = priority_tiles(&state.source, &job.settings, reference, Some(side), true)
            .into_iter()
            .filter(|&i| state.source.tiles[i].as_ref().is_none_or(|t| t.ratio != 1))
            .collect::<Vec<_>>();
        if !indices.is_empty() {
            candidates.push((id, indices));
        }
    }
    // Neither source may fill offscreen tiles while the other still needs its
    // visible side. Otherwise each gets an independent bounded idle queue.
    let visible = !candidates.is_empty();
    if !visible && idle {
        for side in 0..2 {
            let id = job.pair[side].0;
            if side == 1 && id == job.pair[0].0 {
                continue;
            }
            let state = &states[&id];
            if state.full.is_some() {
                continue;
            }
            let indices =
                priority_tiles(&state.source, &job.settings, reference, Some(side), false)
                    .into_iter()
                    .filter(|&i| state.source.tiles[i].as_ref().is_none_or(|t| t.ratio != 1))
                    .collect::<Vec<_>>();
            if !indices.is_empty() {
                candidates.push((id, indices));
            }
        }
    }
    let quota = (threads / candidates.len().max(1)).max(1);
    candidates
        .into_iter()
        .map(|(id, indices)| {
            let state = &states[&id];
            // Once a first center/divider update is visible, amortize viewport
            // composition across a larger low-zoom batch. Thread count stays
            // bounded by the shared pool, not by this number of queued tiles.
            let quota = if job.settings.camera.scale(reference, job.settings.viewport) < 0.5
                && state.source.tiles.iter().any(Option::is_some)
            {
                quota * 4
            } else {
                quota
            };
            let coarse = native
                && job.settings.mode != Mode::Diff
                && job.settings.camera.scale(reference, job.settings.viewport) < 0.5
                && state.source.progressive_hint
                // Keep the first fast coarse update. Repeating it for every
                // single-image tile serializes decoding and regional development.
                // Paired Slider keeps its original scheduling policy.
                && (!cfg!(feature = "experimental-single-parallel")
                    || job.pair[0].0 != job.pair[1].0
                    || state.source.tiles.iter().all(Option::is_none))
                && state.source.tiles[indices[0]].is_none();
            let work = if let Some((index, _)) = &state.pending {
                if indices.contains(index) {
                    Work::Step {
                        index: *index,
                        coarse: false,
                    }
                } else {
                    Work::Batch(
                        indices
                            .into_iter()
                            .filter(|&i| state.source.tiles[i].is_none())
                            .take(quota)
                            .collect(),
                    )
                }
            } else if coarse {
                Work::Step {
                    index: indices[0],
                    coarse: true,
                }
            } else {
                Work::Batch(indices.into_iter().take(quota).collect())
            };
            Planned { id, work, visible }
        })
        .collect()
}
fn parallel_pair<A: Send, B: Send>(
    pool: &rayon::ThreadPool,
    a: impl FnOnce() -> A + Send,
    b: impl FnOnce() -> B + Send,
) -> (A, B) {
    pool.install(|| rayon::join(a, b))
}
fn execute_round(
    states: &mut HashMap<PhotoId, State>,
    plans: &[Planned],
    pool: &rayon::ThreadPool,
    library: Option<&Arc<jxl::Library>>,
    cancelled: &(dyn Fn() -> bool + Sync),
) -> anyhow::Result<()> {
    let advance = |state: &mut State, work: &Work| match work {
        Work::Step { index, coarse } => state.step(*index, library, *coarse, cancelled),
        Work::Batch(indices) => batch(state, indices, pool, library, cancelled),
    };
    match plans {
        [one] => {
            let state = states.get_mut(&one.id).unwrap();
            if cfg!(feature = "experimental-single-parallel") {
                // A retained progressive session must use this caller's pool,
                // not libjxl's separate CLI fallback registry.
                pool.install(|| advance(state, &one.work))
            } else {
                advance(state, &one.work)
            }
        }
        [a, b] => {
            let [a_state, b_state] = states.get_disjoint_mut([&a.id, &b.id]);
            let (a_result, b_result) = parallel_pair(
                pool,
                || advance(a_state.unwrap(), &a.work),
                || advance(b_state.unwrap(), &b.work),
            );
            a_result.and(b_result)
        }
        _ => Ok(()),
    }
}
fn finish_round(
    states: &mut HashMap<PhotoId, State>,
    ids: [PhotoId; 2],
    pool: &rayon::ThreadPool,
    cancelled: &(dyn Fn() -> bool + Sync),
) -> anyhow::Result<Vec<(PhotoId, Arc<Frame>)>> {
    let mut needed: Vec<_> = ids
        .into_iter()
        .filter(|id| states[id].full.is_none())
        .collect();
    needed.dedup();
    let finish = |state: &mut State| pool.install(|| state.source.finish(cancelled));
    let frames = match needed.as_slice() {
        [a, b] => {
            let [sa, sb] = states.get_disjoint_mut([a, b]);
            let (fa, fb) = parallel_pair(pool, || finish(sa.unwrap()), || finish(sb.unwrap()));
            vec![(*a, fa?), (*b, fb?)]
        }
        [a] => vec![(*a, finish(states.get_mut(a).unwrap())?)],
        _ => Vec::new(),
    };
    for (id, frame) in &frames {
        let state = states.get_mut(id).unwrap();
        state.full = Some(frame.clone());
        state.source.tiles.iter_mut().for_each(|t| *t = None);
    }
    Ok(frames)
}
fn worker_threads(logical: usize, single: bool) -> usize {
    let paired = logical.div_ceil(2).clamp(2, 6);
    if cfg!(feature = "experimental-single-parallel") && single {
        // Extra SMT workers help independent tile development. Leave two logical
        // CPUs of scheduling headroom; paired decoding keeps its original budget.
        logical.saturating_sub(2).clamp(2, 12).max(paired)
    } else {
        paired
    }
}
fn run(jobs: Receiver<(u64, Command)>, tx: Sender<Event>, revision: Arc<AtomicU64>) {
    let library = jxl::Library::installed().ok();
    let logical = thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(2);
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(worker_threads(logical, false))
        .thread_name(|i| format!("dng-tiles-{i}"))
        .build()
        .expect("DNG tile pool");
    #[cfg(feature = "experimental-single-parallel")]
    let mut single_pool = None;
    let mut states = HashMap::<PhotoId, State>::new();
    let mut retention = retention::Retention::default();
    let mut difference = difference::Preview::default();
    let mut queued = None;
    'requests: loop {
        let (token, job) = match queued.take().or_else(|| jobs.recv().ok()) {
            Some(command) => command,
            None => return,
        };
        // Skip superseded loads, but never consume/discard a Clear barrier.
        if matches!(&job, Command::Load(_)) && revision.load(Ordering::Acquire) != token {
            continue;
        }
        let job = match job {
            Command::Load(job) => job,
            Command::Pause => {
                retention.trim(&mut states, &[]);
                difference.clear();
                continue;
            }
            Command::Clear => {
                states.clear();
                retention.clear();
                difference.clear();
                continue;
            }
        };
        let selected: Vec<_> = job.pair.iter().map(|p| p.0).collect();
        #[cfg(feature = "experimental-single-parallel")]
        let pool = if selected[0] == selected[1]
            && worker_threads(logical, true) != pool.current_num_threads()
        {
            single_pool.get_or_insert_with(|| {
                rayon::ThreadPoolBuilder::new()
                    .num_threads(worker_threads(logical, true))
                    .thread_name(|i| format!("dng-single-{i}"))
                    .build()
                    .expect("DNG single-source pool")
            }) as &rayon::ThreadPool
        } else {
            &pool
        };
        #[cfg(not(feature = "experimental-single-parallel"))]
        let pool = &pool;
        // Only one registry runs at a time, on this coordinator. Returning to a
        // pair never inherits the single-source runner's larger worker count.
        let threads = pool.current_num_threads();
        for (id, path) in &job.pair {
            if states
                .get(id)
                .is_some_and(|state| !state.source.matches(path))
            {
                states.remove(id);
                difference.clear();
            }
        }
        retention.trim(&mut states, &selected);
        for (side, (id, path)) in job.pair.iter().enumerate() {
            if !states.contains_key(id) {
                match Source::open(path) {
                    Ok(source) => {
                        states.insert(
                            *id,
                            State {
                                source,
                                full: None,
                                pending: None,
                            },
                        );
                    }
                    Err(error) => {
                        let _ = tx.send(Event::Unsupported {
                            ids: selected.clone(),
                            reason: format!("Full DNG fallback: {error:#}"),
                        });
                        continue 'requests;
                    }
                }
            }
            if let Some(frame) = &job.ready[side] {
                let state = states.get_mut(id).unwrap();
                if frame.size() == state.source.size && state.full.is_none() {
                    state.full = Some(frame.clone());
                    state.pending = None;
                    state.source.tiles.iter_mut().for_each(|t| *t = None);
                }
            }
        }
        retention.touch(&selected);
        let start = Instant::now();
        let mut published = Instant::now() - Duration::from_secs(1);
        let mut view_dirty = true;
        let cancelled = || revision.load(Ordering::Acquire) != token;
        // A resumed viewport must show cached tiles before starting another
        // decode batch, rather than hiding the cache behind newly missing work.
        if !cancelled()
            && selected.iter().any(|id| {
                states[id].full.is_some() || states[id].source.tiles.iter().any(Option::is_some)
            })
        {
            publish(&tx, &job, &states, &mut difference, pool);
            view_dirty = false;
            published = Instant::now();
        }
        loop {
            if cancelled() {
                continue 'requests;
            }
            let idle = start.elapsed() >= Duration::from_millis(250);
            let plans = next_round(&states, &job, idle, threads, library.is_some());
            if !plans.is_empty() {
                let result = execute_round(&mut states, &plans, pool, library.as_ref(), &cancelled);
                if let Err(error) = result {
                    if cancelled() {
                        continue 'requests;
                    }
                    let _ = tx.send(Event::Unsupported {
                        ids: selected.clone(),
                        reason: format!("Full DNG fallback: {error:#}"),
                    });
                    states.retain(|id, _| !selected.contains(id));
                    difference.clear();
                    continue 'requests;
                }
                view_dirty |= plans.iter().any(|p| p.visible);
                if published.elapsed() >= Duration::from_millis(40) {
                    if view_dirty {
                        publish(&tx, &job, &states, &mut difference, pool);
                        view_dirty = false;
                    } else {
                        publish_progress(&tx, &job, &states);
                    }
                    published = Instant::now();
                }
                continue;
            }
            if idle {
                // Publish the last paired-tile view before full LibRaw work.
                if view_dirty {
                    publish(&tx, &job, &states, &mut difference, pool);
                } else {
                    publish_progress(&tx, &job, &states);
                }
                match finish_round(
                    &mut states,
                    job.pair.each_ref().map(|p| p.0),
                    pool,
                    &cancelled,
                ) {
                    Ok(frames) => {
                        for (id, frame) in frames {
                            if tx.send(Event::Complete { id, frame }).is_err() {
                                return;
                            }
                        }
                    }
                    Err(error) => {
                        if cancelled() {
                            continue 'requests;
                        }
                        let _ = tx.send(Event::Unsupported {
                            ids: selected.clone(),
                            reason: format!("Full DNG fallback: {error:#}"),
                        });
                        states.retain(|id, _| !selected.contains(id));
                        difference.clear();
                    }
                }
                // Exact frames now go through the normal GPU/filter/statistics path.
                continue 'requests;
            }
            if view_dirty {
                publish(&tx, &job, &states, &mut difference, pool);
                view_dirty = false;
                published = Instant::now();
            }
            match jobs.recv_timeout(Duration::from_millis(25)) {
                Ok(next) => {
                    queued = Some(next);
                    continue 'requests;
                }
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => return,
                Err(_) => {}
            }
        }
    }
}

/// Headless production-path validation. Reference decoding and comparisons are
/// outside ready timers. No native runtime is required for exact tile fallback.
pub fn probe(path: &std::path::Path) -> anyhow::Result<serde_json::Value> {
    let start = Instant::now();
    let library = jxl::Library::installed().ok();
    let pool = rayon::ThreadPoolBuilder::new().num_threads(6).build()?;
    let source = Source::open(path)?;
    let size = source.size;
    let count = source.tiles.len();
    let mut state = State {
        source,
        full: None,
        pending: None,
    };
    let mut settings = Settings::default();
    settings.viewport = [512, 512];
    settings.camera.zoom = crate::model::Zoom::Fixed(1.);
    let mut views = Vec::new();
    let mut decoded = 0;
    for center in [[0.5, 0.5], [0.52, 0.5], [0.5, 0.5]] {
        settings.camera.center = center;
        settings.camera.clamp(size, settings.viewport);
        let visible = visible_tiles(&state.source, &settings, size);
        let before = decoded;
        let timer = Instant::now();
        let needed: Vec<_> = visible
            .iter()
            .copied()
            .filter(|&index| state.source.tiles[index].is_none())
            .collect();
        for indices in needed.chunks(6) {
            batch(&mut state, indices, &pool, library.as_ref(), &|| false)?;
            decoded += indices.len();
        }
        views.push(serde_json::json!({"center":center,"visible_tiles":visible.len(),"new_tile_decodes":decoded-before,"ready_ms":timer.elapsed().as_secs_f64()*1000.}));
        state.source.prepare_completion_policy();
    }
    let initial_ms = start.elapsed().as_secs_f64() * 1000.;
    let mut stages = Vec::new();
    if let Some(index) = state.source.tiles.iter().position(Option::is_none) {
        let timer = Instant::now();
        state.step(index, library.as_ref(), true, &|| false)?;
        let ratio = state.source.tiles[index].as_ref().unwrap().ratio;
        stages.push(
            serde_json::json!({"ratio":ratio,"ready_ms":timer.elapsed().as_secs_f64()*1000.}),
        );
        if state.pending.is_some() {
            let timer = Instant::now();
            state.step(index, library.as_ref(), false, &|| false)?;
            stages.push(serde_json::json!({"ratio":1,"additional_ms":timer.elapsed().as_secs_f64()*1000.,"same_decoder":true}));
        }
        decoded += 1;
    }
    let missing: Vec<_> = (0..count)
        .filter(|&index| state.source.tiles[index].is_none())
        .collect();
    for indices in missing.chunks(6) {
        batch(&mut state, indices, &pool, library.as_ref(), &|| false)?;
        decoded += indices.len();
    }
    let ready_tiles_ms = start.elapsed().as_secs_f64() * 1000.;
    let (frame, completion) = pool.install(|| state.source.finish_profiled(&|| false))?;
    let full_ready_ms = start.elapsed().as_secs_f64() * 1000.;
    let baseline = crate::decode::decode(path)?;
    let legacy = pool.install(|| state.source.finish_reference(&|| false))?;
    let legacy_differences = frame
        .rgb
        .iter()
        .zip(&legacy.rgb)
        .filter(|(a, b)| a != b)
        .count();
    anyhow::ensure!(
        frame.size() == legacy.size() && legacy_differences == 0,
        "Tile completion differs from independent legacy full LibRaw: {legacy_differences} samples"
    );
    let differing = frame
        .rgb
        .iter()
        .zip(&baseline.rgb)
        .filter(|(a, b)| a != b)
        .count();
    let maximum = frame
        .rgb
        .iter()
        .zip(&baseline.rgb)
        .map(|(a, b)| a.abs_diff(*b))
        .max()
        .unwrap_or(0);
    anyhow::ensure!(frame.size()==baseline.size()&&differing==0,"Cached-tile full completion differs from fresh production decode: {differing} samples, max {maximum}");
    // Also expose regional development policy differences instead of silently
    // advertising provisional camera RGB as exact picker/difference data.
    let mut tile_samples = 0usize;
    let mut differences = 0usize;
    let mut maximum = 0u16;
    for y in 0..size[1] {
        for x in 0..size[0] {
            if (x + y * size[0]) % 97 != 0 {
                continue;
            }
            let pixel = state.samples().sample(x, y).unwrap();
            let p = ((y * size[0] + x) * 3) as usize;
            for c in 0..3 {
                let error = pixel[c].abs_diff(frame.rgb[p + c]);
                tile_samples += 1;
                differences += usize::from(error != 0);
                maximum = maximum.max(error);
            }
        }
    }
    let mut hasher = sha2::Sha256::new();
    use sha2::Digest;
    for sample in &frame.rgb {
        hasher.update(sample.to_le_bytes());
    }
    Ok(
        serde_json::json!({"success":true,"source":path,"native_runtime":library.is_some(),"size":size,"total_tiles":count,
        "views":views,"three_views_ms":initial_ms,"optional_progressive_stage":stages,
        "total_entropy_tile_decodes":decoded,"all_tiles_ready_ms":ready_tiles_ms,"full_developed_ready_ms":full_ready_ms,
        "matches_fresh_full_decode":true,"matches_legacy_full_development":true,"legacy_compared_samples":frame.rgb.len(),
        "legacy_differing_samples":legacy_differences,"completion":completion,"regional_spot_samples":tile_samples,"regional_spot_differences":differences,
        "regional_maximum_absolute_error":maximum,"full_sha256":format!("{:x}",hasher.finalize()),"source_unchanged":state.source.unchanged()}),
    )
}

/// Selection-return validation through the same asynchronous worker as the UI.
pub fn probe_resume(paths: [PathBuf; 2]) -> anyhow::Result<serde_json::Value> {
    use anyhow::{ensure, Context};
    let sources = [Source::open(&paths[0])?, Source::open(&paths[1])?];
    let budget = worker_threads(
        thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(2),
        true,
    );
    ensure!(
        sources.iter().all(|s| s.tiles.len() > budget),
        "Use sources with more tiles than the worker budget for partial-return validation"
    );
    let settings = Settings {
        viewport: [128, 128],
        mode: Mode::Click,
        ..Settings::default()
    };
    let worker = Worker::new();
    let request = |generation: u64, side: usize| Request {
        generation,
        pair: [
            (side as PhotoId + 1, paths[side].clone()),
            (side as PhotoId + 1, paths[side].clone()),
        ],
        settings: settings.clone(),
        ready: [None, None],
    };
    let snapshot = |generation: u64, minimum: usize| -> anyhow::Result<(Progress, f64)> {
        let started = Instant::now();
        loop {
            ensure!(
                started.elapsed() < Duration::from_secs(60),
                "Selection-return probe timed out"
            );
            match worker
                .events
                .recv_timeout(Duration::from_secs(30))
                .context("Waiting for selection-return snapshot")?
            {
                Event::Snapshot {
                    generation: current,
                    progress,
                    ..
                } if current == generation && progress.done[0] >= minimum => {
                    ensure!(progress.done[0] < progress.total[0], "Source completed before partial-return validation; use a larger tiled source");
                    return Ok((progress, started.elapsed().as_secs_f64() * 1000.));
                }
                Event::Unsupported { reason, .. } => anyhow::bail!("{reason}"),
                Event::Complete { .. } => {
                    anyhow::bail!("Source completed before partial-return validation")
                }
                _ => {}
            }
        }
    };
    worker.request(request(1, 0));
    let (initial, _) = snapshot(1, 7)?;
    worker.request(request(2, 1));
    let (other, _) = snapshot(2, 1)?;
    worker.request(request(3, 0));
    let (returned, return_ms) = snapshot(3, 0)?;
    ensure!(
        returned.done[0] >= initial.done[0],
        "Partial source tiles were discarded after changing selection"
    );
    worker.pause();
    worker.request(request(4, 0));
    let (resumed, resume_ms) = snapshot(4, 0)?;
    ensure!(
        resumed.done[0] >= returned.done[0],
        "Pause discarded partial source tiles"
    );
    worker.clear();
    worker.request(request(5, 1));
    worker.request(request(6, 0));
    let (cleared, _) = snapshot(6, 0)?;
    ensure!(
        cleared.done[0] <= budget,
        "Explicit clear did not discard retained tiles"
    );
    ensure!(
        sources.iter().all(Source::unchanged),
        "Source changed during selection-return probe"
    );
    Ok(
        serde_json::json!({"success":true,"sources":paths,"native_runtime":jxl::Library::installed().is_ok(),
        "initial_partial":initial,"other_partial":other,"first_return_snapshot":returned,
        "first_pause_resume_snapshot":resumed,"first_after_clear_snapshot":cleared,
        "return_snapshot_ms":return_ms,"pause_resume_snapshot_ms":resume_ms,"sources_unchanged":true,
        "note":"Actual asynchronous worker; first snapshots prove retained partial progress before new decode work. Not GUI presentation latency."}),
    )
}

/// Exercise the actual asynchronous worker, not only its scheduling helpers.
pub fn probe_pair(
    paths: [PathBuf; 2],
    mode: Mode,
    fit: bool,
    output: Option<&std::path::Path>,
) -> anyhow::Result<serde_json::Value> {
    use anyhow::{ensure, Context};
    ensure!(
        matches!(mode, Mode::Slider | Mode::Diff),
        "Expected Slider or Diff"
    );
    if let Some(output) = output {
        ensure!(!output.exists(), "Use a new probe output directory");
        std::fs::create_dir_all(output)?;
    }
    let settings = Settings {
        mode,
        viewport: [512, 512],
        camera: crate::model::Camera {
            zoom: if fit {
                crate::model::Zoom::Fit
            } else {
                crate::model::Zoom::Fixed(1.)
            },
            ..Default::default()
        },
        ..Settings::default()
    };
    let worker = Worker::new();
    let started = Instant::now();
    worker.request(Request {
        generation: 1,
        pair: [(1, paths[0].clone()), (2, paths[1].clone())],
        settings: settings.clone(),
        ready: [None, None],
    });
    let mut frames: [Option<Arc<Frame>>; 2] = [None, None];
    let mut observations = Vec::new();
    let mut paired_preview = None;
    let mut both_progress_ms = None;
    let mut complete_times = [0.; 2];
    let mut saved_initial = false;
    let save = |name: &str, bgra: &[u8]| -> anyhow::Result<()> {
        if let Some(output) = output {
            let mut rgba = bgra.to_vec();
            for pixel in rgba.chunks_exact_mut(4) {
                pixel.swap(0, 2);
            }
            image::RgbaImage::from_raw(512, 512, rgba)
                .context("Probe image extent")?
                .save(output.join(name))?;
        }
        Ok(())
    };
    while frames.iter().any(Option::is_none) {
        ensure!(
            started.elapsed() < Duration::from_secs(120),
            "Paired tile worker timed out"
        );
        match worker
            .events
            .recv_timeout(Duration::from_secs(30))
            .context("Waiting for paired tiles")?
        {
            Event::Snapshot {
                generation: 1,
                primary,
                alternate,
                progress,
                size,
                ..
            } => {
                let elapsed = started.elapsed().as_secs_f64() * 1000.;
                if progress.done.iter().all(|&n| n > 0) && both_progress_ms.is_none() {
                    both_progress_ms = Some(elapsed);
                }
                observations.push(serde_json::json!({"ready_ms":elapsed,"progress":progress}));
                let mut pixels = primary
                    .as_bytes(0)
                    .context("Missing preview image")?
                    .to_vec();
                if mode == Mode::Slider {
                    let b = alternate
                        .as_ref()
                        .context("Missing Slider B preview")?
                        .as_bytes(0)
                        .context("Missing B pixels")?;
                    for y in 0..512 {
                        for x in 256..512 {
                            let p = (y * 512 + x) * 4;
                            pixels[p..p + 4].copy_from_slice(&b[p..p + 4]);
                        }
                    }
                }
                if !saved_initial {
                    save("initial-preview.png", &pixels)?;
                    saved_initial = true;
                }
                if mode == Mode::Diff && paired_preview.is_none() {
                    let scale = settings.camera.scale(size, settings.viewport);
                    let inside: usize = (0..512)
                        .map(|y| {
                            (0..512)
                                .filter(|&x| {
                                    let sx = (x as f32 + 0.5 - 256.) / scale
                                        + settings.camera.center[0] * size[0] as f32;
                                    let sy = (y as f32 + 0.5 - 256.) / scale
                                        + settings.camera.center[1] * size[1] as f32;
                                    sx >= 0.
                                        && sy >= 0.
                                        && sx < size[0] as f32
                                        && sy < size[1] as f32
                                })
                                .count()
                        })
                        .sum();
                    if inside > 0 && progress.difference_pixels == inside {
                        save("paired-visible-difference.png", &pixels)?;
                        paired_preview = Some(pixels);
                    }
                }
            }
            Event::Complete { id, frame } => {
                let side = (id - 1) as usize;
                ensure!(side < 2, "Unexpected paired completion");
                complete_times[side] = started.elapsed().as_secs_f64() * 1000.;
                frames[side] = Some(frame);
            }
            Event::Unsupported { reason, .. } => anyhow::bail!(reason),
            Event::Snapshot { .. } => {}
            Event::Progress { .. } => {}
        }
    }
    worker.clear();
    ensure!(
        both_progress_ms.is_some(),
        "Neither paired preview showed concurrent A/B progress"
    );
    for side in 0..2 {
        let reference = crate::decode::decode_dng_reference(&paths[side])?;
        let frame = frames[side].as_ref().unwrap();
        ensure!(
            frame.size() == reference.size() && frame.rgb == reference.rgb,
            "Paired full completion differs for side {side}"
        );
    }
    let mut difference_error = None;
    if mode == Mode::Diff {
        let pixels =
            paired_preview.context("No fully paired visible difference before completion")?;
        let expected = crate::processing::Cpu::new().render(
            (1, frames[0].as_ref().unwrap()),
            (2, frames[1].as_ref().unwrap()),
            &settings,
            false,
        );
        let maximum = pixels
            .iter()
            .zip(&expected)
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap_or(0);
        ensure!(
            maximum <= 1,
            "Paired preview differs from full CPU difference by {maximum}/255"
        );
        save("full-reference-difference.png", &expected)?;
        difference_error = Some(maximum);
    }
    Ok(
        serde_json::json!({"success":true,"sources":paths,"mode":mode,"fit":fit,"viewport":settings.viewport,
        "both_sides_progress_before_completion":true,"first_both_sides_progress_ms":both_progress_ms,
        "full_developed_ready_ms":complete_times,"full_frames_match_fresh_decode":true,
        "paired_preview_maximum_error_u8":difference_error,"snapshots":observations,
        "note":"Actual asynchronous tile worker; paired preview comparison permits one 8-bit rounding unit. Not GPU presentation latency."}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    pub(super) fn image_state(
        name: &str,
        size: [u32; 2],
        pixel: impl Fn(u32, u32) -> [u16; 3],
    ) -> (State, Arc<Frame>) {
        let path = crate::dng::tiles::fixture(name, size[0], size[1]);
        let mut source = Source::open(&path).unwrap();
        let rgb = (0..size[1])
            .flat_map(|y| {
                (0..size[0]).flat_map({
                    let pixel = &pixel;
                    move |x| pixel(x, y)
                })
            })
            .collect();
        let frame = Arc::new(Frame {
            width: size[0],
            height: size[1],
            rgb,
        });
        for index in 0..source.tiles.len() {
            let origin = source.origin(index);
            let [w, h] = source.tile_size;
            let mut rgb = Vec::new();
            for y in 0..h {
                for x in 0..w {
                    let p = (((origin[1] + y).min(size[1] - 1) * size[0]
                        + (origin[0] + x).min(size[0] - 1))
                        * 3) as usize;
                    rgb.extend_from_slice(&frame.rgb[p..p + 3]);
                }
            }
            source.tiles[index] = Some(Tile {
                raw: Vec::new(),
                raw_size: [w, h],
                display: Arc::new(Frame {
                    width: w,
                    height: h,
                    rgb,
                }),
                ratio: 1,
            });
        }
        (
            State {
                source,
                full: None,
                pending: None,
            },
            frame,
        )
    }
    pub(super) fn empty_pair(name: &str, settings: Settings) -> (HashMap<PhotoId, State>, Request) {
        let path = crate::dng::tiles::fixture(name, 256, 128);
        let states = [1, 2]
            .into_iter()
            .map(|id| {
                (
                    id,
                    State {
                        source: Source::open(&path).unwrap(),
                        full: None,
                        pending: None,
                    },
                )
            })
            .collect();
        (
            states,
            Request {
                generation: 1,
                pair: [(1, path.clone()), (2, path)],
                settings,
                ready: [None, None],
            },
        )
    }
    fn first_indices(work: &Work) -> Vec<usize> {
        match work {
            Work::Step { index, .. } => vec![*index],
            Work::Batch(indices) => indices.clone(),
        }
    }
    fn selection_request(generation: u64, id: PhotoId, path: &std::path::Path) -> Request {
        Request {
            generation,
            pair: [(id, path.to_owned()), (id, path.to_owned())],
            settings: Settings {
                viewport: [64, 64],
                camera: crate::model::Camera {
                    zoom: crate::model::Zoom::Fixed(1.),
                    ..Default::default()
                },
                ..Settings::default()
            },
            ready: [None, None],
        }
    }
    fn selection_snapshot(worker: &Worker, generation: u64, minimum: usize) -> Progress {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            match worker
                .events
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("Partial tile snapshot")
            {
                Event::Snapshot {
                    generation: current,
                    progress,
                    ..
                } if current == generation && progress.done[0] >= minimum => {
                    assert!(
                        progress.done[0] < progress.total[0],
                        "Selection must remain incomplete"
                    );
                    return progress;
                }
                Event::Unsupported { reason, .. } => panic!("{reason}"),
                Event::Complete { .. } => panic!("Source completed before selection-return test"),
                _ => {}
            }
        }
    }
    #[test]
    fn production_worker_reuses_partial_selections_and_pause_but_honors_clear_barriers() {
        let a = crate::dng::tiles::fixture("worker-return-a", 1024, 512);
        let b = crate::dng::tiles::fixture("worker-return-b", 1024, 512);
        let worker = Worker::new();
        worker.request(selection_request(1, 1, &a));
        let before = selection_snapshot(&worker, 1, 12);
        worker.request(selection_request(2, 2, &b));
        selection_snapshot(&worker, 2, 1);
        worker.request(selection_request(3, 1, &a));
        let returned = selection_snapshot(&worker, 3, 1);
        assert!(returned.done[0] >= before.done[0]);
        worker.pause();
        worker.request(selection_request(4, 1, &a));
        let resumed = selection_snapshot(&worker, 4, 1);
        assert!(resumed.done[0] >= returned.done[0]);
        // A superseded load after Clear must not cause the barrier to be drained.
        worker.clear();
        worker.request(selection_request(5, 2, &b));
        worker.request(selection_request(6, 1, &a));
        let cleared = selection_snapshot(&worker, 6, 1);
        assert!(
            cleared.done[0]
                <= worker_threads(
                    thread::available_parallelism()
                        .map(usize::from)
                        .unwrap_or(2),
                    true
                ),
            "Explicit Clear must discard retained tiles"
        );
    }
    #[test]
    fn production_worker_rejects_retained_tiles_for_a_different_path_or_revision() {
        let a = crate::dng::tiles::fixture("worker-path-a", 1024, 512);
        let b = crate::dng::tiles::fixture("worker-path-b", 1024, 512);
        let worker = Worker::new();
        worker.request(selection_request(1, 1, &a));
        selection_snapshot(&worker, 1, 12);
        worker.request(selection_request(2, 1, &b));
        let budget = worker_threads(
            thread::available_parallelism()
                .map(usize::from)
                .unwrap_or(2),
            true,
        );
        assert!(selection_snapshot(&worker, 2, 1).done[0] <= budget);
        selection_snapshot(&worker, 2, 12);
        std::fs::OpenOptions::new()
            .write(true)
            .open(&b)
            .unwrap()
            .set_modified(std::time::SystemTime::now() + Duration::from_secs(2))
            .unwrap();
        worker.request(selection_request(3, 1, &b));
        assert!(selection_snapshot(&worker, 3, 1).done[0] <= budget);
    }
    #[test]
    fn successful_batch_tiles_survive_other_cancelled_or_failed_results() {
        let (mut state, _) = image_state("partial-batch-results", [64, 48], |_, _| [1234; 3]);
        let first = state.source.tiles[0].take().unwrap();
        let last = state.source.tiles[2].take().unwrap();
        let display = last.display.clone();
        state.source.tiles.iter_mut().for_each(|slot| *slot = None);
        let result = commit_batch(
            &mut state,
            vec![
                (0, Ok(first)),
                (1, Err(anyhow::anyhow!("Image decode cancelled"))),
                (2, Ok(last)),
                (3, Err(anyhow::anyhow!("Another failure"))),
            ],
        );
        assert_eq!(result.unwrap_err().to_string(), "Image decode cancelled");
        assert!(state.source.tiles[0].is_some() && state.source.tiles[1].is_none());
        assert!(Arc::ptr_eq(
            &display,
            &state.source.tiles[2].as_ref().unwrap().display
        ));
    }
    #[test]
    fn streaming_checkerboard_distinguishes_outside_bounds_from_missing_interior_tiles() {
        let (mut state, _) = image_state("stream-checkerboard", [64, 48], |_, _| [65535, 0, 0]);
        state.source.tiles[0] = None;
        let settings = Settings {
            viewport: [128, 128],
            camera: crate::model::Camera {
                zoom: crate::model::Zoom::Fixed(1.),
                ..Default::default()
            },
            ..Settings::default()
        };
        for mode in [Mode::Click, Mode::Slider] {
            let settings = Settings {
                mode,
                ..settings.clone()
            };
            let output = pixels(&state, [64, 48], &settings);
            let at = |x: usize, y: usize| &output[(y * 128 + x) * 4..(y * 128 + x + 1) * 4];
            assert_eq!(at(0, 0), [22, 22, 22, 255]);
            assert_eq!(at(16, 0), [19, 19, 19, 255]);
            assert_eq!(at(32, 40), [30, 30, 30, 255]);
            assert_eq!(at(48, 40), [0, 0, 255, 255]);
        }
    }
    #[test]
    fn retained_coarse_tiles_are_refined_without_another_coarse_decode() {
        let settings = Settings {
            mode: Mode::Click,
            viewport: [32, 16],
            ..Settings::default()
        };
        let (mut states, job) = empty_pair("retained-coarse-refine", settings);
        let state = states.get_mut(&1).unwrap();
        state.source.progressive_hint = true;
        let index = first_indices(&next_round(&states, &job, false, 6, true)[0].work)[0];
        states.get_mut(&1).unwrap().source.tiles[index] = Some(Tile {
            raw: Vec::new(),
            raw_size: [16, 16],
            display: Arc::new(Frame {
                width: 2,
                height: 2,
                rgb: vec![123; 12],
            }),
            ratio: 8,
        });
        let plans = next_round(&states, &job, false, 6, true);
        assert!(matches!(&plans[0].work, Work::Batch(indices) if indices.contains(&index)));
    }
    #[test]
    fn loading_status_reports_progress_without_decoder_details() {
        for mode in [Mode::Slider, Mode::Diff] {
            let settings = Settings {
                mode,
                viewport: [32, 32],
                ..Settings::default()
            };
            let (states, job) = empty_pair(&format!("public-status-{mode:?}"), settings);
            let (progress, status) = progress_status(&job, &states, 17);
            assert_eq!(progress.done, [0, 0]);
            assert_eq!(progress.total, [128, 128]);
            assert_eq!(progress.difference_pixels, 17);
            let detail = if mode == Mode::Diff {
                "paired-tile difference · "
            } else {
                ""
            };
            assert_eq!(status, format!(
                "DNG tiles A 0/128 · B 0/128 · {detail}preview; exact tools wait for full development"
            ));
        }
    }
    #[test]
    fn completed_tiles_report_exact_development_instead_of_more_loading() {
        let (state, _) = image_state("finishing-status", [64, 48], |_, _| [0; 3]);
        let path = crate::dng::tiles::fixture("finishing-status-request", 64, 48);
        let job = selection_request(1, 1, &path);
        let states = HashMap::from([(1, state)]);
        let (progress, status) = progress_status(&job, &states, 0);
        assert_eq!(progress.done, progress.total);
        assert!(status.ends_with("preparing full-resolution tools"));
    }
    #[test]
    fn slider_queues_start_on_opposite_sides_of_the_divider_and_move_with_it() {
        let settings = Settings {
            mode: Mode::Slider,
            viewport: [128, 64],
            camera: crate::model::Camera {
                zoom: crate::model::Zoom::Fixed(1.),
                ..Default::default()
            },
            ..Settings::default()
        };
        let (states, mut job) = empty_pair("slider-priority", settings);
        let plans = next_round(&states, &job, false, 6, false);
        assert_eq!(plans.len(), 2);
        for (side, x) in [(0, 112), (1, 128)] {
            let indices = first_indices(&plans[side].work);
            assert_eq!(indices.len(), 3);
            assert!(indices
                .iter()
                .all(|&i| states[&plans[side].id].source.origin(i)[0] == x));
        }
        job.settings.split = 0.75;
        let plans = next_round(&states, &job, false, 6, false);
        assert_eq!(
            states[&1].source.origin(first_indices(&plans[0].work)[0])[0],
            144
        );
        assert_eq!(
            states[&2].source.origin(first_indices(&plans[1].work)[0])[0],
            160
        );
        job.settings.split = 0.;
        let plans = next_round(&states, &job, false, 6, false);
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].id, 2);
    }
    #[test]
    fn diff_queues_start_at_the_same_center_and_never_request_coarse_stages() {
        let settings = Settings {
            mode: Mode::Diff,
            viewport: [64, 32],
            camera: crate::model::Camera {
                zoom: crate::model::Zoom::Fixed(0.25),
                ..Default::default()
            },
            ..Settings::default()
        };
        let (mut states, job) = empty_pair("diff-priority", settings);
        states
            .values_mut()
            .for_each(|s| s.source.progressive_hint = true);
        let plans = next_round(&states, &job, false, 6, true);
        assert_eq!(plans.len(), 2);
        assert!(plans.iter().all(|p| matches!(p.work, Work::Batch(_))));
        assert_eq!(first_indices(&plans[0].work), first_indices(&plans[1].work));
        let p = states[&1].source.origin(first_indices(&plans[0].work)[0]);
        assert!((112..=128).contains(&p[0]) && (48..=64).contains(&p[1]));
    }
    #[test]
    fn both_side_tasks_really_overlap_in_the_shared_bounded_pool() {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .unwrap();
        let (a_tx, a_rx) = bounded(1);
        let (b_tx, b_rx) = bounded(1);
        let (a, b) = parallel_pair(
            &pool,
            || {
                a_tx.send(()).unwrap();
                b_rx.recv_timeout(Duration::from_secs(3)).unwrap();
                1
            },
            || {
                b_tx.send(()).unwrap();
                a_rx.recv_timeout(Duration::from_secs(3)).unwrap();
                2
            },
        );
        assert_eq!((a, b), (1, 2));
        let settings = Settings {
            mode: Mode::Slider,
            viewport: [32, 32],
            ..Settings::default()
        };
        let (mut states, job) = empty_pair("paired-round", settings);
        let plans = next_round(&states, &job, false, 2, false);
        execute_round(&mut states, &plans, &pool, None, &|| false).unwrap();
        for id in [1, 2] {
            assert!(states[&id].source.tiles.iter().any(Option::is_some));
        }
    }
    #[test]
    fn a_completed_source_is_not_redecoded_while_its_partner_streams() {
        let settings = Settings {
            mode: Mode::Diff,
            viewport: [32, 32],
            ..Settings::default()
        };
        let (mut states, job) = empty_pair("warm-paired-source", settings);
        states.get_mut(&1).unwrap().full = Some(Arc::new(Frame {
            width: 256,
            height: 128,
            rgb: vec![0; 256 * 128 * 3],
        }));
        let plans = next_round(&states, &job, true, 6, true);
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].id, 2);
        assert!(plans[0].visible);
        assert_eq!(first_indices(&plans[0].work).len(), 6);
    }
    #[test]
    fn visible_tile_plan_covers_only_the_view_and_reuses_pan_overlap() {
        let path = crate::dng::tiles::fixture("visible-plan", 1025, 769);
        let source = Source::open(&path).unwrap();
        let mut settings = Settings::default();
        settings.viewport = [64, 64];
        settings.camera.zoom = crate::model::Zoom::Fixed(1.);
        let first = visible_tiles(&source, &settings, source.size);
        assert!(first.len() < source.tiles.len() / 10);
        settings.camera.center[0] += 16. / source.size[0] as f32;
        let second = visible_tiles(&source, &settings, source.size);
        assert!(second.iter().any(|i| !first.contains(i)));
        assert!(second.iter().any(|i| first.contains(i)));
        let loaded: std::collections::HashSet<_> = first.iter().chain(&second).copied().collect();
        assert!(first.iter().all(|i| loaded.contains(i)));
        settings.camera.zoom = crate::model::Zoom::Fit;
        assert_eq!(
            visible_tiles(&source, &settings, source.size).len(),
            source.tiles.len()
        );
    }
    #[test]
    fn panning_back_does_not_decode_cached_tiles() {
        let path = crate::dng::tiles::fixture("pan-reuse", 128, 96);
        let source = Source::open(&path).unwrap();
        let size = source.size;
        let mut state = State {
            source,
            full: None,
            pending: None,
        };
        let mut settings = Settings::default();
        settings.viewport = [32, 32];
        settings.camera.zoom = crate::model::Zoom::Fixed(1.);
        let first = visible_tiles(&state.source, &settings, size);
        for &index in &first {
            state.step(index, None, false, &|| false).unwrap();
        }
        let before = state.source.tiles.iter().filter(|t| t.is_some()).count();
        for index in visible_tiles(&state.source, &settings, size) {
            assert!(state.source.tiles[index].is_some());
        }
        assert_eq!(
            state.source.tiles.iter().filter(|t| t.is_some()).count(),
            before
        );
    }
    #[test]
    fn visible_b_preempts_idle_a_and_offscreen_tiles_wait_for_idle() {
        let path = crate::dng::tiles::fixture("pair-priority", 128, 96);
        let mut states = HashMap::new();
        for id in [1, 2] {
            states.insert(
                id,
                State {
                    source: Source::open(&path).unwrap(),
                    full: None,
                    pending: None,
                },
            );
        }
        let mut settings = Settings::default();
        settings.viewport = [32, 32];
        settings.camera.zoom = crate::model::Zoom::Fixed(1.);
        let job = Request {
            generation: 1,
            pair: [(1, path.clone()), (2, path)],
            settings,
            ready: [None, None],
        };
        let mark = |state: &mut State| {
            for index in visible_tiles(&state.source, &job.settings, state.source.size) {
                state.source.tiles[index] = Some(Tile {
                    raw: Vec::new(),
                    raw_size: [16, 16],
                    display: Arc::new(Frame {
                        width: 1,
                        height: 1,
                        rgb: vec![0; 3],
                    }),
                    ratio: 1,
                });
            }
        };
        mark(states.get_mut(&1).unwrap());
        assert_eq!(next_round(&states, &job, true, 6, false)[0].id, 2);
        mark(states.get_mut(&2).unwrap());
        assert!(next_round(&states, &job, false, 6, false).is_empty());
        assert_eq!(next_round(&states, &job, true, 6, false).len(), 2);
    }
}
