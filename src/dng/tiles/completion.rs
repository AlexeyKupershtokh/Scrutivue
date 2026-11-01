//! Promote only certified, fully developed RGB16 tiles to the analysis frame.
//! Unknown/header-dependent/global processing retains ordinary full LibRaw.
use super::*;
use std::time::Instant;
#[cfg(test)]
mod tests;

impl Source {
    pub fn prepare_completion_policy(&self) {
        let _ = self.completion_white();
    }
    pub(super) fn completion_white(&self) -> Option<u16> {
        *self
            .pointwise_white
            .get_or_init(|| self.match_completion_policy().ok().flatten())
    }
    pub(super) fn match_completion_policy(&self) -> Result<Option<u16>> {
        // Do not turn opening an oversized transport/obsolete-payload container
        // into an unbounded extra allocation merely to qualify an optimization.
        if self.map.len() > 256 * 1024 * 1024 {
            return Ok(None);
        }
        let tiff = Tiff::parse(&self.map)?.context("Missing TIFF header")?;
        let ifd = tiff.jxl_raw()?.context("Missing raw IFD")?;
        // A matching color state is insufficient if LibRaw could select another
        // raw frame with identical geometry/calibration but different pixels.
        let mut raw_frames = 0;
        for directory in &tiff.ifds {
            if matches!(tiff.value(directory, 262, 0)?, 32803 | 34892) {
                raw_frames += 1;
            }
        }
        if raw_frames != 1 {
            return Ok(None);
        }
        let index = ifd
            .entries
            .iter()
            .position(|e| tiff.endian.u16(e) == 259)
            .context("Missing compression tag")?;
        let entry = &ifd.entries[index];
        ensure!(
            tiff.endian.u16(&entry[2..]) == 3 && tiff.endian.u32(&entry[4..]) == 1,
            "Unsupported compression tag layout"
        );
        // Header offsets must keep their original meaning. This bounded copy is
        // released immediately, not retained as another source-sized cache.
        let mut header = self.map.to_vec();
        let at = ifd.offset + 2 + index * 12 + 8;
        header[at..at + 2].copy_from_slice(&tiff.endian.put16(1));
        let region = self.compact(&vec![0; 32 * 32 * 3], [32, 32])?;
        Ok(decode::matching_tile_policy(
            &header,
            &region,
            self.raw_size,
            self.orientation,
        ))
    }

    fn complete_tiles(&self, cancelled: &(dyn Fn() -> bool + Sync)) -> Result<()> {
        ensure!(!cancelled(), "Image decode cancelled");
        ensure!(self.unchanged(), "Source changed while loading DNG tiles");
        for (index, tile) in self.tiles.iter().enumerate() {
            let tile = tile.as_ref().context("DNG tiles are incomplete")?;
            let [x, y] = self.origin(index);
            let size = [
                self.tile_size[0].min(self.raw_size[0] - x),
                self.tile_size[1].min(self.raw_size[1] - y),
            ];
            ensure!(
                tile.ratio == 1
                    && tile.raw_size[0] >= size[0]
                    && tile.raw_size[1] >= size[1]
                    && tile.raw.len() == tile.raw_size[0] as usize * tile.raw_size[1] as usize * 3,
                "Incomplete or truncated DNG tile"
            );
        }
        Ok(())
    }

