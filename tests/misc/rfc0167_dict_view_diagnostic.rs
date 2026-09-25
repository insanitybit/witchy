//! RFC-0167 row G: dictionary values need an admitted collection carrier.
//! `Dict(String, &String)` must fail during canonical ABI checking with a named
//! reason until dictionary operations authenticate retained String view roots.

#[test]
fn string_view_dictionary_has_a_named_canonical_refusal() {
    let source = r#"
fn main() -> Int:
    let value = "snapshot"
    var d: Dict(String, &String) = dict.new()
    let _ = d.insert("key", &value)
    d.length()
"#;
    let refusal = witchy::resolve_std_only_checked(source)
        .expect_err("a dictionary of String views has no canonical ABI key");
    assert_eq!(
        refusal.to_string(),
        "type error: canonical callable ABI is invalid: nested qualified rooted-view ABI has no structural key"
    );
}
