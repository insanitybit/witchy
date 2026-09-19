//! Authenticated declarations for separately compiled scalar libraries.
//!
//! This is deliberately an AST environment rather than source text supplied
//! by an importer.  Every declaration comes from a sidecar whose checked
//! signature was verified first; the only parsed text is that signed header.

use std::collections::BTreeMap;

use sha2::{Digest, Sha256};
use witchy_syntax::ast::Module;

use crate::opt_callable_export::{OptCallableExportSidecar, OptCallableExportError};
use crate::runtime_type::ModuleLoadIdentity;

const MAX_LIBRARIES: usize = 64;
const MAX_ALIAS_BYTES: usize = 128;

/// One authenticated separately compiled library made visible to a checked
/// source link.  Its fields are private: a source-link caller cannot assert
/// digest triples or insert a decoded sidecar directly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrecompiledDeclarationLibrary {
    alias: String,
    owner: ModuleLoadIdentity,
    wasm_sha256: [u8; 32],
    sidecar_sha256: [u8; 32],
    sidecar: OptCallableExportSidecar,
    binding_digest: [u8; 32],
}

impl PrecompiledDeclarationLibrary {
    /// Construct this carrier only from the actual two artifact byte streams
    /// and the commitments extracted from an already verified signed
    /// manifest.  The hashes are recomputed here, before the sidecar is
    /// decoded, so no caller can pair a benign declaration bundle with a
    /// different Wasm instance by merely supplying matching-looking values.
    pub fn from_authenticated_artifacts(
        alias: String,
        owner: ModuleLoadIdentity,
        wasm: &[u8],
        sidecar_bundle: &[u8],
        manifest_wasm_sha256: [u8; 32],
        manifest_sidecar_sha256: [u8; 32],
    ) -> Result<Self, String> {
        if alias.is_empty() || alias.len() > MAX_ALIAS_BYTES || !is_identifier(&alias) {
            return Err("precompiled library alias must be a bounded identifier".into());
        }
        let wasm_sha256: [u8; 32] = Sha256::digest(wasm).into();
        if wasm_sha256 != manifest_wasm_sha256 {
            return Err("precompiled Wasm differs from signed manifest commitment".into());
        }
        let sidecar_sha256: [u8; 32] = Sha256::digest(sidecar_bundle).into();
        if sidecar_sha256 != manifest_sidecar_sha256 {
            return Err("precompiled signature bundle differs from signed manifest commitment".into());
        }
        let sidecar = OptCallableExportSidecar::decode(sidecar_bundle)
            .map_err(render_export_error)?;
        if sidecar.module() != &owner {
            return Err("precompiled declaration sidecar identity differs from authenticated owner".into());
        }
        // This walks every export and proves that each header is a sealed
        // scalar declaration matching its signed canonical signature. It
        // therefore rejects references, managed values, capabilities,
        // generics, and callback contracts before a linker sees a declaration.
        for (name, export) in sidecar.exports() {
            let declaration = export.checked_scalar_declaration(name)
                .map_err(render_export_error)?;
            if !declaration.public {
                return Err(format!("precompiled export `{name}` is not public"));
            }
        }
        let sidecar_bytes = sidecar.canonical_bytes().map_err(render_export_error)?;
        let mut hasher = Sha256::new();
        hasher.update(b"witchy.precompiled-declaration-library.v1\0");
        hasher.update(&(alias.len() as u32).to_le_bytes());
        hasher.update(alias.as_bytes());
        hasher.update(&wasm_sha256);
        hasher.update(&sidecar_sha256);
        hasher.update(&(sidecar_bytes.len() as u32).to_le_bytes());
        hasher.update(&sidecar_bytes);
        Ok(Self {
            alias,
            owner,
            wasm_sha256,
            sidecar_sha256,
            sidecar,
            binding_digest: hasher.finalize().into(),
        })
    }

    pub fn alias(&self) -> &str { &self.alias }
    pub fn owner(&self) -> &ModuleLoadIdentity { &self.owner }
    pub fn wasm_sha256(&self) -> [u8; 32] { self.wasm_sha256 }
    pub fn sidecar_sha256(&self) -> [u8; 32] { self.sidecar_sha256 }
    pub fn binding_digest(&self) -> [u8; 32] { self.binding_digest }
    pub fn exports(&self) -> impl Iterator<Item = (&str, &crate::opt_callable_export::OptCallableExport)> {
        self.sidecar.exports()
    }

    /// Linker-qualified function spelling for an exported declaration.
    pub fn linked_export_name(&self, export: &str) -> String {
        format!("{}.{}", self.alias, export)
    }

    /// Build the one linked-module input for this library. Signed headers are
    /// never rendered back into source. Each body is the sealed scalar literal
    /// body already checked against the signed declaration envelope.
    pub fn module(&self) -> Result<Module, String> {
        let mut items = Vec::with_capacity(self.sidecar.exports().count());
        for (name, export) in self.sidecar.exports() {
            items.push(witchy_syntax::ast::Item::Function(
                export.checked_scalar_declaration(name).map_err(render_export_error)?,
            ));
        }
        Ok(Module {
            items,
            item_lines: vec![0; self.sidecar.exports().count()],
            ..Module::empty_opt()
        })
    }
}

