//! Descriptor-relative traversal for launch sources.
//!
//! Traversal takes ownership of its starting descriptor and returns an owned
//! descriptor, or closes it on failure. Callers supply validated path bytes.
use core::ffi::{c_char, c_int, c_void};
#[cfg(target_os = "macos")]
use core::mem::MaybeUninit;

struct Stat {
    device: u64,
    inode: u64,
    mode: u16,
    links: u64,
}

#[cfg(target_os = "macos")]
fn stat(fd: c_int) -> Option<Stat> {
    super::seatbelt::stat(fd).map(|v| Stat {
        device: v.device as u32 as u64,
        inode: v.inode,
        mode: v.mode,
        links: u64::from(v.links),
    })
}

#[cfg(target_os = "linux")]
fn stat(fd: c_int) -> Option<Stat> {
    super::linux_stat::stat(fd).map(|v| Stat {
        device: v.device,
        inode: v.inode,
        mode: v.mode as u16,
        links: u64::from(v.links),
    })
}

unsafe extern "C" {
    fn openat(fd: c_int, name: *const c_char, flags: c_int, ...) -> c_int;
    #[cfg(target_os = "macos")]
    fn fstatat(
        fd: c_int,
        name: *const c_char,
        info: *mut super::seatbelt::Stat,
        flags: c_int,
    ) -> c_int;
    fn fcntl(fd: c_int, command: c_int, ...) -> c_int;
    safe fn close(fd: c_int) -> c_int;
    fn write(fd: c_int, bytes: *const c_void, len: usize) -> isize;
}

unsafe fn stat_at(fd: c_int, name: *const c_char) -> Option<Stat> {
    #[cfg(target_os = "macos")]
    {
        let mut raw = MaybeUninit::<super::seatbelt::Stat>::uninit();
        if unsafe { fstatat(fd, name, raw.as_mut_ptr(), SYMLINK_NOFOLLOW) } != 0 {
            return None;
        }
        let v = unsafe { raw.assume_init() };
        Some(Stat {
            device: v.device as u32 as u64,
            inode: v.inode,
            mode: v.mode,
            links: u64::from(v.links),
        })
    }
    #[cfg(target_os = "linux")]
    {
        unsafe { super::linux_stat::stat_at(fd, name) }.map(|v| Stat {
            device: v.device,
            inode: v.inode,
            mode: v.mode as u16,
            links: u64::from(v.links),
        })
    }
}

pub(super) fn refused(message: &[u8]) -> c_int {
    let prefix = b"launch refused: ";
    unsafe {
        write(2, prefix.as_ptr().cast(), prefix.len());
        write(2, message.as_ptr().cast(), message.len());
        write(2, b"\n".as_ptr().cast(), 1);
    }
    -1
}

fn opening_error(error: c_int) -> c_int {
    refused(match error {
        40 | 62 => b"source goes through a symbolic link",
        2 => b"source does not exist",
        20 => b"source is not a directory",
        1 | 13 => b"source is not accessible",
        18 => b"source crosses onto another device below its boundary",
        _ => b"source cannot be opened",
    })
}

#[cfg(target_os = "macos")]
const DIR: c_int = 0x0010_0000;
#[cfg(target_os = "linux")]
const DIR: c_int = 0o40000;
#[cfg(target_os = "macos")]
const CLOEXEC: c_int = 0x0100_0000;
#[cfg(target_os = "linux")]
const CLOEXEC: c_int = 0o2000000;
#[cfg(target_os = "macos")]
const NOFOLLOW_ANY: c_int = 0x2000_0000;
#[cfg(target_os = "linux")]
const NOFOLLOW_ANY: c_int = 0o100000;
#[cfg(target_os = "macos")]
const NONBLOCK: c_int = 0x4;
#[cfg(target_os = "linux")]
const NONBLOCK: c_int = 0o4000;
#[cfg(target_os = "macos")]
const NOCTTY: c_int = 0x0002_0000;
#[cfg(target_os = "linux")]
const NOCTTY: c_int = 0o400;
#[cfg(target_os = "macos")]
const SEARCH: c_int = 0x4000_0000 | DIR;
#[cfg(target_os = "linux")]
const SEARCH: c_int = 0o10000000 | DIR;
#[cfg(target_os = "macos")]
const DUP_CLOEXEC: c_int = 67;
#[cfg(target_os = "linux")]
const DUP_CLOEXEC: c_int = 1030;
#[cfg(target_os = "macos")]
const SYMLINK_NOFOLLOW: c_int = 0x20;
const KIND: u16 = 0o170000;
const REG: u16 = 0o100000;
const DIRECTORY: u16 = 0o040000;

