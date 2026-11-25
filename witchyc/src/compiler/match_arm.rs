//! Only the selected logical arm may establish its immutable payload binding.
use super::ir::{Stmt, Ty};

#[derive(Debug)]
pub(super) struct Arm {
    pub payload: Ty,
    pub binding: Option<u32>,
    pub body: Vec<Stmt>,
}
