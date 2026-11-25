#![no_std]
use core::ffi::c_int;

#[unsafe(no_mangle)]
static witchy_entry_grants: u64 = 1 | 2 | 32768;
const PLAN: &[u8; 150] = b"witchy-launch-plan\t1\t0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\noutput\nallocator\nsecret\ttargets\tsign/witchy-2/targets\tfd\t3\nend\n\0";
#[unsafe(no_mangle)]
static witchy_launch_plan: [u8; PLAN.len()] = *PLAN;
#[unsafe(no_mangle)]
static witchy_secret_slots: [u8; 10] = *b"\x03targets\0\0";

unsafe extern "C" {
    fn witchy_signer_sign(handle: u64, message: u64, length: u64, output: u64, capacity: u64) -> u64;
    fn write(fd: c_int, bytes: *const u8, len: usize) -> isize;
    fn _exit(status: c_int) -> !;
}

#[unsafe(no_mangle)]
extern "C" fn witchy_entry(_: u64) {
    let mut signature = [0u8; 3309];
    let message = b"witchy/v2/targets\0release 1\n";
    let result = unsafe { witchy_signer_sign(18, message.as_ptr() as u64, message.len() as u64, signature.as_mut_ptr() as u64, signature.len() as u64) };
    if result != 0 || unsafe { write(1, b"signed\n".as_ptr(), 7) } != 7 {
        unsafe { _exit(11) }
    }
}
