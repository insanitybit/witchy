// Source-only native entry. This helper does not confer an admission capability.
#![no_std]

#[cfg(not(all(target_os = "macos", target_arch = "aarch64", target_pointer_width = "64", target_endian = "little")))]
compile_error!("the pinned Lean helper ABI requires macOS ARM64");

use core::ffi::{c_char, c_int};
use core::panic::PanicInfo;

// Lean 4.36's 64-bit object header and single-field constructor layout.
// This boundary is pinned to the selected SDK by the helper builder.
#[repr(C)]
struct LeanObject {
    rc: i32,
    size: u16,
    fields: u8,
    tag: u8,
}

unsafe extern "C" {
    fn lean_dec_ref_cold(object: *mut LeanObject);
    fn lean_mk_string(value: *const c_char) -> *mut LeanObject;
    fn lean_setup_args(argc: c_int, argv: *mut *mut c_char) -> *mut *mut c_char;
    fn initialize_Resident(builtin: u8) -> *mut LeanObject;
    fn witchy_resident_main(root: *mut LeanObject, binding: *mut LeanObject, nonce: *mut LeanObject) -> *mut LeanObject;
    fn lean_io_mark_end_initialization();
    fn lean_io_result_show_error(result: *mut LeanObject);
    fn lean_init_task_manager_using(workers: u32);
    fn lean_finalize_task_manager();
    fn lean_run_main(main: unsafe extern "C" fn(c_int, *mut *mut c_char) -> *mut LeanObject, argc: c_int, argv: *mut *mut c_char) -> *mut LeanObject;
    fn alarm(seconds: u32) -> u32;
    fn write(fd: c_int, bytes: *const u8, length: usize) -> isize;
    fn abort() -> !;
}

#[cfg(witchy_body)]
unsafe extern "C" {
    fn lean_alloc_small_object_core(size: u32) -> *mut LeanObject;
    fn witchy_save_header(root: *mut LeanObject, path: *mut LeanObject) -> *mut LeanObject;
    fn lean_mk_io_user_error(value: *mut LeanObject) -> *mut LeanObject;
}

#[cfg(witchy_body)]
unsafe extern "C" {
    fn _dyld_get_shared_cache_uuid(uuid: *mut u8) -> bool;
}

#[panic_handler]
fn panic(_: &PanicInfo<'_>) -> ! {
    unsafe { abort() }
}

// Lean's constructors own their field. The allocator initializes the size field.
#[cfg(witchy_body)]
unsafe fn result(tag: u8, value: *mut LeanObject) -> *mut LeanObject {
    let object = unsafe { lean_alloc_small_object_core(16) };
    unsafe {
        (*object).rc = 1;
        (*object).fields = 1;
        (*object).tag = tag;
        object.cast::<u8>().add(8).cast::<*mut LeanObject>().write(value);
    }
    object
}

unsafe fn release(object: *mut LeanObject) {
    unsafe {
        if (*object).rc > 1 {
            (*object).rc -= 1;
        } else if (*object).rc != 0 {
            lean_dec_ref_cold(object);
        }
    }
}

fn boxed(value: usize) -> *mut LeanObject {
    ((value << 1) | 1) as *mut LeanObject
}

#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_resident_deadline(seconds: u32) -> *mut LeanObject {
    unsafe { alarm(seconds) };
    boxed(0)
}

#[cfg(witchy_body)]
#[unsafe(no_mangle)]
extern "C" fn witchy_use_fixed_header() -> u8 {
    u8::from(option_env!("WITCHY_FIXED_HEADER").is_some())
}

#[cfg(witchy_body)]
unsafe fn fixed_header_error(message: &'static [u8]) -> *mut LeanObject {
    unsafe { result(1, lean_mk_io_user_error(lean_mk_string(message.as_ptr().cast()))) }
}

#[cfg(witchy_body)]
fn shared_cache_matches(expected: &str) -> Option<bool> {
    let mut uuid = [0u8; 16];
    if !unsafe { _dyld_get_shared_cache_uuid(uuid.as_mut_ptr()) } {
        return None;
    }
    let mut text = [0u8; 36];
    let digits = b"0123456789abcdef";
    let mut position = 0;
    for (index, byte) in uuid.into_iter().enumerate() {
        if matches!(index, 4 | 6 | 8 | 10) {
            text[position] = b'-';
            position += 1;
        }
        text[position] = digits[(byte >> 4) as usize];
        text[position + 1] = digits[(byte & 15) as usize];
        position += 2;
    }
    Some(expected.as_bytes() == text)
}

unsafe extern "C" fn run_resident(_: c_int, argv: *mut *mut c_char) -> *mut LeanObject {
    #[cfg(witchy_body)]
    if let Some(expected) = option_env!("WITCHY_FIXED_HEADER") {
        match shared_cache_matches(expected) {
            None => return unsafe { fixed_header_error(b"Darwin shared cache unavailable\0") },
            Some(false) => return unsafe { fixed_header_error(b"Darwin shared cache changed; reinstall proof helper\0") },
            Some(true) => {}
        }
        let first = unsafe { core::ffi::CStr::from_ptr(*argv.add(1)) };
        if first.to_bytes() == b"--install-fixed-header" {
            return unsafe { witchy_save_header(lean_mk_string(*argv.add(2)), lean_mk_string(*argv.add(3))) };
        }
    }
    unsafe { witchy_resident_main(lean_mk_string(*argv.add(1)), lean_mk_string(*argv.add(2)), lean_mk_string(*argv.add(3))) }
}

fn diagnostic(message: &[u8]) {
    unsafe { write(2, message.as_ptr(), message.len()); }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn main(argc: c_int, argv: *mut *mut c_char) -> c_int {
    if argc != 4 {
        diagnostic(b"UNSEALED_DIAGNOSTIC: expected private root, binding, nonce\n");
        return 2;
    }
    let argv = unsafe { lean_setup_args(argc, argv) };
    let initial = unsafe { initialize_Resident(1) };
    unsafe { lean_io_mark_end_initialization() };
    if unsafe { (*initial).tag != 0 } {
        diagnostic(b"UNSEALED_DIAGNOSTIC: native initialization error\n");
        unsafe { lean_io_result_show_error(initial); release(initial) };
        return 1;
    }
    unsafe { release(initial) };
    let workers = if cfg!(witchy_body) { 4 } else { 1 };
    unsafe { lean_init_task_manager_using(workers) };
    let outcome = unsafe { lean_run_main(run_resident, argc, argv) };
    unsafe { alarm(0); lean_finalize_task_manager() };
    if unsafe { (*outcome).tag != 0 } {
        diagnostic(b"UNSEALED_DIAGNOSTIC: native request error\n");
        unsafe { lean_io_result_show_error(outcome); release(outcome) };
        return 1;
    }
    let value = unsafe { outcome.cast::<u8>().add(8).cast::<*mut LeanObject>().read() };
    let status = ((value as usize) >> 1) as u32 as c_int;
    unsafe { release(outcome) };
    status
}
