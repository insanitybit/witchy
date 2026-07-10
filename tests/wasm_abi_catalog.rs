#![cfg(feature = "native")]

use std::path::Path;
use std::process::Command;

use wasm_encoder::{
    CodeSection, EntityType, ExportKind, ExportSection, Function, FunctionSection, ImportSection,
    Instruction, MemorySection, MemoryType, Module, TypeSection, ValType,
};
use witchy_runtime::runtime::{Capabilities, Runtime, SecretGrant};
use witchy_wir::wir_prelude::{
    abi_import_info, prelude, render_abi_import_catalog, PreludeImport, WasmTy,
    WITCHY_ABI_VERSION,
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
        clock: true,
        rand: true,
        env: true,
        // BuildEnv shares this allow-list path in the native linker.
        env_allow: Some(Vec::new()),
        dir_root: Some(root.clone()),
        file_grants: vec![root.join("witchy-abi-catalog-file")],
        dir_read: true,
        dir_write: true,
        exec: true,
        exec_allow: Some(vec!["true".into()]),
        net_allow: Some(vec!["127.0.0.1:0".into()]),
        build_net_allow: Some(vec!["127.0.0.1:0".into()]),
        net_connect: true,
        net_listen: true,
        signing_key: Some([7; 32]),
        secrets: vec![SecretGrant::new("abi", vec![1, 2, 3])],
        build_out: Some(root.clone()),
        build_read_roots: vec![root],
        ..Default::default()
    }
}

#[test]
fn native_host_links_every_catalog_import_with_the_declared_signature() {
    let wasm = importing_module(&prelude().imports);
    let mut runtime = Runtime::new().expect("native runtime");
    let _vm = runtime
        .spawn(wasm, maximal_capabilities(), 4)
        .expect("a maximal native host must satisfy the complete compiler ABI");
}

#[test]
fn public_spec_contains_the_generated_complete_catalog() {
    let spec = include_str!("../spec/wasm-abi.md");
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

fn node_available() -> bool {
    Command::new("node")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[test]
fn browser_host_implements_exactly_the_catalogued_browser_surface() {
    if !node_available() {
        eprintln!("skipping: `node` is not available on PATH");
        return;
    }

    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let wasm_path = std::env::temp_dir().join(format!(
        "witchy-abi-catalog-{}-{nonce}.wasm",
        std::process::id()
    ));
    std::fs::write(&wasm_path, importing_module(&[])).expect("write empty ABI probe");

    let mut expected = prelude()
        .imports
        .iter()
        .filter(|import| abi_import_info(&import.name).is_some_and(|info| info.browser))
        .map(|import| import.name.as_str())
        .collect::<Vec<_>>();
    expected.sort_unstable();

    let out = Command::new("node")
        .arg(manifest.join("web/witchy-runtime/import-catalog.test.mjs"))
        .arg(&wasm_path)
        .env("WITCHY_EXPECTED_IMPORTS", expected.join("\n"))
        .env("WITCHY_EXPECTED_ABI_VERSION", WITCHY_ABI_VERSION.to_string())
        .current_dir(manifest)
        .output()
        .expect("spawn browser ABI probe");
    let _ = std::fs::remove_file(wasm_path);

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "browser ABI probe failed:\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    assert!(
        stdout.contains("WITCHY-IMPORT-CATALOG OK"),
        "missing browser ABI success marker: {stdout}"
    );
}
