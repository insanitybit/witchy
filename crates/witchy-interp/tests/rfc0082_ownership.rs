//! RFC-0082 ownership boundary conformance against RFC-0083 loans.

#[path = "../../../tests/support/authenticated.rs"]
mod authenticated;
use authenticated::checked_result;

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
    assert!(
        error.contains("use of unique value `values` after it was transferred"),
        "{error}"
    );
}
