use anyhow::{bail, Context, Result};
use image::{DynamicImage, ImageBuffer, Rgb};
use std::{
    ffi::{c_char, c_void, CStr},
    path::Path,
    sync::Arc,
};

#[derive(Debug)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub rgb: Vec<u16>,
}
impl Frame {
    pub fn size(&self) -> [u32; 2] {
        [self.width, self.height]
    }
    pub fn bytes(&self) -> usize {
        self.rgb.len() * 2
    }
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
    area: crate::dng::RawArea,
    open_ns: u64,
    unpack_ns: u64,
    process_ns: u64,
    bitmap_ns: u64,
}
unsafe extern "C" {
    fn diff_raw_matching_tile_policy(
        original: *const c_void,
        original_length: usize,
        region: *const c_void,
        region_length: usize,
        width: u32,
        height: u32,
        orientation: u32,
    ) -> u32;
    fn diff_raw_open_cancellable(
        bytes: *const c_void,
        length: usize,
        thumbnail: i32,
        out: *mut RawView,
        error: *mut c_char,
        capacity: usize,
        cancelled: extern "C" fn(*mut c_void) -> i32,
        context: *mut c_void,
    ) -> i32;
    fn diff_raw_open_region_cancellable(
        bytes: *const c_void,
        length: usize,
        out: *mut RawView,
        error: *mut c_char,
        capacity: usize,
        cancelled: extern "C" fn(*mut c_void) -> i32,
        context: *mut c_void,
    ) -> i32;
    fn diff_raw_close(owner: *mut c_void);
    fn libraw_init(flags: u32) -> *mut c_void;
    fn libraw_open_buffer(raw: *mut c_void, bytes: *const c_void, length: usize) -> i32;
    fn libraw_adjust_sizes_info_only(raw: *mut c_void) -> i32;
    fn libraw_get_iwidth(raw: *mut c_void) -> i32;
    fn libraw_get_iheight(raw: *mut c_void) -> i32;
    fn libraw_close(raw: *mut c_void);
}

/// Match the effective, fixed RGB processing state without unpacking either input.
pub(crate) fn matching_tile_policy(
    original: &[u8],
    region: &[u8],
    size: [u32; 2],
    orientation: u32,
) -> Option<u16> {
    let white = unsafe {
        diff_raw_matching_tile_policy(
            original.as_ptr().cast(),
            original.len(),
            region.as_ptr().cast(),
            region.len(),
            size[0],
            size[1],
            orientation,
        )
    };
    (white > 0 && white <= 65535).then_some(white as u16)
}

struct Cancellation<'a>(&'a (dyn Fn() -> bool + Sync));
extern "C" fn check_cancel(context: *mut c_void) -> i32 {
    let cancelled = unsafe { &*context.cast::<Cancellation<'_>>() };
    // Never unwind through LibRaw's C++ progress callback.
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (cancelled.0)())).unwrap_or(true)
        as i32
}
impl Drop for RawView {
    fn drop(&mut self) {
        if !self.owner.is_null() {
            unsafe { diff_raw_close(self.owner) }
        }
    }
}

fn raw(path: &Path, thumbnail: bool) -> Result<DynamicImage> {
    raw_cancellable(path, thumbnail, &|| false)
}

