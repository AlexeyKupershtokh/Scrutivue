use crate::{
    decode::Frame,
    model::{Mode, PhotoId, Settings},
};
use rayon::prelude::*;
use std::collections::HashMap;

pub fn gaussian_weights(sigma: f32) -> Vec<f32> {
    let sigma = sigma.clamp(0.05, 64.0);
    let radius = (sigma * 3.0).ceil() as i32;
    let mut weights: Vec<_> = (-radius..=radius)
        .map(|x| (-0.5 * (x as f32 / sigma).powi(2)).exp())
        .collect();
    let sum: f32 = weights.iter().sum();
    for weight in &mut weights {
        *weight /= sum;
    }
    weights
}

pub fn highpass(frame: &Frame, sigmas: [f32; 2]) -> Vec<f32> {
    let w = frame.width as usize;
    let h = frame.height as usize;
    let wx = gaussian_weights(sigmas[0]);
    let wy = gaussian_weights(sigmas[1]);
    let rx = (wx.len() / 2) as isize;
    let ry = (wy.len() / 2) as isize;
    let mut temporary = vec![0.0f32; frame.rgb.len()];
    temporary
        .par_chunks_mut(w * 3)
        .enumerate()
        .for_each(|(y, row)| {
            for x in 0..w {
                for c in 0..3 {
                    row[x * 3 + c] = wx
                        .iter()
                        .enumerate()
                        .map(|(k, weight)| {
                            let sx =
                                (x as isize + k as isize - rx).clamp(0, w as isize - 1) as usize;
                            frame.rgb[(y * w + sx) * 3 + c] as f32 / 65535.0 * weight
                        })
                        .sum();
                }
            }
        });
    let mut result = vec![0.0f32; frame.rgb.len()];
    result
        .par_chunks_mut(w * 3)
        .enumerate()
        .for_each(|(y, row)| {
            for x in 0..w {
                for c in 0..3 {
                    let blur: f32 = wy
                        .iter()
                        .enumerate()
                        .map(|(k, weight)| {
                            let sy =
                                (y as isize + k as isize - ry).clamp(0, h as isize - 1) as usize;
                            temporary[(sy * w + x) * 3 + c] * weight
                        })
                        .sum();
                    row[x * 3 + c] = frame.rgb[(y * w + x) * 3 + c] as f32 / 65535.0 - blur;
                }
            }
        });
    result
}

fn interpolate(x: f32, y: f32, zoom: [f32; 2], read: impl Fn(i32, i32, usize) -> f32) -> [f32; 3] {
    let ix = x.floor() as i32;
    let iy = y.floor() as i32;
    let blend = |f: f32, z: f32| {
        if z > 2. {
            ((f - 0.5) * z + 0.5).clamp(0., 1.)
        } else {
            f
        }
    };
    let fx = blend(x - x.floor(), zoom[0]);
    let fy = blend(y - y.floor(), zoom[1]);
    std::array::from_fn(|c| {
        let top = read(ix, iy, c) * (1.0 - fx) + read(ix + 1, iy, c) * fx;
        let bottom = read(ix, iy + 1, c) * (1.0 - fx) + read(ix + 1, iy + 1, c) * fx;
        top * (1.0 - fy) + bottom * fy
    })
}
pub(crate) fn sample(
    frame: &Frame,
    filtered: Option<&[f32]>,
    x: f32,
    y: f32,
    zoom: [f32; 2],
) -> [f32; 3] {
    let read = |x: i32, y: i32, c: usize| {
        let index = (y.clamp(0, frame.height as i32 - 1) as usize * frame.width as usize
            + x.clamp(0, frame.width as i32 - 1) as usize)
            * 3
            + c;
        filtered
            .map(|v| v[index])
            .unwrap_or_else(|| frame.rgb[index] as f32 / 65535.0)
    };
    interpolate(x, y, zoom, read)
}

