//! Native and browser host conformance to the compiler-owned WASM ABI catalog.
#![cfg(feature = "native")]


use wasm_encoder::{
    CodeSection, EntityType, ExportKind, ExportSection, Function, FunctionSection, ImportSection,
    Instruction, MemorySection, MemoryType, Module, TypeSection, ValType,
};
use witchy_runtime::runtime::{Capabilities, Runtime, SecretGrant};
use witchy_wir::wir_prelude::{
    abi_import_info, prelude, render_abi_import_catalog, AbiImportAuthority, AbiImportClass,
    PreludeImport, WasmTy, WITCHY_ABI_VERSION,
};

const CATALOG_BEGIN: &str = "<!-- BEGIN GENERATED WASM ABI IMPORTS -->";
const CATALOG_END: &str = "<!-- END GENERATED WASM ABI IMPORTS -->";

fn val_type(ty: WasmTy) -> ValType {
    match ty {
        WasmTy::I32 => ValType::I32,
        WasmTy::I64 => ValType::I64,
        WasmTy::F32 => ValType::F32,
        WasmTy::F64 => ValType::F64,
        WasmTy::ExternRef => ValType::EXTERNREF,
    }
}

/// Build a module that declares every supplied import with the compiler's exact
/// signature. Its `run` is empty: instantiation alone proves a host's names and
/// types agree without invoking authority or requiring valid handles.
fn importing_module(imports: &[PreludeImport]) -> Vec<u8> {
    let mut types = TypeSection::new();
    for import in imports {
        types.ty().function(
            import.params.iter().copied().map(val_type),
            import.results.iter().copied().map(val_type),
        );
    }
    types.ty().function([], []);

    let mut wasm_imports = ImportSection::new();
    for (i, import) in imports.iter().enumerate() {
        wasm_imports.import("witchy", &import.name, EntityType::Function(i as u32));
    }

    let mut functions = FunctionSection::new();
    functions.function(imports.len() as u32);

    let mut memories = MemorySection::new();
    memories.memory(MemoryType {
        minimum: 1,
        maximum: None,
        memory64: false,
        shared: false,
        page_size_log2: None,
    });

    let mut exports = ExportSection::new();
    exports.export("memory", ExportKind::Memory, 0);
    exports.export("run", ExportKind::Func, imports.len() as u32);

    let mut run = Function::new([]);
    run.instruction(&Instruction::End);
    let mut code = CodeSection::new();
    code.function(&run);

    let mut module = Module::new();
    module.section(&types);
    module.section(&wasm_imports);
    module.section(&functions);
    module.section(&memories);
    module.section(&exports);
    module.section(&code);
    module.finish()
}

fn maximal_capabilities() -> Capabilities {
    let root = std::env::temp_dir();
    Capabilities {
        print: true,
        print_int: true,
        console_read: true,
        clock: true,
        rand: true,
        env: true,
        env_allow: Some(Vec::new()),
        dir_root: Some(root.clone()),
        file_grants: vec![root.join("witchy-abi-catalog-file")],
        dir_read: true,
        dir_write: true,
        exec: true,
        exec_allow: Some(vec!["true".into()]),
        net_allow: Some(vec!["127.0.0.1:0".into()]),
        fetch_grants: vec![vec!["http://127.0.0.1:1".into()]],
        build_net_allow: Some(vec!["127.0.0.1:0".into()]),
        net_connect: true,
        net_listen: true,
        signing_key: Some([7; 32]),
        secrets: vec![SecretGrant::new("abi", vec![1, 2, 3])],
        build_out: Some(root.clone()),
        build_read_roots: vec![root],
        build_env: Some(Default::default()),
        ..Default::default()
    }
}

#[test]
fn native_host_links_every_catalog_import_with_the_declared_signature() {
    let (compiler_only, ordinary): (Vec<_>, Vec<_>) = prelude().imports.iter().cloned().partition(|import|
        abi_import_info(&import.name).expect("catalog metadata").class == AbiImportClass::CompilerSession);
    let wasm = importing_module(&ordinary);
    let mut runtime = Runtime::new().expect("native runtime");
    let _vm = runtime
        .spawn(wasm, maximal_capabilities(), 4)
        .expect("a maximal native host must satisfy the complete ordinary ABI");
    // Compiler-session imports are intentionally absent even with maximal
    // application grants. The admitted comptime witness checks their positive
    // signature/execution through the dedicated launcher.
    for import in compiler_only {
        let error = runtime.spawn(importing_module(std::slice::from_ref(&import)), maximal_capabilities(), 4)
            .err().expect("ordinary native launch must omit compiler-session imports");
        assert!(error.to_string().contains(&import.name), "{error}");
    }
}

