//! Experimental, dynamically loaded public libjxl 0.11/0.12 decoder ABI.
//!
//! No parallel runner, preview decode, display profile, or orientation transform
//! is installed. Original-profile streams are returned without color conversion;
//! XYB streams request their original structured RGB/gray profile. An ICC-only
//! XYB stream is rejected rather than silently returning display-sRGB samples.
//!
//! Timing scopes are disjoint except for `allocator_ns`, which is a subset of
//! native setup/header/reconstruction/destruction time. Tracing uses a custom
//! public JxlMemoryManager and adds two clock reads and counter bookkeeping per
//! malloc/free; use separate traced and untraced benchmark runs. Its counters
//! cover native callback requests only, not Rust output allocation, allocations
//! outside that memory manager, live bytes, or peak memory. `allocator_bytes`
//! includes failed allocation requests; `free_calls` excludes null pointers.
//!
//! Reuse mode charges reset (including previous-image cleanup) to setup. Its
//! final decoder destruction occurs at worker drop and is not a per-tile timing.
//! Baseline mode creates/destroys a decoder and allocates a fresh initialized
//! output Vec per tile. Output allocation time includes initialization and any
//! previous output Vec release. Reuse preserves initialized output capacity.
//!
//! ABI definitions follow the upstream v0.12.0 public headers:
//! <https://github.com/libjxl/libjxl/tree/v0.12.0/lib/include/jxl>.

use anyhow::{anyhow, bail, ensure, Context, Result};
use std::{cell::Cell, ffi::c_void, path::Path, ptr::NonNull, time::Instant};

const SUCCESS: i32 = 0;
const ERROR: i32 = 1;
const NEED_MORE_INPUT: i32 = 2;
const NEED_IMAGE_OUT_BUFFER: i32 = 5;
const BASIC_INFO: i32 = 0x40;
const COLOR_ENCODING: i32 = 0x100;
const FRAME: i32 = 0x400;
const FULL_IMAGE: i32 = 0x1000;
const ORIGINAL_PROFILE: i32 = 0;

/// Per-tile wall-clock durations in nanoseconds and optional allocator counters.
#[derive(Clone, Copy, Debug, Default)]
pub struct JxlTimings {
    pub decoder_setup_ns: u64,
    pub header_ns: u64,
    pub output_alloc_ns: u64,
    pub reconstruction_ns: u64,
    pub destroy_ns: u64,
    pub allocator_ns: u64,
    pub allocator_calls: u64,
    pub allocator_bytes: u64,
    pub free_calls: u64,
}

#[repr(C)]
#[derive(Default)]
struct BasicInfo {
    have_container: i32,
    xsize: u32,
    ysize: u32,
    bits_per_sample: u32,
    exponent_bits_per_sample: u32,
    intensity_target: f32,
    min_nits: f32,
    relative_to_max_display: i32,
    linear_below: f32,
    uses_original_profile: i32,
    have_preview: i32,
    have_animation: i32,
    orientation: i32,
    num_color_channels: u32,
    num_extra_channels: u32,
    alpha_bits: u32,
    alpha_exponent_bits: u32,
    alpha_premultiplied: i32,
    preview: [u32; 2],
    animation: [u32; 4],
    intrinsic_xsize: u32,
    intrinsic_ysize: u32,
    padding: Padding,
}

#[repr(transparent)]
struct Padding([u8; 100]);

impl Default for Padding {
    fn default() -> Self {
        Self([0; 100])
    }
}

#[repr(C)]
#[derive(Default)]
struct ColorEncoding {
    color_space: i32,
    white_point: i32,
    white_point_xy: [f64; 2],
    primaries: i32,
    red: [f64; 2],
    green: [f64; 2],
    blue: [f64; 2],
    transfer_function: i32,
    gamma: f64,
    rendering_intent: i32,
}

#[repr(C)]
struct PixelFormat {
    channels: u32,
    data_type: i32,
    endianness: i32,
    align: usize,
}

#[repr(C)]
#[derive(Default)]
struct BlendInfo {
    blendmode: i32,
    source: u32,
    alpha: u32,
    clamp: i32,
}

#[repr(C)]
#[derive(Default)]
struct LayerInfo {
    have_crop: i32,
    crop_x0: i32,
    crop_y0: i32,
    xsize: u32,
    ysize: u32,
    blend_info: BlendInfo,
    save_as_reference: u32,
}

