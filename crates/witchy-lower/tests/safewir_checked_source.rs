//! SafeWIR tests that need a real checked source (RFC-0170 P4).
//!
//! witchy-wir cannot depend on witchy-types, not even as a dev-dependency:
//! Cargo applies one to every test target, so each checker edit would rebuild
//! wir's test binaries. The tests below used to live in witchy-wir under the
//! same names. The authority, owner-authority and closure-contract cases judge
//! the checker's product with wir-private predicates, so their bodies stay in
//! wir behind the dev-only `checked-source-test-api` feature and this file
//! supplies the real frontend. The last two modules use only public wir API
//! and moved here whole.

use witchy_wir::canonical_safewir::checked_source_test_api::{
    self as cases, CheckedFixture, SourceChecker,
};

/// The checked frontend, exactly as the tests in witchy-wir used to call it.
struct Frontend;

impl SourceChecker for Frontend {
    fn with_checked(&self, entry: &str, source: &[u8], body: &mut dyn FnMut(CheckedFixture<'_>)) {
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![(entry.to_string(), source.to_vec())],
            entry,
            witchy_syntax::linker::ComptimeExpander::new(|_, _, _| Ok(witchy_syntax::origin::OriginTable::default())),
        )
        .expect("checked source");
        let source = checked.canonical_source().expect("canonical source");
        body(CheckedFixture {
            view: source.view(),
            typed_module: source.typed_module().module(),
            closure_witness: source.closure_witness(),
        });
    }
}
/// Formerly `witchy-wir` `canonical_safewir::authority::tests`.
mod authority {
    use super::*;

    #[test]
    fn checked_source_abi_binding_accepts_the_actual_matching_candidate() {
        cases::authority::checked_source_abi_binding_accepts_the_actual_matching_candidate(&Frontend);
    }

    #[test]
    fn source_own_scalar_rejects_a_unique_candidate_convention() {
        cases::authority::source_own_scalar_rejects_a_unique_candidate_convention(&Frontend);
    }

    #[test]
    fn unique_pop_source_abi_requires_checked_unique_input_and_output() {
        cases::authority::unique_pop_source_abi_requires_checked_unique_input_and_output(&Frontend);
    }

    #[test]
    fn source_closure_family_normalizes_nominals_and_default_let_not_own() {
        cases::authority::source_closure_family_normalizes_nominals_and_default_let_not_own(&Frontend);
    }

    #[test]
    fn option_rooted_int_view_requires_checked_payload_state() {
        cases::authority::option_rooted_int_view_requires_checked_payload_state(&Frontend);
    }

    #[test]
    fn checked_source_abi_binding_rejects_bool_substitution() {
        cases::authority::checked_source_abi_binding_rejects_bool_substitution(&Frontend);
    }

    #[test]
    fn checked_source_abi_binding_rejects_an_extra_result_lane() {
        cases::authority::checked_source_abi_binding_rejects_an_extra_result_lane(&Frontend);
    }

    #[test]
    fn source_dir_and_env_match_their_authenticated_capability_kinds() {
        cases::authority::source_dir_and_env_match_their_authenticated_capability_kinds(&Frontend);
    }

    #[test]
    fn source_console_requires_the_authenticated_capability_kind() {
        cases::authority::source_console_requires_the_authenticated_capability_kind(&Frontend);
    }

    #[test]
    fn source_higher_order_callable_matches_the_actual_nested_signature() {
        cases::authority::source_higher_order_callable_matches_the_actual_nested_signature(&Frontend);
    }

    #[test]
    fn aggregate_callable_field_rejects_a_registered_wrong_signature() {
        cases::authority::aggregate_callable_field_rejects_a_registered_wrong_signature(&Frontend);
    }

    #[test]
    fn source_higher_order_unit_callable_requires_zero_result_lanes() {
        cases::authority::source_higher_order_unit_callable_requires_zero_result_lanes(&Frontend);
    }

    #[test]
    fn checked_source_abi_binding_rejects_missing_and_duplicate_entries() {
        cases::authority::checked_source_abi_binding_rejects_missing_and_duplicate_entries(&Frontend);
    }

    #[test]
    fn final_authority_refuses_a_mismatched_closure_witness() {
        cases::authority::final_authority_refuses_a_mismatched_closure_witness(&Frontend);
    }
}

/// Formerly `witchy-wir` `canonical_safewir::owner_authority::tests`.
mod owner_authority {
    use super::*;

    #[test]
    fn aggregate_owner_source_schema_rejects_type_and_storage_forgery() {
        cases::owner_authority::aggregate_owner_source_schema_rejects_type_and_storage_forgery(&Frontend);
    }

    #[test]
    fn exact_int_slice_record_field_rejects_erased_and_wrong_owner_carriers() {
        cases::owner_authority::exact_int_slice_record_field_rejects_erased_and_wrong_owner_carriers(&Frontend);
    }

    #[test]
    fn aggregate_tuple_owner_binds_exact_fields_and_storage() {
        cases::owner_authority::aggregate_tuple_owner_binds_exact_fields_and_storage(&Frontend);
    }

    #[test]
    fn aggregate_gc_correspondence_requires_exact_normalized_source_type() {
        cases::owner_authority::aggregate_gc_correspondence_requires_exact_normalized_source_type(&Frontend);
    }
}

/// Formerly `witchy-wir` `canonical_safewir::closure_owner_contract::tests`.
mod closure_owner_contract {
    use super::*;

    #[test]
    fn opaque_closure_contract_keeps_caps_and_managed_captures_typed() {
        cases::closure_owner_contract::opaque_closure_contract_keeps_caps_and_managed_captures_typed(&Frontend);
    }

    #[test]
    fn opaque_closure_contract_does_not_enable_pure_capability_capture() {
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".into(), b"fn main(console: Console):\n    let bad = pure fn(msg: String): console.print(msg)\n    bad(\"no\")\n".to_vec())],
            "main", witchy_syntax::linker::ComptimeExpander::new(|_, _, _| Ok(Default::default())),
        );
        assert!(checked.is_err(), "pure capture restrictions remain a source obligation");
    }
}
