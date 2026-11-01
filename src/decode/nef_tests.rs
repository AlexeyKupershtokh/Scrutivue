//! Read-only actual-NEF CFA/RGB parity and native stage diagnostics.
use super::*;
use sha2::{Digest, Sha256};
use std::{path::PathBuf, time::Instant};

unsafe extern "C" {
    fn diff_raw_open_legacy_cancellable(
        bytes: *const c_void,
        length: usize,
        thumbnail: i32,
        out: *mut RawView,
        error: *mut c_char,
        capacity: usize,
        cancelled: extern "C" fn(*mut c_void) -> i32,
        context: *mut c_void,
    ) -> i32;
    fn diff_raw_uses_parallel(owner: *mut c_void) -> i32;
    fn diff_raw_sensor_samples(owner: *mut c_void, count: *mut usize) -> *const u16;
    fn diff_raw_test_parallel_kernels(error: *mut c_char, capacity: usize) -> i32;
}

#[test]
fn native_raw_kernels_match_pinned_reference_for_borders_black_histogram_gamma_and_all_flips() {
    for threads in [1, 6] {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .unwrap();
        pool.install(|| {
            let mut error = [0i8; 512];
            let status = unsafe { diff_raw_test_parallel_kernels(error.as_mut_ptr(), error.len()) };
            assert_eq!(
                status,
                0,
                "{}",
                unsafe { CStr::from_ptr(error.as_ptr()) }.to_string_lossy()
            );
        });
    }
}

#[test]
#[ignore = "requires SCRUTIVUE_NEF_PROFILE_DIRECTORY"]
fn nef_parallel_cancellation_concurrency_and_truncation_are_safe() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let root = PathBuf::from(std::env::var_os("SCRUTIVUE_NEF_PROFILE_DIRECTORY").unwrap());
    let bytes = std::fs::read(root.join("AK8_0005.NEF")).unwrap();
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(6)
        .build()
        .unwrap();
    for limit in [3, 25, 125, 250, 1000, 7000, 13000, 17050] {
        let calls = AtomicUsize::new(0);
        pool.install(|| {
            let result = native(&bytes, true, &|| {
                calls.fetch_add(1, Ordering::AcqRel) >= limit
            });
            assert!(
                result.is_err(),
                "Cancelled native work returned a partial image at limit {limit}"
            );
        });
        assert!(calls.load(Ordering::Acquire) > limit);
    }
    assert!(native(&bytes[..bytes.len() / 2], true, &|| false).is_err());
    let second = std::fs::read(root.join("AK8_0036.NEF")).unwrap();
    let expected: Vec<_> = [&bytes, &second]
        .into_iter()
        .map(|source| {
            pool.install(|| {
                let view = native(source, true, &|| false).unwrap();
                (
                    Sha256::digest(samples(&view)),
                    Sha256::digest(bytemuck::cast_slice(sensor(&view))),
                )
            })
        })
        .collect();
    let run = |source: &[u8], index: usize| {
        let view = native(source, true, &|| false).unwrap();
        assert_eq!(Sha256::digest(samples(&view)), expected[index].0);
        assert_eq!(
            Sha256::digest(bytemuck::cast_slice(sensor(&view))),
            expected[index].1
        );
    };
    pool.install(|| rayon::join(|| run(&bytes, 0), || run(&second, 1)));
}

