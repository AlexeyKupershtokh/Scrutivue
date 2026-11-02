use crate::{
    color,
    decode::Frame,
    file_actions::FileActions,
    metrics, minimap,
    model::{self, Mode, Photo, PhotoId, Settings, Zoom},
    preferences::{Preferences, Store},
    services::{self, Event, LibraryChange, RenderJob, RenderRequest, Services},
};
use gpui_kit::component::{
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
    menu::{ContextMenuExt, PopupMenuItem},
    scroll::{Scrollbar, ScrollbarMode},
    slider::{Slider, SliderEvent, SliderState},
    Disableable, Selectable, Sizable,
};
use gpui_kit::{prelude::*, *};
use lru::LruCache;
use std::{
    cell::Cell,
    collections::{HashMap, HashSet},
    num::NonZeroUsize,
    ops::Range,
    path::PathBuf,
    rc::Rc,
    sync::{Arc, Weak},
    time::{Duration, Instant},
};

const BG: u32 = 0x1b1b1d;
const PANEL: u32 = 0x232325;
const LINE: u32 = 0x353538;
const TEXT: u32 = 0xe5e5e7;
const MUTED: u32 = 0x939398;
const CAMERA_IDLE: Duration = Duration::from_millis(10);
const STATISTICS_WIDTH: f32 = 288.;
const PICKER_SIZE: [f32; 2] = [424., 424.];
const PICKER_SWATCH_HEIGHT: f32 = 56.;

// Camera/viewport/split changes can reuse a render; source/filter changes cannot.
#[derive(Clone, Copy, PartialEq, Eq)]
struct RenderContent {
    pair: [PhotoId; 2],
    difference: bool,
    highpass: bool,
    sigma: u32,
    gain: u32,
}
impl RenderContent {
    fn new(pair: [PhotoId; 2], settings: &Settings) -> Self {
        Self {
            pair,
            difference: settings.mode == Mode::Diff,
            highpass: settings.highpass,
            sigma: if settings.highpass {
                settings.sigma.to_bits()
            } else {
                0
            },
            gain: if settings.mode == Mode::Diff {
                settings.gain
            } else {
                0
            },
        }
    }
}

struct DisplayedView {
    settings: Settings,
    image_size: [u32; 2],
    content: RenderContent,
    generation: u64,
    source_texture: bool,
}

pub struct Desk {
    services: Services,
    streaming: crate::streaming::Worker,
    stream_fallback: HashSet<PhotoId>,
    stream_sizes: HashMap<PhotoId, [u32; 2]>,
    stream_status: String,
    file_actions: FileActions,
    photos: Vec<Photo>,
    directory_roots: HashSet<PathBuf>,
    auto_select_scans: HashSet<u64>,
    initial_path: Option<(u64, PathBuf)>,
    refreshing: bool,
    indices: HashMap<PhotoId, usize>,
    next_id: PhotoId,
    selected: [Option<PhotoId>; 2],
    focused_photo: Option<PhotoId>,
    frames: HashMap<PhotoId, Arc<Frame>>,
    decode_errors: HashMap<PhotoId, String>,
    thumbnails: LruCache<PhotoId, Arc<RenderImage>>,
    thumb_pending: HashSet<PhotoId>,
    thumb_done: HashSet<PhotoId>,
    thumb_errors: HashSet<PhotoId>,
    warm_cursor: usize,
    visible: Range<usize>,
    scroll: UniformListScrollHandle,
    settings: Settings,
    preferred_mode: Mode,
    radius: Entity<SliderState>,
    _subscriptions: Vec<Subscription>,
    viewport_bounds: Rc<Cell<Bounds<Pixels>>>,
    display_scale: f32,
    generation: u64,
    render_valid_from: u64,
    render_content: Option<RenderContent>,
    displayed_view: Option<DisplayedView>,
    last_camera_change: Option<Instant>,
    primary: Option<Arc<RenderImage>>,
    alternate: Option<Arc<RenderImage>>,
    retired: Vec<Arc<RenderImage>>,
    held: bool,
    space_held: bool,
    pan_last: Option<Point<Pixels>>,
    pan_button: Option<MouseButton>,
    split_drag: bool,
    rendering: bool,
    elapsed_ms: f64,
    backend: String,
    message: String,
    scanning: usize,
    recursive: bool,
    last_sigma_change: Option<Instant>,
    focus: FocusHandle,
    help_window: Option<AnyWindowHandle>,
    settings_window: Option<AnyWindowHandle>,
    settings_sender: crossbeam_channel::Sender<crate::settings_window::Command>,
    settings_commands: crossbeam_channel::Receiver<crate::settings_window::Command>,
    cache_settings: crate::cache::CacheSettings,
    cached_preview_pair: Option<[PhotoId; 2]>,
    cache_done: HashSet<PhotoId>,
    cache_pending: Option<PhotoId>,
    cache_cursor: usize,
    library_open: bool,
    metrics_open: bool,
    metrics_worker: metrics::Worker,
    metrics_token: u64,
    metrics_pair: Option<[Option<PhotoId>; 2]>,
    metrics_edge: u32,
    metrics_result: Option<Result<metrics::Report, String>>,
    picker_enabled: bool,
    picker_point: Option<[f32; 2]>,
    picker_samples: Option<[color::Sample; 2]>,
    picker_loupe: Option<Arc<color::Loupe>>,
    picker_updated: Instant,
    picker_dirty: bool,
    picker_worker: color::LoupeWorker,
    picker_token: u64,
    picker_content: Option<RenderContent>,
    navigator_worker: minimap::Worker,
    navigator_token: u64,
    navigator_source: Option<PhotoId>,
    navigator_frame: Option<Weak<Frame>>,
    navigator_image: Option<Arc<RenderImage>>,
    navigator_bounds: Rc<Cell<Bounds<Pixels>>>,
    navigator_drag: Option<[f32; 2]>,
    preferences: Store,
    window_size: [f32; 2],
}

