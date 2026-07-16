//! RFC-0080 tagged-expansion provenance diagnostics.

use std::collections::HashSet;

use witchy::{parser, pipeline};

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
