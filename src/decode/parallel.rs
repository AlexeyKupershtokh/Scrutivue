//! Synchronous native RAW jobs share the caller's bounded Rayon registry.
use rayon::prelude::*;
use std::{ffi::c_void, panic::AssertUnwindSafe, sync::OnceLock};

type Callback = unsafe extern "C" fn(*mut c_void, u32);
struct Work {
    context: *mut c_void,
    callback: Callback,
}
// Native callers supply independent jobs and retain their context until the
// synchronous iterator joins. C++ catches every exception inside each callback.
unsafe impl Sync for Work {}
impl Work {
    fn execute(&self, index: u32) {
        unsafe { (self.callback)(self.context, index) };
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
            .thread_name(|index| format!("raw-cli-{index}"))
            .build()
            .expect("RAW fallback pool")
    })
}

#[no_mangle]
unsafe extern "C" fn diff_raw_parallel_for(
    context: *mut c_void,
    callback: Callback,
    count: u32,
) -> i32 {
    // Never unwind a Rust panic into the C++ caller. All work is joined before
    // returning, including when a native callback records an error/cancellation.
    std::panic::catch_unwind(AssertUnwindSafe(|| {
        let work = Work { context, callback };
        let execute = || {
            (0..count)
                .into_par_iter()
                .for_each(|index| work.execute(index))
        };
        if rayon::current_thread_index().is_some() {
            execute();
        } else {
            fallback_pool().install(execute);
        }
    }))
    .map(|_| 0)
    .unwrap_or(-1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Barrier,
    };

    unsafe extern "C" fn increment(context: *mut c_void, index: u32) {
        let counts = unsafe { &*context.cast::<Vec<AtomicUsize>>() };
        counts[index as usize].fetch_add(1, Ordering::SeqCst);
    }

    #[test]
    fn native_raw_jobs_run_exactly_once_including_empty_and_nested_calls() {
        let counts: Vec<_> = (0..87).map(|_| AtomicUsize::new(0)).collect();
        let invoke = || unsafe {
            diff_raw_parallel_for(
                (&counts as *const Vec<AtomicUsize>).cast_mut().cast(),
                increment,
                87,
            )
        };
        assert_eq!(invoke(), 0);
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .unwrap();
        pool.install(|| rayon::join(|| assert_eq!(invoke(), 0), || assert_eq!(invoke(), 0)));
        assert!(counts.iter().all(|count| count.load(Ordering::SeqCst) == 3));
        assert_eq!(
            unsafe { diff_raw_parallel_for(std::ptr::null_mut(), increment, 0) },
            0
        );
        assert!(fallback_pool().current_num_threads() <= 6);
    }

    #[test]
    fn native_raw_jobs_reuse_the_callers_pool_and_really_overlap() {
        struct State {
            barrier: Barrier,
            calls: AtomicUsize,
        }
        unsafe extern "C" fn call(context: *mut c_void, _: u32) {
            let state = unsafe { &*context.cast::<State>() };
            assert_eq!(rayon::current_num_threads(), 2);
            state.calls.fetch_add(1, Ordering::SeqCst);
            state.barrier.wait();
        }
        let state = State {
            barrier: Barrier::new(2),
            calls: AtomicUsize::new(0),
        };
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .unwrap();
        let result = pool.install(|| unsafe {
            diff_raw_parallel_for((&state as *const State).cast_mut().cast(), call, 2)
        });
        assert_eq!(result, 0);
        assert_eq!(state.calls.load(Ordering::SeqCst), 2);
    }
}
