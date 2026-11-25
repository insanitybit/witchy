//! Byte-oriented flag classification for the native launcher.

use core::ffi::{CStr, c_char};

fn same_identity(fd: core::ffi::c_int, device: u64, inode: u64) -> bool {
    #[cfg(target_os = "macos")]
    {
        super::seatbelt::stat(fd).is_some_and(|v| v.device as u64 == device && v.inode == inode)
    }
    #[cfg(target_os = "linux")]
    {
        super::linux_stat::stat(fd).is_some_and(|v| v.device == device && v.inode == inode)
    }
}

struct CommonFlags {
    streaming: bool,
    plain: bool,
    unmetered: bool,
}

/// Return 1 for an applied option, 0 for an unhandled option, or -1 for a
/// diagnostic already emitted. The launcher supplies live argument storage.
unsafe fn witchy_apply_common_flag(
    flag: u32,
    arg: *const c_char,
    grants: *mut u64,
    options: *mut CommonFlags,
    stderr: *mut core::ffi::c_void,
) -> core::ffi::c_int {
    unsafe extern "C" {
        fn witchy_bind_limit_label(input: *const c_char) -> bool;
        fn fputs(text: *const c_char, stream: *mut core::ffi::c_void) -> core::ffi::c_int;
    }
    let options = unsafe { &mut *options };
    match flag {
        1 => unsafe { *grants |= 1 },
        2 if !options.streaming => options.streaming = true,
        3 if !options.plain => options.plain = true,
        4 if unsafe { witchy_bind_limit_label(arg.add(c"--limit-name=".count_bytes())) } => {}
        4 => {
            unsafe {
                fputs(
                    c"invalid or duplicate --limit-name=FLAG=TEXT\n".as_ptr(),
                    stderr,
                )
            };
            return -1;
        }
        5 | 6 => {
            unsafe {
                fputs(c"native instruction fuel is unavailable; use the Wasmtime target for a fuel-limited launch\n".as_ptr(), stderr)
            };
            return -1;
        }
        7 if !options.unmetered => options.unmetered = true,
        8 => unsafe { *grants |= 2 },
        _ => return 0,
    }
    1
}

struct ConnectionFlags {
    limit: u64,
    wait: u64,
    deadline: u64,
    seen: u32,
    enabled: bool,
    fs_entries: u64,
}

unsafe fn witchy_apply_connection_flag(
    flag: u32,
    arg: *const c_char,
    required: u64,
    options: *mut ConnectionFlags,
) -> bool {
    unsafe extern "C" {
        fn witchy_budget_configure(
            text: *const c_char,
            network: bool,
            per_connection: bool,
        ) -> bool;
    }
    let options = unsafe { &mut *options };
    if flag == 16 {
        return !core::mem::replace(&mut options.enabled, true);
    }
    if flag == 49 || flag == 50 {
        let bit = 1 << (flag - 44);
        let rights = if flag == 49 {
            256 | 1024 | 4096 | 8192
        } else {
            4096 | 8192
        };
        if options.seen & bit != 0 || required & rights == 0 {
            return false;
        }
        let prefix = if flag == 49 {
            c"--fs-bytes-per-connection="
        } else {
            c"--fs-entries-per-connection="
        };
        let bytes = unsafe { CStr::from_ptr(arg.add(prefix.count_bytes())) }.to_bytes();
        let Some(limit) = super::budget::parse_limit(bytes) else {
            return false;
        };
        if flag == 49 {
            super::io_budget::set_connection_filesystem(limit);
        } else {
            options.fs_entries = limit;
        }
        options.seen |= bit;
        return true;
    }
    if !(17..=21).contains(&flag) {
        return false;
    }
    let bit = 1 << (flag - 17);
    if options.seen & bit != 0 {
        return false;
    }
    if flag >= 20 {
        let network = flag == 20;
        if !network && required & 1 == 0 {
            return false;
        }
        let prefix = if network {
            c"--net-bytes-per-connection="
        } else {
            c"--output-bytes-per-connection="
        };
        if !unsafe { witchy_budget_configure(arg.add(prefix.count_bytes()), network, true) } {
            return false;
        }
    } else {
        let (prefix, maximum, target) = match flag {
            17 => (c"--connections=", u64::MAX, &mut options.limit),
            18 => (c"--waiting-connections=", 4096, &mut options.wait),
            _ => (
                c"--connection-deadline-ms=",
                86_400_000,
                &mut options.deadline,
            ),
        };
        let value = unsafe { CStr::from_ptr(arg.add(prefix.count_bytes())) }.to_bytes();
        let Some(value) = super::budget::parse_limit(value).filter(|&n| n != 0 && n <= maximum)
        else {
            return false;
        };
        *target = value;
    }
    options.seen |= bit;
    true
}

struct BudgetFlags {
    deadline: u64,
    input: u64,
    entries: u64,
    seen: u32,
}

