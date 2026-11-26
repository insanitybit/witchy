//! Guest-visible Exec capabilities and run handles. Broker operations are
//! serialized by the guest thread; handles are never recycled in a run.
use super::broker_transport::{
    witchy_broker_exec_args, witchy_broker_exec_env, witchy_broker_exec_input,
    witchy_broker_exec_out, witchy_broker_exec_run,
};
use super::global::{Global, State, Table};
use super::{exec_pending::Buffer, file_reader};
use core::{
    ffi::{CStr, c_char, c_int, c_void},
    mem::size_of,
    ptr,
};

const EXEC: u64 = 262144;
const HANDLE_BASE: u64 = 1 << 24;
const MAX_HANDLES: usize = 4096;
const MAX_PENDING: usize = 1 << 20;
const CHUNK: usize = 16384;

unsafe extern "C" {
    static witchy_entry_grants: u64;
    fn malloc(size: usize) -> *mut c_void;
    safe fn witchy_broker_exec_cwd(channel: c_int, source: u32, writable: bool) -> c_int;
    safe fn witchy_directory(op: u64, handle: u64, value: u64) -> u64;
}

struct Entry {
    kind: u8,
    allowed: u32,
    name: [u8; 256],
    name_len: usize,
    args: Buffer,
    argc: u32,
    invalid: bool,
    arg: [u8; 4097],
    arg_len: usize,
    output: Buffer,
    output_at: usize,
    errors: Buffer,
    errors_at: usize,
    code: i64,
    input: Buffer,
    deadline: u64,
    output_limit: u64,
    limited: bool,
    refused: u8,
    cwd: u64,
    env: Buffer,
    env_count: u32,
    env_name: [u8; 65],
    env_name_len: usize,
    env_value: [u8; 4097],
    env_value_len: usize,
}

impl Entry {
    const fn new(kind: u8) -> Self {
        Self {
            kind,
            allowed: 0,
            name: [0; 256],
            name_len: 0,
            args: Buffer::EMPTY,
            argc: 0,
            invalid: false,
            arg: [0; 4097],
            arg_len: 0,
            output: Buffer::EMPTY,
            output_at: 0,
            errors: Buffer::EMPTY,
            errors_at: 0,
            code: 0,
            input: Buffer::EMPTY,
            deadline: 0,
            output_limit: 0,
            limited: false,
            refused: 0,
            cwd: 0,
            env: Buffer::EMPTY,
            env_count: 0,
            env_name: [0; 65],
            env_name_len: 0,
            env_value: [0; 4097],
            env_value_len: 0,
        }
    }
}

static PROGRAMS: Table<*const c_char, 16> = Table::new(ptr::null());

/// Every handle's entry. An entry is allocated once and never freed; its place
/// here is empty before it is issued and while `witchy_exec` works on it.
struct Handles {
    entries: [Option<&'static mut Entry>; MAX_HANDLES],
    count: usize,
    rooted: bool,
}

static HANDLES: State<Handles> = State::new(Handles {
    entries: [const { None }; MAX_HANDLES],
    count: 0,
    rooted: false,
});
static PENDING: Global<usize> = Global::new(0);
static BYTES_LEFT: Global<u64> = Global::new(1048576);
static BYTES_GRANTED: Global<u64> = Global::new(1048576);

fn trap() -> ! {
    unsafe { core::arch::asm!("brk #0", options(noreturn, nostack)) }
}

fn allocate(kind: u8) -> &'static mut Entry {
    let p = unsafe { malloc(size_of::<Entry>()) }.cast::<Entry>();
    if p.is_null() {
        trap();
    }
    // SAFETY: `p` is a fresh allocation of an `Entry`'s size, aligned for any
    // type, and it is never freed, so the one reference made here may live as
    // long as the process.
    unsafe {
        p.write(Entry::new(kind));
        &mut *p
    }
}

