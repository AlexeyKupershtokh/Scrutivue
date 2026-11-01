use super::*;
use crate::model::{Camera, Zoom};

#[test]
fn single_worker_budget_is_bounded_and_never_changes_the_paired_budget() {
    for logical in [1usize, 2, 4, 6, 8, 12, 16, 32, 128] {
        let paired = logical.div_ceil(2).clamp(2, 6);
        assert_eq!(worker_threads(logical, false), paired);
        let single = worker_threads(logical, true);
        assert!(single >= paired && single <= 12);
        if cfg!(feature = "experimental-single-parallel") {
            assert_eq!(single, logical.saturating_sub(2).clamp(2, 12).max(paired));
        } else {
            assert_eq!(single, paired);
        }
    }
}

#[test]
fn single_preview_rows_match_serial_bytes_for_partial_coarse_and_full_sources() {
    let one = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .unwrap();
    let six = rayon::ThreadPoolBuilder::new()
        .num_threads(6)
        .build()
        .unwrap();
    let (mut state, frame) = tests::image_state("single-preview-rows", [65, 49], |x, y| {
        [
            (x * 917 + y * 113) as u16,
            (x * 47 + y * 1259) as u16,
            (x * 523 + y * 11) as u16,
        ]
    });
    for stage in 0..4 {
        match stage {
            1 => state.source.tiles[0] = None,
            2 => {
                let tile = state.source.tiles[1].as_mut().unwrap();
                tile.ratio = 8;
                tile.display = Arc::new(Frame {
                    width: 2,
                    height: 2,
                    rgb: vec![12345; 12],
                });
            }
            3 => {
                state.full = Some(frame.clone());
                state.source.tiles.iter_mut().for_each(|tile| *tile = None);
            }
            _ => {}
        }
        for viewport in [[79, 53], [1, 1], [128, 128], [0, 0]] {
            for reference in [[65, 49], [47, 37]] {
                for zoom in [
                    Zoom::Fit,
                    Zoom::Fixed(0.37),
                    Zoom::Fixed(1.),
                    Zoom::Fixed(4.),
                ] {
                    let settings = Settings {
                        viewport,
                        camera: Camera {
                            zoom,
                            center: [0.43, 0.57],
                        },
                        ..Default::default()
                    };
                    let expected = pixels(&state, reference, &settings);
                    let samples = state.samples();
                    let serial_pool =
                        one.install(|| pixels_from_samples(samples, reference, &settings, true));
                    let parallel_pool =
                        six.install(|| pixels_from_samples(samples, reference, &settings, true));
                    assert_eq!(serial_pool, expected, "one worker, stage={stage}");
                    assert_eq!(parallel_pool, expected, "six workers, stage={stage}");
                }
            }
        }
    }
}

#[cfg(feature = "experimental-single-parallel")]
#[test]
fn single_progressive_keeps_the_first_coarse_update_then_batches_without_changing_pairs() {
    let settings = Settings {
        mode: Mode::Click,
        viewport: [32, 16],
        ..Default::default()
    };
    let (mut states, mut job) = tests::empty_pair("single-progressive-batch", settings);
    states
        .values_mut()
        .for_each(|state| state.source.progressive_hint = true);
    let paired = next_round(&states, &job, false, 6, true);
    assert_eq!(paired.len(), 2);
    assert!(paired
        .iter()
        .all(|plan| matches!(plan.work, Work::Step { coarse: true, .. })));
    job.pair[1] = job.pair[0].clone();
    states.remove(&2);
    let first = next_round(&states, &job, false, 6, true);
    let index = match first[0].work {
        Work::Step {
            index,
            coarse: true,
        } => index,
        _ => panic!("The first progressive update must stay coarse"),
    };
    states.get_mut(&1).unwrap().source.tiles[index] = Some(Tile {
        raw: Vec::new(),
        raw_size: [16, 16],
        ratio: 8,
        display: Arc::new(Frame {
            width: 2,
            height: 2,
            rgb: vec![123; 12],
        }),
    });
    let next = next_round(&states, &job, false, 6, true);
    assert!(
        matches!(&next[0].work, Work::Batch(indices) if indices.contains(&index) && indices.len() == 24)
    );
    // A retained partial source and a fresh viewport must not restart the
    // one-session-per-tile loop. Offscreen rounds obey the same batching policy.
    let state = states.get_mut(&1).unwrap();
    state.source.tiles[index].as_mut().unwrap().ratio = 1;
    let later = next_round(&states, &job, true, 6, true);
    assert!(
        matches!(&later[0].work, Work::Batch(indices) if !indices.contains(&index) && indices.len() > 1)
    );
}

#[cfg(feature = "experimental-single-parallel")]
#[test]
#[ignore = "requires SCRUTIVUE_PROFILE_PAIR_A and SCRUTIVUE_JXL_TEST_LIBRARY"]
fn native_progressive_session_can_switch_single_and_paired_worker_budgets() {
    let path = PathBuf::from(std::env::var_os("SCRUTIVUE_PROFILE_PAIR_A").unwrap());
    let library = Arc::new(
        jxl::Library::load(&PathBuf::from(
            std::env::var_os("SCRUTIVUE_JXL_TEST_LIBRARY").unwrap(),
        ))
        .unwrap(),
    );
    let source = Source::open(&path).unwrap();
    let paired = rayon::ThreadPoolBuilder::new()
        .num_threads(6)
        .build()
        .unwrap();
    let single = rayon::ThreadPoolBuilder::new()
        .num_threads(10)
        .build()
        .unwrap();
    for index in [0, source.tiles.len() / 2, source.tiles.len() - 1] {
        for start_single in [false, true] {
            let mut reference = source.session(index, library.clone()).unwrap();
            let mut moving = source.session(index, library.clone()).unwrap();
            for (stage, ratio) in [8, 4, 2, 1].into_iter().enumerate() {
                let pool = if (stage & 1 == 0) == start_single {
                    &single
                } else {
                    &paired
                };
                let expected = paired
                    .install(|| reference.advance(ratio, &|| false))
                    .unwrap();
                assert!(pool.install(|| moving.advance(ratio, &|| true)).is_err());
                let actual = pool.install(|| moving.advance(ratio, &|| false)).unwrap();
                assert_eq!(actual, expected);
                assert_eq!(
                    moving.pixels, reference.pixels,
                    "tile={index}, ratio={ratio}"
                );
            }
        }
    }
}
