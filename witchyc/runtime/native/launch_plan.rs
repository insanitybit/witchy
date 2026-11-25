//! Launch-plan ABI. The caller owns the returned plan storage and its descriptors.
use core::ffi::{c_char, c_int, c_uint, c_void};
use core::ptr;

const RECORDS: usize = 512;
const FLAGS: usize = 32;
const FIELDS: usize = 8;
const WORDS: usize = 256;

#[repr(C)]
#[derive(Copy, Clone)]
pub(super) struct Record {
    pub(super) count: c_uint,
    pub(super) fields: [*mut c_char; FIELDS],
}

#[repr(C)]
#[derive(Copy, Clone)]
pub(super) struct Flag {
    pub(super) name: *const c_char,
    pub(super) descriptor: bool,
    pub(super) secret: bool,
    pub(super) value: *const c_char,
    default_kind: *const c_char,
    default_value: *const c_char,
    pub(super) kind_object: bool,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct Source {
    number: c_int,
    copy: c_int,
    taken: bool,
}

#[repr(C)]
pub(super) struct Launch {
    text: *mut c_char,
    pub(super) records: [Record; RECORDS],
    pub(super) record_count: c_uint,
    pub(super) flags: [Flag; FLAGS],
    pub(super) flag_count: c_uint,
    pub(super) words: [*const c_char; WORDS],
    pub(super) word_count: c_uint,
    pub(super) cwd: c_int,
    sources: [Source; RECORDS],
    source_count: c_uint,
}

unsafe extern "C" {
    fn strnlen(s: *const c_char, maximum: usize) -> usize;
    fn strdup(s: *const c_char) -> *mut c_char;
    fn write(fd: c_int, bytes: *const c_void, len: usize) -> isize;
    fn fcntl(fd: c_int, command: c_int, ...) -> c_int;
}

fn refuse() -> bool {
    let message = b"launch refused: launch plan: malformed or unavailable\n";
    unsafe {
        write(2, message.as_ptr().cast(), message.len());
    }
    false
}

fn argument_refuse(name: Option<&[u8]>, message: &[u8]) -> bool {
    let prefix = b"launch refused: ";
    unsafe {
        write(2, prefix.as_ptr().cast(), prefix.len());
    }
    if let Some(name) = name.filter(|name| {
        !name.is_empty() && name.len() <= 64 && name.iter().all(|b| (0x21..=0x7e).contains(b))
    }) {
        unsafe {
            write(2, b"--".as_ptr().cast(), 2);
            write(2, name.as_ptr().cast(), name.len());
            write(2, b" ".as_ptr().cast(), 1);
        }
    }
    unsafe {
        write(2, message.as_ptr().cast(), message.len());
        write(2, b"\n".as_ptr().cast(), 1);
    }
    false
}

/// Both pointers must be valid, and `embedded` must be NUL terminated.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn witchy_launch_read(
    embedded: *const c_char,
    launch: *mut Launch,
) -> bool {
    if embedded.is_null() || launch.is_null() {
        return refuse();
    }
    let length = unsafe { strnlen(embedded, 64 * 1024 + 1) };
    if length > 64 * 1024 {
        return refuse();
    }
    let input = unsafe { core::slice::from_raw_parts(embedded.cast::<u8>(), length) };
    if !super::launch_plan_validate::valid(input) {
        return refuse();
    }
    let text = unsafe { strdup(embedded) };
    if text.is_null() {
        return refuse();
    }
    unsafe {
        // The C ABI uses zero-initialized arrays and a sentinel for the cwd.
        ptr::write_bytes(launch, 0, 1);
        (*launch).cwd = -1;
        (*launch).text = text;
        let mut offset = 0;
        let mut first = true;
        while offset < length {
            let start = offset;
            while *text.add(offset) != b'\n' as c_char {
                offset += 1;
            }
            *text.add(offset) = 0;
            let end = offset;
            offset += 1;
            if first {
                first = false;
                continue;
            }
            if end - start == 3
                && core::slice::from_raw_parts(text.add(start).cast::<u8>(), 3) == b"end"
            {
                break;
            }
            let mut record = Record {
                count: 0,
                fields: [ptr::null_mut(); FIELDS],
            };
            let mut pos = start;
            loop {
                record.fields[record.count as usize] = text.add(pos);
                record.count += 1;
                while pos < end && *text.add(pos) != b'\t' as c_char {
                    pos += 1;
                }
                if pos == end {
                    break;
                }
                *text.add(pos) = 0;
                pos += 1;
            }
            let tag = core::ffi::CStr::from_ptr(record.fields[0]).to_bytes();
            if tag == b"flag" {
                let kind = core::ffi::CStr::from_ptr(record.fields[2]).to_bytes();
                let secret = core::ffi::CStr::from_ptr(record.fields[3]).to_bytes() == b"1";
                let optional = core::ffi::CStr::from_ptr(record.fields[4]).to_bytes() != b"-";
                let flag = Flag {
                    name: record.fields[1],
                    descriptor: kind == b"fd",
                    secret,
                    value: ptr::null(),
                    default_kind: if optional {
                        record.fields[4]
                    } else {
                        ptr::null()
                    },
                    default_value: if optional {
                        record.fields[5]
                    } else {
                        ptr::null()
                    },
                    kind_object: kind == b"object",
                };
                (*launch).flags[(*launch).flag_count as usize] = flag;
                (*launch).flag_count += 1;
            }
            (*launch).records[(*launch).record_count as usize] = record;
            (*launch).record_count += 1;
        }
    }
    true
}

