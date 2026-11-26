//! Descriptor-relative directory traversal.

#[cfg(target_os = "linux")]
use super::linux_stat::{Stat, stat};
#[cfg(target_os = "macos")]
use super::seatbelt::{Stat, stat};
use super::global::Table;
use core::{
    ffi::{CStr, c_char, c_int, c_void},
    mem::MaybeUninit,
};

unsafe extern "C" {
    safe fn close(fd: c_int) -> c_int;
    safe fn witchy_exec_guarded() -> bool;
    safe fn witchy_exec_program(device: u64, inode: u64) -> bool;
    safe fn _exit(code: c_int) -> !;
}

#[cfg(target_os = "macos")]
const CLOEXEC: c_int = 0x0100_0000;
#[cfg(target_os = "linux")]
const CLOEXEC: c_int = 0o2000000;
#[cfg(target_os = "macos")]
const NOFOLLOW_ANY: c_int = 0x2000_0000;
#[cfg(target_os = "linux")]
const NOFOLLOW_ANY: c_int = 0o100000;
#[cfg(target_os = "macos")]
const NONBLOCK: c_int = 4;
#[cfg(target_os = "linux")]
const NONBLOCK: c_int = 0o4000;
const NO_LINK_READ: c_int = CLOEXEC | NOFOLLOW_ANY | NONBLOCK;
#[cfg(target_os = "macos")]
const DIRECTORY: c_int = 0x0010_0000;
#[cfg(target_os = "linux")]
const DIRECTORY: c_int = 0o40000;
#[cfg(target_os = "macos")]
type Device = i32;
#[cfg(target_os = "linux")]
type Device = u64;

fn open_checked(parent: c_int, name: &CStr, flags: c_int) -> c_int {
    #[cfg(target_os = "macos")]
    { super::sys::openat(parent, name, flags) }
    #[cfg(target_os = "linux")]
    // SAFETY: `name` is a C string, and the call keeps nothing of it.
    { unsafe { super::linux_stat::openat_no_symlinks(parent, name.as_ptr(), flags) } }
}

/// Opens a path that is not relative to a descriptor the broker holds.
fn open_absolute(path: &CStr, flags: c_int) -> c_int {
    #[cfg(target_os = "macos")]
    { super::sys::open(path, flags) }
    #[cfg(target_os = "linux")]
    { open_checked(-100, path, flags) }
}

/// The text before the first NUL in `bytes`. None when there is no NUL.
fn text(bytes: &[u8]) -> Option<&CStr> {
    CStr::from_bytes_until_nul(bytes).ok()
}
const EXDEV: c_int = 18;
const EINVAL: c_int = 22;

static SECRET_OBJECTS: Table<(u64, u64), 192> = Table::new((0, 0));

/// Startup supplies the secret identities before the broker forks. At most
/// 192 are kept, each device with the inode in the same place.
#[cfg(any(witchy_native_aws_lc, test))]
fn witchy_broker_secret_objects(devices: &[u64], inodes: &[u64]) {
    SECRET_OBJECTS.clear();
    for (&device, &inode) in devices.iter().zip(inodes).take(192) {
        SECRET_OBJECTS.push((device, inode));
    }
}

fn secret_identity(info: &Stat) -> bool {
    SECRET_OBJECTS
        .iter()
        .any(|(device, inode)| device == info.device as u64 && inode == info.inode)
}

fn protected_identity(info: &Stat) -> bool {
    secret_identity(info) || witchy_exec_program(info.device as u64, info.inode)
}

/// The broker passes a validated relative path with its NUL; the path ends at
/// the first NUL. This traversal consumes separators in that buffer.
fn witchy_broker_open_data_path(root: c_int, path: &mut [u8], directory: bool) -> c_int {
    let Some(base) = stat(root) else {
        return -1;
    };
    let mut parent = root;
    let Some(end) = path.iter().position(|&byte| byte == 0) else {
        super::errno::set(EINVAL);
        return -1;
    };
    // The components still to open, and the NUL that ends the last of them.
    let mut rest = &mut path[..=end];
    loop {
        let slash = rest.iter().position(|&byte| byte == b'/');
        if let Some(offset) = slash {
            rest[offset] = 0;
        }
        let flags = NO_LINK_READ
            | if slash.is_some() || directory {
                DIRECTORY
            } else {
                0
            };
        let fd = match text(rest) {
            Some(name) => open_checked(parent, name, flags),
            None => {
                super::errno::set(EINVAL);
                -1
            }
        };
        let mut error = if fd < 0 { super::errno::get() } else { 0 };
        if parent != root {
            close(parent);
        }
        if fd >= 0 {
            error = match stat(fd) {
                None => super::errno::get(),
                Some(info) if info.device != base.device => EXDEV,
                Some(_) => 0,
            };
            if error != 0 {
                close(fd);
            }
        }
        if error != 0 {
            super::errno::set(error);
            return -1;
        }
        let Some(offset) = slash else { return fd };
        parent = fd;
        // The separator is before the NUL that ends `rest`, so the next
        // component and that NUL remain.
        rest = &mut core::mem::take(&mut rest)[offset + 1..];
    }
}

/// Open a validated relative path, passed with its NUL, and classify the result.
/// The path is writable because the data-only traversal consumes separators.
fn witchy_broker_open_rooted(
    root: c_int,
    path: &mut [u8],
    kind: &mut u32,
    error: &mut u8,
    data_only: bool,
) -> c_int {
    let fd = if data_only {
        witchy_broker_open_data_path(root, path, false)
    } else if let Some(path) = text(path) {
        open_checked(root, path, NO_LINK_READ)
    } else {
        super::errno::set(EINVAL);
        -1
    };
    if fd < 0 {
        let code = match super::errno::get() {
            2 => 1,               // ENOENT
            40 | 62 | EXDEV => 2, // ELOOP
            20 => 4,              // ENOTDIR
            _ => return -1,
        };
        *error = code;
        return -1;
    }
    if let Some(info) = stat(fd) {
        if secret_identity(&info) {
            _exit(3)
        }
        let file_kind = match info.mode & 0o170000 {
            0o100000 => 1,
            0o040000 => 2,
            _ => 0,
        };
        if file_kind == 1 && info.links != 1 {
            *error = 2;
            close(fd);
            return -1;
        }
        *kind = file_kind;
        if file_kind != 0 {
            return fd;
        }
        *error = 4;
    }
    close(fd);
    -1
}

