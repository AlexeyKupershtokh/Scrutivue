//! DNG default crop and JPEG-XL transport support. Preserve camera metadata and
//! let the pinned LibRaw perform WB, camera-to-sRGB, active crop and orientation.
//! Apply DefaultCrop separately: LibRaw exposes it but does not enable it.
//! The source file is read-only; the uncompressed TIFF exists only in memory.
use anyhow::{bail, ensure, Context, Result};
use rayon::prelude::*;
pub mod tiles;

#[derive(Clone, Copy)]
struct Endian(bool);
impl Endian {
    fn u16(self, b: &[u8]) -> u16 {
        if self.0 {
            u16::from_le_bytes(b[..2].try_into().unwrap())
        } else {
            u16::from_be_bytes(b[..2].try_into().unwrap())
        }
    }
    fn u32(self, b: &[u8]) -> u32 {
        if self.0 {
            u32::from_le_bytes(b[..4].try_into().unwrap())
        } else {
            u32::from_be_bytes(b[..4].try_into().unwrap())
        }
    }
    fn put16(self, v: u16) -> [u8; 2] {
        if self.0 {
            v.to_le_bytes()
        } else {
            v.to_be_bytes()
        }
    }
    fn put32(self, v: u32) -> [u8; 4] {
        if self.0 {
            v.to_le_bytes()
        } else {
            v.to_be_bytes()
        }
    }
}
fn region(bytes: &[u8], offset: usize, length: usize) -> Result<&[u8]> {
    bytes
        .get(offset..offset.checked_add(length).context("TIFF offset overflow")?)
        .context("Truncated DNG")
}
struct Ifd {
    offset: usize,
    entries: Vec<[u8; 12]>,
    next: u32,
}
struct Tiff<'a> {
    bytes: &'a [u8],
    endian: Endian,
    ifds: Vec<Ifd>,
}
impl<'a> Tiff<'a> {
    fn parse(bytes: &'a [u8]) -> Result<Option<Self>> {
        if bytes.len() < 8 || !matches!(&bytes[..4], b"II*\0" | b"MM\0*") {
            return Ok(None);
        }
        let endian = Endian(bytes[0] == b'I');
        let mut todo = vec![endian.u32(&bytes[4..])];
        let mut tiff = Self {
            bytes,
            endian,
            ifds: Vec::new(),
        };
        while let Some(offset) = todo.pop() {
            if offset == 0 || tiff.ifds.iter().any(|i| i.offset == offset as usize) {
                continue;
            }
            ensure!(tiff.ifds.len() < 64, "Too many DNG IFDs");
            let count = endian.u16(region(bytes, offset as usize, 2)?) as usize;
            ensure!(count <= 4096, "Too many DNG tags");
            let entries = region(bytes, offset as usize + 2, count * 12)?
                .chunks_exact(12)
                .map(|v| v.try_into().unwrap())
                .collect();
            let next = endian.u32(region(bytes, offset as usize + 2 + count * 12, 4)?);
            let ifd = Ifd {
                offset: offset as usize,
                entries,
                next,
            };
            todo.push(next);
            todo.extend(tiff.values(&ifd, 330)?);
            tiff.ifds.push(ifd);
        }
        Ok(Some(tiff))
    }
    fn values(&self, ifd: &Ifd, tag: u16) -> Result<Vec<u32>> {
        let Some(e) = ifd.entries.iter().find(|e| self.endian.u16(*e) == tag) else {
            return Ok(Vec::new());
        };
        let kind = self.endian.u16(&e[2..]);
        let count = self.endian.u32(&e[4..]) as usize;
        let unit = match kind {
            1 => 1,
            3 => 2,
            4 => 4,
            _ => bail!("Unsupported DNG integer tag {tag}"),
        };
        let len = count.checked_mul(unit).context("TIFF count overflow")?;
        ensure!(len <= 16 * 1024 * 1024, "DNG integer table too large");
        let b = if len <= 4 {
            &e[8..8 + len]
        } else {
            region(self.bytes, self.endian.u32(&e[8..]) as usize, len)?
        };
        Ok(b.chunks_exact(unit)
            .map(|b| match kind {
                1 => b[0] as u32,
                3 => self.endian.u16(b) as u32,
                _ => self.endian.u32(b),
            })
            .collect())
    }
    fn value(&self, ifd: &Ifd, tag: u16, default: u32) -> Result<u32> {
        Ok(self.values(ifd, tag)?.first().copied().unwrap_or(default))
    }
    fn rationals(&self, ifd: &Ifd, tag: u16) -> Result<Vec<Rational>> {
        let Some(e) = ifd.entries.iter().find(|e| self.endian.u16(*e) == tag) else {
            return Ok(Vec::new());
        };
        let kind = self.endian.u16(&e[2..]);
        let count = self.endian.u32(&e[4..]) as usize;
        ensure!(count == 2, "DNG tag {tag} must contain two coordinates");
        let unit = match kind {
            3 => 2,
            4 => 4,
            5 => 8,
            _ => bail!("Unsupported DNG coordinate type for tag {tag}"),
        };
        let len = count * unit;
        let data = if len <= 4 {
            &e[8..8 + len]
        } else {
            region(self.bytes, self.endian.u32(&e[8..]) as usize, len)?
        };
        data.chunks_exact(unit)
            .map(|b| {
                let n = if kind == 3 {
                    self.endian.u16(b) as u32
                } else {
                    self.endian.u32(b)
                };
                let d = if kind == 5 {
                    self.endian.u32(&b[4..])
                } else {
                    1
                };
                ensure!(d != 0, "Zero denominator in DNG tag {tag}");
                Ok(Rational { n, d })
            })
            .collect()
    }
    fn is_dng(&self) -> bool {
        self.ifds
            .iter()
            .any(|i| i.entries.iter().any(|e| self.endian.u16(e) == 50706))
    }
    fn jxl_raw(&self) -> Result<Option<&Ifd>> {
        if !self.is_dng() {
            return Ok(None);
        }
        for ifd in &self.ifds {
            if self.value(ifd, 259, 1)? == 52546 && self.value(ifd, 254, 0)? & 1 == 0 {
                return Ok(Some(ifd));
            }
        }
        Ok(None)
    }
}

