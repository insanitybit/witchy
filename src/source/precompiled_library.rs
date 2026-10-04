//! Authenticated input for a separately compiled library.
//!
//! This is intentionally not an ordinary `--dep` source file. The package
//! manager selects the library and verifies where it came from; this boundary
//! checks the Wasm against its export-signature bundle, and the bundle against
//! the claimed owner, before a decoded export can reach the linker.

use sha2::{Digest, Sha256};
use witchy_types::opt_callable_export::{OptCallableExport, OptCallableExportSidecar};
use witchy_types::precompiled_declarations::PrecompiledDeclarationLibrary;
use witchy_types::runtime_type::ModuleLoadIdentity;

#[derive(Clone, Debug)]
pub(crate) struct PrecompiledLibrary {
    owner: ModuleLoadIdentity,
    wasm: Vec<u8>,
    export_signature_bundle: Vec<u8>,
    wasm_sha256: [u8; 32],
    sidecar_sha256: [u8; 32],
    exports: OptCallableExportSidecar,
}

impl PrecompiledLibrary {
    /// A library the caller selected. The compiler holds no signed manifest
    /// for it, so the digests are the bytes' own; `verify` still checks the
    /// bundle names `owner` and every export it declares.
    pub(crate) fn from_bytes(owner: ModuleLoadIdentity, wasm: Vec<u8>, export_signature_bundle: &[u8]) -> Result<Self, String> {
        let hex = |bytes: &[u8]| -> String { Sha256::digest(bytes).iter().map(|byte| format!("{byte:02x}")).collect() };
        let wasm_sha256 = hex(&wasm);
        let sidecar_sha256 = hex(export_signature_bundle);
        Self::verify(owner, wasm, export_signature_bundle, &wasm_sha256, &sidecar_sha256)
    }

    /// Authenticate the two artifact files against expected digests. No raw
    /// Wasm section, package-local source input, import-site declaration, or
    /// file name is authority here.
    pub(crate) fn verify(
        owner: ModuleLoadIdentity,
        wasm: Vec<u8>,
        export_signature_bundle: &[u8],
        manifest_wasm_sha256: &str,
        manifest_export_signatures_sha256: &str,
    ) -> Result<Self, String> {
        let manifest_wasm_sha256 = parse_sha256(manifest_wasm_sha256, "Wasm")?;
        let manifest_sidecar_sha256 = parse_sha256(manifest_export_signatures_sha256, "export-signature bundle")?;
        let wasm_sha256: [u8; 32] = Sha256::digest(&wasm).into();
        if wasm_sha256 != manifest_wasm_sha256 {
            return Err("precompiled library Wasm does not match signed manifest digest".into());
        }
        let sidecar_sha256: [u8; 32] = Sha256::digest(export_signature_bundle).into();
        if sidecar_sha256 != manifest_sidecar_sha256 {
            return Err("precompiled library export-signature bundle does not match signed manifest digest".into());
        }
        let exports = OptCallableExportSidecar::decode(export_signature_bundle)
            .map_err(|error| format!("invalid authenticated export-signature bundle: {error}"))?;
        if exports.module() != &owner {
            return Err("precompiled library export-signature bundle identity does not match authenticated module owner".into());
        }
        // Reconstruct every signed header now. This rejects a stale digest or
        // malformed declaration before the bundle can reach a linker. A later
        // external-declaration environment consumes these same checked facts.
        for (name, export) in exports.exports() {
            export.checked_scalar_signature(name)
                .map_err(|error| format!("precompiled export `{name}` is not a checked signed declaration: {error}"))?;
        }
        Ok(Self {
            owner,
            wasm,
            export_signature_bundle: export_signature_bundle.to_vec(),
            wasm_sha256,
            sidecar_sha256,
            exports,
        })
    }

    pub(crate) fn owner(&self) -> &ModuleLoadIdentity { &self.owner }
    pub(crate) fn wasm(&self) -> &[u8] { &self.wasm }
    pub(crate) fn export(&self, name: &str) -> Option<&OptCallableExport> { self.exports.export(name) }

    /// The only production handoff into source linking.  It repeats the
    /// byte/commitment comparison at the declaration-carrier boundary rather
    /// than trusting this wrapper's cached fields.
    pub(crate) fn declaration_library(
        &self,
        alias: String,
    ) -> Result<PrecompiledDeclarationLibrary, String> {
        PrecompiledDeclarationLibrary::from_authenticated_artifacts(
            alias,
            self.owner.clone(),
            &self.wasm,
            &self.export_signature_bundle,
            self.wasm_sha256,
            self.sidecar_sha256,
        )
    }

    /// Pair the same verified bytes with the declaration environment's exact
    /// binding identity for Wasmtime. Callers should create the declaration
    /// and runtime carriers from this one method result path, never assemble
    /// a module/name lookup themselves.
    pub(crate) fn runtime_library(
        &self,
        alias: String,
    ) -> Result<witchy_runtime::runtime::AuthenticatedExternalLibrary, String> {
        let declaration = self.declaration_library(alias)?;
        witchy_runtime::runtime::AuthenticatedExternalLibrary::from_verified_bytes(
            witchy_wir::canonical_safewir::ExternalWasmIdentity {
                library_binding_digest: declaration.binding_digest(),
                wasm_sha256: declaration.wasm_sha256(),
                sidecar_sha256: declaration.sidecar_sha256(),
            },
            self.wasm.clone(),
        )
        .map_err(|error| error.to_string())
    }
}

fn parse_sha256(value: &str, artifact: &str) -> Result<[u8; 32], String> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!("signed manifest {artifact} digest is not a lowercase SHA-256 hex string"));
    }
    let mut digest = [0; 32];
    for (index, chunk) in value.as_bytes().chunks_exact(2).enumerate() {
        let nibble = |byte: u8| match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            _ => None,
        };
        digest[index] = (nibble(chunk[0]).ok_or_else(|| format!("signed manifest {artifact} digest is not lowercase hex"))? << 4)
            | nibble(chunk[1]).ok_or_else(|| format!("signed manifest {artifact} digest is not lowercase hex"))?;
    }
    Ok(digest)
}
