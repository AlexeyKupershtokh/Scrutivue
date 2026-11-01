//! Read-only stage timings for a ready A and a still-streaming B.
use super::*;

#[test]
#[ignore = "requires SCRUTIVUE_PROFILE_PAIR_A/B and SCRUTIVUE_STREAM_PROFILE_OUTPUT"]
fn ready_a_streaming_b_stage_profile() {
    let paths = ["SCRUTIVUE_PROFILE_PAIR_A", "SCRUTIVUE_PROFILE_PAIR_B"]
        .map(|name| PathBuf::from(std::env::var_os(name).expect(name)));
    let output = PathBuf::from(std::env::var_os("SCRUTIVUE_STREAM_PROFILE_OUTPUT").unwrap());
    assert!(!output.exists(), "Use a new profile output path");
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(6)
        .build()
        .unwrap();
    let library = Arc::new(
        jxl::Library::load(&PathBuf::from(
            std::env::var_os("SCRUTIVUE_JXL_TEST_LIBRARY").unwrap(),
        ))
        .unwrap(),
    );
    let frame = pool
        .install(|| {
            Source::open(&paths[0])
                .unwrap()
                .decode_full_native(library.clone(), &|| false)
        })
        .unwrap();
    let pointer = Arc::as_ptr(&frame);
    let mut states = HashMap::new();
    for (index, path) in paths.iter().enumerate() {
        states.insert(
            index as u64 + 1,
            State {
                source: Source::open(path).unwrap(),
                full: (index == 0).then(|| frame.clone()),
                pending: None,
            },
        );
    }
    let viewport = std::env::var("SCRUTIVUE_STREAM_PROFILE_VIEWPORT").unwrap_or("2560x1440".into());
    let (width, height) = viewport.split_once('x').unwrap();
    let job = Request {
        generation: 1,
        pair: [(1, paths[0].clone()), (2, paths[1].clone())],
        settings: Settings {
            mode: Mode::Diff,
            viewport: [width.parse().unwrap(), height.parse().unwrap()],
            camera: crate::model::Camera {
                zoom: if std::env::var_os("SCRUTIVUE_STREAM_PROFILE_100").is_some() {
                    crate::model::Zoom::Fixed(1.)
                } else {
                    crate::model::Zoom::Fit
                },
                ..Default::default()
            },
            ..Default::default()
        },
        ready: [Some(frame.clone()), None],
    };
    let (tx, rx) = bounded(8);
    let mut preview = difference::Preview::default();
    let mut last = None;
    let mut drain = || {
        while let Ok(event) = rx.try_recv() {
            if let Event::Snapshot {
                primary, progress, ..
            } = event
            {
                last = Some((primary, progress));
            }
        }
    };
    let start = Instant::now();
    let mut plan_ms = 0.;
    let mut decode_ms = 0.;
    let mut compose_ms = 0.;
    let mut snapshots = 0;
    let tick = Instant::now();
    publish(&tx, &job, &states, &mut preview, &pool);
    compose_ms += tick.elapsed().as_secs_f64() * 1000.;
    snapshots += 1;
    drain();
    let initial_snapshot_ms = start.elapsed().as_secs_f64() * 1000.;
    let mut published = Instant::now();
    let mut rounds = 0;
    let mut minimum_batch = usize::MAX;
    let mut maximum_batch = 0;
    let mut view_dirty = false;
    loop {
        let tick = Instant::now();
        let plans = next_round(&states, &job, true, 6, true);
        plan_ms += tick.elapsed().as_secs_f64() * 1000.;
        if plans.is_empty() {
            break;
        }
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].id, 2);
        if let Work::Batch(indices) = &plans[0].work {
            minimum_batch = minimum_batch.min(indices.len());
            maximum_batch = maximum_batch.max(indices.len());
        }
        let tick = Instant::now();
        execute_round(&mut states, &plans, &pool, Some(&library), &|| false).unwrap();
        decode_ms += tick.elapsed().as_secs_f64() * 1000.;
        rounds += 1;
        view_dirty |= plans.iter().any(|plan| plan.visible);
        if published.elapsed() >= Duration::from_millis(40) {
            if view_dirty {
                let tick = Instant::now();
                publish(&tx, &job, &states, &mut preview, &pool);
                compose_ms += tick.elapsed().as_secs_f64() * 1000.;
                snapshots += 1;
                view_dirty = false;
            } else {
                publish_progress(&tx, &job, &states);
            }
            drain();
            published = Instant::now();
        }
    }
    if view_dirty {
        let tick = Instant::now();
        publish(&tx, &job, &states, &mut preview, &pool);
        compose_ms += tick.elapsed().as_secs_f64() * 1000.;
        snapshots += 1;
    } else {
        publish_progress(&tx, &job, &states);
    }
    drain();
    drop(drain);
    let tick = Instant::now();
    let completed = finish_round(&mut states, [1, 2], &pool, &|| false).unwrap();
    let finish_ms = tick.elapsed().as_secs_f64() * 1000.;
    let total_ms = start.elapsed().as_secs_f64() * 1000.;
    assert_eq!(completed.len(), 1);
    assert_eq!(completed[0].0, 2);
    assert_eq!(Arc::as_ptr(states[&1].full.as_ref().unwrap()), pointer);
    assert!(states[&1].source.tiles.iter().all(Option::is_none));
    assert!(states.values().all(|state| state.source.unchanged()));
    let expected = crate::processing::Cpu::new().render(
        (1, &frame),
        (2, &completed[0].1),
        &job.settings,
        false,
    );
    let (actual, progress) = last.unwrap();
    assert!(progress.difference_pixels > 0);
    let error = actual
        .as_bytes(0)
        .unwrap()
        .iter()
        .zip(&expected)
        .map(|(a, b)| a.abs_diff(*b))
        .max()
        .unwrap();
    assert!(error <= 1, "Preview error: {error}");
    let report = serde_json::json!({"success":true,"viewport":job.settings.viewport,
        "fit":matches!(job.settings.camera.zoom, crate::model::Zoom::Fit),
        "ready_a_redecoded":false,"full_ready_ms":total_ms,"initial_snapshot_ms":initial_snapshot_ms,
        "plan_ms":plan_ms,"decode_ms":decode_ms,"compose_ms":compose_ms,"finish_ms":finish_ms,
        "snapshots":snapshots,"rounds":rounds,"minimum_batch":minimum_batch,"maximum_batch":maximum_batch,
        "diff_maximum_error_u8":error,"sources_unchanged":true,
        "scope":"Six-worker production scheduling/decoding/composition functions with A already complete; input opening/A decode and reference rendering excluded. No GPU presentation or filesystem cache flush."});
    std::fs::create_dir_all(output.parent().unwrap()).unwrap();
    std::fs::write(&output, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    println!("{report}");
}

