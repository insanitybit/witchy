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
            "coven-v1\nname={}\nversion={}\nstate={}\nhash={}\nrt={}\nbuild={}\nuploaded_by={}\npromoted_by={}\nfactor={}\nprovenance={}",
            self.name,
            self.version,
            state,
            self.hash,
            self.runtime_footprint.join(","),
            self.build_footprint.join(","),
            self.uploaded_by,
            self.promoted_by.as_deref().unwrap_or(""),
            self.second_factor.as_deref().unwrap_or(""),
            self.provenance.as_deref().unwrap_or(""),
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

/// A path-component segment: starts with `[a-z0-9_]`, then `[a-z0-9_.-]`, and
/// never contains `..`. `allow_dot` lets the version-suffix-bearing name segment
/// include `.` (the namespace segment may not).
fn valid_segment(s: &str, allow_dot: bool) -> bool {
    if s.is_empty() || s.contains("..") {
        return false;
    }
    let bytes = s.as_bytes();
    let first = bytes[0];
    if !(first.is_ascii_lowercase() || first.is_ascii_digit() || first == b'_') {
        return false;
    }
    bytes.iter().all(|&b| {
        b.is_ascii_lowercase()
            || b.is_ascii_digit()
            || b == b'_'
            || b == b'-'
            || (allow_dot && b == b'.')
    })
}

/// Validate a rune name before it is ever used to build a filesystem path —
/// `namespace/name`, lowercase, exactly one `/`, no `..`, no traversal, no
/// backslashes or absolute paths. (Defeats path-traversal via a malicious
/// publish/fetch over the network.)
pub fn valid_name(name: &str) -> bool {
    let mut parts = name.split('/');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(ns), Some(nm), None) => valid_segment(ns, false) && valid_segment(nm, true),
        _ => false,
    }
}

/// A version must parse as strict `major.minor.patch` — which, being digits and
/// dots only, can carry no path-traversal payload.
pub fn valid_version(version: &str) -> bool {
    Version::parse(version).is_ok()
}

fn check_ref(name: &str, version: &str) -> PmResult<()> {
    if !valid_name(name) {
        return err(format!("invalid rune name `{name}` (must be lowercase `namespace/name`, no path traversal)"));
    }
    if !valid_version(version) {
        return err(format!("invalid version `{version}`"));
    }
    Ok(())
}

/// The local, directory-backed registry implementation. Used directly by the
/// `coven serve` server, and wrapped by [`Registry::Local`] for in-process use.
pub struct LocalRegistry {
    root: PathBuf,
}

impl LocalRegistry {
    pub fn new(root: PathBuf) -> LocalRegistry {
        LocalRegistry { root }
    }

    /// This registry's *existing* root public key (hex). Does NOT mint one —
    /// errors if the registry has published nothing yet — so a TOFU pin check
    /// against an absent registry simply finds no key rather than fabricating a
    /// new (mismatching) one.
    pub fn root_public_hex(&self) -> PmResult<String> {
        super::keys::read_public(&self.root)
    }

    /// Ensure the root signing key exists (minting it on first use). Called by
    /// the server at startup and implicitly on the first publish.
    pub fn ensure_key(&self) -> PmResult<()> {
        self.key().map(|_| ())
    }

    fn snapshot_path(&self) -> PathBuf {
        self.root.join("snapshot.json")
    }
    fn timestamp_path(&self) -> PathBuf {
        self.root.join("timestamp.json")
    }

