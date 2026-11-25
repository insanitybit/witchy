//! Resolved specification fragments. Formal identities never use call-site names.
use super::logical_body::Ty;
use std::ops::Range;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Binder { pub declaration: u32, pub parameter: u32 }
#[derive(Clone)]
pub(super) struct Expr { pub kind: Kind, pub ty: Ty, pub span: Range<usize> }
#[derive(Clone)]
pub(super) enum Kind {
    Bound(Binder), Integer(u64), Bool(bool),
    Unary(&'static str, Box<Expr>), Binary(&'static str, Box<Expr>, Box<Expr>),
    Field(Box<Expr>, String, Option<u32>),
    /// A typed, shared receiver projection. Its checked body is used by the proof model.
    Method(Box<Expr>, String, Option<u32>),
    Let(Binder, Box<Expr>, Box<Expr>),
    Builtin(String, Vec<Expr>), Apply(usize, Vec<Expr>),
}
