//! Bit-exact JPEG display preparation without an intermediate rotated RGB8 copy.
//! Entropy/IDCT/color decoding stays with the existing image/zune backend.
use super::{DynamicImage, Frame};
use anyhow::{Context, Result};
use image::metadata::Orientation;
use rayon::prelude::*;
use std::sync::{Arc, OnceLock};

fn fallback_pool() -> &'static rayon::ThreadPool {
    static POOL: OnceLock<rayon::ThreadPool> = OnceLock::new();
    POOL.get_or_init(|| {
        let threads = std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(2)
            .div_ceil(2)
            .clamp(2, 6);
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .thread_name(|i| format!("jpeg-pixels-{i}"))
            .build()
            .expect("JPEG pixel pool")
    })
}

pub(super) fn frame(
    image: DynamicImage,
    orientation: Orientation,
    cancelled: &(dyn Fn() -> bool + Sync),
) -> Result<Arc<Frame>> {
    anyhow::ensure!(!cancelled(), "Image decode cancelled");
    let convert = || match image {
        DynamicImage::ImageRgb8(rgb) => expand::<3>(
            rgb.as_raw(),
            rgb.width(),
            rgb.height(),
            orientation,
            cancelled,
        ),
        DynamicImage::ImageLuma8(gray) => expand::<1>(
            gray.as_raw(),
            gray.width(),
            gray.height(),
            orientation,
            cancelled,
        ),
        mut image => {
            // Preserve the old path if a future JPEG backend adds another
            // pixel layout; only RGB8 and grayscale8 use the fused kernel.
            image.apply_orientation(orientation);
            let rgb = image.into_rgb16();
            anyhow::ensure!(!cancelled(), "Image decode cancelled");
            Ok(Frame {
                width: rgb.width(),
                height: rgb.height(),
                rgb: rgb.into_raw(),
            })
        }
    };
    // The GUI already supplies its shared, bounded decode registry. Only headless
    // callers need this one process-wide fallback; never create a pool per image.
    let frame = if rayon::current_thread_index().is_some() {
        convert()?
    } else {
        fallback_pool().install(convert)?
    };
    Ok(Arc::new(frame))
}

