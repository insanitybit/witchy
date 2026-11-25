//! Synchronous transport for the native broker's framed socket.

use core::ffi::{c_int, c_void};

unsafe extern "C" {
    fn read(fd: c_int, out: *mut c_void, len: usize) -> isize;
    #[cfg(target_os = "macos")]
    fn write(fd: c_int, bytes: *const c_void, len: usize) -> isize;
    #[cfg(target_os = "linux")]
    fn send(fd: c_int, bytes: *const c_void, len: usize, flags: c_int) -> isize;
}

const EINTR: c_int = 4;

/// Writes all of `bytes` to the descriptor, retrying interrupted calls.
pub(super) fn witchy_broker_send(fd: c_int, bytes: &[u8]) -> bool {
    let mut sent = 0;
    while sent < bytes.len() {
        let rest = &bytes[sent..];
        // SAFETY: `rest` is readable for its whole length.
        let count = unsafe {
            #[cfg(target_os = "macos")]
            {
                write(fd, rest.as_ptr().cast(), rest.len())
            }
            #[cfg(target_os = "linux")]
            {
                send(fd, rest.as_ptr().cast(), rest.len(), 0x4000)
            }
        };
        if count < 0 && super::errno::get() == EINTR {
            continue;
        }
        if count <= 0 || count as usize > rest.len() {
            return false;
        }
        sent += count as usize;
    }
    true
}

/// Fills all of `bytes` from the descriptor, retrying interrupted calls.
pub(super) fn witchy_broker_receive(fd: c_int, bytes: &mut [u8]) -> bool {
    let mut received = 0;
    while received < bytes.len() {
        let rest = &mut bytes[received..];
        // SAFETY: `rest` is writable for its whole length.
        let count = unsafe { read(fd, rest.as_mut_ptr().cast(), rest.len()) };
        if count < 0 && super::errno::get() == EINTR {
            continue;
        }
        if count <= 0 || count as usize > rest.len() {
            return false;
        }
        received += count as usize;
    }
    true
}

const IO_ERROR: c_int = -1;
const REFUSED: c_int = -2;
const TIMED_OUT: c_int = -3;
const WOULD_BLOCK: c_int = -4;

