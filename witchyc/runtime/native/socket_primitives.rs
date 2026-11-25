//! Portable socket primitives for the native broker.
use super::sys;
use core::ffi::{CStr, c_int};

#[repr(C)]
struct Timeval {
    seconds: i64,
    #[cfg(target_os = "macos")]
    micros: i32,
    #[cfg(target_os = "linux")]
    micros: i64,
}
pub(super) use super::sys::PollFd;
unsafe extern "C" {
    safe fn close(fd: c_int) -> c_int;
}

#[cfg(target_os = "macos")]
const SOL_SOCKET: c_int = 0xffff;
#[cfg(target_os = "linux")]
const SOL_SOCKET: c_int = 1;
#[cfg(target_os = "macos")]
const SO_RCVTIMEO: c_int = 0x1006;
#[cfg(target_os = "linux")]
const SO_RCVTIMEO: c_int = 20;
#[cfg(target_os = "macos")]
const SO_SNDTIMEO: c_int = 0x1005;
#[cfg(target_os = "linux")]
const SO_SNDTIMEO: c_int = 21;
#[cfg(target_os = "macos")]
const SO_ERROR: c_int = 0x1007;
#[cfg(target_os = "linux")]
const SO_ERROR: c_int = 4;
#[cfg(target_os = "macos")]
const O_NONBLOCK: c_int = 4;
#[cfg(target_os = "linux")]
const O_NONBLOCK: c_int = 0o4000;
#[cfg(target_os = "macos")]
const AF_INET6: c_int = 30;
#[cfg(target_os = "linux")]
const AF_INET6: c_int = 10;

/// The address family, from the first two bytes of a socket address.
fn family(head: [u8; 2]) -> c_int {
    #[cfg(target_os = "macos")]
    {
        i32::from(head[1])
    }
    #[cfg(target_os = "linux")]
    {
        i32::from(u16::from_ne_bytes(head))
    }
}

pub(super) fn witchy_broker_set_timeout(fd: c_int) -> bool {
    let timeout = Timeval {
        seconds: 5,
        micros: 0,
    };
    sys::setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &timeout) == 0
        && sys::setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &timeout) == 0
}

#[cfg(target_os = "macos")]
fn accepted_peer_closed(fd: c_int) -> bool {
    const EINVAL: c_int = 22;
    const POLLIN: i16 = 1;
    const POLLHUP: i16 = 0x10;
    if super::errno::get() != EINVAL {
        return false;
    }
    let mut ready = PollFd {
        fd,
        events: POLLIN,
        returned: 0,
    };
    sys::poll(core::slice::from_mut(&mut ready), 0) == 1 && ready.returned & POLLHUP != 0
}

/// Sets `timed_out` when no connection came within `timeout`.
pub(super) fn witchy_broker_accept_stream(
    listener: c_int,
    timeout: c_int,
    timed_out: &mut bool,
) -> c_int {
    const POLLIN: i16 = 1;
    let mut ready = PollFd {
        fd: listener,
        events: POLLIN,
        returned: 0,
    };
    let result = sys::poll(core::slice::from_mut(&mut ready), timeout);
    *timed_out = result == 0;
    if result != 1 {
        return -1;
    }
    let stream = sys::accept(listener);
    if stream < 0 {
        return -1;
    }
    let blocking = sys::set_status_flags(stream, 0) >= 0;
    if blocking && witchy_broker_set_timeout(stream) {
        return stream;
    }
    // Darwin can return EINVAL for socket options when both directions close.
    // Treat a confirmed hangup as an accept that produced no usable connection.
    #[cfg(target_os = "macos")]
    if blocking && accepted_peer_closed(stream) {
        *timed_out = true;
    }
    close(stream);
    -1
}

