//! Kani checks of the runtime's pure logic: the text it parses and the frames it
//! reads. Proofs of a file's private functions sit in that file, under `cfg(kani)`.
#![allow(dead_code)]

#[path = "../../../runtime/native/broker_transport.rs"]
mod broker_transport;
#[path = "../../../runtime/native/env_names.rs"]
mod env_names;
#[path = "../../../runtime/native/errno.rs"]
mod errno;
#[path = "../../../runtime/native/exec_deadline.rs"]
mod exec_deadline;
#[path = "../../../runtime/native/file_identity.rs"]
mod file_identity;
#[path = "../../../runtime/native/launch_plan_validate.rs"]
mod launch_plan_validate;
#[path = "../../../runtime/native/sys.rs"]
mod sys;

#[cfg(kani)]
mod proofs {
    use super::{env_names, exec_deadline, file_identity};

    /// A command's deadline is never past the launch's, and is the default when none is asked.
    #[kani::proof]
    fn a_command_deadline_is_within_the_launch() {
        let requested: u64 = kani::any();
        let remaining: Option<u64> = kani::any();
        let chosen = exec_deadline::command_ms(requested, remaining);
        if let Some(remaining) = remaining {
            assert!(chosen <= remaining);
        }
        assert!(chosen <= if requested == 0 { exec_deadline::DEFAULT_MS } else { requested });
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
