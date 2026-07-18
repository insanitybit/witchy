fn check(src: &str) -> Result<(), String> {
    let linked = witchy::resolve_std_only(src)?;
    witchy::typeck::check(&linked).map_err(|error| error.to_string())
}

#[test]
fn unused_async_function_enforces_its_completed_value_type() {
    let src = r#"
pub async fn wrong() -> Int:
    "not an int"

fn main(console: Console):
    console.print("ok")
"#;
    let error = check(src).expect_err("an unused async declaration must keep its return contract");
    assert!(
        error.contains("Int") && error.contains("String"),
        "async return mismatch should name both types: {error}",
    );
}

#[test]
fn unused_async_inherent_method_enforces_its_completed_value_type() {
    let src = r#"
type Counter:
    value: Int

impl Counter:
    async fn wrong(self) -> Int:
        "not an int"

fn main(console: Console):
    console.print("ok")
"#;
    let error = check(src).expect_err("an unused async method must keep its return contract");
    assert!(
        error.contains("Int") && error.contains("String"),
        "async method return mismatch should name both types: {error}",
    );
}

#[test]
fn async_main_cannot_hide_a_result_return_annotation() {
    let src = r#"
async fn main(console: Console) -> Result(Int, String):
    Ok(1)
"#;
    let error = check(src).expect_err("async main must not bypass the main return contract");
    assert!(
        error.contains("async fn `main`")
            && error.contains("Result(Int, String)")
            && error.contains("Task(())"),
        "async main diagnostic should explain the executor contract: {error}",
    );
}
