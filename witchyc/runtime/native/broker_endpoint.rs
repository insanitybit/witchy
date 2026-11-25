//! Resolve numeric and DNS endpoints before the broker is confined.
use super::socket_primitives::witchy_broker_endpoint_address;
use core::{
    ffi::{CStr, c_int},
    ptr,
};

fn dns_endpoint(text: &[u8]) -> Option<(&[u8], u16)> {
    let colon = text.iter().rposition(|&byte| byte == b':')?;
    let (host, tail) = text.split_at_checked(colon)?;
    let digits = tail.get(1..)?;
    if host.is_empty() || host.len() >= 254 || digits.is_empty() {
        return None;
    }
    let mut has_letter = false;
    let mut label_len = 0;
    let mut last = 0;
    for &byte in host {
        let letter = byte.is_ascii_alphabetic();
        has_letter |= letter;
        if byte == b'.' {
            if label_len == 0 || last == b'-' {
                return None;
            }
            label_len = 0;
        } else if letter || byte.is_ascii_digit() || (byte == b'-' && label_len != 0) {
            label_len += 1;
            if label_len > 63 {
                return None;
            }
        } else {
            return None;
        }
        last = byte;
    }
    if !has_letter || last == b'-' {
        return None;
    }
    let port = digits.iter().try_fold(0u16, |value, &byte| {
        if !byte.is_ascii_digit() {
            return None;
        }
        value.checked_mul(10)?.checked_add(u16::from(byte - b'0'))
    })?;
    (port != 0).then_some((host, port))
}

pub(super) fn witchy_net_same_endpoint(left: &CStr, right: &CStr) -> bool {
    let mut a = [0u8; 28];
    let mut b = [0u8; 28];
    let len = witchy_broker_endpoint_address(left, &mut a, false);
    if len == 0 {
        let left = dns_endpoint(left.to_bytes());
        let right = dns_endpoint(right.to_bytes());
        return left
            .zip(right)
            .is_some_and(|((name, port), (other, other_port))| {
                port == other_port && name.eq_ignore_ascii_case(other)
            });
    }
    witchy_broker_endpoint_address(right, &mut b, false) == len && a == b
}

// sockaddr_storage is 128 bytes with eight-byte alignment on both targets.
#[derive(Clone, Copy)]
#[repr(C, align(8))]
pub(super) struct Address(pub(super) [u8; 128]);

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct Endpoint {
    pub(super) addresses: [Address; 8],
    pub(super) lengths: [u32; 8],
    pub(super) count: u32,
}

impl Endpoint {
    /// No address: dialing it fails.
    pub(super) const EMPTY: Self = Self {
        addresses: [Address([0; 128]); 8],
        lengths: [0; 8],
        count: 0,
    };
}

#[repr(C)]
struct AddrInfo {
    flags: c_int,
    family: c_int,
    socket_type: c_int,
    protocol: c_int,
    len: u32,
    #[cfg(target_os = "macos")]
    canonical: *mut core::ffi::c_char,
    address: *mut u8,
    #[cfg(target_os = "linux")]
    canonical: *mut core::ffi::c_char,
    next: *mut AddrInfo,
}

/// The addresses `text` names, numeric or by DNS, or `None` when it names none.
pub(super) fn witchy_broker_resolve_endpoint(text: &CStr) -> Option<Endpoint> {
    use core::ffi::c_char;
    unsafe extern "C" {
        fn getaddrinfo(
            host: *const c_char,
            service: *const c_char,
            hints: *const AddrInfo,
            result: *mut *mut AddrInfo,
        ) -> c_int;
        fn freeaddrinfo(result: *mut AddrInfo);
    }
    let mut endpoint = Endpoint::EMPTY;
    let first = endpoint.addresses.first_mut()?;
    let length = witchy_broker_endpoint_address(text, &mut first.0, false);
    if length != 0 {
        endpoint.lengths = [length, 0, 0, 0, 0, 0, 0, 0];
        endpoint.count = 1;
        return Some(endpoint);
    }
    let (host, mut port) = dns_endpoint(text.to_bytes())?;
    let mut hostname = [0u8; 254];
    for (dest, &byte) in hostname.iter_mut().zip(host) {
        *dest = byte;
    }
    let mut service = [0u8; 6];
    let mut start = 5;
    if let Some((_, digits)) = service.split_last_mut() {
        for digit in digits.iter_mut().rev() {
            *digit = b'0' + (port % 10) as u8;
            start -= 1;
            port /= 10;
            if port == 0 {
                break;
            }
        }
    }
    let service = service.get(start..)?;
    #[cfg(target_os = "macos")]
    const NUMERIC_SERVICE: c_int = 0x1000;
    #[cfg(target_os = "linux")]
    const NUMERIC_SERVICE: c_int = 0x400;
    #[cfg(target_os = "macos")]
    const INET6: c_int = 30;
    #[cfg(target_os = "linux")]
    const INET6: c_int = 10;
    let hints = AddrInfo {
        flags: NUMERIC_SERVICE,
        family: 0,
        socket_type: 1,
        protocol: 6,
        len: 0,
        canonical: ptr::null_mut(),
        address: ptr::null_mut(),
        next: ptr::null_mut(),
    };
    let mut answers = ptr::null_mut();
    if unsafe {
        getaddrinfo(
            hostname.as_ptr().cast(),
            service.as_ptr().cast(),
            &hints,
            &mut answers,
        )
    } != 0
    {
        return None;
    }
    let mut answer = answers;
    for (address, length) in endpoint.addresses.iter_mut().zip(&mut endpoint.lengths) {
        while let Some(info) = unsafe { answer.as_ref() } {
            answer = info.next;
            if !matches!(info.family, 2 | INET6)
                || info.len == 0
                || info.len > 128
                || info.address.is_null()
            {
                continue;
            }
            unsafe {
                ptr::copy_nonoverlapping(info.address, address.0.as_mut_ptr(), info.len as usize)
            };
            *length = info.len;
            endpoint.count += 1;
            break;
        }
        if answer.is_null() {
            break;
        }
    }
    if !answers.is_null() {
        unsafe { freeaddrinfo(answers) };
    }
    if endpoint.count == 0 {
        return None;
    }
    Some(endpoint)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_and_localhost_dns_resolve_with_correct_abi() {
        for name in [c"127.0.0.1:443", c"localhost:443"] {
            let endpoint = witchy_broker_resolve_endpoint(name).unwrap();
            assert!(endpoint.count > 0 && endpoint.count <= 8);
            for (address, &len) in endpoint
                .addresses
                .iter()
                .zip(&endpoint.lengths)
                .take(endpoint.count as usize)
            {
                assert!(len == 16 || len == 28);
                assert_eq!(&address.0[2..4], &443u16.to_be_bytes());
            }
        }
    }

    #[test]
    fn endpoint_identity_normalizes_dns_names_and_ports() {
        assert!(witchy_net_same_endpoint(c"LOCALHOST:80", c"localhost:080"));
        assert!(!witchy_net_same_endpoint(c"localhost:80", c"localhost:81"));
        assert!(!witchy_net_same_endpoint(c"bad/name:80", c"bad/name:80"));
    }
}
