//! Private executable copies and bounded descriptor-relative cleanup.
#[cfg(target_os = "linux")]
use super::global::Global;
#[cfg(target_os = "linux")]
use super::linux_stat::{Stat, stat};
#[cfg(target_os = "macos")]
use super::seatbelt::{Stat, stat};
use core::ffi::{CStr, c_char, c_int, c_void};
use core::mem::MaybeUninit;

const PATH_MAX: usize = 1024;
#[cfg(target_os = "macos")]
const DIR: c_int = 0x0010_0000;
#[cfg(target_os = "linux")]
const DIR: c_int = 0o40000;
#[cfg(target_os = "macos")]
const CLOEXEC: c_int = 0x0100_0000;
#[cfg(target_os = "linux")]
const CLOEXEC: c_int = 0o2000000;
#[cfg(target_os = "macos")]
const NOFOLLOW: c_int = 0x100;
#[cfg(target_os = "linux")]
const NOFOLLOW: c_int = 0o100000;
#[cfg(target_os = "macos")]
const AT_SYMLINK_NOFOLLOW: c_int = 0x20;
#[cfg(target_os = "linux")]
const AT_SYMLINK_NOFOLLOW: c_int = 0x100;
#[cfg(target_os = "macos")]
const AT_REMOVEDIR: c_int = 0x80;
#[cfg(target_os = "linux")]
const AT_REMOVEDIR: c_int = 0x200;

#[repr(C)]
#[cfg(target_os = "macos")]
struct Dirent {
    inode: u64,
    seek: u64,
    record_len: u16,
    name_len: u16,
    kind: u8,
    name: [c_char; 1024],
}

#[cfg(target_os = "linux")]
use super::linux_stat::Dirent;

unsafe extern "C" {
    #[cfg(target_os = "macos")]
    fn confstr(name: c_int, buffer: *mut c_char, len: usize) -> usize;
    fn mkdtemp(template: *mut c_char) -> *mut c_char;
    fn open(path: *const c_char, flags: c_int, ...) -> c_int;
    fn openat(parent: c_int, name: *const c_char, flags: c_int, ...) -> c_int;
    fn fstatat(parent: c_int, name: *const c_char, info: *mut Stat, flags: c_int) -> c_int;
    safe fn dup(fd: c_int) -> c_int;
    safe fn close(fd: c_int) -> c_int;
    fn fdopendir(fd: c_int) -> *mut c_void;
    fn readdir(dir: *mut c_void) -> *const Dirent;
    fn closedir(dir: *mut c_void) -> c_int;
    fn unlinkat(fd: c_int, name: *const c_char, flags: c_int) -> c_int;
    fn rmdir(path: *const c_char) -> c_int;
    safe fn getpid() -> c_int;
    safe fn getuid() -> u32;
    safe fn kill(pid: c_int, signal: c_int) -> c_int;
    fn pread(fd: c_int, output: *mut c_void, count: usize, offset: i64) -> isize;
    fn write(fd: c_int, input: *const c_void, count: usize) -> isize;
}

fn remove_tree(fd: c_int, depth: u32) {
    let scan = dup(fd);
    if scan < 0 {
        return;
    }
    let dir = unsafe { fdopendir(scan) };
    if dir.is_null() {
        close(scan);
        return;
    }
    for _ in 0..65536 {
        let entry = unsafe { readdir(dir) };
        if entry.is_null() {
            break;
        }
        let name = unsafe { CStr::from_ptr((&raw const (*entry).name).cast()) };
        if matches!(name.to_bytes(), b"." | b"..") {
            continue;
        }
        if unsafe { unlinkat(fd, name.as_ptr(), 0) } == 0 || depth >= 32 {
            continue;
        }
        let child = unsafe { openat(fd, name.as_ptr(), DIR | CLOEXEC | NOFOLLOW) };
        if child >= 0 {
            remove_tree(child, depth + 1);
            unsafe {
                close(child);
                unlinkat(fd, name.as_ptr(), AT_REMOVEDIR);
            }
        }
    }
    unsafe {
        closedir(dir);
    }
}