/// The only carrier from a verified package loader into source linking.
/// Lowering receives this exact retained environment through
/// `CanonicalCheckedSource`; it must never rebuild it from an import-site
/// declaration or a Wasm custom section.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PrecompiledDeclarationEnvironment {
    libraries: BTreeMap<String, PrecompiledDeclarationLibrary>,
}

impl PrecompiledDeclarationEnvironment {
    pub fn new(
        libraries: impl IntoIterator<Item = PrecompiledDeclarationLibrary>,
    ) -> Result<Self, String> {
        let mut entries = BTreeMap::new();
        for library in libraries {
            if entries.len() == MAX_LIBRARIES {
                return Err("too many precompiled libraries in one source link".into());
            }
            if entries.insert(library.alias.clone(), library).is_some() {
                return Err("duplicate precompiled library alias".into());
            }
        }
        Ok(Self { libraries: entries })
    }

    pub fn is_empty(&self) -> bool { self.libraries.is_empty() }
    pub fn library(&self, alias: &str) -> Option<&PrecompiledDeclarationLibrary> { self.libraries.get(alias) }
    pub fn libraries(&self) -> impl Iterator<Item = &PrecompiledDeclarationLibrary> { self.libraries.values() }

    pub fn library_for_linked_export(
        &self,
        linked_name: &str,
    ) -> Option<(&PrecompiledDeclarationLibrary, &str)> {
        self.libraries.values().find_map(|library| {
            library.exports().find_map(|(export, _)| {
                (library.linked_export_name(export) == linked_name).then_some((library, export))
            })
        })
    }

    pub fn modules(&self) -> Result<Vec<(String, Module)>, String> {
        self.libraries.values().map(|library| Ok((library.alias.clone(), library.module()?))).collect()
    }

    /// Stable input to checked-source identity.  This binds the loader's alias
    /// choice, sidecar identity, Wasm digest, and all signed declaration facts.
    pub fn digest(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(b"witchy.precompiled-declaration-environment.v1\0");
        hasher.update(&(self.libraries.len() as u32).to_le_bytes());
        for library in self.libraries.values() {
            hasher.update(&(library.alias.len() as u32).to_le_bytes());
            hasher.update(library.alias.as_bytes());
            hasher.update(&library.binding_digest);
        }
        hasher.finalize().into()
    }
}

fn render_export_error(error: OptCallableExportError) -> String {
    format!("invalid authenticated precompiled declaration: {error}")
}

fn is_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    matches!(chars.next(), Some('a'..='z' | '_'))
        && chars.all(|character| character.is_ascii_alphanumeric() || character == '_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::opt_callable_export::{
        CheckedCallableSignatureDigest, OptCallableContract, OptCallableExport,
        OptCallableExportSidecar, OptCallableSlot, canonical_checked_signature_bytes,
    };
    use crate::runtime_type::{PackageCoordinate, PackageSource};

    fn identity(name: &str) -> ModuleLoadIdentity {
        ModuleLoadIdentity::new(
            PackageCoordinate::new(
                PackageSource::Registry("https://registry.example".into()),
                name,
                "1.0.0",
            )
            .unwrap(),
            ["lib", "math"],
        )
        .unwrap()
    }

    fn signed_scalar_sidecar(owner: ModuleLoadIdentity) -> Vec<u8> {
        let typed = crate::typeck::annotate_checked(
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

    fn digest(bytes: &[u8]) -> [u8; 32] { Sha256::digest(bytes).into() }

    #[test]
    fn authenticated_artifacts_reject_tampered_wasm_sidecar_and_owner() {
        let owner = identity("acme/math");
        let wasm = b"verified wasm bytes".to_vec();
        let sidecar = signed_scalar_sidecar(owner.clone());
        let library = PrecompiledDeclarationLibrary::from_authenticated_artifacts(
            "math".into(), owner.clone(), &wasm, &sidecar, digest(&wasm), digest(&sidecar),
        )
        .expect("verified bytes construct the opaque declaration library");
        assert_eq!(library.linked_export_name("map"), "math.map");

        let mut tampered_wasm = wasm.clone();
        tampered_wasm.push(0);
        assert!(PrecompiledDeclarationLibrary::from_authenticated_artifacts(
            "math".into(), owner.clone(), &tampered_wasm, &sidecar, digest(&wasm), digest(&sidecar),
        ).unwrap_err().contains("Wasm differs"));

        let mut tampered_sidecar = sidecar.clone();
        *tampered_sidecar.last_mut().unwrap() ^= 1;
        assert!(PrecompiledDeclarationLibrary::from_authenticated_artifacts(
            "math".into(), owner.clone(), &wasm, &tampered_sidecar, digest(&wasm), digest(&sidecar),
        ).unwrap_err().contains("signature bundle differs"));

        let other_owner = identity("acme/other");
        assert!(PrecompiledDeclarationLibrary::from_authenticated_artifacts(
            "math".into(), other_owner, &wasm, &sidecar, digest(&wasm), digest(&sidecar),
        ).unwrap_err().contains("identity differs"));
    }
}
