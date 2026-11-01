mod io;
mod memory;
use anyhow::{ensure, Context, Result};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{path::PathBuf, time::Instant};
fn run() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let mode = args
        .next()
        .context("Usage: dng_io_bench.exe buffered|mmap|direct INPUT")?;
    ensure!(
        mode == "buffered" || mode == "mmap" || mode == "direct",
        "Unknown I/O mode"
    );
    let path = PathBuf::from(args.next().context("Missing input")?);
    ensure!(args.next().is_none(), "Unexpected argument");
    memory::checkpoint("start");
    let (input, timings) = if mode == "direct" {
        io::read_direct(&path)?
    } else {
        io::read(&path, mode == "mmap")?
    };
    memory::checkpoint("resident");
    let start = Instant::now();
    let hash = format!("{:x}", Sha256::digest(input.as_ref()));
    let hash_ms = io::ms(start);
    let start = Instant::now();
    drop(input);
    let release_ms = io::ms(start);
    println!(
        "{}",
        json!({"success": true, "source_name": path.file_name().map(|s| s.to_string_lossy()),
        "io": timings, "sha256": hash, "hash_ms": hash_ms, "release_ms": release_ms,
        "memory": memory::snapshot(), "checkpoints": memory::checkpoints(),
        "cache_boundary": "No device-cache flush; buffered/mmap can hit Windows cache; direct bypasses Windows file cache only"})
    );
    Ok(())
}
fn main() {
    if let Err(err) = run() {
        eprintln!("{}", json!({"success": false, "error": format!("{err:#}")}));
        std::process::exit(1);
    }
}
