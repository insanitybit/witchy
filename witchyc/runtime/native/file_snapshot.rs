//! Read-only file grants are snapshotted before entering the sandbox.

use super::global::Global;
#[cfg(target_os = "linux")]
use super::linux_stat::stat;
#[cfg(target_os = "macos")]
use super::seatbelt::stat;
use super::sys::Stream;
use core::{
    ffi::{CStr, c_int, c_void},
    ptr,
};

unsafe extern "C" {
    fn read(fd: c_int, out: *mut c_void, size: usize) -> isize;
    safe fn close(fd: c_int) -> c_int;
    fn malloc(size: usize) -> *mut c_void;
    fn free(data: *mut c_void);
}

const MAX_FILE: usize = 1024 * 1024;
const EINTR: c_int = 4;

#[derive(Clone, Copy)]
struct Snapshot {
    bytes: ptr::NonNull<u8>,
    len: usize,
    cursor: usize,
}

// Startup and guest calls are serialized and cannot suspend while they hold a
// copy of the snapshot's place.
static FILE: Global<Option<Snapshot>> = Global::new(None);

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
        // SAFETY: cursor starts at zero and advances only by a successful
        // read count. libc writes and reports at most capacity - cursor bytes.
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

/// Snapshot the file a pathname names. A launch with no pathname has no file.
fn snapshot_file(path: Option<&CStr>) -> bool {
    if FILE.get().is_some() {
        return false;
    }
    let Some(path) = path else {
        return false;
    };
    // Traverse no final symlink; the descriptor is verified after opening.
    #[cfg(target_os = "macos")]
    const FLAGS: c_int = 0x2100_0004;
    #[cfg(target_os = "macos")]
    let fd = super::sys::open(path, FLAGS);
    #[cfg(target_os = "linux")]
    let fd = super::linux_stat::open_no_symlinks(path);
    if fd < 0 {
        return false;
    }
    snapshot_opened(fd)
}

/// Consume a file descriptor already opened by the trusted launch resolver.
pub(super) fn snapshot_opened(fd: c_int) -> bool {
    if fd < 0 || FILE.get().is_some() {
        if fd >= 0 {
            close(fd);
        }
        return false;
    }
    let previous = super::io_budget::filesystem_total();
    let loaded = load(fd, previous);
    let closed = close(fd) == 0;
    let Some(snapshot) = loaded else { return false };
    // SAFETY: `load` allocated the bytes, and nothing else has the snapshot yet.
    if !closed {
        unsafe { free(snapshot.bytes.as_ptr().cast()) };
        return false;
    }
    if !super::io_budget::charge_snapshot(snapshot.len as u64) {
        unsafe { free(snapshot.bytes.as_ptr().cast()) };
        return false;
    }
    FILE.set(Some(snapshot));
    true
}

#[unsafe(no_mangle)]
extern "C" fn witchy_snapshot_check() {
    if FILE.get().is_none() {
        trap()
    }
}

#[unsafe(no_mangle)]
extern "C" fn witchy_snapshot_byte() -> u64 {
    let Some(mut file) = FILE.get() else { trap() };
    if file.cursor == file.len {
        return 256;
    }
    super::io_budget::snapshot_read_length(1);
    // SAFETY: the snapshot owns `len` read bytes at `bytes` until it is
    // closed, and the cursor is short of `len`.
    let byte = unsafe { file.bytes.as_ptr().add(file.cursor).read() };
    file.cursor += 1;
    FILE.set(Some(file));
    u64::from(byte)
}

/// For a nonzero result, the guest output must be writable for that many bytes
/// and disjoint from the runtime-owned snapshot allocation.
#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_snapshot_read(out: *mut u8, count: usize) -> u64 {
    let Some(mut file) = FILE.get() else { trap() };
    let len = super::io_budget::snapshot_read_length(count.min(file.len - file.cursor));
    if len != 0 {
        // SAFETY: len is bounded by the remaining initialized snapshot bytes.
        // The caller supplies a writable, disjoint output for that count.
        unsafe { ptr::copy_nonoverlapping(file.bytes.as_ptr().add(file.cursor), out, len) };
        file.cursor += len;
        FILE.set(Some(file));
    }
    len as u64
}

#[unsafe(no_mangle)]
extern "C" fn witchy_snapshot_close() {
    let Some(file) = FILE.get() else { trap() };
    FILE.set(None);
    // SAFETY: `load` allocated the bytes, and the snapshot that had them is gone.
    unsafe { free(file.bytes.as_ptr().cast()) };
}

/// Validate path-backed grants in launch diagnostic order and snapshot the
/// direct read grant. The remaining paths are opened by the broker.
/// `stderr` is the launcher's diagnostic stream.
pub(super) fn witchy_bind_path_grants(
    required: u64,
    supplied: u64,
    file_path: Option<&CStr>,
    file_fd: c_int,
    stderr: Stream,
) -> bool {
    const READ_FILE: u64 = 256;
    let error = if required & READ_FILE != 0
        && (supplied & READ_FILE == 0
            || !if file_fd >= 0 {
                snapshot_opened(file_fd)
            } else {
                snapshot_file(file_path)
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
        stderr.put(message);
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
    let fd = super::sys::open(&path, 0);
    assert!(fd >= 0);
    assert!(load(fd, 10).is_none());
    close(fd);
    std::fs::remove_file(root.join("link")).unwrap();
    let fd = super::sys::open(&path, 0);
    let snapshot = load(fd, 10).unwrap();
    assert_eq!(snapshot.len, 2);
    unsafe {
        free(snapshot.bytes.as_ptr().cast());
        close(fd);
    }
    std::fs::remove_dir_all(root).unwrap();
}
