//! OAuth exposes typed trust-boundary failures while String bridges stay explicit.

use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_witchy");

fn run(args: &[&str]) -> Output {
    Command::new(BIN).args(args).output().expect("spawn witchy")
}

#[test]
fn oauth_typed_errors_are_matchable_and_bridge_to_string() {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "witchy-typed-oauth-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("main.witchy");
    std::fs::write(
        &source,
        r#"import oauth
import http
import json
import show

fn classify(e: oauth.OAuthError) -> String:
    match e:
        oauth.TokenEndpointNotHttps(endpoint) -> "token-https:" + endpoint
        oauth.BearerEndpointNotHttps(endpoint) -> "bearer-https:" + endpoint
        oauth.TokenEndpointUnreachable(reason) -> "token-http:" + http.http_error_message(reason)
        oauth.BearerRequestFailed(reason) -> "bearer-http:" + http.http_error_message(reason)
        oauth.TokenEndpointRejected(status, body) -> "token-status:${status}:" + body
        oauth.BearerEndpointRejected(status) -> "bearer-status:${status}"
        oauth.TokenResponseJson(reason) -> "token-json:" + json.decode_error_message(reason)
        oauth.BearerResponseJson(reason) -> "bearer-json:" + json.decode_error_message(reason)
        oauth.ProviderError(reason) -> "provider:" + reason
        oauth.MissingTokenField(field) -> "missing:" + field

fn fail_oauth() -> Result(String, oauth.OAuthError):
    Err(oauth.MissingTokenField("access_token"))

fn via_string() -> Result(String, String):
    let token = fail_oauth()?
    Ok(token)

fn main(console: Console):
    console.print(classify(oauth.TokenEndpointNotHttps("http://idp/token")))
    console.print(classify(oauth.BearerEndpointNotHttps("http://api/user")))
    console.print(classify(oauth.TokenEndpointUnreachable(http.UnsupportedScheme("ftp"))))
    console.print(classify(oauth.BearerRequestFailed(http.UnsupportedScheme("ftp"))))
    console.print(classify(oauth.TokenEndpointRejected(400, "bad")))
    console.print(classify(oauth.BearerEndpointRejected(401)))
    console.print(classify(oauth.TokenResponseJson(json.DecodeError("bad json"))))
    console.print(classify(oauth.BearerResponseJson(json.DecodeError("bad json"))))
    console.print(classify(oauth.ProviderError("invalid_client")))
    console.print(classify(oauth.MissingTokenField("id_token")))
    console.print(oauth.oauth_error_message(oauth.MissingTokenField("id_token")))
    console.print(show.render(oauth.ProviderError("invalid_client")))
    match via_string():
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
            "token-https:http://idp/token",
            "bearer-https:http://api/user",
            "token-http:unsupported URL scheme `ftp` (only http and https are supported)",
            "bearer-http:unsupported URL scheme `ftp` (only http and https are supported)",
            "token-status:400:bad",
            "bearer-status:401",
            "token-json:bad json",
            "bearer-json:bad json",
            "provider:invalid_client",
            "missing:id_token",
            "no id_token in token response",
            "token endpoint error: invalid_client",
            "no access_token in token response",
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
