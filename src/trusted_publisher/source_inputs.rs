//! Untrusted, closed compilation inputs. None of these fields is an admission
//! receipt. Only fresh production checking and exact Wasm comparison restore one.
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceFile {
    pub path: String,
    /// None records an observed absence, including a bundled-source fallback.
    pub contents: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceDependency {
    pub alias: String,
    pub path: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceInputs {
    pub version: u32,
    pub compiler_version: String,
    pub launcher_digest: [u8; 32],
    pub bundled_digest: [u8; 32],
    pub host_abi: u32,
    pub canonical_abi: u32,
    pub opt_schema: u32,
    pub opt_mask: u32,
    pub comptime_seed_version: u32,
    pub entry: String,
    pub dependencies: Vec<SourceDependency>,
    pub files: Vec<SourceFile>,
    /// The manifest whose target table a trusted-publisher image binds.
    /// Portable Wasm binds nothing from a manifest and may omit it.
    pub target_manifest: Option<String>,
}

/// Domain bytes standing in for the launcher prefix of portable Wasm. A
/// trusted image digests its real launcher instead, so inputs sealed for one
/// format never validate in the other.
pub const PORTABLE_WASM_DOMAIN: &[u8] = b"witchy.portable-wasm.v1";

const VERSION: u32 = 2;

fn bundled_digest() -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    hash.update(b"witchy.packaged-bundled-inputs.v1\0");
    for names in [witchy_syntax::linker::STD_MODULES, witchy_syntax::linker::PLAYGROUND_MODULES] {
        for name in names {
            hash.update((name.len() as u64).to_le_bytes());
            hash.update(name.as_bytes());
            let source = witchy_syntax::linker::bundled_source(name).expect("bundled module registry");
            hash.update((source.len() as u64).to_le_bytes());
            hash.update(source.as_bytes());
        }
    }
    hash.finalize().into()
}

pub fn validate_path(path: &str) -> Result<(), String> {
    if path.is_empty() || path.contains(['\\', '\0', ':'])
        || path.split('/').any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(format!("invalid closed package path `{path}`"));
    }
    Ok(())
}

impl SourceInputs {
    /// Untrusted census only. The registry replay verifier hashes these raw
    /// files against the signed, locked source digest before assigning owners.
    pub fn registry_source_paths(&self, root: &str) -> Result<Vec<String>, String> {
        validate_path(root)?;
        let prefix = format!("{root}/src/");
        let mut paths = self.files.iter().filter(|file| file.contents.is_some()
            && file.path.starts_with(&prefix) && file.path.ends_with(".witchy"))
            .map(|file| file.path.clone()).collect::<Vec<_>>();
        paths.sort();
        if paths.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err("duplicate registry source in closed package".into());
        }
        Ok(paths)
    }

    pub fn captured(entry: String, dependencies: Vec<SourceDependency>, files: Vec<SourceFile>, target_manifest: Option<String>) -> Self {
        let options = witchy_syntax::opt::active_schema_key();
        Self {
            version: VERSION,
            compiler_version: env!("CARGO_PKG_VERSION").into(),
            launcher_digest: [0; 32],
            bundled_digest: bundled_digest(),
            host_abi: super::HOST_ABI,
            canonical_abi: witchy_wir::wir_prelude::WITCHY_ABI_VERSION,
            opt_schema: options.version(), opt_mask: options.enabled_mask(),
            comptime_seed_version: 1,
            entry, dependencies, files, target_manifest,
        }
    }

    /// Compatibility checks run before any lazy optimization initialization or
    /// source checking. The launcher prefix is the validated envelope's prefix,
    /// never the whole executable (which includes this input payload).
    fn validate(&self, launcher: &[u8]) -> Result<(), String> {
        if self.version != VERSION || self.compiler_version != env!("CARGO_PKG_VERSION")
            || self.launcher_digest != super::digest(launcher)
            || self.bundled_digest != bundled_digest()
            || self.host_abi != super::HOST_ABI
            || self.canonical_abi != witchy_wir::wir_prelude::WITCHY_ABI_VERSION
            || self.opt_schema != witchy_syntax::opt::OPT_SCHEMA_VERSION
            || self.opt_mask & !((1u32 << witchy_syntax::opt::Opt::ALL.len()) - 1) != 0
            || self.comptime_seed_version != 1
        {
            return Err("packaged source compiler/stdlib/ABI/options compatibility mismatch".into());
        }
        validate_path(&self.entry)?;
        if let Some(manifest) = &self.target_manifest { validate_path(manifest)?; }
        let mut paths = BTreeSet::new();
        for file in &self.files {
            validate_path(&file.path)?;
            if !paths.insert(&file.path) { return Err("duplicate closed package file".into()); }
        }
        let mut aliases = BTreeSet::new();
        for dependency in &self.dependencies {
            validate_path(&dependency.path)?;
            if dependency.alias.is_empty() || !aliases.insert(&dependency.alias) {
                return Err("duplicate or empty closed dependency alias".into());
            }
        }
        for required in std::iter::once(&self.entry).chain(&self.target_manifest) {
            if !self.files.iter().any(|file| &file.path == required && file.contents.is_some()) {
                return Err(format!("closed package is missing `{required}`"));
            }
        }
        Ok(())
    }

    /// A trusted-publisher image binds its launch from the target manifest.
    pub fn validate_trusted(&self, launcher: &[u8]) -> Result<(), String> {
        self.validate(launcher)?;
        if self.target_manifest.is_none() {
            return Err("trusted-publisher closed package has no target manifest".into());
        }
        Ok(())
    }

    pub fn validate_portable(&self) -> Result<(), String> {
        self.validate(PORTABLE_WASM_DOMAIN)
    }

    pub fn configure_options(&self) -> Result<(), String> {
        let mut specification = String::from("none");
        for (index, option) in witchy_syntax::opt::Opt::ALL.iter().enumerate() {
            if self.opt_mask & (1 << index) != 0 {
                specification.push(',');
                specification.push_str(option.name());
            }
        }
        witchy_syntax::opt::configure(&specification)
    }
}