fn temporary_base(buffer: &mut [u8; PATH_MAX]) -> Option<usize> {
    #[cfg(target_os = "macos")]
    let length = unsafe { confstr(65537, buffer.as_mut_ptr().cast(), buffer.len()) };
    #[cfg(target_os = "linux")]
    let length = {
        buffer[..5].copy_from_slice(b"/tmp\0");
        5
    };
    if length < 2 || length > buffer.len() || buffer[0] != b'/' || buffer[length - 1] != 0 {
        return None;
    }
    Some(length - 1)
}

#[cfg(target_os = "linux")]
static DIRECTORY_OWNER: Global<c_int> = Global::new(0);

/// Set to the broker's PID before the Linux runner is created, so a killed
/// broker's supervisor can remove the runner's private directories.
#[cfg(target_os = "linux")]
pub(super) fn set_directory_owner(pid: c_int) {
    DIRECTORY_OWNER.set(pid);
}

pub(super) struct Directory {
    path: [u8; PATH_MAX],
    pub(super) fd: c_int,
}

impl Directory {
    pub(super) fn path_ptr(&self) -> *const c_char {
        self.path.as_ptr().cast()
    }
    pub(super) fn create() -> Option<Self> {
        let mut path = [0u8; PATH_MAX];
        let mut at = temporary_base(&mut path)?;
        if path[at - 1] != b'/' {
            path[at] = b'/';
            at += 1;
        }
        #[cfg(target_os = "macos")]
        let pid = getpid();
        #[cfg(target_os = "linux")]
        let pid = if DIRECTORY_OWNER.get() > 1 {
            DIRECTORY_OWNER.get()
        } else {
            getpid()
        };
        if pid <= 1 {
            return None;
        }
        let mut digits = [0u8; 10];
        let mut n = pid as u32;
        let mut start = digits.len();
        while n != 0 {
            start -= 1;
            digits[start] = b'0' + (n % 10) as u8;
            n /= 10;
        }
        for part in [
            b"witchy-exec-".as_slice(),
            &digits[start..],
            b"-XXXXXXXXXXXX",
        ] {
            let end = at.checked_add(part.len())?;
            if end >= path.len() {
                return None;
            }
            path[at..end].copy_from_slice(part);
            at = end;
        }
        path[at] = 0;
        if unsafe { mkdtemp(path.as_mut_ptr().cast()) }.is_null() {
            return None;
        }
        let fd = unsafe { open(path.as_ptr().cast(), DIR | CLOEXEC | NOFOLLOW) };
        if fd < 0 {
            unsafe {
                rmdir(path.as_ptr().cast());
            }
            return None;
        }
        Some(Self { path, fd })
    }

    pub(super) fn program_path(&self, name: &[u8], out: &mut [u8; PATH_MAX]) -> bool {
        let Some(length) = self.path.iter().position(|&b| b == 0) else {
            return false;
        };
        let Some(end) = length
            .checked_add(1)
            .and_then(|n| n.checked_add(name.len()))
        else {
            return false;
        };
        if end >= out.len() || name.contains(&0) {
            return false;
        }
        out[..length].copy_from_slice(&self.path[..length]);
        out[length] = b'/';
        out[length + 1..end].copy_from_slice(name);
        out[end] = 0;
        true
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        remove_tree(self.fd, 0);
        unsafe {
            close(self.fd);
            rmdir(self.path.as_ptr().cast());
        }
    }
}

pub(super) fn copy_program(source: c_int, target: c_int) -> bool {
    let mut buffer = [0u8; 65536];
    let mut offset = 0i64;
    loop {
        let count = unsafe { pread(source, buffer.as_mut_ptr().cast(), buffer.len(), offset) };
        if count < 0 && super::errno::get() == 4 {
            continue;
        }
        if count < 0 || offset + count as i64 > (256 << 20) {
            return false;
        }
        if count == 0 {
            return true;
        }
        let mut at = 0;
        while at < count as usize {
            let written = unsafe {
                write(
                    target,
                    buffer[at..count as usize].as_ptr().cast(),
                    count as usize - at,
                )
            };
            if written < 0 && super::errno::get() == 4 {
                continue;
            }
            if written <= 0 {
                return false;
            }
            at += written as usize;
        }
        offset += count as i64;
    }
}