/// `address` is all of a socket address the resolver initialized.
pub(super) fn witchy_broker_dial(address: &[u8]) -> c_int {
    unsafe extern "C" {
        safe fn socket(family: c_int, kind: c_int, protocol: c_int) -> c_int;
    }
    const SOCK_STREAM: c_int = 1;
    #[cfg(target_os = "macos")]
    const EINPROGRESS: c_int = 36;
    #[cfg(target_os = "linux")]
    const EINPROGRESS: c_int = 115;
    const POLLOUT: i16 = 4;
    let &[first, second, ..] = address else {
        return -1;
    };
    let family = family([first, second]);
    let fd = socket(family, SOCK_STREAM, 0);
    if fd < 0 {
        return -1;
    }
    let flags = sys::status_flags(fd);
    let connected = flags >= 0
        && sys::set_status_flags(fd, flags | O_NONBLOCK) >= 0
        && (sys::connect(fd, address) == 0 || {
            let mut ready = PollFd {
                fd,
                events: POLLOUT,
                returned: 0,
            };
            let mut error = 0;
            super::errno::get() == EINPROGRESS
                && sys::poll(core::slice::from_mut(&mut ready), 5000) == 1
                && sys::getsockopt_int(fd, SOL_SOCKET, SO_ERROR, &mut error) == 0
                && error == 0
        });
    if connected && sys::set_status_flags(fd, flags) >= 0 && witchy_broker_set_timeout(fd) {
        return fd;
    }
    close(fd);
    -1
}

/// One request byte for the listener and one for each stream, each replaced
/// by what is ready.
pub(super) fn witchy_broker_readiness(
    listener: c_int,
    streams: &[c_int; 16],
    entries: &mut [u8; 17],
    timeout: c_int,
) -> bool {
    let mut fds = [const {
        PollFd {
            fd: -1,
            events: 0,
            returned: 0,
        }
    }; 17];
    let descriptors = core::iter::once(listener).chain(streams.iter().copied());
    for ((fd, &wanted), pollfd) in descriptors.zip(entries.iter()).zip(&mut fds) {
        if wanted > 7 || (wanted & 4 != 0 && fd < 0) {
            return false;
        }
        pollfd.fd = if wanted & 4 != 0 { fd } else { -1 };
        pollfd.events = i16::from(wanted & 1) | (i16::from(wanted & 2) << 1);
    }
    const EINTR: c_int = 4;
    if sys::poll(&mut fds, timeout) < 0 && super::errno::get() != EINTR {
        return false;
    }
    for (entry, fd) in entries.iter_mut().zip(&fds) {
        *entry = (if fd.returned & 1 != 0 { 4 } else { 0 })
            | (if fd.returned & 4 != 0 { 8 } else { 0 })
            | (if fd.returned & (8 | 16 | 32) != 0 {
                16
            } else {
                0
            });
    }
    true
}

pub(super) fn witchy_broker_private_descriptor(fd: c_int) -> c_int {
    const FD_CLOEXEC: c_int = 1;
    if fd < 0 || (fd > 2 && sys::set_descriptor_flags(fd, FD_CLOEXEC) == 0) {
        return fd;
    }
    let moved = sys::duplicate_close_on_exec(fd, 3);
    close(fd);
    moved
}

/// Binds the socket address in the first `len` bytes of `address`, replaces
/// it with the address bound, and writes the bound port on success.
pub(super) fn witchy_broker_bind_listener(
    address: &mut [u8; 128],
    mut len: u32,
    port: &mut u16,
) -> c_int {
    unsafe extern "C" {
        safe fn socket(family: c_int, kind: c_int, protocol: c_int) -> c_int;
        safe fn listen(fd: c_int, backlog: c_int) -> c_int;
    }
    const AF_INET: c_int = 2;
    const IPPROTO_IPV6: c_int = 41;
    #[cfg(target_os = "macos")]
    const IPV6_V6ONLY: c_int = 27;
    #[cfg(target_os = "linux")]
    const IPV6_V6ONLY: c_int = 26;
    const SOCK_STREAM: c_int = 1;
    if !(4..=128).contains(&len) {
        return -1;
    }
    let family = family([address[0], address[1]]);
    if !matches!(family, AF_INET | AF_INET6) {
        return -1;
    }
    let fd = witchy_broker_private_descriptor(socket(family, SOCK_STREAM, 0));
    if fd < 0 {
        return -1;
    }
    let on: c_int = 1;
    // The range check above keeps `len` within the storage.
    let bound = &mut address[..len as usize];
    let ok = (family != AF_INET6 || sys::setsockopt(fd, IPPROTO_IPV6, IPV6_V6ONLY, &on) == 0)
        && sys::bind(fd, bound) == 0
        && listen(fd, 16) == 0
        && sys::set_status_flags(fd, O_NONBLOCK) == 0
        && sys::getsockname(fd, bound, &mut len) == 0;
    if !ok || len < 4 {
        close(fd);
        return -1;
    }
    // Both Darwin sockaddr_in variants store the network-order port at byte 2.
    *port = u16::from_be_bytes([address[2], address[3]]);
    fd
}