fn descriptor_number(value: &[u8]) -> Option<c_int> {
    if value.is_empty() || value.len() > 4 || value[0] == b'0' {
        return None;
    }
    let mut number = 0;
    for &byte in value {
        if !byte.is_ascii_digit() {
            return None;
        }
        number = number * 10 + c_int::from(byte - b'0');
    }
    (3..=1023).contains(&number).then_some(number)
}

fn valid_object(value: &[u8]) -> bool {
    let Some(split) = value.iter().position(|&b| b == b':') else {
        return false;
    };
    let (device, inode) = value.split_at(split);
    let inode = &inode[1..];
    !device.is_empty()
        && device.len() <= 20
        && !inode.is_empty()
        && inode.len() <= 20
        && device.iter().chain(inode).all(u8::is_ascii_digit)
}

unsafe fn bytes<'a>(value: *const c_char) -> &'a [u8] {
    unsafe { core::ffi::CStr::from_ptr(value).to_bytes() }
}

unsafe fn find_flag(launch: &Launch, name: &[u8]) -> Option<usize> {
    (0..launch.flag_count as usize).find(|&i| unsafe { bytes(launch.flags[i].name) == name })
}

/// Checks all argument names and values before duplicating inherited descriptors.
/// `argv` must contain `argc` non-null C strings, live for the launch's lifetime.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn witchy_launch_arguments(
    launch: *mut Launch,
    argc: c_int,
    argv: *const *const c_char,
    words: bool,
) -> bool {
    if launch.is_null() || argv.is_null() || argc < 0 {
        return refuse();
    }
    let launch = unsafe { &mut *launch };
    let mut i = 1usize;
    let mut after_words = false;
    while i < argc as usize {
        let argument = unsafe { *argv.add(i) };
        if argument.is_null() {
            return refuse();
        }
        let arg = unsafe { bytes(argument) };
        if after_words {
            if launch.word_count as usize == WORDS {
                return refuse();
            }
            launch.words[launch.word_count as usize] = argument;
            launch.word_count += 1;
            i += 1;
            continue;
        }
        if arg == b"--" {
            if !words {
                return argument_refuse(None, b"this executable takes no words after `--`");
            }
            after_words = true;
            i += 1;
            continue;
        }
        let Some(name_value) = arg.strip_prefix(b"--").filter(|v| !v.is_empty()) else {
            return argument_refuse(None, b"argument is not a declared `--NAME`");
        };
        let equal = name_value.iter().position(|&byte| byte == b'=');
        let name = equal.map_or(name_value, |n| &name_value[..n]);
        let Some(index) = (unsafe { find_flag(launch, name) }) else {
            return argument_refuse(Some(name), b"is not a declared argument");
        };
        if !launch.flags[index].value.is_null() {
            return argument_refuse(Some(name), b"is given twice");
        }
        let value = if let Some(n) = equal {
            unsafe { argument.add(2 + n + 1) }
        } else {
            i += 1;
            if i >= argc as usize {
                return argument_refuse(Some(name), b"needs a value");
            }
            let value = unsafe { *argv.add(i) };
            if value.is_null() || unsafe { bytes(value) }.starts_with(b"--") {
                return argument_refuse(Some(name), b"needs a value");
            }
            value
        };
        let data = unsafe { bytes(value) };
        let flag = &mut launch.flags[index];
        let valid = if flag.descriptor {
            descriptor_number(data).is_some()
        } else if flag.default_kind.is_null() && flag.kind_object {
            valid_object(data)
        } else {
            !data.is_empty() && data.len() <= 4096
        };
        if !valid {
            return argument_refuse(
                Some(name),
                if flag.descriptor {
                    b"is a descriptor number, 3 to 1023"
                } else if flag.kind_object {
                    b"is DEVICE:INODE"
                } else {
                    b"is a path of 1 to 4096 bytes"
                },
            );
        }
        flag.value = value;
        i += 1;
    }
    if let Some(flag) = launch.flags[..launch.flag_count as usize]
        .iter()
        .find(|flag| flag.value.is_null() && flag.default_kind.is_null())
    {
        return argument_refuse(Some(unsafe { bytes(flag.name) }), b"is required");
    }
    // Check every source before duplicating any: an earlier duplicate must not
    // occupy a descriptor number named by a later source.
    for index in 0..launch.record_count as usize {
        let record = launch.records[index];
        let tag = unsafe { bytes(record.fields[0]) };
        let at = match tag {
            b"dir" | b"secret" => 3,
            b"file" | b"input" | b"exec" => 1,
            _ => continue,
        };
        if at + 1 >= record.count as usize {
            continue;
        }
        let kind = unsafe { bytes(record.fields[at]) };
        let value = unsafe { bytes(record.fields[at + 1]) };
        let number = if kind == b"fd" {
            descriptor_number(value)
        } else if kind == b"arg" {
            unsafe { find_flag(launch, value) }.and_then(|i| {
                let flag = &launch.flags[i];
                if flag.descriptor && !flag.value.is_null() {
                    descriptor_number(unsafe { bytes(flag.value) })
                } else {
                    None
                }
            })
        } else {
            None
        };
        if kind == b"fd" && number.is_none() {
            return refuse();
        }
        let Some(number) = number else {
            continue;
        };
        if launch.sources[..launch.source_count as usize]
            .iter()
            .any(|source| source.number == number)
        {
            return argument_refuse(None, b"names a descriptor another source also names");
        }
        if unsafe { fcntl(number, 1) } < 0 {
            return argument_refuse(None, b"is not an open descriptor");
        }
        launch.sources[launch.source_count as usize] = Source {
            number,
            copy: -1,
            taken: false,
        };
        launch.source_count += 1;
    }
    #[cfg(target_os = "macos")]
    const DUPFD_CLOEXEC: c_int = 67;
    #[cfg(target_os = "linux")]
    const DUPFD_CLOEXEC: c_int = 1030;
    for source in &mut launch.sources[..launch.source_count as usize] {
        source.copy = unsafe { fcntl(source.number, DUPFD_CLOEXEC, 3 as c_int) };
        if source.copy < 0 {
            return refuse();
        }
    }
    true
}

