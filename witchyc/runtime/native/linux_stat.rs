//! Linux AArch64 descriptor metadata. The libc `stat` layout is target ABI,
//! never a guest-controlled value; callers use only initialized fields.
use core::ffi::{CStr, c_char, c_int};

#[repr(C)]
pub(super) struct Dirent {
    inode: u64,
    offset: i64,
    record_len: u16,
    pub(super) kind: u8,
    pub(super) name: [c_char; 256],
}

#[repr(C)]
pub(super) struct Stat {
    pub(super) device: u64,
    pub(super) inode: u64,
    pub(super) mode: u32,
    pub(super) links: u32,
    pub(super) uid: u32,
    gid: u32,
    rdev: u64,
    pad1: u64,
    pub(super) size: i64,
    block_size: i32,
    pad2: i32,
    blocks: i64,
    times: [[i64; 2]; 3],
    reserved: [u32; 2],
}

impl Stat {
    /// What a call fills in. Every field is an integer, so zero is a value of each.
    const ZERO: Self = Self {
        device: 0,
        inode: 0,
        mode: 0,
        links: 0,
        uid: 0,
        gid: 0,
        rdev: 0,
        pad1: 0,
        size: 0,
        block_size: 0,
        pad2: 0,
        blocks: 0,
        times: [[0; 2]; 3],
        reserved: [0; 2],
    };
}

unsafe extern "C" {
    fn fstat(fd: c_int, info: *mut Stat) -> c_int;
    fn fstatat(fd: c_int, name: *const c_char, info: *mut Stat, flags: c_int) -> c_int;
    fn syscall(number: core::ffi::c_long, ...) -> core::ffi::c_long;
}

/// Open without following a symlink in any component. On kernels without
/// openat2, fail closed instead of falling back to final-component checks.
pub(super) fn open_no_symlinks(path: &CStr) -> c_int {
    openat_no_symlinks(-100, path, 0o2000000 | 0o100000 | 0o4000)
}

/// Opens `path` under the directory `base`, or as `open` does when `base` is
/// -100, following no symlink.
pub(super) fn openat_no_symlinks(base: c_int, path: &CStr, flags: c_int) -> c_int {
    #[repr(C)]
    struct How {
        flags: u64,
        mode: u64,
        resolve: u64,
    }
    let how = How {
        flags: flags as u64,
        mode: 0,
        // A descriptor-relative path must stay beneath that descriptor and
        // may not cross a mount, including a bind mount on the same device.
        resolve: 0x04 | if base == -100 { 0 } else { 0x08 | 0x01 },
    };
    // SAFETY: `openat2` reads `path`, a C string, and `how` for the size
    // given, and keeps neither.
    unsafe { syscall(437, base, path.as_ptr(), &raw const how, size_of::<How>()) as c_int }
}

pub(super) fn stat(fd: c_int) -> Option<Stat> {
    let mut info = Stat::ZERO;
    // SAFETY: the call writes one `Stat`, and `info` is one.
    if unsafe { fstat(fd, &mut info) } != 0 {
        return None;
    }
    Some(info)
}

/// The object `name` names under the directory `fd`, a final link not followed.
pub(super) fn stat_at(fd: c_int, name: &CStr) -> Option<Stat> {
    let mut info = Stat::ZERO;
    // SAFETY: `name` is a C string, and the call writes one `Stat`.
    if unsafe { fstatat(fd, name.as_ptr(), &mut info, 0x100) } != 0 {
        return None;
    }
    Some(info)
}

#[cfg(test)]
#[test]
fn opening_rejects_symlinks_in_every_path_component() {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::symlink;
    unsafe extern "C" {
        safe fn close(fd: c_int) -> c_int;
    }
    let root = std::env::temp_dir().join(format!("witchy-linux-open-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(root.join("real")).unwrap();
    std::fs::write(root.join("real/file"), b"ok").unwrap();
    symlink("real", root.join("parent-link")).unwrap();
    symlink("file", root.join("real/file-link")).unwrap();
    let open = |path: &std::path::Path| {
        let name = CString::new(path.as_os_str().as_bytes()).unwrap();
        open_no_symlinks(&name)
    };
    assert_eq!(open(&root.join("parent-link/file")), -1);
    assert_eq!(open(&root.join("real/file-link")), -1);
    let fd = open(&root.join("real/file"));
    assert!(fd >= 0);
    close(fd);
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(test)]
#[test]
fn rooted_open_refuses_parent_escape_and_mount_crossing() {
    use std::os::unix::fs::MetadataExt;
    unsafe extern "C" { fn close(fd: c_int) -> c_int; }
    let root = openat_no_symlinks(-100, c"/", 0o2000000 | 0o40000);
    assert!(root >= 0);
    assert_eq!(openat_no_symlinks(root, c"../etc", 0o2000000), -1);
    // /proc is normally a separate mount in Linux containers. If this host has
    // no such mount, retain the escape check rather than assuming one exists.
    if let (Ok(root_info), Ok(proc_info)) = (std::fs::metadata("/"), std::fs::metadata("/proc")) {
        if root_info.dev() != proc_info.dev() {
            assert_eq!(openat_no_symlinks(root, c"proc/version", 0o2000000), -1);
        }
    }
    unsafe { close(root) };
}
