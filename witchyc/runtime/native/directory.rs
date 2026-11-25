//! Guest directory handles and path staging on the serialized guest thread.

use super::global::Global;
use core::ffi::c_int;

use super::broker_transport::{
    witchy_broker_directory_change, witchy_broker_file_open, witchy_broker_file_probe,
    witchy_broker_rename,
};
use super::file_reader::channel;

const SLOTS: usize = 16;
const PATH_CAPACITY: usize = 4096;
const READ_ROOT: u64 = 1024;
const WRITE_ROOT: u64 = 4096;
const FULL_ROOT: u64 = 8192;
const FILE: u32 = 1;
const READ: u32 = 2;
const WRITE: u32 = 4;
const BOTH: u32 = READ | WRITE;
const IO: u64 = 5;

#[derive(Clone, Copy)]
struct Path {
    bytes: [u8; PATH_CAPACITY],
    len: usize,
}

impl Path {
    const EMPTY: Self = Self {
        bytes: [0; PATH_CAPACITY],
        len: 0,
    };

    fn push(&mut self, byte: u8) {
        if let Some(out) = self.bytes.get_mut(self.len + 1) {
            *out = byte;
            self.len += 1;
        }
    }
}

#[derive(Clone, Copy)]
struct Paths {
    path: Path,
    from: Path,
}

impl Paths {
    const EMPTY: Self = Self {
        path: Path::EMPTY,
        from: Path::EMPTY,
    };
}

#[derive(Clone, Copy)]
struct Entry {
    handle: u64,
    kind: u32,
}

static mut ENTRIES: [Entry; SLOTS] = [Entry { handle: 0, kind: 0 }; SLOTS];
static mut PATHS: [Paths; SLOTS] = [Paths::EMPTY; SLOTS];
static mut READ_PATH: Paths = Paths::EMPTY;
static mut WRITE_PATH: Paths = Paths::EMPTY;
static CLOSED: Global<u32> = Global::new(0);
static NEXT: Global<u64> = Global::new(16384);

unsafe extern "C" {
    safe fn witchy_broker_root_close(channel: c_int, writable: bool) -> c_int;
    safe fn witchy_broker_file_close(channel: c_int, slot: u32) -> c_int;
    fn witchy_open_directory_output(
        path: *const u8,
        length: usize,
        exclusive: bool,
        append: bool,
    ) -> u64;
    safe fn witchy_broker_directory_list(channel: c_int, slot: u32, source: u8) -> c_int;
    #[link_name = "write"]
    fn write_stderr(fd: c_int, bytes: *const core::ffi::c_void, length: usize) -> isize;
    safe fn exit(status: c_int) -> !;
}

fn trap() -> ! {
    unsafe { core::arch::asm!("brk #0", options(noreturn, nostack)) }
}

pub(super) fn file_slot(handle: u64, close: bool) -> u32 {
    let slot = handle as usize % SLOTS;
    let entry = unsafe { &mut (*(&raw mut ENTRIES))[slot] };
    if handle == 0 || entry.handle != handle || entry.kind != FILE {
        trap();
    }
    if close {
        entry.handle = 0;
    }
    slot as u32
}

