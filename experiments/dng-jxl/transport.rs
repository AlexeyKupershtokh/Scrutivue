// Included inside the original dng module by the experiment builder. This reuses
// its exact bounded TIFF parser and IFD patching semantics without changing it.
#[derive(Default, Clone, Copy, serde::Serialize)]
pub struct BenchWorkers {
    pub decoder_setup_ns: u64,
    pub header_ns: u64,
    pub output_alloc_ns: u64,
    pub reconstruction_ns: u64,
    pub sample_conversion_ns: u64,
    pub assembly_ns: u64,
    pub destroy_ns: u64,
    pub allocator_ns: u64,
    pub allocator_calls: u64,
    pub allocator_bytes: u64,
    pub free_calls: u64,
    pub tiles: u64,
}
impl BenchWorkers {
    fn merge(&mut self, other: Self) {
        self.decoder_setup_ns += other.decoder_setup_ns;
        self.header_ns += other.header_ns;
        self.output_alloc_ns += other.output_alloc_ns;
        self.reconstruction_ns += other.reconstruction_ns;
        self.sample_conversion_ns += other.sample_conversion_ns;
        self.assembly_ns += other.assembly_ns;
        self.destroy_ns += other.destroy_ns;
        self.allocator_ns += other.allocator_ns;
        self.allocator_calls += other.allocator_calls;
        self.allocator_bytes += other.allocator_bytes;
        self.free_calls += other.free_calls;
        self.tiles += other.tiles;
    }
}
#[derive(Default, serde::Serialize)]
pub struct BenchTransportTimings {
    pub parse_ms: f64,
    pub reserve_ms: f64,
    pub compressed_copy_ms: f64,
    pub zero_first_touch_ms: f64,
    pub parallel_tiles_ms: f64,
    pub patch_ifd_ms: f64,
    pub total_ms: f64,
    pub workers: BenchWorkers,
}
#[derive(serde::Serialize)]
pub struct BenchLayout {
    pub width: usize,
    pub height: usize,
    pub tile_width: usize,
    pub tile_height: usize,
    pub channels: usize,
    pub tiles: usize,
    pub compressed_tile_bytes: usize,
    pub sample_bytes: usize,
    pub little_endian: bool,
}
pub struct BenchTransport {
    pub bytes: Vec<u8>,
    pub base: usize,
    pub layout: BenchLayout,
    pub timings: BenchTransportTimings,
}
fn bench_layout(tiff: &Tiff<'_>, ifd: &Ifd) -> Result<BenchLayout> {
    let width = tiff.value(ifd, 256, 0)? as usize;
    let height = tiff.value(ifd, 257, 0)? as usize;
    let tile_width = tiff.value(ifd, 322, 0)? as usize;
    let tile_height = tiff.value(ifd, 323, 0)? as usize;
    let channels = tiff.value(ifd, 277, 1)? as usize;
    ensure!(
        width > 0 && height > 0 && width <= 100000 && height <= 100000,
        "Invalid canvas dimensions"
    );
    ensure!(
        tile_width > 0 && tile_height > 0 && tile_width <= 16384 && tile_height <= 16384,
        "Invalid tile dimensions"
    );
    ensure!(
        matches!(channels, 1 | 3) && tiff.value(ifd, 284, 1)? == 1,
        "Expected chunky grey/RGB"
    );
    ensure!(
        tiff.values(ifd, 258)? == vec![16; channels] && tiff.value(ifd, 339, 1)? == 1,
        "Expected integer RGB/grey16"
    );
    let offsets = tiff.values(ifd, 324)?;
    let lengths = tiff.values(ifd, 325)?;
    let tiles = width
        .div_ceil(tile_width)
        .checked_mul(height.div_ceil(tile_height))
        .context("Tile count overflow")?;
    ensure!(
        tiles == offsets.len() && tiles == lengths.len(),
        "Invalid tile table"
    );
    for (&offset, &length) in offsets.iter().zip(&lengths) {
        ensure!(length > 0, "Empty tile");
        region(tiff.bytes, offset as usize, length as usize)?;
    }
    let sample_bytes = width
        .checked_mul(height)
        .and_then(|n| n.checked_mul(channels * 2))
        .context("Sample size overflow")?;
    ensure!(sample_bytes <= 1024 * 1024 * 1024, "Canvas exceeds 1 GiB");
    Ok(BenchLayout {
        width,
        height,
        tile_width,
        tile_height,
        channels,
        tiles,
        compressed_tile_bytes: lengths.iter().map(|n| *n as usize).sum(),
        sample_bytes,
        little_endian: tiff.endian.0,
    })
}
pub fn bench_tile(bytes: &[u8], index: usize) -> Result<(&[u8], BenchLayout)> {
    let tiff = Tiff::parse(bytes)?.context("Not a classic TIFF")?;
    let ifd = tiff.jxl_raw()?.context("No JPEG XL raw IFD")?;
    let layout = bench_layout(&tiff, ifd)?;
    ensure!(index < layout.tiles, "Tile index out of bounds");
    let offsets = tiff.values(ifd, 324)?;
    let lengths = tiff.values(ifd, 325)?;
    Ok((
        region(bytes, offsets[index] as usize, lengths[index] as usize)?,
        layout,
    ))
}
fn bench_pack(dest: &mut [u8], pixels: &[u16], e: Endian) {
    if e.0 == cfg!(target_endian = "little") {
        // u16 has no invalid bit patterns; the byte view is bounded by its slice.
        let bytes =
            unsafe { std::slice::from_raw_parts(pixels.as_ptr().cast::<u8>(), pixels.len() * 2) };
        dest.copy_from_slice(bytes);
    } else {
        for (dst, &sample) in dest.chunks_exact_mut(2).zip(pixels) {
            dst.copy_from_slice(&e.put16(sample));
        }
    }
}
pub fn bench_unpack(
    bytes: &[u8],
    native: Option<&crate::native_jxl::JxlLibrary>,
    pool: &rayon::ThreadPool,
    reuse: bool,
    trace_alloc: bool,
) -> Result<BenchTransport> {
    use std::time::Instant;
    let total = Instant::now();
    let mut timings = BenchTransportTimings::default();
    let start = Instant::now();
    let tiff = Tiff::parse(bytes)?.context("Not a classic TIFF")?;
    let ifd = tiff.jxl_raw()?.context("No JPEG XL raw IFD")?;
    let layout = bench_layout(&tiff, ifd)?;
    let (w, h, tw, th, channels) = (
        layout.width,
        layout.height,
        layout.tile_width,
        layout.tile_height,
        layout.channels,
    );
    let offsets = tiff.values(ifd, 324)?;
    let lengths = tiff.values(ifd, 325)?;
    let nx = w.div_ceil(tw);
    let ny = h.div_ceil(th);
    let e = tiff.endian;
    let base = bytes.len().next_multiple_of(4);
    ensure!(
        base + layout.sample_bytes + 65536 < u32::MAX as usize,
        "Classic TIFF overflow"
    );
    timings.parse_ms = crate::io::ms(start);
    let start = Instant::now();
    let mut output = Vec::with_capacity(base + layout.sample_bytes + 65536);
    timings.reserve_ms = crate::io::ms(start);
    let start = Instant::now();
    output.extend_from_slice(bytes);
    output.resize(base, 0);
    timings.compressed_copy_ms = crate::io::ms(start);
    let start = Instant::now();
    output.resize(base + layout.sample_bytes, 0);
    timings.zero_first_touch_ms = crate::io::ms(start);
    crate::memory::checkpoint("virtual_tiff_allocated");
    let rows_per_worker = ny.div_ceil(pool.current_num_threads());
    let band_bytes = rows_per_worker * th * w * channels * 2;
    let start = Instant::now();
    let workers = pool.install(|| {
        output[base..]
            .par_chunks_mut(band_bytes)
            .enumerate()
            .map(|(band, dest)| -> Result<BenchWorkers> {
                let mut stats = BenchWorkers::default();
                let worker_start = Instant::now();
                let mut native_worker = native
                    .map(|lib| crate::native_jxl::JxlWorker::new(lib, reuse, trace_alloc))
                    .transpose()?;
                stats.decoder_setup_ns += worker_start.elapsed().as_nanos() as u64;
                let first_row = band * rows_per_worker;
                for local_ty in 0..rows_per_worker.min(ny - first_row) {
                    let ty = first_row + local_ty;
                    for tx in 0..nx {
                        let index = ty * nx + tx;
                        let tile = region(bytes, offsets[index] as usize, lengths[index] as usize)?;
                        let mut oxide_pixels = Vec::new();
                        let (pixels, iw, ih) = if let Some(worker) = &mut native_worker {
                            let (pixels, [iw, ih], t) = worker.decode(tile, channels, tw, th)?;
                            stats.decoder_setup_ns += t.decoder_setup_ns;
                            stats.header_ns += t.header_ns;
                            stats.output_alloc_ns += t.output_alloc_ns;
                            stats.reconstruction_ns += t.reconstruction_ns;
                            stats.destroy_ns += t.destroy_ns;
                            stats.allocator_ns += t.allocator_ns;
                            stats.allocator_calls += t.allocator_calls;
                            stats.allocator_bytes += t.allocator_bytes;
                            stats.free_calls += t.free_calls;
                            (pixels, iw, ih)
                        } else {
                            let timer = Instant::now();
                            let image = jxl_oxide::JxlImage::builder()
                                .pool(jxl_oxide::JxlThreadPool::none())
                                .alloc_tracker(jxl_oxide::AllocTracker::with_limit(
                                    (tw * th * 64).clamp(64 * 1024 * 1024, 512 * 1024 * 1024),
                                ))
                                .read(tile)
                                .map_err(|err| anyhow::anyhow!("Tile {index}: {err}"))?;
                            stats.header_ns += timer.elapsed().as_nanos() as u64;
                            ensure!(
                                image.width() as usize <= tw && image.height() as usize <= th,
                                "Oversized tile"
                            );
                            let timer = Instant::now();
                            let render = image
                                .render_frame(0)
                                .map_err(|err| anyhow::anyhow!("Tile {index}: {err}"))?;
                            stats.reconstruction_ns += timer.elapsed().as_nanos() as u64;
                            let mut stream = render.stream_no_alpha();
                            let (iw, ih) = (stream.width() as usize, stream.height() as usize);
                            ensure!(
                                stream.channels() as usize == channels && iw <= tw && ih <= th,
                                "Unexpected tile output"
                            );
                            let timer = Instant::now();
                            oxide_pixels.resize(iw * ih * channels, 0);
                            stats.output_alloc_ns += timer.elapsed().as_nanos() as u64;
                            let timer = Instant::now();
                            ensure!(
                                stream.write_to_buffer(&mut oxide_pixels) == oxide_pixels.len(),
                                "Incomplete sample stream"
                            );
                            stats.sample_conversion_ns += timer.elapsed().as_nanos() as u64;
                            let timer = Instant::now();
                            drop(stream);
                            drop(render);
                            drop(image);
                            stats.destroy_ns += timer.elapsed().as_nanos() as u64;
                            (oxide_pixels.as_slice(), iw, ih)
                        };
                        let cw = tw.min(w - tx * tw);
                        let ch = th.min(h - ty * th);
                        ensure!(
                            iw >= cw && ih >= ch && pixels.len() == iw * ih * channels,
                            "Tile layout mismatch"
                        );
                        let timer = Instant::now();
                        for y in 0..ch {
                            let start = ((local_ty * th + y) * w + tx * tw) * channels * 2;
                            bench_pack(
                                &mut dest[start..start + cw * channels * 2],
                                &pixels[y * iw * channels..y * iw * channels + cw * channels],
                                e,
                            );
                        }
                        stats.assembly_ns += timer.elapsed().as_nanos() as u64;
                        stats.tiles += 1;
                    }
                }
                let timer = Instant::now();
                drop(native_worker);
                stats.destroy_ns += timer.elapsed().as_nanos() as u64;
                Ok(stats)
            })
            .collect::<Result<Vec<_>>>()
    })?;
    timings.parallel_tiles_ms = crate::io::ms(start);
    crate::memory::checkpoint("jxl_tiles_decoded");
    for worker in workers {
        timings.workers.merge(worker);
    }
    let start = Instant::now();
    let mut entries: Vec<_> = ifd
        .entries
        .iter()
        .filter(|entry| {
            !matches!(
                e.u16(*entry),
                259 | 273 | 278 | 279 | 317 | 322 | 323 | 324 | 325
            )
        })
        .copied()
        .collect();
    for (tag, kind, value) in [
        (259, 3, 1),
        (273, 4, base as u32),
        (278, 4, h as u32),
        (279, 4, layout.sample_bytes as u32),
    ] {
        let mut entry = [0u8; 12];
        entry[..2].copy_from_slice(&e.put16(tag));
        entry[2..4].copy_from_slice(&e.put16(kind));
        entry[4..8].copy_from_slice(&e.put32(1));
        if kind == 3 {
            entry[8..10].copy_from_slice(&e.put16(value as u16));
        } else {
            entry[8..12].copy_from_slice(&e.put32(value));
        }
        entries.push(entry);
    }
    entries.sort_by_key(|entry| e.u16(entry));
    ensure!(
        entries.len() <= ifd.entries.len(),
        "Replacement IFD too large"
    );
    output[ifd.offset..ifd.offset + 2].copy_from_slice(&e.put16(entries.len() as u16));
    for (i, entry) in entries.iter().enumerate() {
        let start = ifd.offset + 2 + i * 12;
        output[start..start + 12].copy_from_slice(entry);
    }
    let next = ifd.offset + 2 + entries.len() * 12;
    output[next..next + 4].copy_from_slice(&e.put32(ifd.next));
    timings.patch_ifd_ms = crate::io::ms(start);
    timings.total_ms = crate::io::ms(total);
    Ok(BenchTransport {
        bytes: output,
        base,
        layout,
        timings,
    })
}