fn expand<const N: usize>(
    input: &[u8],
    width: u32,
    height: u32,
    orientation: Orientation,
    cancelled: &(dyn Fn() -> bool + Sync),
) -> Result<Frame> {
    let pixels = (width as usize)
        .checked_mul(height as usize)
        .context("JPEG dimensions overflow")?;
    let count = pixels.checked_mul(3).context("JPEG dimensions overflow")?;
    anyhow::ensure!(
        (N == 1 || N == 3) && pixels.checked_mul(N) == Some(input.len()),
        "Invalid JPEG output layout"
    );
    let transposed = matches!(
        orientation,
        Orientation::Rotate90
            | Orientation::Rotate270
            | Orientation::Rotate90FlipH
            | Orientation::Rotate270FlipH
    );
    let [w, h] = if transposed {
        [height, width]
    } else {
        [width, height]
    };
    if pixels == 0 {
        return Ok(Frame {
            width: w,
            height: h,
            rgb: Vec::new(),
        });
    }
    let stride = (width as usize)
        .checked_mul(N)
        .context("JPEG dimensions overflow")?;
    isize::try_from(input.len()).context("JPEG dimensions overflow")?;
    let stride = isize::try_from(stride).context("JPEG dimensions overflow")?;
    let last_row = (height as isize - 1) * stride;
    let last_column = (width as isize - 1) * N as isize;
    let n = N as isize;
    // Source origin and steps for one output pixel/row, including mirrored EXIF.
    let (origin, dx, dy) = match orientation {
        Orientation::NoTransforms => (0, n, stride),
        Orientation::FlipHorizontal => (last_column, -n, stride),
        Orientation::FlipVertical => (last_row, n, -stride),
        Orientation::Rotate180 => (last_row + last_column, -n, -stride),
        Orientation::Rotate90 => (last_row, -stride, n),
        Orientation::Rotate270 => (last_column, stride, -n),
        Orientation::Rotate90FlipH => (0, stride, n),
        Orientation::Rotate270FlipH => (last_row + last_column, -stride, -n),
    };
    let row_samples = w as usize * 3;
    let mut rgb = Vec::<u16>::new();
    rgb.try_reserve_exact(count)
        .context("Allocate JPEG RGB16 frame")?;
    // Each task exclusively owns complete output rows. Small two-dimensional
    // tiles keep transposed source reads cache-local instead of striding across
    // the entire 45 MP image. Expand u8 * 257 directly into the final RGB16 owner.
    const TILE: usize = 32;
    let rows_per_job = TILE.min(h as usize);
    rgb.spare_capacity_mut()[..count]
        .par_chunks_mut(row_samples * rows_per_job)
        .enumerate()
        .try_for_each(|(block, rows)| -> Result<()> {
            anyhow::ensure!(!cancelled(), "Image decode cancelled");
            for left in (0..w as usize).step_by(TILE) {
                let right = (left + TILE).min(w as usize);
                for local_y in 0..rows.len() / row_samples {
                    let y = block * rows_per_job + local_y;
                    let row = &mut rows
                        [local_y * row_samples + left * 3..local_y * row_samples + right * 3];
                    let mut from = origin + y as isize * dy + left as isize * dx;
                    for pixel in row.chunks_exact_mut(3) {
                        let index = from as usize;
                        let values = if N == 1 {
                            [u16::from(input[index]) * 257; 3]
                        } else {
                            std::array::from_fn(|c| u16::from(input[index + c]) * 257)
                        };
                        for (to, value) in pixel.iter_mut().zip(values) {
                            to.write(value);
                        }
                        from += dx;
                    }
                }
            }
            Ok(())
        })?;
    anyhow::ensure!(!cancelled(), "Image decode cancelled");
    // Every channel in every disjoint row was initialized above. On cancellation
    // the length stays zero; no partially initialized frame is read or published.
    unsafe {
        rgb.set_len(count);
    }
    Ok(Frame {
        width: w,
        height: h,
        rgb,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Luma, LumaA, Rgb, Rgba};
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn fused_jpeg_preparation_matches_every_legacy_sample_for_all_exif_orientations() {
        for threads in [1, 6] {
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .build()
                .unwrap();
            for (w, h) in [(5, 3), (65, 49), (32, 33), (1, 73), (71, 1), (0, 4), (4, 0)] {
                for grayscale in [false, true] {
                    let source = if grayscale {
                        DynamicImage::ImageLuma8(ImageBuffer::from_fn(w, h, |x, y| {
                            Luma([(x * 17 + y * 31) as u8])
                        }))
                    } else {
                        DynamicImage::ImageRgb8(ImageBuffer::from_fn(w, h, |x, y| {
                            Rgb([
                                (x * 17 + y * 31) as u8,
                                (x * 11 + y * 101) as u8,
                                (x * 193 + y * 5) as u8,
                            ])
                        }))
                    };
                    for exif in 1..=8 {
                        let orientation = Orientation::from_exif(exif).unwrap();
                        let mut expected = source.clone();
                        expected.apply_orientation(orientation);
                        let expected = expected.into_rgb16();
                        let actual = pool
                            .install(|| frame(source.clone(), orientation, &|| false))
                            .unwrap();
                        assert_eq!(
                            actual.size(),
                            [expected.width(), expected.height()],
                            "threads={threads}, exif={exif}"
                        );
                        assert_eq!(
                            actual.rgb,
                            expected.into_raw(),
                            "threads={threads}, size={w}x{h}, exif={exif}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn other_pixel_layouts_keep_the_legacy_conversion_fallback() {
        for source in [
            DynamicImage::ImageRgba8(ImageBuffer::from_fn(5, 3, |x, y| {
                Rgba([(x * 17) as u8, (y * 31) as u8, 123, 64])
            })),
            DynamicImage::ImageRgb16(ImageBuffer::from_fn(7, 3, |x, y| {
                Rgb([(x * 1234) as u16, (y * 9123) as u16, 60000])
            })),
            DynamicImage::ImageLumaA8(ImageBuffer::from_fn(3, 5, |x, y| {
                LumaA([(x * 17 + y * 31) as u8, 96])
            })),
        ] {
            for exif in 1..=8 {
                let orientation = Orientation::from_exif(exif).unwrap();
                let mut expected = source.clone();
                expected.apply_orientation(orientation);
                let expected = expected.into_rgb16();
                let actual = frame(source.clone(), orientation, &|| false).unwrap();
                assert_eq!(actual.size(), [expected.width(), expected.height()]);
                assert_eq!(actual.rgb, expected.into_raw());
            }
        }
    }

    #[test]
    fn cancelled_jpeg_preparation_never_returns_a_partial_frame() {
        let image = DynamicImage::new_rgb8(129, 129);
        let orientation = Orientation::Rotate270;
        assert_eq!(
            frame(image.clone(), orientation, &|| true)
                .unwrap_err()
                .to_string(),
            "Image decode cancelled"
        );
        let calls = AtomicUsize::new(0);
        assert_eq!(
            frame(image, orientation, &|| calls.fetch_add(1, Ordering::AcqRel)
                >= 2)
            .unwrap_err()
            .to_string(),
            "Image decode cancelled"
        );
        assert!(calls.load(Ordering::Acquire) >= 3);
    }
}
