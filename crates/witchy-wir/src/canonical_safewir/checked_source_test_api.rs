//! Test-only surface for the SafeWIR cases that need a real checked source
//! (RFC-0170 P4). Compiled only under the dev-only `checked-source-test-api`
//! feature.
//!
//! witchy-wir does not depend on the checker, not even for tests: Cargo
//! applies a dev-dependency to every test target, so a typeck edit would
//! rebuild wir's test binaries. A case that judges checker products with
//! wir-private predicates therefore lives beside those predicates, and
//! `crates/witchy-lower/tests/safewir_checked_source.rs` runs it under the same
//! name with the real frontend as its [`SourceChecker`].
//!
//! Nothing here mints authority. A case takes a checker, asserts, and returns
//! `()`. The only views it sees are ones the checker minted, because
//! `CanonicalCheckedSource::view` stays the single construction site of
//! [`CanonicalSourceView`].

use witchy_canonical_source::closure::ClosureWitnessManifest;
use witchy_canonical_source::source::CanonicalSourceView;
use witchy_syntax::ast::Module;

pub use super::authority::checked_source_cases as authority;
pub use super::closure_owner_contract::checked_source_cases as closure_owner_contract;
pub use super::owner_authority::checked_source_cases as owner_authority;

/// One checked source, borrowed from the checker's retained product.
pub struct CheckedFixture<'s> {
    pub view: CanonicalSourceView<'s>,
    /// The checker's own typed AST (`CanonicalCheckedSource::typed_module`),
    /// so a case can pin that a contract borrows it rather than a copy.
    pub typed_module: &'s Module,
    /// The checker's own closure witness manifest, for the same reason.
    pub closure_witness: &'s ClosureWitnessManifest,
}

/// The checked frontend, as the cases call it.
pub trait SourceChecker {
    /// Check `source` as module `entry` with entry declaration `entry`, and run
    /// `body` over the result. Panics if the source does not check.
    fn with_checked(&self, entry: &str, source: &[u8], body: &mut dyn FnMut(CheckedFixture<'_>));
}
