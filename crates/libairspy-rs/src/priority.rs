//! `SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST)`,
//! which `consumer_threadproc` and `transfer_threadproc` each do first
//! thing on Windows (airspy.c, `#ifdef _WIN32`). C does nothing on
//! other systems and neither does this: a program that wants more
//! there raises the threads by name (`airspy-reader`,
//! `airspy-consumer`).

/// Raise the calling thread as C does on Windows; nothing elsewhere.
/// A refusal is ignored, as in C.
pub(crate) fn raise() {
    #[cfg(windows)]
    windows::raise();
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod windows {
    /// `THREAD_PRIORITY_HIGHEST` (winbase.h).
    const THREAD_PRIORITY_HIGHEST: i32 = 2;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentThread() -> *mut core::ffi::c_void;
        fn SetThreadPriority(thread: *mut core::ffi::c_void, priority: i32) -> i32;
    }

    pub(super) fn raise() {
        // SAFETY: GetCurrentThread returns a pseudo-handle that is always
        // valid for the calling thread and needs no closing;
        // SetThreadPriority only reads it.
        unsafe {
            SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST);
        }
    }
}
