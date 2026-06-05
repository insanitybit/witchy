//! coven — the registry, here as a local directory-backed implementation.
//!
//! This is a faithful local model of the hosted coven registry: it is
//! content-addressed and immutable (a version's bytes never change), it
//! recomputes every rune's capability footprint from source (server-side
//! enforcement — metadata is never trusted, T7), and it enforces the two-phase
//! publish lifecycle (§8.1): `publish` lands a version *staged* and not
//! resolvable; a separate, second-factor `promote` is required to *release* it.
//!
//! Layout: `<root>/<namespace>/<name>/<version>/{coven.json, rune/...}`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::footprint::Footprint;
use super::manifest::Manifest;
use super::semver::{Req, Version};
use super::store::RuneSource;
use super::{PmResult, PmError, err};

/// Lifecycle state of a published version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    /// Uploaded but NOT resolvable. The default landing state of `publish`.
    Staged,
    /// Promoted (out-of-band, second factor). Resolvable.
    Released,
    /// Excluded from new resolutions; existing locks still resolve it.
    Yanked,
}

/// The registry's record for one published version (`coven.json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    pub name: String,
    pub version: String,
    pub state: State,
    pub hash: String,
    #[serde(default)]
    pub runtime_footprint: Vec<String>,
    #[serde(default)]
    pub build_footprint: Vec<String>,
    #[serde(default)]
    pub determinism: String,
    pub uploaded_by: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub promoted_by: Option<String>,
    /// The kind of second factor used to promote (e.g. "webauthn", "totp").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub second_factor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<String>,
    /// Ed25519 signature (hex) over [`Record::signing_payload`], by the registry
    /// root key. Re-signed on every state transition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sig: Option<String>,
}

impl Record {
    pub fn footprint(&self) -> Footprint {
        Footprint {
            runtime: self.runtime_footprint.iter().cloned().collect(),
            build: self.build_footprint.iter().cloned().collect(),
        }
    }

    /// The canonical, signed view of a record: every security-relevant field
    /// except the signature itself. Tampering with any of these breaks the
    /// signature.
    fn signing_payload(&self) -> String {
        let state = match self.state {
            State::Staged => "staged",
            State::Released => "released",
            State::Yanked => "yanked",
        };
        format!(
            "coven-v1\nname={}\nversion={}\nstate={}\nhash={}\nrt={}\nbuild={}\nuploaded_by={}\npromoted_by={}\nfactor={}",
            self.name,
            self.version,
            state,
            self.hash,
            self.runtime_footprint.join(","),
            self.build_footprint.join(","),
            self.uploaded_by,
            self.promoted_by.as_deref().unwrap_or(""),
            self.second_factor.as_deref().unwrap_or(""),
        )
    }
}

const META: &str = "coven.json";

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub struct Registry {
    root: PathBuf,
}

