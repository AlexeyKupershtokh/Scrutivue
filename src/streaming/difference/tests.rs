use super::*;
use crate::{
    model::{Camera, Mode, Zoom},
    processing::Cpu,
    streaming::tests::image_state,
};

#[test]
fn incremental_difference_matches_full_cpu_pyramids_and_scaled_pair_alignment() {
    for bsize in [[65, 49], [47, 37]] {
        let (a, af) = image_state(&format!("diff-a-{}", bsize[0]), [65, 49], |x, y| {
            [
                (x * 911 + y * 131) as u16,
                (x * 17 + y * 1181) as u16,
                (x * 751 + y * 41) as u16,
            ]
        });
        let (b, bf) = image_state(&format!("diff-b-{}", bsize[0]), bsize, |x, y| {
            [
                (x * 761 + y * 311) as u16,
                (x * 37 + y * 1091) as u16,
                (x * 641 + y * 71) as u16,
            ]
        });
        let mut preview = Preview::default();
        let mut cpu = Cpu::new();
        for zoom in [0.25, 0.37, 0.5, 0.73, 1., 1.8, 4., 8.] {
            for gain in [1, 16, 256] {
                let settings = Settings {
                    mode: Mode::Diff,
                    gain,
                    viewport: [31, 23],
                    camera: Camera {
                        zoom: Zoom::Fixed(zoom),
                        center: [0.43, 0.57],
                    },
                    ..Settings::default()
                };
                let (actual, ready) = preview.render([1, 2], &a, &b, &settings);
                assert!(ready > 0);
                let expected = cpu.render((1, &af), (2, &bf), &settings, false);
                let error = actual
                    .iter()
                    .zip(&expected)
                    .map(|(a, b)| a.abs_diff(*b))
                    .max()
                    .unwrap();
                assert!(
                    error <= 1,
                    "size={bsize:?}, zoom={zoom}, gain={gain}, max error={error}"
                );
            }
        }
    }
}

#[test]
fn missing_and_coarse_tiles_are_masked_until_the_matching_pair_is_ready() {
    let (a, _) = image_state("masked-a", [128, 96], |_, _| [12345; 3]);
    let (mut b, _) = image_state("masked-b", [128, 96], |_, _| [12345; 3]);
    let settings = Settings {
        mode: Mode::Diff,
        gain: 256,
        viewport: [32, 32],
        camera: Camera {
            zoom: Zoom::Fixed(1.),
            ..Default::default()
        },
        ..Settings::default()
    };
    let index = b.source.index_at(b.source.raw_point(64, 48));
    let tile = b.source.tiles[index].take();
    let mut preview = Preview::default();
    let (pixels, partial) = preview.render([1, 2], &a, &b, &settings);
    assert!(partial > 0 && partial < 32 * 32);
    let p = (16 * 32 + 16) * 4;
    assert_eq!(&pixels[p..p + 4], &[30, 30, 30, 255]);
    b.source.tiles[index] = tile;
    b.source.tiles[index].as_mut().unwrap().ratio = 8;
    let (_, coarse) = preview.render([1, 2], &a, &b, &settings);
    assert_eq!(coarse, partial);
    b.source.tiles[index].as_mut().unwrap().ratio = 1;
    let (pixels, ready) = preview.render([1, 2], &a, &b, &settings);
    assert_eq!(ready, 32 * 32);
    assert!(pixels.chunks_exact(4).all(|p| p == [0, 0, 0, 255]));
}

#[test]
fn difference_is_formed_before_downsampling_not_between_downsampled_images() {
    let (a, _) = image_state("diff-checker-a", [128, 96], |x, y| {
        [if (x + y) & 1 == 0 { 65535 } else { 0 }; 3]
    });
    let (b, _) = image_state("diff-checker-b", [128, 96], |x, y| {
        [if (x + y) & 1 == 0 { 0 } else { 65535 }; 3]
    });
    let settings = Settings {
        mode: Mode::Diff,
        gain: 1,
        viewport: [16, 12],
        camera: Camera {
            zoom: Zoom::Fixed(0.125),
            ..Default::default()
        },
        ..Settings::default()
    };
    let (pixels, ready) = Preview::default().render([1, 2], &a, &b, &settings);
    assert_eq!(ready, 16 * 12);
    assert!(pixels.chunks_exact(4).all(|p| p == [255; 4]));
}

