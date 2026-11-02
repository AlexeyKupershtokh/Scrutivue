use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    time::SystemTime,
};

pub type PhotoId = u64;

#[derive(Debug, Clone)]
pub struct Photo {
    pub id: PhotoId,
    pub path: PathBuf,
    pub name: String,
    pub metadata: PhotoMetadata,
    /// Explicit additions survive rescans even when a containing root disappears.
    pub explicit: bool,
    pub roots: HashSet<PathBuf>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PhotoMetadata {
    pub dimensions: Option<[u32; 2]>,
    pub bytes: u64,
    pub modified: Option<SystemTime>,
    pub created: Option<SystemTime>,
}
impl PhotoMetadata {
    pub fn same_revision(&self, other: &Self) -> bool {
        self.bytes == other.bytes
            && self.modified == other.modified
            && self.created == other.created
    }
    pub fn label(&self) -> String {
        let dimensions = self
            .dimensions
            .map(|[w, h]| format!("{w} × {h}"))
            .unwrap_or_else(|| "— × —".into());
        format!("{dimensions}\n{}", file_size_label(self.bytes))
    }
}

/// Decimal file-size units, rounded to one decimal without trailing zeroes.
pub fn file_size_label(bytes: u64) -> String {
    const UNITS: [&str; 7] = ["B", "KB", "MB", "GB", "TB", "PB", "EB"];
    if bytes < 1000 {
        return format!("{bytes} B");
    }
    // Integer arithmetic keeps rounding predictable, even for u64::MAX.
    let bytes = u128::from(bytes);
    let mut unit = 1;
    let mut divisor = 1000u128;
    while unit + 1 < UNITS.len() && bytes >= divisor * 1000 {
        divisor *= 1000;
        unit += 1;
    }
    let mut tenths = (bytes * 10 + divisor / 2) / divisor;
    if tenths >= 10_000 && unit + 1 < UNITS.len() {
        divisor *= 1000;
        unit += 1;
        tenths = (bytes * 10 + divisor / 2) / divisor;
    }
    if tenths % 10 == 0 {
        format!("{} {}", tenths / 10, UNITS[unit])
    } else {
        format!("{}.{} {}", tenths / 10, tenths % 10, UNITS[unit])
    }
}

pub fn select_slot(selected: &mut [Option<PhotoId>; 2], id: PhotoId, slot: usize) {
    let slot = slot.min(1);
    let other = 1 - slot;
    if selected[other] == Some(id) {
        selected[other] = selected[slot].take();
    }
    selected[slot] = Some(id);
    // The first (or sole) selected image is the reference. Keeping a B-only
    // state would leave the viewer and single-image noise analysis empty.
    if selected[0].is_none() {
        selected[0] = selected[1].take();
    }
}

pub fn select_initial(selected: &mut [Option<PhotoId>; 2], photos: &[Photo]) -> bool {
    let before = *selected;
    if selected[0].is_none() {
        selected[0] = selected[1]
            .take()
            .or_else(|| photos.first().map(|photo| photo.id));
    }
    before != *selected
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Mode {
    Diff,
    Slider,
    Click,
}
impl Mode {
    pub fn available(self, has_pair: bool) -> bool {
        self == Self::Click || has_pair
    }
    pub fn effective(self, has_pair: bool) -> Self {
        if self.available(has_pair) {
            self
        } else {
            Self::Click
        }
    }
    /// Update the remembered choice only when the requested mode is available.
    pub fn request(&mut self, requested: Self, has_pair: bool) -> bool {
        if !requested.available(has_pair) {
            return false;
        }
        *self = requested;
        true
    }
    pub fn prepared_pair(self) -> bool {
        self != Self::Diff
    }
    pub const ALL: [Self; 3] = [Self::Click, Self::Slider, Self::Diff];
    pub fn label(self) -> &'static str {
        match self {
            Self::Diff => "Diff",
            Self::Slider => "Slider",
            Self::Click => "Single",
        }
    }
    pub fn tooltip(self) -> &'static str {
        match self {
            Self::Click => "Show A. Hold the mouse button or Space to show B.",
            Self::Slider => "Show both images with a draggable divider.",
            Self::Diff => "Show the difference between A and B with the selected gain.",
        }
    }
    pub fn kernel_id(self) -> i32 {
        match self {
            Self::Click => 0,
            Self::Diff => 1,
            Self::Slider => 2,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Zoom {
    Fit,
    Fixed(f32),
}

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Camera {
    pub zoom: Zoom,
    pub center: [f32; 2],
}
impl Default for Camera {
    fn default() -> Self {
        Self {
            zoom: Zoom::Fit,
            center: [0.5, 0.5],
        }
    }
}
impl Camera {
    pub fn fit_scale(image: [u32; 2], viewport: [u32; 2]) -> f32 {
        (viewport[0] as f32 / image[0].max(1) as f32)
            .min(viewport[1] as f32 / image[1].max(1) as f32)
            .max(f32::MIN_POSITIVE)
            .min(8.)
    }
    pub fn zoom_limits(image: [u32; 2], viewport: [u32; 2]) -> [f32; 2] {
        [Self::fit_scale(image, viewport) * 0.5, 8.]
    }
    pub fn scale(&self, image: [u32; 2], viewport: [u32; 2]) -> f32 {
        match self.zoom {
            Zoom::Fit => Self::fit_scale(image, viewport),
            Zoom::Fixed(x) if x.is_finite() => x,
            Zoom::Fixed(_) => Self::fit_scale(image, viewport),
        }
        .clamp(Self::zoom_limits(image, viewport)[0], 8.)
    }
    /// Keep the viewport centre inside the image at every zoom, including Fit.
    /// Empty space around the edges is allowed so overlays need not hide detail.
    pub fn clamp(&mut self, image: [u32; 2], viewport: [u32; 2]) {
        let scale = self.scale(image, viewport);
        if matches!(self.zoom, Zoom::Fixed(_)) {
            self.zoom = Zoom::Fixed(scale);
        }
        for center in &mut self.center {
            *center = if center.is_finite() {
                center.clamp(0., 1.)
            } else {
                0.5
            };
        }
    }
    pub fn fit(&mut self) {
        *self = Self::default();
    }
    pub fn pan(&mut self, delta: [f32; 2], image: [u32; 2], viewport: [u32; 2]) {
        self.clamp(image, viewport);
        let scale = self.scale(image, viewport);
        for i in 0..2 {
            if delta[i].is_finite() {
                self.center[i] -= delta[i] / scale / image[i].max(1) as f32;
            }
        }
        self.clamp(image, viewport);
    }
    pub fn zoom_at(&mut self, factor: f32, point: [f32; 2], image: [u32; 2], viewport: [u32; 2]) {
        self.clamp(image, viewport);
        let old = self.scale(image, viewport);
        let factor = if factor.is_finite() {
            factor.max(0.)
        } else {
            1.
        };
        let new = (old * factor).clamp(Self::zoom_limits(image, viewport)[0], 8.);
        for i in 0..2 {
            if point[i].is_finite() {
                let offset = point[i] - viewport[i] as f32 * 0.5;
                self.center[i] += offset * (1.0 / old - 1.0 / new) / image[i].max(1) as f32;
            }
        }
        self.zoom = Zoom::Fixed(new);
        self.clamp(image, viewport);
    }
    /// Position/size, in current physical viewport pixels, of an immutable old
    /// viewport texture. GPUI can preview a gesture with this affine map alone.
    pub fn viewport_transform(
        &self,
        prepared: &Self,
        image: [u32; 2],
        viewport: [u32; 2],
        prepared_viewport: [u32; 2],
    ) -> ViewportTransform {
        let scale = self.scale(image, viewport);
        let ratio = scale / prepared.scale(image, prepared_viewport);
        ViewportTransform {
            origin: std::array::from_fn(|i| {
                viewport[i] as f32 * 0.5 - prepared_viewport[i] as f32 * ratio * 0.5
                    + (prepared.center[i] - self.center[i]) * image[i].max(1) as f32 * scale
            }),
            size: prepared_viewport.map(|v| v as f32 * ratio),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewportTransform {
    pub origin: [f32; 2],
    pub size: [f32; 2],
}

#[derive(Debug, Clone)]
pub struct Settings {
    pub mode: Mode,
    pub gain: u32,
    pub sigma: f32,
    pub highpass: bool,
    pub split: f32,
    pub camera: Camera,
    pub viewport: [u32; 2],
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            mode: Mode::Click,
            gain: 16,
            sigma: 2.0,
            highpass: false,
            split: 0.5,
            camera: Camera::default(),
            viewport: [960, 720],
        }
    }
}

pub fn supported(path: &Path) -> bool {
    path.extension()
        .and_then(|x| x.to_str())
        .map(|x| {
            matches!(
                x.to_ascii_lowercase().as_str(),
                "jpg"
                    | "jpeg"
                    | "png"
                    | "tif"
                    | "tiff"
                    | "webp"
                    | "bmp"
                    | "exr"
                    | "nef"
                    | "nrw"
                    | "arw"
                    | "cr2"
                    | "cr3"
                    | "crw"
                    | "dng"
                    | "raf"
                    | "rw2"
                    | "orf"
                    | "pef"
                    | "srw"
                    | "3fr"
                    | "fff"
                    | "iiq"
                    | "rwl"
                    | "raw"
                    | "mos"
                    | "mrw"
            )
        })
        .unwrap_or(false)
}
pub fn is_raw(path: &Path) -> bool {
    let ext = path
        .extension()
        .unwrap_or_default()
        .to_string_lossy()
        .to_ascii_lowercase();
    !matches!(
        ext.as_str(),
        "jpg" | "jpeg" | "png" | "tif" | "tiff" | "webp" | "bmp" | "exr"
    )
}

pub fn move_photo(photos: &mut Vec<Photo>, id: PhotoId, delta: isize) {
    if let Some(index) = photos.iter().position(|x| x.id == id) {
        let target = index.saturating_add_signed(delta).min(photos.len() - 1);
        let photo = photos.remove(index);
        photos.insert(target, photo);
    }
}

pub fn click_selection(selected: &mut [Option<PhotoId>; 2], id: PhotoId, control: bool) {
    if !control || selected[0].is_none() {
        *selected = [Some(id), None];
    } else if selected[1] == Some(id) {
        selected[1] = None;
    } else if selected[0] != Some(id) {
        selected[1] = Some(id);
    }
}

/// Arrow navigation keeps a comparison reference fixed, but never creates B.
pub fn navigate_selection(selected: &mut [Option<PhotoId>; 2], id: PhotoId) {
    if selected[1].is_some() {
        if selected[0] != Some(id) {
            selected[1] = Some(id);
        }
    } else {
        click_selection(selected, id, false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn file_sizes_use_readable_decimal_units() {
        for (bytes, expected) in [
            (0, "0 B"),
            (1, "1 B"),
            (999, "999 B"),
            (1000, "1 KB"),
            (500_000, "500 KB"),
            (7_100_000, "7.1 MB"),
            (92_000_000, "92 MB"),
            (1_000_000_000, "1 GB"),
            (1_000_000_000_000, "1 TB"),
            (1_000_000_000_000_000, "1 PB"),
            (u64::MAX, "18.4 EB"),
        ] {
            assert_eq!(file_size_label(bytes), expected, "{bytes}");
        }
    }
    #[test]
    fn file_sizes_round_and_promote_at_unit_cutoffs() {
        for (bytes, expected) in [
            (1049, "1 KB"),
            (1050, "1.1 KB"),
            (999_949, "999.9 KB"),
            (999_950, "1 MB"),
            (999_999, "1 MB"),
            (1_000_000, "1 MB"),
            (999_949_999, "999.9 MB"),
            (999_950_000, "1 GB"),
        ] {
            assert_eq!(file_size_label(bytes), expected, "{bytes}");
        }
        assert_eq!(
            PhotoMetadata {
                dimensions: Some([6000, 4000]),
                bytes: 7_100_000,
                ..PhotoMetadata::default()
            }
            .label(),
            "6000 × 4000\n7.1 MB"
        );
    }
    fn selection_photos(ids: &[PhotoId]) -> Vec<Photo> {
        ids.iter()
            .map(|&id| Photo {
                id,
                path: PathBuf::new(),
                name: id.to_string(),
                metadata: PhotoMetadata::default(),
                explicit: true,
                roots: HashSet::new(),
            })
            .collect()
    }
    #[test]
    fn additions_choose_only_a_even_across_batches() {
        let mut selected = [None; 2];
        assert!(select_initial(&mut selected, &selection_photos(&[1])));
        assert_eq!(selected, [Some(1), None]);
        assert!(!select_initial(
            &mut selected,
            &selection_photos(&[1, 2, 3])
        ));
        assert_eq!(selected, [Some(1), None]);
        assert!(!select_initial(
            &mut selected,
            &selection_photos(&[1, 2, 3, 4])
        ));
        assert_eq!(selected, [Some(1), None]);
        selected[1] = Some(3);
        assert!(!select_initial(
            &mut selected,
            &selection_photos(&[1, 2, 3, 4])
        ));
        assert_eq!(selected, [Some(1), Some(3)]);
        let mut many = [None; 2];
        assert!(select_initial(&mut many, &selection_photos(&[7, 8, 9])));
        assert_eq!(many, [Some(7), None]);
    }
    #[test]
    fn explicit_slot_assignment_swaps_instead_of_selecting_the_same_file_twice() {
        let mut selected = [Some(1), Some(2)];
        select_slot(&mut selected, 2, 0);
        assert_eq!(selected, [Some(2), Some(1)]);
        select_slot(&mut selected, 3, 1);
        assert_eq!(selected, [Some(2), Some(3)]);
        let mut empty = [None; 2];
        select_slot(&mut empty, 4, 1);
        assert_eq!(empty, [Some(4), None]);
        select_slot(&mut empty, 4, 1);
        assert_eq!(empty, [Some(4), None]);
        select_slot(&mut selected, 3, 1);
        assert_eq!(selected, [Some(2), Some(3)]);
    }
    #[test]
    fn plain_clicks_select_only_a_and_never_toggle_it_off() {
        let mut selected = [None; 2];
        click_selection(&mut selected, 7, false);
        assert_eq!(selected, [Some(7), None]);
        click_selection(&mut selected, 7, false);
        assert_eq!(selected, [Some(7), None]);
        selected[1] = Some(8);
        click_selection(&mut selected, 8, false);
        assert_eq!(selected, [Some(8), None]);
        click_selection(&mut selected, 9, false);
        assert_eq!(selected, [Some(9), None]);
    }
    #[test]
    fn control_click_adds_toggles_and_replaces_b_without_changing_a() {
        let mut selected = [None; 2];
        click_selection(&mut selected, 7, true);
        assert_eq!(selected, [Some(7), None]);
        click_selection(&mut selected, 8, true);
        assert_eq!(selected, [Some(7), Some(8)]);
        click_selection(&mut selected, 7, true);
        assert_eq!(selected, [Some(7), Some(8)]);
        click_selection(&mut selected, 9, true);
        assert_eq!(selected, [Some(7), Some(9)]);
        click_selection(&mut selected, 9, true);
        assert_eq!(selected, [Some(7), None]);
    }
    #[test]
    fn comparison_preference_survives_missing_b_and_ignores_unavailable_requests() {
        assert_eq!(Settings::default().mode, Mode::Click);
        for mut preferred in [Mode::Slider, Mode::Diff] {
            let remembered = preferred;
            assert_eq!(preferred.effective(false), Mode::Click);
            assert!(!preferred.request(Mode::Diff, false));
            assert!(!preferred.request(Mode::Slider, false));
            assert_eq!(preferred, remembered);
            assert_eq!(preferred.effective(true), remembered);
            assert!(preferred.request(Mode::Click, false));
            assert_eq!(preferred.effective(true), Mode::Click);
            assert!(preferred.request(remembered, true));
            assert_eq!(preferred.effective(false), Mode::Click);
            assert_eq!(preferred.effective(true), remembered);
        }
    }
    #[test]
    fn arrows_replace_only_a_when_single_and_only_b_when_paired() {
        let mut selected = [Some(1), None];
        navigate_selection(&mut selected, 2);
        assert_eq!(selected, [Some(2), None]);
        selected[1] = Some(3);
        navigate_selection(&mut selected, 4);
        assert_eq!(selected, [Some(2), Some(4)]);
        navigate_selection(&mut selected, 2);
        assert_eq!(selected, [Some(2), Some(4)]);
    }
    #[test]
    fn cursor_anchored_zoom() {
        let mut camera = Camera::default();
        let image = [8000, 6000];
        let view = [1000, 750];
        let point = [250.0, 120.0];
        let before = [
            camera.center[0] * 8000.0 + (point[0] - 500.0) / camera.scale(image, view),
            camera.center[1] * 6000.0 + (point[1] - 375.0) / camera.scale(image, view),
        ];
        camera.zoom_at(2.0, point, image, view);
        assert!(
            (before[0]
                - (camera.center[0] * 8000.0 + (point[0] - 500.0) / camera.scale(image, view)))
            .abs()
                < 0.001
        );
        assert!(
            (before[1]
                - (camera.center[1] * 6000.0 + (point[1] - 375.0) / camera.scale(image, view)))
            .abs()
                < 0.001
        );
    }
    #[test]
    fn zoom_is_limited_to_half_fit_and_eight_x_even_after_resize() {
        let image = [8000, 6000];
        let view = [1000, 750];
        let mut c = Camera::default();
        c.zoom_at(1e-8, [500., 375.], image, view);
        assert_eq!(c.scale(image, view), 0.0625);
        c.zoom_at(1e8, [500., 375.], image, view);
        assert_eq!(c.scale(image, view), 8.);
        c.zoom = Zoom::Fixed(0.001);
        assert_eq!(c.scale(image, [2000, 1500]), 0.125);
        assert_eq!(Camera::default().scale([1, 1], [3000, 2000]), 8.);
    }
    #[test]
    fn clamp_chooses_nearest_legal_center_per_axis() {
        let mut camera = Camera {
            zoom: Zoom::Fixed(1.),
            center: [-3., 4.],
        };
        camera.clamp([2000, 400], [1000, 750]);
        assert_eq!(camera.center, [0., 1.]);
        camera.center = [0.6, -10.];
        camera.clamp([2000, 400], [1000, 750]);
        assert_eq!(camera.center, [0.6, 0.]);
        camera.center = [8., 0.];
        camera.clamp([2000, 400], [1000, 750]);
        assert_eq!(camera.center, [1., 0.]);
    }
    #[test]
    fn small_equal_and_fit_images_can_be_panned_without_changing_zoom() {
        let mut camera = Camera::default();
        camera.pan([100., -75.], [2000, 1000], [1000, 750]);
        assert_eq!(camera.center, [0.4, 0.65]);
        assert_eq!(camera.zoom, Zoom::Fit);
        camera.pan([10000., -10000.], [2000, 1000], [1000, 750]);
        assert_eq!(camera.center, [0., 1.]);
        camera.zoom = Zoom::Fixed(1.);
        camera.pan([-10000., 10000.], [1000, 400], [1000, 750]);
        assert_eq!(camera.center, [1., 0.]);
        assert_eq!(camera.zoom, Zoom::Fixed(1.));
        camera.zoom = Zoom::Fixed(0.5);
        camera.center = [0.5; 2];
        camera.pan([50., 25.], [200, 100], [1000, 750]);
        // The existing half-Fit minimum still applies to very small images.
        assert_eq!(camera.center, [0.4, 0.4]);
        assert_eq!(camera.zoom, Zoom::Fixed(2.5));
    }
    #[test]
    fn clamp_applies_after_pan_zoom_resize_and_new_image() {
        let image = [4000, 3000];
        let viewport = [1000, 750];
        let mut camera = Camera {
            zoom: Zoom::Fixed(1.),
            center: [0.5, 0.5],
        };
        camera.pan([1e6, -1e6], image, viewport);
        assert_eq!(camera.center, [0., 1.]);
        camera.zoom_at(0.25, [1000., 0.], image, viewport);
        assert_eq!(camera.center, [0., 1.]);
        camera.center = [0., 1.];
        camera.clamp(image, [2000, 1500]);
        assert_eq!(camera.zoom, Zoom::Fixed(0.25));
        assert_eq!(camera.center, [0., 1.]);
        camera.zoom = Zoom::Fixed(2.);
        camera.center = [0.9, 0.1];
        camera.clamp([1000, 500], viewport);
        assert_eq!(camera.center, [0.9, 0.1]);
    }
    #[test]
    fn viewport_centre_stays_inside_the_image_at_every_scale_and_size() {
        for image in [[4000, 3000], [400, 3000], [4000, 300], [1, 1]] {
            for viewport in [[1000, 750], [2000, 500]] {
                for scale in [0.01, 0.5, 1., 8., 100.] {
                    for center in [[-10., 10.], [0., 1.], [0.4, 0.6]] {
                        let mut camera = Camera {
                            zoom: Zoom::Fixed(scale),
                            center,
                        };
                        camera.clamp(image, viewport);
                        let scale = camera.scale(image, viewport);
                        for i in 0..2 {
                            let extent = image[i] as f32 * scale;
                            assert!((0. ..=1.).contains(&camera.center[i]));
                            let middle = viewport[i] as f32 * 0.5;
                            let left = middle - camera.center[i] * extent;
                            assert!(left <= middle + 0.001);
                            assert!(left + extent >= middle - 0.001);
                            assert_eq!(camera.center[i], center[i].clamp(0., 1.));
                        }
                    }
                }
            }
        }
    }
    #[test]
    fn non_finite_restored_camera_is_repaired() {
        let mut camera = Camera {
            zoom: Zoom::Fixed(f32::NAN),
            center: [f32::NAN, f32::INFINITY],
        };
        camera.clamp([4000, 3000], [1000, 750]);
        assert_eq!(camera.zoom, Zoom::Fixed(0.25));
        assert_eq!(camera.center, [0.5, 0.5]);
    }
    #[test]
    fn viewport_texture_transform_preserves_image_coordinates() {
        let image = [4000, 3000];
        let old_view = [1000, 750];
        let new_view = [1200, 900];
        let prepared = Camera {
            zoom: Zoom::Fixed(0.5),
            center: [0.4, 0.6],
        };
        let current = Camera {
            zoom: Zoom::Fixed(0.8),
            center: [0.5, 0.4],
        };
        let transform = current.viewport_transform(&prepared, image, new_view, old_view);
        assert_eq!(transform.size, [1600., 1200.]);
        for source in [[2000., 1500.], [100., 200.], [3500., 2800.]] {
            for i in 0..2 {
                let old_pixel = (source[i] - prepared.center[i] * image[i] as f32) * 0.5
                    + old_view[i] as f32 * 0.5;
                let transformed = transform.origin[i] + old_pixel * 1.6;
                let new_pixel = (source[i] - current.center[i] * image[i] as f32) * 0.8
                    + new_view[i] as f32 * 0.5;
                assert!((transformed - new_pixel).abs() < 0.001);
            }
        }
        let identity = prepared.viewport_transform(&prepared, image, old_view, old_view);
        assert_eq!(
            identity,
            ViewportTransform {
                origin: [0., 0.],
                size: [1000., 750.]
            }
        );
    }
    #[test]
    fn reorder_does_not_change_ids() {
        let mut photos: Vec<_> = (0..4)
            .map(|id| Photo {
                id,
                path: PathBuf::new(),
                name: id.to_string(),
                metadata: PhotoMetadata::default(),
                explicit: true,
                roots: HashSet::new(),
            })
            .collect();
        move_photo(&mut photos, 1, 2);
        assert_eq!(
            photos.iter().map(|x| x.id).collect::<Vec<_>>(),
            [0, 2, 3, 1]
        );
    }
    #[test]
    fn extensions_are_case_insensitive() {
        assert!(supported(Path::new("a.NEF")));
        assert!(!supported(Path::new("a.txt")));
    }
}