    /// Regenerate and re-sign the snapshot + timestamp roles. Called after every
    /// mutation (publish/promote/yank), so the snapshot version bumps and the
    /// timestamp's expiry window refreshes.
    fn rebuild_metadata(&self) -> PmResult<()> {
        let key = self.key()?;
        let mut targets = std::collections::BTreeMap::new();
        for name in self.list_all() {
            for rec in self.versions(&name) {
                targets.insert(
                    format!("{}@{}", rec.name, rec.version),
                    crate::pm::tuf::target_digest(&rec.signing_payload()),
                );
            }
        }
        let prev = self
            .read_signed::<crate::pm::tuf::Snapshot>(&self.snapshot_path())
            .map(|s| s.signed.version)
            .unwrap_or(0);
        let snapshot = crate::pm::tuf::Snapshot {
            version: prev + 1,
            created: crate::pm::tuf::now_unix(),
            targets,
        };
        let signed_snapshot = crate::pm::tuf::sign(&key, snapshot);
        self.write_signed(&self.snapshot_path(), &signed_snapshot)?;

        let timestamp = crate::pm::tuf::Timestamp {
            snapshot_version: signed_snapshot.signed.version,
            snapshot_hash: crate::pm::tuf::sha256_hex(&crate::pm::tuf::canonical(&signed_snapshot.signed)),
            expires: crate::pm::tuf::now_unix() + crate::pm::tuf::TIMESTAMP_TTL_SECS,
        };
        let signed_timestamp = crate::pm::tuf::sign(&key, timestamp);
        self.write_signed(&self.timestamp_path(), &signed_timestamp)?;
        Ok(())
    }

