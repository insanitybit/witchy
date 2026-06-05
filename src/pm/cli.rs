//! The `coven` command-line surface, folded into the `witchy` binary.
//!
//! Commands: new, init, add, build, run, update, audit, why, why-cap, publish,
//! promote, list, verify, vendor. The invariant throughout: nothing here ever
//! runs rune code (except `run`, which runs the *user's own* program) and nothing
//! here grants authority — the gate only makes the capability footprint legible
//! and blocks silent widening.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::ast::{Item, Module};
use crate::{interpreter, linker, parser, typeck};

use super::footprint::Widening;
use super::lockfile::Lockfile;
use super::manifest::{Dep, DepDetail, Manifest, DEFAULT_REGISTRY};
use super::registry::{Registry, State};
use super::resolve::{self, Resolution};
use super::semver::Req;
use super::store::{RuneSource, Store};
use super::{err, PmResult};

/// Subcommands coven owns. `main` routes to [`run`] when arg 1 is one of these.
pub fn is_command(s: &str) -> bool {
    matches!(
        s,
        "new" | "init" | "add" | "build" | "run" | "update" | "audit" | "why" | "why-cap"
            | "publish" | "promote" | "yank" | "list" | "verify" | "vendor" | "coven"
    )
}

/// Runtime environment: where the store and registry live, and who we are.
struct CovenEnv {
    store: Store,
    registry: Registry,
    user: String,
}

impl CovenEnv {
    fn load() -> CovenEnv {
        let home = std::env::var("WITCHY_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                let base = std::env::var("HOME").unwrap_or_else(|_| ".".into());
                PathBuf::from(base).join(".witchy")
            });
        let user = std::env::var("WITCHY_USER").unwrap_or_else(|_| "anon".into());
        CovenEnv {
            store: Store::new(home.join("store")),
            registry: Registry::new(home.join("registry")),
            user,
        }
    }
}

pub fn run(args: &[String]) -> PmResult<()> {
    let Some((cmd, rest)) = args.split_first() else {
        return print_help();
    };
    match cmd.as_str() {
        "coven" => run(rest), // allow `witchy coven <subcommand>` too
        "new" => cmd_new(rest),
        "init" => cmd_init(rest),
        "add" => cmd_add(rest),
        "build" => cmd_build(rest),
        "run" => cmd_run(rest),
        "update" => cmd_update(rest),
        "audit" => cmd_audit(rest),
        "why" => cmd_why(rest),
        "why-cap" => cmd_why_cap(rest),
        "publish" => cmd_publish(rest),
        "promote" => cmd_promote(rest),
        "yank" => cmd_yank(rest),
        "list" => cmd_list(rest),
        "verify" => cmd_verify(rest),
        "vendor" => cmd_vendor(rest),
        other => err(format!("unknown coven command `{other}`")),
    }
}

