//! HTTP fallible paths expose typed failures while legacy wrappers keep String errors.

use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_witchy");

fn run(args: &[&str]) -> Output {
    Command::new(BIN).args(args).output().expect("spawn witchy")
}

#[test]
fn http_typed_errors_are_matchable_and_bridge_to_string() {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "witchy-typed-http-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("main.witchy");
    std::fs::write(
        &source,
        r#"import http
import show
import url

fn classify(e: http.HttpError) -> String:
    match e:
        http.InvalidUrl(reason) ->
            match reason:
                url.MissingSchemeSeparator(raw) -> "url:missing:" + raw
                _ -> "url:other"
        http.UnsupportedScheme(scheme) -> "scheme:" + scheme
        http.ConnectFailed(host, port) -> "connect:" + host + ":${port}"
        http.NoResolvedAddressPassedPinPolicy(host) -> "pin-policy:" + host
        http.PinnedConnectFailed(host, ip, port) -> "pinned:" + host + ":" + ip + ":${port}"
        http.MalformedResponse(reason) -> "response:" + reason

fn via_string(raw: String) -> Result(http.Response, String):
    let resp = http.try_parse_response(raw)?
    Ok(resp)

fn main(console: Console):
    let bad = "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nz\r\nno\r\n"
    match http.try_parse_response(bad):
        Ok(_) -> console.print("bad")
        Err(e) ->
            console.print(classify(e))
            console.print(http.http_error_message(e))
            console.print(show.render(e))
    match via_string(bad):
        Ok(_) -> console.print("bad")
        Err(e) -> console.print(e)
    console.print(http.http_error_message(http.InvalidUrl(url.MissingSchemeSeparator("nope"))))
    console.print(show.render(http.UnsupportedScheme("ftp")))
    console.print(http.http_error_message(http.ConnectFailed("example.test", 80)))
    console.print(http.http_error_message(http.NoResolvedAddressPassedPinPolicy("example.test")))
    console.print(http.http_error_message(http.PinnedConnectFailed("example.test", "203.0.113.1", 443)))
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
            "response:chunked response has invalid chunk size `z`",
            "chunked response has invalid chunk size `z`",
            "chunked response has invalid chunk size `z`",
            "chunked response has invalid chunk size `z`",
            "invalid URL: missing `scheme://` in: nope",
            "unsupported URL scheme `ftp` (only http and https are supported)",
            "connect to example.test:80 failed (unreachable)",
            "no resolved address for example.test passed the pin policy",
            "connect to example.test (203.0.113.1:443) failed",
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
