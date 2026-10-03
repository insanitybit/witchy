//! witchy-runtime's tests. The crate sets `[lib] test = false`: a second
//! `cfg(test)` compile of the opt-level-2 runtime was the tail of every gate
//! test build. Each test body lives in the library behind the `self-checks`
//! feature (next to the private items it exercises); these one-line drivers
//! keep every test's former name, so `runtime::host::fetch::tests::…` filters
//! still select it (now in binary `runtime` instead of the lib).
#![cfg(feature = "self-checks")]

/// A test body returns `()` or a `Result`; an `Err` fails the test.
trait Outcome { fn check(self); }
impl Outcome for () { fn check(self) {} }
impl<E: std::fmt::Debug> Outcome for Result<(), E> {
    fn check(self) { self.unwrap() }
}

mod confine {
    #[cfg(unix)]
    mod tests {
        use crate::Outcome;
        use witchy_runtime::confine::tests as checks;
        #[test] fn a_fresh_operation_rejects_a_swapped_parent() { checks::a_fresh_operation_rejects_a_swapped_parent().check() }
        #[test] fn a_symlink_leaf_is_never_followed_for_writes() { checks::a_symlink_leaf_is_never_followed_for_writes().check() }
        #[test] fn exact_entry_identity_rejects_symlink_and_case_aliases() { checks::exact_entry_identity_rejects_symlink_and_case_aliases().check() }
        #[test] fn lexical_escape_is_rejected() { checks::lexical_escape_is_rejected().check() }
        #[test] fn retained_file_and_subdir_authority_survive_name_replacement() { checks::retained_file_and_subdir_authority_survive_name_replacement().check() }
    }
}
mod fetch {
    mod tests {
        use crate::Outcome;
        use witchy_runtime::fetch::tests as checks;
        #[test] fn fetch_chunk_size_overflow_is_a_malformed_response_not_a_panic() { checks::fetch_chunk_size_overflow_is_a_malformed_response_not_a_panic().check() }
        #[test] fn narrowing_is_intersective_and_denial_precedes_io() { checks::narrowing_is_intersective_and_denial_precedes_io().check() }
        #[test] fn net_derivation_is_bounded_and_preserves_the_network_floor() { checks::net_derivation_is_bounded_and_preserves_the_network_floor().check() }
        #[test] fn origins_are_canonical_and_credentials_are_rejected() { checks::origins_are_canonical_and_credentials_are_rejected().check() }
        #[test] fn redirects_are_uniform_errors_and_never_followed() { checks::redirects_are_uniform_errors_and_never_followed().check() }
        #[test] fn request_injection_and_oversized_responses_fail_closed() { checks::request_injection_and_oversized_responses_fail_closed().check() }
        #[test] fn resolve_failure_of_an_admitted_origin_is_network_not_denial() { checks::resolve_failure_of_an_admitted_origin_is_network_not_denial().check() }
    }
}
mod net {
    #[cfg(not(target_arch = "wasm32"))]
    mod tests {
        use crate::Outcome;
        use witchy_runtime::net::tests as checks;
        #[test] fn listener_binds_only_the_concrete_identity_admitted_by_policy() { checks::listener_binds_only_the_concrete_identity_admitted_by_policy().check() }
    }
}
mod runtime {
    mod compiler {
        mod doc_tests {
            use crate::Outcome;
            use witchy_runtime::runtime::self_checks::compiler_doc_tests as checks;
            #[test] fn bounded_compiler_doc_preflights_every_range_before_provider() { checks::bounded_compiler_doc_preflights_every_range_before_provider().check() }
            #[test] fn bounded_compiler_doc_rejects_invalid_utf8_without_provider_or_writes() { checks::bounded_compiler_doc_rejects_invalid_utf8_without_provider_or_writes().check() }
        }
        mod tests {
            use crate::Outcome;
            use witchy_runtime::runtime::self_checks::compiler_tests as checks;
            #[test] fn bounded_compiler_imports_execute_utf8_bounds_and_no_partial_write() { checks::bounded_compiler_imports_execute_utf8_bounds_and_no_partial_write().check() }
            #[test] fn bounded_compiler_result_requires_full_capacity_and_valid_utf8_input() { checks::bounded_compiler_result_requires_full_capacity_and_valid_utf8_input().check() }
        }
    }
    mod host {
        mod build {
            mod tests {
                use crate::Outcome;
                use witchy_runtime::runtime::self_checks::host_build_tests as checks;
                #[test] fn build_env_length_rejects_values_outside_the_guest_abi() { checks::build_env_length_rejects_values_outside_the_guest_abi().check() }
            }
        }
        mod build_env {
            mod tests {
                use crate::Outcome;
                use witchy_runtime::runtime::self_checks::host_build_env_tests as checks;
                #[test] fn build_env_authenticates_kind_session_and_read_before_memory() { checks::build_env_authenticates_kind_session_and_read_before_memory().check() }
                #[test] fn build_env_missing_grant_and_explicit_empty_snapshot_stay_distinct() { checks::build_env_missing_grant_and_explicit_empty_snapshot_stay_distinct().check() }
                #[test] fn build_env_none_and_foreign_responses_refuse_before_memory() { checks::build_env_none_and_foreign_responses_refuse_before_memory().check() }
                #[test] fn build_env_preserves_some_empty_unset_and_denied() { checks::build_env_preserves_some_empty_unset_and_denied().check() }
                #[test] fn build_env_validates_full_key_slice_and_strict_utf8_before_lookup() { checks::build_env_validates_full_key_slice_and_strict_utf8_before_lookup().check() }
            }
        }
        mod build_exec {
            mod tests {
                use crate::Outcome;
                use witchy_runtime::runtime::self_checks::host_build_exec_tests as checks;
                #[test] fn build_exec_authenticates_kind_session_and_execute_before_memory() { checks::build_exec_authenticates_kind_session_and_execute_before_memory().check() }
                #[test] fn build_exec_requires_explicit_resolved_grants_and_never_looks_up_guest_paths() { checks::build_exec_requires_explicit_resolved_grants_and_never_looks_up_guest_paths().check() }
                #[test] fn build_exec_response_is_not_a_root_and_requires_current_response_issuer() { checks::build_exec_response_is_not_a_root_and_requires_current_response_issuer().check() }
                #[test] fn build_exec_validates_both_complete_utf8_slices_before_any_execution() { checks::build_exec_validates_both_complete_utf8_slices_before_any_execution().check() }
            }
        }
        mod build_net {
            mod tests {
                use crate::Outcome;
                use witchy_runtime::runtime::self_checks::host_build_net_tests as checks;
                #[test] fn build_net_authenticates_kind_current_issuer_and_read_before_memory() { checks::build_net_authenticates_kind_current_issuer_and_read_before_memory().check() }
                #[test] fn build_net_legacy_staging_and_old_signature_do_not_link() { checks::build_net_legacy_staging_and_old_signature_do_not_link().check() }
                #[test] fn build_net_preflights_both_complete_utf8_slices_without_network_or_writes() { checks::build_net_preflights_both_complete_utf8_slices_without_network_or_writes().check() }
                #[test] fn build_net_requires_explicit_grants_and_denies_unlisted_hosts_before_network() { checks::build_net_requires_explicit_grants_and_denies_unlisted_hosts_before_network().check() }
                #[test] fn build_net_responses_are_not_roots_and_reauthenticate_the_response_issuer() { checks::build_net_responses_are_not_roots_and_reauthenticate_the_response_issuer().check() }
            }
        }
        mod build_out {
            mod tests {
                use crate::Outcome;
                use witchy_runtime::runtime::self_checks::host_build_out_tests as checks;
                #[test] fn build_out_authentication_rejects_kind_session_and_rights() { checks::build_out_authentication_rejects_kind_session_and_rights().check() }
                #[test] fn build_out_slices_are_bounded_and_strict_utf8() { checks::build_out_slices_are_bounded_and_strict_utf8().check() }
            }
        }
        mod build_read {
            mod tests {
                use crate::Outcome;
                use witchy_runtime::runtime::self_checks::host_build_read_tests as checks;
                #[test] fn build_read_authenticates_grants_before_memory_and_rejects_missing_grants() { checks::build_read_authenticates_grants_before_memory_and_rejects_missing_grants().check() }
                #[test] fn build_read_confined_root_failure_can_fall_back_but_never_escape() { checks::build_read_confined_root_failure_can_fall_back_but_never_escape().check() }
                #[test] fn build_read_confines_absolute_and_parent_paths() { checks::build_read_confines_absolute_and_parent_paths().check() }
                #[test] fn build_read_rejects_malformed_path_slices_before_filesystem_lookup() { checks::build_read_rejects_malformed_path_slices_before_filesystem_lookup().check() }
                #[test] fn build_read_response_kind_and_session_are_not_read_authority() { checks::build_read_response_kind_and_session_are_not_read_authority().check() }
                #[test] fn build_read_response_length_never_wraps_the_i32_abi() { checks::build_read_response_length_never_wraps_the_i32_abi().check() }
            }
        }
        mod canonical_fixture {
            #[cfg(feature = "test-fixtures")]
            mod tests {
                use crate::Outcome;
                use witchy_runtime::runtime::self_checks::host_canonical_fixture_tests as checks;
                #[test] fn canonical_console_fixture_capture_checks_the_active_store_session_and_rights() { checks::canonical_console_fixture_capture_checks_the_active_store_session_and_rights().check() }
                #[test] fn canonical_console_fixture_slices_refuse_overflow_oob_and_invalid_utf8() { checks::canonical_console_fixture_slices_refuse_overflow_oob_and_invalid_utf8().check() }
                #[test] fn canonical_console_fixture_tokens_are_kind_rights_and_session_checked() { checks::canonical_console_fixture_tokens_are_kind_rights_and_session_checked().check() }
            }
        }
        mod clock_env_rand {
            mod tests {
                use crate::Outcome;
                use witchy_runtime::runtime::self_checks::host_clock_env_rand_tests as checks;
                #[test] fn env_get_at_answers_denied_without_an_existence_oracle() { checks::env_get_at_answers_denied_without_an_existence_oracle().check() }
            }
        }
        mod closure_owner {
            mod tests {
                use crate::Outcome;
                use witchy_runtime::runtime::self_checks::host_closure_owner_tests as checks;
                #[test] fn build_env_captures_reauthenticate_root_and_optional_response_and_cleanup() { checks::build_env_captures_reauthenticate_root_and_optional_response_and_cleanup().check() }
                #[test] fn build_exec_captures_reauthenticate_root_response_and_release_owner() { checks::build_exec_captures_reauthenticate_root_response_and_release_owner().check() }
                #[test] fn build_net_captures_reauthenticate_root_response_and_release_owner() { checks::build_net_captures_reauthenticate_root_response_and_release_owner().check() }
                #[test] fn build_read_captures_authenticate_kind_session_projection_and_cleanup() { checks::build_read_captures_authenticate_kind_session_projection_and_cleanup().check() }
                #[test] fn captured_owner_list_mutation_preserves_age_and_copy_on_write() { checks::captured_owner_list_mutation_preserves_age_and_copy_on_write().check() }
                #[test] fn captured_owner_list_rejects_wrong_kind_family_instance_and_forgery() { checks::captured_owner_list_rejects_wrong_kind_family_instance_and_forgery().check() }
                #[test] fn captured_owner_list_retains_reads_and_cleans_duplicate_elements_once() { checks::captured_owner_list_retains_reads_and_cleans_duplicate_elements_once().check() }
                #[test] fn carried_net_capture_requires_host_kind_not_wrapper_authority() { checks::carried_net_capture_requires_host_kind_not_wrapper_authority().check() }
                #[test] fn exec_capture_requires_exact_host_grant_not_wrapper_or_response_kind() { checks::exec_capture_requires_exact_host_grant_not_wrapper_or_response_kind().check() }
                #[test] fn fetch_captures_authenticate_kind_session_projection_and_cleanup() { checks::fetch_captures_authenticate_kind_session_projection_and_cleanup().check() }
                #[test] fn nested_owner_construction_rejects_substitution_without_retaining() { checks::nested_owner_construction_rejects_substitution_without_retaining().check() }
                #[test] fn nested_owner_fields_retain_read_and_cleanup_in_capture_order() { checks::nested_owner_fields_retain_read_and_cleanup_in_capture_order().check() }
                #[test] fn opaque_owner_gc_payload_and_list_hold_actual_references() { checks::opaque_owner_gc_payload_and_list_hold_actual_references().check() }
                #[test] fn opaque_owner_lifecycle_and_substitution_negatives() { checks::opaque_owner_lifecycle_and_substitution_negatives().check() }
                #[test] fn opaque_owner_list_drops_all_variants_and_refuses_dead_list() { checks::opaque_owner_list_drops_all_variants_and_refuses_dead_list().check() }
                #[test] fn owner_list_set_is_bounds_checked_copy_on_write_and_cleans_replacement_once() { checks::owner_list_set_is_bounds_checked_copy_on_write_and_cleans_replacement_once().check() }
                #[test] fn owner_list_transfer_keeps_replacement_cleanup_and_duplicate_tokens() { checks::owner_list_transfer_keeps_replacement_cleanup_and_duplicate_tokens().check() }
                #[test] fn owner_tag_is_constructor_metadata_with_family_level_authentication() { checks::owner_tag_is_constructor_metadata_with_family_level_authentication().check() }
                #[test] fn registry_hosts_captures_authenticate_kind_session_projection_and_cleanup() { checks::registry_hosts_captures_authenticate_kind_session_projection_and_cleanup().check() }
            }
        }
        mod console {
            mod canonical_console_tests {
                use crate::Outcome;
                use witchy_runtime::runtime::self_checks::host_console_canonical_console_tests as checks;
                #[test] fn canonical_abort_checks_both_unsigned_slices_without_output() { checks::canonical_abort_checks_both_unsigned_slices_without_output().check() }
                #[test] fn canonical_console_rejects_null_forged_tokens_and_missing_grants() { checks::canonical_console_rejects_null_forged_tokens_and_missing_grants().check() }
                #[test] fn canonical_list_bounds_checks_unsigned_label_slice_and_signed_index() { checks::canonical_list_bounds_checks_unsigned_label_slice_and_signed_index().check() }
            }
        }
        mod crypto {
            mod extensions {
                mod tests {
                    use crate::Outcome;
                    use witchy_runtime::runtime::self_checks::host_crypto_extensions_tests as checks;
                    #[test] fn crypto_extensions_slices_preflight_every_range_before_decoding() { checks::crypto_extensions_slices_preflight_every_range_before_decoding().check() }
                }
            }
            mod hash_slices_tests {
                use crate::Outcome;
                use witchy_runtime::runtime::self_checks::host_crypto_hash_slices_tests as checks;
                #[test] fn crypto_hash_slices_preflight_full_input_and_output_ranges() { checks::crypto_hash_slices_preflight_full_input_and_output_ranges().check() }
                #[test] fn rune_hash_slices_reject_malformed_transport_and_preserve_native_length_error() { checks::rune_hash_slices_reject_malformed_transport_and_preserve_native_length_error().check() }
            }
            mod rsa_slices_tests {
                use crate::Outcome;
                use witchy_runtime::runtime::self_checks::host_crypto_rsa_slices_tests as checks;
                #[test] fn rsa_slices_preflight_every_range_without_wraparound() { checks::rsa_slices_preflight_every_range_without_wraparound().check() }
            }
            mod slices_tests {
                use crate::Outcome;
                use witchy_runtime::runtime::self_checks::host_crypto_slices_tests as checks;
                #[test] fn ed25519_slices_accept_exact_end_and_require_exported_memory() { checks::ed25519_slices_accept_exact_end_and_require_exported_memory().check() }
                #[test] fn ed25519_slices_reject_invalid_extents_and_utf8_without_writes() { checks::ed25519_slices_reject_invalid_extents_and_utf8_without_writes().check() }
            }
        }
        mod fetch {
            mod tests {
                use crate::Outcome;
                use witchy_runtime::runtime::self_checks::host_fetch_tests as checks;
                #[test] fn fetch_host_authenticates_private_kinds_and_issuer_before_memory() { checks::fetch_host_authenticates_private_kinds_and_issuer_before_memory().check() }
                #[test] fn fetch_host_canonical_only_authenticates_before_memory_and_checks_full_slice() { checks::fetch_host_canonical_only_authenticates_before_memory_and_checks_full_slice().check() }
                #[test] fn fetch_host_canonical_only_preserves_subset_issuer_and_dns_floor() { checks::fetch_host_canonical_only_preserves_subset_issuer_and_dns_floor().check() }
                #[test] fn fetch_host_direct_grants_are_explicit_ordinal_and_empty_is_not_missing() { checks::fetch_host_direct_grants_are_explicit_ordinal_and_empty_is_not_missing().check() }
                #[test] fn fetch_host_errors_are_values_redirects_injection_and_size_remain_enforced() { checks::fetch_host_errors_are_values_redirects_injection_and_size_remain_enforced().check() }
                #[test] fn fetch_host_https_does_not_downgrade_to_plaintext() { checks::fetch_host_https_does_not_downgrade_to_plaintext().check() }
                #[test] fn fetch_host_legacy_mint_and_only_keep_authenticated_payload_and_attenuation() { checks::fetch_host_legacy_mint_and_only_keep_authenticated_payload_and_attenuation().check() }
                #[test] fn fetch_host_net_derivation_keeps_the_post_dns_floor() { checks::fetch_host_net_derivation_keeps_the_post_dns_floor().check() }
                #[test] fn fetch_host_preflights_every_complete_utf8_input_before_io() { checks::fetch_host_preflights_every_complete_utf8_input_before_io().check() }
            }
        }
        mod filesystem {
            mod canonical_exec {
                #[cfg(unix)]
                mod tests {
                    use crate::Outcome;
                    use witchy_runtime::runtime::self_checks::host_filesystem_canonical_exec_tests as checks;
                    #[test] fn exec_slices_reject_wrong_authority_malformed_inputs_and_response_forgery() { checks::exec_slices_reject_wrong_authority_malformed_inputs_and_response_forgery().check() }
                    #[test] fn exec_slices_require_dir_read_before_invalid_buffers_or_effects() { checks::exec_slices_require_dir_read_before_invalid_buffers_or_effects().check() }
                }
            }
            mod dir_installer_tests {
                use crate::Outcome;
                use witchy_runtime::runtime::self_checks::host_filesystem_dir_installer_tests as checks;
                #[test] fn dir_installer_authentication_and_write_right_precede_memory() { checks::dir_installer_authentication_and_write_right_precede_memory().check() }
                #[test] fn dir_installer_entry_policy_applies_to_both_rename_paths() { checks::dir_installer_entry_policy_applies_to_both_rename_paths().check() }
                #[test] fn dir_installer_full_buffer_preflight_prevents_filesystem_effects() { checks::dir_installer_full_buffer_preflight_prevents_filesystem_effects().check() }
                #[test] fn dir_installer_imports_require_existing_write_grant_linkage() { checks::dir_installer_imports_require_existing_write_grant_linkage().check() }
                #[test] fn dir_installer_symlink_behavior_preserves_confined_provider_policy() { checks::dir_installer_symlink_behavior_preserves_confined_provider_policy().check() }
            }
            mod dir_storage_tests {
                use crate::Outcome;
                use witchy_runtime::runtime::self_checks::host_filesystem_dir_storage_tests as checks;
                #[test] fn dir_storage_entry_policy_is_preserved_for_every_file_operation() { checks::dir_storage_entry_policy_is_preserved_for_every_file_operation().check() }
                #[test] fn dir_storage_full_input_preflight_precedes_file_mutation() { checks::dir_storage_full_input_preflight_precedes_file_mutation().check() }
                #[test] fn dir_read_at_answers_failures_as_statuses() { checks::dir_read_at_answers_failures_as_statuses().check() }
                #[test] fn file_read_at_answers_failures_as_statuses() { checks::file_read_at_answers_failures_as_statuses().check() }
                #[test] fn dir_list_at_lists_only_what_the_grant_observes() { checks::dir_list_at_lists_only_what_the_grant_observes().check() }
                #[test] fn dir_navigation_answers_failures_as_statuses() { checks::dir_navigation_answers_failures_as_statuses().check() }
                #[test] fn write_only_dir_links_write_file() { checks::write_only_dir_links_write_file().check() }
                #[test] fn dir_storage_imports_remain_grant_gated() { checks::dir_storage_imports_remain_grant_gated().check() }
                #[test] fn dir_storage_kind_rights_and_response_issuer_precede_memory() { checks::dir_storage_kind_rights_and_response_issuer_precede_memory().check() }
                #[test] fn dir_storage_response_preflight_short_capacity_and_exact_prefix() { checks::dir_storage_response_preflight_short_capacity_and_exact_prefix().check() }
                #[test] fn dir_storage_symlinks_never_acquire_outside_authority() { checks::dir_storage_symlinks_never_acquire_outside_authority().check() }
            }
            mod metadata_tests {
                use crate::Outcome;
                use witchy_runtime::runtime::self_checks::host_filesystem_metadata_tests as checks;
                #[test] fn dir_is_dir_slices_link_only_with_existing_read_grant() { checks::dir_is_dir_slices_link_only_with_existing_read_grant().check() }
                #[test] fn dir_is_dir_slices_preserve_confined_and_policy_alias_behavior() { checks::dir_is_dir_slices_preserve_confined_and_policy_alias_behavior().check() }
                #[test] fn dir_is_dir_slices_preserve_entry_policy_without_minting_authority() { checks::dir_is_dir_slices_preserve_entry_policy_without_minting_authority().check() }
                #[test] fn dir_is_dir_slices_reject_invalid_transport_after_authentication() { checks::dir_is_dir_slices_reject_invalid_transport_after_authentication().check() }
            }
            mod tests {
                use crate::Outcome;
                use witchy_runtime::runtime::self_checks::host_filesystem_tests as checks;
                #[test] fn dir_write_at_is_not_linked_without_a_write_grant() { checks::dir_write_at_is_not_linked_without_a_write_grant().check() }
                #[test] fn dir_write_at_refuses_a_read_only_dir() { checks::dir_write_at_refuses_a_read_only_dir().check() }
            }
        }
        mod network {
            mod tests {
                use crate::Outcome;
                use witchy_runtime::runtime::self_checks::host_network_tests as checks;
                #[test] fn pool_listener_is_reused_only_for_its_bound_identity_and_mode() { checks::pool_listener_is_reused_only_for_its_bound_identity_and_mode().check() }
            }
        }
        mod regex {
            mod tests {
                use crate::Outcome;
                use witchy_runtime::runtime::self_checks::host_regex_tests as checks;
                #[test] fn canonical_regex_bounded_writes_and_invalid_slices() { checks::canonical_regex_bounded_writes_and_invalid_slices().check() }
            }
        }
        mod secret {
            mod registry_tests {
                use crate::Outcome;
                use witchy_runtime::runtime::self_checks::host_secret_registry_tests as checks;
                #[test] fn registry_hosts_clock_grant_kind_session_and_provider() { checks::registry_hosts_clock_grant_kind_session_and_provider().check() }
                #[test] fn registry_hosts_full_slice_preflight_and_invalid_seed_never_write() { checks::registry_hosts_full_slice_preflight_and_invalid_seed_never_write().check() }
                #[test] fn registry_hosts_make_dir_rejects_outside_symlink() { checks::registry_hosts_make_dir_rejects_outside_symlink().check() }
                #[test] fn registry_hosts_make_dir_right_and_kind_precede_memory() { checks::registry_hosts_make_dir_right_and_kind_precede_memory().check() }
                #[test] fn registry_hosts_secret_kind_and_foreign_issuer_precede_memory() { checks::registry_hosts_secret_kind_and_foreign_issuer_precede_memory().check() }
            }
        }
        mod vm_worker {
            mod server_startup_tests {
                use crate::Outcome;
                use witchy_runtime::runtime::self_checks::host_vm_worker_server_startup_tests as checks;
                #[test] fn server_workers_refuse_missing_or_export_derived_admission() { checks::server_workers_refuse_missing_or_export_derived_admission().check() }
            }
            mod vm_service {
                mod tests {
                    use crate::Outcome;
                    use witchy_runtime::runtime::self_checks::host_vm_worker_vm_service_tests as checks;
                    #[test] fn response_kind_bounds_and_immutable_short_write() { checks::response_kind_bounds_and_immutable_short_write().check() }
                    #[test] fn service_start_refuses_raw_exports_and_invalid_site_numbers() { checks::service_start_refuses_raw_exports_and_invalid_site_numbers().check() }
                }
            }
        }
    }
    mod launch_arguments {
        mod tests {
            use crate::Outcome;
            use witchy_runtime::runtime::self_checks::launch_arguments_tests as checks;
            #[test] fn argument_image_rejects_address_overflow() { checks::argument_image_rejects_address_overflow().check() }
            #[test] fn service_byte_image_is_owned_binary_and_rejects_overflow() { checks::service_byte_image_is_owned_binary_and_rejects_overflow().check() }
        }
    }
    mod tests {
        use crate::Outcome;
        use witchy_runtime::runtime::self_checks::tests as checks;
        #[test] fn admitted_module_builder_validates_without_binaryen_or_byte_rewriting() { checks::admitted_module_builder_validates_without_binaryen_or_byte_rewriting().check() }
        #[test] fn artifact_layout_metadata_is_checked_before_instantiation() { checks::artifact_layout_metadata_is_checked_before_instantiation().check() }
        #[test] fn authenticated_external_library_rejects_tampering_imports_and_start() { checks::authenticated_external_library_rejects_tampering_imports_and_start().check() }
        #[test] fn binaryen_pin_resolution_rejects_mismatch_partial_sets_and_cross_source_mixing() { checks::binaryen_pin_resolution_rejects_mismatch_partial_sets_and_cross_source_mixing().check() }
        #[test] fn binaryen_requires_an_authenticated_bundle_and_runs_the_exact_recipe() { checks::binaryen_requires_an_authenticated_bundle_and_runs_the_exact_recipe().check() }
        #[test] fn byte_export_round_trips_non_utf8_and_enforces_output_limit() { checks::byte_export_round_trips_non_utf8_and_enforces_output_limit().check() }
        #[test] fn checked_uaf_double_free_still_traps() { checks::checked_uaf_double_free_still_traps().check() }
        #[test] fn compiled_secret_fixture_keeps_material_opaque_and_scripts_crypto() { checks::compiled_secret_fixture_keeps_material_opaque_and_scripts_crypto().check() }
        #[test] fn concrete_grants_have_one_complete_confinement_policy() { checks::concrete_grants_have_one_complete_confinement_policy().check() }
        #[test] fn duplicate_artifact_layout_metadata_fails_closed() { checks::duplicate_artifact_layout_metadata_fails_closed().check() }
        #[test] fn empty_build_grants_do_not_widen_outer_authority() { checks::empty_build_grants_do_not_widen_outer_authority().check() }
        #[test] fn empty_network_grant_remains_distinct_from_no_network_grant() { checks::empty_network_grant_remains_distinct_from_no_network_grant().check() }
        #[test] fn exec_child_paths_widen_only_the_read_only_outer_fence() { checks::exec_child_paths_widen_only_the_read_only_outer_fence().check() }
        #[test] fn dir_read_file_rejects_an_invalid_utf8_path() { checks::dir_read_file_rejects_an_invalid_utf8_path().check() }
        #[test] fn forged_par_map_count_is_bounded_before_host_allocation() { checks::forged_par_map_count_is_bounded_before_host_allocation().check() }
        #[test] fn heap_check_overrun_redzone_traps() { checks::heap_check_overrun_redzone_traps().check() }
        #[test] fn host_imports_require_generated_layout_metadata_before_linking() { checks::host_imports_require_generated_layout_metadata_before_linking().check() }
        #[test] fn memory_budget_is_enforced() { checks::memory_budget_is_enforced().check() }
        #[test] fn null_externrefs_are_rejected_by_every_capability_family() { checks::null_externrefs_are_rejected_by_every_capability_family().check() }
        #[test] fn optimized_wasm_cache_envelope_rejects_corruption_and_swaps() { checks::optimized_wasm_cache_envelope_rejects_corruption_and_swaps().check() }
        #[test] fn optimizer_policy_is_versioned_and_fails_closed() { checks::optimizer_policy_is_versioned_and_fails_closed().check() }
        #[test] fn read_only_direct_file_does_not_link_write() { checks::read_only_direct_file_does_not_link_write().check() }
        #[test] fn read_wstr_list_huge_count_fails_closed_not_aborts() { checks::read_wstr_list_huge_count_fails_closed_not_aborts().check() }
        #[test] fn runaway_vm_is_preempted() { checks::runaway_vm_is_preempted().check() }
        #[test] fn trapped_vm_is_terminal() { checks::trapped_vm_is_terminal().check() }
        #[test] fn ungranted_capability_is_unreachable() { checks::ungranted_capability_is_unreachable().check() }
        #[test] fn vm_with_dir_start_observes_only_the_delegated_dir() { checks::vm_with_dir_start_observes_only_the_delegated_dir().check() }
        #[test] fn vms_have_independent_memories() { checks::vms_have_independent_memories().check() }
    }
}