#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn witchy_launch_take(launch: *mut Launch, number: c_int) -> c_int {
    if launch.is_null() || number < 0 {
        return -1;
    }
    let launch = unsafe { &mut *launch };
    for source in &mut launch.sources[..launch.source_count as usize] {
        if source.number == number {
            if source.taken {
                return -1;
            }
            source.taken = true;
            return source.copy;
        }
    }
    -1
}

#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn witchy_launch_descriptor(
    launch: *const Launch,
    name: *const c_char,
) -> c_int {
    if launch.is_null() || name.is_null() {
        return -1;
    }
    let launch = unsafe { &*launch };
    let Some(index) = (unsafe { find_flag(launch, bytes(name)) }) else {
        return -1;
    };
    let flag = &launch.flags[index];
    if !flag.descriptor || flag.value.is_null() {
        return -1;
    }
    descriptor_number(unsafe { bytes(flag.value) }).unwrap_or(-1)
}

pub(super) mod opening {
    use super::*;
    use crate::launch_walk::{self, Boundary};
    #[cfg(target_os = "linux")]
    use crate::linux_stat::stat;
    #[cfg(target_os = "macos")]
    use crate::seatbelt::stat;

    #[cfg(target_os = "macos")]
    const DIRECTORY: c_int = 0x0010_0000;
    #[cfg(target_os = "linux")]
    const DIRECTORY: c_int = 0o40000;
    #[cfg(target_os = "macos")]
    const CLOEXEC: c_int = 0x0100_0000;
    #[cfg(target_os = "linux")]
    const CLOEXEC: c_int = 0o2000000;
    #[cfg(target_os = "macos")]
    const DUP_CLOEXEC: c_int = 67;
    #[cfg(target_os = "linux")]
    const DUP_CLOEXEC: c_int = 1030;