/// The actual LibRaw input rectangle before any processing/rotation. Must match
/// RawView's trailing six uint32_t fields in native/raw.cpp.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct RawArea {
    pub raw_width: u32,
    pub raw_height: u32,
    pub left: u32,
    pub top: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Copy)]
struct Rational {
    n: u32,
    d: u32,
}
impl Rational {
    fn integer(n: u32) -> Self {
        Self { n, d: 1 }
    }
    fn round(self) -> u32 {
        ((u64::from(self.n) + u64::from(self.d) / 2) / u64::from(self.d)) as u32
    }
    fn sum_fits(self, other: Self, limit: u32) -> bool {
        // Exact even for hostile u32 numerators/denominators (no float rounding).
        u128::from(self.n) * u128::from(other.d) + u128::from(other.n) * u128::from(self.d)
            <= u128::from(limit) * u128::from(self.d) * u128::from(other.d)
    }
}

fn active_area(tiff: &Tiff<'_>, ifd: &Ifd, raw_size: [u32; 2]) -> Result<[u32; 4]> {
    let active = tiff.values(ifd, 50829)?;
    let (top, left, bottom, right) = match active.as_slice() {
        [] => (0, 0, raw_size[1], raw_size[0]),
        &[t, l, b, r] => (t, l, b, r),
        _ => bail!("DNG ActiveArea must contain four coordinates"),
    };
    ensure!(
        left < right && top < bottom && right <= raw_size[0] && bottom <= raw_size[1],
        "Invalid DNG ActiveArea"
    );
    Ok([left, top, right - left, bottom - top])
}

fn default_crop(tiff: &Tiff<'_>, ifd: &Ifd, raw_size: [u32; 2]) -> Result<Option<[u32; 4]>> {
    let origin = tiff.rationals(ifd, 50719)?;
    let size = tiff.rationals(ifd, 50720)?;
    if origin.is_empty() && size.is_empty() {
        return Ok(None);
    }
    let scale = tiff.rationals(ifd, 50718)?;
    ensure!(
        scale.iter().all(|v| v.n == v.d),
        "DNG DefaultScale resampling is not supported by pixel-preserving default crop"
    );
    let [left, top, aw, ah] = active_area(tiff, ifd, raw_size)?;
    let active_size = [aw, ah];
    let mut start = [0; 2];
    let mut length = [0; 2];
    for axis in 0..2 {
        let o = origin.get(axis).copied().unwrap_or(Rational::integer(0));
        let s = size
            .get(axis)
            .copied()
            .unwrap_or(Rational::integer(active_size[axis]));
        ensure!(
            s.n > 0 && o.sum_fits(s, active_size[axis]),
            "DNG DefaultCrop is outside ActiveArea"
        );
        length[axis] = s.round();
        ensure!(length[axis] > 0, "DNG DefaultCrop rounds to an empty image");
        // Adobe DNG SDK DefaultCropArea: round origin and size separately;
        // slide back if rounding alone puts the far edge one pixel outside.
        start[axis] = o.round().min(active_size[axis] - length[axis]);
    }
    Ok(Some([
        left + start[0],
        top + start[1],
        length[0],
        length[1],
    ]))
}

