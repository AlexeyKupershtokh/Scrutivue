//! Background desktop integration, separate from image/decode services.
use crate::services::Event;
use crossbeam_channel::{bounded, unbounded, Receiver, Sender};
use std::path::{Path, PathBuf};

pub struct FileActions {
    jobs: Sender<PathBuf>,
    pub events: Receiver<Event>,
}

impl FileActions {
    pub fn new() -> Self {
        let (jobs, paths) = bounded::<PathBuf>(4);
        let (sender, events) = unbounded();
        std::thread::Builder::new()
            .name("file-actions".into())
            .spawn(move || {
                for path in paths {
                    if let Err(error) = open_containing_folder(&path) {
                        let _ = sender.send(Event::Error(format!(
                            "Could not open the containing folder for {}: {error:#}",
                            path.display()
                        )));
                    }
                }
            })
            .expect("file actions thread");
        Self { jobs, events }
    }

    pub fn open_containing_folder(&self, path: PathBuf) -> Result<(), String> {
        self.jobs.try_send(path).map_err(|_| {
            "Could not open folder: the file action queue is busy or unavailable".into()
        })
    }
}

fn containing_folder(path: &Path) -> anyhow::Result<PathBuf> {
    use anyhow::Context;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    // Resolve only the parent: a removed photo should not stop its folder opening.
    parent
        .canonicalize()
        .with_context(|| format!("Could not resolve folder {}", parent.display()))
}

fn open_containing_folder(path: &Path) -> anyhow::Result<()> {
    open_directory(&containing_folder(path)?)
}

#[cfg(windows)]
fn open_directory(path: &Path) -> anyhow::Result<()> {
    use std::{ffi::c_void, os::windows::ffi::OsStrExt, ptr};

    #[link(name = "ole32")]
    unsafe extern "system" {
        fn CoInitializeEx(reserved: *mut c_void, flags: u32) -> i32;
        fn CoUninitialize();
        fn CoTaskMemFree(memory: *mut c_void);
    }
    #[link(name = "shell32")]
    unsafe extern "system" {
        fn SHParseDisplayName(
            name: *const u16,
            bind_context: *mut c_void,
            item: *mut *mut c_void,
            attributes_in: u32,
            attributes_out: *mut u32,
        ) -> i32;
        fn SHOpenFolderAndSelectItems(
            folder: *const c_void,
            count: u32,
            items: *const *const c_void,
            flags: u32,
        ) -> i32;
    }

    fn check(result: i32, operation: &str) -> anyhow::Result<()> {
        anyhow::ensure!(
            result >= 0,
            "{operation} failed (Windows error 0x{:08X})",
            result as u32
        );
        Ok(())
    }

    struct ComApartment;
    impl Drop for ComApartment {
        fn drop(&mut self) {
            unsafe { CoUninitialize() };
        }
    }
    struct ShellItem(*mut c_void);
    impl Drop for ShellItem {
        fn drop(&mut self) {
            unsafe { CoTaskMemFree(self.0) };
        }
    }

    // canonicalize produces extended-length paths; Explorer's shell namespace
    // expects normal drive/UNC paths. Work in UTF-16 to preserve Unicode paths.
    let path = explorer_path(path.as_os_str().encode_wide().collect());
    anyhow::ensure!(!path.contains(&0), "Folder path contains a NUL character");
    let path: Vec<u16> = path.into_iter().chain(Some(0)).collect();
    check(
        unsafe { CoInitializeEx(ptr::null_mut(), 2) },
        "COM initialization",
    )?;
    let _apartment = ComApartment;
    let mut item = ShellItem(ptr::null_mut());
    check(
        unsafe {
            SHParseDisplayName(
                path.as_ptr(),
                ptr::null_mut(),
                &mut item.0,
                0,
                ptr::null_mut(),
            )
        },
        "Finding the folder",
    )?;
    // Passing zero items opens the directory itself in Windows Explorer. No
    // command shell, command-line quoting or user-controlled executable is used.
    check(
        unsafe { SHOpenFolderAndSelectItems(item.0, 0, ptr::null(), 0) },
        "Opening Windows Explorer",
    )
}

#[cfg(windows)]
fn explorer_path(path: Vec<u16>) -> Vec<u16> {
    let extended: Vec<u16> = "\\\\?\\".encode_utf16().collect();
    let unc: Vec<u16> = "\\\\?\\UNC\\".encode_utf16().collect();
    if path.starts_with(&unc) {
        "\\\\"
            .encode_utf16()
            .chain(path[unc.len()..].iter().copied())
            .collect()
    } else if path.starts_with(&extended) {
        path[extended.len()..].to_vec()
    } else {
        path
    }
}

#[cfg(not(windows))]
fn open_directory(path: &Path) -> anyhow::Result<()> {
    use anyhow::Context;
    #[cfg(target_os = "macos")]
    let mut command = std::process::Command::new("open");
    #[cfg(not(target_os = "macos"))]
    let mut command = std::process::Command::new("xdg-open");
    let status = command
        .arg(path)
        .status()
        .context("Starting the file manager")?;
    anyhow::ensure!(status.success(), "The file manager exited with {status}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_photo_still_resolves_its_existing_folder() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        assert_eq!(
            containing_folder(&root.join("not-a-real-photo.png")).unwrap(),
            root.canonicalize().unwrap()
        );
        assert!(containing_folder(&root.join("not-a-real-folder/photo.png")).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn explorer_paths_keep_spaces_unicode_and_shell_metacharacters_literal() {
        for (input, expected) in [
            (
                r"\\?\C:\Photo set\snow & sunshine (1)\image.png",
                r"C:\Photo set\snow & sunshine (1)\image.png",
            ),
            (
                r"\\?\UNC\server\share\Photo set",
                r"\\server\share\Photo set",
            ),
            (r"C:\Photos\日本語 & 100%", r"C:\Photos\日本語 & 100%"),
        ] {
            assert_eq!(
                String::from_utf16(&explorer_path(input.encode_utf16().collect())).unwrap(),
                expected
            );
        }
    }
}
