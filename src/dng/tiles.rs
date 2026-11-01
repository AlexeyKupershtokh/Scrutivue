//! Worker-owned, bounded LinearRaw RGB16 tile source. No source writes.
use super::*;
use crate::{
    decode::{self, Frame},
    jxl,
};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
    time::SystemTime,
};
mod completion;

pub struct Tile {
    pub raw: Vec<u16>,
    pub raw_size: [u32; 2],
    pub display: Arc<Frame>,
    pub ratio: u32,
}
#[derive(serde::Serialize)]
pub struct FinishTimings {
    pub assembly_ms: f64,
    pub development_ms: f64,
    pub path: &'static str,
    pub fallback_reason: Option<&'static str>,
}
pub struct Source {
    path: PathBuf,
    modified: Option<SystemTime>,
    created: Option<SystemTime>,
    length: u64,
    map: memmap2::Mmap,
    pub raw_size: [u32; 2],
    pub size: [u32; 2],
    pub tile_size: [u32; 2],
    pub crop: [u32; 4],
    orientation: u32,
    pub progressive_hint: bool,
    offsets: Vec<u32>,
    lengths: Vec<u32>,
    metadata: Vec<([u8; 12], Vec<u8>)>,
    pointwise_white: OnceLock<Option<u16>>,
    pub tiles: Vec<Option<Tile>>,
}
impl Source {
    pub fn open(path: &Path) -> Result<Self> {
        let file = std::fs::File::open(path)?;
        let stat = file.metadata()?;
        ensure!(
            stat.len() > 0 && stat.len() <= 2 * 1024 * 1024 * 1024,
            "DNG streaming input exceeds 2 GiB"
        );
        let map = unsafe { memmap2::MmapOptions::new().map(&file)? };
        let tiff = Tiff::parse(&map)?.context("Not a classic TIFF DNG")?;
        let ifd = tiff.jxl_raw()?.context("Not a tiled JXL DNG")?;
        ensure!(
            tiff.value(ifd, 262, 0)? == 34892
                && tiff.value(ifd, 277, 0)? == 3
                && tiff.values(ifd, 258)? == [16, 16, 16]
                && tiff.value(ifd, 284, 1)? == 1
                && tiff.value(ifd, 339, 1)? == 1,
            "Streaming requires chunky LinearRaw RGB16"
        );
        // Spatial opcodes, patterned black levels and linearization need a
        // separately validated region pipeline. Never silently ignore them.
        ensure!(
            ifd.entries.iter().all(|entry| !matches!(
                tiff.endian.u16(entry),
                50712 | 50715 | 50716 | 51008 | 51009 | 51022
            )),
            "DNG spatial processing requires full development"
        );
        ensure!(
            tiff.values(ifd, 50713)?.is_empty() || tiff.values(ifd, 50713)? == [1, 1],
            "Patterned DNG black level requires full development"
        );
        let raw_size = [tiff.value(ifd, 256, 0)?, tiff.value(ifd, 257, 0)?];
        let tile_size = [tiff.value(ifd, 322, 0)?, tiff.value(ifd, 323, 0)?];
        ensure!(
            raw_size.iter().all(|&n| n > 0 && n <= 100000)
                && tile_size.iter().all(|&n| n > 0 && n <= 4096)
                && raw_size[0] as u64 * raw_size[1] as u64 * 12 <= 768 * 1024 * 1024,
            "DNG streaming memory/dimension limit"
        );
        let offsets = tiff.values(ifd, 324)?;
        let lengths = tiff.values(ifd, 325)?;
        let count = raw_size[0].div_ceil(tile_size[0]) as usize
            * raw_size[1].div_ceil(tile_size[1]) as usize;
        ensure!(
            offsets.len() == count && lengths.len() == count,
            "Invalid DNG tile table"
        );
        for (&offset, &length) in offsets.iter().zip(&lengths) {
            ensure!(
                length > 0 && length <= 128 * 1024 * 1024,
                "DNG tile payload exceeds 128 MiB"
            );
            region(&map, offset as usize, length as usize)?;
        }
        let header = jxl_oxide::JxlImage::builder()
            .pool(jxl_oxide::JxlThreadPool::none())
            .alloc_tracker(jxl_oxide::AllocTracker::with_limit(512 * 1024 * 1024))
            .read(region(&map, offsets[0] as usize, lengths[0] as usize)?)
            .map_err(|e| anyhow::anyhow!("JXL header: {e}"))?;
        // A scheduling hint only: every decoded tile still validates its own
        // events/layout. Mixed encodings may simply miss an optional preview.
        let progressive_hint = header
            .frame_header(0)
            .is_some_and(|h| h.passes.num_passes > 1);
        let crop =
            default_crop(&tiff, ifd, raw_size)?.unwrap_or(active_area(&tiff, ifd, raw_size)?);
        let mut orientation = 1;
        for entry in &tiff.ifds {
            let value = tiff.value(entry, 274, 1)?;
            ensure!((1..=8).contains(&value), "Invalid orientation");
            if value != 1 {
                orientation = value;
                break;
            }
        }
        let size = if orientation >= 5 {
            [crop[3], crop[2]]
        } else {
            [crop[2], crop[3]]
        };
        let mut metadata = Vec::new();
        let mut metadata_bytes = 0usize;
        for &entry in &ifd.entries {
            let tag = tiff.endian.u16(&entry);
            if matches!(tag, 254 | 256 | 257 | 259 | 273 | 274 | 278 | 279 | 317 | 322..=325 | 330 | 34665 | 34853 | 50718..=50720 | 50781 | 50829 | 50972)
            {
                continue;
            }
            let unit = match tiff.endian.u16(&entry[2..]) {
                1 | 2 | 6 | 7 => 1,
                3 | 8 => 2,
                4 | 9 | 11 | 13 => 4,
                5 | 10 | 12 => 8,
                _ => bail!("Unsupported DNG field type"),
            };
            let length = (tiff.endian.u32(&entry[4..]) as usize)
                .checked_mul(unit)
                .context("DNG field overflow")?;
            ensure!(length <= 16 * 1024 * 1024, "DNG metadata field too large");
            metadata_bytes = metadata_bytes
                .checked_add(length)
                .context("DNG metadata overflow")?;
            ensure!(
                metadata_bytes <= 16 * 1024 * 1024,
                "DNG metadata exceeds 16 MiB"
            );
            let payload = if length <= 4 {
                entry[8..8 + length].to_vec()
            } else {
                region(&map, tiff.endian.u32(&entry[8..]) as usize, length)?.to_vec()
            };
            metadata.push((entry, payload));
        }
        Ok(Self {
            path: path.to_owned(),
            modified: stat.modified().ok(),
            created: stat.created().ok(),
            length: stat.len(),
            map,
            raw_size,
            size,
            tile_size,
            crop,
            orientation,
            progressive_hint,
            offsets,
            lengths,
            metadata,
            pointwise_white: OnceLock::new(),
            tiles: (0..count).map(|_| None).collect(),
        })
    }
    pub fn unchanged(&self) -> bool {
        self.path.metadata().is_ok_and(|s| {
            s.len() == self.length
                && s.modified().ok() == self.modified
                && s.created().ok() == self.created
        })
    }
    pub fn matches(&self, path: &Path) -> bool {
        self.path == path && self.modified.is_some() && self.unchanged()
    }
    pub fn origin(&self, index: usize) -> [u32; 2] {
        let nx = self.raw_size[0].div_ceil(self.tile_size[0]);
        [
            index as u32 % nx * self.tile_size[0],
            index as u32 / nx * self.tile_size[1],
        ]
    }
    /// Display pixel -> uncropped sensor pixel, including mirrored EXIF cases.
    pub fn raw_point(&self, x: u32, y: u32) -> [u32; 2] {
        let [_, _, w, h] = self.crop;
        let [x, y] = match self.orientation {
            2 => [w - 1 - x, y],
            3 => [w - 1 - x, h - 1 - y],
            4 => [x, h - 1 - y],
            5 => [y, x],
            6 => [y, h - 1 - x],
            7 => [w - 1 - y, h - 1 - x],
            8 => [w - 1 - y, x],
            _ => [x, y],
        };
        [x + self.crop[0], y + self.crop[1]]
    }
    pub fn index_at(&self, p: [u32; 2]) -> usize {
        (p[1] / self.tile_size[1] * self.raw_size[0].div_ceil(self.tile_size[0])
            + p[0] / self.tile_size[0]) as usize
    }
    /// Oriented/cropped display rectangle of a stored tile. Cropped-away tiles
    /// have no display footprint, irrespective of their compressed tile index.
    pub fn display_bounds(&self, index: usize) -> Option<[u32; 4]> {
        let p = self.origin(index);
        let lo = [p[0].max(self.crop[0]), p[1].max(self.crop[1])];
        let hi = std::array::from_fn::<_, 2, _>(|a| {
            (p[a] + self.tile_size[a])
                .min(self.raw_size[a])
                .min(self.crop[a] + self.crop[a + 2])
        });
        if (0..2).any(|a| lo[a] >= hi[a]) {
            return None;
        }
        let points: [[u32; 2]; 4] = std::array::from_fn(|i| {
            let x = if i & 1 == 0 { lo[0] } else { hi[0] - 1 } - self.crop[0];
            let y = if i & 2 == 0 { lo[1] } else { hi[1] - 1 } - self.crop[1];
            let [_, _, w, h] = self.crop;
            match self.orientation {
                2 => [w - 1 - x, y],
                3 => [w - 1 - x, h - 1 - y],
                4 => [x, h - 1 - y],
                5 => [y, x],
                6 => [h - 1 - y, x],
                7 => [h - 1 - y, w - 1 - x],
                8 => [y, w - 1 - x],
                _ => [x, y],
            }
        });
        let left = points.iter().map(|p| p[0]).min()?;
        let top = points.iter().map(|p| p[1]).min()?;
        Some([
            left,
            top,
            points.iter().map(|p| p[0]).max()? - left + 1,
            points.iter().map(|p| p[1]).max()? - top + 1,
        ])
    }
    pub fn payload(&self, index: usize) -> Result<&[u8]> {
        region(
            &self.map,
            self.offsets[index] as usize,
            self.lengths[index] as usize,
        )
    }
    pub fn session(&self, index: usize, library: Arc<jxl::Library>) -> Result<jxl::Session> {
        jxl::Session::new(library, self.payload(index)?.to_vec(), self.tile_size)
    }
    pub fn decode_oxide(&self, index: usize) -> Result<(Vec<u16>, [u32; 2])> {
        let image = jxl_oxide::JxlImage::builder()
            .pool(jxl_oxide::JxlThreadPool::none())
            .alloc_tracker(jxl_oxide::AllocTracker::with_limit(512 * 1024 * 1024))
            .read(self.payload(index)?)
            .map_err(|e| anyhow::anyhow!("JXL tile: {e}"))?;
        ensure!(
            image.image_header().metadata.orientation == 1,
            "Oriented JXL tile is unsupported"
        );
        let render = image
            .render_frame(0)
            .map_err(|e| anyhow::anyhow!("JXL tile: {e}"))?;
        let mut stream = render.stream_no_alpha();
        let size = [stream.width(), stream.height()];
        ensure!(
            stream.channels() == 3 && size[0] <= self.tile_size[0] && size[1] <= self.tile_size[1],
            "Invalid JXL tile output"
        );
        let mut pixels = vec![0; size[0] as usize * size[1] as usize * 3];
        ensure!(
            stream.write_to_buffer(&mut pixels) == pixels.len(),
            "Incomplete JXL output"
        );
        Ok((pixels, size))
    }
    pub fn decode_display(
        &self,
        index: usize,
        library: Option<&Arc<jxl::Library>>,
        cancelled: &(dyn Fn() -> bool + Sync),
    ) -> Result<Tile> {
        ensure!(!cancelled(), "Image decode cancelled");
        let (raw, raw_size) = if let Some(library) = library {
            let mut session = self.session(index, library.clone())?;
            session.advance(1, &|| cancelled())?;
            (std::mem::take(&mut session.pixels), session.size)
        } else {
            self.decode_oxide(index)?
        };
        ensure!(!cancelled(), "Image decode cancelled");
        let display = self.develop(&raw, raw_size, 1, cancelled)?;
        Ok(Tile {
            raw,
            raw_size,
            display,
            ratio: 1,
        })
    }
    /// Compact, standalone development input; offset-bearing TIFF directories
    /// are rebuilt, not blindly copied. Calibration payloads remain byte-exact.
    fn compact(&self, rgb: &[u16], size: [u32; 2]) -> Result<Vec<u8>> {
        let e = Endian(self.map[0] == b'I');
        ensure!(
            rgb.len() == size[0] as usize * size[1] as usize * 3,
            "Invalid region samples"
        );
        let mut entries = self.metadata.clone();
        for (tag, kind, values) in [
            (254, 4, vec![0]),
            (256, 4, vec![size[0]]),
            (257, 4, vec![size[1]]),
            (259, 3, vec![1]),
            (274, 3, vec![1]),
            (273, 4, vec![0]),
            (278, 4, vec![size[1]]),
            (279, 4, vec![(rgb.len() * 2) as u32]),
            (50719, 4, vec![0, 0]),
            (50720, 4, size.to_vec()),
            (50829, 4, vec![0, 0, size[1], size[0]]),
        ] {
            let mut entry = [0; 12];
            entry[..2].copy_from_slice(&e.put16(tag));
            entry[2..4].copy_from_slice(&e.put16(kind));
            entry[4..8].copy_from_slice(&e.put32(values.len() as u32));
            let payload = values
                .into_iter()
                .flat_map(|v| {
                    if kind == 3 {
                        e.put16(v as u16).to_vec()
                    } else {
                        e.put32(v).to_vec()
                    }
                })
                .collect();
            entries.push((entry, payload));
        }
        entries.sort_by_key(|(entry, _)| e.u16(entry));
        let mut out = vec![0; 8 + 2 + entries.len() * 12 + 4];
        out[..4].copy_from_slice(if e.0 { b"II*\0" } else { b"MM\0*" });
        out[4..8].copy_from_slice(&e.put32(8));
        out[8..10].copy_from_slice(&e.put16(entries.len() as u16));
        let mut strip_entry = 0;
        for (i, (mut entry, payload)) in entries.into_iter().enumerate() {
            if e.u16(&entry) == 273 {
                strip_entry = 10 + i * 12;
            }
            entry[8..].fill(0);
            if payload.len() <= 4 {
                entry[8..8 + payload.len()].copy_from_slice(&payload);
            } else {
                out.resize(out.len().next_multiple_of(4), 0);
                entry[8..12].copy_from_slice(&e.put32(out.len() as u32));
                out.extend_from_slice(&payload);
            }
            out[10 + i * 12..22 + i * 12].copy_from_slice(&entry);
        }
        out.resize(out.len().next_multiple_of(4), 0);
        let offset = out.len() as u32;
        out[strip_entry + 8..strip_entry + 12].copy_from_slice(&e.put32(offset));
        for &v in rgb {
            out.extend_from_slice(&e.put16(v));
        }
        Ok(out)
    }
    pub fn develop(
        &self,
        raw: &[u16],
        size: [u32; 2],
        ratio: u32,
        cancelled: &(dyn Fn() -> bool + Sync),
    ) -> Result<Arc<Frame>> {
        let target = [size[0].div_ceil(ratio), size[1].div_ceil(ratio)];
        // LibRaw rejects very small raw dimensions. Edge-replicate only this
        // bounded local LinearRaw input, then trim; never upscale source samples.
        let development = target.map(|v| v.max(32));
        let pixels = if ratio == 1 && development == size {
            None
        } else {
            Some(
                (0..development[1])
                    .flat_map(|y| {
                        (0..development[0]).flat_map(move |x| {
                            let p = ((y * ratio).min(size[1] - 1) * size[0]
                                + (x * ratio).min(size[0] - 1))
                                as usize
                                * 3;
                            raw[p..p + 3].iter().copied()
                        })
                    })
                    .collect::<Vec<_>>(),
            )
        };
        let frame = decode::develop_dng_region_bytes(
            &self.compact(pixels.as_deref().unwrap_or(raw), development)?,
            cancelled,
        )?;
        if development == target {
            return Ok(frame);
        }
        let mut rgb = Vec::with_capacity(target[0] as usize * target[1] as usize * 3);
        for y in 0..target[1] {
            let p = (y * frame.width * 3) as usize;
            rgb.extend_from_slice(&frame.rgb[p..p + target[0] as usize * 3]);
        }
        Ok(Arc::new(Frame {
            width: target[0],
            height: target[1],
            rgb,
        }))
    }
    fn assemble(&self, cancelled: &(dyn Fn() -> bool + Sync)) -> Result<Vec<u8>> {
        ensure!(!cancelled(), "Image decode cancelled");
        ensure!(
            self.tiles
                .iter()
                .all(|t| t.as_ref().is_some_and(|t| t.ratio == 1)),
            "DNG tiles are incomplete"
        );
        ensure!(self.unchanged(), "Source changed while loading DNG tiles");
        // Use original metadata for authoritative full processing, not compact
        // regional development. All entropy output comes from the tile cache.
        let tiff = Tiff::parse(&self.map)?.unwrap();
        let ifd = tiff.jxl_raw()?.unwrap();
        let e = tiff.endian;
        let base = self.map.len().next_multiple_of(4);
        let pixel_bytes = self.raw_size[0] as usize * self.raw_size[1] as usize * 6;
        ensure!(
            base + pixel_bytes + 65536 < u32::MAX as usize,
            "DNG exceeds classic TIFF bounds"
        );
        let mut out = self.map.to_vec();
        out.resize(base + pixel_bytes, 0);
        for (i, tile) in self.tiles.iter().enumerate() {
            let tile = tile.as_ref().unwrap();
            let [x, y] = self.origin(i);
            let w = self.tile_size[0].min(self.raw_size[0] - x);
            let h = self.tile_size[1].min(self.raw_size[1] - y);
            ensure!(
                tile.raw_size[0] >= w
                    && tile.raw_size[1] >= h
                    && tile.raw.len() == tile.raw_size[0] as usize * tile.raw_size[1] as usize * 3,
                "Truncated edge tile"
            );
        }
        let nx = self.raw_size[0].div_ceil(self.tile_size[0]) as usize;
        // Rows are disjoint. On matching-endian TIFFs copy complete sample spans
        // instead of serially re-encoding every channel of every 45 MP pixel.
        out[base..]
            .par_chunks_mut(self.raw_size[0] as usize * 6)
            .enumerate()
            .try_for_each(|(y, row)| -> Result<()> {
                ensure!(!cancelled(), "Image decode cancelled");
                let ty = y / self.tile_size[1] as usize;
                let local_y = y % self.tile_size[1] as usize;
                for tx in 0..nx {
                    let tile = self.tiles[ty * nx + tx].as_ref().unwrap();
                    let x = tx * self.tile_size[0] as usize;
                    let width = (self.tile_size[0] as usize).min(self.raw_size[0] as usize - x);
                    let from = local_y * tile.raw_size[0] as usize * 3;
                    let samples = &tile.raw[from..from + width * 3];
                    let target = &mut row[x * 6..x * 6 + width * 6];
                    if e.0 == cfg!(target_endian = "little") {
                        target.copy_from_slice(bytemuck::cast_slice(samples));
                    } else {
                        for (bytes, &value) in target.chunks_exact_mut(2).zip(samples) {
                            bytes.copy_from_slice(&e.put16(value));
                        }
                    }
                }
                Ok(())
            })?;
        let mut entries: Vec<_> = ifd
            .entries
            .iter()
            .filter(|entry| !matches!(e.u16(*entry), 259 | 273 | 278 | 279 | 317 | 322..=325))
            .copied()
            .collect();
        for (tag, kind, value) in [
            (259, 3, 1),
            (273, 4, base as u32),
            (278, 4, self.raw_size[1]),
            (279, 4, pixel_bytes as u32),
        ] {
            let mut entry = [0; 12];
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
            "DNG replacement IFD overflow"
        );
        out[ifd.offset..ifd.offset + 2].copy_from_slice(&e.put16(entries.len() as u16));
        for (i, entry) in entries.iter().enumerate() {
            out[ifd.offset + 2 + i * 12..ifd.offset + 14 + i * 12].copy_from_slice(entry);
        }
        let next = ifd.offset + 2 + entries.len() * 12;
        out[next..next + 4].copy_from_slice(&e.put32(ifd.next));
        Ok(out)
    }
    /// Match streamed completion and ordinary full decode to the SAME native
    /// reconstruction/rounding policy for supported LinearRaw JXL sources.
    pub fn decode_full_native(
        self,
        library: Arc<jxl::Library>,
        cancelled: &(dyn Fn() -> bool + Sync),
    ) -> Result<Arc<Frame>> {
        self.decode_native(library, false, cancelled)
    }
    pub fn decode_full_native_reference(
        self,
        library: Arc<jxl::Library>,
        cancelled: &(dyn Fn() -> bool + Sync),
    ) -> Result<Arc<Frame>> {
        self.decode_native(library, true, cancelled)
    }
    fn decode_native(
        mut self,
        library: Arc<jxl::Library>,
        reference: bool,
        cancelled: &(dyn Fn() -> bool + Sync),
    ) -> Result<Arc<Frame>> {
        let placeholder = Arc::new(Frame {
            width: 0,
            height: 0,
            rgb: Vec::new(),
        });
        let mut tiles = std::mem::take(&mut self.tiles);
        let develop_tiles = !reference && self.completion_white().is_some();
        tiles
            .par_iter_mut()
            .enumerate()
            .try_for_each(|(index, slot)| -> Result<()> {
                let mut session = self.session(index, library.clone())?;
                session.advance(1, &|| cancelled())?;
                let display = if develop_tiles {
                    self.develop(&session.pixels, session.size, 1, cancelled)?
                } else {
                    placeholder.clone()
                };
                *slot = Some(Tile {
                    raw: std::mem::take(&mut session.pixels),
                    raw_size: session.size,
                    display,
                    ratio: 1,
                });
                Ok(())
            })?;
        self.tiles = tiles;
        if reference {
            self.finish_reference(cancelled)
        } else {
            self.finish(cancelled)
        }
    }
}

