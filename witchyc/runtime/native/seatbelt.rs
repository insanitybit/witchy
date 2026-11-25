//! macOS ARM64 Seatbelt startup adapter.

use super::seatbelt_fds::witchy_close_inherited_descriptors;
use super::sys::{self, Stream};
use core::ffi::{CStr, c_char, c_int, c_void};
use core::ptr;

const READ_ONLY: c_int = 0;
const WRITE_ONLY: c_int = 1;
const READ_WRITE: c_int = 2;
const ACCESS_MASK: c_int = 3;
const NONBLOCK: c_int = 4;
const NOFOLLOW: c_int = 0x100;
const NOCTTY: c_int = 0x20000;
const CLOEXEC: c_int = 0x1000000;
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

impl Stat {
    /// What a call fills in. Every field is an integer, so zero is a value of each.
    const ZERO: Self = Self {
        device: 0,
        mode: 0,
        links: 0,
        inode: 0,
        uid: 0,
        gid: 0,
        special_device: 0,
        times: [[0; 2]; 4],
        size: 0,
        blocks: 0,
        block_size: 0,
        flags: 0,
        generation: 0,
        spare: 0,
        reserved: [0; 2],
    };
}

unsafe extern "C" {
    safe fn close(fd: c_int) -> c_int;
    safe fn dup2(from: c_int, to: c_int) -> c_int;
    fn ttyname_r(fd: c_int, path: *mut c_char, len: usize) -> c_int;
    fn fstat(fd: c_int, stat: *mut Stat) -> c_int;
    fn fstatat(fd: c_int, name: *const c_char, info: *mut Stat, flags: c_int) -> c_int;
    fn sandbox_init(profile: *const c_char, flags: u64, error: *mut *mut c_char) -> c_int;
    fn sandbox_free_error(error: *mut c_char);
    fn fprintf(stream: *mut c_void, format: *const c_char, ...) -> c_int;
}

pub(super) fn stat(fd: c_int) -> Option<Stat> {
    let mut value = Stat::ZERO;
    // SAFETY: the call writes one `Stat`, and `value` is one.
    if unsafe { fstat(fd, &mut value) } != 0 {
        return None;
    }
    Some(value)
}

/// The object `name` names under the directory `fd`, looked up as `flags` say.
pub(super) fn stat_at(fd: c_int, name: &CStr, flags: c_int) -> Option<Stat> {
    let mut value = Stat::ZERO;
    // SAFETY: `name` is a C string, and the call writes one `Stat`.
    if unsafe { fstatat(fd, name.as_ptr(), &mut value, flags) } != 0 {
        return None;
    }
    Some(value)
}

fn restrict_stream(stream: c_int, mode: c_int) -> bool {
    let flags = sys::status_flags(stream);
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
    let mut path = [0u8; 1024];
    // SAFETY: the call writes at most `path.len()` bytes to `path`.
    if unsafe { ttyname_r(stream, path.as_mut_ptr().cast(), path.len()) } != 0 {
        return false;
    }
    let Some(original) = stat(stream) else {
        return false;
    };
    // The call succeeded, so it wrote a name and the NUL after it.
    let Ok(path) = CStr::from_bytes_until_nul(&path) else {
        return false;
    };
    let fd = sys::open(
        path,
        mode | NOFOLLOW | NOCTTY | CLOEXEC | (flags & NONBLOCK),
    );
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
    if relay >= 0 && (relay <= 2 || sys::descriptor_flags(relay) < 0) {
        return false;
    }
    if output && !restrict_stream(1, WRITE_ONLY) {
        Stream::stderr().put(c"native sandbox requires write-only stdout or a terminal\n");
        return false;
    }
    if input && !restrict_stream(0, READ_ONLY) {
        Stream::stderr().put(c"native sandbox requires read-only stdin or a terminal\n");
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
        // SAFETY: the format takes one string, and `message` is the error the
        // call gave or a literal.
        unsafe {
            fprintf(
                Stream::stderr().as_ptr(),
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
        Stream::stderr().put(c"native sandbox descriptor cleanup failed\n");
        return false;
    }
    close(2) == 0 || super::errno::get() == EBADF
}

#[unsafe(no_mangle)]
extern "C" fn witchy_install_sandbox(output: bool, allocator: bool) -> bool {
    witchy_install_sandbox_with_relay(output, allocator, false, -1)
}
