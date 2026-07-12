//! Duration parsing exposes typed failures on the primary parse API.

use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_witchy");

fn run(args: &[&str]) -> Output {
    Command::new(BIN).args(args).output().expect("spawn witchy")
}

#[test]
fn parse_exposes_matchable_duration_errors_and_string_bridge() {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "witchy-typed-duration-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("main.witchy");
    std::fs::write(
        &source,
        r#"import duration
import show

fn classify(e: duration.DurationParseError) -> String:
    match e:
        duration.DurationOverflow(raw) -> "overflow:" + raw
        duration.InvalidDurationShape(raw) -> "shape:" + raw
        duration.UnitWithoutCount(raw, unit) -> "unit:" + raw + ":" + unit
        duration.TrailingUnitlessNumber(raw) -> "trailing:" + raw
        duration.EmptyDuration(raw) -> "empty:" + raw

fn via_string(raw: String) -> Result(Duration, String):
    let d = duration.parse(raw)?
    Ok(d)

fn main(console: Console):
    match duration.parse("1h30"):
        Ok(_) -> console.print("bad")
        Err(e) ->
            console.print(classify(e))
            console.print(duration.parse_error_message(e))
            console.print(show.render(e))
    match duration.parse("ms"):
        Ok(_) -> console.print("bad")
        Err(e) -> console.print(classify(e))
    match duration.parse("1x"):
        Ok(_) -> console.print("bad")
        Err(e) -> console.print(classify(e))
    match duration.parse("999999999999999999999999999999999ms"):
        Ok(_) -> console.print("bad")
        Err(e) -> console.print(classify(e))
    match duration.parse(""):
        Ok(_) -> console.print("bad")
        Err(e) -> console.print(classify(e))
    match via_string("1h30"):
        Ok(_) -> console.print("bad")
        Err(e) -> console.print(e)
    match duration.parse_string("ms"):
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
            "trailing:1h30",
            "`1h30` has a trailing unit-less number (expected a unit like `s`/`m`/`h` after every count)",
            "`1h30` has a trailing unit-less number (expected a unit like `s`/`m`/`h` after every count)",
            "unit:ms:ms",
            "shape:1x",
            "overflow:999999999999999999999999999999999ms",
            "empty:",
            "`1h30` has a trailing unit-less number (expected a unit like `s`/`m`/`h` after every count)",
            "`ms` has a unit with no count (expected a number before `ms`)",
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
