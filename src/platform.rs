#[cfg(windows)]
pub fn style_window(window: &gpui_kit::Window) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use std::ffi::c_void;
    #[link(name = "dwmapi")]
    unsafe extern "system" {
        fn DwmSetWindowAttribute(
            hwnd: *mut c_void,
            attribute: u32,
            value: *const c_void,
            size: u32,
        ) -> i32;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetModuleHandleW(name: *const u16) -> *mut c_void;
    }
    #[link(name = "user32")]
    unsafe extern "system" {
        fn LoadImageW(
            instance: *mut c_void,
            name: *const u16,
            kind: u32,
            width: i32,
            height: i32,
            flags: u32,
        ) -> *mut c_void;
        fn SendMessageW(hwnd: *mut c_void, message: u32, wparam: usize, lparam: isize) -> isize;
    }
    if let Ok(handle) = HasWindowHandle::window_handle(window) {
        if let RawWindowHandle::Win32(handle) = handle.as_raw() {
            unsafe {
                for (kind, size) in [(0, 16), (1, 32)] {
                    let icon = LoadImageW(
                        GetModuleHandleW(std::ptr::null()),
                        1usize as *const u16,
                        1,
                        size,
                        size,
                        0x8000,
                    );
                    if !icon.is_null() {
                        SendMessageW(handle.hwnd.get() as *mut c_void, 0x80, kind, icon as isize);
                    }
                }
            }
            let enabled = 1i32;
            // Override only this native titlebar, not the system appearance.
            unsafe {
                DwmSetWindowAttribute(
                    handle.hwnd.get() as *mut c_void,
                    20,
                    (&enabled as *const i32).cast(),
                    4,
                );
            }
        }
    }
}

#[cfg(not(windows))]
pub fn style_window(_: &gpui_kit::Window) {}

pub fn app_icon() -> std::sync::Arc<gpui_kit::RenderImage> {
    static ICON: std::sync::OnceLock<std::sync::Arc<gpui_kit::RenderImage>> =
        std::sync::OnceLock::new();
    ICON.get_or_init(|| {
        let mut image = image::load_from_memory(include_bytes!("../assets/lumen-diff.png"))
            .expect("bundled icon")
            .into_rgba8();
        for pixel in image.pixels_mut() {
            pixel.0.swap(0, 2);
        }
        std::sync::Arc::new(gpui_kit::RenderImage::new(vec![image::Frame::new(image)]))
    })
    .clone()
}
