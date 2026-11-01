// Included inside the unchanged production DNG parser by the experiment builder.
// This capability probe returns raw samples, not LibRaw-developed display RGB.

#[derive(Clone, Copy)]
struct RegionTile {
    index: usize,
    left: usize,
    top: usize,
    width: usize,
    height: usize,
    tile_left: usize,
    tile_top: usize,
}
fn region_tiles(layout: &BenchLayout, roi: [usize; 4]) -> Result<Vec<RegionTile>> {
    let [x, y, width, height] = roi;
    let right = x.checked_add(width).context("ROI overflow")?;
    let bottom = y.checked_add(height).context("ROI overflow")?;
    ensure!(
        width > 0 && height > 0 && right <= layout.width && bottom <= layout.height,
        "ROI is outside the raw canvas"
    );
    let nx = layout.width.div_ceil(layout.tile_width);
    let mut selected = Vec::new();
    for ty in y / layout.tile_height..bottom.div_ceil(layout.tile_height) {
        for tx in x / layout.tile_width..right.div_ceil(layout.tile_width) {
            let tile_left = tx * layout.tile_width;
            let tile_top = ty * layout.tile_height;
            let left = x.max(tile_left);
            let top = y.max(tile_top);
            selected.push(RegionTile {
                index: ty * nx + tx,
                left,
                top,
                width: right.min(tile_left + layout.tile_width) - left,
                height: bottom.min(tile_top + layout.tile_height) - top,
                tile_left,
                tile_top,
            });
        }
    }
    Ok(selected)
}

pub struct RegionSamples {
    pub pixels: Vec<u16>,
    pub layout: BenchLayout,
    pub roi: [usize; 4],
    pub report: serde_json::Value,
}

pub fn decode_region(
    bytes: &[u8],
    roi: [usize; 4],
    native: Option<&crate::native_jxl::JxlLibrary>,
    pool: &rayon::ThreadPool,
    crop_inside_tile: bool,
) -> Result<RegionSamples> {
    use std::time::Instant;
    let start = Instant::now();
    let tiff = Tiff::parse(bytes)?.context("Not a classic TIFF")?;
    let ifd = tiff.jxl_raw()?.context("No JXL raw IFD")?;
    let layout = bench_layout(&tiff, ifd)?;
    let selected = region_tiles(&layout, roi)?;
    let offsets = tiff.values(ifd, 324)?;
    let lengths = tiff.values(ifd, 325)?;
    let addressed_compressed_bytes: usize = selected
        .iter()
        .map(|tile| lengths[tile.index] as usize)
        .sum();
    let parse_and_plan_ms = crate::io::ms(start);
    let timer = Instant::now();
    let mut pixels = vec![0u16; roi[2] * roi[3] * layout.channels];
    let output_alloc_ms = crate::io::ms(timer);
    let timer = Instant::now();
    let pieces = pool.install(|| {
        selected
            .par_iter()
            .map(|&tile| -> Result<(RegionTile, Vec<u16>)> {
                let compressed = region(
                    bytes,
                    offsets[tile.index] as usize,
                    lengths[tile.index] as usize,
                )?;
                let mut worker = native
                    .map(|lib| crate::native_jxl::JxlWorker::new(lib, false, false))
                    .transpose()?;
                let mut owned = Vec::new();
                let (samples, iw, ih, sx, sy) = if let Some(worker) = worker.as_mut() {
                    let (samples, [iw, ih], _) = worker.decode(
                        compressed,
                        layout.channels,
                        layout.tile_width,
                        layout.tile_height,
                    )?;
                    (
                        samples,
                        iw,
                        ih,
                        tile.left - tile.tile_left,
                        tile.top - tile.tile_top,
                    )
                } else {
                    let mut image = jxl_oxide::JxlImage::builder()
                        .pool(jxl_oxide::JxlThreadPool::none())
                        .alloc_tracker(jxl_oxide::AllocTracker::with_limit(128 * 1024 * 1024))
                        .read(compressed)
                        .map_err(|err| anyhow::anyhow!("Tile {}: {err}", tile.index))?;
                    ensure!(
                        image.image_header().metadata.orientation == 1,
                        "Experimental sub-tile ROI requires stored tile orientation 1"
                    );
                    if crop_inside_tile {
                        image.set_image_region(jxl_oxide::CropInfo {
                            left: (tile.left - tile.tile_left) as u32,
                            top: (tile.top - tile.tile_top) as u32,
                            width: tile.width as u32,
                            height: tile.height as u32,
                        });
                    }
                    let render = image
                        .render_frame(0)
                        .map_err(|err| anyhow::anyhow!("Tile {}: {err}", tile.index))?;
                    let mut stream = render.stream_no_alpha();
                    let (iw, ih) = (stream.width() as usize, stream.height() as usize);
                    ensure!(
                        stream.channels() as usize == layout.channels
                            && iw <= layout.tile_width
                            && ih <= layout.tile_height,
                        "Unexpected tile output"
                    );
                    owned.resize(iw * ih * layout.channels, 0);
                    ensure!(
                        stream.write_to_buffer(&mut owned) == owned.len(),
                        "Incomplete tile samples"
                    );
                    let (sx, sy) = if crop_inside_tile {
                        (0, 0)
                    } else {
                        (tile.left - tile.tile_left, tile.top - tile.tile_top)
                    };
                    (owned.as_slice(), iw, ih, sx, sy)
                };
                ensure!(
                    sx + tile.width <= iw && sy + tile.height <= ih,
                    "ROI outside decoded tile"
                );
                let mut piece = Vec::with_capacity(tile.width * tile.height * layout.channels);
                for row in 0..tile.height {
                    let start = ((sy + row) * iw + sx) * layout.channels;
                    piece.extend_from_slice(&samples[start..start + tile.width * layout.channels]);
                }
                Ok((tile, piece))
            })
            .collect::<Result<Vec<_>>>()
    })?;
    let selected_tiles_ms = crate::io::ms(timer);
    let timer = Instant::now();
    for (tile, piece) in pieces {
        for row in 0..tile.height {
            let dst = (((tile.top - roi[1]) + row) * roi[2] + tile.left - roi[0]) * layout.channels;
            let src = row * tile.width * layout.channels;
            pixels[dst..dst + tile.width * layout.channels]
                .copy_from_slice(&piece[src..src + tile.width * layout.channels]);
        }
    }
    let assembly_ms = crate::io::ms(timer);
    let report = serde_json::json!({"roi_raw_coordinates": roi, "selected_tiles": selected.len(), "total_tiles": layout.tiles,
        "addressed_compressed_bytes": addressed_compressed_bytes, "all_compressed_tile_bytes": layout.compressed_tile_bytes,
        "output_sample_bytes": pixels.len() * 2, "crop_inside_oxide_tile": crop_inside_tile,
        "parse_and_plan_ms": parse_and_plan_ms, "output_alloc_ms": output_alloc_ms,
        "selected_tiles_ms": selected_tiles_ms, "assembly_ms": assembly_ms, "total_ms": crate::io::ms(start),
        "domain": "unoriented raw sample canvas; no LibRaw WB/color/opcodes/DefaultCrop development",
        "memory_method": "bounded parallel tile decodes; selected cropped pieces collected then assembled; not an optimized production cache"});
    Ok(RegionSamples {
        pixels,
        layout,
        roi,
        report,
    })
}