    #[repr(C)]
    struct Passwd {
        name: *mut c_char,
        password: *mut c_char,
        uid: u32,
        gid: u32,
        #[cfg(target_os = "macos")]
        change: i64,
        #[cfg(target_os = "macos")]
        class: *mut c_char,
        gecos: *mut c_char,
        directory: *mut c_char,
        shell: *mut c_char,
        #[cfg(target_os = "macos")]
        expire: i64,
    }
    unsafe extern "C" {
        fn open(path: *const c_char, flags: c_int, ...) -> c_int;
        safe fn close(fd: c_int) -> c_int;
        safe fn getuid() -> u32;
        fn getpwuid_r(
            uid: u32,
            entry: *mut Passwd,
            buffer: *mut c_char,
            length: usize,
            result: *mut *mut Passwd,
        ) -> c_int;
        #[cfg(target_os = "macos")]
        safe fn sysconf(name: c_int) -> isize;
        fn malloc(size: usize) -> *mut c_void;
        fn free(value: *mut c_void);
    }

    fn private(fd: c_int) -> c_int {
        if fd < 0 {
            return -1;
        }
        if fd > 2 {
            if unsafe { fcntl(fd, 2, 1 as c_int) } == 0 {
                return fd;
            }
            close(fd);
            return -1;
        }
        let result = unsafe { fcntl(fd, DUP_CLOEXEC, 3 as c_int) };
        close(fd);
        result
    }

    fn root() -> c_int {
        private(unsafe { open(c"/".as_ptr(), DIRECTORY | CLOEXEC) })
    }

    fn home() -> c_int {
        #[cfg(target_os = "macos")]
        let hint = sysconf(71);
        #[cfg(target_os = "linux")]
        let hint = 0;
        let length = if hint > 0 && hint < (1 << 20) {
            hint as usize
        } else {
            16384
        };
        let storage = unsafe { malloc(length) };
        if storage.is_null() {
            return -1;
        }
        let mut entry = core::mem::MaybeUninit::<Passwd>::uninit();
        let mut found = ptr::null_mut();
        let status = unsafe {
            getpwuid_r(
                getuid(),
                entry.as_mut_ptr(),
                storage.cast(),
                length,
                &mut found,
            )
        };
        let result = if status == 0 && !found.is_null() && !unsafe { (*found).directory }.is_null()
        {
            let path = unsafe { bytes((*found).directory) };
            if let Some(relative) = path.strip_prefix(b"/") {
                launch_walk::walk(root(), relative, true, None, true)
            } else {
                -1
            }
        } else {
            -1
        };
        unsafe {
            free(storage);
        }
        result
    }

    fn startup(launch: &mut Launch) -> c_int {
        if launch.cwd < 0 {
            launch.cwd = private(unsafe { open(c".".as_ptr(), DIRECTORY | CLOEXEC) });
        }
        if launch.cwd < 0 {
            -1
        } else {
            unsafe { fcntl(launch.cwd, DUP_CLOEXEC, 3 as c_int) }
        }
    }

    unsafe fn inherited(launch: &mut Launch, number: c_int, object: c_int) -> c_int {
        let copy = unsafe { witchy_launch_take(launch, number) };
        let flags = if copy >= 0 {
            unsafe { fcntl(copy, 3) }
        } else {
            -1
        };
        let info = if flags >= 0 { stat(copy) } else { None };
        let valid = info.as_ref().is_some_and(|info| {
            let kind = info.mode & 0o170000;
            if object == 0 {
                kind == 0o040000
            } else {
                (kind == 0o100000 || (object == 2 && kind == 0o010000)) && flags & 3 == 0
            }
        });
        if valid {
            copy
        } else {
            if copy >= 0 {
                close(copy);
            }
            if flags < 0 || info.is_none() {
                launch_walk::refused(
                    b"source is not an open descriptor, or another source already took it",
                )
            } else if object == 0 {
                launch_walk::refused(b"source is not a directory")
            } else {
                launch_walk::refused(b"source is not a read-only file")
            }
        }
    }

