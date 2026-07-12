//! Encoding decoders expose typed failures on the primary decode APIs.

use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_witchy");

fn run(args: &[&str]) -> Output {
    Command::new(BIN).args(args).output().expect("spawn witchy")
}

#[test]
fn encoding_typed_errors_are_matchable_and_bridge_to_string() {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "witchy-typed-encoding-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("main.witchy");
    std::fs::write(
        &source,
        r#"import encoding
import show

fn classify(e: encoding.EncodingError) -> String:
    match e:
        encoding.InvalidHex(raw) -> "hex:" + raw
        encoding.InvalidBase64(raw) -> "base64:" + raw
        encoding.InvalidBase64Url(raw) -> "base64url:" + raw

fn via_string(raw: String) -> Result(String, String):
    let text = encoding.base64url_decode(raw)?
    Ok(text)

fn main(console: Console):
    match encoding.hex_decode("zz"):
        Ok(_) -> console.print("bad")
        Err(e) ->
            console.print(classify(e))
            console.print(encoding.encoding_error_message(e))
            console.print(show.render(e))
    match encoding.base64_decode("@@@"):
        Ok(_) -> console.print("bad")
        Err(e) -> console.print(classify(e))
    match encoding.base64url_decode("QQD/Lw=="):
        Ok(_) -> console.print("bad")
        Err(e) -> console.print(classify(e))
    match encoding.hex_to_base64url("abc"):
        Ok(_) -> console.print("bad")
        Err(e) -> console.print(classify(e))
    match encoding.base64url_to_hex("QQD_Lw"):
        Ok(hex) -> console.print(hex)
        Err(_) -> console.print("bad")
    match via_string("QQD/Lw=="):
        Ok(_) -> console.print("bad")
        Err(e) -> console.print(e)
    match encoding.base64_decode_string("@@@"):
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
            "hex:zz",
            "`zz` is not valid hex (expected an even count of `0-9a-fA-F` digits)",
            "`zz` is not valid hex (expected an even count of `0-9a-fA-F` digits)",
            "base64:@@@",
            "base64url:QQD/Lw==",
            "hex:abc",
            "4100ff2f",
            "`QQD/Lw==` is not valid base64url (expected the URL-safe `A-Za-z0-9-_` alphabet)",
            "`@@@` is not valid base64 (expected the `A-Za-z0-9+/` alphabet)",
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
