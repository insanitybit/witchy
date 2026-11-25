//! Descriptor-relative traversal for launch sources.
//!
//! Traversal takes ownership of its starting descriptor and returns an owned
//! descriptor, or closes it on failure. Callers supply validated path bytes.
use super::sys;
use core::ffi::{CStr, c_int};

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
    safe fn close(fd: c_int) -> c_int;
}

/// The object `name` names under the directory `fd`, a final link not followed.
fn stat_at(fd: c_int, name: &CStr) -> Option<Stat> {
    #[cfg(target_os = "macos")]
    {
        super::seatbelt::stat_at(fd, name, SYMLINK_NOFOLLOW).map(|v| Stat {
            device: v.device as u32 as u64,
            inode: v.inode,
            mode: v.mode,
            links: u64::from(v.links),
        })
    }
    #[cfg(target_os = "linux")]
    {
        super::linux_stat::stat_at(fd, name).map(|v| Stat {
            device: v.device,
            inode: v.inode,
            mode: v.mode as u16,
            links: u64::from(v.links),
        })
    }
}

pub(super) fn refused(message: &[u8]) -> c_int {
    sys::write(2, b"launch refused: ");
    sys::write(2, message);
    sys::write(2, b"\n");
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
    let mut current = sys::duplicate_close_on_exec(fd, 3);
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
        let mut parent = sys::openat(current, c"..", DIR | CLOEXEC | NOFOLLOW_ANY);
        if parent < 0 && super::errno::get() == 13 {
            parent = sys::openat(current, c"..", SEARCH | CLOEXEC | NOFOLLOW_ANY);
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
        if sys::set_close_on_exec(fd) == 0 {
            return fd;
        }
        close(fd);
        return -1;
    }
    let moved = sys::duplicate_close_on_exec(fd, 3);
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
        // The component is at most 255 bytes, none of them a NUL, so the name
        // is the component and the NUL after it.
        let Ok(name) = CStr::from_bytes_until_nul(&name) else {
            close(current);
            return -1;
        };
        if !is_dir {
            let Some(info) = stat_at(current, name) else {
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
        let fd = private(sys::openat(
            current,
            name,
            CLOEXEC | NOFOLLOW_ANY | NONBLOCK | NOCTTY | if is_dir { DIR } else { 0 },
        ));
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
        let flags = sys::status_flags(current);
        if flags < 0 || sys::set_status_flags(current, flags & !NONBLOCK) < 0 {
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

    #[test]
    fn boundary_rejects_symlink_parent_and_hard_link() {
        let root = std::env::temp_dir().join(format!("witchy-launch-walk-{}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("file"), b"content").unwrap();
        symlink("file", root.join("link")).unwrap();
        std::fs::hard_link(root.join("file"), root.join("hard")).unwrap();
        let root_c = CString::new(root.as_os_str().as_bytes()).unwrap();
        let base = || sys::open(&root_c, DIR | CLOEXEC);
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
        close(file);
        close(boundary_fd);
        std::fs::remove_dir_all(root).unwrap();
    }
}
