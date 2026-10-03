    use super::*;
    use std::borrow::Cow;
    use wasm_encoder::{
        CodeSection, CustomSection, EntityType, Function, FunctionSection, ImportSection,
        Instruction, Module as WasmModule, Section as _, StartSection, TypeSection, ValType,
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

    pub fn optimizer_policy_is_versioned_and_fails_closed() {
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

    pub fn admitted_module_builder_validates_without_binaryen_or_byte_rewriting() {
        let runtime = Runtime::batch().expect("runtime");
        let wasm = BARE.as_bytes();
        let module = build_admitted_module(&runtime.engine, wasm)
            .expect("the exact admitted-byte path performs structural validation");
        let mut runtime = runtime;
        let mut vm = runtime
            .instantiate_module(crate::runtime::WorkerModule::Unadmitted(module), Capabilities::none(), 4, None, &[])
            .expect("the exact admitted-byte path uses the normal capability instantiation");
        vm.run().expect("the exact admitted bytes execute without rewriting");
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

    fn external_identity(wasm: &[u8]) -> ExternalWasmIdentity {
        ExternalWasmIdentity {
            library_binding_digest: [7; 32],
            wasm_sha256: sha256(wasm),
            sidecar_sha256: [9; 32],
        }
    }

    fn wasm_with_external_import() -> Vec<u8> {
        let mut types = TypeSection::new();
        types.ty().function([], []);
        let mut imports = ImportSection::new();
        imports.import("other", "call", EntityType::Function(0));
        let mut module = WasmModule::new();
        module.section(&types);
        module.section(&imports);
        module.finish()
    }

    fn wasm_with_start_function() -> Vec<u8> {
        let mut types = TypeSection::new();
        types.ty().function([], []);
        let mut functions = FunctionSection::new();
        functions.function(0);
        let mut body = Function::new(Vec::new());
        body.instruction(&Instruction::End);
        let mut code = CodeSection::new();
        code.function(&body);
        let mut module = WasmModule::new();
        module.section(&types);
        module.section(&functions);
        module.section(&StartSection { function_index: 0 });
        module.section(&code);
        module.finish()
    }

    pub fn authenticated_external_library_rejects_tampering_imports_and_start() {
        let closed = WasmModule::new().finish();
        assert!(AuthenticatedExternalLibrary::from_verified_bytes(
            external_identity(&closed), closed.clone(),
        ).is_ok());

        let closed_identity = external_identity(&closed);
        let mut tampered = closed;
        tampered.push(0);
        assert!(AuthenticatedExternalLibrary::from_verified_bytes(
            closed_identity, tampered,
        ).unwrap_err().to_string().contains("differ"));

        let imported = wasm_with_external_import();
        assert!(AuthenticatedExternalLibrary::from_verified_bytes(
            external_identity(&imported), imported,
        ).unwrap_err().to_string().contains("imports are unsupported"));

        let started = wasm_with_start_function();
        assert!(AuthenticatedExternalLibrary::from_verified_bytes(
            external_identity(&started), started,
        ).unwrap_err().to_string().contains("start functions are unsupported"));
    }

    pub fn artifact_layout_metadata_is_checked_before_instantiation() {
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

    pub fn duplicate_artifact_layout_metadata_fails_closed() {
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

    pub fn host_imports_require_generated_layout_metadata_before_linking() {
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

    pub fn forged_par_map_count_is_bounded_before_host_allocation() {
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

    pub fn trapped_vm_is_terminal() {
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

    pub fn optimized_wasm_cache_envelope_rejects_corruption_and_swaps() {
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
    pub fn binaryen_requires_an_authenticated_bundle_and_runs_the_exact_recipe() {
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

    pub fn binaryen_pin_resolution_rejects_mismatch_partial_sets_and_cross_source_mixing() {
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

    #[cfg(any(
        all(target_arch = "aarch64", target_os = "linux"),
        all(target_arch = "x86_64", target_os = "linux"),
        all(target_arch = "aarch64", target_os = "macos"),
        all(target_arch = "x86_64", target_os = "macos"),
    ))]
    #[cfg(any(
        all(target_arch = "aarch64", target_os = "linux"),
        all(target_arch = "x86_64", target_os = "linux"),
        all(target_arch = "aarch64", target_os = "macos"),
        all(target_arch = "x86_64", target_os = "macos"),
    ))]
    const NULL_FILE_READ: &str = r#"
        (module
          (import "witchy" "file_read_len" (func $file_read_len (param externref) (result i32)))
          (func (export "run")
            (drop (call $file_read_len (ref.null extern)))))
    "#;

    const DIR_READ_FILE_INVALID_PATH: &str = r#"
        (module
          (import "witchy" "mint_dir" (func $mint_dir (param i32) (result externref)))
          (import "witchy" "dir_read_file_at"
            (func $dir_read_file (param externref i32 i32) (result externref)))
          (memory (export "memory") 1)
          (data (i32.const 0) "\ff")
          (func (export "run")
            (local $dir externref)
            i32.const 0
            call $mint_dir
            local.set $dir
            local.get $dir
            i32.const 0
            i32.const 1
            call $dir_read_file
            drop)
        )
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
    pub fn ungranted_capability_is_unreachable() {
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

    pub fn vm_with_dir_start_observes_only_the_delegated_dir() {
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
    pub fn vms_have_independent_memories() {
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

    pub fn memory_budget_is_enforced() {
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

    pub fn null_externrefs_are_rejected_by_every_capability_family() {
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

    pub fn dir_read_file_rejects_an_invalid_utf8_path() {
        let root = tempfile::tempdir().expect("temporary capability root");
        let caps = Capabilities {
            dir_root: Some(root.path().to_owned()),
            dir_read: true,
            ..Default::default()
        };
        let mut runtime = Runtime::new().expect("runtime");
        let mut invalid_path = runtime
            .spawn(DIR_READ_FILE_INVALID_PATH, caps, 4)
            .expect("invalid-path fixture links");
        let path_error = invalid_path.run().expect_err("invalid UTF-8 path must trap");
        let path_detail = format!("{path_error:?}");
        assert!(path_detail.contains("not valid UTF-8"), "{path_detail}");
    }

    pub fn read_only_direct_file_does_not_link_write() {
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
    pub fn runaway_vm_is_preempted() {
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

    pub fn byte_export_round_trips_non_utf8_and_enforces_output_limit() {
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

    pub fn read_wstr_list_huge_count_fails_closed_not_aborts() {
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

    const HEAP_OVERRUN: &str = r#"
        (module
          (import "witchy" "heap_register" (func $reg (param i32 i32)))
          (memory (export "memory") 1)
          (func (export "run")
            (call $reg (i32.const 16) (i32.const 32))
            (i32.store (i32.const 32) (i32.const 0))))
    "#;

    pub fn heap_check_overrun_redzone_traps() {
        let mut rt = Runtime::new().unwrap();
        let mut vm = rt.spawn(HEAP_OVERRUN, Capabilities::none(), 4).unwrap();
        let err = vm.run().map(|_| ()).unwrap_err();
        assert!(
            err.to_string().contains("HEAP CHECK"),
            "an overrun into the redzone must trap the sweep, got: {err}"
        );
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
        witchy_wir::wir_encode::try_encode_with_gc_source_map(&module, &[], &[])
            .map(|encoded| encoded.wasm)
            .expect("combined checked-UAF fixture must encode")
    }

    pub fn checked_uaf_double_free_still_traps() {
        let mut rt = Runtime::new().unwrap();
        let mut vm = rt.spawn(checked_uaf_module(true), Capabilities::none(), 4).unwrap();
        vm.run().expect_err("the zero RC quarantine marker must trap a repeated free");
    }

pub fn concrete_grants_have_one_complete_confinement_policy() {
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

pub fn empty_build_grants_do_not_widen_outer_authority() {
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

pub fn exec_child_paths_widen_only_the_read_only_outer_fence() {
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

pub fn empty_network_grant_remains_distinct_from_no_network_grant() {
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
pub fn compiled_secret_fixture_keeps_material_opaque_and_scripts_crypto() {
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

use std::path::PathBuf;