#[test]
fn unavailable_mip_cells_are_retried_and_full_development_invalidates_preview_cells() {
    let (mut a, _) = image_state("mip-retry-a", [128, 96], |_, _| [12345; 3]);
    let (mut b, _) = image_state("mip-retry-b", [128, 96], |_, _| [12345; 3]);
    let settings = Settings {
        mode: Mode::Diff,
        gain: 1,
        viewport: [32, 32],
        camera: Camera {
            zoom: Zoom::Fixed(0.5),
            ..Default::default()
        },
        ..Settings::default()
    };
    let index = b.source.index_at(b.source.raw_point(64, 48));
    let missing = b.source.tiles[index].take();
    let mut preview = Preview::default();
    let (_, partial) = preview.render([1, 2], &a, &b, &settings);
    assert!(partial > 0 && partial < 32 * 32);
    b.source.tiles[index] = missing;
    let (pixels, ready) = preview.render([1, 2], &a, &b, &settings);
    assert_eq!(ready, 32 * 32);
    assert!(pixels.chunks_exact(4).all(|p| p == [0, 0, 0, 255]));
    a.full = Some(std::sync::Arc::new(crate::decode::Frame {
        width: 128,
        height: 96,
        rgb: vec![40000; 128 * 96 * 3],
    }));
    let (pixels, ready) = preview.render([1, 2], &a, &b, &settings);
    assert_eq!(ready, 32 * 32);
    assert!(pixels.chunks_exact(4).all(|p| p[0] > 0));
}

#[test]
fn ready_a_and_partial_b_are_byte_identical_with_one_or_six_render_workers() {
    let one = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .unwrap();
    let six = rayon::ThreadPoolBuilder::new()
        .num_threads(6)
        .build()
        .unwrap();
    for bsize in [[65, 49], [47, 37]] {
        let (mut a, af) = image_state(
            &format!("parallel-ready-a-{}", bsize[0]),
            [65, 49],
            |x, y| [(x * 917 + y * 113) as u16; 3],
        );
        let (mut b, bf) = image_state(
            &format!("parallel-partial-b-{}", bsize[0]),
            bsize,
            |x, y| [(x * 233 + y * 1259) as u16; 3],
        );
        a.full = Some(af);
        a.source.tiles.iter_mut().for_each(|tile| *tile = None);
        let count = b.source.tiles.len();
        let mut tiles = std::mem::replace(&mut b.source.tiles, (0..count).map(|_| None).collect());
        let (mut serial, mut parallel) = (Preview::default(), Preview::default());
        for stage in 0..5 {
            match stage {
                1 => b.source.tiles[0] = tiles[0].take(),
                2 => b.source.tiles[0].as_mut().unwrap().ratio = 8,
                3 => {
                    b.source.tiles[0].as_mut().unwrap().ratio = 1;
                    for (slot, saved) in b.source.tiles.iter_mut().zip(&mut tiles).skip(1) {
                        *slot = saved.take();
                    }
                }
                4 => {
                    b.full = Some(bf.clone());
                    b.source.tiles.iter_mut().for_each(|tile| *tile = None);
                }
                _ => {}
            }
            for zoom in [0.25, 0.37, 1., 4.] {
                let settings = Settings {
                    mode: Mode::Diff,
                    viewport: [79, 53],
                    gain: 16,
                    camera: Camera {
                        zoom: Zoom::Fixed(zoom),
                        center: [0.43, 0.57],
                    },
                    ..Default::default()
                };
                let (av, bv) = (a.samples(), b.samples());
                let expected = one.install(|| serial.render_samples([1, 2], av, bv, &settings));
                let actual = six.install(|| parallel.render_samples([1, 2], av, bv, &settings));
                assert_eq!(actual, expected, "b={bsize:?}, stage={stage}, zoom={zoom}");
                if stage == 0 || stage == 2 {
                    assert_eq!(actual.1, 0);
                }
            }
        }
    }
}