/// Opens the directory that holds the last component of `path` and stores
/// that component in `name`, with its NUL.
fn witchy_broker_open_output_parent(
    base: c_int,
    path: &CStr,
    name: &mut [u8; 256],
    data_only: bool,
) -> c_int {
    let path = path.to_bytes();
    let slash = path.iter().rposition(|&byte| byte == b'/');
    let Some(leaf) = path.get(slash.map_or(0, |offset| offset + 1)..) else {
        return -1;
    };
    if leaf.is_empty() || leaf.len() > 255 || leaf == b"." || leaf == b".." {
        return -1;
    }
    // The leaf is at most 255 bytes, so it and its NUL fit.
    name[..leaf.len()].copy_from_slice(leaf);
    name[leaf.len()] = 0;
    let Some(parent_len) = slash else {
        return super::sys::openat(base, c".", CLOEXEC | DIRECTORY);
    };
    if parent_len == 0 {
        return super::sys::open(c"/", CLOEXEC | DIRECTORY);
    }
    let mut parent = [0u8; 1024];
    if parent_len >= parent.len() {
        return -1;
    }
    let Some(prefix) = path.get(..parent_len) else {
        return -1;
    };
    for (out, &byte) in parent.iter_mut().zip(prefix) {
        *out = byte;
    }
    // The prefix is shorter than `parent`, so a NUL follows it.
    if data_only {
        witchy_broker_open_data_path(base, &mut parent, true)
    } else if let Some(parent) = text(&parent) {
        open_checked(base, parent, CLOEXEC | NOFOLLOW_ANY | DIRECTORY)
    } else {
        super::errno::set(EINVAL);
        -1
    }
}

fn staged_name(text: &[u8]) -> bool {
    let Some(split) = text.iter().position(|&byte| byte == b'-') else {
        return false;
    };
    let Some((pid, rest)) = text.split_at_checked(split) else {
        return false;
    };
    let Some(serial) = rest.get(1..) else {
        return false;
    };
    [pid, serial]
        .into_iter()
        .all(|part| (1..=20).contains(&part.len()) && part.iter().all(u8::is_ascii_digit))
}

#[cfg(kani)]
mod proofs {
    use super::*;

    /// A staged name is digits, one hyphen, digits, with at most twenty on
    /// each side, for every name of up to 43 bytes.
    #[kani::proof]
    #[kani::unwind(45)]
    fn a_staged_name_is_two_numbers() {
        let bytes: [u8; 43] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= bytes.len());
        let text = &bytes[..length];
        let staged = staged_name(text);
        kani::cover!(staged && length == 41);
        if staged {
            assert!((3..=41).contains(&length));
            let Some(split) = text.iter().position(|&byte| byte == b'-') else {
                panic!("accepted without its separator");
            };
            assert!((1..=20).contains(&split) && (1..=20).contains(&(length - split - 1)));
            let at: usize = kani::any();
            kani::assume(at < length && at != split);
            assert!(text[at].is_ascii_digit());
        }
    }
}

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
    fn fdopendir(fd: c_int) -> *mut c_void;
    fn readdir(dir: *mut c_void) -> *const Dirent;
    fn closedir(dir: *mut c_void) -> c_int;
    fn fstatat(fd: c_int, name: *const c_char, info: *mut Stat, flags: c_int) -> c_int;
}
#[cfg(target_os = "macos")]
const AT_SYMLINK_NOFOLLOW: c_int = 0x20;
#[cfg(target_os = "linux")]
const AT_SYMLINK_NOFOLLOW: c_int = 0x100;

/// An open directory stream. Dropping it closes it and its descriptor.
struct Entries(*mut c_void);

/// One record of a stream. The stream cannot be read again while it is held.
struct Entry<'a> {
    name: &'a CStr,
    kind: u8,
}

impl Entries {
    /// Takes over `fd`. On failure the descriptor is still the caller's.
    fn open(fd: c_int) -> Option<Self> {
        // SAFETY: the call takes any descriptor value and gives null for one
        // it cannot read as a directory.
        let dir = unsafe { fdopendir(fd) };
        (!dir.is_null()).then_some(Self(dir))
    }

    /// The next record. `errno` is cleared first, so after `None` it is zero
    /// at the end of the directory and the cause after a failure.
    fn next(&mut self) -> Option<Entry<'_>> {
        super::errno::set(0);
        // SAFETY: the stream is open until `self` is dropped.
        let entry = unsafe { readdir(self.0) };
        if entry.is_null() {
            return None;
        }
        // SAFETY: readdir gives a live record with a NUL-terminated name. It
        // stays in place until the stream is read again or closed, and the
        // borrow of `self` prevents both. Only the fields are read, since a
        // record may be shorter than the type that describes it.
        Some(unsafe {
            Entry {
                name: CStr::from_ptr((&raw const (*entry).name).cast()),
                kind: (&raw const (*entry).kind).read(),
            }
        })
    }
}

impl Drop for Entries {
    fn drop(&mut self) {
        // SAFETY: the stream is open, and nothing uses it after this.
        unsafe { closedir(self.0) };
    }
}

/// What `name` under `fd` is, without following a link, or the `errno` of the failure.
fn entry_stat(fd: c_int, name: &CStr) -> Result<Stat, c_int> {
    let mut info = MaybeUninit::<Stat>::uninit();
    // SAFETY: `name` is a C string and `info` has room for the one record the
    // call writes.
    if unsafe { fstatat(fd, name.as_ptr(), info.as_mut_ptr(), AT_SYMLINK_NOFOLLOW) } != 0 {
        return Err(super::errno::get());
    }
    // SAFETY: the call succeeded, so it filled the record.
    Ok(unsafe { info.assume_init() })
}

// Fail closed if a moved tree contains a secret or granted executable, or if
// it cannot be scanned. This applies even to non-data-only directory grants.
fn protected_tree_mode(
    parent: c_int,
    name: &CStr,
    depth: u32,
    remaining: &mut u32,
    programs: bool,
) -> u8 {
    if SECRET_OBJECTS.is_empty() && (!programs || !witchy_exec_guarded()) {
        return 0;
    }
    if depth == 0 || *remaining == 0 {
        return 5;
    }
    *remaining -= 1;
    let info = match entry_stat(parent, name) {
        Ok(info) => info,
        Err(error) => return if error == 2 { 0 } else { 5 },
    };
    if secret_identity(&info)
        || (programs && witchy_exec_program(info.device as u64, info.inode))
    {
        return 2;
    }
    if info.mode & 0o170000 != 0o040000 {
        return 0;
    }
    let fd = open_checked(parent, name, DIRECTORY | CLOEXEC | NOFOLLOW_ANY);
    if fd < 0 {
        return 5;
    }
    if !stat(fd).is_some_and(|opened| opened.device == info.device && opened.inode == info.inode) {
        close(fd);
        return 5;
    }
    let Some(mut dir) = Entries::open(fd) else {
        close(fd);
        return 5;
    };
    let result = loop {
        let Some(entry) = dir.next() else {
            break if super::errno::get() == 0 { 0 } else { 5 };
        };
        let child = entry.name;
        if matches!(child.to_bytes(), b"." | b"..") {
            continue;
        }
        let code = protected_tree_mode(fd, child, depth - 1, remaining, programs);
        if code != 0 {
            break code;
        }
    };
    drop(dir);
    result
}

