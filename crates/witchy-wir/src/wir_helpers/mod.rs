//! `wir_helpers` — the runtime-helper library.
//!
//! Every stdlib primitive the compiled backend leans on — list/dict/string ops,
//! crypto, encoding, capability host calls — expressed as a [`WirFunc`] rather
//! than a raw wasm body. Expressing each as WIR lets the encoder re-index it by
//! name, so a module emits only the helpers it actually reaches and imports only
//! their authority (capability-correct, and no `wat` in the build).
//!
//! [`wir_helper`] is the by-name dispatcher: given a helper name it returns the
//! [`WirHelperSpec`] (the function plus its helper/import dependencies), which is
//! how `codegen` resolves a module's reachable helper set.

mod bytes;
use bytes::*;
mod collections;
pub(crate) use collections::*;
mod diagnostics;
pub use diagnostics::*;
mod dict;
pub(crate) use dict::*;
mod encoding;
use encoding::*;
mod host;
pub use host::*;
mod memory;
pub use memory::*;
mod numeric;
pub(crate) use numeric::*;
mod strings;
pub(crate) use strings::*;
mod vm;
pub use vm::*;
mod registry;
pub use registry::{WirHelperSpec, wir_helper};
