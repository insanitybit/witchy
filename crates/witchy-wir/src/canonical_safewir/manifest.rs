//! RFC-0165 B1, B2, B4, B7: the operation-to-rule manifest.
//!
//! Every canonical rule (`CanonicalRuleId`, one per `Operation` and
//! `Terminator` variant) has a row naming the checker judgment that admits
//! or refuses it, what that judgment decides, the interpreter's semantics,
//! the backend's correspondence rule (or its refusal), and the focused tests
//! that exercise the admitting and refusing sides. Two further tables record
//! the ownership transfers of reference-counted strings and lists (B2) and
//! the compact supported-call forms (B4).
//!
//! The tables are data, checked mechanically:
//!
//! - every rule appears exactly once (a new `Operation` variant fails to
//!   compile until it has a row and a backend decision);
//! - the backend column equals `canonical_wasmir::rule_backend`, the one
//!   place the SafeWIR-to-WasmIR mapping is decided;
//! - every cited test exists in the file it is attributed to;
//! - `rfcs/0165-rule-manifest.md` is the rendering of these tables, so the
//!   reviewed document cannot drift from the code.

use super::CanonicalRuleId;
use crate::canonical_wasmir::{rule_backend, CorrespondenceRule, RuleBackend};

/// Where a cited test lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EvidenceFile {
    /// `crates/witchy-wir/src/canonical_safewir/check.rs`
    Check,
    /// `crates/witchy-wir/src/canonical_safewir/check/memory.rs`
    Memory,
    /// `crates/witchy-wir/src/canonical_interpreter.rs`
    Interpreter,
    /// `crates/witchy-wir/src/canonical_wasmir.rs`
    Backend,
    /// Nullable fixed-field admission probes, using internal source witnesses.
    NullableBackend,
    UnitBackend,
    UniquePopBackend,
    /// `tests/misc/rfc0165_admitted_launch.rs` (end to end through the runtime)
    Launch,
    /// `crates/witchy-wir/src/canonical_safewir/authority.rs`
    Authority,
    /// `crates/witchy-wir/src/canonical_safewir/check/owners.rs`
    OwnerCheck,
    /// `crates/witchy-wir/src/canonical_wasmir/closure_owner_kernel.rs`
    OwnerBackend,
    /// Opaque owner/list source, oracle, and admitted Wasm probes.
    OwnerLaunch,
    /// `crates/witchy-runtime/src/runtime/host/closure_owner.rs`
    OwnerRuntime,
}

impl EvidenceFile {
    pub const fn path(self) -> &'static str {
        match self {
            Self::Check => "crates/witchy-wir/src/canonical_safewir/check.rs",
            Self::Memory => "crates/witchy-wir/src/canonical_safewir/check/memory.rs",
            Self::Interpreter => "crates/witchy-wir/src/canonical_interpreter.rs",
            Self::Backend => "crates/witchy-wir/src/canonical_wasmir.rs",
            Self::UnitBackend => "crates/witchy-wir/src/canonical_wasmir/tests/unit_storage.rs",
            Self::UniquePopBackend => "crates/witchy-wir/src/canonical_wasmir/tests/unique_pop.rs",
            Self::NullableBackend => "crates/witchy-wir/src/canonical_wasmir/tests/nullable_admission.rs",
            Self::Launch => "tests/misc/rfc0165_admitted_launch.rs",
            Self::Authority => "crates/witchy-wir/src/canonical_safewir/authority.rs",
            Self::OwnerCheck => "crates/witchy-wir/src/canonical_safewir/check/owners.rs",
            Self::OwnerBackend => "crates/witchy-wir/src/canonical_wasmir/closure_owner_kernel.rs",
            Self::OwnerLaunch => "tests/misc/rfc0165_oracle.rs",
            Self::OwnerRuntime => "crates/witchy-runtime/src/runtime/host/closure_owner.rs",
        }
    }

    fn source(self) -> &'static str {
        match self {
            Self::Check => include_str!("check.rs"),
            Self::Memory => include_str!("check/memory.rs"),
            Self::Interpreter => include_str!("../canonical_interpreter.rs"),
            Self::Backend => include_str!("../canonical_wasmir.rs"),
            Self::UnitBackend => include_str!("../canonical_wasmir/tests/unit_storage.rs"),
            Self::UniquePopBackend => include_str!("../canonical_wasmir/tests/unique_pop.rs"),
            Self::NullableBackend => include_str!("../canonical_wasmir/tests/nullable_admission.rs"),
            Self::Launch => include_str!("../../../../tests/misc/rfc0165_admitted_launch.rs"),
            Self::Authority => include_str!("authority.rs"),
            Self::OwnerCheck => include_str!("check/owners.rs"),
            Self::OwnerBackend => include_str!("../canonical_wasmir/closure_owner_kernel.rs"),
            Self::OwnerLaunch => include_str!("../../../../tests/misc/rfc0165_oracle.rs"),
            Self::OwnerRuntime => include_str!("../../../witchy-runtime/src/runtime/host/closure_owner.rs"),
        }
    }
}

/// A focused test by file and function name.
#[derive(Clone, Copy, Debug)]
pub struct TestRef {
    pub file: EvidenceFile,
    pub name: &'static str,
}

const fn t(file: EvidenceFile, name: &'static str) -> TestRef {
    TestRef { file, name }
}

use EvidenceFile::{Authority, Backend, Check, Interpreter, Launch, Memory, OwnerBackend, OwnerCheck, OwnerLaunch, OwnerRuntime};

/// Reviewed nested operation census. The declaration generates the comparison
/// list, not these entries, so a new variant requires a manifest decision.
pub const OWNER_OPERATION_VARIANTS: &[&str] = &[
    "Construct", "ReadCapability", "ReadOwner", "ReadTag", "ReadCallable", "Clone", "Drop",
    "CloneEnvironment", "NewList", "ListGet", "ListLength", "CloneList",
    "ListPush", "ListSet", "DropList",
];

#[cfg(test)]
#[test]
fn owner_operation_manifest_lists_every_variant() {
    assert_eq!(
        OWNER_OPERATION_VARIANTS,
        super::closure_owner_contract::OwnerOperation::VARIANTS,
        "every owner operation must have an explicit manifest entry",
    );
}

/// One canonical rule's row.
#[derive(Clone, Copy, Debug)]
pub struct RuleEntry {
    pub rule: CanonicalRuleId,
    /// The checker judgment: the function that decides the rule.
    pub judgment: &'static str,
    /// What the judgment admits and what it refuses.
    pub predicate: &'static str,
    /// The interpreter's semantics for an admitted instance.
    pub interpreter: &'static str,
    /// The backend disposition: an instruction receipt, a subjudgment of
    /// existing parent receipts, or refusal before lowering.
    pub backend: RuleBackend,
    /// Tests that exercise admitted instances.
    pub admits: &'static [TestRef],
    /// Tests that exercise refused or trapping instances.
    pub refuses: &'static [TestRef],
}

