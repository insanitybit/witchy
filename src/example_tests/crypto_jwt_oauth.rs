use super::*;
use crate::{codegen, parser, typeck};

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

    /// End-to-end `std/jwt`: a REAL aws-lc-signed compact RS256 JWT, embedded as a
    /// runtime argument, verifies identically on both backends — `Ok` yields the
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
        // `now` = 1000, audience "coven". Run all token cases in one process per
        // backend, paying program setup and compilation once.
        let src = "import jwt\nimport json\nimport list\nfn subject_or_error(token: String, key: String) -> String:\n    match jwt.verify_rs256(token, key, \"coven\", 1000):\n        Ok(claims) -> json.get_string(claims, \"sub\").unwrap_or(\"?\")\n        Err(e) -> jwt.jwt_error_message(e)\n\nfn main(console: Console, argv: List(String)):\n    console.print(subject_or_error(list.at(argv, 0), list.at(argv, 4)))\n    console.print(subject_or_error(list.at(argv, 1), list.at(argv, 4)))\n    console.print(subject_or_error(list.at(argv, 2), list.at(argv, 4)))\n    console.print(subject_or_error(list.at(argv, 3), list.at(argv, 4)))\n";
        let args = vec![good, expired, wrong_aud, tampered, pk_hex];
        let expected = [
            "octocat",
            "JWT has expired",
            "JWT audience mismatch (wrong relying party / replay)",
            "JWT signature is invalid (untrusted or forged)",
        ];
        assert_eq!(run_on_wasm_args(src, args), expected, "compiled Wasm results");
    }

    /// `jwt.verify_oidc` is the full relying-party check: a real RS256 GitHub-Actions-
    /// shaped OIDC token verifies only against its TRUE issuer (the bind to a trusted
    /// provider), and rejects a not-yet-active (`nbf`) token. On success the caller reads
    /// identity claims — here the `repository` a trusted-publishing flow would authorize.
    /// This is the verification half of OIDC login / publishing.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn jwt_verify_oidc_binds_issuer() {
        if !is_nightly() {
            return;
        }
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
        let src = "import jwt\nimport json\nimport list\nfn main(console: Console, argv: List(String)):\n    let token = list.at(argv, 0)\n    let key = list.at(argv, 1)\n    let issuer = list.at(argv, 2)\n    match jwt.verify_oidc(token, key, issuer, \"coven\", 1000):\n        Ok(claims) -> console.print(json.get_string(claims, \"repository\").unwrap_or(\"?\"))\n        Err(e) -> console.print(jwt.jwt_error_message(e))\n";
        let run = |tok: &str, issuer: &str| -> Vec<String> {
            let args = vec![tok.to_owned(), pk_hex.clone(), issuer.to_owned()];
            run_on_wasm_args(src, args)
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
        let fresh_src = r#"import jwt
import json
import list
fn main(console: Console, argv: List(String)):
    let token = list.at(argv, 0)
    let key = list.at(argv, 1)
    let issuer = list.at(argv, 2)
    match jwt.verify_oidc_fresh(token, key, issuer, "coven", 1000, 600, 60):
        Ok(claims) -> console.print("ok:" + json.get_string(claims, "repository").unwrap_or("?"))
        Err(e) ->
            match e:
                jwt.TokenLifetimeTooLong(actual, maximum) -> console.print("ttl:${actual}:${maximum}")
                jwt.IssuedAtInFuture(iat, skew) -> console.print("future:${iat}:${skew}")
                jwt.MissingClaim(name) -> console.print("missing:" + name)
                _ -> console.print(jwt.jwt_error_message(e))
"#;
        let run_fresh = |tok: &str| -> Vec<String> {
            let args = vec![tok.to_owned(), pk_hex.clone(), gh.to_owned()];
            run_on_wasm_args(fresh_src, args)
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
        // Audience = "myclient", now = 1000. Run all claims through one actor per
        // backend so this boundary test pays setup and Wasm compilation once.
        let src = "import jwt\nimport json\nimport list\nfn subject_or_error(token: String, key: String, issuer: String) -> String:\n    match jwt.verify_oidc(token, key, issuer, \"myclient\", 1000):\n        Ok(claims) -> json.get_string(claims, \"sub\").unwrap_or(\"?\")\n        Err(e) -> jwt.jwt_error_message(e)\n\nfn main(console: Console, argv: List(String)):\n    console.print(subject_or_error(list.at(argv, 0), list.at(argv, 4), list.at(argv, 5)))\n    console.print(subject_or_error(list.at(argv, 1), list.at(argv, 4), list.at(argv, 5)))\n    console.print(subject_or_error(list.at(argv, 2), list.at(argv, 4), list.at(argv, 5)))\n    console.print(subject_or_error(list.at(argv, 3), list.at(argv, 4), list.at(argv, 5)))\n";
        let args = vec![
            single,
            multi_ok,
            multi_wrong,
            multi_missing,
            pk_hex,
            iss.to_owned(),
        ];
        let expected = [
            "u1",
            "u2",
            "JWT `azp` mismatch: a multi-audience OIDC token must name this client as the authorized party",
            "JWT payload is missing `azp`",
        ];
        assert_eq!(run_on_wasm_args(src, args), expected, "compiled Wasm results");
    }

    /// `jwt.rsa_key_for_kid` distinguishes malformed issuer metadata from a
    /// non-matching key (BUG-408): a `keys` field that is present but NOT an array
    /// is a distinct "not an array" error, not a defaulted-empty "no matching key".
    /// A valid array with no matching kid still reports the no-match error. No key
    /// material needed — this exercises the JWKS-shape boundary. Both backends.
    #[test]
    fn jwt_rsa_key_for_kid_rejects_malformed_keys() {
        if !is_nightly() {
            return;
        }
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

    /// SEC-050: possessing authority for one endpoint must not turn `Net.resolve`
    /// into an ambient DNS-query channel for unrelated names. Admitted Wasm
    /// answers `Denied` before consulting the resolver.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn net_resolve_requires_queried_host_authority_on_compiled_wasm() {
        use crate::runtime::{Capabilities, Runtime};
        let src = "fn main(console: Console, net: Net):\n\
                   \x20   match net.resolve(\"unrelated.invalid\"):\n\
                   \x20       Ok(_) -> console.print(\"resolved\")\n\
                   \x20       Err(e) -> console.print(e.message())\n";
        let checked = witchy::resolve_std_only_checked(src).expect("checked resolve source");
        let artifact = codegen::compile_checked_module_artifact(&checked)
            .expect_lowered("the canonical path lowers net.resolve");
        let mut runtime = Runtime::batch().expect("runtime");
        let mut actor = runtime
            .spawn_admitted(
                &artifact,
                Capabilities {
                    print: true,
                    quiet: true,
                    net_allow: Some(vec!["127.0.0.1:9".to_string()]),
                    net_connect: true,
                    ..Default::default()
                },
                64,
            )
            .expect("spawn");
        actor.run().expect("a denied resolve is a value, not a trap");
        assert_eq!(actor.output(), vec!["`unrelated.invalid` is not permitted by this Net capability".to_string()]);
    }

    /// RFC-0020: `connect_pinned` re-checks the Net allowlist on the PINNED IP — a hostile or
    /// buggy chooser that returns an internal address the capability forbids is still refused
    /// (the hard floor under the sealed policy). Granted only a public endpoint, a pin to
    /// loopback traps on compiled Wasm.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn connect_pinned_rechecks_the_allowlist_on_compiled_wasm() {
        use crate::runtime::{Capabilities, Runtime};
        let src = "fn main(console: Console, net: Net):\n\
	                   \x20   let s = net.connect_pinned(\"127.0.0.1\", \"example.com\", 80, false)\n\
                   \x20   s.close()\n";
        let linked = resolve_std_src(src);
        typeck::check(&linked).expect("typecheck");
        // Granted only 93.184.216.34:80 (a public IP) — the loopback pin is outside it.
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

    #[cfg(not(target_arch = "wasm32"))]
    fn run_admitted_oauth_net(source: &str, address: &str) -> Vec<String> {
        let checked = witchy::resolve_std_only_checked(source).expect("checked OAuth source");
        let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
            .expect_lowered("canonical OAuth client admits");
        let mut runtime = witchy::runtime::Runtime::batch().expect("runtime");
        let mut actor = runtime.spawn_admitted(&artifact, witchy::runtime::Capabilities {
            print: true, quiet: true, net_connect: true,
            net_allow: Some(vec![address.to_owned()]), ..Default::default()
        }, 64).expect("admitted OAuth client");
        actor.run().expect("compiled OAuth execution");
        actor.output().to_vec()
    }
