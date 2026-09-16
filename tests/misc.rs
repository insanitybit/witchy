//! Driver binary consolidating the single-test miscellaneous integration tests
//! into one test binary. Each module below was formerly its own top-level
//! `tests/*.rs` file; collapsing them into one crate cuts merge-gate compile +
//! discovery cost. Files live in `tests/misc/` (a subdir is not auto-compiled
//! as its own binary) and are attached here via `#[path]` since a test crate
//! root resolves bare `mod` names against `tests/`, not the subdir.
#[path = "support/temp_dir.rs"]
mod temp_dir;

#[path = "misc/rfc0165_bounded_heap.rs"]
mod rfc0165_bounded_heap;

#[path = "misc/rfc0165_packaged_source.rs"]
mod rfc0165_packaged_source;

#[path = "misc/rfc0165_project_run.rs"]
mod rfc0165_project_run;

#[path = "misc/rfc0165_build_read_project.rs"]
mod rfc0165_build_read_project;

#[path = "misc/rfc0165_build_env_project.rs"]
mod rfc0165_build_env_project;

#[path = "misc/rfc0165_messages.rs"]
mod rfc0165_messages;

#[path = "misc/rfc0165_registry_hosts.rs"]
mod rfc0165_registry_hosts;

#[path = "misc/rfc0165_dir_storage.rs"]
mod rfc0165_dir_storage;
#[path = "misc/rfc0165_enum_equality.rs"]
mod rfc0165_enum_equality;

#[path = "misc/rfc0165_range_bounds.rs"]
mod rfc0165_range_bounds;

#[path = "misc/rfc0165_net.rs"]
mod rfc0165_net;

#[path = "misc/canonical_net_fixture.rs"]
mod canonical_net_fixture;
#[path = "misc/canonical_net_read_profile.rs"]
mod canonical_net_read_profile;
#[path = "misc/canonical_tail_binding.rs"]
mod canonical_tail_binding;

#[path = "misc/rfc_status_hygiene.rs"]
mod rfc_status_hygiene;
#[path = "misc/example_source_paths.rs"]
mod example_source_paths;
#[path = "misc/examples_index.rs"]
mod examples_index;
#[path = "misc/fmt_cli.rs"]
mod fmt_cli;
#[path = "misc/http_url_fallible.rs"]
mod http_url_fallible;
#[path = "misc/list_filter_method.rs"]
mod list_filter_method;
#[path = "misc/region_copyout.rs"]
mod region_copyout;
#[path = "misc/binaryen_production_differential.rs"]
mod binaryen_production_differential;
#[path = "misc/release_packaging.rs"]
mod release_packaging;
#[path = "misc/rendering_protocol.rs"]
mod rendering_protocol;
#[path = "misc/semantic_conformance.rs"]
mod semantic_conformance;
#[path = "misc/sealed_task_handles.rs"]
mod sealed_task_handles;
#[path = "misc/spec_freshness.rs"]
mod spec_freshness;
#[path = "misc/url_components.rs"]
mod url_components;
#[path = "misc/async_contract.rs"]
mod async_contract;
#[path = "misc/rfc0129_async_lowering.rs"]
mod rfc0129_async_lowering;
#[path = "misc/rfc0130_surface.rs"]
mod rfc0130_surface;
#[path = "misc/rfc0130_adapter_collection.rs"]
mod rfc0130_adapter_collection;
#[path = "misc/architecture.rs"]
mod architecture;
#[path = "misc/policy_sealing.rs"]
mod policy_sealing;
#[path = "misc/rfc0063_bytes_catalog.rs"]
mod rfc0063_bytes_catalog;
#[path = "misc/rfc0077_integration_grants.rs"]
mod rfc0077_integration_grants;
#[path = "misc/rfc0087_uniform_var.rs"]
mod rfc0087_uniform_var;
#[path = "misc/rfc0087_migration_census.rs"]
mod rfc0087_migration_census;
#[path = "misc/rfc0089_fip_contract.rs"]
mod rfc0089_fip_contract;
#[path = "misc/rfc0094_persistent_std_cache.rs"]
mod rfc0094_persistent_std_cache;
#[path = "misc/rfc0105_fixture_cli.rs"]
mod rfc0105_fixture_cli;
#[path = "misc/rfc0129_negative_matrix.rs"]
mod rfc0129_negative_matrix;
#[path = "misc/rfc0129_cooperative_worker_boundary.rs"]
mod rfc0129_cooperative_worker_boundary;
#[path = "misc/rfc0129_channel_contract.rs"]
mod rfc0129_channel_contract;
#[path = "misc/rfc0152_select3_frame_contract.rs"]
mod rfc0152_select3_frame_contract;
#[path = "misc/rfc0152_semantic_matrix.rs"]
mod rfc0152_semantic_matrix;
#[path = "misc/rfc0152_async_region_formation.rs"]
mod rfc0152_async_region_formation;
#[path = "misc/typed_errors.rs"]
mod typed_errors;
#[path = "misc/vm_worker_isolation.rs"]
mod vm_worker_isolation;
#[path = "misc/vm_service_runtime.rs"]
mod vm_service_runtime;
#[path = "misc/wasm_abi_catalog.rs"]
mod wasm_abi_catalog;
#[path = "misc/rfc0102_console.rs"]
mod rfc0102_console;
#[path = "misc/rfc0103_build_step_confinement.rs"]
mod rfc0103_build_step_confinement;
#[path = "misc/rfc0103_csp.rs"]
mod rfc0103_csp;
#[path = "misc/rfc0165_admitted_launch.rs"]
mod rfc0165_admitted_launch;
#[path = "misc/rfc0165_oracle.rs"]
mod rfc0165_oracle;
#[path = "misc/rfc0165_corpus.rs"]
mod rfc0165_corpus;
#[path = "misc/rfc0165_encoding.rs"]
mod rfc0165_encoding;
#[path = "misc/rfc0165_crypto_extensions.rs"]
mod rfc0165_crypto_extensions;
#[path = "misc/rfc0165_chars_allocation.rs"]
mod rfc0165_chars_allocation;
#[path = "misc/rfc0165_regex_spans.rs"]
mod rfc0165_regex_spans;
#[path = "misc/rfc0165_nullable_source.rs"]
mod rfc0165_nullable_source;
#[path = "misc/rfc0165_dictionary_mutation.rs"]
mod rfc0165_dictionary_mutation;
#[path = "misc/rfc0165_list_push_value.rs"]
mod rfc0165_list_push_value;
#[path = "misc/rfc0165_file_capability.rs"]
mod rfc0165_file_capability;
#[path = "misc/rfc0165_calc.rs"]
mod rfc0165_calc;
#[path = "misc/rfc0165_recursive.rs"]
mod rfc0165_recursive;
#[path = "misc/rfc0165_recursive_generic.rs"]
mod rfc0165_recursive_generic;
#[path = "misc/rfc0165_recursive_render.rs"]
mod rfc0165_recursive_render;
#[path = "misc/rfc0165_coalesce.rs"]
mod rfc0165_coalesce;
#[path = "misc/rfc0165_loop_shadowing.rs"]
mod rfc0165_loop_shadowing;
#[path = "misc/rfc0165_reference_list_patterns.rs"]
mod rfc0165_reference_list_patterns;
#[path = "misc/rfc0167_mode_interop.rs"]
mod rfc0167_mode_interop;
#[path = "misc/rfc0165_nested_patterns.rs"]
mod rfc0165_nested_patterns;
#[path = "misc/rfc0165_owner_adapters.rs"]
mod rfc0165_owner_adapters;