#[unsafe(no_mangle)]
extern "C" fn witchy_directory(op: u64, handle: u64, value: u64) -> u64 {
    let channel = channel();
    if channel < 0 || op > 15 {
        trap();
    }
    if (1..=IO).contains(&handle) && matches!(op, 2 | 3) {
        return if op == 2 { handle } else { 0 };
    }
    let entries = unsafe { &mut *(&raw mut ENTRIES) };
    let (source, rights) = match handle {
        READ_ROOT => (SLOTS, READ),
        WRITE_ROOT => (SLOTS, WRITE),
        FULL_ROOT => (SLOTS, BOTH),
        _ => {
            let slot = handle as usize % SLOTS;
            let entry = entries[slot];
            if handle == 0 || entry.handle != handle || !matches!(entry.kind, READ | WRITE | BOTH) {
                trap();
            }
            (slot, entry.kind)
        }
    };
    if source == SLOTS && CLOSED.get() & rights != 0 {
        trap();
    }
    let read = rights & READ != 0;
    let write = rights & WRITE != 0;

    if op == 2 {
        return 0;
    }
    if op == 3 {
        if source != SLOTS {
            entries.get_mut(source).unwrap_or_else(|| trap()).handle = 0;
            return u64::from(witchy_broker_file_close(channel, source as u32) < 0);
        }
        CLOSED.set(CLOSED.get() | rights);
        let read_ok = !read || witchy_broker_root_close(channel, false) >= 0;
        let write_ok = !write || witchy_broker_root_close(channel, true) >= 0;
        return if read_ok && write_ok { 0 } else { IO };
    }
    if (op >= 7 && !write) || (!read && matches!(op, 1 | 4 | 5 | 6 | 14)) {
        trap();
    }
    let paths = if source == SLOTS {
        unsafe {
            if write {
                &mut *(&raw mut WRITE_PATH)
            } else {
                &mut *(&raw mut READ_PATH)
            }
        }
    } else {
        unsafe {
            (&mut *(&raw mut PATHS))
                .get_mut(source)
                .unwrap_or_else(|| trap())
        }
    };
    let Paths { path, from } = paths;
    if op == 0 {
        path.push(u8::try_from(value).unwrap_or_else(|_| trap()));
        return 0;
    }
    if op == 12 {
        *from = *path;
        path.len = 0;
        return 0;
    }
    let len = core::mem::take(&mut path.len);
    if op == 13 {
        let from_len = core::mem::take(&mut from.len);
        if from_len >= PATH_CAPACITY - 1 || len >= PATH_CAPACITY - 1 {
            return 2;
        }
        return witchy_broker_rename(
            channel,
            source as u32,
            &from.bytes[1..1 + from_len],
            &path.bytes[1..1 + len],
        ) as u64;
    }
    if op != 6 && (len == 0 || len == PATH_CAPACITY - 1) {
        return if op == 4 { 0 } else { 2 };
    }
    path.bytes[0] = source as u8;
    // `Path::push` keeps the source byte and `len` path bytes inside the array.
    let bytes = &path.bytes[..len + 1];
    match op {
        7 | 10 => {
            return witchy_broker_directory_change(channel, source as u32, &bytes[1..], op == 10)
                as u64;
        }
        8 | 9 | 11 => {
            return unsafe {
                witchy_open_directory_output(bytes.as_ptr(), bytes.len(), op == 9, op == 11)
            };
        }
        4 => return witchy_broker_file_probe(channel, bytes) as u64,
        _ => {}
    }
    let kind = match op {
        14 => BOTH,
        15 => WRITE,
        5 => READ,
        _ => FILE,
    };
    for _ in 0..SLOTS {
        let token = NEXT.get();
        if token == u64::MAX {
            break;
        }
        NEXT.set(token + 1);
        let slot = token as usize % SLOTS;
        if entries[slot].handle != 0 {
            continue;
        }
        // The source path belongs to a live slot or a root, so the new
        // slot cannot alias it. End the path borrow before resetting it.
        let result = if op == 6 {
            witchy_broker_directory_list(channel, slot as u32, source as u8)
        } else {
            witchy_broker_file_open(channel, slot as u32, bytes, kind)
        };
        if result == -1 {
            let message = b"witchy: the directory grant reached a secret's source, or its broker failed; the run is stopped\n";
            unsafe {
                write_stderr(2, message.as_ptr().cast(), message.len());
                exit(2);
            }
        }
        if result != 0 {
            return if result < 0 { IO } else { result as u64 };
        }
        entries[slot] = Entry {
            handle: token,
            kind,
        };
        unsafe {
            let paths = (&raw mut PATHS).cast::<Paths>().add(slot);
            (*paths).path.len = 0;
            (*paths).from.len = 0;
        }
        return token;
    }
    1
}

/// Resolve directory paths and additional grants without opening anything.
fn resolve_roots<T: Copy>(
    required: u64,
    mut read: Option<T>,
    mut write: Option<T>,
    data_only: bool,
    identity: bool,
    write_only: bool,
    combined: bool,
) -> Result<(Option<T>, Option<T>, u64), &'static core::ffi::CStr> {
    let single_root = read.is_some() && write.is_none();
    if data_only && !single_root {
        return Err(c"--data-only requires exactly one --dir=PATH grant\n");
    }
    if identity && !single_root {
        return Err(c"--dir-id requires exactly one --dir=PATH\n");
    }
    let mut grants = 0;
    if write_only {
        if !single_root {
            return Err(c"--dir-rights=write requires one --dir=PATH and no --write-dir\n");
        }
        write = read.take();
        grants |= WRITE_ROOT;
    }
    if required & FULL_ROOT != 0 {
        if !combined || read.is_none() || write.is_some() {
            return Err(c"Dir requires --dir=PATH --dir-rights=read,write\n");
        }
        write = read;
        grants |= FULL_ROOT;
    } else if read.is_some() {
        if required & READ_ROOT == 0 {
            return Err(c"use --dir-rights=write or --write-dir=PATH for Dir<Write>\n");
        }
        grants |= READ_ROOT;
    }
    Ok((read, write, grants))
}

/// The launcher supplies pointers to its live path and grant variables.
pub(super) fn witchy_directory_grants(
    required: u64,
    grants: &mut u64,
    read: &mut *const core::ffi::c_char,
    write: &mut *const core::ffi::c_char,
    data_only: bool,
    identity: bool,
    write_only: bool,
    combined: bool,
    stderr: *mut core::ffi::c_void,
) -> bool {
    unsafe extern "C" {
        fn fputs(text: *const core::ffi::c_char, stream: *mut core::ffi::c_void) -> c_int;
    }
    let present = |path: *const core::ffi::c_char| (!path.is_null()).then_some(path);
    match resolve_roots(
        required,
        present(*read),
        present(*write),
        data_only,
        identity,
        write_only,
        combined,
    ) {
        Ok((read_path, write_path, additional)) => {
            *read = read_path.unwrap_or(core::ptr::null());
            *write = write_path.unwrap_or(core::ptr::null());
            *grants |= additional;
            true
        }
        Err(message) => {
            unsafe { fputs(message.as_ptr(), stderr) };
            false
        }
    }
}
