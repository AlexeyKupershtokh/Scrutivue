//! Full-frame pixel errors and explicitly sized perceptual comparisons.
//! These inputs never contain viewport transforms, gain or the high-pass filter.
use crate::{decode::Frame, lfu::Lfu, model::PhotoId, noise, services::Mailbox};
use anyhow::{bail, Context, Result};
use crossbeam_channel::{bounded, Receiver, Sender, TrySendError};
use ort::{
    session::{RunOptions, Session},
    value::Tensor,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, Weak,
    },
    thread,
    time::Instant,
};
mod model;

pub fn run_options() -> Result<RunOptions> {
    static RUNTIME: std::sync::OnceLock<std::result::Result<(), String>> =
        std::sync::OnceLock::new();
    let result = RUNTIME.get_or_init(|| {
        let name = if cfg!(windows) {
            "onnxruntime.dll"
        } else if cfg!(target_os = "macos") {
            "libonnxruntime.dylib"
        } else {
            "libonnxruntime.so"
        };
        let load = (|| -> Result<()> {
            let path = std::env::current_exe()?.with_file_name(name);
            anyhow::ensure!(
                path.is_file(),
                "{name} is missing beside the executable; restore it from the distribution"
            );
            ort::init_from(path)?.commit();
            Ok(())
        })();
        load.map_err(|e| format!("{e:#}"))
    });
    if let Err(error) = result {
        bail!("{error}");
    }
    Ok(RunOptions::new()?)
}

#[derive(Clone, Debug, Serialize)]
pub struct PixelStats {
    /// None represents +infinity (identical images), not a missing measurement.
    pub psnr_db: Option<f64>,
    pub mae: f64,
    pub rmse: f64,
    pub max_error: f64,
    pub bias_b_minus_a: f64,
    pub changed_percent: f64,
}
#[derive(Clone, Debug, Serialize)]
pub struct Neural {
    pub lpips: f64,
}
#[derive(Clone, Debug, Serialize)]
pub struct Report {
    pub original_size: [u32; 2],
    pub original_size_b: Option<[u32; 2]>,
    pub comparison_error: Option<String>,
    pub perceptual_size: [u32; 2],
    pub neural_size: [u32; 2],
    pub pixels: Option<PixelStats>,
    pub ssim_y: Option<f64>,
    pub ms_ssim_y: Option<f64>,
    pub gmsd_y: Option<f64>,
    pub ssimulacra2: std::result::Result<f64, String>,
    pub neural: std::result::Result<Neural, String>,
    pub neural_pending: bool,
    pub noise_a: noise::Estimates,
    pub noise_b: noise::Estimates,
    pub noise_sampling: noise::Sampling,
    pub noise_sampling_b: Option<noise::Sampling>,
    /// Distinguishes not-yet-computed values from unsupported small-image scales.
    pub pending: Vec<Stage>,
    pub elapsed_ms: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Pixels,
    HaarA,
    HaarB,
    ImmerkaerA,
    ImmerkaerB,
    HighPassA,
    HighPassB,
    Ssim,
    Gmsd,
    MsSsim,
    Ssimulacra2,
    Neural,
}
impl Report {
    fn empty(size: [u32; 2], b_size: Option<[u32; 2]>, edge: u32) -> Self {
        let comparison_error = match b_size {
            None => {
                Some("Choose B to compare; A's noise estimates are available separately.".into())
            }
            Some(b) if b != size => Some(format!(
                "Different resolutions: A {} × {}, B {} × {}. Paired metrics are unavailable.",
                size[0], size[1], b[0], b[1]
            )),
            Some(_) => None,
        };
        let mut pending = vec![Stage::HaarA, Stage::ImmerkaerA, Stage::HighPassA];
        if b_size.is_some() {
            pending.extend([Stage::HaarB, Stage::ImmerkaerB, Stage::HighPassB]);
        }
        if comparison_error.is_none() {
            pending.extend([
                Stage::Pixels,
                Stage::Ssim,
                Stage::Gmsd,
                Stage::MsSsim,
                Stage::Ssimulacra2,
                Stage::Neural,
            ]);
        }
        let pair_status = comparison_error
            .clone()
            .unwrap_or_else(|| "Computing…".into());
        Self {
            original_size: size,
            original_size_b: b_size,
            comparison_error,
            perceptual_size: analysis_size(size, edge),
            neural_size: analysis_size(size, 768),
            pixels: None,
            ssim_y: None,
            ms_ssim_y: None,
            gmsd_y: None,
            ssimulacra2: Err(pair_status.clone()),
            neural: Err(pair_status),
            neural_pending: pending.contains(&Stage::Neural),
            noise_a: noise::Estimates::default(),
            noise_b: noise::Estimates::default(),
            noise_sampling: noise::sampling(size),
            noise_sampling_b: b_size.map(noise::sampling),
            pending,
            elapsed_ms: 0.,
        }
    }
    pub fn is_pending(&self, stage: Stage) -> bool {
        self.pending.contains(&stage)
    }
    pub fn is_complete(&self) -> bool {
        self.pending.is_empty()
    }
}

