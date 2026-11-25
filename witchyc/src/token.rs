use std::ops::Range;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    Name(String),
    Brand,
    Reserved(&'static str),
    Integer(u64),
    /// `250ms`, `1m30s`: a Duration of that many milliseconds (library/wasm-time.wy).
    Duration(u64),
    /// `1.5`, `2.0e-3`: the 64 bits of the nearest F64.
    Float(u64),
    Text(String),
    Symbol(&'static str),
    Indent,
    Dedent,
    Newline,
    End,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    pub kind: Kind,
    pub span: Range<usize>,
}
