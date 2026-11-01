//! JxlParallelRunner adapter for the pinned libjxl 0.12 public C ABI.
//! Reuse the caller's Rayon registry: nested tile jobs never create tile-local pools.
use rayon::prelude::*;
use std::{ffi::c_void, panic::AssertUnwindSafe, sync::OnceLock};

type Init = unsafe extern "C" fn(*mut c_void, usize) -> i32;
type Function = unsafe extern "C" fn(*mut c_void, u32, usize);
pub(super) type Runner =
    unsafe extern "C" fn(*mut c_void, *mut c_void, Init, Function, u32, u32) -> i32;

struct Task {
    opaque: *mut c_void,
    function: Function,
}
// libjxl's runner contract permits concurrent callbacks with this context after
// successful init. The synchronous parallel iterator joins before run returns.
unsafe impl Sync for Task {}
impl Task {
    fn execute(&self, value: u32) {
        // Rayon worker indices are stable and unique within this registry, and
        // strictly below the thread count supplied to libjxl's init callback.
        let index = rayon::current_thread_index().expect("JXL callback must run in its Rayon pool");
        unsafe { (self.function)(self.opaque, value, index) };
    }
}

fn fallback_pool() -> &'static rayon::ThreadPool {
    static POOL: OnceLock<rayon::ThreadPool> = OnceLock::new();
    POOL.get_or_init(|| {
        let threads = std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(2)
            .div_ceil(2)
            .clamp(2, 6);
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .thread_name(|i| format!("jxl-cli-{i}"))
            .build()
            .expect("JXL fallback worker pool")
    })
}

pub(super) unsafe extern "C" fn run(
    _runner: *mut c_void,
    opaque: *mut c_void,
    init: Init,
    function: Function,
    start: u32,
    end: u32,
) -> i32 {
    // A Rust panic must not unwind through the libjxl DLL's C stack.
    std::panic::catch_unwind(AssertUnwindSafe(|| {
        if start > end {
            return -1;
        }
        let in_pool = rayon::current_thread_index().is_some();
        let threads = if in_pool {
            rayon::current_num_threads()
        } else {
            fallback_pool().current_num_threads()
        };
        // The ABI requires init exactly once, ON THE CALLING THREAD, before
        // dispatching any work. Do not put this call inside pool.install.
        let status = unsafe { init(opaque, threads) };
        if status != 0 {
            return status;
        }
        let task = Task { opaque, function };
        let execute = || {
            (start..end)
                .into_par_iter()
                .for_each(|value| task.execute(value))
        };
        if in_pool {
            execute();
        } else {
            fallback_pool().install(execute);
        }
        0
    }))
    .unwrap_or(-1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Barrier, Mutex,
    };

    struct Calls {
        caller: std::thread::ThreadId,
        init_calls: AtomicUsize,
        threads: AtomicUsize,
        seen: Vec<AtomicUsize>,
        workers: Mutex<std::collections::HashSet<usize>>,
        init_error: i32,
        barrier: Option<Barrier>,
    }
    impl Calls {
        fn new(error: i32) -> Self {
            Self {
                caller: std::thread::current().id(),
                init_calls: AtomicUsize::new(0),
                threads: AtomicUsize::new(0),
                seen: (0..64).map(|_| AtomicUsize::new(0)).collect(),
                workers: Mutex::new(std::collections::HashSet::new()),
                init_error: error,
                barrier: None,
            }
        }
        fn invoke(&self, start: u32, end: u32) -> i32 {
            unsafe {
                run(
                    std::ptr::null_mut(),
                    (self as *const Self).cast_mut().cast(),
                    init,
                    call,
                    start,
                    end,
                )
            }
        }
    }
    unsafe extern "C" fn init(opaque: *mut c_void, threads: usize) -> i32 {
        let state = unsafe { &*opaque.cast::<Calls>() };
        assert_eq!(std::thread::current().id(), state.caller);
        assert!(threads > 0);
        state.init_calls.fetch_add(1, Ordering::SeqCst);
        state.threads.store(threads, Ordering::SeqCst);
        state.init_error
    }
    unsafe extern "C" fn call(opaque: *mut c_void, value: u32, index: usize) {
        let state = unsafe { &*opaque.cast::<Calls>() };
        assert_eq!(state.init_calls.load(Ordering::SeqCst), 1);
        assert!(index < state.threads.load(Ordering::SeqCst));
        state.seen[value as usize].fetch_add(1, Ordering::SeqCst);
        state.workers.lock().unwrap().insert(index);
        if let Some(barrier) = &state.barrier {
            barrier.wait();
        }
    }

    #[test]
    fn runner_initializes_once_on_caller_and_runs_each_value_once_outside_a_pool() {
        let state = Calls::new(0);
        assert_eq!(state.invoke(7, 57), 0);
        assert_eq!(state.init_calls.load(Ordering::SeqCst), 1);
        for (index, count) in state.seen.iter().enumerate() {
            assert_eq!(
                count.load(Ordering::SeqCst),
                usize::from((7..57).contains(&index))
            );
        }
        assert!(state.threads.load(Ordering::SeqCst) <= 6);
    }

    #[test]
    fn runner_reuses_caller_pool_and_really_overlaps_callbacks() {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .unwrap();
        pool.install(|| {
            let mut state = Calls::new(0);
            state.barrier = Some(Barrier::new(2));
            assert_eq!(state.invoke(0, 2), 0);
            assert_eq!(state.threads.load(Ordering::SeqCst), 2);
            assert_eq!(state.workers.lock().unwrap().len(), 2);
        });
    }

    #[test]
    fn runner_respects_init_errors_empty_ranges_and_invalid_ranges() {
        let error = Calls::new(17);
        assert_eq!(error.invoke(0, 64), 17);
        assert!(error.seen.iter().all(|v| v.load(Ordering::SeqCst) == 0));
        let empty = Calls::new(0);
        assert_eq!(empty.invoke(5, 5), 0);
        assert_eq!(empty.init_calls.load(Ordering::SeqCst), 1);
        let invalid = Calls::new(0);
        assert_eq!(invalid.invoke(5, 4), -1);
        assert_eq!(invalid.init_calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn concurrent_nested_runners_keep_independent_contexts_and_the_same_budget() {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(4)
            .build()
            .unwrap();
        let work = || {
            let state = Calls::new(0);
            assert_eq!(state.invoke(0, 64), 0);
            assert_eq!(state.threads.load(Ordering::SeqCst), 4);
            assert!(state.seen.iter().all(|v| v.load(Ordering::SeqCst) == 1));
        };
        pool.install(|| rayon::join(work, work));
    }
}
