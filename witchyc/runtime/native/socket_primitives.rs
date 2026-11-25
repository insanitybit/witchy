//! Portable socket primitives for the native broker.
use core::{
    ffi::{c_int, c_void},
    ptr,
};

#[repr(C)]
struct Timeval {
    seconds: i64,
    #[cfg(target_os = "macos")]
    micros: i32,
    #[cfg(target_os = "linux")]
    micros: i64,
}
#[repr(C)]
pub(super) struct PollFd {
    pub(super) fd: c_int,
    pub(super) events: i16,
    pub(super) returned: i16,
}
#[cfg(target_os = "macos")]
type PollCount = u32;
#[cfg(target_os = "linux")]
type PollCount = usize;
unsafe extern "C" {
    fn setsockopt(fd: c_int, level: c_int, option: c_int, value: *const c_void, len: u32) -> c_int;
    fn poll(fds: *mut PollFd, count: PollCount, timeout: c_int) -> c_int;
    fn accept(fd: c_int, address: *mut c_void, length: *mut u32) -> c_int;
    fn fcntl(fd: c_int, op: c_int, ...) -> c_int;
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

unsafe fn family(address: *const u8) -> c_int {
    #[cfg(target_os = "macos")]
    {
        i32::from(unsafe { *address.add(1) })
    }
    #[cfg(target_os = "linux")]
    {
        i32::from(u16::from_ne_bytes(unsafe { [*address, *address.add(1)] }))
    }
}

pub(super) fn witchy_broker_set_timeout(fd: c_int) -> bool {
    let timeout = Timeval {
        seconds: 5,
        micros: 0,
    };
    let value = (&raw const timeout).cast();
    let len = size_of::<Timeval>() as u32;
    unsafe {
        setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, value, len) == 0
            && setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, value, len) == 0
    }
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
    (unsafe { poll(&mut ready, 1, 0) }) == 1 && ready.returned & POLLHUP != 0
}

/// The broker supplies a writable timeout flag.
pub(super) unsafe fn witchy_broker_accept_stream(
    listener: c_int,
    timeout: c_int,
    timed_out: *mut bool,
) -> c_int {
    const POLLIN: i16 = 1;
    const F_SETFL: c_int = 4;
    let mut ready = PollFd {
        fd: listener,
        events: POLLIN,
        returned: 0,
    };
    let result = unsafe { poll(&mut ready, 1, timeout) };
    unsafe { timed_out.write(result == 0) };
    if result != 1 {
        return -1;
    }
    let stream = unsafe { accept(listener, ptr::null_mut(), ptr::null_mut()) };
    if stream < 0 {
        return -1;
    }
    let blocking = unsafe { fcntl(stream, F_SETFL, 0 as c_int) } >= 0;
    if blocking && witchy_broker_set_timeout(stream) {
        return stream;
    }
    // Darwin can return EINVAL for socket options when both directions close.
    // Treat a confirmed hangup as an accept that produced no usable connection.
    #[cfg(target_os = "macos")]
    if blocking && accepted_peer_closed(stream) {
        unsafe { timed_out.write(true) };
    }
    close(stream);
    -1
}

