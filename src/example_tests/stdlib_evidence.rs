use super::{link_run, wasm_run};

/// Property tests over the standard library: invariants that must hold for
/// *any* input — encode/decode round-trips, calendar inverses, semver
/// rendering — checked by generating the input, running it through the witchy
/// stdlib, and comparing to a Rust reference. These catch edge cases (empty
/// strings, embedded quotes/newlines, negative timestamps) unit tests miss.
mod stdlib_properties {
    use super::link_run;
    use proptest::prelude::*;

    /// Escape a Rust string into the body of a witchy `"..."` literal.
    fn esc(s: &str) -> String {
        let mut out = String::new();
        for c in s.chars() {
            match c {
                '\\' => out.push_str("\\\\"),
                '"' => out.push_str("\\\""),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                _ => out.push(c),
            }
        }
        out
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        /// `encoding.hex_encode` equals the byte-wise lowercase hex reference.
        #[test]
        fn hex_encode_matches_reference(s in "[ -#%-z|~]{0,40}") {
            let src = format!(
                "import encoding\nfn main(console: Console):\n    console.print(encoding.hex_encode(\"{}\"))\n",
                esc(&s)
            );
            let reference: String = s.bytes().map(|b| format!("{b:02x}")).collect();
            prop_assert_eq!(link_run(&src), vec![reference]);
        }

        /// base64 decode is the inverse of encode, for any printable ASCII.
        #[test]
        fn base64_roundtrips(s in "[ -#%-z|~]{0,48}") {
            let src = format!(
                "import encoding\nfn main(console: Console):\n    let s = \"{}\"\n    console.print(yn(encoding.base64_decode(encoding.base64_encode(s)).unwrap_or(\"?\") == s))\n\nfn yn(b: Bool) -> String:\n    if b: \"y\" else: \"n\"\n",
                esc(&s)
            );
            prop_assert_eq!(link_run(&src), vec!["y".to_string()]);
        }

        /// hex decode is the inverse of encode.
        #[test]
        fn hex_roundtrips(s in "[ -#%-z|~]{0,48}") {
            let src = format!(
                "import encoding\nfn main(console: Console):\n    let s = \"{}\"\n    console.print(yn(encoding.hex_decode(encoding.hex_encode(s)).unwrap_or(\"?\") == s))\n\nfn yn(b: Bool) -> String:\n    if b: \"y\" else: \"n\"\n",
                esc(&s)
            );
            prop_assert_eq!(link_run(&src), vec!["y".to_string()]);
        }

        /// `time.to_unix` is the exact inverse of `time.from_unix`, across the
        /// CE range and negative (pre-1970) timestamps.
        #[test]
        fn time_unix_roundtrips(n in -62135596800i64..=253402300799i64) {
            let src = format!(
                "import time\nfn main(console: Console):\n    console.print(yn(time.to_unix(time.from_unix({n})) == {n}))\n\nfn yn(b: Bool) -> String:\n    if b: \"y\" else: \"n\"\n"
            );
            prop_assert_eq!(link_run(&src), vec!["y".to_string()]);
        }

        /// `semver.format` after `parse` reproduces the canonical version.
        #[test]
        fn semver_roundtrips(a in 0i64..2000, b in 0i64..2000, c in 0i64..2000) {
            let v = format!("{a}.{b}.{c}");
            let src = format!(
                "import semver\nfn main(console: Console):\n    match semver.parse(\"{v}\"):\n        Ok(x) -> console.print(semver.format(x))\n        Err(e) -> console.print(\"err\")\n"
            );
            prop_assert_eq!(link_run(&src), vec![v]);
        }

        /// `path.normalize` is idempotent — normalizing an already-normal path
        /// changes nothing — over arbitrary `.`/`..`/segment soup.
        #[test]
        fn path_normalize_is_idempotent(p in "[a-c./]{0,24}") {
            let src = format!(
                "import path\nfn main(console: Console):\n    let once = path.normalize(\"{}\")\n    console.print(yn(path.normalize(once) == once))\n\nfn yn(b: Bool) -> String:\n    if b: \"y\" else: \"n\"\n",
                esc(&p)
            );
            prop_assert_eq!(link_run(&src), vec!["y".to_string()]);
        }

        /// Run-length decode is the inverse of encode (the `examples/rle`
        /// algorithm, exercising string.to_chars/repeat + ascii.is_digit/
        /// to_digit). Restricted to digit-free input: the count-prefix format
        /// is only unambiguous when the data carries no digits, so this both
        /// asserts the round-trip and documents that boundary.
        #[test]
        fn rle_round_trips_over_digit_free_text(s in "[a-zA-Z ]{0,40}") {
            let src = format!(
                "import ascii\n\nfn encode(t: String) -> String:\n    let cs = t.chars()\n    let n = list.length(cs)\n    var out = \"\"\n    var i = 0\n    while i < n:\n        let c = list.at(cs, i)\n        var k = 0\n        while i < n && list.at(cs, i) == c:\n            k = k + 1\n            i = i + 1\n        out = out + \"${{k}}\" + c\n    out\n\nfn decode(e: String) -> String:\n    let cs = e.chars()\n    let n = list.length(cs)\n    var out = \"\"\n    var i = 0\n    while i < n:\n        var k = 0\n        while i < n && ascii.is_digit(list.at(cs, i)):\n            k = k * 10 + (ascii.to_digit(list.at(cs, i)) ?? 0)\n            i = i + 1\n        if i < n:\n            out = out + list.at(cs, i).repeat(k)\n            i = i + 1\n    out\n\nfn yn(b: Bool) -> String:\n    if b: \"y\" else: \"n\"\n\nfn main(console: Console):\n    let s = \"{}\"\n    console.print(yn(decode(encode(s)) == s))\n",
                esc(&s)
            );
            prop_assert_eq!(link_run(&src), vec!["y".to_string()]);
        }
    }
}

