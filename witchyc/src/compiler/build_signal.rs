//! CLI-only cancellation. The handler only sets an atomic; normal Rust code reaps the checker.
use std::sync::atomic::{AtomicBool, Ordering};
static STOP: AtomicBool = AtomicBool::new(false);
extern "C" fn stop(_: i32) { STOP.store(true, Ordering::Relaxed); }
#[cfg(target_os = "macos")]
type PollCount = u32;
#[cfg(not(target_os = "macos"))]
type PollCount = usize;
unsafe extern "C" {
    fn signal(number: i32, handler: usize) -> usize;
    fn poll(fds: *mut PollFd, count: PollCount, timeout_ms: i32) -> i32;
}
#[repr(C)]
struct PollFd { fd: i32, events: i16, revents: i16 }
pub(super) struct Signals([usize; 2]);
impl Signals {
    pub fn install() -> Result<Self, String> {
        STOP.store(false, Ordering::Relaxed);
        let old = unsafe { signal(2, stop as *const () as usize) };
        if old == usize::MAX { return Err("cannot install build cancellation handler".into()); }
        let term = unsafe { signal(15, stop as *const () as usize) };
        if term == usize::MAX {
            unsafe { signal(2, old); }
            return Err("cannot install build cancellation handler".into());
        }
        Ok(Self([old, term]))
    }
    pub fn stopped(&self) -> bool { STOP.load(Ordering::Relaxed) }
    /// One byte from standard input, unbuffered (it pairs with `poll` on the same descriptor):
    /// read through a `File` over descriptor 0 that is never closed.
    pub fn byte(&self) -> Result<Option<u8>, String> {
        use std::io::Read;
        use std::os::fd::FromRawFd;
        let mut stdin = std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(0) });
        let mut byte = [0u8; 1];
        match stdin.read(&mut byte) {
            Ok(0) => Ok(None), Ok(_) => Ok(Some(byte[0])), Err(error) => Err(error.to_string()),
        }
    }
    pub fn stdin_ready(&self) -> Result<bool, String> {
        let mut descriptor = PollFd { fd: 0, events: 1, revents: 0 };
        let result = unsafe { poll(&mut descriptor, 1, 200) };
        if result < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted { return Ok(false); }
            return Err(error.to_string());
        }
        if descriptor.revents & (8 | 32) != 0 { return Err("build stdin polling failed".into()); }
        Ok(result > 0)
    }
}
impl Drop for Signals {
    fn drop(&mut self) { unsafe { signal(2, self.0[0]); signal(15, self.0[1]); } }
}
