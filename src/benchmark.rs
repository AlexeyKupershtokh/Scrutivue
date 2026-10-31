use crate::{
    decode::{self, Frame},
    gpu::Gpu,
    model::{Mode, Settings, Zoom},
    processing::Cpu,
};
use anyhow::{Context, Result};
use rayon::prelude::*;
use serde_json::json;
use std::{path::PathBuf, sync::Arc, time::Instant};

pub fn probe(paths: &[PathBuf]) -> Result<()> {
    anyhow::ensure!(!paths.is_empty(), "Pass at least one image path");
    let mut results = Vec::new();
    for path in paths {
        let start = Instant::now();
        let thumbnail = decode::thumbnail(path)?;
        let thumbnail_ms = start.elapsed().as_secs_f64() * 1000.;
        let start = Instant::now();
        let frame = decode::decode(path)?;
        results.push(json!({ "path": path, "dimensions": frame.size(), "rgb16_bytes": frame.bytes(),
            "thumbnail_dimensions": [thumbnail.width(), thumbnail.height()], "thumbnail_ms": thumbnail_ms,
            "decode_ms": start.elapsed().as_secs_f64() * 1000., "sample_sum": frame.rgb.iter().map(|v| *v as u64).sum::<u64>() }));
    }
    println!("{}", serde_json::to_string_pretty(&results)?);
    Ok(())
}

pub fn run(paths: &[PathBuf], force_cpu: bool, iterations: usize) -> Result<()> {
    let load_start = Instant::now();
    let (a, b) = if paths.len() >= 2 {
        (decode::decode(&paths[0])?, decode::decode(&paths[1])?)
    } else {
        let w = 8256;
        let h = 5504;
        let mut rgb = vec![0u16; w as usize * h as usize * 3];
        rgb.par_chunks_mut(3).enumerate().for_each(|(i, p)| {
            let hash = (i as u32).wrapping_mul(747796405).wrapping_add(2891336453);
            p.copy_from_slice(&[
                (hash & 65535) as u16,
                (hash >> 16) as u16,
                ((hash ^ (hash >> 9)) & 65535) as u16,
            ]);
        });
        let mut second = rgb.clone();
        second.par_iter_mut().enumerate().for_each(|(i, v)| {
            *v = v.saturating_add((i % 19) as u16);
        });
        (
            Arc::new(Frame {
                width: w,
                height: h,
                rgb,
            }),
            Arc::new(Frame {
                width: w,
                height: h,
                rgb: second,
            }),
        )
    };
    let decode_ms = load_start.elapsed().as_secs_f64() * 1000.;
    let mut settings = Settings::default();
    settings.viewport = [1920, 1080];
    settings.mode = Mode::Diff;
    let mut gpu = if force_cpu {
        None
    } else {
        Some(Gpu::new().context("GPU benchmark initialization (use --cpu for CPU)")?)
    };
    let backend = gpu
        .as_ref()
        .map(|g| g.name.clone())
        .unwrap_or_else(|| "CPU / Rayon".into());
    let mut cpu = Cpu::new();
    let mut render = |s: &Settings| -> Result<()> {
        let pixels = match &mut gpu {
            Some(g) => g.render((1, &a), (2, &b), s, false)?,
            None => cpu.render((1, &a), (2, &b), s, false),
        };
        std::hint::black_box(pixels);
        Ok(())
    };
    let start = Instant::now();
    render(&settings)?;
    let cold_ms = start.elapsed().as_secs_f64() * 1000.;
    let mut scenarios = Vec::new();
    for highpass in [false, true] {
        settings.highpass = highpass;
        let start = Instant::now();
        render(&settings)?;
        let prepare_ms = start.elapsed().as_secs_f64() * 1000.;
        let mut times = Vec::new();
        for i in 0..iterations.max(3) {
            settings.camera.zoom = Zoom::Fixed([1., 2., 4.][i % 3]);
            settings.camera.center = [0.5 + (i as f32 * 0.001), 0.5];
            settings.gain = [1, 4, 16, 64, 256][i % 5];
            let start = Instant::now();
            render(&settings)?;
            times.push(start.elapsed().as_secs_f64() * 1000.);
        }
        times.sort_by(f64::total_cmp);
        scenarios.push(json!({"highpass": highpass, "prepare_ms": prepare_ms,
            "median_ms": times[times.len() / 2], "p95_ms": times[((times.len() as f64 * 0.95).ceil() as usize - 1).min(times.len()-1)],
            "max_ms": times.last(), "iterations": times.len()}));
    }
    let mut radius_changes = Vec::new();
    for sigma in [1.0, 2.0, 4.0, 8.0] {
        settings.sigma = sigma;
        let start = Instant::now();
        render(&settings)?;
        radius_changes.push(json!({"sigma": sigma, "ms": start.elapsed().as_secs_f64() * 1000.}));
    }
    println!(
        "{}",
        serde_json::to_string_pretty(
            &json!({ "backend": backend, "source_a": a.size(), "source_b": b.size(),
        "viewport": settings.viewport, "decode_or_generate_ms": decode_ms, "cold_upload_render_ms": cold_ms,
        "note": "Compute, GPU readback and host Vec copy; excludes GPUI texture upload and display presentation.", "scenarios": scenarios, "radius_changes": radius_changes })
        )?
    );
    Ok(())
}
