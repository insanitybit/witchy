//! Start granted programs only after the host owns their process group.
use std::ffi::{OsStr, OsString};
use std::io::{self, Read, Write};
use std::os::fd::FromRawFd;
use std::os::unix::{ffi::OsStrExt, net::UnixStream, process::CommandExt};
use std::path::Path;
use std::process::{Child, Command, ExitStatus};
use std::sync::Mutex;
use std::time::Instant;

use rustix::io::{FdFlags, fcntl_setfd};
use rustix::process::{Pid, WaitId, WaitIdOptions, waitid};

pub const WORKER: &str = "--internal-exec-worker";
const MAX_ENV: usize = 16 * (64 + 4096 + 2);

#[derive(PartialEq)]
enum Run {
    Idle,
    Child(Pid),
    Stopping,
}

static RUN: Mutex<Run> = Mutex::new(Run::Idle);

pub fn stop() {
    let mut run = RUN.lock().unwrap();
    if let Run::Child(pid) = *run {
        kill(pid);
    }
    *run = Run::Stopping;
}

fn kill(pid: Pid) {
    let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
}

pub struct RunningChild {
    pub child: Child,
    pid: Pid,
    reaped: bool,
}

impl RunningChild {
    pub fn start(mut command: Command, child_socket: UnixStream, mut gate: UnixStream,
                 env: &[(Vec<u8>, Vec<u8>)], until: Instant) -> io::Result<Self> {
        // Only the trusted worker inherits this endpoint, and it closes it on exec.
        unsafe {
            command.process_group(0).pre_exec(move || {
                fcntl_setfd(&child_socket, FdFlags::empty()).map_err(Into::into)
            });
        }
        let child = command.spawn()?;
        drop(command);
        let pid = Pid::from_raw(child.id() as i32).expect("positive child PID");
        let child = Self { child, pid, reaped: false };
        {
            let mut run = RUN.lock().unwrap();
            if *run != Run::Idle {
                return Err(io::Error::other("host is stopping or already running a child"));
            }
            *run = Run::Child(pid);
        }
        let mut wire = Vec::new();
        for (name, value) in env {
            wire.extend(name);
            wire.push(b'=');
            wire.extend(value);
            wire.push(0);
        }
        if wire.len() > MAX_ENV {
            return Err(io::Error::other("child environment exceeds its bound"));
        }
        let remaining = || {
            until.checked_duration_since(Instant::now()).filter(|left| !left.is_zero())
                .ok_or_else(|| io::Error::from(io::ErrorKind::TimedOut))
        };
        gate.set_write_timeout(Some(remaining()?))?;
        gate.write_all(&(wire.len() as u32).to_le_bytes())?;
        gate.write_all(&wire)?;
        gate.set_read_timeout(Some(remaining()?))?;
        let mut failure = [0];
        if gate.read(&mut failure)? != 0 {
            return Err(io::Error::other("child executable could not start"));
        }
        Ok(child)
    }

    pub fn exited(&self) -> io::Result<bool> {
        // Keep the group leader's PID reserved through cleanup, including when it exited
        // before the rest of its process group.
        waitid(WaitId::Pid(self.pid), WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT)
            .map(|status| status.is_some()).map_err(Into::into)
    }

    pub fn finish(mut self) -> io::Result<ExitStatus> {
        self.reap()
    }

    fn reap(&mut self) -> io::Result<ExitStatus> {
        if !self.reaped {
            let mut run = RUN.lock().unwrap();
            kill(self.pid);
            if *run == Run::Child(self.pid) {
                *run = Run::Idle;
            }
            self.reaped = true;
        }
        self.child.wait()
    }
}

impl Drop for RunningChild {
    fn drop(&mut self) {
        let _ = self.reap();
    }
}

pub fn worker(mut args: impl Iterator<Item = OsString>) -> i32 {
    fn run(gate: &mut UnixStream, args: &mut impl Iterator<Item = OsString>) -> io::Result<()> {
        fcntl_setfd(&*gate, FdFlags::CLOEXEC)?;
        let target = args.next().filter(|arg| Path::new(arg).is_absolute())
            .ok_or_else(|| io::Error::other("expected absolute worker target"))?;
        let mut length = [0; 4];
        gate.read_exact(&mut length)?;
        let length = u32::from_le_bytes(length) as usize;
        if length > MAX_ENV {
            return Err(io::Error::other("worker environment exceeds its bound"));
        }
        let mut env = vec![0; length];
        gate.read_exact(&mut env)?;
        let mut command = Command::new(target);
        command.args(args).env_clear();
        for field in env.split(|byte| *byte == 0).filter(|field| !field.is_empty()) {
            let equal = field.iter().position(|byte| *byte == b'=')
                .ok_or_else(|| io::Error::other("invalid worker environment"))?;
            command.env(OsStr::from_bytes(&field[..equal]), OsStr::from_bytes(&field[equal + 1..]));
        }
        Err(command.exec())
    }
    if let Some(fd) = args.next().and_then(|arg| arg.to_str()?.parse::<i32>().ok())
        .filter(|fd| *fd >= 3 && super::sys::open_descriptor(*fd)) {
        // The worker owns the inherited endpoint. Closing it during exec acknowledges success.
        let mut gate = unsafe { UnixStream::from_raw_fd(fd) };
        let _ = run(&mut gate, &mut args);
        let _ = gate.write_all(&[1]);
    }
    126
}