impl Registry {
    pub fn new(root: PathBuf) -> Registry {
        Registry { root }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn key(&self) -> PmResult<super::keys::RegistryKey> {
        super::keys::RegistryKey::load_or_create(&self.root)
    }

    /// The fingerprint of this registry's root signing key — the value a client
    /// pins (TOFU) so the key cannot be silently swapped.
    pub fn root_fingerprint(&self) -> PmResult<String> {
        Ok(self.key()?.fingerprint())
    }

    /// Verify a record's signature against the registry's published root key.
    /// A missing or invalid signature is a hard failure (tampered metadata).
    pub fn verify_record(&self, record: &Record) -> PmResult<()> {
        let pubkey = super::keys::read_public(&self.root)?;
        let Some(sig) = &record.sig else {
            return err(format!(
                "{}@{} has no signature — refusing to trust unsigned metadata",
                record.name, record.version
            ));
        };
        if super::keys::verify(&pubkey, record.signing_payload().as_bytes(), sig) {
            Ok(())
        } else {
            err(format!(
                "signature verification FAILED for {}@{} — registry metadata was tampered with",
                record.name, record.version
            ))
        }
    }

    fn version_dir(&self, name: &str, version: &str) -> PathBuf {
        // name is `ns/n`; map directly to nested dirs.
        self.root.join(name).join(version)
    }

    fn meta_path(&self, name: &str, version: &str) -> PathBuf {
        self.version_dir(name, version).join(META)
    }

    fn rune_dir(&self, name: &str, version: &str) -> PathBuf {
        self.version_dir(name, version).join("rune")
    }

    /// Publish a rune. Recomputes the footprint from source (server-side
    /// enforcement) and, if the manifest declares a `[capabilities]` contract,
    /// rejects any mismatch. Lands the version **staged** — not resolvable until
    /// promoted. Immutable: re-publishing an existing version is an error.
    pub fn publish(
        &self,
        src: &RuneSource,
        manifest: &Manifest,
        uploaded_by: &str,
    ) -> PmResult<Record> {
        let name = &manifest.rune.name;
        let version = &manifest.rune.version;
        if self.meta_path(name, version).exists() {
            return err(format!(
                "{name}@{version} already published — versions are immutable, bump the version"
            ));
        }
        Version::parse(version)?;

        let hash = src.hash();
        // Server-side recomputation: the footprint is computed here from source,
        // never taken on faith from the uploader.
        let computed = super::footprint::of_modules(&src.modules())?;
        verify_declared(&computed, manifest)?;

        let dir = self.rune_dir(name, version);
        std::fs::create_dir_all(&dir)?;
        src.write_to(&dir)?;

        let mut record = Record {
            name: name.clone(),
            version: version.clone(),
            state: State::Staged,
            hash: hash.clone(),
            runtime_footprint: computed.runtime.iter().cloned().collect(),
            build_footprint: computed.build.iter().cloned().collect(),
            determinism: computed.determinism().to_string(),
            uploaded_by: uploaded_by.to_string(),
            promoted_by: None,
            second_factor: None,
            // Provenance always binds bytes -> uploader -> time; a declared source
            // repo is an optional stronger anchor (SLSA-style) when present.
            provenance: Some({
                let src = manifest
                    .rune
                    .source
                    .as_deref()
                    .map(|s| format!("|source={s}"))
                    .unwrap_or_default();
                format!("uploader={uploaded_by}|at={}|hash={hash}{src}", now_unix())
            }),
            sig: None,
        };
        self.write_record(&mut record)?;
        Ok(record)
    }

    /// Promote a staged version to released — the "double confirmation". Requires
    /// a non-empty second factor (the out-of-band, 2FA-able event), records the
    /// promoter (separation of duties — a distinct identity from the uploader),
    /// and reports whether the promoter differs from the uploader.
    pub fn promote(
        &self,
        name: &str,
        version: &str,
        promoter: &str,
        second_factor: &str,
    ) -> PmResult<Promotion> {
        let mut record = self.record(name, version)?;
        match record.state {
            State::Released => {
                return err(format!("{name}@{version} is already released"));
            }
            State::Yanked => {
                return err(format!("{name}@{version} is yanked; cannot promote"));
            }
            State::Staged => {}
        }
        if second_factor.trim().is_empty() {
            return err(format!(
                "promotion of {name}@{version} requires a second factor (the out-of-band 2FA event) — refusing to release"
            ));
        }
        if promoter.trim().is_empty() {
            return err("promotion requires an authenticated promoter identity");
        }

        // What does releasing this version newly expose vs. the currently
        // released version? The human vouches for this delta.
        let prior = self.latest_released(name).map(|r| r.footprint());
        let delta = match &prior {
            Some(p) => record.footprint().widening_over(p),
            None => record.footprint().widening_over(&Footprint::default()),
        };

        let distinct = promoter != record.uploaded_by;
        record.state = State::Released;
        record.promoted_by = Some(promoter.to_string());
        record.second_factor = Some(second_factor.to_string());
        self.write_record(&mut record)?;

        Ok(Promotion {
            record,
            footprint_delta: delta,
            separation_of_duties: distinct,
        })
    }

    pub fn yank(&self, name: &str, version: &str) -> PmResult<()> {
        let mut record = self.record(name, version)?;
        record.state = State::Yanked;
        self.write_record(&mut record)
    }

    /// Read a version's metadata record.
    pub fn record(&self, name: &str, version: &str) -> PmResult<Record> {
        let path = self.meta_path(name, version);
        let text = std::fs::read_to_string(&path)
            .map_err(|_| PmError(format!("{name}@{version} not found in registry")))?;
        serde_json::from_str(&text).map_err(|e| PmError(format!("corrupt {META} for {name}: {e}")))
    }

    /// Sign and persist a record. Signing happens here so every state
    /// transition (publish/promote/yank) re-signs the canonical payload.
    fn write_record(&self, record: &mut Record) -> PmResult<()> {
        let key = self.key()?;
        record.sig = Some(key.sign(record.signing_payload().as_bytes()));
        let path = self.meta_path(&record.name, &record.version);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(record)
            .map_err(|e| PmError(format!("serialize record: {e}")))?;
        std::fs::write(&path, text)?;
        Ok(())
    }

    /// All published versions of a rune (any state), sorted ascending.
    pub fn versions(&self, name: &str) -> Vec<Record> {
        let dir = self.root.join(name);
        let mut out = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for e in entries.flatten() {
                if let Some(v) = e.file_name().to_str()
                    && let Ok(rec) = self.record(name, v)
                {
                    out.push(rec);
                }
            }
        }
        out.sort_by(|a, b| {
            Version::parse(&a.version)
                .ok()
                .cmp(&Version::parse(&b.version).ok())
        });
        out
    }

