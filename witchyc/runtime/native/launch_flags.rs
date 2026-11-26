//! Byte-oriented flag classification for the native launcher.

use super::launch_plan::Argv;
use super::sys::Stream;
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

/// The option's text from byte `prefix` on. The classifier matched the
/// prefix, so the text is at least that long.
fn after(arg: &CStr, prefix: usize) -> &CStr {
    super::launch_plan::suffix(arg, prefix).unwrap_or(c"")
}

/// Write a diagnostic to one of the launcher's two streams.
fn say(text: &CStr, stream: Stream) {
    stream.put(text);
}

struct CommonFlags {
    streaming: bool,
    plain: bool,
    unmetered: bool,
}

/// Return 1 for an applied option, 0 for an unhandled option, or -1 for a
/// diagnostic already emitted. A limit's label is kept for the process, so
/// the option's text must last that long.
fn witchy_apply_common_flag(
    flag: u32,
    arg: &'static CStr,
    grants: &mut u64,
    options: &mut CommonFlags,
    stderr: Stream,
) -> core::ffi::c_int {
    use super::limit_labels::witchy_bind_limit_label;
    match flag {
        1 => *grants |= 1,
        2 if !options.streaming => options.streaming = true,
        3 if !options.plain => options.plain = true,
        4 if witchy_bind_limit_label(after(arg, c"--limit-name=".count_bytes())) => {}
        4 => {
            say(c"invalid or duplicate --limit-name=FLAG=TEXT\n", stderr);
            return -1;
        }
        5 | 6 => {
            say(c"native instruction fuel is unavailable; use the Wasmtime target for a fuel-limited launch\n", stderr);
            return -1;
        }
        7 if !options.unmetered => options.unmetered = true,
        8 => *grants |= 2,
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

fn witchy_apply_connection_flag(
    flag: u32,
    arg: &CStr,
    required: u64,
    options: &mut ConnectionFlags,
) -> bool {
    use super::io_budget::witchy_budget_configure;
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
        let bytes = after(arg, prefix.count_bytes()).to_bytes();
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
        if !witchy_budget_configure(after(arg, prefix.count_bytes()), network, true) {
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
        let value = after(arg, prefix.count_bytes()).to_bytes();
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

fn witchy_apply_budget_flag(
    flag: u32,
    arg: &CStr,
    required: u64,
    options: &mut BudgetFlags,
    stderr: Stream,
) -> core::ffi::c_int {
    use super::io_budget::{witchy_budget_configure, witchy_filesystem_limit};
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
    if (flag != 35 && required & grants == 0) || options.seen & bit != 0 {
        return 0;
    }
    let value = after(arg, prefix.count_bytes());
    let valid = match flag {
        36 | 41 => witchy_budget_configure(value, flag == 41, false),
        40 => witchy_filesystem_limit(value),
        _ => {
            let parsed = super::budget::parse_limit(value.to_bytes());
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
        say(diagnostic, stderr);
        return -1;
    }
    options.seen |= bit;
    1
}

struct PathFlags {
    read_dir: *const c_char,
    write_dir: *const c_char,
    file: Option<&'static CStr>,
    output: *const c_char,
    input: Option<&'static CStr>,
    input_identity: super::file_identity::Identity,
    directory_identity: super::file_identity::Identity,
    data_only: bool,
    write_only: bool,
    combined: bool,
    input_id_set: bool,
    directory_id_set: bool,
}

/// `storage` is the pointer `arg` was read from. The directory and output
/// paths are kept as pointers into it, since the broker takes them that way,
/// and the file and input paths are kept as text: the option must last for
/// the life of the process.
fn witchy_apply_path_flag(
    flag: u32,
    arg: &'static CStr,
    storage: *const c_char,
    required: u64,
    grants: &mut u64,
    paths: &mut PathFlags,
    stderr: Stream,
) -> core::ffi::c_int {
    const INPUT: u64 = 8;
    const READ_FILE: u64 = 256;
    const READ_DIR: u64 = 1024;
    const WRITE_FILE: u64 = 2048;
    const WRITE_DIR: u64 = 4096;
    const FULL_DIR: u64 = 8192;
    let (path, prefix, grant) = match flag {
        14 if required & WRITE_FILE != 0 => (&mut paths.output, c"--write-file=", WRITE_FILE),
        15 if required & WRITE_DIR != 0 => (&mut paths.write_dir, c"--write-dir=", WRITE_DIR),
        26 if required & (READ_DIR | WRITE_DIR | FULL_DIR) != 0 => {
            (&mut paths.read_dir, c"--dir=", 0)
        }
        27 | 44 => {
            let (path, prefix, grant) = if flag == 27 && required & READ_FILE != 0 {
                (&mut paths.file, c"--file=", READ_FILE)
            } else if flag == 44 && required & INPUT != 0 && *grants & INPUT == 0 {
                (&mut paths.input, c"--input=", INPUT)
            } else {
                return 0;
            };
            if path.is_some() {
                return 0;
            }
            *path = Some(after(arg, prefix.count_bytes()));
            *grants |= grant;
            return 1;
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
            let text = after(arg, prefix.count_bytes()).to_bytes();
            let Some(parsed) = super::file_identity::parse(text) else {
                say(diagnostic, stderr);
                return -1;
            };
            *identity = parsed;
            *seen = true;
            return 1;
        }
        _ => return 0,
    };
    if !path.is_null() {
        return 0;
    }
    *path = storage.wrapping_add(prefix.count_bytes());
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
    // The compiler emits both with the entry, and nothing writes either.
    safe static witchy_entry_grants: u64;
    /// The first byte of the launch plan's NUL-terminated text.
    safe static witchy_launch_plan: u8;
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
    let (out, err, required) = (Stream::stdout(), Stream::stderr(), witchy_entry_grants);
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
    // SAFETY: the C runtime passes `argc` NUL-terminated strings and keeps
    // them for the life of the process.
    let args: Option<Argv<'static>> = unsafe { Argv::new(argc, argv) };
    if witchy_launch_plan != 0 {
        return launch_manifest(args.as_ref(), required, out, err);
    }
    let args = args.unwrap_or(Argv::EMPTY);
    for index in 1..args.len() {
        let Some(arg) = args.get(index) else {
            return 2;
        };
        let flag = super::launch_option::classify(arg.to_bytes());
        let handled = witchy_apply_common_flag(flag, arg, &mut grants, &mut common, err);
        let handled = if handled == 0 {
            // SAFETY: the entry is this argument's own storage, which the C
            // runtime keeps for the life of the process, and `arg` is not
            // read after an option is split.
            unsafe { mac.apply(flag, args.raw(index), required, &mut grants, err) }
        } else {
            handled
        };
        if handled == 0 {
            say(c"usage: executable [--output] [--allocator] [--input] [--clock] [--rand] [--net=IP:PORT] [--listen=IP:PORT]\n", err);
        }
        if handled <= 0 {
            return 2;
        }
    }
    // SAFETY: both streams are the C runtime's own.
    if !unsafe {
        witchy_launch_console(
            out.as_ptr(),
            err.as_ptr(),
            grants & OUTPUT != 0,
            common.streaming,
            common.plain,
        )
    } {
        return 2;
    }
    mac.run(required, grants, out, err)
}

/// What the options or the plan asked for. Each pointer is null or text that
/// lasts for the life of the process: an argument, a field of the plan's
/// copy, or a static. `run` relies on that, and only this file sets them.
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
                file: None,
                output: null,
                input: None,
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

    /// The limit options. Each reads its text during the call and keeps
    /// nothing of it, so the text need only last that long. Returns as
    /// `apply` does.
    fn apply_limit(&mut self, flag: u32, text: &CStr, required: u64, err: Stream) -> c_int {
        let handled = witchy_apply_budget_flag(flag, text, required, &mut self.budget, err);
        if handled != 0 {
            return handled;
        }
        if witchy_apply_connection_flag(flag, text, required, &mut self.connection) {
            return 1;
        }
        self.apply_exec_limit(flag, text, required, err)
    }

    /// `--exec-processes=` and `--exec-bytes=`. Zero for any other option.
    fn apply_exec_limit(&mut self, flag: u32, text: &CStr, required: u64, err: Stream) -> c_int {
        match flag {
            47 if required & 262144 != 0 && !self.exec_processes_seen => {
                let value = after(text, 17).to_bytes();
                let Some(value) =
                    super::budget::parse_limit(value).filter(|&n| n != 0 && n <= 65536)
                else {
                    say(c"invalid --exec-processes limit\n", err);
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
                1
            }
            48 if required & 262144 != 0 && !self.exec_budget_seen => {
                let value = after(text, 13).to_bytes();
                let Some(value) = super::budget::parse_limit(value).filter(|&n| n <= 268435456)
                else {
                    say(c"invalid --exec-bytes limit\n", err);
                    return -1;
                };
                self.exec_budget = value;
                self.exec_budget_seen = true;
                1
            }
            _ => 0,
        }
    }

    /// Return 1 for an applied option, 0 for an unhandled option, or -1 for
    /// a diagnostic already emitted.
    ///
    /// `arg` is a writable NUL-terminated option that nothing else borrows
    /// and that lasts for the life of the process: the path, listener, exec
    /// and endpoint options keep pointers into it, and the endpoint and exec
    /// options are split in place.
    unsafe fn apply(
        &mut self,
        flag: u32,
        arg: *mut c_char,
        required: u64,
        grants: &mut u64,
        err: Stream,
    ) -> c_int {
        use super::launch_args::{witchy_args_bind, witchy_args_limits};
        use super::launch_env::witchy_bind_env;
        use super::policy_grants_abi::witchy_bind_policy_grant;
        #[cfg(witchy_native_aws_lc)]
        use super::signer_binding::witchy_bind_store_secret;
        unsafe extern "C" {
            #[cfg(witchy_native_aws_lc)]
            fn witchy_bind_signer(text: *const c_char) -> bool;
        }
        // SAFETY: the caller's promise. No arm reads `text` after it has
        // written through `arg`.
        let text: &'static CStr = unsafe { CStr::from_ptr(arg) };
        let handled = witchy_apply_budget_flag(flag, text, required, &mut self.budget, err);
        if handled != 0 {
            return handled;
        }
        let handled =
            witchy_apply_path_flag(flag, text, arg, required, grants, &mut self.paths, err);
        if handled != 0 {
            return handled;
        }
        if witchy_apply_connection_flag(flag, text, required, &mut self.connection) {
            return 1;
        }
        let (grant, bound, diagnostic) = match flag {
            45 if required & 262144 != 0 && !self.exec_env_seen => {
                if !super::exec_environment::witchy_exec_env_names(after(text, 11)) {
                    say(c"invalid --exec-env=NAME,NAME grant\n", err);
                    return -1;
                }
                self.exec_env_seen = true;
                return 1;
            }
            46 if required & 262144 != 0 && self.exec_dir.is_null() => {
                // The option is `--exec=` and its value, so the value starts
                // seven bytes in.
                let value = arg.wrapping_add(7);
                let bytes = after(text, 7).to_bytes();
                let Some(colon) = bytes.iter().rposition(|&b| b == b':').filter(|&n| n != 0) else {
                    say(c"invalid --exec=DIR:name,name grant\n", err);
                    return -1;
                };
                // SAFETY: the names start after the colon, within the option,
                // which lasts for the life of the process.
                if !unsafe { super::exec_guest::bind_programs(value.wrapping_add(colon + 1)) } {
                    say(c"invalid --exec=DIR:name,name grant\n", err);
                    return -1;
                }
                // SAFETY: the colon is a byte of the option, which is writable,
                // and `text` is not read again.
                unsafe {
                    value.add(colon).write(0);
                }
                self.exec_dir = value;
                *grants |= 262144;
                return 1;
            }
            47 | 48 => return self.apply_exec_limit(flag, text, required, err),
            9 => return 1, // Path resolution already rejects links.
            #[cfg(witchy_native_aws_lc)]
            10 | 11 if required & 65536 != 0 => {
                let reveal = flag == 10;
                (
                    65536,
                    witchy_bind_store_secret(after(text, if reveal { 9 } else { 14 }), reveal),
                    c"invalid or duplicate native secret store binding\n",
                )
            }
            #[cfg(witchy_native_aws_lc)]
            12 if required & (32768 | super::tls_grant::TLS_SIGNER) != 0 => {
                let binding = super::signer_binding::parse(after(text, 14).to_bytes());
                let grant = if binding.is_some_and(|slot| slot.kind == 4) {
                    super::tls_grant::TLS_SIGNER
                } else {
                    32768
                };
                // SAFETY: the option is `--secret-slot=` and its value, so the
                // value is a string fourteen bytes in.
                (grant, required & grant != 0 && unsafe { witchy_bind_signer(arg.add(14)) },
                    c"invalid or duplicate native secret slot binding\n")
            }
            13 if required & POLICY != 0 => (
                POLICY,
                witchy_bind_policy_grant(after(text, 8)),
                c"invalid or duplicate --grant=Type.field=VALUE binding\n",
            ),
            28 if required & ENV != 0 => (
                ENV,
                witchy_bind_env(after(text, 6)),
                c"invalid --env=NAME[=VALUE] binding\n",
            ),
            29 if required & ARGS != 0 && *grants & ARGS == 0 => (
                ARGS,
                witchy_args_limits(after(text, 7), &mut self.words, &mut self.arg_bytes),
                c"invalid --args=COUNT:BYTES grant\n",
            ),
            30 if required & ARGS != 0 => (
                0,
                witchy_args_bind(after(text, 6)),
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
                // SAFETY: the option is `--net=` and its value, so the value
                // is a writable string six bytes in that lasts for the life
                // of the process, and `text` is not read again.
                unsafe { super::broker_socket::witchy_bind_endpoints(arg.add(6)) },
                c"invalid native --net endpoint list\n",
            ),
            43 if required & (LISTENER | ENTRY_SOCKET) != 0 && self.listener.is_null() => {
                self.listener = arg.wrapping_add(9);
                *grants |= required & (LISTENER | ENTRY_SOCKET);
                return 1;
            }
            _ => return 0,
        };
        if !bound {
            say(diagnostic, err);
            return -1;
        }
        *grants |= grant;
        1
    }

    fn run(mut self, required: u64, mut grants: u64, out: Stream, err: Stream) -> c_int {
        use super::directory::witchy_directory_grants;
        use super::file_snapshot::witchy_bind_path_grants;
        use super::input_stream::witchy_launch_input;
        #[cfg(witchy_native_aws_lc)]
        use super::launch_env::witchy_validate_secret_grants;
        unsafe extern "C" {
            safe fn witchy_exec_granted();
            safe fn witchy_exec_sweep(pid: c_int);
            safe fn witchy_broker_exec_budget(bytes: u64);
            safe fn witchy_broker_output(parent: c_int, exclusive: bool);
            safe fn close(fd: c_int) -> c_int;
        }
        let paths = &mut self.paths;
        #[cfg(witchy_native_aws_lc)]
        if !witchy_validate_secret_grants(required, grants, err) {
            return 2;
        }
        // SAFETY: the stream is the C runtime's own.
        if !unsafe {
            super::connection_queue::witchy_validate_connection_launch(
                required,
                grants,
                self.connection.enabled,
                !self.listener.is_null(),
                self.connection.seen != 0,
                err.as_ptr(),
            )
        } || !witchy_directory_grants(
            required,
            &mut grants,
            &mut paths.read_dir,
            &mut paths.write_dir,
            paths.data_only,
            paths.directory_id_set,
            paths.write_only,
            paths.combined,
            err,
        ) || !super::witchy_validate_grants(required, grants, self.words, self.arg_bytes, err)
        {
            return 2;
        }
        grants &= required;
        if !witchy_bind_path_grants(required, grants, paths.file, self.file_fd, err)
            || !witchy_launch_input(
                paths.input,
                self.input_fd,
                paths.input_id_set.then_some(&paths.input_identity),
                self.budget.input,
                self.budget.seen & 4 != 0,
                err,
            )
        {
            return 2;
        }
        if grants & INPUT != 0 && !super::input_stream::safe_stdin() {
            say(
                c"native input is a secret's source or a file with more than one link\n",
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
        // SAFETY: the writable directory is null or a path that lasts for
        // the life of the process, as every pointer of a launch is.
        if !unsafe { super::broker_directory::prepare_secret_protection(paths.write_dir, writable_fd) }
        {
            say(c"native secrets: the writable directory grant holds a secret's object, or could not be walked completely; refused\n", err);
            return 2;
        }
        let exec_fd = if grants & 262144 != 0 {
            let fd = if self.exec_fd >= 0 {
                self.exec_fd
            } else {
                // SAFETY: as above, for the exec directory.
                super::exec_grant::open_directory(
                    (!self.exec_dir.is_null()).then(|| unsafe { CStr::from_ptr(self.exec_dir) }),
                )
            };
            // SAFETY: as above, for the writable directory and the output.
            if fd < 0
                || !unsafe {
                    super::exec_grant::separated_from_writes(
                        fd,
                        (!paths.write_dir.is_null()).then(|| CStr::from_ptr(paths.write_dir)),
                        writable_fd,
                        (!paths.output.is_null()).then(|| CStr::from_ptr(paths.output)),
                        self.output_fd,
                    )
                }
            {
                if fd >= 0 {
                    close(fd);
                }
                say(
                    c"the exec directory could not be opened or overlaps a writable grant\n",
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
        // SAFETY: every path is null or lasts for the life of the process,
        // the programs are the bound table and its length, and the identity
        // is null or this launch's own.
        unsafe {
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
    let tag = record.field(0)?.to_bytes();
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
        b"secret" => match record.field(2).map(CStr::to_bytes) {
            Some(b"sign/tls13/server-p384") => super::tls_grant::TLS_SIGNER,
            _ => 32768,
        },
        b"store" => 65536,
        b"socket" => 131072,
        b"exec" => 262144,
        b"dir" => match record.field(1).map(CStr::to_bytes) {
            Some(b"read") => 1024,
            Some(b"write") => 4096,
            Some(b"all") => 8192,
            _ => return None,
        },
        _ => 0,
    })
}

/// Four fields of a record in a row, when it has them all.
fn four(record: &super::launch_plan::Record, first: usize) -> Option<[&CStr; 4]> {
    Some([
        record.field(first)?,
        record.field(first + 1)?,
        record.field(first + 2)?,
        record.field(first + 3)?,
    ])
}

/// `args` is the process argument list.
fn launch_manifest(
    args: Option<&Argv<'static>>,
    required: u64,
    out: Stream,
    err: Stream,
) -> c_int {
    use super::launch_plan::Launch;
    /// The plan, which is too large for the stack. It is used once, to read
    /// the plan and bind what it names, and that use ends before the console
    /// is set up and the guest is launched: reading forks nothing, runs no
    /// guest code and does not come back here. What a launch keeps of the
    /// plan is text in the plan's own copy, which is not in this place.
    static PLAN: super::global::State<Launch> = super::global::State::new(Launch::ZERO);
    let (mac, grants) = match PLAN.with(|launch| read_manifest(launch, args, required, err)) {
        Ok(read) => read,
        Err(status) => return status,
    };
    // SAFETY: both streams are the C runtime's own.
    if !unsafe {
        witchy_launch_console(out.as_ptr(), err.as_ptr(), grants & OUTPUT != 0, false, false)
    } {
        say(
            c"launch refused: invalid or unsupported embedded launch plan\n",
            err,
        );
        return 2;
    }
    mac.run(required, grants, out, err)
}

/// Reads the embedded plan into `launch` and binds what it and the arguments
/// name. The launch to run and the grants the plan gives, or the status to
/// exit with after a diagnostic.
fn read_manifest(
    launch: &mut super::launch_plan::Launch,
    args: Option<&Argv<'static>>,
    required: u64,
    err: Stream,
) -> Result<(NativeLaunch, u64), c_int> {
    use super::launch_args::witchy_args_bind;
    use super::launch_env::witchy_bind_env;
    use super::launch_plan::opening::{witchy_launch_open, witchy_launch_open_parent};
    use super::launch_plan::{witchy_launch_arguments, witchy_launch_read};
    #[cfg(witchy_native_aws_lc)]
    use super::launch_plan::{witchy_launch_descriptor, witchy_launch_take};
    use super::policy_grants_abi::witchy_bind_policy_grant;
    #[cfg(witchy_native_aws_lc)]
    use super::signer_binding::witchy_bind_store_secret;
    unsafe extern "C" {
        safe fn close(fd: c_int) -> c_int;
        #[cfg(witchy_native_aws_lc)]
        safe fn witchy_secret_add_object(device: u64, inode: u64) -> bool;
        #[cfg(witchy_native_aws_lc)]
        fn witchy_bind_signer(text: *const c_char) -> bool;
        #[cfg(witchy_native_aws_lc)]
        fn snprintf(out: *mut c_char, length: usize, format: *const c_char, ...) -> c_int;
    }
    /// The name of the file a `write-file` record names, and the NUL after it.
    static OUTPUT_NAME: super::global::Table<c_char, 256> = super::global::Table::new(0);
    let fail = || {
        say(
            c"launch refused: invalid or unsupported embedded launch plan\n",
            err,
        );
        Err(2)
    };
    // SAFETY: the compiler emits the plan as NUL-terminated text.
    let plan = unsafe { CStr::from_ptr((&raw const witchy_launch_plan).cast()) };
    if !witchy_launch_read(plan, launch) {
        return fail();
    }
    let mut planned = 0u64;
    for record in launch.records() {
        let Some(grant) = manifest_grant(record) else {
            return fail();
        };
        const UNIQUE: u64 = 1024 | 4096 | 8192 | 256 | 2048 | 8 | 64 | 512 | 262144 | 131072;
        if grant & planned & UNIQUE != 0 {
            return fail();
        }
        planned |= grant;
    }
    if planned != required || !witchy_launch_arguments(launch, args, required & 64 != 0) {
        return fail();
    }
    let mut mac = NativeLaunch::new();
    mac.manifest = true;
    let mut grants = 0u64;
    let mut last_opened = -1;
    // The records whose third field names an identity or object already seen.
    let mut objects = [0usize; 64];
    let mut object_count = 0;
    let read_marker = c"(launch plan directory)".as_ptr();
    let write_marker = c"(launch plan writable directory)".as_ptr();
    let file_marker = c"(launch plan file)";
    for index in 0..launch.records().len() {
        let record = launch.records()[index];
        let Some(tag) = record.field(0).map(CStr::to_bytes) else {
            return fail();
        };
        let grant = manifest_grant(&record).unwrap_or(0);
        grants |= grant;
        match tag {
            b"env" => {
                if !record.field(1).is_some_and(witchy_bind_env) {
                    return fail();
                }
            }
            b"args" => {
                let parse = |value: Option<&CStr>, max: u64| -> Option<u32> {
                    let bytes = value?.to_bytes();
                    if !bytes.iter().all(u8::is_ascii_digit) {
                        return None;
                    }
                    super::budget::parse_limit(bytes)
                        .filter(|&n| n <= max)
                        .map(|n| n as u32)
                };
                let Some(words) = parse(record.field(1), 256) else {
                    return fail();
                };
                let Some(bytes) = parse(record.field(2), 65536) else {
                    return fail();
                };
                mac.words = words;
                mac.arg_bytes = bytes;
                let mut total = 0usize;
                for word in launch.words() {
                    let value = word.to_bytes();
                    total = match total.checked_add(value.len()) {
                        Some(n) => n,
                        None => return fail(),
                    };
                    if launch.word_count() > words
                        || total > bytes as usize
                        || !super::launch_args::valid_utf8(value)
                    {
                        // SAFETY: the stream is the C runtime's own, and the
                        // format takes the two unsigned numbers given.
                        unsafe {
                            fprintf(err.as_ptr(), c"launch refused: the words after `--` are at most %u, %u bytes in all, UTF-8\n".as_ptr(), words, bytes);
                        }
                        return Err(2);
                    }
                    if !witchy_args_bind(word) {
                        return fail();
                    }
                }
            }
            b"grant" => {
                if !record.field(1).is_some_and(witchy_bind_policy_grant) {
                    return fail();
                }
            }
            b"net" => {
                // SAFETY: the field is null or text in the plan's copy, which
                // lasts for the life of the process.
                if !unsafe { super::broker_socket::append_launch_endpoint(record.raw(1)) } {
                    return fail();
                }
            }
            b"listen" => mac.listener = record.raw(1),
            b"socket" => {
                mac.listener = record.raw(1);
                mac.connection.enabled = true;
            }
            b"dir" => {
                let Some([kind, value, within_kind, within_value]) = four(&record, 3) else {
                    return fail();
                };
                let fd = witchy_launch_open(launch, 0, kind, value, within_kind, within_value);
                if fd < 0 {
                    return fail();
                }
                last_opened = fd;
                match record.field(2).map(CStr::to_bytes) {
                    Some(b"data-only") => mac.paths.data_only = true,
                    Some(b"any") => {}
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
                if tag == b"input"
                    && record.field(1).is_some_and(|source| source.to_bytes() == b"stdin")
                {
                    continue;
                }
                let Some([kind, value, within_kind, within_value]) = four(&record, 1) else {
                    return fail();
                };
                let fd = witchy_launch_open(
                    launch,
                    if tag == b"input" { 2 } else { 1 },
                    kind,
                    value,
                    within_kind,
                    within_value,
                );
                if fd < 0 {
                    return fail();
                }
                last_opened = fd;
                if tag == b"file" {
                    mac.file_fd = fd;
                    mac.paths.file = Some(file_marker);
                } else {
                    mac.input_fd = fd;
                }
            }
            b"write-file" => {
                let Some(mode) = record.field(1).map(CStr::to_bytes) else {
                    return fail();
                };
                if mode != b"replace" && mode != b"create" {
                    return fail();
                }
                let Some([kind, value, within_kind, within_value]) = four(&record, 2) else {
                    return fail();
                };
                let mut name = [0; 256];
                let fd = witchy_launch_open_parent(
                    launch,
                    kind,
                    value,
                    within_kind,
                    within_value,
                    &mut name,
                );
                if fd < 0 {
                    return fail();
                }
                OUTPUT_NAME.clear();
                for byte in name {
                    OUTPUT_NAME.push(byte);
                }
                mac.output_fd = fd;
                mac.output_exclusive = mode == b"create";
                mac.paths.output = OUTPUT_NAME.as_ptr();
            }
            b"exec-env" => {
                if mac.exec_env_seen
                    || !record
                        .field(1)
                        .is_some_and(super::exec_environment::witchy_exec_env_names)
                {
                    return fail();
                }
                mac.exec_env_seen = true;
            }
            b"exec" => {
                let fd = match (record.field(1), record.field(2)) {
                    (Some(kind), Some(value)) => {
                        witchy_launch_open(launch, 0, kind, value, c"-", c"-")
                    }
                    _ => -1,
                };
                // SAFETY: the field is null or writable text in the plan's
                // copy, which lasts for the life of the process.
                if fd < 0 || !unsafe { super::exec_guest::bind_programs(record.raw(3)) } {
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
                let (Some(name), Some(value)) = (record.field(1), record.field(2)) else {
                    return fail();
                };
                let (name, value) = (name.to_bytes(), value.to_bytes());
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
                // The option is shorter than its zeroed buffer, so a NUL ends it.
                let Ok(option) = CStr::from_bytes_until_nul(&argument) else {
                    return fail();
                };
                // The buffer lasts only for this record, which is enough for a
                // limit: no limit keeps its text.
                if flag == 0 || mac.apply_limit(flag, option, required, err) != 1 {
                    return fail();
                }
            }
            b"identity" | b"object" => {
                let (Some(source), Some(name)) = (record.field(1), record.field(2)) else {
                    return fail();
                };
                let (source, name) = (source.to_bytes(), name.to_bytes());
                if source != b"arg"
                    || object_count == objects.len()
                    || objects[..object_count].iter().any(|&used| {
                        launch.records()[used]
                            .field(2)
                            .is_some_and(|used| used.to_bytes() == name)
                    })
                {
                    return fail();
                }
                objects[object_count] = index;
                object_count += 1;
                let Some(flag) = launch.flag(name) else {
                    return fail();
                };
                if !flag.kind_object {
                    return fail();
                }
                let Some(expected) = flag
                    .value()
                    .and_then(|value| super::file_identity::parse(value.to_bytes()))
                else {
                    return fail();
                };
                if tag == b"identity" {
                    if last_opened < 0
                        || !same_identity(last_opened, expected.device, expected.inode)
                    {
                        say(c"launch refused: the opened object is not the one named DEVICE:INODE\n", err);
                        return Err(2);
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
                        let (Some(binding), Some(mode)) = (record.field(1), record.field(2)) else {
                            return fail();
                        };
                        let mode = mode.to_bytes();
                        if (mode != b"reveal" && mode != b"seal")
                            || !witchy_bind_store_secret(binding, mode == b"reveal")
                        {
                            return fail();
                        }
                    } else {
                        let Some(kind) = record.field(3).map(CStr::to_bytes) else {
                            return fail();
                        };
                        let mut buffer = [0 as c_char; 512];
                        // SAFETY (both calls): the buffer is writable for the
                        // length given, and validation showed that a secret
                        // record has the fields each format takes as strings.
                        let size = if kind == b"env" {
                            unsafe {
                                snprintf(
                                    buffer.as_mut_ptr(),
                                    buffer.len(),
                                    c"%s:%s:env:%s".as_ptr(),
                                    record.raw(1),
                                    record.raw(2),
                                    record.raw(4),
                                )
                            }
                        } else {
                            let number = if kind == b"arg" {
                                record
                                    .field(4)
                                    .map_or(-1, |name| witchy_launch_descriptor(launch, name))
                            } else if kind == b"fd" {
                                record
                                    .field(4)
                                    .and_then(|text| super::budget::parse_limit(text.to_bytes()))
                                    .and_then(|n| c_int::try_from(n).ok())
                                    .unwrap_or(-1)
                            } else {
                                -1
                            };
                            let copy = witchy_launch_take(launch, number);
                            if copy < 0 {
                                return fail();
                            }
                            unsafe {
                                snprintf(
                                    buffer.as_mut_ptr(),
                                    buffer.len(),
                                    c"%s:%s:fd:%d".as_ptr(),
                                    record.raw(1),
                                    record.raw(2),
                                    copy,
                                )
                            }
                        };
                        if size <= 0
                            || size as usize >= buffer.len()
                            || !unsafe { witchy_bind_signer(buffer.as_ptr()) }
                        {
                            say(c"launch refused: the secret could not be bound from its declared source\n", err);
                            return Err(2);
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
    Ok((mac, grants))
}
