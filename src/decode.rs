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
}
unsafe extern "C" {
    fn diff_raw_open(
        bytes: *const c_void,
        length: usize,
        thumbnail: i32,
        out: *mut RawView,
        error: *mut c_char,
        capacity: usize,
    ) -> i32;
    fn diff_raw_close(owner: *mut c_void);
}
impl Drop for RawView {
    fn drop(&mut self) {
        if !self.owner.is_null() {
            unsafe { diff_raw_close(self.owner) }
        }
    }
}

fn raw(path: &Path, thumbnail: bool) -> Result<DynamicImage> {
    let file = std::fs::File::open(path).with_context(|| format!("Read {}", path.display()))?;
    // The mapped file outlives RawView. Thumbnail extraction faults in only the
    // headers and embedded preview, not an entire 25–100 MB compressed RAW.
    // Source files must not be truncated/replaced while they are being decoded.
    let bytes = unsafe { memmap2::MmapOptions::new().map(&file) }
        .with_context(|| format!("Map {}", path.display()))?;
    let mut view = RawView::default();
    let mut error = [0i8; 512];
    let result = unsafe {
        diff_raw_open(
            bytes.as_ptr().cast(),
            bytes.len(),
            thumbnail as i32,
            &mut view,
            error.as_mut_ptr(),
            error.len(),
        )
    };
    if result != 0 {
        bail!(
            "LibRaw: {}",
            unsafe { CStr::from_ptr(error.as_ptr()) }.to_string_lossy()
        );
    }
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
    let image = match view.bits {
        16 if data.len() == count * 2 => {
            let values = data
                .chunks_exact(2)
                .map(|v| u16::from_ne_bytes([v[0], v[1]]))
                .collect();
            DynamicImage::ImageRgb16(
                ImageBuffer::<Rgb<u16>, _>::from_raw(view.width, view.height, values)
                    .context("Invalid RAW dimensions")?,
            )
        }
        8 if data.len() == count => DynamicImage::ImageRgb8(
            ImageBuffer::from_raw(view.width, view.height, data.to_vec())
                .context("Invalid preview dimensions")?,
        ),
        _ => bail!("Unsupported LibRaw bitmap depth"),
    };
    Ok(orient_thumbnail(image, view.flip))
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

pub fn decode(path: &Path) -> Result<Arc<Frame>> {
    let image = if crate::model::is_raw(path) {
        raw(path, false)?
    } else {
        raster(path)?
    };
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