    pub fn latest_released(&self, name: &str) -> Option<Record> {
        self.versions(name)
            .into_iter()
            .rfind(|r| r.state == State::Released)
    }

    /// The best released version satisfying `req`. With `include_staged`, staged
    /// versions are eligible too (used only by the publisher's own `--include-staged`
    /// testing path, never normal resolution).
    pub fn best_match(&self, name: &str, req: &Req, include_staged: bool) -> Option<Record> {
        let mut candidates: Vec<Record> = self
            .versions(name)
            .into_iter()
            .filter(|r| match r.state {
                State::Released => true,
                State::Staged => include_staged,
                State::Yanked => false,
            })
            .filter(|r| Version::parse(&r.version).map(|v| req.matches(&v)).unwrap_or(false))
            .collect();
        candidates.sort_by(|a, b| {
            Version::parse(&a.version)
                .ok()
                .cmp(&Version::parse(&b.version).ok())
        });
        candidates.pop()
    }

    /// Fetch a version's source, verifying the content hash against its record.
    pub fn fetch(&self, name: &str, version: &str) -> PmResult<RuneSource> {
        let record = self.record(name, version)?;
        // The record itself must be validly signed — otherwise an attacker who
        // swapped the source could just rewrite `hash` to match. Signature first,
        // then confirm the source matches the signed hash.
        self.verify_record(&record)?;
        let src = RuneSource::read_dir(&self.rune_dir(name, version))?;
        if src.hash() != record.hash {
            return err(format!(
                "integrity failure: {name}@{version} source hashes to {} but record says {} (tampered registry?)",
                src.hash(),
                record.hash
            ));
        }
        Ok(src)
    }
}

/// The result of a successful promotion.
pub struct Promotion {
    pub record: Record,
    /// Capability kinds this release newly exposes vs. the prior released version.
    pub footprint_delta: super::footprint::Widening,
    /// Whether the promoter is a distinct identity from the uploader.
    pub separation_of_duties: bool,
}

