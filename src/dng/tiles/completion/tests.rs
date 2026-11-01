use super::*;

fn calibrated(name: &str, little: bool, orientation: u32, camera_wb: bool, black: u32) -> Source {
    let path = fixture(name, 65, 49);
    let e = Endian(little);
    let mut bytes = super::super::super::tests::synthetic_size(e, false, 65, 49);
    let tiff = Tiff::parse(&bytes).unwrap().unwrap();
    let ifd = tiff.jxl_raw().unwrap().unwrap();
    let mut entries = ifd.entries.clone();
    let mut append = |tag: u16, kind: u16, count: u32, payload: Vec<u8>| {
        let mut entry = [0; 12];
        entry[..2].copy_from_slice(&e.put16(tag));
        entry[2..4].copy_from_slice(&e.put16(kind));
        entry[4..8].copy_from_slice(&e.put32(count));
        if payload.len() <= 4 {
            entry[8..8 + payload.len()].copy_from_slice(&payload);
        } else {
            entry[8..12].copy_from_slice(&e.put32(bytes.len() as u32));
            bytes.extend(payload);
        }
        entries.push(entry);
    };
    append(274, 3, 1, e.put16(orientation as u16).to_vec());
    append(50717, 4, 1, e.put32(65535).to_vec());
    append(50714, 5, 3, [e.put32(black), e.put32(1)].repeat(3).concat());
    if camera_wb {
        append(50728, 5, 3, [e.put32(1); 6].concat());
    }
    entries.sort_by_key(|entry| e.u16(entry));
    let root = bytes.len() as u32;
    bytes[4..8].copy_from_slice(&e.put32(root));
    bytes.extend(e.put16(entries.len() as u16));
    for entry in entries {
        bytes.extend(entry);
    }
    bytes.extend(e.put32(0));
    std::fs::write(&path, bytes).unwrap();
    Source::open(&path).unwrap()
}

fn populate(source: &mut Source, peak: u16, bright_padding: bool) {
    for index in 0..source.tiles.len() {
        let origin = source.origin(index);
        let size = if !bright_padding && index % 2 == 1 {
            [
                16.min(source.raw_size[0] - origin[0]),
                16.min(source.raw_size[1] - origin[1]),
            ]
        } else {
            [16, 16]
        };
        let mut raw = Vec::new();
        for y in 0..size[1] {
            for x in 0..size[0] {
                for c in 0..3 {
                    let p = [origin[0] + x, origin[1] + y];
                    let value = if bright_padding
                        && (p[0] >= source.raw_size[0] || p[1] >= source.raw_size[1])
                    {
                        65535
                    } else if p == [0, 0] {
                        peak
                    } else {
                        ((p[0] * 379 + p[1] * 919 + c * 1703) % u32::from(peak)) as u16
                    };
                    raw.push(value);
                }
            }
        }
        let display = source.develop(&raw, size, 1, &|| false).unwrap();
        source.tiles[index] = Some(Tile {
            raw,
            raw_size: size,
            display,
            ratio: 1,
        });
    }
}

fn with_tags(source: Source, tags: &[(u16, Vec<u32>)]) -> Source {
    let mut bytes = source.map.to_vec();
    let path = source.path.clone();
    let tiff = Tiff::parse(&bytes).unwrap().unwrap();
    let e = tiff.endian;
    let mut entries = tiff.jxl_raw().unwrap().unwrap().entries.clone();
    drop(source);
    for (tag, values) in tags {
        entries.retain(|entry| e.u16(entry) != *tag);
        let mut entry = [0; 12];
        entry[..2].copy_from_slice(&e.put16(*tag));
        entry[2..4].copy_from_slice(&e.put16(4));
        entry[4..8].copy_from_slice(&e.put32(values.len() as u32));
        let payload: Vec<_> = values.iter().flat_map(|&v| e.put32(v)).collect();
        if payload.len() <= 4 {
            entry[8..8 + payload.len()].copy_from_slice(&payload);
        } else {
            entry[8..12].copy_from_slice(&e.put32(bytes.len() as u32));
            bytes.extend(payload);
        }
        entries.push(entry);
    }
    entries.sort_by_key(|entry| e.u16(entry));
    let root = bytes.len() as u32;
    bytes[4..8].copy_from_slice(&e.put32(root));
    bytes.extend(e.put16(entries.len() as u16));
    for entry in entries {
        bytes.extend(entry);
    }
    bytes.extend(e.put32(0));
    std::fs::write(&path, bytes).unwrap();
    Source::open(&path).unwrap()
}

