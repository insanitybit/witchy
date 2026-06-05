//! Dependency resolution: turn a manifest's dependency requirements into a fully
//! pinned graph, fetching from the registry (released versions only) and local
//! paths, placing every rune in the content-addressed store, and **recomputing
//! every rune's footprint from source** (never trusting metadata).
//!
//! Resolution executes no rune code — it only reads, hashes, and type-agnostically
//! analyzes source. The graph it produces is the input to the capability gate and
//! the lockfile.

use std::collections::BTreeMap;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use super::footprint::Footprint;
use super::lockfile::{LockedRune, Lockfile};
use super::manifest::{Dep, Manifest};
use super::registry::Registry;
use super::semver::Req;
use super::store::{RuneSource, Store};
use super::{PmResult, err};

#[derive(Debug, Clone)]
pub struct ResolvedRune {
    pub name: String,
    pub version: String,
    pub registry: Option<String>,
    /// `path:<dir>` for local path deps.
    pub source_kind: Option<String>,
    pub hash: String,
    pub footprint: Footprint,
    pub provenance: Option<String>,
    /// The actual source, for gathering modules at build time.
    pub src: RuneSource,
}

#[derive(Debug, Clone, Default)]
pub struct Resolution {
    /// Dependencies only (not the root rune), sorted by name.
    pub runes: Vec<ResolvedRune>,
}

impl Resolution {
    pub fn to_lockfile(&self) -> Lockfile {
        let mut runes: Vec<LockedRune> = self
            .runes
            .iter()
            .map(|r| {
                let mut lr = LockedRune {
                    name: r.name.clone(),
                    version: r.version.clone(),
                    registry: r.registry.clone(),
                    source: r.source_kind.clone(),
                    hash: r.hash.clone(),
                    runtime_footprint: vec![],
                    build_footprint: vec![],
                    determinism: String::new(),
                    provenance: r.provenance.clone(),
                };
                lr.set_footprint(&r.footprint);
                lr
            })
            .collect();
        runes.sort_by(|a, b| a.name.cmp(&b.name));
        Lockfile {
            registry_root: None,
            runes,
        }
    }

    /// The aggregate (maximum) authority of the resolved dependency tree.
    pub fn aggregate_footprint(&self) -> Footprint {
        let mut fp = Footprint::default();
        for r in &self.runes {
            fp.runtime.extend(r.footprint.runtime.iter().cloned());
            fp.build.extend(r.footprint.build.iter().cloned());
        }
        fp
    }

    pub fn find(&self, name: &str) -> Option<&ResolvedRune> {
        self.runes.iter().find(|r| r.name == name)
    }
}

/// A pending dependency to resolve: how it's named, its requirement, and the
/// directory path-deps resolve relative to.
struct Pending {
    dep: Dep,
    /// The key the *parent* referred to it by (for diagnostics).
    referrer_key: String,
    base_dir: PathBuf,
}

pub fn resolve(
    root_manifest: &Manifest,
    root_dir: &Path,
    registry: &Registry,
    store: &Store,
) -> PmResult<Resolution> {
    let mut resolved: BTreeMap<String, ResolvedRune> = BTreeMap::new();
    let mut queue: VecDeque<Pending> = VecDeque::new();

    for (key, dep) in &root_manifest.dependencies {
        queue.push_back(Pending {
            dep: dep.clone(),
            referrer_key: key.clone(),
            base_dir: root_dir.to_path_buf(),
        });
    }

    while let Some(p) = queue.pop_front() {
        let (src, manifest, registry_name, source_kind, provenance) =
            fetch_dep(&p, registry, store)?;
        let name = manifest.rune.name.clone();
        let version = manifest.rune.version.clone();

        // Already resolved? Ensure the existing pick satisfies this requirement
        // too (MVP: no backtracking — a hard conflict is an error).
        if let Some(existing) = resolved.get(&name) {
            if let Some(reqstr) = p.dep.version_req() {
                let req = Req::parse(reqstr)?;
                let v = super::semver::Version::parse(&existing.version)?;
                if !req.matches(&v) {
                    return err(format!(
                        "version conflict for `{name}`: already resolved to {} but `{}` requires {reqstr}",
                        existing.version, p.referrer_key
                    ));
                }
            }
            continue;
        }

        let hash = store.put(&src)?;
        let footprint = super::footprint::of_modules(&src.modules())?;

        resolved.insert(
            name.clone(),
            ResolvedRune {
                name: name.clone(),
                version,
                registry: registry_name,
                source_kind,
                hash,
                footprint,
                provenance,
                src,
            },
        );

        // Enqueue this dep's own dependencies (transitive).
        let dep_base = match p.dep.path() {
            Some(rel) => p.base_dir.join(rel),
            None => p.base_dir.clone(), // registry deps: base_dir unused downstream
        };
        for (key, dep) in &manifest.dependencies {
            queue.push_back(Pending {
                dep: dep.clone(),
                referrer_key: format!("{name} -> {key}"),
                base_dir: dep_base.clone(),
            });
        }
    }

    let mut runes: Vec<ResolvedRune> = resolved.into_values().collect();
    runes.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(Resolution { runes })
}