impl Desk {
    pub fn new(
        paths: Vec<PathBuf>,
        open_with: bool,
        recursive: bool,
        cpu: bool,
        cache: PathBuf,
        prefs: Preferences,
        preferences: Store,
        settings_warning: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let recursive = recursive || prefs.recursive;
        let services = Services::with_cache(cpu, cache, prefs.cache.clone());
        let (settings_sender, settings_commands) = crossbeam_channel::bounded(8);
        let mut auto_select_scans = HashSet::new();
        if !paths.is_empty() {
            let scan = if open_with {
                services.scan_open_with(paths.into_iter().next().unwrap())
            } else {
                services.scan(paths, recursive)
            };
            auto_select_scans.insert(scan);
        }
        let scanning = auto_select_scans.len();
        let radius = cx.new(|_| {
            SliderState::new()
                .min(0.3)
                .max(20.)
                .step(0.1)
                .default_value(prefs.sigma)
        });
        let subscription = cx.subscribe(&radius, |this, _, event, cx| {
            match event {
                SliderEvent::Change(value) => {
                    this.settings.sigma = value.end();
                    this.last_sigma_change = Some(Instant::now());
                }
                SliderEvent::Release(value) => {
                    this.settings.sigma = value.end();
                    this.last_sigma_change = None;
                    this.request_render();
                }
            }
            cx.notify();
        });
        let entity = cx.entity().downgrade();
        window.on_window_should_close(cx, move |_, cx| {
            let _ = entity.update(cx, |this, cx| {
                this.preferences.flush(this.preference_snapshot());
                if let Some(help) = this.help_window.take() {
                    let _ = help.update(cx, |_, window, _| window.remove_window());
                }
                if let Some(settings) = this.settings_window.take() {
                    let _ = settings.update(cx, |_, window, _| window.remove_window());
                }
            });
            true
        });
        cx.spawn(async move |this, cx| loop {
            smol::Timer::after(Duration::from_millis(8)).await;
            if this.update(cx, |this, cx| this.pump(cx)).is_err() {
                break;
            }
        })
        .detach();
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        Self {
            services,
            streaming: crate::streaming::Worker::new(),
            stream_fallback: HashSet::new(),
            stream_sizes: HashMap::new(),
            stream_status: String::new(),
            file_actions: FileActions::new(),
            photos: Vec::new(),
            directory_roots: HashSet::new(),
            auto_select_scans,
            initial_path: None,
            refreshing: false,
            indices: HashMap::new(),
            next_id: 1,
            selected: [None; 2],
            focused_photo: None,
            frames: HashMap::new(),
            decode_errors: HashMap::new(),
            thumbnails: LruCache::new(NonZeroUsize::new(1024).unwrap()),
            thumb_pending: HashSet::new(),
            thumb_done: HashSet::new(),
            thumb_errors: HashSet::new(),
            warm_cursor: 0,
            visible: 0..12,
            scroll: UniformListScrollHandle::new(),
            settings: prefs.settings(),
            preferred_mode: prefs.mode,
            radius,
            _subscriptions: vec![subscription],
            viewport_bounds: Rc::new(Cell::new(Bounds::default())),
            display_scale: 1.,
            generation: 0,
            render_valid_from: 0,
            render_content: None,
            displayed_view: None,
            last_camera_change: None,
            primary: None,
            alternate: None,
            retired: Vec::new(),
            held: false,
            space_held: false,
            pan_last: None,
            pan_button: None,
            split_drag: false,
            rendering: false,
            elapsed_ms: 0.,
            backend: "Initializing GPU…".into(),
            message: settings_warning,
            scanning,
            recursive,
            last_sigma_change: None,
            focus,
            help_window: None,
            settings_window: None,
            settings_sender,
            settings_commands,
            cache_settings: prefs.cache.clone(),
            cached_preview_pair: None,
            cache_done: HashSet::new(),
            cache_pending: None,
            cache_cursor: 0,
            library_open: prefs.library_open,
            metrics_open: prefs.metrics_open,
            metrics_worker: metrics::Worker::new(),
            metrics_token: 0,
            metrics_pair: None,
            metrics_edge: prefs.metrics_edge,
            metrics_result: None,
            picker_enabled: prefs.picker_enabled,
            picker_point: None,
            picker_samples: None,
            picker_loupe: None,
            picker_updated: Instant::now(),
            picker_dirty: false,
            picker_worker: color::LoupeWorker::new(),
            picker_token: 0,
            picker_content: None,
            navigator_worker: minimap::Worker::new(),
            navigator_token: 0,
            navigator_source: None,
            navigator_frame: None,
            navigator_image: None,
            navigator_bounds: Rc::new(Cell::new(Bounds::default())),
            navigator_drag: None,
            preferences,
            window_size: prefs.window_size,
        }
    }
    fn preference_snapshot(&self) -> Preferences {
        Preferences {
            version: 1,
            mode: self.preferred_mode,
            gain: self.settings.gain,
            sigma: self.settings.sigma,
            highpass: self.settings.highpass,
            split: self.settings.split,
            camera: self.settings.camera,
            library_open: self.library_open,
            recursive: self.recursive,
            metrics_open: self.metrics_open,
            metrics_edge: self.metrics_edge,
            picker_enabled: self.picker_enabled,
            window_size: self.window_size,
            cache: self.cache_settings.clone(),
        }
    }
    fn pump(&mut self, cx: &mut Context<Self>) {
        while let Ok(command) = self.settings_commands.try_recv() {
            match command {
                crate::settings_window::Command::Apply(config) => {
                    self.cache_settings = config.clone();
                    self.services.configure_cache(config);
                    self.cache_done.clear();
                    self.cache_cursor = 0;
                    self.schedule_decodes();
                    self.request_render();
                }
                crate::settings_window::Command::ClearDisk => {
                    self.services.preview_cache.clear_disk()
                }
            }
            cx.notify();
        }
        self.preferences.update(self.preference_snapshot());
        if let Ok(error) = self.preferences.errors.try_recv() {
            self.message = error;
            cx.notify();
        }
        while let Ok(Event::Error(error)) = self.file_actions.events.try_recv() {
            self.message = error;
            cx.notify();
        }
        let retired = std::mem::take(&mut self.retired);
        for image in &retired {
            cx.drop_image(image.clone(), None);
        }
        self.services.retire_images(retired);
        let mut changed = false;
        while let Ok(event) = self.streaming.events.try_recv() {
            changed = true;
            match event {
                crate::streaming::Event::Snapshot {
                    generation,
                    pair,
                    settings,
                    size,
                    primary,
                    alternate,
                    status,
                    progress: _,
                } if generation == self.generation
                    && self.selected[0] == Some(pair[0])
                    && self.selected[1].unwrap_or(pair[0]) == pair[1]
                    && self.streaming_eligible()
                    && self.cached_preview_pair != Some(pair) =>
                {
                    self.stream_sizes.insert(pair[0], size);
                    self.retire_views();
                    self.primary = Some(primary);
                    self.alternate = alternate;
                    if let Some(content) = self.current_render_content() {
                        self.displayed_view = Some(DisplayedView {
                            settings,
                            image_size: size,
                            content,
                            generation,
                            source_texture: false,
                        });
                    }
                    self.stream_status = status;
                    self.rendering = true;
                }
                crate::streaming::Event::Snapshot {
                    primary, alternate, ..
                } => {
                    self.services
                        .retire_images(std::iter::once(primary).chain(alternate).collect());
                }
                crate::streaming::Event::Complete { id, frame }
                    if self.selected.contains(&Some(id)) =>
                {
                    if let Some(index) = self.indices.get(&id) {
                        self.photos[*index].metadata.dimensions = Some(frame.size());
                        let photo = &self.photos[*index];
                        self.services.remember_frame(
                            photo.path.clone(),
                            photo.metadata.clone(),
                            frame.clone(),
                        );
                    }
                    if let Some(old) = self.frames.insert(id, frame) {
                        self.services.retire_frames(vec![old]);
                    }
                    if self
                        .selected
                        .iter()
                        .flatten()
                        .all(|id| self.frames.contains_key(id))
                    {
                        self.request_render();
                    }
                }
                crate::streaming::Event::Complete { frame, .. } => {
                    self.services.retire_frames(vec![frame])
                }
                crate::streaming::Event::Progress { generation, status } => {
                    if generation == self.generation
                        && self.streaming_eligible()
                        && self.cached_preview_pair.is_none()
                    {
                        self.stream_status = status;
                    }
                }
                crate::streaming::Event::Unsupported { ids, reason } => {
                    if ids.iter().any(|id| self.selected.contains(&Some(*id))) {
                        self.stream_fallback.extend(ids);
                        self.stream_status.clear();
                        self.message = reason;
                        self.schedule_decodes();
                        self.request_render();
                    }
                }
            }
        }
        while let Ok(event) = self.services.preview_cache.events.try_recv() {
            match event {
                crate::cache::Event::Snapshot(snapshot)
                    if snapshot.generation == self.generation
                        && self.selected[0] == Some(snapshot.pair[0])
                        && self.selected[1].unwrap_or(snapshot.pair[0]) == snapshot.pair[1]
                        && self.cached_preview_job().is_some() =>
                {
                    #[cfg(feature = "experimental-nef-progressive")]
                    {
                        crate::nef_preview::trace::record(
                            "disk_preview_accepted",
                            serde_json::json!({"generation":snapshot.generation,"pair":snapshot.pair}),
                        );
                        self.services.nef_preview.request(None);
                    }
                    self.cached_preview_pair = Some(snapshot.pair);
                    self.stream_sizes.insert(snapshot.pair[0], snapshot.size);
                    self.retire_views();
                    self.primary = Some(snapshot.primary);
                    self.alternate = snapshot.alternate;
                    if let Some(content) = self.current_render_content() {
                        self.displayed_view = Some(DisplayedView {
                            settings: snapshot.settings,
                            image_size: snapshot.size,
                            content,
                            generation: snapshot.generation,
                            source_texture: true,
                        });
                    }
                    self.stream_status =
                        "Cached preview · 2560 px · exact tools wait for full development".into();
                    self.rendering = true;
                    changed = true;
                }
                crate::cache::Event::Snapshot(snapshot) => self.services.retire_images(
                    std::iter::once(snapshot.primary)
                        .chain(snapshot.alternate)
                        .collect(),
                ),
                crate::cache::Event::Built { id, complete } => {
                    #[cfg(feature = "experimental-nef-progressive")]
                    crate::nef_preview::trace::record(
                        "cache_preview_built",
                        serde_json::json!({"id":id,"complete":complete}),
                    );
                    if self.cache_pending == Some(id) {
                        self.cache_pending = None;
                    }
                    if complete && self.indices.contains_key(&id) {
                        self.cache_done.insert(id);
                    }
                }
                crate::cache::Event::Cleared(result) => {
                    self.cache_done.clear();
                    self.cache_cursor = 0;
                    self.cached_preview_pair = None;
                    self.message=match result {Ok(())=>"Disk preview cache cleared; source images and thumbnails were preserved".into(),Err(error)=>error};
                    self.request_render();
                    changed = true;
                }
                crate::cache::Event::Error(error) => {
                    self.message = error;
                    changed = true;
                }
            }
        }
        #[cfg(feature = "experimental-nef-progressive")]
        while let Ok(snapshot) = self.services.nef_preview.events.try_recv() {
            if snapshot.generation == self.generation
                && snapshot.is_valid()
                && self.selected[0] == Some(snapshot.pair[0])
                && self.selected[1].unwrap_or(snapshot.pair[0]) == snapshot.pair[1]
                && self.nef_preview_job().is_some()
                && self.cached_preview_pair != Some(snapshot.pair)
            {
                crate::nef_preview::trace::record(
                    "preview_accepted",
                    serde_json::json!({"generation":snapshot.generation,
                    "pair":snapshot.pair,"covered_sampled_pixels":snapshot.covered_pixels}),
                );
                self.stream_sizes.insert(snapshot.pair[0], snapshot.size);
                self.retire_views();
                self.primary = Some(snapshot.primary);
                self.alternate = snapshot.alternate;
                if let Some(content) = self.current_render_content() {
                    self.displayed_view = Some(DisplayedView {
                        settings: snapshot.settings,
                        image_size: snapshot.size,
                        content,
                        generation: snapshot.generation,
                        source_texture: false,
                    });
                }
                self.stream_status = format!("NEF preview · 1:4 sampled bands · A {} / B {} pixels · exact tools wait for full development",
                    snapshot.covered_pixels[0], snapshot.covered_pixels[1]);
                self.rendering = true;
                changed = true;
            } else {
                self.services.retire_images(
                    std::iter::once(snapshot.primary)
                        .chain(snapshot.alternate)
                        .collect(),
                );
            }
        }
        while let Ok(update) = self.navigator_worker.events.try_recv() {
            if update.token == self.navigator_token {
                if let Some(old) = self.navigator_image.replace(update.image) {
                    self.retired.push(old);
                }
                changed = true;
            } else {
                self.services.retire_images(vec![update.image]);
            }
        }
        while let Ok(update) = self.picker_worker.events.try_recv() {
            if update.token == self.picker_token
                && self.picker_enabled
                && self.picker_samples.is_some()
            {
                self.picker_loupe = Some(Arc::new(update.loupe));
                changed = true;
            }
        }
        if self.picker_dirty && self.picker_updated.elapsed() >= Duration::from_millis(16) {
            self.update_picker();
            changed = true;
        }
        while let Ok(update) = self.metrics_worker.events.try_recv() {
            if update.token == self.metrics_token {
                self.metrics_result = Some(update.result);
                changed = true;
            }
        }
        for _ in 0..48 {
            let Ok(event) = self.services.events.try_recv() else {
                break;
            };
            changed = true;
            match event {
                Event::Backend(backend) => self.backend = backend,
                Event::Roots { epoch, roots } if epoch == self.services.epoch() => {
                    self.directory_roots.extend(roots);
                }
                Event::OpenWithTarget { epoch, scan, path } if epoch == self.services.epoch() => {
                    if self.auto_select_scans.contains(&scan) {
                        self.initial_path = Some((scan, path));
                    }
                }
                Event::Files { epoch, scan, files } if epoch == self.services.epoch() => {
                    let change = services::merge_files(&mut self.photos, &mut self.next_id, files);
                    let mut selection_changed = self.apply_library_change(change);
                    if self.auto_select_scans.contains(&scan) {
                        let preferred = self
                            .initial_path
                            .as_ref()
                            .filter(|(target_scan, _)| *target_scan == scan)
                            .map(|(_, path)| path.as_path());
                        let initial_changed = if preferred.is_some() {
                            model::select_initial_path(&mut self.selected, &self.photos, preferred)
                        } else {
                            model::select_initial(&mut self.selected, &self.photos)
                        };
                        if initial_changed && preferred.is_some() {
                            self.focused_photo = self.selected[0];
                            if let Some(index) =
                                self.focused_photo.and_then(|id| self.indices.get(&id))
                            {
                                center_open_with_row(&self.scroll, *index);
                            }
                        }
                        selection_changed |= initial_changed;
                    }
                    if selection_changed {
                        self.selection_changed();
                    }
                }
                Event::ScanDone {
                    epoch,
                    scan,
                    skipped,
                } if epoch == self.services.epoch() => {
                    let auto_select = self.auto_select_scans.remove(&scan);
                    if let Some((_, path)) = self
                        .initial_path
                        .as_ref()
                        .filter(|(target_scan, _)| *target_scan == scan)
                    {
                        // Earlier batches may not contain enough following rows
                        // to center the target. Retry with the complete list,
                        // unless the user has taken over selection/navigation.
                        if auto_select {
                            if let Some(index) = self.selected[0]
                                .and_then(|id| self.indices.get(&id))
                                .filter(|index| self.photos[**index].path == *path)
                            {
                                center_open_with_row(&self.scroll, *index);
                            }
                        }
                        self.initial_path = None;
                    }
                    self.scanning = self.scanning.saturating_sub(1);
                    if skipped > 0 {
                        self.message =
                            format!("Skipped inaccessible or unsupported paths: {skipped}");
                    }
                }
                Event::Refreshed {
                    epoch,
                    files,
                    complete_roots,
                    skipped,
                } if epoch == self.services.epoch() => {
                    let change = services::refresh_files(
                        &mut self.photos,
                        &mut self.next_id,
                        files,
                        &complete_roots,
                    );
                    if self.apply_library_change(change) {
                        self.selection_changed();
                    }
                    self.refreshing = false;
                    self.scanning = self.scanning.saturating_sub(1);
                    self.message = if skipped == 0 {
                        "Library refreshed".into()
                    } else {
                        format!("Refresh: {skipped} inaccessible paths; their entries were kept")
                    };
                }
                Event::PathsChosen { epoch, paths } if epoch == self.services.epoch() => {
                    self.add_paths(paths)
                }
                Event::Thumbnail { id, result } if self.indices.contains_key(&id) => {
                    self.thumb_pending.remove(&id);
                    self.thumb_done.insert(id);
                    match result {
                        Ok(image) => {
                            if let Some((_, old)) = self.thumbnails.push(id, image) {
                                self.retired.push(old);
                            }
                        }
                        Err(_) => {
                            self.thumb_errors.insert(id);
                        }
                    }
                }
                Event::Decoded { id, result } if self.selected.contains(&Some(id)) => {
                    match result {
                        Ok(frame) => {
                            #[cfg(feature = "experimental-nef-progressive")]
                            crate::nef_preview::trace::record(
                                "full_decoded",
                                serde_json::json!({"id":id}),
                            );
                            self.decode_errors.remove(&id);
                            if let Some(index) = self.indices.get(&id) {
                                self.photos[*index].metadata.dimensions = Some(frame.size());
                            }
                            if let Some(old) = self.frames.insert(id, frame) {
                                self.services.retire_frames(vec![old]);
                            }
                            self.request_render();
                        }
                        Err(error) => {
                            self.message = error.clone();
                            self.decode_errors.insert(id, error);
                            let busy = self.selected.iter().flatten().any(|selected| {
                                !self.frames.contains_key(selected)
                                    && !self.decode_errors.contains_key(selected)
                            });
                            self.services.preview_cache.request(None, busy);
                            if self.cached_preview_pair.is_some() {
                                self.retire_views();
                                self.stream_status.clear();
                                self.cached_preview_pair = None;
                            }
                            #[cfg(feature = "experimental-nef-progressive")]
                            {
                                self.services.nef_preview.request(None);
                                if self.stream_status.starts_with("NEF preview") {
                                    self.retire_views();
                                    self.stream_status.clear();
                                }
                            }
                            self.rendering = false;
                        }
                    }
                }
                Event::Rendered {
                    generation,
                    settings,
                    primary,
                    alternate,
                    elapsed,
                    backend,
                } if self.render_content.is_some()
                    && self.render_content == self.current_render_content()
                    && accepts_snapshot(
                        generation,
                        self.generation,
                        self.render_valid_from,
                        self.displayed_view.as_ref().map(|view| view.generation),
                    ) =>
                {
                    #[cfg(feature = "experimental-nef-progressive")]
                    crate::nef_preview::trace::record(
                        "full_render_accepted",
                        serde_json::json!({"generation":generation,
                        "pair":self.selected,"render_ms":elapsed.as_secs_f64()*1000.}),
                    );
                    self.retire_views();
                    self.primary = Some(primary);
                    self.alternate = alternate;
                    self.displayed_view = Some(DisplayedView {
                        settings,
                        image_size: self.image_size(),
                        content: self.render_content.unwrap(),
                        generation,
                        source_texture: false,
                    });
                    self.elapsed_ms = elapsed.as_secs_f64() * 1000.;
                    self.cached_preview_pair = None;
                    self.stream_status.clear();
                    self.backend = backend;
                    self.rendering =
                        generation < self.generation || self.last_camera_change.is_some();
                    self.update_picker();
                }
                Event::Error(error) => self.message = error,
                stale => self.services.discard(stale),
            }
        }
        let bounds = self.viewport_bounds.get();
        let size = [
            (f32::from(bounds.size.width) * self.display_scale)
                .round()
                .max(1.) as u32,
            (f32::from(bounds.size.height) * self.display_scale)
                .round()
                .max(1.) as u32,
        ];
        if size[0] > 8 && size[1] > 8 && size != self.settings.viewport {
            self.settings.viewport = size;
            self.picker_point = None;
            self.clear_picker();
            self.preview_camera_change();
            changed = true;
        }
        if camera_render_due(self.last_camera_change, Instant::now()) {
            self.request_render();
            changed = true;
        }
        if self
            .last_sigma_change
            .is_some_and(|t| t.elapsed() >= Duration::from_millis(100))
        {
            self.last_sigma_change = None;
            if self.settings.highpass {
                self.request_render();
            }
            changed = true;
        }
        self.schedule_thumbnails();
        self.schedule_cache_builds();
        self.schedule_metrics();
        changed |= self.schedule_navigator();
        if changed {
            cx.notify();
        }
    }
    fn add_paths(&mut self, paths: Vec<PathBuf>) {
        if !paths.is_empty() {
            self.scanning += 1;
            let scan = self.services.scan(paths, self.recursive);
            if self.selected == [None, None] {
                self.auto_select_scans.insert(scan);
            }
        }
    }
    fn refresh_library(&mut self, cx: &mut Context<Self>) {
        if self.directory_roots.is_empty() || self.scanning > 0 {
            return;
        }
        let mut roots: Vec<_> = self.directory_roots.iter().cloned().collect();
        roots.sort();
        self.scanning += 1;
        self.refreshing = true;
        self.services.refresh(roots, self.recursive);
        cx.notify();
    }
    fn apply_library_change(&mut self, change: LibraryChange) -> bool {
        let before = self.selected;
        change.remap_selection(&mut self.selected, &mut self.focused_photo);
        let mut retired_frames = Vec::new();
        for id in change
            .removed
            .iter()
            .copied()
            .chain(change.replaced.iter().map(|(old, _)| *old))
        {
            if let Some(frame) = self.frames.remove(&id) {
                retired_frames.push(frame);
            }
            if let Some(image) = self.thumbnails.pop(&id) {
                self.retired.push(image);
            }
            self.decode_errors.remove(&id);
            self.thumb_pending.remove(&id);
            self.thumb_done.remove(&id);
            self.thumb_errors.remove(&id);
            self.cache_done.remove(&id);
            if self.cache_pending == Some(id) {
                self.cache_pending = None;
            }
        }
        self.services.retire_frames(retired_frames);
        self.warm_cursor = self.warm_cursor.min(self.photos.len());
        if !change.replaced.is_empty() || !change.removed.is_empty() {
            self.warm_cursor = 0;
            self.cache_cursor = 0;
        }
        self.reindex();
        before != self.selected
    }
    fn schedule_metrics(&mut self) {
        if !self.metrics_open {
            return;
        }
        let Some(aid) = self.selected[0] else {
            return;
        };
        let Some(a) = self.frames.get(&aid) else {
            return;
        };
        let b = self.selected[1].and_then(|id| self.frames.get(&id).map(|frame| (id, frame)));
        let pair = [Some(aid), b.map(|(id, _)| id)];
        if self.metrics_pair == Some(pair) {
            return;
        }
        self.metrics_token = if let Some((bid, b)) = b {
            self.metrics_worker
                .request(aid, a.clone(), bid, b.clone(), self.metrics_edge)
        } else {
            self.metrics_worker
                .request_single(aid, a.clone(), self.metrics_edge)
        };
        self.metrics_pair = Some(pair);
        self.metrics_result = None;
        // Ready reports are published synchronously; avoid a loading flash when
        // revisiting a known pair while another worker job is winding down.
        while let Ok(update) = self.metrics_worker.events.try_recv() {
            if update.token == self.metrics_token {
                self.metrics_result = Some(update.result);
            }
        }
    }
    fn reset_metrics(&mut self) {
        self.metrics_token = self.metrics_worker.cancel();
        self.metrics_pair = None;
        self.metrics_result = None;
    }
    fn schedule_navigator(&mut self) -> bool {
        let frame =
            self.selected[0].and_then(|id| self.frames.get(&id).map(|frame| (id, frame.clone())));
        let same_source = match &frame {
            Some((id, frame)) => {
                self.navigator_source == Some(*id)
                    && self
                        .navigator_frame
                        .as_ref()
                        .is_some_and(|weak| weak.ptr_eq(&Arc::downgrade(frame)))
            }
            None => self.navigator_source.is_none(),
        };
        if same_source {
            return false;
        }
        self.navigator_drag = None;
        if let Some(old) = self.navigator_image.take() {
            self.retired.push(old);
        }
        if let Some((id, frame)) = frame {
            self.navigator_source = Some(id);
            self.navigator_frame = Some(Arc::downgrade(&frame));
            self.navigator_token = self.navigator_worker.request(id, frame);
        } else {
            self.navigator_source = None;
            self.navigator_frame = None;
            self.navigator_token = self.navigator_worker.cancel();
        }
        true
    }
    fn navigator_point(&self, position: Point<Pixels>) -> [f32; 2] {
        let bounds = self.navigator_bounds.get();
        let local = [
            f32::from(position.x - bounds.origin.x),
            f32::from(position.y - bounds.origin.y),
        ];
        let image = minimap::image_rect(
            self.image_size(),
            [f32::from(bounds.size.width), f32::from(bounds.size.height)],
        );
        minimap::normalized_point(local, image)
    }
    fn navigator_ready(&self) -> bool {
        self.navigator_image.is_some()
            && self.navigator_source == self.selected[0]
            && self.selected[0]
                .and_then(|id| self.frames.get(&id))
                .is_some_and(|frame| {
                    self.navigator_frame
                        .as_ref()
                        .is_some_and(|weak| weak.ptr_eq(&Arc::downgrade(frame)))
                })
    }
    fn move_navigator(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(offset) = self.navigator_drag else {
            return;
        };
        let target = self.navigator_point(position);
        let old = self.settings.camera;
        self.settings.camera.center = std::array::from_fn(|i| target[i] - offset[i]);
        self.settings
            .camera
            .clamp(self.image_size(), self.settings.viewport);
        if self.settings.camera != old {
            self.preview_camera_change();
            self.clear_picker();
            cx.notify();
        }
    }
    fn navigator_overlay(&self, cx: &mut Context<Self>) -> AnyElement {
        let cell = self.navigator_bounds.clone();
        let image = self.navigator_image.clone();
        let image_size = self.image_size();
        let visible =
            minimap::visible_rect(&self.settings.camera, image_size, self.settings.viewport);
        glass()
            .id("navigator")
            .absolute()
            .bottom_3()
            .right_3()
            .p_2()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child("NAVIGATOR · A"),
            )
            .child(
                canvas(
                    move |bounds, _, _| cell.set(bounds),
                    move |bounds, _, window, _| {
                        window.paint_quad(fill(bounds, rgb(0x141416)));
                        let content = minimap::image_rect(
                            image_size,
                            [f32::from(bounds.size.width), f32::from(bounds.size.height)],
                        );
                        let content_bounds = map_bounds(bounds.origin, content);
                        if let Some(image) = image {
                            let _ = window.paint_image(
                                content_bounds,
                                content_bounds,
                                Corners::default(),
                                image,
                                0,
                                false,
                            );
                        }
                        let view = map_bounds(bounds.origin, minimap::project(visible, content));
                        window.paint_quad(fill(view, rgba(0xe5e5e72b)));
                        window.paint_quad(outline(view, rgb(0xffffff), BorderStyle::Solid));
                    },
                )
                .w(px(minimap::MAP_SIZE[0]))
                .h(px(minimap::MAP_SIZE[1])),
            )
            .cursor_crosshair()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    if !this.navigator_bounds.get().contains(&event.position) {
                        return;
                    }
                    window.focus(&this.focus, cx);
                    let point = this.navigator_point(event.position);
                    let visible = minimap::visible_rect(
                        &this.settings.camera,
                        this.image_size(),
                        this.settings.viewport,
                    );
                    this.navigator_drag = Some(if visible.contains(point) {
                        std::array::from_fn(|i| point[i] - this.settings.camera.center[i])
                    } else {
                        [0.; 2]
                    });
                    this.move_navigator(event.position, cx);
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                cx.stop_propagation();
                if this.picker_samples.is_some() {
                    this.clear_picker();
                    cx.notify();
                }
                if event.pressed_button == Some(MouseButton::Left) {
                    this.move_navigator(event.position, cx);
                } else {
                    this.navigator_drag = None;
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.navigator_drag = None;
                    cx.stop_propagation();
                }),
            )
            .on_mouse_down(MouseButton::Middle, |_, _, cx| cx.stop_propagation())
            .on_scroll(cx.listener(|this, event: &ScrollEvent, _, cx| {
                cx.stop_propagation();
                if !this.navigator_bounds.get().contains(&event.position) {
                    return;
                }
                let anchor = minimap::viewport_point(
                    this.navigator_point(event.position),
                    &this.settings.camera,
                    this.image_size(),
                    this.settings.viewport,
                );
                this.zoom_camera(event, anchor, None, cx);
            }))
            .into_any_element()
    }
    fn clear_picker(&mut self) {
        self.picker_dirty = false;
        self.picker_samples = None;
        self.picker_loupe = None;
        self.picker_token = self.picker_worker.cancel();
        self.picker_content = None;
    }
    fn toggle_picker(&mut self, cx: &mut Context<Self>) {
        self.picker_enabled = !self.picker_enabled;
        if !self.picker_enabled {
            self.clear_picker();
        } else {
            self.update_picker();
        }
        cx.notify();
    }
    fn update_picker(&mut self) {
        self.picker_dirty = false;
        self.picker_updated = Instant::now();
        if !self.picker_enabled {
            self.clear_picker();
            return;
        }
        let Some(point) = self.picker_point else {
            self.clear_picker();
            return;
        };
        let content = self.current_render_content();
        if content != self.picker_content {
            self.clear_picker();
        }
        let Some(a) = self.selected[0].and_then(|id| self.frames.get(&id)) else {
            self.clear_picker();
            return;
        };
        let b = match self.selected[1] {
            Some(id) => match self.frames.get(&id) {
                Some(b) => b,
                None => {
                    self.clear_picker();
                    return;
                }
            },
            None => a,
        };
        let Some(samples) = color::sample_pair(a, b, &self.settings, point) else {
            self.clear_picker();
            return;
        };
        self.picker_token = self.picker_worker.request(
            a.clone(),
            b.clone(),
            self.settings.clone(),
            point,
            self.held || self.space_held,
            self.display_scale,
        );
        self.picker_samples = Some(samples);
        self.picker_content = content;
    }
    fn toggle_metrics(&mut self, cx: &mut Context<Self>) {
        self.metrics_open = !self.metrics_open;
        // Opening Statistics also cancels an already-running speculative decode;
        // selected A/B loads remain wanted and continue normally.
        self.schedule_decodes();
        if self.metrics_open {
            self.schedule_metrics();
        } else {
            self.reset_metrics();
        }
        cx.notify();
    }
    fn retire_views(&mut self) {
        self.displayed_view = None;
        if let Some(image) = self.primary.take() {
            self.retired.push(image);
        }
        if let Some(image) = self.alternate.take() {
            self.retired.push(image);
        }
    }
    fn clear(&mut self, cx: &mut Context<Self>) {
        self.services.clear();
        #[cfg(feature = "experimental-nef-progressive")]
        self.services.nef_preview.request(None);
        self.streaming.clear();
        self.stream_fallback.clear();
        self.stream_sizes.clear();
        self.stream_status.clear();
        self.reset_metrics();
        self.clear_picker();
        self.generation += 1;
        self.retire_views();
        while let Some((_, image)) = self.thumbnails.pop_lru() {
            self.retired.push(image);
        }
        self.photos.clear();
        self.directory_roots.clear();
        self.auto_select_scans.clear();
        self.initial_path = None;
        self.refreshing = false;
        self.indices.clear();
        self.selected = [None; 2];
        self.settings.mode = Mode::Click;
        self.focused_photo = None;
        self.services
            .retire_frames(self.frames.drain().map(|(_, frame)| frame).collect());
        self.decode_errors.clear();
        self.thumb_pending.clear();
        self.thumb_done.clear();
        self.thumb_errors.clear();
        self.warm_cursor = 0;
        self.cache_done.clear();
        self.cache_pending = None;
        self.cache_cursor = 0;
        self.cached_preview_pair = None;
        self.visible = 0..12;
        self.scroll = UniformListScrollHandle::new();
        self.rendering = false;
        self.scanning = 0;
        self.message.clear();
        self.elapsed_ms = 0.;
        self.held = false;
        self.space_held = false;
        self.pan_last = None;
        self.pan_button = None;
        self.split_drag = false;
        self.settings.camera.fit();
        self.settings.split = 0.5;
        cx.notify();
    }
    fn schedule_thumbnails(&mut self) {
        if self.thumb_pending.len() >= 6 {
            return;
        }
        for index in self.visible.clone() {
            let Some(photo) = self.photos.get(index) else {
                break;
            };
            if self.thumbnails.peek(&photo.id).is_some()
                || self.thumb_pending.contains(&photo.id)
                || self.thumb_errors.contains(&photo.id)
            {
                continue;
            }
            if !self.services.thumbnail(photo.id, photo.path.clone()) {
                return;
            }
            self.thumb_pending.insert(photo.id);
            if self.thumb_pending.len() >= 6 {
                return;
            }
        }
        while let Some(photo) = self.photos.get(self.warm_cursor) {
            if self.thumb_done.contains(&photo.id) || self.thumb_pending.contains(&photo.id) {
                self.warm_cursor += 1;
                continue;
            }
            if self.services.thumbnail(photo.id, photo.path.clone()) {
                self.thumb_pending.insert(photo.id);
                self.warm_cursor += 1;
            }
            break;
        }
    }
    fn current_render_content(&self) -> Option<RenderContent> {
        self.selected[0]
            .map(|a| RenderContent::new([a, self.selected[1].unwrap_or(a)], &self.settings))
    }
    fn schedule_cache_builds(&mut self) {
        if !self.cache_settings.build_all
            || self.cache_pending.is_some()
            || self.rendering
            || self
                .selected
                .iter()
                .flatten()
                .any(|id| !self.frames.contains_key(id))
        {
            return;
        }
        while let Some(photo) = self.photos.get(self.cache_cursor) {
            if self.cache_done.contains(&photo.id) {
                self.cache_cursor += 1;
                continue;
            }
            if self
                .services
                .preview_cache
                .build(photo.id, photo.path.clone())
            {
                self.cache_pending = Some(photo.id);
            }
            break;
        }
    }
    fn cached_preview_job(&self) -> Option<crate::streaming::Request> {
        if self.settings.highpass || self.settings.mode == Mode::Diff {
            return None;
        }
        let aid = self.selected[0]?;
        let bid = self.selected[1].unwrap_or(aid);
        if [aid, bid].iter().all(|id| self.frames.contains_key(id))
            || [aid, bid]
                .iter()
                .any(|id| self.decode_errors.contains_key(id))
        {
            return None;
        }
        if [aid, bid].iter().any(|id| {
            !self.frames.contains_key(id)
                && self
                    .indices
                    .get(id)
                    .and_then(|index| self.photos[*index].metadata.dimensions)
                    .is_some_and(|size| !crate::cache::eligible(size))
        }) {
            return None;
        }
        let path = |id| {
            self.indices
                .get(&id)
                .map(|index| self.photos[*index].path.clone())
        };
        Some(crate::streaming::Request {
            generation: self.generation,
            pair: [(aid, path(aid)?), (bid, path(bid)?)],
            settings: self.settings.clone(),
            ready: [
                self.frames.get(&aid).cloned(),
                self.frames.get(&bid).cloned(),
            ],
        })
    }
    fn preview_camera_change(&mut self) {
        if self.selected[0].is_some() {
            self.settings
                .camera
                .clamp(self.image_size(), self.settings.viewport);
        }
        self.last_camera_change = Some(Instant::now());
        self.rendering = self.selected[0].is_some();
    }
    fn zoom_camera(
        &mut self,
        event: &ScrollEvent,
        anchor: [f32; 2],
        picker_point: Option<[f32; 2]>,
        cx: &mut Context<Self>,
    ) {
        if self.selected[0].is_none() || self.image_size() == [1, 1] {
            return;
        }
        let delta = event.delta.pixel_delta(px(36.));
        let old = self.settings.camera;
        self.settings.camera.zoom_at(
            (f32::from(delta.y) * 0.0025).clamp(-0.7, 0.7).exp(),
            anchor,
            self.image_size(),
            self.settings.viewport,
        );
        if let Some(point) = picker_point {
            self.picker_point = Some(point);
        } else {
            // The Navigator cursor is not a main-image inspection point.
            self.picker_point = None;
            self.clear_picker();
        }
        if self.settings.camera != old || self.last_camera_change.is_some() {
            self.preview_camera_change();
            // Keep the current same-source loupe visible until its replacement
            // arrives. Sampling uses the new camera, not the prepared texture.
            self.update_picker();
            cx.notify();
        }
    }
    fn request_render(&mut self) {
        self.last_camera_change = None;
        self.generation += 1;
        let busy = self
            .selected
            .iter()
            .flatten()
            .any(|id| !self.frames.contains_key(id));
        self.services
            .preview_cache
            .request(self.cached_preview_job(), busy);
        self.schedule_decodes();
        #[cfg(feature = "experimental-nef-progressive")]
        self.services.nef_preview.request(self.nef_preview_job());
        let content = self.current_render_content();
        if content != self.render_content {
            self.render_valid_from = self.generation;
            self.render_content = content;
        }
        if content != self.picker_content {
            self.clear_picker();
        }
        let Some(aid) = self.selected[0] else {
            self.streaming.pause();
            self.stream_status.clear();
            self.rendering = false;
            return;
        };
        let bid = self.selected[1].unwrap_or(aid);
        if self.streaming_eligible()
            && (!self.frames.contains_key(&aid) || !self.frames.contains_key(&bid))
        {
            self.rendering = true;
            let path = |id| self.photos[*self.indices.get(&id).unwrap()].path.clone();
            self.streaming.request(crate::streaming::Request {
                generation: self.generation,
                pair: [(aid, path(aid)), (bid, path(bid))],
                settings: self.settings.clone(),
                ready: [
                    self.frames.get(&aid).cloned(),
                    self.frames.get(&bid).cloned(),
                ],
            });
            return;
        }
        self.streaming.pause();
        self.stream_status.clear();
        let Some(a) = self.frames.get(&aid) else {
            self.rendering = true;
            self.schedule_decodes();
            return;
        };
        self.settings.camera.clamp(a.size(), self.settings.viewport);
        let Some(b) = self.frames.get(&bid) else {
            self.rendering = true;
            self.schedule_decodes();
            return;
        };
        self.rendering = true;
        self.services.renders.put(RenderJob::Frame(RenderRequest {
            generation: self.generation,
            a: (aid, a.clone()),
            b: (bid, b.clone()),
            settings: self.settings.clone(),
        }));
        self.update_picker();
    }
    fn selection_changed(&mut self) {
        self.cached_preview_pair = None;
        self.stream_status.clear();
        self.settings.mode = self.preferred_mode.effective(self.selected[1].is_some());
        self.reset_metrics();
        self.clear_picker();
        self.held = false;
        self.space_held = false;
        self.pan_last = None;
        self.pan_button = None;
        self.split_drag = false;
        let obsolete: Vec<_> = self
            .frames
            .keys()
            .copied()
            .filter(|id| !self.selected.contains(&Some(*id)))
            .collect();
        let retired = obsolete
            .into_iter()
            .filter_map(|id| self.frames.remove(&id))
            .collect();
        self.services.retire_frames(retired);
        if self.selected[0].is_none() {
            self.retire_views();
            self.services.renders.put(RenderJob::Clear);
        }
        #[cfg(feature = "experimental-nef-progressive")]
        self.services.nef_preview.request(self.nef_preview_job());
        self.schedule_decodes();
        if self.streaming_eligible() {
            for id in self.selected.iter().flatten() {
                if !self.frames.contains_key(id) {
                    if let Some(index) = self.indices.get(id) {
                        self.services
                            .lookup_frame(*id, self.photos[*index].path.clone());
                    }
                }
            }
        }
        self.request_render();
    }
    fn schedule_decodes(&self) {
        let streaming = self.streaming_eligible();
        let mut jobs: Vec<_> = self
            .selected
            .iter()
            .flatten()
            .filter(|id| !self.frames.contains_key(id))
            .filter(|_| !streaming)
            .filter_map(|id| {
                self.indices
                    .get(id)
                    .map(|i| (*id, self.photos[*i].path.clone()))
            })
            .collect();
        let selected_decodes = jobs.len();
        if let Some(index) = self
            .focused_photo
            .and_then(|id| self.indices.get(&id))
            .copied()
            .filter(|_| {
                (!streaming
                    || self
                        .selected
                        .iter()
                        .flatten()
                        .all(|id| self.frames.contains_key(id)))
                    && neighbor_prefetch_allowed(
                        self.selected,
                        self.metrics_open,
                        self.cache_settings.prefetch,
                    )
            })
        {
            for neighbor in [index.checked_add(1), index.checked_sub(1)]
                .into_iter()
                .flatten()
            {
                if let Some(photo) = self.photos.get(neighbor) {
                    if !self.selected.contains(&Some(photo.id))
                        && !jobs.iter().any(|(id, _)| *id == photo.id)
                    {
                        jobs.push((photo.id, photo.path.clone()));
                    }
                }
            }
        }
        if self.selected[0].is_none() {
            jobs.clear();
        }
        let protected = self
            .selected
            .iter()
            .flatten()
            .filter_map(|id| self.indices.get(id).map(|i| self.photos[*i].path.clone()))
            .collect();
        self.services.decode(jobs, selected_decodes, protected);
    }
    fn select(&mut self, id: PhotoId, control: bool, cx: &mut Context<Self>) {
        if !self.indices.contains_key(&id) {
            return;
        }
        self.auto_select_scans.clear();
        let before = self.selected;
        model::click_selection(&mut self.selected, id, control);
        self.focused_photo = Some(id);
        if self.selected != before {
            self.selection_changed();
        }
        cx.notify();
    }
    fn select_as(&mut self, id: PhotoId, slot: usize, cx: &mut Context<Self>) {
        if !self.indices.contains_key(&id) {
            return;
        }
        self.auto_select_scans.clear();
        model::select_slot(&mut self.selected, id, slot);
        self.focused_photo = Some(id);
        self.selection_changed();
        cx.notify();
    }
    fn open_photo(&mut self, id: PhotoId, choose_application: bool) {
        if let Some(index) = self.indices.get(&id) {
            self.services
                .open_file(self.photos[*index].path.clone(), choose_application);
        }
    }
    fn copy_photo_path(&self, id: PhotoId, file_name_only: bool, cx: &mut App) {
        if let Some(index) = self.indices.get(&id) {
            let photo = &self.photos[*index];
            let text = if file_name_only {
                photo
                    .path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
            } else {
                photo.path.to_string_lossy().into_owned()
            };
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }
    fn open_photo_folder(&mut self, id: PhotoId, cx: &mut Context<Self>) {
        if let Some(index) = self.indices.get(&id) {
            if let Err(error) = self
                .file_actions
                .open_containing_folder(self.photos[*index].path.clone())
            {
                self.message = error;
                cx.notify();
            }
        }
    }
    fn navigate(&mut self, delta: isize, cx: &mut Context<Self>) {
        self.auto_select_scans.clear();
        if self.photos.is_empty() {
            return;
        }
        let current = if self.selected[1].is_some() {
            self.focused_photo.or(self.selected[1]).or(self.selected[0])
        } else {
            self.selected[0].or(self.focused_photo)
        };
        let index = current
            .and_then(|id| self.indices.get(&id))
            .copied()
            .unwrap_or(0)
            .saturating_add_signed(delta)
            .min(self.photos.len() - 1);
        let id = self.photos[index].id;
        model::navigate_selection(&mut self.selected, id);
        self.focused_photo = Some(id);
        self.scroll.scroll_to_item(index, ScrollStrategy::Nearest);
        self.selection_changed();
        cx.notify();
    }
    fn reorder(&mut self, delta: isize, cx: &mut Context<Self>) {
        if let Some(id) = self.focused_photo {
            model::move_photo(&mut self.photos, id, delta);
            self.reindex();
            if let Some(index) = self.indices.get(&id) {
                self.scroll.scroll_to_item(*index, ScrollStrategy::Nearest);
            }
            cx.notify();
        }
    }
    fn reindex(&mut self) {
        self.indices.clear();
        for (i, photo) in self.photos.iter().enumerate() {
            self.indices.insert(photo.id, i);
        }
    }
    fn image_size(&self) -> [u32; 2] {
        self.selected[0]
            .and_then(|id| self.frames.get(&id))
            .map(|f| f.size())
            .or_else(|| self.selected[0].and_then(|id| self.stream_sizes.get(&id).copied()))
            .or_else(|| {
                self.selected[0]
                    .and_then(|id| self.indices.get(&id))
                    .and_then(|i| self.photos[*i].metadata.dimensions)
            })
            .unwrap_or([1, 1])
    }
    fn streaming_eligible(&self) -> bool {
        self.selected[0].is_some()
            && !self.settings.highpass
            && self.selected.iter().flatten().all(|id| {
                !self.stream_fallback.contains(id)
                    && self.indices.get(id).is_some_and(|i| {
                        self.photos[*i]
                            .path
                            .extension()
                            .is_some_and(|e| e.eq_ignore_ascii_case("dng"))
                    })
            })
    }
    #[cfg(feature = "experimental-nef-progressive")]
    fn nef_preview_job(&self) -> Option<crate::streaming::Request> {
        if self.settings.highpass || self.settings.mode == Mode::Diff {
            return None;
        }
        let aid = self.selected[0]?;
        let bid = self.selected[1].unwrap_or(aid);
        if [aid, bid].iter().all(|id| self.frames.contains_key(id))
            || [aid, bid]
                .iter()
                .any(|id| self.decode_errors.contains_key(id))
        {
            return None;
        }
        let a = self.photos.get(*self.indices.get(&aid)?)?;
        let b = self.photos.get(*self.indices.get(&bid)?)?;
        if ![a, b].iter().any(|photo| {
            photo
                .path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("nef"))
        }) {
            return None;
        }
        Some(crate::streaming::Request {
            generation: self.generation,
            pair: [(aid, a.path.clone()), (bid, b.path.clone())],
            settings: self.settings.clone(),
            ready: [
                self.frames.get(&aid).cloned(),
                self.frames.get(&bid).cloned(),
            ],
        })
    }
    fn local_point(&self, p: Point<Pixels>) -> [f32; 2] {
        let origin = self.viewport_bounds.get().origin;
        [
            f32::from(p.x - origin.x) * self.display_scale,
            f32::from(p.y - origin.y) * self.display_scale,
        ]
    }
    fn show_help(&mut self, cx: &mut Context<Self>) {
        if let Some(handle) = self.help_window {
            if handle
                .update(cx, |_, window, _| window.activate_window())
                .is_ok()
            {
                return;
            }
        }
        match crate::help::open(cx) {
            Ok(handle) => self.help_window = Some(handle),
            Err(error) => self.message = format!("Could not open help: {error:#}"),
        }
    }
    fn show_settings(&mut self, cx: &mut Context<Self>) {
        if let Some(handle) = self.settings_window {
            if handle
                .update(cx, |_, window, _| window.activate_window())
                .is_ok()
            {
                return;
            }
        }
        match crate::settings_window::open(
            self.cache_settings.clone(),
            self.settings_sender.clone(),
            cx,
        ) {
            Ok(handle) => self.settings_window = Some(handle),
            Err(error) => self.message = format!("Could not open settings: {error:#}"),
        }
    }
    fn choose_mode(&mut self, mode: Mode, cx: &mut Context<Self>) {
        if !self
            .preferred_mode
            .request(mode, self.selected[1].is_some())
        {
            return;
        }
        self.settings.mode = mode;
        self.held = false;
        self.space_held = false;
        self.request_render();
        cx.notify();
    }
    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        let ctrl = event.keystroke.modifiers.control || event.keystroke.modifiers.platform;
        if ctrl && key == "o" {
            self.services.choose(event.keystroke.modifiers.shift);
            return;
        }
        match key {
            "c" if !ctrl => {
                self.toggle_picker(cx);
                return;
            }
            "l" if !ctrl => {
                self.library_open = !self.library_open;
                cx.notify();
                return;
            }
            "s" if !ctrl => {
                self.toggle_metrics(cx);
                return;
            }
            "f1" => {
                self.show_help(cx);
                return;
            }
            "f11" => {
                if !event.is_held {
                    window.toggle_fullscreen();
                }
                cx.notify();
                return;
            }
            "left" | "up" => {
                self.navigate(-1, cx);
                return;
            }
            "right" | "down" => {
                self.navigate(1, cx);
                return;
            }
            "home" => {
                self.navigate(-(self.photos.len() as isize), cx);
                return;
            }
            "end" => {
                self.navigate(self.photos.len() as isize, cx);
                return;
            }
            "1" | "2" | "3" => {
                let mode = match key {
                    "2" => Mode::Slider,
                    "3" => Mode::Diff,
                    _ => Mode::Click,
                };
                self.choose_mode(mode, cx);
                return;
            }
            "8" => self.settings.camera.zoom = Zoom::Fixed(8.),
            "f" | "0" => self.settings.camera.fit(),
            "escape" => {
                self.navigator_drag = None;
                self.held = false;
                self.space_held = false;
                self.pan_last = None;
                self.pan_button = None;
                self.split_drag = false;
                window.focus(&self.focus, cx);
                cx.notify();
                return;
            }
            _ => return,
        }
        self.request_render();
        cx.notify();
    }
    fn render_rows(&mut self, range: Range<usize>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        self.visible = range.clone();
        range
            .filter_map(|index| {
                let photo = self.photos.get(index)?;
                let id = photo.id;
                let name = photo.name.clone();
                let metadata = photo.metadata.label();
                let desk = cx.entity().downgrade();
                let a = self.selected[0] == Some(id);
                let b = self.selected[1] == Some(id);
                let mut preview = div()
                    .w(px(76.))
                    .h(px(56.))
                    .flex_none()
                    .bg(rgb(0x171719))
                    .rounded_md()
                    .overflow_hidden()
                    .flex()
                    .items_center()
                    .justify_center();
                if let Some(thumb) = self.thumbnails.get(&id).cloned() {
                    preview = preview.child(img(thumb).size_full().object_fit(ObjectFit::Contain));
                } else {
                    preview = preview.child(div().text_xs().text_color(rgb(MUTED)).child(
                        if self.thumb_errors.contains(&id) {
                            "RAW"
                        } else {
                            "···"
                        },
                    ));
                }
                Some(
                    div()
                        .id(("photo", id))
                        .h(px(82.))
                        .w_full()
                        .px_3()
                        .py_2()
                        .flex()
                        .gap_3()
                        .items_center()
                        .border_b_1()
                        .border_color(rgb(0x2b2b2e))
                        .bg(rgb(if a || b { 0x454549 } else { PANEL }))
                        .hover(move |s| s.bg(rgb(if a || b { 0x505054 } else { 0x303033 })))
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                            window.focus(&this.focus, cx);
                            this.select(
                                id,
                                event.modifiers().control || event.modifiers().platform,
                                cx,
                            );
                        }))
                        .on_drag(
                            DragPhoto {
                                id,
                                name: name.clone(),
                            },
                            |photo, _, _, cx| cx.new(|_| photo.clone()),
                        )
                        .on_drop(cx.listener(move |this, dragged: &DragPhoto, _, cx| {
                            if let (Some(from), Some(to)) =
                                (this.indices.get(&dragged.id), this.indices.get(&id))
                            {
                                model::move_photo(
                                    &mut this.photos,
                                    dragged.id,
                                    *to as isize - *from as isize,
                                );
                                this.reindex();
                                cx.notify();
                            }
                        }))
                        .child(preview)
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(div().text_sm().truncate().child(name))
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .child(
                                            div().text_xs().text_color(rgb(MUTED)).child(metadata),
                                        )
                                        .when(a || b, |el| {
                                            el.child(
                                                div()
                                                    .px_2()
                                                    .rounded_sm()
                                                    .text_xs()
                                                    .font_weight(FontWeight::SEMIBOLD)
                                                    .text_color(rgb(TEXT))
                                                    .child(if a { "A" } else { "B" }),
                                            )
                                        }),
                                ),
                        )
                        .context_menu(move |menu, _, _| {
                            let open = desk.clone();
                            let open_with = desk.clone();
                            let open_folder = desk.clone();
                            let copy_name = desk.clone();
                            let copy_path = desk.clone();
                            let set_a = desk.clone();
                            let set_b = desk.clone();
                            menu.item(PopupMenuItem::new("Open file").on_click(move |_, _, cx| {
                                let _ = open.update(cx, |this, _| this.open_photo(id, false));
                            }))
                            .item(PopupMenuItem::new("Open with…").on_click(move |_, _, cx| {
                                let _ = open_with.update(cx, |this, _| this.open_photo(id, true));
                            }))
                            .item(PopupMenuItem::new("Open containing folder").on_click(
                                move |_, _, cx| {
                                    let _ = open_folder
                                        .update(cx, |this, cx| this.open_photo_folder(id, cx));
                                },
                            ))
                            .separator()
                            .item(
                                PopupMenuItem::new("Copy file name").on_click(move |_, _, cx| {
                                    let _ = copy_name
                                        .update(cx, |this, cx| this.copy_photo_path(id, true, cx));
                                }),
                            )
                            .item(
                                PopupMenuItem::new("Copy full path").on_click(move |_, _, cx| {
                                    let _ = copy_path
                                        .update(cx, |this, cx| this.copy_photo_path(id, false, cx));
                                }),
                            )
                            .separator()
                            .item(
                                PopupMenuItem::new("Set as A").on_click(move |_, window, cx| {
                                    let _ = set_a.update(cx, |this, cx| {
                                        window.focus(&this.focus, cx);
                                        this.select_as(id, 0, cx);
                                    });
                                }),
                            )
                            .item(
                                PopupMenuItem::new("Set as B").on_click(move |_, window, cx| {
                                    let _ = set_b.update(cx, |this, cx| {
                                        window.focus(&this.focus, cx);
                                        this.select_as(id, 1, cx);
                                    });
                                }),
                            )
                        })
                        .into_any_element(),
                )
            })
            .collect()
    }
    fn header(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(control(
                Button::new("color-picker")
                    .label("Color picker")
                    .small()
                    .ghost()
                    .tooltip("Original A/B colors and a 1600% pixel loupe (C)")
                    .selected(self.picker_enabled)
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_picker(cx))),
            ))
            .child(control(
                Button::new("statistics")
                    .label("Statistics")
                    .small()
                    .ghost()
                    .tooltip("Comparison metrics and A/B noise estimates (S)")
                    .selected(self.metrics_open)
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_metrics(cx))),
            ))
            .child(control(
                Button::new("settings")
                    .label("Settings")
                    .small()
                    .ghost()
                    .tooltip("Memory, prefetch and disk preview cache settings")
                    .on_click(cx.listener(|this, _, _, cx| this.show_settings(cx))),
            ))
            .child(control(
                Button::new("help")
                    .label("Help")
                    .small()
                    .ghost()
                    .tooltip("Controls and help (F1); toggle fullscreen with F11")
                    .on_click(cx.listener(|this, _, _, cx| this.show_help(cx))),
            ))
            .into_any_element()
    }
    fn top_controls(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .absolute()
            .top_3()
            .left_3()
            .right_3()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_4()
            .when(!self.library_open, |el| {
                el.child(control(
                    Button::new("expand-library")
                        .label("☰")
                        .small()
                        .ghost()
                        .tooltip("Show the image library (L)")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.library_open = true;
                            cx.notify();
                        })),
                ))
            })
            .child(self.mode_toolbar(cx))
            .child(self.highpass_toolbar(cx))
            .child(self.header(cx))
            .into_any_element()
    }
    fn library(&self, cx: &mut Context<Self>) -> AnyElement {
        let entity = cx.entity().downgrade();
        div()
            .w(px(282.))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .bg(rgb(PANEL))
            .border_r_1()
            .border_color(rgb(LINE))
            .child(
                div()
                    .flex_none()
                    .px_3()
                    .py_3()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .border_b_1()
                    .border_color(rgb(LINE))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(img(crate::platform::app_icon()).size(px(20.)))
                            .child(div().font_weight(FontWeight::SEMIBOLD).child("Images"))
                            .child(div().text_xs().text_color(rgb(MUTED)).child(format!(
                                "{}{}",
                                self.photos.len(),
                                if self.scanning > 0 { " +" } else { "" }
                            )))
                            .child(
                                Button::new("collapse-library")
                                    .label("‹")
                                    .xsmall()
                                    .ghost()
                                    .tooltip("Hide the image library (L)")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.library_open = false;
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(
                        Button::new("add-files")
                            .label("Add images")
                            .small()
                            .w_full()
                            .on_click(cx.listener(|this, _, _, _| this.services.choose(false))),
                    )
                    .child(
                        Button::new("add-folder")
                            .label("Add folder")
                            .small()
                            .w_full()
                            .on_click(cx.listener(|this, _, _, _| this.services.choose(true))),
                    )
                    .when(!self.directory_roots.is_empty(), |el| {
                        el.child(
                            Button::new("refresh-folders")
                                .label(if self.refreshing {
                                    "Refreshing…"
                                } else {
                                    "Refresh"
                                })
                                .small()
                                .w_full()
                                .disabled(self.scanning > 0)
                                .on_click(cx.listener(|this, _, _, cx| this.refresh_library(cx))),
                        )
                    })
                    .child(
                        div().py_1().child(
                            Checkbox::new("recursive")
                                .label("Include subfolders")
                                .checked(self.recursive)
                                .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                    this.recursive = *checked;
                                    cx.notify();
                                })),
                        ),
                    )
                    .child(
                        Button::new("clear-files")
                            .label("Clear list")
                            .small()
                            .ghost()
                            .w_full()
                            .disabled(self.photos.is_empty() && self.scanning == 0)
                            .on_click(cx.listener(|this, _, _, cx| this.clear(cx))),
                    ),
            )
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(
                        uniform_list("photos", self.photos.len(), move |range, _, cx| {
                            entity
                                .update(cx, |this, cx| this.render_rows(range, cx))
                                .unwrap_or_default()
                        })
                        .track_scroll(&self.scroll)
                        .size_full(),
                    )
                    .child(Scrollbar::vertical(&self.scroll).mode(ScrollbarMode::Always)),
            )
            .child(
                div()
                    .px_3()
                    .py_2()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_2()
                    .border_t_1()
                    .border_color(rgb(LINE))
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .text_color(rgb(MUTED))
                            .child(format!(
                                "Thumbnails: {}/{}",
                                self.thumb_done.len(),
                                self.photos.len()
                            )),
                    )
                    .child(
                        Button::new("move-up")
                            .label("↑")
                            .tooltip("Move the focused image up")
                            .small()
                            .ghost()
                            .disabled(self.focused_photo.is_none())
                            .on_click(cx.listener(|this, _, _, cx| this.reorder(-1, cx))),
                    )
                    .child(
                        Button::new("move-down")
                            .label("↓")
                            .tooltip("Move the focused image down")
                            .small()
                            .ghost()
                            .disabled(self.focused_photo.is_none())
                            .on_click(cx.listener(|this, _, _, cx| this.reorder(1, cx))),
                    ),
            )
            .into_any_element()
    }
    fn statistics_overlay(&self, cx: &mut Context<Self>) -> AnyElement {
        let source_size = |slot: usize| {
            self.selected[slot].and_then(|id| {
                self.frames.get(&id).map(|frame| frame.size()).or_else(|| {
                    self.indices
                        .get(&id)
                        .and_then(|index| self.photos[*index].metadata.dimensions)
                })
            })
        };
        let (a_size, b_size) = (source_size(0), source_size(1));
        let size_controls = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_1()
            .children([512, 1536].map(|edge| {
                let (label, available) = statistics_size_choice(a_size, b_size, edge);
                Button::new(("metric-size", edge as usize))
                    .label(label)
                    .small()
                    .h(px(28.))
                    .ghost()
                    .disabled(!available)
                    .selected(self.metrics_edge == edge)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if this.metrics_edge != edge {
                            this.metrics_edge = edge;
                            this.reset_metrics();
                            this.schedule_metrics();
                            cx.notify();
                        }
                    }))
            }))
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child("Triangle · RGB"),
            );
        let mut panel = floating()
            .id("statistics-panel")
            .absolute()
            .top(px(112.))
            .right_3()
            .w(px(STATISTICS_WIDTH))
            .max_h(px((f32::from(self.viewport_bounds.get().size.height)
                - 310.)
                .max(160.)))
            .overflow_y_scroll()
            .p_3()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(section_label("STATISTICS · A → B"))
                    .child(
                        Button::new("close-statistics")
                            .label("×")
                            .small()
                            .h(px(28.))
                            .ghost()
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_metrics(cx))),
                    ),
            );
        match &self.metrics_result {
            Some(Ok(report)) => {
                let number = |v: Option<f64>, stage: metrics::Stage| {
                    if report.is_pending(stage) {
                        "…".into()
                    } else {
                        v.map(|v| format!("{v:.6}")).unwrap_or_else(|| "—".into())
                    }
                };
                let pixels = report.pixels.as_ref();
                let pixel_empty = if report.is_pending(metrics::Stage::Pixels) {
                    "…"
                } else {
                    "—"
                };
                let pixel_number = |f: fn(&metrics::PixelStats) -> f64| {
                    pixels
                        .map(|p| format!("{:.5}%", f(p) * 100.))
                        .unwrap_or_else(|| pixel_empty.into())
                };
                let comparison_notice = statistics_comparison_notice(
                    report.comparison_error.as_deref(),
                    report.original_size_b.is_some(),
                    self.selected[1].is_some(),
                    self.selected[1]
                        .and_then(|id| self.decode_errors.get(&id))
                        .map(String::as_str),
                );
                if let Some(error) = comparison_notice {
                    panel = panel.child(div().text_xs().text_color(rgb(0xe3bd87)).child(error));
                }
                panel = panel
                    .child(section_label("FULL RESOLUTION · RGB16"))
                    .child(div().text_xs().text_color(rgb(MUTED)).child(format!(
                        "{} × {}",
                        report.original_size[0], report.original_size[1]
                    )))
                    .child(metric_row(
                        "PSNR ↑",
                        pixels
                            .map(|p| {
                                p.psnr_db
                                    .map(|v| format!("{v:.3} dB"))
                                    .unwrap_or_else(|| "∞ dB".into())
                            })
                            .unwrap_or_else(|| pixel_empty.into()),
                    ))
                    .child(metric_row("MAE ↓", pixel_number(|p| p.mae)))
                    .child(metric_row("RMSE ↓", pixel_number(|p| p.rmse)))
                    .child(metric_row("Max. error ↓", pixel_number(|p| p.max_error)))
                    .child(metric_row(
                        "Bias B − A",
                        pixels
                            .map(|p| format!("{:+.5}%", p.bias_b_minus_a * 100.))
                            .unwrap_or_else(|| pixel_empty.into()),
                    ))
                    .child(metric_row(
                        "Changed pixels",
                        pixels
                            .map(|p| format!("{:.3}%", p.changed_percent))
                            .unwrap_or_else(|| pixel_empty.into()),
                    ));
                let noise_value = |v: Option<f64>, stage| {
                    if report.is_pending(stage) {
                        "…".into()
                    } else {
                        v.map(|v| format!("{:.4}%", v * 100.))
                            .unwrap_or_else(|| "—".into())
                    }
                };
                let noise_row = |label: &str, a: String, b: String| {
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .text_xs()
                        .child(div().flex_1().text_color(rgb(TEXT)).child(label.to_owned()))
                        .child(div().w(px(68.)).flex_none().child(a))
                        .child(div().w(px(68.)).flex_none().child(b))
                };
                panel = panel
                    .child(section_label("NOISE ESTIMATES · σY′"))
                    .child(noise_row("Method", "A".into(), "B".into()))
                    .child(noise_row(
                        "Haar HH · MAD",
                        noise_value(report.noise_a.haar_hh_mad, metrics::Stage::HaarA),
                        noise_value(report.noise_b.haar_hh_mad, metrics::Stage::HaarB),
                    ))
                    .child(noise_row(
                        "Immerkær",
                        noise_value(report.noise_a.immerkaer, metrics::Stage::ImmerkaerA),
                        noise_value(report.noise_b.immerkaer, metrics::Stage::ImmerkaerB),
                    ))
                    .child(noise_row(
                        "High-pass · MAD",
                        noise_value(report.noise_a.high_pass_mad, metrics::Stage::HighPassA),
                        noise_value(report.noise_b.high_pass_mad, metrics::Stage::HighPassB),
                    ))
                    .child(section_label("STRUCTURE AND PERCEPTION"))
                    .child(size_controls)
                    .child(metric_row(
                        "SSIM · Y ↑",
                        number(report.ssim_y, metrics::Stage::Ssim),
                    ))
                    .child(metric_row(
                        "MS-SSIM · Y ↑",
                        number(report.ms_ssim_y, metrics::Stage::MsSsim),
                    ))
                    .child(metric_row(
                        "GMSD · Y ↓",
                        number(report.gmsd_y, metrics::Stage::Gmsd),
                    ))
                    .child(metric_row(
                        "SSIMULACRA2 ↑",
                        if report.is_pending(metrics::Stage::Ssimulacra2) {
                            "…".into()
                        } else {
                            report
                                .ssimulacra2
                                .as_ref()
                                .map(|v| format!("{v:.4}"))
                                .unwrap_or_else(|_| "—".into())
                        },
                    ))
                    .child(section_label("NEURAL"))
                    .child(div().text_xs().text_color(rgb(MUTED)).child(format!(
                        "LPIPS: {} × {} · ONNX Runtime · CPU",
                        report.neural_size[0], report.neural_size[1]
                    )));
                if report.comparison_error.is_some() {
                    panel = panel.child(metric_row("LPIPS · Squeeze v0.1 ↓", "—".into()));
                } else if report.neural_pending {
                    panel = panel.child(metric_row("LPIPS · Squeeze v0.1 ↓", "…".into()));
                } else {
                    match &report.neural {
                        Ok(result) => {
                            panel = panel.child(metric_row(
                                "LPIPS · Squeeze v0.1 ↓",
                                format!("{:.6}", result.lpips),
                            ));
                        }
                        Err(error) => {
                            panel = panel
                                .child(div().text_xs().text_color(rgb(MUTED)).child(error.clone()));
                        }
                    }
                }
                if report.comparison_error.is_none()
                    && !report.is_pending(metrics::Stage::MsSsim)
                    && report.ms_ssim_y.is_none()
                {
                    panel =
                        panel.child(div().text_xs().text_color(rgb(MUTED)).child(
                            "MS-SSIM requires at least 176 px on each axis for five scales.",
                        ));
                }
                if report.comparison_error.is_none()
                    && !report.is_pending(metrics::Stage::Ssimulacra2)
                {
                    if let Err(error) = &report.ssimulacra2 {
                        panel = panel
                            .child(div().text_xs().text_color(rgb(MUTED)).child(error.clone()));
                    }
                }
                panel = panel.child(div().text_xs().text_color(rgb(MUTED)).child(format!(
                    "{} · {:.0} ms · ↑ higher is better · ↓ lower is better",
                    if report.is_complete() {
                        "Done".into()
                    } else {
                        format!("Stages remaining: {}", report.pending.len())
                    },
                    report.elapsed_ms
                )));
            }
            Some(Err(error)) => {
                panel = panel.child(div().text_color(rgb(0xe3bd87)).child(error.clone()))
            }
            None => {
                panel = panel.child(if self.selected[0].is_none() {
                    "Choose image A for noise estimates; B for comparison."
                } else {
                    "Waiting for decoding / computing…"
                });
                if self.selected[0].is_some() {
                    panel = panel
                        .child(section_label("STRUCTURE AND PERCEPTION"))
                        .child(size_controls);
                }
            }
        }
        panel.into_any_element()
    }
    fn mode_toolbar(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .children(Mode::ALL.map(|mode| {
                floating()
                    .h(px(38.))
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(
                        Button::new(mode.label())
                            .label(mode.label())
                            .small()
                            .h(px(28.))
                            .ghost()
                            .tooltip(mode.tooltip())
                            .selected(self.settings.mode == mode)
                            .disabled(!mode.available(self.selected[1].is_some()))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.choose_mode(mode, cx);
                            })),
                    )
                    .when(
                        mode == Mode::Diff && self.settings.mode == Mode::Diff,
                        |el| {
                            el.child(div().flex().items_center().gap_0p5().ml_1().children(
                                [1, 4, 16, 64, 256].map(|gain| {
                                    Button::new(("gain", gain as usize))
                                        .label(format!("{gain}×"))
                                        .small()
                                        .h(px(28.))
                                        .ghost()
                                        .selected(self.settings.gain == gain)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.settings.gain = gain;
                                            this.request_render();
                                            cx.notify();
                                        }))
                                }),
                            ))
                        },
                    )
            }))
            .into_any_element()
    }
    fn highpass_toolbar(&self, cx: &mut Context<Self>) -> AnyElement {
        floating()
            .h(px(38.))
            .flex()
            .items_center()
            .gap_3()
            .child(
                Button::new("highpass")
                    .label("High pass filter")
                    .small()
                    .h(px(28.))
                    .ghost()
                    .selected(self.settings.highpass)
                    .tooltip("Compare high-frequency detail: original minus Gaussian blur")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.settings.highpass = !this.settings.highpass;
                        this.request_render();
                        cx.notify();
                    })),
            )
            .when(self.settings.highpass, |el| {
                el.child(
                    div().w(px(100.)).child(
                        Slider::new(&self.radius)
                            .accessibility_label("High pass filter: radius σ")
                            .w_full(),
                    ),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(MUTED))
                        .child(format!("σ {:.1}px", self.settings.sigma)),
                )
            })
            .into_any_element()
    }
    fn picker_overlay(&self) -> AnyElement {
        let [a, b] = self.picker_samples.unwrap();
        let has_b = self.selected[1].is_some();
        let point = self.picker_point.unwrap();
        let view = [
            self.settings.viewport[0] as f32 / self.display_scale,
            self.settings.viewport[1] as f32 / self.display_scale,
        ];
        let point = point.map(|v| v / self.display_scale);
        let [x, y] = color::popup_position(point, view, PICKER_SIZE);
        glass()
            .absolute()
            .left(px(x))
            .top(px(y))
            .w(px(PICKER_SIZE[0]))
            .h(px(PICKER_SIZE[1]))
            .overflow_hidden()
            .p_3()
            .flex()
            .flex_col()
            .gap_1()
            .child(section_label("COLOR PICKER · sRGB / D65"))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        pixel_loupe(self.picker_loupe.clone().unwrap(), self.display_scale)
                            .w(px(color::LOUPE_WINDOW[0]))
                            .h(px(color::LOUPE_WINDOW[1])),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(div().flex().text_xs()
                                .child(div().flex_1().child(format!("A · {}, {}", a.x, a.y)))
                                .when(has_b, |el| el.child(div().flex_1().child(format!("B · {}, {}", b.x, b.y)))))
                            .child(comparison_swatch(a.rgb, has_b.then_some(b.rgb))
                                .w_full().h(px(PICKER_SWATCH_HEIGHT)).flex_none())
                            .when(!has_b, |el| el.child(div().text_xs().text_color(rgb(MUTED)).child("B — not selected")))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(MUTED))
                                    .child("Loupe: 1600%, nearest pixels, including Diff/High pass. Values: original RGB16."),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child(div().w(px(56.)))
                    .child(div().flex_1().child("A"))
                    .child(div().flex_1().child("B")),
            )
            .children(
                color::formats(a.rgb)
                    .into_iter()
                    .zip(color::formats(b.rgb))
                    .map(|((name, a), (_, b))| {
                        div()
                            .flex()
                            .gap_2()
                            .text_xs()
                            .child(
                                div()
                                    .w(px(56.))
                                    .flex_none()
                                    .text_color(rgb(MUTED))
                                    .child(name),
                            )
                            .child(div().flex_1().min_w_0().child(a))
                            .child(div().flex_1().min_w_0().child(if has_b {
                                b
                            } else {
                                "—".into()
                            }))
                    }),
            )
            .into_any_element()
    }
    fn viewer(&self, cx: &mut Context<Self>) -> AnyElement {
        let cell = self.viewport_bounds.clone();
        let displayed = self
            .displayed_view
            .as_ref()
            .filter(|view| Some(view.content) == self.current_render_content());
        let a = displayed.and(self.primary.clone());
        let b = displayed.and(self.alternate.clone());
        let transform = displayed.map(|view| {
            if view.source_texture {
                self.settings
                    .camera
                    .source_transform(view.image_size, self.settings.viewport)
            } else {
                self.settings.camera.viewport_transform(
                    &view.settings.camera,
                    view.image_size,
                    self.settings.viewport,
                    view.settings.viewport,
                )
            }
        });
        let display_scale = self.display_scale;
        let has_image = a.is_some();
        let mode = self.settings.mode;
        let split = self.settings.split;
        let show_b = mode == Mode::Click && (self.held || self.space_held);
        let failed = self
            .selected
            .iter()
            .flatten()
            .any(|id| self.decode_errors.contains_key(id));
        let mut viewport = div()
            .id("viewport")
            .relative()
            .flex_1()
            .min_w_0()
            .h_full()
            .min_h_0()
            .w_full()
            .overflow_hidden()
            .bg(rgb(0x161618))
            .cursor_grab()
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                if this.picker_enabled && this.pan_last.is_none() && !this.split_drag {
                    this.picker_point = Some(this.local_point(event.position));
                    this.picker_dirty = true;
                    if this.picker_updated.elapsed() >= Duration::from_millis(16) {
                        this.update_picker();
                        cx.notify();
                    }
                }
            }))
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                if !hovered && this.picker_enabled {
                    this.picker_point = None;
                    this.clear_picker();
                    cx.notify();
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    window.focus(&this.focus, cx);
                    let p = this.local_point(event.position);
                    if this.settings.mode == Mode::Click {
                        this.held = true;
                        this.pan_last = Some(event.position);
                        this.pan_button = Some(MouseButton::Left);
                        this.update_picker();
                    } else if this.settings.mode == Mode::Slider
                        && (p[0] - this.settings.split * this.settings.viewport[0] as f32).abs()
                            < 18. * this.display_scale
                    {
                        this.split_drag = true;
                        this.clear_picker();
                    } else {
                        this.pan_last = Some(event.position);
                        this.pan_button = Some(MouseButton::Left);
                        this.clear_picker();
                    }
                    cx.notify();
                }),
            )
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    window.focus(&this.focus, cx);
                    this.pan_last = Some(event.position);
                    this.pan_button = Some(MouseButton::Middle);
                    this.clear_picker();
                    cx.notify();
                }),
            )
            .on_scroll(cx.listener(|this, event: &ScrollEvent, _, cx| {
                let point = this.local_point(event.position);
                this.zoom_camera(event, point, Some(point), cx);
            }))
            .child(
                canvas(
                    move |bounds, _, _| cell.set(bounds),
                    move |bounds, _, window, _| {
                        if let Some(a) = a {
                            let image_bounds =
                                transformed_image_bounds(bounds, transform, display_scale);
                            // Both complete textures are uploaded on the first paint, even
                            // when one is covered. Divider motion only changes clipping.
                            if let Some(b) = b {
                                if show_b {
                                    let _ = window.paint_image(
                                        bounds,
                                        image_bounds,
                                        Corners::default(),
                                        a,
                                        0,
                                        false,
                                    );
                                    let _ = window.paint_image(
                                        bounds,
                                        image_bounds,
                                        Corners::default(),
                                        b,
                                        0,
                                        false,
                                    );
                                } else {
                                    let _ = window.paint_image(
                                        bounds,
                                        image_bounds,
                                        Corners::default(),
                                        b.clone(),
                                        0,
                                        false,
                                    );
                                    let _ = window.paint_image(
                                        bounds,
                                        image_bounds,
                                        Corners::default(),
                                        a,
                                        0,
                                        false,
                                    );
                                    if mode == Mode::Slider {
                                        let _ = window.paint_image(
                                            right_clip(bounds, split),
                                            image_bounds,
                                            Corners::default(),
                                            b,
                                            0,
                                            false,
                                        );
                                    }
                                }
                            } else {
                                let _ = window.paint_image(
                                    bounds,
                                    image_bounds,
                                    Corners::default(),
                                    a,
                                    0,
                                    false,
                                );
                            }
                        }
                    },
                )
                .size_full(),
            );
        if !has_image {
            let text = if failed {
                "Could not read the image"
            } else if self.rendering {
                "Decoding…"
            } else if self.photos.is_empty() {
                "The image library is empty"
            } else {
                "No images selected"
            };
            viewport = viewport.child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(rgb(MUTED))
                    .child(text),
            );
        }
        if failed && has_image {
            viewport = viewport.child(
                div()
                    .absolute()
                    .top_12()
                    .left_4()
                    .right_4()
                    .p_3()
                    .rounded_md()
                    .bg(rgb(0x4b3029))
                    .text_color(rgb(0xf2c4b8))
                    .child("Could not read the image. Showing the previous result."),
            );
        }
        if mode == Mode::Slider && has_image {
            viewport = viewport.child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(relative(split))
                    .w(px(1.))
                    .bg(rgb(0xe5e5e7))
                    .child(
                        div()
                            .absolute()
                            .top(relative(0.5))
                            .left(px(-13.))
                            .w(px(27.))
                            .h(px(38.))
                            .rounded_lg()
                            .bg(rgb(0xe5e5e7))
                            .text_color(rgb(0x343436))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child("↔"),
                    ),
            );
        }
        if has_image {
            viewport = viewport
                .child(viewer_badge(if show_b { "B" } else { "A" }, true))
                .when(mode == Mode::Slider || mode == Mode::Diff, |el| {
                    el.child(viewer_badge("B", false))
                });
        }
        let mismatch = self.selected[0]
            .and_then(|id| self.frames.get(&id))
            .zip(self.selected[1].and_then(|id| self.frames.get(&id)))
            .is_some_and(|(a, b)| a.size() != b.size());
        viewport=viewport
            .child(self.top_controls(cx))
            .when(mismatch,|el|el.child(floating().absolute().bottom(px(106.)).left_3().right(px(260.)).px_3()
                .text_xs().text_color(rgb(0xe3bd87)).child("Different resolutions: B is scaled to A for viewing. Comparison metrics require equal dimensions.")))
            .when(self.metrics_open,|el|el.child(self.statistics_overlay(cx)))
            .child(div().absolute().bottom_3().left_3()
                .when(self.navigator_ready(), |el| el.right(px(260.)))
                .when(!self.navigator_ready(), |el| el.right_3())
                .flex().flex_col().items_start().gap_2()
                .child(floating().max_w_full().px_2().text_xs().text_color(rgb(MUTED))
                    .child(div().max_w_full().truncate().child(self.backend.clone()))
                    .when(!self.stream_status.is_empty(),|el|el.child(div().max_w_full().truncate().child(self.stream_status.clone())))
                    .when(!self.message.is_empty(),|el|el.child(div().max_w_full().truncate().child(self.message.clone()))))
                .child(self.zoom_toolbar(cx)))
            .when(self.navigator_ready(), |el| el.child(self.navigator_overlay(cx)));
        if self.picker_enabled && self.picker_samples.is_some() && self.picker_loupe.is_some() {
            viewport = viewport.child(self.picker_overlay());
        }
        viewport.into_any_element()
    }
    fn zoom_toolbar(&self, cx: &mut Context<Self>) -> AnyElement {
        let has_image = self.primary.is_some();
        let zoom = self
            .settings
            .camera
            .scale(self.image_size(), self.settings.viewport)
            * 100.;
        div()
            .max_w_full()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .child(control(
                Button::new("fit")
                    .label("Fit to screen")
                    .small()
                    .ghost()
                    .selected(self.settings.camera.zoom == Zoom::Fit)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.settings.camera.fit();
                        this.request_render();
                        cx.notify();
                    })),
            ))
            .children([1., 2., 4., 8.].map(|scale| {
                control(
                    Button::new(("zoom", scale as usize))
                        .label(format!("{}%", (scale * 100.) as u32))
                        .small()
                        .ghost()
                        .selected(self.settings.camera.zoom == Zoom::Fixed(scale))
                        .disabled(
                            scale
                                < crate::model::Camera::zoom_limits(
                                    self.image_size(),
                                    self.settings.viewport,
                                )[0],
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.settings.camera.zoom = Zoom::Fixed(scale);
                            this.request_render();
                            cx.notify();
                        })),
                )
            }))
            .child(
                div()
                    .ml_3()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child(if has_image {
                        format!("{zoom:.0}%")
                    } else {
                        "—".into()
                    }),
            )
            .child(
                div()
                    .ml_3()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child(if self.rendering {
                        "Processing…".into()
                    } else if has_image {
                        format!("{:.1} ms", self.elapsed_ms)
                    } else {
                        String::new()
                    }),
            )
            .into_any_element()
    }
}

