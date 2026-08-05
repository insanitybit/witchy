use super::*;
use crate::{codegen, interpreter, parser, typeck};

    #[cfg(not(target_arch = "wasm32"))]
    fn b64url(bytes: &[u8]) -> String {
        const ALPHABET: &[u8] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let value = ((chunk[0] as u32) << 16)
                | ((*chunk.get(1).unwrap_or(&0) as u32) << 8)
                | (*chunk.get(2).unwrap_or(&0) as u32);
            out.push(ALPHABET[(value >> 18 & 63) as usize] as char);
            out.push(ALPHABET[(value >> 12 & 63) as usize] as char);
            if chunk.len() > 1 {
                out.push(ALPHABET[(value >> 6 & 63) as usize] as char);
            }
            if chunk.len() > 2 {
                out.push(ALPHABET[(value & 63) as usize] as char);
            }
        }
        out
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn hex_string(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn sign_rs256(key: &aws_lc_rs::rsa::KeyPair, payload: &str) -> String {
        let signed = format!(
            "{}.{}",
            b64url(br#"{"alg":"RS256","typ":"JWT"}"#),
            b64url(payload.as_bytes())
        );
        let mut signature = vec![0u8; key.public_modulus_len()];
        key.sign(
            &aws_lc_rs::signature::RSA_PKCS1_SHA256,
            &aws_lc_rs::rand::SystemRandom::new(),
            signed.as_bytes(),
            &mut signature,
        )
        .expect("sign");
        format!("{signed}.{}", b64url(&signature))
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn der_two_ints(der: &[u8]) -> (Vec<u8>, Vec<u8>) {
        fn length(bytes: &[u8], index: &mut usize) -> usize {
            let mut len = bytes[*index] as usize;
            *index += 1;
            if len & 0x80 != 0 {
                let count = len & 0x7f;
                len = 0;
                for _ in 0..count {
                    len = (len << 8) | bytes[*index] as usize;
                    *index += 1;
                }
            }
            len
        }
        fn integer(bytes: &[u8], index: &mut usize) -> Vec<u8> {
            *index += 1;
            let len = length(bytes, index);
            let value = bytes[*index..*index + len].to_vec();
            *index += len;
            value
        }
        let mut index = 1;
        let _ = length(der, &mut index);
        (integer(der, &mut index), integer(der, &mut index))
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn tls_server_fixture() -> (std::sync::Arc<rustls::ServerConfig>, String) {
        let certificate =
            rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).expect("cert");
        let cert_der = certificate.cert.der().clone();
        let key_der = rustls::pki_types::PrivatePkcs8KeyDer::from(
            certificate.key_pair.serialize_der(),
        );
        let config = rustls::ServerConfig::builder_with_provider(
            rustls::crypto::aws_lc_rs::default_provider().into(),
        )
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![cert_der],
            rustls::pki_types::PrivateKeyDer::Pkcs8(key_der),
        )
        .unwrap();
        (std::sync::Arc::new(config), certificate.cert.pem())
    }

    /// RS256 (`crypto.rsa_pkcs1_sha256_verify`, the OIDC/JWT signature algorithm) is
    /// reachable on both backends — a malformed key/signature yields `Err`, never
    /// a trap. (The verify LOGIC is proven by `rs256_native_roundtrip_verifies`.)
    #[test]
    fn rsa_pkcs1_sha256_verify_total_backends_agree() {
        let src = "import crypto\nfn main(console: Console):\n    match crypto.rsa_pkcs1_sha256_verify(\"00\", \"msg\", \"00\"):\n        Err(_e) -> console.print(\"malformed\")\n        Ok(true) -> console.print(\"valid\")\n        Ok(false) -> console.print(\"invalid\")\n";
        let expected = vec!["malformed".to_string()];
        assert_eq!(link_run(src), expected, "interp");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "wasm");
    }

    /// RS256 verify LOGIC is correct: a real RSA-2048 PKCS#1 signature over a message
    /// verifies, a wrong message is rejected, and a malformed key is reported —
    /// exercising the native aws-lc path both backends route through.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn rs256_native_roundtrip_verifies() {
        use witchy_runtime::value::NativeValue as NV;
        use aws_lc_rs::signature::KeyPair; // brings `public_key()` into scope
        let kp = aws_lc_rs::rsa::KeyPair::generate(aws_lc_rs::rsa::KeySize::Rsa2048).expect("keygen");
        let pk_hex = hex_string(kp.public_key().as_ref());
        let msg = "hello rs256";
        let mut sig = vec![0u8; kp.public_modulus_len()];
        kp.sign(
            &aws_lc_rs::signature::RSA_PKCS1_SHA256,
            &aws_lc_rs::rand::SystemRandom::new(),
            msg.as_bytes(),
            &mut sig,
        )
        .expect("sign");
        let sig_hex = hex_string(&sig);
        // Reach the private intrinsic through the native registry; std/crypto maps
        // this status into the public Result API.
        let f = witchy_runtime::native::lookup("crypto.__rsa_pkcs1_sha256_verify_status")
            .expect("registered");
        let verify = |pk: &str, m: &str, s: &str| {
            f(&[NV::Str(pk.into()), NV::Str(m.into()), NV::Str(s.into())]).unwrap()
        };
        assert_eq!(verify(&pk_hex, msg, &sig_hex), NV::Int(1), "valid RS256 signature verifies");
        assert_eq!(verify(&pk_hex, "tampered", &sig_hex), NV::Int(0), "wrong message rejected");
        assert_eq!(verify("00", msg, &sig_hex), NV::Int(-1), "malformed key is reported");
    }

    /// End-to-end `std/jwt`: a REAL aws-lc-signed compact RS256 JWT, embedded as a
    /// witchy string literal, verifies identically on both backends — `Ok` yields the
    /// claims (we read `sub`); a tampered signature, an expired token, and a wrong
    /// audience each reject with the module's reason. Proves `jwt.verify_rs256`
    /// composes RS256 + base64url + json end to end, with no host capability.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn jwt_verify_rs256_backends_agree() {
        use aws_lc_rs::signature::KeyPair;
        // base64url, no padding — the JWT segment encoding.
        let kp = aws_lc_rs::rsa::KeyPair::generate(aws_lc_rs::rsa::KeySize::Rsa2048).expect("keygen");
        let pk_hex = hex_string(kp.public_key().as_ref());
        let good = sign_rs256(&kp, r#"{"aud":"coven","exp":9999,"sub":"octocat"}"#);
        let expired = sign_rs256(&kp, r#"{"aud":"coven","exp":5,"sub":"octocat"}"#);
        let wrong_aud = sign_rs256(&kp, r#"{"aud":"evil","exp":9999,"sub":"octocat"}"#);
        let tampered = {
            // Flip the FIRST char of the signature segment. base64url's last char of a
            // 256-byte RSA signature carries only 2 significant bits (4 are padding), so
            // a last-char flip can decode to the same bytes — a no-op; the first char is
            // always fully significant, so this reliably corrupts the signature.
            let sig_start = good.rfind('.').unwrap() + 1;
            let mut chars: Vec<char> = good.chars().collect();
            chars[sig_start] = if chars[sig_start] == 'A' { 'B' } else { 'A' };
            chars.into_iter().collect::<String>()
        };
        // `now` = 1000, audience "coven". Print `sub` on success, else the error.
        let run = |token: &str| -> Vec<String> {
            let src = format!(
                "import jwt\nimport json\nfn main(console: Console):\n    match jwt.verify_rs256(\"{token}\", \"{pk_hex}\", \"coven\", 1000):\n        Ok(claims) -> console.print(json.get_string(claims, \"sub\").unwrap_or(\"?\"))\n        Err(e) -> console.print(jwt.jwt_error_message(e))\n"
            );
            let interp = link_run(&src);
            let wasm = run_linked_on_wasm(&[("main", src.as_str())], "main");
            assert_eq!(interp, wasm, "interp vs wasm must agree");
            interp
        };
        assert_eq!(run(&good), vec!["octocat".to_string()], "valid JWT yields its claims");
        assert_eq!(run(&expired), vec!["JWT has expired".to_string()]);
        assert_eq!(
            run(&wrong_aud),
            vec!["JWT audience mismatch (wrong relying party / replay)".to_string()]
        );
        assert_eq!(
            run(&tampered),
            vec!["JWT signature is invalid (untrusted or forged)".to_string()]
        );
    }

    /// `jwt.rsa_key_from_jwk` reconstructs a DER PKCS#1 RSA public key from a JWK's
    /// base64url `n`/`e` BYTE-FOR-BYTE identically to aws-lc's own encoding — the pure-
    /// witchy ASN.1 DER (length long-form, the signed-integer `00` pad) is exact, and
    /// matches on both backends. This is the bridge from a JWKS entry to `verify_rs256`.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn jwt_rsa_key_from_jwk_matches_aws_lc_der() {
        use aws_lc_rs::signature::KeyPair;
        let kp = aws_lc_rs::rsa::KeyPair::generate(aws_lc_rs::rsa::KeySize::Rsa2048).expect("keygen");
        let der = kp.public_key().as_ref();
        let (n_int, e_int) = der_two_ints(der);
        // The JWK carries the unsigned magnitude — drop the DER sign byte if present.
        let strip = |v: &[u8]| if v.first() == Some(&0) { v[1..].to_vec() } else { v.to_vec() };
        let n_b64 = b64url(&strip(&n_int));
        let e_b64 = b64url(&strip(&e_int));
        let src = format!(
            "import jwt\nfn main(console: Console):\n    console.print(jwt.rsa_key_from_jwk(\"{n_b64}\", \"{e_b64}\").unwrap_or(\"?\"))\n"
        );
        let expected = vec![hex_string(der)];
        assert_eq!(link_run(&src), expected, "interp: JWK->DER byte-exact vs aws-lc");
        assert_eq!(run_linked_on_wasm(&[("main", src.as_str())], "main"), expected, "wasm");
    }

    /// `jwt.verify_oidc` is the full relying-party check: a real RS256 GitHub-Actions-
    /// shaped OIDC token verifies only against its TRUE issuer (the bind to a trusted
    /// provider), and rejects a not-yet-active (`nbf`) token. On success the caller reads
    /// identity claims — here the `repository` a trusted-publishing flow would authorize.
    /// Both backends agree. This is the verification half of OIDC login / publishing.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn jwt_verify_oidc_binds_issuer_backends_agree() {
        use aws_lc_rs::signature::KeyPair;
        let kp = aws_lc_rs::rsa::KeyPair::generate(aws_lc_rs::rsa::KeySize::Rsa2048).expect("keygen");
        let pk_hex = hex_string(kp.public_key().as_ref());
        let gh = "https://token.actions.githubusercontent.com";
        let token = sign_rs256(
            &kp,
            r#"{"iss":"https://token.actions.githubusercontent.com","aud":"coven","sub":"repo:octo/witchy:ref:refs/heads/main","repository":"octo/witchy","nbf":0,"iat":900,"exp":1200}"#,
        );
        let future = sign_rs256(
            &kp,
            r#"{"iss":"https://token.actions.githubusercontent.com","aud":"coven","repository":"octo/witchy","nbf":5000,"iat":900,"exp":5200}"#,
        );
        let long_lived = sign_rs256(
            &kp,
            r#"{"iss":"https://token.actions.githubusercontent.com","aud":"coven","repository":"octo/witchy","iat":1000,"exp":1601}"#,
        );
        let missing_iat = sign_rs256(
            &kp,
            r#"{"iss":"https://token.actions.githubusercontent.com","aud":"coven","repository":"octo/witchy","exp":1200}"#,
        );
        let future_iat = sign_rs256(
            &kp,
            r#"{"iss":"https://token.actions.githubusercontent.com","aud":"coven","repository":"octo/witchy","iat":1061,"exp":1200}"#,
        );
        let skew_boundary = sign_rs256(
            &kp,
            r#"{"iss":"https://token.actions.githubusercontent.com","aud":"coven","repository":"octo/witchy","iat":1060,"exp":1200}"#,
        );
        // (token, issuer-to-trust) -> printed line. now = 1000, audience "coven".
        let run = |tok: &str, issuer: &str| -> Vec<String> {
            let src = format!(
                "import jwt\nimport json\nfn main(console: Console):\n    match jwt.verify_oidc(\"{tok}\", \"{pk_hex}\", \"{issuer}\", \"coven\", 1000):\n        Ok(claims) -> console.print(json.get_string(claims, \"repository\").unwrap_or(\"?\"))\n        Err(e) -> console.print(jwt.jwt_error_message(e))\n"
            );
            let interp = link_run(&src);
            assert_eq!(interp, run_linked_on_wasm(&[("main", src.as_str())], "main"), "backends agree");
            interp
        };
        assert_eq!(run(&token, gh), vec!["octo/witchy".to_string()], "trusted issuer admits, claims readable");
        assert_eq!(
            run(&token, "https://evil.example"),
            vec!["JWT issuer mismatch (untrusted identity provider)".to_string()],
            "a token from the wrong issuer is rejected even with a valid signature"
        );
        assert_eq!(
            run(&future, gh),
            vec!["JWT is not yet valid (nbf is in the future)".to_string()]
        );

        // BUG-068: high-value relying parties opt into an explicit maximum signed
        // lifetime and future-iat skew. This is application policy, not a silent
        // change to generic OIDC verification. Typed errors expose the rejected
        // claim relationship without requiring callers to parse display text.
        let run_fresh = |tok: &str| -> Vec<String> {
            let src = format!(
                r#"import jwt
import json
fn main(console: Console):
    match jwt.verify_oidc_fresh("{tok}", "{pk_hex}", "{gh}", "coven", 1000, 600, 60):
        Ok(claims) -> console.print("ok:" + json.get_string(claims, "repository").unwrap_or("?"))
        Err(e) ->
            match e:
                jwt.TokenLifetimeTooLong(actual, maximum) -> console.print("ttl:${{actual}}:${{maximum}}")
                jwt.IssuedAtInFuture(iat, skew) -> console.print("future:${{iat}}:${{skew}}")
                jwt.MissingClaim(name) -> console.print("missing:" + name)
                _ -> console.print(jwt.jwt_error_message(e))
"#
            );
            let interp = link_run(&src);
            assert_eq!(interp, run_linked_on_wasm(&[("main", src.as_str())], "main"), "freshness policy backends agree");
            interp
        };
        assert_eq!(run_fresh(&token), vec!["ok:octo/witchy".to_string()]);
        assert_eq!(run_fresh(&skew_boundary), vec!["ok:octo/witchy".to_string()]);
        assert_eq!(run_fresh(&long_lived), vec!["ttl:601:600".to_string()]);
        assert_eq!(run_fresh(&missing_iat), vec!["missing:iat".to_string()]);
        assert_eq!(run_fresh(&future_iat), vec!["future:1061:60".to_string()]);
    }

    /// OIDC Core §3.1.3.7 (BUG-270): when an ID token names MORE THAN ONE audience,
    /// `azp` must be present and must be THIS client. A single-audience token needs
    /// no `azp`; a multi-audience token is admitted only when `azp` == our client id,
    /// and rejected when `azp` is absent or names a co-audience — so a token minted
    /// for several parties cannot be replayed at ours. A wrong `azp` is an
    /// authorization mismatch; a missing `azp` is a malformed registered claim.
    /// Real RS256, both backends.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn jwt_verify_oidc_enforces_azp_for_multi_audience_backends_agree() {
        use aws_lc_rs::signature::KeyPair;
        let kp = aws_lc_rs::rsa::KeyPair::generate(aws_lc_rs::rsa::KeySize::Rsa2048).expect("keygen");
        let pk_hex = hex_string(kp.public_key().as_ref());
        let iss = "https://accounts.google.com";
        // Single audience "myclient" — no azp required.
        let single = sign_rs256(&kp, r#"{"iss":"https://accounts.google.com","aud":"myclient","sub":"u1","nbf":0,"exp":9999}"#);
        // Multi-audience including us, azp == us: admitted.
        let multi_ok = sign_rs256(&kp, r#"{"iss":"https://accounts.google.com","aud":["myclient","other"],"azp":"myclient","sub":"u2","nbf":0,"exp":9999}"#);
        // Multi-audience including us, but azp names a CO-AUDIENCE: rejected.
        let multi_wrong = sign_rs256(&kp, r#"{"iss":"https://accounts.google.com","aud":["myclient","other"],"azp":"other","sub":"u3","nbf":0,"exp":9999}"#);
        // Multi-audience including us, azp ABSENT: rejected.
        let multi_missing = sign_rs256(&kp, r#"{"iss":"https://accounts.google.com","aud":["myclient","other"],"sub":"u4","nbf":0,"exp":9999}"#);
        // audience = "myclient", now = 1000.
        let run = |tok: &str| -> Vec<String> {
            let src = format!(
                "import jwt\nimport json\nfn main(console: Console):\n    match jwt.verify_oidc(\"{tok}\", \"{pk_hex}\", \"{iss}\", \"myclient\", 1000):\n        Ok(claims) -> console.print(json.get_string(claims, \"sub\").unwrap_or(\"?\"))\n        Err(e) -> console.print(jwt.jwt_error_message(e))\n"
            );
            let interp = link_run(&src);
            assert_eq!(interp, run_linked_on_wasm(&[("main", src.as_str())], "main"), "backends agree");
            interp
        };
        assert_eq!(run(&single), vec!["u1".to_string()], "single-audience token needs no azp");
        assert_eq!(run(&multi_ok), vec!["u2".to_string()], "multi-audience with matching azp is admitted");
        let azp_err = "JWT `azp` mismatch: a multi-audience OIDC token must name this client as the authorized party".to_string();
        assert_eq!(run(&multi_wrong), vec![azp_err], "azp naming a co-audience is rejected");
        assert_eq!(
            run(&multi_missing),
            vec!["JWT payload is missing `azp`".to_string()],
            "multi-audience with no azp is rejected as malformed"
        );
    }

    /// The full OIDC-via-JWKS verification (how "Log in with Google" / GitHub-Actions
    /// publishing checks an id_token): read the token's `kid`, pick the matching RSA key
    /// from the provider's published JWKS, and `verify_oidc`. Exercised against a REAL
    /// aws-lc-signed id_token + a JWKS built from the same key — identical on both backends.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn jwt_verify_oidc_via_jwks_backends_agree() {
        use aws_lc_rs::signature::KeyPair;
        let kp = aws_lc_rs::rsa::KeyPair::generate(aws_lc_rs::rsa::KeySize::Rsa2048).expect("keygen");
        let (n_int, e_int) = der_two_ints(kp.public_key().as_ref());
        let strip = |v: &[u8]| if v.first() == Some(&0) { v[1..].to_vec() } else { v.to_vec() };
        let jwks = format!(
            r#"{{"keys":[{{"kty":"RSA","kid":"google-key-1","n":"{}","e":"{}"}}]}}"#,
            b64url(&strip(&n_int)),
            b64url(&strip(&e_int))
        );
        let signed = format!(
            "{}.{}",
            b64url(br#"{"alg":"RS256","kid":"google-key-1","typ":"JWT"}"#),
            b64url(br#"{"iss":"https://accounts.google.com","aud":"myclient","email":"a@b.com","sub":"42","exp":9999,"nbf":0}"#)
        );
        let mut sig = vec![0u8; kp.public_modulus_len()];
        kp.sign(
            &aws_lc_rs::signature::RSA_PKCS1_SHA256,
            &aws_lc_rs::rand::SystemRandom::new(),
            signed.as_bytes(),
            &mut sig,
        )
        .expect("sign");
        let token = format!("{signed}.{}", b64url(&sig));
        let jwks_lit = jwks.replace('"', "\\\"");
        let src = format!(
            "import jwt\nimport json\nfn main(console: Console):\n    match json.decode(\"{jwks_lit}\"):\n        Err(e) -> console.print(\"bad jwks\")\n        Ok(doc) ->\n            match jwt.kid(\"{token}\"):\n                None -> console.print(\"no kid\")\n                Some(k) ->\n                    match jwt.rsa_key_for_kid(doc, k):\n                        Err(e) -> console.print(\"key: \" + jwt.jwt_error_message(e))\n                        Ok(der) ->\n                            match jwt.verify_oidc(\"{token}\", der, \"https://accounts.google.com\", \"myclient\", 1000):\n                                Ok(claims) -> console.print(json.get_string(claims, \"email\").unwrap_or(\"?\"))\n                                Err(e) -> console.print(jwt.jwt_error_message(e))\n"
        );
        let expected = vec!["a@b.com".to_string()];
        assert_eq!(link_run(&src), expected, "interp OIDC-via-JWKS");
        assert_eq!(run_linked_on_wasm(&[("main", src.as_str())], "main"), expected, "wasm OIDC-via-JWKS");
    }

    /// `jwt.rsa_key_for_kid` distinguishes malformed issuer metadata from a
    /// non-matching key (BUG-408): a `keys` field that is present but NOT an array
    /// is a distinct "not an array" error, not a defaulted-empty "no matching key".
    /// A valid array with no matching kid still reports the no-match error. No key
    /// material needed — this exercises the JWKS-shape boundary. Both backends.
    #[test]
    fn jwt_rsa_key_for_kid_rejects_malformed_keys_backends_agree() {
        let run = |jwks_json: &str| -> Vec<String> {
            let lit = jwks_json.replace('"', "\\\"");
            let src = format!(
                "import jwt\nimport json\nfn main(console: Console):\n    match json.decode(\"{lit}\"):\n        Err(_e) -> console.print(\"bad json\")\n        Ok(doc) -> match jwt.rsa_key_for_kid(doc, \"k1\"):\n            Ok(_der) -> console.print(\"ok\")\n            Err(e) -> console.print(jwt.jwt_error_message(e))\n"
            );
            let interp = link_run(&src);
            assert_eq!(interp, run_linked_on_wasm(&[("main", src.as_str())], "main"), "backends agree");
            interp
        };
        assert_eq!(
            run(r#"{"keys":"nope"}"#),
            vec!["JWKS `keys` is not an array (malformed issuer metadata)".to_string()],
            "a wrong-typed `keys` is malformed metadata, not an empty key set"
        );
        assert_eq!(
            run(r#"{"keys":42}"#),
            vec!["JWKS `keys` is not an array (malformed issuer metadata)".to_string()],
            "a numeric `keys` is malformed metadata"
        );
        assert_eq!(
            run(r#"{"foo":1}"#),
            vec!["JWKS has no `keys` array".to_string()],
            "an absent `keys` is still its own error"
        );
        assert_eq!(
            run(r#"{"keys":[{"kty":"RSA","kid":"other","n":"x","e":"y"}]}"#),
            vec!["no RSA key in the JWKS matches kid `k1`".to_string()],
            "a valid array with no matching kid still reports no-match"
        );
    }

    /// `jwt.claims_unverified` decodes a token's payload WITHOUT checking the signature —
    /// for reading `iss` to select the verification key before `verify_oidc`. Both backends.
    #[test]
    fn jwt_claims_unverified_reads_routing_fields() {
        let src = "import jwt\nimport json\nimport encoding\nfn main(console: Console):\n    let payload = encoding.hex_to_base64url(encoding.hex_encode(\"{\\\"iss\\\":\\\"acme\\\",\\\"sub\\\":\\\"x\\\"}\")).unwrap_or(\"?\")\n    match jwt.claims_unverified(\"aaa.\" + payload + \".bbb\"):\n        Err(e) -> console.print(jwt.jwt_error_message(e))\n        Ok(claims) -> console.print(json.get_string(claims, \"iss\").unwrap_or(\"?\"))\n";
        let expected = vec!["acme".to_string()];
        assert_eq!(link_run(src), expected, "interp");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "wasm");
    }

    /// The `tls:` scheme is split off an address before the allowlist match: the
    /// capability governs the bare `host:port`, the scheme is a connect-time choice.
    #[test]
    fn tls_scheme_is_stripped_for_the_allowlist() {
        assert_eq!(
            witchy_runtime::net::parse_scheme("tls:github.com:443"),
            (true, "github.com:443"),
        );
        assert_eq!(
            witchy_runtime::net::parse_scheme("github.com:443"),
            (false, "github.com:443"),
        );
    }

    /// RFC-0011 carried-state: a SEALED record capability (`capability X:` with named
    /// fields) wraps a host capability AND carries policy data. It is footprint-
    /// transparent (audits as its cap fields), refines monotonically, and enforces its
    /// carried policy in the library's own operations — identically on both backends.
    #[test]
    fn carried_state_capability_runs_and_audits_through_record() {
        let src = "capability Postgres:\n    net: Net[Connect, Tcp]\n    table: String\npub fn connect(net: Net[Connect, Tcp]) -> Postgres:\n    Postgres(net, \"public\")\npub fn use_table(pg: Postgres, name: String) -> Postgres:\n    match pg:\n        Postgres(net, _) -> Postgres(net, name)\npub fn count_rows(pg: Postgres, requested: String) -> String:\n    match pg:\n        Postgres(_, table) ->\n            if requested == table:\n                \"ok: \" + requested\n            else:\n                \"denied: \" + requested\nfn main(console: Console, net: Net):\n    let users = use_table(connect(net), \"users\")\n    console.print(count_rows(users, \"users\"))\n    console.print(count_rows(users, \"secrets\"))\n";
        let want = vec!["ok: users".to_string(), "denied: secrets".to_string()];
        assert_eq!(link_run_net(src, &[]), want, "interpreter");
        assert_eq!(run_linked_on_wasm_net(&[("main", src)], "main", &[]), want, "compiled WASM must agree");

        // Footprint sees through the record: the sealed `Postgres` (a `Net` + a
        // `String`) audits as exactly `Net` — the carried `String` adds no authority.
        let module = parser::parse_module(src).expect("parse");
        let fp = crate::capabilities::analyze(&module);
        let connect_fn = fp.per_function.iter().find(|e| e.name == "connect").expect("connect entry");
        let keys: Vec<&str> = connect_fn.capabilities.keys().copied().collect();
        assert_eq!(keys, vec!["Net"], "carried String adds no authority — Postgres audits as Net only");
    }

    /// A sealed record capability is OPAQUE: its fields cannot be read with `.field`
    /// (only `match`, which the linker confines to the home module) and it cannot be
    /// `update`d — otherwise an alias would leak the underlying authority past the
    /// carried policy.
    #[test]
    fn sealed_capability_fields_are_opaque() {
        let leak = "capability Vault:\n    net: Net[Connect, Tcp]\n    label: String\npub fn open(net: Net[Connect, Tcp]) -> Vault:\n    Vault(net, \"x\")\nfn main(console: Console, net: Net):\n    let v = open(net)\n    let raw = v.net\n    console.print(\"leaked\")\n";
        let module = parser::parse_module(leak).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        let err = typeck::check(&linked).expect_err("`.field` on a sealed cap must be rejected");
        assert!(err.message.contains("sealed capability"), "got: {}", err.message);
    }

    /// TLS works end to end through the `tls:` address scheme (RFC-0009), HERMETICALLY:
    /// a local rustls server with a self-signed `localhost` cert (trusted via the
    /// concurrency-safe test-root registry), and a witchy program that `connect`s to
    /// `tls:localhost:PORT`, sends a line, and reads the echo — identical on BOTH
    /// backends. Proves rustls+aws-lc terminates TLS host-side (the guest sees
    /// plaintext) with real certificate validation, no network access.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn tls_scheme_connects_through_a_local_server_backends_agree() {
        use std::io::{Read, Write};
        let (server_config, cert_pem) = tls_server_fixture();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let cert_path = std::env::temp_dir().join(format!("witchy-tls-test-{port}.pem"));
        std::fs::write(&cert_path, cert_pem.as_bytes()).unwrap();
        let _tls_root = witchy_runtime::net::register_test_tls_root(cert_path.clone());

        // Echo server: two connections (one per backend run), each echoing one line.
        let sc = server_config.clone();
        let server = std::thread::spawn(move || {
            for _ in 0..2 {
                let (tcp, _) = listener.accept().unwrap();
                let conn = rustls::ServerConnection::new(sc.clone()).unwrap();
                let mut tls = rustls::StreamOwned::new(conn, tcp);
                let mut line = Vec::new();
                let mut b = [0u8; 1];
                while tls.read_exact(&mut b).is_ok() {
                    if b[0] == b'\n' {
                        break;
                    }
                    line.push(b[0]);
                }
                let _ = tls.write_all(&line).and_then(|_| tls.write_all(b"\n")).and_then(|_| tls.flush());
            }
        });

        let src = format!(
            "fn main(console: Console, net: Net):\n    match net.try_connect(\"tls:localhost:{port}\"):\n        None -> console.print(\"connect failed\")\n        Some(sock) ->\n            sock.send_line(\"ping\")\n            console.print(sock.recv_line())\n            sock.close()\n"
        );
        let allow = format!("localhost:{port}");
        assert_eq!(link_run_net(&src, &[allow.as_str()]), vec!["ping".to_string()], "interp TLS echo");
        assert_eq!(
            run_linked_on_wasm_net(&[("main", src.as_str())], "main", &[allow.as_str()]),
            vec!["ping".to_string()],
            "wasm TLS echo"
        );
        server.join().unwrap();
        let _ = std::fs::remove_file(&cert_path);
    }

    /// RFC-0020 Layers 2–3: `net.resolve` + `net.connect_pinned`, end to end on BOTH backends.
    /// The program resolves `127.0.0.1` to its IP literals, pins the first, and dials THAT exact
    /// IP with the hostname carried only for SNI/`Host` — the resolve-once-and-pin shape that
    /// closes the DNS-rebinding TOCTOU (the checked IP IS the dialed IP; there is no second
    /// resolution). A loopback echo server proves the pinned socket really connects; both
    /// backends print the resolved IP and the echoed line identically.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn net_resolve_and_connect_pinned_backends_agree() {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        // Echo server: two connections (one per backend run), each echoing one line.
        let server = std::thread::spawn(move || {
            for _ in 0..2 {
                let (tcp, _) = listener.accept().unwrap();
                let mut r = BufReader::new(tcp);
                let mut line = String::new();
                let _ = r.read_line(&mut line);
                let _ = r.get_mut().write_all(line.as_bytes());
            }
        });
        let src = format!(
            "fn main(console: Console, net: Net):\n\
             \x20   let ips = net.resolve(\"127.0.0.1\")\n\
             \x20   let ip = ips[0]\n\
             \x20   console.print(ip)\n\
             \x20   let s = net.connect_pinned(ip, \"127.0.0.1\", {port}, false)\n\
             \x20   s.send_line(\"ping\")\n\
             \x20   console.print(s.recv_line())\n\
             \x20   s.close()\n"
        );
        let allow = format!("127.0.0.1:{port}");
        let expected = vec!["127.0.0.1".to_string(), "ping".to_string()];
        assert_eq!(link_run_net(&src, &[allow.as_str()]), expected, "interp resolve+pin");
        assert_eq!(
            run_linked_on_wasm_net(&[("main", src.as_str())], "main", &[allow.as_str()]),
            expected,
            "wasm resolve+pin",
        );
        server.join().unwrap();
    }

    /// RFC-0020: `connect_pinned` re-checks the Net allowlist on the PINNED IP — a hostile or
    /// buggy chooser that returns an internal address the capability forbids is still refused
    /// (the hard floor under the sealed policy). Granted only a public endpoint, a pin to
    /// loopback traps on both backends.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn connect_pinned_rechecks_the_allowlist_backends_agree() {
        use crate::runtime::{Capabilities, Runtime};
        let src = "fn main(console: Console, net: Net):\n\
	                   \x20   let s = net.connect_pinned(\"127.0.0.1\", \"example.com\", 80, false)\n\
                   \x20   s.send_line(\"x\")\n";
        let linked = resolve_std_src(src);
        typeck::check(&linked).expect("typecheck");
        // Granted only 93.184.216.34:80 (a public IP) — the loopback pin is outside it.
        assert!(
            interpreter::run_module(linked.clone(), ".", vec!["93.184.216.34:80".into()]).is_err(),
            "interp must refuse a pinned dial to an IP outside the allowlist",
        );
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers this program");
        let mut rt = Runtime::new().expect("runtime");
        let mut actor = rt
            .spawn(
                &bytes,
                Capabilities {
                    print: true,
                    quiet: true,
                    net_allow: Some(vec!["93.184.216.34:80".to_string()]),
                    net_connect: true,
                    net_listen: true,
                    ..Default::default()
                },
                64,
            )
            .expect("spawn");
        assert!(
            actor.run().is_err(),
            "compiled must refuse a pinned dial to an IP outside the allowlist",
        );
    }

    /// `url.encode` percent-encodes query values (RFC 3986): the unreserved set passes,
    /// reserved/space bytes become `%XX`. Both backends agree.
    #[test]
    fn url_encode_percent_encodes_query_values() {
        let src = "import url\nfn main(console: Console):\n    console.print(url.encode(\"a b/c:?=&-_.~Z9\"))\n";
        let expected = vec!["a%20b%2Fc%3A%3F%3D%26-_.~Z9".to_string()];
        assert_eq!(link_run(src), expected, "interp");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "wasm");
    }

    /// `oauth.exchange_code` POSTs to a token endpoint over HTTPS and reads the
    /// `access_token` — exercised HERMETICALLY against a local rustls server that
    /// returns the GitHub/Google JSON token shape, identical on BOTH backends. This is
    /// the network step of "Log in with GitHub" (code → access token).
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn oauth_exchange_code_against_a_local_token_server_backends_agree() {
        use std::io::{Read, Write};
        let (server_config, cert_pem) = tls_server_fixture();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let cert_path = std::env::temp_dir().join(format!("witchy-oauth-test-{port}.pem"));
        std::fs::write(&cert_path, cert_pem.as_bytes()).unwrap();
        let _tls_root = witchy_runtime::net::register_test_tls_root(cert_path.clone());

        let sc = server_config.clone();
        let server = std::thread::spawn(move || {
            let body = b"{\"access_token\":\"gho_test_token\",\"token_type\":\"bearer\"}";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                std::str::from_utf8(body).unwrap()
            );
            for _ in 0..2 {
                let (tcp, _) = listener.accept().unwrap();
                let conn = rustls::ServerConnection::new(sc.clone()).unwrap();
                let mut tls = rustls::StreamOwned::new(conn, tcp);
                let mut req = Vec::new();
                let mut b = [0u8; 1];
                while tls.read_exact(&mut b).is_ok() {
                    req.push(b[0]);
                    if req.ends_with(b"\r\n\r\n") {
                        break;
                    }
                }
                let _ = tls.write_all(response.as_bytes());
                let _ = tls.flush();
                tls.conn.send_close_notify();
                let _ = tls.flush();
            }
        });

        let src = format!(
            "import http\nimport oauth\nfn main(console: Console, net: Net):\n    let target = \"https://localhost:{port}/token\"\n    match oauth.exchange_code(net.fetch(http.origin(target)), target, \"cid\", \"sekret\", \"thecode\", \"http://app/cb\"):\n        Ok(tok) -> console.print(tok)\n        Err(e) -> console.print(\"error: \" + oauth.oauth_error_message(e))\n"
        );
        let allow = format!("localhost:{port}");
        assert_eq!(link_run_net(&src, &[allow.as_str()]), vec!["gho_test_token".to_string()], "interp exchange");
        assert_eq!(
            run_linked_on_wasm_net(&[("main", src.as_str())], "main", &[allow.as_str()]),
            vec!["gho_test_token".to_string()],
            "wasm exchange"
        );
        server.join().unwrap();
        let _ = std::fs::remove_file(&cert_path);
    }

    /// `oauth.bearer_get_json` GETs an API with a `Bearer` token and parses the JSON —
    /// the "fetch the signed-in user" step. HERMETIC: a local rustls server checks the
    /// `Authorization` header and returns a GitHub-`/user`-shaped body; the witchy
    /// program reads `login`. Identical on BOTH backends.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn oauth_bearer_get_json_against_a_local_api_backends_agree() {
        use std::io::{Read, Write};
        let (server_config, cert_pem) = tls_server_fixture();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let cert_path = std::env::temp_dir().join(format!("witchy-bearer-test-{port}.pem"));
        std::fs::write(&cert_path, cert_pem.as_bytes()).unwrap();
        let _tls_root = witchy_runtime::net::register_test_tls_root(cert_path.clone());

        let sc = server_config.clone();
        let server = std::thread::spawn(move || {
            for _ in 0..2 {
                let (tcp, _) = listener.accept().unwrap();
                let conn = rustls::ServerConnection::new(sc.clone()).unwrap();
                let mut tls = rustls::StreamOwned::new(conn, tcp);
                let mut req = Vec::new();
                let mut b = [0u8; 1];
                while tls.read_exact(&mut b).is_ok() {
                    req.push(b[0]);
                    if req.ends_with(b"\r\n\r\n") {
                        break;
                    }
                }
                // Honour the bearer token: 401 without it, the user JSON with it.
                let authed = String::from_utf8_lossy(&req).to_lowercase().contains("authorization: bearer gho_test_token");
                let body: &[u8] = if authed {
                    b"{\"login\":\"octocat\",\"id\":583231}"
                } else {
                    b"{\"message\":\"Requires authentication\"}"
                };
                let code = if authed { "200 OK" } else { "401 Unauthorized" };
                let response = format!(
                    "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    code,
                    body.len(),
                    std::str::from_utf8(body).unwrap()
                );
                let _ = tls.write_all(response.as_bytes());
                let _ = tls.flush();
                tls.conn.send_close_notify();
                let _ = tls.flush();
            }
        });

        let src = format!(
            "import http\nimport oauth\nimport json\nfn main(console: Console, net: Net):\n    let target = \"https://localhost:{port}/user\"\n    match oauth.bearer_get_json(net.fetch(http.origin(target)), target, \"gho_test_token\"):\n        Ok(doc) -> console.print(json.get_string(doc, \"login\").unwrap_or(\"?\"))\n        Err(e) -> console.print(\"error: \" + oauth.oauth_error_message(e))\n"
        );
        let allow = format!("localhost:{port}");
        assert_eq!(link_run_net(&src, &[allow.as_str()]), vec!["octocat".to_string()], "interp bearer get");
        assert_eq!(
            run_linked_on_wasm_net(&[("main", src.as_str())], "main", &[allow.as_str()]),
            vec!["octocat".to_string()],
            "wasm bearer get"
        );
        server.join().unwrap();
        let _ = std::fs::remove_file(&cert_path);
    }

    /// (BUG-006 / RFC-0044 rule 2) Every decoder rejects malformed input with a
    /// reachable `Err` that names the input — not a silent truncation — and the
    /// message is byte-identical on both backends. The named repro
    /// `base64url_decode("QUJD#WFO")` used to yield "ABC" (dropping everything from
    /// the `#`); a lone trailing base64 symbol ("QUJDW", "abc") is a truncated
    /// group; and a valid segment ("QUJD") still decodes to `Ok`.
    #[test]
    fn encoding_decoders_reject_malformed_backends_agree() {
        let src = r#"import encoding
fn main(console: Console):
    console.print(r(encoding.base64url_decode("QUJD#WFO")))
    console.print(r(encoding.base64_decode("aGVsbG8#")))
    console.print(r(encoding.hex_decode("zz")))
    console.print(r(encoding.hex_decode("abc")))
    console.print(r(encoding.base64url_to_hex("QUJD#")))
    console.print(r(encoding.base64url_decode("QUJDW")))
    console.print(r(encoding.base64url_decode("QUJD")))

fn r(x: Result(String, encoding.EncodingError)) -> String:
    match x:
        Ok(s) -> "ok:" + s
        Err(e) -> "err:" + encoding.encoding_error_message(e)
"#;
        let expected = vec![
            "err:`QUJD#WFO` is not valid base64url (expected the URL-safe `A-Za-z0-9-_` alphabet)".to_string(),
            "err:`aGVsbG8#` is not valid base64 (expected the `A-Za-z0-9+/` alphabet)".to_string(),
            "err:`zz` is not valid hex (expected an even count of `0-9a-fA-F` digits)".to_string(),
            "err:`abc` is not valid hex (expected an even count of `0-9a-fA-F` digits)".to_string(),
            "err:`QUJD#` is not valid base64url (expected the URL-safe `A-Za-z0-9-_` alphabet)".to_string(),
            "err:`QUJDW` is not valid base64url (expected the URL-safe `A-Za-z0-9-_` alphabet)".to_string(),
            "ok:ABC".to_string(),
        ];
        assert_eq!(link_run(src), expected, "interp");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "wasm");
    }
