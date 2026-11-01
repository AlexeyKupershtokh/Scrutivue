//! Buffered I/O and explicit first-touch measurements, not a physical-disk oracle.
use anyhow::{ensure, Context, Result};
use serde::Serialize;
use std::{fs::File, io::Read, path::Path, time::Instant};

pub fn ms(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}

#[derive(Default, Serialize)]
pub struct IoTimings {
    pub open_stat_ms: f64,
    pub reserve_ms: f64,
    pub zero_first_touch_ms: f64,
    pub buffered_read_ms: f64,
    pub map_ms: f64,
    pub mapped_prefault_ms: f64,
    pub commit_ms: f64,
    pub unbuffered_read_ms: f64,
    pub total_ms: f64,
    pub bytes: usize,
    pub mode: String,
    pub prefault_checksum: u64,
}

pub enum Input {
    Buffered(Vec<u8>),
    Mapped(memmap2::Mmap),
    Direct(DirectBuffer),
}
impl AsRef<[u8]> for Input {
    fn as_ref(&self) -> &[u8] {
        match self {
            Self::Buffered(bytes) => bytes,
            Self::Mapped(bytes) => bytes,
            Self::Direct(bytes) => unsafe {
                std::slice::from_raw_parts(bytes.pointer, bytes.length)
            },
        }
    }
}

pub struct DirectBuffer {
    pointer: *mut u8,
    length: usize,
}
#[link(name = "kernel32")]
extern "system" {
    fn VirtualAlloc(
        address: *mut std::ffi::c_void,
        bytes: usize,
        kind: u32,
        protection: u32,
    ) -> *mut std::ffi::c_void;
    fn VirtualFree(address: *mut std::ffi::c_void, bytes: usize, kind: u32) -> i32;
}
impl Drop for DirectBuffer {
    fn drop(&mut self) {
        unsafe {
            VirtualFree(self.pointer.cast(), 0, 0x8000);
        }
    }
}

/// FILE_FLAG_NO_BUFFERING bypasses the Windows file cache, not the drive's cache.
/// Requests and buffers are 64 KiB aligned; unsupported devices fail explicitly.
pub fn read_direct(path: &Path) -> Result<(Input, IoTimings)> {
    use std::os::windows::fs::OpenOptionsExt;
    let total = Instant::now();
    let mut t = IoTimings {
        mode: "unbuffered_no_os_cache".into(),
        ..IoTimings::default()
    };
    let start = Instant::now();
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(0x20000000 | 0x08000000)
        .open(path)?;
    let before = file.metadata()?;
    let length = usize::try_from(before.len())?;
    ensure!(
        before.is_file() && length > 0 && length <= 1024 * 1024 * 1024,
        "Expected a nonempty input of at most 1 GiB"
    );
    t.open_stat_ms = ms(start);
    t.bytes = length;
    let capacity = length.next_multiple_of(65536);
    let start = Instant::now();
    let pointer = unsafe { VirtualAlloc(std::ptr::null_mut(), capacity, 0x2000, 1) }.cast::<u8>();
    ensure!(
        !pointer.is_null(),
        "Reserve direct I/O buffer: {}",
        std::io::Error::last_os_error()
    );
    let buffer = DirectBuffer { pointer, length };
    t.reserve_ms = ms(start);
    let start = Instant::now();
    let committed = unsafe { VirtualAlloc(pointer.cast(), capacity, 0x1000, 4) };
    ensure!(
        !committed.is_null(),
        "Commit direct I/O buffer: {}",
        std::io::Error::last_os_error()
    );
    t.commit_ms = ms(start);
    let start = Instant::now();
    unsafe { std::ptr::write_bytes(pointer, 0, capacity) };
    let bytes = unsafe { std::slice::from_raw_parts_mut(pointer, capacity) };
    std::hint::black_box(&*bytes);
    t.zero_first_touch_ms = ms(start);
    let start = Instant::now();
    let read = file.read(bytes)?;
    t.unbuffered_read_ms = ms(start);
    ensure!(
        read == length,
        "Short unbuffered read: expected {length}, got {read}"
    );
    let after = file.metadata()?;
    ensure!(
        after.len() == before.len() && after.modified().ok() == before.modified().ok(),
        "Source changed during read"
    );
    t.total_ms = ms(total);
    Ok((Input::Direct(buffer), t))
}

pub fn read(path: &Path, mapped: bool) -> Result<(Input, IoTimings)> {
    let total = Instant::now();
    let mut t = IoTimings::default();
    let start = Instant::now();
    let mut file = File::open(path).with_context(|| format!("Read {}", path.display()))?;
    let before = file.metadata()?;
    let length = usize::try_from(before.len())?;
    ensure!(
        before.is_file() && length > 0 && length <= 1024 * 1024 * 1024,
        "Expected a nonempty input of at most 1 GiB"
    );
    t.open_stat_ms = ms(start);
    t.bytes = length;
    let input = if mapped {
        t.mode = "mmap_prefault".into();
        let start = Instant::now();
        // The read-only mapping and its source must not be truncated during the run.
        let bytes = unsafe { memmap2::MmapOptions::new().map(&file)? };
        t.map_ms = ms(start);
        let start = Instant::now();
        let mut sum = 0u64;
        for index in (0..length).step_by(4096) {
            sum = sum
                .wrapping_add(unsafe { std::ptr::read_volatile(bytes.as_ptr().add(index)) } as u64);
        }
        sum = sum.wrapping_add(bytes[length - 1] as u64);
        t.prefault_checksum = std::hint::black_box(sum);
        t.mapped_prefault_ms = ms(start);
        Input::Mapped(bytes)
    } else {
        t.mode = "buffered".into();
        let start = Instant::now();
        let mut bytes = Vec::with_capacity(length);
        t.reserve_ms = ms(start);
        let start = Instant::now();
        bytes.resize(length, 0);
        t.zero_first_touch_ms = ms(start);
        let start = Instant::now();
        file.read_exact(&mut bytes)?;
        t.buffered_read_ms = ms(start);
        let mut extra = [0];
        ensure!(file.read(&mut extra)? == 0, "Input grew during read");
        Input::Buffered(bytes)
    };
    let after = file.metadata()?;
    ensure!(
        after.len() == before.len() && after.modified().ok() == before.modified().ok(),
        "Source changed during read"
    );
    t.total_ms = ms(total);
    Ok((input, t))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_io_modes_preserve_a_non_sector_aligned_file() {
        let root =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/dng-jxl-io-tests");
        std::fs::create_dir_all(&root).unwrap();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = root.join(format!("unaligned-{stamp}.bin"));
        let bytes: Vec<_> = (0..10003usize)
            .map(|i| (i.wrapping_mul(73) & 255) as u8)
            .collect();
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .unwrap();
        std::io::Write::write_all(&mut file, &bytes).unwrap();
        drop(file);
        for mapped in [false, true] {
            let (input, _) = read(&path, mapped).unwrap();
            assert_eq!(input.as_ref(), bytes);
        }
        let (input, _) = read_direct(&path).unwrap();
        assert_eq!(input.as_ref(), bytes);
        drop(input);
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        std::fs::remove_file(path).unwrap();
    }
}
