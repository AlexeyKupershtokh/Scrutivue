//! Exact sampled RGB16 bands, never promoted to a complete/analysis Frame.
use super::*;

#[derive(Debug)]
pub struct Patch {
    pub image_size: [u32; 2],
    /// Origin and dimensions in the globally aligned sampled grid.
    pub origin: [u32; 2],
    pub size: [u32; 2],
    pub ratio: u32,
    pub rgb: Vec<u16>,
}
pub struct Progress<'a> {
    pub center: [f32; 2],
    pub emit: &'a (dyn Fn(Patch) + Sync),
}
unsafe extern "C" {
    fn diff_raw_open_preview_cancellable(
        bytes: *const c_void,
        length: usize,
        out: *mut RawView,
        error: *mut c_char,
        capacity: usize,
        cancelled: extern "C" fn(*mut c_void) -> i32,
        cancel_context: *mut c_void,
        preview: extern "C" fn(*mut c_void, *const u16, u32, u32, u32, u32, u32, u32, u32),
        context: *mut c_void,
        center_x: f32,
        center_y: f32,
    ) -> i32;
}
extern "C" fn emit(
    context: *mut c_void,
    pixels: *const u16,
    iw: u32,
    ih: u32,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    ratio: u32,
) {
    // No Rust panic may unwind through C++. The native borrowed buffer lives
    // only for this synchronous call; retain an independently owned small band.
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if context.is_null()
            || pixels.is_null()
            || ratio != 4
            || iw == 0
            || ih == 0
            || iw > 100000
            || ih > 100000
            || w == 0
            || h == 0
            || u64::from(x) + u64::from(w) > u64::from(iw.div_ceil(ratio))
            || u64::from(y) + u64::from(h) > u64::from(ih.div_ceil(ratio))
            || u64::from(w) * u64::from(h) * 6 > 16 * 1024 * 1024
        {
            return;
        }
        let progress = unsafe { &*context.cast::<Progress<'_>>() };
        let rgb =
            unsafe { std::slice::from_raw_parts(pixels, w as usize * h as usize * 3) }.to_vec();
        (progress.emit)(Patch {
            image_size: [iw, ih],
            origin: [x, y],
            size: [w, h],
            ratio,
            rgb,
        });
    }));
}
pub(super) unsafe fn open(
    bytes: &[u8],
    view: &mut RawView,
    error: &mut [c_char],
    cancellation: &mut Cancellation<'_>,
    progress: &Progress<'_>,
) -> i32 {
    unsafe {
        diff_raw_open_preview_cancellable(
            bytes.as_ptr().cast(),
            bytes.len(),
            view,
            error.as_mut_ptr(),
            error.len(),
            check_cancel,
            (cancellation as *mut Cancellation<'_>).cast(),
            emit,
            (progress as *const Progress<'_>).cast_mut().cast(),
            progress.center[0],
            progress.center[1],
        )
    }
}
pub fn decode(
    path: &Path,
    cancelled: &(dyn Fn() -> bool + Sync),
    progress: &Progress<'_>,
) -> Result<Arc<Frame>> {
    anyhow::ensure!(!cancelled(), "Image decode cancelled");
    let image = super::raw_with_progress(path, false, cancelled, Some(progress))?.into_rgb16();
    anyhow::ensure!(!cancelled(), "Image decode cancelled");
    Ok(Arc::new(Frame {
        width: image.width(),
        height: image.height(),
        rgb: image.into_raw(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::{path::PathBuf, sync::Mutex, time::Instant};

    #[test]
    fn native_callback_copies_owned_samples_and_rejects_invalid_layouts() {
        let patches = Mutex::new(Vec::new());
        let callback = |patch| patches.lock().unwrap().push(patch);
        let progress = Progress {
            center: [0.5; 2],
            emit: &callback,
        };
        let context = (&progress as *const Progress<'_>).cast_mut().cast();
        let mut rgb = vec![100, 200, 300];
        emit(context, rgb.as_ptr(), 4, 4, 0, 0, 1, 1, 4);
        rgb.fill(0);
        assert_eq!(patches.lock().unwrap()[0].rgb, [100, 200, 300]);
        emit(context, rgb.as_ptr(), 4, 4, 0, 0, u32::MAX, u32::MAX, 4);
        emit(context, rgb.as_ptr(), 4, 4, 1, 0, 1, 1, 4);
        emit(context, rgb.as_ptr(), 4, 4, 0, 0, 1, 1, 0);
        emit(context, std::ptr::null(), 4, 4, 0, 0, 1, 1, 4);
        emit(std::ptr::null_mut(), rgb.as_ptr(), 4, 4, 0, 0, 1, 1, 4);
        assert_eq!(patches.lock().unwrap().len(), 1);
    }

    #[test]
    #[ignore = "requires SCRUTIVUE_NEF_PROFILE_DIRECTORY and the requested read-only corpus"]
    fn cancellation_after_the_first_native_band_never_returns_a_partial_frame() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        let root = PathBuf::from(std::env::var_os("SCRUTIVUE_NEF_PROFILE_DIRECTORY").unwrap());
        let path = std::fs::read_dir(root)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| {
                path.extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("nef"))
            })
            .unwrap();
        let cancelled = AtomicBool::new(false);
        let bands = AtomicUsize::new(0);
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(6)
            .build()
            .unwrap();
        let result = pool.install(|| {
            decode(
                &path,
                &|| cancelled.load(Ordering::Acquire),
                &Progress {
                    center: [0.5; 2],
                    emit: &|_| {
                        bands.fetch_add(1, Ordering::Relaxed);
                        cancelled.store(true, Ordering::Release);
                    },
                },
            )
        });
        assert!(result.is_err());
        assert_eq!(bands.load(Ordering::Relaxed), 1);
    }

    #[test]
    #[ignore = "requires SCRUTIVUE_NEF_PROFILE_DIRECTORY/OUTPUT and the requested read-only corpus"]
    fn progressive_nef_bands_match_every_sample_and_preserve_final_frames() {
        let root = PathBuf::from(std::env::var_os("SCRUTIVUE_NEF_PROFILE_DIRECTORY").unwrap());
        let output = PathBuf::from(std::env::var_os("SCRUTIVUE_NEF_PROFILE_OUTPUT").unwrap());
        assert!(!output.exists(), "Use a fresh output filename");
        let mut paths: Vec<_> = std::fs::read_dir(root)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("nef"))
            })
            .collect();
        paths.sort();
        assert_eq!(paths.len(), 6);
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(6)
            .build()
            .unwrap();
        let mut records = Vec::new();
        for path in paths {
            let source_hash = Sha256::digest(std::fs::read(&path).unwrap());
            let reference = pool.install(|| super::super::decode(&path)).unwrap();
            let started = Instant::now();
            let updates = Mutex::new(Vec::new());
            let frame = pool
                .install(|| {
                    decode(
                        &path,
                        &|| false,
                        &Progress {
                            center: [0.5, 0.5],
                            emit: &|patch| {
                                updates
                                    .lock()
                                    .unwrap()
                                    .push((started.elapsed().as_secs_f64() * 1000., patch));
                            },
                        },
                    )
                })
                .unwrap();
            let ready_ms = started.elapsed().as_secs_f64() * 1000.;
            assert_eq!(frame.size(), reference.size());
            assert_eq!(frame.rgb, reference.rgb);
            let updates = updates.into_inner().unwrap();
            assert!(updates.len() > 2, "No early NEF bands");
            let grid = frame.size().map(|v| v.div_ceil(4));
            let mut visits = vec![0; grid[0] as usize * grid[1] as usize];
            for (_, patch) in &updates {
                assert_eq!(patch.image_size, frame.size());
                for y in 0..patch.size[1] {
                    for x in 0..patch.size[0] {
                        let gx = patch.origin[0] + x;
                        let gy = patch.origin[1] + y;
                        visits[(gy * grid[0] + gx) as usize] += 1;
                        let p = ((gy * 4 * frame.width + gx * 4) * 3) as usize;
                        let q = ((y * patch.size[0] + x) * 3) as usize;
                        assert_eq!(
                            &patch.rgb[q..q + 3],
                            &frame.rgb[p..p + 3],
                            "{}, sample {gx},{gy}",
                            path.display()
                        );
                    }
                }
            }
            assert!(visits.iter().all(|&visits| visits == 1));
            assert!(updates[0].0 < ready_ms);
            assert_eq!(Sha256::digest(std::fs::read(&path).unwrap()), source_hash);
            records.push(serde_json::json!({"file":path.file_name().unwrap().to_string_lossy(), "first_band_ms":updates[0].0,
                "last_band_ms":updates.last().unwrap().0,"full_frame_ms":ready_ms,"bands":updates.len(),
                "sampled_pixels_compared":visits.len(),"every_preview_sample_equal":true,"final_frame_equal":true,
                "full_rgb16_sha256":format!("{:x}",Sha256::digest(bytemuck::cast_slice(&frame.rgb)))}));
        }
        std::fs::write(
            output,
            serde_json::to_vec_pretty(&serde_json::json!({"success":true,"records":records}))
                .unwrap(),
        )
        .unwrap();
    }
}