/// Enforce that a declared `[capabilities]` contract matches the recomputed
/// footprint (T7). An under-declaration (the rune demands more than it admits)
/// is the dangerous case and is always rejected.
fn verify_declared(computed: &Footprint, manifest: &Manifest) -> PmResult<()> {
    super::footprint::check_declared(
        computed,
        &manifest.capabilities.runtime,
        &manifest.capabilities.build,
    )
    .map_err(|gap| {
        super::PmError(format!(
            "declared [capabilities] does not cover what the source actually demands ({gap}). \
             Update the manifest's declared capabilities to match — coven recomputes and will not \
             publish an under-declared rune."
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_registry() -> (Registry, PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "witchy-reg-{}-{}",
            std::process::id(),
            fastish_nonce()
        ));
        let _ = std::fs::remove_dir_all(&root);
        (Registry::new(root.clone()), root)
    }

    fn fastish_nonce() -> u128 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static CTR: AtomicU64 = AtomicU64::new(0);
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        n.wrapping_mul(1000) + CTR.fetch_add(1, Ordering::Relaxed) as u128
    }

    fn rune(name: &str, version: &str, body: &str) -> (RuneSource, Manifest) {
        let toml = format!("[rune]\nname = \"{name}\"\nversion = \"{version}\"\n");
        let manifest = Manifest::parse(&toml).unwrap();
        let module = name.rsplit('/').next().unwrap();
        let mut files = vec![
            ("witchy.toml".to_string(), toml.into_bytes()),
            (format!("src/{module}.witchy"), body.as_bytes().to_vec()),
        ];
        files.sort_by(|a, b| a.0.cmp(&b.0));
        (RuneSource { files }, manifest)
    }

    #[test]
    fn publish_stages_not_resolvable() {
        let (reg, root) = tmp_registry();
        let (src, m) = rune("acme/json", "1.0.0", "fn parse(s: String) -> String { s }");
        let rec = reg.publish(&src, &m, "ci-bot").unwrap();
        assert_eq!(rec.state, State::Staged);
        // Not resolvable while staged.
        assert!(reg.best_match("acme/json", &Req::Any, false).is_none());
        // But visible to the publisher's own --include-staged path.
        assert!(reg.best_match("acme/json", &Req::Any, true).is_some());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn promote_requires_second_factor() {
        let (reg, root) = tmp_registry();
        let (src, m) = rune("acme/json", "1.0.0", "fn parse(s: String) -> String { s }");
        reg.publish(&src, &m, "ci-bot").unwrap();
        // No second factor -> refused.
        assert!(reg.promote("acme/json", "1.0.0", "alice", "").is_err());
        // With second factor -> released and resolvable.
        let p = reg.promote("acme/json", "1.0.0", "alice", "webauthn").unwrap();
        assert_eq!(p.record.state, State::Released);
        assert!(p.separation_of_duties, "alice != ci-bot");
        assert!(reg.best_match("acme/json", &Req::Any, false).is_some());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn immutable_versions() {
        let (reg, root) = tmp_registry();
        let (src, m) = rune("acme/json", "1.0.0", "fn parse(s: String) -> String { s }");
        reg.publish(&src, &m, "ci-bot").unwrap();
        assert!(reg.publish(&src, &m, "ci-bot").is_err(), "republish must fail");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn server_recomputes_footprint() {
        let (reg, root) = tmp_registry();
        let (src, m) = rune("acme/http", "1.0.0", "fn get(net: Net, url: String) -> String { url }");
        let rec = reg.publish(&src, &m, "ci-bot").unwrap();
        assert!(rec.runtime_footprint.contains(&"Net".to_string()));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn under_declared_contract_is_rejected() {
        let (reg, root) = tmp_registry();
        // Declares nothing-but-build is empty, runtime declared empty but source uses Net.
        let toml = "[rune]\nname = \"acme/sneaky\"\nversion = \"1.0.0\"\n\n[capabilities]\nruntime = [\"Console\"]\n";
        let m = Manifest::parse(toml).unwrap();
        let files = vec![
            ("witchy.toml".to_string(), toml.as_bytes().to_vec()),
            ("src/sneaky.witchy".to_string(), b"fn x(net: Net) -> Int { 0 }".to_vec()),
        ];
        let mut files = files;
        files.sort_by(|a, b| a.0.cmp(&b.0));
        let src = RuneSource { files };
        let res = reg.publish(&src, &m, "ci-bot");
        assert!(res.is_err(), "under-declared Net must be rejected");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn fetch_verifies_hash() {
        let (reg, root) = tmp_registry();
        let (src, m) = rune("acme/json", "1.0.0", "fn parse(s: String) -> String { s }");
        let rec = reg.publish(&src, &m, "ci-bot").unwrap();
        let fetched = reg.fetch("acme/json", "1.0.0").unwrap();
        assert_eq!(fetched.hash(), rec.hash);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn records_are_signed() {
        let (reg, root) = tmp_registry();
        let (src, m) = rune("acme/json", "1.0.0", "fn parse(s: String) -> String { s }");
        let rec = reg.publish(&src, &m, "ci-bot").unwrap();
        assert!(rec.sig.is_some(), "publish must sign the record");
        reg.verify_record(&rec).expect("freshly signed record must verify");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn tampered_record_fails_verification() {
        let (reg, root) = tmp_registry();
        let (src, m) = rune("acme/json", "1.0.0", "fn parse(s: String) -> String { s }");
        reg.publish(&src, &m, "ci-bot").unwrap();
        reg.promote("acme/json", "1.0.0", "alice", "webauthn").unwrap();

        // An attacker with write access edits a *signed* field of the metadata.
        // (Source bytes untouched, so the content hash alone would NOT catch it —
        // only the signature does.)
        let path = reg.meta_path("acme/json", "1.0.0");
        let json = std::fs::read_to_string(&path).unwrap().replace("ci-bot", "attacker");
        std::fs::write(&path, json).unwrap();

        let tampered = reg.record("acme/json", "1.0.0").unwrap();
        assert!(reg.verify_record(&tampered).is_err(), "tampered record must fail");
        // ...and fetch refuses it.
        assert!(reg.fetch("acme/json", "1.0.0").is_err());
        let _ = std::fs::remove_dir_all(&root);
    }
}
