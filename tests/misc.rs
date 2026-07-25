//! Driver binary consolidating the single-test miscellaneous integration tests
//! into one test binary. Each module below was formerly its own top-level
//! `tests/*.rs` file; collapsing them into one crate cuts merge-gate compile +
//! discovery cost. Files live in `tests/misc/` (a subdir is not auto-compiled
//! as its own binary) and are attached here via `#[path]` since a test crate
//! root resolves bare `mod` names against `tests/`, not the subdir.
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
#[path = "misc/typed_errors.rs"]
mod typed_errors;
#[path = "misc/vm_worker_isolation.rs"]
mod vm_worker_isolation;
#[path = "misc/wasm_abi_catalog.rs"]
mod wasm_abi_catalog;
