//! JWT key selection preserves typed failures through std and both backends.

use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_witchy");

fn run(args: &[&str]) -> Output {
    Command::new(BIN).args(args).output().expect("spawn witchy")
}

#[test]
fn require_kid_exposes_a_matchable_missing_kid_error() {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "witchy-typed-jwt-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("main.witchy");
    std::fs::write(
        &source,
        r#"import jwt

fn classify(e: jwt.JwtError) -> String:
    match e:
        jwt.HeaderMissingKid -> "missing-kid"
        _ -> "other"

fn main(console: Console):
    match jwt.require_kid("eyJhbGciOiJSUzI1NiJ9.e30.AA"):
        Ok(kid) -> console.print(kid)
        Err(e) -> console.print(classify(e))
"#,
    )
    .unwrap();

    let path = source.to_str().unwrap();
    let output = run(&[path]);
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "missing-kid\n");

    let parity = run(&["parity", path]);
    assert!(
        parity.status.success()
            && String::from_utf8_lossy(&parity.stdout).contains("outcome=agree"),
        "{}{}",
        String::from_utf8_lossy(&parity.stdout),
        String::from_utf8_lossy(&parity.stderr),
    );

    std::fs::remove_dir_all(dir).unwrap();
}