impl Render for Desk {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.display_scale = window.scale_factor();
        let size = window.viewport_size();
        if !window.is_fullscreen() {
            self.window_size = [f32::from(size.width), f32::from(size.height)];
        }
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(BG))
            .text_color(rgb(TEXT))
            .text_sm()
            .font_family("Segoe UI")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "space" && this.settings.mode == Mode::Click {
                    this.space_held = true;
                    this.update_picker();
                    cx.notify();
                }
            }))
            .on_key_up(cx.listener(|this, event: &KeyUpEvent, _, cx| {
                if event.keystroke.key == "space" {
                    this.space_held = false;
                    this.update_picker();
                    cx.notify();
                }
            }))
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                if this.navigator_drag.is_some() {
                    if event.pressed_button == Some(MouseButton::Left) {
                        this.move_navigator(event.position, cx);
                        return;
                    }
                    this.navigator_drag = None;
                }
                if event.pressed_button.is_none() {
                    let changed = this.held || this.pan_last.is_some() || this.split_drag;
                    this.held = false;
                    this.pan_last = None;
                    this.pan_button = None;
                    this.split_drag = false;
                    if changed {
                        this.update_picker();
                        cx.notify();
                    }
                    return;
                }
                if this.split_drag {
                    this.settings.split = (this.local_point(event.position)[0]
                        / this.settings.viewport[0] as f32)
                        .clamp(0., 1.);
                    if this.streaming_eligible()
                        && this
                            .selected
                            .iter()
                            .flatten()
                            .any(|id| !this.frames.contains_key(id))
                    {
                        this.last_camera_change = Some(Instant::now());
                    }
                    // Complete views only change clipping. Loading views also
                    // reprioritize missing tiles around the new divider.
                    cx.notify();
                } else if let Some(last) = this
                    .pan_last
                    .filter(|_| this.pan_button == event.pressed_button)
                {
                    let delta = [
                        f32::from(event.position.x - last.x) * this.display_scale,
                        f32::from(event.position.y - last.y) * this.display_scale,
                    ];
                    this.pan_last = Some(event.position);
                    let old = this.settings.camera;
                    this.settings
                        .camera
                        .pan(delta, this.image_size(), this.settings.viewport);
                    if this.settings.camera != old || this.last_camera_change.is_some() {
                        this.preview_camera_change();
                        this.clear_picker();
                        cx.notify();
                    }
                }
            }))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                this.add_paths(paths.0.to_vec());
                cx.notify();
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.navigator_drag = None;
                    this.held = false;
                    if this.pan_button == Some(MouseButton::Left) {
                        this.pan_last = None;
                        this.pan_button = None;
                    }
                    this.split_drag = false;
                    this.update_picker();
                    cx.notify();
                }),
            )
            .on_mouse_up(
                MouseButton::Middle,
                cx.listener(|this, _, _, cx| {
                    if this.pan_button == Some(MouseButton::Middle) {
                        this.pan_last = None;
                        this.pan_button = None;
                    }
                    this.update_picker();
                    cx.notify();
                }),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .when(self.library_open, |el| el.child(self.library(cx)))
                    .child(self.viewer(cx)),
            )
    }
}

