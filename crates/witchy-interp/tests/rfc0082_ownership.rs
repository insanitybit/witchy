//! RFC-0082 ownership boundary conformance against RFC-0083 loans.

#[path = "../../../tests/support/authenticated.rs"]
mod authenticated;
use authenticated::checked_result;

#[test]
fn borrowed_views_require_explicit_materialization_before_dynamic_storage() {
    let source = r#"
mode opt

import dynamic

fn view(text: let('a) String) -> View(String, 'a):
    text

fn main(console: Console):
    var owner = "borrowed"
    let borrowed = view(owner)
    let packed = dynamic.dynamic(borrowed)
    console.print(dynamic.type_name(dynamic.type_of(packed)))
"#;
    let error = checked_result(source)
        .expect_err("borrowed Dynamic payload must be rejected")
        .to_string();
    assert!(error.contains("cannot be stored in Dynamic"), "{error}");
    assert!(error.contains(".owned()"), "{error}");
}

#[test]
fn dynamic_construction_consumes_unique_payloads() {
    let source = r#"
import dynamic

fn main(console: Console):
    let values: unique List(Int) = [1, 2]
    let packed = dynamic.dynamic(values)
    console.print("${list.length(values)}")
    console.print(dynamic.type_name(dynamic.type_of(packed)))
"#;
    let error = checked_result(source)
        .expect_err("Dynamic must consume its owned payload")
        .to_string();
    assert!(error.contains("after it was moved"), "{error}");
}