#[test]
fn header_qualification_is_lazy_and_can_be_warmed_after_the_first_snapshot() {
    let source = calibrated("lazy-completion-policy", true, 1, true, 0);
    assert!(source.pointwise_white.get().is_none());
    source.prepare_completion_policy();
    assert_eq!(source.pointwise_white.get(), Some(&Some(65535)));
    source.prepare_completion_policy();
    assert_eq!(source.completion_white(), Some(65535));
}

#[test]
fn certified_tiles_ignore_default_crop_and_match_full_active_rgb16_for_all_orientations() {
    for little in [true, false] {
        for orientation in 1..=8 {
            let mut source = calibrated(
                &format!("certified-{little}-{orientation}"),
                little,
                orientation,
                true,
                0,
            );
            assert_eq!(source.completion_white(), Some(65535));
            populate(&mut source, 65535, false);
            let legacy = source.finish_reference(&|| false).unwrap();
            let expected_size = if orientation >= 5 { [49, 65] } else { [65, 49] };
            assert_eq!(source.size, expected_size);
            let (frame, timings) = source.finish_profiled(&|| false).unwrap();
            assert_eq!(timings.path, "developed-tiles");
            assert_eq!(timings.development_ms, 0.);
            assert_eq!(frame.size(), legacy.size());
            assert_eq!(
                frame.rgb, legacy.rgb,
                "orientation {orientation}, little {little}"
            );
            source = with_tags(source, &[(50719, vec![3, 5]), (50720, vec![57, 39])]);
            assert_eq!(source.crop, [0, 0, 65, 49]);
            assert_eq!(source.size, expected_size);
            populate(&mut source, 65535, false);
            let with_crop = source.finish_reference(&|| false).unwrap();
            assert_eq!(with_crop.size(), legacy.size());
            assert_eq!(with_crop.rgb, legacy.rgb);
            let (joined, timings) = source.finish_profiled(&|| false).unwrap();
            assert_eq!(timings.path, "developed-tiles");
            assert_eq!(joined.size(), legacy.size());
            assert_eq!(joined.rgb, legacy.rgb);
            assert!(source.join_developed(&|| true).is_err());
            let visits = std::sync::atomic::AtomicUsize::new(0);
            assert!(source
                .join_developed(&|| visits.fetch_add(1, std::sync::atomic::Ordering::Relaxed) > 2)
                .is_err());
        }
    }
}

#[test]
fn active_margins_are_retained_in_stream_coordinates_and_full_completion() {
    for little in [true, false] {
        for orientation in 1..=8 {
            let source = calibrated(
                &format!("active-geometry-{little}-{orientation}"),
                little,
                orientation,
                true,
                0,
            );
            let mut source = with_tags(
                source,
                &[
                    (50829, vec![1, 1, 48, 64]),
                    (50719, vec![3, 5]),
                    (50720, vec![57, 39]),
                ],
            );
            assert_eq!(source.crop, [1, 1, 63, 47]);
            let expected = if orientation >= 5 { [47, 63] } else { [63, 47] };
            assert_eq!(source.size, expected);
            assert_eq!(dimensions(&source.map).unwrap(), Some(expected));
            for y in 0..expected[1] {
                for x in 0..expected[0] {
                    let [rx, ry] = source.raw_point(x, y);
                    assert!((1..64).contains(&rx) && (1..48).contains(&ry));
                }
            }
            populate(&mut source, 65535, false);
            let reference = source.finish_reference(&|| false).unwrap();
            let (frame, timings) = source.finish_profiled(&|| false).unwrap();
            assert_eq!(timings.path, "full-libraw");
            assert_eq!(frame.size(), expected);
            assert_eq!(frame.rgb, reference.rgb);
        }
    }
}