#[test]
fn public_spec_contains_the_generated_complete_catalog() {
    let spec = include_str!("../../spec/wasm-abi.md");
    assert!(
        spec.contains(&format!("The current ABI version is **{WITCHY_ABI_VERSION}**.")),
        "the public spec must name the compiler-owned ABI version"
    );
    let (_, tail) = spec.split_once(CATALOG_BEGIN).expect("ABI spec catalog start marker");
    let (committed, _) = tail
        .split_once(CATALOG_END)
        .expect("ABI spec catalog end marker");
    assert_eq!(
        committed.trim(),
        render_abi_import_catalog().trim(),
        "regenerate with `cargo run -p witchy-wir --example abi_catalog`"
    );
}

#[test]
fn console_fixture_import_links_but_grants_no_session_or_memory_access() {
    let mut types = TypeSection::new();
    types.ty().function([
        ValType::EXTERNREF, ValType::I32, ValType::I32,
        ValType::I32, ValType::I32, ValType::I32,
    ], []);
    types.ty().function([], []);
    let mut imports = ImportSection::new();
    imports.import("witchy", "console_print_at", EntityType::Function(0));
    let mut functions = FunctionSection::new();
    functions.function(1);
    let mut exports = ExportSection::new();
    exports.export("run", ExportKind::Func, 1);
    // The default local is null. Deliberately supply invalid slices and no
    // memory: refusal must precede any guest-memory access or output.
    let mut run = Function::new([(1, ValType::EXTERNREF)]);
    run.instruction(&Instruction::LocalGet(0));
    for _ in 0..5 { run.instruction(&Instruction::I32Const(-1)); }
    run.instruction(&Instruction::Call(0));
    run.instruction(&Instruction::End);
    let mut code = CodeSection::new();
    code.function(&run);
    let mut module = Module::new();
    module.section(&types);
    module.section(&imports);
    module.section(&functions);
    module.section(&exports);
    module.section(&code);
    let wasm = module.finish();
    for caps in [Capabilities::none(), maximal_capabilities()] {
        let mut runtime = Runtime::new().expect("native runtime");
        let mut vm = runtime.spawn(&wasm, caps, 4).expect("fixture ABI is linkable");
        let error = vm.run().expect_err("linking does not create fixture authority");
        let message = error.root_cause().to_string();
        assert!(message == "fixture Console token is null"
            || message == "Console fixture support is unavailable in this runtime", "{message}");
        assert!(vm.output().is_empty());
    }
}

fn authorities_for(name: &str) -> Vec<AbiImportAuthority> {
    abi_import_info(name)
        .unwrap_or_else(|| panic!("missing ABI metadata for `{name}`"))
        .authorities
        .to_vec()
}

#[test]
fn capability_imports_name_their_authority_family() {
    for import in &prelude().imports {
        let info = abi_import_info(&import.name)
            .unwrap_or_else(|| panic!("missing ABI metadata for `{}`", import.name));
        if info.class == AbiImportClass::CapabilityAuthority {
            assert!(
                !info.authorities.is_empty(),
                "capability import `{}` must name its concrete authority family",
                import.name
            );
        } else {
            assert!(
                info.authorities.is_empty(),
                "non-capability import `{}` must not claim authority {:?}",
                import.name,
                info.authorities
            );
        }
    }
}

#[test]
fn precompiled_runner_authority_rows_are_cataloged() {
    use AbiImportAuthority as A;

    assert_eq!(authorities_for("mint_dir"), vec![A::DirGrant]);
    assert_eq!(authorities_for("dir_open"), vec![A::DirRead]);
    assert_eq!(authorities_for("dir_create"), vec![A::DirWrite]);
    assert_eq!(authorities_for("mint_file"), vec![A::FileGrant]);
    assert_eq!(authorities_for("file_read_len"), vec![A::FileAuthority]);
    assert_eq!(authorities_for("mint_net"), vec![A::NetGrant]);
    assert_eq!(authorities_for("net_connect_pinned"), vec![A::NetConnect]);
    assert_eq!(authorities_for("net_listen"), vec![A::NetListen]);
    assert_eq!(authorities_for("serve_pool"), vec![A::NetListen]);
    assert_eq!(authorities_for("net_listen_tls"), vec![A::NetListen, A::Secret]);
    assert_eq!(authorities_for("mint_secret"), vec![A::Secret]);
    assert_eq!(authorities_for("crypto.sign"), vec![A::Secret]);
    assert_eq!(authorities_for("now_monotonic"), vec![A::Clock]);
}