fn issue(entry: &'static mut Entry) -> u64 {
    HANDLES.with(|handles| {
        if handles.count == 0 {
            handles.count = 1;
        }
        if handles.count >= MAX_HANDLES {
            trap();
        }
        handles.entries[handles.count] = Some(entry);
        let result = HANDLE_BASE + handles.count as u64;
        handles.count += 1;
        result
    })
}

fn failed(code: u8) -> u64 {
    let entry = allocate(3);
    entry.code = i64::from(code);
    issue(entry)
}

/// Takes the entry `handle` names out of its place, so that the one reference
/// to it is the caller's until `put` returns it. Nothing `witchy_exec` calls
/// yields to another task or calls back into guest code, so no second call
/// meets the empty place; one that did would trap as for a handle never issued.
fn take(handle: u64) -> (usize, &'static mut Entry) {
    let index = if handle == EXEC {
        if unsafe { witchy_entry_grants & EXEC == 0 } || file_reader::channel() < 0 {
            trap();
        }
        if !HANDLES.with(|handles| handles.rooted) {
            let root = allocate(1);
            root.allowed = (1u32 << PROGRAMS.len()) - 1;
            HANDLES.with(|handles| {
                handles.entries[0] = Some(root);
                handles.rooted = true;
                if handles.count == 0 {
                    handles.count = 1;
                }
            });
        }
        0
    } else {
        let Some(index) = handle
            .checked_sub(HANDLE_BASE)
            .and_then(|i| usize::try_from(i).ok())
        else {
            trap();
        };
        if index == 0 || index >= HANDLES.with(|handles| handles.count) {
            trap();
        }
        index
    };
    match HANDLES.with(|handles| handles.entries[index].take()) {
        Some(entry) => (index, entry),
        None => trap(),
    }
}

fn put(index: usize, entry: &'static mut Entry) {
    HANDLES.with(|handles| handles.entries[index] = Some(entry));
}

fn chosen(entry: &mut Entry) -> Result<u32, u8> {
    let length = entry.name_len;
    entry.name_len = 0;
    if length > 255
        || !super::exec_environment::witchy_exec_plain(&entry.name[..length])
        || !super::launch_args::valid_utf8(&entry.name[..length])
    {
        return Err(2);
    }
    for (index, name) in PROGRAMS.iter().enumerate() {
        if !name.is_null() && unsafe { CStr::from_ptr(name) }.to_bytes() == &entry.name[..length] {
            return if entry.allowed & (1 << index) != 0 {
                Ok(index as u32)
            } else {
                Err(1)
            };
        }
    }
    Err(1)
}

fn close_cwd(entry: &mut Entry) {
    if entry.cwd != 0 {
        witchy_directory(3, entry.cwd, 0);
        entry.cwd = 0;
    }
}

fn reset(entry: &mut Entry) {
    let used = entry.args.bytes().len()
        + entry.arg_len
        + entry.input.bytes().len()
        + entry.env.bytes().len();
    PENDING.set(PENDING.get().checked_sub(used).unwrap_or_else(|| trap()));
    entry.args.clear();
    entry.input.clear();
    entry.env.clear();
    close_cwd(entry);
    entry.arg_len = 0;
    entry.argc = 0;
    entry.invalid = false;
    entry.deadline = 0;
    entry.output_limit = 0;
    entry.limited = false;
    entry.refused = 0;
    entry.env_count = 0;
    entry.env_name_len = 0;
    entry.env_value_len = 0;
}

fn send(channel: c_int, bytes: &[u8], send: fn(c_int, &[u8]) -> c_int) {
    for part in bytes.chunks(CHUNK) {
        if send(channel, part) != 0 {
            trap();
        }
    }
}

