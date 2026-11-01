#![allow(dead_code)]
mod io;
mod memory;
mod native_jxl;
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
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    ffi::{c_void, CStr},
    io::Write,
    path::{Path, PathBuf},
    time::Instant,
};

#[derive(Default)]
struct Args {
    path: PathBuf,
    library: Option<PathBuf>,
    backend: String,
    threads: usize,
    mapped: bool,
    reuse: bool,
    trace_alloc: bool,
    transport_only: bool,
    extract: Option<PathBuf>,
    tile_index: usize,
    raw_output: Option<PathBuf>,
    rgb_output: Option<PathBuf>,
}
fn args() -> Result<Args> {
    let mut result = Args {
        backend: "oxide".into(),
        threads: 6,
        ..Args::default()
    };
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        let name = arg.to_string_lossy();
        let value = |args: &mut std::iter::Skip<std::env::ArgsOs>| {
            args.next()
                .with_context(|| format!("Missing value for {name}"))
        };
        match name.as_ref() {
            "--backend" => result.backend = value(&mut args)?.to_string_lossy().into_owned(),
            "--threads" => result.threads = value(&mut args)?.to_string_lossy().parse()?,
            "--library" => result.library = Some(value(&mut args)?.into()),
            "--io" => {
                let mode = value(&mut args)?;
                ensure!(mode == "buffered" || mode == "mmap", "Unknown I/O mode");
                result.mapped = mode == "mmap";
            }
            "--reuse" => result.reuse = true,
            "--trace-alloc" => result.trace_alloc = true,
            "--transport-only" => result.transport_only = true,
            "--extract-tile" => result.extract = Some(value(&mut args)?.into()),
            "--tile-index" => result.tile_index = value(&mut args)?.to_string_lossy().parse()?,
            "--raw-output" => result.raw_output = Some(value(&mut args)?.into()),
            "--rgb-output" => result.rgb_output = Some(value(&mut args)?.into()),
            _ if !name.starts_with('-') && result.path.as_os_str().is_empty() => {
                result.path = arg.into()
            }
            _ => anyhow::bail!("Unknown argument: {name}"),
        }
    }
    ensure!(!result.path.as_os_str().is_empty(), "Usage: dng_jxl_bench.exe [--backend oxide|production|libjxl] [--library ABSOLUTE_DLL] [--threads N] [--io buffered|mmap] [--reuse] [--trace-alloc] [--transport-only] INPUT.dng");
    ensure!(
        (1..=64).contains(&result.threads),
        "Threads must be between 1 and 64"
    );
    ensure!(
        ["oxide", "production", "libjxl"].contains(&result.backend.as_str()),
        "Unknown backend"
    );
    ensure!(
        result.backend == "libjxl" || (!result.reuse && !result.trace_alloc),
        "Reuse/allocation tracing require libjxl"
    );
    ensure!(
        !result.transport_only || result.rgb_output.is_none(),
        "RGB export requires full development"
    );
    Ok(result)
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("Create new output {}", path.display()))?;
    file.write_all(bytes)?;
    Ok(())
}
#[repr(C)]
#[derive(Default)]
struct RawView {
    owner: *mut c_void,
    data: *const u8,
    width: u32,
    height: u32,
    colors: u32,
    bits: u32,
    length: u32,
    format: u32,
    flip: u32,
    area: dng::RawArea,
    open_ns: u64,
    unpack_ns: u64,
    process_ns: u64,
    bitmap_ns: u64,
}
extern "C" {
    fn diff_raw_open(
        bytes: *const c_void,
        length: usize,
        thumbnail: i32,
        out: *mut RawView,
        error: *mut i8,
        capacity: usize,
    ) -> i32;
    fn diff_raw_close(owner: *mut c_void);
}
impl Drop for RawView {
    fn drop(&mut self) {
        if !self.owner.is_null() {
            unsafe { diff_raw_close(self.owner) };
            self.owner = std::ptr::null_mut();
        }
    }
}
fn develop(bytes: &[u8]) -> Result<(Vec<u16>, [u32; 2], serde_json::Value)> {
    let start = Instant::now();
    let mut view = RawView::default();
    let mut error = [0i8; 512];
    let status = unsafe {
        diff_raw_open(
            bytes.as_ptr().cast(),
            bytes.len(),
            0,
            &mut view,
            error.as_mut_ptr(),
            error.len(),
        )
    };
    ensure!(
        status == 0,
        "LibRaw: {}",
        unsafe { CStr::from_ptr(error.as_ptr()) }.to_string_lossy()
    );
    ensure!(
        !view.data.is_null()
            && view.width > 0
            && view.height > 0
            && view.colors == 3
            && view.bits == 16
            && view.format == 2,
        "Unexpected native bitmap"
    );
    ensure!(
        view.length as usize == view.width as usize * view.height as usize * 6,
        "Invalid native bitmap length"
    );
    let native_call_ms = io::ms(start);
    memory::checkpoint("libraw_bitmap_ready");
    let timer = Instant::now();
    let [x, y, width, height] =
        dng::crop_rect(bytes, [view.width, view.height], view.area, view.flip)?;
    let crop_geometry_ms = io::ms(timer);
    let timer = Instant::now();
    let count = width as usize * height as usize * 3;
    let mut pixels = Vec::<u16>::with_capacity(count);
    let rgb_reserve_ms = io::ms(timer);
    let timer = Instant::now();
    for row in 0..height as usize {
        let source = ((y as usize + row) * view.width as usize + x as usize) * 6;
        unsafe {
            std::ptr::copy_nonoverlapping(
                view.data.add(source),
                pixels
                    .as_mut_ptr()
                    .add(row * width as usize * 3)
                    .cast::<u8>(),
                width as usize * 6,
            )
        };
    }
    // All bytes of these valid u16 samples were initialized by the row copies.
    unsafe { pixels.set_len(count) };
    let rgb_copy_first_touch_ms = io::ms(timer);
    memory::checkpoint("final_rgb_ready");
    let native = json!({"call_ms": native_call_ms, "open_ms": view.open_ns as f64 / 1e6,
        "unpack_ms": view.unpack_ns as f64 / 1e6, "process_ms": view.process_ns as f64 / 1e6,
        "bitmap_ms": view.bitmap_ns as f64 / 1e6});
    let timer = Instant::now();
    drop(view);
    let native_release_ms = io::ms(timer);
    Ok((
        pixels,
        [width, height],
        json!({"native": native, "crop_geometry_ms": crop_geometry_ms,
        "rgb_reserve_ms": rgb_reserve_ms, "rgb_copy_first_touch_ms": rgb_copy_first_touch_ms,
        "native_release_ms": native_release_ms, "total_ms": io::ms(start)}),
    ))
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
    memory::checkpoint("process_start");
    let process_start = Instant::now();
    let setup = Instant::now();
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(args.threads)
        .build()?;
    let pool_setup_ms = io::ms(setup);
    let setup = Instant::now();
    let library = if args.backend == "libjxl" {
        Some(native_jxl::JxlLibrary::load(
            args.library
                .as_deref()
                .context("libjxl requires --library ABSOLUTE_DLL")?,
        )?)
    } else {
        None
    };
    let library_setup_ms = io::ms(setup);
    let (input, io_timings) = io::read(&args.path, args.mapped)?;
    memory::checkpoint("source_resident");
    if let Some(path) = &args.extract {
        let (tile, layout) = dng::bench_tile(input.as_ref(), args.tile_index)?;
        write_new(path, tile)?;
        println!(
            "{}",
            json!({"success": true, "operation": "extract_tile", "tile_index": args.tile_index,
            "layout": layout, "io": io_timings, "tile_sha256": hash(tile),
            "tile_bytes": tile.len(), "prefix_hex": tile.iter().take(16).map(|b| format!("{b:02x}")).collect::<String>()})
        );
        return Ok(());
    }
    let transport = if args.backend == "production" {
        let start = Instant::now();
        let (_, layout) = dng::bench_tile(input.as_ref(), 0)?;
        let source = input.as_ref();
        let bytes = pool
            .install(|| dng::unpack(source))?
            .context("No JPEG XL raw IFD")?;
        dng::BenchTransport {
            bytes,
            base: input.as_ref().len().next_multiple_of(4),
            layout,
            timings: dng::BenchTransportTimings {
                total_ms: io::ms(start),
                ..Default::default()
            },
        }
    } else {
        dng::bench_unpack(
            input.as_ref(),
            library.as_ref(),
            &pool,
            args.reuse,
            args.trace_alloc,
        )?
    };
    memory::checkpoint("transport_ready");
    let (pixels, size, development) = if args.transport_only {
        (Vec::new(), [0, 0], json!(null))
    } else {
        develop(&transport.bytes)?
    };
    let validation = Instant::now();
    let raw = &transport.bytes[transport.base..transport.base + transport.layout.sample_bytes];
    let raw_hash = hash(raw);
    let source_hash = hash(input.as_ref());
    let rgb_bytes =
        unsafe { std::slice::from_raw_parts(pixels.as_ptr().cast::<u8>(), pixels.len() * 2) };
    let rgb_hash = (!args.transport_only).then(|| hash(rgb_bytes));
    if let Some(path) = &args.raw_output {
        write_new(path, raw)?;
    }
    if let Some(path) = &args.rgb_output {
        write_new(path, rgb_bytes)?;
    }
    let validation_and_export_ms = io::ms(validation);
    let source_bytes = input.as_ref().len();
    let virtual_bytes = transport.bytes.len();
    let dng::BenchTransport {
        bytes,
        layout,
        timings,
        ..
    } = transport;
    let timer = Instant::now();
    drop(bytes);
    drop(input);
    let transport_and_source_release_ms = io::ms(timer);
    memory::checkpoint("only_final_rgb_retained");
    let decode_ms = io_timings.total_ms
        + timings.total_ms
        + development["total_ms"].as_f64().unwrap_or(0.0)
        + transport_and_source_release_ms;
    println!(
        "{}",
        json!({"success": true, "source_name": args.path.file_name().map(|s| s.to_string_lossy()),
        "backend": args.backend, "threads": args.threads, "native_threads_per_tile": 1,
        "reuse_decoder": args.reuse, "allocation_tracing": args.trace_alloc,
        "library_version": library.as_ref().map(|lib| lib.version()), "library_setup_ms": library_setup_ms,
        "pool_setup_ms": pool_setup_ms, "source_bytes": source_bytes, "virtual_bytes": virtual_bytes,
        "layout": layout, "io": io_timings, "transport": timings, "development": development,
        "transport_and_source_release_ms": transport_and_source_release_ms,
        "pipeline_ms_excluding_setup_validation": decode_ms, "validation_and_export_ms": validation_and_export_ms,
        "process_wall_ms": io::ms(process_start), "source_sha256": source_hash,
        "canvas_sha256": raw_hash, "rgb16_sha256": rgb_hash, "rgb_size": size,
        "memory": memory::snapshot(), "checkpoints": memory::checkpoints(),
        "cache_boundary": "fresh process; OS file cache not flushed; buffered reads or explicit mmap prefault; no UI/GPU"})
    );
    Ok(())
}
#[path = "../../src/jxl.rs"]
mod jxl;
