//! The minimal value type the native stdlib registry (`native.rs`: crypto /
//! encoding / regex / …) passes and returns. It deliberately does NOT depend on
//! the interpreter's full `Value` or its evaluator, so the native registry, the
//! WASM runtime host (`runtime.rs`), and the browser shim (`lib.rs`) can all call
//! pure native functions without pulling the interpreter in. The interpreter
//! bridges its own `Value` to/from this at its single native-dispatch site.

/// The argument/return type of a native-module function. Only the shapes a pure
/// stdlib function actually exchanges — strings, ints, bools, lists, and the
/// `Secret` seed (host-only; the ability to sign is authority).
#[derive(Debug, Clone, PartialEq)]
pub enum NativeValue {
    Int(i64),
    Str(String),
    Bool(bool),
    List(Vec<NativeValue>),
    /// A secret's raw bytes — a signing seed (read as a hex Ed25519 seed by
    /// `crypto.sign`/`public_key`) or a value secret (returned by `reveal`).
    Secret(Vec<u8>),
}

/// A native function's error — just a message, surfaced by the interpreter as a
/// `RuntimeError` and by the runtime sandbox as a host trap.
#[derive(Debug, Clone, PartialEq)]
pub struct NativeError {
    pub message: String,
}

impl std::fmt::Display for NativeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for NativeError {}
