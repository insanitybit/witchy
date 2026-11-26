//! Open and retain the exact executable identities granted to the broker.
#[cfg(target_os = "linux")]
use super::linux_stat::stat;
#[cfg(target_os = "macos")]
use super::seatbelt::stat;
use super::global::State;
use super::sys;
use core::ffi::{CStr, c_char, c_int};

unsafe extern "C" {
    safe fn close(fd: c_int) -> c_int;
}

const PROGRAM_LIMIT: i64 = 256 << 20;
const BUDGET_LIMIT: u64 = 256 << 20;
const MAX_PROGRAMS: usize = 16;
#[cfg(target_os = "macos")]
const OPEN_FLAGS: c_int = 0x0100_0000 | 0x2000_0000 | 0x4;
#[cfg(target_os = "linux")]
const OPEN_FLAGS: c_int = 0o2000000 | 0o100000 | 0o4000;

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
// Every use below is short and calls nothing that comes back here; none forks.
static PROGRAMS: State<Programs> = State::new(Programs::EMPTY);

pub(super) fn configured() -> bool {
    PROGRAMS.with(|programs| programs.configured)
}

/// The Linux broker retains executable identities after the runner inherits
/// the open descriptors, but must not retain the descriptors themselves.
#[cfg(target_os = "linux")]
pub(super) fn close_broker_descriptors() {
    PROGRAMS.with(|programs| {
        for entry in &mut programs.entries[..programs.count] {
            if entry.fd >= 0 {
                close(entry.fd);
                entry.fd = -2;
            }
        }
    })
}

pub(super) fn program(index: usize) -> Option<Program> {
    PROGRAMS.with(|programs| {
        programs
            .entries
            .get(index)
            .copied()
            .filter(|program| index < programs.count && program.fd >= 0)
    })
}

pub(super) fn budget() -> u64 {
    PROGRAMS.with(|programs| {
        if programs.shared_budget.is_null() {
            programs.own_budget
        } else {
            // SAFETY: `witchy_exec_share_budget` was given a mapping that stays readable.
            unsafe { *programs.shared_budget }
        }
    })
}

pub(super) fn spend(amount: u64) {
    PROGRAMS.with(|programs| {
        if programs.shared_budget.is_null() {
            programs.own_budget -= amount;
        } else {
            // SAFETY: `witchy_exec_share_budget` was given a mapping that stays writable.
            unsafe {
                *programs.shared_budget -= amount;
            }
        }
    })
}

#[unsafe(no_mangle)]
extern "C" fn witchy_exec_granted() {
    PROGRAMS.with(|programs| programs.granted = true)
}

#[unsafe(no_mangle)]
extern "C" fn witchy_exec_guarded() -> bool {
    PROGRAMS.with(|programs| programs.guarded())
}

#[unsafe(no_mangle)]
extern "C" fn witchy_exec_program(device: u64, inode: u64) -> bool {
    PROGRAMS.with(|programs| programs.protected(device, inode))
}

/// `shared` is null or stays mapped and writable for the rest of the process.
pub(super) unsafe fn witchy_exec_share_budget(shared: *mut u64) {
    if shared.is_null() {
        return;
    }
    PROGRAMS.with(|programs| {
        let value = if programs.shared_budget.is_null() {
            programs.own_budget
        } else {
            unsafe { *programs.shared_budget }
        };
        unsafe {
            *shared = value;
        }
        programs.shared_budget = shared;
    })
}

/// Writes the open program descriptors to the front of `out`; returns how many.
pub(super) fn witchy_exec_descriptors(out: &mut [c_int]) -> usize {
    PROGRAMS.with(|programs| {
        let mut count = 0;
        for entry in &programs.entries[..programs.count] {
            if entry.fd >= 0
                && let Some(slot) = out.get_mut(count)
            {
                *slot = entry.fd;
                count += 1;
            }
        }
        count
    })
}

/// The launcher's and the decoder fuzz driver's entry (the driver,
/// `tests/fixtures/native-sandbox/exec-fuzz.rs`, links the archive by this
/// symbol). `names` holds `count` pointers, each null or a live C string.
#[unsafe(export_name = "witchy_exec_setup")]
pub(super) unsafe extern "C" fn setup_from_table(
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
    let mut table = [c""; MAX_PROGRAMS];
    for (index, slot) in table[..count as usize].iter_mut().enumerate() {
        // SAFETY: the caller's table has `count` entries; a non-null entry is a live C string.
        let name = unsafe { *names.add(index) };
        if name.is_null() {
            return false;
        }
        *slot = unsafe { CStr::from_ptr(name) };
    }
    witchy_exec_setup(directory, &table[..count as usize], limit)
}

/// `directory` is owned on success.
pub(super) fn witchy_exec_setup(directory: c_int, names: &[&CStr], limit: u64) -> bool {
    if directory < 0 || names.len() > MAX_PROGRAMS || limit > BUDGET_LIMIT {
        return false;
    }
    // The opens, the stat and the closes below call nothing that uses `PROGRAMS`.
    PROGRAMS.with(|programs| {
        if programs.shared_budget.is_null() {
            programs.own_budget = limit;
        } else {
            // SAFETY: `witchy_exec_share_budget` was given a mapping that stays writable.
            unsafe {
                *programs.shared_budget = limit;
            }
        }
        for (index, name) in names.iter().enumerate() {
            let bytes = name.to_bytes();
            if !super::exec_environment::witchy_exec_plain(bytes) {
                return false;
            }
            let entry = &mut programs.entries[index];
            entry.name[..bytes.len()].copy_from_slice(bytes);
            entry.name[bytes.len()] = 0;
            #[cfg(target_os = "macos")]
            let mut fd = sys::openat(directory, name, OPEN_FLAGS);
            #[cfg(target_os = "linux")]
            let mut fd = unsafe {
                super::linux_stat::openat_no_symlinks(directory, name.as_ptr(), OPEN_FLAGS)
            };
            if (0..=2).contains(&fd) {
                let moved = sys::duplicate_close_on_exec(fd, 3);
                close(fd);
                fd = moved;
            }
            let info = if fd >= 0 { stat(fd) } else { None };
            let accepted = info.as_ref().is_some_and(|info| {
                info.mode & 0o170000 == 0o100000
                    && info.mode & 0o111 != 0
                    && info.size <= PROGRAM_LIMIT
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
        programs.count = names.len();
        close(directory);
        programs.configured = true;
        true
    })
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
    let fd = sys::open(&root_name, DIR_FLAGS);
    assert!(fd >= 0);
    assert!(witchy_exec_setup(fd, &[name.as_c_str(), link.as_c_str()], 42));
    assert!(self::program(1).is_none());
    let mut descriptors = [-1];
    assert_eq!(witchy_exec_descriptors(&mut descriptors), 1);
    let info = stat(descriptors[0]).unwrap();
    assert!(witchy_exec_program(info.device as u64, info.inode));
    assert!(!witchy_exec_program(
        info.device as u64,
        info.inode.wrapping_add(1)
    ));
    close(descriptors[0]);
    std::fs::remove_dir_all(root).unwrap();
}