    unsafe fn resolve(
        launch: &mut Launch,
        object: c_int,
        kind: &[u8],
        value: &[u8],
        within_kind: &[u8],
        within_value: &[u8],
    ) -> c_int {
        if !(0..=2).contains(&object) {
            return -1;
        }
        let flag_index = if kind == b"arg" {
            unsafe { find_flag(launch, value) }
        } else {
            None
        };
        let boundary = if within_kind != b"-" {
            let Some(index) = flag_index else {
                return -1;
            };
            if launch.flags[index].descriptor {
                return -1;
            }
            let fd = unsafe { resolve(launch, 0, within_kind, within_value, b"-", b"-") };
            if fd < 0 {
                return -1;
            }
            let info = stat(fd);
            close(fd);
            let Some(info) = info else {
                return -1;
            };
            Some(Boundary {
                #[cfg(target_os = "macos")]
                device: info.device as u32 as u64,
                #[cfg(target_os = "linux")]
                device: info.device,
                inode: info.inode,
            })
        } else {
            None
        };
        let directory = object == 0;
        match kind {
            b"fd" => unsafe { inherited(launch, descriptor_number(value).unwrap_or(-1), object) },
            b"literal" => match value.strip_prefix(b"/") {
                Some(path) => launch_walk::walk(root(), path, directory, boundary, false),
                None => -1,
            },
            b"home" => launch_walk::walk(home(), value, directory, boundary, false),
            b"cwd" => launch_walk::walk(startup(launch), value, directory, boundary, false),
            b"arg" => {
                let Some(index) = flag_index else {
                    return -1;
                };
                let flag = launch.flags[index];
                if flag.value.is_null() && !flag.default_kind.is_null() && !flag.descriptor {
                    return unsafe {
                        resolve(
                            launch,
                            object,
                            bytes(flag.default_kind),
                            bytes(flag.default_value),
                            within_kind,
                            within_value,
                        )
                    };
                }
                if flag.kind_object || flag.value.is_null() {
                    return -1;
                }
                let path = unsafe { bytes(flag.value) };
                if flag.descriptor {
                    return unsafe {
                        inherited(launch, descriptor_number(path).unwrap_or(-1), object)
                    };
                }
                if let Some(relative) = path.strip_prefix(b"/") {
                    launch_walk::walk(root(), relative, directory, boundary, true)
                } else {
                    launch_walk::walk(startup(launch), path, directory, boundary, true)
                }
            }
            _ => -1,
        }
    }

    #[unsafe(no_mangle)]
    pub(crate) unsafe extern "C" fn witchy_launch_open(
        launch: *mut Launch,
        object: c_int,
        kind: *const c_char,
        value: *const c_char,
        within_kind: *const c_char,
        within_value: *const c_char,
    ) -> c_int {
        if launch.is_null()
            || kind.is_null()
            || value.is_null()
            || within_kind.is_null()
            || within_value.is_null()
        {
            return -1;
        }
        unsafe {
            resolve(
                &mut *launch,
                object,
                bytes(kind),
                bytes(value),
                bytes(within_kind),
                bytes(within_value),
            )
        }
    }