#[cfg(test)]
mod bench_tests {
    use super::*;

    pub(super) fn source(e: Endian) -> Vec<u8> {
        let tile = include_bytes!("../../assets/tests/dng-jxl-tile.jxl");
        let tags = [
            (254, 4, 1, 0),
            (256, 4, 1, 17),
            (257, 4, 1, 19),
            (258, 3, 3, 400),
            (259, 3, 1, 52546),
            (262, 3, 1, 34892),
            (277, 3, 1, 3),
            (284, 3, 1, 1),
            (322, 4, 1, 16),
            (323, 4, 1, 16),
            (324, 4, 4, 416),
            (325, 4, 4, 432),
            (50706, 1, 4, e.u32(&[1, 7, 0, 0])),
        ];
        let mut bytes = vec![0; 512];
        bytes[..4].copy_from_slice(if e.0 { b"II*\0" } else { b"MM\0*" });
        bytes[4..8].copy_from_slice(&e.put32(8));
        bytes[8..10].copy_from_slice(&e.put16(tags.len() as u16));
        for (index, (tag, kind, count, value)) in tags.into_iter().enumerate() {
            let entry = &mut bytes[10 + index * 12..22 + index * 12];
            entry[..2].copy_from_slice(&e.put16(tag));
            entry[2..4].copy_from_slice(&e.put16(kind));
            entry[4..8].copy_from_slice(&e.put32(count));
            if kind == 3 && count == 1 {
                entry[8..10].copy_from_slice(&e.put16(value as u16));
            } else {
                entry[8..12].copy_from_slice(&e.put32(value));
            }
        }
        for channel in 0..3 {
            bytes[400 + channel * 2..402 + channel * 2].copy_from_slice(&e.put16(16));
        }
        for index in 0..4 {
            bytes[416 + index * 4..420 + index * 4]
                .copy_from_slice(&e.put32((512 + index * tile.len()) as u32));
            bytes[432 + index * 4..436 + index * 4].copy_from_slice(&e.put32(tile.len() as u32));
            bytes.extend_from_slice(tile);
        }
        bytes
    }
    #[test]
    fn instrumented_transport_matches_production_including_partial_edges_and_endian() {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(3)
            .build()
            .unwrap();
        for e in [Endian(true), Endian(false)] {
            let bytes = source(e);
            let expected = pool.install(|| unpack(&bytes)).unwrap().unwrap();
            let actual = bench_unpack(&bytes, None, &pool, false, false).unwrap();
            assert_eq!(actual.bytes, expected);
            assert_eq!(actual.timings.workers.tiles, 4);
            assert_eq!(
                bench_tile(&bytes, 3).unwrap().0,
                include_bytes!("../../assets/tests/dng-jxl-tile.jxl")
            );
            assert!(bench_tile(&bytes, 4).is_err());
        }
    }
    #[test]
    fn malformed_tile_extents_are_rejected_before_allocation() {
        let mut bytes = source(Endian(true));
        bytes[416..420].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(bench_tile(&bytes, 0).is_err());
        assert!(bench_tile(b"II*\0\xff\xff\xff\xff", 0).is_err());
    }
    #[test]
    fn native_transport_matches_reference_including_partial_edges_and_endian() {
        let Some(path) = std::env::var_os("DNG_JXL_LIBRARY") else {
            eprintln!("Skipped native transport: DNG_JXL_LIBRARY is not set");
            return;
        };
        let library = crate::native_jxl::JxlLibrary::load(std::path::Path::new(&path)).unwrap();
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(3)
            .build()
            .unwrap();
        for e in [Endian(true), Endian(false)] {
            let bytes = source(e);
            let expected = pool.install(|| unpack(&bytes)).unwrap().unwrap();
            for reuse in [false, true] {
                let actual = bench_unpack(&bytes, Some(&library), &pool, reuse, true).unwrap();
                assert_eq!(actual.bytes, expected);
                assert!(actual.timings.workers.allocator_calls > 0);
            }
        }
    }
}