fn native(bytes: &[u8], parallel: bool, cancelled: &(dyn Fn() -> bool + Sync)) -> Result<RawView> {
    let mut view = RawView::default();
    let mut error = [0i8; 512];
    let mut cancellation = Cancellation(cancelled);
    let open = if parallel {
        diff_raw_open_cancellable
    } else {
        diff_raw_open_legacy_cancellable
    };
    let status = unsafe {
        open(
            bytes.as_ptr().cast(),
            bytes.len(),
            0,
            &mut view,
            error.as_mut_ptr(),
            error.len(),
            check_cancel,
            (&mut cancellation as *mut Cancellation<'_>).cast(),
        )
    };
    anyhow::ensure!(
        status == 0,
        "{}",
        unsafe { CStr::from_ptr(error.as_ptr()) }.to_string_lossy()
    );
    Ok(view)
}

fn samples(view: &RawView) -> &[u8] {
    assert!(!view.data.is_null());
    unsafe { std::slice::from_raw_parts(view.data, view.length as usize) }
}

fn sensor(view: &RawView) -> &[u16] {
    let mut count = 0;
    let data = unsafe { diff_raw_sensor_samples(view.owner, &mut count) };
    assert!(!data.is_null());
    unsafe { std::slice::from_raw_parts(data, count) }
}

#[test]
#[ignore = "requires SCRUTIVUE_NEF_PROFILE_DIRECTORY and SCRUTIVUE_NEF_PROFILE_OUTPUT"]
fn nef_parallel_matches_pinned_cfa_and_rgb_references() {
    let root = PathBuf::from(std::env::var_os("SCRUTIVUE_NEF_PROFILE_DIRECTORY").unwrap());
    let output = PathBuf::from(std::env::var_os("SCRUTIVUE_NEF_PROFILE_OUTPUT").unwrap());
    assert!(!output.exists(), "Use a new output filename");
    let mut paths: Vec<_> = std::fs::read_dir(root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("nef"))
        })
        .collect();
    paths.sort();
    assert_eq!(paths.len(), 6, "Use the requested six-file corpus");
    let pools: Vec<_> = [1, 6]
        .into_iter()
        .map(|threads| {
            rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .build()
                .unwrap()
        })
        .collect();
    let mut rows = Vec::new();
    for path in paths {
        let source = std::fs::read(&path).unwrap();
        let before = Sha256::digest(&source);
        let tick = Instant::now();
        let reference = native(&source, false, &|| false).unwrap();
        let reference_ms = tick.elapsed().as_secs_f64() * 1000.;
        assert_eq!(unsafe { diff_raw_uses_parallel(reference.owner) }, 0);
        let expected_rgb = format!("{:x}", Sha256::digest(samples(&reference)));
        let expected_cfa = format!(
            "{:x}",
            Sha256::digest(bytemuck::cast_slice(sensor(&reference)))
        );
        let reference_rgb = samples(&reference).to_vec();
        let reference_cfa = sensor(&reference).to_vec();
        let reference_layout = [
            reference.width,
            reference.height,
            reference.colors,
            reference.bits,
            reference.flip,
        ];
        let reference_stages = [
            reference.open_ns as f64 / 1e6,
            reference.unpack_ns as f64 / 1e6,
            reference.process_ns as f64 / 1e6,
            reference.bitmap_ns as f64 / 1e6,
        ];
        drop(reference);
        for pool in &pools {
            let row = pool.install(|| {
            let tick = Instant::now();
            let actual = native(&source, true, &|| false).unwrap();
            let ready_ms = tick.elapsed().as_secs_f64() * 1000.;
            assert_eq!(
                unsafe { diff_raw_uses_parallel(actual.owner) },
                1,
                "Candidate must really use parallel NEF path: {}",
                path.display()
            );
            assert_eq!(
                [
                    actual.width,
                    actual.height,
                    actual.colors,
                    actual.bits,
                    actual.flip
                ],
                reference_layout
            );
            assert_eq!(
                sensor(&actual),
                reference_cfa,
                "CFA mismatch: {}",
                path.display()
            );
            assert_eq!(
                samples(&actual),
                reference_rgb,
                "RGB mismatch: {}",
                path.display()
            );
            serde_json::json!({"source":path,"threads":pool.current_num_threads(),
                "reference_ms":reference_ms,"candidate_ms":ready_ms,"candidate_stages_ms":[actual.open_ns as f64/1e6,actual.unpack_ns as f64/1e6,actual.process_ns as f64/1e6,actual.bitmap_ns as f64/1e6],
                "reference_stages_ms":reference_stages,
                "rgb16_sha256":expected_rgb,"cfa_sha256":expected_cfa,"cfa_samples":sensor(&actual).len(),
                "rgb16_bytes":actual.length,"all_samples_equal":true})
            });
            println!("{row}");
            rows.push(row);
        }
        assert_eq!(Sha256::digest(std::fs::read(&path).unwrap()), before);
    }
    let report = serde_json::json!({"success":true,"records":rows,
        "scope":"Pinned native legacy vs parallel CFA and developed RGB16, same settings and orientation. Timers cover native calls only; allocation/decode/development/bitmap included, validation, output copy, native release, GPU and presentation excluded. Optimized-debug host, native C++ optimized."});
    std::fs::write(output, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
}

