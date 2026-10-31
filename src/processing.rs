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

fn sample(frame: &Frame, filtered: Option<&[f32]>, x: f32, y: f32) -> [f32; 3] {
    let ix = x.floor() as i32;
    let iy = y.floor() as i32;
    let fx = x - x.floor();
    let fy = y - y.floor();
    let read = |x: i32, y: i32, c: usize| {
        let index = (y.clamp(0, frame.height as i32 - 1) as usize * frame.width as usize
            + x.clamp(0, frame.width as i32 - 1) as usize)
            * 3
            + c;
        filtered
            .map(|v| v[index])
            .unwrap_or_else(|| frame.rgb[index] as f32 / 65535.0)
    };
    std::array::from_fn(|c| {
        let top = read(ix, iy, c) * (1.0 - fx) + read(ix + 1, iy, c) * fx;
        let bottom = read(ix, iy + 1, c) * (1.0 - fx) + read(ix + 1, iy + 1, c) * fx;
        top * (1.0 - fy) + bottom * fy
    })
}

pub struct Cpu {
    filtered: HashMap<PhotoId, ([u32; 2], Vec<f32>)>,
}
impl Cpu {
    pub fn new() -> Self {
        Self {
            filtered: HashMap::new(),
        }
    }
    pub fn render(
        &mut self,
        a: (PhotoId, &Frame),
        b: (PhotoId, &Frame),
        settings: &Settings,
        show_b: bool,
    ) -> Vec<u8> {
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
                            let av = sample(a.1, fa, sx, sy);
                            let bv = sample(b.1, fb, bx, by);
                            std::array::from_fn(|c| (av[c] - bv[c]).abs() * settings.gain as f32)
                        } else {
                            let second = if settings.mode == Mode::Slider {
                                x as f32 >= settings.split * vw as f32
                            } else {
                                show_b
                            };
                            let v = if second {
                                sample(b.1, fb, bx, by)
                            } else {
                                sample(a.1, fa, sx, sy)
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
            mode: Mode::Single,
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
        s.viewport = [4, 2];
        let mut cpu = Cpu::new();
        let out = cpu.render((1, &a), (2, &b), &s, false);
        assert_eq!(&out[0..4], &[0, 0, 0, 255]);
        assert_eq!(&out[12..16], &[255; 4]);
        s.mode = Mode::Click;
        assert_eq!(&cpu.render((1, &a), (2, &b), &s, true)[..4], &[255; 4]);
    }
}
