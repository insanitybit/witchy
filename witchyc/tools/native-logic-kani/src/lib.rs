//! Kani checks of the runtime's pure logic: the text it parses and the frames it
//! reads. Proofs of a file's private functions sit in that file, under `cfg(kani)`.
//! The files below are those with proofs and the files they name with `super::`.
//! `linux_sandbox.rs` exports a symbol `seatbelt.rs` also does, so its proofs
//! are run from `tools/native-sandbox-kani`.
#![allow(dead_code, unknown_lints, suspicious_runtime_symbol_definitions)]

#[path = "../../../runtime/native/broker_directory.rs"]
mod broker_directory;
#[path = "../../../runtime/native/broker_endpoint.rs"]
mod broker_endpoint;
#[path = "../../../runtime/native/broker_loop.rs"]
mod broker_loop;
#[path = "../../../runtime/native/broker_path.rs"]
mod broker_path;
#[path = "../../../runtime/native/broker_socket.rs"]
mod broker_socket;
#[path = "../../../runtime/native/broker_transport.rs"]
mod broker_transport;
#[path = "../../../runtime/native/budget.rs"]
mod budget;
#[path = "../../../runtime/native/connection_queue.rs"]
mod connection_queue;
#[path = "../../../runtime/native/deadline.rs"]
mod deadline;
#[path = "../../../runtime/native/directory.rs"]
mod directory;
#[path = "../../../runtime/native/entry_budget.rs"]
mod entry_budget;
#[path = "../../../runtime/native/env_names.rs"]
mod env_names;
#[path = "../../../runtime/native/errno.rs"]
mod errno;
#[path = "../../../runtime/native/exec_arguments.rs"]
mod exec_arguments;
#[path = "../../../runtime/native/exec_broker.rs"]
mod exec_broker;
#[path = "../../../runtime/native/exec_capture.rs"]
mod exec_capture;
#[path = "../../../runtime/native/exec_deadline.rs"]
mod exec_deadline;
#[path = "../../../src/compiler/exec_env_policy.rs"]
mod exec_env_policy;
#[path = "../../../runtime/native/exec_environment.rs"]
mod exec_environment;
#[path = "../../../runtime/native/exec_pending.rs"]
mod exec_pending;
#[path = "../../../runtime/native/exec_programs.rs"]
mod exec_programs;
#[path = "../../../runtime/native/exec_supervision.rs"]
mod exec_supervision;
#[path = "../../../runtime/native/exec_temp.rs"]
mod exec_temp;
#[path = "../../../runtime/native/file_identity.rs"]
mod file_identity;
#[path = "../../../runtime/native/file_reader.rs"]
mod file_reader;
#[path = "../../../runtime/native/global.rs"]
mod global;
#[path = "../../../runtime/native/input_stream.rs"]
mod input_stream;
#[path = "../../../runtime/native/io_budget.rs"]
mod io_budget;
#[path = "../../../runtime/native/launch_args.rs"]
mod launch_args;
#[path = "../../../runtime/native/launch_plan.rs"]
mod launch_plan;
#[path = "../../../runtime/native/launch_plan_validate.rs"]
mod launch_plan_validate;
#[path = "../../../runtime/native/launch_walk.rs"]
mod launch_walk;
#[path = "../../../runtime/native/limit_labels.rs"]
mod limit_labels;
#[path = "../../../runtime/native/policy_grants.rs"]
mod policy_grants;
#[path = "../../../runtime/native/seatbelt.rs"]
mod seatbelt;
#[path = "../../../runtime/native/seatbelt_fds.rs"]
mod seatbelt_fds;
#[path = "../../../runtime/native/secret_guard.rs"]
mod secret_guard;
#[path = "../../../runtime/native/socket_primitives.rs"]
mod socket_primitives;
#[path = "../../../runtime/native/sys.rs"]
mod sys;
#[path = "../../../runtime/native/timers.rs"]
mod timers;
#[path = "../../../runtime/native/tls_grant.rs"]
mod tls_grant;

#[cfg(kani)]
mod proofs {
    use super::{env_names, exec_arguments, exec_deadline, file_identity};

    /// A command's deadline is never past the launch's, and is the default when none is asked.
    #[kani::proof]
    fn a_command_deadline_is_within_the_launch() {
        let requested: u64 = kani::any();
        let remaining: Option<u64> = kani::any();
        let chosen = exec_deadline::command_ms(requested, remaining);
        if let Some(remaining) = remaining {
            assert!(chosen <= remaining);
        }
        assert!(
            chosen
                <= if requested == 0 {
                    exec_deadline::DEFAULT_MS
                } else {
                    requested
                }
        );
    }

    /// A name that is accepted is not empty, starts with no digit and has only
    /// letters, digits and underscores: so no `=` and no NUL.
    #[kani::proof]
    #[kani::unwind(8)]
    fn an_accepted_environment_name_is_plain() {
        let bytes: [u8; 6] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= bytes.len());
        let name = &bytes[..length];
        if env_names::valid(name) {
            assert!(length != 0 && !name[0].is_ascii_digit());
            let at: usize = kani::any();
            kani::assume(at < length);
            assert!(name[at] == b'_' || name[at].is_ascii_alphanumeric());
        }
    }

    /// Accepted arguments are exactly `count` texts that each end in a zero and
    /// together are the whole buffer: every offset starts one, inside it.
    #[kani::proof]
    #[kani::unwind(10)]
    fn argument_offsets_start_each_argument() {
        let bytes: [u8; 7] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= bytes.len());
        let pending = &bytes[..length];
        let count: usize = kani::any();
        let mut out = [0; 64];
        if exec_arguments::offsets(pending, count, &mut out) {
            kani::cover!(count == 7, "seven empty arguments");
            kani::cover!(count == 2 && out[1] == 4);
            assert!(count <= length && (count == 0 || pending[length - 1] == 0));
            let mut zeros = 0;
            let mut at = 0;
            while at < length {
                zeros += usize::from(pending[at] == 0);
                at += 1;
            }
            assert_eq!(zeros, count);
            let index: usize = kani::any();
            kani::assume(index < count);
            let start = out[index];
            assert!(start < length);
            if index == 0 {
                assert_eq!(start, 0);
            } else {
                assert!(out[index - 1] < start && pending[start - 1] == 0);
            }
        }
    }

    /// An identity is read only from digits, one colon, digits; no text makes the
    /// reader overflow or stop the process.
    #[kani::proof]
    #[kani::unwind(10)]
    fn an_identity_is_two_decimal_numbers() {
        let bytes: [u8; 8] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= bytes.len());
        let text = &bytes[..length];
        if file_identity::parse(text).is_some() {
            assert!(length >= 3);
            let at: usize = kani::any();
            kani::assume(at < length);
            assert!(text[at] == b':' || text[at].is_ascii_digit());
            assert!(text[0] != b':' && text[length - 1] != b':');
        }
    }
}