/// Fetch one dependency's source + manifest, from a local path or the registry.
fn fetch_dep(
    p: &Pending,
    registry: &Registry,
    _store: &Store,
) -> PmResult<(RuneSource, Manifest, Option<String>, Option<String>, Option<String>)> {
    if let Some(rel) = p.dep.path() {
        let dir = p.base_dir.join(rel);
        let src = RuneSource::read_dir(&dir)?;
        let manifest = src.manifest()?;
        let source_kind = Some(format!("path:{}", dir.display()));
        return Ok((src, manifest, None, source_kind, source_kind_provenance(&dir)));
    }

    // Registry dependency.
    let reqstr = p
        .dep
        .version_req()
        .ok_or_else(|| super::PmError(format!(
            "dependency `{}` needs a version (registry deps must be pinned)",
            p.referrer_key
        )))?;
    let req = Req::parse(reqstr)?;
    let name = registry_dep_name(p)?;
    let registry_name = p.dep.registry().to_string();

    let Some(record) = registry.best_match(&name, &req, false) else {
        // Distinguish "nothing matches" from "only a staged version exists".
        let staged = registry.best_match(&name, &req, true).is_some();
        if staged {
            return err(format!(
                "`{name}` matching {reqstr} exists but is only STAGED, not released — it must be promoted (second factor) before it can be resolved"
            ));
        }
        return err(format!(
            "no released version of `{name}` matching {reqstr} in registry `{registry_name}`"
        ));
    };

    let src = registry.fetch(&name, &record.version)?;
    let manifest = src.manifest()?;
    Ok((
        src,
        manifest,
        Some(registry_name),
        None,
        record.provenance.clone(),
    ))
}

fn source_kind_provenance(dir: &Path) -> Option<String> {
    Some(format!("path:{}", dir.display()))
}

/// For a registry dep, the rune name is the dependency *key* (which is the
/// namespaced rune name, e.g. `acme/json`). The first segment of `referrer_key`
/// before any ` -> ` is the key when it's a direct dep; for transitive deps we
/// take the part after the last ` -> `.
fn registry_dep_name(p: &Pending) -> PmResult<String> {
    let key = p.referrer_key.rsplit(" -> ").next().unwrap_or(&p.referrer_key);
    if key.trim().is_empty() {
        return err("internal: empty registry dependency name");
    }
    Ok(key.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(label: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "witchy-resolve-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn publish(reg: &Registry, name: &str, version: &str, body: &str, deps: &str) {
        let toml = format!("[rune]\nname = \"{name}\"\nversion = \"{version}\"\n{deps}");
        let manifest = Manifest::parse(&toml).unwrap();
        let module = name.rsplit('/').next().unwrap();
        let mut files = vec![
            ("witchy.toml".to_string(), toml.clone().into_bytes()),
            (format!("src/{module}.witchy"), body.as_bytes().to_vec()),
        ];
        files.sort_by(|a, b| a.0.cmp(&b.0));
        let src = RuneSource { files };
        reg.publish(&src, &manifest, "ci-bot").unwrap();
        reg.promote(name, version, "alice", "webauthn").unwrap();
    }

    #[test]
    fn resolves_transitive_registry_deps() {
        let reg_root = tmp("reg");
        let store_root = tmp("store");
        let reg = Registry::new(reg_root.clone());
        let store = Store::new(store_root.clone());

        publish(&reg, "acme/url", "1.0.0", "fn parse(s: String) -> String { s }", "");
        publish(
            &reg,
            "acme/http",
            "1.0.0",
            "fn get(net: Net, u: String) -> String { u }",
            "\n[dependencies]\n\"acme/url\" = \"^1.0.0\"\n",
        );

        let root = Manifest::parse(
            "[rune]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\n\"acme/http\" = \"^1.0.0\"\n",
        )
        .unwrap();
        let res = resolve(&root, &reg_root, &reg, &store).unwrap();
        // Both http and its transitive dep url are resolved.
        assert!(res.find("acme/http").is_some());
        assert!(res.find("acme/url").is_some());
        // Net surfaces in the aggregate footprint.
        assert!(res.aggregate_footprint().runtime.contains("Net"));

        let _ = std::fs::remove_dir_all(&reg_root);
        let _ = std::fs::remove_dir_all(&store_root);
    }

    #[test]
    fn staged_only_dep_is_not_resolvable() {
        let reg_root = tmp("reg2");
        let store_root = tmp("store2");
        let reg = Registry::new(reg_root.clone());
        let store = Store::new(store_root.clone());

        // Publish but do NOT promote.
        let toml = "[rune]\nname = \"acme/json\"\nversion = \"1.0.0\"\n";
        let m = Manifest::parse(toml).unwrap();
        let mut files = vec![
            ("witchy.toml".to_string(), toml.as_bytes().to_vec()),
            ("src/json.witchy".to_string(), b"fn p(s: String) -> String { s }".to_vec()),
        ];
        files.sort_by(|a, b| a.0.cmp(&b.0));
        reg.publish(&RuneSource { files }, &m, "ci-bot").unwrap();

        let root = Manifest::parse(
            "[rune]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\n\"acme/json\" = \"^1.0.0\"\n",
        )
        .unwrap();
        let res = resolve(&root, &reg_root, &reg, &store);
        assert!(res.is_err(), "staged-only dep must not resolve");
        assert!(res.unwrap_err().to_string().contains("STAGED"));

        let _ = std::fs::remove_dir_all(&reg_root);
        let _ = std::fs::remove_dir_all(&store_root);
    }
}