/// Header-only DNG geometry, including the same default crop as full decode.
/// Reads TIFF metadata only: no JXL tile, JPEG preview or sensor unpacking.
pub fn dimensions(bytes: &[u8]) -> Result<Option<[u32; 2]>> {
    let Some(tiff) = Tiff::parse(bytes)? else {
        return Ok(None);
    };
    if !tiff.is_dng() {
        return Ok(None);
    }
    let mut selected = None;
    for ifd in &tiff.ifds {
        if tiff.value(ifd, 254, 0)? & 1 == 0 && matches!(tiff.value(ifd, 262, 0)?, 32803 | 34892) {
            ensure!(selected.is_none(), "Ambiguous DNG raw IFD for dimensions");
            selected = Some(ifd);
        }
    }
    let ifd = selected.context("Cannot identify DNG raw IFD for dimensions")?;
    let raw_size = [tiff.value(ifd, 256, 0)?, tiff.value(ifd, 257, 0)?];
    let rect = default_crop(&tiff, ifd, raw_size)?;
    // For ordinary DNGs without declared DefaultCrop, let LibRaw's header-only
    // sizing handle camera-specific geometry, just as for NEF. JXL is not
    // recognizable by this pin until unpacked, so its simple canvas/active
    // rectangle is the metadata-only fallback.
    if rect.is_none() && tiff.value(ifd, 259, 1)? != 52546 {
        return Ok(None);
    }
    let [_, _, w, h] = match rect {
        Some(rect) => rect,
        None => active_area(&tiff, ifd, raw_size)?,
    };
    // LibRaw apply_tiff uses the first non-identity TIFF orientation, including
    // IFD0 when the sensor image is a SubIFD and inherits IFD0's orientation.
    let mut rotated = false;
    for ifd in &tiff.ifds {
        let orientation = tiff.value(ifd, 274, 1)?;
        ensure!((1..=8).contains(&orientation), "Invalid DNG orientation");
        if orientation != 1 {
            rotated = orientation >= 5;
            break;
        }
    }
    Ok(Some(if rotated { [h, w] } else { [w, h] }))
}

pub fn crop(
    bytes: &[u8],
    image: image::DynamicImage,
    area: RawArea,
    flip: u32,
) -> Result<image::DynamicImage> {
    let [x, y, w, h] = crop_rect(bytes, [image.width(), image.height()], area, flip)?;
    if [x, y, w, h] == [0, 0, image.width(), image.height()] {
        return Ok(image);
    }
    Ok(image.crop_imm(x, y, w, h))
}

/// Same geometry as `crop`, usable before copying the native RGB16 bitmap.
pub fn crop_rect(bytes: &[u8], size: [u32; 2], area: RawArea, flip: u32) -> Result<[u32; 4]> {
    let full = [0, 0, size[0], size[1]];
    let Some(tiff) = Tiff::parse(bytes)? else {
        return Ok(full);
    };
    if !tiff.is_dng() {
        return Ok(full);
    }
    // Do not mistake a JPEG preview's crop/dimensions for the selected raw IFD.
    let mut selected = None;
    for ifd in &tiff.ifds {
        if tiff.value(ifd, 254, 0)? & 1 == 0
            && matches!(tiff.value(ifd, 262, 0)?, 32803 | 34892)
            && tiff.value(ifd, 256, 0)? == area.raw_width
            && tiff.value(ifd, 257, 0)? == area.raw_height
        {
            ensure!(selected.is_none(), "Ambiguous DNG raw IFD for default crop");
            selected = Some(ifd);
        }
    }
    let ifd = selected.context("Cannot map DNG raw IFD to LibRaw output for default crop")?;
    let Some([x, y, mut w, mut h]) = default_crop(&tiff, ifd, [area.raw_width, area.raw_height])?
    else {
        return Ok(full);
    };
    ensure!(
        x >= area.left
            && y >= area.top
            && u64::from(x) + u64::from(w) <= u64::from(area.left) + u64::from(area.width)
            && u64::from(y) + u64::from(h) <= u64::from(area.top) + u64::from(area.height),
        "DNG DefaultCrop is outside the LibRaw output rectangle"
    );
    let (mut x, mut y) = (x - area.left, y - area.top);
    let (mut iw, mut ih) = (area.width, area.height);
    ensure!(flip <= 7, "Unsupported LibRaw orientation for DNG crop");
    // Inverse of LibRaw flip_index: reflect in sensor axes, then transpose.
    // This also handles mirrored orientations, not just 90/180/270 rotations.
    if flip & 1 != 0 {
        x = iw - x - w;
    }
    if flip & 2 != 0 {
        y = ih - y - h;
    }
    if flip & 4 != 0 {
        std::mem::swap(&mut x, &mut y);
        std::mem::swap(&mut w, &mut h);
        std::mem::swap(&mut iw, &mut ih);
    }
    ensure!(
        size == [iw, ih],
        "LibRaw rescaled DNG output; cannot apply a pixel-preserving default crop"
    );
    Ok([x, y, w, h])
}

pub fn preview(bytes: &[u8]) -> Result<Option<image::DynamicImage>> {
    let Some(tiff) = Tiff::parse(bytes)? else {
        return Ok(None);
    };
    if tiff.jxl_raw()?.is_none() {
        return Ok(None);
    }
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
        if !jpeg.starts_with(&[0xff, 0xd8]) {
            continue;
        }
        let mut image = image::load_from_memory_with_format(jpeg, image::ImageFormat::Jpeg)?;
        image.apply_orientation(
            image::metadata::Orientation::from_exif(tiff.value(ifd, 274, 1)? as u8)
                .unwrap_or(image::metadata::Orientation::NoTransforms),
        );
        return Ok(Some(image));
    }
    bail!("JPEG XL DNG has no embedded JPEG preview; full decode is not run in thumbnail workers")
}

