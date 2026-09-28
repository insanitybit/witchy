//! Runtime-neutral compiler result data used by static-site tooling.

/// An owned, authority-free result admitted across a compiler-evaluation
/// boundary. Runtime handles, capabilities, closures, and opaque values are
/// rejected instead of being exposed to tooling.
#[derive(Clone, Debug, PartialEq)]
pub enum CompilerValue {
    Int(i64),
    Float(f64),
    String(String),
    Bytes(Vec<u8>),
    Bool(bool),
    List(Vec<CompilerValue>),
    Tuple(Vec<CompilerValue>),
    Constructor {
        name: String,
        fields: Vec<CompilerValue>,
    },
    Dict(Vec<(CompilerValue, CompilerValue)>),
    Unit,
}