#[test]
#[ignore = "requires SCRUTIVUE_PROFILE_PAIR_A/B, SCRUTIVUE_JXL_TEST_LIBRARY and SCRUTIVUE_STREAM_PROFILE_OUTPUT"]
fn ready_a_streaming_b_actual_worker() {
    let paths = ["SCRUTIVUE_PROFILE_PAIR_A", "SCRUTIVUE_PROFILE_PAIR_B"]
        .map(|name| PathBuf::from(std::env::var_os(name).expect(name)));
    let library = Arc::new(
        jxl::Library::load(&PathBuf::from(
            std::env::var_os("SCRUTIVUE_JXL_TEST_LIBRARY").unwrap(),
        ))
        .unwrap(),
    );
    let frame = Source::open(&paths[0])
        .unwrap()
        .decode_full_native(library, &|| false)
        .unwrap();
    let pointer = Arc::as_ptr(&frame);
    let output = PathBuf::from(std::env::var_os("SCRUTIVUE_STREAM_PROFILE_OUTPUT").unwrap());
    assert!(!output.exists());
    // The worker loads its native bundle relative to this test executable.
    assert!(
        jxl::Library::installed().is_ok(),
        "Install the pinned bundle beside the test executable"
    );
    let settings = Settings {
        mode: Mode::Diff,
        viewport: [2560, 1440],
        ..Default::default()
    };
    let worker = Worker::new();
    let start = Instant::now();
    worker.request(Request {
        generation: 1,
        pair: [(1, paths[0].clone()), (2, paths[1].clone())],
        settings: settings.clone(),
        ready: [Some(frame.clone()), None],
    });
    let mut snapshots = 0;
    let mut last = None;
    let mut first_b = None;
    let completed = loop {
        assert!(start.elapsed() < Duration::from_secs(120));
        match worker.events.recv_timeout(Duration::from_secs(30)).unwrap() {
            Event::Snapshot {
                primary, progress, ..
            } => {
                assert_eq!(progress.done[0], progress.total[0]);
                if progress.done[1] > 0 && first_b.is_none() {
                    first_b = Some(start.elapsed().as_secs_f64() * 1000.);
                }
                last = Some(primary);
                snapshots += 1;
            }
            Event::Complete { id, frame } => {
                assert_eq!(id, 2, "Ready A must not be redeveloped or republished");
                break frame;
            }
            Event::Unsupported { reason, .. } => panic!("{reason}"),
            Event::Progress { .. } => {}
        }
    };
    let full_ms = start.elapsed().as_secs_f64() * 1000.;
    worker.clear();
    assert_eq!(Arc::as_ptr(&frame), pointer);
    let expected =
        crate::processing::Cpu::new().render((1, &frame), (2, &completed), &settings, false);
    let error = last
        .unwrap()
        .as_bytes(0)
        .unwrap()
        .iter()
        .zip(expected)
        .map(|(a, b)| a.abs_diff(b))
        .max()
        .unwrap();
    assert!(error <= 1);
    let report = serde_json::json!({"success":true,"actual_worker":true,"ready_a_redecoded":false,
        "viewport":settings.viewport,"first_b_progress_ms":first_b,"full_ready_ms":full_ms,
        "snapshots":snapshots,"diff_maximum_error_u8":error,
        "scope":"Actual asynchronous tile worker with ready A, empty B and a 2560x1440 Fit Diff viewport. A decoding and reference rendering are outside ready timers; not GUI presentation latency."});
    std::fs::write(output, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    println!("{report}");
}
