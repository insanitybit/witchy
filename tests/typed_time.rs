//! Time parsing exposes typed failures on the primary constructor and parser APIs.

use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_witchy");

fn run(args: &[&str]) -> Output {
    Command::new(BIN).args(args).output().expect("spawn witchy")
}

#[test]
fn time_typed_errors_are_matchable_and_bridge_to_string() {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "witchy-typed-time-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("main.witchy");
    std::fs::write(
        &source,
        r#"import time
import show

fn classify(e: time.TimeError) -> String:
    match e:
        time.YearOutOfRange(y) -> "year:${y}"
        time.MonthOutOfRange(mo) -> "month:${mo}"
        time.DayOutOfRange(y, mo, da) -> "day:${y}:${mo}:${da}"
        time.ClockOutOfRange(h, mi, s) -> "clock:${h}:${mi}:${s}"
        time.InvalidIsoDate(text) -> "date:" + text
        time.MissingDateTimeSeparator(text) -> "separator:" + text
        time.InvalidIsoTime(text) -> "time:" + text
        time.InvalidDigits(text, from, to, piece) -> "digits:" + text + ":${from}:${to}:" + piece
        time.EmptyFractionalSeconds(text) -> "fraction:" + text
        time.BadUtcOffset(offset, text) -> "offset:" + offset + ":" + text
        time.UtcOffsetOutOfRange(offset) -> "offset-range:" + offset

fn via_string(raw: String) -> Result(time.DateTime, String):
    let d = time.parse_iso8601(raw)?
    Ok(d)

fn main(console: Console):
    match time.civil(2026, 2, 30, 0, 0, 0):
        Ok(_) -> console.print("bad")
        Err(e) ->
            console.print(classify(e))
            console.print(time.time_error_message(e))
            console.print(show.render(e))
    match time.civil(0, 1, 1, 0, 0, 0):
        Ok(_) -> console.print("bad")
        Err(e) -> console.print(classify(e))
    match time.civil(2026, 13, 1, 0, 0, 0):
        Ok(_) -> console.print("bad")
        Err(e) -> console.print(classify(e))
    match time.civil(2026, 1, 1, 24, 0, 0):
        Ok(_) -> console.print("bad")
        Err(e) -> console.print(classify(e))
    match time.parse_iso8601("2026/01/01"):
        Ok(_) -> console.print("bad")
        Err(e) -> console.print(classify(e))
    match time.parse_iso8601("2026-01-01X00:00:00Z"):
        Ok(_) -> console.print("bad")
        Err(e) -> console.print(classify(e))
    match time.parse_iso8601("2026-01-01T00:00Z"):
        Ok(_) -> console.print("bad")
        Err(e) -> console.print(classify(e))
    match time.parse_iso8601("2026-01-01Taa:00:00Z"):
        Ok(_) -> console.print("bad")
        Err(e) -> console.print(classify(e))
    match time.parse_iso8601("2026-01-01T00:00:00."):
        Ok(_) -> console.print("bad")
        Err(e) -> console.print(classify(e))
    match time.parse_iso8601("2026-01-01T00:00:00+0"):
        Ok(_) -> console.print("bad")
        Err(e) -> console.print(classify(e))
    match time.parse_iso8601("2026-01-01T00:00:00+25:00"):
        Ok(_) -> console.print("bad")
        Err(e) -> console.print(classify(e))
    match via_string("2026/01/01"):
        Ok(_) -> console.print("bad")
        Err(e) -> console.print(e)
    match time.civil_string(2026, 13, 1, 0, 0, 0):
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
            "day:2026:2:30",
            "day 30 is out of range for 2026-2",
            "day 30 is out of range for 2026-2",
            "year:0",
            "month:13",
            "clock:24:0:0",
            "date:2026/01/01",
            "separator:2026-01-01X00:00:00Z",
            "time:2026-01-01T00:00Z",
            "digits:2026-01-01Taa:00:00Z:11:13:aa",
            "fraction:2026-01-01T00:00:00.",
            "offset:+0:2026-01-01T00:00:00+0",
            "offset-range:+25:00",
            "`2026/01/01` is not an ISO 8601 date (expected `YYYY-MM-DD`)",
            "month 13 is out of range 1..12",
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