fn protected_tree(parent: c_int, name: &CStr, depth: u32, remaining: &mut u32) -> u8 {
    protected_tree_mode(parent, name, depth, remaining, true)
}

/// Record all secret source identities before forking the broker, then reject
/// a writable tree that contains one or cannot be completely inspected.
/// `write_dir` is null or a live C string.
pub(super) unsafe fn prepare_secret_protection(write_dir: *const c_char, write_fd: c_int) -> bool {
    #[cfg(witchy_native_aws_lc)]
    {
        #[cfg(target_os = "macos")]
        const DUP_CLOEXEC: c_int = 67;
        #[cfg(target_os = "linux")]
        const DUP_CLOEXEC: c_int = 1030;

        unsafe extern "C" {
            fn fcntl(fd: c_int, command: c_int, ...) -> c_int;
        }
        let mut devices = [0u64; 192];
        let mut inodes = [0u64; 192];
        let count = super::signer_crypto::witchy_secret_identities(&mut devices, &mut inodes);
        let kept = (count as usize).min(192);
        witchy_broker_secret_objects(&devices[..kept], &inodes[..kept]);
        if count != 0 && (!write_dir.is_null() || write_fd >= 0) {
            let fd = if write_fd >= 0 {
                // SAFETY: duplicating a descriptor takes an integer argument.
                unsafe { fcntl(write_fd, DUP_CLOEXEC, 3 as c_int) }
            } else {
                // SAFETY: `write_dir` is not null here, so the caller vouches
                // that it is a live C string.
                let write_dir = unsafe { CStr::from_ptr(write_dir) };
                open_absolute(write_dir, DIRECTORY | CLOEXEC | NOFOLLOW_ANY)
            };
            if fd < 0 {
                return false;
            }
            let mut remaining = 65536;
            let code = protected_tree_mode(fd, c".", 64, &mut remaining, false);
            close(fd);
            return code == 0;
        }
    }
    #[cfg(not(witchy_native_aws_lc))]
    let _ = (write_dir, write_fd);
    true
}

fn sweep_staging(fd: c_int) -> bool {
    let scan = super::sys::openat(fd, c".", CLOEXEC | DIRECTORY);
    if scan < 0 {
        return false;
    }
    let Some(mut entries) = Entries::open(scan) else {
        close(scan);
        return false;
    };
    let ok = loop {
        let Some(entry) = entries.next() else {
            break super::errno::get() == 0;
        };
        let name = entry.name;
        if matches!(name.to_bytes(), b"." | b"..") {
            continue;
        }
        if !staged_name(name.to_bytes()) {
            break false;
        }
        let Ok(info) = entry_stat(fd, name) else {
            break false;
        };
        if info.mode & 0o170000 != 0o100000
            || info.links != 1
            || super::sys::unlinkat(fd, name, 0) != 0
        {
            break false;
        }
    };
    drop(entries);
    ok
}

/// Open and lock the staging directory, sweeping under the exclusive lock.
pub(super) fn witchy_broker_open_staging(root: c_int) -> c_int {
    unsafe extern "C" {
        safe fn geteuid() -> u32;
        safe fn flock(fd: c_int, operation: c_int) -> c_int;
    }
    const EEXIST: c_int = 17;
    const LOCK_SHARED: c_int = 1;
    const LOCK_EXCLUSIVE_NONBLOCK: c_int = 2 | 4;
    const FLAGS: c_int = DIRECTORY | CLOEXEC | NOFOLLOW_ANY;
    let name = c".witchy-staging";
    if super::sys::mkdirat(root, name, 0o700) != 0 && super::errno::get() != EEXIST {
        return -1;
    }
    let fd = open_checked(root, name, FLAGS);
    if fd < 0 {
        return -1;
    }
    let trusted = stat(root).zip(stat(fd)).is_some_and(|(base, staging)| {
        staging.device == base.device
            && staging.uid == geteuid()
            && staging.mode & 0o77 == 0
    });
    // Match the staging lock protocol used by the Wasmtime host.
    if !trusted
        || (flock(fd, LOCK_EXCLUSIVE_NONBLOCK) == 0 && !sweep_staging(fd))
        || (flock(fd, LOCK_SHARED) != 0)
    {
        close(fd);
        return -1;
    }
    fd
}

fn check_rename_tree(
    parent: c_int,
    name: &CStr,
    device: Device,
    depth: u32,
    remaining: &mut u32,
) -> u8 {
    if depth == 0 || *remaining == 0 {
        return 5;
    }
    *remaining -= 1;
    let Ok(info) = entry_stat(parent, name) else {
        return 5;
    };
    let kind = info.mode & 0o170000;
    if info.device != device || kind == 0o120000 {
        return 2;
    }
    if kind == 0o100000 {
        return 0;
    }
    if kind != 0o040000 {
        return 4;
    }
    let fd = open_checked(parent, name, DIRECTORY | CLOEXEC | NOFOLLOW_ANY);
    if fd < 0 {
        return 5;
    }
    if !stat(fd).is_some_and(|opened| opened.device == device && opened.inode == info.inode) {
        close(fd);
        return 2;
    }
    let Some(mut dir) = Entries::open(fd) else {
        close(fd);
        return 5;
    };
    let code = loop {
        let Some(entry) = dir.next() else {
            break if super::errno::get() == 0 { 0 } else { 5 };
        };
        let child = entry.name;
        if matches!(child.to_bytes(), b"." | b"..") {
            continue;
        }
        if !super::broker_path::permitted_name(child.to_bytes(), 2) {
            break 2;
        }
        let code = check_rename_tree(fd, child, device, depth - 1, remaining);
        if code != 0 {
            break code;
        }
    };
    drop(dir);
    code
}

fn regular_or_directory(info: &Stat) -> bool {
    matches!(info.mode & 0o170000, 0o100000 | 0o040000)
}