fn center_open_with_row(scroll: &UniformListScrollHandle, index: usize) {
    // Non-strict scrolling would leave an already visible row off-center.
    scroll.scroll_to_item_strict(index, ScrollStrategy::Center);
}

fn camera_render_due(last_change: Option<Instant>, now: Instant) -> bool {
    last_change.is_some_and(|time| now.saturating_duration_since(time) >= CAMERA_IDLE)
}
fn neighbor_prefetch_allowed(
    selected: [Option<PhotoId>; 2],
    statistics_open: bool,
    enabled: bool,
) -> bool {
    enabled && selected[0].is_some() && !statistics_open
}
fn map_bounds(origin: Point<Pixels>, rect: minimap::Rect) -> Bounds<Pixels> {
    Bounds {
        origin: origin + point(px(rect.origin[0]), px(rect.origin[1])),
        size: size(px(rect.size[0]), px(rect.size[1])),
    }
}

fn accepts_snapshot(
    generation: u64,
    requested: u64,
    valid_from: u64,
    displayed: Option<u64>,
) -> bool {
    generation >= valid_from
        && generation <= requested
        && displayed.is_none_or(|shown| generation > shown)
}

fn transformed_image_bounds(
    bounds: Bounds<Pixels>,
    transform: Option<model::ViewportTransform>,
    display_scale: f32,
) -> Bounds<Pixels> {
    match transform {
        Some(transform) => Bounds {
            origin: point(
                bounds.origin.x + px(transform.origin[0] / display_scale),
                bounds.origin.y + px(transform.origin[1] / display_scale),
            ),
            size: size(
                px(transform.size[0] / display_scale),
                px(transform.size[1] / display_scale),
            ),
        },
        None => bounds,
    }
}

