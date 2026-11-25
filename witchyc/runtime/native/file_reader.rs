//! Guest file reads from either the launch snapshot or a broker slot.

use super::global::Global;
use core::ffi::c_int;

use super::io_budget::bounded_file_read;

const SNAPSHOT: u64 = 256;
const CHUNK: u64 = 16_384;

static CHANNEL: Global<c_int> = Global::new(-1);

pub(super) fn channel() -> c_int {
    CHANNEL.get()
}

unsafe extern "C" {
    safe fn witchy_broker_file_close(channel: c_int, slot: u32) -> c_int;
    safe fn witchy_snapshot_check();
    safe fn witchy_snapshot_byte() -> u64;
    fn witchy_snapshot_read(out: *mut u8, count: usize) -> u64;
    safe fn witchy_snapshot_close();
}

// Called once before guest entry.
pub(super) fn bind_channel(channel: c_int) {
    CHANNEL.set(channel);
}

#[unsafe(no_mangle)]
extern "C" fn witchy_file_status(handle: u64) -> u64 {
    if (1..=5).contains(&handle) {
        return handle;
    }
    if handle == SNAPSHOT {
        witchy_snapshot_check();
    } else {
        super::directory::file_slot(handle, false);
    }
    0
}

#[unsafe(no_mangle)]
extern "C" fn witchy_file_byte(handle: u64) -> u64 {
    if handle == SNAPSHOT {
        return witchy_snapshot_byte();
    }
    let mut byte = 0;
    let channel = channel();
    let slot = super::directory::file_slot(handle, false);
    let size = unsafe { bounded_file_read(channel, slot, &mut byte, 1) };
    if size == 0 { 256 } else { u64::from(byte) }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_file_read(handle: u64, address: u64, count: u64) -> u64 {
    let out = address as *mut u8;
    if handle == SNAPSHOT {
        return unsafe { witchy_snapshot_read(out, count as usize) };
    }
    let channel = channel();
    let slot = super::directory::file_slot(handle, false);
    unsafe { bounded_file_read(channel, slot, out, count.min(CHUNK) as usize) }
}

#[unsafe(no_mangle)]
extern "C" fn witchy_file_close(handle: u64) -> u64 {
    if (1..=5).contains(&handle) {
        return 0;
    }
    if handle == SNAPSHOT {
        witchy_snapshot_close();
        return 0;
    }
    let slot = super::directory::file_slot(handle, true);
    let channel = channel();
    u64::from(witchy_broker_file_close(channel, slot) < 0)
}
