//! Independent bounded stream model. Never calls std::io::BufReader or native
//! receive helpers. Arrival boundaries do not imply read-completion boundaries.
use super::*;

/// Version-one profile: native conformance must demonstrate this capacity.
pub const ORACLE_NET_BUFFER_CAPACITY: usize = 8192;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OracleNetRead {
    Data(usize),
    Eof,
    Timeout,
    WouldBlock,
    Interrupted,
    Error(String),
}

#[derive(Clone, Default)]
pub(super) struct BufferedSocket {
    transport: VecDeque<u8>,
    ahead: VecDeque<u8>,
}

impl NetState {
    pub(super) fn enable_buffered(&mut self, socket: u64) {
        if self.events.front() == Some(&OracleNetEvent::Buffered { socket }) {
            self.sockets.get_mut(&socket).unwrap().buffered = Some(BufferedSocket::default());
            self.pop();
        }
    }

    // Only consecutive arrivals for this socket are eligible. Inspect them
    // without mutation so a missing/wrong completion cannot eat a barrier.
    fn arrivals(&self, socket: u64) -> Result<(usize, usize), InterpretError> {
        let mut count = 0;
        let mut available = self.sockets[&socket].buffered.as_ref().unwrap().transport.len();
        for event in &self.events {
            let OracleNetEvent::Arrive { socket: expected, bytes } = event else { break };
            if *expected != socket { break; }
            if self.sockets[&socket].closed { return Err(refused(OracleNetError::TranscriptMismatch)); }
            available = available.checked_add(bytes.len()).ok_or(InterpretError::ArithmeticTrap)?;
            count += 1;
        }
        Ok((count, available))
    }

    fn accept_arrivals(&mut self, socket: u64, count: usize) {
        for _ in 0..count {
            let Some(OracleNetEvent::Arrive { bytes, .. }) = self.events.front() else { unreachable!() };
            self.sockets.get_mut(&socket).unwrap().buffered.as_mut().unwrap().transport.extend(bytes);
            self.pop();
        }
    }

    fn stream_read(&mut self, socket: u64, requested: usize) -> Result<OracleNetRead, InterpretError> {
        let (arrivals, available) = self.arrivals(socket)?;
        let outcome = match self.events.get(arrivals) {
            Some(OracleNetEvent::StreamRead { socket: expected, outcome }) if *expected == socket => outcome.clone(),
            _ => return Err(refused(OracleNetError::TranscriptMismatch)),
        };
        if let OracleNetRead::Data(count) = outcome {
            if count == 0 || count > requested || count > ORACLE_NET_BUFFER_CAPACITY || count > available {
                return Err(refused(OracleNetError::TranscriptMismatch));
            }
        }
        self.accept_arrivals(socket, arrivals);
        self.pop();
        if let OracleNetRead::Data(count) = outcome {
            let state = self.sockets.get_mut(&socket).unwrap().buffered.as_mut().unwrap();
            let bytes: Vec<_> = state.transport.drain(..count).collect();
            state.ahead.extend(bytes);
        }
        Ok(outcome)
    }

    pub(super) fn close_buffered(&mut self, socket: u64) -> Result<(), InterpretError> {
        let (arrivals, available) = self.arrivals(socket)?;
        let keep = match self.events.get(arrivals) {
            Some(OracleNetEvent::BufferedClose { socket: expected, keep_transport_prefix })
                if *expected == socket && *keep_transport_prefix <= available => *keep_transport_prefix,
            _ => return Err(refused(OracleNetError::TranscriptMismatch)),
        };
        self.accept_arrivals(socket, arrivals);
        let state = self.sockets.get_mut(&socket).unwrap();
        state.buffered.as_mut().unwrap().transport.truncate(keep);
        state.closed = true;
        self.pop();
        Ok(())
    }

    pub(super) fn receive_buffered(&mut self, socket: u64, mode: Receive, limit: usize) -> Result<u64, InterpretError> {
        // Protocol refusal is transactional across the WHOLE host receive.
        // Actual IO errors/cap failures consume completed reads and do not
        // roll them back; neither path fabricates a staged result.
        let events = self.events.clone();
        let observed = self.observed.len();
        let offset = self.chunk_offset;
        let state = self.sockets[&socket].clone();
        let result = self.receive_buffered_inner(socket, mode, limit);
        if matches!(result, Err(InterpretError::NetFixture(OracleNetError::TranscriptMismatch))) {
            self.events = events;
            self.observed.truncate(observed);
            self.chunk_offset = offset;
            self.sockets.insert(socket, state);
        }
        result
    }

    fn receive_buffered_inner(&mut self, socket: u64, mode: Receive, limit: usize) -> Result<u64, InterpretError> {
        let wanted = match mode { Receive::Bytes(count) => count, Receive::All => limit.saturating_add(1), Receive::Line => limit };
        let mut raw = Vec::new();
        while raw.len() < wanted {
            if self.sockets[&socket].buffered.as_ref().unwrap().ahead.is_empty() {
                // Byte receives request min(remaining, 8192) from BufReader:
                // small requests refill 8192, full ones bypass with 8192.
                // Line refills also request 8192. Native read_to_end requests
                // may be larger; v1 completions are short reads <=8192. Drain
                // each completion here, independently of Vec growth probes.
                match self.stream_read(socket, ORACLE_NET_BUFFER_CAPACITY)? {
                    OracleNetRead::Data(_) => {}
                    OracleNetRead::Eof | OracleNetRead::Timeout | OracleNetRead::WouldBlock => break,
                    OracleNetRead::Interrupted if matches!(mode, Receive::All) => continue,
                    OracleNetRead::Interrupted => return Err(refused(OracleNetError::ReadFailed("interrupted".into()))),
                    OracleNetRead::Error(message) => return Err(refused(OracleNetError::ReadFailed(message))),
                }
            }
            let ahead = &mut self.sockets.get_mut(&socket).unwrap().buffered.as_mut().unwrap().ahead;
            let amount = ahead.len().min(wanted - raw.len());
            let newline = if matches!(mode, Receive::Line) {
                ahead.iter().take(amount).position(|byte| *byte == b'\n')
            } else { None };
            raw.extend(ahead.drain(..newline.map_or(amount, |index| index + 1)));
            if newline.is_some() { break; }
            if matches!(mode, Receive::Line) && raw.len() >= limit {
                return Err(refused(OracleNetError::ReceiveLimit));
            }
        }
        if matches!(mode, Receive::All) && raw.len() > limit { return Err(refused(OracleNetError::ReceiveLimit)); }
        let decoded = String::from_utf8_lossy(&raw);
        let bytes = if matches!(mode, Receive::Line) { decoded.trim_end_matches('\n').as_bytes().to_vec() }
            else { decoded.into_owned().into_bytes() };
        let length = u64::try_from(bytes.len()).map_err(|_| InterpretError::ArithmeticTrap)?;
        if length > i32::MAX as u64 { return Err(InterpretError::ArithmeticTrap); }
        self.sockets.get_mut(&socket).unwrap().pending = Some(bytes);
        Ok(length)
    }
}

#[cfg(test)]
#[path = "net_buffered_tests.rs"]
mod tests;