#[derive(Clone, Copy)]
pub(super) struct Boundary {
    pub device: u64,
    pub inode: u64,
}

fn matches(info: &Stat, boundary: Boundary) -> bool {
    info.device == boundary.device && info.inode == boundary.inode
}

/// 1 inside, 0 outside, -1 on another device below the boundary, -2 unknown.
fn below(fd: c_int, boundary: Boundary) -> c_int {
    let mut current = unsafe { fcntl(fd, DUP_CLOEXEC, 3 as c_int) };
    let mut other_device = false;
    let mut result = -2;
    for _ in 0..4096 {
        if current < 0 {
            break;
        }
        let Some(info) = stat(current) else {
            break;
        };
        if matches(&info, boundary) {
            result = if other_device { -1 } else { 1 };
            break;
        }
        other_device |= info.device != boundary.device;
        let mut parent = unsafe { openat(current, c"..".as_ptr(), DIR | CLOEXEC | NOFOLLOW_ANY) };
        if parent < 0 && super::errno::get() == 13 {
            parent = unsafe { openat(current, c"..".as_ptr(), SEARCH | CLOEXEC | NOFOLLOW_ANY) };
        }
        let Some(up) = (if parent < 0 { None } else { stat(parent) }) else {
            if parent >= 0 {
                close(parent);
            }
            break;
        };
        if up.device == info.device && up.inode == info.inode {
            close(parent);
            result = 0;
            break;
        }
        close(current);
        current = parent;
    }
    if current >= 0 {
        close(current);
    }
    result
}

#[unsafe(no_mangle)]
extern "C" fn witchy_launch_within(fd: c_int, boundary_fd: c_int) -> c_int {
    let Some(info) = stat(boundary_fd) else {
        return -1;
    };
    match below(
        fd,
        Boundary {
            device: info.device,
            inode: info.inode,
        },
    ) {
        -2 => -1,
        0 => 0,
        _ => 1,
    }
}

fn private(fd: c_int) -> c_int {
    if fd < 0 {
        return -1;
    }
    if fd > 2 {
        if unsafe { fcntl(fd, 2, 1 as c_int) } == 0 {
            return fd;
        }
        close(fd);
        return -1;
    }
    let moved = unsafe { fcntl(fd, DUP_CLOEXEC, 3 as c_int) };
    close(fd);
    moved
}

