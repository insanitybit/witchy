//! `witchy.toml` — the rune manifest.
//!
//! Declares the rune's identity, its *declared* capability contract (verified by
//! recomputation, never trusted — see [`super::footprint`]), its dependencies
//! (each naming its registry explicitly — no implicit fallback), and the
//! build-time capability grants the consuming project hands to specific deps.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::{PmResult, err};

pub const MANIFEST_NAME: &str = "witchy.toml";
pub const DEFAULT_REGISTRY: &str = "coven";

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Manifest {
    pub rune: RuneMeta,
    #[serde(default)]
    pub capabilities: DeclaredCaps,
    #[serde(default)]
    pub dependencies: BTreeMap<String, Dep>,
    #[serde(default)]
    pub build: BuildSection,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RuneMeta {
    /// `namespace/name`, e.g. `acme/http-client`.
    pub name: String,
    pub version: String,
    /// Provenance anchor: where the source canonically lives.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct DeclaredCaps {
    #[serde(default)]
    pub runtime: Vec<String>,
    #[serde(default)]
    pub build: Vec<String>,
}

/// A dependency value: either a bare version string or a detailed table.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum Dep {
    Version(String),
    Detailed(DepDetail),
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct DepDetail {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// A local path dependency (for development). Hashed + footprinted like any
    /// other; never trusted to skip verification.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registry: Option<String>,
}

impl Dep {
    pub fn version_req(&self) -> Option<&str> {
        match self {
            Dep::Version(v) => Some(v),
            Dep::Detailed(d) => d.version.as_deref(),
        }
    }
    pub fn path(&self) -> Option<&str> {
        match self {
            Dep::Version(_) => None,
            Dep::Detailed(d) => d.path.as_deref(),
        }
    }
    pub fn registry(&self) -> &str {
        match self {
            Dep::Version(_) => DEFAULT_REGISTRY,
            Dep::Detailed(d) => d.registry.as_deref().unwrap_or(DEFAULT_REGISTRY),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct BuildSection {
    /// Per-rune build-time capability grants: `[build.grants."ns/name"]`.
    #[serde(default)]
    pub grants: BTreeMap<String, Grant>,
}

/// The attenuated build-time authority the consumer grants to one dependency's
/// build step. Absent kinds are denied.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct Grant {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub read: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exec: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub net: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<String>,
}

impl Grant {
    /// The build capability *kinds* this grant authorizes.
    pub fn granted_kinds(&self) -> std::collections::BTreeSet<String> {
        let mut s = std::collections::BTreeSet::new();
        // BuildOut is always implicitly granted (confined output sandbox).
        s.insert("BuildOut".to_string());
        if !self.read.is_empty() {
            s.insert("BuildRead".to_string());
        }
        if !self.exec.is_empty() {
            s.insert("BuildExec".to_string());
        }
        if !self.net.is_empty() {
            s.insert("BuildNet".to_string());
        }
        if !self.env.is_empty() {
            s.insert("BuildEnv".to_string());
        }
        s
    }
}

impl Manifest {
    pub fn load(dir: &Path) -> PmResult<Manifest> {
        let path = dir.join(MANIFEST_NAME);
        let text = std::fs::read_to_string(&path)
            .map_err(|e| super::PmError(format!("cannot read {}: {e}", path.display())))?;
        Manifest::parse(&text)
    }

    pub fn parse(text: &str) -> PmResult<Manifest> {
        let m: Manifest =
            toml::from_str(text).map_err(|e| super::PmError(format!("invalid {MANIFEST_NAME}: {e}")))?;
        m.validate()?;
        Ok(m)
    }

    fn validate(&self) -> PmResult<()> {
        if self.rune.name.trim().is_empty() {
            return err("[rune].name must not be empty");
        }
        super::semver::Version::parse(&self.rune.version)?;
        Ok(())
    }

    pub fn save(&self, dir: &Path) -> PmResult<()> {
        let text =
            toml::to_string_pretty(self).map_err(|e| super::PmError(format!("serialize manifest: {e}")))?;
        std::fs::write(dir.join(MANIFEST_NAME), text)?;
        Ok(())
    }

    /// The import/module name this rune exposes: the last path segment of its
    /// namespaced name (`acme/http-client` -> `http-client`). Witchy module
    /// names are simple identifiers; the namespace is for registry identity.
    #[allow(dead_code)]
    pub fn module_name(&self) -> &str {
        self.rune.name.rsplit('/').next().unwrap_or(&self.rune.name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_manifest() {
        let m = Manifest::parse(
            r#"
            [rune]
            name = "acme/json"
            version = "1.2.0"
        "#,
        )
        .unwrap();
        assert_eq!(m.rune.name, "acme/json");
        assert_eq!(m.module_name(), "json");
        assert!(m.dependencies.is_empty());
    }

    #[test]
    fn parses_dep_shorthand_and_detailed() {
        let m = Manifest::parse(
            r#"
            [rune]
            name = "app"
            version = "0.1.0"

            [dependencies]
            "acme/json" = "^1.0.0"
            "acme/url" = { version = "2.0.0", registry = "coven" }
            "local/util" = { path = "../util" }
        "#,
        )
        .unwrap();
        assert_eq!(m.dependencies["acme/json"].version_req(), Some("^1.0.0"));
        assert_eq!(m.dependencies["acme/url"].registry(), "coven");
        assert_eq!(m.dependencies["local/util"].path(), Some("../util"));
    }

    #[test]
    fn parses_build_grants() {
        let m = Manifest::parse(
            r#"
            [rune]
            name = "app"
            version = "0.1.0"

            [build.grants."acme/protobuf-gen"]
            read = ["proto/"]
            exec = ["protoc"]
        "#,
        )
        .unwrap();
        let g = &m.build.grants["acme/protobuf-gen"];
        assert!(g.granted_kinds().contains("BuildExec"));
        assert!(g.granted_kinds().contains("BuildRead"));
        assert!(g.granted_kinds().contains("BuildOut"));
        assert!(!g.granted_kinds().contains("BuildNet"));
    }

    #[test]
    fn roundtrips() {
        let src = "[rune]\nname = \"acme/json\"\nversion = \"1.0.0\"\n";
        let m = Manifest::parse(src).unwrap();
        let out = toml::to_string_pretty(&m).unwrap();
        let m2 = Manifest::parse(&out).unwrap();
        assert_eq!(m.rune.name, m2.rune.name);
        assert_eq!(m.rune.version, m2.rune.version);
    }
}