fn rename_entries(
    root: c_int,
    from: c_int,
    from_name: &CStr,
    to: c_int,
    to_name: &CStr,
    data_only: bool,
) -> u8 {
    const ENOENT: c_int = 2;
    let source = match entry_stat(from, from_name) {
        Ok(info) => info,
        Err(ENOENT) => return 1,
        Err(_) => return 5,
    };
    if !regular_or_directory(&source) {
        return 4;
    }
    if source.mode & 0o170000 == 0o100000 && (source.links != 1 || protected_identity(&source)) {
        return 2;
    }
    let mut guarded = 65536;
    let code = protected_tree(from, from_name, 64, &mut guarded);
    if code != 0 {
        return code;
    }
    let code = protected_tree(to, to_name, 64, &mut guarded);
    if code != 0 {
        return code;
    }
    let mut remaining = 65536;
    let device = if data_only {
        let Some(base) = stat(root) else { return 5 };
        let code = check_rename_tree(from, from_name, base.device, 64, &mut remaining);
        if code != 0 {
            return code;
        }
        Some(base.device)
    } else {
        None
    };
    match entry_stat(to, to_name) {
        Ok(info) => {
            if info.mode & 0o170000 == 0o100000 && (info.links != 1 || protected_identity(&info)) {
                return 2;
            }
            if let Some(device) = device {
                let code = check_rename_tree(to, to_name, device, 64, &mut remaining);
                if code != 0 {
                    return code;
                }
            } else if !regular_or_directory(&info) {
                return 5;
            }
        }
        Err(ENOENT) => {}
        Err(_) => return 5,
    }
    if super::sys::renameat(from, from_name, to, to_name) == 0 {
        0
    } else {
        5
    }
}

/// `bytes` begins with the `count` packet bytes and has one more byte after
/// them for the target path terminator.
pub(super) fn witchy_broker_rename_rooted(
    root: c_int,
    bytes: &mut [u8],
    count: usize,
    data_only: u32,
) -> u8 {
    use super::broker_path::relative_path;
    if !(4..16384).contains(&count) {
        return 2;
    }
    let Some(bytes) = bytes.get_mut(..=count) else {
        return 2;
    };
    let source_len = u16::from_be_bytes([bytes[0], bytes[1]]) as usize;
    if source_len == 0 || source_len >= count - 2 {
        return 2;
    }
    // The source path is at 2. The target path follows it and ends at `count`.
    let target = 2 + source_len;
    if !relative_path(&bytes[target..count], data_only) {
        return 2;
    }
    let mut to_name = [0; 256];
    let mut from_name = [0; 256];
    bytes[count] = 0;
    let to = text(&bytes[target..]).map_or(-1, |path| {
        witchy_broker_open_output_parent(root, path, &mut to_name, data_only != 0)
    });
    let code = if !relative_path(&bytes[2..target], data_only) {
        2
    } else if to < 0 {
        5
    } else {
        bytes[target] = 0;
        let from = text(&bytes[2..]).map_or(-1, |path| {
            witchy_broker_open_output_parent(root, path, &mut from_name, data_only != 0)
        });
        if from < 0 {
            5
        } else {
            // Opening each parent stored its name with a NUL.
            let code = match (text(&from_name), text(&to_name)) {
                (Some(from_name), Some(to_name)) => {
                    rename_entries(root, from, from_name, to, to_name, data_only != 0)
                }
                _ => 5,
            };
            close(from);
            code
        }
    };
    if to >= 0 {
        close(to);
    }
    code
}

/// `path` begins with the `len` path bytes and has one more byte after them
/// for the terminator.
pub(super) fn witchy_broker_modify_directory(
    root: c_int,
    path: &mut [u8],
    len: usize,
    data_only: u32,
    create: bool,
    entries: &mut u64,
) -> u8 {
    if len >= 16383 {
        return 2;
    }
    let Some(path) = path.get_mut(..=len) else {
        return 2;
    };
    if !super::broker_path::relative_path(&path[..len], data_only) {
        return 2;
    }
    let mut leaf = [0; 256];
    path[len] = 0;
    let parent = text(path).map_or(-1, |path| {
        witchy_broker_open_output_parent(root, path, &mut leaf, data_only != 0)
    });
    if parent < 0 {
        return 5;
    }
    // Opening the parent stored the name with a NUL.
    let code = match text(&leaf) {
        Some(leaf) if create => create_directory(parent, leaf, entries),
        Some(leaf) => remove_file(parent, leaf),
        None => 5,
    };
    close(parent);
    code
}

fn create_directory(parent: c_int, leaf: &CStr, entries: &mut u64) -> u8 {
    const EEXIST: c_int = 17;
    if !super::entry_budget::available(*entries) {
        return if entry_stat(parent, leaf).is_ok() {
            6
        } else {
            3
        };
    }
    if super::sys::mkdirat(parent, leaf, 0o777) == 0 {
        super::entry_budget::spend(entries);
        0
    } else if super::errno::get() == EEXIST {
        6
    } else {
        5
    }
}

fn remove_file(parent: c_int, leaf: &CStr) -> u8 {
    const ENOENT: c_int = 2;
    match entry_stat(parent, leaf) {
        Ok(info) if info.mode & 0o170000 != 0o100000 => return 4,
        Ok(info) if secret_identity(&info) => return 2,
        Ok(_) => {}
        Err(ENOENT) => return 1,
        Err(_) => return 5,
    }
    if super::sys::unlinkat(parent, leaf, 0) == 0 {
        0
    } else if super::errno::get() == ENOENT {
        1
    } else {
        5
    }
}