pub type DiffKey = ([PhotoId; 2], bool, u32);
pub fn diff_key(a: PhotoId, b: PhotoId, s: &Settings) -> DiffKey {
    (
        [a, b],
        s.highpass,
        if s.highpass { s.sigma.to_bits() } else { 0 },
    )
}
pub fn mip_lod(scale: f32, count: usize) -> (usize, usize, f32) {
    let lod = (1. / scale).log2().max(0.).min((count - 1) as f32);
    let low = lod.floor() as usize;
    (low, (low + 1).min(count - 1), lod - low as f32)
}
struct Mip {
    size: [u32; 2],
    values: Vec<f32>,
}
fn difference_pyramid(a: &Frame, b: &Frame, fa: Option<&[f32]>, fb: Option<&[f32]>) -> Vec<Mip> {
    let mut values = vec![0.; a.rgb.len()];
    values.par_chunks_mut(3).enumerate().for_each(|(i, out)| {
        let x = (i % a.width as usize) as f32;
        let y = (i / a.width as usize) as f32;
        let av = sample(a, fa, x, y, [1.; 2]);
        let bv = sample(
            b,
            fb,
            (x + 0.5) * b.width as f32 / a.width as f32 - 0.5,
            (y + 0.5) * b.height as f32 / a.height as f32 - 0.5,
            [1.; 2],
        );
        for c in 0..3 {
            out[c] = (av[c] - bv[c]).abs();
        }
    });
    let mut levels = vec![Mip {
        size: a.size(),
        values,
    }];
    while levels.last().unwrap().size != [1, 1] {
        let src = levels.last().unwrap();
        let [w, h] = src.size;
        let size = [w.div_ceil(2), h.div_ceil(2)];
        let mut values = vec![0.; (size[0] * size[1] * 3) as usize];
        values.par_chunks_mut(3).enumerate().for_each(|(i, out)| {
            let x = (i % size[0] as usize) as u32 * 2;
            let y = (i / size[0] as usize) as u32 * 2;
            for c in 0..3 {
                for dy in 0..2 {
                    for dx in 0..2 {
                        out[c] += src.values
                            [(((y + dy).min(h - 1) * w + (x + dx).min(w - 1)) * 3) as usize + c]
                            * 0.25;
                    }
                }
            }
        });
        levels.push(Mip { size, values });
    }
    levels
}
fn sample_mip(mip: &Mip, level: usize, x: f32, y: f32, scale: f32) -> [f32; 3] {
    let factor = (1u32 << level) as f32;
    interpolate(
        (x + 0.5) / factor - 0.5,
        (y + 0.5) / factor - 0.5,
        [scale * factor; 2],
        |x, y, c| {
            mip.values[((y.clamp(0, mip.size[1] as i32 - 1) as u32 * mip.size[0]
                + x.clamp(0, mip.size[0] as i32 - 1) as u32)
                * 3) as usize
                + c]
        },
    )
}

