//! Startup checks for the executable directory and writable grants.
use super::sys;
use core::ffi::{CStr, c_int};

#[cfg(target_os = "macos")]
const DIRECTORY_FLAGS: c_int = 0x2110_0000;
#[cfg(target_os = "linux")]
const DIRECTORY_FLAGS: c_int = 0o40000 | 0o2000000 | 0o100000;

unsafe extern "C" {
    safe fn close(fd: c_int) -> c_int;
    safe fn witchy_launch_within(fd: c_int, boundary: c_int) -> c_int;
}

/// Open the launch-authorized executable directory without following links.
/// With no path there is nothing to open, and the answer is -1.
pub(super) fn open_directory(path: Option<&CStr>) -> c_int {
    let Some(path) = path else {
        return -1;
    };
    let fd = open_path(path);
    if !(0..=2).contains(&fd) {
        return fd;
    }
    let moved = sys::duplicate_close_on_exec(fd, 3);
    close(fd);
    moved
}

fn open_path(path: &CStr) -> c_int {
    #[cfg(target_os = "macos")]
    {
        sys::open(path, DIRECTORY_FLAGS)
    }
    #[cfg(target_os = "linux")]
    {
        // SAFETY: `path` is a C string, and the call keeps nothing of it.
        unsafe { super::linux_stat::openat_no_symlinks(-100, path.as_ptr(), DIRECTORY_FLAGS) }
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
pub(super) fn separated_from_writes(
    exec: c_int,
    writable: Option<&CStr>,
    writable_fd: c_int,
    output: Option<&CStr>,
    output_fd: c_int,
) -> bool {
    if exec < 0 {
        return false;
    }
    // The descriptor is used when there is one, and the path only when there is not.
    let held = if writable_fd >= 0 {
        Some(sys::duplicate_close_on_exec(writable_fd, 3))
    } else {
        writable.map(open_path)
    };
    if let Some(fd) = held {
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
    } else if let Some(output) = output {
        let path = output.to_bytes();
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
        // `part` is shorter than `parent`, which began as zeros, and holds no zero.
        let Ok(parent) = CStr::from_bytes_until_nul(&parent) else {
            return false;
        };
        let fd = open_path(parent);
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
    let exec = open_directory(Some(&c(&root.join("bin"))));
    assert!(exec >= 0);
    assert!(!separated_from_writes(exec, Some(&c(&root)), -1, None, -1));
    assert!(!separated_from_writes(
        exec,
        None,
        -1,
        Some(&c(&root.join("bin/file"))),
        -1,
    ));
    assert!(separated_from_writes(
        exec,
        Some(&c(&root.join("other"))),
        -1,
        None,
        -1,
    ));
    close(exec);
    std::fs::remove_dir_all(root).unwrap();
}
