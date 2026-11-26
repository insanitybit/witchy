#![allow(dead_code)]

#[path = "../../../runtime/native/clock_random.rs"]
mod clock_random;
#[cfg(target_os = "linux")]
#[path = "../../../runtime/native/errno.rs"]
mod errno;

#[cfg(all(target_os = "linux", witchy_test_wrapped_getrandom))]
mod wrapped {
    use core::{cell::Cell, ffi::c_void};

    thread_local! {
        static CALL: Cell<Option<u8>> = const { Cell::new(None) };
    }

    unsafe extern "C" {
        fn __real_getrandom(out: *mut c_void, size: usize, flags: u32) -> isize;
        fn witchy_rand_next(grant: u64) -> u64;
    }

    #[unsafe(no_mangle)]
    unsafe extern "C" fn __wrap_getrandom(out: *mut c_void, size: usize, flags: u32) -> isize {
        let Some(call) = CALL.with(Cell::get) else {
            return unsafe { __real_getrandom(out, size, flags) };
        };
        CALL.with(|counter| counter.set(Some(call + 1)));
        assert_eq!(flags, 0);
        let bytes: &[u8] = match call {
            0 => {
                assert_eq!(size, 8);
                super::errno::set(4);
                return -1;
            }
            1 => {
                assert_eq!(size, 8);
                &[1, 2, 3]
            }
            2 => {
                assert_eq!(size, 5);
                &[4, 5, 6, 7, 8]
            }
            _ => panic!("unexpected getrandom call"),
        };
        unsafe { bytes.as_ptr().copy_to_nonoverlapping(out.cast(), bytes.len()) };
        bytes.len() as isize
    }

    #[test]
    fn interrupted_and_short_reads_fill_the_result() {
        CALL.with(|counter| counter.set(Some(0)));
        let result = unsafe { witchy_rand_next(32) };
        let calls = CALL.with(|counter| counter.replace(None));
        assert_eq!(calls, Some(3));
        assert_eq!(result.to_ne_bytes(), [1, 2, 3, 4, 5, 6, 7, 8]);
    }
}
