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
    witchy_interp::pipeline::link_with_user_modules(parsed, "main", &user_modules)
        .map_err(|error| error.message)
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
