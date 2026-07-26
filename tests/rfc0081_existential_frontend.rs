//! RFC-0081 existential (`dyn Trait`) public frontend contract on the
//! full linked pipeline — resolved type identity across aliases and modules,
//! existential-safety diagnostics, transitive capability-payload rejection,
//! and successful public checking after runtime dispatch landed.

use std::collections::HashSet;

use witchy::runtime::{Capabilities, Runtime};

fn check(source: &str) -> Result<(), String> {
    witchy::resolve_std_only_checked(source)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn link_and_check(modules: Vec<(&str, &str)>) -> Result<(), String> {
    let linked = link_modules(modules)?;
    witchy::typeck::check(&linked).map_err(|error| error.to_string())
}

fn link_modules(modules: Vec<(&str, &str)>) -> Result<witchy::ast::Module, String> {
    let user_modules: HashSet<String> =
        modules.iter().map(|(name, _)| name.to_string()).collect();
    let parsed = modules
        .into_iter()
        .map(|(name, source)| {
            let module = witchy::parser::parse_module(source)
                .unwrap_or_else(|e| panic!("module `{name}` parses: {e}"));
            (name.to_string(), module)
        })
        .collect();
    witchy::pipeline::link_with_user_modules(parsed, "main", &user_modules)
        .map_err(|error| error.message)
}

/// (a) One existential identity across an alias and across modules: module
/// `boxed` names `dyn Render` through `type Boxed = …` (the trait lives in a
/// third, shared module), while `main` spells `dyn Render` directly. The two
/// spellings must interchange as parameter and return types without any
/// unification/type error.
#[test]
fn rfc0081_dyn_identity_is_stable_across_aliases_and_modules() {
    let modules = vec![
        (
            "render",
            "trait Render:\n    fn render(let self) -> String\n",
        ),
        (
            "boxed",
            "import render\n\n\
             type Boxed = dyn Render\n\n\
             pub fn wrap(x: Boxed) -> Boxed:\n    x\n",
        ),
        (
            "main",
            "import render\nimport boxed\n\n\
             fn take(x: dyn Render) -> dyn Render:\n    boxed.wrap(x)\n\n\
             fn main(console: Console):\n    console.print(\"hi\")\n",
        ),
    ];
    let linked = link_modules(modules.clone()).expect("link existential identities");
    let render_trait = linked
        .items
        .iter()
        .find_map(|item| match item {
            witchy::ast::Item::Trait(tr) if tr.name.ends_with("Render") => Some(tr),
            _ => None,
        })
        .expect("linked Render trait");
    assert_eq!(render_trait.name, "render.Render");
    for function_name in ["boxed.wrap", "main.take"] {
        let function = linked
            .items
            .iter()
            .find_map(|item| match item {
                witchy::ast::Item::Function(function) if function.name == function_name => {
                    Some(function)
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("linked function `{function_name}`"));
        let witchy::ast::Type::Dyn(name, _) =
            function.params[0].ty.as_ref().expect("dyn parameter")
        else {
            panic!("expected dyn parameter on `{function_name}`");
        };
        assert_eq!(name, "render.Render");
    }

    link_and_check(modules).expect("the linked existential identities must check");
}

#[test]
fn rfc0081_same_spelled_traits_have_distinct_linked_identity() {
    let linked = link_modules(vec![
        (
            "left",
            "trait Render:\n    fn render(let self) -> String\n\n\
             pub fn hold(x: dyn Render) -> dyn Render:\n    x\n",
        ),
        (
            "right",
            "trait Render:\n    fn render(let self) -> String\n\n\
             pub fn hold(x: dyn Render) -> dyn Render:\n    x\n",
        ),
        (
            "main",
            "import left\nimport right\n\n\
             fn main(console: Console):\n    console.print(\"hi\")\n",
        ),
    ])
    .expect("same-spelled trait declarations link");

    let trait_names: HashSet<&str> = linked
        .items
        .iter()
        .filter_map(|item| match item {
            witchy::ast::Item::Trait(tr) => Some(tr.name.as_str()),
            _ => None,
        })
        .collect();
    assert!(trait_names.contains("left.Render"), "{trait_names:?}");
    assert!(trait_names.contains("right.Render"), "{trait_names:?}");

    for (function_name, expected_trait) in
        [("left.hold", "left.Render"), ("right.hold", "right.Render")]
    {
        let function = linked
            .items
            .iter()
            .find_map(|item| match item {
                witchy::ast::Item::Function(function) if function.name == function_name => {
                    Some(function)
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("linked function `{function_name}`"));
        let witchy::ast::Type::Dyn(name, _) =
            function.params[0].ty.as_ref().expect("dyn parameter")
        else {
            panic!("expected dyn parameter on `{function_name}`");
        };
        assert_eq!(name, expected_trait);
    }
}

#[test]
fn rfc0081_same_spelled_traits_dispatch_independently_on_both_backends() {
    let linked = link_modules(vec![
        (
            "left",
            "trait Render:\n    fn render(let self) -> String\n\n\
             impl Render for Int:\n    fn render(let self) -> String:\n        \"left\"\n\n\
             pub fn show(x: a) -> String where a: Render:\n    x.render()\n",
        ),
        (
            "right",
            "trait Render:\n    fn render(let self) -> String\n\n\
             impl Render for Int:\n    fn render(let self) -> String:\n        \"right\"\n\n\
             pub fn show(x: a) -> String where a: Render:\n    x.render()\n",
        ),
        (
            "main",
            "import left\nimport right\n\n\
             fn main(console: Console):\n\
             \x20   console.print(left.show(1))\n\
             \x20   console.print(right.show(1))\n",
        ),
    ])
    .expect("same-spelled trait program links");
    witchy::typeck::check(&linked).expect("same-spelled trait program checks");
    let expected = vec!["left".to_string(), "right".to_string()];
    assert_eq!(
        witchy::interpreter::run_module(linked.clone(), ".", Vec::new())
            .expect("interpret same-spelled traits"),
        expected
    );

    let wasm = witchy::codegen::compile_module_binary(&linked)
        .expect_lowered("compile same-spelled traits");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut actor = runtime
        .spawn(
            &wasm,
            Capabilities { print: true, quiet: true, ..Default::default() },
            64,
        )
        .expect("spawn");
    actor.run().expect("run compiled same-spelled traits");
    assert_eq!(actor.output(), expected);
}

/// (b) Equivalent trait arguments give the same identity: `type P =
/// dyn Convert(Int)` and a directly spelled `dyn Convert(Int)` interchange as
/// parameter/return with no mismatch.
#[test]
fn rfc0081_dyn_identity_with_arguments_matches_its_alias() {
    check(
        "trait Convert(t):\n    fn convert(let self) -> t\n\n\
         type P = dyn Convert(Int)\n\n\
         fn through(x: P) -> dyn Convert(Int):\n    x\n\n\
         fn back(x: dyn Convert(Int)) -> P:\n    x\n\n\
         fn main(console: Console):\n    console.print(\"hi\")\n",
    )
    .expect("equivalent closed existential arguments must check");
}

/// (c) A specific existential-safety violation in a linked multi-module
/// program surfaces the safety diagnostic.
#[test]
fn rfc0081_safety_violation_in_linked_module_is_rejected() {
    let err = link_and_check(vec![
        (
            "shapes",
            "trait Maker:\n    fn make() -> Int\n",
        ),
        (
            "main",
            "import shapes\n\n\
             fn f(x: dyn Maker) -> Int:\n    1\n\n\
             fn main(console: Console):\n    console.print(\"hi\")\n",
        ),
    ])
    .expect_err("a receiver-less method blocks dyn use");
    assert!(
        err.contains("is not existential-safe as `dyn Maker`")
            && err.contains("method `make` has no receiver"),
        "{err}"
    );
}

/// (d) A capability nested in a record DEFINED IN ANOTHER MODULE is still
/// rejected at the explicit erasure site: the transitive payload walk sees
/// through the linked nominal type.
#[test]
fn rfc0081_transitive_cap_payload_from_another_module_is_rejected() {
    let err = link_and_check(vec![
        (
            "store",
            "type Holder:\n    dir: Dir\n\n\
             pub fn hold(dir: Dir) -> Holder:\n    Holder(dir)\n",
        ),
        (
            "main",
            "import store\n\n\
             trait Render:\n    fn render(let self) -> String\n\n\
             fn main(dir: Dir, console: Console):\n\
             \x20   let h = store.hold(dir)\n\
             \x20   let r = h as dyn Render\n\
             \x20   console.print(\"hi\")\n",
        ),
    ])
    .expect_err("a Dir nested in a linked record cannot be erased");
    assert!(
        err.contains("`as dyn Render`")
            && err.contains("carries a `Dir` capability")
            && err.contains("capability-carrying existential payloads are rejected (RFC-0081)"),
        "{err}"
    );
}

/// (e) The shared public checker accepts a valid `dyn` signature. Backend
/// preparation still rechecks compiler-owned calls and preserves any failure.
#[test]
fn rfc0081_public_check_accepts_valid_existential_signatures() {
    witchy::resolve_std_only_checked(
        "trait Render:\n    fn render(let self) -> String\n\n\
         fn describe(part: dyn Render) -> String:\n    \"static\"\n\n\
         fn main(console: Console):\n    console.print(\"hi\")\n",
    )
    .expect("the program links");
}