unsafe fn witchy_apply_budget_flag(
    flag: u32,
    arg: *const c_char,
    required: u64,
    options: *mut BudgetFlags,
    stderr: *mut core::ffi::c_void,
) -> core::ffi::c_int {
    unsafe extern "C" {
        fn witchy_budget_configure(
            text: *const c_char,
            network: bool,
            per_connection: bool,
        ) -> bool;
        fn witchy_filesystem_limit(text: *const c_char) -> bool;
        fn fputs(text: *const c_char, stream: *mut core::ffi::c_void) -> core::ffi::c_int;
    }
    let (bit, grants, prefix, diagnostic) = match flag {
        35 => (
            1,
            u64::MAX,
            c"--deadline-ms=",
            c"invalid native --deadline-ms limit\n",
        ),
        36 => (2, 1, c"--output-bytes=", c"invalid --output-bytes limit\n"),
        37 => (4, 8, c"--input-bytes=", c"invalid --input-bytes limit\n"),
        39 => (
            8,
            2048 | 4096 | 8192,
            c"--fs-entries=",
            c"invalid --fs-entries limit\n",
        ),
        40 => (
            16,
            256 | 1024 | 2048 | 4096 | 8192,
            c"--fs-bytes=",
            c"invalid --fs-bytes limit\n",
        ),
        41 => (
            32,
            4 | 512 | 131072,
            c"--net-bytes=",
            c"invalid --net-bytes limit\n",
        ),
        _ => return 0,
    };
    let options = unsafe { &mut *options };
    if (flag != 35 && required & grants == 0) || options.seen & bit != 0 {
        return 0;
    }
    let value = unsafe { arg.add(prefix.count_bytes()) };
    let valid = match flag {
        36 | 41 => unsafe { witchy_budget_configure(value, flag == 41, false) },
        40 => unsafe { witchy_filesystem_limit(value) },
        _ => {
            let parsed = super::budget::parse_limit(unsafe { CStr::from_ptr(value) }.to_bytes());
            let parsed = parsed.filter(|&n| flag != 35 || (n != 0 && n <= u64::from(u32::MAX)));
            if let Some(value) = parsed {
                match flag {
                    35 => options.deadline = value,
                    37 => options.input = value,
                    _ => options.entries = value,
                }
                true
            } else {
                false
            }
        }
    };
    if !valid {
        unsafe { fputs(diagnostic.as_ptr(), stderr) };
        return -1;
    }
    options.seen |= bit;
    1
}

struct PathFlags {
    read_dir: *const c_char,
    write_dir: *const c_char,
    file: *const c_char,
    output: *const c_char,
    input: *const c_char,
    input_identity: super::file_identity::Identity,
    directory_identity: super::file_identity::Identity,
    data_only: bool,
    write_only: bool,
    combined: bool,
    input_id_set: bool,
    directory_id_set: bool,
}

unsafe fn witchy_apply_path_flag(
    flag: u32,
    arg: *const c_char,
    required: u64,
    grants: *mut u64,
    paths: *mut PathFlags,
    stderr: *mut core::ffi::c_void,
) -> core::ffi::c_int {
    use super::file_identity::Identity;
    unsafe extern "C" {
        fn witchy_parse_file_identity(text: *const c_char, identity: *mut Identity) -> bool;
        fn fputs(text: *const c_char, stream: *mut core::ffi::c_void) -> core::ffi::c_int;
    }
    const INPUT: u64 = 8;
    const READ_FILE: u64 = 256;
    const READ_DIR: u64 = 1024;
    const WRITE_FILE: u64 = 2048;
    const WRITE_DIR: u64 = 4096;
    const FULL_DIR: u64 = 8192;
    let paths = unsafe { &mut *paths };
    let grants = unsafe { &mut *grants };
    let (path, prefix, grant) = match flag {
        14 if required & WRITE_FILE != 0 => (&mut paths.output, c"--write-file=", WRITE_FILE),
        15 if required & WRITE_DIR != 0 => (&mut paths.write_dir, c"--write-dir=", WRITE_DIR),
        26 if required & (READ_DIR | WRITE_DIR | FULL_DIR) != 0 => {
            (&mut paths.read_dir, c"--dir=", 0)
        }
        27 if required & READ_FILE != 0 => (&mut paths.file, c"--file=", READ_FILE),
        44 if required & INPUT != 0 && *grants & INPUT == 0 => {
            (&mut paths.input, c"--input=", INPUT)
        }
        22 if !paths.data_only => {
            paths.data_only = true;
            return 1;
        }
        24 if required & FULL_DIR != 0 && !paths.combined => {
            paths.combined = true;
            return 1;
        }
        25 if required & WRITE_DIR != 0 && !paths.write_only => {
            paths.write_only = true;
            return 1;
        }
        23 | 38 => {
            let (seen, identity, prefix, diagnostic) = if flag == 23 {
                (
                    &mut paths.directory_id_set,
                    &mut paths.directory_identity,
                    c"--dir-id=",
                    c"invalid --dir-id=DEVICE:INODE\n",
                )
            } else {
                if required & INPUT == 0 {
                    return 0;
                }
                (
                    &mut paths.input_id_set,
                    &mut paths.input_identity,
                    c"--input-id=",
                    c"invalid --input-id=DEVICE:INODE\n",
                )
            };
            if *seen {
                return 0;
            }
            if !unsafe { witchy_parse_file_identity(arg.add(prefix.count_bytes()), identity) } {
                unsafe { fputs(diagnostic.as_ptr(), stderr) };
                return -1;
            }
            *seen = true;
            return 1;
        }
        _ => return 0,
    };
    if !path.is_null() {
        return 0;
    }
    *path = unsafe { arg.add(prefix.count_bytes()) };
    *grants |= grant;
    1
}

use core::ffi::{c_int, c_void};