/// The address points to a Darwin socket address initialized by the resolver.
pub(super) unsafe fn witchy_broker_dial(address: *const u8, length: u32) -> c_int {
    unsafe extern "C" {
        safe fn socket(family: c_int, kind: c_int, protocol: c_int) -> c_int;
        fn connect(fd: c_int, address: *const c_void, length: u32) -> c_int;
        fn getsockopt(
            fd: c_int,
            level: c_int,
            option: c_int,
            value: *mut c_void,
            len: *mut u32,
        ) -> c_int;
    }
    const SOCK_STREAM: c_int = 1;
    const F_GETFL: c_int = 3;
    const F_SETFL: c_int = 4;
    #[cfg(target_os = "macos")]
    const EINPROGRESS: c_int = 36;
    #[cfg(target_os = "linux")]
    const EINPROGRESS: c_int = 115;
    const POLLOUT: i16 = 4;
    if length < 2 {
        return -1;
    }
    let family = unsafe { family(address) };
    let fd = socket(family, SOCK_STREAM, 0);
    if fd < 0 {
        return -1;
    }
    let flags = unsafe { fcntl(fd, F_GETFL) };
    let connected = flags >= 0
        && unsafe { fcntl(fd, F_SETFL, flags | O_NONBLOCK) } >= 0
        && (unsafe { connect(fd, address.cast(), length) } == 0 || {
            let mut ready = PollFd {
                fd,
                events: POLLOUT,
                returned: 0,
            };
            let mut error = 0;
            let mut size = size_of::<c_int>() as u32;
            unsafe {
                super::errno::get() == EINPROGRESS
                    && poll(&mut ready, 1, 5000) == 1
                    && getsockopt(fd, SOL_SOCKET, SO_ERROR, (&raw mut error).cast(), &mut size) == 0
                    && error == 0
            }
        });
    if connected && unsafe { fcntl(fd, F_SETFL, flags) } >= 0 && witchy_broker_set_timeout(fd) {
        return fd;
    }
    close(fd);
    -1
}