/// `crypto.ed25519_verify` — a native intrinsic of the `crypto` module — is a
/// fallible signature check: it accepts a genuine signature, rejects a
/// tampered message, and reports malformed input.
#[test]
fn crypto_ed25519_verify_checks_signatures() {
    use ed25519_dalek::{Signer, SigningKey};
    let sk = SigningKey::from_bytes(&[7u8; 32]);
    let hex = |bs: &[u8]| -> String { bs.iter().map(|b| format!("{b:02x}")).collect() };
    let pk = hex(sk.verifying_key().as_bytes());
    let msg = "release: acme/widget@1.0.0";
    let sig = hex(&sk.sign(msg.as_bytes()).to_bytes());

    let prog = |pubk: &str, m: &str, s: &str| {
        format!(
            "import crypto\nfn main(console: Console):\n    match crypto.ed25519_verify(\"{pubk}\", \"{m}\", \"{s}\"):\n        Ok(true) -> console.print(\"ok\")\n        Ok(false) -> console.print(\"bad\")\n        Err(_e) -> console.print(\"err\")\n"
        )
    };
    assert_eq!(link_run(&prog(&pk, msg, &sig)), vec!["ok"], "valid signature must verify");
    assert_eq!(
        link_run(&prog(&pk, "release: acme/widget@1.0.1", &sig)),
        vec!["bad"],
        "tampered message must fail"
    );
    assert_eq!(link_run(&prog(&pk, msg, "00")), vec!["err"], "malformed sig must be an error");
}

