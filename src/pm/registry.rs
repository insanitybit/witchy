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
    /// Unix seconds when this version was promoted to released. `0` = unknown
    /// (legacy records), which is treated as past any cooldown. Signed — the
    /// staging-cooldown window keys off it, so it must be tamper-evident.
    #[serde(default)]
    pub released_at: u64,
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
            "coven-v1\nname={}\nversion={}\nstate={}\nhash={}\nrt={}\nbuild={}\nuploaded_by={}\npromoted_by={}\nfactor={}\nprovenance={}\nreleased_at={}",
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
            self.released_at,
        )
    }
}

const META: &str = "coven.json";

pub fn now_unix() -> u64 {
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

// ---------------------------------------------------------------------------
// Publish-time naming policy: minimum length + typosquatting guard.
//
// These are POLICY (enforced only when a *new* name is first published), kept
// separate from `valid_name`, which is a path-safety check used everywhere
// (including fetch/resolve of already-published names). The constants below are
// the only tuning knobs.
// ---------------------------------------------------------------------------

/// Minimum length of the *name* segment (the part after `namespace/`). Blocks
/// single-character junk/squat-prone names. Enforced only on new publishes, so
/// it can never strand an already-published name.
const MIN_NAME_SEGMENT_LEN: usize = 2;

/// A new name within this *typo distance* of an existing rune owned by a
/// DIFFERENT publisher is rejected as a likely typosquat.
const TYPO_BLOCK_DISTANCE: usize = 1;

/// Distance-based blocking only applies once the longer normalized name is at
/// least this long — short names sit one typo apart too often for proximity to
/// mean anything. (Names that normalize *identically* are blocked at any length.)
const TYPO_MIN_LEN: usize = 5;

/// Fold a rune name to a canonical form for typosquat comparison: drop the
/// separators (`-`, `_`, `.`) and the namespace slash, and collapse the common
/// homoglyphs/confusables (`0→o`, `1→l`, `5→s`, `3→e`, `rn→m`, `vv→w`) so visual
/// look-alikes and separator tricks land on the same string. Names are already
/// `[a-z0-9_.-/]`, so this stays ASCII.
fn normalize_for_typo(name: &str) -> String {
    let folded: String = name
        .chars()
        .filter_map(|c| match c {
            '-' | '_' | '.' | '/' => None,
            '0' => Some('o'),
            '1' => Some('l'),
            '5' => Some('s'),
            '3' => Some('e'),
            other => Some(other),
        })
        .collect();
    folded.replace("rn", "m").replace("vv", "w")
}

/// Damerau–Levenshtein (optimal string alignment) distance — like plain edit
/// distance, but an adjacent transposition (`form`↔`from`) counts as ONE
/// operation, which is the whole point of a *typo* distance: transposition is a
/// single slip of the fingers, not two unrelated edits.
fn typo_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let (n, m) = (a.len(), b.len());
    if n == 0 {
        return m;
    }
    if m == 0 {
        return n;
    }
    let mut d = vec![vec![0usize; m + 1]; n + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in d[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=n {
        for j in 1..=m {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let mut best = (d[i - 1][j] + 1) // deletion
                .min(d[i][j - 1] + 1) // insertion
                .min(d[i - 1][j - 1] + cost); // substitution
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                best = best.min(d[i - 2][j - 2] + 1); // transposition
            }
            d[i][j] = best;
        }
    }
    d[n][m]
}

/// Is `candidate` close enough to `other` to be a likely typosquat? Both are
/// compared in normalized form: an identical normalization is always a hit (a
/// separator/homoglyph squat such as `my-pkg` vs `my_pkg`); otherwise a typo
/// distance within the block radius counts, but only once the names are long
/// enough that the proximity isn't coincidence.
fn is_typosquat(candidate: &str, other: &str) -> bool {
    let a = normalize_for_typo(candidate);
    let b = normalize_for_typo(other);
    if a == b {
        return true;
    }
    if a.chars().count().max(b.chars().count()) < TYPO_MIN_LEN {
        return false;
    }
    typo_distance(&a, &b) <= TYPO_BLOCK_DISTANCE
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

    /// Enforce publish-time naming policy for a rune name: a minimum name-segment
    /// length, and a typosquatting guard rejecting names too close to an existing
    /// rune owned by a *different* publisher. Skipped once the name already exists
    /// (publishing a new version of your own rune is never a squat).
    fn check_new_name(&self, name: &str, uploaded_by: &str) -> PmResult<()> {
        let leaf = name.rsplit('/').next().unwrap_or(name);
        if leaf.chars().count() < MIN_NAME_SEGMENT_LEN {
            return err(format!(
                "rune name `{name}` is too short — the name segment must be at least {MIN_NAME_SEGMENT_LEN} characters"
            ));
        }
        // Only guard brand-new names; a new version of an existing rune isn't a squat.
        if !self.versions(name).is_empty() {
            return Ok(());
        }
        for existing in self.list_all() {
            if existing == name || !is_typosquat(name, &existing) {
                continue;
            }
            // Same-publisher near-names are fine (e.g. `acme/foo`, `acme/foo-cli`);
            // only a DIFFERENT user's near-name is a typosquat.
            let owner = self.versions(&existing).first().map(|r| r.uploaded_by.clone());
            if owner.as_deref() != Some(uploaded_by) {
                return err(format!(
                    "rune name `{name}` is too similar to the existing rune `{existing}` (typosquatting guard) — choose a more distinct name"
                ));
            }
        }
        Ok(())
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
        // Minimum-length + typosquatting policy for new names (server-side, with
        // the full corpus visible — the remote publish path routes through here).
        self.check_new_name(name, uploaded_by)?;

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
            released_at: 0,
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
        // The release moment starts the staging-cooldown clock (§8): a fresh
        // release is not resolvable until the window passes (or --allow-fresh).
        record.released_at = now_unix();
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

// --- staging cooldown (§8) ---------------------------------------------------
//
// A freshly released version is not resolvable until it has been released for a
// cooldown window — time for the ecosystem to notice a compromised release —
// unless the consumer explicitly accepts it (`--allow-fresh`). The window is
// `WITCHY_COOLDOWN_SECS` (default 72 hours); `released_at == 0` (legacy records)
// is treated as past any window. Locked versions are unaffected: like yank, the
// cooldown gates *new resolution* only.

use std::sync::atomic::{AtomicBool, Ordering};

static ALLOW_FRESH: AtomicBool = AtomicBool::new(false);

/// Accept versions still inside their cooldown window for this invocation
/// (the `--allow-fresh` flag).
pub fn set_allow_fresh(v: bool) {
    ALLOW_FRESH.store(v, Ordering::Relaxed);
}

pub fn cooldown_secs() -> u64 {
    std::env::var("WITCHY_COOLDOWN_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(72 * 3600)
}

/// Whether a released record is still cooling down (and fresh ones aren't being
/// accepted).
fn cooling(r: &Record) -> bool {
    if ALLOW_FRESH.load(Ordering::Relaxed) || r.released_at == 0 {
        return false;
    }
    now_unix() < r.released_at.saturating_add(cooldown_secs())
}

/// Select the best version of a rune satisfying `req`: released always eligible
/// (once its cooldown window has passed), staged only when `include_staged`,
/// yanked never. Shared by the local and remote registries so version-selection
/// policy lives in exactly one place.
pub fn select_best(versions: Vec<Record>, req: &Req, include_staged: bool) -> Option<Record> {
    let mut candidates: Vec<Record> = versions
        .into_iter()
        .filter(|r| match r.state {
            State::Released => !cooling(r),
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

/// A released version that *would* satisfy `req` but is still inside its
/// cooldown window — so callers can explain "blocked by cooldown, use
/// --allow-fresh" instead of a bare "no version found".
pub fn cooling_match(versions: Vec<Record>, req: &Req) -> Option<Record> {
    let mut candidates: Vec<Record> = versions
        .into_iter()
        .filter(|r| matches!(r.state, State::Released) && cooling(r))
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

    /// A released version that would satisfy `req` but is still inside its
    /// staging-cooldown window — for "blocked by cooldown" diagnostics.
    pub fn cooling_match(&self, name: &str, req: &Req) -> Option<Record> {
        let versions = match self {
            Registry::Local(r) => r.versions(name),
            Registry::Remote(r) => r.versions(name),
        };
        cooling_match(versions, req)
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
        // In-process tests publish and immediately resolve; zero the staging
        // cooldown so they exercise their own subject. (The cooldown has its own
        // e2e test, which runs in a subprocess with its own window.)
        unsafe { std::env::set_var("WITCHY_COOLDOWN_SECS", "0") };
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
    fn typo_distance_counts_transposition_as_one() {
        assert_eq!(typo_distance("from", "form"), 1); // adjacent swap = 1 op
        assert_eq!(typo_distance("request", "reqest"), 1); // omission
        assert_eq!(typo_distance("json", "json"), 0);
        assert_eq!(typo_distance("serde", "tokio"), 5);
    }

    #[test]
    fn normalize_folds_separators_and_homoglyphs() {
        // separator tricks and homoglyphs collapse to the same canonical form
        assert_eq!(normalize_for_typo("acme/my-pkg"), normalize_for_typo("acme/my_pkg"));
        assert_eq!(normalize_for_typo("acme/my-pkg"), normalize_for_typo("acme/mypkg"));
        assert_eq!(normalize_for_typo("acme/rust0"), normalize_for_typo("acme/rusto"));
        assert_eq!(normalize_for_typo("acme/json"), "acmejson");
    }

    #[test]
    fn typosquat_detection() {
        // identical-after-normalization → squat at any length
        assert!(is_typosquat("acme/my-pkg", "acme/my_pkg"));
        assert!(is_typosquat("acme/acrne", "acme/acme")); // rn→m homoglyph
        // one typo on a long-enough name → squat
        assert!(is_typosquat("acme/reqwest", "acme/reqvest"));
        assert!(is_typosquat("acme/request", "acme/reqest"));
        // genuinely different names → fine
        assert!(!is_typosquat("acme/serde", "acme/tokio"));
        // different namespace keeps the distance large → not flagged
        assert!(!is_typosquat("mallory/json", "acme/json"));
        // short names: only identical-normalization blocks, not distance-1
        assert!(!is_typosquat("acme/io", "acme/os"));
    }

    #[test]
    fn publish_rejects_too_short_name() {
        let (reg, root) = tmp_registry();
        let (src, m) = rune("acme/x", "1.0.0", "fn f() -> Nil:\n    nil\n");
        let e = reg.publish(&src, &m, "ci-bot", None).unwrap_err();
        assert!(e.to_string().contains("too short"), "{e}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn publish_blocks_typosquat_of_another_user() {
        let (reg, root) = tmp_registry();
        let body = "fn f() -> Nil:\n    nil\n";
        let (src, m) = rune("acme/reqwest", "1.0.0", body);
        reg.publish(&src, &m, "alice", None).unwrap();

        // A DIFFERENT user publishing a near-identical name is rejected.
        let (src2, m2) = rune("acme/reqvest", "1.0.0", body);
        let e = reg.publish(&src2, &m2, "mallory", None).unwrap_err();
        assert!(e.to_string().contains("typosquatting"), "{e}");

        // The SAME user may publish a similar name (their own family of runes).
        let (src3, m3) = rune("acme/reqvest", "1.0.0", body);
        assert!(reg.publish(&src3, &m3, "alice", None).is_ok());

        // A clearly distinct name is fine for anyone.
        let (src4, m4) = rune("acme/hyper", "1.0.0", body);
        assert!(reg.publish(&src4, &m4, "mallory", None).is_ok());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn publish_stages_not_resolvable() {
        let (reg, root) = tmp_registry();
        let (src, m) = rune("acme/json", "1.0.0", r#"
fn parse(s: String) -> String:
    s
"#);
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
        let (src, m) = rune("acme/json", "1.0.0", r#"
fn parse(s: String) -> String:
    s
"#);
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
        let (src, m) = rune("acme/json", "1.0.0", r#"
fn parse(s: String) -> String:
    s
"#);
        reg.publish(&src, &m, "ci-bot", None).unwrap();
        assert!(reg.publish(&src, &m, "ci-bot", None).is_err(), "republish must fail");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn server_recomputes_footprint() {
        let (reg, root) = tmp_registry();
        let (src, m) = rune("acme/http", "1.0.0", r#"
fn get(net: Net, url: String) -> String:
    url
"#);
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
        let (src, m) = rune("acme/json", "1.0.0", r#"
fn parse(s: String) -> String:
    s
"#);
        let rec = reg.publish(&src, &m, "ci-bot", None).unwrap();
        let fetched = reg.fetch("acme/json", "1.0.0").unwrap();
        assert_eq!(fetched.hash(), rec.hash);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn records_are_signed() {
        let (reg, root) = tmp_registry();
        let (src, m) = rune("acme/json", "1.0.0", r#"
fn parse(s: String) -> String:
    s
"#);
        let rec = reg.publish(&src, &m, "ci-bot", None).unwrap();
        assert!(rec.sig.is_some(), "publish must sign the record");
        reg.verify_record(&rec).expect("freshly signed record must verify");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn tuf_chain_verifies_and_freeze_is_detected() {
        let (reg, root) = tmp_registry();
        let (src, m) = rune("acme/json", "1.0.0", r#"
fn parse(s: String) -> String:
    s
"#);
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
        let (src, m) = rune("acme/json", "1.0.0", r#"
fn parse(s: String) -> String:
    s
"#);
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
        let (src, m) = rune("acme/json", "1.0.0", r#"
fn parse(s: String) -> String:
    s
"#);
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