#[cfg(test)]
#[test]
fn protected_tree_finds_secrets_inside_nested_directory() {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let root = std::env::temp_dir().join(format!("witchy-protected-tree-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(root.join("nested")).unwrap();
    std::fs::write(root.join("nested/secret"), b"key").unwrap();
    let root_name = CString::new(root.as_os_str().as_bytes()).unwrap();
    let root_fd = super::sys::open(&root_name, DIRECTORY | CLOEXEC);
    let secret_fd = super::sys::openat(root_fd, c"nested/secret", CLOEXEC);
    assert!(root_fd >= 0 && secret_fd >= 0);
    let info = stat(secret_fd).unwrap();
    let device = info.device as u64;
    let inode = info.inode;
    witchy_broker_secret_objects(&[device], &[inode]);
    let mut remaining = 100;
    assert_eq!(protected_tree(root_fd, c"nested", 64, &mut remaining), 2);
    assert_eq!(
        rename_entries(root_fd, root_fd, c"nested", root_fd, c"moved", false),
        2
    );
    let nested_fd = super::sys::openat(root_fd, c"nested", DIRECTORY | CLOEXEC);
    assert!(nested_fd >= 0);
    assert_eq!(remove_file(nested_fd, c"secret"), 2);
    close(nested_fd);
    let mut remaining = 100;
    assert_eq!(protected_tree(root_fd, c"absent", 64, &mut remaining), 0);
    let mut remaining = 0;
    assert_eq!(protected_tree(root_fd, c"nested", 64, &mut remaining), 5);
    witchy_broker_secret_objects(&[], &[]);
    close(secret_fd);
    close(root_fd);
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(test)]
#[test]
fn per_connection_entry_limit_applies_with_unlimited_run_budget() {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let root = std::env::temp_dir().join(format!("witchy-entry-budget-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let name = CString::new(root.as_os_str().as_bytes()).unwrap();
    let fd = super::sys::open(&name, DIRECTORY | CLOEXEC);
    assert!(fd >= 0);
    super::entry_budget::set_connection(1);
    let mut run = u64::MAX;
    assert_eq!(create_directory(fd, c"first", &mut run), 0);
    assert_eq!(create_directory(fd, c"second", &mut run), 3);
    assert!(!root.join("second").exists());
    super::entry_budget::set_connection(u64::MAX);
    close(fd);
    std::fs::remove_dir_all(root).unwrap();
}

/// Appends the staged file to the named one. `scratch` is the copy buffer.
fn append_output(parent: c_int, name: &CStr, staged: c_int, scratch: &mut [u8; 16384]) -> u8 {
    // O_WRONLY | O_APPEND | O_NOFOLLOW | O_NONBLOCK | O_CLOEXEC.
    #[cfg(target_os = "macos")]
    const APPEND_FLAGS: c_int = 0x0100_010d;
    #[cfg(target_os = "linux")]
    const APPEND_FLAGS: c_int = CLOEXEC | NOFOLLOW_ANY | NONBLOCK | 0o2001;
    let fd = open_checked(parent, name, APPEND_FLAGS);
    if fd < 0 {
        return 5;
    }
    let code = match stat(fd).zip(stat(parent)) {
        Some((target, _)) if target.mode & 0o170000 != 0o100000 => 4,
        Some((target, _)) if secret_identity(&target) => _exit(3),
        Some((target, dir))
            if target.links != 1 || target.device != dir.device || protected_identity(&target) =>
        {
            2
        }
        Some(_) => append_bytes(fd, staged, scratch),
        None => 5,
    };
    if close(fd) != 0 && code == 0 {
        5
    } else {
        code
    }
}

fn append_bytes(target: c_int, staged: c_int, scratch: &mut [u8; 16384]) -> u8 {
    unsafe extern "C" {
        safe fn lseek(fd: c_int, offset: i64, whence: c_int) -> i64;
        safe fn fsync(fd: c_int) -> c_int;
    }
    const EINTR: c_int = 4;
    if lseek(staged, 0, 0) < 0 {
        return 5;
    }
    loop {
        let count = super::sys::read(staged, &mut scratch[..]);
        if count == 0 {
            return if fsync(target) == 0 { 0 } else { 5 };
        }
        if count < 0 {
            if super::errno::get() == EINTR {
                continue;
            }
            return 5;
        }
        let count = count as usize;
        let mut offset = 0;
        while offset < count {
            // A read gives no more than the buffer holds.
            let Some(rest) = scratch.get(offset..count) else {
                return 5;
            };
            let written = super::sys::write(target, rest);
            if written < 0 && super::errno::get() == EINTR {
                continue;
            }
            if written <= 0 {
                return 5;
            }
            offset += written as usize;
        }
    }
}

/// Moves `source` onto `target` as the kernel's `flags` for the call direct.
fn rename_with(from: c_int, source: &CStr, to: c_int, target: &CStr, flags: u32) -> bool {
    #[cfg(target_os = "macos")]
    { super::sys::renameatx_np(from, source, to, target, flags) == 0 }
    #[cfg(target_os = "linux")]
    { super::sys::renameat2(from, source, to, target, flags) == 0 }
}

/// Publish a staged file and consume its descriptor. `scratch` is the copy
/// buffer for an append.
fn publish(
    staging: c_int,
    temp: &CStr,
    parent: c_int,
    name: &CStr,
    fd: c_int,
    scratch: &mut [u8; 16384],
    mode: u32,
    entries: &mut u64,
    mut ok: bool,
) -> u8 {
    const SWAP: u32 = 2;
    #[cfg(target_os = "macos")]
    const EXCL: u32 = 4;
    #[cfg(target_os = "linux")]
    const EXCL: u32 = 1;
    const EXISTS: c_int = 17;
    const MISSING: c_int = 2;
    let rename = |flags| rename_with(staging, temp, parent, name, flags);
    let info = || entry_stat(parent, name);
    let regular = |info: &Stat| info.mode & 0o170000 == 0o100000;
    let mut append = || append_output(parent, name, fd, scratch);
    let mut code = 5;
    let available = super::entry_budget::available(*entries);
    if ok && super::entry_budget::limited(*entries) {
        if available && rename(EXCL) {
            super::entry_budget::spend(entries);
            close(fd);
            return 0;
        }
        match info() {
            Ok(_) if mode == 1 => code = 6,
            Ok(info) if regular(&info) && (info.links != 1 || protected_identity(&info)) => {
                code = 2
            }
            Ok(info) if regular(&info) => {
                if mode == 2 {
                    code = append();
                } else if rename(SWAP) {
                    code = 0;
                }
            }
            Err(MISSING) if !available => code = 3,
            _ => {}
        }
        close(fd);
    } else {
        if ok && mode == 2 {
            if rename(EXCL) {
                close(fd);
                return 0;
            }
            if super::errno::get() == EXISTS {
                code = append();
            }
        }
        ok &= close(fd) == 0;
        if ok && mode == 1 {
            if rename(EXCL) {
                return 0;
            }
            if super::errno::get() == EXISTS {
                code = 6;
            }
        } else if ok && mode == 0 {
            let admissible = match info() {
                Ok(info) if regular(&info) && (info.links != 1 || protected_identity(&info)) => {
                    code = 2;
                    false
                }
                Ok(info) => regular(&info),
                Err(error) => error == MISSING,
            };
            if admissible && super::sys::renameat(staging, temp, parent, name) == 0 {
                return 0;
            }
        }
    }
    super::sys::unlinkat(staging, temp, 0);
    code
}

/// Receives one staged write of `length` bytes through `scratch` and
/// publishes it as `name` under `parent`.
fn witchy_broker_commit_output(
    channel: c_int,
    staging: c_int,
    parent: c_int,
    name: &CStr,
    mut length: usize,
    scratch: &mut [u8; 16384],
    mode: u32,
    entries: &mut u64,
) -> u8 {
    unsafe extern "C" {
        fn snprintf(out: *mut c_char, len: usize, format: *const c_char, ...) -> c_int;
        safe fn getpid() -> c_int;
        #[cfg(target_os = "macos")]
        safe fn arc4random() -> u32;
        safe fn fsync(fd: c_int) -> c_int;
    }
    const EINTR: c_int = 4;
    // O_RDWR | O_CREAT | O_EXCL | O_CLOEXEC | O_NOFOLLOW.
    #[cfg(target_os = "macos")]
    const FLAGS: c_int = 0x0100_0b02;
    #[cfg(target_os = "linux")]
    const FLAGS: c_int = CLOEXEC | NOFOLLOW_ANY | 0o302;
    #[cfg(target_os = "macos")]
    let random = || arc4random();
    #[cfg(target_os = "linux")]
    let random = || {
        let mut value = [0u8; size_of::<u32>()];
        let count = super::sys::getrandom(&mut value, 0);
        if count != size_of::<u32>() as isize {
            return None;
        }
        Some(u32::from_ne_bytes(value))
    };
    #[cfg(target_os = "macos")]
    let random = || Some(random());
    let Some(a) = random() else {
        _exit(2)
    };
    let Some(b) = random() else {
        _exit(2)
    };
    let Some(c) = random() else {
        _exit(2)
    };
    let mut temp = [0u8; 64];
    // SAFETY: the output has room for the length passed, and each format
    // reads only the integers that follow it.
    let named = unsafe {
        if staging != parent {
            snprintf(
                temp.as_mut_ptr().cast(),
                temp.len(),
                c"%d-%u".as_ptr(),
                getpid(),
                a,
            )
        } else {
            snprintf(
                temp.as_mut_ptr().cast(),
                temp.len(),
                c".witchy-%08x%08x%08x".as_ptr(),
                a,
                b,
                c,
            )
        }
    };
    // A name that fit was written with its NUL.
    let temp = if named > 0 && (named as usize) < temp.len() {
        text(&temp)
    } else {
        None
    };
    let fd = temp.map_or(-1, |temp| super::sys::openat_mode(staging, temp, FLAGS, 0o600));
    let mut ok = fd >= 0;
    while length != 0 {
        let chunk = length.min(16384);
        if !super::broker_transport::witchy_broker_receive(channel, &mut scratch[..chunk]) {
            ok = false;
            break;
        }
        if ok {
            let mut offset = 0;
            while offset < chunk {
                let count = super::sys::write(fd, &scratch[offset..chunk]);
                if count < 0 && super::errno::get() == EINTR {
                    continue;
                }
                if count <= 0 {
                    ok = false;
                    break;
                }
                offset += count as usize;
            }
        }
        length -= chunk;
    }
    let Some(temp) = temp.filter(|_| fd >= 0) else {
        return 5;
    };
    if ok {
        loop {
            let synced = fsync(fd);
            if synced < 0 && super::errno::get() == EINTR {
                continue;
            }
            ok = synced == 0;
            break;
        }
    }
    publish(staging, temp, parent, name, fd, scratch, mode, entries, ok)
}

/// `bytes` is null, or `length` bytes from `malloc` (one byte when `length`
/// is zero), and `cursor` is at most `length`.
#[repr(C)]
pub(super) struct Listing {
    pub(super) bytes: *mut u8,
    pub(super) length: usize,
    pub(super) cursor: usize,
}

unsafe extern "C" {
    fn malloc(size: usize) -> *mut c_void;
    fn free(address: *mut c_void);
    fn qsort(
        base: *mut c_void,
        count: usize,
        size: usize,
        compare: unsafe extern "C" fn(*const c_void, *const c_void) -> c_int,
    );
    fn strcmp(left: *const c_char, right: *const c_char) -> c_int;
}

unsafe extern "C" fn compare_names(left: *const c_void, right: *const c_void) -> c_int {
    unsafe {
        strcmp(
            left.cast::<*const c_char>().read(),
            right.cast::<*const c_char>().read(),
        )
    }
}

/// On success, `listing` is replaced, without being freed, by a malloc-owned
/// listing with cursor zero.
fn witchy_broker_list_directory(base: c_int, listing: &mut Listing, mode: u32) -> bool {
    const MAX_BYTES: usize = 1024 * 1024;
    const MAX_ENTRIES: usize = 65536;
    let fd = super::sys::openat(base, c".", DIRECTORY | CLOEXEC);
    if fd < 0 {
        return false;
    }
    let Some(mut dir) = Entries::open(fd) else {
        close(fd);
        return false;
    };
    let names = unsafe { malloc(MAX_ENTRIES * size_of::<*mut u8>()) }.cast::<*mut u8>();
    if names.is_null() {
        drop(dir);
        return false;
    }
    let (mut count, mut length, mut visited) = (0, 0, 0);
    let ok = loop {
        let Some(entry) = dir.next() else {
            break super::errno::get() == 0;
        };
        let name = entry.name;
        let bytes = name.to_bytes();
        if matches!(bytes, b"." | b"..") {
            continue;
        }
        visited += 1;
        if visited > MAX_ENTRIES {
            break false;
        }
        if (mode != 0 && !super::broker_path::permitted_name(bytes, mode))
            || bytes.iter().any(|&byte| byte == b'\n')
            || !super::launch_args::valid_utf8(bytes)
        {
            continue;
        }
        let directory = match entry.kind {
            4 => true, // DT_DIR
            0 => {
                let Ok(info) = entry_stat(fd, name) else {
                    break false;
                };
                info.mode & 0o170000 == 0o040000
            }
            _ => false,
        };
        let line = bytes.len() + if directory { 2 } else { 1 };
        if line > MAX_BYTES - length {
            break false;
        }
        let copy = unsafe { malloc(line) }.cast::<u8>();
        if copy.is_null() {
            break false;
        }
        unsafe {
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), copy, bytes.len());
            if directory {
                copy.add(bytes.len()).write(b'/');
            }
            copy.add(line - 1).write(0);
            names.add(count).write(copy);
        }
        count += 1;
        length += line;
    };
    drop(dir);
    let bytes = if ok {
        unsafe { malloc(length.max(1)) }.cast::<u8>()
    } else {
        core::ptr::null_mut()
    };
    if !bytes.is_null() {
        unsafe { qsort(names.cast(), count, size_of::<*mut u8>(), compare_names) };
        let mut offset = 0;
        for index in 0..count {
            let name = unsafe { CStr::from_ptr(names.add(index).read().cast()) }.to_bytes();
            unsafe { core::ptr::copy_nonoverlapping(name.as_ptr(), bytes.add(offset), name.len()) };
            offset += name.len();
            unsafe { bytes.add(offset).write(b'\n') };
            offset += 1;
        }
        *listing = Listing {
            bytes,
            length,
            cursor: 0,
        };
    }
    for index in 0..count {
        unsafe { free(names.add(index).read().cast()) };
    }
    unsafe { free(names.cast()) };
    !bytes.is_null()
}

#[repr(C)]
pub(super) struct Roots {
    pub(super) read: c_int,
    pub(super) write: c_int,
    pub(super) output: c_int,
    pub(super) staging: c_int,
    pub(super) name: [c_char; 256],
}

/// Called during broker child startup. On failure the child exits, which
/// closes any descriptors opened before a later check failed. Each path is
/// null or a live C string, `identity` is null or readable, and `roots` is
/// writable.
pub(super) unsafe fn open_roots(
    read: *const c_char,
    write: *const c_char,
    read_fd: c_int,
    write_fd: c_int,
    output: *const c_char,
    output_parent: c_int,
    identity: *const super::file_identity::Identity,
    data_only: bool,
    roots: *mut Roots,
) -> bool {
    unsafe extern "C" {
        safe fn dup(fd: c_int) -> c_int;
    }
    const FLAGS: c_int = DIRECTORY | CLOEXEC | NOFOLLOW_ANY;
    #[cfg(target_os = "macos")]
    const AT_FDCWD: c_int = -2;
    #[cfg(target_os = "linux")]
    const AT_FDCWD: c_int = -100;
    let mut opened = Roots {
        read: -1,
        write: -1,
        output: -1,
        staging: -1,
        name: [0; 256],
    };
    if !read.is_null() {
        opened.read = if read_fd >= 0 {
            read_fd
        } else {
            // SAFETY: `read` is not null, so it is a live C string.
            open_absolute(unsafe { CStr::from_ptr(read) }, FLAGS)
        };
        if opened.read < 0 {
            return false;
        }
    }
    if !write.is_null() {
        opened.write = if write == read {
            dup(opened.read)
        } else if write_fd >= 0 {
            write_fd
        } else {
            // SAFETY: `write` is not null, so it is a live C string.
            open_absolute(unsafe { CStr::from_ptr(write) }, FLAGS)
        };
        if opened.write < 0 {
            return false;
        }
    }
    // SAFETY: the caller passes null or a readable identity.
    if let Some(expected) = unsafe { identity.as_ref() } {
        let selected = if opened.read >= 0 {
            opened.read
        } else {
            opened.write
        };
        if !stat(selected).is_some_and(|info| {
            info.device as u64 == expected.device && info.inode == expected.inode
        }) {
            return false;
        }
    }
    if !output.is_null() {
        // SAFETY: `output` is not null, so it is a live C string.
        let output = unsafe { CStr::from_ptr(output) };
        opened.output = if output_parent >= 0 {
            let name = output.to_bytes();
            if name.is_empty()
                || name.len() > 255
                || name.contains(&b'/')
                || name == b"."
                || name == b".."
            {
                return false;
            }
            for (out, byte) in opened.name.iter_mut().zip(name) {
                *out = *byte as c_char;
            }
            output_parent
        } else {
            let mut name = [0; 256];
            let fd = witchy_broker_open_output_parent(AT_FDCWD, output, &mut name, false);
            for (out, byte) in opened.name.iter_mut().zip(name) {
                *out = byte as c_char;
            }
            fd
        };
        if opened.output < 0 {
            return false;
        }
    }
    if data_only && opened.write >= 0 {
        opened.staging = witchy_broker_open_staging(opened.write);
        if opened.staging < 0 {
            return false;
        }
    }
    // SAFETY: the caller passes a place for the result.
    unsafe { roots.write(opened) };
    true
}

#[repr(C)]
pub(super) struct Output {
    pub(super) parent: c_int,
    pub(super) mode: u32,
    pub(super) name: [u8; 256],
}

/// While `root_output` holds a descriptor, `root_name` is a live C string.
pub(super) unsafe fn witchy_broker_output_request(
    channel: c_int,
    operation: u8,
    slot: usize,
    count: usize,
    scratch: &mut [u8; 16384],
    outputs: &mut [Output; 16],
    root_output: &mut c_int,
    root_name: *const c_char,
    root_exclusive: bool,
    write_root: c_int,
    staging: c_int,
    files: &[c_int; 16],
    kinds: &[u32; 16],
    data_only: u32,
    entries: &mut u64,
) -> bool {
    use super::broker_transport::{witchy_broker_receive, witchy_broker_reply};
    if matches!(operation, 18 | 20 | 22) {
        let Some(output) = outputs.get_mut(slot) else {
            return false;
        };
        if output.parent >= 0
            || !(2..16384).contains(&count)
            || !witchy_broker_receive(channel, &mut scratch[..count])
        {
            return false;
        }
        let source = usize::from(scratch[0]);
        let base = if source == 16 {
            write_root
        } else if kinds.get(source).is_some_and(|kind| kind & 4 != 0) {
            files.get(source).copied().unwrap_or(-1)
        } else {
            -1
        };
        let mut failure = 2;
        if base >= 0 && super::broker_path::relative_path(&scratch[1..count], data_only) {
            scratch[count] = 0;
            output.parent = match text(&scratch[1..]) {
                Some(path) => witchy_broker_open_output_parent(
                    base,
                    path,
                    &mut output.name,
                    data_only != 0,
                ),
                None => {
                    super::errno::set(EINVAL);
                    -1
                }
            };
            if output.parent < 0 {
                failure = match super::errno::get() {
                    2 => 1,
                    20 => 4,
                    62 | EXDEV => 2,
                    _ => 5,
                };
            }
        }
        output.mode = match operation {
            22 => 2,
            20 => 1,
            _ => 0,
        };
        return if output.parent >= 0 {
            witchy_broker_reply(channel, 0, 0)
        } else {
            witchy_broker_reply(channel, 1, failure)
        };
    }

    let (parent, name, mode) = if slot == 16 {
        (root_output, None, u32::from(root_exclusive))
    } else if let Some(output) = outputs.get_mut(slot) {
        (&mut output.parent, Some(&output.name), output.mode)
    } else {
        return false;
    };
    let commit = operation == 16;
    if *parent < 0
        || count != if commit { 4 } else { 0 }
        || !witchy_broker_receive(channel, &mut scratch[..count])
    {
        return false;
    }
    let length = if commit {
        u32::from_be_bytes([scratch[0], scratch[1], scratch[2], scratch[3]]) as usize
    } else {
        0
    };
    if length > 1024 * 1024 {
        return false;
    }
    let temporary = if staging >= 0 && slot < 16 {
        staging
    } else {
        *parent
    };
    let code = if commit {
        let name = match name {
            // Opening the slot stored its name with a NUL.
            Some(name) => text(name),
            // SAFETY: the root output is open, so the caller vouches for its name.
            None => Some(unsafe { CStr::from_ptr(root_name) }),
        };
        let Some(name) = name else {
            return false;
        };
        witchy_broker_commit_output(
            channel, temporary, *parent, name, length, scratch, mode, entries,
        )
    } else {
        0
    };
    close(*parent);
    *parent = -1;
    witchy_broker_reply(channel, 0, usize::from(code))
}

/// Each listing whose slot has kind 3 is as `Listing` describes, and no other
/// copy of its pointer is in use.
pub(super) unsafe fn witchy_broker_file_request(
    channel: c_int,
    op: u8,
    slot: usize,
    count: usize,
    bytes: &mut [u8; 16384],
    root: c_int,
    write_root: c_int,
    combined: bool,
    mode: u32,
    files: &mut [c_int; 16],
    kinds: &mut [u32; 16],
    listings: &mut [Listing; 16],
) -> bool {
    use super::broker_transport::{
        witchy_broker_receive as receive, witchy_broker_reply as reply, witchy_broker_send as send,
    };
    use core::ptr::null_mut;
    let bad = || {
        reply(channel, 2, 0);
        false
    };
    let Some((&fd, &kind)) = files.get(slot).zip(kinds.get(slot)) else {
        return bad();
    };
    if count > 16384 {
        return bad();
    }
    let mut payload = 0;
    let (ok, failure) = match op {
        15 if kind == 0 && count == 1 => {
            if !receive(channel, &mut bytes[..count]) {
                return false;
            }
            let source = usize::from(bytes[0]);
            let base = if source == 16 {
                root
            } else if kinds.get(source).is_some_and(|k| matches!(k, 2 | 6)) {
                files[source]
            } else {
                -1
            };
            let ok = base >= 0 && witchy_broker_list_directory(base, &mut listings[slot], mode);
            if ok {
                kinds[slot] = 3;
            }
            (ok, 0)
        }
        11 if kind == 3 => {
            let listing = &mut listings[slot];
            payload = count.min(listing.length - listing.cursor);
            if payload != 0 {
                // SAFETY: the slot holds a listing, so `bytes` is readable for
                // `length`. `payload` is no more than what follows the cursor
                // there, and no more than `count`, which the scratch holds.
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        listing.bytes.add(listing.cursor),
                        bytes.as_mut_ptr(),
                        payload,
                    )
                };
            }
            listing.cursor += payload;
            (true, 0)
        }
        12 if kind == 3 && count == 0 => {
            // SAFETY: the slot holds a listing, whose bytes came from `malloc`
            // and are forgotten below.
            unsafe { free(listings[slot].bytes.cast()) };
            listings[slot] = Listing {
                bytes: null_mut(),
                length: 0,
                cursor: 0,
            };
            kinds[slot] = 0;
            (true, 0)
        }
        10 | 13 | 14 | 24 | 26 => {
            if count == 0 || !receive(channel, &mut bytes[..count]) {
                return false;
            }
            let source = usize::from(bytes[0]);
            let full = op == 26;
            let write = full || op == 24;
            let requested = if write { 4 } else { 2 };
            let base = if source == 16 {
                if full && !combined {
                    -1
                } else if write {
                    write_root
                } else {
                    root
                }
            } else if kinds
                .get(source)
                .is_some_and(|&k| (k == requested || k == 6) && (!full || k == 6))
            {
                files[source]
            } else {
                -1
            };
            if base < 0 || (op != 13 && kind != 0) {
                return bad();
            }
            let mut opened_kind = 0;
            let mut code = 5;
            let opened = if count < 16384
                && super::broker_path::relative_path(&bytes[1..count], mode)
            {
                bytes[count] = 0;
                witchy_broker_open_rooted(
                    base,
                    &mut bytes[1..=count],
                    &mut opened_kind,
                    &mut code,
                    mode != 0,
                )
            } else {
                code = 2;
                -1
            };
            if op == 13 {
                if opened >= 0 {
                    close(opened);
                }
                bytes[0] = opened_kind as u8;
                payload = 1;
                (true, 0)
            } else if opened >= 0 && opened_kind == if op == 10 { 1 } else { 2 } {
                files[slot] = opened;
                kinds[slot] = if full {
                    6
                } else if write {
                    4
                } else {
                    opened_kind
                };
                (true, 0)
            } else if opened >= 0 {
                close(opened);
                (false, 4)
            } else {
                (false, code)
            }
        }
        11 if fd >= 0 && kind == 1 => loop {
            let got = super::sys::read(fd, &mut bytes[..count]);
            if got < 0 && super::errno::get() == 4 {
                continue;
            }
            if got >= 0 {
                payload = got as usize;
            }
            break (got >= 0, 0);
        },
        12 if fd >= 0 && count == 0 => {
            files[slot] = -1;
            kinds[slot] = 0;
            (close(fd) == 0, 0)
        }
        _ => return bad(),
    };
    reply(
        channel,
        u8::from(!ok),
        if ok { payload } else { usize::from(failure) },
    ) && (payload == 0
        || bytes
            .get(..payload)
            .is_some_and(|bytes| send(channel, bytes)))
}

