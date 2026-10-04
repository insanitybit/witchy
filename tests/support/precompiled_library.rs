//! A precompiled library as a package manager hands it to the compiler:
//! `lib/math.wasm` and its export-signature bundle `lib/math.opt`, owned by a
//! registry package, next to an application that imports it. Shared by the
//! crate's and the CLI's tests, each of which uses part of it.
#![allow(dead_code)]
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::PathBuf;
use witchy_types::opt_callable_export::{
    CheckedCallableSignatureDigest, OptCallableContract, OptCallableExport,
    OptCallableExportSidecar, OptCallableSlot, canonical_checked_signature_bytes,
};
use witchy_types::runtime_type::{
    ModuleLoadIdentity, PackageCoordinate, PackageSource,
};

/// The registry identity the library's bundle names.
pub const LIBRARY_ROOT: &str = "acme-root";

pub struct PrecompiledLibraryFixture {
    pub root: PathBuf,
}

impl Drop for PrecompiledLibraryFixture {
    fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.root); }
}

impl PrecompiledLibraryFixture {
    pub fn new() -> Self {
        let root = unique_dir("precompiled_library_project");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("lib")).unwrap();
        std::fs::write(root.join("src/main.witchy"),
            "import external_math\n\nfn main(console: Console):\n    console.print(\"${external_math.map(5)}\")\n").unwrap();
        std::fs::write(root.join("witchy.toml"),
            "[rune]\nname=\"app\"\nversion=\"1.0.0\"\n[dependencies]\n").unwrap();
        std::fs::write(root.join("lib/math.wasm"), scalar_map_wasm()).unwrap();
        std::fs::write(root.join("lib/math.opt"), signed_scalar_sidecar(library_owner())).unwrap();
        Self { root }
    }

    pub fn entry_claim(&self) -> ModuleLoadIdentity {
        ModuleLoadIdentity::new(
            PackageCoordinate::new(PackageSource::Workspace, "app", "1.0.0").unwrap(), ["src", "main"],
        ).unwrap()
    }

    /// The caller's owner assertion for the library alias.
    pub fn owner_claims(&self) -> HashMap<String, ModuleLoadIdentity> {
        HashMap::from([("external_math".to_owned(), library_owner())])
    }

    /// The caller's selection: alias to the library's Wasm.
    pub fn libraries(&self) -> HashMap<String, PathBuf> {
        HashMap::from([("external_math".to_owned(), self.root.join("lib/math.wasm"))])
    }
}

/// The package that published the library.
pub fn library_owner() -> ModuleLoadIdentity {
    ModuleLoadIdentity::new(
        PackageCoordinate::new(PackageSource::Registry(LIBRARY_ROOT.into()), "acme/math", "1.0.0").unwrap(),
        ["src", "math"],
    ).unwrap()
}

pub fn unique_dir(name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("witchy_{name}_{}_{}", std::process::id(), nanos))
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn signed_scalar_sidecar(owner: ModuleLoadIdentity) -> Vec<u8> {
    let typed = witchy_types::typeck::annotate_checked(
        witchy_syntax::parser::parse_module(
            "mode opt\npub fn map(value: Int) -> Int:\n    value\n",
        )
        .unwrap(),
    )
    .unwrap();
    let signature = canonical_checked_signature_bytes(&typed, "map").unwrap();
    let export = OptCallableExport::new(
        "pub fn map(value: Int) -> Int".into(),
        CheckedCallableSignatureDigest::from_canonical_checked_signature(&signature).unwrap(),
        OptCallableContract {
            parameters: vec![OptCallableSlot::Value],
            result: OptCallableSlot::Value,
        },
    )
    .unwrap();
    OptCallableExportSidecar::new(owner, [("map".into(), export)])
        .unwrap()
        .canonical_bytes()
        .unwrap()
}

pub fn scalar_map_wasm() -> Vec<u8> {
    use wasm_encoder::{
        CodeSection, ExportKind, ExportSection, Function, FunctionSection,
        Instruction, Module, TypeSection, ValType,
    };
    let mut module = Module::new();
    let mut types = TypeSection::new();
    types.ty().function([ValType::I64], [ValType::I64]);
    module.section(&types);
    let mut functions = FunctionSection::new();
    functions.function(0);
    module.section(&functions);
    let mut exports = ExportSection::new();
    exports.export("map", ExportKind::Func, 0);
    module.section(&exports);
    let mut code = CodeSection::new();
    let mut body = Function::new(Vec::new());
    body.instruction(&Instruction::LocalGet(0));
    body.instruction(&Instruction::I64Const(10));
    body.instruction(&Instruction::I64Add);
    body.instruction(&Instruction::End);
    code.function(&body);
    module.section(&code);
    module.finish()
}
