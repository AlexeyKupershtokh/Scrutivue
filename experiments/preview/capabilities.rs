#![allow(dead_code)]
#[path = "../dng-jxl/io.rs"]
mod io;
#[path = "../dng-jxl/memory.rs"]
mod memory;
#[path = "../dng-jxl/native_jxl.rs"]
mod native_jxl;
#[path = "../../src/jxl.rs"]
mod jxl;
mod dng {
    include!(env!("DNG_EXPERIMENT_SOURCE"));
}
#[path = "../../src/decode.rs"]
mod decode;
mod model {
    pub fn is_raw(path: &std::path::Path) -> bool {
        path.extension().is_some_and(|ext| {
            ["dng", "nef", "arw", "cr3"]
                .iter()
                .any(|name| ext.eq_ignore_ascii_case(name))
        })
    }
}

use anyhow::{ensure, Context, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    io::Write,
    path::{Path, PathBuf},
    time::Instant,
};

struct Args {
    mode: String,
    source: PathBuf,
    library: Option<PathBuf>,
    reference: Option<PathBuf>,
    roi: [usize; 4],
    threads: usize,
    output: Option<PathBuf>,
    progressive: bool,
    size: [usize; 2],
    downsampling: usize,
}
fn args() -> Result<Args> {
    let mut result = Args {
        mode: "embedded".into(),
        source: PathBuf::new(),
        library: None,
        reference: None,
        roi: [3884, 2504, 512, 512],
        threads: 6,
        output: None,
        progressive: false,
        size: [4096, 3072],
        downsampling: 4,
    };
    let mut args = std::env::args_os().skip(1);
    while let Some(argument) = args.next() {
        let name = argument.to_string_lossy();
        match name.as_ref() {
            "--mode" => {
                result.mode = args
                    .next()
                    .context("Missing mode")?
                    .to_string_lossy()
                    .into_owned()
            }
            "--library" => result.library = Some(args.next().context("Missing library")?.into()),
            "--reference" => {
                result.reference = Some(args.next().context("Missing reference")?.into())
            }
            "--output" => result.output = Some(args.next().context("Missing output")?.into()),
            "--threads" => {
                result.threads = args
                    .next()
                    .context("Missing threads")?
                    .to_string_lossy()
                    .parse()?
            }
            "--roi" => {
                let value = args.next().context("Missing x,y,width,height")?;
                let coordinates = value
                    .to_string_lossy()
                    .split(',')
                    .map(str::parse)
                    .collect::<std::result::Result<Vec<usize>, _>>()?;
                result.roi = coordinates
                    .try_into()
                    .map_err(|_| anyhow::anyhow!("Expected four ROI coordinates"))?;
            }
            "--size" => {
                let value = args.next().context("Missing width,height")?;
                let dimensions = value
                    .to_string_lossy()
                    .split(',')
                    .map(str::parse)
                    .collect::<std::result::Result<Vec<usize>, _>>()?;
                result.size = dimensions
                    .try_into()
                    .map_err(|_| anyhow::anyhow!("Expected width,height"))?;
            }
            "--progressive" => result.progressive = true,
            "--downsampling" => {
                result.downsampling = args
                    .next()
                    .context("Missing downsampling ratio")?
                    .to_string_lossy()
                    .parse()?;
            }
            "--help" => {
                println!("Usage: preview_capabilities.exe --mode MODE [--library ABSOLUTE_DLL] [--roi x,y,width,height] [--reference RAW_CANVAS] INPUT\nModes: embedded, jpeg-current, dng-roi-native, dng-roi-oxide, dng-roi-oxide-crop, standalone-full, standalone-roi, standalone-expand, native-progressive, djxl-downsampling, oxide-stream, generate\nDownsampling: --downsampling 1|2|4|8\nGenerate: --output NEW_JXL [--size width,height] [--progressive]");
                std::process::exit(0);
            }
            _ if !name.starts_with('-') && result.source.as_os_str().is_empty() => {
                result.source = argument.into()
            }
            _ => anyhow::bail!("Unknown argument: {name}"),
        }
    }
    ensure!((1..=64).contains(&result.threads), "Expected 1..64 workers");
    ensure!(
        matches!(result.downsampling, 1 | 2 | 4 | 8),
        "Expected downsampling 1, 2, 4 or 8"
    );
    ensure!(
        result.mode == "generate" || !result.source.as_os_str().is_empty(),
        "Missing input (see --help)"
    );
    Ok(result)
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn samples_bytes(samples: &[u16]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(samples.as_ptr().cast::<u8>(), samples.len() * 2) }
}
fn collect(render: &jxl_oxide::Render) -> Result<(Vec<u16>, [usize; 3])> {
    let mut stream = render.stream_no_alpha();
    let size = [
        stream.width() as usize,
        stream.height() as usize,
        stream.channels() as usize,
    ];
    let count = size[0]
        .checked_mul(size[1])
        .and_then(|n| n.checked_mul(size[2]))
        .context("Output overflow")?;
    ensure!(count <= 512 * 1024 * 1024, "Output exceeds 1 GiB");
    let mut samples = vec![0; count];
    ensure!(
        stream.write_to_buffer(&mut samples) == samples.len(),
        "Incomplete output"
    );
    Ok((samples, size))
}
fn map(path: &Path) -> Result<(std::fs::File, memmap2::Mmap)> {
    let file = std::fs::File::open(path)?;
    ensure!(
        file.metadata()?.len() > 0 && file.metadata()?.len() <= 1024 * 1024 * 1024,
        "Input outside 1 GiB bound"
    );
    // No prefault: the timed work faults in only the metadata/payloads it uses.
    let mapping = unsafe { memmap2::MmapOptions::new().map(&file)? };
    Ok((file, mapping))
}
fn compare_samples(
    pixels: &[u16],
    roi: [usize; 4],
    full: [usize; 3],
    little_endian: bool,
    path: &Path,
) -> Result<Value> {
    let (_file, reference) = map(path)?;
    ensure!(
        reference.len() == full[0] * full[1] * full[2] * 2,
        "Reference canvas length mismatch"
    );
    let [x, y, width, height] = roi;
    let mut different = 0u64;
    let mut maximum = 0u16;
    for row in 0..height {
        let first = ((y + row) * full[0] + x) * full[2];
        for sample in 0..width * full[2] {
            let offset = (first + sample) * 2;
            let bytes = [reference[offset], reference[offset + 1]];
            let expected = if little_endian {
                u16::from_le_bytes(bytes)
            } else {
                u16::from_be_bytes(bytes)
            };
            let actual = pixels[row * width * full[2] + sample];
            different += u64::from(actual != expected);
            maximum = maximum.max(actual.abs_diff(expected));
        }
    }
    Ok(
        json!({"different_samples": different, "maximum_absolute_error": maximum, "exact_match": different == 0}),
    )
}
fn render_standalone(
    bytes: &[u8],
    mode: &str,
    roi: [usize; 4],
    reference: Option<&Path>,
    output: Option<&Path>,
) -> Result<Value> {
    let total = Instant::now();
    let mut image = jxl_oxide::JxlImage::builder()
        .pool(jxl_oxide::JxlThreadPool::none())
        .alloc_tracker(jxl_oxide::AllocTracker::with_limit(1024 * 1024 * 1024))
        .read(bytes)
        .map_err(|err| anyhow::anyhow!("JXL parse: {err}"))?;
    let parse_ms = io::ms(total);
    let full = [image.width() as usize, image.height() as usize];
    ensure!(
        image.image_header().metadata.orientation == 1,
        "Standalone ROI experiment requires orientation 1"
    );
    if mode != "standalone-full" {
        ensure!(
            roi[2] > 0
                && roi[3] > 0
                && roi[0].checked_add(roi[2]).is_some_and(|n| n <= full[0])
                && roi[1].checked_add(roi[3]).is_some_and(|n| n <= full[1]),
            "ROI outside image"
        );
        image.set_image_region(jxl_oxide::CropInfo {
            left: roi[0] as u32,
            top: roi[1] as u32,
            width: roi[2] as u32,
            height: roi[3] as u32,
        });
    }
    let timer = Instant::now();
    let render = image
        .render_frame(0)
        .map_err(|err| anyhow::anyhow!("JXL render: {err}"))?;
    let reconstruction_ms = io::ms(timer);
    let timer = Instant::now();
    let (samples, size) = collect(&render)?;
    let stream_ms = io::ms(timer);
    let first_ready_ms = io::ms(total);
    let first_memory = memory::snapshot();
    let first_hash = hash(samples_bytes(&samples));
    let validation = reference
        .map(|path| {
            compare_samples(
                &samples,
                if mode == "standalone-full" {
                    [0, 0, full[0], full[1]]
                } else {
                    roi
                },
                [full[0], full[1], size[2]],
                cfg!(target_endian = "little"),
                path,
            )
        })
        .transpose()?;
    if let Some(value) = &validation {
        ensure!(
            value["exact_match"] == true,
            "Standalone ROI differs from full decode"
        );
    }
    if let Some(path) = output {
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(path)?;
        file.write_all(samples_bytes(&samples))?;
    }
    drop(render);
    if mode == "standalone-expand" {
        image.set_image_region(jxl_oxide::CropInfo {
            left: 0,
            top: 0,
            width: full[0] as u32,
            height: full[1] as u32,
        });
        let timer = Instant::now();
        let render = image
            .render_frame(0)
            .map_err(|err| anyhow::anyhow!("Expanded JXL render: {err}"))?;
        let (all, full_size) = collect(&render)?;
        let expansion_ms = io::ms(timer);
        let final_hash = hash(samples_bytes(&all));
        let mut different = 0u64;
        for y in 0..roi[3] {
            let first = ((y + roi[1]) * full_size[0] + roi[0]) * full_size[2];
            let actual = &samples[y * size[0] * size[2]..(y + 1) * size[0] * size[2]];
            different += actual
                .iter()
                .zip(&all[first..first + actual.len()])
                .filter(|(a, b)| a != b)
                .count() as u64;
        }
        ensure!(
            different == 0,
            "ROI changed after expanding the same decoder"
        );
        return Ok(
            json!({"full_size": full, "roi": roi, "parse_ms": parse_ms, "first_roi_ms": first_ready_ms,
            "reconstruction_ms": reconstruction_ms, "stream_ms": stream_ms, "roi_sha256": first_hash,
            "expansion_to_full_ms": expansion_ms, "full_sha256": final_hash, "roi_vs_expanded_different_samples": different,
            "first_roi_memory": first_memory, "same_decoder_resumed": true, "native_threads": 1, "validation": validation}),
        );
    }
    Ok(
        json!({"full_size": full, "output_size": size, "roi": (mode != "standalone-full").then_some(roi),
        "parse_ms": parse_ms, "reconstruction_ms": reconstruction_ms, "stream_ms": stream_ms,
        "ready_ms": first_ready_ms, "sha256": first_hash, "output_sample_bytes": samples.len() * 2,
        "memory_at_first_ready": first_memory, "native_threads": 1, "validation": validation}),
    )
}