/// `crypto.ecdsa_p256_verify` (WebAuthn "ES256") verifies a real P-256/SHA-256
/// signature, rejects a tampered message, and reports malformed signatures.
/// KAT: SEC1-uncompressed pubkey + ASN.1-DER sig (generated with the `cryptography` lib).
#[test]
fn crypto_ecdsa_p256_verify_checks_signatures() {
    let pk = "048f81cd9fca785a42a6f5dd58972cc0f702e83b1c960b5912354471496597e227fec81ff1d52530b06d7091649e6beb49dba70968b4b727bb24e3ceb7dd01a039";
    let msg = "webauthn-es256-test-message";
    let sig = "304402203260029f4c6beb2e78afdd906c057c63f8828e2b03820de7053d97254577fb8c02204478b9b75f8fd7a1ce4298f0d119e12926dafda116ae4c197b0048dc117bc9de";
    let prog = |pubk: &str, m: &str, s: &str| {
        format!(
            "import crypto\nfn main(console: Console):\n    match crypto.ecdsa_p256_verify(\"{pubk}\", \"{m}\", \"{s}\"):\n        Ok(true) -> console.print(\"ok\")\n        Ok(false) -> console.print(\"bad\")\n        Err(_e) -> console.print(\"err\")\n"
        )
    };
    assert_eq!(link_run(&prog(pk, msg, sig)), vec!["ok"], "valid ES256 signature must verify");
    assert_eq!(link_run(&prog(pk, "wrong-message", sig)), vec!["bad"], "tampered message must fail");
    assert_eq!(link_run(&prog(pk, msg, "00")), vec!["err"], "malformed sig must be an error");
}

/// `crypto.sha512` and `crypto.hmac_sha256` against standard known-answer vectors
/// (SHA-512("abc"); HMAC-SHA256 RFC 4231 test case 1).
#[test]
fn crypto_sha512_and_hmac_match_known_vectors() {
    let p1 = "import crypto\nfn main(console: Console):\n    console.print(crypto.sha512(\"abc\"))\n";
    assert_eq!(
        link_run(p1),
        vec!["ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"]
    );
    let key = "0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b";
    let p2 = format!(
        "import crypto\nfn main(console: Console):\n    console.print(crypto.hmac_sha256(\"{key}\", \"Hi There\"))\n"
    );
    assert_eq!(link_run(&p2), vec!["b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"]);
}