#[repr(C)]
#[derive(Default)]
struct FrameHeader {
    duration: u32,
    timecode: u32,
    name_length: u32,
    is_last: i32,
    layer_info: LayerInfo,
}

#[repr(C)]
struct MemoryManager {
    opaque: *mut c_void,
    alloc: Option<unsafe extern "C" fn(*mut c_void, usize) -> *mut c_void>,
    free: Option<unsafe extern "C" fn(*mut c_void, *mut c_void)>,
}

type Create = unsafe extern "C" fn(*const MemoryManager) -> *mut c_void;
type Destroy = unsafe extern "C" fn(*mut c_void);
type Reset = unsafe extern "C" fn(*mut c_void);
type Subscribe = unsafe extern "C" fn(*mut c_void, i32) -> i32;
type KeepOrientation = unsafe extern "C" fn(*mut c_void, i32) -> i32;
type SetInput = unsafe extern "C" fn(*mut c_void, *const u8, usize) -> i32;
type CloseInput = unsafe extern "C" fn(*mut c_void);
type ReleaseInput = unsafe extern "C" fn(*mut c_void) -> usize;
type Process = unsafe extern "C" fn(*mut c_void) -> i32;
type GetBasicInfo = unsafe extern "C" fn(*const c_void, *mut BasicInfo) -> i32;
type GetFrameHeader = unsafe extern "C" fn(*const c_void, *mut FrameHeader) -> i32;
type GetColor = unsafe extern "C" fn(*const c_void, i32, *mut ColorEncoding) -> i32;
type SetColor = unsafe extern "C" fn(*mut c_void, *const ColorEncoding) -> i32;
type BufferSize = unsafe extern "C" fn(*const c_void, *const PixelFormat, *mut usize) -> i32;
type SetBuffer = unsafe extern "C" fn(*mut c_void, *const PixelFormat, *mut c_void, usize) -> i32;

/// Immutable function table retaining the caller-selected trusted library.
pub struct JxlLibrary {
    version: u32,
    create: Create,
    destroy: Destroy,
    reset: Reset,
    subscribe: Subscribe,
    keep_orientation: KeepOrientation,
    set_input: SetInput,
    close_input: CloseInput,
    release_input: ReleaseInput,
    process: Process,
    get_basic_info: GetBasicInfo,
    get_frame_header: GetFrameHeader,
    get_color: GetColor,
    set_color: SetColor,
    buffer_size: BufferSize,
    set_buffer: SetBuffer,
    _library: libloading::Library,
}

impl JxlLibrary {
    /// Load an absolute, caller-audited library path. Loading executes native
    /// code: an absolute path is not itself proof that a library is trustworthy.
    pub fn load(path: &Path) -> Result<Self> {
        ensure!(
            path.is_absolute(),
            "libjxl requires an absolute trusted library path"
        );
        let path = path.canonicalize().context("Resolve trusted libjxl path")?;
        ensure!(path.is_file(), "libjxl path must name a library file");
        // Restrict dependency lookup without changing process-global PATH.
        #[cfg(windows)]
        let library: libloading::Library =
            unsafe { libloading::os::windows::Library::load_with_flags(&path, 0x100 | 0x1000) }
                .with_context(|| format!("Load trusted libjxl {}", path.display()))?
                .into();
        #[cfg(not(windows))]
        let library = unsafe { libloading::Library::new(&path) }
            .with_context(|| format!("Load trusted libjxl {}", path.display()))?;

        // All types here are the public C ABI, never C++ or private exports.
        unsafe {
            let version_fn: unsafe extern "C" fn() -> u32 =
                symbol(&library, b"JxlDecoderVersion\0")?;
            let version = version_fn();
            ensure!(
                (11000..13000).contains(&version),
                "Public decoder binding requires libjxl 0.11.x or 0.12.x, got {version}"
            );
            Ok(Self {
                version,
                create: symbol(&library, b"JxlDecoderCreate\0")?,
                destroy: symbol(&library, b"JxlDecoderDestroy\0")?,
                reset: symbol(&library, b"JxlDecoderReset\0")?,
                subscribe: symbol(&library, b"JxlDecoderSubscribeEvents\0")?,
                keep_orientation: symbol(&library, b"JxlDecoderSetKeepOrientation\0")?,
                set_input: symbol(&library, b"JxlDecoderSetInput\0")?,
                close_input: symbol(&library, b"JxlDecoderCloseInput\0")?,
                release_input: symbol(&library, b"JxlDecoderReleaseInput\0")?,
                process: symbol(&library, b"JxlDecoderProcessInput\0")?,
                get_basic_info: symbol(&library, b"JxlDecoderGetBasicInfo\0")?,
                get_frame_header: symbol(&library, b"JxlDecoderGetFrameHeader\0")?,
                get_color: symbol(&library, b"JxlDecoderGetColorAsEncodedProfile\0")?,
                set_color: symbol(&library, b"JxlDecoderSetPreferredColorProfile\0")?,
                buffer_size: symbol(&library, b"JxlDecoderImageOutBufferSize\0")?,
                set_buffer: symbol(&library, b"JxlDecoderSetImageOutBuffer\0")?,
                _library: library,
            })
        }
    }

