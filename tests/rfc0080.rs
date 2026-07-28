//! Driver binary consolidating the rfc0080 conformance tests into one test
//! binary. Each module below was formerly its own `tests/rfc0080_*.rs` file;
//! collapsing them into one crate cuts merge-gate compile + discovery cost.
//! Files live in `tests/rfc0080/` (a subdir is not auto-compiled as its own
//! binary) and are attached here via `#[path]` since a test crate root
//! resolves bare `mod` names against `tests/`, not the subdir.

fn assert_compiled_output(
    module: &witchy::ast::Module,
    expected: &[String],
    label: &str,
    stack_size: usize,
) {
    let wasm = witchy::codegen::compile_module_binary(module).expect_lowered(label);
    let mut runtime = witchy::runtime::Runtime::batch().expect("runtime");
    let mut actor = runtime
        .spawn(
            &wasm,
            witchy::runtime::Capabilities { print: true, quiet: true, ..Default::default() },
            stack_size,
        )
        .expect("spawn");
    actor.run().expect(label);
    assert_eq!(actor.output(), expected, "{label}");
}

#[path = "rfc0080/fresh_ident.rs"]
mod fresh_ident;
#[path = "rfc0080/expansion_diagnostics.rs"]
mod expansion_diagnostics;
#[path = "rfc0080/owned_body_syntax.rs"]
mod owned_body_syntax;
#[path = "rfc0080/owned_expr_syntax.rs"]
mod owned_expr_syntax;
#[path = "rfc0080/owned_item_syntax.rs"]
mod owned_item_syntax;
#[path = "rfc0080/owned_pattern_syntax.rs"]
mod owned_pattern_syntax;
#[path = "rfc0080/owned_type_syntax.rs"]
mod owned_type_syntax;
#[path = "rfc0080/qualified_identities.rs"]
mod qualified_identities;
#[path = "rfc0080/structural_expr_holes.rs"]
mod structural_expr_holes;
#[path = "rfc0080/structural_item_holes.rs"]
mod structural_item_holes;
#[path = "rfc0080/structural_pattern_holes.rs"]
mod structural_pattern_holes;
#[path = "rfc0080/structural_body_holes.rs"]
mod structural_body_holes;
#[path = "rfc0080/structural_type_holes.rs"]
mod structural_type_holes;
#[path = "rfc0080/tag_definition_site.rs"]
mod tag_definition_site;
#[path = "rfc0080/structural_leaf_builders.rs"]
mod structural_leaf_builders;
#[path = "rfc0080/structural_item_builders.rs"]
mod structural_item_builders;