fn start(entry: &mut Entry, program: u32) -> u64 {
    if entry.invalid || entry.refused != 0 {
        let code = if entry.refused != 0 { entry.refused } else { 2 };
        reset(entry);
        return failed(code);
    }
    let channel = file_reader::channel();
    if entry.cwd != 0 {
        let source = if matches!(entry.cwd, 1024 | 4096 | 8192) {
            16
        } else {
            (entry.cwd % 16) as u32
        };
        let result = witchy_broker_exec_cwd(channel, source, entry.cwd == 4096);
        close_cwd(entry);
        if result != 0 {
            reset(entry);
            return failed(5);
        }
    }
    send(channel, entry.env.bytes(), witchy_broker_exec_env);
    send(channel, entry.input.bytes(), witchy_broker_exec_input);
    send(channel, entry.args.bytes(), witchy_broker_exec_args);
    let timeout = super::exec_deadline::command_ms(entry.deadline, super::deadline::left_ms());
    let allowed = BYTES_LEFT.get().min(if entry.limited {
        entry.output_limit
    } else {
        u64::MAX
    });
    let budget = allowed.min(u64::from(u32::MAX - 1)) as u32;
    let (mut code, mut out, mut err) = (0i64, 0u32, 0u32);
    let status = witchy_broker_exec_run(
        channel,
        program,
        timeout as u32,
        budget,
        entry.argc,
        &mut code,
        &mut out,
        &mut err,
    );
    reset(entry);
    if status != 0 {
        return failed(status as u8);
    }
    if out > budget || err > budget - out {
        trap();
    }
    let run = allocate(2);
    run.code = code;
    let mut chunk = [0u8; CHUNK];
    for (length, buffer) in [
        (out as usize, &mut run.output),
        (err as usize, &mut run.errors),
    ] {
        let mut remaining = length;
        while remaining != 0 {
            let want = remaining.min(CHUNK);
            let got = witchy_broker_exec_out(channel, &mut chunk[..want]);
            if got <= 0 || got as usize > want || !buffer.append(&chunk[..got as usize]) {
                trap();
            }
            remaining -= got as usize;
        }
    }
    BYTES_LEFT.set(BYTES_LEFT.get() - (u64::from(out) + u64::from(err)));
    issue(run)
}

#[unsafe(no_mangle)]
extern "C" fn witchy_exec(op: u64, handle: u64, value: u64) -> u64 {
    let (index, entry) = take(handle);
    let result = operate(op, entry, value);
    put(index, entry);
    result
}