#[cfg(test)]
#[test]
fn staged_publication_replaces_file_and_refuses_hard_links() {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let root = std::env::temp_dir().join(format!("witchy-publish-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let path = CString::new(root.as_os_str().as_bytes()).unwrap();
    let root_fd = super::sys::open(&path, DIRECTORY | CLOEXEC);
    assert!(root_fd >= 0);
    let staging = witchy_broker_open_staging(root_fd);
    assert!(staging >= 0);
    let mut scratch = [0u8; 16384];
    let mut entries = u64::MAX;
    let run = |temporary: &CStr, target: &CStr, entries: &mut u64, scratch: &mut [u8; 16384]| {
        #[cfg(target_os = "macos")]
        const NEW: c_int = 0x0100_0b02;
        #[cfg(target_os = "linux")]
        const NEW: c_int = CLOEXEC | NOFOLLOW_ANY | 0o302;
        let fd = super::sys::openat_mode(staging, temporary, NEW, 0o600);
        assert!(fd >= 0);
        assert_eq!(super::sys::write(fd, b"new"), 3);
        publish(staging, temporary, root_fd, target, fd, scratch, 0, entries, true)
    };
    std::fs::write(root.join("target"), b"old").unwrap();
    assert_eq!(run(c"1-1", c"target", &mut entries, &mut scratch), 0);
    assert_eq!(std::fs::read(root.join("target")).unwrap(), b"new");
    std::fs::hard_link(root.join("target"), root.join("alias")).unwrap();
    assert_eq!(run(c"1-2", c"target", &mut entries, &mut scratch), 2);
    assert_eq!(std::fs::read(root.join("alias")).unwrap(), b"new");
    close(staging);
    close(root_fd);
    std::fs::remove_dir_all(root).unwrap();
}