pub fn embedded_jpeg_payload(bytes: &[u8]) -> Result<&[u8]> {
    let tiff = Tiff::parse(bytes)?.context("Not a classic TIFF")?;
    ensure!(tiff.jxl_raw()?.is_some(), "Expected a JXL DNG");
    for ifd in &tiff.ifds {
        if tiff.value(ifd, 259, 0)? != 7 {
            continue;
        }
        let offsets = tiff.values(ifd, 273)?;
        let lengths = tiff.values(ifd, 279)?;
        if offsets.len() != 1 || lengths.len() != 1 {
            continue;
        }
        let jpeg = region(bytes, offsets[0] as usize, lengths[0] as usize)?;
        if jpeg.starts_with(&[0xff, 0xd8]) {
            return Ok(jpeg);
        }
    }
    bail!("No embedded single-strip JPEG preview")
}

#[cfg(test)]
mod region_tests {
    use super::*;
    #[test]
    fn tile_plan_clips_edges_and_rejects_invalid_rectangles() {
        let layout = BenchLayout {
            width: 50,
            height: 45,
            tile_width: 16,
            tile_height: 16,
            channels: 3,
            tiles: 12,
            compressed_tile_bytes: 0,
            sample_bytes: 50 * 45 * 6,
            little_endian: true,
        };
        let plan = region_tiles(&layout, [15, 15, 18, 18]).unwrap();
        assert_eq!(plan.len(), 9);
        assert_eq!(
            plan.iter()
                .map(|tile| tile.width * tile.height)
                .sum::<usize>(),
            18 * 18
        );
        assert!(region_tiles(&layout, [0, 0, 0, 1]).is_err());
        assert!(region_tiles(&layout, [usize::MAX, 0, 10, 1]).is_err());
        assert!(region_tiles(&layout, [49, 44, 2, 2]).is_err());
    }
    #[test]
    fn partial_tile_roi_matches_full_raw_samples() {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(3)
            .build()
            .unwrap();
        for endian in [Endian(true), Endian(false)] {
            let bytes = super::bench_tests::source(endian);
            let full = pool.install(|| unpack(&bytes)).unwrap().unwrap();
            let base = bytes.len().next_multiple_of(4);
            for roi in [[0, 0, 17, 19], [13, 11, 4, 8], [16, 18, 1, 1]] {
                for crop in [false, true] {
                    let region = decode_region(&bytes, roi, None, &pool, crop).unwrap();
                    for y in 0..roi[3] {
                        for x in 0..roi[2] {
                            for c in 0..3 {
                                let src = base + (((roi[1] + y) * 17 + roi[0] + x) * 3 + c) * 2;
                                assert_eq!(
                                    region.pixels[(y * roi[2] + x) * 3 + c],
                                    endian.u16(&full[src..src + 2])
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}