/// The rule manifest, in `CanonicalRuleId` order.
pub const RULES: &[RuleEntry] = &[
    RuleEntry {
        rule: CanonicalRuleId::ConstUnit,
        judgment: "check.rs::check_operation",
        predicate: "defines only Unit, the sole Nil value; no integer or Bool authority",
        interpreter: "execute_operation: binds InterpreterValue::Unit",
        backend: RuleBackend::Receipt(CorrespondenceRule::ScalarConstant),
        admits: &[t(EvidenceFile::UnitBackend, "unit_storage_construct_call_native_oracle")],
        refuses: &[t(EvidenceFile::UnitBackend, "unit_storage_rejects_type_layout_initialization_and_bounds")],
    },

    RuleEntry {
        rule: CanonicalRuleId::ConstInt,
        judgment: "check.rs::check_operation",
        predicate: "defines the destination as `Int`; a constant has no failing predicate",
        interpreter: "execute_operation: binds the literal",
        backend: RuleBackend::Receipt(CorrespondenceRule::ScalarConstant),
        admits: &[t(Backend, "encoded_scalar_module_executes"), t(Interpreter, "validated_scalar_arithmetic_executes")],
        refuses: &[t(Check, "block_arguments_are_typed_ssa_definitions")],
    },
    RuleEntry {
        rule: CanonicalRuleId::ConstFloat,
        judgment: "check.rs::check_operation",
        predicate: "defines the destination as `Float` from raw bits",
        interpreter: "execute_operation: binds the bits",
        backend: RuleBackend::Receipt(CorrespondenceRule::ScalarConstant),
        admits: &[t(Launch, "admitted_float_conversions_bool_render_and_duration_identities_run")],
        refuses: &[t(Check, "block_arguments_are_typed_ssa_definitions")],
    },
    RuleEntry {
        rule: CanonicalRuleId::ConstBool,
        judgment: "check.rs::check_operation",
        predicate: "defines the destination as `Bool`",
        interpreter: "execute_operation: binds the literal",
        backend: RuleBackend::Receipt(CorrespondenceRule::ScalarConstant),
        admits: &[t(Backend, "encoded_structured_branch_executes_both_arms")],
        refuses: &[t(Check, "block_arguments_are_typed_ssa_definitions")],
    },
    RuleEntry {
        rule: CanonicalRuleId::ConstSize,
        judgment: "check.rs::check_operation",
        predicate: "defines the destination as `Size` and records the constant for later bounds facts",
        interpreter: "execute_operation: binds the literal as an i32-width size",
        backend: RuleBackend::Receipt(CorrespondenceRule::ScalarConstant),
        admits: &[t(Backend, "full_u32_size_constant_encodes_and_validates"), t(Backend, "full_u32_size_constant_matches_exact_admission_plan")],
        refuses: &[t(Interpreter, "validated_size_values_reject_values_wider_than_i32")],
    },
    RuleEntry {
        rule: CanonicalRuleId::CapabilityNone,
        judgment: "check.rs::check_operation",
        predicate: "defines only `NullableCapability(kind)` and has no host or memory representation",
        interpreter: "execute_operation: binds absence without authority",
        backend: RuleBackend::Receipt(CorrespondenceRule::ScalarGuard),
        admits: &[t(Check, "nullable_capability_operations_are_checked")],
        refuses: &[t(Check, "nullable_capability_operations_are_checked")],
    },
    RuleEntry {
        rule: CanonicalRuleId::CapabilitySome,
        judgment: "check.rs::check_operation",
        predicate: "wraps only an authenticated `Capability(kind)` into the same nullable kind",
        interpreter: "execute_operation: preserves the opaque token behind a present optional value",
        backend: RuleBackend::Receipt(CorrespondenceRule::ScalarGuard),
        admits: &[t(Check, "nullable_capability_operations_are_checked")],
        refuses: &[t(Check, "nullable_capability_operations_are_checked")],
    },
    RuleEntry {
        rule: CanonicalRuleId::CapabilityIsPresent,
        judgment: "check.rs::check_operation",
        predicate: "reads only a nullable capability and defines `Bool`",
        interpreter: "execute_operation: tests absence without exposing a token",
        backend: RuleBackend::Receipt(CorrespondenceRule::ScalarGuard),
        admits: &[t(Check, "nullable_capability_operations_are_checked")],
        refuses: &[t(Check, "nullable_capability_operations_are_checked")],
    },
    RuleEntry {
        rule: CanonicalRuleId::CapabilityUnwrap,
        judgment: "check.rs::check_operation",
        predicate: "converts only `NullableCapability(kind)` to the same non-null `Capability(kind)`; absence traps",
        interpreter: "execute_operation: traps before producing authority on absence",
        backend: RuleBackend::Receipt(CorrespondenceRule::ScalarGuard),
        admits: &[t(Check, "nullable_capability_operations_are_checked")],
        refuses: &[t(Check, "nullable_capability_operations_are_checked")],
    },
    RuleEntry {
        rule: CanonicalRuleId::Arithmetic,
        judgment: "check.rs::check_operation; check/memory.rs::MemoryChecker (exact-addition facts)",
        predicate: "operands and destination share one scalar type; `Int` and `Size` addition traps or wraps exactly as declared; a trapping `Size` add records an exact relation for bounds",
        interpreter: "int_arithmetic/size_arithmetic/float_arithmetic: wraps or traps exactly as declared; `/0` and signed `MIN % -1` trap or define",
        backend: RuleBackend::Receipt(CorrespondenceRule::ScalarArithmetic),
        admits: &[
            t(Interpreter, "validated_scalar_arithmetic_executes"),
            t(Interpreter, "validated_size_arithmetic_uses_i32_width_and_signed_bits"),
            t(Backend, "int_subtraction_matches_the_exact_encoded_plan"),
            t(Backend, "trapping_int_add_accepts_signed_boundaries"),
            t(Backend, "trapping_size_add_accepts_the_unsigned_boundary"),
            t(Memory, "guarded_wrapping_size_add_records_an_exact_relation"),
        ],
        refuses: &[
            t(Interpreter, "validated_size_arithmetic_traps_at_i32_boundary"),
            t(Interpreter, "signed_remainder_by_zero_traps"),
            t(Backend, "trapping_int_add_overflow_matches_interpreter"),
            t(Backend, "size_add_overflow_traps_or_wraps_exactly_as_declared"),
            t(Memory, "unchecked_wrapping_size_add_does_not_record_an_exact_relation"),
        ],
    },
    RuleEntry {
        rule: CanonicalRuleId::Compare,
        judgment: "check.rs::check_operation; check/memory.rs::MemoryChecker (guard facts)",
        predicate: "operands share one type, the destination is `Bool`; an unsigned `Size` comparison is the only shape a `Guard` may cite",
        interpreter: "execute_operation: signed, unsigned, and float comparisons by operator",
        backend: RuleBackend::Receipt(CorrespondenceRule::ScalarComparison),
        admits: &[t(Backend, "comparison_encodes_and_validates"), t(Backend, "encoded_comparison_and_full_u32_size_execute")],
        refuses: &[t(Check, "bounds_guard_without_a_comparison_is_refused")],
    },
    RuleEntry {
        rule: CanonicalRuleId::NarrowSize,
        judgment: "check.rs::check_operation",
        predicate: "`Int` to `Size`; the value must be in `0..=u32::MAX` at runtime or the backend traps",
        interpreter: "execute_operation: traps outside the i32 width",
        backend: RuleBackend::Receipt(CorrespondenceRule::ScalarConversion),
        admits: &[t(Backend, "narrow_size_valid_value_executes")],
        refuses: &[t(Backend, "narrow_size_negative_traps_in_wasmtime"), t(Backend, "narrow_size_u32_overflow_traps_in_wasmtime")],
    },
    RuleEntry {
        rule: CanonicalRuleId::WidenSize,
        judgment: "check.rs::check_operation",
        predicate: "`Size` to `Int`, always representable",
        interpreter: "execute_operation: zero-extends",
        backend: RuleBackend::Receipt(CorrespondenceRule::ScalarConversion),
        admits: &[t(Backend, "admitted_int_list_loop_executes_with_bounds_checks"), t(Launch, "admitted_nested_list_index_runs")],
        refuses: &[t(Interpreter, "validated_size_values_reject_values_wider_than_i32")],
    },
    RuleEntry {
        rule: CanonicalRuleId::FloatUnary,
        judgment: "check.rs::check_operation",
        predicate: "`Sqrt`: Float to Float; `TruncateToInt`: Float to Int (saturating, NaN to 0); `FromInt`: Int to Float",
        interpreter: "execute_operation: IEEE sqrt, saturating truncation, exact conversion",
        backend: RuleBackend::Receipt(CorrespondenceRule::ScalarConversion),
        admits: &[t(Launch, "admitted_float_conversions_bool_render_and_duration_identities_run")],
        refuses: &[t(Launch, "admitted_float_conversions_bool_render_and_duration_identities_run")],
    },
    RuleEntry {
        rule: CanonicalRuleId::Guard,
        judgment: "check/memory.rs::MemoryChecker (guard kinds); check.rs::check_operation",
        predicate: "the condition is a `Bool` from an unsigned `Size` comparison; a `Bounds` guard proves an index below a logical length, never a capacity; a false guard traps",
        interpreter: "execute_operation: a false condition traps",
        backend: RuleBackend::Receipt(CorrespondenceRule::ScalarGuard),
        admits: &[t(Check, "guarded_dynamic_element_load_is_admitted"), t(Check, "update_count_grows_logical_length_only_after_capacity_guard")],
        refuses: &[
            t(Check, "dynamic_element_load_without_guard_is_refused"),
            t(Check, "bounds_guard_without_a_comparison_is_refused"),
            t(Check, "bounds_guard_requires_unsigned_size_operands"),
            t(Check, "logical_length_guard_is_not_satisfied_by_capacity_guard"),
            t(Interpreter, "validated_false_guard_traps"),
            t(Backend, "packed_scalar_collection_oob_guard_traps_in_wasmtime"),
            t(Launch, "admitted_guard_near_misses_trap_exactly_at_the_boundary"),
        ],
    },
    RuleEntry {
        rule: CanonicalRuleId::Alloc,
        judgment: "check/memory.rs::MemoryChecker::allocate (check_extent)",
        predicate: "a reference-counted object of the declared layout whose count is at most its capacity and whose capacity fits the address space; a pointer-element list starts with an empty prefix; scratch classes refuse",
        interpreter: "allocate: a fresh object with uninitialized cells and count one",
        backend: RuleBackend::Receipt(CorrespondenceRule::MemoryAlloc),
        admits: &[
            t(Backend, "consecutive_rc_allocations_reserve_non_overlapping_prefixes"),
            t(Backend, "valid_scalar_and_packed_list_artifacts_admit"),
        ],
        refuses: &[
            t(Check, "dynamic_allocation_extent_must_fit_wasm_address_space"),
            t(Backend, "allocation_past_linear_memory_grows_or_traps_when_growth_is_refused"),
            t(Backend, "unsupported_allocator_class_is_refused_by_canonical_backend"),
        ],
    },
    RuleEntry {
        rule: CanonicalRuleId::StaticData,
        judgment: "check/memory.rs::MemoryChecker::static_data; check.rs::static_data_extent_matches",
        predicate: "a shared immutable object over a static segment whose declared extent matches the segment; it is never written, released, or reclaimed",
        interpreter: "execute_operation: a static object outside reference counting",
        backend: RuleBackend::Receipt(CorrespondenceRule::MemoryStaticData),
        admits: &[t(Launch, "admitted_string_literals_cross_owned_parameters_and_returns"), t(Interpreter, "static_shared_clone_and_drop_do_not_touch_rc_state")],
        refuses: &[t(Memory, "shared_static_storage_clones_and_drops_without_mutation_authority"), t(Memory, "shared_boundary_union_allows_only_immutable_token_operations")],
    },
    RuleEntry {
        rule: CanonicalRuleId::Load,
        judgment: "check/memory.rs::MemoryChecker::load (check_place, promote_header_extent)",
        predicate: "a header field loads a typed scalar; an element load needs a `Bounds` guard against the logical length; a reference load takes a shared clone of a stored slot or an initialized prefix element; TakeLast requires an exclusive complete packed list and exact final index, transfers one owning edge and publishes the shorter prefix atomically",
        interpreter: "load: reads an initialized cell; cloning retains; TakeLast detaches reference cells, preserves scalar bytes (fresh storage is zero), and shortens count without retaining",
        backend: RuleBackend::Receipt(CorrespondenceRule::MemoryLoad),
        admits: &[
            t(Check, "guarded_dynamic_element_load_is_admitted"),
            t(Check, "counted_header_projections_load_typed_scalars"),
            t(Backend, "admitted_int_list_load_store_executes"),
            t(Memory, "pointer_slot_move_and_clone_preserve_structural_ownership"),
            t(EvidenceFile::UniquePopBackend, "unique_pop_native_preserves_receiver_address_heap_top_and_shared_child"),
            t(EvidenceFile::UniquePopBackend, "unique_pop_oracle_complete_call_chain_reclaims_every_reference"),
            t(EvidenceFile::UniquePopBackend, "unique_pop_append_after_pop_preserves_prefix_direct_and_moved"),
            t(EvidenceFile::UniquePopBackend, "unique_pop_append_then_complete_transfer_preserves_shared_child"),
            t(EvidenceFile::UniquePopBackend, "unique_pop_scalar_zero_storage_and_republished_bytes_agree"),
        ],
        refuses: &[
            t(Check, "dynamic_element_load_without_guard_is_refused"),
            t(EvidenceFile::UniquePopBackend, "unique_pop_checker_refuses_shared_container_missing_bounds_and_non_tail"),
            t(EvidenceFile::UniquePopBackend, "unique_pop_rejects_live_prefix_overwrite_direct_moved_and_unknown"),
            t(Memory, "unique_pop_synthetic_zero_prefix_has_only_zero_start"),
            t(EvidenceFile::UniquePopBackend, "unique_pop_byte_plan_rejects_capacity_write_instead_of_tail_publication"),
            t(Authority, "unique_pop_source_abi_requires_checked_unique_input_and_output"),
            t(Check, "header_projection_cannot_claim_non_scalar_type"),
            t(Memory, "pointer_bearing_ranges_fail_closed_without_element_transfer_state"),
            t(Check, "uninitialized_scalar_load_is_refused"),
            t(Interpreter, "reclaimed_scalar_storage_does_not_initialize_a_reused_object"),
        ],
    },
    RuleEntry {
        rule: CanonicalRuleId::InitNullableOwned,
        judgment: "check/memory.rs::MemoryChecker::init_nullable_owned",
        predicate: "unique RC root and typed nullable fixed slot, never a live-edge overwrite; naked slots, nullable ranges, and opaque nominals remain refused",
        interpreter: "explicit absent storage state, distinct from uninitialized storage and SSA references",
        backend: RuleBackend::Receipt(CorrespondenceRule::MemoryStore),
        admits: &[t(Memory, "nullable_owned_checker_requires_initialization_and_checked_presence"), t(Interpreter, "nullable_owned_oracle_distinguishes_absent_uninitialized_and_present"), t(EvidenceFile::NullableBackend, "nullable_admission_present_and_absent_agree_with_oracle")],
        refuses: &[t(Memory, "nullable_owned_checker_refuses_nonnullable_and_shared_initialization"), t(Check, "nullable_owned_slot_is_refused_at_the_initial_catalog_gate"), t(Check, "nullable_owned_collection_cannot_borrow_a_fixed_field_catalog_exception"), t(EvidenceFile::NullableBackend, "nullable_admission_rejects_valid_but_mutated_bytes")],
    },
    RuleEntry {
        rule: CanonicalRuleId::Store,
        judgment: "check/memory.rs::MemoryChecker::store",
        predicate: "the object is uniquely owned and writable; a scalar store copies (a byte slot takes a `Size` the path proves fits a byte); a reference store moves a unique token or a shared clone into an empty slot and never overwrites a stored reference or an initialized prefix; the stored value is complete",
        interpreter: "store: writes the cell; MoveUnique transfers its token and CloneShared retains the new field edge",
        backend: RuleBackend::Receipt(CorrespondenceRule::MemoryStore),
        admits: &[
            t(Interpreter, "validated_counted_element_store_and_load_execute"),
            t(EvidenceFile::NullableBackend, "shared_store_preserves_external_owner_after_parent_drop"),
            t(Backend, "packed_scalar_collection_access_mutates_and_preserves_headers"),
            t(Memory, "pointer_slot_move_and_clone_preserve_structural_ownership"),
            t(Launch, "admitted_ascii_case_mapping_writes_bytes_under_a_byte_bound"),
        ],
        refuses: &[
            t(Memory, "pointer_slot_rejects_scalar_copy_transfer"),
            t(Memory, "store_cannot_overwrite_a_stored_reference"),
            t(Backend, "mutated_int_list_store_offset_fails_admission"),
        ],
    },
    RuleEntry {
        rule: CanonicalRuleId::ClosureSlot,
        judgment: "check/memory.rs::MemoryChecker::closure_slot_type (from check_place)",
        predicate: "slot 0 of a closure record is `Callable(sig)` where `sig` is the module's registered signature for that layout; a load copies the scalar and a store copies a `Callable(sig)` value (only `FunctionValue` produces one); a mismatched signature, a non-callable value type, or a layout absent from the registry refuses",
        interpreter: "load/store (CopyScalar): copies the `Function` cell",
        backend: RuleBackend::Subjudgment(&[CanonicalRuleId::Load, CanonicalRuleId::Store]),
        admits: &[
            t(Check, "closure_record_stores_loads_and_calls_its_callable_slot"),
            t(Interpreter, "validated_closure_record_calls_through_its_callable_slot"),
        ],
        refuses: &[
            t(Check, "closure_slot_load_before_store_is_refused"), t(Check, "closure_slot_rejects_an_integer_store"),
            t(Check, "closure_slot_load_rejects_a_different_signature"),
            t(Check, "closure_record_outside_the_registry_has_no_callable_slot"),
        ],
    },
    RuleEntry {
        rule: CanonicalRuleId::CopyBytes,
        judgment: "check/memory.rs::MemoryChecker::copy_bytes (check_byte_slice)",
        predicate: "both slices are raw-byte buffers within their logical lengths, proven by a dominating end guard; typed or pointer elements never copy as bytes",
        interpreter: "copy_range(Copy): copies scalar cells",
        backend: RuleBackend::Receipt(CorrespondenceRule::MemoryCopy),
        admits: &[t(Check, "raw_byte_slice_copy_is_accepted"), t(Launch, "admitted_string_equality_and_char_count_scan_bytes")],
        refuses: &[t(Check, "typed_element_slice_cannot_use_copy_bytes"), t(Memory, "byte_copy_requires_a_dominating_dynamic_end_guard"), t(Check, "uninitialized_scalar_load_is_refused")],
    },
    RuleEntry {
        rule: CanonicalRuleId::MoveRange,
        judgment: "check/memory.rs::MemoryChecker::move_range (require_initialized_range, require_destination_range_empty)",
        predicate: "an initialized scalar prefix moves to the destination and leaves the source uninitialized; a pointer prefix moves every stored token from a complete initialized source into an empty destination of the same element, distinct storage, both unique and reference counted; the source prefix is consumed",
        interpreter: "copy_range(Move): moves cells out of the source",
        backend: RuleBackend::Receipt(CorrespondenceRule::MemoryOwnership),
        admits: &[
            t(Memory, "scalar_move_range_is_bounds_checked_and_admitted"),
            t(Interpreter, "scalar_move_range_transfers_cells_and_uninitializes_the_source"),
            t(Backend, "scalar_move_range_admits_and_executes_destination_value"),
            t(Memory, "dynamic_pointer_move_range_transfers_initialized_ownership_once"),
            t(Backend, "admitted_pointer_list_move_and_clone_execute_with_retained_child"),
        ],
        refuses: &[
            t(Memory, "pointer_move_range_rejects_uninitialized_source"),
            t(Memory, "pointer_move_range_rejects_source_reuse_after_consumption"),
            t(Memory, "pointer_move_range_rejects_overlapping_storage_before_transfer"),
            t(Memory, "pointer_move_range_rejects_initialized_destination"),
            t(Memory, "pointer_move_range_rejects_container_as_element_type"),
            t(Memory, "range_transfer_rejects_a_slice_past_logical_length"),
            t(Memory, "range_transfer_rejects_same_storage_without_overlap_policy"),
            t(Check, "uninitialized_scalar_load_is_refused"),
        ],
    },
    RuleEntry {
        rule: CanonicalRuleId::CloneRange,
        judgment: "check/memory.rs::MemoryChecker::clone_range",
        predicate: "a scalar prefix copies; a pointer prefix retains every element of an initialized managed source into an empty unique destination, after which both prefixes are shared",
        interpreter: "copy_range(Copy): copies cells and retains every reference cell",
        backend: RuleBackend::Receipt(CorrespondenceRule::MemoryCopy),
        admits: &[
            t(Memory, "scalar_clone_range_is_ownership_safe_and_admitted"),
            t(Memory, "dynamic_pointer_clone_range_shares_initialized_elements"),
            t(Backend, "scalar_clone_range_admits_emits_and_matches_exact_correspondence"),
            t(Backend, "pointer_bearing_clone_range_lowers_in_canonical_backend"),
            t(Backend, "pointer_clone_range_retains_executes_and_recursively_releases"),
        ],
        refuses: &[
            t(Memory, "pointer_bearing_ranges_fail_closed_without_element_transfer_state"),
            t(Memory, "pointer_clone_range_rejects_same_allocation_overlap"),
            t(Memory, "pointer_clone_range_rejects_partially_initialized_source"),
            t(Memory, "pointer_clone_range_rejects_destination_past_capacity"),
            t(Backend, "malformed_clone_range_places_are_refused_by_canonical_backend"),
            t(Backend, "scalar_clone_range_count_stride_overflow_traps_before_copy"),
            t(Backend, "scalar_clone_range_start_stride_overflow_traps_before_copy"),
            t(Check, "uninitialized_scalar_load_is_refused"),
        ],
    },
    RuleEntry {
        rule: CanonicalRuleId::UpdateCount,
        judgment: "check/memory.rs::MemoryChecker::update_count",
        predicate: "a unique counted object's logical length moves to a value at most its capacity; a pointer list's new prefix must be initialized",
        interpreter: "update_count: sets the logical length",
        backend: RuleBackend::Receipt(CorrespondenceRule::MemoryUpdateCount),
        admits: &[t(Check, "update_count_grows_logical_length_only_after_capacity_guard"), t(Launch, "admitted_lists_of_strings_are_read_iterated_and_pushed")],
        refuses: &[t(Check, "logical_length_guard_is_not_satisfied_by_capacity_guard"), t(Memory, "dynamic_pointer_prefix_cannot_hide_an_unaccounted_child_slot"), t(Check, "uninitialized_scalar_load_is_refused")],
    },
    RuleEntry {
        rule: CanonicalRuleId::MoveValue,
        judgment: "check/memory.rs::MemoryChecker::move_value (references); check.rs::check_operation (scalars)",
        predicate: "a scalar copies; a reference token moves once from a live, unaliased, unstored source, which becomes consumed",
        interpreter: "execute_operation: a reference move transfers the token, a scalar move copies",
        backend: RuleBackend::Receipt(CorrespondenceRule::ScalarMove),
        admits: &[t(Backend, "encoded_structured_loop_executes_carried_arguments"), t(Launch, "admitted_var_alias_of_a_list_parameter_copies_on_write")],
        refuses: &[
            t(Memory, "move_value_rejects_shared_source"),
            t(Memory, "move_value_rejects_live_object_alias"),
            t(Memory, "move_value_rejects_stored_alias"),
        ],
    },
    RuleEntry {
        rule: CanonicalRuleId::MakeInlineAggregate,
        judgment: "check.rs::check_operation (inline_scalar_layout_type); check_function refuses inline aggregates in control flow",
        predicate: "builds a packed scalar record from typed scalar fields; the value never crosses a block edge or a call",
        interpreter: "execute_operation: a tuple of scalar cells",
        backend: RuleBackend::Receipt(CorrespondenceRule::ScalarMove),
        admits: &[t(Launch, "admitted_tuples_construct_index_destructure_and_nest")],
        refuses: &[t(Check, "block_arguments_are_typed_ssa_definitions")],
    },
    RuleEntry {
        rule: CanonicalRuleId::ExtractInlineField,
        judgment: "check.rs::check_operation (inline_scalar_layout_type)",
        predicate: "reads one scalar field of an inline aggregate by index and type",
        interpreter: "execute_operation: projects the cell",
        backend: RuleBackend::Receipt(CorrespondenceRule::ScalarMove),
        admits: &[t(Launch, "admitted_tuples_construct_index_destructure_and_nest")],
        refuses: &[t(Check, "block_arguments_are_typed_ssa_definitions")],
    },
    RuleEntry {
        rule: CanonicalRuleId::CloneRef,
        judgment: "check/memory.rs::MemoryChecker::clone_ref",
        predicate: "a second shared token of a live unique-or-shared object with reference-count authority (or a static); the source becomes shared; a saturated count traps at runtime",
        interpreter: "retain: increments the count, trapping at the i32 maximum",
        backend: RuleBackend::Receipt(CorrespondenceRule::MemoryOwnership),
        admits: &[t(Memory, "clone_ref_tracks_two_affine_shared_tokens"), t(Interpreter, "validated_clone_and_two_drops_execute")],
        refuses: &[
            t(Memory, "clone_ref_rejects_storage_without_refcount_authority"),
            t(Interpreter, "reference_counts_trap_at_i32_maximum"),
            t(Backend, "share_at_max_refcount_traps_before_destination_or_refcount_write"),
        ],
    },
    RuleEntry {
        rule: CanonicalRuleId::Retain,
        judgment: "check/memory.rs::MemoryChecker::check (unsupported)",
        predicate: "not a v1 rule: a second owner comes only from `CloneRef`; the checker refuses it and the backend has no mapping",
        interpreter: "retain (unreachable through an admitted module)",
        backend: RuleBackend::Refused,
        admits: &[],
        refuses: &[t(Memory, "retain_is_refused_in_v1")],
    },
    RuleEntry {
        rule: CanonicalRuleId::Release,
        judgment: "check/memory.rs::MemoryChecker::release",
        predicate: "drops one shared token of a reference-counted object whose owned slots are initialized; a zero count traps at runtime",
        interpreter: "release: decrements, reclaiming at zero and trapping below",
        backend: RuleBackend::Receipt(CorrespondenceRule::MemoryOwnership),
        admits: &[t(Interpreter, "validated_clone_and_two_drops_execute")],
        refuses: &[
            t(Memory, "release_rejects_shared_value_without_refcount_authority"),
            t(Backend, "release_at_zero_refcount_traps_before_decrement"),
            t(Check, "tail_call_after_release_is_rejected"),
        ],
    },
    RuleEntry {
        rule: CanonicalRuleId::DropRef,
        judgment: "check/memory.rs::MemoryChecker::drop_ref (require_owned_slots_initialized, drop_ref_layout_supported)",
        predicate: "drops an owned token of a complete object; when the count reaches zero every initialized child (pointer elements, owned pointer fields) drops recursively before reclamation",
        interpreter: "release: recursive drop at zero",
        backend: RuleBackend::Receipt(CorrespondenceRule::MemoryOwnership),
        admits: &[
            t(Check, "owned_parameter_read_then_drop_is_accepted"),
            t(Backend, "admitted_pointer_list_move_and_clone_execute_with_retained_child"),
            t(Launch, "admitted_loop_over_list_parameters_drops_the_current_binding_at_exit"),
        ],
        refuses: &[t(Check, "owned_parameter_must_be_discharged_at_exit"), t(Memory, "pointer_bearing_reclaim_requires_recursive_drop")],
    },
    RuleEntry {
        rule: CanonicalRuleId::Reclaim,
        judgment: "check/memory.rs::MemoryChecker::reclaim",
        predicate: "frees a uniquely owned, complete, scalar-only object; shared objects and pointer-bearing layouts refuse (they drop recursively instead)",
        interpreter: "reclaim: frees the object",
        backend: RuleBackend::Receipt(CorrespondenceRule::MemoryOwnership),
        admits: &[t(Launch, "admitted_recursive_search_threads_a_list_parameter_through_loop_and_join")],
        refuses: &[
            t(Check, "reclaim_without_unique_object_authority_fails_closed"),
            t(Check, "shared_ownership_cannot_be_reclaimed_as_unique"),
            t(Memory, "pointer_bearing_reclaim_requires_recursive_drop"),
        ],
    },
    RuleEntry {
        rule: CanonicalRuleId::FunctionValue,
        judgment: "check.rs::check_operation",
        predicate: "a callable value of a checked function whose signature matches the destination's declared signature",
        interpreter: "execute_operation: a function value",
        backend: RuleBackend::Receipt(CorrespondenceRule::ScalarConstant),
        admits: &[t(Check, "function_value_authenticates_checked_target_signature"), t(Backend, "admitted_scalar_var_call_uses_authenticated_table_and_executes")],
        refuses: &[t(Check, "function_value_rejects_destination_signature_mismatch"), t(Check, "function_value_rejects_unknown_function")],
    },
    RuleEntry {
        rule: CanonicalRuleId::Call,
        judgment: "check.rs::check_call; check/memory.rs::MemoryChecker::consume_call_move",
        predicate: "arguments match the callee's contract by type and ownership: a moved reference is consumed, a value argument is a scalar; results follow the declared result contracts",
        interpreter: "execute: a nested frame; owned arguments transfer",
        backend: RuleBackend::Receipt(CorrespondenceRule::ScalarCall),
        admits: &[
            t(Interpreter, "validated_direct_call_executes_callee"),
            t(Backend, "direct_call_encodes_as_wasm_call"),
            t(Backend, "admitted_owned_reference_call_executes_and_preserves_ownership"),
            t(Check, "matching_callable_result_contract_is_accepted"),
        ],
        refuses: &[
            t(Check, "call_move_consumes_unique_and_later_use_is_rejected"),
            t(Check, "memory_value_call_is_fail_closed"),
            t(Check, "shared_move_into_unique_parameter_is_rejected"),
            t(Check, "declared_callable_result_must_match_checked_signature"),
            t(Check, "checked_value_memory_result_is_refused"),
            t(Backend, "mutated_direct_call_callee_index_fails_admission"),
        ],
    },
    RuleEntry {
        rule: CanonicalRuleId::VarCall,
        judgment: "check.rs::check_call (check_indirect_reference_arguments)",
        predicate: "an indirect call through a function value of the declared signature; memory Value arguments refuse, while reference Move arguments require the declared owned/shared convention and checked token transfer",
        interpreter: "execute: the callee is the function value",
        backend: RuleBackend::Receipt(CorrespondenceRule::ScalarCall),
        admits: &[t(Backend, "admitted_scalar_var_call_uses_authenticated_table_and_executes"), t(Launch, "admitted_scalar_var_write_back_through_an_indirect_call")],
        refuses: &[
            t(Check, "indirect_reference_value_argument_is_refused"),
            t(Backend, "variable_call_is_refused_fail_closed"),
            t(Backend, "mutated_scalar_var_call_signature_fails_exact_admission"),
        ],
    },
    RuleEntry {
        rule: CanonicalRuleId::HostCall,
        judgment: "check.rs::check_operation (authenticated import contracts)",
        predicate: "the import is authenticated; each argument fills its contract role: a capability only by a capability value of the right kind, a read buffer by a raw-byte slice within its logical length, a write buffer by a raw-byte slice within the capacity of a unique object (the guest publishes the written length under its own capacity guard); mutation and reentrancy beyond the contract refuse",
        interpreter: "execute_operation: the host fixture",
        backend: RuleBackend::Receipt(CorrespondenceRule::HostCall),
        admits: &[t(Check, "typed_capability_host_call_is_admitted"), t(Check, "authenticated_dynamic_host_slice_is_accepted"), t(Check, "write_buffer_host_call_is_admitted_within_capacity"), t(Launch, "admitted_console_program_runs_through_the_runtime_entry"), t(Launch, "admitted_float_render_writes_through_the_authenticated_float_to_str_import")],
        refuses: &[
            t(Check, "integer_cannot_fill_capability_host_role"),
            t(Check, "integer_cannot_be_wrapped_as_capability_host_argument"),
            t(Check, "wrong_capability_kind_is_rejected"),
            t(Check, "unknown_import_contract_is_rejected"),
            t(Check, "candidate_abi_bytes_do_not_authenticate_an_import"),
            t(Check, "host_buffer_extent_must_satisfy_contract"),
            t(Check, "host_slice_outside_logical_length_is_rejected"),
            t(Check, "pointer_or_typed_element_buffers_cannot_be_exposed_as_host_bytes"),
            t(Check, "unsupported_host_mutation_and_reentrancy_fail_closed"),
            t(Check, "host_mutation_must_match_explicit_buffer_access"),
            t(Check, "write_buffer_host_call_refuses_a_slice_beyond_capacity"),
            t(Check, "write_buffer_host_call_refuses_a_shared_object"),
            t(Check, "write_buffer_host_call_refuses_a_read_only_contract"),
        ],
    },
    RuleEntry {
        rule: CanonicalRuleId::Branch,
        judgment: "check.rs::check_branch; check/memory.rs::MemoryChecker::transfer_edge and meet",
        predicate: "arguments match the target's parameters by type; facts are renamed to the parameters; a unique phi takes a sole live token, a shared phi a shared or weakenable one; the join keeps only what every edge establishes",
        interpreter: "execute_frame: binds the parameters",
        backend: RuleBackend::Receipt(CorrespondenceRule::Control),
        admits: &[
            t(Backend, "encoded_structured_loop_executes_carried_arguments"),
            t(Memory, "unique_phi_rebases_distinct_fresh_identities_before_join"),
            t(Memory, "affine_boundary_join_preserves_release_obligations_on_both_edges"),
        ],
        refuses: &[
            t(Check, "block_arguments_are_typed_ssa_definitions"),
            t(Check, "definition_on_one_branch_does_not_dominate_join"),
            t(Memory, "unique_phi_rejects_stored_alias"),
            t(Memory, "affine_boundary_join_does_not_forget_owned_heap_slots"),
        ],
    },
    RuleEntry {
        rule: CanonicalRuleId::BranchIf,
        judgment: "check.rs::check_branch (both edges); check/memory.rs::MemoryChecker::transfer_edge",
        predicate: "a `Bool` condition; each edge is a `Branch`; the backend structures reducible control flow only",
        interpreter: "execute_frame: takes the edge the condition selects",
        backend: RuleBackend::Receipt(CorrespondenceRule::Control),
        admits: &[t(Backend, "encoded_structured_branch_executes_both_arms"), t(Launch, "admitted_while_loops_with_short_circuit_conditions_run")],
        refuses: &[
            t(Backend, "irreducible_cfg_is_refused_without_dispatcher_lowering"),
            t(Backend, "common_reachable_join_with_non_terminating_bypass_is_refused"),
            t(Backend, "structured_loop_plan_rejects_control_opcode_mutation"),
        ],
    },
    RuleEntry {
        rule: CanonicalRuleId::Return,
        judgment: "check.rs::check_terminator; check/memory.rs (return contracts, affine boundary)",
        predicate: "values match the signature's result contracts; an owned result is the sole live fresh token; no other owned token remains live; a tail obligation is not discharged by a return",
        interpreter: "execute_frame: yields the values",
        backend: RuleBackend::Receipt(CorrespondenceRule::Control),
        admits: &[t(Memory, "return_contract_accepts_only_sole_unique_fresh_object"), t(Memory, "owned_result_contract_accepts_unique_rc_without_inventing_a_clone")],
        refuses: &[
            t(Memory, "return_contract_rejects_fresh_allocation_class_mismatch"),
            t(Memory, "affine_boundary_rejects_duplicate_owned_return_lanes"),
            t(Check, "ordinary_return_cannot_discharge_a_tail_obligation"),
            t(Check, "owned_parameter_must_be_discharged_at_exit"),
        ],
    },
    RuleEntry {
        rule: CanonicalRuleId::TailCall,
        judgment: "check.rs::check_tail_call and observe_tail_site; check/memory.rs (no untransferred tokens)",
        predicate: "a proper-tail site from the checked manifest, discharged exactly once; arguments transfer every live owned token; the callee's results are the caller's",
        interpreter: "execute_frame: trampolines without growing the frame stack",
        backend: RuleBackend::Receipt(CorrespondenceRule::Control),
        admits: &[
            t(Interpreter, "validated_tail_call_trampolines_three_hundred_thousand_times"),
            t(Backend, "proper_tail_call_encodes_as_wasm_return_call"),
            t(Launch, "admitted_proper_tail_calls_inside_if_and_match_arms_run_in_constant_stack"),
            t(Check, "tail_shared_move_transfers_without_reclaim"),
        ],
        refuses: &[
            t(Check, "proper_tail_site_is_matched_exactly_once"),
            t(Check, "duplicate_tail_terminators_cannot_discharge_one_obligation"),
            t(Check, "unreachable_tail_decoy_cannot_discharge_an_obligation"),
            t(Check, "tail_call_after_release_is_rejected"),
            t(Memory, "affine_boundary_tail_call_requires_no_untransferred_tokens"),
        ],
    },
    RuleEntry {
        rule: CanonicalRuleId::VarTailCall,
        judgment: "check.rs::check_tail_call (indirect)",
        predicate: "a proper-tail site through a function value of the declared signature, under the `VarCall` argument rules",
        interpreter: "execute_frame: trampolines through the function value",
        backend: RuleBackend::Receipt(CorrespondenceRule::Control),
        admits: &[t(Backend, "admitted_scalar_var_tail_call_executes_with_return_call_indirect")],
        refuses: &[t(Check, "indirect_reference_value_argument_is_refused"), t(Check, "proper_tail_site_is_matched_exactly_once")],
    },
    RuleEntry {
        rule: CanonicalRuleId::Trap,
        judgment: "check.rs::check_terminator",
        predicate: "always admitted; execution stops with a trap",
        interpreter: "execute_frame: a trap",
        backend: RuleBackend::Receipt(CorrespondenceRule::Control),
        admits: &[t(Backend, "encoded_trap_terminator_remains_terminal"), t(Launch, "admitted_guard_near_misses_trap_exactly_at_the_boundary")],
        refuses: &[],
    },
    RuleEntry {
        rule: CanonicalRuleId::Unreachable,
        judgment: "check.rs::check_terminator",
        predicate: "always admitted; the block must be unreachable, which the backend's structuring verifies",
        interpreter: "execute_frame: a trap if ever reached",
        backend: RuleBackend::Receipt(CorrespondenceRule::Control),
        admits: &[t(Check, "valid_unreachable_block_is_available_to_pruning")],
        refuses: &[t(Backend, "common_reachable_join_with_non_terminating_bypass_is_refused")],
    },
    RuleEntry {
        rule: CanonicalRuleId::ClosureOwner,
        judgment: "check/owners.rs::Registry::new and check_owner_operation",
        predicate: "an authenticated owner family binds every constructor to a function with the body signature (leading Shared Gc owner plus declaration lanes), validates capture kind/storage/transfer, demotes every live token of a CloneManaged Ref capture to Shared, and requires registered owner/list tokens for linear clone, call, list, and drop operations",
        interpreter: "execute_owner_operation: constructs and authenticates opaque owners/lists, preserves actual capability and managed-reference captures, and releases final owner/list tokens",
        backend: RuleBackend::Receipt(CorrespondenceRule::ClosureOwner),
        admits: &[
            t(Interpreter, "owner_oracle_clone_managed_capture_releases_exactly_once"),
            t(OwnerBackend, "opaque_owner_cleanup_reclaims_once_and_rejects_unknown_family"),
            t(OwnerLaunch, "opaque_closure_plugin_host_expected_three_legs"),
            t(OwnerLaunch, "opaque_closure_mixed_string_console_list_after_original_cleanup_three_legs"),
            t(OwnerRuntime, "opaque_owner_gc_payload_and_list_hold_actual_references"),
        ],
        refuses: &[
            t(OwnerCheck, "opaque_owner_affine_dead_list_and_wrong_family_refuse"),
            t(OwnerCheck, "opaque_owner_affine_call_requires_move_and_no_leak"),
            t(OwnerRuntime, "opaque_owner_list_drops_all_variants_and_refuses_dead_list"),
        ],
    },
];