    fn read_signed<T: serde::de::DeserializeOwned>(
        &self,
        path: &Path,
    ) -> Option<crate::pm::tuf::Signed<T>> {
        let text = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&text).ok()
    }

    fn write_signed<T: serde::Serialize>(
        &self,
        path: &Path,
        value: &crate::pm::tuf::Signed<T>,
    ) -> PmResult<()> {
        let text =
            serde_json::to_string_pretty(value).map_err(|e| PmError(format!("serialize: {e}")))?;
        std::fs::write(path, text)?;
        Ok(())
    }

    /// The signed snapshot role, rebuilding the metadata if it does not yet exist.
    pub fn snapshot_signed(&self) -> PmResult<crate::pm::tuf::Signed<crate::pm::tuf::Snapshot>> {
        if !self.snapshot_path().exists() {
            self.rebuild_metadata()?;
        }
        self.read_signed(&self.snapshot_path())
            .ok_or_else(|| PmError("registry snapshot unavailable".into()))
    }

    /// The signed timestamp role, rebuilding the metadata if it does not yet exist.
    pub fn timestamp_signed(&self) -> PmResult<crate::pm::tuf::Signed<crate::pm::tuf::Timestamp>> {
        if !self.timestamp_path().exists() {
            self.rebuild_metadata()?;
        }
        self.read_signed(&self.timestamp_path())
            .ok_or_else(|| PmError("registry timestamp unavailable".into()))
    }

    /// Every published rune name (namespaced), by walking the registry tree.
    pub fn list_all(&self) -> Vec<String> {
        let mut out = Vec::new();
        collect_rune_names(&self.root, &self.root, &mut out);
        out.sort();
        out
    }

    fn key(&self) -> PmResult<super::keys::RegistryKey> {
        crate::pm::keys::RegistryKey::load_or_create(&self.root)
    }

    /// Verify a record's signature against the registry's published root key.
    /// A missing or invalid signature is a hard failure (tampered metadata).
    pub fn verify_record(&self, record: &Record) -> PmResult<()> {
        let pubkey = super::keys::read_public(&self.root)?;
        verify_record_with(&pubkey, record)
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
    /// Publish a rune. The local registry is *anonymous* (filesystem access is
    /// the trust boundary); authentication/authorization for the networked path
    /// lives in the server's trust store ([`super::trusted`]). `provenance`, when
    /// `Some`, overrides the manifest-derived provenance with a verified
    /// trusted-publishing attestation.
    pub fn publish(
        &self,
        src: &RuneSource,
        manifest: &Manifest,
        uploaded_by: &str,
        provenance: Option<String>,
    ) -> PmResult<Record> {
        let name = &manifest.rune.name;
        let version = &manifest.rune.version;
        check_ref(name, version)?;
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
            // A verified trusted-publishing attestation (from the server) takes
            // precedence; otherwise provenance binds bytes -> uploader -> time,
            // with a declared source repo as an optional anchor.
            provenance: provenance.or_else(|| {
                let src = manifest
                    .rune
                    .source
                    .as_deref()
                    .map(|s| format!("|source={s}"))
                    .unwrap_or_default();
                Some(format!("uploader={uploaded_by}|at={}|hash={hash}{src}", now_unix()))
            }),
            sig: None,
        };
        self.write_record(&mut record)?;
        self.rebuild_metadata()?;
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
        check_ref(name, version)?;
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
        self.rebuild_metadata()?;

        Ok(Promotion {
            record,
            footprint_delta: delta,
            separation_of_duties: distinct,
        })
    }

    pub fn yank(&self, name: &str, version: &str) -> PmResult<()> {
        check_ref(name, version)?;
        let mut record = self.record(name, version)?;
        record.state = State::Yanked;
        self.write_record(&mut record)?;
        self.rebuild_metadata()
    }

    /// Read a version's metadata record.
    pub fn record(&self, name: &str, version: &str) -> PmResult<Record> {
        check_ref(name, version)?;
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
        if !valid_name(name) {
            return Vec::new();
        }
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
        select_best(self.versions(name), req, include_staged)
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

/// A remote (networked) registry requires a short-lived identity token — bearer
/// API tokens are not accepted.
fn require_token(token: Option<&super::trusted::IdToken>) -> PmResult<&super::trusted::IdToken> {
    token.ok_or_else(|| {
        PmError(
            "publishing/promoting to a remote registry requires a short-lived identity token \
             (set COVEN_ID_TOKEN via your CI's OIDC / `witchy coven-mint-token`) — long-lived \
             API tokens are not accepted"
                .into(),
        )
    })
}

/// Select the best version of a rune satisfying `req`: released always eligible,
/// staged only when `include_staged`, yanked never. Shared by the local and
/// remote registries so version-selection policy lives in exactly one place.
pub fn select_best(versions: Vec<Record>, req: &Req, include_staged: bool) -> Option<Record> {
    let mut candidates: Vec<Record> = versions
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

/// Verify a record's signature against a known public key (hex). A missing or
/// invalid signature is a hard failure (tampered metadata).
pub fn verify_record_with(pubkey_hex: &str, record: &Record) -> PmResult<()> {
    let Some(sig) = &record.sig else {
        return err(format!(
            "{}@{} has no signature — refusing to trust unsigned metadata",
            record.name, record.version
        ));
    };
    if super::keys::verify(pubkey_hex, record.signing_payload().as_bytes(), sig) {
        Ok(())
    } else {
        err(format!(
            "signature verification FAILED for {}@{} — registry metadata was tampered with",
            record.name, record.version
        ))
    }
}

fn collect_rune_names(base: &Path, dir: &Path, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if !p.is_dir() {
            continue;
        }
        // A rune dir has version subdirs that contain coven.json.
        let is_rune = std::fs::read_dir(&p)
            .map(|mut it| {
                it.any(|c| c.map(|c| c.path().join(META).exists()).unwrap_or(false))
            })
            .unwrap_or(false);
        if is_rune {
            out.push(
                p.strip_prefix(base)
                    .unwrap_or(&p)
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
        } else {
            collect_rune_names(base, &p, out);
        }
    }
}

/// A registry handle: either the in-process local directory model, or a remote
/// coven server reached over HTTP. The two expose an identical method surface so
/// the resolver and CLI are transport-agnostic.
pub enum Registry {
    Local(LocalRegistry),
    Remote(super::remote::RemoteRegistry),
}

impl Registry {
    pub fn local(root: PathBuf) -> Registry {
        Registry::Local(LocalRegistry::new(root))
    }

    pub fn remote(base_url: String) -> Registry {
        Registry::Remote(super::remote::RemoteRegistry::new(base_url))
    }

    pub fn publish(
        &self,
        src: &RuneSource,
        manifest: &Manifest,
        uploaded_by: &str,
        id_token: Option<&super::trusted::IdToken>,
    ) -> PmResult<Record> {
        match self {
            Registry::Local(r) => r.publish(src, manifest, uploaded_by, None),
            Registry::Remote(r) => r.publish(src, manifest, require_token(id_token)?),
        }
    }

    pub fn promote(
        &self,
        name: &str,
        version: &str,
        promoter: &str,
        second_factor: &str,
        id_token: Option<&super::trusted::IdToken>,
    ) -> PmResult<Promotion> {
        match self {
            Registry::Local(r) => r.promote(name, version, promoter, second_factor),
            Registry::Remote(r) => {
                r.promote(name, version, second_factor, require_token(id_token)?)
            }
        }
    }

    pub fn yank(&self, name: &str, version: &str, id_token: Option<&super::trusted::IdToken>) -> PmResult<()> {
        match self {
            Registry::Local(r) => r.yank(name, version),
            Registry::Remote(r) => r.yank(name, version, require_token(id_token)?),
        }
    }

    pub fn record(&self, name: &str, version: &str) -> PmResult<Record> {
        match self {
            Registry::Local(r) => r.record(name, version),
            Registry::Remote(r) => r.record(name, version),
        }
    }

    pub fn versions(&self, name: &str) -> Vec<Record> {
        match self {
            Registry::Local(r) => r.versions(name),
            Registry::Remote(r) => r.versions(name),
        }
    }

    pub fn best_match(&self, name: &str, req: &Req, include_staged: bool) -> Option<Record> {
        match self {
            Registry::Local(r) => r.best_match(name, req, include_staged),
            Registry::Remote(r) => select_best(r.versions(name), req, include_staged),
        }
    }

    pub fn fetch(&self, name: &str, version: &str) -> PmResult<RuneSource> {
        match self {
            Registry::Local(r) => r.fetch(name, version),
            Registry::Remote(r) => r.fetch(name, version),
        }
    }

    /// The fingerprint of the registry's root signing key — the value pinned in
    /// the lockfile (TOFU) so the key cannot be silently swapped.
    pub fn root_fingerprint(&self) -> PmResult<String> {
        Ok(super::keys::fingerprint_of(&self.root_public_hex()?))
    }

    pub fn root_public_hex(&self) -> PmResult<String> {
        match self {
            Registry::Local(r) => r.root_public_hex(),
            Registry::Remote(r) => r.root_public_hex(),
        }
    }

    pub fn list_all(&self) -> Vec<String> {
        match self {
            Registry::Local(r) => r.list_all(),
            Registry::Remote(r) => r.list_all().unwrap_or_default(),
        }
    }

    pub fn tuf_timestamp(&self) -> PmResult<crate::pm::tuf::Signed<crate::pm::tuf::Timestamp>> {
        match self {
            Registry::Local(r) => r.timestamp_signed(),
            Registry::Remote(r) => r.tuf_timestamp(),
        }
    }

    pub fn tuf_snapshot(&self) -> PmResult<crate::pm::tuf::Signed<crate::pm::tuf::Snapshot>> {
        match self {
            Registry::Local(r) => r.snapshot_signed(),
            Registry::Remote(r) => r.tuf_snapshot(),
        }
    }

    /// Verify the full TUF chain: timestamp signature + freshness (freeze
    /// protection), snapshot signature + consistency with the timestamp, and —
    /// when `pinned` is given — that the snapshot version has not regressed
    /// (rollback protection). Returns the verified snapshot version and payload.
    pub fn verify_tuf_chain(&self, pinned: Option<u64>) -> PmResult<(u64, crate::pm::tuf::Snapshot)> {
        let pubhex = self.root_public_hex()?;
        let ts = self.tuf_timestamp()?;
        if !crate::pm::tuf::verify_signed(&pubhex, &ts) {
            return err("timestamp role signature invalid — registry metadata was tampered with");
        }
        if crate::pm::tuf::now_unix() > ts.signed.expires {
            return err(
                "registry timestamp has expired — metadata is stale (possible freeze attack); the registry must re-sign",
            );
        }
        let snap = self.tuf_snapshot()?;
        if !crate::pm::tuf::verify_signed(&pubhex, &snap) {
            return err("snapshot role signature invalid — registry metadata was tampered with");
        }
        if crate::pm::tuf::sha256_hex(&crate::pm::tuf::canonical(&snap.signed)) != ts.signed.snapshot_hash {
            return err("snapshot hash does not match the timestamp — inconsistent registry metadata");
        }
        if snap.signed.version != ts.signed.snapshot_version {
            return err("snapshot/timestamp version mismatch — inconsistent registry metadata");
        }
        if let Some(p) = pinned
            && snap.signed.version < p
        {
            return err(format!(
                "snapshot rolled back: registry presents v{} but the lock pinned v{p} — possible rollback attack",
                snap.signed.version
            ));
        }
        Ok((snap.signed.version, snap.signed))
    }

    /// Confirm a specific record is exactly what the signed snapshot pins —
    /// catching an omitted (rolled-back) or swapped record.
    pub fn snapshot_contains(
        &self,
        snap: &crate::pm::tuf::Snapshot,
        name: &str,
        version: &str,
    ) -> PmResult<()> {
        let rec = self.record(name, version)?;
        let key = format!("{name}@{version}");
        let want = snap.targets.get(&key).ok_or_else(|| {
            PmError(format!("{key} is absent from the signed snapshot (omitted or rolled back)"))
        })?;
        if want == &crate::pm::tuf::target_digest(&rec.signing_payload()) {
            Ok(())
        } else {
            err(format!(
                "{key} record digest does not match the signed snapshot — tampered metadata"
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_registry() -> (LocalRegistry, PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "witchy-reg-{}-{}",
            std::process::id(),
            fastish_nonce()
        ));
        let _ = std::fs::remove_dir_all(&root);
        (LocalRegistry::new(root.clone()), root)
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
        let rec = reg.publish(&src, &m, "ci-bot", None).unwrap();
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
        reg.publish(&src, &m, "ci-bot", None).unwrap();
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
        reg.publish(&src, &m, "ci-bot", None).unwrap();
        assert!(reg.publish(&src, &m, "ci-bot", None).is_err(), "republish must fail");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn server_recomputes_footprint() {
        let (reg, root) = tmp_registry();
        let (src, m) = rune("acme/http", "1.0.0", "fn get(net: Net, url: String) -> String { url }");
        let rec = reg.publish(&src, &m, "ci-bot", None).unwrap();
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
        let res = reg.publish(&src, &m, "ci-bot", None);
        assert!(res.is_err(), "under-declared Net must be rejected");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn fetch_verifies_hash() {
        let (reg, root) = tmp_registry();
        let (src, m) = rune("acme/json", "1.0.0", "fn parse(s: String) -> String { s }");
        let rec = reg.publish(&src, &m, "ci-bot", None).unwrap();
        let fetched = reg.fetch("acme/json", "1.0.0").unwrap();
        assert_eq!(fetched.hash(), rec.hash);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn records_are_signed() {
        let (reg, root) = tmp_registry();
        let (src, m) = rune("acme/json", "1.0.0", "fn parse(s: String) -> String { s }");
        let rec = reg.publish(&src, &m, "ci-bot", None).unwrap();
        assert!(rec.sig.is_some(), "publish must sign the record");
        reg.verify_record(&rec).expect("freshly signed record must verify");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn tuf_chain_verifies_and_freeze_is_detected() {
        let (reg, root) = tmp_registry();
        let (src, m) = rune("acme/json", "1.0.0", "fn parse(s: String) -> String { s }");
        reg.publish(&src, &m, "ci-bot", None).unwrap();
        reg.promote("acme/json", "1.0.0", "alice", "webauthn").unwrap();

        let registry = Registry::Local(reg);
        // Fresh chain verifies.
        let (v, snap) = registry.verify_tuf_chain(None).unwrap();
        assert!(v >= 1);
        registry
            .snapshot_contains(&snap, "acme/json", "1.0.0")
            .expect("released version must be in the snapshot");

        // Forge a validly-signed but EXPIRED timestamp — freeze attack.
        let key = crate::pm::keys::RegistryKey::load_or_create(&root).unwrap();
        let stale = crate::pm::tuf::Timestamp {
            snapshot_version: v,
            snapshot_hash: crate::pm::tuf::sha256_hex(&crate::pm::tuf::canonical(&snap)),
            expires: crate::pm::tuf::now_unix() - 10,
        };
        let signed = crate::pm::tuf::sign(&key, stale);
        std::fs::write(
            root.join("timestamp.json"),
            serde_json::to_string_pretty(&signed).unwrap(),
        )
        .unwrap();

        let res = registry.verify_tuf_chain(None);
        assert!(res.is_err(), "expired timestamp must be rejected");
        assert!(res.unwrap_err().to_string().contains("expired"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn tuf_rollback_is_detected() {
        let (reg, root) = tmp_registry();
        let (src, m) = rune("acme/json", "1.0.0", "fn parse(s: String) -> String { s }");
        reg.publish(&src, &m, "ci-bot", None).unwrap();
        reg.promote("acme/json", "1.0.0", "alice", "webauthn").unwrap();
        let registry = Registry::Local(reg);
        let (v, _) = registry.verify_tuf_chain(None).unwrap();
        // Pinning a higher version than the registry presents = rollback.
        let res = registry.verify_tuf_chain(Some(v + 100));
        assert!(res.is_err());
        assert!(res.unwrap_err().to_string().contains("rolled back"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rune_names_reject_path_traversal() {
        assert!(valid_name("acme/json"));
        assert!(valid_name("acme/http-client"));
        assert!(valid_name("std/json2"));
        // Traversal / unsafe forms are rejected.
        assert!(!valid_name("../../etc"));
        assert!(!valid_name("acme/../../etc"));
        assert!(!valid_name("acme/..%2f"));
        assert!(!valid_name("/etc/passwd"));
        assert!(!valid_name("acme"), "must be namespaced");
        assert!(!valid_name("acme/json/extra"));
        assert!(!valid_name("acme\\json"));
        assert!(!valid_name(".hidden/x"));
        assert!(!valid_name("acme/.."));
        assert!(valid_version("1.2.3"));
        assert!(!valid_version("../../etc"));
        assert!(!valid_version("1.0.0/.."));
    }

    #[test]
    fn registry_refuses_malicious_refs() {
        let (reg, root) = tmp_registry();
        // Reads with a traversal name/version must error, not touch the fs path.
        assert!(reg.record("../../etc", "1.0.0").is_err());
        assert!(reg.record("acme/json", "../../etc").is_err());
        assert!(reg.fetch("../../etc", "1.0.0").is_err());
        assert!(reg.versions("../../etc").is_empty());
        // Publish of a traversal name is refused.
        let toml = "[rune]\nname = \"../../evil\"\nversion = \"1.0.0\"\n";
        let m = Manifest::parse(toml).unwrap();
        let files = vec![("witchy.toml".to_string(), toml.as_bytes().to_vec())];
        let src = RuneSource { files };
        assert!(reg.publish(&src, &m, "ci-bot", None).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn tampered_record_fails_verification() {
        let (reg, root) = tmp_registry();
        let (src, m) = rune("acme/json", "1.0.0", "fn parse(s: String) -> String { s }");
        reg.publish(&src, &m, "ci-bot", None).unwrap();
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
