//! Semver parsing exposes typed failures on the primary parse APIs.

use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_witchy");

fn run(args: &[&str]) -> Output {
    Command::new(BIN).args(args).output().expect("spawn witchy")
}

#[test]
fn parse_exposes_matchable_semver_errors_and_string_bridge() {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "witchy-typed-semver-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("main.witchy");
    std::fs::write(
        &source,
        r#"import semver
import show
from semver import Req
from semver import Version

fn classify(e: semver.SemverError) -> String:
    match e:
        semver.BadVersionShape(raw) -> "shape:" + raw
        semver.SignedVersionComponent(component) -> "signed:" + component
        semver.NegativeVersionComponent(component) -> "negative:" + component
        semver.NonNumericVersionComponent(component) -> "nonnumeric:" + component

fn via_string(raw: String) -> Result(Version, String):
    let v = semver.parse(raw)?
    Ok(v)

fn req_via_string(raw: String) -> Result(Req, String):
    let r = semver.parse_req(raw)?
    Ok(r)

fn main(console: Console):
    match semver.parse("+1.2.3"):
        Ok(_) -> console.print("bad")
        Err(e) ->
            console.print(classify(e))
            console.print(semver.semver_error_message(e))
            console.print(show.render(e))
    match via_string("+1.2.3"):
        Ok(_) -> console.print("bad")
        Err(e) -> console.print(e)
    match semver.parse_req("^1.x"):
        Ok(_) -> console.print("bad")
        Err(e) -> console.print(classify(e))
    match req_via_string("^1.x"):
        Ok(_) -> console.print("bad")
        Err(e) -> console.print(e)
    match semver.parse_string("1.2.3.4"):
        Ok(_) -> console.print("bad")
        Err(e) -> console.print(e)
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
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        [
            "signed:+1",
            "bad version component `+1`: sign characters are not allowed",
            "bad version component `+1`: sign characters are not allowed",
            "bad version component `+1`: sign characters are not allowed",
            "nonnumeric:x",
            "bad version component `x`",
            "bad version `1.2.3.4`: expected major.minor.patch",
            "",
        ]
        .join("\n")
    );

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
