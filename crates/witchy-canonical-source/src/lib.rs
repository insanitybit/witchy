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
pub mod carrier;
pub mod callable_storage;
pub mod closure;
pub mod opt_value;
pub mod proper_tail;
pub mod rooted_record;
pub mod rooted_view;
pub mod scope;
pub mod service;
pub mod source;
pub mod storage;
pub mod structural_identity;
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

/// The reserved compiler message lane used by the cooperative scheduler.
/// Recognizing its source spelling is not an ownership or layout proof.
pub fn is_erased_message_type(ty: &Type) -> bool {
    matches!(ty, Type::Named(name, arguments)
        if name == "__Msg" && arguments.is_empty())
}
