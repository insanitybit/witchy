//! Startup checks for the executable directory and writable grants.
use core::ffi::{CStr, c_char, c_int};

#[cfg(target_os = "macos")]
const DIRECTORY_FLAGS: c_int = 0x2110_0000;
#[cfg(target_os = "linux")]
const DIRECTORY_FLAGS: c_int = 0o40000 | 0o2000000 | 0o100000;
#[cfg(target_os = "macos")]
const DUP_CLOEXEC: c_int = 67;
#[cfg(target_os = "linux")]
const DUP_CLOEXEC: c_int = 1030;

unsafe extern "C" {
    #[cfg(target_os = "macos")]
    fn open(path: *const c_char, flags: c_int, ...) -> c_int;
    safe fn close(fd: c_int) -> c_int;
    fn fcntl(fd: c_int, command: c_int, ...) -> c_int;
    safe fn witchy_launch_within(fd: c_int, boundary: c_int) -> c_int;
}

/// Open the launch-authorized executable directory without following links.
pub(super) unsafe fn open_directory(path: *const c_char) -> c_int {
    if path.is_null() {
        return -1;
    }
    let fd = unsafe { open_path(path) };
    if !(0..=2).contains(&fd) {
        return fd;
    }
    let moved = unsafe { fcntl(fd, DUP_CLOEXEC, 3 as c_int) };
    close(fd);
    moved
}

unsafe fn open_path(path: *const c_char) -> c_int {
    #[cfg(target_os = "macos")]
    {
        unsafe { open(path, DIRECTORY_FLAGS) }
    }
    #[cfg(target_os = "linux")]
    {
        unsafe { super::linux_stat::openat_no_symlinks(-100, path, DIRECTORY_FLAGS) }
    }
}

fn separated(exec: c_int, other: c_int) -> bool {
    other >= 0
        && {
            witchy_launch_within(other, exec) == 0 && witchy_launch_within(exec, other) == 0
        }
}

/// Refuse equal or nested directory grants in either direction. An ancestry
/// check that cannot complete is a refusal, never interpreted as separation.
pub(super) unsafe fn separated_from_writes(
    exec: c_int,
    writable: *const c_char,
    writable_fd: c_int,
    output: *const c_char,
    output_fd: c_int,
) -> bool {
    if exec < 0 {
        return false;
    }
    if !writable.is_null() || writable_fd >= 0 {
        let fd = if writable_fd >= 0 {
            unsafe { fcntl(writable_fd, DUP_CLOEXEC, 3 as c_int) }
        } else {
            unsafe { open_path(writable) }
        };
        let ok = separated(exec, fd);
        if fd >= 0 {
            close(fd);
        }
        if !ok {
            return false;
        }
    }
    if output_fd >= 0 {
        if !separated(exec, output_fd) {
            return false;
        }
    } else if !output.is_null() {
        let path = unsafe { CStr::from_ptr(output) }.to_bytes();
        let slash = path.iter().rposition(|&b| b == b'/');
        let part = match slash {
            Some(0) => b"/".as_slice(),
            Some(n) => &path[..n],
            None => b".".as_slice(),
        };
        let mut parent = [0u8; 1024];
        if part.len() >= parent.len() {
            return false;
        }
        parent[..part.len()].copy_from_slice(part);
        let fd = unsafe { open_path(parent.as_ptr().cast()) };
        let ok = separated(exec, fd);
        if fd >= 0 {
            close(fd);
        }
        if !ok {
            return false;
        }
    }
    true
}

#[cfg(test)]
#[test]
fn refuses_overlapping_exec_and_write_roots() {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let root = std::env::temp_dir().join(format!("witchy-exec-overlap-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let root = std::fs::canonicalize(root).unwrap();
    std::fs::create_dir(root.join("bin")).unwrap();
    std::fs::create_dir(root.join("other")).unwrap();
    let c = |path: &std::path::Path| CString::new(path.as_os_str().as_bytes()).unwrap();
    let exec = unsafe { open_directory(c(&root.join("bin")).as_ptr()) };
    assert!(exec >= 0);
    assert!(!unsafe { separated_from_writes(exec, c(&root).as_ptr(), -1, core::ptr::null(), -1) });
    assert!(!unsafe {
        separated_from_writes(
            exec,
            core::ptr::null(),
            -1,
            c(&root.join("bin/file")).as_ptr(),
            -1,
        )
    });
    assert!(unsafe {
        separated_from_writes(
            exec,
            c(&root.join("other")).as_ptr(),
            -1,
            core::ptr::null(),
            -1,
        )
    });
    unsafe {
        close(exec);
    }
    std::fs::remove_dir_all(root).unwrap();
}