    fn completion_fallback(
        &self,
        cancelled: &(dyn Fn() -> bool + Sync),
    ) -> Result<Option<&'static str>> {
        let Some(white) = self.completion_white() else {
            return Ok(Some("Header processing is not certified pointwise RGB"));
        };
        if self
            .tiles
            .iter()
            .flatten()
            .any(|t| t.display.size() != t.raw_size || t.display.rgb.len() != t.raw.len())
        {
            return Ok(Some("Full-resolution developed tiles are unavailable"));
        }
        // Ordinary LibRaw may replace the metadata white level with the global
        // peak. Check actual image samples, never padded edge-tile pixels.
        let peak = self
            .tiles
            .par_iter()
            .enumerate()
            .map(|(index, tile)| -> Result<u16> {
                let tile = tile.as_ref().unwrap();
                let [x, y] = self.origin(index);
                let width = self.tile_size[0].min(self.raw_size[0] - x) as usize * 3;
                let height = self.tile_size[1].min(self.raw_size[1] - y) as usize;
                let mut peak = 0;
                for row in tile
                    .raw
                    .chunks_exact(tile.raw_size[0] as usize * 3)
                    .take(height)
                {
                    ensure!(!cancelled(), "Image decode cancelled");
                    peak = peak.max(row[..width].iter().copied().max().unwrap_or(0));
                }
                Ok(peak)
            })
            .try_reduce(|| 0, |a, b| Ok(a.max(b)))?;
        Ok(
            (peak > 0 && peak < white && u32::from(peak) * 4 > u32::from(white) * 3)
                .then_some("Global white-level normalization requires full development"),
        )
    }

    pub fn finish(&self, cancelled: &(dyn Fn() -> bool + Sync)) -> Result<Arc<Frame>> {
        self.finish_profiled(cancelled).map(|(frame, _)| frame)
    }
    pub fn finish_profiled(
        &self,
        cancelled: &(dyn Fn() -> bool + Sync),
    ) -> Result<(Arc<Frame>, FinishTimings)> {
        let started = Instant::now();
        self.complete_tiles(cancelled)?;
        let fallback_reason = self.completion_fallback(cancelled)?;
        if fallback_reason.is_none() {
            let frame = self.join_developed(cancelled)?;
            ensure!(self.unchanged(), "Source changed during DNG completion");
            return Ok((
                frame,
                FinishTimings {
                    assembly_ms: started.elapsed().as_secs_f64() * 1000.,
                    development_ms: 0.,
                    path: "developed-tiles",
                    fallback_reason: None,
                },
            ));
        }
        let out = self.assemble(cancelled)?;
        let assembly_ms = started.elapsed().as_secs_f64() * 1000.;
        let started = Instant::now();
        let frame = decode::develop_dng_bytes(&out, cancelled)?;
        ensure!(self.unchanged(), "Source changed during DNG development");
        Ok((
            frame,
            FinishTimings {
                assembly_ms,
                development_ms: started.elapsed().as_secs_f64() * 1000.,
                path: "full-libraw",
                fallback_reason,
            },
        ))
    }
    pub fn finish_reference(&self, cancelled: &(dyn Fn() -> bool + Sync)) -> Result<Arc<Frame>> {
        let frame = decode::develop_dng_bytes(&self.assemble(cancelled)?, cancelled)?;
        ensure!(self.unchanged(), "Source changed during DNG development");
        Ok(frame)
    }

    fn join_developed(&self, cancelled: &(dyn Fn() -> bool + Sync)) -> Result<Arc<Frame>> {
        let [width, height] = self.size;
        let stride = width as usize * 3;
        let mut rgb = vec![0; stride * height as usize];
        // A small row block keeps both source and destination cache-friendly,
        // including a transposed crop. Each parallel task owns disjoint rows.
        rgb.par_chunks_mut(stride * 16).enumerate().try_for_each(
            |(block, rows)| -> Result<()> {
                for x in 0..width {
                    ensure!(!cancelled(), "Image decode cancelled");
                    for (dy, row) in rows.chunks_exact_mut(stride).enumerate() {
                        let p = self.raw_point(x, (block * 16 + dy) as u32);
                        let tile = self.tiles[self.index_at(p)].as_ref().unwrap();
                        let offset = ((p[1] % self.tile_size[1]) * tile.display.width
                            + p[0] % self.tile_size[0])
                            as usize
                            * 3;
                        row[x as usize * 3..x as usize * 3 + 3]
                            .copy_from_slice(&tile.display.rgb[offset..offset + 3]);
                    }
                }
                Ok(())
            },
        )?;
        Ok(Arc::new(Frame { width, height, rgb }))
    }
}