#[cfg(test)]
pub(crate) fn fixture(name: &str, w: u32, h: u32) -> std::path::PathBuf {
    let directory = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/tile-tests")
        .join(format!("{}-{name}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("source.dng");
    std::fs::write(
        &path,
        super::tests::synthetic_size(Endian(true), false, w, h),
    )
    .unwrap();
    path
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cancelled_display_decode_stops_before_accessing_a_tile() {
        let path = fixture("cancel-before-tile", 64, 48);
        let source = Source::open(&path).unwrap();
        let error = source
            .decode_display(usize::MAX, None, &|| true)
            .err()
            .unwrap();
        assert_eq!(error.to_string(), "Image decode cancelled");
    }
    #[test]
    fn source_identity_requires_the_same_path_and_revision() {
        let path = fixture("revision-check", 64, 48);
        let other = fixture("revision-check-other", 64, 48);
        let source = Source::open(&path).unwrap();
        assert!(source.matches(&path));
        assert!(!source.matches(&other));
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(SystemTime::now() + std::time::Duration::from_secs(2))
            .unwrap();
        assert!(!source.matches(&path));
    }
    #[test]
    fn assembly_copies_exact_samples_for_both_endiannesses_and_clipped_edges() {
        for little in [true, false] {
            let e = Endian(little);
            let path = fixture(&format!("assembly-{little}"), 65, 49);
            std::fs::write(&path, super::super::tests::synthetic_size(e, false, 65, 49)).unwrap();
            let mut source = Source::open(&path).unwrap();
            let placeholder = Arc::new(Frame {
                width: 0,
                height: 0,
                rgb: Vec::new(),
            });
            let sample = |x: u32, y: u32, c: u32| ((x * 503 + y * 997 + c * 11027) % 65536) as u16;
            for index in 0..source.tiles.len() {
                let [x, y] = source.origin(index);
                // Alternate genuinely clipped edge outputs and padded outputs.
                let size = if index % 2 == 0 {
                    [16, 16]
                } else {
                    [16.min(65 - x), 16.min(49 - y)]
                };
                let raw = (0..size[1])
                    .flat_map(|dy| {
                        (0..size[0])
                            .flat_map(move |dx| (0..3).map(move |c| sample(x + dx, y + dy, c)))
                    })
                    .collect();
                source.tiles[index] = Some(Tile {
                    raw,
                    raw_size: size,
                    display: placeholder.clone(),
                    ratio: 1,
                });
            }
            let out = source.assemble(&|| false).unwrap();
            let tiff = Tiff::parse(&out).unwrap().unwrap();
            let ifd = &tiff.ifds[0];
            let base = tiff.value(ifd, 273, 0).unwrap() as usize;
            assert_eq!(tiff.value(ifd, 259, 0).unwrap(), 1);
            assert_eq!(tiff.value(ifd, 279, 0).unwrap(), 65 * 49 * 6);
            for y in 0..49 {
                for x in 0..65 {
                    for c in 0..3 {
                        let p = base + ((y * 65 + x) * 3 + c) as usize * 2;
                        assert_eq!(e.u16(&out[p..p + 2]), sample(x, y, c));
                    }
                }
            }
            let visits = std::sync::atomic::AtomicUsize::new(0);
            assert!(source
                .assemble(&|| visits.fetch_add(1, std::sync::atomic::Ordering::Relaxed) > 3)
                .is_err());
            source.tiles.last_mut().unwrap().as_mut().unwrap().raw.pop();
            assert!(source.assemble(&|| false).is_err());
        }
    }
    #[test]
    fn regional_white_level_is_independent_of_tile_content() {
        let path = fixture("regional-exposure", 64, 64);
        let mut source = Source::open(&path).unwrap();
        let e = Endian(true);
        // Explicit, identical metadata avoids automatic-WB fallback in this
        // exposure test. Only the brightest sample differs between the regions.
        for (tag, kind, count, payload) in [
            (50717, 4, 1, e.put32(65535).to_vec()),
            (50728, 5, 3, [e.put32(1); 6].concat()),
        ] {
            let mut entry = [0; 12];
            entry[..2].copy_from_slice(&e.put16(tag));
            entry[2..4].copy_from_slice(&e.put16(kind));
            entry[4..8].copy_from_slice(&e.put32(count));
            source.metadata.push((entry, payload));
        }
        let mut a = vec![15000; 64 * 64 * 3];
        let mut b = a.clone();
        a[64 * 64 * 3 - 3..].fill(60000);
        b[64 * 64 * 3 - 3..].fill(65535);
        let bytes_a = source.compact(&a, [64, 64]).unwrap();
        let bytes_b = source.compact(&b, [64, 64]).unwrap();
        let old_a = decode::develop_dng_bytes(&bytes_a, &|| false).unwrap();
        let old_b = decode::develop_dng_bytes(&bytes_b, &|| false).unwrap();
        assert_ne!(
            &old_a.rgb[..3],
            &old_b.rgb[..3],
            "The fixture must reproduce local maximum normalization"
        );
        let new_a = source.develop(&a, [64, 64], 1, &|| false).unwrap();
        let new_b = source.develop(&b, [64, 64], 1, &|| false).unwrap();
        assert_eq!(&new_a.rgb[..3], &new_b.rgb[..3]);
        assert_eq!(&new_b.rgb[..3], &old_b.rgb[..3]);
        let coarse_a = source.develop(&a, [64, 64], 8, &|| false).unwrap();
        let coarse_b = source.develop(&b, [64, 64], 8, &|| false).unwrap();
        assert_eq!(&coarse_a.rgb[..3], &coarse_b.rgb[..3]);
        assert_eq!(&new_a.rgb[..3], &coarse_a.rgb[..3]);
        assert!(source.develop(&a, [64, 64], 1, &|| true).is_err());
    }
    #[test]
    #[ignore = "read-only requested corpus; set SCRUTIVUE_TILE_TEST_CORPUS/OUTPUT and SCRUTIVUE_JXL_TEST_LIBRARY"]
    fn requested_halo_tile_exposure_matches_full_development() {
        use sha2::{Digest, Sha256};
        let corpus = PathBuf::from(std::env::var_os("SCRUTIVUE_TILE_TEST_CORPUS").unwrap());
        let output = PathBuf::from(std::env::var_os("SCRUTIVUE_TILE_TEST_OUTPUT").unwrap());
        std::fs::create_dir(&output).unwrap();
        let library = Arc::new(
            jxl::Library::load(Path::new(
                &std::env::var_os("SCRUTIVUE_JXL_TEST_LIBRARY").unwrap(),
            ))
            .unwrap(),
        );
        let mut records = Vec::new();
        for strength in [0, 24, 48, 96, 256] {
            let name = format!("AK8_0036_halo{strength}");
            let path = corpus.join(format!("{name}.dng"));
            let hash = format!("{:x}", Sha256::digest(std::fs::read(&path).unwrap()));
            let source = Source::open(&path).unwrap();
            let index = 349;
            assert_eq!(source.origin(index), [4864, 2560]);
            let mut session = source.session(index, library.clone()).unwrap();
            session.advance(1, &|| false).unwrap();
            let legacy = decode::develop_dng_bytes(
                &source.compact(&session.pixels, session.size).unwrap(),
                &|| false,
            )
            .unwrap();
            let corrected = source
                .develop(&session.pixels, session.size, 1, &|| false)
                .unwrap();
            let full = decode::decode(&path).unwrap();
            let origin = source.origin(index);
            let [_, _, w, h] = source.crop;
            let reference_pixel = |x: u32, y: u32| {
                let x = origin[0] + x - source.crop[0];
                let y = origin[1] + y - source.crop[1];
                let [x, y] = match source.orientation {
                    2 => [w - 1 - x, y],
                    3 => [w - 1 - x, h - 1 - y],
                    4 => [x, h - 1 - y],
                    5 => [y, x],
                    6 => [h - 1 - y, x],
                    7 => [h - 1 - y, w - 1 - x],
                    8 => [y, w - 1 - x],
                    _ => [x, y],
                };
                let p = ((y * full.width + x) * 3) as usize;
                [full.rgb[p], full.rgb[p + 1], full.rgb[p + 2]]
            };
            let mut before_count = 0usize;
            let mut after_count = 0usize;
            let mut before_maximum = 0u16;
            let mut after_maximum = 0u16;
            for y in 0..corrected.height {
                for x in 0..corrected.width {
                    let p = ((y * corrected.width + x) * 3) as usize;
                    for (c, expected) in reference_pixel(x, y).into_iter().enumerate() {
                        let before = legacy.rgb[p + c].abs_diff(expected);
                        let after = corrected.rgb[p + c].abs_diff(expected);
                        before_count += usize::from(before != 0);
                        after_count += usize::from(after != 0);
                        before_maximum = before_maximum.max(before);
                        after_maximum = after_maximum.max(after);
                    }
                }
            }
            let evidence =
                image::RgbImage::from_fn(corrected.width * 3, corrected.height, |x, y| {
                    let panel = x / corrected.width;
                    let x = x % corrected.width;
                    let p = ((y * corrected.width + x) * 3) as usize;
                    let rgb = match panel {
                        0 => [legacy.rgb[p], legacy.rgb[p + 1], legacy.rgb[p + 2]],
                        1 => [corrected.rgb[p], corrected.rgb[p + 1], corrected.rgb[p + 2]],
                        _ => reference_pixel(x, y),
                    };
                    image::Rgb(rgb.map(|v| (v as u32 * 255 / 65535) as u8))
                });
            evidence
                .save(output.join(format!("{name}-before-after-reference.png")))
                .unwrap();
            let record = serde_json::json!({"source":path,"source_sha256":hash,"tile_index":index,
                "raw_origin":origin,"tile_size":session.size,"tile_raw_maximum":session.pixels.iter().max(),
                "compared_samples":corrected.rgb.len(),"legacy_differences":before_count,"legacy_maximum_error":before_maximum,
                "corrected_differences":after_count,"corrected_maximum_error":after_maximum,
                "panels":"Legacy tile / corrected tile / independent whole-frame reference, in stored tile orientation"});
            println!("{record}");
            records.push(record);
            assert!(before_count > 0);
            assert_eq!(after_count, 0);
            assert_eq!(
                hash,
                format!("{:x}", Sha256::digest(std::fs::read(&path).unwrap()))
            );
            assert!(source.unchanged());
        }
        std::fs::write(
            output.join("measurements.json"),
            serde_json::to_string_pretty(&records).unwrap(),
        )
        .unwrap();
    }
    #[test]
    fn stored_orientation_inverse_is_bounded_and_bijective() {
        let path = fixture("orientation", 80, 64);
        let mut source = Source::open(&path).unwrap();
        source.crop = [3, 5, 40, 32];
        for orientation in 1..=8 {
            source.orientation = orientation;
            source.size = if orientation >= 5 { [32, 40] } else { [40, 32] };
            let mut seen = std::collections::HashSet::new();
            for y in 0..source.size[1] {
                for x in 0..source.size[0] {
                    let p = source.raw_point(x, y);
                    assert!(p[0] >= 3 && p[0] < 43 && p[1] >= 5 && p[1] < 37);
                    assert!(seen.insert(p));
                    let rect = source.display_bounds(source.index_at(p)).unwrap();
                    assert!(x >= rect[0] && x < rect[0] + rect[2]);
                    assert!(y >= rect[1] && y < rect[1] + rect[3]);
                }
            }
            assert_eq!(seen.len(), 40 * 32);
        }
    }
    #[test]
    fn cached_raw_completion_matches_full_decode_and_detects_changes() {
        let path = fixture("completion", 65, 49);
        let mut source = Source::open(&path).unwrap();
        for index in 0..source.tiles.len() {
            let (raw, raw_size) = source.decode_oxide(index).unwrap();
            let display = source.develop(&raw, raw_size, 1, &|| false).unwrap();
            source.tiles[index] = Some(Tile {
                raw,
                raw_size,
                display,
                ratio: 1,
            });
        }
        let frame = source.finish(&|| false).unwrap();
        let full = decode::decode(&path).unwrap();
        assert_eq!(frame.size(), full.size());
        assert_eq!(frame.rgb, full.rgb);
        assert!(source.finish(&|| true).is_err());
        // Windows prevents truncating a mapped file; a timestamp revision can
        // still invalidate its cached decode without changing source pixels.
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(SystemTime::now() + std::time::Duration::from_secs(2))
            .unwrap();
        assert!(!source.unchanged());
        assert!(source.finish(&|| false).is_err());
    }
    #[test]
    fn incomplete_cache_never_becomes_an_exact_frame() {
        let path = fixture("incomplete", 65, 49);
        let source = Source::open(&path).unwrap();
        assert!(source.finish(&|| false).is_err());
    }
}
