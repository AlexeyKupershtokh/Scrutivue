//! Pixel inspection of decoded sRGB16, never the amplified screen color.
use crate::{
    decode::Frame,
    model::{Mode, Settings},
    services::Mailbox,
};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

#[cfg(test)]
const LOUPE_SIZE: [usize; 2] = [21, 15];
pub const LOUPE_ZOOM: f32 = 16.;
pub const LOUPE_WINDOW: [f32; 2] = [168., 120.];
fn loupe_grid(display_scale: f32) -> [usize; 2] {
    // Odd dimensions keep the inspected source pixel exactly in the centre.
    LOUPE_WINDOW.map(|n| ((n * display_scale.clamp(0.5, 8.) / LOUPE_ZOOM).ceil() as usize) | 1)
}
/// One colour per original pixel. The UI paints solid 16×16 physical-pixel quads,
/// rather than scaling a viewport bitmap (which would magnify its smoothing).
pub struct Loupe {
    pub size: [usize; 2],
    pub colors: Vec<[u8; 3]>,
}
pub struct LoupeUpdate {
    pub token: u64,
    pub loupe: Loupe,
}
struct LoupeJob {
    token: u64,
    a: Arc<Frame>,
    b: Arc<Frame>,
    settings: Settings,
    point: [f32; 2],
    show_b: bool,
    size: [usize; 2],
}
pub struct LoupeWorker {
    pub events: crossbeam_channel::Receiver<LoupeUpdate>,
    jobs: Arc<Mailbox<Option<LoupeJob>>>,
    token: Arc<AtomicU64>,
}
impl LoupeWorker {
    pub fn new() -> Self {
        let jobs = Arc::new(Mailbox::<Option<LoupeJob>>::new());
        let token = Arc::new(AtomicU64::new(0));
        let (tx, events) = crossbeam_channel::unbounded();
        let (queue, current) = (jobs.clone(), token.clone());
        std::thread::Builder::new()
            .name("pixel-loupe".into())
            .spawn(move || {
                while let Some(job) = queue.take() {
                    let cancelled = || current.load(Ordering::Acquire) != job.token;
                    if let Some(loupe) = loupe(
                        &job.a,
                        &job.b,
                        &job.settings,
                        job.point,
                        job.show_b,
                        job.size,
                        &cancelled,
                    ) {
                        if !cancelled()
                            && tx
                                .send(LoupeUpdate {
                                    token: job.token,
                                    loupe,
                                })
                                .is_err()
                        {
                            break;
                        }
                    }
                }
            })
            .expect("pixel loupe thread");
        Self {
            events,
            jobs,
            token,
        }
    }
    pub fn request(
        &self,
        a: Arc<Frame>,
        b: Arc<Frame>,
        settings: Settings,
        point: [f32; 2],
        show_b: bool,
        display_scale: f32,
    ) -> u64 {
        let token = self.token.fetch_add(1, Ordering::AcqRel) + 1;
        let size = loupe_grid(display_scale);
        self.jobs.put(Some(LoupeJob {
            token,
            a,
            b,
            settings,
            point,
            show_b,
            size,
        }));
        token
    }
    pub fn cancel(&self) -> u64 {
        self.token.fetch_add(1, Ordering::AcqRel) + 1
    }
}
impl Drop for LoupeWorker {
    fn drop(&mut self) {
        self.cancel();
        self.jobs.put(None);
    }
}

