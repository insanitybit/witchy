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
#[path = "misc/http_url_fallible.rs"]
mod http_url_fallible;
#[path = "misc/list_filter_method.rs"]
mod list_filter_method;
#[path = "misc/region_copyout.rs"]
mod region_copyout;
#[path = "misc/rendering_protocol.rs"]
mod rendering_protocol;
#[path = "misc/sealed_task_handles.rs"]
mod sealed_task_handles;
#[path = "misc/spec_freshness.rs"]
mod spec_freshness;
#[path = "misc/url_components.rs"]
mod url_components;
