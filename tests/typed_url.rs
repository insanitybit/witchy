//! URL parsing exposes typed failures without breaking the String wrapper.

use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_witchy");

fn run(args: &[&str]) -> Output {
    Command::new(BIN).args(args).output().expect("spawn witchy")
}

#[test]
fn parse_typed_exposes_matchable_url_errors_and_string_bridge() {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "witchy-typed-url-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("main.witchy");
    std::fs::write(
        &source,
        r#"import show
import url
from url import Url

fn classify(e: url.UrlError) -> String:
    match e:
        url.InvalidPort(raw) -> "invalid-port:" + raw
        url.MalformedIpv6Literal(raw) -> "bad-ipv6:" + raw
        _ -> "other"

fn via_string(raw: String) -> Result(Url, String):
    let u = url.parse_typed(raw)?
    Ok(u)

fn main(console: Console):
    match url.parse_typed("http://h:999999/p"):
        Ok(_) -> console.print("bad")
        Err(e) ->
            console.print(classify(e))
            console.print(url.url_error_message(e))
            console.print(show.render(e))
    match via_string("http://h:999999/p"):
        Ok(_) -> console.print("bad")
        Err(e) -> console.print(e)
    match url.parse_typed("http://[::1/p"):
        Ok(_) -> console.print("bad")
        Err(e) -> console.print(classify(e))
    match url.parse("noscheme"):
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
            "invalid-port:http://h:999999/p",
            "invalid port in: http://h:999999/p",
            "invalid port in: http://h:999999/p",
            "invalid port in: http://h:999999/p",
            "bad-ipv6:http://[::1/p",
            "missing `scheme://` in: noscheme",
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