fn print_help() -> PmResult<()> {
    println!(
        "coven — witchy's package manager\n\n\
         USAGE: witchy <command> [args]\n\n\
         Project:\n  \
           new <name>            scaffold a new rune\n  \
           init                  add a manifest to the current directory\n  \
           add <pkg>[@ver]       add a dependency (blocks on capability widening)\n  \
           build                 resolve + link + type-check (offline)\n  \
           run                   build and run the program\n  \
           update [pkg]          re-resolve within constraints\n  \
           vendor                materialize resolved sources into ./vendor\n\n\
         Audit:\n  \
           audit                 print runtime + build footprints + determinism\n  \
           why <pkg>             show why a rune is in the tree\n  \
           why-cap <Kind>        show which runes introduce a capability kind\n  \
           verify                re-verify the lock against the store/registry\n\n\
         Registry (coven):\n  \
           publish [dir]         publish a rune (lands STAGED, not resolvable)\n  \
           promote <pkg>@<ver> --factor <type>   release a staged version (2FA)\n  \
           list [pkg]            list published versions and their state"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Argument parsing
// ---------------------------------------------------------------------------

struct ParsedArgs {
    positional: Vec<String>,
    values: BTreeMap<String, Vec<String>>,
    bools: BTreeSet<String>,
}

const VALUE_FLAGS: &[&str] = &[
    "--allow-cap",
    "--allow-build-cap",
    "--factor",
    "--as",
    "--registry",
    "--path",
    "--version",
];

fn parse_args(rest: &[String]) -> ParsedArgs {
    let mut positional = Vec::new();
    let mut values: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut bools = BTreeSet::new();
    let mut i = 0;
    while i < rest.len() {
        let tok = &rest[i];
        if let Some(name) = tok.strip_prefix("--") {
            let flag = format!("--{name}");
            if VALUE_FLAGS.contains(&flag.as_str()) && i + 1 < rest.len() {
                values.entry(flag).or_default().push(rest[i + 1].clone());
                i += 2;
                continue;
            }
            bools.insert(flag);
            i += 1;
        } else {
            positional.push(tok.clone());
            i += 1;
        }
    }
    ParsedArgs {
        positional,
        values,
        bools,
    }
}

impl ParsedArgs {
    fn val(&self, flag: &str) -> Option<&str> {
        self.values.get(flag).and_then(|v| v.first()).map(|s| s.as_str())
    }
    fn vals(&self, flag: &str) -> Vec<String> {
        self.values.get(flag).cloned().unwrap_or_default()
    }
    fn has(&self, flag: &str) -> bool {
        self.bools.contains(flag)
    }
}

// ---------------------------------------------------------------------------
// new / init
// ---------------------------------------------------------------------------

fn module_ident(name: &str) -> String {
    name.rsplit('/').next().unwrap_or(name).replace('-', "_")
}

fn cmd_new(rest: &[String]) -> PmResult<()> {
    let a = parse_args(rest);
    let name = a
        .positional
        .first()
        .ok_or_else(|| super::PmError("usage: witchy new <name>".into()))?;
    let dir_name = name.rsplit('/').next().unwrap_or(name);
    let dir = PathBuf::from(dir_name);
    if dir.exists() {
        return err(format!("`{}` already exists", dir.display()));
    }
    let module = module_ident(name);
    std::fs::create_dir_all(dir.join("src"))?;
    let manifest = format!("[rune]\nname = \"{name}\"\nversion = \"0.1.0\"\n\n[dependencies]\n");
    std::fs::write(dir.join("witchy.toml"), manifest)?;
    let starter = format!(
        "// {name} — a witchy rune.\n\
         // `main` is the root actor: the host mints the capabilities it declares\n\
         // (here, Console) and nothing else can perform effects.\n\n\
         fn main(console: Console) {{\n  \
           print(console, \"hello from {name}\")\n\
         }}\n"
    );
    std::fs::write(dir.join("src").join(format!("{module}.witchy")), starter)?;
    println!("created rune `{name}` in {}/", dir.display());
    println!("  cd {dir_name} && witchy run");
    Ok(())
}

fn cmd_init(rest: &[String]) -> PmResult<()> {
    let _ = rest;
    let dir = PathBuf::from(".");
    if dir.join("witchy.toml").exists() {
        return err("witchy.toml already exists here");
    }
    let cwd = std::env::current_dir()?;
    let name = cwd
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("app")
        .to_string();
    let module = module_ident(&name);
    std::fs::create_dir_all(dir.join("src"))?;
    std::fs::write(
        dir.join("witchy.toml"),
        format!("[rune]\nname = \"{name}\"\nversion = \"0.1.0\"\n\n[dependencies]\n"),
    )?;
    let src = dir.join("src").join(format!("{module}.witchy"));
    if !src.exists() {
        std::fs::write(
            &src,
            format!("fn main(console: Console) {{\n  print(console, \"hello from {name}\")\n}}\n"),
        )?;
    }
    println!("initialized rune `{name}` (witchy.toml + src/{module}.witchy)");
    Ok(())
}

// ---------------------------------------------------------------------------
// Program assembly (build / run share this)
// ---------------------------------------------------------------------------

struct Assembled {
    linked: Module,
    /// The entry module name (the one whose `main` is run), if any.
    entry: String,
    has_main: bool,
    resolution: Resolution,
    manifest: Manifest,
}

/// Write the lockfile for a resolution, pinning the registry's root-key
/// fingerprint (TOFU) when any dependency comes from a registry.
fn save_lock(root: &Path, resolution: &Resolution, env: &CovenEnv) -> PmResult<()> {
    let mut lock = resolution.to_lockfile();
    if resolution.runes.iter().any(|r| r.registry.is_some()) {
        lock.registry_root = Some(env.registry.root_fingerprint()?);
    }
    lock.save(root)
}

fn assemble(root_dir: &Path, env: &CovenEnv) -> PmResult<Assembled> {
    let manifest = Manifest::load(root_dir)?;
    let root_src = RuneSource::read_dir(root_dir)?;
    let resolution = resolve::resolve(&manifest, root_dir, &env.registry, &env.store)?;

    // Lock-drift detection: a present lock must agree with what we resolved.
    let lock = Lockfile::load(root_dir)?;
    // TOFU: the registry's root signing key must match the one the lock pinned.
    if let Some(pinned) = &lock.registry_root {
        if resolution.runes.iter().any(|r| r.registry.is_some()) {
            let current = env.registry.root_fingerprint()?;
            if &current != pinned {
                return err(format!(
                    "coven registry root key changed: lock pins {pinned} but the registry now presents {current} — refusing to build (possible key compromise). If intentional, delete witchy.lock and re-resolve.",
                ));
            }
        }
    }
    if !lock.runes.is_empty() {
        for r in &resolution.runes {
            if let Some(l) = lock.find(&r.name) {
                if l.hash != r.hash {
                    return err(format!(
                        "lockfile drift for `{}`: lock pins {} but resolution produced {} — run `witchy update`",
                        r.name, l.hash, r.hash
                    ));
                }
            }
        }
    } else if !resolution.runes.is_empty() {
        // Bootstrap a lock so the next build is reproducible.
        save_lock(root_dir, &resolution, env)?;
    }

    // §7.1: a dependency's build step may run only with the build-time
    // capabilities the consuming project explicitly granted it. Enforce
    // grant ⊇ demand (BuildOut is always implicitly granted; everything else
    // must be named in [build.grants]).
    for r in &resolution.runes {
        if r.footprint.build.is_empty() {
            continue;
        }
        let granted = manifest
            .build
            .grants
            .get(&r.name)
            .map(|g| g.granted_kinds())
            .unwrap_or_else(|| ["BuildOut".to_string()].into_iter().collect());
        let missing: Vec<String> = r.footprint.build.difference(&granted).cloned().collect();
        if !missing.is_empty() {
            return err(format!(
                "`{}` build step demands build capabilities you have not granted: {}.\n  \
                 Add a [build.grants.\"{}\"] entry authorizing them (read/exec/net/env).",
                r.name,
                missing.join(", "),
                r.name
            ));
        }
    }

    // Gather every module: the root rune's own, plus each dependency's.
    let mut modules: Vec<(String, String)> = root_src.modules();
    for r in &resolution.runes {
        modules.extend(r.src.modules());
    }
    if modules.is_empty() {
        return err("rune has no src/*.witchy modules");
    }

    // Parse all modules; find the entry (the module defining `main`).
    let mut parsed: Vec<(String, Module)> = Vec::new();
    let mut main_module: Option<String> = None;
    let root_module_names: BTreeSet<String> = root_src.modules().into_iter().map(|(n, _)| n).collect();
    for (name, src) in &modules {
        let m = parser::parse_module(src).map_err(|e| super::PmError(format!("{name}: {e}")))?;
        if main_module.is_none()
            && root_module_names.contains(name)
            && m.items.iter().any(|it| matches!(it, Item::Function(f) if f.name == "main"))
        {
            main_module = Some(name.clone());
        }
        parsed.push((name.clone(), m));
    }

    let has_main = main_module.is_some();
    let entry = main_module
        .or_else(|| root_module_names.iter().next().cloned())
        .ok_or_else(|| super::PmError("no entry module".into()))?;

    let linked = linker::link(parsed, &entry).map_err(|e| super::PmError(e.to_string()))?;
    typeck::check(&linked).map_err(|e| super::PmError(e.to_string()))?;

    Ok(Assembled {
        linked,
        entry,
        has_main,
        resolution,
        manifest,
    })
}

fn cmd_build(rest: &[String]) -> PmResult<()> {
    let _ = rest;
    let env = CovenEnv::load();
    let a = assemble(Path::new("."), &env)?;
    println!(
        "build OK: `{}` ({} dependenc{} resolved, linked + type-checked)",
        a.manifest.rune.name,
        a.resolution.runes.len(),
        if a.resolution.runes.len() == 1 { "y" } else { "ies" }
    );
    if !a.has_main {
        println!("  (library — no `main`; import it from another rune)");
    }
    let agg = a.resolution.aggregate_footprint();
    if agg.is_empty() {
        println!("  dependency tree demands no capabilities.");
    } else {
        println!("  dependency tree max authority: {}", render_footprint(&agg));
    }
    Ok(())
}

fn cmd_run(rest: &[String]) -> PmResult<()> {
    let _ = rest;
    let env = CovenEnv::load();
    let a = assemble(Path::new("."), &env)?;
    if !a.has_main {
        return err(format!(
            "`{}` is a library (no `main` in module `{}`) — nothing to run",
            a.manifest.rune.name, a.entry
        ));
    }
    let output = interpreter::run_module(a.linked, Path::new("."), Vec::new())
        .map_err(|e| super::PmError(e.to_string()))?;
    for line in output {
        println!("{line}");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// add / update — the gate lives here
// ---------------------------------------------------------------------------

fn cmd_add(rest: &[String]) -> PmResult<()> {
    let env = CovenEnv::load();
    let a = parse_args(rest);
    let spec = a
        .positional
        .first()
        .ok_or_else(|| super::PmError("usage: witchy add <pkg>[@version]".into()))?;
    let (name, version_spec) = match spec.split_once('@') {
        Some((n, v)) => (n.to_string(), Some(v.to_string())),
        None => (spec.clone(), None),
    };

    let root = Path::new(".");
    let mut manifest = Manifest::load(root)?;

    // Build the dependency entry.
    let dep = if let Some(path) = a.val("--path") {
        Dep::Detailed(DepDetail {
            version: None,
            path: Some(path.to_string()),
            registry: None,
        })
    } else {
        let registry = a.val("--registry").unwrap_or(DEFAULT_REGISTRY).to_string();
        let reqstr = match version_spec {
            Some(v) => v,
            None => {
                // Default to a caret on the latest released version.
                let latest = env.registry.best_match(&name, &Req::Any, false).ok_or_else(|| {
                    if env.registry.best_match(&name, &Req::Any, true).is_some() {
                        super::PmError(format!(
                            "`{name}` exists but is only STAGED, not released — it must be promoted (second factor) before it can be added"
                        ))
                    } else {
                        super::PmError(format!(
                            "no released version of `{name}` in registry `{registry}` to add"
                        ))
                    }
                })?;
                format!("^{}", latest.version)
            }
        };
        Dep::Detailed(DepDetail {
            version: Some(reqstr),
            path: None,
            registry: Some(registry),
        })
    };
    manifest.dependencies.insert(name.clone(), dep);

    // Resolve with the proposed manifest and run the gate before persisting.
    let resolution = resolve::resolve(&manifest, root, &env.registry, &env.store)?;
    let old_lock = Lockfile::load(root)?;
    let allowed_runtime: BTreeSet<String> = a.vals("--allow-cap").into_iter().collect();
    let allowed_build: BTreeSet<String> = a.vals("--allow-build-cap").into_iter().collect();
    let report = super::gate::check(&resolution, &old_lock, &allowed_runtime, &allowed_build);

    if report.is_blocked() {
        print_block(&name, spec, &report);
        return err("blocked: capability footprint would widen (see above)");
    }

    if a.has("--dry-run") {
        println!("dry run: `{name}` would be added; capability gate OK. Nothing written.");
        return Ok(());
    }

    // Persist manifest + lock.
    manifest.save(root)?;
    save_lock(root, &resolution, &env)?;

    let added = resolution.find(&name);
    match added {
        Some(r) => {
            println!(
                "added `{}`@{} [{}]",
                r.name,
                r.version,
                r.registry.as_deref().or(r.source_kind.as_deref()).unwrap_or("?")
            );
            if r.footprint.is_empty() {
                println!("  demands no capabilities.");
            } else {
                println!("  capability footprint: {}", render_footprint(&r.footprint));
            }
        }
        None => println!("added `{name}`"),
    }
    let agg = resolution.aggregate_footprint();
    println!("  tree max authority now: {}", render_footprint(&agg));
    Ok(())
}

fn cmd_update(rest: &[String]) -> PmResult<()> {
    let env = CovenEnv::load();
    let a = parse_args(rest);
    let root = Path::new(".");
    let manifest = Manifest::load(root)?;
    let resolution = resolve::resolve(&manifest, root, &env.registry, &env.store)?;
    let old_lock = Lockfile::load(root)?;
    let allowed_runtime: BTreeSet<String> = a.vals("--allow-cap").into_iter().collect();
    let allowed_build: BTreeSet<String> = a.vals("--allow-build-cap").into_iter().collect();
    let report = super::gate::check(&resolution, &old_lock, &allowed_runtime, &allowed_build);
    if report.is_blocked() {
        print_block("(update)", "update", &report);
        return err("blocked: capability footprint would widen (see above)");
    }
    save_lock(root, &resolution, &env)?;
    println!("lock updated: {} rune(s) resolved", resolution.runes.len());
    Ok(())
}

fn print_block(_name: &str, spec: &str, report: &super::gate::GateReport) {
    println!("BLOCKED: this change would widen your dependency tree's capability footprint.\n");
    let mut allow_runtime = Vec::new();
    let mut allow_build = Vec::new();
    for (kind, who) in &report.contributors {
        let blocking = report.blocking.runtime.contains(kind) || report.blocking.build.contains(kind);
        if !blocking {
            continue;
        }
        let axis = if report.blocking.build.contains(kind) {
            allow_build.push(kind.clone());
            "build"
        } else {
            allow_runtime.push(kind.clone());
            "runtime"
        };
        println!("  + {kind}  ({axis}) introduced by: {}", who.join(", "));
    }
    for (rune, w) in &report.per_rune {
        println!("  (upgrade) {rune} would additionally demand {}", render_widening(w));
    }
    println!("\nNo authority is granted yet — this is a conscious choice you must make.");
    print!("To accept, re-run:  witchy add {spec}");
    for k in &allow_runtime {
        print!(" --allow-cap {k}");
    }
    for k in &allow_build {
        print!(" --allow-build-cap {k}");
    }
    println!();
}

// ---------------------------------------------------------------------------
// audit / why / why-cap / verify / vendor
// ---------------------------------------------------------------------------

fn cmd_audit(rest: &[String]) -> PmResult<()> {
    let _ = rest;
    let env = CovenEnv::load();
    let root = Path::new(".");
    let manifest = Manifest::load(root)?;
    let resolution = resolve::resolve(&manifest, root, &env.registry, &env.store)?;

    println!("audit: `{}`@{}", manifest.rune.name, manifest.rune.version);
    if resolution.runes.is_empty() {
        println!("  no dependencies.");
    }
    for r in &resolution.runes {
        let where_ = r
            .registry
            .as_deref()
            .map(|reg| format!("registry {reg}"))
            .or_else(|| r.source_kind.clone())
            .unwrap_or_else(|| "?".into());
        let fp = if r.footprint.is_empty() {
            "none".to_string()
        } else {
            render_footprint(&r.footprint)
        };
        println!(
            "  {}@{}  [{}]\n      caps: {}   determinism: {}",
            r.name,
            r.version,
            where_,
            fp,
            r.footprint.determinism()
        );
        match &r.provenance {
            Some(p) => println!("      provenance: {p}"),
            None => println!("      WARNING: no provenance attestation recorded."),
        }
        // Flag yanked deps for registry runes.
        if r.registry.is_some() {
            if let Ok(rec) = env.registry.record(&r.name, &r.version) {
                if rec.state == State::Yanked {
                    println!("      WARNING: this version is YANKED.");
                }
            }
        }
    }
    let agg = resolution.aggregate_footprint();
    println!("\n  AGGREGATE max authority of the whole tree:");
    println!("    runtime: {}", set_or_none(&agg.runtime));
    println!("    build:   {}", set_or_none(&agg.build));
    println!("    determinism: {}", agg.determinism());
    Ok(())
}

fn cmd_why(rest: &[String]) -> PmResult<()> {
    let env = CovenEnv::load();
    let a = parse_args(rest);
    let target = a
        .positional
        .first()
        .ok_or_else(|| super::PmError("usage: witchy why <pkg>".into()))?;
    let root = Path::new(".");
    let manifest = Manifest::load(root)?;
    let resolution = resolve::resolve(&manifest, root, &env.registry, &env.store)?;

    // Build adjacency: rune-name -> child rune-names (via each rune's manifest).
    let mut edges: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let root_children: Vec<String> = resolution
        .runes
        .iter()
        .filter(|r| manifest.dependencies.contains_key(&r.name))
        .map(|r| r.name.clone())
        .collect();
    edges.insert(manifest.rune.name.clone(), root_children);
    for r in &resolution.runes {
        if let Ok(m) = r.src.manifest() {
            let kids: Vec<String> = m
                .dependencies
                .keys()
                .filter(|k| resolution.find(k).is_some())
                .cloned()
                .collect();
            edges.insert(r.name.clone(), kids);
        }
    }

    let mut paths = Vec::new();
    let mut stack = vec![manifest.rune.name.clone()];
    find_paths(&edges, &manifest.rune.name, target, &mut stack, &mut paths);
    if paths.is_empty() {
        println!("`{target}` is not in the dependency tree.");
    } else {
        println!("`{target}` is required via:");
        for p in paths {
            println!("  {}", p.join(" -> "));
        }
    }
    Ok(())
}

fn find_paths(
    edges: &BTreeMap<String, Vec<String>>,
    node: &str,
    target: &str,
    stack: &mut Vec<String>,
    out: &mut Vec<Vec<String>>,
) {
    if node == target && stack.len() > 1 {
        out.push(stack.clone());
        return;
    }
    if let Some(children) = edges.get(node) {
        for c in children {
            if stack.contains(c) {
                continue; // cycle guard
            }
            stack.push(c.clone());
            find_paths(edges, c, target, stack, out);
            stack.pop();
        }
    }
}

fn cmd_why_cap(rest: &[String]) -> PmResult<()> {
    let env = CovenEnv::load();
    let a = parse_args(rest);
    let kind = a
        .positional
        .first()
        .ok_or_else(|| super::PmError("usage: witchy why-cap <Kind>".into()))?;
    let root = Path::new(".");
    let manifest = Manifest::load(root)?;
    let resolution = resolve::resolve(&manifest, root, &env.registry, &env.store)?;
    let mut found = false;
    for r in &resolution.runes {
        if r.footprint.runtime.contains(kind) || r.footprint.build.contains(kind) {
            println!("  {}@{} demands `{kind}`", r.name, r.version);
            found = true;
        }
    }
    if !found {
        println!("no rune in the tree demands `{kind}`.");
    }
    Ok(())
}

fn cmd_verify(rest: &[String]) -> PmResult<()> {
    let _ = rest;
    let env = CovenEnv::load();
    let root = Path::new(".");
    let lock = Lockfile::load(root)?;
    if lock.runes.is_empty() {
        println!("no lockfile to verify (run `witchy build` first).");
        return Ok(());
    }
    let mut ok = true;
    // Confirm the registry's signing key still matches the pinned fingerprint.
    if let Some(pinned) = &lock.registry_root {
        match env.registry.root_fingerprint() {
            Ok(current) if &current == pinned => {
                println!("  OK  coven root key {pinned}");
            }
            Ok(current) => {
                println!("  FAIL coven root key changed: pinned {pinned}, now {current}");
                ok = false;
            }
            Err(e) => {
                println!("  FAIL coven root key unreadable: {e}");
                ok = false;
            }
        }
    }
    for r in &lock.runes {
        // Re-fetch the source and confirm its content hash matches the lock.
        let src = if let Some(src_kind) = &r.source {
            let path = src_kind.strip_prefix("path:").unwrap_or(src_kind);
            RuneSource::read_dir(Path::new(path))
        } else {
            env.registry.fetch(&r.name, &r.version)
        };
        match src {
            Ok(s) if s.hash() == r.hash => {
                println!("  OK  {}@{}  {}", r.name, r.version, short(&r.hash));
            }
            Ok(s) => {
                println!(
                    "  FAIL {}@{}: hash {} != locked {}",
                    r.name,
                    r.version,
                    short(&s.hash()),
                    short(&r.hash)
                );
                ok = false;
            }
            Err(e) => {
                println!("  FAIL {}@{}: {e}", r.name, r.version);
                ok = false;
            }
        }
    }
    if ok {
        println!("all {} locked rune(s) verified.", lock.runes.len());
        Ok(())
    } else {
        err("verification failed (see above)")
    }
}

fn cmd_vendor(rest: &[String]) -> PmResult<()> {
    let _ = rest;
    let env = CovenEnv::load();
    let root = Path::new(".");
    let manifest = Manifest::load(root)?;
    let resolution = resolve::resolve(&manifest, root, &env.registry, &env.store)?;
    let vendor = root.join("vendor");
    for r in &resolution.runes {
        let dir = vendor.join(&r.name);
        std::fs::create_dir_all(&dir)?;
        r.src.write_to(&dir)?;
    }
    println!(
        "vendored {} rune(s) into ./vendor/ — builds are now fully offline & auditable.",
        resolution.runes.len()
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// publish / promote / list
// ---------------------------------------------------------------------------

fn cmd_publish(rest: &[String]) -> PmResult<()> {
    let env = CovenEnv::load();
    let a = parse_args(rest);
    let dir = a.positional.first().map(PathBuf::from).unwrap_or_else(|| ".".into());
    let manifest = Manifest::load(&dir)?;
    let src = RuneSource::read_dir(&dir)?;
    let rec = env.registry.publish(&src, &manifest, &env.user)?;
    println!(
        "published `{}`@{} as STAGED (uploaded by {}).",
        rec.name, rec.version, env.user
    );
    println!("  hash: {}", short(&rec.hash));
    println!("  footprint: runtime={} build={} determinism={}",
        vec_or_none(&rec.runtime_footprint),
        vec_or_none(&rec.build_footprint),
        rec.determinism);
    println!(
        "\n  It is NOT downloadable yet. Releasing requires a separate, out-of-band\n  \
         second-factor promotion by a human:\n      \
         witchy promote {}@{} --factor webauthn",
        rec.name, rec.version
    );
    Ok(())
}

fn cmd_promote(rest: &[String]) -> PmResult<()> {
    let env = CovenEnv::load();
    let a = parse_args(rest);
    let spec = a
        .positional
        .first()
        .ok_or_else(|| super::PmError("usage: witchy promote <pkg>@<version> --factor <type>".into()))?;
    let (name, version) = spec
        .split_once('@')
        .ok_or_else(|| super::PmError("specify the version: <pkg>@<version>".into()))?;
    let factor = a.val("--factor").ok_or_else(|| {
        super::PmError(
            "promotion requires --factor <type> (the out-of-band second factor, e.g. webauthn) — refusing to release".into(),
        )
    })?;
    let promoter = a.val("--as").unwrap_or(&env.user).to_string();

    let p = env.registry.promote(name, version, &promoter, factor)?;
    println!("RELEASED `{name}`@{version}.");
    println!("  promoted by: {promoter}  (second factor: {factor})");
    if p.separation_of_duties {
        println!("  separation of duties: OK — promoter differs from uploader `{}`.", p.record.uploaded_by);
    } else {
        println!(
            "  NOTE: promoter is the same identity as the uploader `{}` — for sensitive\n        \
             namespaces, require a distinct promoter.",
            p.record.uploaded_by
        );
    }
    if p.footprint_delta.is_empty() {
        println!("  capability footprint: unchanged from the prior release.");
    } else {
        println!("  this release NEWLY exposes: {}", render_widening(&p.footprint_delta));
        println!("  (you vouched for this by promoting.)");
    }
    Ok(())
}

fn cmd_yank(rest: &[String]) -> PmResult<()> {
    let env = CovenEnv::load();
    let a = parse_args(rest);
    let spec = a
        .positional
        .first()
        .ok_or_else(|| super::PmError("usage: witchy yank <pkg>@<version>".into()))?;
    let (name, version) = spec
        .split_once('@')
        .ok_or_else(|| super::PmError("specify the version: <pkg>@<version>".into()))?;
    env.registry.yank(name, version)?;
    println!(
        "yanked `{name}`@{version} — excluded from new resolutions; existing locks still resolve it."
    );
    Ok(())
}

fn cmd_list(rest: &[String]) -> PmResult<()> {
    let env = CovenEnv::load();
    let a = parse_args(rest);
    match a.positional.first() {
        Some(name) => {
            let versions = env.registry.versions(name);
            if versions.is_empty() {
                println!("no published versions of `{name}`.");
            } else {
                println!("`{name}`:");
                for v in versions {
                    let state = match v.state {
                        State::Staged => "staged (not resolvable)",
                        State::Released => "released",
                        State::Yanked => "yanked",
                    };
                    println!(
                        "  {}  {}  caps: runtime={} build={}",
                        v.version,
                        state,
                        vec_or_none(&v.runtime_footprint),
                        vec_or_none(&v.build_footprint)
                    );
                }
            }
            Ok(())
        }
        None => {
            // Walk the registry root listing namespaces/names.
            let root = env.registry.root().to_path_buf();
            if !root.exists() {
                println!("registry is empty.");
                return Ok(());
            }
            println!("published runes:");
            list_runes(&root, &root);
            Ok(())
        }
    }
}

fn list_runes(base: &Path, dir: &Path) {
    // A rune directory is one that has version subdirs containing coven.json.
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut names = Vec::new();
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            let has_version = std::fs::read_dir(&p)
                .map(|mut it| it.any(|c| c.map(|c| c.path().join("coven.json").exists()).unwrap_or(false)))
                .unwrap_or(false);
            if has_version {
                names.push(p.strip_prefix(base).unwrap_or(&p).to_string_lossy().replace('\\', "/"));
            } else {
                list_runes(base, &p);
            }
        }
    }
    names.sort();
    for n in names {
        println!("  {n}");
    }
}

// ---------------------------------------------------------------------------
// Rendering helpers
// ---------------------------------------------------------------------------

fn render_footprint(fp: &super::footprint::Footprint) -> String {
    let mut parts = Vec::new();
    if !fp.runtime.is_empty() {
        parts.push(format!("runtime[{}]", fp.runtime.iter().cloned().collect::<Vec<_>>().join(", ")));
    }
    if !fp.build.is_empty() {
        parts.push(format!("build[{}]", fp.build.iter().cloned().collect::<Vec<_>>().join(", ")));
    }
    if parts.is_empty() {
        "none".into()
    } else {
        parts.join(" ")
    }
}

fn render_widening(w: &Widening) -> String {
    let mut parts = Vec::new();
    if !w.runtime.is_empty() {
        parts.push(format!("runtime[{}]", w.runtime.iter().cloned().collect::<Vec<_>>().join(", ")));
    }
    if !w.build.is_empty() {
        parts.push(format!("build[{}]", w.build.iter().cloned().collect::<Vec<_>>().join(", ")));
    }
    parts.join(" ")
}

fn set_or_none(s: &BTreeSet<String>) -> String {
    if s.is_empty() {
        "none".into()
    } else {
        s.iter().cloned().collect::<Vec<_>>().join(", ")
    }
}

fn vec_or_none(v: &[String]) -> String {
    if v.is_empty() {
        "none".into()
    } else {
        v.join(", ")
    }
}

fn short(hash: &str) -> String {
    let h = hash.strip_prefix("sha256:").unwrap_or(hash);
    format!("sha256:{}", &h[..h.len().min(12)])
}
