//! Descriptor cleanup for the macOS launcher and broker.

use core::ffi::{c_int, c_void};
use core::{mem::size_of, ptr, slice};

#[repr(C)]
struct Descriptor {
    fd: i32,
    kind: u32,
}

unsafe extern "C" {
    safe fn getpid() -> c_int;
    fn proc_pidinfo(pid: c_int, flavor: c_int, arg: u64, buf: *mut c_void, size: c_int) -> c_int;
    fn malloc(size: usize) -> *mut c_void;
    fn free(buffer: *mut c_void);
    safe fn close(fd: c_int) -> c_int;
}

const PROC_PIDLISTFDS: c_int = 1;
const EBADF: c_int = 9;

fn keep(fd: c_int, output: bool, input: bool, kept: &[c_int]) -> bool {
    kept.iter().any(|&value| value >= 0 && fd == value)
        || fd == 2
        || (output && fd == 1)
        || (input && fd == 0)
}

// Startup owns the process descriptor table. Enumerate actual descriptors,
// including inherited descriptors above the current RLIMIT_NOFILE.
pub(super) fn witchy_close_inherited_descriptors(output: bool, input: bool, relay: c_int) -> bool {
    witchy_close_inherited_keeping(output, input, &[relay])
}

pub(super) fn witchy_close_inherited_keeping(output: bool, input: bool, kept: &[c_int]) -> bool {
    let pid = getpid();
    let size = unsafe { proc_pidinfo(pid, PROC_PIDLISTFDS, 0, ptr::null_mut(), 0) };
    if size <= 0 {
        return false;
    }
    let buffer = unsafe { malloc(size as usize) };
    if buffer.is_null() {
        return false;
    }
    super::errno::set(0);
    let used = unsafe { proc_pidinfo(pid, PROC_PIDLISTFDS, 0, buffer, size) };
    let item_size = size_of::<Descriptor>();
    let mut ok = used >= 0
        && used < size
        && (used as usize).is_multiple_of(item_size)
        && (used != 0 || super::errno::get() == 0);
    if ok {
        // proc_pidinfo initialized this many whole records in the allocation.
        let descriptors = unsafe {
            slice::from_raw_parts(buffer.cast::<Descriptor>(), used as usize / item_size)
        };
        for item in descriptors {
            if !keep(item.fd, output, input, kept)
                && close(item.fd) != 0
                && super::errno::get() != EBADF
            {
                ok = false;
            }
        }
    }
    unsafe { free(buffer) };
    ok
}