fn right_clip(bounds: Bounds<Pixels>, split: f32) -> Bounds<Pixels> {
    let offset = bounds.size.width * split.clamp(0., 1.);
    Bounds {
        origin: point(bounds.origin.x + offset, bounds.origin.y),
        size: size(bounds.size.width - offset, bounds.size.height),
    }
}
fn section_label(text: &'static str) -> AnyElement {
    div()
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(MUTED))
        .child(text)
        .into_any_element()
}
fn glass() -> Div {
    div()
        .min_w_0()
        .p_1()
        .rounded_lg()
        .border_1()
        .border_color(rgba(0x65656bcc))
        .bg(rgba(0x2b2b2eef))
}
fn floating() -> Div {
    glass()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_mouse_down(MouseButton::Middle, |_, _, cx| cx.stop_propagation())
        .on_scroll(|_, _, cx| cx.stop_propagation())
        .on_mouse_move(|_, _, cx| cx.stop_propagation())
}
fn control(button: Button) -> Div {
    floating()
        .h(px(38.))
        .flex()
        .items_center()
        .child(button.h(px(28.)))
}
fn pixel_loupe(loupe: Arc<color::Loupe>, display_scale: f32) -> Canvas<()> {
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let [w, h] = loupe.size;
            let cell_size = color::LOUPE_ZOOM / display_scale;
            let origin = bounds.origin
                + point(
                    (bounds.size.width - px(w as f32 * cell_size)) * 0.5,
                    (bounds.size.height - px(h as f32 * cell_size)) * 0.5,
                );
            // Paint solid quads instead of a bilinearly sampled texture. One
            // original pixel is always 16 physical pixels, at every viewport zoom.
            window.with_content_mask(Some(ContentMask { bounds }), |window| {
                for (i, &[r, g, b]) in loupe.colors.iter().enumerate() {
                    let cell = Bounds {
                        origin: origin
                            + point(
                                px((i % w) as f32 * cell_size),
                                px((i / w) as f32 * cell_size),
                            ),
                        size: size(px(cell_size), px(cell_size)),
                    };
                    window.paint_quad(fill(
                        cell,
                        rgb((r as u32) << 16 | (g as u32) << 8 | b as u32),
                    ));
                }
                let cell = Bounds {
                    origin: origin
                        + point(
                            px((w / 2) as f32 * cell_size),
                            px((h / 2) as f32 * cell_size),
                        ),
                    size: size(px(cell_size), px(cell_size)),
                };
                window.paint_quad(outline(cell, rgb(0xffffff), BorderStyle::Solid));
            });
        },
    )
}
fn comparison_swatch(a: [u16; 3], b: Option<[u16; 3]>) -> Canvas<()> {
    let packed = |sample| {
        let [r, g, b] = color::rgb8(sample);
        (r as u32) << 16 | (g as u32) << 8 | b as u32
    };
    let a = packed(a);
    let b = b.map(packed);
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            // One continuous field: A fills it, B covers the right half. No gap,
            // per-swatch outline or rounded inner edge can hide the color boundary.
            window.paint_quad(fill(bounds, rgb(a)));
            if let Some(b) = b {
                window.paint_quad(fill(right_clip(bounds, 0.5), rgb(b)));
            }
        },
    )
}
fn statistics_size_choice(a: Option<[u32; 2]>, b: Option<[u32; 2]>, edge: u32) -> (String, bool) {
    match (a, b) {
        (Some(a), Some(b)) if a == b && !a.contains(&0) => {
            let [w, h] = metrics::analysis_size(a, edge);
            (format!("{w}×{h}"), true)
        }
        // Do not invent a paired output size for missing, unknown or unequal
        // sources. Header dimensions may enable the choice while decoding.
        _ => ("—×—".into(), false),
    }
}
fn statistics_comparison_notice(
    comparison_error: Option<&str>,
    b_measured: bool,
    b_selected: bool,
    b_decode_error: Option<&str>,
) -> Option<String> {
    if !b_measured && b_selected {
        Some(match b_decode_error {
            Some(error) => format!("B could not be decoded: {error}"),
            None => "Waiting for B to decode…".into(),
        })
    } else {
        comparison_error.map(str::to_owned)
    }
}
#[cfg(test)]
mod statistics_tests {
    use super::{statistics_comparison_notice, statistics_size_choice};

