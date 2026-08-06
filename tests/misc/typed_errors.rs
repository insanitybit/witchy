//! Typed error tests: each stdlib module that exposes typed Result failures
//! (bytes, duration, encoding, http, jwt, oauth, semver, time, url) has its
//! matchability + show + String bridge tested here. Consolidated from separate
//! files to save 8 binary links (~10-16s) on every test compilation.

use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_witchy");

fn run(args: &[&str]) -> Output {
    Command::new(BIN).args(args).output().expect("spawn witchy")
}

fn run_witchy_test(slug: &str, source: &str, expected_lines: &[&str]) {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "witchy-typed-{slug}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let source_path = dir.join("main.witchy");
    std::fs::write(&source_path, source).unwrap();

    let path = source_path.to_str().unwrap();
    let output = run(&[path]);
    assert!(
        output.status.success(),
        "{slug}: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let expected = expected_lines.join("\n") + "\n";
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        expected,
        "{slug}: output mismatch"
    );

    let parity = run(&["parity", path]);
    assert!(
        parity.status.success()
            && String::from_utf8_lossy(&parity.stdout).contains("outcome=agree"),
        "{slug}: parity failed\n{}\n{}",
        String::from_utf8_lossy(&parity.stdout),
        String::from_utf8_lossy(&parity.stderr),
    );

    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn bytes_typed_errors_are_matchable_and_bridge_to_string() {
    run_witchy_test(
        "bytes",
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
        Ok(b) -> console.print("${bytes.to_list(b)}")
        Err(_) -> console.print("bad")
    match bytes.from_list([256]):
        Ok(_) -> console.print("bad")
        Err(e) ->
            console.print(classify(e))
            console.print(bytes.bytes_error_message(e))
            console.print(show.render(e))
    match bytes.from_list([0 - 1]):
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
        &[
            "[0, 255, 65]",
            "range:256",
            "bytes.from_list: value 256 is outside 0..=255",
            "bytes.from_list: value 256 is outside 0..=255",
            "range:-1",
            "bytes.from_list: value -1 is outside 0..=255",
            "bytes.from_list: value -1 is outside 0..=255",
            "utf8",
            "bytes.decode_utf8: invalid UTF-8",
            "bytes.decode_utf8: invalid UTF-8",
            "bytes.from_list: value 256 is outside 0..=255",
            "bytes.decode_utf8: invalid UTF-8",
        ],
    );
}

#[test]
fn duration_typed_errors_are_matchable_and_bridge_to_string() {
    run_witchy_test(
        "duration",
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
        &[
            "trailing:1h30",
            "`1h30` has a trailing unit-less number (expected a unit like `s`/`m`/`h` after every count)",
            "`1h30` has a trailing unit-less number (expected a unit like `s`/`m`/`h` after every count)",
            "unit:ms:ms",
            "shape:1x",
            "overflow:999999999999999999999999999999999ms",
            "empty:",
            "`1h30` has a trailing unit-less number (expected a unit like `s`/`m`/`h` after every count)",
            "`ms` has a unit with no count (expected a number before `ms`)",
        ],
    );
}

#[test]
fn encoding_typed_errors_are_matchable_and_bridge_to_string() {
    run_witchy_test(
        "encoding",
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
        &[
            "hex:zz",
            "`zz` is not valid hex (expected an even count of `0-9a-fA-F` digits)",
            "`zz` is not valid hex (expected an even count of `0-9a-fA-F` digits)",
            "base64:@@@",
            "base64url:QQD/Lw==",
            "hex:abc",
            "4100ff2f",
            "`QQD/Lw==` is not valid base64url (expected the URL-safe `A-Za-z0-9-_` alphabet)",
            "`@@@` is not valid base64 (expected the `A-Za-z0-9+/` alphabet)",
        ],
    );
}

#[test]
fn http_typed_errors_are_matchable_and_bridge_to_string() {
    run_witchy_test(
        "http",
        r#"import http
import show

fn classify(e: http.HttpError) -> String:
    match e:
        http.Denied(message) -> "denied:" + message
        http.InvalidRequest(message) -> "invalid:" + message
        http.Timeout -> "timeout"
        http.Redirect(message) -> "redirect:" + message
        http.Network(message) -> "network:" + message
        http.ProviderMalformedResponse(message) -> "provider-response:" + message
        http.ResponseTooLarge(message) -> "too-large:" + message
        http.UnknownProviderFailure(code, message) -> "unknown:" + code + ":" + message
        http.MalformedResponse(reason) -> "response:" + http.response_parse_error_message(reason)

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
    console.print(classify(http.Denied("blocked")))
    console.print(show.render(http.InvalidRequest("bad request")))
    console.print(http.http_error_message(http.Timeout))
    console.print(classify(http.Redirect("redirect refused")))
    console.print(classify(http.Network("offline")))
    console.print(classify(http.ProviderMalformedResponse("bad status")))
    console.print(classify(http.ResponseTooLarge("limit 10")))
    console.print(http.http_error_message(http.UnknownProviderFailure("custom", "detail")))
"#,
        &[
            "response:chunked response has invalid chunk size `z`",
            "chunked response has invalid chunk size `z`",
            "chunked response has invalid chunk size `z`",
            "chunked response has invalid chunk size `z`",
            "denied:blocked",
            "bad request",
            "Fetch request timed out",
            "redirect:redirect refused",
            "network:offline",
            "provider-response:bad status",
            "too-large:limit 10",
            "Fetch provider error `custom`: detail",
        ],
    );
}

#[test]
fn jwt_typed_errors_are_matchable_and_bridge_to_string() {
    run_witchy_test(
        "jwt",
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
        &["missing-kid"],
    );
}

#[test]
fn oauth_typed_errors_are_matchable_and_bridge_to_string() {
    run_witchy_test(
        "oauth",
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
    console.print(classify(oauth.TokenEndpointUnreachable(http.Network("offline"))))
    console.print(classify(oauth.BearerRequestFailed(http.Denied("blocked"))))
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
        &[
            "token-https:http://idp/token",
            "bearer-https:http://api/user",
            "token-http:offline",
            "bearer-http:blocked",
            "token-status:400:bad",
            "bearer-status:401",
            "token-json:bad json",
            "bearer-json:bad json",
            "provider:invalid_client",
            "missing:id_token",
            "no id_token in token response",
            "token endpoint error: invalid_client",
            "no access_token in token response",
        ],
    );
}

#[test]
fn semver_typed_errors_are_matchable_and_bridge_to_string() {
    run_witchy_test(
        "semver",
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
        &[
            "signed:+1",
            "bad version component `+1`: sign characters are not allowed",
            "bad version component `+1`: sign characters are not allowed",
            "bad version component `+1`: sign characters are not allowed",
            "nonnumeric:x",
            "bad version component `x`",
            "bad version `1.2.3.4`: expected major.minor.patch",
        ],
    );
}

#[test]
fn time_typed_errors_are_matchable_and_bridge_to_string() {
    run_witchy_test(
        "time",
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
        &[
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
        ],
    );
}

#[test]
fn url_typed_errors_are_matchable_and_bridge_to_string() {
    run_witchy_test(
        "url",
        r#"import show
import url
from url import Url

fn classify(e: url.UrlError) -> String:
    match e:
        url.InvalidPort(raw) -> "invalid-port:" + raw
        url.MalformedIpv6Literal(raw) -> "bad-ipv6:" + raw
        _ -> "other"

fn via_string(raw: String) -> Result(Url, String):
    let u = url.parse(raw)?
    Ok(u)

fn main(console: Console):
    match url.parse("http://h:999999/p"):
        Ok(_) -> console.print("bad")
        Err(e) ->
            console.print(classify(e))
            console.print(url.url_error_message(e))
            console.print(show.render(e))
    match via_string("http://h:999999/p"):
        Ok(_) -> console.print("bad")
        Err(e) -> console.print(e)
    match url.parse("http://[::1/p"):
        Ok(_) -> console.print("bad")
        Err(e) -> console.print(classify(e))
    match url.parse("http://::1"):
        Ok(u) -> console.print(url.host(u) + " " + "${url.port(u)}")
        Err(_) -> console.print("bad")
    match url.parse("http://2001:db8::1/path"):
        Ok(u) -> console.print(url.host(u) + " " + "${url.port(u)}")
        Err(_) -> console.print("bad")
    match url.parse_string("noscheme"):
        Ok(_) -> console.print("bad")
        Err(e) -> console.print(e)
"#,
        &[
            "invalid-port:http://h:999999/p",
            "invalid port in: http://h:999999/p",
            "invalid port in: http://h:999999/p",
            "invalid port in: http://h:999999/p",
            "bad-ipv6:http://[::1/p",
            "::1 80",
            "2001:db8::1 80",
            "missing `scheme://` in: noscheme",
        ],
    );
}
