fn check(src: &str) -> Result<(), String> {
    witchy::resolve_std_only_checked(src)
        .map(|_| ())
        .map_err(|error| error.to_string())
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