fn owned_directory_name(name: &[u8]) -> Option<c_int> {
    let tail = name.strip_prefix(b"witchy-exec-")?;
    let split = tail.iter().position(|&b| b == b'-')?;
    let (digits, suffix) = tail.split_at(split);
    let suffix = &suffix[1..];
    if digits.is_empty()
        || digits.len() > 9
        || suffix.len() != 12
        || !digits.iter().all(u8::is_ascii_digit)
        || !suffix.iter().all(u8::is_ascii_alphanumeric)
    {
        return None;
    }
    let mut pid = 0i32;
    for digit in digits {
        pid = pid * 10 + i32::from(*digit - b'0');
    }
    (pid > 1).then_some(pid)
}

/// Sweeps only directories owned by this user that match a dead broker or the
/// supplied broker PID, which the caller must keep unreaped while sweeping.
#[unsafe(no_mangle)]
extern "C" fn witchy_exec_sweep(broker: c_int) {
    let mut path = [0u8; PATH_MAX];
    if temporary_base(&mut path).is_none() {
        return;
    }
    let root = unsafe { open(path.as_ptr().cast(), DIR | CLOEXEC | NOFOLLOW) };
    if root < 0 {
        return;
    }
    let scan = dup(root);
    let dir = if scan >= 0 {
        unsafe { fdopendir(scan) }
    } else {
        ptr::null_mut()
    };
    if dir.is_null() {
        if scan >= 0 {
            close(scan);
        }
        close(root);
        return;
    }
    for _ in 0..65536 {
        let entry = unsafe { readdir(dir) };
        if entry.is_null() {
            break;
        }
        let name = unsafe { CStr::from_ptr((&raw const (*entry).name).cast()) };
        let Some(pid) = owned_directory_name(name.to_bytes()) else {
            continue;
        };
        if if broker > 0 {
            pid != broker
        } else {
            kill(pid, 0) == 0 || super::errno::get() != 3
        } {
            continue;
        }
        let mut named = MaybeUninit::<Stat>::uninit();
        if unsafe { fstatat(root, name.as_ptr(), named.as_mut_ptr(), AT_SYMLINK_NOFOLLOW) } != 0 {
            continue;
        }
        let named = unsafe { named.assume_init() };
        if named.mode & 0o170000 != 0o040000
            || named.uid != getuid()
            || named.mode & 0o7777 != 0o700
        {
            continue;
        }
        let fd = unsafe { openat(root, name.as_ptr(), DIR | CLOEXEC | NOFOLLOW) };
        if fd < 0 {
            continue;
        }
        if stat(fd)
            .is_some_and(|opened| opened.device == named.device && opened.inode == named.inode)
        {
            remove_tree(fd, 0);
            let mut after = MaybeUninit::<Stat>::uninit();
            if unsafe { fstatat(root, name.as_ptr(), after.as_mut_ptr(), AT_SYMLINK_NOFOLLOW) } == 0
            {
                let after = unsafe { after.assume_init() };
                if after.device == named.device && after.inode == named.inode {
                    unsafe {
                        unlinkat(root, name.as_ptr(), AT_REMOVEDIR);
                    }
                }
            }
        }
        close(fd);
    }
    unsafe {
        closedir(dir);
        close(root);
    }
}

use core::ptr;

#[cfg(test)]
#[test]
fn private_directory_is_removed_and_names_are_checked() {
    use std::os::unix::ffi::OsStrExt;
    assert_eq!(
        owned_directory_name(b"witchy-exec-123-abcdefghijkl"),
        Some(123)
    );
    assert_eq!(owned_directory_name(b"witchy-exec-0-abcdefghijkl"), None);
    assert_eq!(owned_directory_name(b"witchy-exec-123-abc/efghijkl"), None);
    let path = {
        let dir = Directory::create().unwrap();
        let mut path = [0; PATH_MAX];
        assert!(dir.program_path(b"helper", &mut path));
        let path = unsafe { CStr::from_ptr(path.as_ptr().cast()) };
        let path = std::path::PathBuf::from(std::ffi::OsStr::from_bytes(path.to_bytes()));
        std::fs::write(&path, b"contents").unwrap();
        path
    };
    assert!(!path.exists());
}
