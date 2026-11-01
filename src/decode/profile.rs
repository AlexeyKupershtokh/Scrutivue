//! Read-only raster decode stages and actual asynchronous Single loading.
use super::*;
use image::ImageDecoder;
use sha2::{Digest, Sha256};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

fn hash(frame: &Frame) -> String {
    let mut hash = Sha256::new();
    #[cfg(target_endian = "little")]
    hash.update(bytemuck::cast_slice(&frame.rgb));
    #[cfg(target_endian = "big")]
    for value in &frame.rgb {
        hash.update(value.to_le_bytes());
    }
    format!("{:x}", hash.finalize())
}

#[test]
#[ignore = "requires SCRUTIVUE_JPEG_PROFILE_PATH and SCRUTIVUE_JPEG_PROFILE_OUTPUT"]
fn jpeg_loading_stage_profile() {
    let path = PathBuf::from(std::env::var_os("SCRUTIVUE_JPEG_PROFILE_PATH").unwrap());
    let output = PathBuf::from(std::env::var_os("SCRUTIVUE_JPEG_PROFILE_OUTPUT").unwrap());
    assert!(!output.exists(), "Use a new output filename");
    let threads: usize = std::env::var("SCRUTIVUE_JPEG_PROFILE_THREADS")
        .unwrap_or("6".into())
        .parse()
        .unwrap();
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .unwrap();
    let before = std::fs::metadata(&path).unwrap();
    let mut stages = Vec::new();
    let mut expected_hash = None;
    for _ in 0..3 {
        let (frame, report) = pool.install(|| {
            let start = Instant::now();
            let reader = image::ImageReader::open(&path)
                .unwrap()
                .with_guessed_format()
                .unwrap();
            let open_ms = start.elapsed().as_secs_f64() * 1000.;
            let tick = Instant::now();
            let mut decoder = reader.into_decoder().unwrap();
            let read_headers_ms = tick.elapsed().as_secs_f64() * 1000.;
            let tick = Instant::now();
            let orientation = decoder
                .orientation()
                .unwrap_or(image::metadata::Orientation::NoTransforms);
            let metadata_ms = tick.elapsed().as_secs_f64() * 1000.;
            let encoded_size = decoder.dimensions();
            let tick = Instant::now();
            let mut image = DynamicImage::from_decoder(decoder).unwrap();
            let codec_ms = tick.elapsed().as_secs_f64() * 1000.;
            let tick = Instant::now();
            image.apply_orientation(orientation);
            let orientation_ms = tick.elapsed().as_secs_f64() * 1000.;
            let tick = Instant::now();
            let rgb = image.into_rgb16();
            let conversion_ms = tick.elapsed().as_secs_f64() * 1000.;
            let frame = Frame {
                width: rgb.width(),
                height: rgb.height(),
                rgb: rgb.into_raw(),
            };
            let total_ms = start.elapsed().as_secs_f64() * 1000.;
            (
                frame,
                serde_json::json!({"open_ms":open_ms,"read_headers_ms":read_headers_ms,
                "metadata_ms":metadata_ms,"codec_ms":codec_ms,"orientation_ms":orientation_ms,
                "conversion_ms":conversion_ms,"total_ms":total_ms,"encoded_size":encoded_size,
                "orientation":format!("{orientation:?}")}),
            )
        });
        let digest = hash(&frame);
        if let Some(expected) = &expected_hash {
            assert_eq!(&digest, expected);
        }
        expected_hash = Some(digest);
        stages.push(report);
    }
    let mut fused_preparation_ms = Vec::new();
    for _ in 0..3 {
        let (frame, ms) = pool.install(|| {
            let (image, orientation, is_jpeg) = raster_unoriented(&path).unwrap();
            assert!(is_jpeg);
            let tick = Instant::now();
            let frame = jpeg::frame(image, orientation, &|| false).unwrap();
            (frame, tick.elapsed().as_secs_f64() * 1000.)
        });
        assert_eq!(hash(&frame), expected_hash.as_ref().unwrap().as_str());
        fused_preparation_ms.push(ms);
    }
    let startup = Instant::now();
    let services = crate::services::Services::new(false, output.with_extension("thumbnails"));
    let backend = loop {
        match services
            .events
            .recv_timeout(Duration::from_secs(30))
            .unwrap()
        {
            crate::services::Event::Backend(backend) => break backend,
            crate::services::Event::Error(error) => panic!("{error}"),
            event => services.discard(event),
        }
    };
    let startup_ms = startup.elapsed().as_secs_f64() * 1000.;
    let start = Instant::now();
    services.decode(vec![(1, path.clone())], 1, vec![path.clone()]);
    let frame = loop {
        match services
            .events
            .recv_timeout(Duration::from_secs(30))
            .unwrap()
        {
            crate::services::Event::Decoded { id, result } => {
                assert_eq!(id, 1);
                break result.unwrap();
            }
            crate::services::Event::Error(error) => panic!("{error}"),
            event => services.discard(event),
        }
    };
    let decoded_ms = start.elapsed().as_secs_f64() * 1000.;
    services.renders.put(crate::services::RenderJob::Frame(
        crate::services::RenderRequest {
            generation: 1,
            a: (1, frame.clone()),
            b: (1, frame.clone()),
            settings: crate::model::Settings {
                viewport: [2560, 1440],
                mode: crate::model::Mode::Click,
                ..Default::default()
            },
        },
    ));
    let render_ms = loop {
        match services
            .events
            .recv_timeout(Duration::from_secs(30))
            .unwrap()
        {
            crate::services::Event::Rendered {
                elapsed,
                primary,
                alternate,
                ..
            } => {
                services.retire_images(std::iter::once(primary).chain(alternate).collect());
                break elapsed.as_secs_f64() * 1000.;
            }
            crate::services::Event::Error(error) => panic!("{error}"),
            event => services.discard(event),
        }
    };
    let ready_ms = start.elapsed().as_secs_f64() * 1000.;
    assert_eq!(hash(&frame), expected_hash.as_ref().unwrap().as_str());
    let after = std::fs::metadata(&path).unwrap();
    assert_eq!(before.len(), after.len());
    assert_eq!(before.modified().unwrap(), after.modified().unwrap());
    let report = serde_json::json!({"success":true,"source":path,"threads":threads,"stages":stages,
        "stage_path":"Legacy serial orientation/conversion reference; production loading uses fused JPEG preparation",
        "fused_preparation_ms":fused_preparation_ms,
        "services_startup_ms":startup_ms,"backend":backend,"services_decode_ms":decoded_ms,
        "render_worker_ms":render_ms,"services_render_ready_ms":ready_ms,"rgb16_sha256":expected_hash,
        "source_unchanged":true,"scope":"Legacy raster reference stages, isolated fused preparation, then actual asynchronous single-image decode/render. Warm file cache, 2560x1440 Fit. Codec stage includes RGB8 allocation/decode/free, orientation and RGB16 conversion separate. GPU startup excluded from ready timers; GPUI upload, scanner/thumbnail contention and screen presentation not timed."});
    std::fs::create_dir_all(output.parent().unwrap()).unwrap();
    std::fs::write(output, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    println!("{report}");
}
