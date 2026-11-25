#![no_std]
use core::ffi::c_int;
#[unsafe(no_mangle)]
static witchy_entry_grants: u64 = 1 | 1024;
const PLAN: &[u8; 124] = b"witchy-launch-plan\t1\t0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\noutput\ndir\tread\tany\tcwd\tdata\t-\t-\nend\n\0";
#[unsafe(no_mangle)]
static witchy_launch_plan: [u8; PLAN.len()] = *PLAN;
// An empty secret-slot table, as emitted for guests without fixed secrets.
#[unsafe(no_mangle)]
static witchy_secret_slots: u8 = 0;

unsafe extern "C" {
    fn witchy_directory(op: u64, handle: u64, value: u64) -> u64;
    fn witchy_file_byte(handle: u64) -> u64;
    fn witchy_file_close(handle: u64) -> u64;
    fn write(fd: c_int, bytes: *const u8, len: usize) -> isize;
    fn _exit(code: c_int) -> !;
}
#[unsafe(no_mangle)]
extern "C" fn witchy_entry(_: u64) {
    unsafe {
        for &byte in b"allowed" {
            witchy_directory(0, 1024, u64::from(byte));
        }
        let file = witchy_directory(1, 1024, 0);
        if file <= 5 {
            _exit(11);
        }
        let mut out = [0u8; 4];
        for byte in &mut out {
            let n = witchy_file_byte(file);
            if n > 255 {
                _exit(12);
            }
            *byte = n as u8;
        }
        if witchy_file_close(file) != 0 || write(1, out.as_ptr(), out.len()) != 4 {
            _exit(13);
        }
    }
}