fn raw_cancellable(
    path: &Path,
    thumbnail: bool,
    cancelled: &(dyn Fn() -> bool + Sync),
) -> Result<DynamicImage> {
    let started = std::time::Instant::now();
    let file = std::fs::File::open(path).with_context(|| format!("Read {}", path.display()))?;
    // The mapped file outlives RawView. Thumbnail extraction faults in only the
    // headers and embedded preview, not an entire 25–100 MB compressed RAW.
    // Source files must not be truncated/replaced while they are being decoded.
    let mapped = unsafe { memmap2::MmapOptions::new().map(&file) }
        .with_context(|| format!("Map {}", path.display()))?;
    let is_dng = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("dng"));
    if thumbnail && is_dng {
        if let Some(preview) = crate::dng::preview(&mapped)? {
            return Ok(preview);
        }
    }
    let transport_start = std::time::Instant::now();
    let unpacked = if !thumbnail && is_dng {
        crate::dng::unpack_cancellable(&mapped, cancelled)?
    } else {
        None
    };
    let transport_ms = transport_start.elapsed().as_secs_f64() * 1000.;
    anyhow::ensure!(!cancelled(), "Image decode cancelled");
    let bytes: &[u8] = unpacked.as_deref().unwrap_or(&mapped);
    let mut view = RawView::default();
    let mut error = [0i8; 512];
    let mut cancellation = Cancellation(cancelled);
    let result = unsafe {
        diff_raw_open_cancellable(
            bytes.as_ptr().cast(),
            bytes.len(),
            thumbnail as i32,
            &mut view,
            error.as_mut_ptr(),
            error.len(),
            check_cancel,
            (&mut cancellation as *mut Cancellation<'_>).cast(),
        )
    };
    if result != 0 {
        bail!(
            "LibRaw: {}",
            unsafe { CStr::from_ptr(error.as_ptr()) }.to_string_lossy()
        );
    }
    anyhow::ensure!(!cancelled(), "Image decode cancelled");
    if view.data.is_null() {
        bail!("LibRaw returned an empty image");
    }
    let data = unsafe { std::slice::from_raw_parts(view.data, view.length as usize) };
    // LibRaw JPEG thumbnails are encoded; bitmap previews and developments are packed RGB.
    if thumbnail && view.format == 1 {
        return Ok(orient_thumbnail(image::load_from_memory(data)?, view.flip));
    }
    if view.colors != 3 || view.width == 0 || view.height == 0 {
        bail!("Unsupported LibRaw output layout");
    }
    let count = (view.width as usize)
        .checked_mul(view.height as usize)
        .and_then(|n| n.checked_mul(3))
        .context("Image dimensions overflow")?;
    let copy_start = std::time::Instant::now();
    let [x, y, width, height] = if !thumbnail && is_dng {
        crate::dng::crop_rect(bytes, [view.width, view.height], view.area, view.flip)?
    } else {
        [0, 0, view.width, view.height]
    };
    let image = match view.bits {
        16 if data.len() == count * 2 => {
            // Copy only the final oriented crop, directly into the RGB16 owner.
            // Native samples already use host endianness. Byte copies do not
            // require alignment of libraw_processed_image_t::data.
            let length = width as usize * height as usize * 3;
            let mut values = Vec::<u16>::with_capacity(length);
            for row in 0..height as usize {
                let start = ((y as usize + row) * view.width as usize + x as usize) * 6;
                let samples = width as usize * 3;
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        data[start..start + samples * 2].as_ptr(),
                        values.as_mut_ptr().add(row * samples).cast::<u8>(),
                        samples * 2,
                    );
                }
            }
            // Every byte of all `length` valid u16 samples was initialized above.
            unsafe { values.set_len(length) };
            DynamicImage::ImageRgb16(
                ImageBuffer::<Rgb<u16>, _>::from_raw(width, height, values)
                    .context("Invalid RAW dimensions")?,
            )
        }
        8 if data.len() == count => DynamicImage::ImageRgb8(
            ImageBuffer::from_raw(view.width, view.height, data.to_vec())
                .context("Invalid preview dimensions")?,
        ),
        _ => bail!("Unsupported LibRaw bitmap depth"),
    };
    let copy_ms = copy_start.elapsed().as_secs_f64() * 1000.;
    let image = if thumbnail {
        orient_thumbnail(image, view.flip)
    } else {
        image
    };
    let native_ns = [
        view.open_ns,
        view.unpack_ns,
        view.process_ns,
        view.bitmap_ns,
    ];
    let release_start = std::time::Instant::now();
    drop(view);
    drop(unpacked);
    drop(mapped);
    let release_ms = release_start.elapsed().as_secs_f64() * 1000.;
    if !thumbnail && std::env::var_os("SCRUTIVUE_DECODE_TRACE").is_some() {
        eprintln!("decode {}: transport={transport_ms:.3}ms open={:.3}ms unpack={:.3}ms process={:.3}ms bitmap={:.3}ms copy_crop={copy_ms:.3}ms release={release_ms:.3}ms total={:.3}ms rayon_threads={}", path.display(), native_ns[0] as f64 / 1e6, native_ns[1] as f64 / 1e6, native_ns[2] as f64 / 1e6, native_ns[3] as f64 / 1e6, started.elapsed().as_secs_f64()*1000., rayon::current_num_threads());
    }
    Ok(image)
}

fn orient_thumbnail(image: DynamicImage, flip: u32) -> DynamicImage {
    match flip {
        3 => image.rotate180(),
        5 => image.rotate270(),
        6 => image.rotate90(),
        _ => image,
    }
}

