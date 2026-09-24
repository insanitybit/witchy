use super::link_run;

/// Property tests over the standard library: invariants that must hold for
/// *any* input — encode/decode round-trips, calendar inverses, semver
/// rendering — checked by generating the input, running it through the witchy
/// stdlib, and comparing to a Rust reference. These catch edge cases (empty
/// strings, embedded quotes/newlines, negative timestamps) unit tests miss.
mod stdlib_properties {
    use super::super::{link_run_args, proptest_cases};
    use proptest::prelude::*;

    const HEX_ENCODE_PROG: &str = "import encoding\nimport list\nfn main(console: Console, argv: List(String)):\n    let s = list.at(argv, 0)\n    console.print(encoding.hex_encode(s))\n";
    const BASE64_PROG: &str = "import encoding\nimport list\nfn main(console: Console, argv: List(String)):\n    let s = list.at(argv, 0)\n    console.print(yn(encoding.base64_decode(encoding.base64_encode(s)).unwrap_or(\"?\") == s))\n\nfn yn(b: Bool) -> String:\n    if b: \"y\" else: \"n\"\n";
    const SEMVER_PROG: &str = "import semver\nimport list\nfn main(console: Console, argv: List(String)):\n    let v = list.at(argv, 0)\n    match semver.parse(v):\n        Ok(x) -> console.print(semver.format(x))\n        Err(e) -> console.print(\"err\")\n";

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(proptest_cases()))]

        /// `encoding.hex_encode` equals the byte-wise lowercase hex reference.
        #[test]
        fn hex_encode_matches_reference(s in "[ -#%-z|~]{0,40}") {
            let reference: String = s.bytes().map(|b| format!("{b:02x}")).collect();
            prop_assert_eq!(link_run_args(HEX_ENCODE_PROG, vec![s]), vec![reference]);
        }

        /// base64 decode is the inverse of encode, for any printable ASCII.
        #[test]
        fn base64_roundtrips(s in "[ -#%-z|~]{0,48}") {
            prop_assert_eq!(link_run_args(BASE64_PROG, vec![s]), vec!["y".to_string()]);
        }

        /// `semver.format` after `parse` reproduces the canonical version.
        #[test]
        fn semver_roundtrips(a in 0i64..2000, b in 0i64..2000, c in 0i64..2000) {
            let v = format!("{a}.{b}.{c}");
            prop_assert_eq!(link_run_args(SEMVER_PROG, vec![v.clone()]), vec![v]);
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

/// `std/webauthn.verify_registration` (RFC-0099) verifies a server-challenged
/// WebAuthn *create* ceremony: it CBOR-decodes the attestationObject (via the new
/// `std/cbor` decoder), checks rpIdHash + presence/verification flags, and extracts
/// the attested COSE ES256 key as an uncompressed SEC1 point. Built from a
/// synthetic-but-well-formed attestation (rpIdHash re-derived in-program) so it
/// needs no live authenticator, and it agrees on BOTH backends.
#[test]
fn webauthn_verify_registration_parses_the_create_ceremony() {
    let prog = |challenge: &str| {
        format!(
"import webauthn
import crypto
fn xrep(h: String, n: Int) -> String:
    var out = \"\"
    for _i in 0..n:
        out = out + h
    out
fn main(console: Console):
    let rp = \"coven.example\"
    let cose = \"a5010203262001215820\" + xrep(\"ab\", 32) + \"225820\" + xrep(\"cd\", 32)
    let auth = crypto.sha256(rp) + \"45\" + \"00000000\" + xrep(\"00\", 16) + \"0004\" + \"01020304\" + cose
    let att = \"a363666d74646e6f6e656761747453746d74a06861757468446174615888\" + auth
    let cdj = \"{{\\\"type\\\":\\\"webauthn.create\\\",\\\"challenge\\\":\\\"{challenge}\\\",\\\"origin\\\":\\\"https://coven.example\\\"}}\"
    match webauthn.verify_registration(cdj, att, \"c\", \"https://coven.example\", rp, true):
        Ok(cred) -> console.print(cred.public_key_hex)
        Err(e) -> console.print(webauthn.registration_error_message(e))
"
        )
    };
    let expected = format!("04{}{}", "ab".repeat(32), "cd".repeat(32));
    assert_eq!(
        link_run(&prog("c")),
        vec![expected],
        "a valid create ceremony yields the attested ES256 public key"
    );
    assert!(
        link_run(&prog("WRONG")).join("").contains("challenge"),
        "a create ceremony whose clientData challenge != the server's must be rejected"
    );
}