    pub fn version(&self) -> u32 {
        self.version
    }
}

unsafe fn symbol<T: Copy>(library: &libloading::Library, name: &[u8]) -> Result<T> {
    // Call sites supply a matching public function-pointer type and NUL name.
    Ok(*unsafe { library.get::<T>(name) }
        .with_context(|| format!("Load libjxl export {}", String::from_utf8_lossy(name)))?)
}

#[derive(Clone, Copy, Default)]
struct AllocationCounters {
    ns: u64,
    calls: u64,
    bytes: u64,
    frees: u64,
}

// No runner is installed. Callbacks and all counter access stay on this worker's
// thread; Cell deliberately prevents sharing the allocator between threads.
#[derive(Default)]
struct AllocationTrace(Cell<AllocationCounters>);

unsafe extern "C" {
    fn malloc(size: usize) -> *mut c_void;
    fn free(address: *mut c_void);
}

unsafe extern "C" fn traced_alloc(opaque: *mut c_void, size: usize) -> *mut c_void {
    let started = Instant::now();
    let result = unsafe { malloc(size) };
    let elapsed = ns(started);
    // The stable Box outlives the decoder, including destruction callbacks.
    let trace = unsafe { &*opaque.cast::<AllocationTrace>() };
    let mut counters = trace.0.get();
    counters.ns = counters.ns.saturating_add(elapsed);
    counters.calls = counters.calls.saturating_add(1);
    counters.bytes = counters.bytes.saturating_add(size as u64);
    trace.0.set(counters);
    result
}

unsafe extern "C" fn traced_free(opaque: *mut c_void, address: *mut c_void) {
    if address.is_null() {
        return;
    }
    let started = Instant::now();
    unsafe { free(address) };
    let elapsed = ns(started);
    let trace = unsafe { &*opaque.cast::<AllocationTrace>() };
    let mut counters = trace.0.get();
    counters.ns = counters.ns.saturating_add(elapsed);
    counters.frees = counters.frees.saturating_add(1);
    trace.0.set(counters);
}

struct Decoder<'a> {
    library: &'a JxlLibrary,
    handle: NonNull<c_void>,
}

impl Drop for Decoder<'_> {
    fn drop(&mut self) {
        unsafe { (self.library.destroy)(self.handle.as_ptr()) };
    }
}

/// A single-thread native decoder and tile output buffer. Make one per worker.
pub struct JxlWorker<'a> {
    library: &'a JxlLibrary,
    reuse: bool,
    decoder: Option<Decoder<'a>>,
    output: Vec<u16>,
    trace: Option<Box<AllocationTrace>>,
}

impl<'a> JxlWorker<'a> {
    /// Decoder creation is deferred to the first tile so setup is timed even
    /// for the first decode. `new` performs no native work or file I/O.
    pub fn new(library: &'a JxlLibrary, reuse: bool, trace_alloc: bool) -> Result<Self> {
        Ok(Self {
            library,
            reuse,
            decoder: None,
            output: Vec::new(),
            trace: trace_alloc.then(|| Box::new(AllocationTrace::default())),
        })
    }