fn raster(path: &Path) -> Result<DynamicImage> {
    use image::ImageDecoder;
    let reader = image::ImageReader::open(path)?.with_guessed_format()?;
    let mut decoder = reader.into_decoder()?;
    let orientation = decoder
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut image = DynamicImage::from_decoder(decoder)?;
    image.apply_orientation(orientation);
    Ok(image)
}

/// Display-oriented, default-cropped dimensions from headers only. Suitable for
/// background file metadata; does not develop RAWs or decompress DNG JXL tiles.
pub fn dimensions(path: &Path) -> Result<[u32; 2]> {
    if !crate::model::is_raw(path) {
        use image::{metadata::Orientation, ImageDecoder};
        let mut decoder = image::ImageReader::open(path)?
            .with_guessed_format()?
            .into_decoder()?;
        let (w, h) = decoder.dimensions();
        let rotated = matches!(
            decoder.orientation().ok(),
            Some(
                Orientation::Rotate90
                    | Orientation::Rotate270
                    | Orientation::Rotate90FlipH
                    | Orientation::Rotate270FlipH
            )
        );
        return Ok(if rotated { [h, w] } else { [w, h] });
    }
    let file = std::fs::File::open(path).with_context(|| format!("Read {}", path.display()))?;
    let bytes = unsafe { memmap2::MmapOptions::new().map(&file) }
        .with_context(|| format!("Map {}", path.display()))?;
    if path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("dng"))
    {
        if let Some(size) = crate::dng::dimensions(&bytes)? {
            return Ok(size);
        }
    }
    struct Header(*mut c_void);
    impl Drop for Header {
        fn drop(&mut self) {
            unsafe { libraw_close(self.0) }
        }
    }
    let ptr = unsafe { libraw_init(0) };
    if ptr.is_null() {
        bail!("Cannot allocate LibRaw header reader");
    }
    let header = Header(ptr);
    unsafe {
        if libraw_open_buffer(header.0, bytes.as_ptr().cast(), bytes.len()) != 0
            || libraw_adjust_sizes_info_only(header.0) != 0
        {
            bail!("LibRaw could not identify RAW dimensions");
        }
        let (w, h) = (libraw_get_iwidth(header.0), libraw_get_iheight(header.0));
        if w <= 0 || h <= 0 {
            bail!("Invalid RAW dimensions");
        }
        Ok([w as u32, h as u32])
    }
}

pub fn decode(path: &Path) -> Result<Arc<Frame>> {
    decode_cancellable(path, &|| false)
}

/// Independent legacy whole-frame reference for streaming validation only.
pub(crate) fn decode_dng_reference(path: &Path) -> Result<Arc<Frame>> {
    if let Ok(library) = crate::jxl::Library::installed() {
        return crate::dng::tiles::Source::open(path)?
            .decode_full_native_reference(library, &|| false);
    }
    let image = raw_cancellable(path, false, &|| false)?.into_rgb16();
    Ok(Arc::new(Frame {
        width: image.width(),
        height: image.height(),
        rgb: image.into_raw(),
    }))
}

/// Develop an in-memory, uncompressed DNG on the caller's background worker.
/// Authoritative whole-frame completion retains the pinned production settings.
pub fn develop_dng_bytes(
    bytes: &[u8],
    cancelled: &(dyn Fn() -> bool + Sync),
) -> Result<Arc<Frame>> {
    develop_dng_bytes_with_policy(bytes, false, cancelled)
}

/// Preview regions must share the metadata white level, not independently
/// normalize brightness to the brightest sample of each tile.
pub fn develop_dng_region_bytes(
    bytes: &[u8],
    cancelled: &(dyn Fn() -> bool + Sync),
) -> Result<Arc<Frame>> {
    develop_dng_bytes_with_policy(bytes, true, cancelled)
}

