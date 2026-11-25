//! Linux Exec's privilege reduction, applied in the child before execve.
use core::ffi::{c_int, c_long};

#[repr(C)]
struct Header {
    version: u32,
    pid: c_int,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct Data {
    effective: u32,
    permitted: u32,
    inheritable: u32,
}

unsafe extern "C" {
    fn syscall(number: c_long, ...) -> c_long;
    fn prctl(option: c_int, ...) -> c_int;
}

/// Drop the bounding and ambient sets, lock root-related securebits when
/// capabilities are present, and clear the process capability sets.
pub(super) fn drop_capabilities() -> bool {
    const VERSION_3: u32 = 0x2008_0522;
    const SECUREBITS: c_int = 0xef;
    let header = Header {
        version: VERSION_3,
        pid: 0,
    };
    let mut data = [Data {
        effective: 0,
        permitted: 0,
        inheritable: 0,
    }; 2];
    if unsafe { syscall(90, &raw const header, data.as_mut_ptr()) } != 0 {
        return false;
    }
    let any = data.iter().any(|d| d.effective != 0 || d.permitted != 0);
    if any {
        if unsafe { prctl(28, SECUREBITS, 0, 0, 0) } != 0 {
            return false;
        }
        for capability in 0..=63 {
            if unsafe { prctl(24, capability, 0, 0, 0) } != 0
                && super::errno::get() != 22
            {
                return false;
            }
        }
    }
    if unsafe { prctl(47, 4, 0, 0, 0) } != 0 && super::errno::get() != 22 {
        return false;
    }
    data.fill(Data {
        effective: 0,
        permitted: 0,
        inheritable: 0,
    });
    // Inheritable capabilities can be present even when both other sets are
    // empty; explicitly clear them in that case too.
    (unsafe { syscall(91, &raw const header, data.as_ptr()) }) == 0
}

#[cfg(test)]
#[test]
fn child_loses_all_capabilities() {
    unsafe extern "C" {
        fn fork() -> c_int;
        safe fn _exit(code: c_int) -> !;
    }
    let child = unsafe { fork() };
    assert!(child >= 0);
    if child == 0 {
        if !drop_capabilities() {
            _exit(1)
        }
        let header = Header {
            version: 0x2008_0522,
            pid: 0,
        };
        let mut data = [Data {
            effective: 1,
            permitted: 1,
            inheritable: 1,
        }; 2];
        if unsafe { syscall(90, &raw const header, data.as_mut_ptr()) } != 0
            || data
                .iter()
                .any(|d| d.effective != 0 || d.permitted != 0 || d.inheritable != 0)
        {
            _exit(2)
        }
        _exit(0)
    }
    let mut status = 0;
    assert_eq!(super::sys::waitpid(child, Some(&mut status), 0), child);
    assert_eq!(status, 0);
}

#[cfg(test)]
#[test]
fn inheritable_set_is_cleared_when_permitted_set_is_empty() {
    unsafe extern "C" {
        fn fork() -> c_int;
        safe fn _exit(code: c_int) -> !;
    }
    let child = unsafe { fork() };
    assert!(child >= 0);
    if child == 0 {
        let header = Header {
            version: 0x2008_0522,
            pid: 0,
        };
        let mut data = [Data {
            effective: 0,
            permitted: 0,
            inheritable: 0,
        }; 2];
        if unsafe { syscall(90, &raw const header, data.as_mut_ptr()) } != 0 {
            _exit(1)
        }
        let permitted = data[0].permitted;
        if permitted == 0 {
            _exit(0)
        }
        data[0].inheritable = permitted & permitted.wrapping_neg();
        data[0].effective = 0;
        data[0].permitted = 0;
        data[1].effective = 0;
        data[1].permitted = 0;
        data[1].inheritable = 0;
        if unsafe { syscall(91, &raw const header, data.as_ptr()) } != 0 {
            _exit(2)
        }
        if !drop_capabilities() {
            _exit(3)
        }
        if unsafe { syscall(90, &raw const header, data.as_mut_ptr()) } != 0
            || data.iter().any(|d| d.inheritable != 0)
        {
            _exit(4)
        }
        _exit(0)
    }
    let mut status = 0;
    assert_eq!(super::sys::waitpid(child, Some(&mut status), 0), child);
    assert_eq!(status, 0);
}
