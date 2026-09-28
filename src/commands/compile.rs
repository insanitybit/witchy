//! Native compilation, artifact emission, and source-cache services.

use witchy::{enforce_performance_modes, trusted_publisher};
use witchy_syntax::opt;
use witchy_lower::codegen;
use witchy_types::pipeline;
use crate::{link_file_checked, link_file_checked_with_deps};
use witchy_types::runtime_type::{ModuleLoadIdentity, PackageCoordinate, PackageSource};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

pub(crate) fn run_compile() -> Result<bool, wasmtime::Error> {
    // `witchy compile <entry> [--dep name=path]... [--out <file.wasm>]` links the
    // entry with explicitly-provided dependency sources, type-checks, and compiles
    // to a wasm binary — the low-level surface the witchy CLI front-end drives to
    // build a multi-rune project (rfcs/0004-self-hosted-cli.md §4). Without `--out`
    // it just verifies the program compiles.
    let source_run = std::env::args().nth(1).as_deref() == Some("run-source");
    if source_run || std::env::args().nth(1).as_deref() == Some("compile") {
        let mut entry: Option<String> = None;
        let mut deps: std::collections::HashMap<String, std::path::PathBuf> =
            std::collections::HashMap::new();
        let mut dep_owners: std::collections::HashMap<String, ModuleLoadIdentity> =
            std::collections::HashMap::new();
        let mut package_owner: Option<ModuleLoadIdentity> = None;
        let mut out: Option<String> = None;
        let mut target = "wasm".to_string();
        let mut manifest: Option<String> = None;
        let mut dir_roots = Vec::new();
        let mut net_allow = Vec::new();
        let mut launch_args = Vec::new();
        let mut argv = std::env::args().skip(2);
        while let Some(a) = argv.next() {
            match a.as_str() {
                "--" if source_run => {
                    launch_args.extend(argv);
                    break;
                }
                "--dir" | "--net" if source_run => {
                    let value = argv.next().ok_or_else(|| wasmtime::Error::msg(format!("{a} requires a value")))?;
                    if a == "--dir" { dir_roots.push(std::path::PathBuf::from(value)); }
                    else { net_allow.push(value); }
                }
                "--dep" => match argv.next().and_then(|s| {
                    s.split_once('=')
                        .map(|(n, p)| (n.to_string(), std::path::PathBuf::from(p)))
                }) {
                    Some((n, p)) => {
                        if deps.insert(n.clone(), p).is_some() {
                            return Err(wasmtime::Error::msg(format!("duplicate dependency `{n}`")));
                        }
                    }
                    None => {
                        eprintln!("--dep needs name=path");
                        std::process::exit(1);
                    }
                },
                "--package-owner" => match parse_module_owner(&mut argv, "--package-owner") {
                    Ok(owner) if package_owner.is_none() => package_owner = Some(owner),
                    Ok(_) => {
                        eprintln!("--package-owner may be supplied only once");
                        std::process::exit(1);
                    }
                    Err(error) => {
                        eprintln!("{error}");
                        std::process::exit(1);
                    }
                },
                "--dep-owner" => {
                    let Some(alias) = argv.next() else {
                        eprintln!("--dep-owner needs alias source package version module");
                        std::process::exit(1);
                    };
                    match parse_module_owner(&mut argv, "--dep-owner") {
                        Ok(owner) => {
                            if dep_owners.contains_key(&alias) {
                                eprintln!("--dep-owner for `{alias}` was supplied more than once");
                                std::process::exit(1);
                            }
                            dep_owners.insert(alias, owner);
                        }
                        Err(error) => {
                            eprintln!("{error}");
                            std::process::exit(1);
                        }
                    }
                }
                "--out" => match argv.next() {
                    Some(f) => out = Some(f),
                    None => {
                        eprintln!("--out needs a file");
                        std::process::exit(1);
                    }
                },
                "--target" => match argv.next() {
                    Some(value) => target = value,
                    None => {
                        eprintln!("--target needs `wasm` or `trusted-publisher`");
                        std::process::exit(1);
                    }
                },
                "--manifest" => match argv.next() {
                    Some(value) => manifest = Some(value),
                    None => {
                        eprintln!("--manifest needs a witchy.toml path");
                        std::process::exit(1);
                    }
                },
                "--release" => {
                    opt::configure("all").map_err(wasmtime::Error::msg)?;
                }
                "--debug" => {
                    opt::configure("none").map_err(wasmtime::Error::msg)?;
                }
                _ if entry.is_none() && !a.starts_with('-') => entry = Some(a),
                _ if source_run => return Err(wasmtime::Error::msg(format!("unknown run-source option `{a}`; application arguments must follow --"))),
                _ => {}
            }
        }
        let Some(entry) = entry else {
            eprintln!(
                "usage: witchy compile <entry.witchy> [--dep name=path]... [--package-owner source package version module] [--dep-owner alias source package version module]... [--target wasm|trusted-publisher] [--manifest witchy.toml] [--out <file>]"
            );
            std::process::exit(1);
        };
        let result = (|| -> Result<(), String> {
            if source_run {
                if out.is_some() || target != "wasm" {
                    return Err("run-source does not emit or execute an external Wasm file".into());
                }
                let claim = package_owner.as_ref().ok_or("run-source requires the resolver's workspace owner assertion")?;
                let linked = crate::source::capture_project_source_with_precompiled(
                    &entry, &deps, manifest.as_deref(), claim, &dep_owners,
                )?;
                enforce_performance_modes(linked.checked.module(), &linked.entry)?;
                let (lines, code) = if linked.external_libraries.is_empty() {
                    super::execution::run_checked_compiled(
                        &linked.checked, dir_roots, Vec::new(), net_allow, Vec::new(),
                        None, None, Vec::new(), launch_args, None, Vec::new(), Vec::new(),
                        true, witchy_confinement::EnforcementMode::BestEffort,
                    )?
                } else {
                    super::execution::run_precompiled_checked_compiled(
                        &linked, dir_roots, Vec::new(), net_allow, Vec::new(),
                        None, None, Vec::new(), launch_args, None, Vec::new(), Vec::new(),
                        true, witchy_confinement::EnforcementMode::BestEffort,
                    )?
                };
                for line in lines { println!("{line}"); }
                std::process::exit(code.unwrap_or(0));
            }
            if target == "trusted-publisher" {
                let output = out.as_ref().ok_or("`--target trusted-publisher` requires `--out <executable>`")?;
                let manifest = manifest.as_ref().ok_or("`--target trusted-publisher` requires `--manifest <witchy.toml>`")?;
                if crate::source::project_declares_precompiled_dependencies(&entry, Some(manifest))? {
                    return Err("trusted-publisher output cannot embed precompiled external libraries".into());
                }
                let (checked, stem, inputs) = if let Some(claim) = &package_owner {
                    crate::source::capture_project_source(&entry, &deps, Some(manifest), claim, &dep_owners)?
                } else if !dep_owners.is_empty() {
                    return Err("dependency owner assertions require --package-owner".into());
                } else {
                    crate::source::capture_packaged_source(&entry, &deps, manifest)?
                };
                enforce_performance_modes(checked.module(), &stem)?;
                let artifact = match codegen::compile_checked_module_artifact(&checked) {
                    codegen::LoweringOutcome::Lowered(artifact) => artifact,
                    codegen::LoweringOutcome::Unsupported(reason) => return Err(reason.to_string()),
                    codegen::LoweringOutcome::Rejected(error) => return Err(error.to_string()),
                };
                let manifest = inputs.files.iter().find(|file| file.path == inputs.target_manifest)
                    .and_then(|file| file.contents.as_deref()).ok_or("captured target manifest missing")?;
                let bindings = trusted_publisher::build_binding_plan(checked.module(), manifest)?;
                let launcher = std::env::current_exe().map_err(|error| error.to_string())?;
                trusted_publisher::package_file(&launcher, std::path::Path::new(output), artifact.wasm(), &bindings, &inputs)?;
                return Ok(());
            }
            if crate::source::project_declares_precompiled_dependencies(&entry, manifest.as_deref())? {
                return Err("precompiled external libraries may only be launched by authenticated `run-source`, not compiled to a standalone Wasm file".into());
            }
            let (checked, stem) = if package_owner.is_some() || !dep_owners.is_empty() {
                let package_owner = package_owner.as_ref().ok_or_else(|| {
                    "authenticated dependency ownership requires --package-owner".to_string()
                })?;
                let (checked, stem, _) = crate::source::capture_project_source(
                    &entry, &deps, None, package_owner, &dep_owners,
                )?;
                (checked, stem)
            } else {
                link_file_checked_with_deps(&entry, &deps)?
            };
            let linked = checked.module();
            enforce_performance_modes(linked, &stem)?;
            let bytes = compile_checked_to_wasm(&checked)?;
            match target.as_str() {
                "wasm" => {
                    if manifest.is_some() {
                        return Err("--manifest applies only to `--target trusted-publisher`".into());
                    }
                    if let Some(f) = &out {
                        std::fs::write(f, &bytes)
                            .map_err(|e| format!("cannot write `{f}`: {e}"))?;
                    }
                }
                other => {
                    return Err(format!(
                        "unknown compile target `{other}` (expected `wasm` or `trusted-publisher`; use `trusted-publisher` for a packaged application)"
                    ));
                }
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                match &out {
                    Some(f) => println!("{entry}: compiled {target} -> {f}"),
                    None => println!("{entry}: ok"),
                }
                return Ok(true);
            }
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
    }
    Ok(false)
}

fn parse_module_owner(
    argv: &mut impl Iterator<Item = String>,
    flag: &str,
) -> Result<ModuleLoadIdentity, String> {
    let source = argv
        .next()
        .ok_or_else(|| format!("{flag} needs source package version module"))?;
    let package = argv
        .next()
        .ok_or_else(|| format!("{flag} needs source package version module"))?;
    let version = argv
        .next()
        .ok_or_else(|| format!("{flag} needs source package version module"))?;
    let module = argv
        .next()
        .ok_or_else(|| format!("{flag} needs source package version module"))?;
    let source = match source.as_str() {
        "toolchain" => PackageSource::Toolchain,
        "workspace" => PackageSource::Workspace,
        value if value.starts_with("registry:") && value.len() > "registry:".len() => {
            PackageSource::Registry(value["registry:".len()..].to_string())
        }
        _ => {
            return Err(format!(
                "{flag} source must be `toolchain`, `workspace`, or `registry:<identity>`"
            ))
        }
    };
    let package = PackageCoordinate::new(source, package, version)
        .map_err(|error| format!("{flag}: {error}"))?;
    let module_path: Vec<&str> = module.split('.').collect();
    ModuleLoadIdentity::new(package, module_path).map_err(|error| format!("{flag}: {error}"))
}

pub(crate) fn run_emit() -> bool {
    // `witchy emit-wat <file>` prints the compiled WebAssembly text — the same
    // module `sandbox` runs — for inspecting/optimizing the generated code.
    if std::env::args().nth(1).as_deref() == Some("emit-wat") {
        let Some(path) = std::env::args().nth(2) else {
            eprintln!("usage: witchy emit-wat <file.witchy>");
            std::process::exit(1);
        };
        match emit_wat_file(&path) {
            Ok(wat) => print!("{wat}"),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
        return true;
    }
    // `witchy emit-wasm <file.witchy> [-o out.wasm]` compiles a program to a wasm
    // BINARY — the Tier-1 distribution artifact. Run it with `witchy <out.wasm>`,
    // which grants its source-derived launch contract plus imported host families.
    if std::env::args().nth(1).as_deref() == Some("emit-wasm") {
        let mut argv = std::env::args().skip(2);
        let mut path: Option<String> = None;
        let mut out: Option<String> = None;
        while let Some(a) = argv.next() {
            match a.as_str() {
                "-o" | "--out" => out = argv.next(),
                _ => path = path.or(Some(a)),
            }
        }
        let Some(path) = path else {
            eprintln!("usage: witchy emit-wasm <file.witchy> [-o out.wasm]");
            std::process::exit(1);
        };
        let out = out.unwrap_or_else(|| {
            std::path::Path::new(&path)
                .file_stem()
                .and_then(|s| s.to_str())
                .map(|s| format!("{s}.wasm"))
                .unwrap_or_else(|| "out.wasm".to_string())
        });
        match emit_wasm_file(&path, &out) {
            Ok(()) => eprintln!("wrote {out}"),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
        return true;
    }
    false
}

/// Compile a program to WebAssembly text (WAT) and return it — the same module
/// `sandbox` would run. For inspecting and optimizing the generated code.
pub(crate) fn emit_wat_file(path: &str) -> Result<String, String> {
    let (checked, stem) = link_file_checked(path)?;
    let linked = checked.module();
    // Honor `mode opt` (BUG-163): `emit-wat` renders the SAME module `sandbox` runs,
    // so a copy-cliff / missing-convention that `check`, `emit-wasm`, and `sandbox`
    // reject must not be quietly rendered here (exit 0 with a copy-cliff file).
    enforce_performance_modes(linked, &stem)?;
    print_admitted_wat(codegen::compile_checked_module_artifact(&checked))
}

/// Render only bytes that crossed canonical admission. The returned WAT is a
/// diagnostic, not an admission receipt for any subsequently assembled bytes.
fn print_admitted_wat(
    outcome: codegen::LoweringOutcome<witchy_wir::AdmittedCanonicalArtifact>,
) -> Result<String, String> {
    let artifact = match outcome {
        codegen::LoweringOutcome::Lowered(artifact) => artifact,
        codegen::LoweringOutcome::Unsupported(reason) => {
            return Err(format!("cannot compile to WASM: {reason}"));
        }
        codegen::LoweringOutcome::Rejected(error) => {
            return Err(format!("cannot compile to WASM: {error}"));
        }
    };
    wasmprinter::print_bytes(artifact.wasm())
        .map_err(|error| format!("cannot print admitted WASM: {error}"))
}

#[cfg(test)]
#[test]
fn emit_wat_preserves_canonical_refusals() {
    let unsupported = codegen::UnsupportedLowering {
        message: "fixture outside canonical coverage".into(),
    };
    let expected = format!("cannot compile to WASM: {unsupported}");
    assert_eq!(
        print_admitted_wat(codegen::LoweringOutcome::Unsupported(unsupported)),
        Err(expected),
    );
    let rejected = codegen::CodegenError {
        message: "fixture canonical admission rejection".into(),
    };
    let expected = format!("cannot compile to WASM: {rejected}");
    assert_eq!(
        print_admitted_wat(codegen::LoweringOutcome::Rejected(rejected)),
        Err(expected),
    );
}

pub(super) fn compile_checked_to_wasm(checked: &pipeline::CheckedModule) -> Result<Vec<u8>, String> {
    let bytes = match codegen::compile_checked_module_binary(checked) {
        codegen::LoweringOutcome::Lowered(bytes) => bytes,
        codegen::LoweringOutcome::Unsupported(reason) => {
            return Err(format!("cannot compile to WASM: {reason}"));
        }
        codegen::LoweringOutcome::Rejected(error) => {
            return Err(format!("cannot compile to WASM: {error}"));
        }
    };
    // Canonical admission already emitted and authenticated `witchy.launch`.
    // These exact admitted bytes must not be mutated by packaging.
    Ok(bytes)
}

pub(super) fn compile_checked_to_development_wasm(
    checked: &pipeline::CheckedModule,
) -> Result<codegen::CompiledDevelopmentModule, String> {
    let compiled = match codegen::compile_checked_development_module(checked) {
        codegen::LoweringOutcome::Lowered(compiled) => compiled,
        codegen::LoweringOutcome::Unsupported(reason) => {
            return Err(format!("cannot compile to WASM: {reason}"));
        }
        codegen::LoweringOutcome::Rejected(error) => {
            return Err(format!("cannot compile to WASM: {error}"));
        }
    };
    // Metadata belongs in the compiler's admitted output, never in a later
    // packaging mutation. The development lowerer remains fail-closed until
    // it can produce that output through canonical admission.
    Ok(compiled)
}

/// A cheap fingerprint of the compiler build: the `witchy` binary's size + mtime.
/// Any recompile of the compiler (or its bundled std) changes it, so the source
/// cache can never serve codegen from an older compiler. A `stat`, not a read, so
/// it costs nothing; computed once per process.
fn compiler_fingerprint() -> &'static str {
    use std::sync::OnceLock;
    static FP: OnceLock<String> = OnceLock::new();
    FP.get_or_init(|| {
        std::env::current_exe()
            .ok()
            .and_then(|p| std::fs::metadata(&p).ok())
            .map(|m| {
                let mt = m
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_nanos())
                    .unwrap_or(0);
                format!("{}-{mt}", m.len())
            })
            .unwrap_or_else(|| "unknown".to_string())
    })
}

/// The active optimization set as a stable string — part of the source-cache key,
/// since every `WITCHY_OPT` setting compiles to different wasm. Reads the same
/// `opt::enabled` the compiler does, so a test override or the env both flow in.
fn active_opt_key() -> String {
    witchy_syntax::opt::active_schema_key().to_string()
}

/// Best-effort publication for compiler caches. The temporary is created
/// exclusively under an unpredictable same-directory name, so an existing
/// path is never followed or truncated. Rename remains the single visibility
/// point for readers.
fn publish_cache_file(path: &std::path::Path, bytes: &[u8]) {
    use std::io::Write as _;

    for _ in 0..32 {
        let mut random = [0u8; 16];
        if getrandom::fill(&mut random).is_err() {
            return;
        }
        let suffix: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
        let temporary = path.with_extension(format!("{suffix}.tmp"));
        let mut file = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => return,
        };
        let published = file.write_all(bytes).is_ok()
            && file.sync_all().is_ok()
            && std::fs::rename(&temporary, path).is_ok();
        if !published {
            let _ = std::fs::remove_file(&temporary);
        }
        return;
    }
}

/// The wasm for an EMBEDDED program (`witchy pm`, `coven-serve`), cached across
/// the WHOLE front-end pipeline — parse, link, typecheck, AND codegen. The
/// embedded sources are `include_str!` constants, so the binary fingerprint
/// covers them exactly: a hit proves THIS binary already parsed/linked/checked/
/// compiled THESE sources successfully, and the ~300ms front-end cost (which
/// dominates a warm `pm` invocation — the source cache below only skips
/// codegen, the last ~90ms) is skipped entirely. Sound by construction like
/// the source cache: a stale key just misses. The capability grant is host
/// policy (CLI flags), never derived from the AST, so skipping the front end
/// changes no authority decision.
pub(crate) fn embedded_wasm_cached(
    name: &str,
    link: impl FnOnce() -> Result<pipeline::CheckedModule, String>,
) -> Result<Vec<u8>, String> {
    let checked = link()?;
    match codegen::compile_checked_module_binary(&checked) {
        codegen::LoweringOutcome::Lowered(bytes) => Ok(bytes),
        codegen::LoweringOutcome::Unsupported(reason) => {
            Err(format!("the embedded {name} {reason}"))
        }
        codegen::LoweringOutcome::Rejected(error) => Err(error.to_string()),
    }
}

/// Compile a checked production module with the proof wrapper, including
/// authenticated loader ownership, in the cache key. Identical source loaded
/// from different packages therefore cannot share runtime type identities or
/// dynamic method catalogs.
pub(crate) fn compile_checked_to_wasm_cached(
    checked: &pipeline::CheckedModule,
) -> Result<Vec<u8>, String> {
    // A detached cache file carries no admitted-artifact authority and can be
    // modified independently. Re-enable this only with an authenticated cache
    // format that verifies an admission receipt before returning bytes.
    compile_checked_to_wasm(checked)
}

/// Compile a checked development module with a small process-local cache. The
/// cache key is the same content-addressable hash used for production codegen,
/// so stale compiler artifacts never replay.
pub(crate) fn compile_checked_to_development_wasm_cached(
    checked: &pipeline::CheckedModule,
) -> Result<codegen::CompiledDevelopmentModule, String> {
    compile_to_development_wasm_cached(checked, || compile_checked_to_development_wasm(checked))
}

fn development_wasm_cache() -> &'static Mutex<HashMap<String, codegen::CompiledDevelopmentModule>> {
    static CACHE: OnceLock<Mutex<HashMap<String, codegen::CompiledDevelopmentModule>>> =
        OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn compile_to_development_wasm_cached(
    cache_input: &impl std::fmt::Debug,
    compile: impl FnOnce() -> Result<codegen::CompiledDevelopmentModule, String>,
) -> Result<codegen::CompiledDevelopmentModule, String> {
    let key = {
        use std::fmt::Write as _;
        let mut w = {
            struct HashWriter(blake3::Hasher);
            impl std::fmt::Write for HashWriter {
                fn write_str(&mut self, s: &str) -> std::fmt::Result {
                    self.0.update(s.as_bytes());
                    Ok(())
                }
            }
            HashWriter(blake3::Hasher::new())
        };
        let _ = write!(w, "{cache_input:?}");
        let mut hasher = w.0;
        hasher.update(b"\0");
        hasher.update(compiler_fingerprint().as_bytes());
        hasher.update(b"\0");
        hasher.update(active_opt_key().as_bytes());
        hasher.finalize().to_hex().to_string()
    };

    if let Ok(cache) = development_wasm_cache().lock()
        && let Some(cached) = cache.get(&key)
    {
        return Ok(cached.clone());
    }

    let compiled = compile()?;
    if let Ok(mut cache) = development_wasm_cache().lock() {
        if cache.len() >= 32 {
            cache.clear();
        }
        cache.insert(key, compiled.clone());
    }
    Ok(compiled)
}

/// Compile a `.witchy` program to a wasm binary and write it to `out`. The
/// produced module is the Tier-1 distribution artifact: run it with `witchy
/// <out>` under its source-derived launch contract and imported host families.
pub(crate) fn emit_wasm_file(path: &str, out: &str) -> Result<(), String> {
    let (checked, stem) = link_file_checked(path)?;
    enforce_performance_modes(checked.module(), stem.as_str())?;
    let binary = compile_checked_to_wasm(&checked)?;
    std::fs::write(out, &binary).map_err(|e| format!("cannot write `{out}`: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod cache_publication_tests {
    use super::publish_cache_file;

    #[cfg(unix)]
    #[test]
    fn cache_publication_never_follows_the_legacy_pid_temporary_symlink() {
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let root = std::env::temp_dir().join(format!(
            "witchy-cache-publication-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let _cleanup = Cleanup(root.clone());
        let path = root.join("cache.wasm");
        let outside = root.join("outside-sentinel");
        std::fs::write(&outside, b"outside-original").unwrap();
        let legacy = path.with_extension(format!("{}.tmp", std::process::id()));
        std::os::unix::fs::symlink(&outside, legacy).unwrap();

        publish_cache_file(&path, b"compiled-wasm");

        assert_eq!(std::fs::read(outside).unwrap(), b"outside-original");
        assert_eq!(std::fs::read(&path).unwrap(), b"compiled-wasm");
        assert!(!std::fs::symlink_metadata(path).unwrap().file_type().is_symlink());
    }
}
