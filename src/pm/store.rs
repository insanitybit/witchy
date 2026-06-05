//! Content-addressed rune source + the local store.
//!
//! A rune's identity is the sha256 of its *canonicalized* source tree (sorted
//! relative paths, LF-normalized bytes, no timestamps). Versions are immutable;
//! the same bytes always hash the same, and the store is append-only — which is
//! what makes lockfile pins tamper-evident and builds reproducible.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::{PmResult, err};

/// A rune's source as an ordered set of (relative-path, bytes) entries.
#[derive(Debug, Clone)]
pub struct RuneSource {
    /// Sorted by path; LF-normalized. Always includes `witchy.toml` plus the
    /// `src/**/*.witchy` modules.
    pub files: Vec<(String, Vec<u8>)>,
}

impl RuneSource {
    /// Read a rune directory: its `witchy.toml` and every `src/**/*.witchy`.
    pub fn read_dir(dir: &Path) -> PmResult<RuneSource> {
        let mut files = Vec::new();
        let manifest = dir.join(super::manifest::MANIFEST_NAME);
        if !manifest.exists() {
            return err(format!("no {} in {}", super::manifest::MANIFEST_NAME, dir.display()));
        }
        files.push((
            super::manifest::MANIFEST_NAME.to_string(),
            normalize(&std::fs::read(&manifest)?),
        ));
        let src = dir.join("src");
        if src.is_dir() {
            collect_witchy(&src, &src, &mut files)?;
        }
        files.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(RuneSource { files })
    }

    /// The content hash, e.g. `sha256:9f86d081...`.
    pub fn hash(&self) -> String {
        let mut h = Sha256::new();
        for (path, bytes) in &self.files {
            // Length-prefix each field so no concatenation ambiguity is possible.
            h.update((path.len() as u64).to_le_bytes());
            h.update(path.as_bytes());
            h.update((bytes.len() as u64).to_le_bytes());
            h.update(bytes);
        }
        let digest = h.finalize();
        let mut out = String::with_capacity(7 + digest.len() * 2);
        out.push_str("sha256:");
        for b in digest.as_ref() as &[u8] {
            use std::fmt::Write;
            let _ = write!(out, "{b:02x}");
        }
        out
    }

    /// Materialize the source into `dir` (creating `src/` as needed).
    pub fn write_to(&self, dir: &Path) -> PmResult<()> {
        for (rel, bytes) in &self.files {
            let path = dir.join(rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, bytes)?;
        }
        Ok(())
    }

    /// Parse the rune's own `witchy.toml` out of the source tree.
    pub fn manifest(&self) -> PmResult<super::manifest::Manifest> {
        for (rel, bytes) in &self.files {
            if rel == super::manifest::MANIFEST_NAME {
                return super::manifest::Manifest::parse(&String::from_utf8_lossy(bytes));
            }
        }
        err("rune source has no witchy.toml")
    }

    /// The `.witchy` modules as (module-name, source-text) pairs, ready to feed
    /// the parser/linker. The module name is the file stem.
    pub fn modules(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for (rel, bytes) in &self.files {
            if let Some(stem) = rel.strip_prefix("src/").and_then(|r| r.strip_suffix(".witchy")) {
                // Flatten nested module paths to their stem (file name).
                let name = stem.rsplit('/').next().unwrap_or(stem).to_string();
                out.push((name, String::from_utf8_lossy(bytes).into_owned()));
            }
        }
        out
    }
}

fn collect_witchy(root: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) -> PmResult<()> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            collect_witchy(root, &path, out)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("witchy") {
            let rel = path
                .strip_prefix(root.parent().unwrap_or(root))
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            out.push((rel, normalize(&std::fs::read(&path)?)));
        }
    }
    Ok(())
}

fn normalize(bytes: &[u8]) -> Vec<u8> {
    // LF-normalize so CRLF checkouts hash identically.
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\r' && i + 1 < bytes.len() && bytes[i + 1] == b'\n' {
            out.push(b'\n');
            i += 2;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    out
}

/// The append-only content-addressed store under `<root>/<hash>/`.
pub struct Store {
    root: PathBuf,
}

impl Store {
    pub fn new(root: PathBuf) -> Store {
        Store { root }
    }

    fn dir_for(&self, hash: &str) -> PathBuf {
        self.root.join(hash.replace(':', "-"))
    }

    pub fn has(&self, hash: &str) -> bool {
        self.dir_for(hash).join(super::manifest::MANIFEST_NAME).exists()
    }

    /// Place a rune source in the store, returning its hash. Idempotent: storing
    /// the same bytes twice is a no-op (append-only, immutable).
    pub fn put(&self, src: &RuneSource) -> PmResult<String> {
        let hash = src.hash();
        if !self.has(&hash) {
            let dir = self.dir_for(&hash);
            std::fs::create_dir_all(&dir)?;
            src.write_to(&dir)?;
        }
        Ok(hash)
    }

    /// Load a rune source back out of the store, verifying its hash matches.
    pub fn get(&self, hash: &str) -> PmResult<RuneSource> {
        if !self.has(hash) {
            return err(format!("hash {hash} not in store"));
        }
        let src = RuneSource::read_dir(&self.dir_for(hash))?;
        let actual = src.hash();
        if actual != hash {
            return err(format!(
                "store integrity failure: {hash} reads back as {actual} (tampered?)"
            ));
        }
        Ok(src)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(files: &[(&str, &str)]) -> RuneSource {
        let mut files: Vec<(String, Vec<u8>)> = files
            .iter()
            .map(|(p, b)| (p.to_string(), b.as_bytes().to_vec()))
            .collect();
        files.sort_by(|a, b| a.0.cmp(&b.0));
        RuneSource { files }
    }

    #[test]
    fn hash_is_stable_and_order_independent() {
        let a = src(&[("witchy.toml", "x"), ("src/json.witchy", "fn f(){}")]);
        let b = src(&[("src/json.witchy", "fn f(){}"), ("witchy.toml", "x")]);
        assert_eq!(a.hash(), b.hash());
        assert!(a.hash().starts_with("sha256:"));
    }

    #[test]
    fn hash_changes_with_content() {
        let a = src(&[("witchy.toml", "x")]);
        let b = src(&[("witchy.toml", "y")]);
        assert_ne!(a.hash(), b.hash());
    }

    #[test]
    fn crlf_normalizes() {
        assert_eq!(normalize(b"a\r\nb"), b"a\nb");
        assert_eq!(normalize(b"a\nb"), b"a\nb");
    }

    #[test]
    fn store_roundtrip_and_integrity() {
        let tmp = std::env::temp_dir().join(format!("witchy-store-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let store = Store::new(tmp.clone());
        let s = src(&[("witchy.toml", "[rune]\nname=\"a\"\nversion=\"1.0.0\"\n")]);
        let h = store.put(&s).unwrap();
        assert!(store.has(&h));
        let back = store.get(&h).unwrap();
        assert_eq!(back.hash(), h);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn module_extraction() {
        let s = src(&[
            ("witchy.toml", "x"),
            ("src/json.witchy", "fn parse(){}"),
            ("src/internal/util.witchy", "fn helper(){}"),
        ]);
        let mods = s.modules();
        assert!(mods.iter().any(|(n, _)| n == "json"));
        assert!(mods.iter().any(|(n, _)| n == "util"));
    }
}
