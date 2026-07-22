//! Native compilation, artifact emission, and source-cache services.

use witchy::{artifact, enforce_performance_modes, trusted_exe};
use witchy_syntax::{ast, opt};
use witchy_lower::codegen;
use witchy_interp::pipeline;
use witchy_types::typeck;
use witchy_wir::wir;
use crate::{link_file, link_file_checked, link_file_checked_with_deps};
use crate::source::{
    link_file_checked_authenticated_with_deps, AuthenticatedDependency,
};
use witchy_types::runtime_type::{ModuleLoadIdentity, PackageCoordinate, PackageSource};

pub(crate) fn run_compile() -> Result<bool, wasmtime::Error> {
    // `witchy compile <entry> [--dep name=path]... [--out <file.wasm>]` links the
    // entry with explicitly-provided dependency sources, type-checks, and compiles
    // to a wasm binary — the low-level surface the witchy CLI front-end drives to
    // build a multi-rune project (rfcs/0004-self-hosted-cli.md §4). Without `--out`
    // it just verifies the program compiles.
    if std::env::args().nth(1).as_deref() == Some("compile") {
        let mut entry: Option<String> = None;
        let mut deps: std::collections::HashMap<String, std::path::PathBuf> =
            std::collections::HashMap::new();
        let mut dep_owners: std::collections::HashMap<String, ModuleLoadIdentity> =
            std::collections::HashMap::new();
        let mut package_owner: Option<ModuleLoadIdentity> = None;
        let mut out: Option<String> = None;
        let mut target = "wasm".to_string();
        let mut manifest: Option<String> = None;
        let mut argv = std::env::args().skip(2);
        while let Some(a) = argv.next() {
            match a.as_str() {
                "--dep" => match argv.next().and_then(|s| {
                    s.split_once('=')
                        .map(|(n, p)| (n.to_string(), std::path::PathBuf::from(p)))
                }) {
                    Some((n, p)) => {
                        deps.insert(n, p);
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
                        eprintln!("--target needs `wasm` or `trusted-exe`");
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
                _ if entry.is_none() => entry = Some(a),
                _ => {}
            }
        }
        let Some(entry) = entry else {
            eprintln!(
                "usage: witchy compile <entry.witchy> [--dep name=path]... [--package-owner source package version module] [--dep-owner alias source package version module]... [--target wasm|trusted-exe] [--manifest witchy.toml] [--out <file>]"
            );
            std::process::exit(1);
        };
        let result = (|| -> Result<(), String> {
            let (checked, stem) = if package_owner.is_some() || !dep_owners.is_empty() {
                let package_owner = package_owner.ok_or_else(|| {
                    "authenticated dependency ownership requires --package-owner".to_string()
                })?;
                let authenticated = deps
                    .iter()
                    .map(|(alias, path)| {
                        let owner = dep_owners.get(alias).cloned().ok_or_else(|| {
                            format!("dependency `{alias}` is missing --dep-owner metadata")
                        })?;
                        Ok((
                            alias.clone(),
                            AuthenticatedDependency { path: path.clone(), owner },
                        ))
                    })
                    .collect::<Result<std::collections::HashMap<_, _>, String>>()?;
                if let Some(alias) = dep_owners.keys().find(|alias| !deps.contains_key(*alias)) {
                    return Err(format!(
                        "--dep-owner was supplied for unknown dependency `{alias}`"
                    ));
                }
                link_file_checked_authenticated_with_deps(
                    &entry,
                    &authenticated,
                    package_owner,
                )?
            } else {
                link_file_checked_with_deps(&entry, &deps)?
            };
            let linked = checked.module();
            enforce_performance_modes(linked, &stem)?;
            let bytes = compile_checked_to_wasm(&checked)?;
            match target.as_str() {
                "wasm" => {
                    if manifest.is_some() {
                        return Err("--manifest applies only to `--target trusted-exe`".into());
                    }
                    if let Some(f) = &out {
                        std::fs::write(f, &bytes)
                            .map_err(|e| format!("cannot write `{f}`: {e}"))?;
                    }
                }
                "trusted-exe" => {
                    let output = out.as_ref().ok_or_else(|| {
                        "`witchy compile --target trusted-exe` requires `--out <executable>`"
                            .to_string()
                    })?;
                    let manifest = manifest.as_ref().ok_or_else(|| {
                    "`witchy compile --target trusted-exe` requires `--manifest <witchy.toml>`".to_string()
                })?;
                    let source = std::fs::read_to_string(manifest).map_err(|e| {
                        format!("cannot read trusted-exe manifest `{manifest}`: {e}")
                    })?;
                    let bindings = trusted_exe::build_binding_plan(linked, &source)?;
                    let launcher = std::env::current_exe()
                        .map_err(|e| format!("cannot locate trusted-exe launcher template: {e}"))?;
                    trusted_exe::package_file(
                        &launcher,
                        std::path::Path::new(output),
                        &bytes,
                        &bindings,
                    )?;
                }
                other => {
                    return Err(format!(
                        "unknown compile target `{other}` (expected `wasm` or `trusted-exe`)"
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
    let (linked, stem) = link_file(path)?;
    typeck::check(&linked).map_err(|e| e.to_string())?;
    // Honor `mode opt` (BUG-163): `emit-wat` renders the SAME module `sandbox` runs,
    // so a copy-cliff / missing-convention that `check`, `emit-wasm`, and `sandbox`
    // reject must not be quietly rendered here (exit 0 with a copy-cliff file).
    enforce_performance_modes(&linked, &stem)?;
    // The WIR-as-WAT: the actual module the backend encodes and runs
    // (optimization passes included), rendered back to text for inspection —
    // a display of the real WIR, not a separately generated WAT string.
    let wir = match codegen::assemble_optimized_wir_module(&linked) {
        codegen::LoweringOutcome::Lowered(wir) => wir,
        codegen::LoweringOutcome::Unsupported(reason) => {
            return Err(format!("cannot compile to WASM: {reason}"));
        }
        codegen::LoweringOutcome::Rejected(error) => {
            return Err(format!("cannot compile to WASM: {error}"));
        }
    };
    Ok(wir::to_wat(&wir))
}

/// Compile a linked module to a wasm BINARY through the WIR → wasm-binary
/// pipeline (`compile_module_binary`). A program that doesn't fully lower
/// surfaces as a hard "cannot compile" error — there is no WAT fallback.
fn compile_linked_to_wasm(linked: &ast::Module) -> Result<Vec<u8>, String> {
    let bytes = match codegen::compile_module_binary(linked) {
        codegen::LoweringOutcome::Lowered(bytes) => bytes,
        codegen::LoweringOutcome::Unsupported(reason) => {
            return Err(format!("cannot compile to WASM: {reason}"));
        }
        codegen::LoweringOutcome::Rejected(error) => {
            return Err(format!("cannot compile to WASM: {error}"));
        }
    };
    Ok(artifact::embed_launch_contract(bytes, linked))
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
    Ok(artifact::embed_launch_contract(bytes, checked.module()))
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
    use witchy_syntax::opt::{self, Opt};
    Opt::ALL
        .iter()
        .filter(|o| opt::enabled(**o))
        .map(|o| o.name())
        .collect::<Vec<_>>()
        .join(",")
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
    let key = {
        let mut h = blake3::Hasher::new();
        h.update(name.as_bytes());
        h.update(b"\0");
        h.update(compiler_fingerprint().as_bytes());
        h.update(b"\0");
        h.update(active_opt_key().as_bytes());
        h.finalize().to_hex().to_string()
    };
    let path = (|| -> Option<std::path::PathBuf> {
        let base = std::env::var_os("XDG_CACHE_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".cache"))
            })?;
        let dir = base.join("witchy").join("embedded");
        std::fs::create_dir_all(&dir).ok()?;
        Some(dir.join(format!("{key}.wasm")))
    })();
    if let Some(p) = &path {
        if let Ok(bytes) = std::fs::read(p) {
            if !bytes.is_empty() {
                return Ok(bytes);
            }
        }
    }
    let checked = link()?;
    let wasm = match codegen::compile_checked_module_binary(&checked) {
        codegen::LoweringOutcome::Lowered(bytes) => bytes,
        codegen::LoweringOutcome::Unsupported(reason) => {
            return Err(format!("the embedded {name} {reason}"));
        }
        codegen::LoweringOutcome::Rejected(error) => return Err(error.to_string()),
    };
    if let Some(p) = &path {
        // Write-then-rename, pid-tagged temp: same publish discipline as the
        // source cache below.
        let tmp = p.with_extension(format!("{}.tmp", std::process::id()));
        if std::fs::write(&tmp, &wasm).is_ok() {
            let _ = std::fs::rename(&tmp, p);
        }
    }
    Ok(wasm)
}

/// Compile `linked` to wasm, reusing a SOURCE-keyed cache to skip codegen on warm
/// runs. The key hashes the full linked AST + the compiler fingerprint + the active
/// opt set — every input that determines the emitted wasm — so it is sound by
/// construction: a key that fails to reflect some input simply MISSES and recompiles,
/// it can never serve wrong code. Distinct from the runtime's post-Cranelift module
/// caches (`~/.cache/witchy/{optimized-wasm,wasm}`); this one
/// (`~/.cache/witchy/src`) caches the wasm bytes so the front-end's codegen is
/// skipped, not just the native compile. The
/// capability grant and every security check still run from `linked` on every run —
/// only the wasm is cached.
pub(crate) fn compile_linked_to_wasm_cached(linked: &ast::Module) -> Result<Vec<u8>, String> {
    compile_to_wasm_cached(linked, || compile_linked_to_wasm(linked))
}

/// Checked-module twin of [`compile_linked_to_wasm_cached`]. The proof wrapper,
/// including authenticated loader ownership, participates in the cache key so
/// identical source loaded from different packages cannot share runtime type
/// identities or dynamic method catalogs.
pub(crate) fn compile_checked_to_wasm_cached(
    checked: &pipeline::CheckedModule,
) -> Result<Vec<u8>, String> {
    compile_to_wasm_cached(checked, || compile_checked_to_wasm(checked))
}

fn compile_to_wasm_cached(
    cache_input: &impl std::fmt::Debug,
    compile: impl FnOnce() -> Result<Vec<u8>, String>,
) -> Result<Vec<u8>, String> {
    // The AST reaches the hasher STREAMING through a `fmt::Write` adapter: the
    // Debug rendering of a std-linked module runs hundreds of KB, and `format!`
    // used to materialize all of it as a heap String on EVERY run, warm or
    // cold, just to be hashed and dropped. Each formatted fragment now goes
    // straight into the hasher instead. blake3, not sha2: this key is an
    // INTERNAL content-address — its soundness comes from what it depends on
    // (AST + compiler fingerprint + opt set), not from adversarial collision
    // resistance (the cache dir is user-writable anyway) — and blake3 hashes
    // large inputs several times faster. Security-relevant hashing (crypto.*,
    // signing, TUF) stays on sha2/aws-lc untouched.
    struct HashWriter(blake3::Hasher);
    impl std::fmt::Write for HashWriter {
        fn write_str(&mut self, s: &str) -> std::fmt::Result {
            self.0.update(s.as_bytes());
            Ok(())
        }
    }
    let key = {
        use std::fmt::Write as _;
        let mut w = HashWriter(blake3::Hasher::new());
        // Infallible: the adapter never errors, and the AST's derived Debug has
        // no failing formatter.
        let _ = write!(w, "{cache_input:?}");
        let mut h = w.0;
        h.update(b"\0");
        h.update(compiler_fingerprint().as_bytes());
        h.update(b"\0");
        h.update(active_opt_key().as_bytes());
        h.finalize().to_hex().to_string()
    };
    let path = (|| -> Option<std::path::PathBuf> {
        let base = std::env::var_os("XDG_CACHE_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".cache"))
            })?;
        let dir = base.join("witchy").join("src");
        std::fs::create_dir_all(&dir).ok()?;
        Some(dir.join(format!("{key}.wasm")))
    })();
    if let Some(p) = &path {
        if let Ok(bytes) = std::fs::read(p) {
            return Ok(bytes);
        }
    }
    let wasm = compile()?;
    if let Some(p) = &path {
        // Write-then-rename so a concurrent reader never sees a partial file; the
        // pid-tagged temp keeps two processes from racing on one path.
        let tmp = p.with_extension(format!("{}.tmp", std::process::id()));
        if std::fs::write(&tmp, &wasm).is_ok() {
            let _ = std::fs::rename(&tmp, p);
        }
    }
    Ok(wasm)
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