pub fn unpack(bytes: &[u8]) -> Result<Option<Vec<u8>>> {
    unpack_cancellable(bytes, &|| false)
}

pub fn unpack_cancellable(
    bytes: &[u8],
    cancelled: &(dyn Fn() -> bool + Sync),
) -> Result<Option<Vec<u8>>> {
    use std::{
        sync::atomic::{AtomicU64, Ordering},
        time::Instant,
    };
    let started = Instant::now();
    let trace = std::env::var_os("SCRUTIVUE_DECODE_TRACE").is_some();
    let read_ns = AtomicU64::new(0);
    let render_ns = AtomicU64::new(0);
    let stream_ns = AtomicU64::new(0);
    let Some(tiff) = Tiff::parse(bytes)? else {
        return Ok(None);
    };
    let Some(ifd) = tiff.jxl_raw()? else {
        return Ok(None);
    };
    let e = tiff.endian;
    let w = tiff.value(ifd, 256, 0)? as usize;
    let h = tiff.value(ifd, 257, 0)? as usize;
    let tw = tiff.value(ifd, 322, 0)? as usize;
    let th = tiff.value(ifd, 323, 0)? as usize;
    let channels = tiff.value(ifd, 277, 1)? as usize;
    ensure!(
        w > 0
            && h > 0
            && w <= 100000
            && h <= 100000
            && tw > 0
            && th > 0
            && tw <= 16384
            && th <= 16384,
        "Invalid DNG dimensions"
    );
    ensure!(
        matches!(channels, 1 | 3) && tiff.value(ifd, 284, 1)? == 1,
        "JPEG XL DNG requires chunky grey/RGB samples"
    );
    ensure!(
        tiff.values(ifd, 258)? == vec![16; channels] && tiff.value(ifd, 339, 1)? == 1,
        "JPEG XL DNG: only unsigned RGB/grey16 supported"
    );
    let offsets = tiff.values(ifd, 324)?;
    let lengths = tiff.values(ifd, 325)?;
    let nx = w.div_ceil(tw);
    let ny = h.div_ceil(th);
    ensure!(
        offsets.len() == nx * ny && lengths.len() == offsets.len(),
        "Invalid DNG tile table"
    );
    let pixel_bytes = w
        .checked_mul(h)
        .and_then(|n| n.checked_mul(channels * 2))
        .context("DNG size overflow")?;
    ensure!(
        pixel_bytes <= 1024 * 1024 * 1024,
        "DNG exceeds 1 GiB unpack limit"
    );
    let base = bytes.len().next_multiple_of(4);
    ensure!(
        base + pixel_bytes + 65536 < u32::MAX as usize,
        "DNG exceeds classic TIFF limit"
    );
    let mut output = Vec::with_capacity(base + pixel_bytes + 65536);
    output.extend_from_slice(bytes);
    output.resize(base + pixel_bytes, 0);
    let setup_ms = started.elapsed().as_secs_f64() * 1000.;
    // Disjoint tile rows: bounded memory per worker, no second full-frame copy.
    output[base..]
        .par_chunks_mut(th * w * channels * 2)
        .enumerate()
        .try_for_each(|(ty, dest)| -> Result<()> {
            for tx in 0..nx {
                ensure!(!cancelled(), "Image decode cancelled");
                let i = ty * nx + tx;
                let tile = region(bytes, offsets[i] as usize, lengths[i] as usize)?;
                let timer = trace.then(Instant::now);
                let image = jxl_oxide::JxlImage::builder()
                    .pool(jxl_oxide::JxlThreadPool::none())
                    .alloc_tracker(jxl_oxide::AllocTracker::with_limit(
                        (tw * th * 64).clamp(64 * 1024 * 1024, 512 * 1024 * 1024),
                    ))
                    .read(tile)
                    .map_err(|e| anyhow::anyhow!("DNG JXL tile {i}: {e}"))?;
                if let Some(timer) = timer {
                    read_ns.fetch_add(timer.elapsed().as_nanos() as u64, Ordering::Relaxed);
                }
                ensure!(
                    image.width() as usize <= tw && image.height() as usize <= th,
                    "Oversized JXL tile"
                );
                let timer = trace.then(Instant::now);
                let render = image
                    .render_frame(0)
                    .map_err(|e| anyhow::anyhow!("DNG JXL tile {i}: {e}"))?;
                if let Some(timer) = timer {
                    render_ns.fetch_add(timer.elapsed().as_nanos() as u64, Ordering::Relaxed);
                }
                ensure!(!cancelled(), "Image decode cancelled");
                let timer = trace.then(Instant::now);
                let mut stream = render.stream_no_alpha();
                let (iw, ih) = (stream.width() as usize, stream.height() as usize);
                let cw = tw.min(w - tx * tw);
                let ch = th.min(h - ty * th);
                ensure!(
                    stream.channels() as usize == channels && iw >= cw && ih >= ch,
                    "DNG JXL tile layout mismatch"
                );
                let mut row = vec![0u16; iw * channels];
                for y in 0..ch {
                    ensure!(
                        stream.write_to_buffer(&mut row) == row.len(),
                        "Truncated JXL tile"
                    );
                    let start = (y * w + tx * tw) * channels * 2;
                    for (dst, &v) in dest[start..start + cw * channels * 2]
                        .chunks_exact_mut(2)
                        .zip(&row)
                    {
                        dst.copy_from_slice(&e.put16(v));
                    }
                }
                if let Some(timer) = timer {
                    stream_ns.fetch_add(timer.elapsed().as_nanos() as u64, Ordering::Relaxed);
                }
            }
            Ok(())
        })?;
    // Replace only the raw IFD in place. Other IFD offsets and all calibration,
    // black/white levels, matrices, neutral, crop and opcode data remain valid.
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
        (279, 4, pixel_bytes as u32),
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
        "Replacement DNG IFD too large"
    );
    output[ifd.offset..ifd.offset + 2].copy_from_slice(&e.put16(entries.len() as u16));
    for (i, entry) in entries.iter().enumerate() {
        let start = ifd.offset + 2 + i * 12;
        output[start..start + 12].copy_from_slice(entry);
    }
    let next = ifd.offset + 2 + entries.len() * 12;
    output[next..next + 4].copy_from_slice(&e.put32(ifd.next));
    if trace {
        eprintln!("jxl_transport: canvas={w}x{h} tile={tw}x{th} tiles={} setup={setup_ms:.3}ms read_worker_sum={:.3}ms render_worker_sum={:.3}ms stream_worker_sum={:.3}ms wall={:.3}ms input_bytes={} virtual_bytes={}", offsets.len(), read_ns.load(Ordering::Relaxed) as f64 / 1e6, render_ns.load(Ordering::Relaxed) as f64 / 1e6, stream_ns.load(Ordering::Relaxed) as f64 / 1e6, started.elapsed().as_secs_f64()*1000., bytes.len(), output.len());
    }
    Ok(Some(output))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_stops_jxl_transport_before_decoding_tiles() {
        let source = synthetic(Endian(true), false);
        assert_eq!(
            unpack_cancellable(&source, &|| true)
                .unwrap_err()
                .to_string(),
            "Image decode cancelled"
        );
    }
    #[test]
    fn native_progress_cancellation_releases_the_decoder_without_returning_pixels() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(format!("target/native-cancel-tests/{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("cancel.dng");
        let source = unpack(&synthetic_size(Endian(true), false, 33, 35))
            .unwrap()
            .unwrap();
        std::fs::write(&path, source).unwrap();
        let calls = AtomicUsize::new(0);
        // The first two checks precede native processing. Cancel from the first
        // LibRaw progress callback, not merely from a Rust pre/post check.
        let error = crate::decode::decode_cancellable(&path, &|| {
            calls.fetch_add(1, Ordering::Relaxed) >= 2
        })
        .unwrap_err();
        assert!(error.to_string().starts_with("LibRaw:"), "{error:#}");
        assert!(calls.load(Ordering::Relaxed) >= 3);
        // A cancelled native instance does not poison later independent jobs.
        assert_eq!(crate::decode::decode(&path).unwrap().size(), [33, 35]);
    }
    fn synthetic(e: Endian, subifd: bool) -> Vec<u8> {
        synthetic_size(e, subifd, 17, 19)
    }
    pub(super) fn synthetic_size(e: Endian, subifd: bool, w: u32, h: u32) -> Vec<u8> {
        let raw = if subifd { 38 } else { 8 };
        let count = 14;
        let tiles = (w.div_ceil(16) * h.div_ceil(16)) as usize;
        let mut out = vec![0u8; raw + 2 + count * 12 + 4];
        out[..4].copy_from_slice(if e.0 { b"II*\0" } else { b"MM\0*" });
        out[4..8].copy_from_slice(&e.put32(8));
        let bits = out.len() as u32;
        for _ in 0..3 {
            out.extend_from_slice(&e.put16(16));
        }
        let offsets = out.len() as u32;
        out.resize(out.len() + tiles * 4, 0);
        let counts = out.len() as u32;
        out.resize(out.len() + tiles * 4, 0);
        let camera = out.len() as u32;
        out.extend_from_slice(b"TEST-DNG\0");
        let tile = include_bytes!("../assets/tests/dng-jxl-tile.jxl");
        for i in 0..tiles {
            let pos = offsets as usize + i * 4;
            let start = out.len() as u32;
            out[pos..pos + 4].copy_from_slice(&e.put32(start));
            let pos = counts as usize + i * 4;
            out[pos..pos + 4].copy_from_slice(&e.put32(tile.len() as u32));
            out.extend_from_slice(tile);
        }
        let mut entries = vec![
            (254, 4, 1, 0),
            (256, 4, 1, w),
            (257, 4, 1, h),
            (258, 3, 3, bits),
            (259, 3, 1, 52546),
            (262, 3, 1, 34892),
            (277, 3, 1, 3),
            (284, 3, 1, 1),
            (322, 4, 1, 16),
            (323, 4, 1, 16),
            (324, 4, tiles as u32, offsets),
            (325, 4, tiles as u32, counts),
            (50706, 1, 4, 0),
            (50708, 2, 9, camera),
        ];
        entries.sort_by_key(|v| v.0);
        let write = |out: &mut [u8], at: usize, entries: &[(u16, u16, u32, u32)]| {
            out[at..at + 2].copy_from_slice(&e.put16(entries.len() as u16));
            for (i, &(tag, kind, n, value)) in entries.iter().enumerate() {
                let pos = at + 2 + i * 12;
                out[pos..pos + 2].copy_from_slice(&e.put16(tag));
                out[pos + 2..pos + 4].copy_from_slice(&e.put16(kind));
                out[pos + 4..pos + 8].copy_from_slice(&e.put32(n));
                if tag == 50706 {
                    out[pos + 8..pos + 12].copy_from_slice(&[1, 7, 0, 0]);
                } else if kind == 3 && n == 1 {
                    out[pos + 8..pos + 10].copy_from_slice(&e.put16(value as u16));
                } else {
                    out[pos + 8..pos + 12].copy_from_slice(&e.put32(value));
                }
            }
        };
        write(&mut out, raw, &entries);
        if subifd {
            write(&mut out, 8, &[(330, 4, 1, raw as u32), (50706, 1, 4, 0)]);
        }
        out
    }
    fn crop_fixture(
        e: Endian,
        source: Vec<u8>,
        kind: u16,
        origin: [(u32, u32); 2],
        size: [(u32, u32); 2],
        active: [u32; 4],
        orientation: u16,
    ) -> Vec<u8> {
        let tiff = Tiff::parse(&source).unwrap().unwrap();
        let raw = tiff.jxl_raw().unwrap().unwrap();
        let mut entries = raw.entries.clone();
        let root = e.u32(&source[4..]) as usize;
        let pointer = if root == raw.offset {
            4
        } else {
            // Synthetic root's first entry is the raw SubIFD pointer.
            root + 2 + 8
        };
        let mut out = source;
        let mut append = |tag: u16, kind: u16, n: u32, payload: Vec<u8>| {
            let mut entry = [0; 12];
            entry[..2].copy_from_slice(&e.put16(tag));
            entry[2..4].copy_from_slice(&e.put16(kind));
            entry[4..8].copy_from_slice(&e.put32(n));
            if payload.len() <= 4 {
                entry[8..8 + payload.len()].copy_from_slice(&payload);
            } else {
                entry[8..12].copy_from_slice(&e.put32(out.len() as u32));
                out.extend_from_slice(&payload);
            }
            entries.push(entry);
        };
        for (tag, values) in [(50719, origin), (50720, size)] {
            let payload = values
                .into_iter()
                .flat_map(|(n, d)| match kind {
                    3 => e.put16(n as u16).to_vec(),
                    4 => e.put32(n).to_vec(),
                    5 => [e.put32(n), e.put32(d)].concat(),
                    _ => unreachable!(),
                })
                .collect();
            append(tag, kind, 2, payload);
        }
        append(
            50829,
            4,
            4,
            active.into_iter().flat_map(|v| e.put32(v)).collect(),
        );
        append(50718, 5, 2, [e.put32(1); 4].concat());
        append(274, 3, 1, e.put16(orientation).to_vec());
        entries.sort_by_key(|entry| e.u16(entry));
        let offset = out.len() as u32;
        out[pointer..pointer + 4].copy_from_slice(&e.put32(offset));
        out.extend_from_slice(&e.put16(entries.len() as u16));
        for entry in entries {
            out.extend_from_slice(&entry);
        }
        out.extend_from_slice(&e.put32(0));
        out
    }
    fn orient(mut image: image::DynamicImage, flip: u32) -> image::DynamicImage {
        let exif = [1, 2, 4, 3, 5, 8, 6, 7][flip as usize];
        image.apply_orientation(image::metadata::Orientation::from_exif(exif).unwrap());
        image
    }
    fn pixels(w: u32, h: u32) -> image::DynamicImage {
        image::DynamicImage::ImageRgb16(image::ImageBuffer::from_fn(w, h, |x, y| {
            let v = (y * w + x) as u16;
            image::Rgb([v + 1000, v + 32768, 65535 - v])
        }))
    }
    #[test]
    fn default_crop_integer_rational_active_area_and_all_orientations_preserve_rgb16() {
        for e in [Endian(true), Endian(false)] {
            for subifd in [false, true] {
                for kind in [3, 4, 5] {
                    // RATIONAL deliberately has fractional origins and sizes.
                    let (o, s) = if kind == 5 {
                        ([(3, 2), (5, 2)], [(11, 2), (15, 2)])
                    } else {
                        ([(2, 1), (3, 1)], [(6, 1), (8, 1)])
                    };
                    let source =
                        crop_fixture(e, synthetic(e, subifd), kind, o, s, [1, 2, 18, 16], 1);
                    for flip in 0..8 {
                        // LibRaw has already removed ActiveArea margins.
                        let area = RawArea {
                            raw_width: 17,
                            raw_height: 19,
                            left: 2,
                            top: 1,
                            width: 14,
                            height: 17,
                        };
                        let expected = orient(pixels(14, 17).crop_imm(2, 3, 6, 8), flip);
                        let image =
                            crop(&source, orient(pixels(14, 17), flip), area, flip).unwrap();
                        assert!(matches!(image, image::DynamicImage::ImageRgb16(_)));
                        assert_eq!(image.as_rgb16(), expected.as_rgb16());
                        // Full sensor output and already-default-cropped output
                        // must map to the same rectangle (no double crop).
                        let full = RawArea {
                            left: 0,
                            top: 0,
                            width: 17,
                            height: 19,
                            ..area
                        };
                        let original = pixels(17, 19);
                        let expected = orient(original.crop_imm(4, 4, 6, 8), flip);
                        let image = crop(&source, orient(original, flip), full, flip).unwrap();
                        assert_eq!(image.as_rgb16(), expected.as_rgb16());
                        let already = RawArea {
                            left: 4,
                            top: 4,
                            width: 6,
                            height: 8,
                            ..area
                        };
                        assert_eq!(
                            crop(&source, image, already, flip).unwrap().as_rgb16(),
                            expected.as_rgb16()
                        );
                    }
                }
            }
        }
    }
    #[test]
    fn rational_crop_rounding_at_far_edge_slides_back_without_resampling() {
        let source = crop_fixture(
            Endian(true),
            synthetic(Endian(true), false),
            5,
            [(1, 2), (1, 2)],
            [(33, 2), (37, 2)],
            [0, 0, 19, 17],
            1,
        );
        let area = RawArea {
            raw_width: 17,
            raw_height: 19,
            width: 17,
            height: 19,
            ..RawArea::default()
        };
        let image = crop(&source, pixels(17, 19), area, 0).unwrap();
        assert_eq!(image.as_rgb16(), pixels(17, 19).as_rgb16());
    }
    #[test]
    fn header_dimensions_use_crop_and_orientation_without_reading_jxl_pixels() {
        for e in [Endian(true), Endian(false)] {
            for subifd in [false, true] {
                for kind in [3, 4, 5] {
                    for orientation in 1..=8 {
                        let mut source = crop_fixture(
                            e,
                            synthetic(e, subifd),
                            kind,
                            [(2, 1), (3, 1)],
                            [(6, 1), (8, 1)],
                            [1, 2, 18, 16],
                            orientation,
                        );
                        let tiff = Tiff::parse(&source).unwrap().unwrap();
                        let raw = tiff.jxl_raw().unwrap().unwrap();
                        let start = tiff.values(raw, 324).unwrap()[0] as usize;
                        // A full decode cannot succeed; metadata still must.
                        source[start..start + 16].fill(0);
                        assert_eq!(
                            dimensions(&source).unwrap(),
                            Some(if orientation >= 5 { [8, 6] } else { [6, 8] })
                        );
                    }
                }
            }
        }
    }
    #[test]
    fn invalid_crop_and_rescaled_output_are_not_silently_clamped_or_stretched() {
        let e = Endian(true);
        let area = RawArea {
            raw_width: 17,
            raw_height: 19,
            width: 17,
            height: 19,
            ..RawArea::default()
        };
        for (o, s, active) in [
            ([(0, 0), (0, 1)], [(6, 1), (8, 1)], [0, 0, 19, 17]),
            ([(12, 1), (0, 1)], [(6, 1), (8, 1)], [0, 0, 19, 17]),
            ([(0, 1), (0, 1)], [(6, 1), (8, 1)], [0, 0, 20, 17]),
            ([(0, 1), (0, 1)], [(1, 4), (8, 1)], [0, 0, 19, 17]),
        ] {
            let source = crop_fixture(e, synthetic(e, false), 5, o, s, active, 1);
            assert!(crop(&source, pixels(17, 19), area, 0).is_err());
        }
        let source = crop_fixture(
            e,
            synthetic(e, false),
            4,
            [(1, 1), (2, 1)],
            [(6, 1), (8, 1)],
            [0, 0, 19, 17],
            1,
        );
        assert!(crop(&source, pixels(34, 38), area, 0).is_err());
        assert!(crop(&source, pixels(17, 19), RawArea { left: 3, ..area }, 0).is_err());
        let mut scaled = source;
        let tiff = Tiff::parse(&scaled).unwrap().unwrap();
        let raw = tiff.jxl_raw().unwrap().unwrap();
        let entry = raw.entries.iter().find(|v| e.u16(*v) == 50718).unwrap();
        let at = e.u32(&entry[8..]) as usize;
        scaled[at..at + 4].copy_from_slice(&e.put32(2));
        assert!(crop(&scaled, pixels(17, 19), area, 0)
            .unwrap_err()
            .to_string()
            .contains("DefaultScale"));
    }
    #[test]
    fn libraw_default_crop_matches_exact_subset_of_uncropped_rgb16_for_rotated_dng() {
        let e = Endian(true);
        let root = std::env::temp_dir().join(format!("lumen-dng-crop-test-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("synthetic.dng");
        for (orientation, flip) in [(1, 0), (6, 6), (8, 5)] {
            for (kind, active) in [(4, [0, 0, 35, 33]), (5, [1, 2, 34, 32])] {
                let source = synthetic_size(e, true, 33, 35);
                let source = crop_fixture(
                    e,
                    source,
                    kind,
                    if kind == 5 {
                        [(3, 2), (5, 2)]
                    } else {
                        [(2, 1), (3, 1)]
                    },
                    if kind == 5 {
                        [(53, 2), (55, 2)]
                    } else {
                        [(27, 1), (28, 1)]
                    },
                    active,
                    orientation,
                );
                // Keep the SAME ActiveArea, orientation and processing for the
                // reference. ActiveArea can affect LibRaw's channel maxima/WB.
                let mut reference = source.clone();
                let tiff = Tiff::parse(&reference).unwrap().unwrap();
                let raw = tiff.jxl_raw().unwrap().unwrap();
                let tags: Vec<_> = raw
                    .entries
                    .iter()
                    .enumerate()
                    .filter_map(|(i, entry)| {
                        matches!(e.u16(entry), 50719 | 50720).then_some(raw.offset + 2 + i * 12)
                    })
                    .collect();
                for at in tags {
                    reference[at..at + 2]
                        .copy_from_slice(&e.put16(65000 + (e.u16(&source[at..]) - 50719)));
                }
                std::fs::write(&path, &reference).unwrap();
                let uncropped = crate::decode::decode(&path).unwrap();
                assert_eq!(crate::decode::dimensions(&path).unwrap(), uncropped.size());
                std::fs::write(&path, source).unwrap();
                let cropped = crate::decode::decode(&path).unwrap();
                assert_eq!(crate::decode::dimensions(&path).unwrap(), cropped.size());
                let raw = image::DynamicImage::ImageRgb16(
                    image::ImageBuffer::from_raw(
                        uncropped.width,
                        uncropped.height,
                        uncropped.rgb.clone(),
                    )
                    .unwrap(),
                );
                // Undo only the reference orientation, then crop in active axes.
                let inverse = match flip {
                    5 => 6,
                    6 => 5,
                    _ => flip,
                };
                let expected =
                    orient(orient(raw, inverse).crop_imm(2, 3, 27, 28), flip).into_rgb16();
                assert_eq!(cropped.size(), [expected.width(), expected.height()]);
                assert!(
                    cropped.rgb == expected.into_raw(),
                    "RGB16 changed outside cropping: orientation={orientation}, kind={kind}"
                );
            }
        }
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_dir(&root).unwrap();
    }
    #[test]
    fn jxl_rgb16_tiles_preserve_samples_metadata_endian_and_cropped_edges() {
        // Fixture generated by independent libjxl (imagecodecs): 16x16 RGB16,
        // value[i] = (i*17431+50123)&65535, lossless, including full 16-bit range.
        for endian in [Endian(true), Endian(false)] {
            for subifd in [false, true] {
                let source = synthetic(endian, subifd);
                let unpacked = unpack(&source).unwrap().unwrap();
                let tiff = Tiff::parse(&unpacked).unwrap().unwrap();
                let raw = tiff
                    .ifds
                    .iter()
                    .find(|i| tiff.value(i, 256, 0).unwrap() == 17)
                    .unwrap();
                assert_eq!(tiff.value(raw, 259, 0).unwrap(), 1);
                let base = tiff.value(raw, 273, 0).unwrap() as usize;
                for y in 0..19 {
                    for x in 0..17 {
                        for c in 0..3 {
                            let i = ((y * 17 + x) * 3 + c) * 2;
                            let expected =
                                (((y % 16 * 16 + x % 16) * 3 + c) * 17431 + 50123) as u16;
                            assert_eq!(
                                endian.u16(&unpacked[base + i..]),
                                expected,
                                "x={x} y={y} c={c}"
                            );
                        }
                    }
                }
                let entry = raw
                    .entries
                    .iter()
                    .find(|v| endian.u16(*v) == 50708)
                    .unwrap();
                let at = endian.u32(&entry[8..]) as usize;
                assert_eq!(&unpacked[at..at + 9], b"TEST-DNG\0");
                assert!(unpack(&unpacked).unwrap().is_none());
                assert!(unpack(&source[..source.len() - 7]).is_err());
            }
        }
    }
    #[test]
    fn malformed_tiff_never_indexes_out_of_bounds() {
        for b in [
            b"II*\0\xff\xff\xff\xff".as_slice(),
            b"II*\0\x08\0\0\0\x01\0".as_slice(),
        ] {
            assert!(unpack(b).is_err());
        }
        assert!(unpack(b"not TIFF").unwrap().is_none());
    }
}
