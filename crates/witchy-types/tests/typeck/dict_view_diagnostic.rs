//! RFC-0167 row G: the checked retained-child carrier crosses a dictionary's
//! `var unique` callable boundary.

use super::source::checked;

#[test]
fn string_view_dictionary_has_a_checked_callable_carrier() {
    let source = r#"
fn main() -> Int:
    let value = "snapshot"
    var d: Dict(String, &String) = dict.new()
    let _ = d.insert("key", &value)
    d.length()
"#;
    checked(source).expect("the recursively proved dictionary carrier crosses insert");
}
