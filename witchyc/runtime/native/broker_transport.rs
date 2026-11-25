//! Synchronous transport for the native broker's framed socket.

use core::ffi::{c_int, c_void};

unsafe extern "C" {
    fn read(fd: c_int, out: *mut c_void, len: usize) -> isize;
    #[cfg(target_os = "macos")]
    fn write(fd: c_int, bytes: *const c_void, len: usize) -> isize;
    #[cfg(target_os = "linux")]
    fn send(fd: c_int, bytes: *const c_void, len: usize, flags: c_int) -> isize;
}

/// If `out` is null, `bytes` must be writable for `length` bytes. Otherwise
/// `out` must be readable for `length` bytes and `bytes` is ignored.
pub(super) unsafe fn witchy_broker_transfer(
    fd: c_int,
    mut bytes: *mut u8,
    mut out: *const u8,
    mut length: usize,
) -> bool {
    const EINTR: c_int = 4;
    while length != 0 {
        let count = if out.is_null() {
            unsafe { read(fd, bytes.cast(), length) }
        } else {
            #[cfg(target_os = "macos")]
            {
                unsafe { write(fd, out.cast(), length) }
            }
            #[cfg(target_os = "linux")]
            {
                unsafe { send(fd, out.cast(), length, 0x4000) }
            }
        };
        if count < 0 && super::errno::get() == EINTR {
            continue;
        }
        if count <= 0 || count as usize > length {
            return false;
        }
        let count = count as usize;
        if out.is_null() {
            bytes = unsafe { bytes.add(count) };
        } else {
            out = unsafe { out.add(count) };
        }
        length -= count;
    }
    true
}

const IO_ERROR: c_int = -1;
const REFUSED: c_int = -2;
const TIMED_OUT: c_int = -3;
const WOULD_BLOCK: c_int = -4;

pub(super) fn witchy_broker_reply(channel: c_int, status: u8, length: usize) -> bool {
    let [hi, lo] = (length as u16).to_be_bytes();
    let header = [status, hi, lo];
    unsafe {
        witchy_broker_transfer(
            channel,
            core::ptr::null_mut(),
            header.as_ptr(),
            header.len(),
        )
    }
}

fn response(operation: u8, header: [u8; 3], length: usize, read_payload: bool) -> (c_int, usize) {
    let [status, hi, lo] = header;
    let count = usize::from(u16::from_be_bytes([hi, lo]));
    match (status, operation, count) {
        (3, 5 | 6, 0) => return (TIMED_OUT, 0),
        (4, 7 | 8, 0) => return (WOULD_BLOCK, 0),
        (1, 10 | 14 | 18 | 20 | 22 | 24 | 26, 1..=5) => return (count as c_int, 0),
        (1, _, 0) => return (REFUSED, 0),
        _ => {}
    }
    let max = match operation {
        13 => 1,         // FILE_PROBE
        2 | 7 => length, // SEND and TRY_SEND
        _ if read_payload => length,
        _ => 0,
    };
    if status != 0 || count > max {
        (IO_ERROR, 0)
    } else {
        (count as c_int, if read_payload { count } else { 0 })
    }
}

/// Non-null outgoing and incoming pointers must respectively permit reading
/// and writing the operation-specific capacity. The broker channel is exclusively held.
unsafe fn exchange(
    channel: c_int,
    operation: u8,
    slot: u32,
    out: *const u8,
    input: *mut u8,
    length: usize,
    capacity: usize,
) -> c_int {
    use core::ptr::{null, null_mut};
    if length > 16384 {
        return IO_ERROR;
    }
    let [hi, lo] = (length as u16).to_be_bytes();
    let request = [operation, hi, lo, slot as u8];
    let mut header = [0; 3];
    if !unsafe { witchy_broker_transfer(channel, null_mut(), request.as_ptr(), request.len()) }
        || (!out.is_null() && !unsafe { witchy_broker_transfer(channel, null_mut(), out, length) })
        || !unsafe { witchy_broker_transfer(channel, header.as_mut_ptr(), null(), header.len()) }
    {
        return IO_ERROR;
    }
    let (result, payload) = response(operation, header, length, !input.is_null());
    if payload > capacity
        || (payload != 0 && !unsafe { witchy_broker_transfer(channel, input, null(), payload) })
    {
        IO_ERROR
    } else {
        result
    }
}

