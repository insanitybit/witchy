//! The calling thread's C error number.

use core::ffi::c_int;

unsafe extern "C" {
    #[cfg_attr(target_os = "linux", link_name = "__errno_location")]
    fn __error() -> *mut c_int;
}

/// The error number the last failed C library call left.
pub fn get() -> c_int {
    // SAFETY: the C library returns a valid pointer to this thread's error number.
    unsafe { *__error() }
}

/// Sets the error number, as before a call whose only failure report is the number.
pub fn set(value: c_int) {
    // SAFETY: as in `get`; nothing else holds a reference to the number.
    unsafe { *__error() = value }
}
