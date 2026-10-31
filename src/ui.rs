use crate::{
    decode::Frame,
    model::{self, Mode, Photo, PhotoId, Settings, Zoom},
    services::{Event, RenderJob, RenderRequest, Services},
};
use gpui_kit::component::{
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
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
    sync::Arc,
    time::{Duration, Instant},
};

const BG: u32 = 0x1b1b1d;
const PANEL: u32 = 0x232325;
const LINE: u32 = 0x353538;
const TEXT: u32 = 0xe5e5e7;
const MUTED: u32 = 0x939398;
const ACCENT: u32 = 0xe66a56;

pub struct Desk {
    services: Services,
    photos: Vec<Photo>,
    known_paths: HashSet<PathBuf>,
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
    radius: Entity<SliderState>,
    _subscriptions: Vec<Subscription>,
    viewport_bounds: Rc<Cell<Bounds<Pixels>>>,
    display_scale: f32,
    generation: u64,
    primary: Option<Arc<RenderImage>>,
    alternate: Option<Arc<RenderImage>>,
    retired: Vec<Arc<RenderImage>>,
    held: bool,
    space_held: bool,
    pan_last: Option<Point<Pixels>>,
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
}

impl Desk {
    pub fn new(
        paths: Vec<PathBuf>,
        recursive: bool,
        cpu: bool,
        cache: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let services = Services::new(cpu, cache);
        let scanning = if paths.is_empty() {
            0
        } else {
            services.scan(paths, recursive);
            1
        };
        let radius = cx.new(|_| {
            SliderState::new()
                .min(0.3)
                .max(20.)
                .step(0.1)
                .default_value(2.)
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
                if let Some(help) = this.help_window.take() {
                    let _ = help.update(cx, |_, window, _| window.remove_window());
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
            photos: Vec::new(),
            known_paths: HashSet::new(),
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
            settings: Settings::default(),
            radius,
            _subscriptions: vec![subscription],
            viewport_bounds: Rc::new(Cell::new(Bounds::default())),
            display_scale: 1.,
            generation: 0,
            primary: None,
            alternate: None,
            retired: Vec::new(),
            held: false,
            space_held: false,
            pan_last: None,
            split_drag: false,
            rendering: false,
            elapsed_ms: 0.,
            backend: "Инициализация GPU…".into(),
            message: String::new(),
            scanning,
            recursive,
            last_sigma_change: None,
            focus,
            help_window: None,
        }
    }
    fn pump(&mut self, cx: &mut Context<Self>) {
        for image in self.retired.drain(..) {
            cx.drop_image(image, None);
        }
        let mut changed = false;
        for _ in 0..48 {
            let Ok(event) = self.services.events.try_recv() else {
                break;
            };
            changed = true;
            match event {
                Event::Backend(backend) => self.backend = backend,
                Event::Files { epoch, paths } if epoch == self.services.epoch() => {
                    for path in paths {
                        if !self.known_paths.insert(path.clone()) {
                            continue;
                        }
                        let id = self.next_id;
                        self.next_id += 1;
                        let name = path
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned();
                        self.indices.insert(id, self.photos.len());
                        self.photos.push(Photo { id, path, name });
                    }
                }
                Event::ScanDone { epoch, skipped } if epoch == self.services.epoch() => {
                    self.scanning = self.scanning.saturating_sub(1);
                    if skipped > 0 {
                        self.message =
                            format!("Пропущено недоступных или неподдерживаемых путей: {skipped}");
                    }
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
                            self.decode_errors.remove(&id);
                            self.frames.insert(id, frame);
                            self.request_render();
                        }
                        Err(error) => {
                            self.message = error.clone();
                            self.decode_errors.insert(id, error);
                            self.rendering = false;
                        }
                    }
                }
                Event::Rendered {
                    generation,
                    primary,
                    alternate,
                    elapsed,
                    backend,
                } if generation == self.generation => {
                    self.retire_views();
                    self.primary = Some(primary);
                    self.alternate = alternate;
                    self.elapsed_ms = elapsed.as_secs_f64() * 1000.;
                    self.backend = backend;
                    self.rendering = false;
                }
                Event::Error(error) => self.message = error,
                _ => {}
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
        if changed {
            cx.notify();
        }
    }
    fn add_paths(&mut self, paths: Vec<PathBuf>) {
        if !paths.is_empty() {
            self.scanning += 1;
            self.services.scan(paths, self.recursive);
        }
    }
    fn retire_views(&mut self) {
        if let Some(image) = self.primary.take() {
            self.retired.push(image);
        }
        if let Some(image) = self.alternate.take() {
            self.retired.push(image);
        }
    }
    fn clear(&mut self, cx: &mut Context<Self>) {
        self.services.clear();
        self.generation += 1;
        self.retire_views();
        while let Some((_, image)) = self.thumbnails.pop_lru() {
            self.retired.push(image);
        }
        self.photos.clear();
        self.known_paths.clear();
        self.indices.clear();
        self.selected = [None; 2];
        self.focused_photo = None;
        self.frames.clear();
        self.decode_errors.clear();
        self.thumb_pending.clear();
        self.thumb_done.clear();
        self.thumb_errors.clear();
        self.warm_cursor = 0;
        self.visible = 0..12;
        self.scroll = UniformListScrollHandle::new();
        self.rendering = false;
        self.scanning = 0;
        self.message.clear();
        self.elapsed_ms = 0.;
        self.held = false;
        self.space_held = false;
        self.pan_last = None;
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
    fn request_render(&mut self) {
        self.generation += 1;
        let Some(aid) = self.selected[0] else {
            self.rendering = false;
            return;
        };
        let bid = self.selected[1].unwrap_or(aid);
        let (Some(a), Some(b)) = (self.frames.get(&aid), self.frames.get(&bid)) else {
            self.rendering = true;
            return;
        };
        self.rendering = true;
        self.services.renders.put(RenderJob::Frame(RenderRequest {
            generation: self.generation,
            a: (aid, a.clone()),
            b: (bid, b.clone()),
            settings: self.settings.clone(),
        }));
    }
    fn selection_changed(&mut self) {
        self.held = false;
        self.space_held = false;
        self.frames
            .retain(|id, _| self.selected.contains(&Some(*id)));
        let mut jobs: Vec<_> = self
            .selected
            .iter()
            .flatten()
            .filter(|id| !self.frames.contains_key(id))
            .filter_map(|id| {
                self.indices
                    .get(id)
                    .map(|i| (*id, self.photos[*i].path.clone()))
            })
            .collect();
        if let Some(index) = self
            .focused_photo
            .and_then(|id| self.indices.get(&id))
            .copied()
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
            self.retire_views();
            self.services.renders.put(RenderJob::Clear);
        }
        self.services.decode(jobs);
        self.request_render();
    }
    fn select(&mut self, id: PhotoId, cx: &mut Context<Self>) {
        model::click_selection(&mut self.selected, id);
        self.focused_photo = Some(id);
        self.selection_changed();
        cx.notify();
    }
    fn navigate(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.photos.is_empty() {
            return;
        }
        let current = self.focused_photo.or(self.selected[1]).or(self.selected[0]);
        let index = current
            .and_then(|id| self.indices.get(&id))
            .copied()
            .unwrap_or(0)
            .saturating_add_signed(delta)
            .min(self.photos.len() - 1);
        let id = self.photos[index].id;
        if !self.selected.contains(&Some(id)) {
            if let Some(position) = self
                .selected
                .iter()
                .position(|v| v.is_some() && *v == current)
            {
                self.selected[position] = Some(id);
            } else {
                model::click_selection(&mut self.selected, id);
            }
        }
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
        for (i, photo) in self.photos.iter().enumerate() {
            self.indices.insert(photo.id, i);
        }
    }
    fn image_size(&self) -> [u32; 2] {
        self.selected[0]
            .and_then(|id| self.frames.get(&id))
            .map(|f| f.size())
            .unwrap_or([1, 1])
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
            Err(error) => self.message = format!("Справка: {error:#}"),
        }
    }
    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        let ctrl = event.keystroke.modifiers.control || event.keystroke.modifiers.platform;
        if ctrl && key == "o" {
            self.services.choose(event.keystroke.modifiers.shift);
            return;
        }
        match key {
            "f1" => {
                self.show_help(cx);
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
            "1" => self.settings.mode = Mode::Single,
            "2" => self.settings.mode = Mode::Diff,
            "3" => self.settings.mode = Mode::Slider,
            "4" => self.settings.mode = Mode::Click,
            "f" | "0" => self.settings.camera.fit(),
            "escape" => {
                self.held = false;
                self.space_held = false;
                self.pan_last = None;
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
                        .bg(rgb(if a || b { 0x343033 } else { PANEL }))
                        .hover(|s| s.bg(rgb(0x303033)))
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, window, cx| {
                            window.focus(&this.focus, cx);
                            this.select(id, cx);
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
                                .gap_2()
                                .child(div().text_sm().truncate().child(name))
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(rgb(MUTED))
                                                .child(format!("{:04}", index + 1)),
                                        )
                                        .when(a || b, |el| {
                                            el.child(
                                                div()
                                                    .px_2()
                                                    .rounded_sm()
                                                    .text_xs()
                                                    .font_weight(FontWeight::SEMIBOLD)
                                                    .text_color(rgb(if a {
                                                        ACCENT
                                                    } else {
                                                        0xb7bdd3
                                                    }))
                                                    .child(if a { "A" } else { "B" }),
                                            )
                                        }),
                                ),
                        )
                        .into_any_element(),
                )
            })
            .collect()
    }
    fn header(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .h(px(44.))
            .flex_none()
            .px_4()
            .flex()
            .items_center()
            .gap_3()
            .border_b_1()
            .border_color(rgb(LINE))
            .bg(rgb(PANEL))
            .child(img(crate::platform::app_icon()).size(px(26.)))
            .child(div().font_weight(FontWeight::SEMIBOLD).child("Lumen Diff"))
            .child(div().flex_1())
            .child(
                Button::new("help")
                    .label("Справка")
                    .small()
                    .ghost()
                    .on_click(cx.listener(|this, _, _, cx| this.show_help(cx))),
            )
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
                            .child(div().font_weight(FontWeight::SEMIBOLD).child("Изображения"))
                            .child(div().text_xs().text_color(rgb(MUTED)).child(format!(
                                "{}{}",
                                self.photos.len(),
                                if self.scanning > 0 { " +" } else { "" }
                            ))),
                    )
                    .child(
                        Button::new("add-files")
                            .label("Добавить изображение")
                            .small()
                            .w_full()
                            .on_click(cx.listener(|this, _, _, _| this.services.choose(false))),
                    )
                    .child(
                        Button::new("add-folder")
                            .label("Добавить папку")
                            .small()
                            .w_full()
                            .on_click(cx.listener(|this, _, _, _| this.services.choose(true))),
                    )
                    .child(
                        div().py_1().child(
                            Checkbox::new("recursive")
                                .label("Включая подпапки")
                                .checked(self.recursive)
                                .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                    this.recursive = *checked;
                                    cx.notify();
                                })),
                        ),
                    )
                    .child(
                        Button::new("clear-files")
                            .label("Очистить список")
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
                                "Миниатюры: {}/{}",
                                self.thumb_done.len(),
                                self.photos.len()
                            )),
                    )
                    .child(
                        Button::new("move-up")
                            .label("↑")
                            .small()
                            .ghost()
                            .disabled(self.focused_photo.is_none())
                            .on_click(cx.listener(|this, _, _, cx| this.reorder(-1, cx))),
                    )
                    .child(
                        Button::new("move-down")
                            .label("↓")
                            .small()
                            .ghost()
                            .disabled(self.focused_photo.is_none())
                            .on_click(cx.listener(|this, _, _, cx| this.reorder(1, cx))),
                    ),
            )
            .into_any_element()
    }
    fn inspector(&self, cx: &mut Context<Self>) -> AnyElement {
        let mismatch = self.selected[0]
            .and_then(|id| self.frames.get(&id))
            .zip(self.selected[1].and_then(|id| self.frames.get(&id)))
            .is_some_and(|(a, b)| a.size() != b.size());
        div()
            .w(px(238.))
            .flex_none()
            .h_full()
            .bg(rgb(PANEL))
            .border_l_1()
            .border_color(rgb(LINE))
            .child(
                div()
                    .p_4()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .child(section_label("АНАЛИЗ ШУМА"))
                    .child(
                        Checkbox::new("highpass")
                            .label("Вычитание Gaussian")
                            .checked(self.settings.highpass)
                            .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                this.settings.highpass = *checked;
                                this.request_render();
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .child("Радиус σ")
                            .child(format!("{:.1} px", self.settings.sigma)),
                    )
                    .child(
                        Slider::new(&self.radius)
                            .disabled(!self.settings.highpass)
                            .accessibility_label("Радиус Gaussian")
                            .w_full(),
                    )
                    .child(div().mt_3().child(section_label("ИЗОБРАЖЕНИЯ")))
                    .children((0..2).filter_map(|i| {
                        let id = self.selected[i]?;
                        let photo = self.photos.get(*self.indices.get(&id)?)?;
                        let metadata = self
                            .frames
                            .get(&id)
                            .map(|f| {
                                format!(
                                    "{} × {} · {:.1} Мп",
                                    f.width,
                                    f.height,
                                    f.width as f64 * f.height as f64 / 1e6
                                )
                            })
                            .unwrap_or_else(|| {
                                if self.decode_errors.contains_key(&id) {
                                    "Ошибка чтения".into()
                                } else {
                                    "Декодирование…".into()
                                }
                            });
                        Some(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(div().text_sm().truncate().child(format!(
                                    "{}  {}",
                                    if i == 0 { "A" } else { "B" },
                                    photo.name
                                )))
                                .child(div().text_xs().text_color(rgb(MUTED)).child(metadata)),
                        )
                    }))
                    .when(mismatch, |el| {
                        el.child(
                            div()
                                .p_3()
                                .rounded_md()
                                .bg(rgb(0x3a3025))
                                .text_xs()
                                .text_color(rgb(0xe3bd87))
                                .child("Разное разрешение: B масштабируется до размера A."),
                        )
                    }),
            )
            .into_any_element()
    }
    fn mode_toolbar(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .h(px(48.))
            .flex_none()
            .px_3()
            .flex()
            .items_center()
            .gap_1()
            .border_b_1()
            .border_color(rgb(LINE))
            .children(Mode::ALL.map(|mode| {
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(
                        Button::new(mode.label())
                            .label(mode.label())
                            .small()
                            .ghost()
                            .selected(self.settings.mode == mode)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.settings.mode = mode;
                                this.held = false;
                                this.space_held = false;
                                this.request_render();
                                cx.notify();
                            })),
                    )
                    .when(
                        mode == Mode::Diff && self.settings.mode == Mode::Diff,
                        |el| {
                            el.child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_0p5()
                                    .mr_2()
                                    .px_1()
                                    .rounded_md()
                                    .bg(rgb(0x29292c))
                                    .children([1, 4, 16, 64, 256].map(|gain| {
                                        Button::new(("gain", gain as usize))
                                            .label(format!("{gain}×"))
                                            .xsmall()
                                            .ghost()
                                            .selected(self.settings.gain == gain)
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.settings.gain = gain;
                                                this.request_render();
                                                cx.notify();
                                            }))
                                    })),
                            )
                        },
                    )
            }))
            .child(div().flex_1())
            .when(self.settings.mode == Mode::Single, |el| {
                el.children([false, true].map(|b| {
                    Button::new(if b { "show-b" } else { "show-a" })
                        .label(if b { "B" } else { "A" })
                        .small()
                        .ghost()
                        .selected(self.settings.single_b == b)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.settings.single_b = b;
                            cx.notify();
                        }))
                }))
            })
            .into_any_element()
    }
    fn viewer(&self, cx: &mut Context<Self>) -> AnyElement {
        let cell = self.viewport_bounds.clone();
        let a = self.primary.clone();
        let b = self.alternate.clone();
        let has_image = a.is_some();
        let mode = self.settings.mode;
        let split = self.settings.split;
        let show_b = mode == Mode::Single && self.settings.single_b
            || mode == Mode::Click && (self.held || self.space_held);
        let failed = self
            .selected
            .iter()
            .flatten()
            .any(|id| self.decode_errors.contains_key(id));
        let mut viewport = div()
            .id("viewport")
            .relative()
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_hidden()
            .bg(rgb(0x161618))
            .cursor_grab()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    window.focus(&this.focus, cx);
                    let p = this.local_point(event.position);
                    if this.settings.mode == Mode::Click {
                        this.held = true;
                    } else if this.settings.mode == Mode::Slider
                        && (p[0] - this.settings.split * this.settings.viewport[0] as f32).abs()
                            < 18. * this.display_scale
                    {
                        this.split_drag = true;
                    } else {
                        this.pan_last = Some(event.position);
                    }
                    cx.notify();
                }),
            )
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    this.pan_last = Some(event.position);
                    cx.notify();
                }),
            )
            .on_scroll(cx.listener(|this, event: &ScrollEvent, _, cx| {
                let delta = event.delta.pixel_delta(px(36.));
                this.settings.camera.zoom_at(
                    (f32::from(delta.y) * 0.0025).clamp(-0.7, 0.7).exp(),
                    this.local_point(event.position),
                    this.image_size(),
                    this.settings.viewport,
                );
                this.request_render();
                cx.notify();
            }))
            .child(
                canvas(
                    move |bounds, _, _| cell.set(bounds),
                    move |bounds, _, window, _| {
                        if let Some(a) = a {
                            // Both complete textures are uploaded on the first paint, even
                            // when one is covered. Divider motion only changes clipping.
                            if let Some(b) = b {
                                if show_b {
                                    let _ = window.paint_image(
                                        bounds,
                                        bounds,
                                        Corners::default(),
                                        a,
                                        0,
                                        false,
                                    );
                                    let _ = window.paint_image(
                                        bounds,
                                        bounds,
                                        Corners::default(),
                                        b,
                                        0,
                                        false,
                                    );
                                } else {
                                    let _ = window.paint_image(
                                        bounds,
                                        bounds,
                                        Corners::default(),
                                        b.clone(),
                                        0,
                                        false,
                                    );
                                    let _ = window.paint_image(
                                        bounds,
                                        bounds,
                                        Corners::default(),
                                        a,
                                        0,
                                        false,
                                    );
                                    if mode == Mode::Slider {
                                        let _ = window.paint_image(
                                            right_clip(bounds, split),
                                            bounds,
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
                                    bounds,
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
                "Не удалось прочитать изображение"
            } else if self.rendering {
                "Декодирование…"
            } else if self.photos.is_empty() {
                "Список изображений пуст"
            } else {
                "Нет выбранных изображений"
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
                    .child("Ошибка чтения. Показан предыдущий результат."),
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
                    .bg(rgb(0xe5d6ce))
                    .child(
                        div()
                            .absolute()
                            .top(relative(0.5))
                            .left(px(-13.))
                            .w(px(27.))
                            .h(px(38.))
                            .rounded_lg()
                            .bg(rgb(0xe5d6ce))
                            .text_color(rgb(0x3c3431))
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
        let zoom = self
            .settings
            .camera
            .scale(self.image_size(), self.settings.viewport)
            * 100.;
        div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .child(self.mode_toolbar(cx))
            .child(viewport)
            .child(
                div()
                    .h(px(48.))
                    .flex_none()
                    .px_3()
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap_1()
                    .border_t_1()
                    .border_color(rgb(LINE))
                    .child(
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
                    )
                    .children([1., 2., 4.].map(|scale| {
                        Button::new(("zoom", scale as usize))
                            .label(format!("{}%", (scale * 100.) as u32))
                            .small()
                            .ghost()
                            .selected(self.settings.camera.zoom == Zoom::Fixed(scale))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.settings.camera.zoom = Zoom::Fixed(scale);
                                this.request_render();
                                cx.notify();
                            }))
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
                    .child(div().ml_3().text_xs().text_color(rgb(MUTED)).child(
                        if self.rendering {
                            "Обработка…".into()
                        } else if has_image {
                            format!("{:.1} мс", self.elapsed_ms)
                        } else {
                            String::new()
                        },
                    )),
            )
            .into_any_element()
    }
}

impl Render for Desk {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.display_scale = window.scale_factor();
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
                    cx.notify();
                }
            }))
            .on_key_up(cx.listener(|this, event: &KeyUpEvent, _, cx| {
                if event.keystroke.key == "space" {
                    this.space_held = false;
                    cx.notify();
                }
            }))
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                if event.pressed_button.is_none() {
                    let changed = this.held || this.pan_last.is_some() || this.split_drag;
                    this.held = false;
                    this.pan_last = None;
                    this.split_drag = false;
                    if changed {
                        cx.notify();
                    }
                    return;
                }
                if this.split_drag {
                    this.settings.split = (this.local_point(event.position)[0]
                        / this.settings.viewport[0] as f32)
                        .clamp(0., 1.);
                    cx.notify(); // No render request, readback, decode or new image handle.
                } else if let Some(last) = this.pan_last {
                    let delta = [
                        f32::from(event.position.x - last.x) * this.display_scale,
                        f32::from(event.position.y - last.y) * this.display_scale,
                    ];
                    this.pan_last = Some(event.position);
                    this.settings
                        .camera
                        .pan(delta, this.image_size(), this.settings.viewport);
                    this.request_render();
                    cx.notify();
                }
            }))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                this.add_paths(paths.0.to_vec());
                cx.notify();
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.held = false;
                    this.pan_last = None;
                    this.split_drag = false;
                    cx.notify();
                }),
            )
            .on_mouse_up(
                MouseButton::Middle,
                cx.listener(|this, _, _, cx| {
                    this.pan_last = None;
                    cx.notify();
                }),
            )
            .child(self.header(cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(self.library(cx))
                    .child(self.viewer(cx))
                    .child(self.inspector(cx)),
            )
            .child(
                div()
                    .h(px(26.))
                    .flex_none()
                    .px_4()
                    .flex()
                    .items_center()
                    .gap_3()
                    .bg(rgb(0x202022))
                    .border_t_1()
                    .border_color(rgb(LINE))
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child(div().size(px(5.)).rounded_full().bg(rgb(ACCENT)))
                    .child(div().max_w(px(550.)).truncate().child(self.backend.clone()))
                    .child(div().flex_1().truncate().child(self.message.clone())),
            )
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
fn viewer_badge(label: &'static str, left: bool) -> AnyElement {
    div()
        .absolute()
        .top_4()
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
            .bg(rgb(0x443735))
            .text_color(rgb(TEXT))
            .text_sm()
            .child(self.name.clone())
    }
}
#[cfg(test)]
mod tests {
    use super::right_clip;
    use gpui_kit::{point, px, size, Bounds};
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
}