/// One ownership transfer of a reference-counted storage class (B2).
#[derive(Clone, Copy, Debug)]
pub struct OwnershipEntry {
    pub storage: &'static str,
    pub transfer: &'static str,
    pub verdict: &'static str,
    pub evidence: &'static [TestRef],
}

/// Clone, move, replace, drop, reclaim, and count bounds for strings and
/// scalar/string lists.
pub const OWNERSHIP: &[OwnershipEntry] = &[
    OwnershipEntry { storage: "String", transfer: "clone", verdict: "admitted: `CloneRef` yields a second shared token; a copy for storage is a fresh buffer", evidence: &[t(Memory, "clone_ref_tracks_two_affine_shared_tokens"), t(Launch, "admitted_string_literals_cross_owned_parameters_and_returns")] },
    OwnershipEntry { storage: "String", transfer: "move", verdict: "admitted: a unique token moves once into a call or a slot; a later use of the source refuses", evidence: &[t(Check, "call_move_consumes_unique_and_later_use_is_rejected"), t(Check, "tail_shared_move_transfers_without_reclaim")] },
    OwnershipEntry { storage: "String", transfer: "replace", verdict: "refused: a stored reference is never overwritten in place; replacement is a fresh object", evidence: &[t(Memory, "store_cannot_overwrite_a_stored_reference")] },
    OwnershipEntry { storage: "String", transfer: "drop", verdict: "admitted: the last drop reclaims; every owned parameter is discharged at exit", evidence: &[t(Check, "owned_parameter_read_then_drop_is_accepted"), t(Check, "owned_parameter_must_be_discharged_at_exit"), t(Interpreter, "validated_clone_and_two_drops_execute")] },
    OwnershipEntry { storage: "String", transfer: "reclaim", verdict: "admitted only for a unique, complete object; shared tokens refuse", evidence: &[t(Check, "shared_ownership_cannot_be_reclaimed_as_unique"), t(Check, "reclaim_without_unique_object_authority_fails_closed")] },
    OwnershipEntry { storage: "String", transfer: "count overflow", verdict: "traps before the count is written", evidence: &[t(Interpreter, "reference_counts_trap_at_i32_maximum"), t(Backend, "share_at_max_refcount_traps_before_destination_or_refcount_write")] },
    OwnershipEntry { storage: "String", transfer: "count underflow", verdict: "traps before the decrement", evidence: &[t(Backend, "release_at_zero_refcount_traps_before_decrement")] },
    OwnershipEntry { storage: "List(Int)", transfer: "clone", verdict: "admitted: `CloneRange` copies the scalar prefix into a fresh list", evidence: &[t(Memory, "scalar_clone_range_is_ownership_safe_and_admitted"), t(Backend, "scalar_clone_range_admits_emits_and_matches_exact_correspondence"), t(Launch, "admitted_var_alias_of_a_list_parameter_copies_on_write")] },
    OwnershipEntry { storage: "List(Int)", transfer: "move", verdict: "admitted: `MoveRange` copies the scalar prefix (no tokens to transfer) within bounds", evidence: &[t(Memory, "scalar_move_range_is_bounds_checked_and_admitted"), t(Memory, "range_transfer_rejects_a_slice_past_logical_length")] },
    OwnershipEntry { storage: "List(Int)", transfer: "replace", verdict: "admitted: a scalar element store overwrites in place under a bounds guard", evidence: &[t(Interpreter, "validated_counted_element_store_and_load_execute"), t(Backend, "packed_scalar_collection_access_mutates_and_preserves_headers")] },
    OwnershipEntry { storage: "List(Int)", transfer: "drop", verdict: "admitted: the last drop reclaims the buffer", evidence: &[t(Launch, "admitted_loop_over_list_parameters_drops_the_current_binding_at_exit"), t(Backend, "admitted_int_list_loop_executes_with_bounds_checks")] },
    OwnershipEntry { storage: "List(Int)", transfer: "reclaim", verdict: "admitted for a unique object; growth keeps the logical length at most the capacity", evidence: &[t(Check, "update_count_grows_logical_length_only_after_capacity_guard"), t(Launch, "admitted_recursive_search_threads_a_list_parameter_through_loop_and_join")] },
    OwnershipEntry { storage: "List(String)", transfer: "clone", verdict: "admitted: `CloneRange` retains every element; both prefixes become shared", evidence: &[t(Memory, "dynamic_pointer_clone_range_shares_initialized_elements"), t(Memory, "pointer_clone_range_rejects_same_allocation_overlap"), t(Memory, "pointer_clone_range_rejects_partially_initialized_source"), t(Memory, "pointer_clone_range_rejects_destination_past_capacity"), t(Backend, "pointer_clone_range_retains_executes_and_recursively_releases"), t(Launch, "admitted_lists_of_strings_are_read_iterated_and_pushed")] },
    OwnershipEntry { storage: "List(String)", transfer: "move", verdict: "admitted: `MoveRange` transfers every stored token once from a complete source into an empty destination", evidence: &[t(Memory, "dynamic_pointer_move_range_transfers_initialized_ownership_once"), t(Memory, "pointer_move_range_rejects_source_reuse_after_consumption"), t(Backend, "admitted_pointer_list_move_and_clone_execute_with_retained_child")] },
    OwnershipEntry { storage: "List(String)", transfer: "replace", verdict: "refused: a stored element reference is never overwritten in place", evidence: &[t(Memory, "store_cannot_overwrite_a_stored_reference"), t(Memory, "pointer_move_range_rejects_initialized_destination")] },
    OwnershipEntry { storage: "List(String)", transfer: "drop", verdict: "admitted: the last drop releases every initialized element before the buffer", evidence: &[t(Backend, "admitted_pointer_list_move_and_clone_execute_with_retained_child"), t(Memory, "dynamic_pointer_prefix_cannot_hide_an_unaccounted_child_slot")] },
    OwnershipEntry { storage: "List(String)", transfer: "reclaim", verdict: "refused: a pointer-bearing object is dropped recursively, never reclaimed as scalar storage", evidence: &[t(Memory, "pointer_bearing_reclaim_requires_recursive_drop")] },
];