fn develop_dng_bytes_with_policy(
    bytes: &[u8],
    region: bool,
    cancelled: &(dyn Fn() -> bool + Sync),
) -> Result<Arc<Frame>> {
    anyhow::ensure!(!cancelled(), "Image decode cancelled");
    let mut view = RawView::default();
    let mut error = [0i8; 512];
    let mut cancellation = Cancellation(cancelled);
    let status = unsafe {
        if region {
            diff_raw_open_region_cancellable(
                bytes.as_ptr().cast(),
                bytes.len(),
                &mut view,
                error.as_mut_ptr(),
                error.len(),
                check_cancel,
                (&mut cancellation as *mut Cancellation<'_>).cast(),
            )
        } else {
            diff_raw_open_cancellable(
                bytes.as_ptr().cast(),
                bytes.len(),
                0,
                &mut view,
                error.as_mut_ptr(),
                error.len(),
                check_cancel,
                (&mut cancellation as *mut Cancellation<'_>).cast(),
            )
        }
    };
    if status != 0 {
        bail!(
            "LibRaw: {}",
            unsafe { CStr::from_ptr(error.as_ptr()) }.to_string_lossy()
        );
    }
    anyhow::ensure!(
        !cancelled() && !view.data.is_null() && view.bits == 16 && view.colors == 3,
        "Invalid or cancelled DNG development"
    );
    let [x, y, width, height] =
        crate::dng::crop_rect(bytes, [view.width, view.height], view.area, view.flip)?;
    let data = unsafe { std::slice::from_raw_parts(view.data, view.length as usize) };
    anyhow::ensure!(
        data.len() == view.width as usize * view.height as usize * 6,
        "Invalid DNG bitmap length"
    );
    let mut rgb = Vec::with_capacity(width as usize * height as usize * 3);
    for row in 0..height as usize {
        let start = ((y as usize + row) * view.width as usize + x as usize) * 6;
        for value in data[start..start + width as usize * 6].chunks_exact(2) {
            rgb.push(u16::from_ne_bytes([value[0], value[1]]));
        }
    }
    Ok(Arc::new(Frame { width, height, rgb }))
}

pub fn decode_cancellable(
    path: &Path,
    cancelled: &(dyn Fn() -> bool + Sync),
) -> Result<Arc<Frame>> {
    anyhow::ensure!(!cancelled(), "Image decode cancelled");
    if path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("dng"))
    {
        if let Ok(library) = crate::jxl::Library::installed() {
            if let Ok(source) = crate::dng::tiles::Source::open(path) {
                match source.decode_full_native(library, cancelled) {
                    Ok(frame) => return Ok(frame),
                    Err(error) if cancelled() => return Err(error),
                    Err(_) => {} // Unsupported native profiles retain the existing oxide fallback.
                }
            }
        }
    }
    let image = if crate::model::is_raw(path) {
        raw_cancellable(path, false, cancelled)?
    } else {
        raster(path)?
    };
    anyhow::ensure!(!cancelled(), "Image decode cancelled");
    let rgb = image.into_rgb16();
    Ok(Arc::new(Frame {
        width: rgb.width(),
        height: rgb.height(),
        rgb: rgb.into_raw(),
    }))
}

pub fn thumbnail(path: &Path) -> Result<image::RgbaImage> {
    let image = if crate::model::is_raw(path) {
        // A missing embedded preview is not a reason to develop 45 MP in the thumbnail queue.
        raw(path, true)?
    } else {
        raster(path)?
    };
    Ok(image.thumbnail(160, 112).into_rgba8())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancelled_decode_does_not_even_open_the_source() {
        let error = decode_cancellable(Path::new("missing-image.dng"), &|| true).unwrap_err();
        assert_eq!(error.to_string(), "Image decode cancelled");
    }

    #[test]
    fn jpeg_exif_is_applied_once_without_resizing_or_rewriting_the_source() {
        let root = std::env::temp_dir().join(format!(
            "lumen-jpeg-orientation-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("small-export.jpg");
        let source = ImageBuffer::from_fn(5, 3, |x, y| Rgb([(x * 50) as u8, (y * 100) as u8, 123]));
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 95)
            .encode_image(&source)
            .unwrap();
        let decoded = image::load_from_memory(&jpeg).unwrap();
        for exif in 1..=8u16 {
            let mut payload = b"Exif\0\0II*\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0".to_vec();
            payload.extend_from_slice(&exif.to_le_bytes());
            payload.extend_from_slice(&[0; 6]); // inline padding and next IFD.
            let mut bytes = jpeg[..2].to_vec();
            bytes.extend_from_slice(&[0xff, 0xe1]);
            bytes.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
            bytes.extend_from_slice(&payload);
            bytes.extend_from_slice(&jpeg[2..]);
            std::fs::write(&path, &bytes).unwrap();
            let mut expected = decoded.clone();
            expected
                .apply_orientation(image::metadata::Orientation::from_exif(exif as u8).unwrap());
            let expected = expected.into_rgb16();
            let actual = decode(&path).unwrap();
            assert_eq!(actual.size(), [expected.width(), expected.height()]);
            assert_eq!(dimensions(&path).unwrap(), actual.size());
            assert_eq!(actual.rgb, expected.into_raw());
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
        }
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_dir(&root).unwrap();
    }
}
