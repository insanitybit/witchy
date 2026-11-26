//! Native stdin and file-backed input grants.

use super::global::Global;
use super::file_identity::Identity;
#[cfg(target_os = "linux")]
use super::linux_stat::stat;
#[cfg(target_os = "macos")]
use super::seatbelt::stat;
use core::ffi::{c_int, c_void};

unsafe extern "C" {
    safe fn dup2(from: c_int, to: c_int) -> c_int;
    safe fn close(fd: c_int) -> c_int;
    safe fn getchar() -> c_int;
    safe fn exit(status: c_int) -> !;
    fn read(fd: c_int, bytes: *mut c_void, length: usize) -> isize;
    fn malloc(size: usize) -> *mut c_void;
    fn free(value: *mut c_void);
}

// Initialized before guest entry. Input calls do not suspend or reenter.
static LEFT: Global<u64> = Global::new(u64::MAX);
#[derive(Clone, Copy)]
struct Snapshot {
    bytes: *mut u8,
    len: usize,
    at: usize,
}
static SNAPSHOT: Global<Option<Snapshot>> = Global::new(None);

/// Snapshot server input once, before any worker starts. Each fork inherits a
/// fresh cursor and sees the same bounded bytes.
pub(super) fn snapshot_input(limit: u64) -> bool {
    if limit > 16 * 1024 * 1024 {
        return false;
    }
    let capacity = limit as usize + 1;
    let bytes = unsafe { malloc(capacity) }.cast::<u8>();
    if bytes.is_null() {
        return false;
    }
    let mut used = 0;
    while used < capacity {
        let count = unsafe { read(0, bytes.add(used).cast(), capacity - used) };
        if count < 0 && super::errno::get() == 4 {
            continue;
        }
        if count < 0 {
            unsafe {
                free(bytes.cast());
            }
            return false;
        }
        if count == 0 {
            SNAPSHOT.set(Some(Snapshot {
                bytes,
                len: used,
                at: 0,
            }));
            return true;
        }
        used += count as usize;
    }
    unsafe {
        free(bytes.cast());
    }
    false
}

fn witchy_input_limit(bytes: u64) {
    LEFT.set(bytes);
}

fn witchy_bind_input_file(path: &core::ffi::CStr, identity: Option<&Identity>) -> bool {
    // O_RDONLY | O_CLOEXEC | O_NOFOLLOW_ANY | O_NONBLOCK.
    #[cfg(target_os = "macos")]
    let fd = super::sys::open(path, 0x2100_0004);
    #[cfg(target_os = "linux")]
    let fd = super::linux_stat::open_no_symlinks(path);
    if fd < 0 {
        return false;
    }
    bind_opened(fd, identity, false)
}

/// Consume a launch-resolved descriptor and bind it as stdin. Inherited pipes
/// are allowed only when the trusted resolver opened them as an input source.
pub(super) fn bind_opened(fd: c_int, identity: Option<&Identity>, allow_pipe: bool) -> bool {
    if fd < 0 {
        return false;
    }
    let ok = stat(fd).is_some_and(|info| {
        (super::secret_guard::safe_file(&info)
            || (allow_pipe
                && info.mode & 0o170000 == 0o010000
                && !super::secret_guard::secret(&info)))
            && info.size >= 0
            && info.size as u64 <= LEFT.get()
            && identity.is_none_or(|identity| {
                identity.device == info.device as u64 && identity.inode == info.inode
            })
            && dup2(fd, 0) == 0
    });
    if fd != 0 {
        close(fd);
    }
    ok
}

/// Refuse a secret source or multiply linked regular file on stdin, including
/// inherited stdin that was not bound by pathname.
pub(super) fn safe_stdin() -> bool {
    stat(0).is_some_and(|info| {
        !super::secret_guard::secret(&info) && (info.mode & 0o170000 != 0o100000 || info.links == 1)
    })
}

fn trap() -> ! {
    unsafe { core::arch::asm!("brk #0", options(noreturn, nostack)) }
}

fn next_byte() -> Option<u8> {
    if let Some(mut snapshot) = SNAPSHOT.get() {
        if snapshot.at == snapshot.len {
            return None;
        }
        // SAFETY: `snapshot_input` read `len` bytes into the allocation at
        // `bytes`, which is never freed, and `at` is short of `len`.
        let byte = unsafe { snapshot.bytes.add(snapshot.at).read() };
        snapshot.at += 1;
        SNAPSHOT.set(Some(snapshot));
        return Some(byte);
    }
    let byte = getchar();
    if super::sys::stdin_failed() {
        trap();
    }
    let byte = u8::try_from(byte).ok()?;
    let left = LEFT.get();
    if left != u64::MAX {
        let Some(debited) = left.checked_sub(1) else {
            exit(2)
        };
        LEFT.set(debited);
    }
    Some(byte)
}

#[unsafe(no_mangle)]
extern "C" fn witchy_read(grant: u64, run: u64) -> u64 {
    if grant != 8 {
        trap();
    }
    if run == 0 {
        return next_byte().map_or(256, u64::from);
    }
    let mut word = 0;
    let mut count = 0;
    for position in 0..7 {
        let Some(byte) = next_byte() else { break };
        word |= u64::from(byte) << (8 * position);
        count += 1;
        if byte == b'\n' || byte >= 128 {
            break;
        }
    }
    word | (count << 56)
}

/// Bind the input snapshot before guest entry. The pathname and optional
/// identity are borrowed from the launcher, and `stderr` is its diagnostic
/// stream.
pub(super) fn witchy_launch_input(
    path: Option<&core::ffi::CStr>,
    opened: c_int,
    identity: Option<&super::file_identity::Identity>,
    bytes: u64,
    budget_set: bool,
    stderr: super::sys::Stream,
) -> bool {
    witchy_input_limit(bytes);
    let error = if opened >= 0 {
        if bind_opened(opened, identity, true) {
            return true;
        }
        c"--input=PATH requires a regular file without symlinks matching any identity and byte limit\n"
    } else if path.is_some() && budget_set && bytes > 16 * 1024 * 1024 {
        c"named input files support --input-bytes up to 16 MiB\n"
    } else if let Some(path) = path {
        if witchy_bind_input_file(path, identity) {
            return true;
        }
        c"--input=PATH requires a regular file without symlinks matching any identity and byte limit\n"
    } else {
        if identity.is_none() {
            return true;
        }
        c"--input=PATH requires a regular file without symlinks matching any identity and byte limit\n"
    };
    stderr.put(error);
    false
}
