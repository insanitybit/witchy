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
        let src = "import crypto\nfn main(console: Console):\n    console.print(crypto.sha256(\"abc\"))\n";
        assert_eq!(
            crate::run_admitted_artifact(&admitted_artifact(src)).expect("wasm run"),
            vec!["ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"]
        );
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
        let bytes = admitted_artifact(src);
        let mut rt = Runtime::batch().expect("runtime");
        let mut actor = rt
            .spawn_admitted(
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

        // Ungranted: the launch refuses the Secret root before `main` runs.
        let mut rt = Runtime::batch().expect("runtime");
        let denied = rt.spawn_admitted(
            &bytes,
            Capabilities { print: true, quiet: true, ..Default::default() },
            64,
        ).and_then(|mut vm| vm.run().map(|()| vm.output()));
        assert!(denied.is_err(), "a signing program must be refused without the grant: {denied:?}");
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
        let bytes = admitted_artifact(src);
        let mut rt = Runtime::batch().expect("runtime");
        let mut actor = rt
            .spawn_admitted(
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

    /// SEC-004: the signing key (the bare `Secret`, or `require("signing")`) is
    /// SIGN-ONLY, and a secret granted `sealed` is usable only by handle.
    /// `crypto.reveal` answers both with `Err(Denied)`, a value the program
    /// can act on, never the key bytes; a revealable named secret is `Ok`
    /// (RFC-0126 rule 3, admitted Wasm).
    #[test]
    fn reveal_denies_the_signing_key_and_sealed_secrets_in_admitted_wasm() {
        struct ResetOptimization;
        impl Drop for ResetOptimization {
            fn drop(&mut self) { crate::opt::set_for_tests(None); }
        }
        crate::opt::set_for_tests(Some(crate::opt::OptSet::none()));
        let _reset = ResetOptimization;
        let src = "import secretstore\nimport crypto\n\nfn shown(r: Result(String, CapError)) -> String:\n    match r:\n        Ok(value) -> \"ok: \" + value\n        Err(Denied(m)) -> \"denied: \" + m\n        Err(e) -> \"other: \" + e.message()\n\nfn main(console: Console, secrets: SecretStore):\n    console.print(shown(crypto.reveal(secrets.require(\"signing\"))))\n    console.print(shown(crypto.reveal(secrets.require(\"tls\"))))\n    console.print(shown(crypto.reveal(secrets.require(\"token\"))))\n";
        let checked = witchy::resolve_std_only_checked(src).expect("check reveal program");
        let artifact = codegen::compile_checked_module_artifact(&checked)
            .expect_lowered("admit reveal program");
        let seed = [7u8; 32];
        let mut sealed = crate::runtime::SecretGrant::new("tls", b"tls-private-key".to_vec());
        sealed.sealed = true;
        let mut rt = crate::runtime::Runtime::batch().expect("runtime");
        let mut vm = rt.spawn_admitted(&artifact, crate::runtime::Capabilities {
            print: true,
            quiet: true,
            signing_key: Some(seed),
            secrets: vec![
                crate::runtime::SecretGrant::new("signing", seed.to_vec()),
                sealed,
                crate::runtime::SecretGrant::new("token", b"hunter2".to_vec()),
            ],
            ..Default::default()
        }, 64).expect("spawn admitted reveal program");
        vm.run().expect("a refused reveal is a value, not a trap");
        assert_eq!(vm.output(), vec![
            "denied: the signing key is not revealable; use crypto.sign / crypto.public_key".to_string(),
            format!("denied: {}", witchy_caps::capabilities::SEALED_SECRET_REVEAL_ERROR),
            "ok: hunter2".to_string(),
        ]);
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
        let bytes = admitted_artifact(src);
        let mut rt = Runtime::batch().expect("runtime");
        let mut actor = rt
            .spawn_admitted(&bytes, Capabilities { print: true, quiet: true, ..Default::default() }, 64)
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
        let ok_bytes = admitted_artifact(src);
        let mut ok_actor = rt
            .spawn_admitted(
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

    /// (RFC-0106) SHAKE128/256 XOF — FIPS 202 known-answer vectors, exact on both
    /// backends, over the length matrix (0, 1, a rate boundary, multiple rate
    /// blocks). The empty-message prefixes are the canonical NIST values; agreement
    /// with an independent reference (the AWS-LC adapter's own unit test asserts the
    /// same bytes) rules out a common-mode padding/FFI error, and interpreter/Wasm
    /// parity rules out a backend divergence.
    #[test]
    fn shake_matches_fips202_vectors_on_both_backends() {
        let src = concat!(
            "import crypto\n",
            "import encoding\n",
            "import bytes\n",
            "fn hex(input: Bytes, n: Int, use256: Bool) -> String:\n",
            "    let r = if use256: crypto.shake256(input, n) else: crypto.shake128(input, n)\n",
            "    match r:\n",
            "        Ok(d) -> encoding.hex_encode_bytes(d)\n",
            "        Err(_e) -> \"err\"\n",
            "fn main(console: Console):\n",
            "    let empty = bytes.from_string(\"\")\n",
            "    console.print(hex(empty, 16, false))\n",
            "    console.print(hex(empty, 16, true))\n",
            "    console.print(hex(empty, 0, false))\n",
            "    console.print(hex(empty, 1, true))\n",
            "    console.print(hex(bytes.from_string(\"abc\"), 168, false))\n",
        );
        let interp_out = link_run(src);
        assert_eq!(
            interp_out[0], "7f9c2ba4e88f827d616045507605853e",
            "SHAKE128(\"\")[0..16] FIPS-202 KAT",
        );
        assert_eq!(
            interp_out[1], "46b9dd2b0ba88d13233b3feb743eeb24",
            "SHAKE256(\"\")[0..16] FIPS-202 KAT",
        );
        assert_eq!(interp_out[2], "", "zero-length squeeze is empty");
        assert_eq!(interp_out[3].len(), 2, "one byte = two hex chars");
        assert_eq!(interp_out[4].len(), 168 * 2, "a full 168-byte SHAKE128 rate block");
        // Both backends agree, byte-for-byte, on every vector.
        assert_eq!(run_on_wasm(src), interp_out, "SHAKE interpreter/Wasm parity");
    }

    /// (RFC-0121) `Secret[Seal]` narrowing is a real type-level restriction, and the
    /// by-handle operations accept it. Signing and public-key derivation through a
    /// SEALED handle must produce byte-identical results to signing through the bare
    /// `Secret` on BOTH backends — the right is erased before either backend runs, so
    /// narrowing changes what the checker permits, never what the program computes.
    #[test]
    fn sealed_secret_signs_identically_on_both_backends() {
        use crate::runtime::{Capabilities, Runtime};
        let src = concat!(
            "import crypto\n",
            // The narrowed parameter proves this helper cannot reveal the key.
            "fn fingerprint(key: Secret[Seal]) -> String:\n",
            "    key.public_key()\n",
            "fn endorse(key: Secret[Seal], message: String) -> String:\n",
            "    key.sign(message)\n",
            "fn main(console: Console, signer: Secret):\n",
            // Implicit narrowing at the call boundary: bare `Secret` -> `Secret[Seal]`.
            "    console.print(fingerprint(signer))\n",
            "    console.print(endorse(signer, \"msg\"))\n",
            // Explicit `as` ascription narrows to the same thing.
            "    let sealed = signer as Secret[Seal]\n",
            "    console.print(fingerprint(sealed))\n",
            "    console.print(endorse(sealed, \"msg\"))\n",
        );
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("a sealed handle may sign and derive a public key");
        let seed = [7u8; 32];
        let interp_out =
            interpreter::run_module_signed(linked.clone(), ".", Vec::new(), Vec::new(), Some(seed))
                .expect("interp");
        // Narrowing is not observable in the result: the sealed and bare paths agree.
        assert_eq!(interp_out[0], interp_out[2], "public_key is the same through either handle");
        assert_eq!(interp_out[1], interp_out[3], "sign is the same through either handle");

        let bytes = admitted_artifact(src);
        let mut rt = Runtime::batch().expect("runtime");
        let mut actor = rt
            .spawn_admitted(
                &bytes,
                Capabilities {
                    print: true,
                    quiet: true,
                    signing_key: Some(seed),
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
            "a sealed Secret must sign byte-identically on both backends"
        );
    }

    /// (RFC-0121) The reveal refusal is a CHECK-time error, not just a runtime trap:
    /// `crypto.reveal` needs `Reveal`, and a `Secret[Seal]` does not have it. The
    /// diagnostic names the missing right, in the rights-diagnostic family shared with
    /// `Dir`/`Console`. Narrowing is also one-directional — `as` cannot re-widen a
    /// sealed handle back to a revealable one, so the host's grant floor holds.
    #[test]
    fn revealing_a_sealed_secret_is_a_type_error() {
        // Reveal through a narrowed parameter: rejected, naming the right.
        let sealed_reveal = concat!(
            "import crypto\n",
            "fn leak(key: Secret[Seal]) -> String:\n",
            "    key.reveal() ?? \"\"\n",
            "fn main(console: Console, signer: Secret):\n",
            "    console.print(leak(signer))\n",
        );
        let error = typeck::check_str(sealed_reveal)
            .expect_err("reveal on a sealed secret must not type-check");
        assert!(
            error.contains("needs `Reveal`")
                && error.contains("`Secret[Seal]`"),
            "the diagnostic must name the missing right: {}",
            error
        );

        // Re-widening is rejected: `as` only drops rights.
        let widen = concat!(
            "fn main(console: Console, key: Secret[Seal]):\n",
            "    let wider = key as Secret\n",
            "    console.print(\"unreachable\")\n",
        );
        let error =
            typeck::check_str(widen).expect_err("`as` must not widen a sealed secret");
        assert!(
            error.contains("can only drop rights"),
            "unexpected widening error: {}",
            error
        );

        // A bare `Secret` still reveals — the default is revealable, unchanged.
        let bare_reveal = concat!(
            "import crypto\n",
            "fn main(console: Console, key: Secret):\n",
            "    console.print(key.reveal() ?? \"\")\n",
        );
        typeck::check_str(bare_reveal).expect("a bare Secret may still be revealed");
    }

    /// (RFC-0121) A capability's rights vocabulary is closed: an unknown marker is a
    /// check-time error rather than a silently-dropped annotation, so `Secret[Sealed]`
    /// (a plausible typo for `Secret[Seal]`) cannot masquerade as full authority. This
    /// is the BUG-154 guarantee, now covering `Secret`.
    #[test]
    fn an_unknown_secret_right_is_rejected() {
        let error = typeck::check_str(
            "fn main(console: Console, key: Secret[Sealed]):\n    console.print(\"x\")\n",
        )
        .expect_err("`Sealed` is not a Secret right");
        assert!(
            error.contains("unknown `Secret` right `Sealed`")
                && error.contains("Reveal")
                && error.contains("Seal"),
            "the diagnostic must list the admitted rights: {}",
            error
        );
    }
