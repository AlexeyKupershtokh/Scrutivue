//! Public libjxl progressive decoding experiment, not an application decoder.
//!
//! libjxl flushes progressive samples into a full-resolution buffer. An
//! intended ratio of 8, 4 or 2 describes detail, not allocated/output dimensions.
//! Preview samples are approximate and must never replace exact metrics/picker
//! data. No runner, display conversion, private ABI, crop or scale API is used.

use super::*;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const FRAME_PROGRESSION: i32 = 0x8000;
const LAST_PASSES: i32 = 2;
const PASSES: i32 = 3;
const MAX_EVENTS: usize = 4096;

type SetProgressiveDetail = unsafe extern "C" fn(*mut c_void, i32) -> i32;
type IntendedRatio = unsafe extern "C" fn(*mut c_void) -> usize;
type FlushImage = unsafe extern "C" fn(*mut c_void) -> i32;

/// The returned image remains full size, even when `intended_ratio` is 2/4/8.
/// Downsampling for display is a separate operation, not native scaled decode.
pub struct PreviewResult {
    pub samples: Vec<u16>,
    pub width: usize,
    pub height: usize,
    pub intended_ratio: usize,
    pub is_partial: bool,
    /// Decoder setup through the selected successful flush/full-image event.
    /// Excludes destruction and report construction; neither performs I/O.
    pub elapsed_ms: f64,
    pub report: Value,
}

struct Event {
    name: &'static str,
    status: i32,
    ratio: Option<usize>,
    time_ms: f64,
    flush_status: Option<i32>,
    flush_ms: Option<f64>,
    unconsumed_bytes: Option<usize>,
}

/// Flush every available progressive step, then finish the SAME decoder.
/// Compare its final samples with a separate ordinary `JxlWorker` decode.
pub fn probe_progressive(
    library: &JxlLibrary,
    bytes: &[u8],
    channels: usize,
    max_width: usize,
    max_height: usize,
) -> Result<Value> {
    let mut result = decode_progressive(library, bytes, channels, max_width, max_height, None, false)?;
    let hash_started = Instant::now();
    let full_hash = sample_hash(&result.samples);
    let hash_ms = milliseconds(hash_started);
    let baseline_started = Instant::now();
    let mut worker = JxlWorker::new(library, false, false)?;
    let (baseline, dimensions, _) = worker.decode(bytes, channels, max_width, max_height)?;
    let baseline_ms = milliseconds(baseline_started);
    let baseline_hash = sample_hash(baseline);
    let matches = dimensions == [result.width, result.height] && baseline == result.samples;
    let report = result.report.as_object_mut().context("Progressive report is not an object")?;
    report.insert("full_hash".into(), json!(full_hash));
    report.insert("hash_format".into(), json!("SHA-256 of interleaved unsigned-16 little-endian samples"));
    report.insert("full_hash_ms".into(), json!(hash_ms));
    report.insert("baseline_hash".into(), json!(baseline_hash));
    report.insert("baseline_decode_ms".into(), json!(baseline_ms));
    report.insert("matches_full_baseline".into(), json!(matches));
    Ok(result.report)
}

/// Stop at the first successful progression with ratio <= `maximum_ratio`, or
/// return the completed full image if the encoding has no such progression.
/// For example 4 accepts quarter detail, half detail, or full detail. Ratio 1
/// may still be partial: only FULL_IMAGE is authoritative completion.
///
/// This intentionally destroys the stopped decoder; `probe_progressive` is the
/// separate continuation test. All samples still occupy full-size storage.
pub fn preview_only(
    library: &JxlLibrary,
    bytes: &[u8],
    channels: usize,
    max_width: usize,
    max_height: usize,
    maximum_ratio: usize,
) -> Result<PreviewResult> {
    ensure!(matches!(maximum_ratio, 1 | 2 | 4 | 8), "Expected maximum progressive ratio 1, 2, 4 or 8");
    decode_progressive(library, bytes, channels, max_width, max_height, Some(maximum_ratio), false)
}

