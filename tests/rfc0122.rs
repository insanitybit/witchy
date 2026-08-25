//! Driver binary consolidating the RFC-0122 reference tests into one test
//! binary, for the same reason as `rfc0130_generator.rs`: three top-level
//! files meant three linked binaries and three nextest discovery slots for
//! ten tests. The fixture data these modules read already lived in
//! `tests/rfc0122/`, so only the `.rs` files moved.
#[path = "rfc0122/core.rs"]
mod core;
#[path = "rfc0122/migration.rs"]
mod migration;
#[path = "rfc0122/timing.rs"]
mod timing;