/// One supported call form (B4).
#[derive(Clone, Copy, Debug)]
pub struct CallEntry {
    pub form: &'static str,
    pub convention: &'static str,
    pub verdict: &'static str,
    pub evidence: &'static [TestRef],
}

/// The compact supported-call manifest. v1 has no `CallBorrow`: the `let`
/// convention clones a token before a non-tail call and moves it at a tail
/// edge.
pub const CALLS: &[CallEntry] = &[
    CallEntry { form: "direct call", convention: "scalar value", verdict: "admitted", evidence: &[t(Interpreter, "validated_direct_call_executes_callee"), t(Backend, "direct_call_encodes_as_wasm_call"), t(Backend, "parameterized_scalar_call_artifact_executes")] },
    CallEntry { form: "direct call", convention: "`let` reference (shared token, cloned before the call)", verdict: "admitted", evidence: &[t(Backend, "admitted_owned_reference_call_executes_and_preserves_ownership"), t(Check, "shared_move_into_unique_parameter_is_rejected")] },
    CallEntry { form: "direct call", convention: "`own` reference (unique token moved)", verdict: "admitted; the source is consumed", evidence: &[t(Launch, "admitted_string_literals_cross_owned_parameters_and_returns"), t(Check, "call_move_consumes_unique_and_later_use_is_rejected")] },
    CallEntry { form: "direct call", convention: "`var` scalar (write-back result lane)", verdict: "admitted", evidence: &[t(Launch, "admitted_scalar_var_parameters_write_back"), t(Memory, "affine_boundary_rejects_duplicate_owned_return_lanes")] },
    CallEntry { form: "direct call", convention: "memory-bearing value argument", verdict: "refused", evidence: &[t(Check, "memory_value_call_is_fail_closed")] },
    CallEntry { form: "indirect call", convention: "scalar value through a function value", verdict: "admitted", evidence: &[t(Backend, "admitted_scalar_var_call_uses_authenticated_table_and_executes"), t(Launch, "admitted_scalar_var_write_back_through_an_indirect_call")] },
    CallEntry { form: "indirect call", convention: "reference value argument without an owned/shared transfer convention", verdict: "refused", evidence: &[t(Check, "indirect_reference_value_argument_is_refused"), t(Backend, "variable_call_is_refused_fail_closed")] },
    CallEntry { form: "indirect call", convention: "reference under an explicit owned/shared signature convention", verdict: "admitted with checked token transfer", evidence: &[t(Launch, "admitted_lambdas_over_references_pass_through_indirect_calls_and_std_combinators")] },
    CallEntry { form: "proper tail call", convention: "direct, every owned token transferred", verdict: "admitted; constant stack over 300,000 transitions", evidence: &[t(Interpreter, "validated_tail_call_trampolines_three_hundred_thousand_times"), t(Backend, "proper_tail_call_encodes_as_wasm_return_call"), t(Launch, "admitted_proper_tail_calls_inside_if_and_match_arms_run_in_constant_stack")] },
    CallEntry { form: "proper tail call", convention: "indirect", verdict: "admitted", evidence: &[t(Backend, "admitted_scalar_var_tail_call_executes_with_return_call_indirect")] },
    CallEntry { form: "proper tail call", convention: "site not in the checked manifest, or discharged twice", verdict: "refused", evidence: &[t(Check, "proper_tail_site_is_matched_exactly_once"), t(Check, "duplicate_tail_terminators_cannot_discharge_one_obligation"), t(Check, "ordinary_return_cannot_discharge_a_tail_obligation")] },
    CallEntry { form: "host call", convention: "authenticated import with a capability role", verdict: "admitted only for a capability value of the declared kind", evidence: &[t(Check, "typed_capability_host_call_is_admitted"), t(Check, "integer_cannot_fill_capability_host_role"), t(Check, "wrong_capability_kind_is_rejected")] },
    CallEntry { form: "closure value", convention: "named function used as a value (adapter binding)", verdict: "admitted only when the adapter's body is exactly one forwarding call to its target under the environment-taking signature", evidence: &[t(Authority, "adapter_binding_admits_a_trailing_release_of_the_environment"), t(Authority, "adapter_binding_rejects_a_body_that_does_extra_work")] },
    CallEntry { form: "`CallBorrow`", convention: "any", verdict: "absent from v1: no such rule exists", evidence: &[] },
];

