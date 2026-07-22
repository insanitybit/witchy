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
             pub comptime fn broken(parts: List(String), holes: List(String)) -> meta.ExprSyntax:\n\
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
fn private_imported_tags_are_not_invocable() {
    let error = link(vec![
        (
            "tag_library",
            "import meta\n\ncomptime fn hidden(parts: List(String), holes: List(String)) -> meta.ExprSyntax:\n    meta.expr_raw(\"0\")\n",
        ),
        (
            "main",
            "import tag_library\n\nfn main(console: Console):\n    console.print(\"${hidden\"value\"}\")\n",
        ),
    ])
    .expect_err("private imported tags stay private");

    assert!(error.contains("not public in a directly imported module"), "{error}");
}

#[test]
fn transitive_public_tags_are_not_invocable_without_a_direct_import() {
    let error = link(vec![
        (
            "leaf",
            "import meta\n\npub comptime fn distant(parts: List(String), holes: List(String)) -> meta.ExprSyntax:\n    meta.expr_raw(\"0\")\n",
        ),
        ("middle", "import leaf\n\npub fn value() -> Int:\n    1\n"),
        (
            "main",
            "import middle\n\nfn main(console: Console):\n    console.print(\"${distant\"value\"}\")\n",
        ),
    ])
    .expect_err("transitive imports do not inject tag names");

    assert!(error.contains("not public in a directly imported module"), "{error}");
}

#[test]
fn string_returning_tags_are_rejected() {
    let error = link(vec![(
        "main",
        "comptime fn old_tag(parts: List(String), holes: List(String)) -> String:\n\
         \x20   \"0\"\n\n\
         fn main(console: Console):\n\
         \x20   console.print(\"${old_tag\"value\"}\")\n",
    )])
    .expect_err("String-returning tags are no longer a language path");

    assert!(error.contains("tag `old_tag` must return meta.ExprSyntax"), "{error}");
}

#[test]
fn syntax_returning_tags_must_be_declared_comptime() {
    let error = link(vec![(
        "main",
        "import meta\n\n\
         fn runtime_tag(parts: List(String), holes: List(String)) -> meta.ExprSyntax:\n\
         \x20   meta.expr_raw(\"0\")\n\n\
         fn main(console: Console):\n\
         \x20   console.print(\"${runtime_tag\"value\"}\")\n",
    )])
    .expect_err("tag functions must be compile-time-only");

    assert!(error.contains("tag `runtime_tag` must be declared `comptime`"), "{error}");
}

#[test]
fn local_typed_raw_tag_error_has_expansion_trace() {
    let error = link(vec![(
        "main",
        "import meta\n\n\
         comptime fn broken(parts: List(String), holes: List(String)) -> meta.ExprSyntax:\n\
         \x20   meta.expr_raw(\"(\")\n\n\
         fn main(console: Console):\n\
         \x20   console.print(\"before\")\n\
         \x20   console.print(\"${broken\"value\"}\")\n",
    )])
    .expect_err("the typed tag constructs malformed expression source");

    assert!(
        error.contains("tagged literal `broken` at invocation line 8"),
        "{error}"
    );
    assert!(error.contains("defined in module `main` at line 3"), "{error}");
    assert!(error.contains("meta.expr_raw source does not parse"), "{error}");
}

#[test]
fn generated_nested_tag_error_retains_outer_expansion_ancestry() {
    let error = link(vec![(
        "main",
        "import meta\n\n\
         comptime fn inner(parts: List(String), holes: List(String)) -> meta.ExprSyntax:\n\
         \x20   meta.expr_raw(\"(\")\n\n\
         comptime fn outer(parts: List(String), holes: List(String)) -> meta.ExprSyntax:\n\
         \x20   meta.expr_raw(\"inner\\\"nested\\\"\")\n\n\
         fn main(console: Console):\n\
         \x20   console.print(\"${outer\"value\"}\")\n",
    )])
    .expect_err("the outer tag emits a failing inner tag");

    assert!(error.contains("tagged literal `inner`"), "{error}");
    assert!(error.contains("expansion trace:"), "{error}");
    assert!(
        error.contains("from module `main`: tagged literal `outer` at invocation line 10"),
        "{error}"
    );
}