/// The aws-lc-rs crypto extensions (`sha512`, `sha3_256`, `hmac_sha256`,
/// `ecdsa_p256_verify`) produce byte-identical results on the interpreter and
/// the compiled WASM backend: the host imports bridge to the SAME native
/// registry the interpreter calls, so the backends agree by construction.
/// This guards the bridge that lets coven-web run fully sandboxed.
#[test]
fn crypto_extensions_backends_agree() {
    let pk = "048f81cd9fca785a42a6f5dd58972cc0f702e83b1c960b5912354471496597e227fec81ff1d52530b06d7091649e6beb49dba70968b4b727bb24e3ceb7dd01a039";
    let sig = "304402203260029f4c6beb2e78afdd906c057c63f8828e2b03820de7053d97254577fb8c02204478b9b75f8fd7a1ce4298f0d119e12926dafda116ae4c197b0048dc117bc9de";
    let src = format!(
"import crypto
fn main(console: Console):
    console.print(crypto.sha512(\"abc\"))
    console.print(crypto.sha3_256(\"abc\"))
    console.print(crypto.hmac_sha256(\"0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b\", \"Hi There\"))
    match crypto.ecdsa_p256_verify(\"{pk}\", \"webauthn-es256-test-message\", \"{sig}\"):
        Ok(true) -> console.print(\"ok\")
        Ok(false) -> console.print(\"bad\")
        Err(_e) -> console.print(\"err\")
    match crypto.ecdsa_p256_verify(\"{pk}\", \"tampered\", \"{sig}\"):
        Ok(true) -> console.print(\"ok\")
        Ok(false) -> console.print(\"bad\")
        Err(_e) -> console.print(\"err\")
"
    );
    let expected = vec![
        "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f",
        "3a985da74fe225b2045c172d6bd390bd855f086e3e9d525b46bfe24511431532",
        "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7",
        "ok",
        "bad",
    ];
    assert_eq!(link_run(&src), expected, "interpreter");
    assert_eq!(wasm_run(&src), expected, "wasm");
}

/// `string.from_code` (a code point -> its UTF-8 character) agrees across the
/// interpreter and the compiled WASM backend, for a 1-byte (ASCII), 2-byte
/// (é), 3-byte (中) and 4-byte (😀) encoding, and yields U+FFFD for a lone
/// surrogate (an invalid scalar value) rather than trapping.
#[test]
fn string_from_code_backends_agree() {
    let src = "import string\nfn main(console: Console):\n    console.print(string.from_code(65) + string.from_code(233) + string.from_code(20013) + string.from_code(128512) + string.from_code(55296))\n";
    let expected = vec!["A\u{e9}\u{4e2d}\u{1f600}\u{fffd}"];
    assert_eq!(link_run(src), expected, "interpreter");
    assert_eq!(wasm_run(src), expected, "wasm");
}

/// The JSON decoder unescapes `\uXXXX` — including astral characters spelled
/// as a UTF-16 surrogate pair (`😀` -> 😀) — identically on both
/// backends. Guards the `string.from_code`-powered `\u` path in `std/json`.
#[test]
fn json_unicode_escapes_backends_agree() {
    let src = r#"import json
import option

fn show(o: Option(String)) -> String:
    match o:
        Some(s) -> s
        None -> "none"

fn main(console: Console):
    match json.decode("{\"k\":\"caf\\u00e9 \\ud83d\\ude00 \\u4e2d\"}"):
        Ok(j) ->
            match json.get(j, "k"):
                Some(v) -> console.print(show(json.as_string(v)))
                None -> console.print("nokey")
        Err(e) -> console.print("err")
"#;
    let expected = vec!["caf\u{e9} \u{1f600} \u{4e2d}"];
    assert_eq!(link_run(src), expected, "interpreter");
    assert_eq!(wasm_run(src), expected, "wasm");
}

/// `json.encode` escapes every C0 control character — RFC 8259 forbids a raw one
/// inside a string, and a raw byte produced invalid JSON no conformant parser
/// would accept. `\b`/`\f` take the short form, the rest `\u00XX` (a NUL is
/// `\u0000`, not a raw byte); `json.decode` round-trips them. Identical on both
/// backends (the bug was parity-silent — both emitted the same invalid output).
#[test]
fn json_encodes_control_characters_backends_agree() {
    // NUL (\u0000), backspace (short \b), tab (short \t), and 0x1f (\u001f).
    let src = "import json\nfrom json import Json\nfn main(console: Console):\n    let s = string.from_code(0) + string.from_code(8) + string.from_code(9) + string.from_code(31)\n    let enc = json.encode(JsonString(s))\n    console.print(enc)\n    match json.decode(enc):\n        Ok(v) -> match v:\n            JsonString(d) -> console.print(\"${d == s}\")\n            _ -> console.print(\"notstr\")\n        Err(e) -> console.print(\"err\")";
    let expected = vec!["\"\\u0000\\b\\t\\u001f\"".to_string(), "true".to_string()];
    assert_eq!(link_run(src), expected, "interpreter");
    assert_eq!(wasm_run(src), expected, "wasm");
}

/// `std/webauthn.verify_assertion` accepts a real ES256 WebAuthn assertion and
/// rejects a tampered signature and a missing user-verification flag. Vectors
/// generated with the `cryptography` lib (P-256, real authenticatorData).
#[test]
fn webauthn_verify_assertion_checks_an_es256_assertion() {
    let pubkey = "045336195e14d40d2d2d3084160b8d776b7d6cdc2e0d162b8da57d8c87dcb6360b67c39ee3d657d7387cec773723df914e5547359511f051fbb6e327368723dba1";
    let client = "{\\\"type\\\":\\\"webauthn.get\\\",\\\"challenge\\\":\\\"dGVzdC1jaGFsbGVuZ2U\\\",\\\"origin\\\":\\\"https://coven.example\\\"}";
    let ad_uv = "fb829c116ec8fed5624aba5b473a0b3a93ca17f477ea91ab2c6ebc49166f860d0500000001";
    let sig_uv = "304602210088b792258e9149557b201f677ffadeda762a2bbd819fb43a6aaff3940681f16e022100b0b770fd5d498d536a6a7d4e641becad007790eb01a85fb8fd9c6e8304ead0ec";
    let ad_up = "fb829c116ec8fed5624aba5b473a0b3a93ca17f477ea91ab2c6ebc49166f860d0100000001";
    let sig_up = "304402207cdb90e725b9051a0918c3a12b2d18e4c952e8e90acde4f49bd0cc7d0c8a18bd02200b9b3f40d586103527e3aa27677746366d62a200209c9a19a6547d515a49a1f8";
    let prog = |ad: &str, sig: &str, uv: &str| {
        format!(
"import webauthn
fn show(r: Result(Bool, webauthn.AssertionError)) -> String:
    match r:
        Ok(_) -> \"ok\"
        Err(e) -> webauthn.assertion_error_message(e)
fn main(console: Console):
    console.print(show(webauthn.verify_assertion(\"{pubkey}\", \"{ad}\", \"{client}\", \"{sig}\", \"dGVzdC1jaGFsbGVuZ2U\", \"https://coven.example\", \"coven.example\", {uv})))
"
        )
    };
    assert_eq!(link_run(&prog(ad_uv, sig_uv, "true")), vec!["ok"], "valid assertion must verify");
    assert!(
        link_run(&prog(ad_uv, &format!("00{sig_uv}"), "true")).join("").contains("signature"),
        "tampered signature must be rejected"
    );
    assert!(
        link_run(&prog(ad_up, sig_up, "true")).join("").contains("verification"),
        "missing user-verification flag must be rejected when required"
    );
}

/// `encoding.hex_to_base64url` — base64url (no padding) of bytes given as hex.
#[test]
fn encoding_base64url_of_hex_matches() {
    // hex("test-challenge") -> base64url "dGVzdC1jaGFsbGVuZ2U" (WebAuthn challenge form).
    let p = "import encoding\nfn main(console: Console):\n    console.print(encoding.hex_to_base64url(\"746573742d6368616c6c656e6765\").unwrap_or(\"?\"))\n";
    assert_eq!(link_run(p), vec!["dGVzdC1jaGFsbGVuZ2U"]);
}

/// `crypto.ed25519_verify` runs in the *compiled WASM backend* too — bridged
/// into the sandbox as a host import that calls the same `native` registry
/// the interpreter uses, so the two tiers agree. (The native module runs at
/// full Rust speed on the host; the sandbox only sees this one pure import.)
#[test]
fn crypto_ed25519_verify_runs_in_the_wasm_backend() {
    use ed25519_dalek::{Signer, SigningKey};
    let sk = SigningKey::from_bytes(&[9u8; 32]);
    let hex = |bs: &[u8]| -> String { bs.iter().map(|b| format!("{b:02x}")).collect() };
    let pk = hex(sk.verifying_key().as_bytes());
    let msg = "wasm-signed";
    let sig = hex(&sk.sign(msg.as_bytes()).to_bytes());
    let prog = |m: &str| {
        format!(
            "import crypto\nfn main(console: Console):\n    match crypto.ed25519_verify(\"{pk}\", \"{m}\", \"{sig}\"):\n        Ok(true) -> console.print(\"ok\")\n        Ok(false) -> console.print(\"bad\")\n        Err(_e) -> console.print(\"err\")\n"
        )
    };
    // Genuine signature verifies in both backends; a tampered message fails
    // in both — the WASM host import and the interpreter agree.
    assert_eq!(wasm_run(&prog(msg)), vec!["ok"]);
    assert_eq!(link_run(&prog(msg)), vec!["ok"]);
    assert_eq!(wasm_run(&prog("tampered")), vec!["bad"]);
    assert_eq!(link_run(&prog("tampered")), vec!["bad"]);
}
