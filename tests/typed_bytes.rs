//! Bytes conversion failures are typed while String bridges stay explicit.

use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_witchy");

fn run(args: &[&str]) -> Output {
    Command::new(BIN).args(args).output().expect("spawn witchy")
}

#[test]
fn bytes_typed_errors_are_matchable_and_bridge_to_string() {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "witchy-typed-bytes-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("main.witchy");
    std::fs::write(
        &source,
        r#"import bytes
import show

fn classify(e: bytes.BytesError) -> String:
    match e:
        bytes.ByteOutOfRange(n) -> "range:${n}"
        bytes.InvalidUtf8 -> "utf8"

fn from_list_via_string() -> Result(Bytes, String):
    let b = bytes.from_list([256])?
    Ok(b)

fn decode_via_string() -> Result(String, String):
    let text = bytes.decode_utf8(bytes.slice(bytes.from_string("é"), 0, 1))?
    Ok(text)

fn main(console: Console):
    match bytes.from_list([0, 255, 65]):
        Ok(b) -> console.print(__render(bytes.to_list(b)))
        Err(_) -> console.print("bad")
    match bytes.from_list([256]):
        Ok(_) -> console.print("bad")
        Err(e) ->
            console.print(classify(e))
            console.print(bytes.bytes_error_message(e))
            console.print(show.render(e))
    match bytes.decode_utf8(bytes.slice(bytes.from_string("é"), 0, 1)):
        Ok(_) -> console.print("bad")
        Err(e) ->
            console.print(classify(e))
            console.print(bytes.bytes_error_message(e))
            console.print(show.render(e))
    match from_list_via_string():
        Ok(_) -> console.print("bad")
        Err(e) -> console.print(e)
    match decode_via_string():
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
            "[0, 255, 65]",
            "range:256",
            "bytes.from_list: value 256 is outside 0..=255",
            "bytes.from_list: value 256 is outside 0..=255",
            "utf8",
            "bytes.decode_utf8: invalid UTF-8",
            "bytes.decode_utf8: invalid UTF-8",
            "bytes.from_list: value 256 is outside 0..=255",
            "bytes.decode_utf8: invalid UTF-8",
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