    /// Decode one complete, unsigned-16 gray/RGB tile with interleaved samples
    /// in stored (unrotated) orientation, bounded before output allocation.
    pub fn decode<'b>(
        &'b mut self,
        bytes: &[u8],
        channels: usize,
        max_width: usize,
        max_height: usize,
    ) -> Result<(&'b [u16], [usize; 2], JxlTimings)> {
        ensure!(
            matches!(channels, 1 | 3),
            "Expected one or three JXL color channels"
        );
        ensure!(max_width > 0 && max_height > 0, "Invalid JXL tile bounds");
        ensure!(!bytes.is_empty(), "Empty JXL tile");
        if let Some(trace) = &self.trace {
            trace.0.set(AllocationCounters::default());
        }
        let mut timings = JxlTimings::default();
        let result = self.decode_inner(bytes, channels, max_width, max_height, &mut timings);
        // On every error, destroy before input/output or allocator state can
        // become invalid. Successful reuse releases input inside decode_inner.
        if !self.reuse || result.is_err() {
            let started = Instant::now();
            drop(self.decoder.take());
            timings.destroy_ns = ns(started);
        }
        if let Some(trace) = &self.trace {
            let counters = trace.0.get();
            timings.allocator_ns = counters.ns;
            timings.allocator_calls = counters.calls;
            timings.allocator_bytes = counters.bytes;
            timings.free_calls = counters.frees;
        }
        let dimensions = result?;
        Ok((&self.output, dimensions, timings))
    }

    fn decode_inner(
        &mut self,
        bytes: &[u8],
        channels: usize,
        max_width: usize,
        max_height: usize,
        timings: &mut JxlTimings,
    ) -> Result<[usize; 2]> {
        let started = Instant::now();
        let lib = self.library;
        let handle = if let Some(decoder) = &self.decoder {
            // Reset clears the old output pointer before Vec resize/reallocation.
            unsafe { (lib.reset)(decoder.handle.as_ptr()) };
            decoder.handle.as_ptr()
        } else {
            let manager = self.trace.as_deref().map(|trace| MemoryManager {
                opaque: (trace as *const AllocationTrace).cast_mut().cast(),
                alloc: Some(traced_alloc),
                free: Some(traced_free),
            });
            let manager_ptr = manager.as_ref().map_or(std::ptr::null(), |m| m);
            let handle = NonNull::new(unsafe { (lib.create)(manager_ptr) })
                .context("JxlDecoderCreate failed")?;
            self.decoder = Some(Decoder {
                library: lib,
                handle,
            });
            handle.as_ptr()
        };
        unsafe {
            check(
                (lib.subscribe)(handle, BASIC_INFO | COLOR_ENCODING | FRAME | FULL_IMAGE),
                "JxlDecoderSubscribeEvents",
            )?;
            check(
                (lib.keep_orientation)(handle, 1),
                "JxlDecoderSetKeepOrientation",
            )?;
            check(
                (lib.set_input)(handle, bytes.as_ptr(), bytes.len()),
                "JxlDecoderSetInput",
            )?;
            (lib.close_input)(handle);
        }
        timings.decoder_setup_ns = ns(started);

        let header_started = Instant::now();
        let mut reconstruction_started = None;
        let mut info = BasicInfo::default();
        let mut dimensions = [0; 2];
        let mut got_basic = false;
        let mut got_color = false;
        let mut got_frame = false;
        let mut got_buffer = false;
        let mut got_full_image = false;
        let format = PixelFormat {
            channels: channels as u32,
            data_type: 3,  // JXL_TYPE_UINT16, full 16-bit range.
            endianness: 0, // JXL_NATIVE_ENDIAN, matching Vec<u16>.
            align: 0,
        };
        // Exactly six events are expected; reject duplicates and all statuses
        // not explicitly handled, including truncated input and preview output.
        for _ in 0..8 {
            let status = unsafe { (lib.process)(handle) };
            match status {
                BASIC_INFO => {
                    ensure!(
                        !got_basic && !got_color && !got_frame,
                        "Duplicate/out-of-order JXL basic info"
                    );
                    unsafe {
                        check(
                            (lib.get_basic_info)(handle, &mut info),
                            "JxlDecoderGetBasicInfo",
                        )?
                    };
                    dimensions = validate_basic(&info, channels, max_width, max_height)?;
                    got_basic = true;
                }
                COLOR_ENCODING => {
                    ensure!(
                        got_basic && !got_color && !got_frame,
                        "Duplicate/out-of-order JXL color encoding"
                    );
                    if info.uses_original_profile == 0 {
                        let mut original = ColorEncoding::default();
                        unsafe {
                            check((lib.get_color)(handle, ORIGINAL_PROFILE, &mut original),
                                "Original structured JXL profile unavailable (ICC-only XYB is unsupported)")?;
                        }
                        ensure!(
                            original.color_space == if channels == 1 { 1 } else { 0 },
                            "Original JXL profile is not the requested gray/RGB color space"
                        );
                        // No forced sRGB profile or external CMS. For XYB this
                        // requests conversion back into its original encoding.
                        unsafe {
                            check(
                                (lib.set_color)(handle, &original),
                                "JxlDecoderSetPreferredColorProfile(original)",
                            )?
                        };
                    }
                    got_color = true;
                }
                FRAME => {
                    ensure!(
                        got_color && !got_frame && !got_buffer,
                        "Duplicate/out-of-order JXL frame"
                    );
                    let mut frame = FrameHeader::default();
                    unsafe {
                        check(
                            (lib.get_frame_header)(handle, &mut frame),
                            "JxlDecoderGetFrameHeader",
                        )?
                    };
                    ensure!(
                        frame.is_last == 1 && frame.duration == 0 && frame.timecode == 0,
                        "Expected one complete non-animated JXL frame"
                    );
                    // The default coalescing mode must describe the full tile.
                    ensure!(
                        frame.layer_info.have_crop == 0
                            && frame.layer_info.xsize == info.xsize
                            && frame.layer_info.ysize == info.ysize,
                        "JXL frame dimensions do not match the full tile"
                    );
                    got_frame = true;
                }
                NEED_IMAGE_OUT_BUFFER => {
                    ensure!(
                        got_frame && !got_buffer && !got_full_image,
                        "Duplicate/out-of-order JXL output request"
                    );
                    // Initialize the out parameter and check status before use.
                    let mut required_bytes = 0usize;
                    unsafe {
                        check(
                            (lib.buffer_size)(handle, &format, &mut required_bytes),
                            "JxlDecoderImageOutBufferSize",
                        )?
                    };
                    let samples = dimensions[0]
                        .checked_mul(dimensions[1])
                        .and_then(|n| n.checked_mul(channels))
                        .context("JXL tile sample count overflow")?;
                    let expected_bytes =
                        samples.checked_mul(2).context("JXL output size overflow")?;
                    ensure!(
                        expected_bytes <= isize::MAX as usize,
                        "JXL output exceeds Rust allocation limit"
                    );
                    ensure!(required_bytes == expected_bytes, "Unexpected JXL output buffer size: {required_bytes}, expected {expected_bytes}");
                    timings.header_ns = ns(header_started);

                    let started = Instant::now();
                    if !self.reuse {
                        self.output = Vec::new();
                    }
                    if samples > self.output.len() {
                        self.output
                            .try_reserve_exact(samples - self.output.len())
                            .context("Allocate native JXL output")?;
                    }
                    // Never expose uninitialized u16s, even if the native call
                    // fails or an incomplete codestream writes only part of it.
                    self.output.resize(samples, 0);
                    timings.output_alloc_ns = ns(started);

                    reconstruction_started = Some(Instant::now());
                    unsafe {
                        check(
                            (lib.set_buffer)(
                                handle,
                                &format,
                                self.output.as_mut_ptr().cast(),
                                expected_bytes,
                            ),
                            "JxlDecoderSetImageOutBuffer",
                        )?
                    };
                    got_buffer = true;
                }
                FULL_IMAGE => {
                    ensure!(
                        got_buffer && !got_full_image,
                        "Duplicate/out-of-order JXL full image"
                    );
                    got_full_image = true;
                }
                SUCCESS => {
                    ensure!(
                        got_basic && got_color && got_frame && got_buffer && got_full_image,
                        "JXL decoder finished without a complete output frame"
                    );
                    let remaining = unsafe { (lib.release_input)(handle) };
                    ensure!(
                        remaining <= bytes.len(),
                        "Invalid JXL unconsumed-input length"
                    );
                    timings.reconstruction_ns =
                        ns(reconstruction_started.context("Missing JXL reconstruction timer")?);
                    return Ok(dimensions);
                }
                NEED_MORE_INPUT => bail!("Truncated JXL tile: decoder needs more input"),
                ERROR => bail!("libjxl rejected the tile or could not allocate decoder memory"),
                other => bail!("Unexpected libjxl decoder event/status {other:#x}"),
            }
        }
        bail!("libjxl decoder event limit exceeded")
    }
}