#[test]
fn nested_tag_in_hole_reports_the_hole_ancestry() {
    let error = link(vec![(
        "main",
        "import meta\n\n\
         comptime fn inner(parts: List(String), holes: List(String)) -> meta.ExprSyntax:\n\
         \x20   meta.expr_raw(\"(\")\n\n\
         comptime fn passthrough(parts: List(String), holes: List(String)) -> meta.ExprSyntax:\n\
         \x20   meta.expr_raw(list.at(holes, 0))\n\n\
         fn main(console: Console):\n\
         \x20   console.print(\"${passthrough\"${inner\"nested\"}\"}\")\n",
    )])
    .expect_err("a failing tag inside a call-site hole retains its path");

    assert!(error.contains("tagged literal `inner`"), "{error}");
    assert!(error.contains("expansion trace:"), "{error}");
    assert!(error.contains("tagged literal `passthrough`"), "{error}");
    assert!(
        error.contains("tagged literal `passthrough` at invocation line 10"),
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
        "import meta\n\n\
         comptime fn broken(parts: List(String), holes: List(String)) -> meta.ExprSyntax:\n\
         \x20   meta.expr_raw(\"(\")\n\n\
         comptime fn drop_hole(parts: List(String), holes: List(String)) -> meta.ExprSyntax:\n\
         \x20   meta.expr_raw(\"0\")\n\n\
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
        comptime fn duplicate(parts: List(String), holes: List(String)) -> meta.ExprSyntax:\n\
        \x20   meta.expr_raw(\"(${list.at(holes, 0)}, ${list.at(holes, 0)})\")\n\n\
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
        "import meta\n\n\
         comptime fn fail_a(parts: List(String), holes: List(String)) -> meta.ExprSyntax:\n\
         \x20   meta.expr_raw(\"(\")\n\n\
         comptime fn fail_b(parts: List(String), holes: List(String)) -> meta.ExprSyntax:\n\
         \x20   meta.expr_raw(\"[\")\n\n\
         comptime fn reverse(parts: List(String), holes: List(String)) -> meta.ExprSyntax:\n\
         \x20   meta.expr_raw(\"${list.at(holes, 1)} + ${list.at(holes, 0)}\")\n\n\
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

#[test]
fn imported_tag_uses_helpers_from_its_own_module_despite_name_collisions() {
    link(vec![
        (
            "collision",
            "comptime fn helper() -> String:\n\
             \x20   \"(\"\n",
        ),
        (
            "tags",
            "import meta\n\n\
             comptime fn helper() -> String:\n\
             \x20   \"41 + 1\"\n\n\
             pub comptime fn answer(parts: List(String), holes: List(String)) -> meta.ExprSyntax:\n\
             \x20   meta.expr_raw(helper())\n",
        ),
        (
            "main",
            "import collision\n\
             import tags\n\n\
             fn main() -> Int:\n\
             \x20   answer\"ignored\"\n",
        ),
    ])
    .expect("a tag's private helper is selected by module identity");
}

#[test]
fn imported_tag_may_emit_a_private_nested_tag_from_its_definition_module() {
    link(vec![
        (
            "tags",
            "import meta\n\n\
             comptime fn inner(parts: List(String), holes: List(String)) -> meta.ExprSyntax:\n\
             \x20   meta.expr_raw(\"42\")\n\n\
             pub comptime fn outer(parts: List(String), holes: List(String)) -> meta.ExprSyntax:\n\
             \x20   meta.expr_raw(\"inner\\\"nested\\\"\")\n",
        ),
        (
            "main",
            "import tags\n\n\
             fn main() -> Int:\n\
             \x20   outer\"value\"\n",
        ),
    ])
    .expect("definition-site output retains access to private nested tags");
}

#[test]
fn tag_evaluator_bodies_cannot_reenter_tag_expansion() {
    let error = link(vec![(
        "main",
        "import meta\n\n\
         comptime fn recursive(parts: List(String), holes: List(String)) -> meta.ExprSyntax:\n\
         \x20   recursive\"again\"\n\n\
         fn main() -> Int:\n\
         \x20   recursive\"start\"\n",
    )])
    .expect_err("tag evaluator bodies cannot recursively invoke tagged expansion");

    assert!(
        error.contains("tag evaluator function `main.recursive` contains a tagged literal"),
        "{error}"
    );
}

#[test]
fn imported_comptime_helpers_remain_available_to_a_tag() {
    link(vec![
        (
            "support",
            "import meta\n\n\
             pub comptime fn build() -> meta.ExprSyntax:\n\
             \x20   meta.expr_raw(\"42\")\n",
        ),
        (
            "tags",
            "import meta\n\
             import support\n\n\
             pub comptime fn answer(parts: List(String), holes: List(String)) -> meta.ExprSyntax:\n\
             \x20   support.build()\n",
        ),
        (
            "main",
            "import tags\n\n\
             fn main() -> Int:\n\
             \x20   answer\"value\"\n",
        ),
    ])
    .expect("reachable comptime helpers are preserved in every synthetic module");
}

#[test]
fn generated_nested_tag_resolves_a_public_direct_import_at_definition_site() {
    link(vec![
        (
            "inner_tags",
            "import meta\n\n\
             pub comptime fn inner(parts: List(String), holes: List(String)) -> meta.ExprSyntax:\n\
             \x20   meta.expr_raw(\"42\")\n",
        ),
        (
            "outer_tags",
            "import inner_tags\n\
             import meta\n\n\
             pub comptime fn outer(parts: List(String), holes: List(String)) -> meta.ExprSyntax:\n\
             \x20   meta.expr_raw(\"inner\\\"nested\\\"\")\n",
        ),
        (
            "main",
            "import outer_tags\n\n\
             fn main() -> Int:\n\
             \x20   outer\"value\"\n",
        ),
    ])
    .expect("definition-site nested tags resolve direct public imports");
}

#[test]
fn duplicate_directly_imported_tag_names_are_ambiguous() {
    let tag_module = "import meta\n\n\
        pub comptime fn duplicate(parts: List(String), holes: List(String)) -> meta.ExprSyntax:\n\
        \x20   meta.expr_raw(\"42\")\n";
    let error = link(vec![
        ("alpha", tag_module),
        ("beta", tag_module),
        (
            "main",
            "import alpha\n\
             import beta\n\n\
             fn main() -> Int:\n\
             \x20   duplicate\"value\"\n",
        ),
    ])
    .expect_err("duplicate imported tag names require an unambiguous root");

    assert!(
        error.contains("tag `duplicate` is ambiguous across directly imported modules: alpha, beta"),
        "{error}"
    );
}

#[test]
fn tag_reachability_retains_top_level_constants() {
    link(vec![(
        "main",
        "import meta\n\n\
         let answer_source = \"42\"\n\n\
         comptime fn answer(parts: List(String), holes: List(String)) -> meta.ExprSyntax:\n\
         \x20   meta.expr_raw(answer_source)\n\n\
         fn main() -> Int:\n\
         \x20   answer\"value\"\n",
    )])
    .expect("reachable constants remain in the synthetic evaluator");
}

#[test]
fn tag_reachability_retains_from_imported_constructor_bindings() {
    link(vec![
        (
            "support",
            "type Wrapped:\n\
             \x20   Wrapped(String)\n",
        ),
        (
            "tags",
            "from support import Wrapped\n\
             import meta\n\n\
             pub comptime fn answer(parts: List(String), holes: List(String)) -> meta.ExprSyntax:\n\
             \x20   match Wrapped(\"42\"):\n\
             \x20       Wrapped(value) -> meta.expr_raw(value)\n",
        ),
        (
            "main",
            "import tags\n\n\
             fn main() -> Int:\n\
             \x20   answer\"value\"\n",
        ),
    ])
    .expect("a retained constructor keeps its from-import binding");
}

#[test]
fn tag_reachability_retains_custom_trait_implementations() {
    link(vec![(
        "main",
        "import meta\n\n\
         trait Render:\n\
         \x20   fn render(self) -> String\n\n\
         type Value:\n\
         \x20   Value(Int)\n\n\
         impl Render for Value:\n\
         \x20   fn render(self) -> String:\n\
         \x20       \"42\"\n\n\
         comptime fn answer(parts: List(String), holes: List(String)) -> meta.ExprSyntax:\n\
         \x20   meta.expr_raw(Value(0).render())\n\n\
         fn main() -> Int:\n\
         \x20   answer\"value\"\n",
    )])
    .expect("reachable custom trait implementations remain executable");
}

#[test]
fn reachable_tag_evaluator_constants_cannot_contain_tagged_literals() {
    let error = link(vec![(
        "main",
        "import meta\n\n\
         comptime fn inner(parts: List(String), holes: List(String)) -> meta.ExprSyntax:\n\
         \x20   meta.expr_raw(\"42\")\n\n\
         let generated = inner\"nested\"\n\n\
         comptime fn outer(parts: List(String), holes: List(String)) -> meta.ExprSyntax:\n\
         \x20   generated\n\n\
         fn main() -> Int:\n\
         \x20   outer\"value\"\n",
    )])
    .expect_err("reachable evaluator constants must not recursively expand tags");

    assert!(
        error.contains("tag evaluator constant `main.generated` contains a tagged literal"),
        "{error}"
    );
}

#[test]
fn tag_reachability_does_not_retain_same_named_impls_from_other_modules() {
    link(vec![
        (
            "noise",
            "import meta\n\n\
             comptime fn nested(parts: List(String), holes: List(String)) -> meta.ExprSyntax:\n\
             \x20   meta.expr_raw(\"\\\"noise\\\"\")\n\n\
             trait Render:\n\
             \x20   fn render(self) -> String\n\n\
             type Value:\n\
             \x20   Value(Int)\n\n\
             impl Render for Value:\n\
             \x20   fn render(self) -> String:\n\
             \x20       nested\"value\"\n",
        ),
        (
            "wanted",
            "import meta\n\n\
             trait Render:\n\
             \x20   fn render(self) -> String\n\n\
             type Value:\n\
             \x20   Value(Int)\n\n\
             impl Render for Value:\n\
             \x20   fn render(self) -> String:\n\
             \x20       \"42\"\n\n\
             pub comptime fn answer(parts: List(String), holes: List(String)) -> meta.ExprSyntax:\n\
             \x20   meta.expr_raw(Value(0).render())\n",
        ),
        (
            "main",
            "import noise\n\
             import wanted\n\n\
             fn main() -> Int:\n\
             \x20   answer\"value\"\n",
        ),
    ])
    .expect("impl reachability is keyed by resolved declaration identity");
}

#[test]
fn tag_reachability_retains_helpers_named_main_in_the_tag_module() {
    link(vec![
        (
            "tags",
            "import meta\n\n\
             comptime fn main() -> String:\n\
             \x20   \"42\"\n\n\
             pub comptime fn answer(parts: List(String), holes: List(String)) -> meta.ExprSyntax:\n\
             \x20   meta.expr_raw(main())\n",
        ),
        (
            "main",
            "import tags\n\n\
             fn main() -> Int:\n\
             \x20   answer\"value\"\n",
        ),
    ])
    .expect("the synthetic evaluator entry does not reserve helper names across modules");
}

#[test]
fn synthetic_tag_entry_avoids_user_module_name_collisions() {
    link(vec![
        (
            "@compiler:tag-entry",
            "fn harmless() -> Int:\n\
             \x20   0\n",
        ),
        (
            "main",
            "import meta\n\n\
             comptime fn answer(parts: List(String), holes: List(String)) -> meta.ExprSyntax:\n\
             \x20   meta.expr_raw(\"42\")\n\n\
             fn main() -> Int:\n\
             \x20   answer\"value\"\n",
        ),
    ])
    .expect("the synthetic evaluator selects an unused compiler module key");
}
