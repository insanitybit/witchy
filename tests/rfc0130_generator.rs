//! Driver binary consolidating the RFC-0130 generator tests into one test
//! binary. Each module below was formerly its own top-level `tests/*.rs` file
//! and therefore its own linked binary; seven binaries carried ~7 tests worth
//! about 7s of CPU between them while each paid the full per-binary link and
//! nextest discovery cost (~2.4s of a measured 121s discovery phase across 50
//! binaries). Files live in `tests/rfc0130_generator/` (a subdir is not
//! auto-compiled as its own binary) and are attached with `#[path]` since a
//! test crate root resolves bare `mod` names against `tests/`, not the subdir.
#[path = "rfc0130_generator/liveness.rs"]
mod liveness;
#[path = "rfc0130_generator/match_binding.rs"]
mod match_binding;
#[path = "rfc0130_generator/match_cfg.rs"]
mod match_cfg;
#[path = "rfc0130_generator/nested_cfg.rs"]
mod nested_cfg;
#[path = "rfc0130_generator/owned_frame.rs"]
mod owned_frame;
#[path = "rfc0130_generator/scaling.rs"]
mod scaling;
#[path = "rfc0130_generator/while_let_cfg.rs"]
mod while_let_cfg;
