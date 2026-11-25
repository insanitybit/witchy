//! Read-only file grants are snapshotted before entering the sandbox.

#[cfg(target_os = "linux")]
use super::linux_stat::stat;
#[cfg(target_os = "macos")]
use super::seatbelt::stat;
use core::{
    ffi::{c_char, c_int, c_void},
    ptr,
};

unsafe extern "C" {
    #[cfg(any(target_os = "macos", test))]
    fn open(path: *const c_char, flags: c_int, ...) -> c_int;
    fn read(fd: c_int, out: *mut c_void, size: usize) -> isize;
    safe fn close(fd: c_int) -> c_int;
    fn malloc(size: usize) -> *mut c_void;
    fn free(data: *mut c_void);
}

const MAX_FILE: usize = 1024 * 1024;
const EINTR: c_int = 4;

struct Snapshot {
    bytes: ptr::NonNull<u8>,
    len: usize,
    cursor: usize,
}

static mut FILE: Option<Snapshot> = None;

fn trap() -> ! {
    unsafe { core::arch::asm!("brk #0", options(noreturn, nostack)) }
}

fn load(fd: c_int, budget: u64) -> Option<Snapshot> {
    let info = stat(fd)?;
    let len = usize::try_from(info.size).ok()?;
    if !super::secret_guard::safe_file(&info) || len > MAX_FILE || len as u64 > budget {
        return None;
    }
    let capacity = len + 1;
    let bytes = ptr::NonNull::new(unsafe { malloc(capacity) }.cast::<u8>())?;
    let mut cursor = 0;
    while cursor < capacity {
        let count = unsafe { read(fd, bytes.as_ptr().add(cursor).cast(), capacity - cursor) };
        if count == 0 {
            return Some(Snapshot {
                bytes,
                len: cursor,
                cursor: 0,
            });
        }
        if count < 0 {
            if super::errno::get() == EINTR {
                continue;
            }
            break;
        }
        cursor += count as usize;
    }
    unsafe { free(bytes.as_ptr().cast()) };
    None
}

/// The host passes a live C pathname. Startup and
/// guest calls are serialized and cannot suspend while touching FILE.
unsafe fn snapshot_file(path: *const c_char) -> bool {
    if unsafe { (&*(&raw const FILE)).is_some() } {
        return false;
    }
    // Traverse no final symlink; the descriptor is verified after opening.
    #[cfg(target_os = "macos")]
    const FLAGS: c_int = 0x2100_0004;
    #[cfg(target_os = "macos")]
    let fd = unsafe { open(path, FLAGS) };
    #[cfg(target_os = "linux")]
    let fd = unsafe { super::linux_stat::open_no_symlinks(path) };
    if fd < 0 {
        return false;
    }
    unsafe { snapshot_opened(fd) }
}

/// Consume a file descriptor already opened by the trusted launch resolver.
pub(super) unsafe fn snapshot_opened(fd: c_int) -> bool {
    if fd < 0 || unsafe { (&*(&raw const FILE)).is_some() } {
        if fd >= 0 {
            close(fd);
        }
        return false;
    }
    let previous = super::io_budget::filesystem_total();
    let loaded = load(fd, previous);
    let closed = close(fd) == 0;
    let Some(snapshot) = loaded else { return false };
    if !closed {
        unsafe { free(snapshot.bytes.as_ptr().cast()) };
        return false;
    }
    if !super::io_budget::charge_snapshot(snapshot.len as u64) {
        unsafe { free(snapshot.bytes.as_ptr().cast()) };
        return false;
    }
    unsafe { FILE = Some(snapshot) };
    true
}

#[unsafe(no_mangle)]
extern "C" fn witchy_snapshot_check() {
    if unsafe { (&*(&raw const FILE)).is_none() } {
        trap()
    }
}

#[unsafe(no_mangle)]
extern "C" fn witchy_snapshot_byte() -> u64 {
    let Some(file) = (unsafe { (&mut *(&raw mut FILE)).as_mut() }) else {
        trap()
    };
    if file.cursor == file.len {
        return 256;
    }
    super::io_budget::snapshot_read_length(1);
    let byte = unsafe { file.bytes.as_ptr().add(file.cursor).read() };
    file.cursor += 1;
    u64::from(byte)
}

/// The guest output address must permit writing the returned byte count.
#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_snapshot_read(out: *mut u8, count: usize) -> u64 {
    let Some(file) = (unsafe { (&mut *(&raw mut FILE)).as_mut() }) else {
        trap()
    };
    let len = super::io_budget::snapshot_read_length(count.min(file.len - file.cursor));
    if len != 0 {
        unsafe { ptr::copy_nonoverlapping(file.bytes.as_ptr().add(file.cursor), out, len) };
        file.cursor += len;
    }
    len as u64
}

#[unsafe(no_mangle)]
extern "C" fn witchy_snapshot_close() {
    let Some(file) = (unsafe { (&mut *(&raw mut FILE)).take() }) else {
        trap()
    };
    unsafe { free(file.bytes.as_ptr().cast()) };
}

/// Validate path-backed grants in launch diagnostic order and snapshot the
/// direct read grant. The remaining paths are opened by the broker.
#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_bind_path_grants(
    required: u64,
    supplied: u64,
    file_path: *const c_char,
    file_fd: c_int,
    stderr: *mut core::ffi::c_void,
) -> bool {
    unsafe extern "C" {
        fn fputs(text: *const c_char, stream: *mut core::ffi::c_void) -> core::ffi::c_int;
    }
    const READ_FILE: u64 = 256;
    let error = if required & READ_FILE != 0
        && (supplied & READ_FILE == 0
            || !unsafe {
                if file_fd >= 0 {
                    snapshot_opened(file_fd)
                } else {
                    snapshot_file(file_path)
                }
            }) {
        Some(c"File<Read> requires a regular file of at most 1 MiB with one link, reached without symlinks, that is no secret's source\n")
    } else {
        let missing = required & !supplied;
        [
            (512, c"Listener requires --listen=IP:PORT\n"),
            (1024, c"Dir<Read> requires --dir=PATH\n"),
            (2048, c"File<Write> requires --write-file=PATH\n"),
            (4096, c"Dir<Write> requires --write-dir=PATH\n"),
        ]
        .into_iter()
        .find_map(|(grant, message)| (missing & grant != 0).then_some(message))
    };
    if let Some(message) = error {
        unsafe { fputs(message.as_ptr(), stderr) };
        return false;
    }
    true
}

#[cfg(test)]
#[test]
fn opened_snapshot_refuses_hard_links() {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let root = std::env::temp_dir().join(format!("witchy-file-snapshot-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("file"), b"ok").unwrap();
    std::fs::hard_link(root.join("file"), root.join("link")).unwrap();
    let path = CString::new(root.join("file").as_os_str().as_bytes()).unwrap();
    let fd = unsafe { open(path.as_ptr(), 0) };
    assert!(fd >= 0);
    assert!(load(fd, 10).is_none());
    unsafe {
        close(fd);
    }
    std::fs::remove_file(root.join("link")).unwrap();
    let fd = unsafe { open(path.as_ptr(), 0) };
    let snapshot = load(fd, 10).unwrap();
    assert_eq!(snapshot.len, 2);
    unsafe {
        free(snapshot.bytes.as_ptr().cast());
        close(fd);
    }
    std::fs::remove_dir_all(root).unwrap();
}