pub struct Cpu {
    filtered: HashMap<PhotoId, ([u32; 2], Vec<f32>)>,
    difference: Option<(DiffKey, Vec<Mip>)>,
}
impl Cpu {
    pub fn new() -> Self {
        Self {
            filtered: HashMap::new(),
            difference: None,
        }
    }
    pub fn render(
        &mut self,
        a: (PhotoId, &Frame),
        b: (PhotoId, &Frame),
        settings: &Settings,
        show_b: bool,
    ) -> Vec<u8> {
        let key = diff_key(a.0, b.0, settings);
        if self.difference.as_ref().is_some_and(|(old, _)| *old != key) {
            self.difference = None;
        }
        self.filtered
            .retain(|id, _| settings.highpass && (*id == a.0 || *id == b.0));
        if settings.highpass {
            for (id, frame, sigmas) in [
                (a.0, a.1, [settings.sigma; 2]),
                (
                    b.0,
                    b.1,
                    [
                        settings.sigma * b.1.width as f32 / a.1.width as f32,
                        settings.sigma * b.1.height as f32 / a.1.height as f32,
                    ],
                ),
            ] {
                let key = sigmas.map(f32::to_bits);
                if self.filtered.get(&id).map(|x| x.0) != Some(key) {
                    self.filtered.remove(&id);
                    self.filtered.insert(id, (key, highpass(frame, sigmas)));
                }
            }
        }
        let fa = self.filtered.get(&a.0).map(|x| x.1.as_slice());
        let fb = self.filtered.get(&b.0).map(|x| x.1.as_slice());
        let [vw, vh] = settings.viewport;
        let scale = settings.camera.scale(a.1.size(), settings.viewport);
        if settings.mode == Mode::Diff && self.difference.is_none() {
            self.difference = Some((key, difference_pyramid(a.1, b.1, fa, fb)));
        }
        let difference = self.difference.as_ref().map(|(_, levels)| levels);
        let mut pixels = vec![0u8; vw as usize * vh as usize * 4];
        pixels
            .par_chunks_mut(vw as usize * 4)
            .enumerate()
            .for_each(|(y, row)| {
                for x in 0..vw as usize {
                    let sx = (x as f32 + 0.5 - vw as f32 * 0.5) / scale
                        + settings.camera.center[0] * a.1.width as f32
                        - 0.5;
                    let sy = (y as f32 + 0.5 - vh as f32 * 0.5) / scale
                        + settings.camera.center[1] * a.1.height as f32
                        - 0.5;
                    let color = if sx < -0.5
                        || sy < -0.5
                        || sx >= a.1.width as f32 - 0.5
                        || sy >= a.1.height as f32 - 0.5
                    {
                        [if (x / 16 + y / 16) & 1 == 1 {
                            0.075
                        } else {
                            0.085
                        }; 3]
                    } else {
                        let bx = (sx + 0.5) * b.1.width as f32 / a.1.width as f32 - 0.5;
                        let by = (sy + 0.5) * b.1.height as f32 / a.1.height as f32 - 0.5;
                        if settings.mode == Mode::Diff {
                            let levels = difference.unwrap();
                            let (low, high, t) = mip_lod(scale, levels.len());
                            let av = sample_mip(&levels[low], low, sx, sy, scale);
                            let bv = sample_mip(&levels[high], high, sx, sy, scale);
                            std::array::from_fn(|c| {
                                (av[c] * (1. - t) + bv[c] * t) * settings.gain as f32
                            })
                        } else {
                            let second = if settings.mode == Mode::Slider {
                                x as f32 >= settings.split * vw as f32
                            } else {
                                show_b
                            };
                            let v = if second {
                                sample(
                                    b.1,
                                    fb,
                                    bx,
                                    by,
                                    [
                                        scale * a.1.width as f32 / b.1.width as f32,
                                        scale * a.1.height as f32 / b.1.height as f32,
                                    ],
                                )
                            } else {
                                sample(a.1, fa, sx, sy, [scale; 2])
                            };
                            v.map(|v| v + if settings.highpass { 0.5 } else { 0.0 })
                        }
                    };
                    for c in 0..3 {
                        row[x * 4 + 2 - c] =
                            (color[c].clamp(0.0, 1.0) * 255.0).round_ties_even() as u8;
                    }
                    row[x * 4 + 3] = 255;
                }
            });
        pixels
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pixel_interiors_are_flat_but_edges_remain_continuous() {
        let read = |x: i32, _: i32, _: usize| if x >= 1 { 1. } else { 0. };
        assert_eq!(interpolate(0.2, 0., [8.; 2], read), [0.; 3]);
        let left = interpolate(0.49, 0., [8.; 2], read)[0];
        let right = interpolate(0.51, 0., [8.; 2], read)[0];
        assert!(left > 0. && left < right && right < 1.);
        assert!((interpolate(0.2, 0., [2.; 2], read)[0] - 0.2).abs() < 1e-6);
    }
    #[test]
    fn difference_is_averaged_after_absolute_value_and_has_no_checker_moire() {
        let mut a = frame(64, 64, 0);
        let mut b = frame(64, 64, 0);
        for y in 0..64 {
            for x in 0..64 {
                for c in 0..3 {
                    let i = (y * 64 + x) * 3 + c;
                    a.rgb[i] = if (x + y) % 2 == 0 { 0 } else { 65535 };
                    b.rgb[i] = 65535 - a.rgb[i];
                }
            }
        }
        let mut s = Settings {
            mode: Mode::Diff,
            gain: 1,
            viewport: [16, 16],
            ..Settings::default()
        };
        for scale in [0.14, 0.25, 0.32] {
            s.camera.zoom = crate::model::Zoom::Fixed(scale);
            let out = Cpu::new().render((1, &a), (2, &b), &s, false);
            assert_eq!(&out[(8 * 16 + 8) * 4..(8 * 16 + 8) * 4 + 4], &[255; 4]);
        }
        let a = frame(64, 64, 0);
        for scale in [0.25, 0.32] {
            s.camera.zoom = crate::model::Zoom::Fixed(scale);
            let out = Cpu::new().render((1, &a), (2, &b), &s, false);
            let values: Vec<_> = out.chunks_exact(4).map(|p| p[0]).collect();
            assert!(values.iter().all(|v| v.abs_diff(128) <= 1));
        }
    }
    #[test]
    fn output_is_bgra_for_gpui() {
        let a = Frame {
            width: 1,
            height: 1,
            rgb: vec![65535, 0, 0],
        };
        let b = Frame {
            width: 1,
            height: 1,
            rgb: vec![0, 0, 65535],
        };
        let s = Settings {
            viewport: [1, 1],
            mode: Mode::Click,
            ..Settings::default()
        };
        let mut cpu = Cpu::new();
        assert_eq!(cpu.render((1, &a), (2, &b), &s, false), [0, 0, 255, 255]);
        assert_eq!(cpu.render((1, &a), (2, &b), &s, true), [255, 0, 0, 255]);
    }
    fn frame(w: u32, h: u32, value: u16) -> Frame {
        Frame {
            width: w,
            height: h,
            rgb: vec![value; w as usize * h as usize * 3],
        }
    }
    #[test]
    fn gaussian_is_normalized_and_symmetric() {
        let w = gaussian_weights(2.5);
        assert!((w.iter().sum::<f32>() - 1.0).abs() < 1e-6);
        assert_eq!(w, w.iter().rev().copied().collect::<Vec<_>>());
    }
    #[test]
    fn highpass_removes_constant_including_edges() {
        let hp = highpass(&frame(8, 6, 24000), [2.0; 2]);
        assert!(hp.iter().all(|v| v.abs() < 1e-6));
    }
    #[test]
    fn difference_and_gain() {
        let a = frame(2, 2, 10000);
        let b = frame(2, 2, 11000);
        let mut s = Settings::default();
        s.viewport = [2, 2];
        s.mode = Mode::Diff;
        s.gain = 16;
        let out = Cpu::new().render((1, &a), (2, &b), &s, false);
        assert_eq!(&out[..4], &[62, 62, 62, 255]);
        s.gain = 256;
        assert_eq!(
            &Cpu::new().render((1, &a), (2, &b), &s, false)[..4],
            &[255; 4]
        );
    }
    #[test]
    fn different_dimensions_align_at_pixel_centers() {
        let a = frame(4, 4, 20000);
        let b = frame(2, 3, 20000);
        let mut s = Settings::default();
        s.viewport = [4, 4];
        s.mode = Mode::Diff;
        let out = Cpu::new().render((1, &a), (2, &b), &s, false);
        assert!(out.chunks_exact(4).all(|p| p == [0, 0, 0, 255]));
    }
    #[test]
    fn slider_and_click_select_correct_source() {
        let a = frame(4, 2, 0);
        let b = frame(4, 2, 65535);
        let mut s = Settings::default();
        s.mode = Mode::Slider;
        s.viewport = [4, 2];
        let mut cpu = Cpu::new();
        let out = cpu.render((1, &a), (2, &b), &s, false);
        assert_eq!(&out[0..4], &[0, 0, 0, 255]);
        assert_eq!(&out[12..16], &[255; 4]);
        s.mode = Mode::Click;
        assert_eq!(&cpu.render((1, &a), (2, &b), &s, true)[..4], &[255; 4]);
    }
}
