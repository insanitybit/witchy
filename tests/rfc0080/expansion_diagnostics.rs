//! RFC-0080 tagged-expansion provenance diagnostics.

use std::collections::HashSet;

use witchy::{format, parser, pipeline, typeck};

fn link(modules: Vec<(&str, &str)>) -> Result<(), String> {
    let user_modules: HashSet<String> =
        modules.iter().map(|(name, _)| name.to_string()).collect();
    let parsed = modules
        .into_iter()
        .map(|(name, source)| {
            (
                name.to_string(),
                parser::parse_module(source)
                    .unwrap_or_else(|error| panic!("module `{name}` parses: {error}")),
            )
        })
        .collect();
    pipeline::link_with_user_modules(parsed, "main", &user_modules)
        .map(|_| ())
        .map_err(|error| error.message)
}

#[test]
fn imported_typed_tag_error_names_invocation_and_definition_lines() {
    let error = link(vec![
        (
            "tag_library",
            "import meta\n\n\
             comptime fn broken(parts: List(String), holes: List(String)) -> meta.ExprSyntax:\n\
             \x20   missing_helper()\n",
        ),
        (
            "main",
            "import tag_library\n\n\
             fn main(console: Console):\n\
             \x20   console.print(\"before\")\n\
             \x20   console.print(\"${broken\"value\"}\")\n",
        ),
    ])
    .expect_err("the typed tag body does not type-check");

    assert!(
        error.contains("tagged literal `broken` at invocation line 5"),
        "{error}"
    );
    assert!(
        error.contains("defined in module `tag_library` at line 3"),
        "{error}"
    );
    assert!(error.contains("missing_helper"), "{error}");
}

#[test]
fn local_legacy_tag_generated_source_error_has_expansion_trace() {
    let error = link(vec![(
        "main",
        "comptime fn broken(parts: List(String), holes: List(String)) -> String:\n\
         \x20   \"(\"\n\n\
         fn main(console: Console):\n\
         \x20   console.print(\"before\")\n\
         \x20   console.print(\"${broken\"value\"}\")\n",
    )])
    .expect_err("the legacy tag emits malformed expression source");

    assert!(
        error.contains("tagged literal `broken` at invocation line 6"),
        "{error}"
    );
    assert!(error.contains("defined in module `main` at line 1"), "{error}");
    assert!(error.contains("generated source"), "{error}");
}

#[test]
fn generated_nested_tag_error_retains_outer_expansion_ancestry() {
    let error = link(vec![(
        "main",
        "comptime fn inner(parts: List(String), holes: List(String)) -> String:\n\
         \x20   \"(\"\n\n\
         comptime fn outer(parts: List(String), holes: List(String)) -> String:\n\
         \x20   \"inner\\\"nested\\\"\"\n\n\
         fn main(console: Console):\n\
         \x20   console.print(\"${outer\"value\"}\")\n",
    )])
    .expect_err("the outer tag emits a failing inner tag");

    assert!(error.contains("tagged literal `inner`"), "{error}");
    assert!(error.contains("expansion trace:"), "{error}");
    assert!(
        error.contains("from module `main`: tagged literal `outer` at invocation line 8"),
        "{error}"
    );
}

#[test]
fn nested_tag_in_hole_reports_the_hole_ancestry() {
    let error = link(vec![(
        "main",
        "comptime fn inner(parts: List(String), holes: List(String)) -> String:\n\
         \x20   \"(\"\n\n\
         comptime fn passthrough(parts: List(String), holes: List(String)) -> String:\n\
         \x20   list.at(holes, 0)\n\n\
         fn main(console: Console):\n\
         \x20   console.print(\"${passthrough\"${inner\"nested\"}\"}\")\n",
    )])
    .expect_err("a failing tag inside a call-site hole retains its path");

    assert!(error.contains("tagged literal `inner`"), "{error}");
    assert!(error.contains("expansion trace:"), "{error}");
    assert!(error.contains("tagged literal `passthrough`"), "{error}");
    assert!(
        error.contains("tagged literal `passthrough` at invocation line 8"),
        "{error}"
    );
    assert!(
        error.contains("hole 1 at hole-local line 1, column 15"),
        "{error}"
    );
}

#[test]
fn dropped_holes_do_not_expand_nested_tags() {
    link(vec![(
        "main",
        "comptime fn broken(parts: List(String), holes: List(String)) -> String:\n\
         \x20   \"(\"\n\n\
         comptime fn drop_hole(parts: List(String), holes: List(String)) -> String:\n\
         \x20   \"0\"\n\n\
         fn main(console: Console):\n\
         \x20   console.print(\"${drop_hole\"${broken\"unused\"}\"}\")\n",
    )])
    .expect("a nested tag in a dropped hole is not part of the generated tree");
}

#[test]
fn duplicated_holes_expand_nested_tags_independently() {
    let source = "import meta\n\n\
        comptime fn fresh_ref(parts: List(String), holes: List(String)) -> meta.ExprSyntax:\n\
        \x20   let name = meta.fresh(\"dup\")\n\
        \x20   meta.expr_name(name)\n\n\
        comptime fn duplicate(parts: List(String), holes: List(String)) -> String:\n\
        \x20   \"(${list.at(holes, 0)}, ${list.at(holes, 0)})\"\n\n\
        fn main():\n\
        \x20   let pair = \"${duplicate\"${fresh_ref\"value\"}\"}\"\n";
    let parsed = parser::parse_module(source).expect("duplicated-hole program parses");
    let linked = pipeline::link(vec![("main".into(), parsed)], "main")
        .expect("duplicated nested tags expand");
    let expanded = format::module(&linked, &[]);
    let mut names = expanded
        .match_indices("__witchy_fresh_")
        .map(|(start, _)| {
            expanded[start..]
                .chars()
                .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
                .collect::<String>()
        })
        .collect::<Vec<_>>();
    names.sort();
    names.dedup();
    assert_eq!(
        names.len(),
        2,
        "each generated placement gets its own nested-tag invocation: {expanded}"
    );
}

#[test]
fn reordered_holes_fail_in_generated_tree_order() {
    let error = link(vec![(
        "main",
        "comptime fn fail_a(parts: List(String), holes: List(String)) -> String:\n\
         \x20   \"(\"\n\n\
         comptime fn fail_b(parts: List(String), holes: List(String)) -> String:\n\
         \x20   \"[\"\n\n\
         comptime fn reverse(parts: List(String), holes: List(String)) -> String:\n\
         \x20   \"${list.at(holes, 1)} + ${list.at(holes, 0)}\"\n\n\
         fn main(console: Console):\n\
         \x20   console.print(\"${reverse\"${fail_a\"a\"} middle ${fail_b\"b\"}\"}\")\n",
    )])
    .expect_err("the first placed nested tag fails first");

    assert!(error.contains("tagged literal `fail_b`"), "{error}");
    assert!(!error.contains("tagged literal `fail_a`"), "{error}");
}

#[test]
fn source_cannot_forge_the_compiler_only_hole_origin_wrapper() {
    let parsed = parser::parse_module(
        "fn main() -> Int:\n\
         \x20   region:\n\
         \x20       __witchy_hole_origin(0, 1, 2)\n\
         \x20       42\n",
    )
    .expect("source-level forgery parses as an ordinary call");
    let linked = pipeline::link(vec![("main".into(), parsed)], "main")
        .expect("ordinary source reaches type checking unchanged");
    let error = typeck::check(&linked).expect_err("the forged source name is not a compiler marker");

    assert!(
        error.to_string().contains("unknown function `__witchy_hole_origin`"),
        "{error}"
    );
}
