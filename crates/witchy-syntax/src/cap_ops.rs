//! Shared spelling for compiler-internal capability-operation calls.
//!
//! Source-level capability ops are methods (`console.print("hi")`). During trait
//! lowering they become ordinary calls, but keeping a private marker on those
//! calls preserves the fact that the user wrote method syntax. RFC-0076's
//! bare-form rejection depends on that distinction.

pub const PREFIX: &str = "__capop.";

pub fn call_name(method: &str) -> String {
    format!("{PREFIX}{method}")
}

pub fn is_marked(name: &str) -> bool {
    name.starts_with(PREFIX)
}

pub fn surface_name(name: &str) -> &str {
    name.strip_prefix(PREFIX).unwrap_or(name)
}
