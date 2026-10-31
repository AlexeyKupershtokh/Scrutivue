use std::path::{Path, PathBuf};

pub type PhotoId = u64;

#[derive(Debug, Clone)]
pub struct Photo {
    pub id: PhotoId,
    pub path: PathBuf,
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Single,
    Diff,
    Slider,
    Click,
}
impl Mode {
    pub fn prepared_pair(self) -> bool {
        self != Self::Diff
    }
    pub const ALL: [Self; 4] = [Self::Single, Self::Diff, Self::Slider, Self::Click];
    pub fn label(self) -> &'static str {
        match self {
            Self::Single => "Single",
            Self::Diff => "Diff",
            Self::Slider => "Both slider",
            Self::Click => "Both click",
        }
    }
    pub fn kernel_id(self) -> i32 {
        match self {
            Self::Single | Self::Click => 0,
            Self::Diff => 1,
            Self::Slider => 2,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Zoom {
    Fit,
    Fixed(f32),
}

#[derive(Debug, Clone, Copy)]
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
    pub fn scale(&self, image: [u32; 2], viewport: [u32; 2]) -> f32 {
        match self.zoom {
            Zoom::Fit => (viewport[0] as f32 / image[0].max(1) as f32)
                .min(viewport[1] as f32 / image[1].max(1) as f32),
            Zoom::Fixed(x) => x,
        }
        .max(0.001)
    }
    pub fn fit(&mut self) {
        *self = Self::default();
    }
    pub fn pan(&mut self, delta: [f32; 2], image: [u32; 2], viewport: [u32; 2]) {
        let scale = self.scale(image, viewport);
        for i in 0..2 {
            self.center[i] -= delta[i] / scale / image[i].max(1) as f32;
        }
    }
    pub fn zoom_at(&mut self, factor: f32, point: [f32; 2], image: [u32; 2], viewport: [u32; 2]) {
        let old = self.scale(image, viewport);
        let new = (old * factor).clamp(0.005, 32.0);
        for i in 0..2 {
            let offset = point[i] - viewport[i] as f32 * 0.5;
            self.center[i] += offset * (1.0 / old - 1.0 / new) / image[i].max(1) as f32;
        }
        self.zoom = Zoom::Fixed(new);
    }
}

#[derive(Debug, Clone)]
pub struct Settings {
    pub mode: Mode,
    pub gain: u32,
    pub sigma: f32,
    pub highpass: bool,
    pub split: f32,
    pub single_b: bool,
    pub camera: Camera,
    pub viewport: [u32; 2],
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            mode: Mode::Slider,
            gain: 16,
            sigma: 2.0,
            highpass: false,
            split: 0.5,
            single_b: false,
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

pub fn click_selection(selected: &mut [Option<PhotoId>; 2], id: PhotoId) {
    if selected[0] == Some(id) {
        selected[0] = selected[1].take();
    } else if selected[1] == Some(id) {
        selected[1] = None;
    } else if selected[0].is_none() {
        selected[0] = Some(id);
    } else {
        selected[1] = Some(id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clicks_assign_a_then_b_and_keep_reference_on_third_click() {
        let mut selected = [None; 2];
        click_selection(&mut selected, 7);
        assert_eq!(selected, [Some(7), None]);
        click_selection(&mut selected, 8);
        assert_eq!(selected, [Some(7), Some(8)]);
        click_selection(&mut selected, 9);
        assert_eq!(selected, [Some(7), Some(9)]);
        click_selection(&mut selected, 7);
        assert_eq!(selected, [Some(9), None]);
        click_selection(&mut selected, 9);
        assert_eq!(selected, [None, None]);
    }
    #[test]
    fn removing_second_selection_keeps_a() {
        let mut selected = [Some(2), Some(3)];
        click_selection(&mut selected, 3);
        assert_eq!(selected, [Some(2), None]);
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
    fn reorder_does_not_change_ids() {
        let mut photos: Vec<_> = (0..4)
            .map(|id| Photo {
                id,
                path: PathBuf::new(),
                name: id.to_string(),
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