fn grid(samples: &[u16], size: [usize; 3]) -> Vec<u16> {
    let mut values = Vec::new();
    for y in 0..32usize {
        for x in 0..32usize {
            let first = (((y * size[1] / 32).min(size[1] - 1)) * size[0]
                + (x * size[0] / 32).min(size[0] - 1))
                * size[2];
            values.extend_from_slice(&samples[first..first + size[2]]);
        }
    }
    values
}

fn current_jpeg(bytes: &[u8]) -> Result<Value> {
    use image::ImageDecoder;
    let total = Instant::now();
    let decoder = image::codecs::jpeg::JpegDecoder::new(std::io::Cursor::new(bytes))?;
    let header_and_compressed_copy_ms = io::ms(total);
    let (width, height) = decoder.dimensions();
    let channels = decoder.color_type().channel_count();
    ensure!(
        decoder.total_bytes() <= 1024 * 1024 * 1024,
        "JPEG output exceeds 1 GiB"
    );
    let timer = Instant::now();
    let mut pixels = vec![0u8; decoder.total_bytes() as usize];
    let output_alloc_ms = io::ms(timer);
    let timer = Instant::now();
    decoder.read_image(&mut pixels)?;
    let decode_ms = io::ms(timer);
    let ready_ms = io::ms(total);
    let memory_at_first_ready = memory::snapshot();
    Ok(
        json!({"backend": "image 0.25.10 / zune-jpeg 0.5.15, existing optimized dependencies",
        "width": width, "height": height, "channels": channels, "output_bytes": pixels.len(),
        "header_and_compressed_copy_ms": header_and_compressed_copy_ms,
        "output_alloc_ms": output_alloc_ms, "decode_ms": decode_ms, "ready_ms": ready_ms,
        "memory_at_first_ready": memory_at_first_ready, "sha256": hash(&pixels),
        "domain": "full unrotated output, no ICC display conversion; current API exposes neither scaled IDCT nor ROI"}),
    )
}
fn oxide_stream(bytes: &[u8]) -> Result<Value> {
    let total = Instant::now();
    let mut uninit = Some(
        jxl_oxide::JxlImage::builder()
            .pool(jxl_oxide::JxlThreadPool::none())
            .alloc_tracker(jxl_oxide::AllocTracker::with_limit(1024 * 1024 * 1024))
            .build_uninit(),
    );
    let mut image = None::<jxl_oxide::JxlImage>;
    let mut pending = Vec::new();
    let mut supplied = 0;
    let mut boundaries = vec![
        16,
        64,
        256,
        1024,
        4096,
        bytes.len() / 8,
        bytes.len() / 4,
        bytes.len() / 2,
        bytes.len() * 3 / 4,
        bytes.len(),
    ];
    boundaries.retain(|n| *n > 0 && *n <= bytes.len());
    boundaries.sort_unstable();
    boundaries.dedup();
    let mut observations = Vec::new();
    let mut grids = Vec::new();
    for boundary in boundaries {
        pending.extend_from_slice(&bytes[supplied..boundary]);
        supplied = boundary;
        if let Some(current) = image.as_mut() {
            let consumed = current
                .feed_bytes(&pending)
                .map_err(|err| anyhow::anyhow!("Feed JXL: {err}"))?;
            pending.drain(..consumed);
        } else {
            let mut current = uninit.take().unwrap();
            let consumed = current
                .feed_bytes(&pending)
                .map_err(|err| anyhow::anyhow!("Feed header: {err}"))?;
            pending.drain(..consumed);
            match current
                .try_init()
                .map_err(|err| anyhow::anyhow!("Init JXL: {err}"))?
            {
                jxl_oxide::InitializeResult::Initialized(current) => image = Some(current),
                jxl_oxide::InitializeResult::NeedMoreData(current) => uninit = Some(current),
            }
        }
        let Some(current) = image.as_mut() else {
            continue;
        };
        let timer = Instant::now();
        let render = if current.num_loaded_keyframes() > 0 {
            current.render_frame(0)
        } else {
            current.render_loading_frame()
        };
        match render {
            Ok(render) => {
                let (pixels, size) = collect(&render)?;
                observations.push(json!({"supplied_bytes": supplied, "input_fraction": supplied as f64 / bytes.len() as f64,
                    "output_size": size, "loading_done": current.is_loading_done(), "loaded_keyframes": current.num_loaded_keyframes(),
                    "render_and_stream_ms": io::ms(timer), "ready_ms": io::ms(total), "supplied_before_eof": supplied < bytes.len()}));
                grids.push(grid(&pixels, size));
            }
            Err(error) => {
                observations.push(
                    json!({"supplied_bytes": supplied, "render_unavailable": error.to_string()}),
                );
                grids.push(Vec::new());
            }
        }
    }
    let mut image = image.context("No complete JXL header")?;
    image
        .finalize()
        .map_err(|err| anyhow::anyhow!("Finalize JXL: {err}"))?;
    let (final_pixels, size) = collect(
        &image
            .render_frame(0)
            .map_err(|err| anyhow::anyhow!("Final JXL: {err}"))?,
    )?;
    let final_ready_ms = io::ms(total);
    let memory_at_final_ready = memory::snapshot();
    let expected_grid = grid(&final_pixels, size);
    for (event, values) in observations.iter_mut().zip(grids) {
        if values.len() == expected_grid.len() {
            let sum: u64 = values
                .iter()
                .zip(&expected_grid)
                .map(|(a, b)| u64::from(a.abs_diff(*b)))
                .sum();
            event["grid_mean_absolute_error_u16"] = json!(sum as f64 / values.len() as f64);
            event["grid_nonzero_fraction"] =
                json!(values.iter().filter(|v| **v != 0).count() as f64 / values.len() as f64);
        }
    }
    let fresh = jxl_oxide::JxlImage::builder()
        .pool(jxl_oxide::JxlThreadPool::none())
        .read(bytes)
        .map_err(|err| anyhow::anyhow!("Fresh reference: {err}"))?;
    let (fresh_pixels, _) = collect(
        &fresh
            .render_frame(0)
            .map_err(|err| anyhow::anyhow!("Reference render: {err}"))?,
    )?;
    ensure!(
        final_pixels == fresh_pixels,
        "Incremental final output differs from fresh full decode"
    );
    Ok(
        json!({"events": observations, "full_size": size, "final_ready_ms": final_ready_ms,
        "final_sha256": hash(samples_bytes(&final_pixels)), "same_decoder_continued": true, "matches_fresh_full_decode": true,
        "memory_at_final_ready": memory_at_final_ready,
        "scope": "diagnostic snapshots include repeated render overhead; success can be a spatially incomplete image, not guaranteed uniform half/quarter resolution"}),
    )
}