/// The operation vocabulary frozen by the RFC. The implementation manifest is
/// intentionally allowed to split, restrict, defer, or refuse these names;
/// this typed list prevents that relationship from being implicit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RfcKernelId {
    AllocRc,
    AllocRegion,
    CloneRef,
    MoveRef,
    DropRef,
    IsUnique,
    Project,
    Load,
    Initialize,
    Replace,
    StoreScalar,
    ByteCopy,
    MoveValue,
    CloneValue,
    MoveRange,
    CloneRange,
    UpdateCount,
    Guard,
    CheckedNarrow,
    Call,
    VarCall,
    TailCall,
    MakeClosure,
    HostCall,
    Trap,
}

impl RfcKernelId {
    pub const fn name(self) -> &'static str {
        match self {
            Self::AllocRc => "AllocRc",
            Self::AllocRegion => "AllocRegion",
            Self::CloneRef => "CloneRef",
            Self::MoveRef => "MoveRef",
            Self::DropRef => "DropRef",
            Self::IsUnique => "IsUnique",
            Self::Project => "Project",
            Self::Load => "Load",
            Self::Initialize => "Initialize",
            Self::Replace => "Replace",
            Self::StoreScalar => "StoreScalar",
            Self::ByteCopy => "ByteCopy",
            Self::MoveValue => "MoveValue",
            Self::CloneValue => "CloneValue",
            Self::MoveRange => "MoveRange",
            Self::CloneRange => "CloneRange",
            Self::UpdateCount => "UpdateCount",
            Self::Guard => "Guard",
            Self::CheckedNarrow => "CheckedNarrow",
            Self::Call => "Call",
            Self::VarCall => "VarCall",
            Self::TailCall => "TailCall",
            Self::MakeClosure => "MakeClosure",
            Self::HostCall => "HostCall",
            Self::Trap => "Trap",
        }
    }
}

