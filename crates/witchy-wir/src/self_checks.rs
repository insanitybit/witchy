//! One path per in-crate self-check module, so `tests/unit.rs` can reach checks
//! that need private internals without the lib compiling a cfg(test) copy.

pub use crate::canonical_interpreter::self_checks as canonical_interpreter;
pub use crate::canonical_interpreter::capability_self_checks as canonical_interpreter_capability;
pub use crate::canonical_interpreter::build_read::self_checks as canonical_interpreter_build_read;
pub use crate::canonical_interpreter::crypto::rsa_self_checks as canonical_interpreter_crypto_rsa;
pub use crate::canonical_interpreter::registry_host::self_checks as canonical_interpreter_registry_host;
pub use crate::canonical_safewir::self_checks as canonical_safewir_catalog;
pub use crate::canonical_safewir::authority::self_checks as canonical_safewir_authority;
pub use crate::canonical_safewir::build_read::self_checks as canonical_safewir_build_read;
pub use crate::canonical_safewir::manifest::self_checks as canonical_safewir_manifest;
pub use crate::canonical_safewir::check::self_checks as canonical_safewir_check;
#[cfg(not(target_arch = "wasm32"))]
pub use crate::canonical_safewir::check::cache::store::self_checks as canonical_safewir_check_cache;
pub use crate::canonical_safewir::check::contracts::self_checks as canonical_safewir_check_contracts;
pub use crate::canonical_safewir::check::host::self_checks as canonical_safewir_check_host;
pub use crate::canonical_safewir::check::memory::self_checks as canonical_safewir_check_memory;
pub use crate::canonical_safewir::check::owners::self_checks as canonical_safewir_check_owners;
pub use crate::canonical_wasmir::self_checks as canonical_wasmir;
pub use crate::canonical_wasmir::closure_owner_kernel::self_checks as canonical_wasmir_closure_owner_kernel;
#[cfg(feature = "native")]
pub use crate::canonical_wasmir::drop_worklist::self_checks as canonical_wasmir_drop_worklist;
pub use crate::safewir_validate::self_checks as safewir_validate;
