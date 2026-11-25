//! macOS process cleanup while a child still owns its PID and session ID.
use core::ffi::{c_int, c_void};
use core::mem::{MaybeUninit, size_of};

#[repr(C)]
struct ProcessInfo {
    flags: u32,
    status: u32,
    exit_status: u32,
    pid: u32,
    parent: u32,
    ids_and_reserved: [u32; 7],
    command: [u8; 16],
    name: [u8; 32],
    descriptors: u32,
    group: u32,
    jobs: u32,
    terminal: u32,
    terminal_group: u32,
    nice: i32,
    start_seconds: u64,
    start_microseconds: u64,
}

unsafe extern "C" {
    fn proc_pidinfo(pid: c_int, flavor: c_int, arg: u64, buffer: *mut c_void, size: c_int)
    -> c_int;
    fn proc_listchildpids(pid: c_int, buffer: *mut c_void, size: c_int) -> c_int;
    fn proc_listallpids(buffer: *mut c_void, size: c_int) -> c_int;
    safe fn getsid(pid: c_int) -> c_int;
    safe fn kill(pid: c_int, signal: c_int) -> c_int;
}

const PID_BSD_INFO: c_int = 3;
const ZOMBIE: u32 = 5;
const STOP: c_int = 17;
const CONTINUE: c_int = 19;
const KILL: c_int = 9;

fn process(pid: c_int) -> Option<ProcessInfo> {
    let mut info = MaybeUninit::<ProcessInfo>::uninit();
    let size = size_of::<ProcessInfo>() as c_int;
    if unsafe { proc_pidinfo(pid, PID_BSD_INFO, 0, info.as_mut_ptr().cast(), size) } != size {
        return None;
    }
    Some(unsafe { info.assume_init() })
}

fn child_of(pid: c_int, parent: c_int) -> bool {
    process(pid).is_some_and(|info| info.parent as c_int == parent)
}

fn stop_tree(parent: c_int, depth: u32) {
    if depth >= 16 {
        return;
    }
    let mut children = [0; 256];
    let count = unsafe {
        proc_listchildpids(
            parent,
            children.as_mut_ptr().cast(),
            size_of::<[c_int; 256]>() as c_int,
        )
    };
    for child in children.into_iter().take(count.clamp(0, 256) as usize) {
        if child <= 1 || !child_of(child, parent) {
            continue;
        }
        kill(child, STOP);
        if !child_of(child, parent) {
            kill(child, CONTINUE);
            continue;
        }
        stop_tree(child, depth + 1);
        kill(child, KILL);
    }
}

// Only called by one broker thread, and never after `leader` was reaped.
static mut ALL_PIDS: [c_int; 16384] = [0; 16384];

pub(super) fn stop_session(leader: c_int) {
    for _ in 0..32 {
        let pids = unsafe { &mut *(&raw mut ALL_PIDS) };
        let count = unsafe {
            proc_listallpids(
                pids.as_mut_ptr().cast(),
                size_of::<[c_int; 16384]>() as c_int,
            )
        };
        if count <= 0 {
            return;
        }
        let mut found = 0;
        for index in 0..(count as usize).min(pids.len()) {
            let pid = pids[index];
            if pid <= 1
                || pid == leader
                || getsid(pid) != leader
                || !process(pid).is_some_and(|info| info.status != ZOMBIE)
            {
                continue;
            }
            kill(pid, STOP);
            pids[found] = pid;
            found += 1;
        }
        if found == 0 {
            return;
        }
        for &pid in &pids[..found] {
            kill(
                pid,
                if getsid(pid) == leader {
                    KILL
                } else {
                    CONTINUE
                },
            );
        }
    }
}

pub(super) fn stop_live(child: c_int) {
    kill(child, STOP);
    stop_tree(child, 0);
    stop_session(child);
    kill(-child, KILL);
    kill(child, KILL);
}

#[cfg(test)]
#[test]
fn can_identify_a_live_child() {
    unsafe extern "C" {
        fn fork() -> c_int;
        safe fn getpid() -> c_int;
        safe fn pause() -> c_int;
        safe fn _exit(code: c_int) -> !;
    }
    let parent = getpid();
    let child = unsafe { fork() };
    assert!(child >= 0);
    if child == 0 {
        pause();
        _exit(0);
    }
    assert!(child_of(child, parent));
    kill(child, KILL);
    let mut status = 0;
    assert_eq!(super::sys::waitpid(child, Some(&mut status), 0), child);
}
