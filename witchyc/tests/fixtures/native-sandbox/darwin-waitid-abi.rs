#![no_std]
#![allow(dead_code)]

#[path = "../../../runtime/native/sys.rs"]
mod sys;

#[path = "../../../runtime/native/errno.rs"]
mod errno;

const P_PID: i32 = 1;
const WNOHANG: i32 = 1;
const WEXITED: i32 = 4;
const WNOWAIT: i32 = 0x20;

#[unsafe(no_mangle)]
pub extern "C" fn probe_running_child(child: u32) -> i32 {
    let mut info = sys::SignalInfo::zeroed();
    let result = sys::waitid(P_PID, child, &mut info, WEXITED | WNOHANG | WNOWAIT);
    i32::from(result != 0 || info.pid != 0)
}

#[unsafe(no_mangle)]
pub extern "C" fn probe(child: u32, no_child_error: i32) -> i32 {
    let mut peek = sys::SignalInfo::zeroed();
    let peek_result = sys::waitid(P_PID, child, &mut peek, WEXITED | WNOWAIT);

    let mut reaped = sys::SignalInfo::zeroed();
    let reap_result = sys::waitid(P_PID, child, &mut reaped, WEXITED);
    if peek_result != 0 || peek.pid as u32 != child {
        return 1;
    }
    if reap_result != 0 || reaped.pid as u32 != child {
        return 2;
    }
    let mut empty = sys::SignalInfo::zeroed();
    let result = sys::waitid(P_PID, child, &mut empty, WEXITED | WNOHANG);
    if result != -1 || errno::get() != no_child_error {
        return 3;
    }
    0
}
