//! Read-only stage/scaling measurements for one unfinished source.
use super::*;
use sha2::{Digest, Sha256};

fn settings() -> Settings {
    let viewport = std::env::var("SCRUTIVUE_SINGLE_PROFILE_VIEWPORT").unwrap_or("2560x1440".into());
    let (width, height) = viewport.split_once('x').unwrap();
    Settings {
        mode: Mode::Click,
        viewport: [width.parse().unwrap(), height.parse().unwrap()],
        camera: crate::model::Camera {
            zoom: if std::env::var_os("SCRUTIVUE_SINGLE_PROFILE_100").is_some() {
                crate::model::Zoom::Fixed(1.)
            } else {
                crate::model::Zoom::Fit
            },
            ..Default::default()
        },
        ..Default::default()
    }
}

fn frame_hash(frame: &Frame) -> String {
    let mut hash = Sha256::new();
    for value in &frame.rgb {
        hash.update(value.to_le_bytes());
    }
    format!("{:x}", hash.finalize())
}

#[test]
#[ignore = "requires SCRUTIVUE_PROFILE_PAIR_A, SCRUTIVUE_JXL_TEST_LIBRARY and SCRUTIVUE_SINGLE_PROFILE_OUTPUT"]
fn single_file_stage_profile() {
    let path = PathBuf::from(std::env::var_os("SCRUTIVUE_PROFILE_PAIR_A").unwrap());
    let output = PathBuf::from(std::env::var_os("SCRUTIVUE_SINGLE_PROFILE_OUTPUT").unwrap());
    assert!(!output.exists(), "Use a new profile output path");
    let threads: usize = std::env::var("SCRUTIVUE_SINGLE_PROFILE_THREADS")
        .unwrap_or("6".into())
        .parse()
        .unwrap();
    assert!((1..=16).contains(&threads));
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .unwrap();
    let library = Arc::new(
        jxl::Library::load(&PathBuf::from(
            std::env::var_os("SCRUTIVUE_JXL_TEST_LIBRARY").unwrap(),
        ))
        .unwrap(),
    );
    let job = Request {
        generation: 1,
        pair: [(1, path.clone()), (1, path.clone())],
        ready: [None, None],
        settings: settings(),
    };
    let start = Instant::now();
    let source = Source::open(&path).unwrap();
    let size = source.size;
    let total = source.tiles.len();
    let open_ms = start.elapsed().as_secs_f64() * 1000.;
    let mut states = HashMap::from([(
        1,
        State {
            source,
            full: None,
            pending: None,
        },
    )]);
    let (tx, rx) = bounded(8);
    let mut difference = difference::Preview::default();
    let mut last = None;
    let mut drain = || {
        while let Ok(event) = rx.try_recv() {
            if let Event::Snapshot { primary, .. } = event {
                last = Some(primary);
            }
        }
    };
    let mut published = Instant::now() - Duration::from_secs(1);
    let mut dirty = false;
    let (mut plan_ms, mut decode_ms, mut compose_ms) = (0., 0., 0.);
    let (mut steps, mut coarse_steps, mut batches, mut snapshots) = (0, 0, 0, 0);
    let mut first_snapshot_done_ms = None;
    let previews = std::env::var_os("SCRUTIVUE_SINGLE_PROFILE_NO_PREVIEWS").is_none();
    loop {
        let tick = Instant::now();
        let plans = next_round(
            &states,
            &job,
            start.elapsed() >= Duration::from_millis(250),
            threads,
            true,
        );
        plan_ms += tick.elapsed().as_secs_f64() * 1000.;
        if plans.is_empty() {
            if start.elapsed() < Duration::from_millis(250) {
                thread::sleep(Duration::from_millis(1));
                continue;
            }
            break;
        }
        assert_eq!(plans.len(), 1);
        match plans[0].work {
            Work::Step { coarse, .. } => {
                steps += 1;
                coarse_steps += usize::from(coarse);
            }
            Work::Batch(_) => batches += 1,
        }
        let tick = Instant::now();
        execute_round(&mut states, &plans, &pool, Some(&library), &|| false).unwrap();
        decode_ms += tick.elapsed().as_secs_f64() * 1000.;
        dirty |= plans.iter().any(|plan| plan.visible);
        if previews && published.elapsed() >= Duration::from_millis(40) {
            if dirty {
                let tick = Instant::now();
                publish(&tx, &job, &states, &mut difference, &pool);
                compose_ms += tick.elapsed().as_secs_f64() * 1000.;
                snapshots += 1;
                dirty = false;
                first_snapshot_done_ms.get_or_insert_with(|| start.elapsed().as_secs_f64() * 1000.);
            } else {
                publish_progress(&tx, &job, &states);
            }
            drain();
            published = Instant::now();
        }
    }
    if previews && dirty {
        let tick = Instant::now();
        publish(&tx, &job, &states, &mut difference, &pool);
        compose_ms += tick.elapsed().as_secs_f64() * 1000.;
        snapshots += 1;
        drain();
    }
    drop(drain);
    let tick = Instant::now();
    let frames = finish_round(&mut states, [1, 1], &pool, &|| false).unwrap();
    let finish_ms = tick.elapsed().as_secs_f64() * 1000.;
    let full_ms = start.elapsed().as_secs_f64() * 1000.;
    assert_eq!(frames.len(), 1);
    assert!(states[&1].source.unchanged());
    let frame_hash = frame_hash(&frames[0].1);
    if let Some(last) = last {
        let expected = pixels(&states[&1], size, &job.settings);
        assert_eq!(
            last.as_bytes(0).unwrap(),
            expected,
            "Single preview differs from the completed frame"
        );
    }
    let report = serde_json::json!({"success":true,"source":path,"threads":threads,"viewport":job.settings.viewport,
        "fit":matches!(job.settings.camera.zoom,crate::model::Zoom::Fit),"previews":previews,"total_tiles":total,
        "open_ms":open_ms,"plan_ms":plan_ms,"decode_ms":decode_ms,"compose_ms":compose_ms,"finish_ms":finish_ms,
        "full_ready_ms":full_ms,"first_snapshot_publish_done_ms":first_snapshot_done_ms,"steps":steps,
        "coarse_steps":coarse_steps,"batches":batches,"snapshots":snapshots,"size":size,"rgb16_sha256":frame_hash,
        "source_unchanged":true,"scope":"Production single-source planning, decoding, snapshot and completion functions. Pool size is a test-only override; source stat/open and completion included, reference/hash excluded. Not GPU or screen latency. Publish-done includes deferred header qualification."});
    std::fs::create_dir_all(output.parent().unwrap()).unwrap();
    std::fs::write(output, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    println!("{report}");
}

#[test]
#[ignore = "requires SCRUTIVUE_PROFILE_PAIR_A, installed libjxl and SCRUTIVUE_SINGLE_PROFILE_OUTPUT"]
fn single_file_actual_worker_profile() {
    let path = PathBuf::from(std::env::var_os("SCRUTIVUE_PROFILE_PAIR_A").unwrap());
    let output = PathBuf::from(std::env::var_os("SCRUTIVUE_SINGLE_PROFILE_OUTPUT").unwrap());
    assert!(!output.exists(), "Use a new profile output path");
    assert!(jxl::Library::installed().is_ok());
    let settings = settings();
    let worker = Worker::new();
    let start = Instant::now();
    worker.request(Request {
        generation: 1,
        pair: [(1, path.clone()), (1, path.clone())],
        ready: [None, None],
        settings: settings.clone(),
    });
    let (mut first_snapshot_ms, mut snapshots, mut last) = (None, 0, None);
    let frame = loop {
        assert!(start.elapsed() < Duration::from_secs(120));
        match worker.events.recv_timeout(Duration::from_secs(30)).unwrap() {
            Event::Snapshot {
                primary, alternate, ..
            } => {
                assert!(alternate.is_none());
                first_snapshot_ms.get_or_insert_with(|| start.elapsed().as_secs_f64() * 1000.);
                last = Some(primary);
                snapshots += 1;
            }
            Event::Complete { id, frame } => {
                assert_eq!(id, 1);
                break frame;
            }
            Event::Unsupported { reason, .. } => panic!("{reason}"),
            Event::Progress { .. } => {}
        }
    };
    let full_ms = start.elapsed().as_secs_f64() * 1000.;
    worker.clear();
    let source = Source::open(&path).unwrap();
    let state = State {
        source,
        full: Some(frame.clone()),
        pending: None,
    };
    assert_eq!(
        last.unwrap().as_bytes(0).unwrap(),
        pixels(&state, frame.size(), &settings)
    );
    let report = serde_json::json!({"success":true,"actual_worker":true,"source":path,
        "viewport":settings.viewport,"fit":matches!(settings.camera.zoom,crate::model::Zoom::Fit),
        "full_ready_ms":full_ms,"first_snapshot_ms":first_snapshot_ms,"snapshots":snapshots,
        "rgb16_sha256":frame_hash(&frame),"scope":"Actual asynchronous single-source worker, including source opening and completion. Reference/hash excluded. Not GPU or screen latency."});
    std::fs::create_dir_all(output.parent().unwrap()).unwrap();
    std::fs::write(output, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    println!("{report}");
}
