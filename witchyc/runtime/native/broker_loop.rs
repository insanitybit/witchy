//! Dispatch for the private broker channel.

#[cfg(target_os = "macos")]
use super::exec_broker::{witchy_exec_request, witchy_exec_set_cwd};
#[cfg(target_os = "linux")]
use super::exec_linux_runner::{witchy_exec_request, witchy_exec_set_cwd};
use super::{broker_directory::*, broker_socket::*, broker_transport::*};
use core::{
    ffi::{c_char, c_int, c_void},
    ptr::{null, null_mut},
};

unsafe extern "C" {
    safe fn close(fd: c_int) -> c_int;
    fn free(ptr: *mut c_void);
    fn fcntl(fd: c_int, command: c_int, ...) -> c_int;
}

const SLOTS: usize = 16;
const CHUNK: usize = 16384;

pub(super) unsafe fn relay_connection(
    channel: c_int,
    slot: u32,
    endpoints: *const Endpoint,
    endpoint_count: u32,
    root: c_int,
    write_root: c_int,
    combined: bool,
    data_only: bool,
    entries: *mut u64,
    staging: c_int,
) {
    let mut unlimited = u64::MAX;
    let entries = if entries.is_null() {
        &mut unlimited
    } else {
        unsafe { &mut *entries }
    };
    unsafe {
        relay(
            channel,
            endpoints,
            endpoint_count,
            -1,
            root,
            write_root,
            combined,
            -1,
            null(),
            false,
            entries,
            if !data_only {
                0
            } else if write_root >= 0 {
                2
            } else {
                1
            },
            staging,
            0,
            slot as usize,
        )
    }
}

pub(super) unsafe fn relay_shared(
    channel: c_int,
    endpoints: *const Endpoint,
    endpoint_count: u32,
    listener: c_int,
    root: c_int,
    write_root: c_int,
    combined: bool,
    output: c_int,
    output_name: *const c_char,
    output_exclusive: bool,
    mut entries: u64,
    mode: u32,
    staging: c_int,
) {
    unsafe {
        relay(
            channel,
            endpoints,
            endpoint_count,
            listener,
            root,
            write_root,
            combined,
            output,
            output_name,
            output_exclusive,
            &mut entries,
            mode,
            staging,
            -1,
            0,
        )
    }
}