    unsafe fn parent(
        launch: &mut Launch,
        kind: &[u8],
        value: &[u8],
        within_kind: &[u8],
        within_value: &[u8],
        name: *mut c_char,
    ) -> c_int {
        let flag_index = if kind == b"arg" {
            unsafe { find_flag(launch, value) }
        } else {
            None
        };
        if kind == b"arg" && flag_index.is_none() {
            return -1;
        }
        if let Some(index) = flag_index {
            let flag = launch.flags[index];
            if flag.kind_object {
                return -1;
            }
            if flag.value.is_null() && !flag.default_kind.is_null() && !flag.descriptor {
                return unsafe {
                    parent(
                        launch,
                        bytes(flag.default_kind),
                        bytes(flag.default_value),
                        within_kind,
                        within_value,
                        name,
                    )
                };
            }
        }
        let path = if let Some(index) = flag_index {
            let flag = launch.flags[index];
            if flag.descriptor || flag.value.is_null() {
                return -1;
            }
            unsafe { bytes(flag.value) }
        } else {
            if ![b"literal".as_slice(), b"home", b"cwd"].contains(&kind) {
                return -1;
            }
            value
        };
        let slash = path.iter().rposition(|&byte| byte == b'/');
        let leaf = slash.map_or(path, |at| &path[at + 1..]);
        if leaf.is_empty() || leaf.len() > 255 || leaf == b"." || leaf == b".." {
            return launch_walk::refused(b"written source names no file in a directory");
        }
        let mut directory = [0u8; 4098];
        let part = match slash {
            Some(0) => b"/".as_slice(),
            Some(at) => &path[..at],
            None if kind == b"literal" => b"/".as_slice(),
            None => b".".as_slice(),
        };
        if part.len() > 4096 {
            return -1;
        }
        directory[..part.len()].copy_from_slice(part);
        unsafe {
            ptr::copy_nonoverlapping(leaf.as_ptr(), name.cast(), leaf.len());
            name.add(leaf.len()).write(0);
        }
        if let Some(index) = flag_index {
            let old = launch.flags[index].value;
            launch.flags[index].value = directory.as_ptr().cast();
            let fd = unsafe { resolve(launch, 0, kind, value, within_kind, within_value) };
            launch.flags[index].value = old;
            fd
        } else {
            unsafe { resolve(launch, 0, kind, part, within_kind, within_value) }
        }
    }

    #[unsafe(no_mangle)]
    pub(crate) unsafe extern "C" fn witchy_launch_open_parent(
        launch: *mut Launch,
        kind: *const c_char,
        value: *const c_char,
        within_kind: *const c_char,
        within_value: *const c_char,
        name: *mut c_char,
    ) -> c_int {
        if launch.is_null()
            || kind.is_null()
            || value.is_null()
            || within_kind.is_null()
            || within_value.is_null()
            || name.is_null()
        {
            return -1;
        }
        unsafe {
            parent(
                &mut *launch,
                bytes(kind),
                bytes(value),
                bytes(within_kind),
                bytes(within_value),
                name,
            )
        }
    }

