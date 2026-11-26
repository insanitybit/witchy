//! Framed communication with the Linux Exec runner. A working directory is
//! transferred as a descriptor, never resolved from a guest-controlled path.
use core::{
    ffi::{c_int, c_void},
    ptr,
};

#[repr(C)]
struct Iovec {
    base: *mut c_void,
    len: usize,
}
#[repr(C)]
struct Message {
    name: *mut c_void,
    name_len: u32,
    iov: *mut Iovec,
    iov_len: usize,
    control: *mut c_void,
    control_len: usize,
    flags: c_int,
}
#[repr(C)]
struct Control {
    len: usize,
    level: c_int,
    kind: c_int,
    fd: c_int,
    pad: c_int,
}
// Match the glibc layout checked by the Linux C ABI probe.
const _: () = {
    use core::mem::{align_of, offset_of};

    assert!(size_of::<Iovec>() == 16 && align_of::<Iovec>() == 8);
    assert!(offset_of!(Iovec, base) == 0);
    assert!(offset_of!(Iovec, len) == 8);
    assert!(size_of::<Message>() == 56 && align_of::<Message>() == 8);
    assert!(offset_of!(Message, name) == 0);
    assert!(offset_of!(Message, name_len) == 8);
    assert!(offset_of!(Message, iov) == 16);
    assert!(offset_of!(Message, iov_len) == 24);
    assert!(offset_of!(Message, control) == 32);
    assert!(offset_of!(Message, control_len) == 40);
    assert!(offset_of!(Message, flags) == 48);
    assert!(size_of::<Control>() == 24 && align_of::<Control>() == 8);
    assert!(offset_of!(Control, len) == 0);
    assert!(offset_of!(Control, level) == 8);
    assert!(offset_of!(Control, kind) == 12);
    assert!(offset_of!(Control, fd) == 16);
    assert!(offset_of!(Control, pad) == 20);
};

unsafe extern "C" {
    fn sendmsg(fd: c_int, msg: *const Message, flags: c_int) -> isize;
    fn recvmsg(fd: c_int, msg: *mut Message, flags: c_int) -> isize;
    safe fn close(fd: c_int) -> c_int;
}
const EINTR: c_int = 4;
const MSG_NOSIGNAL: c_int = 0x4000;
const MSG_CMSG_CLOEXEC: c_int = 0x4000_0000;
const MSG_CTRUNC: c_int = 8;

pub(super) fn read_exact(fd: c_int, mut data: &mut [u8]) -> bool {
    while !data.is_empty() {
        let n = super::sys::read(fd, data);
        if n < 0 && super::errno::get() == EINTR {
            continue;
        }
        if n <= 0 || n as usize > data.len() {
            return false;
        }
        data = &mut data[n as usize..];
    }
    true
}

pub(super) fn send_all(fd: c_int, mut data: &[u8]) -> bool {
    while !data.is_empty() {
        let n = super::sys::send(fd, data, MSG_NOSIGNAL);
        if n < 0 && super::errno::get() == EINTR {
            continue;
        }
        if n <= 0 || n as usize > data.len() {
            return false;
        }
        data = &data[n as usize..];
    }
    true
}

/// The caller retains `passed`; ownership of a received descriptor transfers
/// to the receiver. A partial head is a failed request and closes the channel.
pub(super) fn send_head(fd: c_int, head: &[u8; 9], passed: c_int) -> bool {
    let mut iov = Iovec {
        base: head.as_ptr().cast_mut().cast(),
        len: head.len(),
    };
    let mut control = Control {
        len: 20,
        level: 1,
        kind: 1,
        fd: passed,
        pad: 0,
    };
    let message = Message {
        name: ptr::null_mut(),
        name_len: 0,
        iov: &raw mut iov,
        iov_len: 1,
        control: (&raw mut control).cast(),
        control_len: size_of::<Control>(),
        flags: 0,
    };
    loop {
        let sent = unsafe { sendmsg(fd, &raw const message, MSG_NOSIGNAL) };
        if sent < 0 && super::errno::get() == EINTR {
            continue;
        }
        return sent == head.len() as isize;
    }
}