impl Drop for JxlWorker<'_> {
    fn drop(&mut self) {
        // Destroy while both the stable custom allocator Box and output Vec are
        // still alive. Decoder's borrow also keeps the DLL loaded throughout.
        drop(self.decoder.take());
    }
}

fn validate_basic(
    info: &BasicInfo,
    channels: usize,
    max_width: usize,
    max_height: usize,
) -> Result<[usize; 2]> {
    let dimensions = [info.xsize as usize, info.ysize as usize];
    ensure!(
        dimensions[0] > 0
            && dimensions[1] > 0
            && dimensions[0] <= max_width
            && dimensions[1] <= max_height,
        "JXL tile dimensions {}x{} exceed bounds {max_width}x{max_height}",
        dimensions[0],
        dimensions[1]
    );
    ensure!(
        info.bits_per_sample == 16 && info.exponent_bits_per_sample == 0,
        "Expected unsigned 16-bit JXL samples"
    );
    ensure!(
        info.num_color_channels as usize == channels,
        "JXL color-channel count mismatch"
    );
    ensure!(
        info.num_extra_channels == 0
            && info.alpha_bits == 0
            && info.alpha_exponent_bits == 0
            && info.alpha_premultiplied == 0,
        "JXL tile must not contain alpha or extra channels"
    );
    ensure!(
        info.have_animation == 0,
        "Animated JXL tiles are unsupported"
    );
    ensure!(
        (1..=8).contains(&info.orientation),
        "Invalid JXL orientation"
    );
    ensure!(
        matches!(info.uses_original_profile, 0 | 1),
        "Invalid JXL original-profile flag"
    );
    Ok(dimensions)
}