use core::ptr::{null, null_mut};

#[unsafe(no_mangle)]
extern "C" fn witchy_net_connect(channel: c_int, slot: u32, endpoint: u32) -> c_int {
    unsafe { exchange(channel, 1, slot, null(), null_mut(), endpoint as usize, 0) }
}

#[unsafe(no_mangle)]
extern "C" fn witchy_net_accept(channel: c_int, slot: u32, wait: bool) -> c_int {
    unsafe {
        exchange(
            channel,
            if wait { 5 } else { 6 },
            slot,
            null(),
            null_mut(),
            0,
            0,
        )
    }
}

#[unsafe(no_mangle)]
extern "C" fn witchy_net_close(channel: c_int, slot: u32) -> c_int {
    unsafe { exchange(channel, 4, slot, null(), null_mut(), 0, 0) }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_net_send(
    channel: c_int,
    slot: u32,
    bytes: *const u8,
    len: usize,
) -> c_int {
    if bytes.is_null() && len != 0 {
        return IO_ERROR;
    }
    unsafe { exchange(channel, 2, slot, bytes, null_mut(), len, 0) }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_net_receive(
    channel: c_int,
    slot: u32,
    bytes: *mut u8,
    len: usize,
) -> c_int {
    if bytes.is_null() && len != 0 {
        return IO_ERROR;
    }
    unsafe { exchange(channel, 3, slot, null(), bytes, len, len) }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_net_try_send(
    channel: c_int,
    slot: u32,
    bytes: *const u8,
    len: usize,
) -> c_int {
    unsafe { exchange(channel, 7, slot, bytes, null_mut(), len, 0) }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_net_try_receive(
    channel: c_int,
    slot: u32,
    bytes: *mut u8,
    len: usize,
) -> c_int {
    unsafe { exchange(channel, 8, slot, null(), bytes, len, len) }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_net_poll(
    channel: c_int,
    entries: *mut [u8; 17],
    timeout: u32,
) -> c_int {
    let [hi, lo] = (timeout as u16).to_be_bytes();
    let bits = unsafe { entries.read() };
    let mut packet = [0; 19];
    packet[0] = hi;
    packet[1] = lo;
    packet[2..].copy_from_slice(&bits);
    let count = unsafe {
        exchange(
            channel,
            9,
            0,
            packet.as_ptr(),
            packet.as_mut_ptr(),
            packet.len(),
            17,
        )
    };
    if count != 17 {
        return IO_ERROR;
    }
    unsafe { entries.write(packet.as_ptr().cast::<[u8; 17]>().read()) };
    0
}

#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_broker_file_open(
    channel: c_int,
    slot: u32,
    path: *const u8,
    len: usize,
    kind: u32,
) -> c_int {
    let operation = match kind {
        6 => 26,
        4 => 24,
        2 => 14,
        _ => 10,
    };
    unsafe { exchange(channel, operation, slot, path, null_mut(), len, 0) }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_broker_file_read(
    channel: c_int,
    slot: u32,
    bytes: *mut u8,
    len: usize,
) -> c_int {
    unsafe { exchange(channel, 11, slot, null(), bytes, len, len) }
}

#[unsafe(no_mangle)]
extern "C" fn witchy_broker_file_close(channel: c_int, slot: u32) -> c_int {
    unsafe { exchange(channel, 12, slot, null(), null_mut(), 0, 0) }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_broker_file_probe(
    channel: c_int,
    path: *const u8,
    len: usize,
) -> c_int {
    let mut code = 0;
    let count = unsafe { exchange(channel, 13, 0, path, &mut code, len, 1) };
    if count == 1 { c_int::from(code) } else { 0 }
}

#[unsafe(no_mangle)]
extern "C" fn witchy_broker_directory_list(channel: c_int, slot: u32, source: u8) -> c_int {
    unsafe { exchange(channel, 15, slot, &source, null_mut(), 1, 0) }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_broker_file_commit(
    channel: c_int,
    slot: u32,
    data: *const u8,
    len: usize,
) -> c_int {
    if len > 1024 * 1024 || (len != 0 && data.is_null()) {
        return IO_ERROR;
    }
    let [a, b, c, d] = (len as u32).to_be_bytes();
    let request = [16, 0, 4, slot as u8, a, b, c, d];
    let mut status = [0; 3];
    if !unsafe { witchy_broker_transfer(channel, null_mut(), request.as_ptr(), request.len()) }
        || !unsafe { witchy_broker_transfer(channel, null_mut(), data, len) }
        || !unsafe { witchy_broker_transfer(channel, status.as_mut_ptr(), null(), status.len()) }
    {
        return IO_ERROR;
    }
    match status {
        [0, 0, code @ 0..=6] => c_int::from(code),
        _ => IO_ERROR,
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_broker_directory_change(
    channel: c_int,
    source: u32,
    path: *const u8,
    len: usize,
    remove: bool,
) -> c_int {
    let mut code = 0;
    let count = unsafe {
        exchange(
            channel,
            if remove { 21 } else { 17 },
            source,
            path,
            &mut code,
            len,
            1,
        )
    };
    if count == 1 { c_int::from(code) } else { 5 }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_broker_output_open(
    channel: c_int,
    slot: u32,
    path: *const u8,
    len: usize,
    exclusive: bool,
    append: bool,
) -> c_int {
    let op = if append {
        22
    } else if exclusive {
        20
    } else {
        18
    };
    unsafe { exchange(channel, op, slot, path, null_mut(), len, 0) }
}

#[unsafe(no_mangle)]
extern "C" fn witchy_broker_output_drop(channel: c_int, slot: u32) -> c_int {
    unsafe { exchange(channel, 19, slot, null(), null_mut(), 0, 0) }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_broker_rename(
    channel: c_int,
    source: u32,
    from: *const u8,
    from_len: usize,
    to: *const u8,
    to_len: usize,
) -> c_int {
    let mut packed = [0u8; 16384];
    let Some(total) = from_len
        .checked_add(to_len)
        .and_then(|len| len.checked_add(2))
    else {
        return 2;
    };
    if from_len == 0 || to_len == 0 || total >= packed.len() {
        return 2;
    }
    let [hi, lo] = (from_len as u16).to_be_bytes();
    packed[0] = hi;
    packed[1] = lo;
    let mut code = 0;
    unsafe {
        core::ptr::copy_nonoverlapping(from, packed.as_mut_ptr().add(2), from_len);
        core::ptr::copy_nonoverlapping(to, packed.as_mut_ptr().add(2 + from_len), to_len);
        if exchange(channel, 23, source, packed.as_ptr(), &mut code, total, 1) == 1 {
            c_int::from(code)
        } else {
            5
        }
    }
}

#[unsafe(no_mangle)]
extern "C" fn witchy_broker_root_close(channel: c_int, writable: bool) -> c_int {
    unsafe { exchange(channel, 25, u32::from(writable), null(), null_mut(), 0, 0) }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_broker_exec_args(
    channel: c_int,
    bytes: *const u8,
    len: usize,
) -> c_int {
    if bytes.is_null() && len != 0 {
        return IO_ERROR;
    }
    unsafe { exchange(channel, 27, 0, bytes, null_mut(), len, 0) }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_broker_exec_input(
    channel: c_int,
    bytes: *const u8,
    len: usize,
) -> c_int {
    if bytes.is_null() && len != 0 {
        return IO_ERROR;
    }
    unsafe { exchange(channel, 30, 0, bytes, null_mut(), len, 0) }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_broker_exec_env(channel: c_int, bytes: *const u8, len: usize) -> c_int {
    if bytes.is_null() && len != 0 {
        return IO_ERROR;
    }
    unsafe { exchange(channel, 31, 0, bytes, null_mut(), len, 0) }
}

#[unsafe(no_mangle)]
extern "C" fn witchy_broker_exec_cwd(channel: c_int, source: u32, writable: bool) -> c_int {
    unsafe {
        exchange(
            channel,
            32,
            source,
            null(),
            null_mut(),
            usize::from(writable),
            0,
        )
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_broker_exec_run(
    channel: c_int,
    program: u32,
    timeout: u32,
    budget: u32,
    argc: u32,
    code: *mut i64,
    length: *mut u32,
    error_length: *mut u32,
) -> c_int {
    if code.is_null() || length.is_null() || error_length.is_null() {
        return 5;
    }
    let mut request = [0u8; 11];
    request[0] = program as u8;
    request[1..5].copy_from_slice(&timeout.to_be_bytes());
    request[5..9].copy_from_slice(&budget.to_be_bytes());
    request[9] = argc as u8;
    let mut answer = [0u8; 11];
    if unsafe {
        exchange(
            channel,
            28,
            0,
            request.as_ptr(),
            answer.as_mut_ptr(),
            11,
            11,
        )
    } != 11
    {
        return 5;
    }
    unsafe {
        *code = i64::from(u16::from_be_bytes([answer[1], answer[2]]));
        *length = u32::from_be_bytes([answer[3], answer[4], answer[5], answer[6]]);
        *error_length = u32::from_be_bytes([answer[7], answer[8], answer[9], answer[10]]);
    }
    i32::from(answer[0])
}

#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_broker_exec_out(
    channel: c_int,
    bytes: *mut u8,
    capacity: usize,
) -> c_int {
    if bytes.is_null() && capacity != 0 {
        return IO_ERROR;
    }
    unsafe { exchange(channel, 29, 0, null(), bytes, capacity, capacity) }
}

/// Flush a launcher-owned byte queue. The queue is consumed even if the
/// socket fails, so close or retry cannot resend a transmitted prefix.
#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_net_flush_queue(
    channel: c_int,
    slot: u32,
    mut bytes: *const u8,
    length: *mut usize,
) -> c_int {
    let mut left = unsafe { length.replace(0) };
    while left != 0 {
        let sent = unsafe { witchy_net_send(channel, slot, bytes, left) };
        if sent <= 0 || sent as usize > left {
            return IO_ERROR;
        }
        left -= sent as usize;
        bytes = unsafe { bytes.add(sent as usize) };
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsRawFd;
    use std::os::unix::net::UnixStream;

    #[test]
    fn transport_delivers_exact_frame_and_detects_eof() {
        let (sender, receiver) = UnixStream::pair().unwrap();
        let bytes = b"framed payload";
        let mut received = [0u8; 14];
        assert!(unsafe {
            witchy_broker_transfer(
                sender.as_raw_fd(),
                core::ptr::null_mut(),
                bytes.as_ptr(),
                bytes.len(),
            )
        });
        assert!(unsafe {
            witchy_broker_transfer(
                receiver.as_raw_fd(),
                received.as_mut_ptr(),
                core::ptr::null(),
                received.len(),
            )
        });
        assert_eq!(&received, bytes);
        drop(sender);
        assert!(!unsafe {
            witchy_broker_transfer(
                receiver.as_raw_fd(),
                received.as_mut_ptr(),
                core::ptr::null(),
                1,
            )
        });
    }

    #[test]
    fn rejects_oversized_or_inconsistent_broker_reply() {
        assert_eq!(response(3, [0, 0, 5], 4, true), (IO_ERROR, 0));
        assert_eq!(response(5, [3, 0, 0], 0, false), (TIMED_OUT, 0));
        assert_eq!(response(7, [4, 0, 0], 1, false), (WOULD_BLOCK, 0));
        assert_eq!(response(3, [0, 0, 4], 4, true), (4, 4));
    }
}
