//! RFC-0167 row G: dictionary values need an admitted collection carrier.
//! `Dict(String, &String)` must fail at canonical admission with a named
//! reason until dictionary operations authenticate retained String view roots.

use witchy::codegen;

#[test]
fn string_view_dictionary_has_a_named_canonical_refusal() {
    let source = r#"
fn main() -> Int:
    let value = "snapshot"
    var d: Dict(String, &String) = dict.new()
    let _ = d.insert("key", &value)
    d.length()
"#;
    let checked = witchy::resolve_std_only_checked(source)
        .expect("view dictionary reaches canonical carrier admission");
    let refusal = codegen::compile_checked_module_artifact(&checked)
        .expect_unsupported("a dictionary of String views has no collection carrier");
    assert!(refusal.to_string().contains("Dict value `&String` has no admitted collection carrier"),
        "refusal names the missing carrier: {refusal}");
}