#[test]
#[ignore = "requires SCRUTIVUE_NEF_PROFILE_PATH/OUTPUT and a native GPU backend"]
fn nef_async_decode_render_profile() {
    use crate::{
        model::{Mode, Settings},
        services::{Event, RenderJob, RenderRequest, Services},
    };
    use std::time::Duration;
    let path = PathBuf::from(std::env::var_os("SCRUTIVUE_NEF_PROFILE_PATH").unwrap());
    let output = PathBuf::from(std::env::var_os("SCRUTIVUE_NEF_PROFILE_OUTPUT").unwrap());
    assert!(!output.exists(), "Use a new output filename");
    let startup = Instant::now();
    let services = Services::new(false, output.with_extension("thumbnails"));
    let backend = loop {
        match services
            .events
            .recv_timeout(Duration::from_secs(60))
            .unwrap()
        {
            Event::Backend(backend) => break backend,
            Event::Error(error) => panic!("{error}"),
            event => services.discard(event),
        }
    };
    let startup_ms = startup.elapsed().as_secs_f64() * 1000.;
    let started = Instant::now();
    services.decode(vec![(1, path.clone())], 1, vec![path.clone()]);
    let frame = loop {
        match services
            .events
            .recv_timeout(Duration::from_secs(60))
            .unwrap()
        {
            Event::Decoded { id, result } => {
                assert_eq!(id, 1);
                break result.unwrap();
            }
            Event::Error(error) => panic!("{error}"),
            event => services.discard(event),
        }
    };
    let decode_ms = started.elapsed().as_secs_f64() * 1000.;
    services.renders.put(RenderJob::Frame(RenderRequest {
        generation: 1,
        a: (1, frame.clone()),
        b: (1, frame.clone()),
        settings: Settings {
            viewport: [2560, 1440],
            mode: Mode::Click,
            ..Default::default()
        },
    }));
    let render_ms = loop {
        match services
            .events
            .recv_timeout(Duration::from_secs(60))
            .unwrap()
        {
            Event::Rendered {
                elapsed,
                primary,
                alternate,
                ..
            } => {
                services.retire_images(std::iter::once(primary).chain(alternate).collect());
                break elapsed.as_secs_f64() * 1000.;
            }
            Event::Error(error) => panic!("{error}"),
            event => services.discard(event),
        }
    };
    let ready_ms = started.elapsed().as_secs_f64() * 1000.;
    let report = serde_json::json!({"success":true,"source":path,"backend":backend,"startup_ms":startup_ms,
        "services_decode_ms":decode_ms,"render_worker_ms":render_ms,"services_render_ready_ms":ready_ms,
        "dimensions":frame.size(),"rgb16_sha256":format!("{:x}",Sha256::digest(bytemuck::cast_slice(&frame.rgb))),
        "scope":"Actual asynchronous candidate Services, six-worker shared decode pool, 2560x1440 Fit. Optimized-debug Rust, optimized C++. Timers exclude GPU startup, reference/hash checks, scanner/thumbnail contention, GPUI texture upload and screen presentation."});
    std::fs::write(output, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    println!("{report}");
}