pub(super) fn witchy_broker_reply(channel: c_int, status: u8, length: usize) -> bool {
    let [hi, lo] = (length as u16).to_be_bytes();
    witchy_broker_send(channel, &[status, hi, lo])
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

/// What a request frame carries after its header.
enum Outgoing<'a> {
    /// Nothing; the header's length field is an operand or a wanted count.
    Count(usize),
    /// These bytes; the header's length field is their length.
    Bytes(&'a [u8]),
}
use Outgoing::{Bytes, Count};

/// A reply payload is read into `input`, and is refused if it would not fit.
/// The broker channel is exclusively held.
fn exchange(
    channel: c_int,
    operation: u8,
    slot: u32,
    out: Outgoing<'_>,
    input: Option<&mut [u8]>,
) -> c_int {
    let (length, out) = match out {
        Count(length) => (length, None),
        Bytes(out) => (out.len(), Some(out)),
    };
    if length > 16384 {
        return IO_ERROR;
    }
    let [hi, lo] = (length as u16).to_be_bytes();
    let request = [operation, hi, lo, slot as u8];
    let mut header = [0; 3];
    if !witchy_broker_send(channel, &request)
        || out.is_some_and(|out| !witchy_broker_send(channel, out))
        || !witchy_broker_receive(channel, &mut header)
    {
        return IO_ERROR;
    }
    let (result, payload) = response(operation, header, length, input.is_some());
    let Some(input) = input.unwrap_or(&mut []).get_mut(..payload) else {
        return IO_ERROR;
    };
    if witchy_broker_receive(channel, input) {
        result
    } else {
        IO_ERROR
    }
}

#[unsafe(no_mangle)]
extern "C" fn witchy_net_connect(channel: c_int, slot: u32, endpoint: u32) -> c_int {
    exchange(channel, 1, slot, Count(endpoint as usize), None)
}

#[unsafe(no_mangle)]
extern "C" fn witchy_net_accept(channel: c_int, slot: u32, wait: bool) -> c_int {
    exchange(channel, if wait { 5 } else { 6 }, slot, Count(0), None)
}

#[unsafe(no_mangle)]
extern "C" fn witchy_net_close(channel: c_int, slot: u32) -> c_int {
    exchange(channel, 4, slot, Count(0), None)
}

pub(super) fn witchy_net_send(channel: c_int, slot: u32, bytes: &[u8]) -> c_int {
    exchange(channel, 2, slot, Bytes(bytes), None)
}

pub(super) fn witchy_net_receive(channel: c_int, slot: u32, bytes: &mut [u8]) -> c_int {
    exchange(channel, 3, slot, Count(bytes.len()), Some(bytes))
}

pub(super) fn witchy_net_try_send(channel: c_int, slot: u32, bytes: &[u8]) -> c_int {
    exchange(channel, 7, slot, Bytes(bytes), None)
}

pub(super) fn witchy_net_try_receive(channel: c_int, slot: u32, bytes: &mut [u8]) -> c_int {
    exchange(channel, 8, slot, Count(bytes.len()), Some(bytes))
}

pub(super) fn witchy_net_poll(channel: c_int, entries: &mut [u8; 17], timeout: u32) -> c_int {
    let [hi, lo] = (timeout as u16).to_be_bytes();
    let mut packet = [0; 19];
    packet[0] = hi;
    packet[1] = lo;
    packet[2..].copy_from_slice(entries);
    let mut answer = [0u8; 17];
    if exchange(channel, 9, 0, Bytes(&packet), Some(&mut answer)) != 17 {
        return IO_ERROR;
    }
    *entries = answer;
    0
}

pub(super) fn witchy_broker_file_open(channel: c_int, slot: u32, path: &[u8], kind: u32) -> c_int {
    let operation = match kind {
        6 => 26,
        4 => 24,
        2 => 14,
        _ => 10,
    };
    exchange(channel, operation, slot, Bytes(path), None)
}

pub(super) fn witchy_broker_file_read(channel: c_int, slot: u32, bytes: &mut [u8]) -> c_int {
    exchange(channel, 11, slot, Count(bytes.len()), Some(bytes))
}

#[unsafe(no_mangle)]
extern "C" fn witchy_broker_file_close(channel: c_int, slot: u32) -> c_int {
    exchange(channel, 12, slot, Count(0), None)
}

pub(super) fn witchy_broker_file_probe(channel: c_int, path: &[u8]) -> c_int {
    let mut code = [0u8];
    let count = exchange(channel, 13, 0, Bytes(path), Some(&mut code));
    if count == 1 { c_int::from(code[0]) } else { 0 }
}

#[unsafe(no_mangle)]
extern "C" fn witchy_broker_directory_list(channel: c_int, slot: u32, source: u8) -> c_int {
    exchange(channel, 15, slot, Bytes(&[source]), None)
}

pub(super) fn witchy_broker_file_commit(channel: c_int, slot: u32, data: &[u8]) -> c_int {
    if data.len() > 1024 * 1024 {
        return IO_ERROR;
    }
    let [a, b, c, d] = (data.len() as u32).to_be_bytes();
    let request = [16, 0, 4, slot as u8, a, b, c, d];
    let mut status = [0; 3];
    if !witchy_broker_send(channel, &request)
        || !witchy_broker_send(channel, data)
        || !witchy_broker_receive(channel, &mut status)
    {
        return IO_ERROR;
    }
    match status {
        [0, 0, code @ 0..=6] => c_int::from(code),
        _ => IO_ERROR,
    }
}

pub(super) fn witchy_broker_directory_change(
    channel: c_int,
    source: u32,
    path: &[u8],
    remove: bool,
) -> c_int {
    let mut code = [0u8];
    let operation = if remove { 21 } else { 17 };
    let count = exchange(channel, operation, source, Bytes(path), Some(&mut code));
    if count == 1 { c_int::from(code[0]) } else { 5 }
}

pub(super) fn witchy_broker_output_open(
    channel: c_int,
    slot: u32,
    path: &[u8],
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
    exchange(channel, op, slot, Bytes(path), None)
}

#[unsafe(no_mangle)]
extern "C" fn witchy_broker_output_drop(channel: c_int, slot: u32) -> c_int {
    exchange(channel, 19, slot, Count(0), None)
}

pub(super) fn witchy_broker_rename(channel: c_int, source: u32, from: &[u8], to: &[u8]) -> c_int {
    let mut packed = [0u8; 16384];
    let Some(total) = from
        .len()
        .checked_add(to.len())
        .and_then(|len| len.checked_add(2))
    else {
        return 2;
    };
    if from.is_empty() || to.is_empty() || total >= packed.len() {
        return 2;
    }
    let [hi, lo] = (from.len() as u16).to_be_bytes();
    packed[0] = hi;
    packed[1] = lo;
    // Both ranges end by `total`, which is inside the packet.
    packed[2..2 + from.len()].copy_from_slice(from);
    packed[2 + from.len()..total].copy_from_slice(to);
    let mut code = [0u8];
    if exchange(channel, 23, source, Bytes(&packed[..total]), Some(&mut code)) == 1 {
        c_int::from(code[0])
    } else {
        5
    }
}

#[unsafe(no_mangle)]
extern "C" fn witchy_broker_root_close(channel: c_int, writable: bool) -> c_int {
    exchange(channel, 25, u32::from(writable), Count(0), None)
}

pub(super) fn witchy_broker_exec_args(channel: c_int, bytes: &[u8]) -> c_int {
    exchange(channel, 27, 0, Bytes(bytes), None)
}

pub(super) fn witchy_broker_exec_input(channel: c_int, bytes: &[u8]) -> c_int {
    exchange(channel, 30, 0, Bytes(bytes), None)
}

pub(super) fn witchy_broker_exec_env(channel: c_int, bytes: &[u8]) -> c_int {
    exchange(channel, 31, 0, Bytes(bytes), None)
}

#[unsafe(no_mangle)]
extern "C" fn witchy_broker_exec_cwd(channel: c_int, source: u32, writable: bool) -> c_int {
    exchange(channel, 32, source, Count(usize::from(writable)), None)
}

pub(super) fn witchy_broker_exec_run(
    channel: c_int,
    program: u32,
    timeout: u32,
    budget: u32,
    argc: u32,
    code: &mut i64,
    length: &mut u32,
    error_length: &mut u32,
) -> c_int {
    let mut request = [0u8; 11];
    request[0] = program as u8;
    request[1..5].copy_from_slice(&timeout.to_be_bytes());
    request[5..9].copy_from_slice(&budget.to_be_bytes());
    request[9] = argc as u8;
    let mut answer = [0u8; 11];
    if exchange(channel, 28, 0, Bytes(&request), Some(&mut answer)) != 11 {
        return 5;
    }
    *code = i64::from(u16::from_be_bytes([answer[1], answer[2]]));
    *length = u32::from_be_bytes([answer[3], answer[4], answer[5], answer[6]]);
    *error_length = u32::from_be_bytes([answer[7], answer[8], answer[9], answer[10]]);
    i32::from(answer[0])
}

pub(super) fn witchy_broker_exec_out(channel: c_int, bytes: &mut [u8]) -> c_int {
    exchange(channel, 29, 0, Count(bytes.len()), Some(bytes))
}

/// Flush a launcher-owned byte queue whose first `length` bytes are pending.
/// The queue is consumed even if the socket fails, so close or retry cannot
/// resend a transmitted prefix.
pub(super) fn witchy_net_flush_queue(
    channel: c_int,
    slot: u32,
    bytes: &[u8],
    length: &mut usize,
) -> c_int {
    let left = core::mem::replace(length, 0);
    let Some(mut bytes) = bytes.get(..left) else {
        return IO_ERROR;
    };
    while !bytes.is_empty() {
        let sent = witchy_net_send(channel, slot, bytes);
        if sent <= 0 || sent as usize > bytes.len() {
            return IO_ERROR;
        }
        bytes = &bytes[sent as usize..];
    }
    0
}

#[cfg(kani)]
mod proofs {
    use super::*;

    /// Whatever header the broker answers with, the payload the guest then
    /// reads is no longer than it asked for, and only when it asked for one.
    #[kani::proof]
    fn a_reply_payload_fits_the_request() {
        let operation: u8 = kani::any();
        let header: [u8; 3] = kani::any();
        let length: usize = kani::any();
        let read_payload: bool = kani::any();
        kani::assume(length <= 16384);
        let (result, payload) = response(operation, header, length, read_payload);
        assert!(result >= WOULD_BLOCK && result <= 65535);
        if payload != 0 {
            assert!(read_payload && result >= 0 && payload == result as usize);
            assert!(payload <= if operation == 13 { 1 } else { length });
        }
    }
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
        assert!(witchy_broker_send(sender.as_raw_fd(), bytes));
        assert!(witchy_broker_receive(receiver.as_raw_fd(), &mut received));
        assert_eq!(&received, bytes);
        drop(sender);
        assert!(!witchy_broker_receive(
            receiver.as_raw_fd(),
            &mut received[..1]
        ));
    }

    #[test]
    fn rejects_oversized_or_inconsistent_broker_reply() {
        assert_eq!(response(3, [0, 0, 5], 4, true), (IO_ERROR, 0));
        assert_eq!(response(5, [3, 0, 0], 0, false), (TIMED_OUT, 0));
        assert_eq!(response(7, [4, 0, 0], 1, false), (WOULD_BLOCK, 0));
        assert_eq!(response(3, [0, 0, 4], 4, true), (4, 4));
    }
}
