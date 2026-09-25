//! Structural SafeWIR and canonical-layout integration contracts.
//!
//! These small suites share the same native `witchy-wir` feature graph. Keep
//! their assertions separate by module while paying the native link and test
//! discovery cost once.

#[path = "structural_contracts/inline_aggregate.rs"]
mod inline_aggregate;
#[path = "structural_contracts/nullable_owned_slot_transport.rs"]
mod nullable_owned_slot_transport;
#[path = "structural_contracts/owned_pointer_initialization.rs"]
mod owned_pointer_initialization;
#[path = "structural_contracts/owned_slot_child_storage.rs"]
mod owned_slot_child_storage;
#[path = "structural_contracts/recursive_layout_transport.rs"]
mod recursive_layout_transport;
#[path = "structural_contracts/safewir_structure.rs"]
mod safewir_structure;