pub(super) fn receive_head(fd: c_int, head: &mut [u8; 9]) -> Result<Option<c_int>, ()> {
    let mut iov = Iovec {
        base: head.as_mut_ptr().cast(),
        len: head.len(),
    };
    let mut control = Control {
        len: 0,
        level: 0,
        kind: 0,
        fd: -1,
        pad: 0,
    };
    let mut message = Message {
        name: ptr::null_mut(),
        name_len: 0,
        iov: &raw mut iov,
        iov_len: 1,
        control: (&raw mut control).cast(),
        control_len: size_of::<Control>(),
        flags: 0,
    };
    let received = loop {
        let n = unsafe { recvmsg(fd, &raw mut message, MSG_CMSG_CLOEXEC) };
        if n < 0 && super::errno::get() == EINTR {
            continue;
        }
        break n;
    };
    if received <= 0 || received as usize > head.len() {
        return Err(());
    }
    let mut descriptor = if message.control_len == 0 {
        None
    } else if message.control_len >= 20
        && control.len == 20
        && control.level == 1
        && control.kind == 1
        && control.fd >= 0
    {
        Some(control.fd)
    } else {
        // If a rights header contains a descriptor, don't leak it on refusal.
        if message.control_len >= 20 && control.level == 1 && control.kind == 1 {
            if control.len >= 20 && control.fd >= 0 {
                close(control.fd);
            }
            if control.len >= 24 && message.control_len >= 24 && control.pad >= 0 {
                close(control.pad);
            }
        }
        return Err(());
    };
    if message.flags & MSG_CTRUNC != 0 || !read_exact(fd, &mut head[received as usize..]) {
        if let Some(descriptor) = descriptor {
            close(descriptor);
        }
        return Err(());
    }
    if let Some(received) = descriptor.filter(|&number| number <= 2) {
        let private = super::sys::duplicate_close_on_exec(received, 3);
        close(received);
        if private < 0 {
            return Err(());
        }
        descriptor = Some(private);
    }
    Ok(descriptor)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::net::UnixStream;

    #[test]
    fn transfers_directory_descriptor_and_sets_cloexec() {
        unsafe extern "C" {
            fn fcntl(fd: c_int, command: c_int, ...) -> c_int;
        }
        let (left, right) = UnixStream::pair().unwrap();
        let path = std::env::temp_dir().join(format!("witchy-exec-fd-{}", std::process::id()));
        std::fs::write(&path, b"held").unwrap();
        let file = std::fs::File::open(&path).unwrap();
        let expected = *b"directory";
        assert!(send_head(left.as_raw_fd(), &expected, file.as_raw_fd()));
        let mut head = [0; 9];
        let fd = receive_head(right.as_raw_fd(), &mut head).unwrap().unwrap();
        assert_eq!(head, expected);
        assert_eq!(unsafe { fcntl(fd, 1) } & 1, 1);
        let mut received = unsafe { std::fs::File::from_raw_fd(fd) };
        let mut text = String::new();
        received.read_to_string(&mut text).unwrap();
        assert_eq!(text, "held");
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn framed_io_handles_partial_head_and_eof() {
        let (left, right) = UnixStream::pair().unwrap();
        assert!(send_all(left.as_raw_fd(), b"123456789"));
        let mut head = [0; 9];
        assert_eq!(receive_head(right.as_raw_fd(), &mut head), Ok(None));
        assert_eq!(&head, b"123456789");
        drop(left);
        assert_eq!(receive_head(right.as_raw_fd(), &mut head), Err(()));
    }

    #[test]
    fn refuses_multiple_descriptors_without_leaking_them() {
        let (left, right) = UnixStream::pair().unwrap();
        let file = std::fs::File::open("/dev/null").unwrap();
        let count = || std::fs::read_dir("/proc/self/fd").unwrap().count();
        let before = count();
        let mut head = *b"directory";
        let mut iov = Iovec {
            base: head.as_mut_ptr().cast(),
            len: head.len(),
        };
        let mut control = Control {
            len: 24,
            level: 1,
            kind: 1,
            fd: file.as_raw_fd(),
            pad: file.as_raw_fd(),
        };
        let message = Message {
            name: ptr::null_mut(),
            name_len: 0,
            iov: &raw mut iov,
            iov_len: 1,
            control: (&raw mut control).cast(),
            control_len: size_of::<Control>(),
            flags: 0,
        };
        assert_eq!(
            unsafe { sendmsg(left.as_raw_fd(), &raw const message, MSG_NOSIGNAL) },
            9
        );
        assert_eq!(receive_head(right.as_raw_fd(), &mut head), Err(()));
        assert_eq!(count(), before);
    }
}