/// The broker supplies 16 stream descriptors and 17 writable request bytes.
pub(super) unsafe fn witchy_broker_readiness(
    listener: c_int,
    streams: *const [c_int; 16],
    entries: *mut [u8; 17],
    timeout: c_int,
) -> bool {
    let entries = unsafe { &mut *entries };
    let streams = unsafe { &*streams };
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
    if unsafe { poll(fds.as_mut_ptr(), fds.len() as PollCount, timeout) < 0 && super::errno::get() != EINTR }
    {
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
    const F_SETFD: c_int = 2;
    const FD_CLOEXEC: c_int = 1;
    #[cfg(target_os = "macos")]
    const F_DUPFD_CLOEXEC: c_int = 67;
    #[cfg(target_os = "linux")]
    const F_DUPFD_CLOEXEC: c_int = 1030;
    if fd < 0 || (fd > 2 && unsafe { fcntl(fd, F_SETFD, FD_CLOEXEC) } == 0) {
        return fd;
    }
    let moved = unsafe { fcntl(fd, F_DUPFD_CLOEXEC, 3 as c_int) };
    close(fd);
    moved
}

/// The broker supplies 128 writable bytes of aligned sockaddr storage
/// and a writable bound-port output.
pub(super) unsafe fn witchy_broker_bind_listener(
    address: *mut u8,
    mut len: u32,
    port: *mut u16,
) -> c_int {
    unsafe extern "C" {
        safe fn socket(family: c_int, kind: c_int, protocol: c_int) -> c_int;
        fn bind(fd: c_int, address: *const c_void, len: u32) -> c_int;
        safe fn listen(fd: c_int, backlog: c_int) -> c_int;
        fn getsockname(fd: c_int, address: *mut c_void, len: *mut u32) -> c_int;
    }
    const AF_INET: c_int = 2;
    const IPPROTO_IPV6: c_int = 41;
    #[cfg(target_os = "macos")]
    const IPV6_V6ONLY: c_int = 27;
    #[cfg(target_os = "linux")]
    const IPV6_V6ONLY: c_int = 26;
    const SOCK_STREAM: c_int = 1;
    const F_SETFL: c_int = 4;
    if !(4..=128).contains(&len) {
        return -1;
    }
    let family = unsafe { family(address) };
    if !matches!(family, AF_INET | AF_INET6) {
        return -1;
    }
    let fd = witchy_broker_private_descriptor(socket(family, SOCK_STREAM, 0));
    if fd < 0 {
        return -1;
    }
    let on: c_int = 1;
    let ok = unsafe {
        (family != AF_INET6
            || setsockopt(
                fd,
                IPPROTO_IPV6,
                IPV6_V6ONLY,
                (&raw const on).cast(),
                size_of::<c_int>() as u32,
            ) == 0)
            && bind(fd, address.cast(), len) == 0
            && listen(fd, 16) == 0
            && fcntl(fd, F_SETFL, O_NONBLOCK) == 0
            && getsockname(fd, address.cast(), &mut len) == 0
    };
    if !ok || len < 4 {
        close(fd);
        return -1;
    }
    // Both Darwin sockaddr_in variants store the network-order port at byte 2.
    unsafe {
        port.write(u16::from_be_bytes([
            address.add(2).read(),
            address.add(3).read(),
        ]))
    };
    fd
}

/// The launcher provides a C string and writable sockaddr storage.
pub(super) unsafe fn witchy_broker_endpoint_address(
    text: *const core::ffi::c_char,
    address: *mut u8,
    allow_zero: bool,
) -> u32 {
    unsafe extern "C" {
        fn inet_pton(family: c_int, text: *const core::ffi::c_char, address: *mut c_void) -> c_int;
    }
    let bytes = unsafe { core::ffi::CStr::from_ptr(text) }.to_bytes();
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
    let [hi, lo] = port.to_be_bytes();
    #[repr(align(4))]
    struct V4([u8; 16]);
    let mut ipv4 = V4([0; 16]);
    #[cfg(target_os = "macos")]
    {
        ipv4.0[0] = 16;
        ipv4.0[1] = 2;
    }
    #[cfg(target_os = "linux")]
    {
        ipv4.0[..2].copy_from_slice(&2u16.to_ne_bytes());
    }
    ipv4.0[2] = hi;
    ipv4.0[3] = lo;
    if !bracketed
        && unsafe { inet_pton(2, name.as_ptr().cast(), ipv4.0.as_mut_ptr().add(4).cast()) } == 1
    {
        unsafe { ptr::copy_nonoverlapping(ipv4.0.as_ptr(), address, 16) };
        return 16;
    }
    #[repr(align(4))]
    struct V6([u8; 28]);
    let mut ipv6 = V6([0; 28]);
    #[cfg(target_os = "macos")]
    {
        ipv6.0[0] = 28;
        ipv6.0[1] = 30;
    }
    #[cfg(target_os = "linux")]
    {
        ipv6.0[..2].copy_from_slice(&(AF_INET6 as u16).to_ne_bytes());
    }
    ipv6.0[2] = hi;
    ipv6.0[3] = lo;
    if unsafe {
        inet_pton(
            AF_INET6,
            name.as_ptr().cast(),
            ipv6.0.as_mut_ptr().add(8).cast(),
        )
    } == 1
    {
        unsafe { ptr::copy_nonoverlapping(ipv6.0.as_ptr(), address, 28) };
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
        let size = unsafe {
            witchy_broker_endpoint_address(c"127.0.0.1:0".as_ptr(), address.as_mut_ptr(), true)
        };
        assert_eq!(size, 16);
        let mut port = 0;
        let listener =
            unsafe { witchy_broker_bind_listener(address.as_mut_ptr(), size, &mut port) };
        assert!(listener >= 0);
        assert_ne!(port, 0);
        let dial = unsafe { witchy_broker_dial(address.as_ptr(), size) };
        assert!(dial >= 0);
        let mut timed_out = false;
        let accepted = unsafe { witchy_broker_accept_stream(listener, 1000, &mut timed_out) };
        assert!(accepted >= 0 && !timed_out);
        unsafe {
            close(dial);
            close(accepted);
            close(listener);
        }
    }

    #[test]
    fn endpoint_parser_checks_ipv6_and_port() {
        let mut address = [0u8; 128];
        assert_eq!(
            unsafe {
                witchy_broker_endpoint_address(c"[::1]:443".as_ptr(), address.as_mut_ptr(), false)
            },
            28
        );
        assert_eq!(unsafe { family(address.as_ptr()) }, AF_INET6);
        assert_eq!(&address[2..4], &443u16.to_be_bytes());
        assert_eq!(
            unsafe {
                witchy_broker_endpoint_address(c"127.0.0.1:0".as_ptr(), address.as_mut_ptr(), false)
            },
            0
        );
    }
}
