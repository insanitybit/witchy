//! One bounded request per connection, over an already-connected Unix
//! stream chosen by the operator. A partial round trip retires the stream.
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex, TryLockError};
use std::time::{Duration, Instant};
use wasmtime::{Caller, Linker};

use super::{Network, State, guest_range, reject};

pub(super) const REQUEST_LIMIT: usize = 1_048_576 + 16_384;
pub(super) const REPLY_LIMIT: usize = 16_384;
pub(super) const MAX_REPLY_LIMIT: usize = 20 * 1024 * 1024;
pub(super) const HANDLE: i64 = 6;

pub(super) struct Grant {
    stream: Arc<Mutex<Option<UnixStream>>>,
    authorized: bool,
    exchanged: bool,
    pub(super) reply_limit: usize,
}

impl Grant {
    pub(super) fn from_fd(fd: OwnedFd, reply_limit: usize) -> io::Result<Self> {
        use rustix::net::{SocketType, sockopt::socket_type};
        if socket_type(&fd)? != SocketType::STREAM {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "expected a connected Unix stream"));
        }
        let stream = UnixStream::from(fd);
        stream.local_addr()?;
        stream.peer_addr()?;
        stream.set_nonblocking(true)?;
        Ok(Self { stream: Arc::new(Mutex::new(Some(stream))), authorized: false, exchanged: false, reply_limit })
    }

    pub(super) fn fresh(&self) -> Self {
        Self { stream: self.stream.clone(), authorized: false, exchanged: false, reply_limit: self.reply_limit }
    }

    pub(super) fn authorize(&mut self) -> bool {
        !std::mem::replace(&mut self.authorized, true)
    }

    pub(super) fn take_exchange(&mut self, handle: i64) -> bool {
        handle == HANDLE && self.authorized && !std::mem::replace(&mut self.exchanged, true)
    }

    pub(super) fn exchange(&self, request: &[u8], capacity: usize, until: Instant) -> Option<Vec<u8>> {
        let mut stream = loop {
            let delay = remaining(until).ok()?;
            match self.stream.try_lock() {
                Ok(stream) => break stream,
                Err(TryLockError::Poisoned(_)) => return None,
                Err(TryLockError::WouldBlock) => std::thread::sleep(delay.min(Duration::from_millis(1))),
            }
        };
        let result = exchange(stream.as_mut()?, request, capacity, until);
        match result {
            Ok(reply) => Some(reply),
            Err(_) => { *stream = None; None }
        }
    }
}

pub(super) fn register(linker: &mut Linker<State>, namespace: &str) -> wasmtime::Result<()> {
    linker.func_wrap(namespace, "local_service_authorize", |mut caller: Caller<'_, State>, code: i64| -> wasmtime::Result<i64> {
        if !caller.data().active || code != 0
            || !caller.data_mut().local_service.as_mut().is_some_and(Grant::authorize) {
            return Err(reject("invalid local service capability"));
        }
        Ok(HANDLE)
    })?;
    linker.func_wrap(namespace, "local_service_exchange", |mut caller: Caller<'_, State>, handle: i64, request: i64, reply: i64| -> wasmtime::Result<i64> {
        if !caller.data().active { return Err(reject("invalid local service capability")); }
        let (memory, at, length) = guest_range(&mut caller, request)?;
        let (_, out, capacity) = guest_range(&mut caller, reply)?;
        let (data, state) = memory.data_and_store_mut(&mut caller);
        let service = state.local_service.as_mut().ok_or_else(|| reject("invalid local service capability"))?;
        if !service.take_exchange(handle) { return Err(reject("invalid or already used local service capability")); }
        if !(1..=REQUEST_LIMIT).contains(&length) || !(1..=service.reply_limit).contains(&capacity) { return Ok(0); }
        let network = &mut state.network;
        let charge = (length + capacity + 8) as u64;
        if charge > network.budget.saturating_sub(network.used) {
            Network::full(network.budget, network.share);
            return Ok(0);
        }
        network.used += charge;
        let until = network.until.ok_or_else(|| reject("local service needs a connection deadline"))?;
        let Some(bytes) = service.exchange(&data[at..at + length], capacity, until) else { return Ok(0); };
        data[out..out + bytes.len()].copy_from_slice(&bytes);
        Ok(bytes.len() as i64)
    })?;
    Ok(())
}

fn remaining(until: Instant) -> io::Result<Duration> {
    until.checked_duration_since(Instant::now()).filter(|left| !left.is_zero())
        .ok_or_else(|| io::Error::from(io::ErrorKind::TimedOut))
}