fn check(cancelled: &impl Fn() -> bool) -> Result<()> {
    if cancelled() {
        bail!("Cancelled");
    }
    Ok(())
}
pub fn analysis_size(size: [u32; 2], edge: u32) -> [u32; 2] {
    let scale = (edge.clamp(32, 1536) as f64 / size[0].max(size[1]).max(1) as f64).min(1.);
    size.map(|v| (v as f64 * scale).round().max(1.) as u32)
}
struct TriangleTaps {
    first: usize,
    weights: Vec<f32>,
}
fn triangle_taps(input: u32, output: u32) -> Vec<TriangleTaps> {
    let ratio = input as f32 / output as f32;
    let support = ratio.max(1.);
    (0..output)
        .map(|out| {
            // Match image 0.25's pixel centers, clipped support, normalization
            // and f32 operation order, including zero-weight boundary taps.
            let center = (out as f32 + 0.5) * ratio;
            let first = ((center - support).floor() as i64).clamp(0, input as i64 - 1);
            let end = ((center + support).ceil() as i64).clamp(first + 1, input as i64);
            let center = center - 0.5;
            let mut weights: Vec<_> = (first..end)
                .map(|i| {
                    let distance = ((i as f32 - center) / support).abs();
                    if distance < 1. {
                        1. - distance
                    } else {
                        0.
                    }
                })
                .collect();
            let sum: f32 = weights.iter().sum();
            for weight in &mut weights {
                *weight /= sum;
            }
            TriangleTaps {
                first: first as usize,
                weights,
            }
        })
        .collect()
}
fn sample(frame: &Frame, size: [u32; 2], cancelled: &impl Fn() -> bool) -> Result<Vec<[f32; 3]>> {
    let convert = |p: &[u16]| {
        [
            p[0] as f32 / 65535.,
            p[1] as f32 / 65535.,
            p[2] as f32 / 65535.,
        ]
    };
    if frame.size() == size {
        let mut out = Vec::with_capacity(size[0] as usize * size[1] as usize);
        for row in frame.rgb.chunks_exact(frame.width as usize * 3) {
            check(cancelled)?;
            out.extend(row.chunks_exact(3).map(convert));
        }
        return Ok(out);
    }
    // Stream the same vertical-then-horizontal separable Triangle filter as
    // image::resize, but retain only one RGB32F intermediate row. The generic
    // resizer allocates source_width * output_height * four f32 channels.
    // Final RGB16 rounding is intentional: existing metric inputs use it too.
    let horizontal = triangle_taps(frame.width, size[0]);
    let vertical = triangle_taps(frame.height, size[1]);
    let width = frame.width as usize;
    let mut row = vec![[0f32; 3]; width];
    let mut out = Vec::with_capacity(size[0] as usize * size[1] as usize);
    for taps in vertical {
        check(cancelled)?;
        row.fill([0.; 3]);
        for (i, &weight) in taps.weights.iter().enumerate() {
            check(cancelled)?;
            let offset = (taps.first + i) * width * 3;
            let source = &frame.rgb[offset..offset + width * 3];
            for (value, pixel) in row.iter_mut().zip(source.chunks_exact(3)) {
                for c in 0..3 {
                    value[c] += pixel[c] as f32 * weight;
                }
            }
        }
        for taps in &horizontal {
            let mut value = [0f32; 3];
            for (pixel, weight) in row[taps.first..].iter().zip(&taps.weights) {
                for c in 0..3 {
                    value[c] += pixel[c] * weight;
                }
            }
            out.push(value.map(|value| value.clamp(0., 65535.).round() / 65535.));
        }
    }
    Ok(out)
}
fn pixel_stats(a: &Frame, b: &Frame, cancelled: &impl Fn() -> bool) -> Result<PixelStats> {
    let (mut squared, mut absolute, mut bias, mut maximum, mut changed) =
        (0., 0., 0., 0f64, 0usize);
    for (i, (a, b)) in a.rgb.chunks_exact(3).zip(b.rgb.chunks_exact(3)).enumerate() {
        if i % 65536 == 0 {
            check(cancelled)?;
        }
        if a != b {
            changed += 1;
        }
        for c in 0..3 {
            let delta = (b[c] as f64 - a[c] as f64) / 65535.;
            squared += delta * delta;
            absolute += delta.abs();
            bias += delta;
            maximum = maximum.max(delta.abs());
        }
    }
    let samples = a.rgb.len() as f64;
    let mse = squared / samples;
    Ok(PixelStats {
        psnr_db: (mse > 0.).then(|| -10. * mse.log10()),
        mae: absolute / samples,
        rmse: mse.sqrt(),
        max_error: maximum,
        bias_b_minus_a: bias / samples,
        changed_percent: changed as f64 * 300. / samples,
    })
}
fn luma(rgb: &[[f32; 3]]) -> Vec<f64> {
    rgb.iter()
        .map(|p| 0.2126 * p[0] as f64 + 0.7152 * p[1] as f64 + 0.0722 * p[2] as f64)
        .collect()
}
fn down2(a: &[f64], w: usize, h: usize) -> (Vec<f64>, usize, usize) {
    let (nw, nh) = (w.div_ceil(2), h.div_ceil(2));
    let mut out = vec![0.; nw * nh];
    for y in 0..nh {
        for x in 0..nw {
            out[y * nw + x] = (a[(2 * y) * w + 2 * x]
                + a[(2 * y) * w + (2 * x + 1).min(w - 1)]
                + a[(2 * y + 1).min(h - 1) * w + 2 * x]
                + a[(2 * y + 1).min(h - 1) * w + (2 * x + 1).min(w - 1)])
                / 4.;
        }
    }
    (out, nw, nh)
}
/// Wang SSIM: 11x11 Gaussian sigma=1.5, population covariance, valid window.
/// A ring buffer keeps moment storage O(width), even for large inputs.
fn ssim(
    a: &[f64],
    b: &[f64],
    w: usize,
    h: usize,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<(f64, f64)>> {
    if w < 11 || h < 11 {
        return Ok(None);
    }
    let mut kernel = [0.; 11];
    for (i, v) in kernel.iter_mut().enumerate() {
        *v = (-((i as f64 - 5.).powi(2)) / 4.5).exp();
    }
    let total: f64 = kernel.iter().sum();
    for v in &mut kernel {
        *v /= total;
    }
    let ow = w - 10;
    let mut ring = vec![vec![[0.; 5]; ow]; 11];
    let (mut sum, mut contrast) = (0., 0.);
    for y in 0..h {
        check(cancelled)?;
        for x in 0..ow {
            let mut m = [0.; 5];
            for k in 0..11 {
                let (a, b) = (a[y * w + x + k], b[y * w + x + k]);
                for (v, value) in m.iter_mut().zip([a, b, a * a, b * b, a * b]) {
                    *v += kernel[k] * value;
                }
            }
            ring[y % 11][x] = m;
        }
        if y < 10 {
            continue;
        }
        for x in 0..ow {
            let mut m = [0.; 5];
            for k in 0..11 {
                for (v, value) in m.iter_mut().zip(ring[(y + 1 + k) % 11][x]) {
                    *v += kernel[k] * value;
                }
            }
            let [ma, mb, aa, bb, ab] = m;
            let va = (aa - ma * ma).max(0.);
            let vb = (bb - mb * mb).max(0.);
            let cs = (2. * (ab - ma * mb) + 0.0009) / (va + vb + 0.0009);
            sum += ((2. * ma * mb + 0.0001) / (ma * ma + mb * mb + 0.0001)) * cs;
            contrast += cs;
        }
    }
    let count = (ow * (h - 10)) as f64;
    Ok(Some((sum / count, contrast / count)))
}
fn multi_ssim(
    mut a: Vec<f64>,
    mut b: Vec<f64>,
    mut w: usize,
    mut h: usize,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<f64>> {
    if w.min(h) < 176 {
        return Ok(None);
    }
    let mut result = 1.;
    for (level, weight) in [0.0448, 0.2856, 0.3001, 0.2363, 0.1333]
        .into_iter()
        .enumerate()
    {
        let (score, cs) = ssim(&a, &b, w, h, cancelled)?.unwrap();
        result *= (if level == 4 { score } else { cs })
            .clamp(0., 1.)
            .powf(weight);
        if level != 4 {
            let (next_a, nw, nh) = down2(&a, w, h);
            let (next_b, _, _) = down2(&b, w, h);
            a = next_a;
            b = next_b;
            w = nw;
            h = nh;
        }
    }
    Ok(Some(result))
}
fn gmsd_impl(a: &[f64], b: &[f64], w: usize, h: usize) -> Option<f64> {
    if w < 3 || h < 3 {
        return None;
    }
    let gradient = |p: &[f64], x: i32, y: i32| {
        let read = |x: i32, y: i32| {
            if x < 0 || y < 0 || x >= w as i32 || y >= h as i32 {
                0.
            } else {
                p[y as usize * w + x as usize]
            }
        };
        let dx = ((read(x + 1, y - 1) + read(x + 1, y) + read(x + 1, y + 1))
            - (read(x - 1, y - 1) + read(x - 1, y) + read(x - 1, y + 1)))
            / 3.;
        let dy = ((read(x - 1, y + 1) + read(x, y + 1) + read(x + 1, y + 1))
            - (read(x - 1, y - 1) + read(x, y - 1) + read(x + 1, y - 1)))
            / 3.;
        (dx * dx + dy * dy).sqrt()
    };
    let mut scores = Vec::with_capacity(w * h);
    const C: f64 = 170. / (255. * 255.);
    for y in 0..h as i32 {
        for x in 0..w as i32 {
            let (a, b) = (gradient(a, x, y), gradient(b, x, y));
            scores.push((2. * a * b + C) / (a * a + b * b + C));
        }
    }
    let mean = scores.iter().sum::<f64>() / scores.len() as f64;
    Some((scores.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / scores.len() as f64).sqrt())
}
fn gmsd_pool(a: &[f64], w: usize, h: usize) -> (Vec<f64>, usize, usize) {
    let (nw, nh) = (w.div_ceil(2), h.div_ceil(2));
    let mut out = vec![0.; nw * nh];
    for y in 0..h {
        for x in 0..w {
            out[(y / 2) * nw + x / 2] += a[y * w + x] * 0.25;
        }
    }
    (out, nw, nh)
}

fn validate(a: &Frame) -> Result<()> {
    if a.width == 0 || a.height == 0 || a.rgb.len() != a.width as usize * a.height as usize * 3 {
        bail!("Invalid RGB frame dimensions");
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct Source {
    // Statistics depend only on decoded pixels, not library IDs or file paths.
    // Dimensions are included in this SHA-256 revision as well as RGB16 values.
    revision: [u8; 32],
}
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct PairKey {
    a: Source,
    b: Option<Source>,
    edge: u32,
}
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct SampleKey {
    source: Source,
    size: [u32; 2],
}
struct Identity {
    frame: Weak<Frame>,
    revision: [u8; 32],
}
struct Sampled {
    size: [u32; 2],
    rgb: Vec<[f32; 3]>,
}

#[cfg(test)]
#[derive(Default)]
struct WorkCounters {
    hashes: AtomicU64,
    hash_bytes: AtomicU64,
    resizes: AtomicU64,
    pixel_reductions: AtomicU64,
}

struct Analyzer {
    reports: Arc<Mutex<Lfu<PairKey, Report>>>,
    resized: Lfu<SampleKey, Arc<Sampled>>,
    identities: Arc<Mutex<Lfu<usize, Identity>>>,
    lpips: Lpips,
    #[cfg(test)]
    resize_count: usize,
    #[cfg(test)]
    stage_count: Arc<AtomicU64>,
    #[cfg(test)]
    work: Arc<WorkCounters>,
}
impl Analyzer {
    fn new() -> Self {
        Self {
            reports: Arc::new(Mutex::new(Lfu::new(100))),
            resized: Lfu::new(20),
            identities: Arc::new(Mutex::new(Lfu::new(220))),
            lpips: Lpips::default(),
            #[cfg(test)]
            resize_count: 0,
            #[cfg(test)]
            stage_count: Arc::new(AtomicU64::new(0)),
            #[cfg(test)]
            work: Arc::new(WorkCounters::default()),
        }
    }
    fn source(&mut self, frame: &Arc<Frame>, cancelled: &impl Fn() -> bool) -> Result<Source> {
        #[cfg(test)]
        let _profile = profiling::Span::new("source_lookup");
        check(cancelled)?;
        let weak = Arc::downgrade(frame);
        let address = Arc::as_ptr(frame) as usize;
        if let Some(identity) = self.identities.lock().unwrap().get(&address) {
            // A Weak does not retain the huge RGB allocation; it also prevents
            // allocator address reuse from masquerading as the same decode.
            if identity.frame.ptr_eq(&weak) {
                return Ok(Source {
                    revision: identity.revision,
                });
            }
        }
        #[cfg(test)]
        let _hash_profile = profiling::Span::new("source_sha256");
        #[cfg(test)]
        {
            self.work.hashes.fetch_add(1, Ordering::Relaxed);
            self.work
                .hash_bytes
                .fetch_add(frame.bytes() as u64, Ordering::Relaxed);
        }
        let mut digest = Sha256::new();
        digest.update(frame.width.to_le_bytes());
        digest.update(frame.height.to_le_bytes());
        for chunk in bytemuck::cast_slice::<u16, u8>(&frame.rgb).chunks(1024 * 1024) {
            check(cancelled)?;
            digest.update(chunk);
        }
        let revision = digest.finalize().into();
        self.identities.lock().unwrap().insert(
            address,
            Identity {
                frame: weak,
                revision,
            },
        );
        Ok(Source { revision })
    }
    fn samples(
        &mut self,
        source: Source,
        frame: &Frame,
        size: [u32; 2],
        cancelled: &impl Fn() -> bool,
    ) -> Result<Arc<Sampled>> {
        check(cancelled)?;
        let key = SampleKey { source, size };
        if let Some(samples) = self.resized.get(&key) {
            return Ok(samples.clone());
        }
        #[cfg(test)]
        let _profile = profiling::Span::new(format!("resize_{}x{}", size[0], size[1]));
        let samples = Arc::new(Sampled {
            size,
            rgb: sample(frame, size, cancelled)?,
        });
        // Only actual reduced copies belong in the cache, never source-sized
        // frames (not even a converted full-resolution RGB32F copy).
        if frame.size() != size {
            #[cfg(test)]
            {
                self.resize_count += 1;
                self.work.resizes.fetch_add(1, Ordering::Relaxed);
            }
            self.resized.insert(key, samples.clone());
        }
        check(cancelled)?;
        Ok(samples)
    }
    fn analyze(
        &mut self,
        key: PairKey,
        a: &Frame,
        b: Option<&Frame>,
        cancelled: &impl Fn() -> bool,
        publish: &mut impl FnMut(&Report),
        include_neural: bool,
        active: Option<&Mutex<Option<Arc<RunOptions>>>>,
    ) -> Result<Report> {
        validate(a)?;
        if let Some(b) = b {
            validate(b)?;
        }
        check(cancelled)?;
        let mut report = self
            .reports
            .lock()
            .unwrap()
            .get(&key)
            .cloned()
            .unwrap_or_else(|| Report::empty(a.size(), b.map(Frame::size), key.edge));
        publish(&report); // cached partial or completed report is the first event
        if report.is_complete() {
            return Ok(report);
        }
        let start = Instant::now();
        let previous_ms = report.elapsed_ms;
        // Commit each finished stage before checking cancellation again. A
        // cancelled in-flight stage never replaces earlier cached progress.
        macro_rules! stage {
            ($stage:expr, $work:expr) => {
                if report.is_pending($stage) {
                    check(cancelled)?;
                    {
                        #[cfg(test)]
                        let _profile = profiling::Span::new(format!("{:?}", $stage));
                        $work;
                    }
                    #[cfg(test)]
                    self.stage_count.fetch_add(1, Ordering::Relaxed);
                    report.pending.retain(|stage| *stage != $stage);
                    report.neural_pending = report.is_pending(Stage::Neural);
                    report.elapsed_ms = previous_ms + start.elapsed().as_secs_f64() * 1000.;
                    self.reports.lock().unwrap().insert(key, report.clone());
                    if !cancelled() {
                        publish(&report);
                    }
                }
            };
        }
        stage!(Stage::Pixels, {
            #[cfg(test)]
            self.work.pixel_reductions.fetch_add(1, Ordering::Relaxed);
            report.pixels = Some(pixel_stats(a, b.unwrap(), cancelled)?);
        });
        for (sa, sb, method) in [
            (Stage::HaarA, Stage::HaarB, noise::Method::Haar),
            (
                Stage::ImmerkaerA,
                Stage::ImmerkaerB,
                noise::Method::Immerkaer,
            ),
            (Stage::HighPassA, Stage::HighPassB, noise::Method::HighPass),
        ] {
            stage!(sa, {
                let value = noise::estimate(a, method, cancelled)?;
                match method {
                    noise::Method::Haar => report.noise_a.haar_hh_mad = value,
                    noise::Method::Immerkaer => report.noise_a.immerkaer = value,
                    noise::Method::HighPass => report.noise_a.high_pass_mad = value,
                }
            });
            stage!(sb, {
                let value = noise::estimate(b.unwrap(), method, cancelled)?;
                match method {
                    noise::Method::Haar => report.noise_b.haar_hh_mad = value,
                    noise::Method::Immerkaer => report.noise_b.immerkaer = value,
                    noise::Method::HighPass => report.noise_b.high_pass_mad = value,
                }
            });
        }
        if [Stage::Ssim, Stage::Gmsd, Stage::MsSsim, Stage::Ssimulacra2]
            .iter()
            .any(|s| report.is_pending(*s))
        {
            let size = report.perceptual_size;
            let ar = self.samples(key.a, a, size, cancelled)?;
            let br = self.samples(key.b.unwrap(), b.unwrap(), size, cancelled)?;
            let (w, h) = (size[0] as usize, size[1] as usize);
            let (ay, by) = (luma(&ar.rgb), luma(&br.rgb));
            stage!(Stage::Ssim, {
                report.ssim_y = ssim(&ay, &by, w, h, cancelled)?.map(|s| s.0);
            });
            stage!(Stage::Gmsd, {
                let (ga, gw, gh) = gmsd_pool(&ay, w, h);
                let (gb, _, _) = gmsd_pool(&by, w, h);
                report.gmsd_y = gmsd_impl(&ga, &gb, gw, gh);
            });
            stage!(Stage::MsSsim, {
                report.ms_ssim_y = multi_ssim(ay, by, w, h, cancelled)?;
            });
            stage!(Stage::Ssimulacra2, {
                let convert = |rgb| {
                    ssimulacra2::Rgb::new(
                        rgb,
                        w,
                        h,
                        ssimulacra2::TransferCharacteristic::SRGB,
                        ssimulacra2::ColorPrimaries::BT709,
                    )
                    .unwrap()
                };
                report.ssimulacra2 = ssimulacra2::compute_frame_ssimulacra2(
                    convert(ar.rgb.clone()),
                    convert(br.rgb.clone()),
                )
                .map_err(|e| e.to_string());
            });
        }
        if include_neural {
            stage!(Stage::Neural, {
                let result = (|| -> Result<Neural> {
                    let ar = self.samples(key.a, a, report.neural_size, cancelled)?;
                    let br =
                        self.samples(key.b.unwrap(), b.unwrap(), report.neural_size, cancelled)?;
                    let options = Arc::new(run_options()?);
                    if let Some(active) = active {
                        *active.lock().unwrap() = Some(options.clone());
                    }
                    check(cancelled)?; // closes the register/terminate race
                    self.lpips.measure_samples(&ar, &br, &options, cancelled)
                })();
                if let Some(active) = active {
                    *active.lock().unwrap() = None;
                }
                check(cancelled)?; // do not cache ONNX termination as a final error
                report.neural = result.map_err(|e| format!("{e:#}"));
            });
        }
        Ok(report)
    }
}

/// Native metrics, leaving LPIPS pending (kept for existing callers/tests).
#[cfg(test)]
pub fn compute(a: &Frame, b: &Frame, edge: u32, cancelled: &impl Fn() -> bool) -> Result<Report> {
    compute_once(a, b, edge, cancelled, false)
}
pub fn compute_complete(
    a: &Frame,
    b: &Frame,
    edge: u32,
    cancelled: &impl Fn() -> bool,
) -> Result<Report> {
    compute_once(a, b, edge, cancelled, true)
}
fn compute_once(
    a: &Frame,
    b: &Frame,
    edge: u32,
    cancelled: &impl Fn() -> bool,
    neural: bool,
) -> Result<Report> {
    // This cache cannot outlive these immutable borrows; no revision scan needed.
    let key = PairKey {
        a: Source { revision: [0; 32] },
        b: Some(Source { revision: [1; 32] }),
        edge: edge.clamp(32, 1536),
    };
    Analyzer::new().analyze(key, a, Some(b), cancelled, &mut |_| {}, neural, None)
}

#[derive(Default)]
pub struct Lpips {
    session: Option<Session>,
}
fn lpips_session(use_arena: bool) -> Result<Session> {
    let path = model::installed()?;
    Ok(Session::builder()?
        .with_execution_providers([ort::ep::CPU::default()
            .with_arena_allocator(use_arena)
            .build()])
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .with_intra_threads(2)
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .with_inter_threads(1)
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .commit_from_file(path)
        .context("LPIPS ONNX model initialization")?)
}
impl Lpips {
    #[cfg(test)]
    pub fn measure(
        &mut self,
        a: &Frame,
        b: &Frame,
        options: &RunOptions,
        cancelled: &impl Fn() -> bool,
    ) -> Result<Neural> {
        anyhow::ensure!(a.size() == b.size(), "LPIPS requires equal dimensions");
        let size = analysis_size(a.size(), 768);
        self.measure_samples(
            &Sampled {
                size,
                rgb: sample(a, size, cancelled)?,
            },
            &Sampled {
                size,
                rgb: sample(b, size, cancelled)?,
            },
            options,
            cancelled,
        )
    }
    fn measure_samples(
        &mut self,
        a: &Sampled,
        b: &Sampled,
        options: &RunOptions,
        cancelled: &impl Fn() -> bool,
    ) -> Result<Neural> {
        anyhow::ensure!(a.size == b.size, "LPIPS requires equal dimensions");
        let size = a.size;
        anyhow::ensure!(
            size[0].min(size[1]) >= 32,
            "LPIPS requires at least 32 px on each axis"
        );
        check(cancelled)?;
        if self.session.is_none() {
            // Keep the model/session, not the CPU arena's high-water activation
            // allocations. Report hits never run ONNX, so retaining its large
            // inference arena offers no benefit for the common revisit path.
            self.session = Some(lpips_session(false)?);
        }
        let input = |frame: &Sampled| -> Result<_> {
            let samples = &frame.rgb;
            let n = samples.len();
            let mut chw = vec![0f32; n * 3];
            for (i, p) in samples.iter().enumerate() {
                for c in 0..3 {
                    chw[c * n + i] = p[c];
                }
            }
            Ok(Tensor::from_array((
                [1usize, 3, size[1] as usize, size[0] as usize],
                chw,
            ))?)
        };
        let (a, b) = (input(a)?, input(b)?);
        check(cancelled)?;
        let outputs = self
            .session
            .as_mut()
            .unwrap()
            .run_with_options(ort::inputs!["a"=>a,"b"=>b], options)?;
        let (_, values) = outputs["lpips"].try_extract_tensor::<f32>()?;
        let value = *values.first().context("Empty LPIPS output")? as f64;
        anyhow::ensure!(
            values.len() == 1 && value.is_finite(),
            "Invalid LPIPS output"
        );
        check(cancelled)?;
        Ok(Neural { lpips: value })
    }
}

pub struct Update {
    pub token: u64,
    pub result: std::result::Result<Report, String>,
}
enum Job {
    Compute {
        token: u64,
        a: Arc<Frame>,
        b: Option<Arc<Frame>>,
        edge: u32,
    },
    Cancel,
    Shutdown,
}
pub struct Worker {
    pub events: Receiver<Update>,
    sender: Sender<Update>,
    reports: Arc<Mutex<Lfu<PairKey, Report>>>,
    identities: Arc<Mutex<Lfu<usize, Identity>>>,
    token: Arc<AtomicU64>,
    mailbox: Arc<Mailbox<Job>>,
    active: Arc<Mutex<Option<Arc<RunOptions>>>>,
    #[cfg(test)]
    stage_count: Arc<AtomicU64>,
    #[cfg(test)]
    work: Arc<WorkCounters>,
}
impl Worker {
    pub fn new() -> Self {
        let (tx, events) = bounded(16);
        let discard = events.clone();
        let token = Arc::new(AtomicU64::new(0));
        let mailbox = Arc::new(Mailbox::new());
        let (current, jobs) = (token.clone(), mailbox.clone());
        let active = Arc::new(Mutex::new(None));
        let running = active.clone();
        let mut analyzer = Analyzer::new();
        let reports = analyzer.reports.clone();
        let identities = analyzer.identities.clone();
        #[cfg(test)]
        let stage_count = analyzer.stage_count.clone();
        #[cfg(test)]
        let work = analyzer.work.clone();
        let sender = tx.clone();
        thread::Builder::new()
            .name("quality-metrics".into())
            .spawn(move || {
                let pool = rayon::ThreadPoolBuilder::new()
                    .num_threads(2)
                    .build()
                    .expect("metric pool");
                loop {
                    let (token, a, b, edge) = match jobs.take() {
                        Job::Compute { token, a, b, edge } => (token, a, b, edge),
                        Job::Cancel => continue,
                        Job::Shutdown => break,
                    };
                    let cancelled = || current.load(Ordering::Acquire) != token;
                    if cancelled() {
                        continue;
                    }
                    let result = pool.install(|| -> Result<Report> {
                        validate(&a)?;
                        if let Some(b) = &b {
                            validate(b)?;
                        }
                        let key = PairKey {
                            a: analyzer.source(&a, &cancelled)?,
                            b: b.as_ref()
                                .map(|frame| analyzer.source(frame, &cancelled))
                                .transpose()?,
                            edge: edge.clamp(32, 1536),
                        };
                        analyzer.analyze(
                            key,
                            &a,
                            b.as_deref(),
                            &cancelled,
                            &mut |report| {
                                if !cancelled() {
                                    publish(
                                        &tx,
                                        &discard,
                                        Update {
                                            token,
                                            result: Ok(report.clone()),
                                        },
                                    );
                                }
                            },
                            true,
                            Some(&running),
                        )
                    });
                    if let Err(error) = result {
                        if !cancelled() {
                            publish(
                                &tx,
                                &discard,
                                Update {
                                    token,
                                    result: Err(format!("{error:#}")),
                                },
                            );
                        }
                    }
                }
            })
            .expect("metrics thread");
        Self {
            events,
            sender,
            reports,
            identities,
            token,
            mailbox,
            active,
            #[cfg(test)]
            stage_count,
            #[cfg(test)]
            work,
        }
    }
    pub fn request(
        &self,
        _aid: PhotoId,
        a: Arc<Frame>,
        _bid: PhotoId,
        b: Arc<Frame>,
        edge: u32,
    ) -> u64 {
        self.enqueue(a, Some(b), edge)
    }
    pub fn request_single(&self, _aid: PhotoId, a: Arc<Frame>, edge: u32) -> u64 {
        self.enqueue(a, None, edge)
    }
    fn enqueue(&self, a: Arc<Frame>, b: Option<Arc<Frame>>, edge: u32) -> u64 {
        let token = self.token.fetch_add(1, Ordering::AcqRel) + 1;
        self.terminate();
        // The cache locks cover only tiny metadata operations, never analysis.
        // A ready report is delivered even while the worker is finishing an
        // uninterruptible library call for the previous request.
        if let Some(report) = self.cached(&a, b.as_ref(), edge) {
            let complete = report.is_complete();
            publish(
                &self.sender,
                &self.events,
                Update {
                    token,
                    result: Ok(report),
                },
            );
            if complete {
                self.mailbox.put(Job::Cancel);
                return token;
            }
        }
        self.mailbox.put(Job::Compute { token, a, b, edge });
        token
    }
    fn cached(&self, a: &Arc<Frame>, b: Option<&Arc<Frame>>, edge: u32) -> Option<Report> {
        let mut identities = self.identities.lock().unwrap();
        let mut source = |frame: &Arc<Frame>| {
            let identity = identities.get(&(Arc::as_ptr(frame) as usize))?;
            identity
                .frame
                .ptr_eq(&Arc::downgrade(frame))
                .then_some(Source {
                    revision: identity.revision,
                })
        };
        let key = PairKey {
            a: source(a)?,
            b: match b {
                Some(frame) => Some(source(frame)?),
                None => None,
            },
            edge: edge.clamp(32, 1536),
        };
        drop(identities);
        let mut reports = self.reports.lock().unwrap();
        let report = reports.peek(&key)?.clone();
        if report.is_complete() {
            reports.get(&key);
        }
        Some(report)
    }
    pub fn cancel(&self) -> u64 {
        let token = self.token.fetch_add(1, Ordering::AcqRel) + 1;
        self.terminate();
        self.mailbox.put(Job::Cancel);
        token
    }
    fn terminate(&self) {
        if let Some(options) = self.active.lock().unwrap().as_ref() {
            let _ = options.terminate();
        }
    }
}
/// Cumulative snapshots may replace the oldest event when the UI is paused.
/// Never block the worker on UI consumption: cancellation/revisit must progress.
fn publish(tx: &Sender<Update>, discard: &Receiver<Update>, mut update: Update) {
    loop {
        match tx.try_send(update) {
            Ok(()) | Err(TrySendError::Disconnected(_)) => return,
            Err(TrySendError::Full(value)) => {
                update = value;
                let _ = discard.try_recv();
            }
        }
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.token.fetch_add(1, Ordering::AcqRel);
        self.terminate();
        self.mailbox.put(Job::Shutdown);
    }
}

#[cfg(test)]
mod profiling {
    use std::time::Instant;

    // Opt-in developer markers, consumed by scripts/profile_statistics.py.
    // Disabled in ordinary tests and absent from production/UI request paths.
    pub(super) struct Span(Option<(String, Instant)>);
    impl Span {
        pub(super) fn new(label: impl Into<String>) -> Self {
            if std::env::var_os("SCRUTIVUE_METRICS_PROFILE").is_none() {
                return Self(None);
            }
            let label = label.into();
            eprintln!("STATISTICS_PROFILE\tbegin\t{label}");
            Self(Some((label, Instant::now())))
        }
    }
    impl Drop for Span {
        fn drop(&mut self) {
            if let Some((label, start)) = &self.0 {
                eprintln!(
                    "STATISTICS_PROFILE\tend\t{label}\t{:.6}",
                    start.elapsed().as_secs_f64() * 1000.
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn streaming_triangle_matches_imageops_rgb16_exactly() {
        use image::{imageops::FilterType, ImageBuffer, Rgb};
        for (source, destination) in [
            ([1, 17], [1, 7]),
            ([17, 1], [7, 1]),
            ([13, 19], [7, 11]),
            ([19, 13], [11, 7]),
            ([65, 97], [32, 47]),
            ([701, 1003], [341, 512]),
            ([1003, 701], [512, 341]),
            ([8256, 65], [1536, 12]),
            ([65, 8256], [12, 1536]),
            ([17, 23], [17, 23]),
            ([37, 51], [1, 1]),
        ] {
            for pattern in 0..4 {
                let [w, h] = source;
                let mut seed = 82731u32;
                let rgb = (0..w * h)
                    .flat_map(|i| {
                        let (x, y) = (i % w, i / w);
                        std::array::from_fn::<_, 3, _>(|c| {
                            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                            match pattern {
                                0 => (seed >> 16) as u16,
                                1 => {
                                    if (x ^ y ^ c as u32) & 1 == 0 {
                                        0
                                    } else {
                                        65535
                                    }
                                }
                                2 => {
                                    if x == 0 || y == h - 1 || x == y {
                                        65535
                                    } else {
                                        1
                                    }
                                }
                                _ => (x * y * 173 + x * 91 + y * 97 + c as u32 * 655) as u16,
                            }
                        })
                    })
                    .collect();
                let frame = Frame {
                    width: w,
                    height: h,
                    rgb,
                };
                let image =
                    ImageBuffer::<Rgb<u16>, _>::from_raw(w, h, frame.rgb.as_slice()).unwrap();
                let reference = image::imageops::resize(
                    &image,
                    destination[0],
                    destination[1],
                    FilterType::Triangle,
                );
                let expected: Vec<_> = reference
                    .as_raw()
                    .chunks_exact(3)
                    .map(|p| {
                        [
                            p[0] as f32 / 65535.,
                            p[1] as f32 / 65535.,
                            p[2] as f32 / 65535.,
                        ]
                    })
                    .collect();
                assert_eq!(
                    sample(&frame, destination, &|| false).unwrap(),
                    expected,
                    "{source:?} -> {destination:?}, pattern {pattern}"
                );
                assert!(sample(&frame, destination, &|| true).is_err());
            }
        }
    }
    fn key(edge: u32) -> PairKey {
        PairKey {
            a: Source { revision: [1; 32] },
            b: Some(Source { revision: [2; 32] }),
            edge,
        }
    }
    #[test]
    fn partial_progress_survives_cancel_and_resumes_at_next_metric() {
        use std::sync::atomic::AtomicBool;
        let mut analyzer = Analyzer::new();
        let a = Arc::new(frame(10000));
        let b = Arc::new(frame(12000));
        let pair = PairKey {
            a: analyzer.source(&a, &|| false).unwrap(),
            b: Some(analyzer.source(&b, &|| false).unwrap()),
            edge: 512,
        };
        let cancelled = AtomicBool::new(false);
        let mut snapshots = Vec::new();
        assert!(analyzer
            .analyze(
                pair,
                &a,
                Some(&b),
                &|| cancelled.load(Ordering::Acquire),
                &mut |report| {
                    snapshots.push(report.clone());
                    if !report.is_pending(Stage::HaarA) {
                        cancelled.store(true, Ordering::Release);
                    }
                },
                false,
                None
            )
            .is_err());
        let partial = snapshots.last().unwrap();
        assert!(partial.pixels.is_some());
        assert!(!partial.is_pending(Stage::HaarA));
        assert!(partial.is_pending(Stage::HaarB));
        let remaining = partial.pending.len();
        let completed_stages = analyzer.stage_count.load(Ordering::Relaxed);
        // Clearing/re-adding the same pixels produces new allocations but must
        // resume the previous stages, just like revisiting live frames.
        drop((a, b));
        let a = Arc::new(frame(10000));
        let b = Arc::new(frame(12000));
        let redecoded_pair = PairKey {
            a: analyzer.source(&a, &|| false).unwrap(),
            b: Some(analyzer.source(&b, &|| false).unwrap()),
            edge: 512,
        };
        assert_eq!(redecoded_pair, pair);
        snapshots.clear();
        let report = analyzer
            .analyze(
                redecoded_pair,
                &a,
                Some(&b),
                &|| false,
                &mut |r| snapshots.push(r.clone()),
                false,
                None,
            )
            .unwrap();
        assert_eq!(snapshots[0].pending.len(), remaining);
        assert_eq!(snapshots[1].pending.len(), remaining - 1);
        assert!(!snapshots[1].is_pending(Stage::HaarB));
        assert_eq!(report.pending, vec![Stage::Neural]);
        assert_eq!(snapshots.len(), remaining); // cached snapshot + all unfinished native stages
        assert_eq!(
            analyzer.stage_count.load(Ordering::Relaxed) - completed_stages,
            (remaining - 1) as u64
        );
    }
    #[test]
    fn completed_cache_is_one_snapshot_and_pair_order_and_edge_are_distinct() {
        let mut analyzer = Analyzer::new();
        let a = frame(10000);
        let mut done = Report::empty(a.size(), Some(a.size()), 512);
        done.pending.clear();
        done.neural_pending = false;
        done.elapsed_ms = 123.;
        analyzer.reports.lock().unwrap().insert(key(512), done);
        let mut count = 0;
        let report = analyzer
            .analyze(
                key(512),
                &a,
                Some(&a),
                &|| false,
                &mut |_| count += 1,
                true,
                None,
            )
            .unwrap();
        assert_eq!(count, 1);
        assert_eq!(report.elapsed_ms, 123.);
        assert!(report.is_complete());
        let reversed = PairKey {
            a: key(512).b.unwrap(),
            b: Some(key(512).a),
            edge: 512,
        };
        assert!(analyzer.reports.lock().unwrap().get(&reversed).is_none());
        assert!(analyzer.reports.lock().unwrap().get(&key(1536)).is_none());
        for revision in 0..105 {
            let pair = PairKey {
                a: Source {
                    revision: [revision; 32],
                },
                ..key(512)
            };
            analyzer
                .reports
                .lock()
                .unwrap()
                .insert(pair, Report::empty(a.size(), Some(a.size()), 512));
        }
        assert_eq!(analyzer.reports.lock().unwrap().len(), 100);
    }
    #[test]
    fn content_identity_survives_new_arcs_and_does_not_retain_source_frames() {
        let mut analyzer = Analyzer::new();
        let first = Arc::new(frame(1000));
        let same = Arc::new(frame(1000));
        let changed = Arc::new(frame(1001));
        let old = analyzer.source(&first, &|| false).unwrap();
        assert_eq!(analyzer.source(&same, &|| false).unwrap(), old);
        assert_ne!(analyzer.source(&changed, &|| false).unwrap(), old);
        let mut reshaped = frame(1000);
        reshaped.width = 96;
        reshaped.height = 384;
        assert_ne!(
            analyzer.source(&Arc::new(reshaped), &|| false).unwrap(),
            old
        );
        let weak = Arc::downgrade(&changed);
        drop(changed);
        assert!(weak.upgrade().is_none());
        assert!(analyzer.source(&same, &|| true).is_err());
    }
    #[test]
    fn only_actual_resizes_are_cached_with_capacity_twenty() {
        let mut analyzer = Analyzer::new();
        let frame = frame(1000);
        analyzer
            .samples(key(512).a, &frame, frame.size(), &|| false)
            .unwrap();
        assert_eq!(analyzer.resized.len(), 0);
        for revision in 0..25 {
            let source = Source {
                revision: [revision; 32],
            };
            let a = analyzer
                .samples(source, &frame, [96, 96], &|| false)
                .unwrap();
            let b = analyzer
                .samples(source, &frame, [96, 96], &|| false)
                .unwrap();
            assert!(Arc::ptr_eq(&a, &b));
        }
        assert_eq!(analyzer.resized.len(), 20);
        assert_eq!(analyzer.resize_count, 25);
    }
    #[test]
    fn redecoded_content_reuses_reduced_samples_but_changed_pixels_do_not() {
        let mut analyzer = Analyzer::new();
        let first = Arc::new(frame(1000));
        let redecoded = Arc::new(frame(1000));
        let changed = Arc::new(frame(1001));
        let source = analyzer.source(&first, &|| false).unwrap();
        let samples = analyzer
            .samples(source, &first, [96, 96], &|| false)
            .unwrap();
        let source = analyzer.source(&redecoded, &|| false).unwrap();
        let reused = analyzer
            .samples(source, &redecoded, [96, 96], &|| false)
            .unwrap();
        assert!(Arc::ptr_eq(&samples, &reused));
        let source = analyzer.source(&changed, &|| false).unwrap();
        let different = analyzer
            .samples(source, &changed, [96, 96], &|| false)
            .unwrap();
        assert!(!Arc::ptr_eq(&samples, &different));
        assert_eq!(analyzer.resize_count, 2);
        assert_eq!(analyzer.resized.len(), 2);
    }
    fn worker_snapshots(worker: &Worker, token: u64) -> Vec<Report> {
        let deadline = Instant::now() + std::time::Duration::from_secs(30);
        let mut snapshots = Vec::new();
        loop {
            let update = worker
                .events
                .recv_deadline(deadline)
                .expect("metrics worker stalled");
            if update.token != token {
                continue;
            }
            let report = update.result.expect("metrics worker failed");
            let complete = report.is_complete();
            snapshots.push(report);
            if complete {
                return snapshots;
            }
        }
    }
    #[test]
    fn worker_reselection_and_clear_readd_reuse_content_without_any_stages() {
        let worker = Worker::new();
        let a = Arc::new(frame(1000));
        let b = Arc::new(frame(1100));
        let token = worker.request(1, a.clone(), 2, b.clone(), 32);
        let initial = worker_snapshots(&worker, token);
        assert!(initial.len() > 1);
        let completed = initial.last().unwrap();
        let stages = worker.stage_count.load(Ordering::Relaxed);
        let hashes = worker.work.hashes.load(Ordering::Relaxed);
        let hash_bytes = worker.work.hash_bytes.load(Ordering::Relaxed);
        let resizes = worker.work.resizes.load(Ordering::Relaxed);
        let reductions = worker.work.pixel_reductions.load(Ordering::Relaxed);
        assert_eq!(stages as usize, initial.len() - 1);
        assert_eq!(worker.reports.lock().unwrap().len(), 1);

        worker.cancel(); // deselect/reselect, including a new library ID
        let token = worker.request(101, a.clone(), 102, b.clone(), 32);
        let update = worker
            .events
            .try_recv()
            .expect("live frames must hit synchronously");
        assert_eq!(update.token, token);
        assert!(update.result.unwrap().is_complete());
        assert_eq!(worker.stage_count.load(Ordering::Relaxed), stages);
        assert_eq!(worker.work.hashes.load(Ordering::Relaxed), hashes);
        assert_eq!(worker.work.hash_bytes.load(Ordering::Relaxed), hash_bytes);
        assert_eq!(worker.work.resizes.load(Ordering::Relaxed), resizes);
        assert_eq!(
            worker.work.pixel_reductions.load(Ordering::Relaxed),
            reductions
        );

        worker.cancel(); // clear/re-add after decode eviction: new IDs and Arcs
        drop((a, b));
        let a = Arc::new(frame(1000));
        let b = Arc::new(frame(1100));
        let token = worker.request(201, a.clone(), 202, b.clone(), 32);
        let reused = worker_snapshots(&worker, token);
        assert_eq!(
            reused.len(),
            1,
            "completed revision must not publish any stages"
        );
        assert_eq!(reused[0].elapsed_ms, completed.elapsed_ms);
        assert_eq!(worker.stage_count.load(Ordering::Relaxed), stages);
        assert_eq!(worker.work.hashes.load(Ordering::Relaxed), hashes + 2);
        assert_eq!(worker.work.resizes.load(Ordering::Relaxed), resizes);
        assert_eq!(
            worker.work.pixel_reductions.load(Ordering::Relaxed),
            reductions
        );
        assert_eq!(worker.reports.lock().unwrap().len(), 1);
        assert!(worker.events.try_recv().is_err());

        // Once the worker verifies a new allocation, it too has the fast path.
        let token = worker.request(301, a, 302, b, 32);
        let update = worker
            .events
            .try_recv()
            .expect("verified revisions must hit synchronously");
        assert_eq!(update.token, token);
        assert!(update.result.unwrap().is_complete());
        assert_eq!(worker.stage_count.load(Ordering::Relaxed), stages);
        assert_eq!(worker.work.hashes.load(Ordering::Relaxed), hashes + 2);
        assert_eq!(worker.work.resizes.load(Ordering::Relaxed), resizes);
        assert_eq!(
            worker.work.pixel_reductions.load(Ordering::Relaxed),
            reductions
        );

        // Reusing IDs is never evidence that the decoded pixels are unchanged.
        let token = worker.request(301, Arc::new(frame(1001)), 302, Arc::new(frame(1100)), 32);
        assert!(worker_snapshots(&worker, token).len() > 1);
        assert!(worker.stage_count.load(Ordering::Relaxed) > stages);
        assert_eq!(worker.reports.lock().unwrap().len(), 2);
    }
    #[test]
    fn structural_and_lpips_reuse_the_same_actual_resizes() {
        let mut analyzer = Analyzer::new();
        let a = Frame {
            width: 1024,
            height: 64,
            rgb: vec![10000; 1024 * 64 * 3],
        };
        let report = analyzer
            .analyze(key(768), &a, Some(&a), &|| false, &mut |_| {}, true, None)
            .unwrap();
        assert!(report.is_complete());
        assert!(report.neural.is_ok(), "{:?}", report.neural);
        assert_eq!(analyzer.resize_count, 2);
        let mut snapshots = 0;
        analyzer
            .analyze(
                key(768),
                &a,
                Some(&a),
                &|| false,
                &mut |_| snapshots += 1,
                true,
                None,
            )
            .unwrap();
        assert_eq!(snapshots, 1);
        assert_eq!(analyzer.resize_count, 2);
    }
    #[test]
    fn full_event_queue_does_not_block_cancellation() {
        let worker = Worker::new();
        for i in 0..30 {
            worker.request(i, Arc::new(frame(1000)), i + 1, Arc::new(frame(1100)), 32);
        }
        let token = worker.cancel();
        let a = Arc::new(frame(500));
        let latest = worker.request(500, a.clone(), 500, a, 32);
        assert!(latest > token);
        let deadline = Instant::now() + std::time::Duration::from_secs(30);
        loop {
            let update = worker
                .events
                .recv_deadline(deadline)
                .expect("worker stalled after cancellation");
            if update.token == latest && update.result.as_ref().is_ok_and(Report::is_complete) {
                break;
            }
        }
    }
    #[test]
    fn single_image_worker_publishes_noise_without_b_or_fake_pair_scores() {
        let worker = Worker::new();
        let a = Arc::new(frame(12000));
        let token = worker.request_single(7, a.clone(), 512);
        let deadline = Instant::now() + std::time::Duration::from_secs(10);
        let mut snapshots = Vec::new();
        loop {
            let update = worker.events.recv_deadline(deadline).unwrap();
            if update.token != token {
                continue;
            }
            let report = update.result.unwrap();
            let complete = report.is_complete();
            snapshots.push(report);
            if complete {
                break;
            }
        }
        assert_eq!(snapshots.len(), 4); // empty + each independent estimator
        let report = snapshots.last().unwrap();
        assert!(report.original_size_b.is_none());
        assert!(report.comparison_error.is_some());
        assert!(report.pixels.is_none());
        assert!(report.ssim_y.is_none());
        assert_eq!(report.noise_a.haar_hh_mad, Some(0.));
        assert!(report.noise_a.immerkaer.is_some());
        assert_eq!(report.noise_a.high_pass_mad, Some(0.));
        assert!(report.noise_b.haar_hh_mad.is_none());
        assert!(!report.neural_pending);
        assert_eq!(worker.work.hashes.load(Ordering::Relaxed), 1);
        assert_eq!(
            worker.work.hash_bytes.load(Ordering::Relaxed),
            a.bytes() as u64
        );
        assert_eq!(worker.work.resizes.load(Ordering::Relaxed), 0);
        assert_eq!(worker.work.pixel_reductions.load(Ordering::Relaxed), 0);
        let cached_token = worker.request_single(7, a, 512);
        let cached = worker
            .events
            .try_recv()
            .expect("ready cache must publish synchronously");
        assert_eq!(cached.token, cached_token);
        assert!(cached.result.unwrap().is_complete());
    }
    #[test]
    #[ignore = "Read-only requested-DNG statistics profile; set SCRUTIVUE_PROFILE_PAIR_A/B"]
    fn profile_dng_statistics() {
        let paths: Vec<_> = ["SCRUTIVUE_PROFILE_PAIR_A", "SCRUTIVUE_PROFILE_PAIR_B"]
            .map(|name| std::path::PathBuf::from(std::env::var_os(name).expect(name)))
            .into();
        let edge: u32 = std::env::var("SCRUTIVUE_PROFILE_METRIC_EDGE")
            .unwrap_or_else(|_| "512".into())
            .parse()
            .unwrap();
        let single = std::env::var_os("SCRUTIVUE_PROFILE_SINGLE").is_some();
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(6)
            .build()
            .unwrap();
        let frames = {
            let _profile = profiling::Span::new("decode_pair");
            std::thread::scope(|scope| {
                let jobs: Vec<_> = paths
                    .iter()
                    .take(if single { 1 } else { 2 })
                    .map(|path| {
                        let pool = &pool;
                        scope.spawn(move || pool.install(|| crate::decode::decode(path).unwrap()))
                    })
                    .collect();
                jobs.into_iter()
                    .map(|job| job.join().unwrap())
                    .collect::<Vec<_>>()
            })
        };
        let a = &frames[0];
        let b = frames.get(1);
        let decoded_hashes: Vec<_> = frames
            .iter()
            .map(|frame| {
                format!(
                    "{:x}",
                    Sha256::digest(bytemuck::cast_slice::<u16, u8>(&frame.rgb))
                )
            })
            .collect();
        eprintln!(
            "STATISTICS_RESULT\tframes\t{}",
            serde_json::json!({
                "sizes": frames.iter().map(|frame| frame.size()).collect::<Vec<_>>(),
                "rgb16_bytes": frames.iter().map(|frame| frame.bytes()).collect::<Vec<_>>(),
                "rgb16_sha256": decoded_hashes,
            })
        );
        let worker = Worker::new();
        let request = |id| {
            if let Some(b) = b {
                worker.request(id, a.clone(), id + 1, b.clone(), edge)
            } else {
                worker.request_single(id, a.clone(), edge)
            }
        };
        let report = {
            let _profile = profiling::Span::new("statistics_request_cold");
            worker_snapshots(&worker, request(1)).pop().unwrap()
        };
        assert!(report.is_complete());
        if !single {
            assert!(report.neural.is_ok(), "{:?}", report.neural);
        }
        eprintln!(
            "STATISTICS_RESULT\treport\t{}",
            serde_json::to_string(&report).unwrap()
        );
        let stage_count = worker.stage_count.load(Ordering::Relaxed);
        let hash_count = worker.work.hashes.load(Ordering::Relaxed);
        let resize_count = worker.work.resizes.load(Ordering::Relaxed);
        {
            let _profile = profiling::Span::new("worker_complete_warm_100");
            for id in 100..200 {
                let token = request(id);
                let update = worker
                    .events
                    .try_recv()
                    .expect("known completed frames must be synchronous");
                assert_eq!(update.token, token);
                let result = update.result.unwrap();
                assert_eq!(result.elapsed_ms, report.elapsed_ms);
            }
        }
        assert_eq!(worker.stage_count.load(Ordering::Relaxed), stage_count);
        assert_eq!(worker.work.hashes.load(Ordering::Relaxed), hash_count);
        assert_eq!(worker.work.resizes.load(Ordering::Relaxed), resize_count);
        for (frame, hash) in frames.iter().zip(&decoded_hashes) {
            assert_eq!(
                &format!(
                    "{:x}",
                    Sha256::digest(bytemuck::cast_slice::<u16, u8>(&frame.rgb))
                ),
                hash
            );
        }
        eprintln!(
            "STATISTICS_RESULT\twork\t{}",
            serde_json::json!({
                "hashes": hash_count, "hash_bytes": worker.work.hash_bytes.load(Ordering::Relaxed),
                "resizes": resize_count, "stages": stage_count,
                "pixel_reductions": worker.work.pixel_reductions.load(Ordering::Relaxed)
            })
        );
        if !single && std::env::var_os("SCRUTIVUE_PROFILE_COMPARE_ARENA").is_some() {
            let size = report.neural_size;
            let samples = {
                let _profile = profiling::Span::new("lpips_allocator_prepare_samples");
                [a, b.unwrap()].map(|frame| Sampled {
                    size,
                    rgb: sample(frame, size, &|| false).unwrap(),
                })
            };
            for (iteration, use_arena) in [true, false, false, true].into_iter().enumerate() {
                let label = if use_arena { "arena_on" } else { "arena_off" };
                let mut model = {
                    let _profile = profiling::Span::new(format!("lpips_{iteration}_{label}_init"));
                    Lpips {
                        session: Some(lpips_session(use_arena).unwrap()),
                    }
                };
                for run in 0..3 {
                    let _profile =
                        profiling::Span::new(format!("lpips_{iteration}_{label}_run_{run}"));
                    let result = model
                        .measure_samples(&samples[0], &samples[1], &run_options().unwrap(), &|| {
                            false
                        })
                        .unwrap();
                    assert_eq!(result.lpips, report.neural.as_ref().unwrap().lpips);
                }
            }
        }
    }
    #[test]
    fn unequal_sizes_keep_both_independent_noise_estimates() {
        let a = frame(20000);
        let mut b = Frame {
            width: 96,
            height: 192,
            rgb: vec![0; 96 * 192 * 3],
        };
        let mut seed = 12345u32;
        for p in b.rgb.chunks_exact_mut(3) {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            p.fill(20000 + (seed >> 20) as u16);
        }
        let report = compute_complete(&a, &b, 512, &|| false).unwrap();
        assert!(report.is_complete());
        assert!(report
            .comparison_error
            .as_ref()
            .unwrap()
            .contains("Different resolutions"));
        assert_eq!(report.original_size_b, Some([96, 192]));
        assert!(report.pixels.is_none());
        assert!(report.ssim_y.is_none());
        assert!(report.neural.is_err());
        assert_eq!(report.noise_a.haar_hh_mad, Some(0.));
        assert!(report.noise_a.immerkaer.unwrap() < 1e-12);
        assert_eq!(report.noise_a.high_pass_mad, Some(0.));
        assert!(report.noise_b.haar_hh_mad.unwrap() > 0.);
        assert!(report.noise_b.immerkaer.unwrap() > 0.);
        assert!(report.noise_b.high_pass_mad.unwrap() > 0.);
    }
    #[test]
    fn onnx_lpips_identity_dynamic_shapes_and_cancellation() {
        let mut model = Lpips::default();
        for [w, h] in [[32, 32], [65, 37], [192, 192]] {
            let a = Frame {
                width: w,
                height: h,
                rgb: (0..w * h * 3)
                    .map(|i| i.wrapping_mul(17431) as u16)
                    .collect(),
            };
            let options = run_options().unwrap();
            assert!(
                model
                    .measure(&a, &a, &options, &|| false)
                    .unwrap()
                    .lpips
                    .abs()
                    < 1e-8
            );
            assert!(model.measure(&a, &a, &options, &|| true).is_err());
        }
        let a = frame(1000);
        let b = frame(30000);
        let options = run_options().unwrap();
        options.terminate().unwrap();
        assert!(model.measure(&a, &b, &options, &|| false).is_err());
        assert!(
            model
                .measure(&a, &b, &run_options().unwrap(), &|| false)
                .unwrap()
                .lpips
                > 0.
        );
    }
    #[test]
    fn onnx_allocator_modes_preserve_lpips_values_across_repeated_runs() {
        let a = frame(1000);
        let b = frame(30000);
        let options = run_options().unwrap();
        let mut expected = None;
        for use_arena in [true, false] {
            let mut model = Lpips {
                session: Some(lpips_session(use_arena).unwrap()),
            };
            for _ in 0..3 {
                let value = model.measure(&a, &b, &options, &|| false).unwrap().lpips;
                if let Some(expected) = expected {
                    assert_eq!(value, expected);
                } else {
                    expected = Some(value);
                }
            }
        }
    }
    fn frame(value: u16) -> Frame {
        Frame {
            width: 192,
            height: 192,
            rgb: vec![value; 192 * 192 * 3],
        }
    }
    #[test]
    fn identical_images_have_identity_scores() {
        let a = frame(20000);
        let r = compute(&a, &a, 1536, &|| false).unwrap();
        assert_eq!(r.pixels.as_ref().unwrap().psnr_db, None);
        assert_eq!(r.pixels.as_ref().unwrap().mae, 0.);
        assert_eq!(r.pixels.as_ref().unwrap().changed_percent, 0.);
        assert!((r.ssim_y.unwrap() - 1.).abs() < 1e-10);
        assert!((r.ms_ssim_y.unwrap() - 1.).abs() < 1e-10);
        assert_eq!(r.gmsd_y.unwrap(), 0.);
        assert!((r.ssimulacra2.unwrap() - 100.).abs() < 1e-4);
    }
    #[test]
    fn known_psnr_uses_full_rgb16_not_an_eight_bit_preview() {
        let a = frame(0);
        let b = frame(65535);
        let r = compute(&a, &b, 512, &|| false).unwrap();
        assert_eq!(r.pixels.as_ref().unwrap().psnr_db, Some(0.));
        assert_eq!(r.pixels.as_ref().unwrap().rmse, 1.);
        assert_eq!(r.pixels.as_ref().unwrap().changed_percent, 100.);
        let mut b = frame(0);
        b.rgb[0] = 1;
        let r = compute(&a, &b, 512, &|| false).unwrap();
        assert!(r.pixels.as_ref().unwrap().psnr_db.unwrap() > 140.);
        assert_eq!(r.pixels.as_ref().unwrap().max_error, 1. / 65535.);
    }
    #[test]
    fn dimensions_are_never_silently_warped() {
        let a = frame(0);
        let mut b = frame(0);
        b.width = 96;
        assert!(compute(&a, &b, 512, &|| false).is_err());
        assert!(compute(&a, &a, 512, &|| true).is_err());
        assert_eq!(analysis_size([8280, 5520], 1536), [1536, 1024]);
        assert_eq!(analysis_size([128, 64], 1536), [128, 64]);
    }
    #[test]
    fn small_odd_images_have_explicit_unavailable_scales() {
        let a = Frame {
            width: 9,
            height: 13,
            rgb: vec![1000; 9 * 13 * 3],
        };
        let r = compute(&a, &a, 512, &|| false).unwrap();
        assert!(r.ssim_y.is_none());
        assert!(r.ms_ssim_y.is_none());
        assert_eq!(r.gmsd_y, Some(0.));
    }
}
