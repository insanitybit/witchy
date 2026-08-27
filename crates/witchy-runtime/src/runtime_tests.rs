    use super::*;
    use std::borrow::Cow;
    use wasm_encoder::{
        CustomSection, EntityType, ImportSection, Module as WasmModule, Section as _,
        TypeSection, ValType,
    };
    use witchy_wir::layout::{LayoutBundle, LayoutInterner};

    fn wasm_with_layout_payload(payload: Vec<u8>) -> Vec<u8> {
        let mut wasm = WasmModule::new().finish();
        CustomSection {
            name: Cow::Borrowed(LAYOUT_SECTION),
            data: Cow::Owned(payload),
        }
        .append_to(&mut wasm);
        wasm
    }

    fn append_optimizer_policy(wasm: &mut Vec<u8>, payload: &[u8]) {
        CustomSection {
            name: Cow::Borrowed(OPTIMIZER_POLICY_SECTION),
            data: Cow::Borrowed(payload),
        }
        .append_to(wasm);
    }

    #[test]
    fn optimizer_policy_is_versioned_and_fails_closed() {
        let mut valid = WasmModule::new().finish();
        append_optimizer_policy(
            &mut valid,
            &witchy_wir::optimizer_policy::PRESERVE_RAW_PAYLOAD,
        );
        assert_eq!(optimizer_policy(&valid).unwrap(), OptimizerPolicy::PreserveRaw);

        let mut unknown = WasmModule::new().finish();
        append_optimizer_policy(&mut unknown, &[99, 1]);
        assert!(optimizer_policy(&unknown).unwrap_err().to_string().contains("unsupported schema"));

        let mut duplicate = valid.clone();
        append_optimizer_policy(
            &mut duplicate,
            &witchy_wir::optimizer_policy::PRESERVE_RAW_PAYLOAD,
        );
        assert!(optimizer_policy(&duplicate).unwrap_err().to_string().contains("duplicate section"));
        assert_eq!(optimizer_policy(b"(module)").unwrap(), OptimizerPolicy::Default);
    }

    #[test]
    fn ordinary_batch_runtime_has_mutable_executable_cache() {
        let publisher = Runtime::batch().expect("publisher runtime");
        assert!(
            publisher.executable_cache,
            "publisher applications use the ordinary optimized-Wasm and Wasmtime cache policy"
        );
    }

    fn wasm_with_host_import(name: &str) -> Vec<u8> {
        let mut types = TypeSection::new();
        types.ty().function([ValType::I32, ValType::I32], []);
        let mut imports = ImportSection::new();
        imports.import("witchy", name, EntityType::Function(0));
        let mut module = WasmModule::new();
        module.section(&types);
        module.section(&imports);
        module.finish()
    }

    #[test]
    fn artifact_layout_metadata_is_checked_before_instantiation() {
        let empty = LayoutBundle::from_interner(&LayoutInterner::new(), [])
            .expect("empty canonical layout bundle");
        let valid = wasm_with_layout_payload(empty.canonical_bytes());
        let mut runtime = Runtime::batch().expect("runtime");
        runtime
            .spawn(&valid, Capabilities::none(), 4)
            .expect("a canonical bundle reaches ordinary instantiation");

        let mut unknown_schema = empty.canonical_bytes();
        unknown_schema[8..12].copy_from_slice(&2u32.to_le_bytes());
        let error = match runtime.spawn(
            wasm_with_layout_payload(unknown_schema),
            Capabilities::none(),
            4,
        ) {
            Ok(_) => panic!("an unknown descriptor schema must fail before instantiation"),
            Err(error) => error,
        };
        assert!(
            error.to_string().contains("unsupported layout schema 2"),
            "{error}"
        );
    }

    #[test]
    fn duplicate_artifact_layout_metadata_fails_closed() {
        let empty = LayoutBundle::from_interner(&LayoutInterner::new(), [])
            .expect("empty canonical layout bundle");
        let mut duplicate = wasm_with_layout_payload(empty.canonical_bytes());
        CustomSection {
            name: Cow::Borrowed(LAYOUT_SECTION),
            data: Cow::Owned(empty.canonical_bytes()),
        }
        .append_to(&mut duplicate);
        let mut runtime = Runtime::batch().expect("runtime");
        let error = match runtime.spawn(duplicate, Capabilities::none(), 4) {
            Ok(_) => panic!("duplicate descriptor metadata must fail before instantiation"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("duplicate section"), "{error}");
    }

    #[test]
    fn host_imports_require_generated_layout_metadata_before_linking() {
        let mut runtime = Runtime::batch().expect("runtime");
        // `print` has generated ABI metadata, so its layout contract
        // authenticates with no bundle (`validate_layout_metadata` passes). Grant
        // `print` so the import actually LINKS and the module instantiates —
        // otherwise the spawn only succeeds when `wasm-opt` happens to be on PATH
        // and dead-code-eliminates the unused import (true on macOS dev boxes,
        // false on Linux CI), making the test environment-dependent. The negative
        // `not_generated` case below still fails closed before linking.
        runtime
            .spawn(
                wasm_with_host_import("print"),
                Capabilities { print: true, ..Default::default() },
                4,
            )
            .expect("print's generated ABI metadata authenticates without a layout bundle");

        let error = match runtime.spawn(
            wasm_with_host_import("not_generated"),
            Capabilities::none(),
            4,
        ) {
            Ok(_) => panic!("an import without generated ABI metadata must fail closed"),
            Err(error) => error,
        };
        assert!(
            error.to_string().contains("has no generated ABI metadata"),
            "{error}"
        );
    }

    #[cfg(feature = "test-fixtures")]
    #[test]
    fn fixture_execution_validates_artifact_layout_metadata() {
        let empty = LayoutBundle::from_interner(&LayoutInterner::new(), [])
            .expect("empty canonical layout bundle");
        let mut unknown_schema = empty.canonical_bytes();
        unknown_schema[8..12].copy_from_slice(&2u32.to_le_bytes());
        let mut runtime = Runtime::batch().expect("runtime");
        let error = runtime
            .run_fixtures(
                wasm_with_layout_payload(unknown_schema),
                witchy_testkit::FixturePlan::default(),
                4,
            )
            .expect_err("fixture execution must reject an unknown layout schema");
        assert!(
            error.to_string().contains("unsupported layout schema 2"),
            "{error}"
        );
    }

    const IMPORTS_PRINT: &str = r#"
        (module
          (import "witchy" "print" (func $print (param i32 i32)))
          (memory (export "memory") 1)
          (func (export "run")))
    "#;

    const BARE: &str = r#"
        (module
          (memory (export "memory") 1)
          (func (export "run")))
    "#;

    const SPINNER: &str = r#"
        (module
          (memory (export "memory") 1)
          (func (export "run") (loop $l (br $l))))
    "#;

    #[test]
    fn forged_par_map_count_is_bounded_before_host_allocation() {
        const FORGED_COUNT: &str = r#"
            (module
              (import "witchy" "vm_par_map_run"
                (func $par_map (param i32 i32) (result i32)))
              (memory (export "memory") 1)
              (data (i32.const 0) "\ff\ff\ff\7f")
              (func (export "run")
                i32.const 0
                i32.const 0
                call $par_map
                drop))
        "#;
        let mut runtime = Runtime::new().expect("runtime");
        let mut vm = runtime
            .spawn(FORGED_COUNT, Capabilities::none(), 1)
            .expect("malformed module instantiates");
        let error = vm.run().expect_err("forged list count must fail closed");
        assert!(
            format!("{error:#}").contains("exceeds guest memory"),
            "the count must be bounded against guest memory before reserving host storage: {error:#}"
        );
    }

    const TRAP_ON_FIRST_RUN: &str = r#"
        (module
          (memory (export "memory") 1)
          (global $entered (mut i32) (i32.const 0))
          (func (export "run")
            (if (i32.eqz (global.get $entered))
              (then
                (global.set $entered (i32.const 1))
                unreachable))))
    "#;

    const GREEDY: &str = r#"
        (module (memory (export "memory") 4) (func (export "run")))
    "#;

    #[test]
    fn trapped_vm_is_terminal() {
        let mut runtime = Runtime::new().unwrap();
        let mut vm = runtime
            .spawn(TRAP_ON_FIRST_RUN, Capabilities::default(), 1)
            .unwrap();

        vm.run().expect_err("the first call traps");
        let err = vm
            .run()
            .expect_err("a trapped VM must not resume with abandoned roots");
        assert!(err.to_string().contains("aborted Witchy VM cannot be run again"), "{err}");
    }

    #[test]
    fn optimized_wasm_cache_envelope_rejects_corruption_and_swaps() {
        let input_hash = sha256(b"original input wasm");
        let payload = b"optimized wasm payload";
        let envelope = encode_optimized_wasm(input_hash, payload);
        assert_eq!(decode_optimized_wasm(&input_hash, &envelope), Some(payload.as_slice()));

        let other_input = sha256(b"different input wasm");
        assert!(decode_optimized_wasm(&other_input, &envelope).is_none(), "a cache file cannot move between input keys");

        let mut corrupt = envelope.clone();
        *corrupt.last_mut().unwrap() ^= 0x80;
        assert!(decode_optimized_wasm(&input_hash, &corrupt).is_none(), "payload corruption must fail before validation");
        assert!(decode_optimized_wasm(&input_hash, &envelope[..envelope.len() - 1]).is_none(), "truncation must be rejected");

        let mut bad_magic = envelope;
        bad_magic[0] ^= 0xff;
        assert!(decode_optimized_wasm(&input_hash, &bad_magic).is_none(), "unknown cache formats must be ignored");

        assert_ne!(
            optimized_wasm_input_hash(b"same wasm", b"wasm-opt version 1"),
            optimized_wasm_input_hash(b"same wasm", b"wasm-opt version 2"),
            "the Binaryen executable identity is part of cache identity",
        );
        assert_ne!(
            optimized_wasm_input_hash(b"wasm A", b"wasm-opt version 1"),
            optimized_wasm_input_hash(b"wasm B", b"wasm-opt version 1"),
            "the input module remains part of cache identity",
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn binaryen_requires_an_authenticated_bundle_and_runs_the_exact_recipe() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("temporary Binaryen fixture");
        let distribution = temp.path().join("binaryen-version_131");
        let configured = distribution.join("bin").join("wasm-opt");
        std::fs::create_dir_all(configured.parent().unwrap()).expect("fake Binaryen bin");
        let script = br#"#!/bin/sh
if [ "$1" = "--version" ]; then
    printf 'wasm-opt version 131 (version_131)\n'
    exit 0
fi
[ "$#" -eq 9 ] || exit 20
[ "$1" = "-O4" ] || exit 21
[ "$2" = "--all-features" ] || exit 22
[ "$3" = "--inlining-optimizing" ] || exit 23
[ "$4" = "--inline-functions-with-loops" ] || exit 24
[ "$5" = "--flexible-inline-max-function-size=100" ] || exit 25
[ "$6" = "--one-caller-inline-max-function-size=200" ] || exit 26
[ "$7" = "-o" ] || exit 27
/bin/cp "$9" "$8"
"#;
        std::fs::write(&configured, script).expect("write fake wasm-opt");
        std::fs::set_permissions(&configured, std::fs::Permissions::from_mode(0o700))
            .expect("make fake wasm-opt executable");
        let digest = sha256(script);
        #[cfg(target_os = "macos")]
        let library_digest = {
            let library = distribution.join("lib").join("libbinaryen.dylib");
            std::fs::create_dir_all(library.parent().unwrap()).expect("fake Binaryen lib");
            let bytes = b"fake authenticated Binaryen runtime library";
            std::fs::write(library, bytes).expect("write fake Binaryen runtime library");
            Some(sha256(bytes))
        };
        #[cfg(target_os = "linux")]
        let library_digest = None;
        let staging = temp.path().join("staging");

        assert!(authenticate_binaryen(
            std::path::Path::new("relative-wasm-opt"),
            digest,
            library_digest,
            &staging,
        )
        .is_none());
        assert!(authenticate_binaryen(&configured, [0x55; 32], library_digest, &staging).is_none());

        let tool = authenticate_binaryen(&configured, digest, library_digest, &staging)
            .expect("the exact configured executable authenticates");
        assert_eq!(&tool.identity[32..], PINNED_BINARYEN_VERSION);
        assert_eq!(sha256(&std::fs::read(&tool.executable).unwrap()), digest);
        assert!(tool.staged_bytes_are_authentic());

        let wasm = b"exact input bytes";
        let optimized = binaryen_optimize(wasm, Some(&tool), false)
            .expect("authenticated fake optimizer runs");
        assert!(
            matches!(optimized, Cow::Owned(ref bytes) if bytes == wasm),
            "the fake tool accepts only the exact production argument vector"
        );

        std::fs::write(&configured, b"#!/bin/sh\nprintf 'substituted\\n'\n")
            .expect("replace ambient configured path after authentication");
        let output = std::process::Command::new(&tool.executable)
            .arg("--version")
            .output()
            .expect("execute private staged tool");
        assert_eq!(output.stdout, PINNED_BINARYEN_VERSION);

        std::fs::set_permissions(&tool.executable, std::fs::Permissions::from_mode(0o700))
            .expect("make staged test fixture writable");
        std::fs::write(&tool.executable, b"#!/bin/sh\nexit 0\n")
            .expect("tamper with staged fixture");
        assert!(
            matches!(
                binaryen_optimize(wasm, Some(&tool), false).unwrap(),
                Cow::Borrowed(bytes) if bytes == wasm
            ),
            "a changed staged executable must fail closed before invocation"
        );
        assert!(
            binaryen_optimize(wasm, Some(&tool), true)
                .unwrap_err()
                .to_string()
                .contains("staged bundle changed after authentication"),
            "required evidence mode must reject staged tampering"
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn binaryen_rejects_unexpected_version_and_preserves_input_on_tool_failure() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("temporary Binaryen fixture");
        let distribution = temp.path().join("binaryen-version_131");
        let configured = distribution.join("bin").join("wasm-opt");
        std::fs::create_dir_all(configured.parent().unwrap()).expect("fake Binaryen bin");
        #[cfg(target_os = "macos")]
        let library_digest = {
            let library = distribution.join("lib").join("libbinaryen.dylib");
            std::fs::create_dir_all(library.parent().unwrap()).expect("fake Binaryen lib");
            let bytes = b"fake authenticated Binaryen runtime library";
            std::fs::write(library, bytes).expect("write fake Binaryen runtime library");
            Some(sha256(bytes))
        };
        #[cfg(target_os = "linux")]
        let library_digest = None;
        let staging = temp.path().join("staging");

        let wrong_version = b"#!/bin/sh\nprintf 'wasm-opt version 130\\n'\n";
        std::fs::write(&configured, wrong_version).expect("write wrong-version tool");
        std::fs::set_permissions(&configured, std::fs::Permissions::from_mode(0o700))
            .expect("make wrong-version tool executable");
        assert!(authenticate_binaryen(
            &configured,
            sha256(wrong_version),
            library_digest,
            &staging,
        )
        .is_none());

        let failing = b"#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then printf 'wasm-opt version 131 (version_131)\\n'; exit 0; fi\nexit 42\n";
        std::fs::write(&configured, failing).expect("write failing tool");
        let tool = authenticate_binaryen(
            &configured,
            sha256(failing),
            library_digest,
            &staging,
        )
        .expect("the failing tool still has the pinned version and identity");
        let wasm = b"already validated input";
        assert!(
            matches!(
                binaryen_optimize(wasm, Some(&tool), false).unwrap(),
                Cow::Borrowed(bytes) if bytes == wasm
            ),
            "a nonzero optimizer exit must preserve the validated input"
        );
        assert!(
            binaryen_optimize(wasm, Some(&tool), true)
                .unwrap_err()
                .to_string()
                .contains("optimizer process exited unsuccessfully"),
            "required evidence mode must reject optimizer process failure"
        );
    }

    #[test]
    fn invalid_binaryen_output_falls_back_to_the_validated_input() {
        let engine = Engine::default();
        let original = WasmModule::new().finish();
        let invalid = Cow::Owned(b"not a wasm module".to_vec());
        let (_, optimized_valid) = module_from_binaryen_output(&engine, &original, &invalid, false)
            .expect("invalid optimizer output falls back to the original module");
        assert!(!optimized_valid, "invalid optimizer output must not be cacheable");
        assert!(
            module_from_binaryen_output(&engine, &original, &invalid, true)
                .unwrap_err()
                .to_string()
                .contains("required pinned Binaryen produced invalid Wasm"),
            "required evidence mode must reject invalid optimizer output"
        );
    }

    #[test]
    fn preserve_raw_returns_before_binaryen_resolution() {
        let engine = Engine::default();
        let mut preserve_raw = WasmModule::new().finish();
        append_optimizer_policy(
            &mut preserve_raw,
            &witchy_wir::optimizer_policy::PRESERVE_RAW_PAYLOAD,
        );
        build_module_with_binaryen(
            &engine,
            &preserve_raw,
            true,
            false,
            || -> Result<Option<&BinaryenTool>> {
                panic!("PreserveRaw must not resolve or invoke Binaryen")
            },
        )
        .expect("PreserveRaw module uses the validated original bytes");
    }

    #[test]
    fn binaryen_pin_resolution_rejects_mismatch_partial_sets_and_cross_source_mixing() {
        let executable_digest = [0x11; 32];
        let library_digest = [0x22; 32];
        let executable_hex: String = executable_digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let library_hex: String = library_digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let absent = || ConfiguredBinaryenPin {
            path: None,
            executable_digest: None,
            runtime_library_digest: None,
        };
        let valid = || ConfiguredBinaryenPin {
            path: Some(std::path::PathBuf::from("/reviewed/bin/wasm-opt")),
            executable_digest: Some(executable_hex.clone()),
            runtime_library_digest: Some(library_hex.clone()),
        };

        assert_eq!(
            resolve_configured_binaryen_pin(
                absent(),
                absent(),
                (executable_digest, Some(library_digest)),
            )
            .unwrap_err(),
            "pin is not configured"
        );

        let partial_runtime = ConfiguredBinaryenPin {
            path: Some(std::path::PathBuf::from("/runtime/bin/wasm-opt")),
            executable_digest: None,
            runtime_library_digest: None,
        };
        assert_eq!(
            resolve_configured_binaryen_pin(
                absent(),
                partial_runtime,
                (executable_digest, Some(library_digest)),
            )
            .unwrap_err(),
            "runtime pin is incomplete"
        );

        let relative = ConfiguredBinaryenPin {
            path: Some(std::path::PathBuf::from("bin/wasm-opt")),
            executable_digest: Some(executable_hex.clone()),
            runtime_library_digest: Some(library_hex.clone()),
        };
        assert_eq!(
            resolve_configured_binaryen_pin(
                absent(),
                relative,
                (executable_digest, Some(library_digest)),
            )
            .unwrap_err(),
            "configured executable path is not absolute"
        );

        assert_eq!(
            resolve_configured_binaryen_pin(absent(), valid(), (executable_digest, Some(library_digest)))
                .unwrap()
                .1,
            executable_digest
        );

        let mismatched = ConfiguredBinaryenPin {
            path: Some(std::path::PathBuf::from("/reviewed/bin/wasm-opt")),
            executable_digest: Some("33".repeat(32)),
            runtime_library_digest: Some(library_hex.clone()),
        };
        assert_eq!(
            resolve_configured_binaryen_pin(absent(), mismatched, (executable_digest, Some(library_digest)))
                .unwrap_err(),
            "configured identity is not the reviewed Binaryen release pin"
        );

        let partial_compile = ConfiguredBinaryenPin {
            path: Some(std::path::PathBuf::from("/compile/bin/wasm-opt")),
            executable_digest: None,
            runtime_library_digest: None,
        };
        assert_eq!(
            resolve_configured_binaryen_pin(
                partial_compile,
                valid(),
                (executable_digest, Some(library_digest)),
            )
            .unwrap_err(),
            "compile-time pin is incomplete",
            "a partial compile-time pin must not borrow runtime values"
        );

        let linux_with_library = valid();
        assert_eq!(
            resolve_configured_binaryen_pin(absent(), linux_with_library, (executable_digest, None))
                .unwrap_err(),
            "configured identity is not the reviewed Binaryen release pin"
        );
        let mac_without_library = ConfiguredBinaryenPin {
            path: Some(std::path::PathBuf::from("/reviewed/bin/wasm-opt")),
            executable_digest: Some(executable_hex),
            runtime_library_digest: None,
        };
        assert_eq!(
            resolve_configured_binaryen_pin(
                absent(),
                mac_without_library,
                (executable_digest, Some(library_digest)),
            )
            .unwrap_err(),
            "configured identity is not the reviewed Binaryen release pin"
        );
    }

    #[test]
    fn required_binaryen_rejects_fallback_while_optional_mode_is_observable() {
        assert!(
            resolve_binaryen_result(false, Err("test authentication failure"))
                .expect("optional policy preserves the validated input")
                .is_none()
        );
        let error = resolve_binaryen_result(true, Err("test authentication failure"))
            .expect_err("required policy must make missing optimization fail closed");
        assert!(
            error.to_string().contains(
                "required pinned Binaryen unavailable: test authentication failure"
            ),
            "{error}"
        );
    }

    #[cfg(any(
        all(target_arch = "aarch64", target_os = "linux"),
        all(target_arch = "x86_64", target_os = "linux"),
        all(target_arch = "aarch64", target_os = "macos"),
        all(target_arch = "x86_64", target_os = "macos"),
    ))]
    #[test]
    fn binaryen_compiled_release_identity_matches_the_reviewed_lock() {
        let lock = include_str!("../../../tools/binaryen-version_131.lock");
        #[cfg(all(target_arch = "aarch64", target_os = "linux"))]
        let platform = "aarch64-linux";
        #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
        let platform = "x86_64-linux";
        #[cfg(all(target_arch = "aarch64", target_os = "macos"))]
        let platform = "arm64-macos";
        #[cfg(all(target_arch = "x86_64", target_os = "macos"))]
        let platform = "x86_64-macos";
        let asset = lock
            .split("[[asset]]")
            .find(|block| block.contains(&format!("platform = \"{platform}\"")))
            .expect("current platform has one reviewed lock asset");
        assert!(asset.contains(RELEASE_BINARYEN_EXECUTABLE_SHA256));
        #[cfg(target_os = "macos")]
        assert!(asset.contains(RELEASE_BINARYEN_LIBRARY_SHA256));
        assert!(lock.contains("version_stdout = \"wasm-opt version 131 (version_131)\\n\""));
    }

    #[cfg(any(
        all(target_arch = "aarch64", target_os = "linux"),
        all(target_arch = "x86_64", target_os = "linux"),
        all(target_arch = "aarch64", target_os = "macos"),
        all(target_arch = "x86_64", target_os = "macos"),
    ))]
    #[test]
    fn binaryen_reviewed_release_bundle_authenticates_and_runs() {
        let Some(configured) = std::env::var_os("WITCHY_TEST_REVIEWED_BINARYEN_PATH")
            .map(std::path::PathBuf::from)
        else {
            eprintln!(
                "skipping official-bundle execution: WITCHY_TEST_REVIEWED_BINARYEN_PATH is unset"
            );
            return;
        };
        let (executable_digest, library_digest) =
            release_binaryen_digests().expect("supported release platform");
        let staging = tempfile::tempdir().expect("private staging root");
        let tool = authenticate_binaryen(
            &configured,
            executable_digest,
            library_digest,
            staging.path(),
        )
        .expect("official reviewed bundle authenticates");
        let input = WasmModule::new().finish();
        let optimized = binaryen_optimize(&input, Some(&tool), true)
            .expect("official reviewed bundle executes the production recipe");
        assert!(matches!(optimized, Cow::Owned(_)));
        Module::new(&Engine::default(), optimized.as_ref())
            .expect("official Binaryen output remains valid Wasm");
    }

    #[test]
    fn binaryen_digest_parser_is_exact() {
        let digest = [0xab; 32];
        let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
        assert_eq!(decode_sha256_hex(&hex), Some(digest));
        assert_eq!(decode_sha256_hex(&hex[..63]), None);
        assert_eq!(decode_sha256_hex(&"z".repeat(64)), None);
        assert_eq!(decode_sha256_hex(&"+f".repeat(32)), None);
        assert_eq!(decode_sha256_hex(&hex.to_uppercase()), None);
    }

    #[test]
    fn bundled_binaryen_path_is_fixed_relative_to_the_installed_executable() {
        let executable = std::path::Path::new("/opt/witchy/bin/witchy");
        assert_eq!(
            bundled_binaryen_path_for_executable(executable).unwrap(),
            std::path::Path::new("/opt/witchy/libexec/witchy-binaryen/bin/wasm-opt")
        );
        assert_eq!(
            bundled_binaryen_path_for_executable(std::path::Path::new("/opt/witchy/witchy"))
                .unwrap_err(),
            "installed executable is not in the release bin directory"
        );
    }

    #[test]
    fn binaryen_evidence_names_the_exact_invocation_recipe() {
        assert_eq!(
            binaryen_recipe_evidence(),
            "-O4|--all-features|--inlining-optimizing|--inline-functions-with-loops|--flexible-inline-max-function-size=100|--one-caller-inline-max-function-size=200"
        );
        assert_eq!(sha256_hex(&sha256(PINNED_BINARYEN_VERSION)).len(), 64);
    }

    #[test]
    fn stale_debug_compilation_cache_generations_are_bounded_without_touching_release_cache() {
        let cache = tempfile::tempdir().expect("cache directory");
        let modules = cache.path().join("modules");
        std::fs::create_dir_all(&modules).expect("modules directory");

        for generation in 0..=MAX_DEBUG_COMPILATION_CACHE_GENERATIONS {
            std::fs::create_dir(modules.join(format!("wasmtime--{generation}")))
                .expect("debug generation");
        }
        std::fs::create_dir(modules.join("wasmtime-45.0.3"))
            .expect("release generation");

        prune_stale_debug_compilation_cache_generations(cache.path());

        assert!(
            !modules.join("wasmtime--0").exists(),
            "the oldest debug generation is unusable after a rebuild"
        );
        assert!(modules.join(format!("wasmtime--{MAX_DEBUG_COMPILATION_CACHE_GENERATIONS}")).exists());
        assert!(modules.join("wasmtime-45.0.3").exists(), "release cache remains reusable");
        let debug_generations = std::fs::read_dir(&modules)
            .expect("modules directory")
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("wasmtime--"))
            .count();
        assert_eq!(debug_generations, MAX_DEBUG_COMPILATION_CACHE_GENERATIONS);
    }

    const NULL_FILE_READ: &str = r#"
        (module
          (import "witchy" "file_read_len" (func $file_read_len (param externref) (result i32)))
          (func (export "run")
            (drop (call $file_read_len (ref.null extern)))))
    "#;

    const NULL_DIR_READ: &str = r#"
        (module
          (import "witchy" "dir_read_len" (func $dir_read_len (param externref i32) (result i32)))
          (memory (export "memory") 1)
          (func (export "run")
            (drop (call $dir_read_len (ref.null extern) (i32.const 0)))))
    "#;

    const NULL_NET_CONNECT: &str = r#"
        (module
          (import "witchy" "net_connect" (func $net_connect (param externref i32) (result externref)))
          (memory (export "memory") 1)
          (func (export "run")
            (drop (call $net_connect (ref.null extern) (i32.const 0)))))
    "#;

    const NULL_SOCKET_CLOSE: &str = r#"
        (module
          (import "witchy" "net_close" (func $net_close (param externref)))
          (func (export "run")
            (call $net_close (ref.null extern))))
    "#;

    const NULL_LISTENER_ACCEPT: &str = r#"
        (module
          (import "witchy" "net_accept" (func $net_accept (param externref) (result externref)))
          (func (export "run")
            (drop (call $net_accept (ref.null extern)))))
    "#;

    const NULL_SECRET_REVEAL: &str = r#"
        (module
          (import "witchy" "crypto_reveal_len" (func $crypto_reveal_len (param externref) (result i32)))
          (func (export "run")
            (drop (call $crypto_reveal_len (ref.null extern)))))
    "#;

    const NULL_SECRET_SIGN: &str = r#"
        (module
          (import "witchy" "crypto.sign" (func $crypto_sign (param externref i32 i32)))
          (memory (export "memory") 1)
          (func (export "run")
            (call $crypto_sign (ref.null extern) (i32.const 0) (i32.const 0))))
    "#;

    const DIRECT_FILE_WRITE: &str = r#"
        (module
          (import "witchy" "mint_file" (func $mint_file (param i32) (result externref)))
          (import "witchy" "file_write" (func $file_write (param externref i32)))
          (memory (export "memory") 1)
          (func (export "run")
            (call $file_write (call $mint_file (i32.const 0)) (i32.const 0))))
    "#;

    const VM_WITH_DIR_START_AUTHORITY: &str = r#"
        (module
          (import "witchy" "print" (func $print (param i32 i32)))
          (import "witchy" "mint_dir" (func $mint_dir (param i32) (result externref)))
          (import "witchy" "dir_exists" (func $dir_exists (param externref i32) (result i32)))
          (import "witchy" "fill_pending" (func $fill_pending (param i32)))
          (import "witchy" "vm_with_dir_run"
            (func $vm_with_dir_run (param externref i32 i32) (result i32)))
          (memory (export "memory") 1)
          (data (i32.const 0) "\0a\00\00\00Cargo.toml")
          (data (i32.const 128) "\00\00\00\00")
          (data (i32.const 260) "STOLEN")
          (global $found (mut i32) (i32.const 0))
          (func $start
            i32.const 0
            call $mint_dir
            i32.const 0
            call $dir_exists
            global.set $found)
          (start $start)
          (func (export "__galloc") (param i32) (result i32) i32.const 1024)
          (func (export "__call_dir_bytes") (param i32 externref i32) (result i32)
            i32.const 256
            global.get $found
            if (result i32) i32.const 6 else i32.const 0 end
            i32.store
            i32.const 256)
          (func (export "run")
            i32.const 0
            call $mint_dir
            i32.const 0
            i32.const 128
            call $vm_with_dir_run
            drop
            i32.const 512
            call $fill_pending
            i32.const 516
            i32.const 512
            i32.load
            call $print))
    "#;

    /// The core thesis: a capability that was not granted simply does not exist
    /// for the VM, so it cannot even be instantiated.
    #[test]
    fn ungranted_capability_is_unreachable() {
        let mut rt = Runtime::new().unwrap();
        let err = rt
            .spawn(IMPORTS_PRINT, Capabilities::none(), 4)
            .map(|_| ())
            .unwrap_err();
        assert!(
            err.to_string().contains("unknown import"),
            "expected an unknown-import error, got: {err}"
        );
    }

    #[test]
    fn granted_capability_instantiates() {
        let mut rt = Runtime::new().unwrap();
        let mut vm = rt
            .spawn(IMPORTS_PRINT, Capabilities { print: true, ..Default::default() }, 4)
            .unwrap();
        vm.run().unwrap();
    }

    #[test]
    fn vm_with_dir_start_observes_only_the_delegated_dir() {
        let root = tempfile::tempdir().expect("empty delegated root");
        assert!(std::path::Path::new("Cargo.toml").exists(), "test requires workspace cwd marker");
        assert!(!root.path().join("Cargo.toml").exists());

        let mut runtime = Runtime::new().expect("runtime");
        let mut vm = runtime
            .spawn(
                VM_WITH_DIR_START_AUTHORITY,
                Capabilities {
                    print: true,
                    quiet: true,
                    dir_root: Some(root.path().to_path_buf()),
                    dir_read: true,
                    ..Capabilities::default()
                },
                4,
            )
            .expect("instantiate authority probe");
        vm.run().expect("run authority probe");
        assert_eq!(vm.output(), vec![String::new()]);
    }

    /// Each VM's linear memory is its own; the runtime hands out separate
    /// `Store`s, so one VM's memory is never visible to another.
    #[test]
    fn vms_have_independent_memories() {
        let mut rt = Runtime::new().unwrap();
        let mut a = rt.spawn(BARE, Capabilities::default(), 4).unwrap();
        let mut b = rt.spawn(BARE, Capabilities::default(), 4).unwrap();
        let mem_a = a.instance.get_memory(&mut a.store, "memory").unwrap();
        let mem_b = b.instance.get_memory(&mut b.store, "memory").unwrap();
        // Distinct backing allocations.
        assert_ne!(
            mem_a.data_ptr(&a.store),
            mem_b.data_ptr(&b.store),
            "VMs must not share a linear memory"
        );
    }

    #[test]
    fn memory_budget_is_enforced() {
        let mut rt = Runtime::new().unwrap();
        let err = rt.spawn(GREEDY, Capabilities::none(), 1).map(|_| ()).unwrap_err();
        assert!(
            err.to_string().contains("memory"),
            "expected a memory-limit error, got: {err}"
        );
    }

    fn assert_null_externref_rejected(module: &str, capabilities: Capabilities, kind: &str) {
        let mut rt = Runtime::new().unwrap();
        let mut vm = rt.spawn(module, capabilities, 4).unwrap();
        let err = vm.run().unwrap_err();
        let detail = format!("{err:?}");
        let expected = format!("{kind} externref is null");
        assert!(
            detail.contains(&expected),
            "expected null {kind} externref rejection, got: {detail}"
        );
    }

    #[test]
    fn null_externrefs_are_rejected_by_every_capability_family() {
        let root = std::env::temp_dir().join(format!(
            "witchy-null-externrefs-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("file.txt");
        std::fs::write(&file, "host-owned").unwrap();

        let cases = [
            (
                NULL_FILE_READ,
                Capabilities {
                    file_grants: vec![file],
                    ..Default::default()
                },
                "File",
            ),
            (
                NULL_DIR_READ,
                Capabilities {
                    dir_root: Some(root.clone()),
                    dir_read: true,
                    ..Default::default()
                },
                "Dir",
            ),
            (
                NULL_NET_CONNECT,
                Capabilities {
                    net_allow: Some(vec!["127.0.0.1:1".to_string()]),
                    net_connect: true,
                    ..Default::default()
                },
                "Net",
            ),
            (
                NULL_SOCKET_CLOSE,
                Capabilities {
                    net_allow: Some(vec!["127.0.0.1:1".to_string()]),
                    net_connect: true,
                    ..Default::default()
                },
                "Socket",
            ),
            (
                NULL_LISTENER_ACCEPT,
                Capabilities {
                    net_allow: Some(vec!["127.0.0.1:0".to_string()]),
                    net_listen: true,
                    ..Default::default()
                },
                "Listener",
            ),
            (NULL_SECRET_REVEAL, Capabilities::default(), "Secret"),
            (
                NULL_SECRET_SIGN,
                Capabilities {
                    signing_key: Some([0x41; 32]),
                    secrets: vec![SecretGrant::new("signing", vec![0x41; 32])],
                    ..Default::default()
                },
                "Secret",
            ),
        ];
        for (module, capabilities, kind) in cases {
            assert_null_externref_rejected(module, capabilities, kind);
        }

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn read_only_direct_file_does_not_link_write() {
        let path = std::env::temp_dir().join(format!(
            "witchy-readonly-file-grant-{}-{}.txt",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, "host-owned").unwrap();

        let mut rt = Runtime::new().unwrap();
        let err = rt
            .spawn(
                DIRECT_FILE_WRITE,
                Capabilities {
                    file_grants: vec![path.clone()],
                    file_rights: vec![FsRights::new(true, false)],
                    ..Default::default()
                },
                4,
            )
            .map(|_| ())
            .unwrap_err();
        let _ = std::fs::remove_file(path);
        assert!(
            err.to_string().contains("unknown import"),
            "read-only File grants must not link file_write, got: {err}"
        );
    }

    /// A runaway VM that never yields is forcibly preempted by the scheduler.
    #[test]
    fn runaway_vm_is_preempted() {
        let mut rt = Runtime::new().unwrap();
        let mut spinner = rt.spawn(SPINNER, Capabilities::none(), 4).unwrap();
        let err = rt
            .run_with_budget(&mut spinner, Duration::from_millis(20))
            .unwrap_err();
        assert!(
            matches!(err.downcast_ref::<wasmtime::Trap>(), Some(wasmtime::Trap::Interrupt)),
            "expected an interrupt trap, got: {err}"
        );
    }

    const BYTE_EXPORT_ECHO: &str = r#"
        (module
          (memory (export "memory") 1)
          (global $heap (mut i32) (i32.const 64))
          (func (export "__galloc") (param $size i32) (result i32)
            (local $ptr i32)
            global.get $heap
            local.tee $ptr
            local.get $size
            i32.add
            global.set $heap
            local.get $ptr)
          (func (export "__export_export_echo") (param $ptr i32) (param $len i32) (result i32)
            local.get $ptr))
    "#;

    const BYTE_EXPORT_SPINNER: &str = r#"
        (module
          (memory (export "memory") 1)
          (global $heap (mut i32) (i32.const 64))
          (func (export "__galloc") (param $size i32) (result i32)
            (local $ptr i32)
            global.get $heap
            local.tee $ptr
            local.get $size
            i32.add
            global.set $heap
            local.get $ptr)
          (func (export "__export_export_spin") (param i32 i32) (result i32)
            (loop $forever
              br $forever)
            i32.const 0))
    "#;

    #[test]
    fn byte_export_round_trips_non_utf8_and_enforces_output_limit() {
        let mut runtime = Runtime::new().unwrap();
        let mut vm = runtime
            .spawn(BYTE_EXPORT_ECHO, Capabilities::none(), 2)
            .unwrap();
        let input = [0, 0xff, 1, 0x80, 2];
        assert_eq!(
            vm.invoke_bytes("__export_export_echo", &input, input.len())
                .unwrap(),
            input
        );

        let mut limited = runtime
            .spawn(BYTE_EXPORT_ECHO, Capabilities::none(), 2)
            .unwrap();
        let error = limited
            .invoke_bytes("__export_export_echo", &input, input.len() - 1)
            .unwrap_err();
        assert!(error.to_string().contains("over the 4-byte limit"), "{error}");
    }

    #[test]
    fn byte_export_budget_interrupts_and_success_cancels_watchdog_promptly() {
        let mut runtime = Runtime::new().unwrap();
        let mut spinner = runtime
            .spawn(BYTE_EXPORT_SPINNER, Capabilities::none(), 2)
            .unwrap();
        let error = runtime
            .invoke_bytes_with_budget(
                &mut spinner,
                "__export_export_spin",
                b"",
                1,
                Duration::from_millis(20),
            )
            .unwrap_err();
        assert!(
            matches!(error.downcast_ref::<wasmtime::Trap>(), Some(wasmtime::Trap::Interrupt)),
            "expected an interrupt trap, got: {error}"
        );

        let mut echo = runtime
            .spawn(BYTE_EXPORT_ECHO, Capabilities::none(), 2)
            .unwrap();
        let started = std::time::Instant::now();
        assert_eq!(
            runtime
                .invoke_bytes_with_budget(
                    &mut echo,
                    "__export_export_echo",
                    b"ok",
                    2,
                    Duration::from_secs(2),
                )
                .unwrap(),
            b"ok"
        );
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "successful invocation waited for the watchdog budget"
        );
    }

    #[test]
    fn read_wstr_list_huge_count_fails_closed_not_aborts() {
        // SEC-033: a guest claiming i32::MAX elements in a tiny buffer must NOT pre-allocate
        // ~51GB and abort the host — the capacity hint is capped at the slots that fit in
        // memory, and the read fails closed (out-of-bounds Err) on the first slot past the end.
        let mut data = vec![0u8; 8];
        data[0..4].copy_from_slice(&i32::MAX.to_le_bytes());
        assert!(
            read_wstr_list(&data, 0).is_err(),
            "a bogus huge list count must fail closed, not allocate/abort"
        );
    }

    // RFC-0023 checked heap: a `heap_register(start,end)` poisons the trailing
    // redzone; the post-run sweep proves it survived. A clean object passes; a write
    // past the object end into the redzone — in-bounds for the linear memory, so
    // invisible to wasmtime — is caught.
    const HEAP_OK: &str = r#"
        (module
          (import "witchy" "heap_register" (func $reg (param i32 i32)))
          (memory (export "memory") 1)
          (func (export "run")
            (call $reg (i32.const 16) (i32.const 32))))
    "#;

    const HEAP_OVERRUN: &str = r#"
        (module
          (import "witchy" "heap_register" (func $reg (param i32 i32)))
          (memory (export "memory") 1)
          (func (export "run")
            (call $reg (i32.const 16) (i32.const 32))
            (i32.store (i32.const 32) (i32.const 0))))
    "#;

    // SEC-035: read_line_capped consumes exactly one line per call (leaving the rest for
    // the next recv — line protocols depend on this) and returns the remainder at EOF.
    #[test]
    fn read_line_capped_splits_one_line_per_call() {
        use std::io::BufReader;
        let data = b"hello\nworld\ntail-no-newline";
        let mut r = BufReader::new(&data[..]);
        assert_eq!(crate::net::read_line_capped(&mut r).unwrap(), b"hello\n");
        assert_eq!(crate::net::read_line_capped(&mut r).unwrap(), b"world\n");
        assert_eq!(crate::net::read_line_capped(&mut r).unwrap(), b"tail-no-newline");
        assert_eq!(crate::net::read_line_capped(&mut r).unwrap(), b""); // EOF
    }

    #[test]
    fn heap_check_clean_redzone_passes() {
        let mut rt = Runtime::new().unwrap();
        let mut vm = rt.spawn(HEAP_OK, Capabilities::none(), 4).unwrap();
        vm.run().expect("an untouched redzone must pass the sweep");
    }

    #[test]
    fn heap_check_overrun_redzone_traps() {
        let mut rt = Runtime::new().unwrap();
        let mut vm = rt.spawn(HEAP_OVERRUN, Capabilities::none(), 4).unwrap();
        let err = vm.run().map(|_| ()).unwrap_err();
        assert!(
            err.to_string().contains("HEAP CHECK"),
            "an overrun into the redzone must trap the sweep, got: {err}"
        );
    }

    // A region/watermark reset moves $heap back and reuses the space. `heap_frontier(wm)`
    // is the reclaim signal (emitted by the checked `$ensure` and the region copy-out): it
    // drops every redzone reaching `wm`, so the reused space's legitimate overwrite (here,
    // the i64 store over A's old redzone) is NOT later mistaken for an overrun.
    const HEAP_REGION_REUSE: &str = r#"
        (module
          (import "witchy" "heap_register" (func $reg (param i32 i32)))
          (import "witchy" "heap_frontier" (func $reclaim (param i32)))
          (memory (export "memory") 1)
          (func (export "run")
            (call $reg (i32.const 16) (i32.const 20))   ;; A [16,20), redzone [20,28)
            (call $reclaim (i32.const 16))              ;; reset to wm=16 reclaims A's redzone
            (i64.store (i32.const 20) (i64.const 123))  ;; reused space overwritten (legit)
            (call $reg (i32.const 16) (i32.const 28))))  ;; B re-registered at the base
    "#;

    #[test]
    fn heap_check_region_reuse_does_not_false_positive() {
        let mut rt = Runtime::new().unwrap();
        let mut vm = rt.spawn(HEAP_REGION_REUSE, Capabilities::none(), 4).unwrap();
        vm.run().expect("a reclaim of the watermark must drop the stale redzone");
    }

    // The reclaim must NOT drop a redzone that lies entirely below the watermark — a
    // pre-region object stays guarded across the region.
    const HEAP_RECLAIM_KEEPS_BELOW: &str = r#"
        (module
          (import "witchy" "heap_register" (func $reg (param i32 i32)))
          (import "witchy" "heap_frontier" (func $reclaim (param i32)))
          (memory (export "memory") 1)
          (func (export "run")
            (call $reg (i32.const 16) (i32.const 20))   ;; A [16,20), redzone [20,28)
            (call $reclaim (i32.const 28))              ;; wm=28 is above A's whole redzone
            (i32.store (i32.const 20) (i32.const 0))))   ;; corrupt A's still-guarded redzone
    "#;

    #[test]
    fn heap_check_reclaim_keeps_redzone_below_watermark() {
        let mut rt = Runtime::new().unwrap();
        let mut vm = rt.spawn(HEAP_RECLAIM_KEEPS_BELOW, Capabilities::none(), 4).unwrap();
        let err = vm.run().map(|_| ()).unwrap_err();
        assert!(
            err.to_string().contains("HEAP CHECK"),
            "a redzone below the reclaim watermark must stay guarded, got: {err}"
        );
    }

    const HEAP_UNREGISTER_IDEMPOTENT: &str = r#"
        (module
          (import "witchy" "heap_register" (func $reg (param i32 i32)))
          (import "witchy" "heap_unregister" (func $unreg (param i32)))
          (memory (export "memory") 1)
          (func (export "run")
            (call $reg (i32.const 16) (i32.const 20))
            (call $reg (i32.const 32) (i32.const 36))
            (call $unreg (i32.const 16))
            (call $unreg (i32.const 16))
            ;; Retired A's redzone may be overwritten; B's remains intact.
            (i64.store (i32.const 20) (i64.const 0))))
    "#;

    const HEAP_UNREGISTER_KEEPS_ADJACENT: &str = r#"
        (module
          (import "witchy" "heap_register" (func $reg (param i32 i32)))
          (import "witchy" "heap_unregister" (func $unreg (param i32)))
          (memory (export "memory") 1)
          (func (export "run")
            (call $reg (i32.const 16) (i32.const 20))
            (call $reg (i32.const 32) (i32.const 36))
            (call $unreg (i32.const 16))
            ;; Exact retirement must leave B's higher redzone guarded.
            (i32.store (i32.const 36) (i32.const 0))))
    "#;

    #[test]
    fn heap_unregister_is_idempotent_for_one_exact_object() {
        let mut rt = Runtime::new().unwrap();
        let mut vm = rt.spawn(HEAP_UNREGISTER_IDEMPOTENT, Capabilities::none(), 4).unwrap();
        vm.run().expect("retiring the same exact object twice must be harmless");
    }

    #[test]
    fn heap_unregister_keeps_higher_adjacent_redzones_guarded() {
        let mut rt = Runtime::new().unwrap();
        let mut vm = rt.spawn(HEAP_UNREGISTER_KEEPS_ADJACENT, Capabilities::none(), 4).unwrap();
        let err = vm.run().expect_err("retiring A must not retire adjacent B");
        assert!(err.to_string().contains("HEAP CHECK"), "adjacent B lost protection: {err}");
    }

    fn checked_uaf_module(double_free: bool) -> Vec<u8> {
        use witchy_wir::wir::{
            BinOp, GlobalInit, Kind, WirExpr as E, WirFunc, WirGlobal, WirImport, WirLocal,
            WirModule, WirNode as N, WirTy,
        };

        let gl = |name: &str| E::GetLocal(name.into());
        let i32_bin = |op, lhs, rhs| E::Binary {
            op,
            kind: Kind::I32,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
        };
        let assert_i32 = |lhs, rhs| N::If {
            cond: i32_bin(BinOp::Eq, lhs, rhs),
            then_: vec![],
            els: vec![N::Unreachable],
            result: None,
        };
        let assert_i64 = |lhs, rhs| N::If {
            cond: E::Binary {
                op: BinOp::Eq,
                kind: Kind::I64,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            },
            then_: vec![],
            els: vec![N::Unreachable],
            result: None,
        };
        let alloc = || E::Call { func: "rc_alloc".into(), args: vec![E::ConstI32(24)] };
        let register = |name: &str| N::Do(E::CallHost {
            import: "heap_register".into(),
            args: vec![gl(name), i32_bin(BinOp::Add, gl(name), E::ConstI32(16))],
        });
        let free = |name: &str| N::Do(E::Call {
            func: "rc_free".into(),
            args: vec![gl(name)],
        });
        let store = |name: &str, offset, value| N::Store {
            ptr: gl(name),
            value: E::ConstI32(value),
            kind: Kind::I32,
            offset,
        };
        let load = |name: &str, offset| E::Load {
            ptr: Box::new(gl(name)),
            kind: Kind::I32,
            offset,
        };

        let mut body = vec![
            N::SetLocal { local: "first".into(), value: alloc() },
            store("first", 0, 101),
            store("first", 4, 102),
            store("first", 8, 103),
            store("first", 12, 104),
            register("first"),
            N::SetLocal { local: "second".into(), value: alloc() },
            store("second", 0, 201),
            store("second", 4, 202),
            store("second", 8, 203),
            store("second", 12, 204),
            register("second"),
            free("first"),
        ];
        if double_free {
            body.push(free("first"));
        } else {
            body.push(N::SetLocal { local: "fresh".into(), value: alloc() });
            body.push(assert_i32(
                i32_bin(BinOp::Eq, gl("first"), gl("fresh")),
                E::ConstI32(0),
            ));
            for offset in [0, 4, 8, 12, 16, 20] {
                body.push(assert_i32(load("first", offset), E::ConstI32(0xDEAD_BEEFu32 as i32)));
            }
            body.extend([
                assert_i32(
                    E::Load {
                        ptr: Box::new(i32_bin(BinOp::Sub, gl("second"), E::ConstI32(8))),
                        kind: Kind::I32,
                        offset: 0,
                    },
                    E::ConstI32(1),
                ),
                assert_i32(
                    E::Load {
                        ptr: Box::new(i32_bin(BinOp::Sub, gl("second"), E::ConstI32(4))),
                        kind: Kind::I32,
                        offset: 0,
                    },
                    E::ConstI32(24),
                ),
                assert_i32(load("second", 0), E::ConstI32(201)),
                assert_i32(load("second", 4), E::ConstI32(202)),
                assert_i32(load("second", 8), E::ConstI32(203)),
                assert_i32(load("second", 12), E::ConstI32(204)),
                assert_i64(
                    E::Load { ptr: Box::new(gl("second")), kind: Kind::I64, offset: 16 },
                    E::ConstI64(i64::from_le_bytes([0xDB; 8])),
                ),
                assert_i32(E::GetGlobal("rc_freelist".into()), E::ConstI32(0)),
                assert_i64(E::GetGlobal("__witchy_rc_reuse_calls".into()), E::ConstI64(0)),
            ]);
        }

        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: ["first", "second", "fresh"]
                .into_iter()
                .map(|name| WirLocal { name: name.into(), ty: WirTy::Bool })
                .collect(),
            body,
            raw_body: None,
        };
        let mut funcs = witchy_wir::wir_helpers::rc_allocator_helpers_for_test(true, true);
        funcs.push(run);
        let module = WirModule {
            imports: vec![
                WirImport { name: "heap_register".into(), params: vec![Kind::I32, Kind::I32], results: vec![] },
                WirImport { name: "heap_unregister".into(), params: vec![Kind::I32], results: vec![] },
                WirImport { name: "heap_frontier".into(), params: vec![Kind::I32], results: vec![] },
            ],
            funcs,
            memory_pages: 1,
            data: vec![],
            globals: vec![
                WirGlobal { name: "heap".into(), kind: Kind::I32, mutable: true, init: GlobalInit::I32(2048), export: None },
                WirGlobal { name: "heap_base".into(), kind: Kind::I32, mutable: false, init: GlobalInit::I32(2048), export: None },
                WirGlobal { name: "rc_freelist".into(), kind: Kind::I32, mutable: true, init: GlobalInit::I32(0), export: None },
                WirGlobal { name: "__rc_reused_bytes".into(), kind: Kind::I64, mutable: true, init: GlobalInit::I64(0), export: None },
                WirGlobal { name: "__witchy_live_cells".into(), kind: Kind::I64, mutable: true, init: GlobalInit::I64(0), export: None },
                WirGlobal { name: "__witchy_rc_alloc_calls".into(), kind: Kind::I64, mutable: true, init: GlobalInit::I64(0), export: None },
                WirGlobal { name: "__witchy_rc_alloc_requested_bytes".into(), kind: Kind::I64, mutable: true, init: GlobalInit::I64(0), export: None },
                WirGlobal { name: "__witchy_bump_alloc_calls".into(), kind: Kind::I64, mutable: true, init: GlobalInit::I64(0), export: None },
                WirGlobal { name: "__witchy_rc_reuse_calls".into(), kind: Kind::I64, mutable: true, init: GlobalInit::I64(0), export: None },
                WirGlobal { name: "__witchy_rc_free_calls".into(), kind: Kind::I64, mutable: true, init: GlobalInit::I64(0), export: None },
            ],
            table: None,
            exports: vec![("run".into(), "run".into())],
        };
        witchy_wir::wir_encode::try_encode_with_gc(&module, &[], &[])
            .expect("combined checked-UAF fixture must encode")
    }

    #[test]
    fn checked_uaf_unregisters_only_the_freed_cell_before_full_poison() {
        let mut rt = Runtime::new().unwrap();
        let mut vm = rt.spawn(checked_uaf_module(false), Capabilities::none(), 4).unwrap();
        vm.run().expect("full-cell poison must retire only the freed object's redzone");
    }

    #[test]
    fn checked_uaf_double_free_still_traps() {
        let mut rt = Runtime::new().unwrap();
        let mut vm = rt.spawn(checked_uaf_module(true), Capabilities::none(), 4).unwrap();
        vm.run().expect_err("the zero RC quarantine marker must trap a repeated free");
    }

#[test]
fn concrete_grants_have_one_complete_confinement_policy() {
    use std::collections::BTreeSet;
    use std::path::PathBuf;
    use witchy_confinement::{FsAccess, FsRule, FsScope, SyscallClass};

    let caps = Capabilities {
        dir_root: Some(PathBuf::from("/read")),
        dir_roots: vec![PathBuf::from("/write")],
        dir_rights: vec![
            FsRights::new(true, false),
            FsRights::new(false, true),
        ],
        file_grants: vec![PathBuf::from("/config/app.toml")],
        file_rights: vec![FsRights::new(true, false)],
        exec: true,
        net_allow: Some(vec!["db.example:5432".into()]),
        net_connect: true,
        net_grants: vec![vec!["127.0.0.1:8080".into()]],
        net_listen: true,
        fetch_grants: vec![vec!["https://api.example".into()]],
        build_out: Some(PathBuf::from("/generated")),
        build_read_roots: vec![PathBuf::from("/schema")],
        build_exec_tools: vec![("tool".into(), PathBuf::from("/bin/tool"))],
        build_exec_runtime_roots: vec![PathBuf::from("/lib")],
        build_net_allow: Some(vec!["cache.example:8443".into()]),
        ..Default::default()
    };

    let policy = caps.confinement_policy();
    assert_eq!(
        policy.filesystem,
        vec![
            FsRule {
                path: PathBuf::from("/bin/tool"),
                scope: FsScope::File,
                access: FsAccess::new(true, false, true),
            },
            FsRule {
                path: PathBuf::from("/config/app.toml"),
                scope: FsScope::File,
                access: FsAccess::new(true, false, false),
            },
            FsRule {
                path: PathBuf::from("/generated"),
                scope: FsScope::Tree,
                // BuildOut is read+write: writing requires opening/traversing the
                // output tree, and the build host lists it afterward.
                access: FsAccess::new(true, true, false),
            },
            FsRule {
                path: PathBuf::from("/lib"),
                scope: FsScope::Tree,
                access: FsAccess::new(true, false, true),
            },
            FsRule {
                path: PathBuf::from("/read"),
                scope: FsScope::Tree,
                access: FsAccess::new(true, false, true),
            },
            FsRule {
                path: PathBuf::from("/schema"),
                scope: FsScope::Tree,
                access: FsAccess::new(true, false, false),
            },
            FsRule {
                path: PathBuf::from("/write"),
                scope: FsScope::Tree,
                access: FsAccess::new(false, true, false),
            },
        ]
    );
    assert_eq!(
        policy.network.connect_tcp_ports,
        BTreeSet::from([443, 5432, 8080, 8443])
    );
    assert_eq!(
        policy.network.bind_tcp_ports,
        BTreeSet::from([5432, 8080])
    );
    assert_eq!(
        policy.syscall_classes,
        BTreeSet::from([
            SyscallClass::Base,
            SyscallClass::FsOpen,
            SyscallClass::Network,
            SyscallClass::Listen,
            SyscallClass::Process,
        ])
    );
}

#[test]
fn empty_build_grants_do_not_widen_outer_authority() {
    use witchy_confinement::SyscallClass;

    let policy = Capabilities {
        exec_allow: Some(Vec::new()),
        build_net_allow: Some(Vec::new()),
        ..Default::default()
    }
    .confinement_policy();

    assert!(!policy.network.connect_requested);
    assert!(!policy.syscall_classes.contains(&SyscallClass::Network));
    assert!(!policy.syscall_classes.contains(&SyscallClass::Process));
}

#[test]
fn exec_child_paths_widen_only_the_read_only_outer_fence() {
    use witchy_confinement::{FsAccess, FsRule, FsScope};

    let policy = Capabilities {
        exec_child_paths: vec![PathBuf::from("/child-config")],
        ..Default::default()
    }
    .confinement_policy();

    assert_eq!(
        policy.filesystem,
        vec![FsRule {
            path: PathBuf::from("/child-config"),
            scope: FsScope::Path,
            access: FsAccess::new(true, false, false),
        }]
    );
}

#[test]
fn empty_network_grant_remains_distinct_from_no_network_grant() {
    use witchy_confinement::SyscallClass;

    let absent = Capabilities::none().confinement_policy();
    let empty = Capabilities {
        net_allow: Some(Vec::new()),
        net_connect: true,
        ..Default::default()
    }
    .confinement_policy();

    assert!(!absent.network.connect_requested);
    assert!(!absent.syscall_classes.contains(&SyscallClass::Network));
    assert!(empty.network.connect_requested);
    assert!(empty.network.connect_tcp_ports.is_empty());
    assert!(empty.syscall_classes.contains(&SyscallClass::Network));
}

#[cfg(feature = "test-fixtures")]
#[test]
fn compiled_basic_fixtures_use_one_shared_host_and_transcript() {
    use witchy_testkit::{
        ClockFixture, ConsoleFixture, EnvFixture, Expectations, FixtureFamily, FixtureOutcome,
        FixturePlan, FixtureStep, FixtureValue, RandFixture, U64Text,
    };

    const MODULE: &str = r#"
        (module
          (import "witchy" "print" (func $print (param i32 i32)))
          (import "witchy" "console_read_len" (func $read (result i32)))
          (import "witchy" "now" (func $now (result i64)))
          (import "witchy" "now_monotonic" (func $mono (result i64)))
          (import "witchy" "rand_u64" (func $rand (result i64)))
          (import "witchy" "mint_env" (func $mint_env (result externref)))
          (import "witchy" "env_len" (func $env_len (param externref i32) (result i32)))
          (import "witchy" "args_size" (func $args_size (result i32)))
          (memory (export "memory") 1)
          (data (i32.const 0) "hello\0a")
          (data (i32.const 16) "\04\00\00\00MODE")
          (func (export "run") (local $env externref)
            i32.const 0
            i32.const 6
            call $print
            call $read
            drop
            call $now
            drop
            call $mono
            drop
            call $rand
            drop
            call $mint_env
            local.set $env
            local.get $env
            i32.const 16
            call $env_len
            drop
            call $args_size
            drop))
    "#;
    let plan = FixturePlan {
        version: 1,
        console: Some(ConsoleFixture {
            script: vec![
                console_output_step("hello"),
                FixtureStep {
                    operation: "console_read_len".to_owned(),
                    target: None,
                    arguments: std::collections::BTreeMap::new(),
                    effective_rights: Some(vec!["Read".to_owned()]),
                    outcome: FixtureOutcome::Return {
                        value: FixtureValue::String("fixture input".to_owned()),
                    },
                    required: true,
                },
            ],
        }),
        clock: Some(ClockFixture {
            start_ns: Some(U64Text::new(2_000_000)),
            step_ns: Some(U64Text::new(1_000_000)),
            repeat_last: false,
            script: Vec::new(),
        }),
        rand: Some(RandFixture {
            seed: Some(U64Text::new(7)),
            script: Vec::new(),
        }),
        env: Some(EnvFixture {
            values: std::collections::BTreeMap::from([(
                "MODE".to_owned(),
                "fixture".to_owned(),
            )]),
            allow: vec!["MODE".to_owned()],
            script: Vec::new(),
        }),
        argv: Some(vec!["one".to_owned()]),
        expectations: Expectations::default(),
        ..FixturePlan::default()
    };
    let mut runtime = Runtime::batch().expect("runtime");
    let outcome = runtime
        .run_fixtures(MODULE, plan, 2)
        .expect("compiled fixture run");
    assert!(matches!(outcome.result, FixtureWasmResult::Passed));
    assert_eq!(outcome.output, vec!["hello"]);
    assert_eq!(outcome.transcript.stdout, vec!["hello"]);
    assert_eq!(
        outcome
            .transcript
            .events
            .iter()
            .map(|event| (event.family, event.operation.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (FixtureFamily::Env, "mint_env"),
            (FixtureFamily::Console, "print"),
            (FixtureFamily::Console, "console_read_len"),
            (FixtureFamily::Clock, "now"),
            (FixtureFamily::Clock, "now"),
            (FixtureFamily::Rand, "rand_u64"),
            (FixtureFamily::Env, "env_len"),
            (FixtureFamily::Argv, "args"),
        ]
    );
}

#[cfg(feature = "test-fixtures")]
#[test]
fn compiled_filesystem_fixture_uses_opaque_handles_and_shared_state() {
    use witchy_testkit::{
        FilesystemEntry, FilesystemFixture, FixtureFamily, FixturePlan,
    };

    const MODULE: &str = r#"
        (module
          (import "witchy" "mint_dir" (func $mint_dir (param i32) (result externref)))
          (import "witchy" "dir_read_len"
            (func $dir_read_len (param externref i32) (result i32)))
          (import "witchy" "dir_write"
            (func $dir_write (param externref i32 i32)))
          (import "witchy" "dir_exists"
            (func $dir_exists (param externref i32) (result i32)))
          (memory (export "memory") 1)
          (data (i32.const 0) "\08\00\00\00seed.txt")
          (data (i32.const 32) "\07\00\00\00new.txt")
          (data (i32.const 64) "\03\00\00\00new")
          (func (export "run") (local $dir externref)
            i32.const 0
            call $mint_dir
            local.set $dir
            local.get $dir
            i32.const 0
            call $dir_read_len
            drop
            local.get $dir
            i32.const 32
            i32.const 64
            call $dir_write
            local.get $dir
            i32.const 32
            call $dir_exists
            drop
            local.get $dir
            i32.const 32
            call $dir_read_len
            drop))
    "#;
    let plan = FixturePlan {
        version: 1,
        filesystem: Some(FilesystemFixture {
            entries: std::collections::BTreeMap::from([(
                "seed.txt".to_owned(),
                FilesystemEntry::File {
                    hex: "6f6c64".to_owned(),
                },
            )]),
            rights: vec!["Read".to_owned(), "Write".to_owned()],
            entry_policy: None,
            script: Vec::new(),
        }),
        ..FixturePlan::default()
    };
    let mut runtime = Runtime::batch().expect("runtime");
    let outcome = runtime
        .run_fixtures(MODULE, plan, 2)
        .expect("compiled fixture run");
    assert!(matches!(outcome.result, FixtureWasmResult::Passed));
    assert_eq!(
        outcome
            .transcript
            .events
            .iter()
            .map(|event| (event.family, event.operation.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (FixtureFamily::Filesystem, "mint_dir"),
            (FixtureFamily::Filesystem, "dir_read_len"),
            (FixtureFamily::Filesystem, "dir_write"),
            (FixtureFamily::Filesystem, "dir_exists"),
            (FixtureFamily::Filesystem, "dir_read_len"),
        ]
    );
}

#[cfg(feature = "test-fixtures")]
#[test]
fn compiled_fetch_fixture_stages_success_and_provider_failures_without_network() {
    use witchy_testkit::{
        ConsoleFixture, FetchFixture, FixtureErrorCode, FixtureFailure, FixtureFamily,
        FixtureOutcome, FixturePlan, FixtureStep, FixtureValue,
    };

    const URL: &str = "https://example.com/data";
    const RESPONSE: &str = "HTTP/1.1 200\r\nX-Test: fixture\r\n\r\nok";
    const TIMEOUT: &str = "WITCHY_FETCH_ERROR:timeout:configured timeout";
    const MODULE: &str = r#"
        (module
          (import "witchy" "mint_fetch" (func $mint_fetch (param i32) (result externref)))
          (import "witchy" "fetch_send_len"
            (func $fetch_send_len (param externref i32 i32 i32 i32) (result i32)))
          (import "witchy" "fill_pending" (func $fill_pending (param i32)))
          (import "witchy" "print" (func $print (param i32 i32)))
          (memory (export "memory") 1)
          (data (i32.const 0) "\03\00\00\00GET")
          (data (i32.const 32) "\18\00\00\00https://example.com/data")
          (data (i32.const 96) "\00\00\00\00")
          (data (i32.const 104) "\00\00\00\00")
          (func (export "run") (local $fetch externref) (local $length i32)
            i32.const 0
            call $mint_fetch
            local.set $fetch
            local.get $fetch
            i32.const 0
            i32.const 32
            i32.const 96
            i32.const 104
            call $fetch_send_len
            local.set $length
            i32.const 256
            call $fill_pending
            i32.const 256
            local.get $length
            call $print
            local.get $fetch
            i32.const 0
            i32.const 32
            i32.const 96
            i32.const 104
            call $fetch_send_len
            local.set $length
            i32.const 256
            call $fill_pending
            i32.const 256
            local.get $length
            call $print))
    "#;
    let request_arguments = std::collections::BTreeMap::from([
        ("method".to_owned(), FixtureValue::String("GET".to_owned())),
        ("headers".to_owned(), FixtureValue::List(Vec::new())),
        ("body".to_owned(), FixtureValue::Bytes(String::new())),
    ]);
    let plan = FixturePlan {
        version: 1,
        console: Some(ConsoleFixture {
            script: vec![
                console_output_step(RESPONSE),
                console_output_step(TIMEOUT),
            ],
        }),
        fetch: Some(FetchFixture {
            origins: vec!["https://example.com:443".to_owned()],
            script: vec![
                FixtureStep {
                    operation: "fetch_send_len".to_owned(),
                    target: Some(URL.to_owned()),
                    arguments: request_arguments.clone(),
                    effective_rights: Some(vec!["https://example.com:443".to_owned()]),
                    outcome: FixtureOutcome::Return {
                        value: FixtureValue::Map(std::collections::BTreeMap::from([
                            ("status".to_owned(), FixtureValue::String("200".to_owned())),
                            (
                                "headers".to_owned(),
                                FixtureValue::List(vec![FixtureValue::Map(
                                    std::collections::BTreeMap::from([
                                        (
                                            "name".to_owned(),
                                            FixtureValue::String("X-Test".to_owned()),
                                        ),
                                        (
                                            "value".to_owned(),
                                            FixtureValue::String("fixture".to_owned()),
                                        ),
                                    ]),
                                )]),
                            ),
                            ("body".to_owned(), FixtureValue::Bytes("6f6b".to_owned())),
                        ])),
                    },
                    required: true,
                },
                FixtureStep {
                    operation: "fetch_send_len".to_owned(),
                    target: Some(URL.to_owned()),
                    arguments: request_arguments,
                    effective_rights: Some(vec!["https://example.com:443".to_owned()]),
                    outcome: FixtureOutcome::Fail {
                        error: FixtureFailure {
                            code: FixtureErrorCode::Timeout,
                            message: "configured timeout".to_owned(),
                        },
                    },
                    required: true,
                },
            ],
        }),
        ..FixturePlan::default()
    };
    let mut runtime = Runtime::batch().expect("runtime");
    let outcome = runtime
        .run_fixtures(MODULE, plan, 2)
        .expect("compiled fixture run");
    assert!(matches!(outcome.result, FixtureWasmResult::Passed));
    assert_eq!(outcome.output, vec![RESPONSE, TIMEOUT]);
    assert_eq!(
        outcome
            .transcript
            .events
            .iter()
            .filter(|event| event.family == FixtureFamily::Fetch)
            .map(|event| event.operation.as_str())
            .collect::<Vec<_>>(),
        vec!["mint_fetch", "fetch_send_len", "fetch_send_len"]
    );
}

#[cfg(feature = "test-fixtures")]
#[test]
fn compiled_secret_fixture_keeps_material_opaque_and_scripts_crypto() {
    use witchy_testkit::{
        ConsoleFixture, FixtureFamily, FixtureOutcome, FixturePlan, FixtureStep, FixtureValue,
        SecretFixture, SecretStoreFixture, SecretUsage,
    };

    const MODULE: &str = r#"
        (module
          (import "witchy" "secretstore_lookup"
            (func $lookup (param i32) (result externref)))
          (import "witchy" "crypto_reveal_len"
            (func $reveal (param externref) (result i32)))
          (import "witchy" "crypto.sign"
            (func $sign (param externref i32 i32)))
          (import "witchy" "crypto.public_key"
            (func $public_key (param externref i32)))
          (import "witchy" "fill_pending" (func $fill_pending (param i32)))
          (import "witchy" "print" (func $print (param i32 i32)))
          (memory (export "memory") 1)
          (data (i32.const 0) "\05\00\00\00token")
          (data (i32.const 32) "\07\00\00\00signing")
          (data (i32.const 64) "\07\00\00\00payload")
          (func (export "run") (local $secret externref) (local $length i32)
            i32.const 0
            call $lookup
            local.set $secret
            local.get $secret
            call $reveal
            local.set $length
            i32.const 256
            call $fill_pending
            i32.const 256
            local.get $length
            call $print
            i32.const 32
            call $lookup
            local.set $secret
            local.get $secret
            i32.const 64
            i32.const 512
            call $sign
            i32.const 512
            i32.const 9
            call $print
            local.get $secret
            i32.const 600
            call $public_key
            i32.const 600
            i32.const 6
            call $print))
    "#;
    let plan = FixturePlan {
        version: 1,
        console: Some(ConsoleFixture {
            script: vec![
                console_output_step("top-secret"),
                console_output_step("signature"),
                console_output_step("public"),
            ],
        }),
        secrets: Some(SecretStoreFixture {
            entries: std::collections::BTreeMap::from([
                (
                    "token".to_owned(),
                    SecretFixture {
                        hex: "746f702d736563726574".to_owned(),
                        usage: SecretUsage::Revealable,
                    },
                ),
                (
                    "signing".to_owned(),
                    SecretFixture {
                        hex: "11".repeat(32),
                        usage: SecretUsage::Signing,
                    },
                ),
            ]),
            script: vec![
                FixtureStep {
                    operation: "secretstore_lookup".to_owned(),
                    target: Some("token".to_owned()),
                    arguments: std::collections::BTreeMap::new(),
                    effective_rights: None,
                    outcome: FixtureOutcome::Return {
                        value: FixtureValue::String("Secret".to_owned()),
                    },
                    required: true,
                },
                FixtureStep {
                    operation: "crypto_reveal_len".to_owned(),
                    target: None,
                    arguments: std::collections::BTreeMap::new(),
                    effective_rights: None,
                    outcome: FixtureOutcome::Return {
                        value: FixtureValue::Map(std::collections::BTreeMap::from([
                            ("redacted".to_owned(), FixtureValue::Bool(true)),
                            ("length".to_owned(), FixtureValue::String("10".to_owned())),
                            (
                                "usage".to_owned(),
                                FixtureValue::String("revealable".to_owned()),
                            ),
                        ])),
                    },
                    required: true,
                },
                FixtureStep {
                    operation: "secretstore_lookup".to_owned(),
                    target: Some("signing".to_owned()),
                    arguments: std::collections::BTreeMap::new(),
                    effective_rights: None,
                    outcome: FixtureOutcome::Return {
                        value: FixtureValue::String("Secret".to_owned()),
                    },
                    required: true,
                },
                FixtureStep {
                    operation: "crypto.sign".to_owned(),
                    target: None,
                    arguments: std::collections::BTreeMap::from([(
                        "message".to_owned(),
                        FixtureValue::String("payload".to_owned()),
                    )]),
                    effective_rights: None,
                    outcome: FixtureOutcome::Return {
                        value: FixtureValue::String("signature".to_owned()),
                    },
                    required: true,
                },
                FixtureStep {
                    operation: "crypto.public_key".to_owned(),
                    target: None,
                    arguments: std::collections::BTreeMap::new(),
                    effective_rights: None,
                    outcome: FixtureOutcome::Return {
                        value: FixtureValue::String("public".to_owned()),
                    },
                    required: true,
                },
            ],
        }),
        ..FixturePlan::default()
    };
    let mut runtime = Runtime::batch().expect("runtime");
    let outcome = runtime
        .run_fixtures(MODULE, plan, 2)
        .expect("compiled fixture run");
    assert!(matches!(outcome.result, FixtureWasmResult::Passed));
    assert_eq!(outcome.output, vec!["top-secret", "signature", "public"]);
    let secret_events = outcome
        .transcript
        .events
        .iter()
        .filter(|event| event.family == FixtureFamily::SecretStore)
        .collect::<Vec<_>>();
    let secret_evidence = format!("{secret_events:?}");
    assert!(!secret_evidence.contains("top-secret"));
    assert!(!secret_evidence.contains("746f702d736563726574"));
    assert_eq!(secret_events.len(), 6);
}

#[cfg(feature = "test-fixtures")]
#[test]
fn compiled_exec_fixture_uses_allowlisted_script_without_spawning() {
    use witchy_testkit::{
        ConsoleFixture, ExecFixture, FilesystemEntry, FilesystemFixture, FixtureFamily,
        FixtureOutcome, FixturePlan, FixtureStep, FixtureValue,
    };

    const PAYLOAD: &str = "7\nouterr";
    const MODULE: &str = r#"
        (module
          (import "witchy" "mint_dir" (func $mint_dir (param i32) (result externref)))
          (import "witchy" "mint_exec" (func $mint_exec (result externref)))
          (import "witchy" "exec_run"
            (func $exec_run (param externref externref i32 i32 i32) (result i32)))
          (import "witchy" "fill_pending" (func $fill_pending (param i32)))
          (import "witchy" "print" (func $print (param i32 i32)))
          (memory (export "memory") 1)
          (data (i32.const 0) "\04\00\00\00tool")
          (data (i32.const 32) "\07\00\00\00--check")
          (data (i32.const 64) "\05\00\00\00input")
          (func (export "run")
            (local $dir externref)
            (local $exec externref)
            (local $length i32)
            i32.const 0
            call $mint_dir
            local.set $dir
            call $mint_exec
            local.set $exec
            local.get $exec
            local.get $dir
            i32.const 0
            i32.const 32
            i32.const 64
            call $exec_run
            local.set $length
            i32.const 256
            call $fill_pending
            i32.const 256
            local.get $length
            call $print))
    "#;
    let plan = FixturePlan {
        version: 1,
        console: Some(ConsoleFixture {
            script: vec![console_output_step(PAYLOAD)],
        }),
        filesystem: Some(FilesystemFixture {
            entries: std::collections::BTreeMap::from([(
                "tool".to_owned(),
                FilesystemEntry::File {
                    hex: "66697874757265".to_owned(),
                },
            )]),
            rights: vec!["Read".to_owned()],
            entry_policy: None,
            script: Vec::new(),
        }),
        exec: Some(ExecFixture {
            tools: vec!["tool".to_owned()],
            script: vec![FixtureStep {
                operation: "exec_run".to_owned(),
                target: Some("tool".to_owned()),
                arguments: std::collections::BTreeMap::from([
                    (
                        "args".to_owned(),
                        FixtureValue::List(vec![FixtureValue::String("--check".to_owned())]),
                    ),
                    (
                        "stdin".to_owned(),
                        FixtureValue::String("input".to_owned()),
                    ),
                ]),
                effective_rights: Some(vec!["exec:tool".to_owned(), "dir:Read".to_owned()]),
                outcome: FixtureOutcome::Return {
                    value: FixtureValue::Map(std::collections::BTreeMap::from([
                        (
                            "exit_code".to_owned(),
                            FixtureValue::String("7".to_owned()),
                        ),
                        ("stdout".to_owned(), FixtureValue::String("out".to_owned())),
                        ("stderr".to_owned(), FixtureValue::String("err".to_owned())),
                    ])),
                },
                required: true,
            }],
        }),
        ..FixturePlan::default()
    };
    let mut runtime = Runtime::batch().expect("runtime");
    let outcome = runtime
        .run_fixtures(MODULE, plan, 2)
        .expect("compiled fixture run");
    assert!(matches!(outcome.result, FixtureWasmResult::Passed));
    assert_eq!(outcome.output, vec![PAYLOAD]);
    assert_eq!(
        outcome
            .transcript
            .events
            .iter()
            .map(|event| (event.family, event.operation.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (FixtureFamily::Filesystem, "mint_dir"),
            (FixtureFamily::Exec, "mint_exec"),
            (FixtureFamily::Exec, "exec_run"),
            (FixtureFamily::Console, "print"),
        ]
    );
}

#[cfg(feature = "test-fixtures")]
fn console_output_step(text: &str) -> witchy_testkit::FixtureStep {
    use witchy_testkit::{FixtureOutcome, FixtureStep, FixtureValue};

    FixtureStep {
        operation: "print".to_owned(),
        target: None,
        arguments: std::collections::BTreeMap::from([(
            "text".to_owned(),
            FixtureValue::String(text.to_owned()),
        )]),
        effective_rights: Some(vec!["Write".to_owned()]),
        outcome: FixtureOutcome::Return {
            value: FixtureValue::Null,
        },
        required: true,
    }
}

#[test]
fn profiling_strategies_can_be_configured() {
    let empty = LayoutBundle::from_interner(&LayoutInterner::new(), [])
        .expect("empty canonical layout bundle");
    let valid = wasm_with_layout_payload(empty.canonical_bytes());

    assert_eq!(
        Runtime::profiling_strategy_from_name("perfmap"),
        Some(wasmtime::ProfilingStrategy::PerfMap)
    );
    assert_eq!(
        Runtime::profiling_strategy_from_name("jitdump"),
        Some(wasmtime::ProfilingStrategy::JitDump)
    );
    assert_eq!(
        Runtime::profiling_strategy_from_name("vtune"),
        Some(wasmtime::ProfilingStrategy::VTune)
    );
    assert_eq!(
        Runtime::profiling_strategy_from_name("none"),
        Some(wasmtime::ProfilingStrategy::None)
    );
    assert_eq!(
        Runtime::profiling_strategy_from_name("invalid_xyz"),
        None
    );

    for strategy in [
        wasmtime::ProfilingStrategy::None,
        wasmtime::ProfilingStrategy::PerfMap,
        wasmtime::ProfilingStrategy::JitDump,
    ] {
        let mut runtime = Runtime::batch_with_profiler(strategy)
            .expect("runtime initialization with profiling strategy");
        runtime
            .spawn(&valid, Capabilities::none(), 4)
            .expect("module instantiation with profiling strategy");
    }
}

use std::path::PathBuf;
