#![no_std]
#![allow(dead_code)]
#[path = "../../../runtime/native/sys.rs"]
mod sys;
#[path = "../../../runtime/native/linux_stat.rs"]
mod linux_stat;

const P_PID: i32 = 1;
const WNOHANG: i32 = 1;
const WEXITED: i32 = 4;
const WNOWAIT: i32 = 0x01000000;
const EINTR: i32 = 4;
const ECHILD: i32 = 10;

unsafe extern "C" {
    fn __errno_location() -> *const i32;
    fn fork() -> i32;
    fn _exit(status: i32) -> !;
}

#[unsafe(no_mangle)]
pub extern "C" fn probe() -> i32 {
    let child = unsafe { fork() };
    if child < 0 {
        return 1;
    }
    if child == 0 {
        unsafe { _exit(42) };
    }
    let mut peek = sys::SignalInfo::zeroed();
    let peek_result = sys::waitid(P_PID, child as u32, &mut peek, WEXITED | WNOWAIT);

    let mut info = sys::SignalInfo::zeroed();
    let result = sys::waitid(P_PID, child as u32, &mut info, WEXITED);
    if peek_result != 0 || peek.pid != child || peek.status != 42 {
        return 2;
    }
    if result != 0 || info.pid != child || info.status != 42 {
        return 3;
    }
    let mut empty = sys::SignalInfo::zeroed();
    let result = sys::waitid(P_PID, child as u32, &mut empty, WEXITED);
    // libc returns a readable pointer to the calling thread's errno.
    let errno = unsafe { *__errno_location() };
    if result != -1 || errno != ECHILD {
        return 4;
    }
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn probe_running_child(child: u32) -> i32 {
    let mut info = sys::SignalInfo::zeroed();
    let result = sys::waitid(P_PID, child, &mut info, WEXITED | WNOHANG | WNOWAIT);
    i32::from(result != 0 || info.pid != 0)
}

#[unsafe(no_mangle)]
pub extern "C" fn probe_interrupted_wait(child: u32) -> i32 {
    let mut info = sys::SignalInfo::zeroed();
    let result = sys::waitid(P_PID, child, &mut info, WEXITED);
    // libc returns a readable pointer to the calling thread's errno.
    let errno = unsafe { *__errno_location() };
    i32::from(result != -1 || errno != EINTR)
}

#[unsafe(no_mangle)]
pub extern "C" fn probe_metadata(
    directory: i32,
    file: i32,
    device: u64,
    inode: u64,
    mode: u32,
    links: u32,
    uid: u32,
    size: i64,
) -> i32 {
    let Some(by_fd) = linux_stat::stat(file) else { return 1 };
    let Some(by_name) = linux_stat::stat_at(directory, c"sample") else { return 2 };
    let expected = (device, inode, mode, links, uid, size);
    for info in [by_fd, by_name] {
        if (info.device, info.inode, info.mode, info.links, info.uid, info.size) != expected {
            return 3;
        }
    }
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn probe_metadata_errors(directory: i32) -> i32 {
    // AT_FDCWD is valid as a lookup base, but -1 is not an open descriptor.
    if linux_stat::stat(-1).is_some() {
        return 1;
    }
    if linux_stat::stat_at(-1, c"sample").is_some() {
        return 2;
    }
    if linux_stat::stat_at(directory, c"missing").is_some() {
        return 3;
    }
    0
}
