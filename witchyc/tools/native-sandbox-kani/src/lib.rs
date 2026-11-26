//! Kani checks of the seccomp programs the Linux runtime builds. They sit in
//! `runtime/native/linux_sandbox.rs` under `cfg(kani)`, and have a crate of their own
//! because that file and macOS's `seatbelt.rs` export the same symbol.
#![allow(dead_code, unknown_lints, suspicious_runtime_symbol_definitions)]

#[path = "../../../runtime/native/global.rs"]
mod global;
#[path = "../../../runtime/native/linux_sandbox.rs"]
mod linux_sandbox;
#[path = "../../../runtime/native/sys.rs"]
mod sys;