fn main() {
    if let Err(error) = run() {
        eprintln!(
            "{}",
            json!({"success": false, "error": format!("{error:#}")})
        );
        std::process::exit(1);
    }
}
fn run() -> Result<()> {
    let args = args()?;
    memory::checkpoint("start");
    let setup = Instant::now();
    let library = if matches!(
        args.mode.as_str(),
        "native-progressive" | "djxl-downsampling" | "dng-roi-native" | "generate"
    ) {
        Some(native_jxl::JxlLibrary::load(
            args.library
                .as_deref()
                .context("This mode needs --library")?,
        )?)
    } else {
        None
    };
    let library_setup_ms = io::ms(setup);
    if args.mode == "generate" {
        let path = args.output.context("Generate needs --output NEW_FILE")?;
        let start = Instant::now();
        let encoded = native_jxl::encode_synthetic(
            library.as_ref().unwrap(),
            args.size[0],
            args.size[1],
            args.progressive,
        )?;
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(path)?;
        file.write_all(&encoded)?;
        println!(
            "{}",
            json!({"success": true, "size": args.size, "progressive": args.progressive, "encoded_bytes": encoded.len(), "sha256": hash(&encoded), "ms": io::ms(start)})
        );
        return Ok(());
    }
    let setup = Instant::now();
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(args.threads)
        .build()?;
    let pool_setup_ms = io::ms(setup);
    let start = Instant::now();
    let (file, bytes) = map(&args.source)?;
    let before = file.metadata()?;
    let open_map_ms = io::ms(start);
    let start = Instant::now();
    let mut validation = json!(null);
    let work;
    let memory_at_ready;
    let work_ms;
    match args.mode.as_str() {
        "jpeg-current" => {
            work = current_jpeg(&bytes)?;
            work_ms = io::ms(start);
            memory_at_ready = memory::snapshot();
        }
        "embedded" => {
            let payload = dng::embedded_jpeg_payload(&bytes)?;
            let image = dng::preview(&bytes)?.context("No embedded preview")?;
            work_ms = io::ms(start);
            memory_at_ready = memory::snapshot();
            work = json!({"jpeg_bytes": payload.len(), "oriented_size": [image.width(), image.height()],
                "decoded_bytes": image.as_bytes().len(), "decode_and_orientation_ms": work_ms,
                "role": "embedded camera/producer preview, approximate appearance; not original RGB16"});
        }
        "dng-roi-native" | "dng-roi-oxide" | "dng-roi-oxide-crop" => {
            let region = dng::decode_region(
                &bytes,
                args.roi,
                library.as_ref(),
                &pool,
                args.mode == "dng-roi-oxide-crop",
            )?;
            work_ms = io::ms(start);
            memory_at_ready = memory::snapshot();
            work = region.report.clone();
            let timer = Instant::now();
            let roi_hash = hash(samples_bytes(&region.pixels));
            if let Some(path) = &args.reference {
                validation = compare_samples(
                    &region.pixels,
                    region.roi,
                    [
                        region.layout.width,
                        region.layout.height,
                        region.layout.channels,
                    ],
                    region.layout.little_endian,
                    path,
                )?;
            }
            validation["roi_sha256"] = json!(roi_hash);
            validation["validation_ms"] = json!(io::ms(timer));
            ensure!(
                validation["exact_match"].as_bool() != Some(false),
                "ROI did not match full raw canvas"
            );
        }
        "standalone-full" | "standalone-roi" | "standalone-expand" => {
            work = render_standalone(
                &bytes,
                &args.mode,
                args.roi,
                args.reference.as_deref(),
                args.output.as_deref(),
            )?;
            work_ms = io::ms(start);
            memory_at_ready = memory::snapshot();
        }
        "native-progressive" => {
            work =
                native_jxl::probe_progressive(library.as_ref().unwrap(), &bytes, 3, 16384, 16384)?;
            work_ms = io::ms(start);
            memory_at_ready = memory::snapshot();
        }
        "djxl-downsampling" => {
            let library = library.as_ref().unwrap();
            let result = native_jxl::downsample_like_djxl(
                library,
                &bytes,
                3,
                16384,
                16384,
                args.downsampling,
            )?;
            work_ms = io::ms(start);
            memory_at_ready = memory::snapshot();
            let timer = Instant::now();
            let output_hash = hash(samples_bytes(&result.samples));
            let mut baseline_worker = native_jxl::JxlWorker::new(library, false, false)?;
            let (baseline, dimensions, _) = baseline_worker.decode(&bytes, 3, 16384, 16384)?;
            let exact = dimensions == [result.width, result.height] && baseline == result.samples;
            ensure!(
                result.is_partial || exact,
                "djxl full fallback differs from independent full output"
            );
            // Independent bitstream metadata, not part of the native ready timer.
            // No render_frame call: this is not a second RGB reconstruction.
            let parsed = jxl_oxide::JxlImage::builder()
                .pool(jxl_oxide::JxlThreadPool::none())
                .alloc_tracker(jxl_oxide::AllocTracker::with_limit(1024 * 1024 * 1024))
                .read(&bytes[..])
                .map_err(|err| anyhow::anyhow!("JXL header inspection: {err}"))?;
            let frame = parsed
                .frame_header(0)
                .context("Missing inspected JXL frame header")?;
            validation = json!({"output_sha256": output_hash, "baseline_sha256": hash(samples_bytes(baseline)),
                "matches_full_baseline": exact, "baseline_hash_and_inspection_ms": io::ms(timer),
                "frame_header": {"encoding": format!("{:?}", frame.encoding), "groups": frame.num_groups(),
                    "passes": frame.passes.num_passes, "downsample_targets": frame.passes.downsample,
                    "last_pass": frame.passes.last_pass}});
            work = result.report;
        }
        "oxide-stream" => {
            work = oxide_stream(&bytes)?;
            work_ms = io::ms(start);
            memory_at_ready = memory::snapshot();
        }
        _ => anyhow::bail!("Unknown mode (see --help)"),
    }
    let validation_start = Instant::now();
    let source_hash = hash(&bytes);
    let after = file.metadata()?;
    ensure!(
        before.len() == after.len() && before.modified().ok() == after.modified().ok(),
        "Source changed during experiment"
    );
    println!(
        "{}",
        json!({"success": true, "mode": args.mode, "source_name": args.source.file_name().map(|s| s.to_string_lossy()),
        "source_sha256": source_hash, "source_bytes": bytes.len(), "threads": args.threads,
        "open_map_ms": open_map_ms, "pool_setup_ms": pool_setup_ms, "library_setup_ms": library_setup_ms,
        "work_ms": work_ms, "work": work,
        "validation": validation, "source_hash_ms": io::ms(validation_start), "memory_at_decode_ready": memory_at_ready,
        "memory_after_validation": memory::snapshot(), "io_scope": "read-only mmap without whole-file prefault; faults for addressed bytes are inside work; OS cache not flushed"})
    );
    Ok(())
}

