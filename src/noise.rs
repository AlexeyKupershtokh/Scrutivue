//! Blind noise estimates on native-resolution luminance patches, never resized.
//! Texture, sharpening, demosaicing and signal-dependent noise bias these estimates.
use crate::decode::Frame;
use anyhow::{ensure, Result};
use serde::Serialize;

const MAD_NORMAL: f64 = 0.6744897501960817;
const PATCH: usize = 128;

#[derive(Clone, Debug, Default, Serialize)]
pub struct Estimates {
    pub haar_hh_mad: Option<f64>,
    pub immerkaer: Option<f64>,
    pub high_pass_mad: Option<f64>,
}
#[derive(Clone, Copy, Debug, Serialize)]
pub struct Sampling {
    pub patch_size: [usize; 2],
    pub patches: usize,
}
#[derive(Clone, Copy)]
pub enum Method {
    Haar,
    Immerkaer,
    HighPass,
}

pub fn sampling(size: [u32; 2]) -> Sampling {
    let [w, h] = size.map(|v| v as usize);
    Sampling {
        patch_size: [w.min(PATCH), h.min(PATCH)],
        patches: (w / PATCH).clamp(1, 4) * (h / PATCH).clamp(1, 4),
    }
}
fn median(values: &mut [f64]) -> f64 {
    let n = values.len();
    let (lower, middle, _) = values.select_nth_unstable_by(n / 2, f64::total_cmp);
    if n % 2 == 0 {
        (*middle + lower.iter().copied().fold(f64::NEG_INFINITY, f64::max)) * 0.5
    } else {
        *middle
    }
}
fn mad(mut values: Vec<f64>) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let center = median(&mut values);
    for value in &mut values {
        *value = (*value - center).abs();
    }
    Some(median(&mut values) / MAD_NORMAL)
}

