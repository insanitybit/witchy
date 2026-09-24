//! RFC-0167 section 7 / row K: immutable views cross suspension.
//!
//! A view retains its own root, so an async function or a spawned task may take
//! one as a parameter; each carrier used here (`&Int` cell, `&Float` value,
//! `&String` and `&List(Int)` tokens) is an authenticated retained-root
//! representation. Independent expected output, admitted unoptimized Wasm, zero leaks on both.

#[test]
fn a_legacy_borrow_still_cannot_cross_an_async_boundary() {
    let error = witchy::resolve_std_only_checked(
        "async fn f(x: &'a Int) -> Int:\n    (*x)\n\nasync fn main(console: Console):\n    let n = 1\n    let r = f(&n).await\n    console.print(\"${r}\")\n",
    )
    .expect_err("a lifetime-bearing reference is retired");
    assert!(error.to_string().contains("retired"), "{error}");
}