const OUTPUT: u64 = 1;
const NET: u64 = 4;
const INPUT: u64 = 8;
const CLOCK: u64 = 16;
const RAND: u64 = 32;
const ARGS: u64 = 64;
const ENV: u64 = 128;
const LISTENER: u64 = 512;
const POLICY: u64 = 16384;
const ENTRY_SOCKET: u64 = 131072;

unsafe extern "C" {
    static witchy_entry_grants: u64;
    static witchy_launch_plan: u8;
    #[cfg_attr(target_os = "macos", link_name = "__stdoutp")]
    #[cfg_attr(target_os = "linux", link_name = "stdout")]
    static mut CRT_OUT: *mut c_void;
    #[cfg_attr(target_os = "macos", link_name = "__stderrp")]
    #[cfg_attr(target_os = "linux", link_name = "stderr")]
    static mut CRT_ERR: *mut c_void;
    fn fputs(text: *const c_char, stream: *mut c_void) -> c_int;
    fn fprintf(stream: *mut c_void, format: *const c_char, ...) -> c_int;
    fn witchy_launch_console(
        out: *mut c_void,
        err: *mut c_void,
        output: bool,
        streaming: bool,
        plain: bool,
    ) -> bool;
    #[cfg(target_os = "linux")]
    safe fn witchy_linux_prepare_supervisor() -> bool;
}

/// The process CRT supplies the argument array and the standard streams.
#[unsafe(no_mangle)]
unsafe extern "C" fn main(argc: c_int, argv: *mut *mut c_char) -> c_int {
    let (out, err, required) = unsafe { (CRT_OUT, CRT_ERR, witchy_entry_grants) };
    #[cfg(target_os = "linux")]
    if !witchy_linux_prepare_supervisor() {
        return 2;
    }
    let mut grants = 0;
    let mut common = CommonFlags {
        streaming: false,
        plain: false,
        unmetered: false,
    };
    let mut mac = NativeLaunch::new();
    if unsafe { witchy_launch_plan } != 0 {
        return unsafe { launch_manifest(argc, argv, required, out, err) };
    }
    for index in 1..argc {
        let arg = unsafe { *argv.add(index as usize) };
        let flag = super::launch_option::classify(unsafe { CStr::from_ptr(arg) }.to_bytes());
        let handled = unsafe { witchy_apply_common_flag(flag, arg, &mut grants, &mut common, err) };
        let handled = if handled == 0 {
            unsafe { mac.apply(flag, arg, required, &mut grants, err) }
        } else {
            handled
        };
        if handled == 0 {
            unsafe {
                fputs(c"usage: executable [--output] [--allocator] [--input] [--clock] [--rand] [--net=IP:PORT] [--listen=IP:PORT]\n".as_ptr(), err)
            };
        }
        if handled <= 0 {
            return 2;
        }
    }
    if !unsafe {
        witchy_launch_console(
            out,
            err,
            grants & OUTPUT != 0,
            common.streaming,
            common.plain,
        )
    } {
        return 2;
    }
    unsafe { mac.run(required, grants, out, err) }
}

struct NativeLaunch {
    listener: *const c_char,
    paths: PathFlags,
    words: u32,
    arg_bytes: u32,
    budget: BudgetFlags,
    connection: ConnectionFlags,
    exec_dir: *const c_char,
    exec_env_seen: bool,
    exec_processes_seen: bool,
    exec_budget_seen: bool,
    exec_budget: u64,
    dir_fd: c_int,
    write_dir_fd: c_int,
    file_fd: c_int,
    input_fd: c_int,
    exec_fd: c_int,
    output_fd: c_int,
    output_exclusive: bool,
    manifest: bool,
}

impl NativeLaunch {
    fn new() -> Self {
        let null = core::ptr::null();
        use super::file_identity::Identity;
        Self {
            listener: null,
            paths: PathFlags {
                read_dir: null,
                write_dir: null,
                file: null,
                output: null,
                input: null,
                input_identity: Identity {
                    device: 0,
                    inode: 0,
                },
                directory_identity: Identity {
                    device: 0,
                    inode: 0,
                },
                data_only: false,
                write_only: false,
                combined: false,
                input_id_set: false,
                directory_id_set: false,
            },
            words: 0,
            arg_bytes: 0,
            budget: BudgetFlags {
                deadline: 0,
                input: u64::MAX,
                entries: u64::MAX,
                seen: 0,
            },
            connection: ConnectionFlags {
                limit: u64::MAX,
                wait: 16,
                deadline: 30_000,
                seen: 0,
                enabled: false,
                fs_entries: u64::MAX,
            },
            exec_dir: null,
            exec_env_seen: false,
            exec_processes_seen: false,
            exec_budget_seen: false,
            exec_budget: 1048576,
            dir_fd: -1,
            write_dir_fd: -1,
            file_fd: -1,
            input_fd: -1,
            exec_fd: -1,
            output_fd: -1,
            output_exclusive: false,
            manifest: false,
        }
    }

