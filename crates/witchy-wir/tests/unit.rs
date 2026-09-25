//! witchy-wir's in-crate checks. The lib is `test = false`, so it is compiled
//! once rather than again as a cfg(test) copy. Checks that need private
//! internals live in `#[doc(hidden)] pub mod self_checks` modules (behind the
//! `raw-encoder-test-api` feature every test build enables) as panicking
//! functions, each driven here by a one-line wrapper. Module paths mirror the
//! old in-crate test paths, so existing name filters keep selecting them.

mod canonical_interpreter {
    mod build_read {
        mod tests {
            #[test] fn build_env_oracle_keeps_denied_unset_empty_and_private_sessions_distinct() { witchy_wir::self_checks::canonical_interpreter_build_read::build_env_oracle_keeps_denied_unset_empty_and_private_sessions_distinct() }
        }
    }
    mod capability_tests {
        #[test] fn a_token_of_another_kind_cannot_serve_a_dir_operation() { witchy_wir::self_checks::canonical_interpreter_capability::a_token_of_another_kind_cannot_serve_a_dir_operation() }
        #[test] fn entry_tokens_are_root_scoped() { witchy_wir::self_checks::canonical_interpreter_capability::entry_tokens_are_root_scoped() }
        #[test] fn root_fallback_refuses_symlinks_and_non_files() { witchy_wir::self_checks::canonical_interpreter_capability::root_fallback_refuses_symlinks_and_non_files() }
    }
    mod crypto_rsa_tests {
        #[test] fn crypto_rsa_oracle_known_answer_and_tampered_inputs() { witchy_wir::self_checks::canonical_interpreter_crypto_rsa::crypto_rsa_oracle_known_answer_and_tampered_inputs() }
    }
    mod registry_host {
        mod tests {
            #[test] fn registry_oracle_dir_write_refuses_explicit_directories_and_granted_root() { witchy_wir::self_checks::canonical_interpreter_registry_host::registry_oracle_dir_write_refuses_explicit_directories_and_granted_root() }
            #[test] fn registry_oracle_launch_requires_explicit_grants_even_when_unused() { witchy_wir::self_checks::canonical_interpreter_registry_host::registry_oracle_launch_requires_explicit_grants_even_when_unused() }
            #[test] fn registry_oracle_mints_bind_kind_scope_and_session() { witchy_wir::self_checks::canonical_interpreter_registry_host::registry_oracle_mints_bind_kind_scope_and_session() }
            #[test] fn registry_oracle_mkdir_fallback_never_writes_or_follows_links() { witchy_wir::self_checks::canonical_interpreter_registry_host::registry_oracle_mkdir_fallback_never_writes_or_follows_links() }
            mod installer_tests {
                #[test] fn dir_installer_oracle_virtual_links_resolve_without_backing_and_stay_confined() { witchy_wir::self_checks::canonical_interpreter_registry_host::installer_tests::dir_installer_oracle_virtual_links_resolve_without_backing_and_stay_confined() }
            }
            mod storage_tests {
                #[test] fn dir_storage_oracle_scoped_curdir_write_and_read_stay_under_grant() { witchy_wir::self_checks::canonical_interpreter_registry_host::storage_tests::dir_storage_oracle_scoped_curdir_write_and_read_stay_under_grant() }
            }
        }
    }
    mod tests {
        #[test] fn reference_counts_trap_at_i32_maximum() { witchy_wir::self_checks::canonical_interpreter::reference_counts_trap_at_i32_maximum() }
        #[test] fn signed_remainder_by_zero_traps() { witchy_wir::self_checks::canonical_interpreter::signed_remainder_by_zero_traps() }
        #[test] fn validated_clone_and_two_drops_execute() { witchy_wir::self_checks::canonical_interpreter::validated_clone_and_two_drops_execute() }
        #[test] fn validated_counted_element_store_and_load_execute() { witchy_wir::self_checks::canonical_interpreter::validated_counted_element_store_and_load_execute() }
        #[test] fn validated_direct_call_executes_callee() { witchy_wir::self_checks::canonical_interpreter::validated_direct_call_executes_callee() }
        #[test] fn validated_false_guard_traps() { witchy_wir::self_checks::canonical_interpreter::validated_false_guard_traps() }
        #[test] fn validated_scalar_arithmetic_executes() { witchy_wir::self_checks::canonical_interpreter::validated_scalar_arithmetic_executes() }
        #[test] fn validated_size_values_reject_values_wider_than_i32() { witchy_wir::self_checks::canonical_interpreter::validated_size_values_reject_values_wider_than_i32() }
    }
}
mod canonical_safewir {
    mod authority {
        mod tests {
            #[test] fn adapter_binding_rejects_a_body_that_does_extra_work() { witchy_wir::self_checks::canonical_safewir_authority::adapter_binding_rejects_a_body_that_does_extra_work() }
            #[test] fn final_authority_requires_the_exact_initial_checked_module() { witchy_wir::self_checks::canonical_safewir_authority::final_authority_requires_the_exact_initial_checked_module() }
            #[test] fn initial_checker_rejects_an_independent_tail_manifest_mismatch() { witchy_wir::self_checks::canonical_safewir_authority::initial_checker_rejects_an_independent_tail_manifest_mismatch() }
            #[test] fn optimized_authority_refuses_a_mutated_output_module() { witchy_wir::self_checks::canonical_safewir_authority::optimized_authority_refuses_a_mutated_output_module() }
            #[test] fn source_generic_layout_graph_checks_instantiated_payload_types() { witchy_wir::self_checks::canonical_safewir_authority::source_generic_layout_graph_checks_instantiated_payload_types() }
            #[test] fn source_recursive_layout_graph_checks_every_pair_and_child() { witchy_wir::self_checks::canonical_safewir_authority::source_recursive_layout_graph_checks_every_pair_and_child() }
        }
    }
    mod build_read {
        mod tests {
            #[test] fn build_env_catalog_authenticates_root_and_keeps_presence_internal() { witchy_wir::self_checks::canonical_safewir_build_read::build_env_catalog_authenticates_root_and_keeps_presence_internal() }
            #[test] fn build_read_catalog_separates_root_authority_from_bounded_responses() { witchy_wir::self_checks::canonical_safewir_build_read::build_read_catalog_separates_root_authority_from_bounded_responses() }
        }
    }
    mod catalog_tests {
        #[test] fn compiler_imports_are_authenticated_internal_buffer_services() { witchy_wir::self_checks::canonical_safewir_catalog::compiler_imports_are_authenticated_internal_buffer_services() }
    }
    mod check {
        mod cache {
            mod store {
                mod tests {
                    #[test] fn verdict_files_round_trip_and_reject_damage() { witchy_wir::self_checks::canonical_safewir_check_cache::verdict_files_round_trip_and_reject_damage() }
                }
            }
        }
        mod contracts {
            mod tests {
                #[test] fn indirect_writeback_summary_requires_owned_results_without_provenance_claims() { witchy_wir::self_checks::canonical_safewir_check_contracts::indirect_writeback_summary_requires_owned_results_without_provenance_claims() }
            }
        }
        mod host {
            mod tests {
                #[test] fn forged_import_contract_is_rejected_by_catalog() { witchy_wir::self_checks::canonical_safewir_check_host::forged_import_contract_is_rejected_by_catalog() }
                #[test] fn integer_cannot_impersonate_a_capability_argument() { witchy_wir::self_checks::canonical_safewir_check_host::integer_cannot_impersonate_a_capability_argument() }
                #[test] fn linear_memory_reference_cannot_impersonate_a_capability_argument() { witchy_wir::self_checks::canonical_safewir_check_host::linear_memory_reference_cannot_impersonate_a_capability_argument() }
                #[test] fn unknown_import_id_is_rejected() { witchy_wir::self_checks::canonical_safewir_check_host::unknown_import_id_is_rejected() }
            }
        }
        mod memory {
            mod tests {
                #[test] fn affine_boundary_rejects_duplicate_owned_return_lanes() { witchy_wir::self_checks::canonical_safewir_check_memory::affine_boundary_rejects_duplicate_owned_return_lanes() }
                #[test] fn affine_boundary_tail_call_requires_no_untransferred_tokens() { witchy_wir::self_checks::canonical_safewir_check_memory::affine_boundary_tail_call_requires_no_untransferred_tokens() }
                #[test] fn bounds_guards_name_the_current_owner_generation() { witchy_wir::self_checks::canonical_safewir_check_memory::bounds_guards_name_the_current_owner_generation() }
                #[test] fn byte_copy_requires_a_dominating_dynamic_end_guard() { witchy_wir::self_checks::canonical_safewir_check_memory::byte_copy_requires_a_dominating_dynamic_end_guard() }
                #[test] fn clone_ref_tracks_two_affine_shared_tokens() { witchy_wir::self_checks::canonical_safewir_check_memory::clone_ref_tracks_two_affine_shared_tokens() }
                #[test] fn complete_managed_abi_subrange_admits_bounded_clone() { witchy_wir::self_checks::canonical_safewir_check_memory::complete_managed_abi_subrange_admits_bounded_clone() }
                #[test] fn constant_prefix_publication_rejects_gaps_and_missing_children() { witchy_wir::self_checks::canonical_safewir_check_memory::constant_prefix_publication_rejects_gaps_and_missing_children() }
                #[test] fn dynamic_pointer_clone_range_shares_initialized_elements() { witchy_wir::self_checks::canonical_safewir_check_memory::dynamic_pointer_clone_range_shares_initialized_elements() }
                #[test] fn dynamic_pointer_move_range_transfers_initialized_ownership_once() { witchy_wir::self_checks::canonical_safewir_check_memory::dynamic_pointer_move_range_transfers_initialized_ownership_once() }
                #[test] fn dynamic_pointer_prefix_cannot_hide_an_unaccounted_child_slot() { witchy_wir::self_checks::canonical_safewir_check_memory::dynamic_pointer_prefix_cannot_hide_an_unaccounted_child_slot() }
                #[test] fn move_value_rejects_shared_source() { witchy_wir::self_checks::canonical_safewir_check_memory::move_value_rejects_shared_source() }
                #[test] fn nullable_owned_checker_requires_initialization_and_checked_presence() { witchy_wir::self_checks::canonical_safewir_check_memory::nullable_owned_checker_requires_initialization_and_checked_presence() }
                #[test] fn owned_prefix_summary_is_invalidated_by_move() { witchy_wir::self_checks::canonical_safewir_check_memory::owned_prefix_summary_is_invalidated_by_move() }
                #[test] fn pointer_bearing_ranges_fail_closed_without_element_transfer_state() { witchy_wir::self_checks::canonical_safewir_check_memory::pointer_bearing_ranges_fail_closed_without_element_transfer_state() }
                #[test] fn pointer_bearing_reclaim_requires_recursive_drop() { witchy_wir::self_checks::canonical_safewir_check_memory::pointer_bearing_reclaim_requires_recursive_drop() }
                #[test] fn pointer_clone_range_rejects_partially_initialized_source() { witchy_wir::self_checks::canonical_safewir_check_memory::pointer_clone_range_rejects_partially_initialized_source() }
                #[test] fn pointer_move_range_rejects_source_reuse_after_consumption() { witchy_wir::self_checks::canonical_safewir_check_memory::pointer_move_range_rejects_source_reuse_after_consumption() }
                #[test] fn range_transfer_rejects_a_slice_past_logical_length() { witchy_wir::self_checks::canonical_safewir_check_memory::range_transfer_rejects_a_slice_past_logical_length() }
                #[test] fn reauthenticate_raw_header_extent_requires_current_fresh_proofs_and_rebinds_conservatively() { witchy_wir::self_checks::canonical_safewir_check_memory::reauthenticate_raw_header_extent_requires_current_fresh_proofs_and_rebinds_conservatively() }
                #[test] fn recursive_drop_layout_graph_requires_bound_supported_children() { witchy_wir::self_checks::canonical_safewir_check_memory::recursive_drop_layout_graph_requires_bound_supported_children() }
                #[test] fn release_rejects_shared_value_without_refcount_authority() { witchy_wir::self_checks::canonical_safewir_check_memory::release_rejects_shared_value_without_refcount_authority() }
                #[test] fn retain_is_refused_in_v1() { witchy_wir::self_checks::canonical_safewir_check_memory::retain_is_refused_in_v1() }
                #[test] fn return_contract_accepts_only_sole_unique_fresh_object() { witchy_wir::self_checks::canonical_safewir_check_memory::return_contract_accepts_only_sole_unique_fresh_object() }
                #[test] fn return_contract_rejects_fresh_allocation_class_mismatch() { witchy_wir::self_checks::canonical_safewir_check_memory::return_contract_rejects_fresh_allocation_class_mismatch() }
                #[test] fn same_identity_ownership_meet_rejects_identity_consumption_and_alias_changes() { witchy_wir::self_checks::canonical_safewir_check_memory::same_identity_ownership_meet_rejects_identity_consumption_and_alias_changes() }
                #[test] fn scalar_clone_range_is_ownership_safe_and_admitted() { witchy_wir::self_checks::canonical_safewir_check_memory::scalar_clone_range_is_ownership_safe_and_admitted() }
                #[test] fn scalar_move_range_is_bounds_checked_and_admitted() { witchy_wir::self_checks::canonical_safewir_check_memory::scalar_move_range_is_bounds_checked_and_admitted() }
                #[test] fn shared_static_storage_clones_and_drops_without_mutation_authority() { witchy_wir::self_checks::canonical_safewir_check_memory::shared_static_storage_clones_and_drops_without_mutation_authority() }
                #[test] fn store_cannot_overwrite_a_stored_reference() { witchy_wir::self_checks::canonical_safewir_check_memory::store_cannot_overwrite_a_stored_reference() }
            }
        }
        mod owners {
            mod tests {
                #[test] fn opaque_owner_affine_call_requires_move_and_no_leak() { witchy_wir::self_checks::canonical_safewir_check_owners::opaque_owner_affine_call_requires_move_and_no_leak() }
                #[test] fn opaque_owner_affine_dead_list_and_wrong_family_refuse() { witchy_wir::self_checks::canonical_safewir_check_owners::opaque_owner_affine_dead_list_and_wrong_family_refuse() }
            }
        }
        mod tests {
            #[test] fn block_arguments_are_typed_ssa_definitions() { witchy_wir::self_checks::canonical_safewir_check::block_arguments_are_typed_ssa_definitions() }
            #[test] fn bounds_guard_requires_unsigned_size_operands() { witchy_wir::self_checks::canonical_safewir_check::bounds_guard_requires_unsigned_size_operands() }
            #[test] fn bounds_guard_without_a_comparison_is_refused() { witchy_wir::self_checks::canonical_safewir_check::bounds_guard_without_a_comparison_is_refused() }
            #[test] fn call_move_consumes_unique_and_later_use_is_rejected() { witchy_wir::self_checks::canonical_safewir_check::call_move_consumes_unique_and_later_use_is_rejected() }
            #[test] fn candidate_abi_bytes_do_not_authenticate_an_import() { witchy_wir::self_checks::canonical_safewir_check::candidate_abi_bytes_do_not_authenticate_an_import() }
            #[test] fn checked_value_memory_result_is_refused() { witchy_wir::self_checks::canonical_safewir_check::checked_value_memory_result_is_refused() }
            #[test] fn closure_record_stores_loads_and_calls_its_callable_slot() { witchy_wir::self_checks::canonical_safewir_check::closure_record_stores_loads_and_calls_its_callable_slot() }
            #[test] fn closure_slot_load_rejects_a_different_signature() { witchy_wir::self_checks::canonical_safewir_check::closure_slot_load_rejects_a_different_signature() }
            #[test] fn declared_callable_result_must_match_checked_signature() { witchy_wir::self_checks::canonical_safewir_check::declared_callable_result_must_match_checked_signature() }
            #[test] fn definition_on_one_branch_does_not_dominate_join() { witchy_wir::self_checks::canonical_safewir_check::definition_on_one_branch_does_not_dominate_join() }
            #[test] fn duplicate_tail_terminators_cannot_discharge_one_obligation() { witchy_wir::self_checks::canonical_safewir_check::duplicate_tail_terminators_cannot_discharge_one_obligation() }
            #[test] fn dynamic_element_load_without_guard_is_refused() { witchy_wir::self_checks::canonical_safewir_check::dynamic_element_load_without_guard_is_refused() }
            #[test] fn external_call_accepts_unit_parameter_and_result_contracts() { witchy_wir::self_checks::canonical_safewir_check::external_call_accepts_unit_parameter_and_result_contracts() }
            #[test] fn external_call_rejects_reference_gc_capability_and_callable_abi() { witchy_wir::self_checks::canonical_safewir_check::external_call_rejects_reference_gc_capability_and_callable_abi() }
            #[test] fn function_value_authenticates_checked_target_signature() { witchy_wir::self_checks::canonical_safewir_check::function_value_authenticates_checked_target_signature() }
            #[test] fn function_value_rejects_destination_signature_mismatch() { witchy_wir::self_checks::canonical_safewir_check::function_value_rejects_destination_signature_mismatch() }
            #[test] fn guarded_dynamic_element_load_is_admitted() { witchy_wir::self_checks::canonical_safewir_check::guarded_dynamic_element_load_is_admitted() }
            #[test] fn host_mutation_must_match_explicit_buffer_access() { witchy_wir::self_checks::canonical_safewir_check::host_mutation_must_match_explicit_buffer_access() }
            #[test] fn host_slice_outside_logical_length_is_rejected() { witchy_wir::self_checks::canonical_safewir_check::host_slice_outside_logical_length_is_rejected() }
            #[test] fn indirect_capability_arguments_require_exact_effect_free_value_contracts() { witchy_wir::self_checks::canonical_safewir_check::indirect_capability_arguments_require_exact_effect_free_value_contracts() }
            #[test] fn indirect_reference_value_argument_is_refused() { witchy_wir::self_checks::canonical_safewir_check::indirect_reference_value_argument_is_refused() }
            #[test] fn integer_cannot_be_wrapped_as_capability_host_argument() { witchy_wir::self_checks::canonical_safewir_check::integer_cannot_be_wrapped_as_capability_host_argument() }
            #[test] fn logical_length_guard_is_not_satisfied_by_capacity_guard() { witchy_wir::self_checks::canonical_safewir_check::logical_length_guard_is_not_satisfied_by_capacity_guard() }
            #[test] fn memory_value_call_is_fail_closed() { witchy_wir::self_checks::canonical_safewir_check::memory_value_call_is_fail_closed() }
            #[test] fn nullable_capability_operations_are_checked() { witchy_wir::self_checks::canonical_safewir_check::nullable_capability_operations_are_checked() }
            #[test] fn nullable_owned_slot_is_refused_at_the_initial_catalog_gate() { witchy_wir::self_checks::canonical_safewir_check::nullable_owned_slot_is_refused_at_the_initial_catalog_gate() }
            #[test] fn owned_parameter_must_be_discharged_at_exit() { witchy_wir::self_checks::canonical_safewir_check::owned_parameter_must_be_discharged_at_exit() }
            #[test] fn pointer_or_typed_element_buffers_cannot_be_exposed_as_host_bytes() { witchy_wir::self_checks::canonical_safewir_check::pointer_or_typed_element_buffers_cannot_be_exposed_as_host_bytes() }
            #[test] fn reachable_unreachable_terminator_is_refused() { witchy_wir::self_checks::canonical_safewir_check::reachable_unreachable_terminator_is_refused() }
            #[test] fn reclaim_without_unique_object_authority_fails_closed() { witchy_wir::self_checks::canonical_safewir_check::reclaim_without_unique_object_authority_fails_closed() }
            #[test] fn scalar_checker_refuses_non_total_backend_combinations() { witchy_wir::self_checks::canonical_safewir_check::scalar_checker_refuses_non_total_backend_combinations() }
            #[test] fn shared_move_into_unique_parameter_is_rejected() { witchy_wir::self_checks::canonical_safewir_check::shared_move_into_unique_parameter_is_rejected() }
            #[test] fn shared_ownership_cannot_be_reclaimed_as_unique() { witchy_wir::self_checks::canonical_safewir_check::shared_ownership_cannot_be_reclaimed_as_unique() }
            #[test] fn tail_call_after_release_is_rejected() { witchy_wir::self_checks::canonical_safewir_check::tail_call_after_release_is_rejected() }
            #[test] fn uninitialized_scalar_load_is_refused() { witchy_wir::self_checks::canonical_safewir_check::uninitialized_scalar_load_is_refused() }
            #[test] fn unsupported_host_mutation_and_reentrancy_fail_closed() { witchy_wir::self_checks::canonical_safewir_check::unsupported_host_mutation_and_reentrancy_fail_closed() }
            #[test] fn update_count_grows_logical_length_only_after_capacity_guard() { witchy_wir::self_checks::canonical_safewir_check::update_count_grows_logical_length_only_after_capacity_guard() }
            #[test] fn valid_unreachable_block_is_available_to_pruning() { witchy_wir::self_checks::canonical_safewir_check::valid_unreachable_block_is_available_to_pruning() }
            #[test] fn write_buffer_host_call_refuses_a_read_only_contract() { witchy_wir::self_checks::canonical_safewir_check::write_buffer_host_call_refuses_a_read_only_contract() }
            #[test] fn write_buffer_host_call_refuses_a_shared_object() { witchy_wir::self_checks::canonical_safewir_check::write_buffer_host_call_refuses_a_shared_object() }
            #[test] fn wrong_capability_kind_is_rejected() { witchy_wir::self_checks::canonical_safewir_check::wrong_capability_kind_is_rejected() }
        }
    }
    mod manifest {
        mod tests {
            #[test] fn backend_column_is_the_reviewed_mapping() { witchy_wir::self_checks::canonical_safewir_manifest::backend_column_is_the_reviewed_mapping() }
            #[test] fn every_cited_test_exists_in_its_file() { witchy_wir::self_checks::canonical_safewir_manifest::every_cited_test_exists_in_its_file() }
            #[test] fn every_rule_cites_admitting_evidence_or_is_refused() { witchy_wir::self_checks::canonical_safewir_manifest::every_rule_cites_admitting_evidence_or_is_refused() }
            #[test] fn manifest_document_is_current() { witchy_wir::self_checks::canonical_safewir_manifest::manifest_document_is_current() }
        }
    }
}
mod canonical_wasmir {
    mod closure_owner_kernel {
        mod tests {
            #[test] fn opaque_owner_cleanup_reclaims_once_and_rejects_unknown_family() { witchy_wir::self_checks::canonical_wasmir_closure_owner_kernel::opaque_owner_cleanup_reclaims_once_and_rejects_unknown_family().unwrap() }
        }
    }
    mod drop_worklist {
        mod tests {
            #[test] fn drop_worklist_releases_25000_nodes_without_call_stack_growth() { witchy_wir::self_checks::canonical_wasmir_drop_worklist::drop_worklist_releases_25000_nodes_without_call_stack_growth() }
        }
    }
    mod tests {
        #[test] fn admitted_scalar_var_tail_call_executes_with_return_call_indirect() { witchy_wir::self_checks::canonical_wasmir::admitted_scalar_var_tail_call_executes_with_return_call_indirect() }
        #[test] fn allocation_past_linear_memory_grows_or_traps_when_growth_is_refused() { witchy_wir::self_checks::canonical_wasmir::allocation_past_linear_memory_grows_or_traps_when_growth_is_refused() }
        #[test] fn bump_allocator_class_is_refused_by_initial_check() { witchy_wir::self_checks::canonical_wasmir::bump_allocator_class_is_refused_by_initial_check() }
        #[test] fn capability_indirect_contract_forgery_fails_initial_admission() { witchy_wir::self_checks::canonical_wasmir::capability_indirect_contract_forgery_fails_initial_admission() }
        #[test] fn comparison_encodes_and_validates() { witchy_wir::self_checks::canonical_wasmir::comparison_encodes_and_validates() }
        #[test] fn consecutive_rc_allocations_reserve_non_overlapping_prefixes() { witchy_wir::self_checks::canonical_wasmir::consecutive_rc_allocations_reserve_non_overlapping_prefixes() }
        #[test] fn encoded_scalar_module_executes() { witchy_wir::self_checks::canonical_wasmir::encoded_scalar_module_executes() }
        #[test] fn encoded_structured_branch_executes_both_arms() { witchy_wir::self_checks::canonical_wasmir::encoded_structured_branch_executes_both_arms() }
        #[test] fn encoded_structured_loop_executes_carried_arguments() { witchy_wir::self_checks::canonical_wasmir::encoded_structured_loop_executes_carried_arguments() }
        #[test] fn foreign_entry_section_fails_admission() { witchy_wir::self_checks::canonical_wasmir::foreign_entry_section_fails_admission() }
        #[test] fn full_u32_size_constant_encodes_and_validates() { witchy_wir::self_checks::canonical_wasmir::full_u32_size_constant_encodes_and_validates() }
        #[test] fn irreducible_cfg_is_refused_without_dispatcher_lowering() { witchy_wir::self_checks::canonical_wasmir::irreducible_cfg_is_refused_without_dispatcher_lowering() }
        #[test] fn malformed_clone_range_places_are_refused_by_canonical_backend() { witchy_wir::self_checks::canonical_wasmir::malformed_clone_range_places_are_refused_by_canonical_backend() }
        #[test] fn narrow_size_u32_overflow_traps_in_wasmtime() { witchy_wir::self_checks::canonical_wasmir::narrow_size_u32_overflow_traps_in_wasmtime() }
        #[test] fn narrow_size_valid_value_executes() { witchy_wir::self_checks::canonical_wasmir::narrow_size_valid_value_executes() }
        #[test] fn packed_scalar_collection_oob_guard_traps_in_wasmtime() { witchy_wir::self_checks::canonical_wasmir::packed_scalar_collection_oob_guard_traps_in_wasmtime() }
        #[test] fn pointer_clone_range_retains_executes_and_recursively_releases() { witchy_wir::self_checks::canonical_wasmir::pointer_clone_range_retains_executes_and_recursively_releases() }
        #[test] fn release_at_zero_refcount_traps_before_decrement() { witchy_wir::self_checks::canonical_wasmir::release_at_zero_refcount_traps_before_decrement() }
        #[test] fn scalar_clone_range_count_stride_overflow_traps_before_copy() { witchy_wir::self_checks::canonical_wasmir::scalar_clone_range_count_stride_overflow_traps_before_copy() }
        #[test] fn share_at_max_refcount_traps_before_destination_or_refcount_write() { witchy_wir::self_checks::canonical_wasmir::share_at_max_refcount_traps_before_destination_or_refcount_write() }
        #[test] fn size_add_overflow_traps_or_wraps_exactly_as_declared() { witchy_wir::self_checks::canonical_wasmir::size_add_overflow_traps_or_wraps_exactly_as_declared() }
        #[test] fn variable_call_is_refused_fail_closed() { witchy_wir::self_checks::canonical_wasmir::variable_call_is_refused_fail_closed() }
        mod nullable_admission {
            #[test] fn nullable_admission_rejects_valid_but_mutated_bytes() { witchy_wir::self_checks::canonical_wasmir::nullable_admission::nullable_admission_rejects_valid_but_mutated_bytes() }
        }
        mod unique_pop {
            #[test] fn unique_pop_checker_refuses_shared_container_missing_bounds_and_non_tail() { witchy_wir::self_checks::canonical_wasmir::unique_pop::unique_pop_checker_refuses_shared_container_missing_bounds_and_non_tail() }
            #[test] fn unique_pop_rejects_live_prefix_overwrite_direct_moved_and_unknown() { witchy_wir::self_checks::canonical_wasmir::unique_pop::unique_pop_rejects_live_prefix_overwrite_direct_moved_and_unknown() }
        }
        mod unit_storage {
            #[test] fn unit_storage_construct_call_native_oracle() { witchy_wir::self_checks::canonical_wasmir::unit_storage::unit_storage_construct_call_native_oracle() }
            #[test] fn unit_storage_rejects_type_layout_initialization_and_bounds() { witchy_wir::self_checks::canonical_wasmir::unit_storage::unit_storage_rejects_type_layout_initialization_and_bounds() }
        }
    }
}
mod rfc0162_mutation_evidence_tests {
    #[test] fn callable_checker_recomputes_parameter_effects() { witchy_wir::rfc0162_mutation_evidence_tests::callable_checker_recomputes_parameter_effects() }
}
mod safewir_call_tests {
    #[test] fn imported_and_host_calls_require_catalog_authentication() { witchy_wir::safewir_call_tests::imported_and_host_calls_require_catalog_authentication() }
}
mod safewir_validate {
    mod tests {
        #[test] fn block_exit_accumulator_never_resurrects_consumed_authority() { witchy_wir::self_checks::safewir_validate::block_exit_accumulator_never_resurrects_consumed_authority() }
        #[test] fn consuming_one_alias_invalidates_the_entire_group() { witchy_wir::self_checks::safewir_validate::consuming_one_alias_invalidates_the_entire_group() }
        #[test] fn ordinary_call_cannot_reach_a_host_import() { witchy_wir::self_checks::safewir_validate::ordinary_call_cannot_reach_a_host_import() }
        #[test] fn report_mode_rule_diagnostics_have_forged_fixtures() { witchy_wir::self_checks::safewir_validate::report_mode_rule_diagnostics_have_forged_fixtures() }
        #[test] fn reuse_extent_is_rejected_at_the_reuse_operation() { witchy_wir::self_checks::safewir_validate::reuse_extent_is_rejected_at_the_reuse_operation() }
        #[test] fn scalar_memory_width_and_representation_are_not_interchangeable() { witchy_wir::self_checks::safewir_validate::scalar_memory_width_and_representation_are_not_interchangeable() }
    }
}
