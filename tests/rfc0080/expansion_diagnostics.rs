//! RFC-0080 tagged-expansion provenance diagnostics after RFC-0167's
//! source-codegen cut. Structural builders replace dynamic source construction;
//! the visibility, reachability, and provenance contracts remain unchanged.

use std::collections::HashSet;

use witchy::{comptime, format, parser, pipeline, typeck};

#[test]
fn tag_free_expansion_preserves_ast_but_still_removes_hole_origin_wrappers() {
    use witchy::ast::{Block, Expr, Item, Stmt};

    let mut ordinary = parser::parse_module("fn answer() -> Int:\n    42\n")
        .expect("tag-free fixture parses");
    let before = ordinary.clone();
    let origins = comptime::expand_compile_time("comptime", &mut ordinary, &[])
        .expect("tag-free expansion succeeds");
    assert_eq!(ordinary, before);
    assert!(origins.nodes().is_empty() && origins.tagged_literals().is_empty());

    let mut wrapped = ordinary.clone();
    let Item::Function(function) = &mut wrapped.items[0] else { unreachable!() };
    function.body.stmts[0] = Stmt::Expr(Expr::Block(Block {
        stmts: vec![
            Stmt::Expr(Expr::Call {
                name: "@hole_origin".to_string(),
                args: vec![Expr::Int(0), Expr::Int(2), Expr::Int(5)],
            }),
            Stmt::Expr(Expr::Int(42)),
        ],
        lines: vec![2, 2],
        region: None,
    }));
    let origins = comptime::expand_compile_time("comptime", &mut wrapped, &[])
        .expect("compiler-owned hole wrapper is removed even without a tag");
    assert_eq!(wrapped, before);
    assert!(origins.nodes().is_empty() && origins.tagged_literals().is_empty());
}

