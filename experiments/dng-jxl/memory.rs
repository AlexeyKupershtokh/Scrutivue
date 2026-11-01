//! Windows process counters; internal allocator bytes are a different quantity.
use serde_json::{json, Value};
use std::{
    ffi::c_void,
    sync::{Mutex, OnceLock},
    time::Instant,
};

#[repr(C)]
#[derive(Default)]
struct Counters {
    cb: u32,
    page_fault_count: u32,
    peak_working_set: usize,
    working_set: usize,
    quota_peak_paged: usize,
    quota_paged: usize,
    quota_peak_nonpaged: usize,
    quota_nonpaged: usize,
    pagefile: usize,
    peak_pagefile: usize,
    private_usage: usize,
}
#[link(name = "kernel32")]
extern "system" {
    fn GetCurrentProcess() -> *mut c_void;
}
#[link(name = "psapi")]
extern "system" {
    fn GetProcessMemoryInfo(process: *mut c_void, counters: *mut Counters, size: u32) -> i32;
}
static START: OnceLock<Instant> = OnceLock::new();
static CHECKPOINTS: Mutex<Vec<Value>> = Mutex::new(Vec::new());

pub fn snapshot() -> Value {
    let mut counters = Counters {
        cb: std::mem::size_of::<Counters>() as u32,
        ..Counters::default()
    };
    if unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.cb) } == 0 {
        return json!({"available": false});
    }
    json!({"rss_bytes": counters.working_set, "peak_rss_bytes": counters.peak_working_set,
        "private_bytes": counters.private_usage, "page_faults": counters.page_fault_count})
}
pub fn checkpoint(name: &str) {
    let mut value = snapshot();
    value["stage"] = json!(name);
    value["elapsed_ms"] = json!(START.get_or_init(Instant::now).elapsed().as_secs_f64() * 1000.0);
    CHECKPOINTS.lock().unwrap().push(value);
}
pub fn checkpoints() -> Vec<Value> {
    CHECKPOINTS.lock().unwrap().clone()
}
