//! Signed registry fixture shared by resolver and CLI precompiled-library tests.
use ed25519_dalek::{Signer, SigningKey};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use witchy_types::opt_callable_export::{
    CheckedCallableSignatureDigest, OptCallableContract, OptCallableExport,
    OptCallableExportSidecar, OptCallableSlot, canonical_checked_signature_bytes,
};
use witchy_types::runtime_type::{
    ModuleLoadIdentity, PackageCoordinate, PackageSource,
};

pub struct PrecompiledRegistryFixture {
    pub root: PathBuf,
}

impl Drop for PrecompiledRegistryFixture {
    fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.root); }
}

impl PrecompiledRegistryFixture {
    pub fn new() -> Self {
        let root = unique_dir("precompiled_registry_project");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("vendor/external_math/artifacts/src")).unwrap();
        std::fs::write(root.join("src/main.witchy"),
            "import external_math\n\nfn main(console: Console):\n    console.print(\"${external_math.map(5)}\")\n").unwrap();
        std::fs::write(root.join("witchy.toml"),
            "[rune]\nname=\"app\"\nversion=\"1.0.0\"\n[dependencies]\nexternal_math={version=\"1.0.0\",kind=\"precompiled\",module=\"src/math\"}\n").unwrap();

        let key = SigningKey::from_bytes(&[71; 32]);
        let rootpub: String = key.verifying_key().as_bytes().iter().map(|byte| format!("{byte:02x}")).collect();
        let owner = ModuleLoadIdentity::new(
            PackageCoordinate::new(PackageSource::Registry(rootpub.clone()), "acme/math", "1.0.0").unwrap(),
            ["src", "math"],
        ).unwrap();
        let wasm = scalar_map_wasm();
        let sidecar = signed_scalar_sidecar(owner);
        let wasm_digest = sha256_hex(&wasm);
        let sidecar_digest = sha256_hex(&sidecar);
        let canonical = format!(
            "v3\nlibrary|src/math|precompiled|{wasm_digest}|{}|{sidecar_digest}|{}",
            wasm.len(), sidecar.len(),
        );
        let artifact_digest = format!("sha256:{}", sha256_hex(canonical.as_bytes()));
        let artifact_manifest = serde_json::json!({
            "version": 3,
            "artifacts": {},
            "libraries": {"src/math": {
                "kind": "precompiled",
                "wasm_sha256": wasm_digest,
                "wasm_size": wasm.len(),
                "opt_callable_export_sha256": sidecar_digest,
                "opt_callable_export_size": sidecar.len(),
            }},
        }).to_string();
        std::fs::write(root.join("vendor/external_math/coven-artifact.json"), artifact_manifest).unwrap();
        std::fs::write(root.join("vendor/external_math/artifacts/src/math.wasm"), wasm).unwrap();
        std::fs::write(root.join("vendor/external_math/artifacts/src/math.opt"), sidecar).unwrap();

        Self::write_signed_record(&root, &artifact_digest);
        Self::write_lock(&root, "precompiled", "src/math", &artifact_digest);
        Self { root }
    }

    pub fn entry_claim(&self) -> ModuleLoadIdentity {
        ModuleLoadIdentity::new(
            PackageCoordinate::new(PackageSource::Workspace, "app", "1.0.0").unwrap(), ["src", "main"],
        ).unwrap()
    }

    pub fn replace_signed_artifact_manifest(&self, manifest: serde_json::Value, canonical: String) {
        let artifact_digest = format!("sha256:{}", sha256_hex(canonical.as_bytes()));
        std::fs::write(
            self.root.join("vendor/external_math/coven-artifact.json"), manifest.to_string(),
        ).unwrap();
        Self::write_signed_record(&self.root, &artifact_digest);
        Self::write_lock(&self.root, "precompiled", "src/math", &artifact_digest);
    }

    pub fn replace_lock_selection(&self, kind: &str, module: &str, artifact_digest: &str) {
        Self::write_lock(&self.root, kind, module, artifact_digest);
    }

    pub fn signed_artifact_digest(&self) -> String {
        serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(
            self.root.join("vendor/external_math/coven.json"),
        ).unwrap()).unwrap()["artifact_digest"].as_str().unwrap().to_owned()
    }

    pub fn add_spurious_lock_hash(&self) {
        let path = self.root.join("witchy.lock");
        let lock = std::fs::read_to_string(&path).unwrap();
        std::fs::write(path, format!("{lock}hash=\"sha256:{}\"\n", "f".repeat(64))).unwrap();
    }

    pub fn artifact_fields(&self) -> (String, usize, String, usize) {
        let wasm = std::fs::read(self.root.join("vendor/external_math/artifacts/src/math.wasm")).unwrap();
        let sidecar = std::fs::read(self.root.join("vendor/external_math/artifacts/src/math.opt")).unwrap();
        (sha256_hex(&wasm), wasm.len(), sha256_hex(&sidecar), sidecar.len())
    }

    fn write_signed_record(root: &Path, artifact_digest: &str) {
        let source_hash = format!("sha256:{}", "0".repeat(64));
        let payload = format!(
            "coven-v2\nname=acme/math\nversion=1.0.0\nstate=released\nhash={source_hash}\nrt=[]\nbuild=\nuploaded_by=\npromoted_by=\nfactor=\nprovenance=\nreleased_at=0\nartifact={artifact_digest}"
        );
        let key = SigningKey::from_bytes(&[71; 32]);
        let record = serde_json::json!({
            "name": "acme/math", "version": "1.0.0", "state": "released", "hash": source_hash,
            "runtime_footprint": [], "released_at": 0, "artifact_digest": artifact_digest,
            "sig": key.sign(payload.as_bytes()).to_bytes().iter().map(|byte| format!("{byte:02x}")).collect::<String>(),
        });
        std::fs::write(root.join("vendor/external_math/coven.json"), record.to_string()).unwrap();
    }

    fn write_lock(root: &Path, kind: &str, module: &str, artifact_digest: &str) {
        let key = SigningKey::from_bytes(&[71; 32]);
        let rootpub: String = key.verifying_key().as_bytes().iter().map(|byte| format!("{byte:02x}")).collect();
        std::fs::write(root.join("witchy.lock"), format!(
            "registry_snapshot_version=1\nregistry_rootpub=\"{rootpub}\"\n[[rune]]\nname=\"acme/math\"\nalias=\"external_math\"\nversion=\"1.0.0\"\nsource=\"coven\"\nkind=\"{kind}\"\nmodule=\"{module}\"\nartifact_digest=\"{artifact_digest}\"\nruntime_footprint=[]\n"
        )).unwrap();
    }
}

pub fn unique_dir(name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    // Absolute for the reason in `coven::temp_root`: a relative `TMPDIR`
    // would place registry fixtures inside the shared checkout.
    let root = std::env::temp_dir();
    let root = if root.is_absolute() { root } else { PathBuf::from("/tmp") };
    root.join(format!("witchy_{name}_{}_{}", std::process::id(), nanos))
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
