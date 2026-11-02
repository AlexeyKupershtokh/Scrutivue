//! Optional pinned libjxl runtime. A session owns input/output through destruction.
//! Public v0.12 C ABI only; no embedded previews or display profile.
//! An opt-in experimental binary adds a bounded, caller-pool parallel runner.
use anyhow::{ensure, Context, Result};
use sha2::{Digest, Sha256};
use std::{ffi::c_void, path::Path, ptr::NonNull, sync::Arc};
mod encode;
pub use encode::encode_preview;
pub(crate) use encode::{DISTANCE as PREVIEW_DISTANCE, EFFORT as PREVIEW_EFFORT};
#[cfg(feature = "experimental-jxl-parallel")]
mod parallel;

#[repr(C)]
#[derive(Default)]
struct Basic {
    container: i32,
    width: u32,
    height: u32,
    bits: u32,
    exponent: u32,
    intensity: f32,
    min_nits: f32,
    relative: i32,
    linear_below: f32,
    original: i32,
    preview: i32,
    animation: i32,
    orientation: i32,
    channels: u32,
    extras: u32,
    alpha_bits: u32,
    alpha_exponent: u32,
    alpha_premultiplied: i32,
    preview_size: [u32; 2],
    animation_info: [u32; 4],
    intrinsic: [u32; 2],
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
struct Color {
    space: i32,
    white: i32,
    white_xy: [f64; 2],
    primaries: i32,
    red: [f64; 2],
    green: [f64; 2],
    blue: [f64; 2],
    transfer: i32,
    gamma: f64,
    intent: i32,
}
#[repr(C)]
struct Format {
    channels: u32,
    kind: i32,
    endian: i32,
    align: usize,
}
#[repr(C)]
#[derive(Default)]
struct FrameHeader {
    duration: u32,
    timecode: u32,
    name_length: u32,
    is_last: i32,
    have_crop: i32,
    crop_x: i32,
    crop_y: i32,
    width: u32,
    height: u32,
    blend_mode: i32,
    source: u32,
    alpha: u32,
    clamp: i32,
    save_reference: u32,
}
type Status = unsafe extern "C" fn(*mut c_void) -> i32;
type Setting = unsafe extern "C" fn(*mut c_void, i32) -> i32;
pub struct Library {
    create: unsafe extern "C" fn(*const c_void) -> *mut c_void,
    destroy: unsafe extern "C" fn(*mut c_void),
    process: Status,
    flush: Status,
    subscribe: Setting,
    orientation: Setting,
    detail: Setting,
    input: unsafe extern "C" fn(*mut c_void, *const u8, usize) -> i32,
    info: unsafe extern "C" fn(*const c_void, *mut Basic) -> i32,
    frame: unsafe extern "C" fn(*const c_void, *mut FrameHeader) -> i32,
    color: unsafe extern "C" fn(*const c_void, i32, *mut Color) -> i32,
    set_color: unsafe extern "C" fn(*mut c_void, *const Color) -> i32,
    size: unsafe extern "C" fn(*const c_void, *const Format, *mut usize) -> i32,
    buffer: unsafe extern "C" fn(*mut c_void, *const Format, *mut c_void, usize) -> i32,
    ratio: unsafe extern "C" fn(*mut c_void) -> usize,
    #[cfg(feature = "experimental-jxl-parallel")]
    runner: unsafe extern "C" fn(*mut c_void, Option<parallel::Runner>, *mut c_void) -> i32,
    _library: libloading::Library,
}
impl Library {
    /// Only the hash-pinned bundle beside the executable is eligible. Never PATH.
    pub fn installed() -> Result<Arc<Self>> {
        let path = std::env::current_exe()?
            .parent()
            .context("Executable directory")?
            .join("libjxl/jxl.dll");
        Self::load(&path).map(Arc::new)
    }
    pub fn load(path: &Path) -> Result<Self> {
        let path = path.canonicalize()?;
        ensure!(
            path.file_name().is_some_and(|name| name == "jxl.dll"),
            "Expected the pinned jxl.dll filename"
        );
        let pin: serde_json::Value = serde_json::from_str(include_str!("../config/libjxl.json"))?;
        let directory = path.parent().context("libjxl directory")?;
        // Check dependent code as well as the main DLL before executing any of it.
        for (name, expected) in pin["files"].as_object().context("libjxl pin")? {
            let hash = format!("{:x}", Sha256::digest(std::fs::read(directory.join(name))?));
            ensure!(
                Some(hash.as_str()) == expected.as_str(),
                "libjxl runtime identity mismatch: {name}"
            );
        }
        #[cfg(windows)]
        let library: libloading::Library =
            unsafe { libloading::os::windows::Library::load_with_flags(&path, 0x100 | 0x1000)? }
                .into();
        #[cfg(not(windows))]
        let library = unsafe { libloading::Library::new(&path)? };
        unsafe fn get<T: Copy>(l: &libloading::Library, name: &[u8]) -> Result<T> {
            Ok(*unsafe { l.get::<T>(name)? })
        }
        unsafe {
            let version: unsafe extern "C" fn() -> u32 = get(&library, b"JxlDecoderVersion\0")?;
            ensure!(version() == 12000, "Expected pinned libjxl 0.12.0");
            Ok(Self {
                create: get(&library, b"JxlDecoderCreate\0")?,
                destroy: get(&library, b"JxlDecoderDestroy\0")?,
                process: get(&library, b"JxlDecoderProcessInput\0")?,
                flush: get(&library, b"JxlDecoderFlushImage\0")?,
                subscribe: get(&library, b"JxlDecoderSubscribeEvents\0")?,
                orientation: get(&library, b"JxlDecoderSetKeepOrientation\0")?,
                detail: get(&library, b"JxlDecoderSetProgressiveDetail\0")?,
                input: get(&library, b"JxlDecoderSetInput\0")?,
                info: get(&library, b"JxlDecoderGetBasicInfo\0")?,
                color: get(&library, b"JxlDecoderGetColorAsEncodedProfile\0")?,
                frame: get(&library, b"JxlDecoderGetFrameHeader\0")?,
                set_color: get(&library, b"JxlDecoderSetPreferredColorProfile\0")?,
                size: get(&library, b"JxlDecoderImageOutBufferSize\0")?,
                buffer: get(&library, b"JxlDecoderSetImageOutBuffer\0")?,
                ratio: get(&library, b"JxlDecoderGetIntendedDownsamplingRatio\0")?,
                #[cfg(feature = "experimental-jxl-parallel")]
                runner: get(&library, b"JxlDecoderSetParallelRunner\0")?,
                _library: library,
            })
        }
    }
}
fn check(status: i32) -> Result<()> {
    ensure!(status == 0, "libjxl operation failed ({status})");
    Ok(())
}

pub struct Session {
    library: Arc<Library>,
    handle: NonNull<c_void>,
    _input: Vec<u8>,
    pub pixels: Vec<u16>,
    pub size: [u32; 2],
    pub complete: bool,
    bounds: [u32; 2],
    info_seen: bool,
    color_seen: bool,
    buffer_set: bool,
    original: bool,
    frame_seen: bool,
}
// Each decoder is exclusively owned, with no thread-affine state. The optional
// runner is synchronous and borrows the current pool, never the decoder handle.
// Moving it preserves the heap addresses of its retained input/output buffers.
// libjxl instances may run on different threads, but never concurrently through
// the same handle; advance requires &mut self. Deliberately do not implement Sync.
unsafe impl Send for Session {}
impl Drop for Session {
    fn drop(&mut self) {
        unsafe { (self.library.destroy)(self.handle.as_ptr()) };
    }
}
impl Session {
    pub fn new(library: Arc<Library>, input: Vec<u8>, bounds: [u32; 2]) -> Result<Self> {
        Self::create(
            library,
            input,
            bounds,
            cfg!(feature = "experimental-jxl-parallel"),
        )
    }
    fn create(
        library: Arc<Library>,
        input: Vec<u8>,
        bounds: [u32; 2],
        _parallel: bool,
    ) -> Result<Self> {
        let handle = NonNull::new(unsafe { (library.create)(std::ptr::null()) })
            .context("Create libjxl decoder")?;
        let session = Self {
            library,
            handle,
            _input: input,
            pixels: Vec::new(),
            size: [0; 2],
            complete: false,
            bounds,
            info_seen: false,
            color_seen: false,
            buffer_set: false,
            original: false,
            frame_seen: false,
        };
        unsafe {
            #[cfg(feature = "experimental-jxl-parallel")]
            if _parallel {
                check((session.library.runner)(
                    handle.as_ptr(),
                    Some(parallel::run),
                    std::ptr::null_mut(),
                ))?;
            }
            check((session.library.subscribe)(
                handle.as_ptr(),
                0x40 | 0x100 | 0x400 | 0x1000 | 0x8000,
            ))?;
            check((session.library.orientation)(handle.as_ptr(), 1))?;
            check((session.library.detail)(handle.as_ptr(), 2))?;
            check((session.library.input)(
                handle.as_ptr(),
                session._input.as_ptr(),
                session._input.len(),
            ))?;
        }
        Ok(session)
    }
    /// Advance the SAME decoder. Earlier stages are not flushed. 1 means full.
    pub fn advance(&mut self, maximum_ratio: usize, cancelled: &dyn Fn() -> bool) -> Result<usize> {
        ensure!(
            matches!(maximum_ratio, 1 | 2 | 4 | 8),
            "Invalid progressive ratio"
        );
        let h = self.handle.as_ptr();
        for _ in 0..4096 {
            ensure!(!cancelled(), "Image decode cancelled");
            if self.complete {
                return Ok(1);
            }
            match unsafe { (self.library.process)(h) } {
                0x40 => {
                    ensure!(!self.info_seen, "Duplicate JXL basic info");
                    let mut info = Basic::default();
                    check(unsafe { (self.library.info)(h, &mut info) })?;
                    ensure!(
                        info.width > 0
                            && info.height > 0
                            && info.width <= self.bounds[0]
                            && info.height <= self.bounds[1]
                            && info.channels == 3
                            && info.extras == 0
                            && info.animation == 0
                            && info.bits == 16
                            && info.exponent == 0
                            && info.orientation == 1,
                        "Unsupported JXL tile layout"
                    );
                    self.size = [info.width, info.height];
                    self.original = info.original != 0;
                    self.info_seen = true;
                }
                0x100 => {
                    ensure!(
                        self.info_seen && !self.color_seen,
                        "Invalid JXL color event"
                    );
                    if !self.original {
                        let mut encoding = Color::default();
                        // Preserve the camera-linear sample domain, including XYB input.
                        check(unsafe { (self.library.color)(h, 0, &mut encoding) })?;
                        ensure!(encoding.space == 0, "JXL tile is not RGB");
                        check(unsafe { (self.library.set_color)(h, &encoding) })?;
                    }
                    self.color_seen = true;
                }
                5 => {
                    ensure!(
                        self.frame_seen && !self.buffer_set,
                        "Invalid JXL buffer event"
                    );
                    let format = Format {
                        channels: 3,
                        kind: 3,
                        endian: 0,
                        align: 0,
                    };
                    let mut count = 0;
                    check(unsafe { (self.library.size)(h, &format, &mut count) })?;
                    ensure!(
                        count == self.size[0] as usize * self.size[1] as usize * 6
                            && count <= 512 * 1024 * 1024,
                        "Invalid JXL output extent"
                    );
                    self.pixels.try_reserve_exact(count / 2)?;
                    self.pixels.resize(count / 2, 0);
                    check(unsafe {
                        (self.library.buffer)(h, &format, self.pixels.as_mut_ptr().cast(), count)
                    })?;
                    self.buffer_set = true;
                }
                0x400 => {
                    ensure!(self.color_seen && !self.frame_seen, "Unexpected JXL frame");
                    let mut frame = FrameHeader::default();
                    check(unsafe { (self.library.frame)(h, &mut frame) })?;
                    ensure!(
                        frame.is_last == 1
                            && frame.duration == 0
                            && frame.timecode == 0
                            && frame.have_crop == 0
                            && [frame.width, frame.height] == self.size,
                        "Unsupported JXL tile frame"
                    );
                    self.frame_seen = true;
                }
                0x8000 => {
                    let ratio = unsafe { (self.library.ratio)(h) };
                    ensure!(matches!(ratio, 1 | 2 | 4 | 8), "Invalid JXL progression");
                    if maximum_ratio > 1 && ratio <= maximum_ratio {
                        check(unsafe { (self.library.flush)(h) })?;
                        return Ok(ratio);
                    }
                }
                0x1000 => {
                    ensure!(self.buffer_set, "JXL output is missing");
                    self.complete = true;
                    return Ok(1);
                }
                other => anyhow::bail!("JXL decoder could not complete the tile ({other})"),
            }
        }
        anyhow::bail!("JXL decoder event limit exceeded")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn public_abi() {
        assert_eq!(std::mem::size_of::<Basic>(), 204);
        assert_eq!(std::mem::size_of::<Color>(), 104);
        assert_eq!(std::mem::size_of::<FrameHeader>(), 56);
    }
    #[test]
    #[ignore = "requires SCRUTIVUE_JXL_TEST_LIBRARY and SCRUTIVUE_PROGRESSIVE_TEST_DNG"]
    fn native_progressive_resume_preserves_exact_final_pixels() {
        let library = Arc::new(
            Library::load(Path::new(
                &std::env::var_os("SCRUTIVUE_JXL_TEST_LIBRARY").unwrap(),
            ))
            .unwrap(),
        );
        let source = crate::dng::tiles::Source::open(Path::new(
            &std::env::var_os("SCRUTIVUE_PROGRESSIVE_TEST_DNG").unwrap(),
        ))
        .unwrap();
        let mut session = source.session(0, library.clone()).unwrap();
        assert!(session.advance(8, &|| true).is_err());
        assert_eq!(session.advance(8, &|| false).unwrap(), 8);
        assert!(!session.complete);
        // Paired streaming can resume this exclusively owned decoder on a
        // different pool thread without moving its registered heap buffers.
        let mut session = std::thread::spawn(move || {
            assert_eq!(session.advance(4, &|| false).unwrap(), 4);
            assert!(!session.complete);
            session
        })
        .join()
        .unwrap();
        assert_eq!(session.advance(2, &|| false).unwrap(), 2);
        assert!(!session.complete);
        assert_eq!(session.advance(1, &|| false).unwrap(), 1);
        assert!(session.complete);
        let mut fresh = source.session(0, library).unwrap();
        fresh.advance(1, &|| false).unwrap();
        assert_eq!(session.pixels, fresh.pixels);
    }

    #[test]
    #[cfg(feature = "experimental-jxl-parallel")]
    #[ignore = "requires SCRUTIVUE_JXL_TEST_LIBRARY and SCRUTIVUE_PROGRESSIVE_TEST_DNG"]
    fn native_parallel_runner_matches_sequential_tiles_and_progressive_stages() {
        let library = Arc::new(
            Library::load(Path::new(
                &std::env::var_os("SCRUTIVUE_JXL_TEST_LIBRARY").unwrap(),
            ))
            .unwrap(),
        );
        let source = crate::dng::tiles::Source::open(Path::new(
            &std::env::var_os("SCRUTIVUE_PROGRESSIVE_TEST_DNG").unwrap(),
        ))
        .unwrap();
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(6)
            .build()
            .unwrap();
        for index in [0, source.tiles.len() / 2, source.tiles.len() - 1] {
            let input = source.payload(index).unwrap().to_vec();
            let mut sequential =
                Session::create(library.clone(), input.clone(), source.tile_size, false).unwrap();
            let mut parallel =
                Session::create(library.clone(), input, source.tile_size, true).unwrap();
            assert!(parallel.advance(8, &|| true).is_err());
            for ratio in [8, 4, 2, 1] {
                let reference = sequential.advance(ratio, &|| false).unwrap();
                let result = pool.install(|| parallel.advance(ratio, &|| false)).unwrap();
                assert_eq!(result, reference);
                assert_eq!(parallel.size, sequential.size);
                assert_eq!(
                    parallel.pixels, sequential.pixels,
                    "tile {index}, ratio {ratio}"
                );
            }
            assert!(parallel.complete && sequential.complete);
        }
        // Also exercise the outside-pool CLI/progressive entry and decoder move.
        let mut session = source.session(0, library).unwrap();
        session.advance(8, &|| false).unwrap();
        std::thread::spawn(move || session.advance(1, &|| false).unwrap())
            .join()
            .unwrap();
    }
}