fn grid(
    frame: &Frame,
    xs: &[u32],
    ys: &[u32],
    sigmas: Option<[f32; 2]>,
    cancelled: &impl Fn() -> bool,
) -> Option<Vec<[f32; 3]>> {
    let read = |x: u32, y: u32| {
        let i = (y as usize * frame.width as usize + x as usize) * 3;
        std::array::from_fn::<_, 3, _>(|c| frame.rgb[i + c] as f32 / 65535.)
    };
    let weights = sigmas.map(|s| s.map(crate::processing::gaussian_weights));
    let mut horizontal: HashMap<i32, Vec<[f32; 3]>> = HashMap::new();
    let mut out = Vec::with_capacity(xs.len() * ys.len());
    for &y in ys {
        if cancelled() {
            return None;
        }
        let mut row: Vec<_> = xs.iter().map(|&x| read(x, y)).collect();
        if let Some([wx, wy]) = &weights {
            for (ky, &weight_y) in wy.iter().enumerate() {
                let sy =
                    (y as i32 + ky as i32 - wy.len() as i32 / 2).clamp(0, frame.height as i32 - 1);
                let values = horizontal.entry(sy).or_insert_with(|| {
                    xs.iter()
                        .map(|&x| {
                            let mut value = [0.; 3];
                            for (kx, &weight) in wx.iter().enumerate() {
                                let sx = (x as i32 + kx as i32 - wx.len() as i32 / 2)
                                    .clamp(0, frame.width as i32 - 1);
                                let pixel = read(sx as u32, sy as u32);
                                for c in 0..3 {
                                    value[c] += pixel[c] * weight;
                                }
                            }
                            value
                        })
                        .collect()
                });
                for (pixel, value) in row.iter_mut().zip(values) {
                    for c in 0..3 {
                        pixel[c] -= value[c] * weight_y;
                    }
                }
            }
        }
        out.extend(row);
    }
    Some(out)
}
fn loupe(
    a: &Frame,
    b: &Frame,
    settings: &Settings,
    point: [f32; 2],
    show_b: bool,
    size: [usize; 2],
    cancelled: &impl Fn() -> bool,
) -> Option<Loupe> {
    let center = sample_pair(a, b, settings, point)?[0];
    let [w, h] = size;
    let xs: Vec<_> = (0..w)
        .map(|x| (center.x as i64 + x as i64 - w as i64 / 2).clamp(0, a.width as i64 - 1) as u32)
        .collect();
    let ys: Vec<_> = (0..h)
        .map(|y| (center.y as i64 + y as i64 - h as i64 / 2).clamp(0, a.height as i64 - 1) as u32)
        .collect();
    let bx: Vec<_> = xs
        .iter()
        .map(|&x| {
            (((x as f64 + 0.5) * b.width as f64 / a.width as f64).floor() as u32).min(b.width - 1)
        })
        .collect();
    let by: Vec<_> = ys
        .iter()
        .map(|&y| {
            (((y as f64 + 0.5) * b.height as f64 / a.height as f64).floor() as u32)
                .min(b.height - 1)
        })
        .collect();
    let av = grid(
        a,
        &xs,
        &ys,
        settings.highpass.then_some([settings.sigma; 2]),
        cancelled,
    )?;
    let bv = grid(
        b,
        &bx,
        &by,
        settings.highpass.then_some([
            settings.sigma * b.width as f32 / a.width as f32,
            settings.sigma * b.height as f32 / a.height as f32,
        ]),
        cancelled,
    )?;
    let scale = settings.camera.scale(a.size(), settings.viewport);
    let image_width = a.width as f32;
    let colors = av
        .into_iter()
        .zip(bv)
        .enumerate()
        .map(|(i, (a, b))| {
            let source_x = xs[i % w] as f32 + 0.5;
            let choose_b = match settings.mode {
                Mode::Click => show_b,
                Mode::Slider => {
                    (source_x - settings.camera.center[0] * image_width) * scale
                        + settings.viewport[0] as f32 * 0.5
                        >= settings.split * settings.viewport[0] as f32
                }
                Mode::Diff => false,
            };
            std::array::from_fn(|c| {
                let value = if settings.mode == Mode::Diff {
                    (a[c] - b[c]).abs() * settings.gain as f32
                } else {
                    (if choose_b { b[c] } else { a[c] }) + if settings.highpass { 0.5 } else { 0. }
                };
                (value.clamp(0., 1.) * 255.).round_ties_even() as u8
            })
        })
        .collect();
    Some(Loupe { size, colors })
}

