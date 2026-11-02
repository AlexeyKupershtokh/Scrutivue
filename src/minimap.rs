//! Small source-image navigator. Geometry is independent of viewport DPI; the
//! thumbnail is a background-only illustration, never an input to comparisons.
use crate::{
    decode::Frame,
    model::{Camera, PhotoId},
    services::{self, Mailbox},
};
use lru::LruCache;
use std::{
    num::NonZeroUsize,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Weak,
    },
};

pub const MAP_SIZE: [f32; 2] = [220., 130.];

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub origin: [f32; 2],
    pub size: [f32; 2],
}
impl Rect {
    pub fn contains(&self, point: [f32; 2]) -> bool {
        (0..2).all(|i| point[i] >= self.origin[i] && point[i] <= self.origin[i] + self.size[i])
    }
}
pub fn image_rect(image: [u32; 2], area: [f32; 2]) -> Rect {
    let factor = (area[0] / image[0].max(1) as f32).min(area[1] / image[1].max(1) as f32);
    let size = image.map(|n| n.max(1) as f32 * factor);
    Rect {
        origin: std::array::from_fn(|i| (area[i] - size[i]) * 0.5),
        size,
    }
}
pub fn visible_rect(camera: &Camera, image: [u32; 2], viewport: [u32; 2]) -> Rect {
    let scale = camera.scale(image, viewport);
    let start = std::array::from_fn::<_, 2, _>(|i| {
        (camera.center[i] - viewport[i] as f32 / (2. * scale * image[i].max(1) as f32))
            .clamp(0., 1.)
    });
    let end = std::array::from_fn::<_, 2, _>(|i| {
        (camera.center[i] + viewport[i] as f32 / (2. * scale * image[i].max(1) as f32))
            .clamp(0., 1.)
    });
    Rect {
        origin: start,
        size: std::array::from_fn(|i| (end[i] - start[i]).max(0.)),
    }
}
pub fn project(rect: Rect, image: Rect) -> Rect {
    Rect {
        origin: std::array::from_fn(|i| image.origin[i] + rect.origin[i] * image.size[i]),
        size: std::array::from_fn(|i| rect.size[i] * image.size[i]),
    }
}
pub fn normalized_point(point: [f32; 2], image: Rect) -> [f32; 2] {
    std::array::from_fn(|i| {
        ((point[i] - image.origin[i]) / image.size[i].max(f32::MIN_POSITIVE)).clamp(0., 1.)
    })
}

/// Project an overview image point into physical main-viewport coordinates.
/// It may be outside the viewport: clipping the anchor would move the image
/// point or recenter the camera before zooming.
pub fn viewport_point(
    point: [f32; 2],
    camera: &Camera,
    image: [u32; 2],
    viewport: [u32; 2],
) -> [f32; 2] {
    let scale = camera.scale(image, viewport);
    std::array::from_fn(|i| {
        viewport[i] as f32 * 0.5 + (point[i] - camera.center[i]) * image[i].max(1) as f32 * scale
    })
}

struct Job {
    token: u64,
    id: PhotoId,
    frame: Arc<Frame>,
}
pub struct Update {
    pub token: u64,
    pub image: Arc<gpui_kit::RenderImage>,
}
struct Cached {
    frame: Weak<Frame>,
    image: Arc<gpui_kit::RenderImage>,
}
pub struct Worker {
    pub events: crossbeam_channel::Receiver<Update>,
    jobs: Arc<Mailbox<Option<Job>>>,
    token: Arc<AtomicU64>,
}
impl Worker {
    pub fn new() -> Self {
        let jobs = Arc::new(Mailbox::<Option<Job>>::new());
        let token = Arc::new(AtomicU64::new(0));
        let (tx, events) = crossbeam_channel::unbounded();
        let (queue, current) = (jobs.clone(), token.clone());
        std::thread::Builder::new()
            .name("navigator-preview".into())
            .spawn(move || {
                let mut cache = LruCache::<PhotoId, Cached>::new(NonZeroUsize::new(16).unwrap());
                while let Some(job) = queue.take() {
                    let cancelled = || current.load(Ordering::Acquire) != job.token;
                    if cancelled() {
                        continue;
                    }
                    let weak = Arc::downgrade(&job.frame);
                    let cached = cache
                        .get(&job.id)
                        .filter(|c| c.frame.ptr_eq(&weak))
                        .map(|c| c.image.clone());
                    let image = if let Some(image) = cached {
                        image
                    } else {
                        let Some((size, pixels)) = thumbnail(&job.frame, &cancelled) else {
                            continue;
                        };
                        let image = services::render_image(size[0], size[1], pixels);
                        cache.put(
                            job.id,
                            Cached {
                                frame: weak,
                                image: image.clone(),
                            },
                        );
                        image
                    };
                    if !cancelled()
                        && tx
                            .send(Update {
                                token: job.token,
                                image,
                            })
                            .is_err()
                    {
                        break;
                    }
                }
            })
            .expect("navigator preview thread");
        Self {
            events,
            jobs,
            token,
        }
    }
    pub fn request(&self, id: PhotoId, frame: Arc<Frame>) -> u64 {
        let token = self.cancel();
        self.jobs.put(Some(Job { token, id, frame }));
        token
    }
    pub fn cancel(&self) -> u64 {
        self.token.fetch_add(1, Ordering::AcqRel) + 1
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.cancel();
        self.jobs.put(None);
    }
}