    unsafe fn apply(
        &mut self,
        flag: u32,
        arg: *mut c_char,
        required: u64,
        grants: &mut u64,
        err: *mut c_void,
    ) -> c_int {
        unsafe extern "C" {
            fn witchy_bind_endpoints(text: *mut c_char) -> bool;
            fn witchy_bind_policy_grant(text: *const c_char) -> bool;
            fn witchy_bind_env(text: *const c_char) -> bool;
            fn witchy_args_limits(text: *const c_char, words: *mut u32, bytes: *mut u32) -> bool;
            fn witchy_args_bind(text: *const c_char) -> bool;
            #[cfg(witchy_native_aws_lc)]
            fn witchy_bind_store_secret(text: *const c_char, reveal: bool) -> bool;
            #[cfg(witchy_native_aws_lc)]
            fn witchy_bind_signer(text: *const c_char) -> bool;
        }
        let handled =
            unsafe { witchy_apply_budget_flag(flag, arg, required, &mut self.budget, err) };
        if handled != 0 {
            return handled;
        }
        let handled =
            unsafe { witchy_apply_path_flag(flag, arg, required, grants, &mut self.paths, err) };
        if handled != 0 {
            return handled;
        }
        if unsafe { witchy_apply_connection_flag(flag, arg, required, &mut self.connection) } {
            return 1;
        }
        let (grant, bound, diagnostic) = match flag {
            45 if required & 262144 != 0 && !self.exec_env_seen => {
                // SAFETY: flag 45 is `--exec-env=`, so its value is the rest of the live argument.
                let names = unsafe { CStr::from_ptr(arg.add(11)) };
                if !super::exec_environment::witchy_exec_env_names(names) {
                    unsafe {
                        fputs(c"invalid --exec-env=NAME,NAME grant\n".as_ptr(), err);
                    }
                    return -1;
                }
                self.exec_env_seen = true;
                return 1;
            }
            46 if required & 262144 != 0 && self.exec_dir.is_null() => {
                let text = unsafe { arg.add(7) };
                let bytes = unsafe { CStr::from_ptr(text) }.to_bytes();
                let Some(colon) = bytes.iter().rposition(|&b| b == b':').filter(|&n| n != 0) else {
                    unsafe {
                        fputs(c"invalid --exec=DIR:name,name grant\n".as_ptr(), err);
                    }
                    return -1;
                };
                if !unsafe { super::exec_guest::bind_programs(text.add(colon + 1)) } {
                    unsafe {
                        fputs(c"invalid --exec=DIR:name,name grant\n".as_ptr(), err);
                    }
                    return -1;
                }
                unsafe {
                    text.add(colon).write(0);
                }
                self.exec_dir = text;
                *grants |= 262144;
                return 1;
            }
            47 if required & 262144 != 0 && !self.exec_processes_seen => {
                let value = unsafe { CStr::from_ptr(arg.add(17)) }.to_bytes();
                let Some(value) =
                    super::budget::parse_limit(value).filter(|&n| n != 0 && n <= 65536)
                else {
                    unsafe {
                        fputs(c"invalid --exec-processes limit\n".as_ptr(), err);
                    }
                    return -1;
                };
                #[cfg(target_os = "linux")]
                {
                    unsafe extern "C" {
                        safe fn witchy_exec_process_limit(limit: u32);
                    }
                    witchy_exec_process_limit(value as u32);
                }
                #[cfg(target_os = "macos")]
                let _ = value;
                self.exec_processes_seen = true;
                return 1;
            }
            48 if required & 262144 != 0 && !self.exec_budget_seen => {
                let value = unsafe { CStr::from_ptr(arg.add(13)) }.to_bytes();
                let Some(value) = super::budget::parse_limit(value).filter(|&n| n <= 268435456)
                else {
                    unsafe {
                        fputs(c"invalid --exec-bytes limit\n".as_ptr(), err);
                    }
                    return -1;
                };
                self.exec_budget = value;
                self.exec_budget_seen = true;
                return 1;
            }
            9 => return 1, // Path resolution already rejects links.
            #[cfg(witchy_native_aws_lc)]
            10 | 11 if required & 65536 != 0 => {
                let reveal = flag == 10;
                (
                    65536,
                    unsafe {
                        witchy_bind_store_secret(arg.add(if reveal { 9 } else { 14 }), reveal)
                    },
                    c"invalid or duplicate native secret store binding\n",
                )
            }
            #[cfg(witchy_native_aws_lc)]
            12 if required & (32768 | super::tls_grant::TLS_SIGNER) != 0 => {
                let text = unsafe { arg.add(14) };
                let binding = super::signer_binding::parse(unsafe { CStr::from_ptr(text) }.to_bytes());
                let grant = if binding.is_some_and(|slot| slot.kind == 4) {
                    super::tls_grant::TLS_SIGNER
                } else {
                    32768
                };
                (grant, required & grant != 0 && unsafe { witchy_bind_signer(text) },
                    c"invalid or duplicate native secret slot binding\n")
            }
            13 if required & POLICY != 0 => (
                POLICY,
                unsafe { witchy_bind_policy_grant(arg.add(8)) },
                c"invalid or duplicate --grant=Type.field=VALUE binding\n",
            ),
            28 if required & ENV != 0 => (
                ENV,
                unsafe { witchy_bind_env(arg.add(6)) },
                c"invalid --env=NAME[=VALUE] binding\n",
            ),
            29 if required & ARGS != 0 && *grants & ARGS == 0 => (
                ARGS,
                unsafe { witchy_args_limits(arg.add(7), &mut self.words, &mut self.arg_bytes) },
                c"invalid --args=COUNT:BYTES grant\n",
            ),
            30 if required & ARGS != 0 => (
                0,
                unsafe { witchy_args_bind(arg.add(6)) },
                c"invalid or oversized UTF-8 argument\n",
            ),
            31 if required & RAND != 0 => {
                *grants |= RAND;
                return 1;
            }
            32 if required & CLOCK != 0 => {
                *grants |= CLOCK;
                return 1;
            }
            33 | 34 if required & INPUT != 0 && *grants & INPUT == 0 => {
                *grants |= INPUT;
                return 1;
            }
            42 if required & NET != 0 && *grants & NET == 0 => (
                NET,
                unsafe { witchy_bind_endpoints(arg.add(6)) },
                c"invalid native --net endpoint list\n",
            ),
            43 if required & (LISTENER | ENTRY_SOCKET) != 0 && self.listener.is_null() => {
                self.listener = unsafe { arg.add(9) };
                *grants |= required & (LISTENER | ENTRY_SOCKET);
                return 1;
            }
            _ => return 0,
        };
        if !bound {
            unsafe { fputs(diagnostic.as_ptr(), err) };
            return -1;
        }
        *grants |= grant;
        1
    }

