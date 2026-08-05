use super::*;
use crate::interpreter;

    /// WebAuthn treats authenticatorData as a trust-boundary decoder input. A
    /// malformed hex flag byte must fail as malformed input before semantic flag
    /// checks can turn it into a misleading user-presence error.
    #[test]
    fn webauthn_authenticator_data_rejects_malformed_hex_before_flags_on_both_backends() {
        let src = r#"import crypto
import webauthn

fn main(console: Console):
    let rp = "example.com"
    let client = "{\"type\":\"webauthn.get\",\"challenge\":\"c\",\"origin\":\"https://example.com\"}"
    let malformed = crypto.sha256(rp) + "zz00000000"
    match webauthn.verify_assertion("00", malformed, client, "00", "c", "https://example.com", rp, true):
        Ok(v) -> console.print("ok ${v}")
        Err(e) ->
            match e:
                webauthn.AuthenticatorDataHex(_) -> console.print("typed")
                _ -> console.print("wrong")
            console.print(webauthn.assertion_error_message(e))
"#;

        let interp = link_run(src);
        assert_eq!(interp.len(), 2, "interpreter produced unexpected output: {interp:?}");
        assert_eq!(interp[0], "typed", "interpreter must expose typed malformed-authenticatorData error");
        assert!(
            interp[1].starts_with("authenticatorData is not valid hex:"),
            "interpreter must reject malformed authenticatorData as malformed hex: {interp:?}"
        );
        assert!(
            !interp[1].contains("user-presence flag not set"),
            "interpreter must not report a semantic flag error for malformed hex: {interp:?}"
        );

        let wasm = run_linked_on_wasm(&[("main", src)], "main");
        assert_eq!(wasm.len(), 2, "WASM produced unexpected output: {wasm:?}");
        assert_eq!(wasm[0], "typed", "WASM must expose typed malformed-authenticatorData error");
        assert!(
            wasm[1].starts_with("authenticatorData is not valid hex:"),
            "WASM must reject malformed authenticatorData as malformed hex: {wasm:?}"
        );
        assert!(
            !wasm[0].contains("user-presence flag not set"),
            "WASM must not report a semantic flag error for malformed hex: {wasm:?}"
        );
    }

    /// JWT/OIDC registered claims are trust-boundary inputs. Missing or
    /// wrong-shaped claims must fail as malformed payloads after the RS256
    /// signature verifies, not default into ordinary expiry/mismatch outcomes.
    #[test]
    fn jwt_registered_claims_reject_malformed_values_on_both_backends() {
        use crate::idp::RegistryKey;

        fn b64url(bytes: &[u8]) -> String {
            const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
            let mut out = String::new();
            for c in bytes.chunks(3) {
                let n = ((c[0] as u32) << 16)
                    | ((*c.get(1).unwrap_or(&0) as u32) << 8)
                    | (*c.get(2).unwrap_or(&0) as u32);
                out.push(ALPHABET[(n >> 18 & 63) as usize] as char);
                out.push(ALPHABET[(n >> 12 & 63) as usize] as char);
                if c.len() > 1 {
                    out.push(ALPHABET[(n >> 6 & 63) as usize] as char);
                }
                if c.len() > 2 {
                    out.push(ALPHABET[(n & 63) as usize] as char);
                }
            }
            out
        }

        fn signed_jwt(key: &RegistryKey, payload_json: &str) -> String {
            let header = b64url(br#"{"alg":"RS256","typ":"JWT"}"#);
            let payload = b64url(payload_json.as_bytes());
            let signing_input = format!("{header}.{payload}");
            let sig = key.sign(signing_input.as_bytes()).expect("sign JWT");
            format!("{signing_input}.{}", b64url(&sig))
        }

        let dir = std::env::temp_dir().join(format!("witchy_jwt_claims_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let key = RegistryKey::load_or_create(&dir).expect("issuer key");
        let pubkey = key.public_hex();
        let missing_exp = signed_jwt(
            &key,
            r#"{"iss":"https://issuer","sub":"s","aud":"aud","iat":1}"#,
        );
        let wrong_aud = signed_jwt(
            &key,
            r#"{"iss":"https://issuer","sub":"s","aud":123,"exp":2000000000,"iat":1}"#,
        );
        let wrong_iss = signed_jwt(
            &key,
            r#"{"iss":123,"sub":"s","aud":"aud","exp":2000000000,"iat":1}"#,
        );

        let src = format!(
            r#"import jwt

fn report(console: Console, token: String):
    match jwt.verify_oidc(token, "{pubkey}", "https://issuer", "aud", 1000):
        Ok(_) -> console.print("ok")
        Err(e) ->
            match e:
                jwt.MissingClaim(name) -> console.print("missing:" + name)
                jwt.AudienceClaimExpected -> console.print("aud-shape")
                jwt.StringClaimExpected(name) -> console.print("string:" + name)
                _ -> console.print("other")
            console.print(jwt.jwt_error_message(e))

fn main(console: Console):
    report(console, "{missing_exp}")
    report(console, "{wrong_aud}")
    report(console, "{wrong_iss}")
"#
        );
        let want = [
            "missing:exp",
            "JWT payload is missing `exp`",
            "aud-shape",
            "JWT payload `aud` must be a string or array of strings",
            "string:iss",
            "JWT payload `iss` must be a string",
        ];
        assert_eq!(link_run(&src), want, "interpreter must reject malformed registered claims");
        assert_eq!(
            run_linked_on_wasm(&[("main", &src)], "main"),
            want,
            "WASM must reject malformed registered claims"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `std/path` does pure '/'-path surgery: base/dir/ext/stem, join (an absolute
    /// right-hand side replaces), and normalize (collapsing `.`/`..`, never
    /// escaping an absolute root, keeping leading `..` when relative).
    #[test]
    fn path_module_components_and_normalize() {
        let src = r#"import path
import option

fn main(console: Console):
    console.print(path.base("a/b/c.txt") + "|" + (path.dir("a/b/c.txt") ?? "<none>"))
    console.print((path.ext("a/b.tar.gz") ?? "<none>") + "|" + path.stem("a/b.tar.gz"))
    console.print("[" + (path.ext(".bashrc") ?? "<none>") + "]|" + path.base("a/b/"))
    console.print((path.dir("c") ?? "<none>") + "|" + (path.ext("README") ?? "<none>"))
    console.print(path.join("a/b", "c") + "|" + path.join("a", "/x"))
    console.print(path.normalize("a/./b/../c/") + "|" + path.normalize("/a/b/../../../x"))
    console.print(path.normalize("../a/../../b"))
"#;
        assert_eq!(
            link_run(src),
            vec![
                "c.txt|a/b",
                "gz|b.tar",
                "[<none>]|b",
                "<none>|<none>",
                "a/b/c|/x",
                "a/c|/x",
                "../../b",
            ]
        );
    }

    /// `fs.collect_files(root, "", "", ext)` walks from the Dir root itself:
    /// root-level files are collected with bare relative paths (never "/name"),
    /// and root-level directories recurse instead of being silently skipped by
    /// the confinement resolver rejecting absolute-looking paths.
    #[test]
    fn fs_collect_files_walks_from_dir_root() {
        let dir = std::env::temp_dir().join(format!("witchy-collect-root-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("top.witchy"), "// top").unwrap();
        std::fs::write(dir.join("sub/nested.witchy"), "// nested").unwrap();
        std::fs::write(dir.join("skip.txt"), "no").unwrap();
        let src = r#"import fs
import list

fn main(console: Console, root: Dir):
    var names = []
    for f in fs.collect_files(root, "", "", ".witchy"):
        let (rel, _contents) = f
        list.push(names, rel)
    list.sort(names)
    console.print(list.join(names, ","))
"#;
        let linked = resolve_std_src(src);
        let interpreted =
            interpreter::run_module(linked, dir.to_str().unwrap(), Vec::new()).expect("interp run");
        assert_eq!(interpreted, vec!["sub/nested.witchy,top.witchy"]);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// `std/time` computes the civil UTC date from a unix timestamp (Hinnant's
    /// days<->civil algorithm), cross-checked against Python's datetime: leap
    /// years, weekday, an exact round-trip, and a pre-1970 timestamp (floor
    /// division, so the day is right).
    #[test]
    fn time_module_civil_date_from_unix() {
        let src = r#"import time

fn main(console: Console):
    console.print(time.iso8601(time.from_unix(1780000000)))
    console.print(time.weekday_name(time.from_unix(0)) + " " + time.iso8601(time.from_unix(0)))
    console.print(time.iso8601(time.from_unix(-86401)))
    console.print(yn(time.is_leap(2000)) + yn(time.is_leap(1900)) + yn(time.is_leap(2024)))
    console.print(yn(time.to_unix(time.from_unix(1780000000)) == 1780000000))

fn yn(b: Bool) -> String:
    if b: "y" else: "n"
"#;
        assert_eq!(
            link_run(src),
            vec![
                "2026-05-28T20:26:40Z",
                "Thursday 1970-01-01T00:00:00Z",
                "1969-12-30T23:59:59Z",
                "yny",
                "y",
            ]
        );
    }

    /// `std/fs` parent_dir + (with a real Dir) the recursive collect — exercised
    /// here for the pure part to confirm the module's functions resolve on import.
    #[test]
    fn fs_module_parent_dir_resolves() {
        let src = "import fs\nimport option\nfn main(console: Console):\n    console.print(fs.parent_dir(\"a/b/c\") ?? \"<none>\")\n    console.print(fs.parent_dir(\"top\") ?? \"<none>\")\n";
        assert_eq!(link_run(src), vec!["a/b", "<none>"]);
    }

    /// REGRESSION (BUG-408/BUG-250): `jwt.rsa_key_from_jwk` rejects an empty JWK (it
    /// used to return an `Ok` bogus key), and `verify_oidc` pins the header `alg` to
    /// RS256 — an `alg: none` token is refused before the signature is even checked
    /// (algorithm-confusion defense, fail closed). Identical on both backends.
    #[test]
    fn jwt_rejects_empty_jwk_and_non_rs256_alg_backends_agree() {
        let src = "import jwt\nfn tag(r: Result(String, jwt.JwtError)) -> String:\n    match r:\n        Ok(_k) -> \"ok\"\n        Err(_e) -> \"err\"\nfn main(console: Console):\n    console.print(tag(jwt.rsa_key_from_jwk(\"\", \"\")))\n    let token = \"eyJhbGciOiJub25lIn0.eyJpc3MiOiJpIiwiYXVkIjoiYSIsImV4cCI6OTk5OTk5OTk5OX0.AAAA\"\n    match jwt.verify_oidc(token, \"00\", \"i\", \"a\", 0):\n        Ok(_c) -> console.print(\"accepted\")\n        Err(e) -> console.print(jwt.jwt_error_message(e))\n";
        let expected = ["err", "JWT `alg` is `none`, not the required `RS256`"];
        let linked = resolve_std_src(src);
        assert_eq!(
            interpreter::run_module(linked, ".", Vec::new()).expect("interp"),
            expected,
            "interp"
        );
        assert_eq!(wasm_run(src), expected, "wasm");
    }