fn thumbnail(frame: &Frame, cancelled: &impl Fn() -> bool) -> Option<([u32; 2], Vec<u8>)> {
    if frame.width == 0 || frame.height == 0 {
        return None;
    }
    let factor = (MAP_SIZE[0] / frame.width as f32)
        .min(MAP_SIZE[1] / frame.height as f32)
        .min(1.);
    let size = frame
        .size()
        .map(|n| (n as f32 * factor).round().max(1.) as u32);
    let mut pixels = vec![0u8; (size[0] * size[1] * 4) as usize];
    // A fixed 4×4 area sample bounds the cost independently of RAW resolution.
    // This is only a navigation preview, not the statistical resize algorithm.
    let samples = [
        frame.width.div_ceil(size[0]).min(4),
        frame.height.div_ceil(size[1]).min(4),
    ];
    for y in 0..size[1] {
        if cancelled() {
            return None;
        }
        for x in 0..size[0] {
            let mut rgb = [0u32; 3];
            for sy in 0..samples[1] {
                for sx in 0..samples[0] {
                    let ix = (((x as f64 + (sx as f64 + 0.5) / samples[0] as f64)
                        * frame.width as f64
                        / size[0] as f64) as u32)
                        .min(frame.width - 1);
                    let iy = (((y as f64 + (sy as f64 + 0.5) / samples[1] as f64)
                        * frame.height as f64
                        / size[1] as f64) as u32)
                        .min(frame.height - 1);
                    let i = (iy as usize * frame.width as usize + ix as usize) * 3;
                    for c in 0..3 {
                        rgb[c] += frame.rgb[i + c] as u32;
                    }
                }
            }
            let i = ((y * size[0] + x) * 4) as usize;
            for c in 0..3 {
                pixels[i + 2 - c] =
                    ((rgb[c] as f64 / (samples[0] * samples[1]) as f64 / 257.).round()) as u8;
            }
            pixels[i + 3] = 255;
        }
    }
    Some((size, pixels))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Zoom;
    #[test]
    fn fit_and_zoomed_camera_project_to_the_correct_overview_region() {
        let image = [8000, 6000];
        let view = [1000, 750];
        let mut camera = Camera::default();
        assert_eq!(
            visible_rect(&camera, image, view),
            Rect {
                origin: [0., 0.],
                size: [1., 1.]
            }
        );
        camera.zoom = Zoom::Fixed(1.);
        camera.center = [0.2, 0.3];
        let rect = visible_rect(&camera, image, view);
        assert!((rect.origin[0] - 0.1375).abs() < 1e-6);
        assert!((rect.origin[1] - 0.2375).abs() < 1e-6);
        assert!(rect.size.iter().all(|n| (n - 0.125).abs() < 1e-6));
        let map = image_rect(image, MAP_SIZE);
        let projected = project(rect, map);
        let middle = std::array::from_fn(|i| projected.origin[i] + projected.size[i] * 0.5);
        let point = normalized_point(middle, map);
        for i in 0..2 {
            assert!((point[i] - camera.center[i]).abs() < 1e-6);
        }
    }
    #[test]
    fn letterbox_and_edge_clicks_do_not_escape_camera_limits() {
        let image = [5504, 8256];
        let rect = image_rect(image, MAP_SIZE);
        assert!(rect.origin[0] > 0.);
        assert_eq!(rect.origin[1], 0.);
        assert_eq!(normalized_point([-100., 1000.], rect), [0., 1.]);
        let mut camera = Camera {
            zoom: Zoom::Fixed(8.),
            center: normalized_point([-100., 1000.], rect),
        };
        camera.clamp(image, [2000, 1400]);
        let visible = visible_rect(&camera, image, [2000, 1400]);
        assert!(visible.origin[0].abs() < 1e-6);
        assert!((visible.origin[1] + visible.size[1] - 1.).abs() < 1e-6);
    }
    #[test]
    fn overview_wheel_anchor_preserves_the_projected_image_point_and_is_reversible() {
        let image = [8000, 6000];
        let viewport = [1000, 750];
        let mut camera = Camera {
            zoom: Zoom::Fixed(0.5),
            center: [0.5, 0.5],
        };
        let before = camera;
        let point = [0.6, 0.4];
        let anchor = viewport_point(point, &camera, image, viewport);
        camera.zoom_at(1.5, anchor, image, viewport);
        let after_anchor = viewport_point(point, &camera, image, viewport);
        for i in 0..2 {
            assert!((anchor[i] - after_anchor[i]).abs() < 0.001);
        }
        assert!((camera.center[0] - (0.5 + 0.1 / 3.)).abs() < 1e-6);
        camera.zoom_at(1. / 1.5, after_anchor, image, viewport);
        assert_eq!(camera.zoom, before.zoom);
        for i in 0..2 {
            assert!((camera.center[i] - before.center[i]).abs() < 1e-6);
        }
    }
    #[test]
    fn overview_wheel_is_dpi_independent_and_clamps_at_image_edges() {
        let image = [8000, 6000];
        let map = image_rect(image, MAP_SIZE);
        let point = normalized_point(
            [
                map.origin[0] + map.size[0] * 0.6,
                map.origin[1] + map.size[1] * 0.55,
            ],
            map,
        );
        let mut centers = Vec::new();
        for dpi in [1, 2, 3] {
            let viewport = [1000 * dpi, 750 * dpi];
            let mut camera = Camera::default();
            camera.zoom_at(
                2.,
                viewport_point(point, &camera, image, viewport),
                image,
                viewport,
            );
            centers.push(camera.center);
            camera.zoom_at(
                0.0001,
                viewport_point([0., 1.], &camera, image, viewport),
                image,
                viewport,
            );
            assert_eq!(camera.center, [1., 0.]);
            assert_eq!(
                camera.scale(image, viewport),
                Camera::zoom_limits(image, viewport)[0]
            );
        }
        for center in centers {
            assert!((center[0] - 0.55).abs() < 1e-6);
            assert!((center[1] - 0.525).abs() < 1e-6);
        }
    }
    #[test]
    fn panned_fit_view_is_clipped_to_the_image_in_the_navigator() {
        let image = [8000, 6000];
        let viewport = [1000, 750];
        let mut camera = Camera::default();
        camera.pan([-300., 225.], image, viewport);
        assert!((camera.center[0] - 0.8).abs() < 1e-6);
        assert!((camera.center[1] - 0.2).abs() < 1e-6);
        assert_eq!(camera.zoom, Zoom::Fit);
        let visible = visible_rect(&camera, image, viewport);
        for i in 0..2 {
            assert!(visible.origin[i] >= 0.);
            assert!(visible.origin[i] + visible.size[i] <= 1.);
        }
        assert!((visible.origin[0] - 0.3).abs() < 1e-6);
        assert!((visible.size[0] - 0.7).abs() < 1e-6);
        assert!((visible.size[1] - 0.7).abs() < 1e-6);
        assert!(visible.contains(camera.center));
    }
    #[test]
    fn preview_is_small_bgra_and_can_be_cancelled() {
        let frame = Frame {
            width: 1000,
            height: 800,
            rgb: vec![65535, 0, 32768].repeat(1000 * 800),
        };
        let (size, pixels) = thumbnail(&frame, &|| false).unwrap();
        assert!(size[0] <= 220 && size[1] <= 130);
        assert_eq!(&pixels[..4], &[128, 0, 255, 255]);
        assert!(thumbnail(&frame, &|| true).is_none());
    }

    #[test]
    fn worker_reuses_only_the_matching_source_and_does_not_retain_originals() {
        let worker = Worker::new();
        let frame = Arc::new(Frame {
            width: 32,
            height: 16,
            rgb: vec![0; 32 * 16 * 3],
        });
        let weak = Arc::downgrade(&frame);
        let receive = || {
            worker
                .events
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap()
        };
        let token = worker.request(1, frame.clone());
        let first = receive();
        assert_eq!(first.token, token);
        worker.request(1, frame.clone());
        let cached = receive();
        assert!(Arc::ptr_eq(&first.image, &cached.image));
        drop(frame);
        let changed = Arc::new(Frame {
            width: 32,
            height: 16,
            rgb: vec![65535; 32 * 16 * 3],
        });
        worker.request(1, changed);
        let refreshed = receive();
        assert!(!Arc::ptr_eq(&first.image, &refreshed.image));
        assert!(weak.upgrade().is_none());
    }
}