    unsafe fn run(
        mut self,
        required: u64,
        mut grants: u64,
        out: *mut c_void,
        err: *mut c_void,
    ) -> c_int {
        use super::file_identity::Identity;
        unsafe extern "C" {
            #[cfg(witchy_native_aws_lc)]
            fn witchy_validate_secret_grants(required: u64, grants: u64, err: *mut c_void) -> bool;
            fn witchy_validate_connection_launch(
                required: u64,
                grants: u64,
                enabled: bool,
                listener: bool,
                options: bool,
                err: *mut c_void,
            ) -> bool;
            fn witchy_directory_grants(
                required: u64,
                grants: *mut u64,
                read: *mut *const c_char,
                write: *mut *const c_char,
                data: bool,
                identity: bool,
                write_only: bool,
                combined: bool,
                err: *mut c_void,
            ) -> bool;
            fn witchy_bind_path_grants(
                required: u64,
                grants: u64,
                file: *const c_char,
                file_fd: c_int,
                err: *mut c_void,
            ) -> bool;
            fn witchy_launch_input(
                path: *const c_char,
                opened: c_int,
                identity: *const Identity,
                bytes: u64,
                budget_set: bool,
                err: *mut c_void,
            ) -> bool;
            safe fn witchy_exec_granted();
            safe fn witchy_exec_sweep(pid: c_int);
            safe fn witchy_broker_exec_budget(bytes: u64);
            safe fn witchy_broker_output(parent: c_int, exclusive: bool);
            safe fn close(fd: c_int) -> c_int;
        }
        let paths = &mut self.paths;
        unsafe {
            #[cfg(witchy_native_aws_lc)]
            if !witchy_validate_secret_grants(required, grants, err) {
                return 2;
            }
            if !witchy_validate_connection_launch(
                required,
                grants,
                self.connection.enabled,
                !self.listener.is_null(),
                self.connection.seen != 0,
                err,
            ) || !witchy_directory_grants(
                required,
                &mut grants,
                &mut paths.read_dir,
                &mut paths.write_dir,
                paths.data_only,
                paths.directory_id_set,
                paths.write_only,
                paths.combined,
                err,
            ) || !super::witchy_validate_grants(
                required,
                grants,
                self.words,
                self.arg_bytes,
                err,
            ) {
                return 2;
            }
            grants &= required;
            if !witchy_bind_path_grants(required, grants, paths.file, self.file_fd, err)
                || !witchy_launch_input(
                    paths.input,
                    self.input_fd,
                    if paths.input_id_set {
                        &paths.input_identity
                    } else {
                        core::ptr::null()
                    },
                    self.budget.input,
                    self.budget.seen & 4 != 0,
                    err,
                )
            {
                return 2;
            }
            if grants & INPUT != 0 && !super::input_stream::safe_stdin() {
                fputs(
                    c"native input is a secret's source or a file with more than one link\n"
                        .as_ptr(),
                    err,
                );
                return 2;
            }
            let writable_fd = if self.write_dir_fd >= 0 {
                self.write_dir_fd
            } else if self.manifest && self.dir_fd >= 0 && paths.write_only {
                self.dir_fd
            } else if !paths.write_dir.is_null() && paths.write_dir == paths.read_dir {
                self.dir_fd
            } else {
                -1
            };
            if !super::broker_directory::prepare_secret_protection(paths.write_dir, writable_fd) {
                fputs(c"native secrets: the writable directory grant holds a secret's object, or could not be walked completely; refused\n".as_ptr(), err);
                return 2;
            }
            let exec_fd = if grants & 262144 != 0 {
                let fd = if self.exec_fd >= 0 {
                    self.exec_fd
                } else {
                    super::exec_grant::open_directory(self.exec_dir)
                };
                if fd < 0
                    || !super::exec_grant::separated_from_writes(
                        fd,
                        paths.write_dir,
                        writable_fd,
                        paths.output,
                        self.output_fd,
                    )
                {
                    if fd >= 0 {
                        close(fd);
                    }
                    fputs(
                        c"the exec directory could not be opened or overlaps a writable grant\n"
                            .as_ptr(),
                        err,
                    );
                    return 2;
                }
                super::exec_guest::configure_budget(self.exec_budget);
                witchy_broker_exec_budget(self.exec_budget);
                witchy_exec_granted();
                witchy_exec_sweep(0);
                fd
            } else {
                -1
            };
            let (programs, program_count) = super::exec_guest::program_names();
            if self.output_fd >= 0 {
                witchy_broker_output(self.output_fd, self.output_exclusive);
            }
            super::witchy_launch_native(
                grants,
                self.connection.enabled,
                self.listener,
                self.connection.limit,
                self.connection.deadline,
                self.budget.deadline,
                self.connection.wait,
                paths.read_dir,
                paths.write_dir,
                self.dir_fd,
                if self.manifest && paths.write_only {
                    self.dir_fd
                } else {
                    self.write_dir_fd
                },
                exec_fd,
                programs,
                program_count,
                paths.output,
                if paths.directory_id_set {
                    &paths.directory_identity
                } else {
                    core::ptr::null()
                },
                self.budget.entries,
                self.connection.fs_entries,
                paths.data_only,
                self.exec_budget,
                self.budget.input,
                self.budget.seen & 4 != 0,
                out,
                err,
            )
        }
    }
}