/// How one RFC kernel name relates to the current canonical implementation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RfcKernelDisposition {
    /// One current rule implements this RFC name within the stated scope.
    ExactAlias { rules: &'static [CanonicalRuleId], scope: &'static str },
    /// Several current rules compose the RFC operation.
    Composite { rules: &'static [CanonicalRuleId], scope: &'static str },
    /// The RFC name is a judgment over existing rules, not another receipt.
    Subjudgment { rules: &'static [CanonicalRuleId], scope: &'static str },
    /// A current rule exists, but only the stated subset is admitted.
    Restricted { rules: &'static [CanonicalRuleId], scope: &'static str },
    /// No current rule is claimed; this remains future work.
    Deferred { reason: &'static str },
    /// The current v1 boundary explicitly refuses this operation.
    Refused { reason: &'static str },
}

impl RfcKernelDisposition {
    fn kind(self) -> &'static str {
        match self {
            Self::ExactAlias { .. } => "exact scoped alias",
            Self::Composite { .. } => "composite",
            Self::Subjudgment { .. } => "subjudgment",
            Self::Restricted { .. } => "restricted",
            Self::Deferred { .. } => "deferred",
            Self::Refused { .. } => "refused",
        }
    }

    fn rules(self) -> &'static [CanonicalRuleId] {
        match self {
            Self::ExactAlias { rules, .. }
            | Self::Composite { rules, .. }
            | Self::Subjudgment { rules, .. }
            | Self::Restricted { rules, .. } => rules,
            Self::Deferred { .. } | Self::Refused { .. } => &[],
        }
    }

    fn scope(self) -> &'static str {
        match self {
            Self::ExactAlias { scope, .. }
            | Self::Composite { scope, .. }
            | Self::Subjudgment { scope, .. }
            | Self::Restricted { scope, .. } => scope,
            Self::Deferred { reason } | Self::Refused { reason } => reason,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct RfcKernelEntry {
    pub kernel: RfcKernelId,
    pub disposition: RfcKernelDisposition,
}

/// The RFC kernel crosswalk, in the exact order of the RFC's operation list.
pub const RFC_KERNEL_CROSSWALK: &[RfcKernelEntry] = &[
    RfcKernelEntry { kernel: RfcKernelId::AllocRc, disposition: RfcKernelDisposition::ExactAlias { rules: &[CanonicalRuleId::Alloc], scope: "reference-counted allocation only; region and scratch classes are not aliases" } },
    RfcKernelEntry { kernel: RfcKernelId::AllocRegion, disposition: RfcKernelDisposition::Refused { reason: "bump/region allocation is outside the current v1 admission boundary" } },
    RfcKernelEntry { kernel: RfcKernelId::CloneRef, disposition: RfcKernelDisposition::ExactAlias { rules: &[CanonicalRuleId::CloneRef], scope: "reference-counted or static shared clone" } },
    RfcKernelEntry { kernel: RfcKernelId::MoveRef, disposition: RfcKernelDisposition::ExactAlias { rules: &[CanonicalRuleId::MoveValue], scope: "reference-token moves; scalar moves share the same typed rule" } },
    RfcKernelEntry { kernel: RfcKernelId::DropRef, disposition: RfcKernelDisposition::ExactAlias { rules: &[CanonicalRuleId::DropRef], scope: "complete owned reference-counted objects" } },
    RfcKernelEntry { kernel: RfcKernelId::IsUnique, disposition: RfcKernelDisposition::Deferred { reason: "no implemented runtime uniqueness predicate is claimed by the current v1 kernel" } },
    RfcKernelEntry { kernel: RfcKernelId::Project, disposition: RfcKernelDisposition::Subjudgment { rules: &[CanonicalRuleId::Load, CanonicalRuleId::Store, CanonicalRuleId::InitNullableOwned, CanonicalRuleId::CopyBytes, CanonicalRuleId::MoveRange, CanonicalRuleId::CloneRange], scope: "typed Place projections are checked through the applicable load/store, nullable initialization, byte-copy, or range-transfer judgment; Project is not a standalone instruction" } },
    RfcKernelEntry { kernel: RfcKernelId::Load, disposition: RfcKernelDisposition::ExactAlias { rules: &[CanonicalRuleId::Load], scope: "typed initialized header, element, field, and slot reads" } },
    RfcKernelEntry { kernel: RfcKernelId::Initialize, disposition: RfcKernelDisposition::Composite { rules: &[CanonicalRuleId::InitNullableOwned, CanonicalRuleId::Store], scope: "nullable owned initialization is explicit; ordinary empty-slot initialization uses Store" } },
    RfcKernelEntry { kernel: RfcKernelId::Replace, disposition: RfcKernelDisposition::Restricted { rules: &[CanonicalRuleId::Store], scope: "scalar replacement is admitted through Store; reference replacement of an initialized slot is refused" } },
    RfcKernelEntry { kernel: RfcKernelId::StoreScalar, disposition: RfcKernelDisposition::ExactAlias { rules: &[CanonicalRuleId::Store], scope: "scalar and byte stores under their typed bounds; reference stores are a separate Store transfer" } },
    RfcKernelEntry { kernel: RfcKernelId::ByteCopy, disposition: RfcKernelDisposition::ExactAlias { rules: &[CanonicalRuleId::CopyBytes], scope: "raw-byte slices only" } },
    RfcKernelEntry { kernel: RfcKernelId::MoveValue, disposition: RfcKernelDisposition::ExactAlias { rules: &[CanonicalRuleId::MoveValue], scope: "typed scalar or reference value movement" } },
    RfcKernelEntry { kernel: RfcKernelId::CloneValue, disposition: RfcKernelDisposition::Composite { rules: &[CanonicalRuleId::Alloc, CanonicalRuleId::Load, CanonicalRuleId::Store, CanonicalRuleId::CloneRef, CanonicalRuleId::CloneRange], scope: "currently admitted typed value-copy templates only: String/Bytes, admitted scalar/reference lists, and admitted record/tuple/sum shapes; the generic RFC operation is not universal" } },
    RfcKernelEntry { kernel: RfcKernelId::MoveRange, disposition: RfcKernelDisposition::ExactAlias { rules: &[CanonicalRuleId::MoveRange], scope: "typed scalar or initialized pointer prefixes" } },
    RfcKernelEntry { kernel: RfcKernelId::CloneRange, disposition: RfcKernelDisposition::ExactAlias { rules: &[CanonicalRuleId::CloneRange], scope: "typed scalar or initialized pointer prefixes" } },
    RfcKernelEntry { kernel: RfcKernelId::UpdateCount, disposition: RfcKernelDisposition::ExactAlias { rules: &[CanonicalRuleId::UpdateCount], scope: "counted logical length under capacity and initialization facts" } },
    RfcKernelEntry { kernel: RfcKernelId::Guard, disposition: RfcKernelDisposition::ExactAlias { rules: &[CanonicalRuleId::Guard], scope: "typed false-condition traps and dominating bounds facts" } },
    RfcKernelEntry { kernel: RfcKernelId::CheckedNarrow, disposition: RfcKernelDisposition::ExactAlias { rules: &[CanonicalRuleId::NarrowSize], scope: "Int to i32-width Size conversion" } },
    RfcKernelEntry { kernel: RfcKernelId::Call, disposition: RfcKernelDisposition::ExactAlias { rules: &[CanonicalRuleId::Call], scope: "authenticated direct calls" } },
    RfcKernelEntry { kernel: RfcKernelId::VarCall, disposition: RfcKernelDisposition::ExactAlias { rules: &[CanonicalRuleId::VarCall], scope: "authenticated indirect calls under v1 argument restrictions" } },
    RfcKernelEntry { kernel: RfcKernelId::TailCall, disposition: RfcKernelDisposition::Composite { rules: &[CanonicalRuleId::TailCall, CanonicalRuleId::VarTailCall], scope: "direct and indirect checked proper-tail sites" } },
    RfcKernelEntry { kernel: RfcKernelId::MakeClosure, disposition: RfcKernelDisposition::Restricted { rules: &[CanonicalRuleId::Alloc, CanonicalRuleId::FunctionValue, CanonicalRuleId::InitNullableOwned, CanonicalRuleId::CloneRef, CanonicalRuleId::Store], scope: "validated closure record/environment construction with checked scalar and supported managed-reference captures; inactive owned slots are explicitly absent; FunctionValue alone is insufficient and arbitrary capture types are not implied" } },
    RfcKernelEntry { kernel: RfcKernelId::HostCall, disposition: RfcKernelDisposition::ExactAlias { rules: &[CanonicalRuleId::HostCall], scope: "authenticated capability and buffer contracts" } },
    RfcKernelEntry { kernel: RfcKernelId::Trap, disposition: RfcKernelDisposition::ExactAlias { rules: &[CanonicalRuleId::Trap], scope: "explicit terminal traps" } },
];

fn backend_name(rule: RuleBackend) -> String {
    match rule {
        RuleBackend::Receipt(rule) => format!("{rule:?}"),
        RuleBackend::Subjudgment(parents) => format!(
            "subjudgment of {}",
            parents.iter().map(|parent| format!("`{parent:?}`")).collect::<Vec<_>>().join(", ")
        ),
        RuleBackend::Refused => "refused".to_owned(),
    }
}

fn cite(tests: &[TestRef]) -> String {
    if tests.is_empty() {
        return "—".to_owned();
    }
    tests
        .iter()
        .map(|test| {
            let file = match test.file {
                Check => "check",
                Memory => "memory",
                Interpreter => "interpreter",
                Backend => "backend",
                Launch => "launch",
                Authority => "authority",
                EvidenceFile::NullableBackend => "nullable backend",
                EvidenceFile::UnitBackend => "unit backend",
                OwnerCheck => "owner checker",
                OwnerBackend => "owner backend",
                OwnerLaunch => "owner launch",
                OwnerRuntime => "owner runtime",
                EvidenceFile::UniquePopBackend => "unique pop backend",
            };
            format!("`{}` ({file})", test.name)
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The reviewed document: `rfcs/0165-rule-manifest.md`.
pub fn render_markdown() -> String {
    let mut out = String::new();
    out.push_str("# RFC-0165 rule manifest (generated)\n\n");
    out.push_str("Generated from `crates/witchy-wir/src/canonical_safewir/manifest.rs`; the test\n`manifest_document_is_current` fails when this file drifts. Regenerate with\n`WITCHY_BLESS_RULE_MANIFEST=1 cargo test -p witchy-wir --lib --features native manifest`.\n\n");
    out.push_str("Evidence files: check = `");
    out.push_str(Check.path());
    out.push_str("`, memory = `");
    out.push_str(Memory.path());
    out.push_str("`, interpreter = `");
    out.push_str(Interpreter.path());
    out.push_str("`, backend = `");
    out.push_str(Backend.path());
    out.push_str("`, launch = `");
    out.push_str(Launch.path());
    out.push_str("`, authority = `");
    out.push_str(Authority.path());
    out.push_str("`, owner checker = `");
    out.push_str(OwnerCheck.path());
    out.push_str("`, owner backend = `");
    out.push_str(OwnerBackend.path());
    out.push_str("`, owner launch = `");
    out.push_str(OwnerLaunch.path());
    out.push_str("`, owner runtime = `");
    out.push_str(OwnerRuntime.path());
    out.push_str("`.\n\n");
    out.push_str("Opaque-owner operation variants: ");
    out.push_str(&OWNER_OPERATION_VARIANTS.join(", "));
    out.push_str(".\n\n");
    out.push_str("## B1/B7: canonical rules\n\n");
    out.push_str("| Rule | Checker judgment | Predicate | Interpreter | Backend | Admitting evidence | Refusing evidence |\n|---|---|---|---|---|---|---|\n");
    for entry in RULES {
        out.push_str(&format!(
            "| `{:?}` | {} | {} | {} | {} | {} | {} |\n",
            entry.rule,
            entry.judgment,
            entry.predicate,
            entry.interpreter,
            backend_name(entry.backend),
            cite(entry.admits),
            cite(entry.refuses),
        ));
    }
    out.push_str("\n## RFC v1 operation kernel crosswalk\n\n");
    out.push_str("| RFC kernel | Disposition | Current canonical rule(s) | Scope or reason |\n|---|---|---|---|\n");
    for entry in RFC_KERNEL_CROSSWALK {
        let rules = entry
            .disposition
            .rules()
            .iter()
            .map(|rule| format!("`{rule:?}`"))
            .collect::<Vec<_>>()
            .join(", ");
        out.push_str(&format!(
            "| `{}` | {} | {} | {} |\n",
            entry.kernel.name(),
            entry.disposition.kind(),
            if rules.is_empty() { "—".to_owned() } else { rules },
            entry.disposition.scope(),
        ));
    }
    out.push_str("\n## B2: ownership transfers of reference-counted storage\n\n");
    out.push_str("| Storage | Transfer | Verdict | Evidence |\n|---|---|---|---|\n");
    for entry in OWNERSHIP {
        out.push_str(&format!("| `{}` | {} | {} | {} |\n", entry.storage, entry.transfer, entry.verdict, cite(entry.evidence)));
    }
    out.push_str("\n## B4: supported calls\n\n");
    out.push_str("| Form | Convention | Verdict | Evidence |\n|---|---|---|---|\n");
    for entry in CALLS {
        out.push_str(&format!("| {} | {} | {} | {} |\n", entry.form, entry.convention, entry.verdict, cite(entry.evidence)));
    }
    out
}

/// Every canonical rule, in declaration order. The `match` makes a new
/// variant a compile error here until it is listed.
pub fn all_rules() -> Vec<CanonicalRuleId> {
    use CanonicalRuleId as R;
    let ordered = [
        R::ConstUnit, R::ConstInt, R::ConstFloat, R::ConstBool, R::ConstSize, R::CapabilityNone, R::CapabilitySome, R::CapabilityIsPresent, R::CapabilityUnwrap, R::Arithmetic, R::Compare, R::NarrowSize, R::WidenSize,
        R::FloatUnary, R::Guard, R::Alloc, R::StaticData, R::Load, R::InitNullableOwned, R::Store, R::ClosureSlot, R::CopyBytes, R::MoveRange,
        R::CloneRange, R::UpdateCount, R::MoveValue, R::MakeInlineAggregate, R::ExtractInlineField, R::CloneRef, R::Retain, R::Release,
        R::DropRef, R::Reclaim, R::FunctionValue, R::Call, R::VarCall, R::HostCall, R::Branch, R::BranchIf, R::Return,
        R::TailCall, R::VarTailCall, R::Trap, R::Unreachable, R::ClosureOwner,
    ];
    for rule in ordered {
        // Exhaustive by construction: adding a variant fails here.
        match rule {
            R::ConstUnit | R::ConstInt | R::ConstFloat | R::ConstBool | R::ConstSize | R::CapabilityNone | R::CapabilitySome | R::CapabilityIsPresent | R::CapabilityUnwrap | R::Arithmetic | R::Compare | R::NarrowSize
            | R::WidenSize | R::FloatUnary | R::Guard | R::Alloc | R::StaticData | R::Load | R::InitNullableOwned | R::Store | R::CopyBytes
            | R::ClosureSlot | R::MoveRange | R::CloneRange | R::UpdateCount | R::MoveValue | R::MakeInlineAggregate
            | R::ExtractInlineField | R::CloneRef | R::Retain | R::Release | R::DropRef | R::Reclaim | R::FunctionValue
            | R::Call | R::VarCall | R::HostCall | R::Branch | R::BranchIf | R::Return | R::TailCall | R::VarTailCall
            | R::Trap | R::Unreachable | R::ClosureOwner => {}
        }
    }
    ordered.to_vec()
}

/// Every RFC kernel name, in the operation order frozen by
/// `rfcs/0165-safewir-canonical-ir.md`.
pub fn all_rfc_kernels() -> Vec<RfcKernelId> {
    let ordered = [
        RfcKernelId::AllocRc,
        RfcKernelId::AllocRegion,
        RfcKernelId::CloneRef,
        RfcKernelId::MoveRef,
        RfcKernelId::DropRef,
        RfcKernelId::IsUnique,
        RfcKernelId::Project,
        RfcKernelId::Load,
        RfcKernelId::Initialize,
        RfcKernelId::Replace,
        RfcKernelId::StoreScalar,
        RfcKernelId::ByteCopy,
        RfcKernelId::MoveValue,
        RfcKernelId::CloneValue,
        RfcKernelId::MoveRange,
        RfcKernelId::CloneRange,
        RfcKernelId::UpdateCount,
        RfcKernelId::Guard,
        RfcKernelId::CheckedNarrow,
        RfcKernelId::Call,
        RfcKernelId::VarCall,
        RfcKernelId::TailCall,
        RfcKernelId::MakeClosure,
        RfcKernelId::HostCall,
        RfcKernelId::Trap,
    ];
    for kernel in ordered {
        match kernel {
            RfcKernelId::AllocRc
            | RfcKernelId::AllocRegion
            | RfcKernelId::CloneRef
            | RfcKernelId::MoveRef
            | RfcKernelId::DropRef
            | RfcKernelId::IsUnique
            | RfcKernelId::Project
            | RfcKernelId::Load
            | RfcKernelId::Initialize
            | RfcKernelId::Replace
            | RfcKernelId::StoreScalar
            | RfcKernelId::ByteCopy
            | RfcKernelId::MoveValue
            | RfcKernelId::CloneValue
            | RfcKernelId::MoveRange
            | RfcKernelId::CloneRange
            | RfcKernelId::UpdateCount
            | RfcKernelId::Guard
            | RfcKernelId::CheckedNarrow
            | RfcKernelId::Call
            | RfcKernelId::VarCall
            | RfcKernelId::TailCall
            | RfcKernelId::MakeClosure
            | RfcKernelId::HostCall
            | RfcKernelId::Trap => {}
        }
    }
    ordered.to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_lists_every_rule_exactly_once_in_order() {
        let listed = RULES.iter().map(|entry| entry.rule).collect::<Vec<_>>();
        assert_eq!(listed, all_rules(), "the rule manifest must name every canonical rule once, in declaration order");
    }

    #[test]
    fn backend_column_is_the_reviewed_mapping() {
        for entry in RULES {
            assert_eq!(
                entry.backend,
                rule_backend(entry.rule),
                "{:?}: the manifest's backend column must equal canonical_wasmir::rule_backend",
                entry.rule
            );
        }
        // Retain is the one refused rule; ClosureSlot is a subjudgment and
        // every other rule has an instruction receipt.
        let refused = RULES.iter().filter(|entry| matches!(entry.backend, RuleBackend::Refused)).map(|entry| entry.rule).collect::<Vec<_>>();
        assert_eq!(refused, vec![CanonicalRuleId::Retain]);
    }

    #[test]
    fn every_rule_cites_admitting_evidence_or_is_refused() {
        for entry in RULES {
            if !matches!(entry.backend, RuleBackend::Refused) {
                assert!(!entry.admits.is_empty(), "{:?}: an admitted rule needs admitting evidence", entry.rule);
            } else {
                assert!(!entry.refuses.is_empty(), "{:?}: a refused rule needs refusing evidence", entry.rule);
            }
        }
    }

    #[test]
    fn closure_slot_is_a_load_store_subjudgment_without_a_receipt() {
        let closure_slot = RULES.iter().find(|entry| entry.rule == CanonicalRuleId::ClosureSlot).unwrap();
        assert_eq!(
            closure_slot.backend,
            RuleBackend::Subjudgment(&[CanonicalRuleId::Load, CanonicalRuleId::Store])
        );
        for parent in [CanonicalRuleId::Load, CanonicalRuleId::Store] {
            assert!(matches!(rule_backend(parent), RuleBackend::Receipt(_)), "{parent:?} must retain its instruction receipt");
        }
        assert!(!matches!(closure_slot.backend, RuleBackend::Receipt(_)), "ClosureSlot must not create a standalone instruction receipt");
    }

    fn assert_cited_tests_exist(tests: &[TestRef], context: &str) {
        for test in tests {
            let needle = format!("fn {}(", test.name);
            assert!(
                test.file.source().contains(&needle),
                "{context}: cited test `{}` is not defined in {}",
                test.name,
                test.file.path()
            );
        }
    }

    #[test]
    fn every_cited_test_exists_in_its_file() {
        for entry in RULES {
            assert_cited_tests_exist(entry.admits, &format!("{:?} admits", entry.rule));
            assert_cited_tests_exist(entry.refuses, &format!("{:?} refuses", entry.rule));
        }
        for entry in OWNERSHIP {
            assert_cited_tests_exist(entry.evidence, &format!("ownership {} {}", entry.storage, entry.transfer));
        }
        for entry in CALLS {
            assert_cited_tests_exist(entry.evidence, &format!("call {} {}", entry.form, entry.convention));
        }
    }

    #[test]
    fn ownership_manifest_covers_every_transfer_of_every_storage_class() {
        let transfers = ["clone", "move", "replace", "drop", "reclaim"];
        for storage in ["String", "List(Int)", "List(String)"] {
            for transfer in transfers {
                assert!(
                    OWNERSHIP.iter().any(|entry| entry.storage == storage && entry.transfer == transfer),
                    "ownership manifest lacks {storage} {transfer}"
                );
            }
        }
        for bound in ["count overflow", "count underflow"] {
            assert!(OWNERSHIP.iter().any(|entry| entry.transfer == bound), "ownership manifest lacks {bound}");
        }
    }

    #[test]
    fn manifest_document_is_current() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../rfcs/0165-rule-manifest.md");
        let expected = render_markdown();
        if std::env::var_os("WITCHY_BLESS_RULE_MANIFEST").is_some() {
            std::fs::write(&path, &expected).expect("write the rule manifest document");
        }
        let actual = std::fs::read_to_string(&path).unwrap_or_default();
        assert_eq!(
            actual, expected,
            "rfcs/0165-rule-manifest.md is stale; regenerate with WITCHY_BLESS_RULE_MANIFEST=1"
        );
    }

    #[test]
    fn rfc_kernel_crosswalk_is_complete_ordered_and_rule_bound() {
        let listed = RFC_KERNEL_CROSSWALK.iter().map(|entry| entry.kernel).collect::<Vec<_>>();
        assert_eq!(listed, all_rfc_kernels(), "the RFC kernel crosswalk must preserve the frozen operation order");
        // Compare with the specification itself, not only a second Rust list.
        // A new RFC operation must acquire an explicit implementation
        // disposition even when no implementation enum has changed yet.
        let rfc = include_str!("../../../../rfcs/0165-safewir-canonical-ir.md");
        let section = rfc.split_once("## V1 operation kernel\n")
            .expect("RFC v1 kernel heading").1;
        let vocabulary = section.split_once("```text\n")
            .expect("RFC v1 kernel vocabulary").1.split_once("```")
            .expect("RFC v1 kernel closing fence").0;
        let specified = vocabulary.split(|character: char| character == ',' || character.is_whitespace())
            .filter(|name| !name.is_empty()).collect::<Vec<_>>();
        let implemented = listed.iter().map(|kernel| kernel.name()).collect::<Vec<_>>();
        assert_eq!(implemented, specified, "every RFC operation needs exactly one ordered crosswalk disposition");
        let rendered = render_markdown();
        let crosswalk = rendered
            .split_once("## RFC v1 operation kernel crosswalk\n\n")
            .and_then(|(_, rest)| rest.split_once("\n## B2:"))
            .map(|(section, _)| section)
            .expect("rendered RFC kernel crosswalk section");
        for (index, kernel) in listed.iter().enumerate() {
            assert_eq!(listed.iter().filter(|candidate| *candidate == kernel).count(), 1, "RFC kernel {:?} appears more than once", kernel);
            let marker = format!("| `{}` |", kernel.name());
            assert_eq!(crosswalk.lines().filter(|line| line.starts_with(&marker)).count(), 1, "RFC kernel {} must render exactly once", kernel.name());
            assert_eq!(RFC_KERNEL_CROSSWALK[index].kernel, *kernel);
        }
        for entry in RFC_KERNEL_CROSSWALK {
            for rule in entry.disposition.rules() {
                assert!(RULES.iter().any(|candidate| candidate.rule == *rule), "RFC kernel {} references missing rule {:?}", entry.kernel.name(), rule);
            }
        }
        let closure = RFC_KERNEL_CROSSWALK.iter().find(|entry| entry.kernel == RfcKernelId::MakeClosure).unwrap();
        assert!(matches!(closure.disposition, RfcKernelDisposition::Restricted { .. }));
        assert!(closure.disposition.scope().contains("checked scalar and supported managed-reference captures"));
    }
}
