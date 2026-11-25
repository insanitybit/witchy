//! Guest-visible Exec capabilities and run handles. Broker operations are
//! serialized by the guest thread; handles are never recycled in a run.
use super::global::{Global, Table};
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
    fn witchy_broker_exec_args(channel: c_int, bytes: *const u8, length: usize) -> c_int;
    fn witchy_broker_exec_input(channel: c_int, bytes: *const u8, length: usize) -> c_int;
    fn witchy_broker_exec_env(channel: c_int, bytes: *const u8, length: usize) -> c_int;
    safe fn witchy_broker_exec_cwd(channel: c_int, source: u32, writable: bool) -> c_int;
    fn witchy_broker_exec_run(
        channel: c_int,
        program: u32,
        timeout: u32,
        budget: u32,
        argc: u32,
        code: *mut i64,
        length: *mut u32,
        err_length: *mut u32,
    ) -> c_int;
    fn witchy_broker_exec_out(channel: c_int, bytes: *mut u8, capacity: usize) -> c_int;
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
static mut ENTRIES: [*mut Entry; MAX_HANDLES] = [ptr::null_mut(); MAX_HANDLES];
static mut ENTRY_COUNT: usize = 0;
static PENDING: Global<usize> = Global::new(0);
static BYTES_LEFT: Global<u64> = Global::new(1048576);
static BYTES_GRANTED: Global<u64> = Global::new(1048576);

fn trap() -> ! {
    unsafe { core::arch::asm!("brk #0", options(noreturn, nostack)) }
}

fn allocate(kind: u8) -> *mut Entry {
    let p = unsafe { malloc(size_of::<Entry>()) }.cast::<Entry>();
    if p.is_null() {
        trap();
    }
    unsafe {
        p.write(Entry::new(kind));
    }
    p
}

fn issue(entry: *mut Entry) -> u64 {
    let count = unsafe { &mut *(&raw mut ENTRY_COUNT) };
    if *count == 0 {
        *count = 1;
    }
    if *count >= MAX_HANDLES || entry.is_null() {
        trap();
    }
    unsafe {
        (*(&raw mut ENTRIES))[*count] = entry;
    }
    let result = HANDLE_BASE + *count as u64;
    *count += 1;
    result
}

fn failed(code: u8) -> u64 {
    let entry = allocate(3);
    unsafe {
        (*entry).code = i64::from(code);
    }
    issue(entry)
}

fn entry(handle: u64) -> &'static mut Entry {
    let entries = unsafe { &mut *(&raw mut ENTRIES) };
    if handle == EXEC {
        if unsafe { witchy_entry_grants & EXEC == 0 } || file_reader::channel() < 0 {
            trap();
        }
        if entries[0].is_null() {
            let root = allocate(1);
            let count = PROGRAMS.len();
            unsafe {
                (*root).allowed = (1u32 << count) - 1;
            }
            entries[0] = root;
            if unsafe { ENTRY_COUNT == 0 } {
                unsafe {
                    ENTRY_COUNT = 1;
                }
            }
        }
        return unsafe { &mut *entries[0] };
    }
    let Some(index) = handle
        .checked_sub(HANDLE_BASE)
        .and_then(|i| usize::try_from(i).ok())
    else {
        trap();
    };
    if index == 0 || index >= unsafe { ENTRY_COUNT } || entries[index].is_null() {
        trap();
    }
    unsafe { &mut *entries[index] }
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

fn send(
    channel: c_int,
    bytes: &[u8],
    send: unsafe extern "C" fn(c_int, *const u8, usize) -> c_int,
) {
    for part in bytes.chunks(CHUNK) {
        if unsafe { send(channel, part.as_ptr(), part.len()) } != 0 {
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
    let status = unsafe {
        witchy_broker_exec_run(
            channel,
            program,
            timeout as u32,
            budget,
            entry.argc,
            &mut code,
            &mut out,
            &mut err,
        )
    };
    reset(entry);
    if status != 0 {
        return failed(status as u8);
    }
    if out > budget || err > budget - out {
        trap();
    }
    let run = allocate(2);
    let run_entry = unsafe { &mut *run };
    run_entry.code = code;
    let mut chunk = [0u8; CHUNK];
    for (length, buffer) in [
        (out as usize, &mut run_entry.output),
        (err as usize, &mut run_entry.errors),
    ] {
        let mut remaining = length;
        while remaining != 0 {
            let want = remaining.min(CHUNK);
            let got = unsafe { witchy_broker_exec_out(channel, chunk.as_mut_ptr(), want) };
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
    let entry = entry(handle);
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
                    unsafe {
                        (*next).allowed = 1 << index;
                    }
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
    unsafe extern "C" {
        fn open(path: *const c_char, flags: c_int, ...) -> c_int;
        safe fn close(fd: c_int) -> c_int;
        fn fork() -> c_int;
        safe fn _exit(code: c_int) -> !;
        fn waitpid(pid: c_int, status: *mut c_int, options: c_int) -> c_int;
        fn socketpair(domain: c_int, kind: c_int, protocol: c_int, fds: *mut c_int) -> c_int;
    }
    let executable = std::env::current_exe().unwrap();
    let parent = CString::new(executable.parent().unwrap().as_os_str().as_bytes()).unwrap();
    let mut name = CString::new(executable.file_name().unwrap().as_bytes())
        .unwrap()
        .into_bytes_with_nul();
    let fd = unsafe { open(parent.as_ptr(), 0x0110_0000) };
    assert!(fd >= 0);
    assert!(unsafe { bind_programs(name.as_mut_ptr().cast()) });
    let granted = CStr::from_bytes_with_nul(&name).unwrap();
    assert!(super::exec_programs::witchy_exec_setup(fd, &[granted], 4096));
    configure_budget(4096);
    let mut sockets = [-1; 2];
    assert_eq!(unsafe { socketpair(1, 1, 0, sockets.as_mut_ptr()) }, 0);
    let child = unsafe { fork() };
    assert!(child >= 0);
    if child == 0 {
        unsafe {
            close(sockets[0]);
        }
        loop {
            let mut head = [0u8; 4];
            if !unsafe {
                super::broker_transport::witchy_broker_transfer(
                    sockets[1],
                    head.as_mut_ptr(),
                    ptr::null(),
                    4,
                )
            } {
                break;
            }
            let count = u16::from_be_bytes([head[1], head[2]]) as usize;
            let mut data = [0u8; CHUNK];
            if count > CHUNK
                || (head[0] != 29
                    && !unsafe {
                        super::broker_transport::witchy_broker_transfer(
                            sockets[1],
                            data.as_mut_ptr(),
                            ptr::null(),
                            count,
                        )
                    })
            {
                unsafe {
                    _exit(1);
                }
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
                || !unsafe {
                    super::broker_transport::witchy_broker_transfer(
                        sockets[1],
                        ptr::null_mut(),
                        data.as_ptr(),
                        answer,
                    )
                }
            {
                unsafe {
                    _exit(2);
                }
            }
        }
        unsafe {
            _exit(0);
        }
    }
    unsafe {
        close(sockets[1]);
    }
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
    unsafe {
        close(sockets[0]);
    }
    let mut status = 0;
    assert_eq!(unsafe { waitpid(child, &mut status, 0) }, child);
    assert_eq!(status, 0);
}
