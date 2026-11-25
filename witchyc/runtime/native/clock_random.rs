//! Native clock and platform randomness adapters.

use core::{ffi::c_int, mem::MaybeUninit};

#[repr(C)]
pub(super) struct Timespec {
    pub(super) seconds: i64,
    pub(super) nanos: i64,
}

unsafe extern "C" {
    fn clock_gettime(clock: c_int, out: *mut Timespec) -> c_int;
    #[cfg(all(not(witchy_native_aws_lc), target_os = "macos"))]
    fn arc4random_buf(out: *mut core::ffi::c_void, size: usize);
    #[cfg(all(not(witchy_native_aws_lc), target_os = "linux"))]
    fn getrandom(out: *mut core::ffi::c_void, size: usize, flags: u32) -> isize;
    #[cfg(witchy_native_aws_lc)]
    #[link_name = "aws_lc_fips_0_14_4_RAND_bytes"]
    fn rand_bytes(out: *mut u8, size: usize) -> c_int;
}

fn trap() -> ! {
    unsafe { core::arch::asm!("brk #0", options(noreturn, nostack)) }
}

#[unsafe(no_mangle)]
extern "C" fn witchy_clock_now_ms(grant: u64) -> u64 {
    const CLOCK: u64 = 16;
    const REALTIME: c_int = 0;
    let mut current = MaybeUninit::<Timespec>::uninit();
    if grant != CLOCK || unsafe { clock_gettime(REALTIME, current.as_mut_ptr()) } != 0 {
        trap();
    }
    let Timespec { seconds, nanos } = unsafe { current.assume_init() };
    if seconds < 0 {
        trap();
    }
    if seconds >= i64::MAX / 1000 {
        return i64::MAX as u64;
    }
    seconds as u64 * 1000 + nanos as u64 / 1_000_000
}

#[unsafe(no_mangle)]
extern "C" fn witchy_rand_next(grant: u64) -> u64 {
    const RAND: u64 = 32;
    if grant != RAND {
        trap();
    }
    let mut result = MaybeUninit::<u64>::uninit();
    unsafe {
        #[cfg(witchy_native_aws_lc)]
        if super::digest::fips_mode() == 0
            || rand_bytes(result.as_mut_ptr().cast(), size_of::<u64>()) != 1
        {
            trap();
        }
        #[cfg(all(not(witchy_native_aws_lc), target_os = "macos"))]
        arc4random_buf(result.as_mut_ptr().cast(), size_of::<u64>());
        #[cfg(all(not(witchy_native_aws_lc), target_os = "linux"))]
        {
            let mut filled = 0;
            while filled < size_of::<u64>() {
                let count = getrandom(
                    result.as_mut_ptr().cast::<u8>().add(filled).cast(),
                    size_of::<u64>() - filled,
                    0,
                );
                if count < 0 && super::errno::get() == 4 {
                    continue;
                }
                if count <= 0 || count as usize > size_of::<u64>() - filled {
                    trap();
                }
                filled += count as usize;
            }
        }
        result.assume_init()
    }
}

#[cfg(all(test, not(witchy_native_aws_lc)))]
#[test]
fn clock_and_random_call_platform_adapters() {
    assert!(witchy_clock_now_ms(16) > 0);
    let _ = witchy_rand_next(32);
}
