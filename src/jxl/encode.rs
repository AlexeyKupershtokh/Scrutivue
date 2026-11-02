//! Small display-only JPEG XL previews, using the existing hash-pinned runtime.
use super::*;
use crate::decode::Frame;

pub const DISTANCE: f32 = 0.05;
pub const EFFORT: i64 = 3;

/// Encode developed sRGB16, never camera-linear RAW or an authoritative frame.
pub fn encode_preview(
    library: &Arc<Library>,
    frame: &Frame,
    cancelled: &dyn Fn() -> bool,
) -> Result<Vec<u8>> {
    ensure!(
        frame.width > 0 && frame.height > 0 && frame.width.max(frame.height) <= 2560,
        "Invalid cache preview dimensions"
    );
    ensure!(
        frame.rgb.len() == frame.width as usize * frame.height as usize * 3,
        "Invalid cache preview samples"
    );
    unsafe fn symbol<T: Copy>(library: &Library, name: &[u8]) -> Result<T> {
        Ok(*unsafe { library._library.get::<T>(name)? })
    }
    struct Encoder {
        handle: *mut c_void,
        destroy: unsafe extern "C" fn(*mut c_void),
    }
    impl Drop for Encoder {
        fn drop(&mut self) {
            unsafe { (self.destroy)(self.handle) }
        }
    }
    unsafe {
        let create: unsafe extern "C" fn(*const c_void) -> *mut c_void =
            symbol(library, b"JxlEncoderCreate\0")?;
        let destroy = symbol(library, b"JxlEncoderDestroy\0")?;
        let handle =
            NonNull::new(create(std::ptr::null())).context("Create JPEG XL cache encoder")?;
        let encoder = Encoder {
            handle: handle.as_ptr(),
            destroy,
        };
        let init: unsafe extern "C" fn(*mut Basic) = symbol(library, b"JxlEncoderInitBasicInfo\0")?;
        let set_info: unsafe extern "C" fn(*mut c_void, *const Basic) -> i32 =
            symbol(library, b"JxlEncoderSetBasicInfo\0")?;
        let srgb: unsafe extern "C" fn(*mut Color, i32) =
            symbol(library, b"JxlColorEncodingSetToSRGB\0")?;
        let set_color: unsafe extern "C" fn(*mut c_void, *const Color) -> i32 =
            symbol(library, b"JxlEncoderSetColorEncoding\0")?;
        let settings: unsafe extern "C" fn(*mut c_void, *const c_void) -> *mut c_void =
            symbol(library, b"JxlEncoderFrameSettingsCreate\0")?;
        let distance: unsafe extern "C" fn(*mut c_void, f32) -> i32 =
            symbol(library, b"JxlEncoderSetFrameDistance\0")?;
        let option: unsafe extern "C" fn(*mut c_void, i32, i64) -> i32 =
            symbol(library, b"JxlEncoderFrameSettingsSetOption\0")?;
        let add: unsafe extern "C" fn(*const c_void, *const Format, *const c_void, usize) -> i32 =
            symbol(library, b"JxlEncoderAddImageFrame\0")?;
        let close: unsafe extern "C" fn(*mut c_void) = symbol(library, b"JxlEncoderCloseInput\0")?;
        let process: unsafe extern "C" fn(*mut c_void, *mut *mut u8, *mut usize) -> i32 =
            symbol(library, b"JxlEncoderProcessOutput\0")?;
        #[cfg(feature = "experimental-jxl-parallel")]
        {
            let runner: unsafe extern "C" fn(
                *mut c_void,
                Option<parallel::Runner>,
                *mut c_void,
            ) -> i32 = symbol(library, b"JxlEncoderSetParallelRunner\0")?;
            check(runner(
                encoder.handle,
                Some(parallel::run),
                std::ptr::null_mut(),
            ))?;
        }
        let mut basic = Basic::default();
        init(&mut basic);
        basic.width = frame.width;
        basic.height = frame.height;
        basic.bits = 16;
        basic.channels = 3;
        basic.original = 0;
        check(set_info(encoder.handle, &basic))?;
        let mut color = Color::default();
        srgb(&mut color, 0);
        check(set_color(encoder.handle, &color))?;
        let settings = settings(encoder.handle, std::ptr::null());
        ensure!(!settings.is_null(), "Create JPEG XL cache frame settings");
        check(distance(settings, DISTANCE))?;
        check(option(settings, 0, EFFORT))?;
        ensure!(!cancelled(), "Cache preview encoding cancelled");
        let format = Format {
            channels: 3,
            kind: 3,
            endian: 0,
            align: 0,
        };
        check(add(
            settings,
            &format,
            frame.rgb.as_ptr().cast(),
            frame.bytes(),
        ))?;
        close(encoder.handle);
        let mut output = Vec::new();
        loop {
            ensure!(!cancelled(), "Cache preview encoding cancelled");
            let mut chunk = [0u8; 65536];
            let mut pointer = chunk.as_mut_ptr();
            let mut remaining = chunk.len();
            let status = process(encoder.handle, &mut pointer, &mut remaining);
            output.extend_from_slice(&chunk[..chunk.len() - remaining]);
            ensure!(
                output.len() <= 64 * 1024 * 1024,
                "JPEG XL cache preview exceeds size limit"
            );
            match status {
                0 => return Ok(output),
                2 => {}
                other => anyhow::bail!("JPEG XL cache encoding failed ({other})"),
            }
        }
    }
}