unsafe fn relay(
    channel: c_int,
    endpoints: *const Endpoint,
    endpoint_count: u32,
    listener: c_int,
    mut root: c_int,
    mut write_root: c_int,
    combined: bool,
    mut output: c_int,
    output_name: *const c_char,
    output_exclusive: bool,
    entries: &mut u64,
    mode: u32,
    staging: c_int,
    connection: c_int,
    connection_slot: usize,
) {
    let mut outputs = [const {
        Output {
            parent: -1,
            mode: 0,
            name: [0; 256],
        }
    }; SLOTS];
    let mut kinds = [0; SLOTS];
    let mut listings = [const {
        Listing {
            bytes: null_mut(),
            length: 0,
            cursor: 0,
        }
    }; SLOTS];
    let mut files = [-1; SLOTS];
    let mut streams = [-1; SLOTS];
    if let Some(slot) = streams.get_mut(connection_slot) {
        *slot = connection;
    }
    let mut scratch = [0; CHUNK];
    let bytes = scratch.as_mut_ptr();
    let (mut operation, mut count, mut frame_slot) = (0, 0, 0);
    while unsafe {
        witchy_broker_read_request(channel, &mut operation, &mut count, &mut frame_slot)
    } {
        let slot = frame_slot as usize;
        let ok = match operation {
            32 => {
                let base = if slot == SLOTS {
                    if count != 0 { write_root } else { root }
                } else if kinds
                    .get(slot)
                    .is_some_and(|kind| matches!(kind, 2 | 4 | 6))
                {
                    files.get(slot).copied().unwrap_or(-1)
                } else {
                    -1
                };
                let taken = if base >= 0 {
                    #[cfg(target_os = "macos")]
                    const DUP_CLOEXEC: c_int = 67;
                    #[cfg(target_os = "linux")]
                    const DUP_CLOEXEC: c_int = 1030;
                    unsafe { fcntl(base, DUP_CLOEXEC, 3 as c_int) }
                } else {
                    -1
                };
                if count > 1 || taken < 0 || !witchy_exec_set_cwd(taken) {
                    witchy_broker_reply(channel, 2, 0);
                    break;
                }
                witchy_broker_reply(channel, 0, 0)
            }
            27..=31 => {
                let capacity = if operation == 29 {
                    count.min(CHUNK)
                } else {
                    CHUNK
                };
                if count > CHUNK
                    || (operation != 29
                        && count != 0
                        && !unsafe { witchy_broker_transfer(channel, bytes, null(), count) })
                {
                    break;
                }
                // SAFETY: `bytes` is the CHUNK-byte scratch buffer and `capacity <= CHUNK`.
                let buffer = unsafe { core::slice::from_raw_parts_mut(bytes, capacity) };
                let Some(answer) = witchy_exec_request(
                    channel,
                    operation,
                    buffer,
                    if operation == 29 { 0 } else { count },
                ) else {
                    witchy_broker_reply(channel, 2, 0);
                    break;
                };
                witchy_broker_reply(channel, 0, answer)
                    && (answer == 0
                        || unsafe { witchy_broker_transfer(channel, null_mut(), bytes, answer) })
            }
            17 | 21 | 23 => {
                let writable = if slot == SLOTS {
                    write_root
                } else if kinds.get(slot).is_some_and(|kind| kind & 4 != 0) {
                    files.get(slot).copied().unwrap_or(-1)
                } else {
                    -1
                };
                if writable < 0
                    || count >= CHUNK
                    || (count == 0 && operation != 23)
                    || !unsafe { witchy_broker_transfer(channel, bytes, null(), count) }
                {
                    break;
                }
                let code = unsafe {
                    if operation == 23 {
                        witchy_broker_rename_rooted(writable, bytes, count, mode)
                    } else {
                        witchy_broker_modify_directory(
                            writable,
                            bytes,
                            count,
                            mode,
                            operation == 17,
                            entries,
                        )
                    }
                };
                witchy_broker_reply(channel, 0, 1)
                    && unsafe { witchy_broker_transfer(channel, null_mut(), &code, 1) }
            }
            16 | 18 | 19 | 20 | 22 => unsafe {
                witchy_broker_output_request(
                    channel,
                    operation,
                    slot,
                    count,
                    bytes,
                    &mut outputs,
                    &mut output,
                    output_name,
                    output_exclusive,
                    write_root,
                    staging,
                    &files,
                    &kinds,
                    mode,
                    entries,
                )
            },
            25 => {
                let directory = if slot == 0 {
                    &mut root
                } else {
                    &mut write_root
                };
                if slot > 1 || count != 0 || *directory < 0 {
                    witchy_broker_reply(channel, 2, 0);
                    break;
                }
                let result = close(*directory);
                *directory = -1;
                witchy_broker_reply(channel, u8::from(result != 0), 0)
            }
            9 => {
                if count != 19 || !unsafe { witchy_broker_transfer(channel, bytes, null(), count) }
                {
                    break;
                }
                let timeout = unsafe { u16::from_be_bytes([bytes.read(), bytes.add(1).read()]) };
                let bits = unsafe { bytes.add(2) };
                if timeout > 5000
                    || !unsafe {
                        witchy_broker_readiness(listener, &streams, bits.cast(), i32::from(timeout))
                    }
                {
                    witchy_broker_reply(channel, 2, 0);
                    break;
                }
                witchy_broker_reply(channel, 0, 17)
                    && unsafe { witchy_broker_transfer(channel, null_mut(), bits, 17) }
            }
            _ => {
                let Some(stream) = streams.get_mut(slot) else {
                    witchy_broker_reply(channel, 2, 0);
                    break;
                };
                if matches!(operation, 10..=15 | 24 | 26) {
                    unsafe {
                        witchy_broker_file_request(
                            channel,
                            operation,
                            slot,
                            count,
                            bytes,
                            root,
                            write_root,
                            combined,
                            mode,
                            &mut files,
                            &mut kinds,
                            &mut listings,
                        )
                    }
                } else {
                    unsafe {
                        witchy_broker_stream_request(
                            channel,
                            endpoints,
                            endpoint_count,
                            listener,
                            stream,
                            operation,
                            count,
                            bytes,
                        )
                    }
                }
            }
        };
        if !ok {
            break;
        }
    }
    for fd in outputs
        .into_iter()
        .map(|out| out.parent)
        .chain(streams)
        .chain(files)
    {
        if fd >= 0 {
            close(fd);
        }
    }
    for listing in listings {
        unsafe { free(listing.bytes.cast()) };
    }
}