    #[cfg(test)]
    #[test]
    fn resolves_parent_under_bound_directory() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let root =
            std::env::temp_dir().join(format!("witchy-launch-source-{}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        let root = std::fs::canonicalize(root).unwrap();
        std::fs::write(root.join("input"), b"ok").unwrap();
        let root_c = CString::new(root.as_os_str().as_bytes()).unwrap();
        let path_c = CString::new(root.join("input").as_os_str().as_bytes()).unwrap();
        let text = CString::new(concat!(
            "witchy-launch-plan\t1\t0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\n",
            "flag\tinput\tpath\t0\t-\t-\nend\n"
        )).unwrap();
        let program = CString::new("program").unwrap();
        let flag = CString::new("--input").unwrap();
        let argv = [program.as_ptr(), flag.as_ptr(), path_c.as_ptr()];
        let mut launch: Launch = unsafe { core::mem::zeroed() };
        assert!(unsafe { witchy_launch_read(text.as_ptr(), &mut launch) });
        assert!(unsafe { witchy_launch_arguments(&mut launch, 3, argv.as_ptr(), false) });
        let fd = unsafe {
            witchy_launch_open(
                &mut launch,
                1,
                c"arg".as_ptr(),
                c"input".as_ptr(),
                c"literal".as_ptr(),
                root_c.as_ptr(),
            )
        };
        assert!(fd >= 0);
        let mut leaf = [0 as c_char; 256];
        let dir = unsafe {
            witchy_launch_open_parent(
                &mut launch,
                c"arg".as_ptr(),
                c"input".as_ptr(),
                c"literal".as_ptr(),
                root_c.as_ptr(),
                leaf.as_mut_ptr(),
            )
        };
        assert!(dir >= 0);
        assert_eq!(
            unsafe { core::ffi::CStr::from_ptr(leaf.as_ptr()) }.to_bytes(),
            b"input"
        );
        unsafe {
            close(fd);
            close(dir);
            free(launch.text.cast());
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::{CStr, CString};

    unsafe extern "C" {
        fn free(p: *mut c_void);
    }

    #[test]
    fn plan_keeps_flag_and_record_fields_in_owned_storage() {
        let text = CString::new(concat!(
            "witchy-launch-plan\t1\t0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\n",
            "flag\tinput\tpath\t1\thome\tfiles\noutput\nend\n"
        )).unwrap();
        let mut launch: Launch = unsafe { core::mem::zeroed() };
        assert!(unsafe { witchy_launch_read(text.as_ptr(), &mut launch) });
        assert_eq!(launch.cwd, -1);
        assert_eq!(launch.record_count, 2);
        assert_eq!(launch.flag_count, 1);
        assert!(launch.flags[0].secret);
        assert!(!launch.flags[0].descriptor);
        assert_eq!(
            unsafe { CStr::from_ptr(launch.flags[0].name) }.to_bytes(),
            b"input"
        );
        assert_eq!(
            unsafe { CStr::from_ptr(launch.flags[0].default_kind) }.to_bytes(),
            b"home"
        );
        assert_eq!(
            unsafe { CStr::from_ptr(launch.records[1].fields[0]) }.to_bytes(),
            b"output"
        );
        unsafe { free(launch.text.cast()) };
    }

    #[test]
    fn malformed_plan_is_refused() {
        let text = CString::new("witchy-launch-plan\t1\tbad\nend\n").unwrap();
        let mut launch: Launch = unsafe { core::mem::zeroed() };
        assert!(!unsafe { witchy_launch_read(text.as_ptr(), &mut launch) });
    }

    #[test]
    fn binds_and_takes_a_descriptor_only_once() {
        unsafe extern "C" {
            fn pipe(fds: *mut c_int) -> c_int;
            safe fn close(fd: c_int) -> c_int;
        }
        let mut pipe_fds = [0; 2];
        assert_eq!(unsafe { pipe(pipe_fds.as_mut_ptr()) }, 0);
        let text = CString::new(concat!(
            "witchy-launch-plan\t1\t0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\n",
            "flag\tsource\tfd\t0\t-\t-\nfile\targ\tsource\t-\t-\nend\n"
        )).unwrap();
        let name = CString::new("source").unwrap();
        let program = CString::new("program").unwrap();
        let flag = CString::new("--source").unwrap();
        let value = CString::new(pipe_fds[0].to_string()).unwrap();
        let argv = [program.as_ptr(), flag.as_ptr(), value.as_ptr()];
        let mut launch: Launch = unsafe { core::mem::zeroed() };
        assert!(unsafe { witchy_launch_read(text.as_ptr(), &mut launch) });
        assert!(unsafe {
            witchy_launch_arguments(&mut launch, argv.len() as c_int, argv.as_ptr(), false)
        });
        assert_eq!(
            unsafe { witchy_launch_descriptor(&launch, name.as_ptr()) },
            pipe_fds[0]
        );
        let copy = unsafe { witchy_launch_take(&mut launch, pipe_fds[0]) };
        assert!(copy >= 3 && copy != pipe_fds[0]);
        assert_eq!(unsafe { witchy_launch_take(&mut launch, pipe_fds[0]) }, -1);
        unsafe {
            close(copy);
            close(pipe_fds[0]);
            close(pipe_fds[1]);
            free(launch.text.cast());
        }
    }

    #[test]
    fn refuses_two_sources_naming_one_descriptor_before_copying() {
        unsafe extern "C" {
            fn pipe(fds: *mut c_int) -> c_int;
            safe fn close(fd: c_int) -> c_int;
        }
        let mut pipe_fds = [0; 2];
        assert_eq!(unsafe { pipe(pipe_fds.as_mut_ptr()) }, 0);
        let text = CString::new(format!(concat!(
            "witchy-launch-plan\t1\t0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\n",
            "file\tfd\t{}\t-\t-\ninput\tfd\t{}\t-\t-\nend\n"
        ), pipe_fds[0], pipe_fds[0])).unwrap();
        let program = CString::new("program").unwrap();
        let argv = [program.as_ptr()];
        let mut launch: Launch = unsafe { core::mem::zeroed() };
        assert!(unsafe { witchy_launch_read(text.as_ptr(), &mut launch) });
        assert!(!unsafe { witchy_launch_arguments(&mut launch, 1, argv.as_ptr(), false) });
        assert_eq!(launch.sources[0].copy, -1);
        unsafe {
            close(pipe_fds[0]);
            close(pipe_fds[1]);
            free(launch.text.cast());
        }
    }
}
