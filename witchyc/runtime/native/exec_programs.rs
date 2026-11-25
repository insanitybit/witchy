//! Open and retain the exact executable identities granted to the broker.
#[cfg(target_os = "linux")]
use super::linux_stat::stat;
#[cfg(target_os = "macos")]
use super::seatbelt::stat;
use core::ffi::{CStr, c_char, c_int};

unsafe extern "C" {
    #[cfg(target_os = "macos")]
    fn openat(fd: c_int, name: *const c_char, flags: c_int, ...) -> c_int;
    fn fcntl(fd: c_int, command: c_int, ...) -> c_int;
    safe fn close(fd: c_int) -> c_int;
    fn witchy_exec_plain(name: *const c_char, length: usize) -> bool;
}

const PROGRAM_LIMIT: i64 = 256 << 20;
const BUDGET_LIMIT: u64 = 256 << 20;
const MAX_PROGRAMS: usize = 16;
#[cfg(target_os = "macos")]
const OPEN_FLAGS: c_int = 0x0100_0000 | 0x2000_0000 | 0x4;
#[cfg(target_os = "linux")]
const OPEN_FLAGS: c_int = 0o2000000 | 0o100000 | 0o4000;
#[cfg(target_os = "macos")]
const DUP_CLOEXEC: c_int = 67;
#[cfg(target_os = "linux")]
const DUP_CLOEXEC: c_int = 1030;

#[derive(Copy, Clone)]
pub(super) struct Program {
    pub(super) name: [u8; 256],
    pub(super) fd: c_int,
    device: u64,
    inode: u64,
}
impl Program {
    const EMPTY: Self = Self {
        name: [0; 256],
        fd: -1,
        device: 0,
        inode: 0,
    };
}

struct Programs {
    entries: [Program; MAX_PROGRAMS],
    count: usize,
    configured: bool,
    granted: bool,
    own_budget: u64,
    shared_budget: *mut u64,
}

impl Programs {
    const EMPTY: Self = Self {
        entries: [Program::EMPTY; MAX_PROGRAMS],
        count: 0,
        configured: false,
        granted: false,
        own_budget: 0,
        shared_budget: core::ptr::null_mut(),
    };
    fn guarded(&self) -> bool {
        self.granted || self.count != 0
    }
    fn protected(&self, device: u64, inode: u64) -> bool {
        (self.granted && !self.configured)
            || self.entries[..self.count]
                .iter()
                .any(|entry| entry.fd != -1 && entry.device == device && entry.inode == inode)
    }
}

// Initialization and broker requests are serialized in one process/thread.
static mut PROGRAMS: Programs = Programs::EMPTY;

pub(super) fn configured() -> bool {
    unsafe { (*(&raw const PROGRAMS)).configured }
}

/// The Linux broker retains executable identities after the runner inherits
/// the open descriptors, but must not retain the descriptors themselves.
#[cfg(target_os = "linux")]
pub(super) fn close_broker_descriptors() {
    let programs = unsafe { &mut *(&raw mut PROGRAMS) };
    for entry in &mut programs.entries[..programs.count] {
        if entry.fd >= 0 {
            close(entry.fd);
            entry.fd = -2;
        }
    }
}

pub(super) fn program(index: usize) -> Option<Program> {
    let programs = unsafe { &*(&raw const PROGRAMS) };
    programs
        .entries
        .get(index)
        .copied()
        .filter(|program| index < programs.count && program.fd >= 0)
}

pub(super) fn budget() -> u64 {
    let programs = unsafe { &*(&raw const PROGRAMS) };
    if programs.shared_budget.is_null() {
        programs.own_budget
    } else {
        unsafe { *programs.shared_budget }
    }
}

pub(super) fn spend(amount: u64) {
    let programs = unsafe { &mut *(&raw mut PROGRAMS) };
    if programs.shared_budget.is_null() {
        programs.own_budget -= amount;
    } else {
        unsafe {
            *programs.shared_budget -= amount;
        }
    }
}

#[unsafe(no_mangle)]
extern "C" fn witchy_exec_granted() {
    unsafe {
        (*(&raw mut PROGRAMS)).granted = true;
    }
}

#[unsafe(no_mangle)]
extern "C" fn witchy_exec_guarded() -> bool {
    unsafe { (&*(&raw const PROGRAMS)).guarded() }
}

