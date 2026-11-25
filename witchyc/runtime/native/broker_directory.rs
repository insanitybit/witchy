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
    fn openat(parent: c_int, name: *const c_char, flags: c_int, ...) -> c_int;
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

unsafe fn open_checked(parent: c_int, name: *const c_char, flags: c_int) -> c_int {
    #[cfg(target_os = "macos")]
    { unsafe { openat(parent, name, flags) } }
    #[cfg(target_os = "linux")]
    { unsafe { super::linux_stat::openat_no_symlinks(parent, name, flags) } }
}
const EXDEV: c_int = 18;

static SECRET_OBJECTS: Table<(u64, u64), 192> = Table::new((0, 0));

/// Startup supplies the arrays of secret identities before the broker forks.
#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_broker_secret_objects(
    devices: *const u64,
    inodes: *const u64,
    count: u32,
) {
    let count = (count as usize).min(192);
    SECRET_OBJECTS.clear();
    for index in 0..count {
        SECRET_OBJECTS.push(unsafe { (devices.add(index).read(), inodes.add(index).read()) });
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

/// The broker passes an exclusively writable, NUL-terminated and validated
/// relative path. This traversal consumes separators in that buffer.
unsafe fn witchy_broker_open_data_path(
    root: c_int,
    mut path: *mut c_char,
    directory: bool,
) -> c_int {
    let Some(base) = stat(root) else {
        return -1;
    };
    let mut parent = root;
    let mut remaining = unsafe { CStr::from_ptr(path) }.to_bytes().len();
    loop {
        let slash = unsafe { core::slice::from_raw_parts(path.cast::<u8>(), remaining) }
            .iter()
            .position(|&byte| byte == b'/');
        if let Some(offset) = slash {
            unsafe { path.add(offset).write(0) };
        }
        let flags = NO_LINK_READ
            | if slash.is_some() || directory {
                DIRECTORY
            } else {
                0
            };
        let fd = unsafe { open_checked(parent, path, flags) };
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
        // The separator is before the original NUL, so the next component
        // remains in the same live allocation.
        path = unsafe { path.add(offset + 1) };
        remaining -= offset + 1;
    }
}

/// Open a validated, NUL-terminated relative path and classify the result.
/// The path is writable because the data-only traversal consumes separators.
unsafe fn witchy_broker_open_rooted(
    root: c_int,
    path: *mut c_char,
    kind: *mut u32,
    error: *mut u8,
    data_only: bool,
) -> c_int {
    let fd = if data_only {
        unsafe { witchy_broker_open_data_path(root, path, false) }
    } else {
        #[cfg(target_os = "macos")]
        {
            unsafe { openat(root, path, NO_LINK_READ) }
        }
        #[cfg(target_os = "linux")]
        {
            unsafe { super::linux_stat::openat_no_symlinks(root, path, NO_LINK_READ) }
        }
    };
    if fd < 0 {
        let code = match super::errno::get() {
            2 => 1,               // ENOENT
            40 | 62 | EXDEV => 2, // ELOOP
            20 => 4,              // ENOTDIR
            _ => return -1,
        };
        unsafe { error.write(code) };
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
            unsafe {
                error.write(2);
                close(fd)
            };
            return -1;
        }
        unsafe { kind.write(file_kind) };
        if file_kind != 0 {
            return fd;
        }
        unsafe { error.write(4) };
    }
    close(fd);
    -1
}

/// The caller provides a live pathname and at least 256 writable name bytes.
unsafe fn witchy_broker_open_output_parent(
    base: c_int,
    path: *const c_char,
    name: *mut c_char,
    data_only: bool,
) -> c_int {
    unsafe extern "C" {
        fn open(path: *const c_char, flags: c_int, ...) -> c_int;
    }
    let path = unsafe { CStr::from_ptr(path) }.to_bytes();
    let slash = path.iter().rposition(|&byte| byte == b'/');
    let Some(leaf) = path.get(slash.map_or(0, |offset| offset + 1)..) else {
        return -1;
    };
    if leaf.is_empty() || leaf.len() > 255 || leaf == b"." || leaf == b".." {
        return -1;
    }
    unsafe {
        core::ptr::copy_nonoverlapping(leaf.as_ptr(), name.cast(), leaf.len());
        name.add(leaf.len()).write(0);
    }
    let Some(parent_len) = slash else {
        return unsafe { openat(base, c".".as_ptr(), CLOEXEC | DIRECTORY) };
    };
    if parent_len == 0 {
        return unsafe { open(c"/".as_ptr(), CLOEXEC | DIRECTORY) };
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
    if data_only {
        unsafe { witchy_broker_open_data_path(base, parent.as_mut_ptr().cast(), true) }
    } else {
        #[cfg(target_os = "macos")]
        unsafe {
            openat(
                base,
                parent.as_ptr().cast(),
                CLOEXEC | NOFOLLOW_ANY | DIRECTORY,
            )
        }
        #[cfg(target_os = "linux")]
        unsafe {
            super::linux_stat::openat_no_symlinks(
                base,
                parent.as_ptr().cast(),
                CLOEXEC | NOFOLLOW_ANY | DIRECTORY,
            )
        }
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
    fn unlinkat(fd: c_int, name: *const c_char, flags: c_int) -> c_int;
}
#[cfg(target_os = "macos")]
const AT_SYMLINK_NOFOLLOW: c_int = 0x20;
#[cfg(target_os = "linux")]
const AT_SYMLINK_NOFOLLOW: c_int = 0x100;

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
    let mut info = MaybeUninit::<Stat>::uninit();
    if unsafe {
        fstatat(
            parent,
            name.as_ptr(),
            info.as_mut_ptr(),
            AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return if super::errno::get() == 2 { 0 } else { 5 };
    }
    let info = unsafe { info.assume_init() };
    if secret_identity(&info)
        || (programs && witchy_exec_program(info.device as u64, info.inode))
    {
        return 2;
    }
    if info.mode & 0o170000 != 0o040000 {
        return 0;
    }
    let fd = unsafe { open_checked(parent, name.as_ptr(), DIRECTORY | CLOEXEC | NOFOLLOW_ANY) };
    if fd < 0 {
        return 5;
    }
    if !stat(fd).is_some_and(|opened| opened.device == info.device && opened.inode == info.inode) {
        close(fd);
        return 5;
    }
    let dir = unsafe { fdopendir(fd) };
    if dir.is_null() {
        close(fd);
        return 5;
    }
    let result = loop {
        {
            super::errno::set(0);
        }
        let entry = unsafe { readdir(dir) };
        if entry.is_null() {
            break if super::errno::get() == 0 { 0 } else { 5 };
        }
        let child = unsafe { CStr::from_ptr((&raw const (*entry).name).cast()) };
        if matches!(child.to_bytes(), b"." | b"..") {
            continue;
        }
        let code = protected_tree_mode(fd, child, depth - 1, remaining, programs);
        if code != 0 {
            break code;
        }
    };
    unsafe {
        closedir(dir);
    }
    result
}

fn protected_tree(parent: c_int, name: &CStr, depth: u32, remaining: &mut u32) -> u8 {
    protected_tree_mode(parent, name, depth, remaining, true)
}

/// Record all secret source identities before forking the broker, then reject
/// a writable tree that contains one or cannot be completely inspected.
pub(super) unsafe fn prepare_secret_protection(write_dir: *const c_char, write_fd: c_int) -> bool {
    #[cfg(witchy_native_aws_lc)]
    {
        #[cfg(target_os = "macos")]
        const DUP_CLOEXEC: c_int = 67;
        #[cfg(target_os = "linux")]
        const DUP_CLOEXEC: c_int = 1030;

        unsafe extern "C" {
            fn witchy_secret_identities(devices: *mut u64, inodes: *mut u64, capacity: u32) -> u32;
            #[cfg(target_os = "macos")]
            fn open(path: *const c_char, flags: c_int, ...) -> c_int;
            fn fcntl(fd: c_int, command: c_int, ...) -> c_int;
        }
        let mut devices = [0u64; 192];
        let mut inodes = [0u64; 192];
        let count =
            unsafe { witchy_secret_identities(devices.as_mut_ptr(), inodes.as_mut_ptr(), 192) };
        unsafe {
            witchy_broker_secret_objects(devices.as_ptr(), inodes.as_ptr(), count);
        }
        if count != 0 && (!write_dir.is_null() || write_fd >= 0) {
            let fd = if write_fd >= 0 {
                unsafe { fcntl(write_fd, DUP_CLOEXEC, 3 as c_int) }
            } else {
                #[cfg(target_os = "macos")]
                {
                    unsafe { open(write_dir, DIRECTORY | CLOEXEC | NOFOLLOW_ANY) }
                }
                #[cfg(target_os = "linux")]
                {
                    unsafe {
                        super::linux_stat::openat_no_symlinks(
                            -100,
                            write_dir,
                            DIRECTORY | CLOEXEC | NOFOLLOW_ANY,
                        )
                    }
                }
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
    let scan = unsafe { openat(fd, c".".as_ptr(), CLOEXEC | DIRECTORY) };
    if scan < 0 {
        return false;
    }
    let entries = unsafe { fdopendir(scan) };
    if entries.is_null() {
        close(scan);
        return false;
    }
    let ok = loop {
        super::errno::set(0);
        let entry = unsafe { readdir(entries) };
        if entry.is_null() {
            break super::errno::get() == 0;
        }
        // readdir guarantees a NUL-terminated name for a live record.
        let name = unsafe { CStr::from_ptr((&raw const (*entry).name).cast()) };
        if matches!(name.to_bytes(), b"." | b"..") {
            continue;
        }
        let mut info = MaybeUninit::<Stat>::uninit();
        if !staged_name(name.to_bytes())
            || unsafe { fstatat(fd, name.as_ptr(), info.as_mut_ptr(), AT_SYMLINK_NOFOLLOW) } != 0
        {
            break false;
        }
        let info = unsafe { info.assume_init() };
        if info.mode & 0o170000 != 0o100000
            || info.links != 1
            || unsafe { unlinkat(fd, name.as_ptr(), 0) } != 0
        {
            break false;
        }
    };
    unsafe { closedir(entries) };
    ok
}

/// Open and lock the staging directory, sweeping under the exclusive lock.
pub(super) fn witchy_broker_open_staging(root: c_int) -> c_int {
    unsafe extern "C" {
        fn mkdirat(parent: c_int, name: *const c_char, mode: u16) -> c_int;
        safe fn geteuid() -> u32;
        safe fn flock(fd: c_int, operation: c_int) -> c_int;
    }
    const EEXIST: c_int = 17;
    const LOCK_SHARED: c_int = 1;
    const LOCK_EXCLUSIVE_NONBLOCK: c_int = 2 | 4;
    const FLAGS: c_int = DIRECTORY | CLOEXEC | NOFOLLOW_ANY;
    let name = c".witchy-staging".as_ptr();
    if unsafe { mkdirat(root, name, 0o700) != 0 && super::errno::get() != EEXIST } {
        return -1;
    }
    let fd = unsafe { open_checked(root, name, FLAGS) };
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
    let mut info = MaybeUninit::<Stat>::uninit();
    if unsafe {
        fstatat(
            parent,
            name.as_ptr(),
            info.as_mut_ptr(),
            AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return 5;
    }
    let info = unsafe { info.assume_init() };
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
    let fd = unsafe { open_checked(parent, name.as_ptr(), DIRECTORY | CLOEXEC | NOFOLLOW_ANY) };
    if fd < 0 {
        return 5;
    }
    if !stat(fd).is_some_and(|opened| opened.device == device && opened.inode == info.inode) {
        close(fd);
        return 2;
    }
    let dir = unsafe { fdopendir(fd) };
    if dir.is_null() {
        close(fd);
        return 5;
    }
    let code = loop {
        super::errno::set(0);
        let entry = unsafe { readdir(dir) };
        if entry.is_null() {
            break if super::errno::get() == 0 { 0 } else { 5 };
        }
        let child = unsafe { CStr::from_ptr((&raw const (*entry).name).cast()) };
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
    unsafe { closedir(dir) };
    code
}

fn entry_stat(fd: c_int, name: &[c_char; 256]) -> Result<Stat, c_int> {
    let mut info = MaybeUninit::<Stat>::uninit();
    if unsafe { fstatat(fd, name.as_ptr(), info.as_mut_ptr(), AT_SYMLINK_NOFOLLOW) } != 0 {
        Err(super::errno::get())
    } else {
        Ok(unsafe { info.assume_init() })
    }
}

fn regular_or_directory(info: &Stat) -> bool {
    matches!(info.mode & 0o170000, 0o100000 | 0o040000)
}

fn rename_entries(
    root: c_int,
    from: c_int,
    from_name: &[c_char; 256],
    to: c_int,
    to_name: &[c_char; 256],
    data_only: bool,
) -> u8 {
    unsafe extern "C" {
        fn renameat(from: c_int, source: *const c_char, to: c_int, target: *const c_char) -> c_int;
    }
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
    let source_name = unsafe { CStr::from_ptr(from_name.as_ptr()) };
    let target_name = unsafe { CStr::from_ptr(to_name.as_ptr()) };
    let code = protected_tree(from, source_name, 64, &mut guarded);
    if code != 0 {
        return code;
    }
    let code = protected_tree(to, target_name, 64, &mut guarded);
    if code != 0 {
        return code;
    }
    let mut remaining = 65536;
    let device = if data_only {
        let Some(base) = stat(root) else { return 5 };
        let source = unsafe { CStr::from_ptr(from_name.as_ptr()) };
        let code = check_rename_tree(from, source, base.device, 64, &mut remaining);
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
                let target = unsafe { CStr::from_ptr(to_name.as_ptr()) };
                let code = check_rename_tree(to, target, device, 64, &mut remaining);
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
    if unsafe { renameat(from, from_name.as_ptr(), to, to_name.as_ptr()) } == 0 {
        0
    } else {
        5
    }
}

/// The broker provides `count` initialized packet bytes followed by one
/// writable byte for the target path terminator.
pub(super) unsafe fn witchy_broker_rename_rooted(
    root: c_int,
    bytes: *mut u8,
    count: usize,
    data_only: u32,
) -> u8 {
    use super::broker_path::relative_path;
    if !(4..16384).contains(&count) {
        return 2;
    }
    let source_len = unsafe { u16::from_be_bytes([bytes.read(), bytes.add(1).read()]) as usize };
    if source_len == 0 || source_len >= count - 2 {
        return 2;
    }
    let source = unsafe { bytes.add(2) };
    let target = unsafe { source.add(source_len) };
    let target_len = count - 2 - source_len;
    if !relative_path(
        unsafe { core::slice::from_raw_parts(target, target_len) },
        data_only,
    ) {
        return 2;
    }
    let mut to_name = [0; 256];
    let mut from_name = [0; 256];
    unsafe { target.add(target_len).write(0) };
    let to = unsafe {
        witchy_broker_open_output_parent(root, target.cast(), to_name.as_mut_ptr(), data_only != 0)
    };
    let code = if !relative_path(
        unsafe { core::slice::from_raw_parts(source, source_len) },
        data_only,
    ) {
        2
    } else if to < 0 {
        5
    } else {
        unsafe { target.write(0) };
        let from = unsafe {
            witchy_broker_open_output_parent(
                root,
                source.cast(),
                from_name.as_mut_ptr(),
                data_only != 0,
            )
        };
        if from < 0 {
            5
        } else {
            let code = rename_entries(root, from, &from_name, to, &to_name, data_only != 0);
            close(from);
            code
        }
    };
    if to >= 0 {
        close(to);
    }
    code
}

/// The broker provides a mutable packet allocation with space for a terminator
/// and exclusive access to the directory entry budget.
pub(super) unsafe fn witchy_broker_modify_directory(
    root: c_int,
    path: *mut u8,
    len: usize,
    data_only: u32,
    create: bool,
    entries: *mut u64,
) -> u8 {
    if len >= 16383
        || !super::broker_path::relative_path(
            unsafe { core::slice::from_raw_parts(path, len) },
            data_only,
        )
    {
        return 2;
    }
    let mut leaf = [0; 256];
    unsafe { path.add(len).write(0) };
    let parent = unsafe {
        witchy_broker_open_output_parent(root, path.cast(), leaf.as_mut_ptr(), data_only != 0)
    };
    if parent < 0 {
        return 5;
    }
    let code = if create {
        create_directory(parent, &leaf, unsafe { &mut *entries })
    } else {
        remove_file(parent, &leaf)
    };
    close(parent);
    code
}

fn create_directory(parent: c_int, leaf: &[c_char; 256], entries: &mut u64) -> u8 {
    unsafe extern "C" {
        fn mkdirat(parent: c_int, name: *const c_char, mode: u16) -> c_int;
    }
    const EEXIST: c_int = 17;
    if !super::entry_budget::available(*entries) {
        return if entry_stat(parent, leaf).is_ok() {
            6
        } else {
            3
        };
    }
    if unsafe { mkdirat(parent, leaf.as_ptr(), 0o777) } == 0 {
        super::entry_budget::spend(entries);
        0
    } else if super::errno::get() == EEXIST {
        6
    } else {
        5
    }
}

fn remove_file(parent: c_int, leaf: &[c_char; 256]) -> u8 {
    const ENOENT: c_int = 2;
    match entry_stat(parent, leaf) {
        Ok(info) if info.mode & 0o170000 != 0o100000 => return 4,
        Ok(info) if secret_identity(&info) => return 2,
        Ok(_) => {}
        Err(ENOENT) => return 1,
        Err(_) => return 5,
    }
    if unsafe { unlinkat(parent, leaf.as_ptr(), 0) } == 0 {
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
    unsafe extern "C" {
        fn open(path: *const c_char, flags: c_int, ...) -> c_int;
    }
    let root = std::env::temp_dir().join(format!("witchy-protected-tree-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(root.join("nested")).unwrap();
    std::fs::write(root.join("nested/secret"), b"key").unwrap();
    let root_name = CString::new(root.as_os_str().as_bytes()).unwrap();
    let root_fd = unsafe { open(root_name.as_ptr(), DIRECTORY | CLOEXEC) };
    let secret_fd = unsafe { openat(root_fd, c"nested/secret".as_ptr(), CLOEXEC) };
    assert!(root_fd >= 0 && secret_fd >= 0);
    let info = stat(secret_fd).unwrap();
    let device = info.device as u64;
    let inode = info.inode;
    unsafe {
        witchy_broker_secret_objects(&device, &inode, 1);
    }
    let mut remaining = 100;
    assert_eq!(protected_tree(root_fd, c"nested", 64, &mut remaining), 2);
    let mut source = [0 as c_char; 256];
    let mut destination = [0 as c_char; 256];
    for (slot, byte) in source.iter_mut().zip(b"nested\0") {
        *slot = *byte as c_char;
    }
    for (slot, byte) in destination.iter_mut().zip(b"moved\0") {
        *slot = *byte as c_char;
    }
    assert_eq!(
        rename_entries(root_fd, root_fd, &source, root_fd, &destination, false),
        2
    );
    let mut secret_name = [0 as c_char; 256];
    for (slot, byte) in secret_name.iter_mut().zip(b"secret\0") {
        *slot = *byte as c_char;
    }
    let nested_fd = unsafe { openat(root_fd, c"nested".as_ptr(), DIRECTORY | CLOEXEC) };
    assert!(nested_fd >= 0);
    assert_eq!(remove_file(nested_fd, &secret_name), 2);
    unsafe {
        close(nested_fd);
    }
    let mut remaining = 100;
    assert_eq!(protected_tree(root_fd, c"absent", 64, &mut remaining), 0);
    let mut remaining = 0;
    assert_eq!(protected_tree(root_fd, c"nested", 64, &mut remaining), 5);
    unsafe {
        witchy_broker_secret_objects(core::ptr::null(), core::ptr::null(), 0);
        close(secret_fd);
        close(root_fd);
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(test)]
#[test]
fn per_connection_entry_limit_applies_with_unlimited_run_budget() {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    unsafe extern "C" {
        fn open(path: *const c_char, flags: c_int, ...) -> c_int;
    }
    let root = std::env::temp_dir().join(format!("witchy-entry-budget-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let name = CString::new(root.as_os_str().as_bytes()).unwrap();
    let fd = unsafe { open(name.as_ptr(), DIRECTORY | CLOEXEC) };
    assert!(fd >= 0);
    let mut first = [0 as c_char; 256];
    let mut second = [0 as c_char; 256];
    for (slot, byte) in first.iter_mut().zip(b"first\0") {
        *slot = *byte as c_char;
    }
    for (slot, byte) in second.iter_mut().zip(b"second\0") {
        *slot = *byte as c_char;
    }
    super::entry_budget::set_connection(1);
    let mut run = u64::MAX;
    assert_eq!(create_directory(fd, &first, &mut run), 0);
    assert_eq!(create_directory(fd, &second, &mut run), 3);
    assert!(!root.join("second").exists());
    super::entry_budget::set_connection(u64::MAX);
    unsafe {
        close(fd);
    }
    std::fs::remove_dir_all(root).unwrap();
}

/// The caller supplies a live C filename and 16 KiB of exclusive scratch space.
unsafe fn append_output(parent: c_int, name: *const c_char, staged: c_int, scratch: *mut u8) -> u8 {
    // O_WRONLY | O_APPEND | O_NOFOLLOW | O_NONBLOCK | O_CLOEXEC.
    #[cfg(target_os = "macos")]
    const APPEND_FLAGS: c_int = 0x0100_010d;
    #[cfg(target_os = "linux")]
    const APPEND_FLAGS: c_int = CLOEXEC | NOFOLLOW_ANY | NONBLOCK | 0o2001;
    let fd = unsafe { open_checked(parent, name, APPEND_FLAGS) };
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
        Some(_) => unsafe { append_bytes(fd, staged, scratch) },
        None => 5,
    };
    if close(fd) != 0 && code == 0 {
        5
    } else {
        code
    }
}

unsafe fn append_bytes(target: c_int, staged: c_int, scratch: *mut u8) -> u8 {
    unsafe extern "C" {
        safe fn lseek(fd: c_int, offset: i64, whence: c_int) -> i64;
        fn read(fd: c_int, bytes: *mut c_void, len: usize) -> isize;
        fn write(fd: c_int, bytes: *const c_void, len: usize) -> isize;
        safe fn fsync(fd: c_int) -> c_int;
    }
    const EINTR: c_int = 4;
    if lseek(staged, 0, 0) < 0 {
        return 5;
    }
    loop {
        let count = unsafe { read(staged, scratch.cast(), 16384) };
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
            let written = unsafe { write(target, scratch.add(offset).cast(), count - offset) };
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

/// Publish a staged file and consume its descriptor. Names and scratch
/// storage belong to the broker for the duration of this call.
unsafe fn publish(
    staging: c_int,
    temp: *const c_char,
    parent: c_int,
    name: *const c_char,
    fd: c_int,
    scratch: *mut u8,
    mode: u32,
    entries: *mut u64,
    mut ok: bool,
) -> u8 {
    unsafe extern "C" {
        #[cfg(target_os = "macos")]
        fn renameatx_np(
            from: c_int,
            source: *const c_char,
            to: c_int,
            target: *const c_char,
            flags: u32,
        ) -> c_int;
        fn renameat(from: c_int, source: *const c_char, to: c_int, target: *const c_char) -> c_int;
        #[cfg(target_os = "linux")]
        fn syscall(number: core::ffi::c_long, ...) -> core::ffi::c_long;
    }
    const SWAP: u32 = 2;
    #[cfg(target_os = "macos")]
    const EXCL: u32 = 4;
    #[cfg(target_os = "linux")]
    const EXCL: u32 = 1;
    const EXISTS: c_int = 17;
    const MISSING: c_int = 2;
    #[cfg(target_os = "macos")]
    let rename = |flags| unsafe { renameatx_np(staging, temp, parent, name, flags) == 0 };
    #[cfg(target_os = "linux")]
    let rename = |flags: u32| unsafe { syscall(276, staging, temp, parent, name, flags) == 0 };
    let info = || {
        let mut info = MaybeUninit::<Stat>::uninit();
        if unsafe { fstatat(parent, name, info.as_mut_ptr(), AT_SYMLINK_NOFOLLOW) } == 0 {
            Ok(unsafe { info.assume_init() })
        } else {
            Err(super::errno::get())
        }
    };
    let regular = |info: &Stat| info.mode & 0o170000 == 0o100000;
    let append = || unsafe { append_output(parent, name, fd, scratch) };
    let entries = unsafe { &mut *entries };
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
            if admissible && unsafe { renameat(staging, temp, parent, name) } == 0 {
                return 0;
            }
        }
    }
    unsafe { unlinkat(staging, temp, 0) };
    code
}

/// Receives one staged write. Scratch is writable for 16 KiB for each received chunk.
unsafe fn witchy_broker_commit_output(
    channel: c_int,
    staging: c_int,
    parent: c_int,
    name: *const c_char,
    mut length: usize,
    scratch: *mut u8,
    mode: u32,
    entries: *mut u64,
) -> u8 {
    unsafe extern "C" {
        fn snprintf(out: *mut c_char, len: usize, format: *const c_char, ...) -> c_int;
        safe fn getpid() -> c_int;
        #[cfg(target_os = "macos")]
        safe fn arc4random() -> u32;
        #[cfg(target_os = "linux")]
        fn getrandom(out: *mut c_void, len: usize, flags: u32) -> isize;
        fn write(fd: c_int, bytes: *const c_void, len: usize) -> isize;
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
        let mut value = 0u32;
        let count = unsafe { getrandom((&raw mut value).cast(), size_of::<u32>(), 0) };
        if count != size_of::<u32>() as isize {
            return None;
        }
        Some(value)
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
    let mut temp = [0; 64];
    let named = unsafe {
        if staging != parent {
            snprintf(
                temp.as_mut_ptr(),
                temp.len(),
                c"%d-%u".as_ptr(),
                getpid(),
                a,
            )
        } else {
            snprintf(
                temp.as_mut_ptr(),
                temp.len(),
                c".witchy-%08x%08x%08x".as_ptr(),
                a,
                b,
                c,
            )
        }
    };
    let fd = if named > 0 && (named as usize) < temp.len() {
        unsafe { openat(staging, temp.as_ptr(), FLAGS, 0o600 as c_int) }
    } else {
        -1
    };
    let mut ok = fd >= 0;
    while length != 0 {
        let chunk = length.min(16384);
        if !unsafe {
            super::broker_transport::witchy_broker_transfer(
                channel,
                scratch,
                core::ptr::null(),
                chunk,
            )
        } {
            ok = false;
            break;
        }
        if ok {
            let mut offset = 0;
            while offset < chunk {
                let count = unsafe { write(fd, scratch.add(offset).cast(), chunk - offset) };
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
    if fd < 0 {
        return 5;
    }
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
    unsafe {
        publish(
            staging,
            temp.as_ptr(),
            parent,
            name,
            fd,
            scratch,
            mode,
            entries,
            ok,
        )
    }
}

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

/// On success, the broker receives a malloc-owned listing and cursor zero.
unsafe fn witchy_broker_list_directory(base: c_int, listing: *mut Listing, mode: u32) -> bool {
    const MAX_BYTES: usize = 1024 * 1024;
    const MAX_ENTRIES: usize = 65536;
    let fd = unsafe { openat(base, c".".as_ptr(), DIRECTORY | CLOEXEC) };
    if fd < 0 {
        return false;
    }
    let dir = unsafe { fdopendir(fd) };
    if dir.is_null() {
        close(fd);
        return false;
    }
    let names = unsafe { malloc(MAX_ENTRIES * size_of::<*mut u8>()) }.cast::<*mut u8>();
    if names.is_null() {
        unsafe { closedir(dir) };
        return false;
    }
    let (mut count, mut length, mut visited) = (0, 0, 0);
    let ok = loop {
        super::errno::set(0);
        let entry = unsafe { readdir(dir) };
        if entry.is_null() {
            break super::errno::get() == 0;
        }
        let name = unsafe { CStr::from_ptr((&raw const (*entry).name).cast()) };
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
        let directory = match unsafe { (*entry).kind } {
            4 => true, // DT_DIR
            0 => {
                let mut info = MaybeUninit::<Stat>::uninit();
                if unsafe { fstatat(fd, name.as_ptr(), info.as_mut_ptr(), AT_SYMLINK_NOFOLLOW) }
                    != 0
                {
                    break false;
                }
                unsafe { info.assume_init() }.mode & 0o170000 == 0o040000
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
    unsafe { closedir(dir) };
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
        unsafe {
            listing.write(Listing {
                bytes,
                length,
                cursor: 0,
            })
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
/// closes any descriptors opened before a later check failed.
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
        #[cfg(target_os = "macos")]
        fn open(path: *const c_char, flags: c_int, ...) -> c_int;
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
            #[cfg(target_os = "macos")]
            {
                unsafe { open(read, FLAGS) }
            }
            #[cfg(target_os = "linux")]
            {
                unsafe { super::linux_stat::openat_no_symlinks(AT_FDCWD, read, FLAGS) }
            }
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
            #[cfg(target_os = "macos")]
            {
                unsafe { open(write, FLAGS) }
            }
            #[cfg(target_os = "linux")]
            {
                unsafe { super::linux_stat::openat_no_symlinks(AT_FDCWD, write, FLAGS) }
            }
        };
        if opened.write < 0 {
            return false;
        }
    }
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
        opened.output = if output_parent >= 0 {
            let name = unsafe { CStr::from_ptr(output) }.to_bytes();
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
            unsafe {
                witchy_broker_open_output_parent(AT_FDCWD, output, opened.name.as_mut_ptr(), false)
            }
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
    unsafe { roots.write(opened) };
    true
}

#[repr(C)]
pub(super) struct Output {
    pub(super) parent: c_int,
    pub(super) mode: u32,
    pub(super) name: [c_char; 256],
}

/// All state pointers belong to the single broker loop. Scratch is 16 KiB.
pub(super) unsafe fn witchy_broker_output_request(
    channel: c_int,
    operation: u8,
    slot: usize,
    count: usize,
    scratch: *mut u8,
    outputs: *mut [Output; 16],
    root_output: *mut c_int,
    root_name: *const c_char,
    root_exclusive: bool,
    write_root: c_int,
    staging: c_int,
    files: *const [c_int; 16],
    kinds: *const [u32; 16],
    data_only: u32,
    entries: *mut u64,
) -> bool {
    use super::broker_transport::{witchy_broker_reply, witchy_broker_transfer};
    use core::ptr::null;
    let outputs = unsafe { &mut *outputs };
    if matches!(operation, 18 | 20 | 22) {
        let Some(output) = outputs.get_mut(slot) else {
            return false;
        };
        if output.parent >= 0
            || !(2..16384).contains(&count)
            || !unsafe { witchy_broker_transfer(channel, scratch, null(), count) }
        {
            return false;
        }
        let source = usize::from(unsafe { scratch.read() });
        let base = if source == 16 {
            write_root
        } else if unsafe { &*kinds }
            .get(source)
            .is_some_and(|kind| kind & 4 != 0)
        {
            unsafe { &*files }.get(source).copied().unwrap_or(-1)
        } else {
            -1
        };
        let path = unsafe { scratch.add(1) };
        let mut failure = 2;
        if base >= 0
            && super::broker_path::relative_path(
                unsafe { core::slice::from_raw_parts(path, count - 1) },
                data_only,
            )
        {
            unsafe { scratch.add(count).write(0) };
            output.parent = unsafe {
                witchy_broker_open_output_parent(
                    base,
                    path.cast(),
                    output.name.as_mut_ptr(),
                    data_only != 0,
                )
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
        (
            unsafe { &mut *root_output },
            root_name,
            u32::from(root_exclusive),
        )
    } else if let Some(output) = outputs.get_mut(slot) {
        (&mut output.parent, output.name.as_ptr(), output.mode)
    } else {
        return false;
    };
    let commit = operation == 16;
    if *parent < 0
        || count != if commit { 4 } else { 0 }
        || !unsafe { witchy_broker_transfer(channel, scratch, null(), count) }
    {
        return false;
    }
    let length = if commit {
        u32::from_be_bytes(unsafe { scratch.cast::<[u8; 4]>().read() }) as usize
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
        unsafe {
            witchy_broker_commit_output(
                channel, temporary, *parent, name, length, scratch, mode, entries,
            )
        }
    } else {
        0
    };
    close(*parent);
    *parent = -1;
    witchy_broker_reply(channel, 0, usize::from(code))
}

/// The relay provides initialized slot tables and exclusive scratch space.
pub(super) unsafe fn witchy_broker_file_request(
    channel: c_int,
    op: u8,
    slot: usize,
    count: usize,
    bytes: *mut u8,
    root: c_int,
    write_root: c_int,
    combined: bool,
    mode: u32,
    files: *mut [c_int; 16],
    kinds: *mut [u32; 16],
    listings: *mut [Listing; 16],
) -> bool {
    use super::broker_transport::{
        witchy_broker_reply as reply, witchy_broker_transfer as transfer,
    };
    use core::ptr::{null, null_mut};
    unsafe extern "C" {
        fn read(fd: c_int, bytes: *mut c_void, len: usize) -> isize;
    }
    let (files, kinds, listings) = unsafe { (&mut *files, &mut *kinds, &mut *listings) };
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
            if !unsafe { transfer(channel, bytes, null(), count) } {
                return false;
            }
            let source = usize::from(unsafe { bytes.read() });
            let base = if source == 16 {
                root
            } else if kinds.get(source).is_some_and(|k| matches!(k, 2 | 6)) {
                files[source]
            } else {
                -1
            };
            let ok = base >= 0
                && unsafe { witchy_broker_list_directory(base, &mut listings[slot], mode) };
            if ok {
                kinds[slot] = 3;
            }
            (ok, 0)
        }
        11 if kind == 3 => {
            let listing = &mut listings[slot];
            payload = count.min(listing.length - listing.cursor);
            if payload != 0 {
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        listing.bytes.add(listing.cursor),
                        bytes,
                        payload,
                    )
                };
            }
            listing.cursor += payload;
            (true, 0)
        }
        12 if kind == 3 && count == 0 => {
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
            if count == 0 || !unsafe { transfer(channel, bytes, null(), count) } {
                return false;
            }
            let source = usize::from(unsafe { bytes.read() });
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
            let path = unsafe { bytes.add(1) };
            let opened = if count < 16384
                && super::broker_path::relative_path(
                    unsafe { core::slice::from_raw_parts(path, count - 1) },
                    mode,
                ) {
                unsafe {
                    bytes.add(count).write(0);
                    witchy_broker_open_rooted(
                        base,
                        path.cast(),
                        &mut opened_kind,
                        &mut code,
                        mode != 0,
                    )
                }
            } else {
                code = 2;
                -1
            };
            if op == 13 {
                if opened >= 0 {
                    close(opened);
                }
                unsafe { bytes.write(opened_kind as u8) };
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
            let got = unsafe { read(fd, bytes.cast(), count) };
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
    ) && (payload == 0 || unsafe { transfer(channel, null_mut(), bytes, payload) })
}

#[cfg(test)]
#[test]
fn staged_publication_replaces_file_and_refuses_hard_links() {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    unsafe extern "C" {
        fn open(path: *const c_char, flags: c_int, ...) -> c_int;
        fn write(fd: c_int, bytes: *const c_void, count: usize) -> isize;
    }
    let root = std::env::temp_dir().join(format!("witchy-publish-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let path = CString::new(root.as_os_str().as_bytes()).unwrap();
    let root_fd = unsafe { open(path.as_ptr(), DIRECTORY | CLOEXEC) };
    assert!(root_fd >= 0);
    let staging = witchy_broker_open_staging(root_fd);
    assert!(staging >= 0);
    let mut scratch = [0u8; 16384];
    let mut entries = u64::MAX;
    let run = |temporary: &CStr, target: &CStr, entries: &mut u64, scratch: &mut [u8]| {
        #[cfg(target_os = "macos")]
        const NEW: c_int = 0x0100_0b02;
        #[cfg(target_os = "linux")]
        const NEW: c_int = CLOEXEC | NOFOLLOW_ANY | 0o302;
        let fd = unsafe { openat(staging, temporary.as_ptr(), NEW, 0o600i32) };
        assert!(fd >= 0);
        assert_eq!(unsafe { write(fd, b"new".as_ptr().cast(), 3) }, 3);
        unsafe {
            publish(
                staging,
                temporary.as_ptr(),
                root_fd,
                target.as_ptr(),
                fd,
                scratch.as_mut_ptr(),
                0,
                entries,
                true,
            )
        }
    };
    std::fs::write(root.join("target"), b"old").unwrap();
    assert_eq!(run(c"1-1", c"target", &mut entries, &mut scratch), 0);
    assert_eq!(std::fs::read(root.join("target")).unwrap(), b"new");
    std::fs::hard_link(root.join("target"), root.join("alias")).unwrap();
    assert_eq!(run(c"1-2", c"target", &mut entries, &mut scratch), 2);
    assert_eq!(std::fs::read(root.join("alias")).unwrap(), b"new");
    unsafe {
        close(staging);
        close(root_fd);
    }
    std::fs::remove_dir_all(root).unwrap();
}