#[derive(Clone, Copy, Debug)]
pub struct Sample {
    pub x: u32,
    pub y: u32,
    pub rgb: [u16; 3],
}
pub fn sample_pair(
    a: &Frame,
    b: &Frame,
    settings: &Settings,
    point: [f32; 2],
) -> Option<[Sample; 2]> {
    let scale = settings.camera.scale(a.size(), settings.viewport);
    let pos = std::array::from_fn::<_, 2, _>(|i| {
        (point[i] - settings.viewport[i] as f32 * 0.5) / scale
            + settings.camera.center[i] * a.size()[i] as f32
    });
    if pos[0] < 0. || pos[1] < 0. || pos[0] >= a.width as f32 || pos[1] >= a.height as f32 {
        return None;
    }
    let read = |frame: &Frame, x: f32, y: f32| {
        let x = (x.floor() as u32).min(frame.width - 1);
        let y = (y.floor() as u32).min(frame.height - 1);
        let i = (y as usize * frame.width as usize + x as usize) * 3;
        Sample {
            x,
            y,
            rgb: [frame.rgb[i], frame.rgb[i + 1], frame.rgb[i + 2]],
        }
    };
    Some([
        read(a, pos[0], pos[1]),
        read(
            b,
            pos[0] * b.width as f32 / a.width as f32,
            pos[1] * b.height as f32 / a.height as f32,
        ),
    ])
}
pub fn rgb8(rgb: [u16; 3]) -> [u8; 3] {
    rgb.map(|v| ((v as u32 + 128) / 257) as u8)
}
pub fn srgb_to_linear(v: f64) -> f64 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}
pub fn oklch(rgb: [u16; 3]) -> [f64; 3] {
    let [r, g, b] = rgb.map(|v| srgb_to_linear(v as f64 / 65535.));
    let l = (0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b).cbrt();
    let m = (0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b).cbrt();
    let s = (0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b).cbrt();
    let light = 0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s;
    let a = 1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s;
    let b = 0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s;
    [light, a.hypot(b), b.atan2(a).to_degrees().rem_euclid(360.)]
}
pub fn formats(rgb: [u16; 3]) -> [(&'static str, String); 9] {
    let [r, g, b] = rgb.map(|v| v as f64 / 65535.);
    let [r8, g8, b8] = rgb8(rgb);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let light = (max + min) / 2.;
    let hue = if d < 1e-12 {
        0.
    } else if max == r {
        ((g - b) / d).rem_euclid(6.) * 60.
    } else if max == g {
        ((b - r) / d + 2.) * 60.
    } else {
        ((r - g) / d + 4.) * 60.
    };
    let sl = if d < 1e-12 {
        0.
    } else {
        d / (1. - (2. * light - 1.).abs())
    };
    let sv = if max == 0. { 0. } else { d / max };
    let [rl, gl, bl] = [r, g, b].map(srgb_to_linear);
    let x = 0.4124564 * rl + 0.3575761 * gl + 0.1804375 * bl;
    let y = 0.2126729 * rl + 0.7151522 * gl + 0.0721750 * bl;
    let z = 0.0193339 * rl + 0.1191920 * gl + 0.9503041 * bl;
    let f = |v: f64| {
        if v > 216. / 24389. {
            v.cbrt()
        } else {
            (24389. / 27. * v + 16.) / 116.
        }
    };
    let [fx, fy, fz] = [x / 0.95047, y, z / 1.08883].map(f);
    let [ol, oc, oh] = oklch(rgb);
    [
        ("HEX", format!("#{r8:02X}{g8:02X}{b8:02X}")),
        ("RGB8", format!("{r8}, {g8}, {b8}")),
        ("RGB16", format!("{}, {}, {}", rgb[0], rgb[1], rgb[2])),
        (
            "HSL",
            format!("{hue:.1}° {:.1}% {:.1}%", sl * 100., light * 100.),
        ),
        (
            "HSV",
            format!("{hue:.1}° {:.1}% {:.1}%", sv * 100., max * 100.),
        ),
        ("OKLCH", format!("{ol:.4} {oc:.4} {oh:.1}°")),
        (
            "Lab D65",
            format!(
                "{:.2} {:+.2} {:+.2}",
                116. * fy - 16.,
                500. * (fx - fy),
                200. * (fy - fz)
            ),
        ),
        ("XYZ D65", format!("{:.3} {:.3} {:.3}", x, y, z)),
        ("Lin RGB", format!("{rl:.4} {gl:.4} {bl:.4}")),
    ]
}
pub fn popup_position(point: [f32; 2], view: [f32; 2], popup: [f32; 2]) -> [f32; 2] {
    std::array::from_fn(|i| {
        let after = point[i] + 20.;
        let before = point[i] - popup[i] - 20.;
        (if after + popup[i] + 8. <= view[i] {
            after
        } else {
            before
        })
        .clamp(8., (view[i] - popup[i] - 8.).max(8.))
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn loupe_is_original_pixel_grid_independent_of_viewport_zoom() {
        assert_eq!(LOUPE_ZOOM, 16.);
        assert_eq!(loupe_grid(1.), [11, 9]);
        assert_eq!(loupe_grid(2.), [21, 15]);
        assert_eq!(loupe_grid(1.25), [15, 11]);
        let a = Frame {
            width: 64,
            height: 64,
            rgb: (0..64 * 64)
                .flat_map(|i| if i % 2 == 0 { [0; 3] } else { [65535; 3] })
                .collect(),
        };
        let mut s = Settings {
            mode: Mode::Click,
            viewport: [64, 64],
            ..Settings::default()
        };
        let one = loupe(&a, &a, &s, [32., 32.], false, LOUPE_SIZE, &|| false).unwrap();
        s.camera.zoom = crate::model::Zoom::Fixed(8.);
        let eight = loupe(&a, &a, &s, [32., 32.], false, LOUPE_SIZE, &|| false).unwrap();
        assert_eq!(one.colors, eight.colors);
        assert!(one.colors.iter().all(|c| *c == [0; 3] || *c == [255; 3]));
        assert_eq!(one.colors.len(), 21 * 15);
        assert!(loupe(&a, &a, &s, [32., 32.], false, LOUPE_SIZE, &|| true).is_none());
    }
    #[test]
    fn loupe_preserves_difference_gain_and_local_highpass() {
        let a = Frame {
            width: 64,
            height: 64,
            rgb: vec![10000; 64 * 64 * 3],
        };
        let b = Frame {
            width: 64,
            height: 64,
            rgb: vec![11000; 64 * 64 * 3],
        };
        let mut s = Settings {
            mode: Mode::Diff,
            gain: 16,
            viewport: [64, 64],
            ..Settings::default()
        };
        let diff = loupe(&a, &b, &s, [32., 32.], false, LOUPE_SIZE, &|| false).unwrap();
        assert!(diff.colors.iter().all(|c| *c == [62; 3]));
        s.highpass = true;
        let diff = loupe(&a, &b, &s, [32., 32.], false, LOUPE_SIZE, &|| false).unwrap();
        assert!(diff.colors.iter().all(|c| *c == [0; 3]));
        s.mode = Mode::Click;
        let hp = loupe(&a, &b, &s, [32., 32.], false, LOUPE_SIZE, &|| false).unwrap();
        assert!(hp
            .colors
            .iter()
            .all(|c| c.iter().all(|v| (127..=128).contains(v))));
    }
    #[test]
    fn known_srgb_red_oklch_and_formats() {
        let v = oklch([65535, 0, 0]);
        assert!((v[0] - 0.627955).abs() < 1e-5);
        assert!((v[1] - 0.257683).abs() < 1e-5);
        assert!((v[2] - 29.2339).abs() < 1e-3);
        let text = formats([65535, 0, 0]);
        assert_eq!(text[0].1, "#FF0000");
        assert_eq!(text[3].1, "0.0° 100.0% 50.0%");
        assert!(oklch([0, 0, 0]).iter().all(|v| v.is_finite()));
    }
    #[test]
    fn popup_stays_inside_view() {
        assert_eq!(
            popup_position([990., 790.], [1000., 800.], [420., 420.]),
            [550., 350.]
        );
    }
    #[test]
    fn probes_originals_and_maps_different_sizes() {
        let a = Frame {
            width: 4,
            height: 4,
            rgb: vec![12345; 48],
        };
        let b = Frame {
            width: 2,
            height: 2,
            rgb: vec![54321; 12],
        };
        let s = Settings {
            viewport: [400, 400],
            ..Settings::default()
        };
        let p = sample_pair(&a, &b, &s, [200., 200.]).unwrap();
        assert_eq!([p[0].x, p[0].y], [2, 2]);
        assert_eq!([p[1].x, p[1].y], [1, 1]);
        assert_eq!(p[0].rgb, [12345; 3]);
        assert!(sample_pair(&a, &b, &s, [0., 0.]).is_none());
    }
    #[test]
    fn picker_tracks_panned_reduced_images_and_ignores_exposed_background() {
        let a = Frame {
            width: 64,
            height: 48,
            rgb: vec![12345; 64 * 48 * 3],
        };
        let b = Frame {
            width: 32,
            height: 24,
            rgb: vec![54321; 32 * 24 * 3],
        };
        let mut settings = Settings {
            viewport: [400, 300],
            ..Default::default()
        };
        settings.camera.zoom = crate::model::Zoom::Fixed(3.125);
        settings
            .camera
            .pan([50., -37.5], a.size(), settings.viewport);
        assert_eq!(settings.camera.center, [0.25, 0.75]);
        let pair = sample_pair(&a, &b, &settings, [200., 150.]).unwrap();
        assert_eq!([pair[0].x, pair[0].y], [16, 36]);
        assert_eq!([pair[1].x, pair[1].y], [8, 18]);
        assert_eq!(pair[0].rgb, [12345; 3]);
        assert_eq!(pair[1].rgb, [54321; 3]);
        assert!(sample_pair(&a, &b, &settings, [399., 0.]).is_none());
    }
}
