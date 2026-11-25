#![no_std]
#![deny(unsafe_op_in_unsafe_fn)]

mod broker_directory;
mod broker_endpoint;
mod broker_loop;
mod broker_path;
mod broker_socket;
mod broker_transport;
mod budget;
mod clock_random;
mod connection_queue;
mod console;
mod deadline;
mod digest;
mod tls_grant;
#[cfg(witchy_native_aws_lc)]
#[path = "../../src/compiler/tls_signature.rs"]
mod tls_signature;
#[cfg(witchy_native_aws_lc)]
mod tls_signer;
#[cfg(witchy_native_aws_lc)]
#[path = "../../src/compiler/pkix.rs"]
mod pkix;
#[cfg(witchy_native_aws_lc)]
mod pkix_abi;
#[cfg(witchy_native_aws_lc)]
mod p384;
#[cfg(witchy_native_aws_lc)]
mod p384_slots;
#[cfg(all(target_os = "macos", witchy_native_aws_lc))]
mod tls_record;
mod directory;
mod entry_budget;
mod errno;
mod global;
mod sys;
mod env_names;
mod exec_arguments;
mod exec_broker;
mod exec_capture;
mod exec_deadline;
#[path = "../../src/compiler/exec_env_policy.rs"]
mod exec_env_policy;
mod exec_environment;
mod exec_grant;
mod exec_guest;
#[cfg(target_os = "linux")]
mod exec_linux_children;
#[cfg(target_os = "linux")]
mod exec_linux_limits;
#[cfg(target_os = "linux")]
mod exec_linux_runner;
#[cfg(target_os = "linux")]
mod exec_linux_security;
#[cfg(target_os = "linux")]
mod exec_linux_transport;
mod exec_pending;
mod exec_programs;
#[cfg(target_os = "macos")]
mod exec_supervision;
mod exec_temp;
mod file_identity;
mod file_reader;
mod file_snapshot;
mod file_writer;
mod grant_policy;
mod heap;
mod heap_layout;
mod heap_range;
mod heap_slots;
mod input_stream;
mod io_budget;
mod launch_args;
mod launch_env;
mod launch_flags;
mod launch_option;
mod launch_plan;
mod launch_plan_validate;
mod launch_walk;
mod limit_labels;
#[cfg(target_os = "linux")]
mod linux_sandbox;
#[cfg(target_os = "linux")]
mod linux_stat;
mod policy_grants;
mod policy_grants_abi;
#[cfg(target_os = "macos")]
mod seatbelt;
#[cfg(target_os = "macos")]
mod seatbelt_fds;
mod secret_guard;
#[cfg(witchy_native_aws_lc)]
mod signer_binding;
#[cfg(witchy_native_aws_lc)]
mod signer_crypto;
mod socket_primitives;
mod tasks;

#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    unsafe extern "C" {
        safe fn abort() -> !;
    }
    // A panic is a runtime defect and must never unwind into C.
    abort()
}

// The diagnostic static archive includes core's unwind metadata, but guest
// execution is compiled with panic=abort and never unwinds across this ABI.
#[cfg(any(witchy_native_ub_checks, target_os = "linux"))]
#[unsafe(no_mangle)]
unsafe extern "C" fn rust_eh_personality() -> ! {
    unsafe extern "C" {
        safe fn abort() -> !;
    }
    abort()
}

mod timers;