fn exchange(stream: &mut UnixStream, request: &[u8], capacity: usize, until: Instant) -> io::Result<Vec<u8>> {
    write_all(stream, &(request.len() as u32).to_be_bytes(), until)?;
    write_all(stream, request, until)?;
    let mut len = [0; 4];
    read_exact(stream, &mut len, until)?;
    let length = u32::from_be_bytes(len) as usize;
    if length == 0 || length > capacity {
        return Err(io::Error::from(io::ErrorKind::InvalidData));
    }
    let mut reply = vec![0; length];
    read_exact(stream, &mut reply, until)?;
    Ok(reply)
}

fn write_all(stream: &mut UnixStream, mut bytes: &[u8], until: Instant) -> io::Result<()> {
    while !bytes.is_empty() {
        remaining(until)?;
        match stream.write(bytes) {
            Ok(0) => return Err(io::Error::from(io::ErrorKind::WriteZero)),
            Ok(count) => bytes = &bytes[count..],
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => await_io(stream, super::sys::POLLOUT, until)?,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn read_exact(stream: &mut UnixStream, mut bytes: &mut [u8], until: Instant) -> io::Result<()> {
    while !bytes.is_empty() {
        remaining(until)?;
        match stream.read(bytes) {
            Ok(0) => return Err(io::Error::from(io::ErrorKind::UnexpectedEof)),
            Ok(count) => bytes = &mut bytes[count..],
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => await_io(stream, super::sys::POLLIN, until)?,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn await_io(stream: &UnixStream, events: i16, until: Instant) -> io::Result<()> {
    let wait = (remaining(until)?.as_millis() + 1).min(i32::MAX as u128) as i32;
    let mut fds = [super::sys::PollFd { fd: stream.as_raw_fd(), events, revents: 0 }];
    if super::sys::poll(&mut fds, wait)? == 0 {
        return Err(io::Error::from(io::ErrorKind::TimedOut));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_grants_share_the_peer_but_allow_only_one_exchange_each() {
        let (front, mut peer) = UnixStream::pair().unwrap();
        let initial = Grant::from_fd(front.into(), REPLY_LIMIT).unwrap();
        let responder = std::thread::spawn(move || {
            let mut frame = [0; 7];
            for _ in 0..2 {
                peer.read_exact(&mut frame).unwrap();
                assert_eq!(&frame[..4], &3u32.to_be_bytes());
                peer.write_all(&frame).unwrap();
            }
        });
        std::thread::scope(|scope| {
            for request in [b"one", b"two"] {
                let mut grant = initial.fresh();
                scope.spawn(move || {
                    assert!(!grant.take_exchange(HANDLE));
                    assert!(grant.authorize());
                    assert!(!grant.authorize());
                    assert!(!grant.take_exchange(HANDLE + 1));
                    assert!(grant.take_exchange(HANDLE));
                    assert!(!grant.take_exchange(HANDLE));
                    assert_eq!(grant.exchange(request, 16, Instant::now() + Duration::from_secs(1)).as_deref(), Some(request.as_slice()));
                });
            }
        });
        responder.join().unwrap();
    }

    #[test]
    fn an_invalid_reply_retires_the_transport() {
        let (front, mut peer) = UnixStream::pair().unwrap();
        let grant = Grant::from_fd(front.into(), REPLY_LIMIT).unwrap();
        let responder = std::thread::spawn(move || {
            let mut frame = [0; 7];
            peer.read_exact(&mut frame).unwrap();
            peer.write_all(&17u32.to_be_bytes()).unwrap();
            assert_eq!(peer.read(&mut frame).unwrap(), 0);
        });
        let until = Instant::now() + Duration::from_secs(1);
        assert!(grant.exchange(b"one", 16, until).is_none());
        assert!(grant.fresh().exchange(b"two", 16, until).is_none());
        responder.join().unwrap();
    }

    #[test]
    fn a_file_datagram_or_tcp_connection_cannot_grant_a_local_service() {
        let file = std::fs::File::open("/dev/null").unwrap();
        assert!(Grant::from_fd(file.into(), REPLY_LIMIT).is_err());
        let (datagram, _peer) = std::os::unix::net::UnixDatagram::pair().unwrap();
        assert!(Grant::from_fd(datagram.into(), REPLY_LIMIT).is_err());
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let tcp = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        assert!(Grant::from_fd(tcp.into(), REPLY_LIMIT).is_err());
    }
}
