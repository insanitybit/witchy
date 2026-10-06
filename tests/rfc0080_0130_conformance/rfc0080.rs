//! Driver binary consolidating the rfc0080 conformance tests into one test
//! binary. Each module below was formerly its own `tests/rfc0080_*.rs` file;
//! collapsing them into one crate cuts merge-gate compile + discovery cost.
//! Files live in `tests/rfc0080/` (a subdir is not auto-compiled as its own
//! binary) and are attached here via `#[path]` since a test crate root
//! resolves bare `mod` names against `tests/`, not the subdir.

fn is_nightly() -> bool {
    std::env::var_os("WITCHY_NIGHTLY").is_some_and(|v| v == "1")
        || std::env::var_os("WITCHY_FULL_SUITE").is_some_and(|v| v == "1")
}

fn assert_admitted_source_output(source: &str, expected: &[String], label: &str, stack_size: usize) {
    struct ResetOptimization;
    impl Drop for ResetOptimization {
        fn drop(&mut self) { witchy::opt::set_for_tests(None); }
    }
    witchy::opt::set_for_tests(Some(witchy::opt::OptSet::none()));
    let _reset = ResetOptimization;
    let checked = witchy_types::pipeline::link_checked_from_source_bundle(
        vec![("main".into(), source.as_bytes().to_vec())],
        "main",
        witchy_interp::comptime::EXPANDER,
    ).expect("authenticate and check generated source");
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered(label);
    let mut runtime = witchy::runtime::Runtime::batch().expect("runtime");
    let mut actor = runtime.spawn_admitted(
        &artifact,
        witchy::runtime::Capabilities {
            print: true,
            quiet: true,
            ..witchy::runtime::Capabilities::none()
        },
        stack_size,
    ).expect("spawn admitted generated program");
    actor.run().expect(label);
    assert_eq!(actor.output(), expected, "{label}");
    assert_eq!(actor.canonical_heap_leaks().expect("heap audit"), Some(0));
}

#[path = "../rfc0080/fresh_ident.rs"]
mod fresh_ident;
#[path = "../rfc0080/expansion_diagnostics.rs"]
mod expansion_diagnostics;
#[path = "../rfc0080/owned_expr_syntax.rs"]
mod owned_expr_syntax;
#[path = "../rfc0080/qualified_identities.rs"]
mod qualified_identities;
#[path = "../rfc0080/derive_boundary_witnesses.rs"]
mod derive_boundary_witnesses;
#[path = "../rfc0080/quote_site.rs"]
mod quote_site;
