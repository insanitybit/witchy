//! Guest output staging and broker commit.

use super::global::Global;
use core::{
    ffi::{c_int, c_void},
    ptr::null_mut,
};

use super::{
    broker_transport::{witchy_broker_file_commit, witchy_broker_output_open},
    file_reader::channel,
    io_budget::{witchy_filesystem_charge, witchy_filesystem_remaining},
};

const SLOTS: usize = 16;
const ROOT: u64 = 2048;
const CAPACITY: usize = 1024 * 1024;
const IO: u64 = 5;
const TOO_LARGE: u64 = 3;

#[derive(Clone, Copy)]
struct Output {
    handle: u64,
    length: usize,
    bytes: *mut u8,
}

impl Output {
    const EMPTY: Self = Self {
        handle: 0,
        length: 0,
        bytes: null_mut(),
    };
}

static mut ROOT_BYTES: [u8; CAPACITY] = [0; CAPACITY];
static mut ROOT_OUTPUT: Output = Output {
    handle: ROOT,
    length: 0,
    bytes: (&raw mut ROOT_BYTES).cast(),
};
static mut OUTPUTS: [Output; SLOTS] = [Output::EMPTY; SLOTS];
static NEXT: Global<u64> = Global::new(8192);

unsafe extern "C" {
    fn malloc(size: usize) -> *mut c_void;
    fn free(bytes: *mut c_void);
    fn write(fd: c_int, bytes: *const c_void, length: usize) -> isize;
    safe fn exit(status: c_int) -> !;
    safe fn witchy_broker_output_drop(channel: c_int, slot: u32) -> c_int;
}

fn trap() -> ! {
    unsafe { core::arch::asm!("brk #0", options(noreturn, nostack)) }
}

fn status(result: c_int) -> u64 {
    if result < 0 { IO } else { result as u64 }
}

/// The directory dispatcher supplies a path valid for the duration of the call.
#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_open_directory_output(
    path: *const u8,
    length: usize,
    exclusive: bool,
    append: bool,
) -> u64 {
    let outputs = unsafe { &mut *(&raw mut OUTPUTS) };
    // SAFETY: the directory dispatcher supplies `length` readable bytes at
    // `path` that stay valid for the duration of the call.
    let path = unsafe { core::slice::from_raw_parts(path, length) };
    for _ in 0..SLOTS {
        let handle = NEXT.get();
        if handle == u64::MAX {
            break;
        }
        NEXT.set(handle + 1);
        let slot = handle as usize % SLOTS;
        let output = &mut outputs[slot];
        if output.handle != 0 {
            continue;
        }
        let bytes = unsafe { malloc(CAPACITY) }.cast::<u8>();
        if bytes.is_null() {
            return IO;
        }
        let result = witchy_broker_output_open(channel(), slot as u32, path, exclusive, append);
        if result != 0 {
            unsafe { free(bytes.cast()) };
            return status(result);
        }
        *output = Output {
            handle,
            length: 0,
            bytes,
        };
        return handle;
    }
    IO
}

/// For a slice write, the guest emitter supplies a readable span. Output
/// buffers are private to the runtime and calls are serialized.
#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_write_file(op: u64, handle: u64, value: u64, count: u64) -> u64 {
    let channel = channel();
    if channel < 0 {
        trap();
    }
    if (1..=IO).contains(&handle) && matches!(op, 0 | 3 | 4) {
        return if op == 0 { handle } else { 0 };
    }
    let (slot, output) = if handle == ROOT {
        (SLOTS, unsafe { &mut *(&raw mut ROOT_OUTPUT) })
    } else {
        let slot = handle as usize % SLOTS;
        (slot, unsafe { &mut (*(&raw mut OUTPUTS))[slot] })
    };
    if handle == 0 || output.handle != handle {
        trap();
    }
    match op {
        0 => return 0,
        3 | 4 => {
            let result = if op == 3 {
                // SAFETY: a live output owns `CAPACITY` bytes at `bytes`, of
                // which writes have initialized the first `length`.
                let staged = unsafe { core::slice::from_raw_parts(output.bytes, output.length) };
                witchy_broker_file_commit(channel, slot as u32, staged)
            } else {
                witchy_broker_output_drop(channel, slot as u32)
            };
            if slot != SLOTS {
                unsafe { free(output.bytes.cast()) };
            }
            *output = Output::EMPTY;
            if result == -1 {
                let message = b"witchy: a write reached a secret's object, or its broker failed; the run is stopped\n";
                unsafe {
                    write(2, message.as_ptr().cast(), message.len());
                    exit(2);
                }
            }
            return status(result);
        }
        1 | 2 => {}
        _ => trap(),
    }
    let available = ((CAPACITY - output.length) as u64).min(witchy_filesystem_remaining());
    if op == 1 {
        let Ok(byte) = u8::try_from(value) else {
            trap()
        };
        if available == 0 {
            return TOO_LARGE;
        }
        unsafe { output.bytes.add(output.length).write(byte) };
        output.length += 1;
        witchy_filesystem_charge(1);
        return 0;
    }
    let copied = count.min(available);
    if copied != 0 {
        unsafe {
            core::ptr::copy_nonoverlapping(
                value as *const u8,
                output.bytes.add(output.length),
                copied as usize,
            );
        }
    }
    output.length += copied as usize;
    witchy_filesystem_charge(copied);
    copied | if copied < count { TOO_LARGE << 32 } else { 0 }
}
