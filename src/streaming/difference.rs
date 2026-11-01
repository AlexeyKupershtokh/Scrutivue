//! Masked, incremental display-only difference. Cache only viewport mip cells;
//! never compare placeholders, coarse decoder stages or downsampled A/B colors.
use super::Samples;
use crate::model::{PhotoId, Settings};
use rayon::prelude::*;

#[derive(Default)]
pub(super) struct Preview {
    key: Option<([PhotoId; 2], [usize; 2])>,
    levels: Vec<Level>,
}
struct Level {
    level: usize,
    origin: [u32; 2],
    size: [u32; 2],
    cells: Vec<Option<[f32; 3]>>,
}
fn dimensions(size: [u32; 2], level: usize) -> [u32; 2] {
    size.map(|v| v.div_ceil(1 << level))
}
fn interpolate(
    x: f32,
    y: f32,
    zoom: [f32; 2],
    mut read: impl FnMut(i32, i32) -> Option<[f32; 3]>,
) -> Option<[f32; 3]> {
    let blend = |f: f32, z: f32| {
        if z > 2. {
            ((f - 0.5) * z + 0.5).clamp(0., 1.)
        } else {
            f
        }
    };
    let fx = blend(x - x.floor(), zoom[0]);
    let fy = blend(y - y.floor(), zoom[1]);
    let mut values = [[0.; 3]; 4];
    for (i, weight) in [
        (1. - fx) * (1. - fy),
        fx * (1. - fy),
        (1. - fx) * fy,
        fx * fy,
    ]
    .into_iter()
    .enumerate()
    {
        if weight > 0. {
            values[i] = read(
                x.floor() as i32 + (i & 1) as i32,
                y.floor() as i32 + (i >> 1) as i32,
            )?;
        }
    }
    Some(std::array::from_fn(|c| {
        let top = values[0][c] * (1. - fx) + values[1][c] * fx;
        let bottom = values[2][c] * (1. - fx) + values[3][c] * fx;
        top * (1. - fy) + bottom * fy
    }))
}
fn difference(a: &Samples<'_>, b: &Samples<'_>, x: u32, y: u32) -> Option<[f32; 3]> {
    // If A is already full, test the still-loading partner before reading A's
    // pixels. Missing B regions must not repeatedly scan the complete A frame.
    let av = if a.full.is_none() {
        Some(a.sample_exact(x, y)?)
    } else {
        None
    };
    let bv = if a.source.size == b.source.size {
        b.sample_exact(x, y)?.map(|v| v as f32 / 65535.)
    } else {
        interpolate(
            (x as f32 + 0.5) * b.source.size[0] as f32 / a.source.size[0] as f32 - 0.5,
            (y as f32 + 0.5) * b.source.size[1] as f32 / a.source.size[1] as f32 - 0.5,
            [1.; 2],
            |x, y| {
                b.sample_exact(
                    x.clamp(0, b.source.size[0] as i32 - 1) as u32,
                    y.clamp(0, b.source.size[1] as i32 - 1) as u32,
                )
                .map(|p| p.map(|v| v as f32 / 65535.))
            },
        )?
    };
    let av = av
        .or_else(|| a.sample_exact(x, y))?
        .map(|v| v as f32 / 65535.);
    Some(std::array::from_fn(|c| (av[c] - bv[c]).abs()))
}
fn aggregate(a: &Samples<'_>, b: &Samples<'_>, level: usize, x: u32, y: u32) -> Option<[f32; 3]> {
    if level == 0 {
        return difference(a, b, x, y);
    }
    let size = dimensions(a.source.size, level - 1);
    let x0 = (x * 2).min(size[0] - 1);
    let y0 = (y * 2).min(size[1] - 1);
    let x1 = (x * 2 + 1).min(size[0] - 1);
    let y1 = (y * 2 + 1).min(size[1] - 1);
    let p00 = aggregate(a, b, level - 1, x0, y0)?;
    let p10 = if x0 == x1 {
        p00
    } else {
        aggregate(a, b, level - 1, x1, y0)?
    };
    let p01 = if y0 == y1 {
        p00
    } else {
        aggregate(a, b, level - 1, x0, y1)?
    };
    let p11 = if x0 == x1 {
        p01
    } else if y0 == y1 {
        p10
    } else {
        aggregate(a, b, level - 1, x1, y1)?
    };
    let mut value = [0.; 3];
    for p in [p00, p10, p01, p11] {
        for c in 0..3 {
            value[c] += p[c] * 0.25;
        }
    }
    Some(value)
}
fn footprint_ready(a: &Samples<'_>, b: &Samples<'_>, level: usize, x: u32, y: u32) -> bool {
    let factor = 1 << level;
    let rect = [
        (x * factor).min(a.source.size[0] - 1),
        (y * factor).min(a.source.size[1] - 1),
        ((x + 1) * factor - 1).min(a.source.size[0] - 1),
        ((y + 1) * factor - 1).min(a.source.size[1] - 1),
    ];
    if !a.rect_ready(rect) {
        return false;
    }
    let mut mapped = rect;
    if a.source.size != b.source.size {
        for axis in 0..2 {
            let scale = b.source.size[axis] as f32 / a.source.size[axis] as f32;
            let first = (rect[axis] as f32 + 0.5) * scale - 0.5;
            let last = (rect[axis + 2] as f32 + 0.5) * scale - 0.5;
            mapped[axis] = first.floor().clamp(0., b.source.size[axis] as f32 - 1.) as u32;
            mapped[axis + 2] = last.ceil().clamp(0., b.source.size[axis] as f32 - 1.) as u32;
        }
    }
    b.rect_ready(mapped)
}
impl Preview {
    pub(super) fn clear(&mut self) {
        self.levels.clear();
        self.key = None;
    }
    fn prepare(&mut self, wanted: &[usize], settings: &Settings, size: [u32; 2]) {
        self.levels.retain(|l| wanted.contains(&l.level));
        let scale = settings.camera.scale(size, settings.viewport);
        for &level in wanted {
            if level == 0 {
                continue;
            }
            let factor = (1 << level) as f32;
            let dims = dimensions(size, level);
            let origin: [u32; 2] = std::array::from_fn(|a| {
                ((settings.camera.center[a] * size[a] as f32
                    - settings.viewport[a] as f32 / (2. * scale))
                    / factor
                    - 2.)
                    .floor()
                    .clamp(0., dims[a] as f32 - 1.) as u32
            });
            let extent: [u32; 2] = std::array::from_fn(|a| {
                let end = ((settings.camera.center[a] * size[a] as f32
                    + settings.viewport[a] as f32 / (2. * scale))
                    / factor
                    + 2.)
                    .ceil()
                    .clamp(origin[a] as f32 + 1., dims[a] as f32) as u32;
                end - origin[a]
            });
            if self
                .levels
                .iter()
                .any(|l| l.level == level && l.origin == origin && l.size == extent)
            {
                continue;
            }
            self.levels.retain(|l| l.level != level);
            self.levels.push(Level {
                level,
                origin,
                size: extent,
                cells: vec![None; extent[0] as usize * extent[1] as usize],
            });
        }
    }
    fn fill(&mut self, a: &Samples<'_>, b: &Samples<'_>) {
        for cache in &mut self.levels {
            let (level, origin, width) = (cache.level, cache.origin, cache.size[0]);
            // Each task owns its cache cell; no locks or duplicated aggregates.
            // Missing cells are retried at the next snapshot, ready cells reused.
            cache
                .cells
                .par_iter_mut()
                .enumerate()
                .for_each(|(index, cell)| {
                    if cell.is_some() {
                        return;
                    }
                    let x = origin[0] + index as u32 % width;
                    let y = origin[1] + index as u32 / width;
                    if footprint_ready(a, b, level, x, y) {
                        *cell = aggregate(a, b, level, x, y);
                    }
                });
        }
    }
    fn cell(
        &self,
        level: usize,
        x: i32,
        y: i32,
        a: &Samples<'_>,
        b: &Samples<'_>,
    ) -> Option<[f32; 3]> {
        let dims = dimensions(a.source.size, level);
        let x = x.clamp(0, dims[0] as i32 - 1) as u32;
        let y = y.clamp(0, dims[1] as i32 - 1) as u32;
        if level == 0 {
            return difference(a, b, x, y);
        }
        let cache = self.levels.iter().find(|l| l.level == level)?;
        if x < cache.origin[0]
            || y < cache.origin[1]
            || x >= cache.origin[0] + cache.size[0]
            || y >= cache.origin[1] + cache.size[1]
        {
            return None;
        }
        let index = ((y - cache.origin[1]) * cache.size[0] + x - cache.origin[0]) as usize;
        cache.cells[index]
    }
    fn sample(
        &self,
        level: usize,
        x: f32,
        y: f32,
        scale: f32,
        a: &Samples<'_>,
        b: &Samples<'_>,
    ) -> Option<[f32; 3]> {
        let factor = (1 << level) as f32;
        interpolate(
            (x + 0.5) / factor - 0.5,
            (y + 0.5) / factor - 0.5,
            [scale * factor; 2],
            |x, y| self.cell(level, x, y, a, b),
        )
    }
    #[cfg(test)]
    pub(super) fn render(
        &mut self,
        ids: [PhotoId; 2],
        a: &super::State,
        b: &super::State,
        settings: &Settings,
    ) -> (Vec<u8>, usize) {
        self.render_samples(ids, a.samples(), b.samples(), settings)
    }
    pub(super) fn render_samples(
        &mut self,
        ids: [PhotoId; 2],
        a: Samples<'_>,
        b: Samples<'_>,
        settings: &Settings,
    ) -> (Vec<u8>, usize) {
        debug_assert!(!settings.highpass);
        let key = (
            ids,
            [a, b].map(|s| {
                s.full
                    .map_or(0, |f| f as *const crate::decode::Frame as usize)
            }),
        );
        if self.key != Some(key) {
            self.clear();
            self.key = Some(key);
        }
        let [w, h] = settings.viewport;
        let size = a.source.size;
        let scale = settings.camera.scale(size, settings.viewport);
        let count = (0..32).find(|&l| dimensions(size, l) == [1, 1]).unwrap() + 1;
        let (low, high, t) = crate::processing::mip_lod(scale, count);
        let wanted = [low, high];
        self.prepare(&wanted[..if t > 0. { 2 } else { 1 }], settings, size);
        let paired = a.has_exact_pixels() && b.has_exact_pixels();
        if paired {
            self.fill(&a, &b);
        }
        let mut output = vec![0; w as usize * h as usize * 4];
        let ready = output
            .par_chunks_mut(w as usize * 4)
            .enumerate()
            .map(|(y, row)| {
                let y = y as u32;
                let mut ready = 0;
                for x in 0..w {
                    let sx = (x as f32 + 0.5 - w as f32 * 0.5) / scale
                        + settings.camera.center[0] * size[0] as f32
                        - 0.5;
                    let sy = (y as f32 + 0.5 - h as f32 * 0.5) / scale
                        + settings.camera.center[1] * size[1] as f32
                        - 0.5;
                    let inside = sx >= -0.5
                        && sy >= -0.5
                        && sx < size[0] as f32 - 0.5
                        && sy < size[1] as f32 - 0.5;
                    let value = if inside && paired {
                        self.sample(low, sx, sy, scale, &a, &b).and_then(|lo| {
                            if t == 0. {
                                return Some(lo);
                            }
                            self.sample(high, sx, sy, scale, &a, &b)
                                .map(|hi| std::array::from_fn(|c| lo[c] * (1. - t) + hi[c] * t))
                        })
                    } else {
                        None
                    };
                    let p = x as usize * 4;
                    row[p + 3] = 255;
                    if let Some(value) = value {
                        ready += 1;
                        for c in 0..3 {
                            row[p + 2 - c] = (value[c] * settings.gain as f32)
                                .clamp(0., 1.)
                                .mul_add(255., 0.)
                                .round_ties_even()
                                as u8;
                        }
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
                ready
            })
            .sum();
        (output, ready)
    }
}

#[cfg(test)]
mod tests;
