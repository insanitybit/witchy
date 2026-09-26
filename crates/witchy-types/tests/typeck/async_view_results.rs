//! RFC-0167 section 5 / rows G and K: an `async fn` may return a view, and a
//! closure (or an awaited task's recipe) may capture a `&[T]` slice view.
//!
//! A view is a value-semantic snapshot handle that retains its own root, so
//! the task result carries the view's carrier and the awaited value reads the
//! snapshot taken before the callee suspended, whatever the caller does to the
//! root meanwhile. A captured `&[T]` is its rooted-slice owner, retained into
//! an owner field of the closure environment.
//!
//! Evidence: an independent hand-computed expectation and admitted
//! unoptimized Wasm with a zero-leak heap audit.

use super::source::checked;

/// Slice-carrier recognition keys on the checker's private `Slice` payload
/// name, so a source type may not take that name.
#[test]
fn a_source_type_cannot_take_the_reserved_slice_name() {
    let error = checked(
        "type Slice(a):\n    items: List(a)\n\nfn main() -> Int:\n    let s = Slice(items: [1, 2])\n    s.items.length()\n",
    )
    .err()
    .map(|error| error.to_string())
    .expect("a type named Slice is refused");
    assert!(error.contains("the name `Slice` is reserved"), "diagnostic names the rule: {error}");
}