    #[test]
    fn size_choices_show_actual_portrait_dimensions_without_upscaling() {
        let portrait = Some([4000, 6000]);
        assert_eq!(
            statistics_size_choice(portrait, portrait, 512),
            ("341×512".into(), true)
        );
        assert_eq!(
            statistics_size_choice(portrait, portrait, 1536),
            ("1024×1536".into(), true)
        );
        let small = Some([128, 64]);
        assert_eq!(
            statistics_size_choice(small, small, 512),
            ("128×64".into(), true)
        );
        assert_eq!(
            statistics_size_choice(small, small, 1536),
            ("128×64".into(), true)
        );
    }

    #[test]
    fn size_choices_never_invent_dimensions_for_unknown_or_incompatible_sources() {
        for (a, b) in [
            (None, None),
            (Some([4000, 6000]), None),
            (None, Some([4000, 6000])),
            (Some([4000, 6000]), Some([6000, 4000])),
            (Some([0, 6000]), Some([0, 6000])),
        ] {
            for edge in [512, 1536] {
                assert_eq!(statistics_size_choice(a, b, edge), ("—×—".into(), false));
            }
        }
    }

    #[test]
    fn a_selected_b_is_waiting_or_failed_not_a_request_to_choose_b() {
        let choose_b = Some("Choose B to compare.");
        assert_eq!(
            statistics_comparison_notice(choose_b, false, false, None),
            choose_b.map(str::to_owned)
        );
        assert_eq!(
            statistics_comparison_notice(choose_b, false, true, None).as_deref(),
            Some("Waiting for B to decode…")
        );
        assert_eq!(
            statistics_comparison_notice(choose_b, false, true, Some("Unsupported image"))
                .as_deref(),
            Some("B could not be decoded: Unsupported image")
        );
        assert_eq!(statistics_comparison_notice(None, true, true, None), None);
        assert_eq!(
            statistics_comparison_notice(Some("Different resolutions"), true, true, None)
                .as_deref(),
            Some("Different resolutions")
        );
    }
}
fn metric_row(label: &'static str, value: String) -> AnyElement {
    div()
        .w_full()
        .flex()
        .items_center()
        .justify_between()
        .gap_2()
        .text_xs()
        .child(div().text_color(rgb(TEXT)).child(label))
        .child(value)
        .into_any_element()
}
fn viewer_badge(label: &'static str, left: bool) -> AnyElement {
    div()
        .absolute()
        .top(px(66.))
        .when(left, |el| el.left_4())
        .when(!left, |el| el.right_4())
        .px_3()
        .py_1()
        .rounded_md()
        .bg(rgba(0x242426dd))
        .text_xs()
        .child(label)
        .into_any_element()
}
#[derive(Clone)]
struct DragPhoto {
    id: PhotoId,
    name: String,
}
impl Render for DragPhoto {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_4()
            .py_2()
            .rounded_md()
            .bg(rgb(0x454549))
            .text_color(rgb(TEXT))
            .text_sm()
            .child(self.name.clone())
    }
}
#[cfg(test)]
mod tests {
    use super::{
        accepts_snapshot, camera_render_due, center_open_with_row, neighbor_prefetch_allowed,
        right_clip, transformed_image_bounds, RenderContent, CAMERA_IDLE,
    };
    use crate::model::{Mode, Settings, ViewportTransform};
    use gpui_kit::{point, px, size, Bounds, ScrollStrategy, UniformListScrollHandle};
    use std::time::{Duration, Instant};
    #[test]
    fn association_scroll_centers_even_when_the_requested_row_is_already_visible() {
        let scroll = UniformListScrollHandle::new();
        for index in [6, 127] {
            center_open_with_row(&scroll, index);
            let state = scroll.0.borrow();
            let request = state.deferred_scroll_to_item.as_ref().unwrap();
            assert_eq!(request.item_index, index);
            assert_eq!(request.strategy, ScrollStrategy::Center);
            assert!(request.scroll_strict);
            assert_eq!(request.offset, 0);
        }
    }
    #[test]
    fn neighbors_require_explicit_prefetch_and_statistics_have_priority() {
        for selected in [[None, None], [Some(1), None], [None, Some(2)]] {
            assert!(!neighbor_prefetch_allowed(selected, false, false));
            assert!(!neighbor_prefetch_allowed(selected, true, true));
        }
        assert!(neighbor_prefetch_allowed([Some(1), Some(2)], false, true));
        assert!(neighbor_prefetch_allowed([Some(1), None], false, true));
        assert!(!neighbor_prefetch_allowed([Some(1), Some(2)], false, false));
        assert!(!neighbor_prefetch_allowed([Some(1), Some(2)], true, true));
    }
    #[test]
    fn divider_clips_without_resizing_image() {
        let bounds = Bounds {
            origin: point(px(100.), px(50.)),
            size: size(px(1000.), px(600.)),
        };
        let clip = right_clip(bounds, 0.75);
        assert_eq!(clip.origin.x, px(850.));
        assert_eq!(clip.size.width, px(250.));
        assert_eq!(clip.size.height, bounds.size.height);
        assert_eq!(right_clip(bounds, 0.).size.width, bounds.size.width);
        assert_eq!(right_clip(bounds, 1.).size.width, px(0.));
    }
    #[test]
    fn joined_picker_swatch_uses_a_shared_boundary_at_fractional_dpi() {
        for dpi in [1., 1.25, 1.5, 2.] {
            let bounds = Bounds {
                origin: point(px(11. / dpi), px(17. / dpi)),
                size: size(px(219. / dpi), px(super::PICKER_SWATCH_HEIGHT)),
            };
            // A is the whole background, not a separately inset rectangle. B
            // covers its right half without creating an unpainted inner border.
            let b = right_clip(bounds, 0.5);
            assert_eq!(b.origin.x, bounds.origin.x + bounds.size.width * 0.5);
            assert!(
                f32::from(b.origin.x + b.size.width - bounds.origin.x - bounds.size.width).abs()
                    < 0.0001
            );
            assert_eq!(b.origin.y, bounds.origin.y);
            assert_eq!(b.size.height, bounds.size.height);
        }
        assert!(super::PICKER_SWATCH_HEIGHT > 2. * 22.);
    }
    #[test]
    fn texture_motion_uses_physical_snapshot_pixels_at_high_dpi() {
        let viewport = Bounds {
            origin: point(px(100.), px(50.)),
            size: size(px(600.), px(450.)),
        };
        let image = transformed_image_bounds(
            viewport,
            Some(ViewportTransform {
                origin: [-300., 120.],
                size: [1800., 1200.],
            }),
            2.,
        );
        assert_eq!(image.origin, point(px(-50.), px(110.)));
        assert_eq!(image.size, size(px(900.), px(600.)));
        // The complete A/B images share image bounds; their screen-space divider
        // is clipped against the viewport, not the moving/scaling image.
        let clip = right_clip(viewport, 0.75);
        assert_eq!(clip.origin.x, px(550.));
        assert_eq!(clip.size.width, px(150.));
        assert_eq!(transformed_image_bounds(viewport, None, 2.), viewport);
    }
    #[test]
    fn camera_debounce_waits_ten_ms_from_the_latest_change() {
        let start = Instant::now();
        assert!(!camera_render_due(None, start + CAMERA_IDLE));
        assert!(!camera_render_due(
            Some(start),
            start + Duration::from_millis(9)
        ));
        assert!(camera_render_due(Some(start), start + CAMERA_IDLE));
        let latest = start + Duration::from_millis(8);
        assert!(!camera_render_due(
            Some(latest),
            start + Duration::from_millis(17)
        ));
        assert!(camera_render_due(
            Some(latest),
            start + Duration::from_millis(18)
        ));
    }
    #[test]
    fn valid_in_flight_snapshots_can_rebase_a_gesture_without_going_backwards() {
        assert!(accepts_snapshot(11, 13, 10, Some(10)));
        assert!(accepts_snapshot(12, 13, 10, Some(11)));
        assert!(!accepts_snapshot(11, 13, 10, Some(12)));
        assert!(!accepts_snapshot(12, 13, 13, Some(11)));
        assert!(!accepts_snapshot(14, 13, 10, None));
        assert!(!accepts_snapshot(13, 13, 10, Some(13)));
    }
    #[test]
    fn only_source_or_render_content_changes_invalidate_camera_snapshots() {
        let settings = Settings::default();
        let original = RenderContent::new([1, 2], &settings);
        let mut moved = settings.clone();
        moved.camera.center = [0.2, 0.8];
        moved.viewport = [2000, 1000];
        moved.mode = Mode::Click;
        moved.split = 0.8;
        moved.gain = 64;
        moved.sigma = 10.;
        assert!(original == RenderContent::new([1, 2], &moved));
        assert!(original != RenderContent::new([1, 3], &moved));
        moved.highpass = true;
        let filtered = RenderContent::new([1, 2], &moved);
        assert!(original != filtered);
        moved.sigma = 3.;
        assert!(filtered != RenderContent::new([1, 2], &moved));
        moved.mode = Mode::Diff;
        let diff = RenderContent::new([1, 2], &moved);
        moved.gain = 16;
        assert!(diff != RenderContent::new([1, 2], &moved));
    }
    #[test]
    fn picker_samples_the_current_camera_after_wheel_clamping_at_each_dpi() {
        use crate::{color, decode::Frame, model::Zoom};
        let frame = Frame {
            width: 640,
            height: 480,
            rgb: vec![12345; 640 * 480 * 3],
        };
        for dpi in [1., 2.] {
            let mut settings = Settings::default();
            settings.viewport = [1000, 750].map(|n| (n as f32 * dpi) as u32);
            settings.camera.zoom = Zoom::Fixed(4. * dpi);
            settings.camera.center = [0.02, 0.5];
            settings.camera.clamp(frame.size(), settings.viewport);
            let point = [750. * dpi, 300. * dpi];
            let before = color::sample_pair(&frame, &frame, &settings, point).unwrap()[0];
            let old_content = RenderContent::new([1, 1], &settings);
            settings
                .camera
                .zoom_at(0.5, point, frame.size(), settings.viewport);
            let after = color::sample_pair(&frame, &frame, &settings, point).unwrap()[0];
            assert_eq!(after.x, 125);
            assert_eq!(after.y, 221);
            assert_eq!(
                after.y, before.y,
                "The unclamped axis keeps its cursor anchor"
            );
            assert_ne!(
                after.x, before.x,
                "The image-centre limit changed the source under the cursor"
            );
            // Camera-only requests can retain the loupe while its worker
            // replacement is pending; another source or Diff content cannot.
            assert!(old_content == RenderContent::new([1, 1], &settings));
            assert!(old_content != RenderContent::new([2, 2], &settings));
            settings.mode = Mode::Diff;
            assert!(old_content != RenderContent::new([1, 1], &settings));
        }
    }
}
