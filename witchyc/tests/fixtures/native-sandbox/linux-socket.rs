#[path = "../../../runtime/native/errno.rs"]
mod errno;
#[path = "../../../runtime/native/socket_primitives.rs"]
mod socket_primitives;
#[path = "../../../runtime/native/sys.rs"]
mod sys;

#[test]
fn broker_sets_both_socket_timeouts() {
    use std::{os::fd::AsRawFd, os::unix::net::UnixStream, time::Duration};

    let (socket, _peer) = UnixStream::pair().unwrap();
    assert_eq!(socket.read_timeout().unwrap(), None);
    assert_eq!(socket.write_timeout().unwrap(), None);
    assert!(socket_primitives::witchy_broker_set_timeout(
        socket.as_raw_fd()
    ));
    let timeout = Some(Duration::from_secs(5));
    assert_eq!(socket.read_timeout().unwrap(), timeout);
    assert_eq!(socket.write_timeout().unwrap(), timeout);
    assert!(!socket_primitives::witchy_broker_set_timeout(-1));
}

#[test]
fn short_byte_io_preserves_the_unwritten_suffix() {
    use std::{
        fs::{OpenOptions, remove_file},
        os::{
            fd::{AsRawFd, FromRawFd, OwnedFd},
            unix::net::UnixStream,
        },
    };

    let mut fds = [-1; 2];
    assert_eq!(sys::pipe(&mut fds), 0);
    let [reader, writer] = fds.map(|fd| unsafe { OwnedFd::from_raw_fd(fd) });
    assert_eq!(sys::write(writer.as_raw_fd(), b"abc"), 3);
    drop(writer);
    let mut buffer = [0xA5; 8];
    assert_eq!(sys::read(reader.as_raw_fd(), &mut buffer), 3);
    assert_eq!(buffer, [b'a', b'b', b'c', 0xA5, 0xA5, 0xA5, 0xA5, 0xA5]);
    assert_eq!(sys::read(reader.as_raw_fd(), &mut buffer), 0);

    let path = std::env::temp_dir().join(format!("witchy-pread-{}", std::process::id()));
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)
        .unwrap();
    remove_file(path).unwrap();
    assert_eq!(sys::write(file.as_raw_fd(), b"abcd"), 4);
    buffer.fill(0xA5);
    assert_eq!(sys::pread(file.as_raw_fd(), &mut buffer, 2), 2);
    assert_eq!(buffer, [b'c', b'd', 0xA5, 0xA5, 0xA5, 0xA5, 0xA5, 0xA5]);
    assert_eq!(sys::read(file.as_raw_fd(), &mut buffer), 0);

    let (sender, receiver) = UnixStream::pair().unwrap();
    assert_eq!(sys::send(sender.as_raw_fd(), b"xy", 0), 2);
    drop(sender);
    buffer.fill(0xA5);
    assert_eq!(sys::recv(receiver.as_raw_fd(), &mut buffer, 0), 2);
    assert_eq!(buffer, [b'x', b'y', 0xA5, 0xA5, 0xA5, 0xA5, 0xA5, 0xA5]);
    assert_eq!(sys::recv(receiver.as_raw_fd(), &mut buffer, 0), 0);
}