#[cfg(test)]
mod capability_tests {
    use super::*;

    const LOSSLESS: &[u8] = include_bytes!("../../assets/tests/dng-jxl-tile.jxl");

    #[test]
    fn standalone_region_expands_without_changing_samples() {
        let result =
            render_standalone(LOSSLESS, "standalone-expand", [3, 2, 8, 9], None, None).unwrap();
        assert_eq!(result["same_decoder_resumed"], true);
        assert_eq!(result["roi_vs_expanded_different_samples"], 0);
        let full = render_standalone(LOSSLESS, "standalone-full", [0; 4], None, None).unwrap();
        assert_eq!(result["full_sha256"], full["sha256"]);
    }

    #[test]
    fn standalone_region_rejects_empty_and_overflowing_coordinates() {
        for roi in [[0, 0, 0, 1], [15, 15, 2, 2], [usize::MAX, 0, 2, 1]] {
            assert!(render_standalone(LOSSLESS, "standalone-roi", roi, None, None).is_err());
        }
    }

    #[test]
    fn incremental_final_matches_fresh_lossless_decode() {
        let result = oxide_stream(LOSSLESS).unwrap();
        assert_eq!(result["matches_fresh_full_decode"], true);
        assert_eq!(result["same_decoder_continued"], true);
        assert!(!result["events"].as_array().unwrap().is_empty());
    }

    #[test]
    fn current_jpeg_probe_reports_complete_native_dimensions() {
        let mut encoded = Vec::new();
        let pixels = vec![96u8; 32 * 16 * 3];
        image::codecs::jpeg::JpegEncoder::new(&mut encoded)
            .encode(&pixels, 32, 16, image::ExtendedColorType::Rgb8)
            .unwrap();
        let result = current_jpeg(&encoded).unwrap();
        assert_eq!(result["width"], 32);
        assert_eq!(result["height"], 16);
        assert_eq!(result["output_bytes"], pixels.len());
        assert!(current_jpeg(b"not a JPEG").is_err());
    }
}