fn manifest_grant(record: &super::launch_plan::Record) -> Option<u64> {
    let tag = unsafe { CStr::from_ptr(record.fields[0]) }.to_bytes();
    Some(match tag {
        b"output" => 1,
        b"allocator" => 2,
        b"net" => 4,
        b"input" => 8,
        b"clock" => 16,
        b"rand" => 32,
        b"args" => 64,
        b"env" => 128,
        b"file" => 256,
        b"listen" => 512,
        b"write-file" => 2048,
        b"grant" => 16384,
        b"secret" => match unsafe { CStr::from_ptr(record.fields[2]) }.to_bytes() {
            b"sign/tls13/server-p384" => super::tls_grant::TLS_SIGNER,
            _ => 32768,
        },
        b"store" => 65536,
        b"socket" => 131072,
        b"exec" => 262144,
        b"dir" => match unsafe { CStr::from_ptr(record.fields[1]) }.to_bytes() {
            b"read" => 1024,
            b"write" => 4096,
            b"all" => 8192,
            _ => return None,
        },
        _ => 0,
    })
}

unsafe fn launch_manifest(
    argc: c_int,
    argv: *mut *mut c_char,
    required: u64,
    out: *mut c_void,
    err: *mut c_void,
) -> c_int {
    use super::launch_plan::opening::{witchy_launch_open, witchy_launch_open_parent};
    use super::launch_plan::{Launch, witchy_launch_arguments, witchy_launch_read};
    #[cfg(witchy_native_aws_lc)]
    use super::launch_plan::{witchy_launch_descriptor, witchy_launch_take};
    use core::{mem::MaybeUninit, ptr};
    unsafe extern "C" {
        safe fn close(fd: c_int) -> c_int;
        fn witchy_bind_env(text: *const c_char) -> bool;
        fn witchy_bind_policy_grant(text: *const c_char) -> bool;
        fn witchy_args_bind(text: *const c_char) -> bool;
        fn witchy_launch_console(
            out: *mut c_void,
            err: *mut c_void,
            output: bool,
            streaming: bool,
            plain: bool,
        ) -> bool;
        #[cfg(witchy_native_aws_lc)]
        safe fn witchy_secret_add_object(device: u64, inode: u64) -> bool;
        #[cfg(witchy_native_aws_lc)]
        fn witchy_bind_signer(text: *const c_char) -> bool;
        #[cfg(witchy_native_aws_lc)]
        fn witchy_bind_store_secret(text: *const c_char, reveal: bool) -> bool;
        #[cfg(witchy_native_aws_lc)]
        fn snprintf(out: *mut c_char, length: usize, format: *const c_char, ...) -> c_int;
    }
    static mut PLAN: MaybeUninit<Launch> = MaybeUninit::uninit();
    static mut OUTPUT_NAME: [c_char; 256] = [0; 256];
    let launch_ptr = (&raw mut PLAN).cast::<Launch>();
    let fail = || {
        unsafe {
            fputs(
                c"launch refused: invalid or unsupported embedded launch plan\n".as_ptr(),
                err,
            );
        }
        2
    };
    if !unsafe { witchy_launch_read((&raw const witchy_launch_plan).cast(), launch_ptr) } {
        return fail();
    }
    let launch = unsafe { &mut *launch_ptr };
    let mut planned = 0u64;
    for record in &launch.records[..launch.record_count as usize] {
        let Some(grant) = manifest_grant(record) else {
            return fail();
        };
        const UNIQUE: u64 = 1024 | 4096 | 8192 | 256 | 2048 | 8 | 64 | 512 | 262144 | 131072;
        if grant & planned & UNIQUE != 0 {
            return fail();
        }
        planned |= grant;
    }
    if planned != required
        || !unsafe { witchy_launch_arguments(launch, argc, argv.cast(), required & 64 != 0) }
    {
        return fail();
    }
    let mut mac = NativeLaunch::new();
    mac.manifest = true;
    let mut grants = 0u64;
    let mut last_opened = -1;
    let mut object_names: [*const c_char; 64] = [ptr::null(); 64];
    let mut object_count = 0;
    let read_marker = c"(launch plan directory)".as_ptr();
    let write_marker = c"(launch plan writable directory)".as_ptr();
    let file_marker = c"(launch plan file)".as_ptr();
    for index in 0..launch.record_count as usize {
        let record = launch.records[index];
        let field = record.fields;
        let tag = unsafe { CStr::from_ptr(field[0]) }.to_bytes();
        let grant = manifest_grant(&record).unwrap_or(0);
        grants |= grant;
        match tag {
            b"env" => {
                if !unsafe { witchy_bind_env(field[1]) } {
                    return fail();
                }
            }
            b"args" => {
                let parse = |value: *const c_char, max: u64| -> Option<u32> {
                    let bytes = unsafe { CStr::from_ptr(value) }.to_bytes();
                    if !bytes.iter().all(u8::is_ascii_digit) {
                        return None;
                    }
                    super::budget::parse_limit(bytes)
                        .filter(|&n| n <= max)
                        .map(|n| n as u32)
                };
                let Some(words) = parse(field[1], 256) else {
                    return fail();
                };
                let Some(bytes) = parse(field[2], 65536) else {
                    return fail();
                };
                mac.words = words;
                mac.arg_bytes = bytes;
                let mut total = 0usize;
                for &word in &launch.words[..launch.word_count as usize] {
                    let value = unsafe { CStr::from_ptr(word) }.to_bytes();
                    total = match total.checked_add(value.len()) {
                        Some(n) => n,
                        None => return fail(),
                    };
                    if launch.word_count > words
                        || total > bytes as usize
                        || !super::launch_args::valid_utf8(value)
                    {
                        unsafe {
                            fprintf(err, c"launch refused: the words after `--` are at most %u, %u bytes in all, UTF-8\n".as_ptr(), words, bytes);
                        }
                        return 2;
                    }
                    if !unsafe { witchy_args_bind(word) } {
                        return fail();
                    }
                }
            }
            b"grant" => {
                if !unsafe { witchy_bind_policy_grant(field[1]) } {
                    return fail();
                }
            }
            b"net" => {
                if !unsafe { super::broker_socket::append_launch_endpoint(field[1]) } {
                    return fail();
                }
            }
            b"listen" => mac.listener = field[1],
            b"socket" => {
                mac.listener = field[1];
                mac.connection.enabled = true;
            }
            b"dir" => {
                let fd = unsafe {
                    witchy_launch_open(launch, 0, field[3], field[4], field[5], field[6])
                };
                if fd < 0 {
                    return fail();
                }
                last_opened = fd;
                match unsafe { CStr::from_ptr(field[2]) }.to_bytes() {
                    b"data-only" => mac.paths.data_only = true,
                    b"any" => {}
                    _ => return fail(),
                }
                match grant {
                    1024 => {
                        mac.paths.read_dir = read_marker;
                        mac.dir_fd = fd;
                    }
                    4096 if required & 1024 != 0 => {
                        mac.paths.write_dir = write_marker;
                        mac.write_dir_fd = fd;
                    }
                    4096 => {
                        mac.paths.read_dir = read_marker;
                        mac.dir_fd = fd;
                        mac.paths.write_only = true;
                    }
                    8192 => {
                        mac.paths.read_dir = read_marker;
                        mac.dir_fd = fd;
                        mac.paths.combined = true;
                    }
                    _ => return fail(),
                }
            }
            b"file" | b"input" => {
                if tag == b"input" && unsafe { CStr::from_ptr(field[1]) }.to_bytes() == b"stdin" {
                    continue;
                }
                let fd = unsafe {
                    witchy_launch_open(
                        launch,
                        if tag == b"input" { 2 } else { 1 },
                        field[1],
                        field[2],
                        field[3],
                        field[4],
                    )
                };
                if fd < 0 {
                    return fail();
                }
                last_opened = fd;
                if tag == b"file" {
                    mac.file_fd = fd;
                    mac.paths.file = file_marker;
                } else {
                    mac.input_fd = fd;
                }
            }
            b"write-file" => {
                let mode = unsafe { CStr::from_ptr(field[1]) }.to_bytes();
                if mode != b"replace" && mode != b"create" {
                    return fail();
                }
                let fd = unsafe {
                    witchy_launch_open_parent(
                        launch,
                        field[2],
                        field[3],
                        field[4],
                        field[5],
                        (&raw mut OUTPUT_NAME).cast(),
                    )
                };
                if fd < 0 {
                    return fail();
                }
                mac.output_fd = fd;
                mac.output_exclusive = mode == b"create";
                mac.paths.output = (&raw const OUTPUT_NAME).cast();
            }
            b"exec-env" => {
                // SAFETY: a non-null plan field is a live C string in the launch record.
                if mac.exec_env_seen
                    || field[1].is_null()
                    || !super::exec_environment::witchy_exec_env_names(unsafe {
                        CStr::from_ptr(field[1])
                    })
                {
                    return fail();
                }
                mac.exec_env_seen = true;
            }
            b"exec" => {
                let fd = unsafe {
                    witchy_launch_open(launch, 0, field[1], field[2], c"-".as_ptr(), c"-".as_ptr())
                };
                if fd < 0 || !unsafe { super::exec_guest::bind_programs(field[3]) } {
                    return fail();
                }
                mac.exec_fd = fd;
            }
            b"limit" => {
                const KNOWN: [&[u8]; 15] = [
                    b"deadline-ms",
                    b"output-bytes",
                    b"input-bytes",
                    b"fs-bytes",
                    b"fs-entries",
                    b"net-bytes",
                    b"exec-bytes",
                    b"exec-processes",
                    b"connections",
                    b"waiting-connections",
                    b"connection-deadline-ms",
                    b"net-bytes-per-connection",
                    b"output-bytes-per-connection",
                    b"fs-bytes-per-connection",
                    b"fs-entries-per-connection",
                ];
                let name = unsafe { CStr::from_ptr(field[1]) }.to_bytes();
                let value = unsafe { CStr::from_ptr(field[2]) }.to_bytes();
                if !KNOWN.contains(&name)
                    || value.is_empty()
                    || value.len() > 15
                    || !value.iter().all(u8::is_ascii_digit)
                {
                    return fail();
                }
                let mut argument = [0u8; 128];
                let length = 2 + name.len() + 1 + value.len();
                if length >= argument.len() {
                    return fail();
                }
                argument[..2].copy_from_slice(b"--");
                argument[2..2 + name.len()].copy_from_slice(name);
                argument[2 + name.len()] = b'=';
                argument[3 + name.len()..length].copy_from_slice(value);
                let flag = super::launch_option::classify(&argument[..length]);
                if flag == 0
                    || unsafe {
                        mac.apply(
                            flag,
                            argument.as_mut_ptr().cast(),
                            required,
                            &mut grants,
                            err,
                        )
                    } != 1
                {
                    return fail();
                }
            }
            b"identity" | b"object" => {
                let source = unsafe { CStr::from_ptr(field[1]) }.to_bytes();
                let name = unsafe { CStr::from_ptr(field[2]) }.to_bytes();
                if source != b"arg"
                    || object_count == object_names.len()
                    || object_names[..object_count]
                        .iter()
                        .any(|&used| unsafe { CStr::from_ptr(used) }.to_bytes() == name)
                {
                    return fail();
                }
                object_names[object_count] = field[2];
                object_count += 1;
                let Some(flag) = launch.flags[..launch.flag_count as usize]
                    .iter()
                    .find(|flag| unsafe { CStr::from_ptr(flag.name) }.to_bytes() == name)
                else {
                    return fail();
                };
                if !flag.kind_object || flag.value.is_null() {
                    return fail();
                }
                let Some(expected) =
                    super::file_identity::parse(unsafe { CStr::from_ptr(flag.value) }.to_bytes())
                else {
                    return fail();
                };
                if tag == b"identity" {
                    if last_opened < 0
                        || !same_identity(last_opened, expected.device, expected.inode)
                    {
                        unsafe {
                            fputs(c"launch refused: the opened object is not the one named DEVICE:INODE\n".as_ptr(), err);
                        }
                        return 2;
                    }
                    last_opened = -1;
                } else {
                    #[cfg(witchy_native_aws_lc)]
                    if !witchy_secret_add_object(expected.device, expected.inode) {
                        return fail();
                    }
                    #[cfg(not(witchy_native_aws_lc))]
                    return fail();
                }
            }
            b"secret" | b"store" => {
                #[cfg(not(witchy_native_aws_lc))]
                return fail();
                #[cfg(witchy_native_aws_lc)]
                {
                    if tag == b"store" {
                        let mode = unsafe { CStr::from_ptr(field[2]) }.to_bytes();
                        if (mode != b"reveal" && mode != b"seal")
                            || !unsafe { witchy_bind_store_secret(field[1], mode == b"reveal") }
                        {
                            return fail();
                        }
                    } else {
                        let kind = unsafe { CStr::from_ptr(field[3]) }.to_bytes();
                        let mut buffer = [0 as c_char; 512];
                        let size = if kind == b"env" {
                            unsafe {
                                snprintf(
                                    buffer.as_mut_ptr(),
                                    buffer.len(),
                                    c"%s:%s:env:%s".as_ptr(),
                                    field[1],
                                    field[2],
                                    field[4],
                                )
                            }
                        } else {
                            let number = if kind == b"arg" {
                                unsafe { witchy_launch_descriptor(launch, field[4]) }
                            } else if kind == b"fd" {
                                super::budget::parse_limit(
                                    unsafe { CStr::from_ptr(field[4]) }.to_bytes(),
                                )
                                .and_then(|n| c_int::try_from(n).ok())
                                .unwrap_or(-1)
                            } else {
                                -1
                            };
                            let copy = unsafe { witchy_launch_take(launch, number) };
                            if copy < 0 {
                                return fail();
                            }
                            unsafe {
                                snprintf(
                                    buffer.as_mut_ptr(),
                                    buffer.len(),
                                    c"%s:%s:fd:%d".as_ptr(),
                                    field[1],
                                    field[2],
                                    copy,
                                )
                            }
                        };
                        if size <= 0
                            || size as usize >= buffer.len()
                            || !unsafe { witchy_bind_signer(buffer.as_ptr()) }
                        {
                            unsafe {
                                fputs(c"launch refused: the secret could not be bound from its declared source\n".as_ptr(), err);
                            }
                            return 2;
                        }
                    }
                }
            }
            _ => {}
        }
    }
    if launch.cwd >= 0 {
        close(launch.cwd);
        launch.cwd = -1;
    }
    if !unsafe { witchy_launch_console(out, err, grants & OUTPUT != 0, false, false) } {
        return fail();
    }
    unsafe { mac.run(required, grants, out, err) }
}