#[test]
fn multiple_raw_ifds_with_matching_geometry_and_calibration_do_not_qualify() {
    let source = calibrated("ambiguous-raw-frame", true, 1, true, 0);
    let path = source.path.clone();
    let mut bytes = source.map.to_vec();
    let tiff = Tiff::parse(&bytes).unwrap().unwrap();
    let ifd = tiff.jxl_raw().unwrap().unwrap();
    let end = ifd.offset + 2 + ifd.entries.len() * 12;
    let copied_ifd = bytes[ifd.offset..end + 4].to_vec();
    let next = bytes.len() as u32;
    drop(source);
    bytes[end..end + 4].copy_from_slice(&Endian(true).put32(next));
    bytes.extend(copied_ifd);
    std::fs::write(&path, bytes).unwrap();
    let source = Source::open(&path).unwrap();
    assert_eq!(source.completion_white(), None);
}

#[test]
fn active_margins_calibration_mismatch_and_wrong_orientation_do_not_qualify() {
    let mut source = calibrated("mismatched-calibration", true, 1, true, 0);
    source.orientation = 8;
    assert_eq!(source.match_completion_policy().unwrap(), None);
    source.orientation = 1;
    let wb = source
        .metadata
        .iter_mut()
        .find(|(entry, _)| Endian(true).u16(entry) == 50728)
        .unwrap();
    wb.1[..4].copy_from_slice(&Endian(true).put32(2));
    assert_eq!(source.match_completion_policy().unwrap(), None);
    let source = calibrated("active-margins", true, 1, true, 0);
    let source = with_tags(source, &[(50829, vec![1, 1, 48, 64])]);
    assert_eq!(source.completion_white(), None);
}

#[test]
fn global_normalization_threshold_and_exact_white_have_separate_outcomes() {
    let mut source = calibrated("global-threshold", true, 1, true, 0);
    for (peak, path) in [
        (49151, "developed-tiles"),
        (49152, "full-libraw"),
        (65535, "developed-tiles"),
    ] {
        populate(&mut source, peak, false);
        let (frame, timings) = source.finish_profiled(&|| false).unwrap();
        assert_eq!(timings.path, path);
        assert_eq!(frame.rgb, source.finish_reference(&|| false).unwrap().rgb);
    }
}

#[test]
fn global_normalization_falls_back_even_if_only_padded_edge_samples_reach_white() {
    for padding in [true, false] {
        let mut source = calibrated(&format!("global-normalization-{padding}"), true, 8, true, 0);
        assert_eq!(source.completion_white(), Some(65535));
        populate(&mut source, 60000, padding);
        let provisional = source.join_developed(&|| false).unwrap();
        let reference = source.finish_reference(&|| false).unwrap();
        assert_ne!(provisional.rgb, reference.rgb);
        let (frame, timings) = source.finish_profiled(&|| false).unwrap();
        assert_eq!(timings.path, "full-libraw");
        assert!(timings
            .fallback_reason
            .unwrap()
            .contains("Global white-level"));
        assert_eq!(frame.rgb, reference.rgb);
    }
}

#[test]
fn data_dependent_wb_and_nonzero_black_are_not_certified() {
    for (wb, black) in [(false, 0), (true, 100)] {
        let mut source = calibrated(&format!("uncertified-{wb}-{black}"), true, 1, wb, black);
        assert_eq!(source.completion_white(), None);
        populate(&mut source, 65535, false);
        let (frame, timings) = source.finish_profiled(&|| false).unwrap();
        assert_eq!(timings.path, "full-libraw");
        assert_eq!(frame.rgb, source.finish_reference(&|| false).unwrap().rgb);
    }
}

#[test]
fn coarse_missing_and_placeholder_tiles_are_never_promoted_to_analysis() {
    let mut source = calibrated("certification-readiness", true, 1, true, 0);
    populate(&mut source, 65535, false);
    assert!(source.finish_profiled(&|| true).is_err());
    source.tiles[0].as_mut().unwrap().ratio = 8;
    assert!(source.finish_profiled(&|| false).is_err());
    source.tiles[0].as_mut().unwrap().ratio = 1;
    source.tiles[0].as_mut().unwrap().display = Arc::new(Frame {
        width: 0,
        height: 0,
        rgb: Vec::new(),
    });
    assert_eq!(
        source.finish_profiled(&|| false).unwrap().1.path,
        "full-libraw"
    );
    source.tiles[0] = None;
    assert!(source.finish_profiled(&|| false).is_err());
}