/// Reproduce djxl's max_downsampling loop for the supported single-frame inputs:
/// kLastPasses, no intermediate flush, one flush at ratio <= maximum, then stop.
/// Ratio 1 requests ordinary full decoding without progression subscriptions.
/// This uses djxl v0.11.2/v0.12.0's public-API control flow, not the djxl binary.
pub fn downsample_like_djxl(
    library: &JxlLibrary,
    bytes: &[u8],
    channels: usize,
    max_width: usize,
    max_height: usize,
    maximum_ratio: usize,
) -> Result<PreviewResult> {
    ensure!(matches!(maximum_ratio, 1 | 2 | 4 | 8), "Expected djxl downsampling 1, 2, 4 or 8");
    decode_progressive(library, bytes, channels, max_width, max_height, Some(maximum_ratio), true)
}

fn decode_progressive(
    library: &JxlLibrary,
    bytes: &[u8],
    channels: usize,
    max_width: usize,
    max_height: usize,
    stop_ratio: Option<usize>,
    djxl_policy: bool,
) -> Result<PreviewResult> {
    ensure!(matches!(channels, 1 | 3), "Expected one or three JXL color channels");
    ensure!(max_width > 0 && max_height > 0, "Invalid JXL tile bounds");
    ensure!(!bytes.is_empty(), "Empty JXL tile");
    let started = Instant::now();
    let (detail, intended_ratio, flush): (SetProgressiveDetail, IntendedRatio, FlushImage) = unsafe {
        (
            symbol(&library._library, b"JxlDecoderSetProgressiveDetail\0")?,
            symbol(&library._library, b"JxlDecoderGetIntendedDownsamplingRatio\0")?,
            symbol(&library._library, b"JxlDecoderFlushImage\0")?,
        )
    };
    // Declare the output before the guard: every error destroys the decoder
    // before releasing the registered output allocation. Input is caller-owned.
    let mut output = Vec::<u16>::new();
    let decoder = Decoder {
        library,
        handle: NonNull::new(unsafe { (library.create)(std::ptr::null()) })
            .context("JxlDecoderCreate failed")?,
    };
    let handle = decoder.handle.as_ptr();
    let want_progression = !djxl_policy || stop_ratio != Some(1);
    let progression_detail = if djxl_policy { LAST_PASSES } else { PASSES };
    unsafe {
        let events = BASIC_INFO | COLOR_ENCODING | FRAME | FULL_IMAGE
            | if want_progression { FRAME_PROGRESSION } else { 0 };
        check((library.subscribe)(handle, events), "JxlDecoderSubscribeEvents(progressive)")?;
        check((library.keep_orientation)(handle, 1), "JxlDecoderSetKeepOrientation")?;
        if want_progression {
            check(detail(handle, progression_detail), "JxlDecoderSetProgressiveDetail")?;
        }
        check((library.set_input)(handle, bytes.as_ptr(), bytes.len()), "JxlDecoderSetInput")?;
    }
    // Deliberately do not CloseInput: ReleaseInput removes input, and SetInput
    // must restore its remaining suffix after each measured progression. The
    // public API permits no CloseInput when BOX events are not requested.
    let mut input_offset = 0;
    let mut info = BasicInfo::default();
    let mut dimensions = [0; 2];
    let mut got_basic = false;
    let mut got_color = false;
    let mut got_frame = false;
    let mut got_buffer = false;
    let mut got_full_image = false;
    let mut first_preview_ms = None;
    let mut full_ms = None;
    let mut output_bytes = 0;
    let mut final_unconsumed = None;
    let mut events = Vec::new();
    let mut completion = None;
    let format = PixelFormat {
        channels: channels as u32,
        data_type: 3,
        endianness: 0,
        align: 0,
    };
    for _ in 0..MAX_EVENTS {
        let status = unsafe { (library.process)(handle) };
        let mut event = Event {
            name: "",
            status,
            ratio: None,
            time_ms: milliseconds(started),
            flush_status: None,
            flush_ms: None,
            unconsumed_bytes: None,
        };
        match status {
            BASIC_INFO => {
                event.name = "basic_info";
                ensure!(!got_basic && !got_color && !got_frame, "Duplicate/out-of-order JXL basic info");
                unsafe { check((library.get_basic_info)(handle, &mut info), "JxlDecoderGetBasicInfo")? };
                dimensions = validate_basic(&info, channels, max_width, max_height)?;
                got_basic = true;
            }
            COLOR_ENCODING => {
                event.name = "color_encoding";
                ensure!(got_basic && !got_color && !got_frame, "Duplicate/out-of-order JXL color encoding");
                if info.uses_original_profile == 0 {
                    let mut original = ColorEncoding::default();
                    unsafe {
                        check((library.get_color)(handle, ORIGINAL_PROFILE, &mut original), "Original structured JXL profile unavailable (ICC-only XYB is unsupported)")?;
                    }
                    ensure!(original.color_space == if channels == 1 { 1 } else { 0 }, "Original JXL profile is not the requested gray/RGB color space");
                    unsafe { check((library.set_color)(handle, &original), "JxlDecoderSetPreferredColorProfile(original)")? };
                }
                got_color = true;
            }
            FRAME => {
                event.name = "frame";
                ensure!(got_color && !got_frame && !got_buffer, "Duplicate/out-of-order JXL frame");
                let mut frame = FrameHeader::default();
                unsafe { check((library.get_frame_header)(handle, &mut frame), "JxlDecoderGetFrameHeader")? };
                ensure!(frame.is_last == 1 && frame.duration == 0 && frame.timecode == 0, "Expected one complete non-animated JXL frame");
                ensure!(frame.layer_info.have_crop == 0 && frame.layer_info.xsize == info.xsize && frame.layer_info.ysize == info.ysize, "JXL frame dimensions do not match the full tile");
                got_frame = true;
            }
            NEED_IMAGE_OUT_BUFFER => {
                event.name = "need_image_out_buffer";
                ensure!(got_frame && !got_buffer && !got_full_image, "Duplicate/out-of-order JXL output request");
                let samples = dimensions[0].checked_mul(dimensions[1]).and_then(|n| n.checked_mul(channels)).context("JXL tile sample count overflow")?;
                output_bytes = samples.checked_mul(2).context("JXL output size overflow")?;
                ensure!(output_bytes <= isize::MAX as usize, "JXL output exceeds Rust allocation limit");
                let mut required_bytes = 0;
                unsafe { check((library.buffer_size)(handle, &format, &mut required_bytes), "JxlDecoderImageOutBufferSize")? };
                ensure!(required_bytes == output_bytes, "Unexpected JXL full-size output buffer requirement");
                output.try_reserve_exact(samples).context("Allocate progressive JXL output")?;
                output.resize(samples, 0);
                unsafe { check((library.set_buffer)(handle, &format, output.as_mut_ptr().cast(), output_bytes), "JxlDecoderSetImageOutBuffer")? };
                got_buffer = true;
            }
            FRAME_PROGRESSION => {
                event.name = "frame_progression";
                ensure!(got_buffer && !got_full_image, "Out-of-order JXL progression");
                let ratio = unsafe { intended_ratio(handle) };
                ensure!(matches!(ratio, 1 | 2 | 4 | 8), "Invalid JXL intended downsampling ratio {ratio}");
                event.ratio = Some(ratio);
                let should_stop = stop_ratio.is_some_and(|maximum| ratio <= maximum);
                // djxl does not reconstruct/flush earlier, coarser stages.
                if djxl_policy && !should_stop {
                    events.push(event);
                    continue;
                }
                let flush_started = Instant::now();
                let status = unsafe { flush(handle) };
                event.flush_ms = Some(milliseconds(flush_started));
                event.flush_status = Some(status);
                // ERROR means no preview was flushed, not a fatal decoder error.
                ensure!(matches!(status, SUCCESS | ERROR), "Unexpected JXL flush status {status}");
                if djxl_policy {
                    ensure!(status == SUCCESS, "JxlDecoderFlushImage failed at requested djxl stage");
                }
                let ready_ms = milliseconds(started);
                if status == SUCCESS {
                    first_preview_ms.get_or_insert(ready_ms);
                }
                if djxl_policy {
                    let remaining = unsafe { (library.release_input)(handle) };
                    ensure!(remaining <= bytes.len(), "Invalid JXL unconsumed-input length");
                    final_unconsumed = Some(remaining);
                    event.unconsumed_bytes = Some(remaining);
                } else {
                    event.unconsumed_bytes = Some(release_and_restore(library, handle, bytes, &mut input_offset)?);
                }
                events.push(event);
                if status == SUCCESS && should_stop {
                    completion = Some((true, ratio, ready_ms));
                    break;
                }
                continue;
            }
            FULL_IMAGE => {
                event.name = "full_image";
                event.ratio = Some(1);
                ensure!(got_buffer && !got_full_image, "Duplicate/out-of-order JXL full image");
                got_full_image = true;
                full_ms = Some(event.time_ms);
            }
            SUCCESS => {
                event.name = "success";
                ensure!(got_basic && got_color && got_frame && got_buffer && got_full_image, "JXL decoder finished without a complete output frame");
                let remaining = unsafe { (library.release_input)(handle) };
                ensure!(remaining <= bytes.len() - input_offset, "Invalid JXL unconsumed-input length");
                final_unconsumed = Some(remaining);
                event.unconsumed_bytes = Some(remaining);
                completion = Some((false, 1, full_ms.context("Missing JXL full-image time")?));
            }
            NEED_MORE_INPUT => bail!("Truncated JXL tile: decoder needs more input"),
            ERROR => bail!("libjxl rejected the tile or could not allocate decoder memory"),
            other => bail!("Unexpected libjxl decoder event/status {other:#x}"),
        }
        events.push(event);
        if completion.is_some() {
            break;
        }
    }
    let (is_partial, intended_ratio, elapsed_ms) = completion.context("libjxl progressive event limit exceeded")?;
    let destroy_started = Instant::now();
    drop(decoder);
    let destroy_ms = milliseconds(destroy_started);
    let events: Vec<Value> = events.into_iter().map(|event| json!({
        "event": event.name,
        "status": event.status,
        "ratio": event.ratio,
        "time_ms": event.time_ms,
        "flush_status": event.flush_status,
        "flush_ms": event.flush_ms,
        "unconsumed_bytes": event.unconsumed_bytes,
    })).collect();
    let report = json!({
        "library_version": library.version(),
        "progressive_detail": if !want_progression { "disabled" } else if djxl_policy { "kLastPasses" } else { "kPasses" },
        "policy": if djxl_policy { "djxl max_downsampling: flush only at matching stage" } else { "diagnostic: flush every kPasses stage" },
        "basic_info": {
            "width": dimensions[0], "height": dimensions[1],
            "bits_per_sample": info.bits_per_sample,
            "channels": info.num_color_channels,
            "uses_original_profile": info.uses_original_profile != 0,
            "have_preview": info.have_preview != 0,
            "preview_width": info.preview[0], "preview_height": info.preview[1],
            "have_animation": info.have_animation != 0,
            "orientation": info.orientation,
            "extra_channels": info.num_extra_channels,
        },
        "embedded_preview_decoded": false,
        "input_bytes": bytes.len(),
        "full_output_bytes": output_bytes,
        "native_scaled_output": false,
        "output_dimensions": dimensions,
        "first_preview_ms": first_preview_ms,
        "full_ms": full_ms,
        "elapsed_ms": elapsed_ms,
        "decoder_destroy_ms": destroy_ms,
        "is_partial": is_partial,
        "intended_ratio": intended_ratio,
        "requested_maximum_ratio": stop_ratio,
        "final_unconsumed_bytes": final_unconsumed,
        "consumed_bytes_at_stop": final_unconsumed.map(|remaining| bytes.len() - remaining),
        "same_decoder_continued": stop_ratio.is_none(),
        "events": events,
        "timing_scope": "Wall clock from export lookup/decoder setup to successful flush or full-image event; input already resident; no file I/O, baseline, hashing, report construction or destruction included",
    });
    Ok(PreviewResult { samples: output, width: dimensions[0], height: dimensions[1], intended_ratio, is_partial, elapsed_ms, report })
}