pub fn estimate(
    frame: &Frame,
    method: Method,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<f64>> {
    let (w, h) = (frame.width as usize, frame.height as usize);
    ensure!(
        w > 0 && h > 0 && frame.rgb.len() == w * h * 3,
        "Invalid noise RGB frame"
    );
    let [pw, ph] = sampling(frame.size()).patch_size;
    let (nx, ny) = ((w / PATCH).clamp(1, 4), (h / PATCH).clamp(1, 4));
    let capacity = match method {
        Method::Haar => nx * ny * (pw / 2) * (ph / 2),
        Method::Immerkaer => 0, // This estimator only accumulates a scalar sum.
        Method::HighPass => nx * ny * pw.saturating_sub(2) * ph.saturating_sub(2),
    };
    let mut values = Vec::with_capacity(capacity);
    // Convert each native patch once, rather than repeating RGB16 luminance
    // arithmetic for every overlapping 3x3 stencil. Scratch is at most 128 KiB.
    let mut patch = vec![0.; pw * ph];
    let (mut sum_abs, mut count) = (0., 0usize);
    for py in 0..ny {
        let y0 = if ny == 1 {
            (h - ph) / 2
        } else {
            py * (h - ph) / (ny - 1)
        };
        for px in 0..nx {
            let x0 = if nx == 1 {
                (w - pw) / 2
            } else {
                px * (w - pw) / (nx - 1)
            };
            for y in 0..ph {
                ensure!(!cancelled(), "Cancelled");
                let offset = ((y0 + y) * w + x0) * 3;
                for (value, pixel) in patch[y * pw..(y + 1) * pw]
                    .iter_mut()
                    .zip(frame.rgb[offset..offset + pw * 3].chunks_exact(3))
                {
                    *value = (0.2126 * pixel[0] as f64
                        + 0.7152 * pixel[1] as f64
                        + 0.0722 * pixel[2] as f64)
                        / 65535.;
                }
            }
            let read = |x: usize, y: usize| patch[y * pw + x];
            match method {
                Method::Haar => {
                    for y in (0..ph.saturating_sub(1)).step_by(2) {
                        ensure!(!cancelled(), "Cancelled");
                        for x in (0..pw.saturating_sub(1)).step_by(2) {
                            values.push(
                                (read(x, y) - read(x + 1, y) - read(x, y + 1) + read(x + 1, y + 1))
                                    * 0.5,
                            );
                        }
                    }
                }
                Method::Immerkaer | Method::HighPass => {
                    for y in 1..ph.saturating_sub(1) {
                        ensure!(!cancelled(), "Cancelled");
                        for x in 1..pw.saturating_sub(1) {
                            let cross =
                                read(x - 1, y) + read(x + 1, y) + read(x, y - 1) + read(x, y + 1);
                            if matches!(method, Method::Immerkaer) {
                                let corners = read(x - 1, y - 1)
                                    + read(x + 1, y - 1)
                                    + read(x - 1, y + 1)
                                    + read(x + 1, y + 1);
                                sum_abs += (corners - 2. * cross + 4. * read(x, y)).abs();
                                count += 1;
                            } else {
                                values.push(read(x, y) - cross * 0.25);
                            }
                        }
                    }
                }
            }
        }
    }
    ensure!(!cancelled(), "Cancelled");
    Ok(match method {
        Method::Haar => mad(values),
        Method::Immerkaer => {
            (count > 0).then(|| sum_abs * (std::f64::consts::PI / 2.).sqrt() / (6. * count as f64))
        }
        Method::HighPass => mad(values).map(|sigma| sigma / 1.25f64.sqrt()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    // Keep a direct RGB16 stencil oracle independent of the optimized patch
    // conversion. This also catches changes to edge/odd-size patch placement.
    fn reference(frame: &Frame, method: Method) -> Option<f64> {
        let (w, h) = (frame.width as usize, frame.height as usize);
        let [pw, ph] = sampling(frame.size()).patch_size;
        let (nx, ny) = ((w / PATCH).clamp(1, 4), (h / PATCH).clamp(1, 4));
        let read = |x: usize, y: usize| {
            let p = &frame.rgb[(y * w + x) * 3..][..3];
            (0.2126 * p[0] as f64 + 0.7152 * p[1] as f64 + 0.0722 * p[2] as f64) / 65535.
        };
        let mut values = Vec::new();
        let (mut sum_abs, mut count) = (0., 0usize);
        for py in 0..ny {
            let y0 = if ny == 1 {
                (h - ph) / 2
            } else {
                py * (h - ph) / (ny - 1)
            };
            for px in 0..nx {
                let x0 = if nx == 1 {
                    (w - pw) / 2
                } else {
                    px * (w - pw) / (nx - 1)
                };
                match method {
                    Method::Haar => {
                        for y in (0..ph.saturating_sub(1)).step_by(2) {
                            for x in (0..pw.saturating_sub(1)).step_by(2) {
                                let (x, y) = (x0 + x, y0 + y);
                                values.push(
                                    (read(x, y) - read(x + 1, y) - read(x, y + 1)
                                        + read(x + 1, y + 1))
                                        * 0.5,
                                );
                            }
                        }
                    }
                    Method::Immerkaer | Method::HighPass => {
                        for y in 1..ph.saturating_sub(1) {
                            for x in 1..pw.saturating_sub(1) {
                                let (x, y) = (x0 + x, y0 + y);
                                let cross = read(x - 1, y)
                                    + read(x + 1, y)
                                    + read(x, y - 1)
                                    + read(x, y + 1);
                                if matches!(method, Method::Immerkaer) {
                                    let corners = read(x - 1, y - 1)
                                        + read(x + 1, y - 1)
                                        + read(x - 1, y + 1)
                                        + read(x + 1, y + 1);
                                    sum_abs += (corners - 2. * cross + 4. * read(x, y)).abs();
                                    count += 1;
                                } else {
                                    values.push(read(x, y) - cross * 0.25);
                                }
                            }
                        }
                    }
                }
            }
        }
        match method {
            Method::Haar => mad(values),
            Method::Immerkaer => (count > 0)
                .then(|| sum_abs * (std::f64::consts::PI / 2.).sqrt() / (6. * count as f64)),
            Method::HighPass => mad(values).map(|sigma| sigma / 1.25f64.sqrt()),
        }
    }
    #[test]
    fn patch_luminance_matches_direct_rgb16_stencils_exactly() {
        for [w, h] in [
            [1, 1],
            [2, 7],
            [7, 2],
            [13, 19],
            [127, 131],
            [259, 523],
            [1003, 701],
        ] {
            let mut seed = 71921u32;
            let frame = Frame {
                width: w,
                height: h,
                rgb: (0..w * h * 3)
                    .map(|_| {
                        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                        (seed >> 16) as u16
                    })
                    .collect(),
            };
            for method in [Method::Haar, Method::Immerkaer, Method::HighPass] {
                assert_eq!(
                    estimate(&frame, method, &|| false).unwrap(),
                    reference(&frame, method)
                );
            }
        }
    }
    #[test]
    fn constant_and_native_gaussian_noise_have_known_sigma() {
        let mut frame = Frame {
            width: 512,
            height: 512,
            rgb: vec![32768; 512 * 512 * 3],
        };
        for method in [Method::Haar, Method::Immerkaer, Method::HighPass] {
            assert!(estimate(&frame, method, &|| false).unwrap().unwrap() < 1e-12);
        }
        let mut seed = 81721u64;
        let mut uniform = || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            ((seed >> 32) as f64 + 0.5) / 4294967296.
        };
        for rgb in frame.rgb.chunks_exact_mut(3) {
            let z = (-2. * uniform().ln()).sqrt() * (std::f64::consts::TAU * uniform()).cos();
            rgb.fill(((0.5 + 0.02 * z) * 65535.).round() as u16);
        }
        for method in [Method::Haar, Method::Immerkaer, Method::HighPass] {
            let sigma = estimate(&frame, method, &|| false).unwrap().unwrap();
            assert!((sigma - 0.02).abs() < 0.001, "sigma={sigma}");
            assert!(estimate(&frame, method, &|| true).is_err());
        }
        assert_eq!(sampling([8000, 6000]).patches, 16);
        assert_eq!(sampling([8000, 6000]).patch_size, [128, 128]);
    }
    #[test]
    fn tiny_images_do_not_invent_estimates() {
        let frame = Frame {
            width: 1,
            height: 1,
            rgb: vec![0; 3],
        };
        for method in [Method::Haar, Method::Immerkaer, Method::HighPass] {
            assert_eq!(estimate(&frame, method, &|| false).unwrap(), None);
        }
    }
}