#[unsafe(no_mangle)]
extern "C" fn witchy_exec_program(device: u64, inode: u64) -> bool {
    unsafe { (&*(&raw const PROGRAMS)).protected(device, inode) }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_exec_share_budget(shared: *mut u64) {
    if shared.is_null() {
        return;
    }
    let programs = unsafe { &mut *(&raw mut PROGRAMS) };
    let value = if programs.shared_budget.is_null() {
        programs.own_budget
    } else {
        unsafe { *programs.shared_budget }
    };
    unsafe {
        *shared = value;
    }
    programs.shared_budget = shared;
}

#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_exec_descriptors(out: *mut c_int, capacity: u32) -> u32 {
    if out.is_null() {
        return 0;
    }
    let programs = unsafe { &*(&raw const PROGRAMS) };
    let mut count = 0;
    for entry in &programs.entries[..programs.count] {
        if entry.fd >= 0 && count < capacity {
            unsafe {
                out.add(count as usize).write(entry.fd);
            }
            count += 1;
        }
    }
    count
}

/// `directory` is owned on success; names point to live C strings.
#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_exec_setup(
    directory: c_int,
    names: *const *const c_char,
    count: u32,
    limit: u64,
) -> bool {
    if directory < 0
        || count as usize > MAX_PROGRAMS
        || limit > BUDGET_LIMIT
        || (count != 0 && names.is_null())
    {
        return false;
    }
    let programs = unsafe { &mut *(&raw mut PROGRAMS) };
    if programs.shared_budget.is_null() {
        programs.own_budget = limit;
    } else {
        unsafe {
            *programs.shared_budget = limit;
        }
    }
    for index in 0..count as usize {
        let name = unsafe { *names.add(index) };
        if name.is_null() {
            return false;
        }
        let bytes = unsafe { CStr::from_ptr(name).to_bytes() };
        if !unsafe { witchy_exec_plain(name, bytes.len()) } {
            return false;
        }
        let entry = &mut programs.entries[index];
        entry.name[..bytes.len()].copy_from_slice(bytes);
        entry.name[bytes.len()] = 0;
        #[cfg(target_os = "macos")]
        let mut fd = unsafe { openat(directory, name, OPEN_FLAGS) };
        #[cfg(target_os = "linux")]
        let mut fd = unsafe { super::linux_stat::openat_no_symlinks(directory, name, OPEN_FLAGS) };
        if (0..=2).contains(&fd) {
            let moved = unsafe { fcntl(fd, DUP_CLOEXEC, 3 as c_int) };
            close(fd);
            fd = moved;
        }
        let info = if fd >= 0 { stat(fd) } else { None };
        let accepted = info.as_ref().is_some_and(|info| {
            info.mode & 0o170000 == 0o100000 && info.mode & 0o111 != 0 && info.size <= PROGRAM_LIMIT
        });
        if fd >= 0 && !accepted {
            close(fd);
            fd = -1;
        }
        entry.fd = fd;
        entry.device = info
            .as_ref()
            .filter(|_| accepted)
            .map_or(0, |info| info.device as u64);
        entry.inode = info.filter(|_| accepted).map_or(0, |info| info.inode);
    }
    programs.count = count as usize;
    close(directory);
    programs.configured = true;
    true
}

#[cfg(test)]
#[test]
fn unconfigured_grant_fails_closed() {
    let mut programs = Programs::EMPTY;
    assert!(!programs.guarded());
    programs.granted = true;
    assert!(programs.guarded());
    assert!(programs.protected(1, 2));
    programs.configured = true;
    assert!(!programs.protected(1, 2));
}

#[cfg(test)]
#[test]
fn setup_records_opened_executable_identity() {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::PermissionsExt;
    unsafe extern "C" {
        fn open(path: *const c_char, flags: c_int, ...) -> c_int;
    }
    let root = std::env::temp_dir().join(format!("witchy-exec-programs-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let program = root.join("helper");
    std::fs::write(&program, b"test").unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::os::unix::fs::symlink("helper", root.join("link")).unwrap();
    let root_name = CString::new(root.as_os_str().as_bytes()).unwrap();
    let name = CString::new("helper").unwrap();
    let link = CString::new("link").unwrap();
    #[cfg(target_os = "macos")]
    const DIR_FLAGS: c_int = 0x0010_0000 | 0x0100_0000;
    #[cfg(target_os = "linux")]
    const DIR_FLAGS: c_int = 0o40000 | 0o2000000;
    let fd = unsafe { open(root_name.as_ptr(), DIR_FLAGS) };
    assert!(fd >= 0);
    assert!(unsafe { witchy_exec_setup(fd, [name.as_ptr(), link.as_ptr()].as_ptr(), 2, 42) });
    assert!(self::program(1).is_none());
    let mut descriptors = [-1];
    assert_eq!(
        unsafe { witchy_exec_descriptors(descriptors.as_mut_ptr(), 1) },
        1
    );
    let info = stat(descriptors[0]).unwrap();
    assert!(witchy_exec_program(info.device as u64, info.inode));
    assert!(!witchy_exec_program(
        info.device as u64,
        info.inode.wrapping_add(1)
    ));
    unsafe {
        close(descriptors[0]);
    }
    std::fs::remove_dir_all(root).unwrap();
}