fn check(status: i32, operation: &str) -> Result<()> {
    if status == SUCCESS {
        Ok(())
    } else {
        Err(anyhow!("{operation}: libjxl status {status}"))
    }
}

fn ns(started: Instant) -> u64 {
    started.elapsed().as_nanos().min(u64::MAX as u128) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_abi_layout() {
        assert_eq!(std::mem::size_of::<BasicInfo>(), 204);
        assert_eq!(std::mem::offset_of!(BasicInfo, uses_original_profile), 36);
        assert_eq!(std::mem::offset_of!(BasicInfo, padding), 104);
        assert_eq!(std::mem::size_of::<ColorEncoding>(), 104);
        assert_eq!(std::mem::offset_of!(ColorEncoding, gamma), 88);
        assert_eq!(std::mem::size_of::<FrameHeader>(), 56);
        assert_eq!(std::mem::offset_of!(FrameHeader, layer_info), 16);
        assert_eq!(std::mem::size_of::<LayerInfo>(), 40);
        if cfg!(target_pointer_width = "64") {
            assert_eq!(std::mem::size_of::<PixelFormat>(), 24);
            assert_eq!(std::mem::offset_of!(PixelFormat, align), 16);
            assert_eq!(std::mem::size_of::<MemoryManager>(), 24);
            assert_eq!(std::mem::offset_of!(MemoryManager, alloc), 8);
            assert_eq!(std::mem::offset_of!(MemoryManager, free), 16);
        }
    }

    #[test]
    fn reject_untrusted_relative_library_paths() {
        assert!(JxlLibrary::load(Path::new("jxl.dll")).is_err());
    }

    #[test]
    fn basic_validation_rejects_unsupported_layouts() {
        let mut info = BasicInfo {
            xsize: 16,
            ysize: 16,
            bits_per_sample: 16,
            num_color_channels: 3,
            orientation: 1,
            uses_original_profile: 1,
            ..Default::default()
        };
        assert_eq!(validate_basic(&info, 3, 16, 16).unwrap(), [16, 16]);
        assert!(validate_basic(&info, 1, 16, 16).is_err());
        assert!(validate_basic(&info, 3, 15, 16).is_err());
        info.bits_per_sample = 12;
        assert!(validate_basic(&info, 3, 16, 16).is_err());
        info.bits_per_sample = 16;
        info.exponent_bits_per_sample = 5;
        assert!(validate_basic(&info, 3, 16, 16).is_err());
        info.exponent_bits_per_sample = 0;
        info.num_extra_channels = 1;
        assert!(validate_basic(&info, 3, 16, 16).is_err());
        info.num_extra_channels = 0;
        info.have_animation = 1;
        assert!(validate_basic(&info, 3, 16, 16).is_err());
        info.have_animation = 0;
        info.orientation = 0;
        assert!(validate_basic(&info, 3, 16, 16).is_err());
    }

    #[test]
    fn allocator_counts_requests_and_ignores_null_free() {
        let trace = AllocationTrace::default();
        let opaque = (&trace as *const AllocationTrace).cast_mut().cast();
        unsafe {
            traced_free(opaque, std::ptr::null_mut());
            let address = traced_alloc(opaque, 128);
            assert!(!address.is_null());
            traced_free(opaque, address);
        }
        let counters = trace.0.get();
        assert_eq!(counters.calls, 1);
        assert_eq!(counters.bytes, 128);
        assert_eq!(counters.frees, 1);
    }

    // Generate test-only codestreams through the same public encoder ABI. The
    // checked-in independent fixture below remains the lossless sample oracle.
    fn encode_test_tile(
        library: &JxlLibrary,
        pixels: &[u16],
        channels: u32,
        orientation: i32,
        lossless: bool,
    ) -> Result<Vec<u8>> {
        struct EncoderGuard<'a> {
            handle: NonNull<c_void>,
            destroy: Destroy,
            _library: &'a JxlLibrary,
        }
        impl Drop for EncoderGuard<'_> {
            fn drop(&mut self) {
                unsafe { (self.destroy)(self.handle.as_ptr()) };
            }
        }
        unsafe {
            let lib = &library._library;
            let create: Create = symbol(lib, b"JxlEncoderCreate\0")?;
            let destroy: Destroy = symbol(lib, b"JxlEncoderDestroy\0")?;
            let init: unsafe extern "C" fn(*mut BasicInfo) =
                symbol(lib, b"JxlEncoderInitBasicInfo\0")?;
            let basic: unsafe extern "C" fn(*mut c_void, *const BasicInfo) -> i32 =
                symbol(lib, b"JxlEncoderSetBasicInfo\0")?;
            let color: SetColor = symbol(lib, b"JxlEncoderSetColorEncoding\0")?;
            let frame: unsafe extern "C" fn(*mut c_void, *const c_void) -> *mut c_void =
                symbol(lib, b"JxlEncoderFrameSettingsCreate\0")?;
            let set_lossless: unsafe extern "C" fn(*mut c_void, i32) -> i32 =
                symbol(lib, b"JxlEncoderSetFrameLossless\0")?;
            let distance: unsafe extern "C" fn(*mut c_void, f32) -> i32 =
                symbol(lib, b"JxlEncoderSetFrameDistance\0")?;
            let add: unsafe extern "C" fn(
                *mut c_void,
                *const PixelFormat,
                *const c_void,
                usize,
            ) -> i32 = symbol(lib, b"JxlEncoderAddImageFrame\0")?;
            let close: CloseInput = symbol(lib, b"JxlEncoderCloseInput\0")?;
            let process: unsafe extern "C" fn(*mut c_void, *mut *mut u8, *mut usize) -> i32 =
                symbol(lib, b"JxlEncoderProcessOutput\0")?;
            let owner = EncoderGuard {
                handle: NonNull::new(create(std::ptr::null())).context("Create test encoder")?,
                destroy,
                _library: library,
            };
            let handle = owner.handle.as_ptr();
            let mut info = BasicInfo::default();
            init(&mut info);
            info.xsize = 5;
            info.ysize = 3;
            info.num_color_channels = channels;
            info.bits_per_sample = 16;
            info.uses_original_profile = i32::from(lossless);
            info.orientation = orientation;
            check(basic(handle, &info), "Set test basic info")?;
            let encoding = ColorEncoding {
                color_space: if channels == 1 { 1 } else { 0 },
                white_point: 1,
                primaries: 9,         // BT.2100, deliberately not display sRGB.
                transfer_function: 8, // Linear.
                rendering_intent: 1,
                ..Default::default()
            };
            check(color(handle, &encoding), "Set test color encoding")?;
            let settings = frame(handle, std::ptr::null());
            ensure!(!settings.is_null(), "Create test frame settings");
            if lossless {
                check(set_lossless(settings, 1), "Set test lossless mode")?;
            } else {
                check(distance(settings, 0.01), "Set test lossy distance")?;
            }
            let format = PixelFormat {
                channels,
                data_type: 3,
                endianness: 0,
                align: 0,
            };
            ensure!(
                pixels.len() == 5 * 3 * channels as usize,
                "Test pixel extent"
            );
            check(
                add(
                    settings,
                    &format,
                    pixels.as_ptr().cast(),
                    std::mem::size_of_val(pixels),
                ),
                "Encode test image",
            )?;
            close(handle);
            let mut bytes = vec![0u8; 65536];
            let mut next = bytes.as_mut_ptr();
            let mut remaining = bytes.len();
            check(
                process(handle, &mut next, &mut remaining),
                "Finish test image",
            )?;
            ensure!(remaining <= bytes.len(), "Invalid test encoded extent");
            bytes.truncate(bytes.len() - remaining);
            Ok(bytes)
        }
    }

    #[test]
    fn stored_orientation_and_original_linear_profile() {
        let Some(path) = std::env::var_os("DNG_JXL_LIBRARY") else {
            eprintln!("Skipped native orientation/profile test: DNG_JXL_LIBRARY is not set");
            return;
        };
        let library = JxlLibrary::load(Path::new(&path)).unwrap();
        let mut worker = JxlWorker::new(&library, true, true).unwrap();
        for channels in [1, 3] {
            let pixels: Vec<u16> = (0..5 * 3 * channels)
                .map(|i| (i * 17431 + 50123) as u16)
                .collect();
            for orientation in 1..=8 {
                let tile = encode_test_tile(&library, &pixels, channels as u32, orientation, true)
                    .unwrap();
                let (decoded, size, _) = worker.decode(&tile, channels, 5, 3).unwrap();
                assert_eq!(size, [5, 3], "orientation {orientation}");
                assert_eq!(decoded, pixels, "orientation {orientation}");
            }
        }
        let pixels = vec![16384; 5 * 3 * 3];
        let tile = encode_test_tile(&library, &pixels, 3, 6, false).unwrap();
        let (decoded, size, _) = worker.decode(&tile, 3, 5, 3).unwrap();
        assert_eq!(size, [5, 3]);
        // Display-sRGB conversion of this linear quarter-scale gray would be
        // about 35000, not 16384. Allow normal XYB lossy reconstruction error.
        for &sample in decoded {
            assert!((sample as i32 - 16384).abs() < 256, "sample {sample}");
        }
    }

    /// Supply DNG_JXL_LIBRARY with an absolute trusted path to enable native
    /// integration checks. No library search, downloads, or app builds occur.
    #[test]
    fn lossless_fixture_native_modes_and_error_recovery() {
        let Some(path) = std::env::var_os("DNG_JXL_LIBRARY") else {
            eprintln!("Skipped native fixture: DNG_JXL_LIBRARY is not set");
            return;
        };
        let library = JxlLibrary::load(Path::new(&path)).unwrap();
        assert!((11000..13000).contains(&library.version()));
        let tile = include_bytes!("../../assets/tests/dng-jxl-tile.jxl");
        for reuse in [false, true] {
            for trace in [false, true] {
                let mut worker = JxlWorker::new(&library, reuse, trace).unwrap();
                for _ in 0..3 {
                    let (pixels, size, timings) = worker.decode(tile, 3, 16, 16).unwrap();
                    assert_eq!(size, [16, 16]);
                    assert_eq!(pixels.len(), 16 * 16 * 3);
                    for (i, &actual) in pixels.iter().enumerate() {
                        assert_eq!(actual, (i * 17431 + 50123) as u16, "sample {i}");
                    }
                    if trace {
                        assert!(timings.allocator_calls > 0);
                        assert!(timings.allocator_bytes > 0);
                    } else {
                        assert_eq!(timings.allocator_calls, 0);
                        assert_eq!(timings.allocator_bytes, 0);
                        assert_eq!(timings.allocator_ns, 0);
                        assert_eq!(timings.free_calls, 0);
                    }
                    if reuse {
                        assert_eq!(timings.destroy_ns, 0);
                    }
                }
                assert!(worker.decode(tile, 3, 15, 16).is_err());
                assert!(worker.decode(tile, 1, 16, 16).is_err());
                assert!(worker.decode(&tile[..tile.len() / 2], 3, 16, 16).is_err());
                assert!(worker.decode(b"not a codestream", 3, 16, 16).is_err());
                let (pixels, size, _) = worker.decode(tile, 3, 16, 16).unwrap();
                assert_eq!(size, [16, 16]);
                assert_eq!(pixels[0], 50123);
            }
        }
    }
}

// Additive experimental child module; the baseline ABI/worker above is unchanged.
#[path = "../preview/native_progressive.rs"]
mod native_progressive;
pub use native_progressive::{downsample_like_djxl, encode_synthetic, preview_only, probe_progressive, PreviewResult};
