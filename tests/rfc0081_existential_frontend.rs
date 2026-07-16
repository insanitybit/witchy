//! RFC-0081 slice 1: the existential (`dyn Trait`) FRONTEND contract on the
//! full linked pipeline — resolved type identity across aliases and modules,
//! existential-safety diagnostics, transitive capability-payload rejection,
//! and the feature-stage gate that keeps every `dyn` program away from both
//! backends until the witness/runtime slice lands.

use std::collections::HashSet;

fn check(source: &str) -> Result<(), String> {
    let linked = witchy::resolve_std_only(source)?;
    witchy::typeck::check(&linked).map_err(|error| error.to_string())
}

fn link_and_check(modules: Vec<(&str, &str)>) -> Result<(), String> {
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
    let linked = witchy::pipeline::link_with_user_modules(parsed, "main", &user_modules)
        .map_err(|error| error.message)?;
    witchy::typeck::check(&linked).map_err(|error| error.to_string())
}

/// The canonical feature-stage diagnostic, as rendered through
/// `Display for TypeError` (the `type error: ` prefix).
fn stage_gate(canonical: &str) -> String {
    format!(
        "type error: `{canonical}`: existential values cannot be constructed or \
         dispatched yet — RFC-0081's witness/runtime slice has not landed; the \
         frontend contract (parsing, identity, existential safety) is checked"
    )
}

/// (a) One existential identity across an alias and across modules: module
/// `boxed` names `dyn Render` through `type Boxed = …` (the trait lives in a
/// third, shared module), while `main` spells `dyn Render` directly. The two
/// spellings must interchange as parameter and return types WITHOUT any
/// unification/type error — so the only diagnostic left is the staging gate
/// in its canonical form.
#[test]
fn rfc0081_dyn_identity_is_stable_across_aliases_and_modules() {
    let err = link_and_check(vec![
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
    ])
    .expect_err("dyn programs are staged until the witness slice lands");
    assert_eq!(err, stage_gate("dyn Render"));
}

/// (b) Equivalent trait arguments give the same identity: `type P =
/// dyn Convert(Int)` and a directly spelled `dyn Convert(Int)` interchange as
/// parameter/return with no mismatch, leaving only the gate.
#[test]
fn rfc0081_dyn_identity_with_arguments_matches_its_alias() {
    let err = check(
        "trait Convert(t):\n    fn convert(let self) -> t\n\n\
         type P = dyn Convert(Int)\n\n\
         fn through(x: P) -> dyn Convert(Int):\n    x\n\n\
         fn back(x: dyn Convert(Int)) -> P:\n    x\n\n\
         fn main(console: Console):\n    console.print(\"hi\")\n",
    )
    .expect_err("dyn programs are staged");
    assert_eq!(err, stage_gate("dyn Convert(Int)"));
}

/// (c) A specific existential-safety violation in a linked multi-module
/// program surfaces the safety diagnostic — the specific error beats the gate.
#[test]
fn rfc0081_safety_violation_in_linked_module_beats_the_stage_gate() {
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
    assert!(
        !err.contains("cannot be constructed or dispatched yet"),
        "the specific safety diagnostic must beat the stage gate: {err}"
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

/// (e) The gate fires from `typeck::check` ITSELF — the shared frontend —
/// which both backends sit behind. Because `check` errors here, neither
/// `interpreter::run_module` nor `codegen::compile_module_binary` is ever
/// reached with a `dyn` type: no backend can lower an existential before the
/// witness slice lands, and no parity divergence is possible.
#[test]
fn rfc0081_stage_gate_fires_in_typeck_before_any_backend() {
    let linked = witchy::resolve_std_only(
        "trait Render:\n    fn render(let self) -> String\n\n\
         fn describe(part: dyn Render) -> String:\n    \"static\"\n\n\
         fn main(console: Console):\n    console.print(\"hi\")\n",
    )
    .expect("the program links");
    let err = witchy::typeck::check(&linked).expect_err("check gates dyn programs");
    assert_eq!(err.to_string(), stage_gate("dyn Render"));
}