fn release_and_restore(library: &JxlLibrary, handle: *mut c_void, bytes: &[u8], offset: &mut usize) -> Result<usize> {
    let remaining = unsafe { (library.release_input)(handle) };
    ensure!(remaining <= bytes.len() - *offset, "Invalid JXL unconsumed-input length");
    *offset = bytes.len() - remaining;
    let suffix = &bytes[*offset..];
    unsafe { check((library.set_input)(handle, suffix.as_ptr(), suffix.len()), "JxlDecoderSetInput(restore remaining bytes)")? };
    Ok(remaining)
}

fn milliseconds(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1000.0
}

fn sample_hash(samples: &[u16]) -> String {
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 8192];
    for chunk in samples.chunks(buffer.len() / 2) {
        for (sample, bytes) in chunk.iter().zip(buffer.chunks_exact_mut(2)) {
            bytes.copy_from_slice(&sample.to_le_bytes());
        }
        hasher.update(&buffer[..chunk.len() * 2]);
    }
    format!("{:x}", hasher.finalize())
}

/// Generate deterministic RGB16 linear-BT.2100 texture, without photo data.
/// Uses one native encoder thread, VarDCT, distance 1 and effort 3. The
/// progressive flag explicitly enables/disables spectral AC progression.
/// Quantized AC and additional DC frames are disabled in both cases.
/// Dimensions are bounded to 64 megapixels for this experimental generator.
pub fn encode_synthetic(
    library: &JxlLibrary,
    width: usize,
    height: usize,
    progressive: bool,
) -> Result<Vec<u8>> {
    ensure!(width > 0 && height > 0 && width <= 16384 && height <= 16384, "Synthetic JXL dimensions must be in 1..=16384");
    let pixels = width.checked_mul(height).context("Synthetic JXL dimensions overflow")?;
    ensure!(pixels <= 64 * 1024 * 1024, "Synthetic JXL exceeds the 64-megapixel limit");
    let samples = pixels.checked_mul(3).context("Synthetic JXL samples overflow")?;
    let mut image = Vec::<u16>::new();
    image.try_reserve_exact(samples).context("Allocate synthetic RGB16")?;
    for y in 0..height {
        for x in 0..width {
            let gx = (x as u64 * 49151 / width.saturating_sub(1).max(1) as u64) as u32;
            let gy = (y as u64 * 49151 / height.saturating_sub(1).max(1) as u64) as u32;
            let mut noise = (x as u32).wrapping_mul(0x9e3779b9)
                ^ (y as u32).wrapping_mul(0x85ebca6b) ^ 0x243f6a88;
            noise ^= noise >> 16;
            noise = noise.wrapping_mul(0x7feb352d);
            noise ^= noise >> 15;
            let texture = (noise & 2047) + (((x / 13 + y / 11) & 1) as u32 * 2048);
            image.extend_from_slice(&[
                (4096 + gx + texture) as u16,
                (4096 + gy + texture) as u16,
                (4096 + (gx + gy) / 2 + texture) as u16,
            ]);
        }
    }
    struct Encoder<'a> {
        handle: NonNull<c_void>,
        destroy: Destroy,
        _library: &'a JxlLibrary,
    }
    impl Drop for Encoder<'_> {
        fn drop(&mut self) {
            unsafe { (self.destroy)(self.handle.as_ptr()) };
        }
    }
    unsafe {
        let lib = &library._library;
        let create: Create = symbol(lib, b"JxlEncoderCreate\0")?;
        let destroy: Destroy = symbol(lib, b"JxlEncoderDestroy\0")?;
        let init: unsafe extern "C" fn(*mut BasicInfo) = symbol(lib, b"JxlEncoderInitBasicInfo\0")?;
        let basic: unsafe extern "C" fn(*mut c_void, *const BasicInfo) -> i32 = symbol(lib, b"JxlEncoderSetBasicInfo\0")?;
        let color: SetColor = symbol(lib, b"JxlEncoderSetColorEncoding\0")?;
        let frame: unsafe extern "C" fn(*mut c_void, *const c_void) -> *mut c_void = symbol(lib, b"JxlEncoderFrameSettingsCreate\0")?;
        let option: unsafe extern "C" fn(*mut c_void, i32, i64) -> i32 = symbol(lib, b"JxlEncoderFrameSettingsSetOption\0")?;
        let distance: unsafe extern "C" fn(*mut c_void, f32) -> i32 = symbol(lib, b"JxlEncoderSetFrameDistance\0")?;
        let add: unsafe extern "C" fn(*const c_void, *const PixelFormat, *const c_void, usize) -> i32 = symbol(lib, b"JxlEncoderAddImageFrame\0")?;
        let close: CloseInput = symbol(lib, b"JxlEncoderCloseInput\0")?;
        let process: unsafe extern "C" fn(*mut c_void, *mut *mut u8, *mut usize) -> i32 = symbol(lib, b"JxlEncoderProcessOutput\0")?;
        let owner = Encoder { handle: NonNull::new(create(std::ptr::null())).context("Create synthetic JXL encoder")?, destroy, _library: library };
        let handle = owner.handle.as_ptr();
        let mut info = BasicInfo::default();
        init(&mut info);
        info.xsize = width as u32;
        info.ysize = height as u32;
        info.num_color_channels = 3;
        info.bits_per_sample = 16;
        info.uses_original_profile = 0;
        check(basic(handle, &info), "Set synthetic JXL basic info")?;
        let encoding = ColorEncoding {
            color_space: 0,
            white_point: 1,
            primaries: 9,
            transfer_function: 8,
            rendering_intent: 1,
            ..Default::default()
        };
        check(color(handle, &encoding), "Set synthetic linear BT.2100 encoding")?;
        let settings = frame(handle, std::ptr::null());
        ensure!(!settings.is_null(), "Create synthetic JXL frame settings");
        // Exact JxlEncoderFrameSettingId values from public v0.12.0 encode.h.
        const EFFORT: i32 = 0;
        const MODULAR: i32 = 11;
        const PROGRESSIVE_AC: i32 = 17;
        const QPROGRESSIVE_AC: i32 = 18;
        const PROGRESSIVE_DC: i32 = 19;
        const OUTPUT_MODE: i32 = 40;
        check(option(settings, EFFORT, 3), "Set synthetic effort 3")?;
        check(option(settings, MODULAR, 0), "Set synthetic VarDCT mode")?;
        check(option(settings, PROGRESSIVE_AC, i64::from(progressive)), "Set synthetic spectral AC progression")?;
        check(option(settings, QPROGRESSIVE_AC, 0), "Disable synthetic quantized AC progression")?;
        check(option(settings, PROGRESSIVE_DC, 0), "Disable extra synthetic DC frames")?;
        // 0.12-only option; 0.11 keeps its normal output order by default.
        if library.version() >= 12000 {
            check(option(settings, OUTPUT_MODE, 0), "Set synthetic normal progressive-compatible output order")?;
        }
        check(distance(settings, 1.0), "Set synthetic distance 1")?;
        let format = PixelFormat { channels: 3, data_type: 3, endianness: 0, align: 0 };
        check(add(settings, &format, image.as_ptr().cast(), samples * 2), "Add synthetic JXL image")?;
        close(handle);
        let mut bytes = Vec::new();
        let mut chunk = vec![0u8; 65536];
        // Bound output independently of native compression behavior.
        let maximum = samples.checked_mul(4).and_then(|n| n.checked_add(1024 * 1024)).context("Synthetic JXL output limit overflow")?;
        loop {
            let mut next = chunk.as_mut_ptr();
            let mut remaining = chunk.len();
            let status = process(handle, &mut next, &mut remaining);
            ensure!(remaining <= chunk.len(), "Invalid synthetic JXL output extent");
            let written = chunk.len() - remaining;
            ensure!(bytes.len().checked_add(written).is_some_and(|n| n <= maximum), "Synthetic JXL output limit exceeded");
            bytes.try_reserve(written).context("Allocate synthetic JXL bytes")?;
            bytes.extend_from_slice(&chunk[..written]);
            match status {
                0 => return Ok(bytes),
                2 => ensure!(written > 0, "Synthetic JXL encoder made no progress"),
                other => bail!("Synthetic JXL encoding failed with status {other}"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_sample_hash() {
        assert_eq!(sample_hash(&[0x1234, 0xabcd]), format!("{:x}", Sha256::digest([0x34, 0x12, 0xcd, 0xab])));
    }

    #[test]
    fn native_lossless_progressive_fallback_and_bounds() {
        let Some(path) = std::env::var_os("DNG_JXL_LIBRARY") else {
            eprintln!("Skipped native progressive test: DNG_JXL_LIBRARY is not set");
            return;
        };
        let library = JxlLibrary::load(Path::new(&path)).unwrap();
        let bytes = include_bytes!("../../assets/tests/dng-jxl-tile.jxl");
        let report = probe_progressive(&library, bytes, 3, 16, 16).unwrap();
        assert_eq!(report["matches_full_baseline"], true);
        assert_eq!(report["full_output_bytes"], 16 * 16 * 3 * 2);
        assert_eq!(report["native_scaled_output"], false);
        let mut worker = JxlWorker::new(&library, false, false).unwrap();
        let (baseline, _, _) = worker.decode(bytes, 3, 16, 16).unwrap();
        for ratio in [1, 2, 4, 8] {
            let result = preview_only(&library, bytes, 3, 16, 16, ratio).unwrap();
            assert_eq!([result.width, result.height], [16, 16]);
            assert_eq!(result.samples.len(), 16 * 16 * 3);
            if !result.is_partial {
                assert_eq!(result.samples, baseline);
                assert_eq!(result.intended_ratio, 1);
            }
            let djxl = downsample_like_djxl(&library, bytes, 3, 16, 16, ratio).unwrap();
            assert!(!djxl.is_partial);
            assert_eq!(djxl.samples, baseline);
            assert_eq!(djxl.report["full_output_bytes"], 16 * 16 * 3 * 2);
        }
        assert!(downsample_like_djxl(&library, bytes, 3, 16, 16, 3).is_err());
        assert!(preview_only(&library, bytes, 3, 16, 16, 3).is_err());
        assert!(probe_progressive(&library, bytes, 3, 15, 16).is_err());
        assert!(probe_progressive(&library, bytes, 1, 16, 16).is_err());
        assert!(probe_progressive(&library, &bytes[..bytes.len()/2], 3, 16, 16).is_err());
        assert!(probe_progressive(&library, b"not a codestream", 3, 16, 16).is_err());
    }

    #[test]
    fn native_synthetic_progressive_continuation() {
        let Some(path) = std::env::var_os("DNG_JXL_LIBRARY") else {
            eprintln!("Skipped native synthetic test: DNG_JXL_LIBRARY is not set");
            return;
        };
        let library = JxlLibrary::load(Path::new(&path)).unwrap();
        assert!(encode_synthetic(&library, 0, 128, true).is_err());
        assert!(encode_synthetic(&library, 16384, 16384, true).is_err());
        for progressive in [false, true] {
            let bytes = encode_synthetic(&library, 256, 256, progressive).unwrap();
            let report = probe_progressive(&library, &bytes, 3, 256, 256).unwrap();
            assert_eq!(report["matches_full_baseline"], true);
            assert_eq!(report["basic_info"]["have_preview"], false);
            if progressive {
                assert!(report["events"].as_array().unwrap().iter().any(|e| e["ratio"] == 4 && e["flush_status"] == 0));
                for ratio in [2, 4, 8] {
                    let result = downsample_like_djxl(&library, &bytes, 3, 256, 256, ratio).unwrap();
                    assert!(result.is_partial);
                    assert_eq!(result.intended_ratio, ratio);
                    assert_eq!(result.samples.len(), 256 * 256 * 3);
                    let events = result.report["events"].as_array().unwrap();
                    assert_eq!(events.iter().filter(|e| e["flush_status"] == 0).count(), 1);
                    assert!(events.iter().filter(|e| e["ratio"].as_u64().is_some_and(|r| r > ratio as u64)).all(|e| e["flush_status"].is_null()));
                }
            }
        }
    }
}
