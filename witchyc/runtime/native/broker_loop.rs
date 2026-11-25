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
}

const SLOTS: usize = 16;
const CHUNK: usize = 16384;

/// `entries` is null or one `u64` that nothing else in this process refers to.
pub(super) unsafe fn relay_connection(
    channel: c_int,
    slot: u32,
    endpoints: &[Endpoint],
    root: c_int,
    write_root: c_int,
    combined: bool,
    data_only: bool,
    entries: *mut u64,
    staging: c_int,
) {
    let mut unlimited = u64::MAX;
    // SAFETY: the caller's promise.
    let entries = unsafe { entries.as_mut() }.unwrap_or(&mut unlimited);
    // SAFETY: a null name is no output name.
    unsafe {
        relay(
            channel,
            endpoints,
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

/// `output_name` is null or a C string.
pub(super) unsafe fn relay_shared(
    channel: c_int,
    endpoints: &[Endpoint],
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
    // SAFETY: the caller's promise.
    unsafe {
        relay(
            channel,
            endpoints,
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

/// `output_name` is null or a C string.
unsafe fn relay(
    channel: c_int,
    endpoints: &[Endpoint],
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
    let mut scratch = [0u8; CHUNK];
    while let Some((operation, count, frame_slot)) = witchy_broker_read_request(channel) {
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
                    super::sys::duplicate_close_on_exec(base, 3)
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
                        && !witchy_broker_receive(channel, &mut scratch[..count]))
                {
                    break;
                }
                let Some(answer) = witchy_exec_request(
                    channel,
                    operation,
                    &mut scratch[..capacity],
                    if operation == 29 { 0 } else { count },
                ) else {
                    witchy_broker_reply(channel, 2, 0);
                    break;
                };
                witchy_broker_reply(channel, 0, answer)
                    && (answer == 0
                        || scratch
                            .get(..answer)
                            .is_some_and(|bytes| witchy_broker_send(channel, bytes)))
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
                    || !witchy_broker_receive(channel, &mut scratch[..count])
                {
                    break;
                }
                let code = if operation == 23 {
                    witchy_broker_rename_rooted(writable, &mut scratch, count, mode)
                } else {
                    witchy_broker_modify_directory(
                        writable,
                        &mut scratch,
                        count,
                        mode,
                        operation == 17,
                        entries,
                    )
                };
                witchy_broker_reply(channel, 0, 1) && witchy_broker_send(channel, &[code])
            }
            16 | 18 | 19 | 20 | 22 => unsafe {
                witchy_broker_output_request(
                    channel,
                    operation,
                    slot,
                    count,
                    &mut scratch,
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
                if count != 19 || !witchy_broker_receive(channel, &mut scratch[..count]) {
                    break;
                }
                let timeout = u16::from_be_bytes([scratch[0], scratch[1]]);
                // The scratch is longer than the 19 bytes of this frame.
                let Some(bits) = scratch[2..].first_chunk_mut::<17>() else {
                    break;
                };
                if timeout > 5000
                    || !witchy_broker_readiness(listener, &streams, bits, i32::from(timeout))
                {
                    witchy_broker_reply(channel, 2, 0);
                    break;
                }
                witchy_broker_reply(channel, 0, 17) && witchy_broker_send(channel, bits)
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
                            &mut scratch,
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
                    witchy_broker_stream_request(
                        channel,
                        endpoints,
                        listener,
                        stream,
                        operation,
                        count,
                        &mut scratch,
                    )
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
