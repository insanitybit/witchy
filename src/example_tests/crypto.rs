use super::*;
use crate::{codegen, interpreter, parser, typeck};

    /// `crypto.sha256` — a native intrinsic of the `crypto` module, *not* a global
    /// builtin — matches the canonical SHA-256 vectors, requires `import crypto`,
    /// and computes the same digest on the interpreter and the compiled WASM
    /// backend (the host fills the guest-allocated result string).
    #[test]
    fn crypto_sha256_matches_known_vectors() {
        let out = link_run(
            "import crypto\nfn main(console: Console):\n    console.print(crypto.sha256(\"\"))\n    console.print(crypto.sha256(\"abc\"))\n",
        );
        assert_eq!(
            out,
            vec![
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
                "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            ]
        );
        // No global builtin: bare `sha256` (without `import crypto`) is unknown.
        assert!(typeck::check_str("fn main(c: Console):\n    c.print(sha256(\"x\"))\n").is_err());
        // The compiled WASM backend computes the same digest (the host fills the
        // 64-byte result the guest pre-allocated) — interpreter↔WASM parity.
        let module = parser::parse_module(
            "import crypto\nfn main(console: Console):\n    console.print(crypto.sha256(\"abc\"))\n",
        )
        .expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers this program");
        assert_eq!(
            crate::run_wasm_bytes(&bytes).expect("wasm run"),
            vec!["ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"]
        );
    }

    #[test]
    fn rfc0054_crypto_verify_uses_typed_error_and_converts_to_string() {
        let src = "import crypto\nimport show\n\nfn via_string() -> Result(Bool, String):\n    let ok = crypto.ed25519_verify(\"not-hex\", \"msg\", \"00\")?\n    Ok(ok)\n\nfn main(console: Console):\n    match crypto.ed25519_verify(\"not-hex\", \"msg\", \"00\"):\n        Ok(_) -> console.print(\"bad\")\n        Err(e) ->\n            match e:\n                crypto.MalformedPublicKey(name) -> console.print(\"key:\" + name)\n                crypto.MalformedMessage(name) -> console.print(\"msg:\" + name)\n                crypto.MalformedSignature(name) -> console.print(\"sig:\" + name)\n                crypto.VerifierUnavailable(name) -> console.print(\"down:\" + name)\n            console.print(crypto.verify_error_message(e))\n            console.print(show.render(e))\n    match via_string():\n        Ok(_) -> console.print(\"bad\")\n        Err(e) -> console.print(e)\n";
        let expected = [
            "key:Ed25519",
            "Ed25519 public key is malformed",
            "Ed25519 public key is malformed",
            "Ed25519 public key is malformed",
        ];
        assert_eq!(link_run(src), expected, "interp: typed crypto.VerifyError");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: typed crypto.VerifyError",
        );
    }

    /// Signing round-trips entirely in witchy: a host-granted `Secret`
    /// capability signs a message (`crypto.sign`), and `crypto.ed25519_verify`
    /// against the key's public half (`crypto.public_key`) accepts it. Without a
    /// granted key, a `Secret` parameter is refused, and the capability
    /// surfaces in the footprint.
    #[test]
    fn crypto_signing_round_trips_in_witchy() {
        let src = "import crypto\nfn main(console: Console, signer: Secret):\n    let msg = \"sign me\"\n    let sig = crypto.sign(signer, msg)\n    match crypto.ed25519_verify(crypto.public_key(signer), msg, sig):\n        Ok(true) -> console.print(\"verified\")\n        Ok(false) -> console.print(\"FAILED\")\n        Err(_e) -> console.print(\"FAILED\")\n";
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let out = interpreter::run_module_signed(linked, ".", Vec::new(), Vec::new(), Some([7u8; 32]))
            .expect("run");
        assert_eq!(out, vec!["verified"]);

        // A `Secret` parameter without a host-granted key is refused.
        let m2 = parser::parse_module("fn main(console: Console, s: Secret):\n    console.print(\"x\")\n").expect("parse");
        let l2 = crate::pipeline::link(vec![("main".into(), m2)], "main").expect("link");
        assert!(interpreter::run_module_signed(l2, ".", Vec::new(), Vec::new(), None).is_err());

        // The signing authority surfaces in the capability footprint.
        let fp = crate::capabilities::analyze(
            &parser::parse_module("fn main(console: Console, s: Secret):\n    console.print(\"x\")\n").expect("parse"),
        );
        assert!(fp.total.contains_key("Secret"), "Secret should appear in the footprint");
    }

    /// The Secret capability is enforced in the WASM sandbox: with the same
    /// seed granted, sign/public_key/verify produce byte-identical results on
    /// both backends (Ed25519 is deterministic), and a module importing the
    /// signing ops cannot instantiate without the grant — the seed never enters
    /// guest memory.
    #[test]
    fn signing_key_compiles_to_wasm_and_is_gated() {
        use crate::runtime::{Capabilities, Runtime};
        let src = "import crypto\nfn main(console: Console, signer: Secret):\n    let msg = \"sign me\"\n    let sig = crypto.sign(signer, msg)\n    console.print(crypto.public_key(signer))\n    console.print(sig)\n    match crypto.ed25519_verify(crypto.public_key(signer), msg, sig):\n        Ok(true) -> console.print(\"verified\")\n        Ok(false) -> console.print(\"FAILED\")\n        Err(_e) -> console.print(\"FAILED\")\n";
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let seed = [7u8; 32];
        let interp_out =
            interpreter::run_module_signed(linked.clone(), ".", Vec::new(), Vec::new(), Some(seed))
                .expect("interp");
        assert_eq!(interp_out[2], "verified");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers this program");
        let mut rt = Runtime::batch().expect("runtime");
        let mut actor = rt
            .spawn(
                &bytes,
                Capabilities {
                    print: true,
                    quiet: true,
                    signing_key: Some(seed),
                    // (RFC-0005) the signing key is a real granted "signing" secret,
                    // not a magic handle-0 fallback — populate it as production does.
                    secrets: vec![crate::runtime::SecretGrant::new("signing", seed.to_vec())],
                    ..Default::default()
                },
                64,
            )
            .expect("spawn");
        actor.run().expect("run");
        assert_eq!(actor.output(), interp_out, "signature + pubkey must be byte-identical");

        // Ungranted: the imports are absent, so instantiation fails.
        let mut rt = Runtime::batch().expect("runtime");
        let denied = rt.spawn(
            &bytes,
            Capabilities { print: true, quiet: true, ..Default::default() },
            64,
        );
        assert!(denied.is_err(), "signing imports must not instantiate without the grant");
    }

    /// `SecretStore.get`/`.require` and `crypto.sign`/`public_key` must behave
    /// identically on both backends. The `signing` secret (granted by the seed) is
    /// fetched via `require`, signed over, and its public key derived; an absent
    /// secret yields `None`. The interpreter (oracle) and the compiled WASM must
    /// produce byte-identical output — the same parity discipline as raw
    /// `crypto.sign`. (Revealing the signing key is SEC-004-gated and covered by
    /// `signing_key_is_not_revealable_on_both_backends`.)
    #[test]
    fn secretstore_and_reveal_agree_on_both_backends() {
        use crate::runtime::{Capabilities, Runtime};
        let src = "import secretstore\nimport crypto\nfn main(console: Console, secrets: SecretStore):\n    let key = secrets.require(\"signing\")\n    console.print(crypto.public_key(key))\n    console.print(crypto.sign(key, \"msg\"))\n    match secrets.get(\"signing\"):\n        Some(k) -> console.print(\"got signing\")\n        None -> console.print(\"no signing\")\n    match secrets.get(\"absent\"):\n        Some(k) -> console.print(\"unexpected\")\n        None -> console.print(\"absent none\")\n";
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let seed = [7u8; 32];
        let interp_out =
            interpreter::run_module_signed(linked.clone(), ".", Vec::new(), Vec::new(), Some(seed))
                .expect("interp");
        assert_eq!(interp_out[2], "got signing");
        assert_eq!(interp_out[3], "absent none");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers this program");
        let mut rt = Runtime::batch().expect("runtime");
        let mut actor = rt
            .spawn(
                &bytes,
                Capabilities {
                    print: true,
                    quiet: true,
                    signing_key: Some(seed),
                    // (RFC-0005) the signing key is a real granted "signing" secret,
                    // not a magic handle-0 fallback — populate it as production does.
                    secrets: vec![crate::runtime::SecretGrant::new("signing", seed.to_vec())],
                    ..Default::default()
                },
                64,
            )
            .expect("spawn");
        actor.run().expect("run");
        assert_eq!(
            actor.output(),
            interp_out,
            "SecretStore.get/require + crypto.sign/public_key must be byte-identical on both backends"
        );
    }

    /// SEC-004: the signing key — the bare `Secret`, or `require("signing")` — is
    /// SIGN-ONLY. `crypto.reveal` on it must error (so handing code a key to sign
    /// with cannot also exfiltrate it), and it must error IDENTICALLY on both
    /// backends (the gate is one shared identity rule). Named value-secrets stay
    /// revealable — covered end-to-end by the `sandbox` CLI e2e test.
    #[test]
    fn signing_key_is_not_revealable_on_both_backends() {
        use crate::runtime::{Capabilities, Runtime};
        let src = "import secretstore\nimport crypto\nfn main(console: Console, secrets: SecretStore):\n    console.print(crypto.reveal(secrets.require(\"signing\")))\n";
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let seed = [7u8; 32];

        // Interpreter (oracle): refuses.
        let interp =
            interpreter::run_module_signed(linked.clone(), ".", Vec::new(), Vec::new(), Some(seed));
        let msg = interp.expect_err("interp must refuse to reveal the signing key").message;
        assert!(msg.contains("not revealable"), "unexpected interp error: {msg}");

        // Compiled WASM (the security boundary): also refuses.
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers this program");
        let mut rt = Runtime::batch().expect("runtime");
        let mut actor = rt
            .spawn(
                &bytes,
                Capabilities {
                    print: true,
                    quiet: true,
                    signing_key: Some(seed),
                    // (RFC-0005) the signing key is a real granted "signing" secret,
                    // not a magic handle-0 fallback — populate it as production does.
                    secrets: vec![crate::runtime::SecretGrant::new("signing", seed.to_vec())],
                    ..Default::default()
                },
                64,
            )
            .expect("spawn");
        assert!(
            actor.run().is_err(),
            "compiled backend must refuse to reveal the signing key"
        );
    }

    /// RFC-0005 step 1: the `signing@0` fallback is GONE. A module that requests
    /// `secrets.require("signing")` when the secret table does NOT carry a `"signing"`
    /// entry now fails — even though `signing_key` is set — instead of resolving the
    /// magic handle `0` to the key. Before, `signing_key.is_some()` alone made
    /// `require("signing")`/handle-0 return the key; now it must be a real granted
    /// entry. Both backends refuse identically.
    #[test]
    fn signing_at_zero_fallback_is_removed_on_both_backends() {
        use crate::runtime::{Capabilities, Runtime};
        let src = "import secretstore\nimport crypto\nfn main(console: Console, secrets: SecretStore):\n    console.print(crypto.public_key(secrets.require(\"signing\")))\n";
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let seed = [7u8; 32];

        // Interpreter (oracle): the store carries no "signing", so require fails —
        // there is no signing_key-backed fallback in the store either.
        let interp = interpreter::run_module_signed(linked.clone(), ".", Vec::new(), Vec::new(), None);
        assert!(interp.is_err(), "interp must refuse require(\"signing\") with no granted entry");

        // Compiled WASM: signing_key IS set, but secrets is EMPTY — the removed
        // fallback would have leaked the key at handle 0; now the lookup returns -1
        // and `require` traps.
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers this program");
        let mut rt = Runtime::batch().expect("runtime");
        let mut actor = rt
            .spawn(
                &bytes,
                Capabilities {
                    print: true,
                    quiet: true,
                    signing_key: Some(seed),
                    // Deliberately do NOT register a "signing" secret: the whole point
                    // is that signing_key alone no longer resolves handle 0.
                    ..Default::default()
                },
                64,
            )
            .expect("spawn");
        assert!(
            actor.run().is_err(),
            "compiled backend must not resolve the signing key via the removed handle-0 fallback"
        );
    }

    /// (BUG-394) `SecretStore.require(name)` for an ungranted secret must fail
    /// EAGERLY on BOTH backends — at the require site — even when the returned
    /// `Secret` is NEVER used. The compiled backend previously lowered `require` to
    /// a bare `secretstore_lookup` returning -1, so an unused missing secret ran to
    /// the end without error (lazy), diverging from the interpreter. A GRANTED
    /// secret still resolves on both backends.
    #[test]
    fn require_missing_secret_errors_eagerly_on_both_backends() {
        use crate::runtime::{Capabilities, Runtime};
        // The required secret is fetched but NEVER used — the compiled backend must
        // still trap at the require, not run to "reached".
        let src = "import secretstore\nfn main(console: Console, secrets: SecretStore):\n    let s = secrets.require(\"missing\")\n    console.print(\"reached\")\n";
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");

        // Interpreter (oracle): errors at the require site.
        let interp = interpreter::run_module(linked.clone(), ".", Vec::new());
        let msg = interp.expect_err("interp must reject an ungranted required secret").message;
        assert!(msg.contains("required secret `missing` was not granted"), "interp msg: {msg}");

        // Compiled WASM: no "missing" secret granted → must trap eagerly (not print).
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers this program");
        let mut rt = Runtime::batch().expect("runtime");
        let mut actor = rt
            .spawn(&bytes, Capabilities { print: true, quiet: true, ..Default::default() }, 64)
            .expect("spawn");
        assert!(
            actor.run().is_err(),
            "compiled backend must trap eagerly on an ungranted required secret, not run to the end"
        );

        // A GRANTED (but unused) secret resolves on both backends.
        let ok_module = parser::parse_module(src).expect("parse");
        let ok_linked = crate::pipeline::link(vec![("main".into(), ok_module)], "main").expect("link");
        let (interp_ok, _) = interpreter::run_module_exit_secrets(
            ok_linked.clone(),
            ".",
            Vec::new(),
            Vec::new(),
            None,
            vec![("missing".to_string(), b"hunter2".to_vec(), false)],
        )
        .expect("interp with grant");
        assert_eq!(interp_ok, vec!["reached"]);
        let ok_bytes = codegen::compile_module_binary(&ok_linked)
            .expect_lowered("lowers");
        let mut ok_actor = rt
            .spawn(
                &ok_bytes,
                Capabilities {
                    print: true,
                    quiet: true,
                    secrets: vec![crate::runtime::SecretGrant::new("missing", b"hunter2".to_vec())],
                    ..Default::default()
                },
                64,
            )
            .expect("spawn");
        ok_actor.run().expect("compiled with grant");
        assert_eq!(ok_actor.output(), vec!["reached"], "granted secret resolves on the compiled backend");
    }

    /// The crypto digest helpers ($crypto_sha256/sha512/sha3_256/hmac_sha256) on
    /// the binary path — host-import wrappers returning a String. The crypto
    /// imports are host-provided regardless of grant (hashing needs no
    /// capability), so the print-only harness instantiates them. Compared against
    /// the linked interpreter oracle (which computes the real digests).
    #[test]
    fn wir_crypto_digests_host_import_binary_path() {
        let src = "import crypto\nfn main(console: Console):\n    console.print(crypto.sha256(\"abc\"))\n    console.print(crypto.sha512(\"abc\"))\n    console.print(crypto.sha3_256(\"abc\"))\n    console.print(crypto.hmac_sha256(\"abcdef\", \"msg\"))\n";
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the WIR binary path should handle the crypto digests via host imports");
        let want = link_run(src);
        // SHA-256("abc") — a well-known vector — confirms the host actually wrote it.
        assert_eq!(want[0], "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(want[1].len(), 128, "sha512 hex is 128 chars");
        assert_eq!(want[2].len(), 64, "sha3_256 hex is 64 chars");
        assert_eq!(want[3].len(), 64, "hmac-sha256 hex is 64 chars");
        assert_eq!(run_bytes_print_only(&bytes), want, "binary path vs oracle");
    }

    /// `$crypto_rune_hash` on the binary path — a host-import wrapper taking two
    /// List(String) args (list literals lower fine) and returning a 71-char
    /// digest String. Ungated, so the print-only harness instantiates it.
    #[test]
    fn wir_crypto_rune_hash_host_import_binary_path() {
        let src = "import crypto\nfn main(console: Console):\n    console.print(crypto.rune_hash([\"a.witchy\", \"b.witchy\"], [\"fn one\", \"fn two\"]))\n";
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the WIR binary path should handle crypto.rune_hash via the host import");
        let want = link_run(src);
        assert_eq!(want.len(), 1);
        assert_eq!(want[0].len(), 71, "rune_hash digest is 71 chars");
        assert_eq!(run_bytes_print_only(&bytes), want, "binary path vs oracle");
    }

    /// `crypto.ecdsa_p256_verify` on the binary path — a P-256 ECDSA verify host
    /// status import (`$crypto_ecdsa_p256_verify_status`, three string headers →
    /// i64 status, no capability). A valid (pubkey, message, signature) triple
    /// verifies; a tampered message does not. Compared against the linked
    /// interpreter oracle.
    #[test]
    fn wir_crypto_ecdsa_verify_binary_path() {
        let pk = "048f81cd9fca785a42a6f5dd58972cc0f702e83b1c960b5912354471496597e227fec81ff1d52530b06d7091649e6beb49dba70968b4b727bb24e3ceb7dd01a039";
        let sig = "304402203260029f4c6beb2e78afdd906c057c63f8828e2b03820de7053d97254577fb8c02204478b9b75f8fd7a1ce4298f0d119e12926dafda116ae4c197b0048dc117bc9de";
        let src = format!("import crypto\nfn main(console: Console):\n    match crypto.ecdsa_p256_verify(\"{pk}\", \"webauthn-es256-test-message\", \"{sig}\"):\n        Ok(true) -> console.print(\"ok\")\n        Ok(false) -> console.print(\"bad\")\n        Err(_e) -> console.print(\"err\")\n    match crypto.ecdsa_p256_verify(\"{pk}\", \"tampered\", \"{sig}\"):\n        Ok(true) -> console.print(\"ok\")\n        Ok(false) -> console.print(\"bad\")\n        Err(_e) -> console.print(\"err\")\n");
        let module = parser::parse_module(&src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the WIR binary path should lower crypto.ecdsa_p256_verify");
        let want = vec!["ok".to_string(), "bad".to_string()];
        assert_eq!(run_bytes_print_only(&bytes), want, "binary path");
        assert_eq!(link_run(&src), want, "interpreter oracle");
    }

    /// `crypto.ed25519_verify` on the binary path — the signature verify the
    /// self-hosted package manager (coven/pm) uses, and the construct whose
    /// missing wir_helper made `pm.witchy` fall back to WAT. Sign a message with
    /// the granted Secret, then verify: the valid (pubkey, message, signature)
    /// triple verifies `Ok(true)`, a tampered message `Ok(false)`.
    #[test]
    fn wir_crypto_ed25519_verify_binary_path() {
        use crate::runtime::{Capabilities, Runtime};
        let src = "import crypto\nfn main(console: Console, signer: Secret):\n    let pk = crypto.public_key(signer)\n    let sig = crypto.sign(signer, \"hello\")\n    console.print(\"${crypto.ed25519_verify(pk, \"hello\", sig)}\")\n    console.print(\"${crypto.ed25519_verify(pk, \"tampered\", sig)}\")\n";
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the WIR binary path should lower crypto.ed25519_verify");
        let mut rt = Runtime::batch().expect("runtime");
        let mut actor = rt
            .spawn(
                &bytes,
                Capabilities {
                    print: true,
                    signing_key: Some([7u8; 32]),
                    secrets: vec![crate::runtime::SecretGrant::new("signing", vec![7u8; 32])],
                    quiet: true,
                    ..Default::default()
                },
                crate::RUN_MEMORY_PAGES,
            )
            .expect("spawn with signing key");
        actor.run().expect("run");
        assert_eq!(actor.output(), vec!["Ok(true)".to_string(), "Ok(false)".to_string()]);
    }