/// Writes the socket address `text` names to the start of `address` and
/// returns its length, or 0. An IPv6 address needs 28 bytes of `address`.
pub(super) fn witchy_broker_endpoint_address(
    text: &CStr,
    address: &mut [u8],
    allow_zero: bool,
) -> u32 {
    let bytes = text.to_bytes();
    let Some(colon) = bytes.iter().rposition(|&byte| byte == b':') else {
        return 0;
    };
    let Some((host, tail)) = bytes.split_at_checked(colon) else {
        return 0;
    };
    let Some(digits) = tail.get(1..) else {
        return 0;
    };
    let bracketed = host.starts_with(b"[");
    let host = if bracketed {
        let Some(inner) = host
            .strip_prefix(b"[")
            .and_then(|value| value.strip_suffix(b"]"))
        else {
            return 0;
        };
        inner
    } else {
        host
    };
    if host.is_empty() || host.len() >= 46 || digits.is_empty() {
        return 0;
    }
    let Some(port) = digits.iter().try_fold(0u16, |port, &byte| {
        if !byte.is_ascii_digit() {
            return None;
        }
        port.checked_mul(10)?.checked_add(u16::from(byte - b'0'))
    }) else {
        return 0;
    };
    if port == 0 && !allow_zero {
        return 0;
    }
    let mut name = [0u8; 46];
    for (dest, &byte) in name.iter_mut().zip(host) {
        *dest = byte;
    }
    // The host is shorter than `name` and holds no NUL, so this is the host.
    let Ok(name) = CStr::from_bytes_until_nul(&name) else {
        return 0;
    };
    let [hi, lo] = port.to_be_bytes();
    let mut ipv4 = [0u8; 16];
    #[cfg(target_os = "macos")]
    {
        ipv4[0] = 16;
        ipv4[1] = 2;
    }
    #[cfg(target_os = "linux")]
    {
        ipv4[..2].copy_from_slice(&2u16.to_ne_bytes());
    }
    ipv4[2] = hi;
    ipv4[3] = lo;
    let mut host4 = [0u8; 4];
    if !bracketed && sys::inet_pton_v4(name, &mut host4) == 1 {
        ipv4[4..8].copy_from_slice(&host4);
        let Some(out) = address.get_mut(..16) else {
            return 0;
        };
        out.copy_from_slice(&ipv4);
        return 16;
    }
    let mut ipv6 = [0u8; 28];
    #[cfg(target_os = "macos")]
    {
        ipv6[0] = 28;
        ipv6[1] = 30;
    }
    #[cfg(target_os = "linux")]
    {
        ipv6[..2].copy_from_slice(&(AF_INET6 as u16).to_ne_bytes());
    }
    ipv6[2] = hi;
    ipv6[3] = lo;
    let mut host6 = [0u8; 16];
    if sys::inet_pton_v6(name, &mut host6) == 1 {
        ipv6[8..24].copy_from_slice(&host6);
        let Some(out) = address.get_mut(..28) else {
            return 0;
        };
        out.copy_from_slice(&ipv6);
        return 28;
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_listener_and_dial_use_native_sockaddr_layout() {
        let mut address = [0u8; 128];
        let size = witchy_broker_endpoint_address(c"127.0.0.1:0", &mut address, true);
        assert_eq!(size, 16);
        let mut port = 0;
        let listener = witchy_broker_bind_listener(&mut address, size, &mut port);
        assert!(listener >= 0);
        assert_ne!(port, 0);
        let dial = witchy_broker_dial(&address[..size as usize]);
        assert!(dial >= 0);
        let mut timed_out = false;
        let accepted = witchy_broker_accept_stream(listener, 1000, &mut timed_out);
        assert!(accepted >= 0 && !timed_out);
        close(dial);
        close(accepted);
        close(listener);
    }

    #[test]
    fn endpoint_parser_checks_ipv6_and_port() {
        let mut address = [0u8; 128];
        assert_eq!(witchy_broker_endpoint_address(c"[::1]:443", &mut address, false), 28);
        assert_eq!(family([address[0], address[1]]), AF_INET6);
        assert_eq!(&address[2..4], &443u16.to_be_bytes());
        assert_eq!(witchy_broker_endpoint_address(c"127.0.0.1:0", &mut address, false), 0);
    }
}
