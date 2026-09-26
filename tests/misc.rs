//! Driver binary consolidating the single-test miscellaneous integration tests
//! into one test binary. Each module below was formerly its own top-level
//! `tests/*.rs` file; collapsing them into one crate cuts merge-gate compile +
//! discovery cost. Files live in `tests/misc/` (a subdir is not auto-compiled
//! as its own binary) and are attached here via `#[path]` since a test crate
//! root resolves bare `mod` names against `tests/`, not the subdir.
fn manifest_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("Cargo sets CARGO_MANIFEST_DIR for tests"),
    )
}

#[path = "support/temp_dir.rs"]
mod temp_dir;

#[path = "support/precompiled_registry.rs"]
mod precompiled_registry;


#[path = "misc/rfc0165_packaged_source.rs"]
mod rfc0165_packaged_source;

#[path = "misc/rfc0165_project_run.rs"]
mod rfc0165_project_run;

#[path = "misc/rfc0165_build_read_project.rs"]
mod rfc0165_build_read_project;

#[path = "misc/rfc0165_build_env_project.rs"]
mod rfc0165_build_env_project;







#[path = "misc/examples_index.rs"]
mod examples_index;
#[path = "misc/fmt_cli.rs"]
mod fmt_cli;
#[path = "misc/http_url_fallible.rs"]
mod http_url_fallible;
#[path = "misc/region_copyout.rs"]
mod region_copyout;
#[path = "misc/binaryen_production_differential.rs"]
mod binaryen_production_differential;
#[path = "misc/rendering_protocol.rs"]
mod rendering_protocol;
#[path = "misc/semantic_conformance.rs"]
mod semantic_conformance;
#[path = "misc/sealed_task_handles.rs"]
mod sealed_task_handles;
#[path = "misc/url_components.rs"]
mod url_components;
#[path = "misc/rfc0129_async_lowering.rs"]
mod rfc0129_async_lowering;
#[path = "misc/rfc0130_surface.rs"]
mod rfc0130_surface;
#[path = "misc/rfc0130_adapter_collection.rs"]
mod rfc0130_adapter_collection;
#[path = "misc/policy_sealing.rs"]
mod policy_sealing;
#[path = "misc/rfc0077_integration_grants.rs"]
mod rfc0077_integration_grants;
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
#[path = "misc/wasm_abi_catalog.rs"]
mod wasm_abi_catalog;
#[path = "misc/rfc0167_stored_callback_invocation.rs"]
mod rfc0167_stored_callback_invocation;
#[path = "misc/rfc0102_console.rs"]
mod rfc0102_console;
#[path = "misc/rfc0103_build_step_confinement.rs"]
mod rfc0103_build_step_confinement;
#[path = "misc/rfc0165_admitted_launch.rs"]
mod rfc0165_admitted_launch;
#[path = "misc/rfc0167_try_context_and_leading_minus.rs"]
mod rfc0167_try_context_and_leading_minus;
#[path = "misc/rfc0167_precompiled_run_source.rs"]
mod rfc0167_precompiled_run_source;
#[path = "misc/rfc0167_views_through_views.rs"]
mod rfc0167_views_through_views;
#[path = "misc/rfc0165_owner_adapters.rs"]
mod rfc0165_owner_adapters;
#[path = "misc/rfc0167_views_through_channels_and_generators.rs"]
mod rfc0167_views_through_channels_and_generators;
#[path = "misc/rfc0167_installed_workflow.rs"]
mod rfc0167_installed_workflow;
#[path = "misc/rfc0167_normal_view_bearing_types.rs"]
mod rfc0167_normal_view_bearing_types;
#[path = "misc/rfc0167_nested_mode_preparation.rs"]
mod rfc0167_nested_mode_preparation;
#[path = "misc/rfc0167_slice_record_field.rs"]
mod rfc0167_slice_record_field;
#[path = "misc/rfc0167_try_int_view_payload.rs"]
mod rfc0167_try_int_view_payload;
#[path = "misc/rfc0167_var_view_parameters.rs"]
mod rfc0167_var_view_parameters;
#[path = "misc/rfc0167_view_locals_across_await.rs"]
mod rfc0167_view_locals_across_await;
#[path = "misc/rfc0167_await_in_nested_expressions.rs"]
mod rfc0167_await_in_nested_expressions;
#[path = "misc/rfc0167_owned_materialization.rs"]
mod rfc0167_owned_materialization;
#[path = "misc/rfc0167_tuple_root_view.rs"]
mod rfc0167_tuple_root_view;
#[path = "misc/rfc0167_nested_list_root_view.rs"]
mod rfc0167_nested_list_root_view;
#[path = "misc/rfc0167_dict_root_view.rs"]
mod rfc0167_dict_root_view;
#[path = "misc/rfc0167_indirect_list_view.rs"]
mod rfc0167_indirect_list_view;
#[path = "misc/rfc0167_list_of_int_views.rs"]
mod rfc0167_list_of_int_views;
#[path = "misc/rfc0167_scalar_views_across_await.rs"]
mod rfc0167_scalar_views_across_await;
#[path = "misc/rfc0167_slice_owned_materialization.rs"]
mod rfc0167_slice_owned_materialization;
#[path = "misc/rfc0167_slice_subscript_in_async.rs"]
mod rfc0167_slice_subscript_in_async;
#[path = "misc/rfc0167_slice_list_int_root_view.rs"]
mod rfc0167_slice_list_int_root_view;
#[path = "misc/rfc0167_resource_fallback.rs"]
mod rfc0167_resource_fallback;