/// The launcher passes admitted grants, the broker channel (or -1), and a
/// live stdout stream. Enter the guest only after confinement succeeds.
#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_run_guest(
    grants: u64,
    _channel: core::ffi::c_int,
    _deadline_ms: u64,
    stdout: *mut core::ffi::c_void,
    _stderr: *mut core::ffi::c_void,
) -> core::ffi::c_int {
    unsafe extern "C" {
        fn witchy_install_sandbox_with_relay(
            output: bool,
            allocator: bool,
            input: bool,
            channel: core::ffi::c_int,
        ) -> bool;
        safe fn witchy_allow_allocator();
        fn witchy_entry(grants: u64);
        fn fputs(
            text: *const core::ffi::c_char,
            stream: *mut core::ffi::c_void,
        ) -> core::ffi::c_int;
        fn fflush(stream: *mut core::ffi::c_void) -> core::ffi::c_int;
    }
    if _deadline_ms != 0 && !deadline::witchy_arm_deadline(_deadline_ms) {
        unsafe { fputs(c"could not install native deadline\n".as_ptr(), _stderr) };
        return 2;
    }
    let output = grants & 1 != 0;
    let allocator = grants & 2 != 0;
    #[cfg(target_os = "linux")]
    if grants & (32768 | 65536 | tls_grant::TLS_SIGNER) != 0 {
        unsafe extern "C" {
            safe fn witchy_sandbox_allow_signing();
        }
        witchy_sandbox_allow_signing();
    }
    let confined =
        unsafe { witchy_install_sandbox_with_relay(output, allocator, grants & 8 != 0, _channel) };
    if !confined {
        return 2;
    }
    unsafe {
        if allocator {
            witchy_allow_allocator();
        }
        witchy_entry(grants);
        let status = if output && fflush(stdout) < 0 { 2 } else { 0 };
        heap::report_allocations();
        status
    }
}

/// Check the ordinary launch grants in diagnostic order. The caller supplies
/// its error stream. File and listener grants are checked during binding.
fn witchy_validate_grants(
    required: u64,
    supplied: u64,
    _max_words: u32,
    _max_bytes: u32,
    stderr: sys::Stream,
) -> bool {
    unsafe extern "C" {
        safe fn witchy_args_fit(words: u32, bytes: u32) -> bool;
    }

    let args_fit = required & 64 == 0
        || supplied & 64 == 0
        || witchy_args_fit(_max_words, _max_bytes);
    if let Some(message) = grant_policy::refusal(required, supplied, args_fit) {
        stderr.put(message);
        return false;
    }
    true
}

/// Start the admitted native launch mode using live launcher strings and streams.
#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_launch_native(
    grants: u64,
    per_connection: bool,
    listener: *const core::ffi::c_char,
    connections: u64,
    connection_ms: u64,
    deadline_ms: u64,
    waiting: u64,
    read_dir: *const core::ffi::c_char,
    write_dir: *const core::ffi::c_char,
    root_fd: core::ffi::c_int,
    write_root_fd: core::ffi::c_int,
    exec_fd: core::ffi::c_int,
    programs: *const *const core::ffi::c_char,
    program_count: u32,
    output: *const core::ffi::c_char,
    identity: *const file_identity::Identity,
    entries: u64,
    connection_entries: u64,
    data_only: bool,
    exec_budget: u64,
    input_budget: u64,
    input_budget_set: bool,
    stdout: *mut core::ffi::c_void,
    stderr: *mut core::ffi::c_void,
) -> core::ffi::c_int {
    if per_connection {
        return unsafe {
            connection_queue::launch_connections(
                listener,
                grants,
                connections,
                connection_ms,
                deadline_ms,
                waiting,
                read_dir,
                write_dir,
                root_fd,
                write_root_fd,
                identity,
                entries,
                connection_entries,
                data_only,
                exec_fd,
                programs,
                program_count,
                exec_budget,
                input_budget,
                input_budget_set,
                stdout,
                stderr,
            )
        };
    }
    #[cfg(target_os = "macos")]
    const BROKER_GRANTS: u64 = 4 | 512 | 131072 | 1024 | 2048 | 4096 | 8192 | 262144;
    #[cfg(target_os = "linux")]
    const BROKER_GRANTS: u64 = 4 | 8 | 16 | 32 | 512 | 131072 | 1024 | 2048 | 4096 | 8192 | 262144;
    let channel = if grants & BROKER_GRANTS != 0 {
        let channel = unsafe {
            broker_socket::launch_broker(
                listener,
                read_dir,
                write_dir,
                root_fd,
                write_root_fd,
                exec_fd,
                programs,
                program_count,
                output,
                identity,
                entries,
                data_only,
                stderr,
            )
        };
        if channel < 0 {
            return 2;
        }
        channel
    } else {
        -1
    };
    unsafe { witchy_run_guest(grants, channel, deadline_ms, stdout, stderr) }
}