#[test]
fn expansion_free_modules_still_finalize_compiler_payloads_on_every_call() {
    use witchy::ast::{CompilerTypeSyntax, Item, Type};

    let mut input = parser::parse_module(r#"
comptime fn helper() -> meta.ExprSyntax:
    quote expr:
        42

fn answer() -> Int:
    42
"#).expect("uninvoked compiler helper parses");
    assert!(!input.compiler_expr_syntax.is_empty());
    input.compiler_type_syntax = [false, true].into_iter().map(|runtime_identity| {
        CompilerTypeSyntax {
            handle: format!("type-{runtime_identity}"),
            ty: Type::Named("Int".into(), vec![]),
            definition_line: 1,
            runtime_identity,
        }
    }).collect();

    for _ in 0..2 {
        let mut compiler_module = input.clone();
        let origins = comptime::expand_compile_time("comptime", &mut compiler_module, &[])
            .expect("compiler context retains its payloads");
        assert_eq!(compiler_module, input);
        assert!(origins.nodes().is_empty());

        let mut runtime_module = input.clone();
        let origins = comptime::expand_compile_time("main", &mut runtime_module, &[])
            .expect("ordinary finalization still runs without expansion work");
        assert_eq!(runtime_module.items.len(), 1);
        assert!(matches!(&runtime_module.items[0], Item::Function(function)
            if function.name == "answer" && !function.comptime_only));
        assert_eq!(runtime_module.item_lines, input.item_lines[1..]);
        assert!(runtime_module.compiler_expr_syntax.is_empty());
        assert_eq!(runtime_module.compiler_type_syntax, input.compiler_type_syntax[1..]);
        assert!(origins.nodes().is_empty() && origins.tagged_literals().is_empty());
    }
}

#[test]
fn tag_presence_guard_keeps_all_expansion_positions_on_the_checked_path() {
    for source in [
        "let answer = missing\"value\"\n",
        "fn answer() -> Int:\n    missing\"value\"\n",
        "fn answer():\n    let nested = fn(): missing\"value\"\n",
        "trait Answer:\n    fn answer(self) -> Int:\n        missing\"value\"\n",
        "type Value:\n    Value\n\nimpl Value:\n    fn answer(self) -> Int:\n        missing\"value\"\n",
    ] {
        let mut module = parser::parse_module(source).expect("tag position fixture parses");
        let error = comptime::expand_compile_time("comptime", &mut module, &[])
            .expect_err("the guard must not skip a tag in any expansion position");
        assert!(error.contains("tagged literal `missing`"), "{source}: {error}");
        assert!(error.contains("is not defined"), "{source}: {error}");
    }
}

fn link(modules: Vec<(&str, &str)>) -> Result<(), String> {
    let user_modules = modules.iter().map(|(name, _)| (*name).to_string()).collect::<HashSet<_>>();
    let parsed = modules
        .into_iter()
        .map(|(name, source)| {
            (name.to_string(), parser::parse_module(source)
                .unwrap_or_else(|error| panic!("module `{name}` parses: {error}")))
        })
        .collect();
    pipeline::link_with_user_modules(parsed, "main", &user_modules)
        .map(|_| ())
        .map_err(|error| error.message)
}

#[test]
fn comptime_generated_tags_keep_imported_sibling_context() {
    link(vec![
        ("tags", "import meta\n\npub comptime fn answer(parts: List(String), holes: List(meta.TagHole)) -> meta.ExprSyntax:\n    meta.expr_int(42)\n"),
        ("main", r#"
import meta
import tags

comptime fn generated_item() -> meta.ItemSyntax:
    let call = meta.expr_tagged(meta.ident("answer"), [""], [])
    quote item:
        pub fn generated() -> Int:
            ${call}

comptime:
    emit_item(generated_item())

fn main() -> Int:
    generated()
"#),
    ]).expect("a comptime block can generate the first imported tag in a module");
}

#[test]
fn quote_hole_diagnostics_keep_definition_and_invocation_context() {
    let parsed = parser::parse_module(
        r#"
import meta

comptime fn bad(parts: List(String), holes: List(meta.TagHole)) -> meta.ExprSyntax:
    missing_builder()

fn main(console: Console):
    console.print("${bad"value ${1}"}")
"#,
    )
    .expect("parse structural diagnostic fixture");
    let error = pipeline::link(vec![("main".into(), parsed)], "main")
        .expect_err("structural expansion failure is diagnosed")
        .to_string();
    assert!(error.contains("tagged literal `bad`"), "{error}");
    assert!(error.contains("missing_builder"), "{error}");
}

#[test]
fn imported_structural_tag_error_names_both_source_locations() {
    let error = link(vec![
        (
            "tag_library",
            "import meta\n\npub comptime fn broken(parts: List(String), holes: List(meta.TagHole)) -> meta.ExprSyntax:\n    missing_helper()\n",
        ),
        (
            "main",
            "import tag_library\n\nfn main(console: Console):\n    console.print(\"before\")\n    console.print(\"${broken\"value\"}\")\n",
        ),
    ])
    .expect_err("the imported structural tag body does not type-check");

    assert!(error.contains("tagged literal `broken` at invocation line 5"), "{error}");
    assert!(error.contains("defined in module `tag_library` at line 3"), "{error}");
    assert!(error.contains("missing_helper"), "{error}");
}

#[test]
fn private_and_transitive_tags_remain_inaccessible() {
    let private = link(vec![
        ("tags", "import meta\n\ncomptime fn hidden(parts: List(String), holes: List(meta.TagHole)) -> meta.ExprSyntax:\n    meta.expr_int(0)\n"),
        ("main", "import tags\n\nfn main(console: Console):\n    console.print(\"${hidden\"value\"}\")\n"),
    ]).expect_err("private imported tag rejects");
    assert!(private.contains("not public in a directly imported module"), "{private}");

    let transitive = link(vec![
        ("leaf", "import meta\n\npub comptime fn distant(parts: List(String), holes: List(meta.TagHole)) -> meta.ExprSyntax:\n    meta.expr_int(0)\n"),
        ("middle", "import leaf\n\npub fn value() -> Int:\n    1\n"),
        ("main", "import middle\n\nfn main(console: Console):\n    console.print(\"${distant\"value\"}\")\n"),
    ]).expect_err("transitive tag rejects");
    assert!(transitive.contains("not public in a directly imported module"), "{transitive}");
}

#[test]
fn tag_signature_contracts_remain_enforced() {
    let legacy_holes = link(vec![("main", "import meta\n\ncomptime fn legacy(parts: List(String), holes: List(String)) -> meta.ExprSyntax:\n    meta.expr_int(0)\n\nfn main() -> Int:\n    legacy\"value\"\n")])
        .expect_err("string marker tag ABI rejects");
    assert!(legacy_holes.contains("must accept holes as List(meta.TagHole)"), "{legacy_holes}");

    let legacy_builder = link(vec![("main", "import meta\n\ncomptime fn removed(parts: List(String), holes: List(meta.TagHole)) -> meta.ExprSyntax:\n    meta.expr_tag_hole(\"__witchy_hole_0\")\n\nfn main() -> Int:\n    removed\"value\"\n")])
        .expect_err("string marker builder rejects");
    assert!(legacy_builder.contains("expr_tag_hole"), "{legacy_builder}");

    let string_return = link(vec![("main", "comptime fn old_tag(parts: List(String), holes: List(meta.TagHole)) -> String:\n    \"0\"\n\nfn main(console: Console):\n    console.print(\"${old_tag\"value\"}\")\n")])
        .expect_err("String tags reject");
    assert!(string_return.contains("tag `old_tag` must return meta.ExprSyntax"), "{string_return}");

    let runtime_tag = link(vec![("main", "import meta\n\nfn runtime_tag(parts: List(String), holes: List(meta.TagHole)) -> meta.ExprSyntax:\n    meta.expr_int(0)\n\nfn main(console: Console):\n    console.print(\"${runtime_tag\"value\"}\")\n")])
        .expect_err("runtime tag rejects");
    assert!(runtime_tag.contains("tag `runtime_tag` must be declared `comptime`"), "{runtime_tag}");
}

#[test]
fn dropped_and_duplicated_structural_holes_keep_their_evaluation_contracts() {
    link(vec![("main", "import meta\n\ncomptime fn broken(parts: List(String), holes: List(meta.TagHole)) -> meta.ExprSyntax:\n    missing_helper()\n\ncomptime fn drop_hole(parts: List(String), holes: List(meta.TagHole)) -> meta.ExprSyntax:\n    meta.expr_int(0)\n\nfn main(console: Console):\n    console.print(\"${drop_hole\"${broken\"unused\"}\"}\")\n")])
        .expect("a dropped structural hole is not evaluated");

    let source = "import meta\n\ncomptime fn fresh_ref(parts: List(String), holes: List(meta.TagHole)) -> meta.ExprSyntax:\n    meta.expr_name(meta.fresh(\"dup\"))\n\ncomptime fn duplicate(parts: List(String), holes: List(meta.TagHole)) -> meta.ExprSyntax:\n    let hole = meta.expr_tag_hole_typed(holes.at(0))\n    meta.expr_tuple([hole, hole])\n\nfn main():\n    let pair = \"${duplicate\"${fresh_ref\"value\"}\"}\"\n";
    let linked = pipeline::link(vec![("main".into(), parser::parse_module(source).expect("parse"))], "main")
        .expect("duplicated structural holes expand");
    let expanded = format::module(&linked, &[]);
    let mut names = expanded.match_indices("__witchy_fresh_").map(|(start, _)| {
        expanded[start..].chars().take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_').collect::<String>()
    }).collect::<Vec<_>>();
    names.sort();
    names.dedup();
    assert_eq!(names.len(), 2, "each placement receives a fresh invocation: {expanded}");
}

#[test]
fn structural_hole_order_and_forgery_diagnostics_remain_fail_closed() {
    let error = link(vec![("main", "import meta\n\ncomptime fn fail_a(parts: List(String), holes: List(meta.TagHole)) -> meta.ExprSyntax:\n    missing_a()\n\ncomptime fn fail_b(parts: List(String), holes: List(meta.TagHole)) -> meta.ExprSyntax:\n    missing_b()\n\ncomptime fn reverse(parts: List(String), holes: List(meta.TagHole)) -> meta.ExprSyntax:\n    meta.expr_binary(\"+\", meta.expr_tag_hole_typed(holes.at(1)), meta.expr_tag_hole_typed(holes.at(0)))\n\nfn main(console: Console):\n    console.print(\"${reverse\"${fail_a\"a\"} middle ${fail_b\"b\"}\"}\")\n")])
        .expect_err("first placed structural hole fails first");
    assert!(error.contains("tagged literal `fail_b`") && !error.contains("tagged literal `fail_a`"), "{error}");

    let parsed = parser::parse_module("fn main() -> Int:\n    region:\n        __witchy_hole_origin(0, 1, 2)\n        42\n").expect("forgery parses ordinarily");
    let linked = pipeline::link(vec![("main".into(), parsed)], "main").expect("link ordinary call");
    let error = typeck::check(&linked).expect_err("forged compiler marker rejects");
    assert!(error.to_string().contains("unknown function `__witchy_hole_origin`"), "{error}");
}

#[test]
fn definition_site_helpers_and_direct_imports_remain_available() {
    link(vec![
        ("collision", "import meta\n\ncomptime fn helper() -> meta.ExprSyntax:\n    meta.expr_int(0)\n"),
        ("tags", "import meta\n\ncomptime fn helper() -> meta.ExprSyntax:\n    meta.expr_int(42)\n\npub comptime fn answer(parts: List(String), holes: List(meta.TagHole)) -> meta.ExprSyntax:\n    helper()\n"),
        ("main", "import collision\nimport tags\n\nfn main() -> Int:\n    answer\"ignored\"\n"),
    ]).expect("tag helper resolution uses its definition module");

    link(vec![
        ("support", "import meta\n\npub comptime fn build() -> meta.ExprSyntax:\n    meta.expr_int(42)\n"),
        ("tags", "import meta\nimport support\n\npub comptime fn answer(parts: List(String), holes: List(meta.TagHole)) -> meta.ExprSyntax:\n    support.build()\n"),
        ("main", "import tags\n\nfn main() -> Int:\n    answer\"value\"\n"),
    ]).expect("imported comptime helper remains reachable");
}

#[test]
fn tag_evaluator_reentry_and_tagged_constants_remain_rejected() {
    let recursive = link(vec![("main", "import meta\n\ncomptime fn recursive(parts: List(String), holes: List(meta.TagHole)) -> meta.ExprSyntax:\n    recursive\"again\"\n\nfn main() -> Int:\n    recursive\"start\"\n")])
        .expect_err("tag evaluator recursion rejects");
    assert!(recursive.contains("tag evaluator function `main.recursive` contains a tagged literal"), "{recursive}");

    let constant = link(vec![("main", "import meta\n\ncomptime fn inner(parts: List(String), holes: List(meta.TagHole)) -> meta.ExprSyntax:\n    meta.expr_int(42)\n\nlet generated = inner\"nested\"\n\ncomptime fn outer(parts: List(String), holes: List(meta.TagHole)) -> meta.ExprSyntax:\n    generated\n\nfn main() -> Int:\n    outer\"value\"\n")])
        .expect_err("tagged evaluator constant rejects");
    assert!(constant.contains("tag evaluator constant `main.generated` contains a tagged literal"), "{constant}");
}

#[test]
fn nested_structural_tags_keep_definition_site_and_hole_ancestry() {
    // These cases link separate source graphs and share no outputs. Run their
    // independent frontend work together so this conformance test does not
    // serialize four full compiler walks.
    let (hole_error, private_nested, imported_nested, nested_hole) =
        std::thread::scope(|scope| {
            let hole_error = scope.spawn(|| {
                link(vec![("main", "import meta\n\ncomptime fn inner(parts: List(String), holes: List(meta.TagHole)) -> meta.ExprSyntax:\n    missing_inner()\n\ncomptime fn passthrough(parts: List(String), holes: List(meta.TagHole)) -> meta.ExprSyntax:\n    meta.expr_tag_hole_typed(holes.at(0))\n\nfn main(console: Console):\n    console.print(\"${passthrough\"${inner\"nested\"}\"}\")\n")])
            });
            let private_nested = scope.spawn(|| {
                link(vec![
                    ("tags", "import meta\n\ncomptime fn inner(parts: List(String), holes: List(meta.TagHole)) -> meta.ExprSyntax:\n    meta.expr_int(42)\n\npub comptime fn outer(parts: List(String), holes: List(meta.TagHole)) -> meta.ExprSyntax:\n    meta.expr_tagged(meta.ident(\"inner\"), [\"\"], [])\n"),
                    ("main", "import tags\n\nfn main() -> Int:\n    outer\"value\"\n"),
                ])
            });
            let imported_nested = scope.spawn(|| {
                link(vec![
                    ("inner_tags", "import meta\n\npub comptime fn inner(parts: List(String), holes: List(meta.TagHole)) -> meta.ExprSyntax:\n    meta.expr_int(42)\n"),
                    ("outer_tags", "import inner_tags\nimport meta\n\npub comptime fn outer(parts: List(String), holes: List(meta.TagHole)) -> meta.ExprSyntax:\n    meta.expr_tagged(meta.ident(\"inner\"), [\"\"], [])\n"),
                    ("main", "import outer_tags\n\nfn main() -> Int:\n    outer\"value\"\n"),
                ])
            });
            let nested_hole = scope.spawn(|| {
                let module = parser::parse_module(
                    "import meta\n\ncomptime fn inner(parts: List(String), holes: List(meta.TagHole)) -> meta.ExprSyntax:\n    meta.expr_tag_hole_typed(holes.at(0))\n\npub comptime fn outer(parts: List(String), holes: List(meta.TagHole)) -> meta.ExprSyntax:\n    meta.expr_tagged(meta.ident(\"inner\"), [\"\", \"\"], [meta.expr_tag_hole_typed(holes.at(0))])\n\nfn main() -> Int:\n    outer\"${41}\"\n",
                )
                .expect("parse structural nested-hole fixture");
                pipeline::link(vec![("main".into(), module)], "main")
                .expect("owned nested tag hole remains structural")
            });
            (
                hole_error.join().expect("hole diagnostic worker"),
                private_nested.join().expect("private nested tag worker"),
                imported_nested.join().expect("imported nested tag worker"),
                nested_hole.join().expect("nested hole worker"),
            )
        });
    let hole_error = hole_error.expect_err("nested structural hole fails with ancestry");
    assert!(hole_error.contains("tagged literal `inner`") && hole_error.contains("tagged literal `passthrough`") && hole_error.contains("expansion trace:"), "{hole_error}");
    assert!(hole_error.contains("tagged literal `passthrough` at invocation line 10"), "{hole_error}");
    assert!(hole_error.contains("hole 1 at hole-local line 1, column 15"), "{hole_error}");

    private_nested.expect("definition-site private nested tag remains available");
    imported_nested.expect("definition-site imported nested tag remains available");

    let linked = nested_hole;
    typeck::check(&linked).expect("typecheck owned nested tag hole");
    assert_eq!(
        witchy::interpreter::run_module(linked, ".", Vec::new())
            .expect("interpret owned nested tag hole"),
        Vec::<String>::new(),
    );
}

#[test]
fn structural_hole_order_reports_the_first_failing_placement() {
    let error = link(vec![(
        "main",
        "import meta\n\ncomptime fn fail_a(parts: List(String), holes: List(meta.TagHole)) -> meta.ExprSyntax:\n    missing_a()\n\ncomptime fn fail_b(parts: List(String), holes: List(meta.TagHole)) -> meta.ExprSyntax:\n    missing_b()\n\ncomptime fn reverse(parts: List(String), holes: List(meta.TagHole)) -> meta.ExprSyntax:\n    meta.expr_binary(\"+\", meta.expr_tag_hole_typed(holes.at(1)), meta.expr_tag_hole_typed(holes.at(0)))\n\nfn main(console: Console):\n    console.print(\"${reverse\"${fail_a\"a\"} middle ${fail_b\"b\"}\"}\")\n",
    )])
    .expect_err("the first placed structural hole fails first");
    assert!(error.contains("tagged literal `fail_b`"), "{error}");
    assert!(!error.contains("tagged literal `fail_a`"), "{error}");
}

#[test]
fn tag_reachability_retains_constants_constructors_traits_and_helper_names() {
    link(vec![("main", "import meta\n\nlet answer = meta.expr_int(42)\n\ncomptime fn tag(parts: List(String), holes: List(meta.TagHole)) -> meta.ExprSyntax:\n    answer\n\nfn main() -> Int:\n    tag\"value\"\n")])
        .expect("top-level syntax constant retained");

    link(vec![
        ("support", "type Wrapped:\n    Wrapped(Int)\n"),
        ("tags", "from support import Wrapped\nimport meta\n\npub comptime fn answer(parts: List(String), holes: List(meta.TagHole)) -> meta.ExprSyntax:\n    match Wrapped(42):\n        Wrapped(value) -> meta.expr_int(value)\n"),
        ("main", "import tags\n\nfn main() -> Int:\n    answer\"value\"\n"),
    ]).expect("from-import constructor retained");

    link(vec![("main", "import meta\n\ntrait Render:\n    fn render(self) -> Int\n\ntype Value:\n    Value(Int)\n\nimpl Render for Value:\n    fn render(self) -> Int:\n        42\n\ncomptime fn answer(parts: List(String), holes: List(meta.TagHole)) -> meta.ExprSyntax:\n    meta.expr_int(Value(0).render())\n\nfn main() -> Int:\n    answer\"value\"\n")])
        .expect("custom trait implementation retained");

    link(vec![
        ("tags", "import meta\n\ncomptime fn main() -> meta.ExprSyntax:\n    meta.expr_int(42)\n\npub comptime fn answer(parts: List(String), holes: List(meta.TagHole)) -> meta.ExprSyntax:\n    main()\n"),
        ("main", "import tags\n\nfn main() -> Int:\n    answer\"value\"\n"),
    ]).expect("helper named main remains available");
}

#[test]
fn tag_resolution_stays_identity_scoped_and_ambiguous_names_reject() {
    let tag_module = "import meta\n\npub comptime fn duplicate(parts: List(String), holes: List(meta.TagHole)) -> meta.ExprSyntax:\n    meta.expr_int(42)\n";
    let ambiguous = link(vec![
        ("alpha", tag_module), ("beta", tag_module),
        ("main", "import alpha\nimport beta\n\nfn main() -> Int:\n    duplicate\"value\"\n"),
    ]).expect_err("ambiguous direct imports reject");
    assert!(ambiguous.contains("tag `duplicate` is ambiguous across directly imported modules: alpha, beta"), "{ambiguous}");

    link(vec![
        ("noise", "trait Render:\n    fn render(self) -> Int\n\ntype Value:\n    Value(Int)\n\nimpl Render for Value:\n    fn render(self) -> Int:\n        0\n"),
        ("wanted", "import meta\n\ntrait Render:\n    fn render(self) -> Int\n\ntype Value:\n    Value(Int)\n\nimpl Render for Value:\n    fn render(self) -> Int:\n        42\n\npub comptime fn answer(parts: List(String), holes: List(meta.TagHole)) -> meta.ExprSyntax:\n    meta.expr_int(Value(0).render())\n"),
        ("main", "import noise\nimport wanted\n\nfn main() -> Int:\n    answer\"value\"\n"),
    ]).expect("same-named unrelated impl is not retained");
}

#[test]
fn synthetic_tag_entry_collision_remains_safe() {
    link(vec![
        ("@compiler:tag-entry", "fn harmless() -> Int:\n    0\n"),
        ("main", "import meta\n\ncomptime fn answer(parts: List(String), holes: List(meta.TagHole)) -> meta.ExprSyntax:\n    meta.expr_int(42)\n\nfn main() -> Int:\n    answer\"value\"\n"),
    ]).expect("synthetic evaluator chooses an unused module key");
}

#[test]
fn raw_source_parse_diagnostics_are_replaced_by_retired_api_diagnostics() {
    let mut parsed = parser::parse_module(
        r#"
import meta

comptime:
    meta.expr_raw("(")
    emit_item(quote item:
        fn must_not_merge() -> Int:
            0
    )

fn main() -> Int:
    0
"#,
    )
    .expect("legacy spelling stays parseable as a normal unresolved call");
    let error = comptime::expand_compile_time("main", &mut parsed, &[])
        .expect_err("raw parse bridge is retired");
    assert!(error.contains("module `meta`") && error.contains("expr_raw"), "{error}");
    assert!(!parsed.items.iter().any(|item| {
        matches!(item, witchy::ast::Item::Function(function) if function.name == "must_not_merge")
    }));
}
