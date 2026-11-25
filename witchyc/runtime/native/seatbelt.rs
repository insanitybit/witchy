//! macOS ARM64 Seatbelt startup adapter.

use super::seatbelt_fds::witchy_close_inherited_descriptors;
use core::ffi::{c_char, c_int, c_void};
use core::{mem::MaybeUninit, ptr};

const READ_ONLY: c_int = 0;
const WRITE_ONLY: c_int = 1;
const READ_WRITE: c_int = 2;
const ACCESS_MASK: c_int = 3;
const NONBLOCK: c_int = 4;
const NOFOLLOW: c_int = 0x100;
const NOCTTY: c_int = 0x20000;
const CLOEXEC: c_int = 0x1000000;
const GETFD: c_int = 1;
const GETFL: c_int = 3;
const EBADF: c_int = 9;

// Darwin's 64-bit inode stat layout.
#[repr(C)]
pub(super) struct Stat {
    pub(super) device: i32,
    pub(super) mode: u16,
    pub(super) links: u16,
    pub(super) inode: u64,
    pub(super) uid: u32,
    gid: u32,
    special_device: i32,
    times: [[i64; 2]; 4],
    pub(super) size: i64,
    blocks: i64,
    block_size: i32,
    flags: u32,
    generation: u32,
    spare: i32,
    reserved: [i64; 2],
}

unsafe extern "C" {
    fn fcntl(fd: c_int, command: c_int, ...) -> c_int;
    fn open(path: *const c_char, flags: c_int, ...) -> c_int;
    safe fn close(fd: c_int) -> c_int;
    safe fn dup2(from: c_int, to: c_int) -> c_int;
    fn ttyname_r(fd: c_int, path: *mut c_char, len: usize) -> c_int;
    fn fstat(fd: c_int, stat: *mut Stat) -> c_int;
    fn sandbox_init(profile: *const c_char, flags: u64, error: *mut *mut c_char) -> c_int;
    fn sandbox_free_error(error: *mut c_char);
    fn fputs(message: *const c_char, stream: *mut c_void) -> c_int;
    fn fprintf(stream: *mut c_void, format: *const c_char, ...) -> c_int;
    static mut __stderrp: *mut c_void;
}

pub(super) fn stat(fd: c_int) -> Option<Stat> {
    let mut value = MaybeUninit::<Stat>::uninit();
    if unsafe { fstat(fd, value.as_mut_ptr()) } != 0 {
        return None;
    }
    Some(unsafe { value.assume_init() })
}

fn restrict_stream(stream: c_int, mode: c_int) -> bool {
    let flags = unsafe { fcntl(stream, GETFL) };
    if flags < 0 {
        return false;
    }
    match flags & ACCESS_MASK {
        access if access == mode => return true,
        READ_WRITE => {}
        _ => return false,
    }

    // Keep only the requested direction. Reopen an interactive terminal
    // and confirm its identity before replacing the inherited descriptor.
    let mut path = [0; 1024];
    if unsafe { ttyname_r(stream, path.as_mut_ptr(), path.len()) } != 0 {
        return false;
    }
    let Some(original) = stat(stream) else {
        return false;
    };
    let fd = unsafe {
        open(
            path.as_ptr(),
            mode | NOFOLLOW | NOCTTY | CLOEXEC | (flags & NONBLOCK),
        )
    };
    if fd < 0 {
        return false;
    }
    let ok = stat(fd).is_some_and(|reopened| {
        reopened.mode & 0o170000 == 0o020000
            && original.device == reopened.device
            && original.inode == reopened.inode
            && original.special_device == reopened.special_device
            && dup2(fd, stream) == stream
    });
    close(fd) == 0 && ok
}

#[unsafe(no_mangle)]
extern "C" fn witchy_install_sandbox_with_relay(
    output: bool,
    _allocator: bool,
    input: bool,
    relay: c_int,
) -> bool {
    if relay >= 0 && (relay <= 2 || unsafe { fcntl(relay, GETFD) } < 0) {
        return false;
    }
    if output && !restrict_stream(1, WRITE_ONLY) {
        unsafe {
            fputs(
                c"native sandbox requires write-only stdout or a terminal\n".as_ptr(),
                __stderrp,
            )
        };
        return false;
    }
    if input && !restrict_stream(0, READ_ONLY) {
        unsafe {
            fputs(
                c"native sandbox requires read-only stdin or a terminal\n".as_ptr(),
                __stderrp,
            )
        };
        return false;
    }

    let mut error = ptr::null_mut();
    let installed =
        unsafe { sandbox_init(c"(version 1)\n(deny default)\n".as_ptr(), 0, &mut error) };
    if installed != 0 {
        let message = if error.is_null() {
            c"failed".as_ptr()
        } else {
            error
        };
        unsafe {
            fprintf(
                __stderrp,
                c"native sandbox Seatbelt installation: %s\n".as_ptr(),
                message,
            )
        };
    }
    unsafe { sandbox_free_error(error) };
    if installed != 0 {
        return false;
    }
    if !witchy_close_inherited_descriptors(output, input, relay) {
        unsafe {
            fputs(
                c"native sandbox descriptor cleanup failed\n".as_ptr(),
                __stderrp,
            )
        };
        return false;
    }
    close(2) == 0 || super::errno::get() == EBADF
}

#[unsafe(no_mangle)]
extern "C" fn witchy_install_sandbox(output: bool, allocator: bool) -> bool {
    witchy_install_sandbox_with_relay(output, allocator, false, -1)
}
