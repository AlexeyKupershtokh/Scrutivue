//! Read-only full-ActiveArea regression against preserved 0.3.0 cropped RGB16 hashes.
use super::*;
use sha2::{Digest, Sha256};
use std::path::PathBuf;

#[test]
#[ignore = "requires SCRUTIVUE_DNG_GEOMETRY_BASELINE/OUTPUT and the requested read-only corpus"]
fn full_active_area_preserves_every_previous_default_crop_sample() {
    let baseline = PathBuf::from(std::env::var_os("SCRUTIVUE_DNG_GEOMETRY_BASELINE").unwrap());
    let output = PathBuf::from(std::env::var_os("SCRUTIVUE_DNG_GEOMETRY_OUTPUT").unwrap());
    assert!(!output.exists(), "Use a new output filename");
    let records: Vec<serde_json::Value> =
        serde_json::from_slice(&std::fs::read(baseline).unwrap()).unwrap();
    let mut rows = Vec::new();
    for previous in records {
        let path = PathBuf::from(previous["path"].as_str().unwrap());
        if !path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("dng"))
        {
            continue;
        }
        let bytes = std::fs::read(&path).unwrap();
        let before = Sha256::digest(&bytes);
        let tiff = Tiff::parse(&bytes).unwrap().unwrap();
        let ifd = tiff.jxl_raw().unwrap().unwrap();
        let raw_size = [
            tiff.value(ifd, 256, 0).unwrap(),
            tiff.value(ifd, 257, 0).unwrap(),
        ];
        let active = display_area(&tiff, ifd, raw_size).unwrap();
        let [x, y, mut w, mut h] = default_crop(&tiff, ifd, raw_size).unwrap().unwrap();
        let (mut x, mut y) = (x - active[0], y - active[1]);
        let (mut iw, mut ih) = (active[2], active[3]);
        let orientation = tiff
            .ifds
            .iter()
            .map(|ifd| tiff.value(ifd, 274, 1).unwrap())
            .find(|&orientation| orientation != 1)
            .unwrap_or(1);
        let flip = [0, 0, 1, 3, 2, 4, 6, 7, 5][orientation as usize];
        // Independent mapping of the historical recommended crop into the full
        // oriented ActiveArea; compare its complete byte stream, not a tolerance.
        if flip & 1 != 0 {
            x = iw - x - w;
        }
        if flip & 2 != 0 {
            y = ih - y - h;
        }
        if flip & 4 != 0 {
            std::mem::swap(&mut x, &mut y);
            std::mem::swap(&mut w, &mut h);
            std::mem::swap(&mut iw, &mut ih);
        }
        let frame = crate::decode::decode(&path).unwrap();
        assert_eq!(frame.size(), [iw, ih]);
        assert_eq!(frame.size(), [5520, 8280], "Use the requested corpus");
        assert_eq!(crate::decode::dimensions(&path).unwrap(), frame.size());
        let source = tiles::Source::open(&path).unwrap();
        assert_eq!(source.size, frame.size());
        assert_eq!(source.crop, active);
        assert_eq!(serde_json::json!([w, h]), previous["dimensions"]);
        let mut hash = Sha256::new();
        let mut sum = 0u64;
        for y in y..y + h {
            let start = (y as usize * frame.width as usize + x as usize) * 3;
            let row = &frame.rgb[start..start + w as usize * 3];
            assert!(cfg!(target_endian = "little"));
            hash.update(bytemuck::cast_slice::<u16, u8>(row));
            sum += row.iter().map(|&value| u64::from(value)).sum::<u64>();
        }
        let overlap_hash = format!("{:x}", hash.finalize());
        assert_eq!(
            overlap_hash,
            previous["rgb16_le_sha256"].as_str().unwrap(),
            "{}",
            path.display()
        );
        assert_eq!(serde_json::json!(sum), previous["sample_sum"]);
        assert_eq!(Sha256::digest(std::fs::read(&path).unwrap()), before);
        rows.push(serde_json::json!({"path":path,"dimensions":frame.size(),
            "rgb16_le_sha256":format!("{:x}", Sha256::digest(bytemuck::cast_slice(&frame.rgb))),
            "previous_crop_sha256":overlap_hash,"previous_crop_sample_sum":sum,
            "overlap_samples_equal":true,"source_unchanged":true}));
    }
    assert_eq!(
        rows.len(),
        36,
        "Use the preserved requested-corpus baseline"
    );
    std::fs::write(
        output,
        serde_json::to_vec_pretty(&serde_json::json!({"success":true,"records":rows})).unwrap(),
    )
    .unwrap();
}