/// Walk one component at a time, rejecting links and enforcing the device and
/// ancestry boundary. `base` is consumed on every return path.
pub(super) fn walk(
    base: c_int,
    path: &[u8],
    directory: bool,
    boundary: Option<Boundary>,
    operator_path: bool,
) -> c_int {
    if base < 0 {
        return refused(b"the base directory cannot be opened");
    }
    if path.len() > 4096 || path.contains(&0) {
        close(base);
        return refused(b"source is longer than 4096 bytes");
    }
    let mut inside = boundary.map_or(1, |b| below(base, b));
    if inside == -2 {
        close(base);
        return refused(b"source cannot be placed relative to its boundary");
    }
    let mut current = base;
    let mut components = path
        .split(|&byte| byte == b'/')
        .filter(|s| !s.is_empty() && *s != b".")
        .peekable();
    while let Some(component) = components.next() {
        let parent = component == b"..";
        if component.len() > 255 || (parent && (boundary.is_some() || !operator_path)) {
            close(current);
            return refused(if parent {
                b"source has a `..` component"
            } else {
                b"source component is too long"
            });
        }
        let is_dir = components.peek().is_some() || directory || parent;
        let mut name = [0u8; 256];
        name[..component.len()].copy_from_slice(component);
        if !is_dir {
            let Some(info) = (unsafe { stat_at(current, name.as_ptr().cast()) }) else {
                let error = super::errno::get();
                close(current);
                return opening_error(error);
            };
            if info.mode & KIND != REG {
                close(current);
                return refused(if info.mode & KIND == 0o120000 {
                    b"source goes through a symbolic link"
                } else {
                    b"source is not a regular file"
                });
            }
        }
        let fd = private(unsafe {
            openat(
                current,
                name.as_ptr().cast(),
                CLOEXEC | NOFOLLOW_ANY | NONBLOCK | NOCTTY | if is_dir { DIR } else { 0 },
            )
        });
        let error = if fd < 0 { super::errno::get() } else { 0 };
        close(current);
        if fd < 0 {
            return opening_error(error);
        }
        current = fd;
        if let Some(boundary) = boundary {
            let Some(info) = stat(current) else {
                close(current);
                return refused(b"source cannot be examined");
            };
            if inside == 1 && info.device != boundary.device {
                close(current);
                return refused(b"source is on another device below its boundary");
            }
            if inside != 1 && matches(&info, boundary) {
                inside = if inside == 0 { 1 } else { -1 };
            }
        }
    }
    let Some(info) = stat(current) else {
        close(current);
        return refused(b"source cannot be examined");
    };
    let error = if info.mode & KIND != if directory { DIRECTORY } else { REG } {
        Some(if directory {
            b"source is not a directory".as_slice()
        } else {
            b"source is not a regular file".as_slice()
        })
    } else if inside == -1 {
        Some(b"source is on another device below its boundary".as_slice())
    } else if inside != 1 {
        Some(b"source is outside the directory it is restricted to".as_slice())
    } else if boundary.is_some() && !directory && info.links != 1 {
        Some(b"source has more than one link".as_slice())
    } else {
        None
    };
    if let Some(error) = error {
        close(current);
        return refused(error);
    }
    if !directory {
        let flags = unsafe { fcntl(current, 3) };
        if flags < 0 || unsafe { fcntl(current, 4, flags & !NONBLOCK) } < 0 {
            close(current);
            return -1;
        }
    }
    current
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::symlink;

    unsafe extern "C" {
        fn open(path: *const c_char, flags: c_int, ...) -> c_int;
    }

    #[test]
    fn boundary_rejects_symlink_parent_and_hard_link() {
        let root = std::env::temp_dir().join(format!("witchy-launch-walk-{}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("file"), b"content").unwrap();
        symlink("file", root.join("link")).unwrap();
        std::fs::hard_link(root.join("file"), root.join("hard")).unwrap();
        let root_c = CString::new(root.as_os_str().as_bytes()).unwrap();
        let base = || unsafe { open(root_c.as_ptr(), DIR | CLOEXEC) };
        let boundary_fd = base();
        assert!(boundary_fd >= 0);
        let info = stat(boundary_fd).unwrap();
        let boundary = Some(Boundary {
            device: info.device,
            inode: info.inode,
        });
        assert_eq!(witchy_launch_within(boundary_fd, boundary_fd), 1);
        assert_eq!(walk(base(), b"link", false, boundary, false), -1);
        assert_eq!(walk(base(), b"../file", false, boundary, true), -1);
        assert_eq!(walk(base(), b"file", false, boundary, false), -1);
        std::fs::remove_file(root.join("hard")).unwrap();
        let file = walk(base(), b"file", false, boundary, false);
        assert!(file >= 0);
        unsafe {
            close(file);
            close(boundary_fd);
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
