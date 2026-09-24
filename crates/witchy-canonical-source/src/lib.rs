//! `witchy-canonical-source` — the checked-source contract between the checker
//! and canonical SafeWIR (RFC-0170).
//!
//! It holds the AST-level shapes and judgments that `witchy-types` produces
//! and `witchy-wir` consumes: scalar aggregate shapes, capability
//! representations, and callable, proper-tail and closure-capture identities.
//! Nothing here performs inference or names the checker's internal `Ty`; a
//! judgment that needs the typing tables belongs upstream in `witchy-types`.

#![allow(clippy::collapsible_if, clippy::collapsible_match, clippy::items_after_test_module)]
#![deny(unsafe_code)]

use witchy_syntax::ast::Type;

pub mod abi;
pub mod access;
pub mod aggregates;
pub mod capabilities;
pub mod closure;
pub mod proper_tail;
pub mod specialization;

/// The structural `()` spelling is canonical; legacy names remain accepted
/// while old source is migrated.
pub fn is_unit_type(ty: &Type) -> bool {
    match ty.unqualified() {
        Type::Tuple(items) => items.is_empty(),
        Type::Named(name, arguments) => {
            arguments.is_empty() && matches!(name.as_str(), "Nil" | "Unit")
        }
        _ => false,
    }
}