fn operate(op: u64, entry: &mut Entry, value: u64) -> u64 {
    match op {
        0 => {
            if entry.kind != 1 || value > 255 {
                trap();
            }
            if entry.name_len <= 255 {
                entry.name[entry.name_len] = value as u8;
                entry.name_len += 1;
            }
            0
        }
        1 => {
            if entry.kind != 1 {
                trap();
            }
            match chosen(entry) {
                Err(code) => failed(code),
                Ok(index) => {
                    let next = allocate(1);
                    next.allowed = 1 << index;
                    issue(next)
                }
            }
        }
        2 => {
            if entry.kind != 1 || value > 255 || PENDING.get() >= MAX_PENDING {
                trap();
            }
            if entry.arg_len <= 4096 {
                entry.arg[entry.arg_len] = value as u8;
                entry.arg_len += 1;
                PENDING.set(PENDING.get() + 1);
            }
            0
        }
        3 => {
            if entry.kind != 1 {
                trap();
            }
            let length = entry.arg_len;
            if length > 4096 || entry.arg[..length].contains(&0) || entry.argc >= 64 {
                entry.invalid = true;
                PENDING.set(PENDING.get() - length);
            } else {
                if PENDING.get() >= MAX_PENDING
                    || !entry.args.append(&entry.arg[..length])
                    || !entry.args.append(&[0])
                {
                    trap();
                }
                PENDING.set(PENDING.get() + 1);
            }
            entry.argc = entry.argc.saturating_add(1);
            entry.arg_len = 0;
            0
        }
        4 => {
            if entry.kind != 1 {
                trap();
            }
            match chosen(entry) {
                Ok(program) => start(entry, program),
                Err(code) => {
                    reset(entry);
                    failed(code)
                }
            }
        }
        5 => {
            if entry.kind == 3 {
                entry.code as u64
            } else {
                0
            }
        }
        6 | 11 => {
            if entry.kind != 2 {
                trap();
            }
            let (buffer, at) = if op == 6 {
                (&entry.output, &mut entry.output_at)
            } else {
                (&entry.errors, &mut entry.errors_at)
            };
            match buffer.bytes().get(*at) {
                Some(&byte) => {
                    *at += 1;
                    u64::from(byte)
                }
                None => 256,
            }
        }
        7 => {
            if entry.kind != 2 {
                trap();
            }
            entry.code as u64
        }
        8 => {
            if entry.kind != 1
                || value > 255
                || PENDING.get() >= MAX_PENDING
                || !entry.input.append(&[value as u8])
            {
                trap();
            }
            PENDING.set(PENDING.get() + 1);
            0
        }
        9 => {
            if entry.kind != 1 {
                trap();
            }
            if value == 0 {
                entry.refused = 2;
            } else if value > 2000 {
                entry.refused = 7;
            } else {
                entry.deadline = value;
            }
            0
        }
        10 => {
            if entry.kind != 1 {
                trap();
            }
            if value > BYTES_GRANTED.get() {
                entry.refused = 7;
            } else {
                entry.output_limit = value;
                entry.limited = true;
            }
            0
        }
        12 => {
            if entry.kind != 1 {
                trap();
            }
            if (1..=5).contains(&value) {
                entry.refused = 2;
                return 0;
            }
            witchy_directory(2, value, 0);
            close_cwd(entry);
            entry.cwd = value;
            0
        }
        13 | 14 => {
            if entry.kind != 1 || value > 255 || PENDING.get() >= MAX_PENDING {
                trap();
            }
            if op == 13 && entry.env_name_len <= 64 {
                entry.env_name[entry.env_name_len] = value as u8;
                entry.env_name_len += 1;
            }
            if op == 14 && entry.env_value_len <= 4096 {
                entry.env_value[entry.env_value_len] = value as u8;
                entry.env_value_len += 1;
            }
            0
        }
        15 => {
            if entry.kind != 1 {
                trap();
            }
            let (name, value) = (entry.env_name_len, entry.env_value_len);
            entry.env_name_len = 0;
            entry.env_value_len = 0;
            if name > 64
                || value > 4096
                || entry.env_value[..value].contains(&0)
                || entry.env_count == 16
            {
                entry.refused = 2;
                return 0;
            }
            if !super::exec_environment::witchy_exec_env_declared(&entry.env_name[..name]) {
                entry.refused = 1;
                return 0;
            }
            let need = name + value + 2;
            if PENDING.get() + need > MAX_PENDING
                || !entry.env.append(&entry.env_name[..name])
                || !entry.env.append(b"=")
                || !entry.env.append(&entry.env_value[..value])
                || !entry.env.append(&[0])
            {
                trap();
            }
            entry.env_count += 1;
            PENDING.set(PENDING.get() + need);
            0
        }
        _ => trap(),
    }
}

/// `names` must remain live for the program's execution. It is split in place.
pub(super) unsafe fn bind_programs(names: *mut c_char) -> bool {
    if names.is_null() || !PROGRAMS.is_empty() {
        return false;
    }
    let mut starts = [0usize; 16];
    let mut ends = [0usize; 16];
    let mut count = 0;
    {
        let bytes = unsafe { CStr::from_ptr(names) }.to_bytes();
        if bytes.is_empty() {
            return false;
        }
        let mut start = 0;
        for end in 0..=bytes.len() {
            if end != bytes.len() && bytes[end] != b',' {
                continue;
            }
            let part = &bytes[start..end];
            if count == 16
                || !super::exec_environment::witchy_exec_plain(part)
                || (0..count).any(|i| &bytes[starts[i]..ends[i]] == part)
            {
                return false;
            }
            starts[count] = start;
            ends[count] = end;
            count += 1;
            start = end + 1;
        }
    }
    for index in 0..count {
        if index + 1 != count {
            unsafe {
                names.add(ends[index]).write(0);
            }
        }
        PROGRAMS.push(unsafe { names.add(starts[index]) });
    }
    true
}

pub(super) fn configure_budget(bytes: u64) {
    BYTES_LEFT.set(bytes);
    BYTES_GRANTED.set(bytes);
}

pub(super) fn program_names() -> (*const *const c_char, u32) {
    (PROGRAMS.as_ptr(), PROGRAMS.len() as u32)
}

#[cfg(test)]
#[test]
fn guest_handles_run_only_a_granted_program() {
    #[cfg(target_os = "macos")]
    use super::exec_broker::witchy_exec_request;
    #[cfg(target_os = "linux")]
    use super::exec_linux_runner::witchy_exec_request;
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use super::sys;
    unsafe extern "C" {
        safe fn close(fd: c_int) -> c_int;
        fn fork() -> c_int;
        safe fn _exit(code: c_int) -> !;
    }
    let executable = std::env::current_exe().unwrap();
    let parent = CString::new(executable.parent().unwrap().as_os_str().as_bytes()).unwrap();
    let mut name = CString::new(executable.file_name().unwrap().as_bytes())
        .unwrap()
        .into_bytes_with_nul();
    let fd = sys::open(&parent, 0x0110_0000);
    assert!(fd >= 0);
    assert!(unsafe { bind_programs(name.as_mut_ptr().cast()) });
    let granted = CStr::from_bytes_with_nul(&name).unwrap();
    assert!(super::exec_programs::witchy_exec_setup(fd, &[granted], 4096));
    configure_budget(4096);
    let mut sockets = [-1; 2];
    assert_eq!(sys::socketpair(1, 1, 0, &mut sockets), 0);
    let child = unsafe { fork() };
    assert!(child >= 0);
    if child == 0 {
        close(sockets[0]);
        loop {
            let mut head = [0u8; 4];
            if !super::broker_transport::witchy_broker_receive(sockets[1], &mut head) {
                break;
            }
            let count = u16::from_be_bytes([head[1], head[2]]) as usize;
            let mut data = [0u8; CHUNK];
            if count > CHUNK
                || (head[0] != 29
                    && !super::broker_transport::witchy_broker_receive(
                        sockets[1],
                        &mut data[..count],
                    ))
            {
                _exit(1);
            }
            let capacity = if head[0] == 29 { count } else { CHUNK };
            let Some(answer) = witchy_exec_request(
                sockets[1],
                head[0],
                &mut data[..capacity],
                if head[0] == 29 { 0 } else { count },
            ) else {
                _exit(2)
            };
            if !super::broker_transport::witchy_broker_reply(sockets[1], 0, answer)
                || !super::broker_transport::witchy_broker_send(sockets[1], &data[..answer])
            {
                _exit(2);
            }
        }
        _exit(0);
    }
    close(sockets[1]);
    file_reader::bind_channel(sockets[0]);
    for byte in b"not-granted" {
        witchy_exec(0, EXEC, u64::from(*byte));
    }
    let refused = witchy_exec(4, EXEC, 0);
    assert_eq!(witchy_exec(5, refused, 0), 1);
    for arg in [
        b"--exact".as_slice(),
        b"exec_broker::subprocess_helper",
        b"--nocapture",
    ] {
        for byte in arg {
            witchy_exec(2, EXEC, u64::from(*byte));
        }
        witchy_exec(3, EXEC, 0);
    }
    for &byte in &name[..name.len() - 1] {
        witchy_exec(0, EXEC, u64::from(byte));
    }
    let run = witchy_exec(4, EXEC, 0);
    assert_eq!(witchy_exec(5, run, 0), 0);
    assert_eq!(witchy_exec(7, run, 0), 0);
    let mut output = Vec::new();
    loop {
        let byte = witchy_exec(6, run, 0);
        if byte == 256 {
            break;
        }
        output.push(byte as u8);
    }
    assert!(
        output
            .windows(b"witchy-exec-hello".len())
            .any(|part| part == b"witchy-exec-hello")
    );
    close(sockets[0]);
    let mut status = 0;
    assert_eq!(sys::waitpid(child, Some(&mut status), 0), child);
    assert_eq!(status, 0);
}
