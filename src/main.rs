//! Native Witchy command-line application.
//!
//! This binary composes the compiler pipeline, interpreter and Wasm runtime,
//! capability policy, project tooling, and bundled self-hosted commands.

// This crate is hand-indented, not rustfmt-managed. Clippy's "collapse nested
// conditionals" lints would rewrite explicit `if { if let ... }` nesting into
// `let`-chains without re-indenting, hurting readability; the nested form is an
// intentional style choice here.
#![allow(clippy::collapsible_if, clippy::collapsible_match, clippy::items_after_test_module)]
#![deny(unsafe_code)]

pub use witchy::analysis;
pub use witchy::artifact;
pub use witchy::ast;
pub use witchy::capabilities;
pub use witchy::codegen;
pub use witchy::comptime;
pub use witchy::doc;
pub use witchy::format;
pub use witchy::generators;
pub use witchy::grants;
pub use witchy::interpreter;
pub use witchy::linker;
pub use witchy::{enforce_performance_modes, is_entry_function};
pub use witchy::opt;
pub use witchy::pipeline;
mod cli;
mod commands;
#[cfg(test)]
pub(crate) use commands::build_steps::run_build_step_sandboxed;
#[cfg(test)]
pub(crate) use commands::sandbox::run_file_sandboxed;
#[cfg(test)]
pub(crate) use commands::test_runner::run_tests_in_file;
mod lsp;
mod source;
pub use witchy::native;
pub use witchy::net;
pub use witchy::parser;
mod idp;
pub use witchy::runtime;
pub use witchy::typeck;
pub use witchy::trusted_exe;
pub use witchy::value;
pub use witchy::wir;
pub use witchy::wir_encode;
pub use witchy::wir_helpers;
pub use witchy::wir_opt;

use cli::{
    compiler_version, flag_value, leading_opt_mode, parse_secret_file, parse_secret_inline,
    print_usage,
};
pub(crate) use commands::capabilities::{
    report_capabilities, report_capability_diff, report_grant_check,
};
#[cfg(test)]
pub(crate) use commands::compile::{emit_wasm_file, emit_wat_file};
use commands::execution::run_linked_compiled;
#[cfg(test)]
pub(crate) use commands::execution::{parity_check, ParityOutcome};
#[cfg(test)]
pub(crate) use commands::frontend::check_file;
use runtime::Runtime;
pub(crate) use source::{
    bundled_module, link_file, link_file_checked,
    link_file_checked_with_deps, link_file_with_mode, linked_has_main, project_entry_file,
};
#[cfg(test)]
pub(crate) use source::link_file_with_deps;



/// Run the EMBEDDED witchy package-manager front-end (`projects/pm/src/pm.witchy`)
/// — the cargo-equivalent CLI, itself written in witchy and bundled into the
/// toolchain like std (rfcs/0004-self-hosted-cli.md). `raw` is the front-end's
/// argv (everything after the `witchy` subcommand): `--net <host:port>` flags are
/// extracted into the program's `Net` allowlist, the rest become `main`'s `args`.
/// It runs capability-confined: Console, the project `Dir` (cwd, grant ordinal
/// 0), a `Dir` to the toolchain bin (grant ordinal 1, so it can drive the
/// compiler via `Exec`), `Net`, `Env`, and its argv. This is the sole entry for
/// the front-end's client verbs — both `witchy pm <verb>` and the top-level
/// `witchy <verb>` route here.
fn run_embedded_pm(raw: Vec<String>) -> ! {
    use std::collections::{HashSet, VecDeque};
    let mut net_allow: Vec<String> = Vec::new();
    let mut pm_args: Vec<String> = Vec::new();
    let mut runtime_net: Vec<String> = Vec::new();
    let mut argv = raw.into_iter();
    while let Some(a) = argv.next() {
        if a == "--net" {
            match argv.next() {
                Some(addr) => {
                    net_allow.push(addr.clone());
                    runtime_net.push(addr);
                }
                None => {
                    eprintln!("--net needs a host:port");
                    std::process::exit(1);
                }
            }
        } else {
            pm_args.push(a);
        }
    }
    // (BUG-406) Forward the user's `--net` grants to the front-end as trailing args
    // so `pm run/build` can propagate them to the INNER `sandbox` run of the compiled
    // app — otherwise a program that needs `Net` at runtime is compiled but then run
    // with no address allow-listed, silently losing its grant. These are appended
    // AFTER the user's verb/target (which the pm reads positionally) and do NOT
    // include the COVEN_URL auto-grant below (that is the front-end's own registry
    // reach, not the app's runtime authority).
    for addr in &runtime_net {
        pm_args.push("--net".to_string());
        pm_args.push(addr.clone());
    }
    // Auto-grant Net to the configured registry (COVEN_URL) so registry commands
    // need no explicit `--net`. The front-end reads COVEN_URL itself (via Env)
    // when no host:port argument is given.
    if let Ok(u) = std::env::var("COVEN_URL") {
        let hp = u
            .trim_start_matches("http://")
            .trim_start_matches("https://")
            .trim_end_matches('/');
        if !hp.is_empty() {
            net_allow.push(hp.to_string());
        }
    }
    // The embedded front-end's wasm: whole-pipeline cached (parse+link+check+
    // codegen all skipped on a warm hit — the sources are include_str! constants,
    // so the binary fingerprint keys them exactly; see embedded_wasm_cached).
    let wasm = commands::compile::embedded_wasm_cached("pm", || {
        let mut modules: Vec<(String, ast::Module)> = Vec::new();
        let mut loaded: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<(String, String)> = VecDeque::new();
        queue.push_back(("pm".to_string(), include_str!("../projects/pm/src/pm.witchy").to_string()));
        while let Some((name, source)) = queue.pop_front() {
            if !loaded.insert(name.clone()) {
                continue;
            }
            let module = parser::parse_module(&source).map_err(|e| format!("{name}: {e}"))?;
            for imp in &module.imports {
                if !loaded.contains(imp) {
                    match embedded_pm_module(imp).or_else(|| bundled_module(imp)) {
                        Some(s) => queue.push_back((imp.clone(), s.to_string())),
                        None => return Err(format!("embedded front-end imports `{imp}`, not a bundled module")),
                    }
                }
            }
            modules.push((name, module));
        }
        pipeline::link_checked(modules, "pm").map_err(|e| e.to_string())
    });
    let wasm = match wasm {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    // Canonicalize first: the launcher is commonly a symlink (the cargo-install
    // layout points `~/.cargo/bin/witchy` at `target/release/witchy`). Without
    // resolving it, `bin` is the symlink's directory, and the `Exec` confinement
    // rejects the compiler binary for escaping that Dir via the symlink.
    let bin = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    // (RFC-0004/BUG-550) Run the front-end on the COMPILED WASM tier — the
    // production tier, exactly like `coven-serve` above. The interpreter is the
    // differential-testing oracle ONLY; running the self-hosted pm through the
    // tree-walker made every command pay it (e.g. `pm add` of a glamour dep took
    // ~170s). All of pm's deps — `compiler.footprint`/`diff`/`doc`, `Exec`,
    // `Dir`/`Net`/`Env`/`Clock` — have host functions, so it lowers cleanly.
    // `run_wasm_module` grants exactly the same authority (Dir grant ordinal 0 =
    // cwd, ordinal 1 = bin so `Exec` finds the compiler) and surfaces `main`'s
    // `Int` exit code.
    match run_wasm_module(&wasm, vec![cwd, bin], Vec::new(), net_allow, pm_args, None, Vec::new(), false) {
        Ok((lines, code)) => {
            for l in &lines {
                println!("{l}");
            }
            std::process::exit(code.unwrap_or(0));
        }
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}

fn embedded_pm_module(name: &str) -> Option<&'static str> {
    match name {
        "coven_proto" => Some(include_str!("../projects/coven/src/coven_proto.witchy")),
        "coven_json" => Some(include_str!("../projects/coven/src/coven_json.witchy")),
        "coven_validate" => Some(include_str!("../projects/coven/src/coven_validate.witchy")),
        _ => None,
    }
}

fn main() -> wasmtime::Result<()> {
    // Install the compiler-service natives (footprint/diff/doc), which live
    // above the runtime kernel in `witchy-interp` so the kernel carries no
    // parser/type/caps dependency. The compiled backend's `CompilerServices`
    // default reads this vtable; a trusted program (pm/coven) calling
    // `compiler.footprint` needs it present before any run.
    witchy_interp::compiler_natives::install();
    // RFC-0092: a packaged application is a normal command. Detect its
    // authenticated overlay before interpreting ANY argv as Witchy compiler or
    // grant flags; every token after argv[0] belongs to the application.
    let executable = std::env::current_exe().map_err(wasmtime::Error::from)?;
    match trusted_exe::load(&executable) {
        Ok(Some(application)) => match run_trusted_application(&application) {
            Ok(code) => std::process::exit(code),
            Err(error) => {
                eprintln!("trusted executable startup failed: {error}");
                std::process::exit(1);
            }
        },
        Ok(None) => {}
        Err(error) => {
            eprintln!("trusted executable startup failed: {error}");
            std::process::exit(1);
        }
    }
    // (RFC-0037) `--release` / `--debug` — thin WITCHY_OPT mode selectors, usable with any
    // subcommand. `--debug` compiles with NO optimizations (maximal debuggability); `--release`
    // is the optimized shipping set (also the default when neither is given). Set the mode here,
    // before any codegen reads WITCHY_OPT; an explicit `WITCHY_OPT` env still wins only if
    // neither flag is present. The user-facing run/sandbox arg loops skip these tokens so they
    // aren't mistaken for the program file.
    {
        let a: Vec<String> = std::env::args().skip(1).collect();
        if let Some(m) = leading_opt_mode(&a) {
            opt::configure(m).unwrap_or_else(|e| {
                eprintln!("cannot select `{m}` optimization mode: {e}");
                std::process::exit(2);
            });
        }
    }
    if commands::frontend::run_document() {
        return Ok(());
    }
    if commands::frontend::run_expand() {
        return Ok(());
    }
    // `witchy which <name>` — where does a function live in the standard
    // library? Exact (or substring) matches print module-qualified signatures
    // with their doc line; a near-miss falls back to the closest name.
    if std::env::args().nth(1).as_deref() == Some("which") {
        let Some(name) = std::env::args().nth(2) else {
            eprintln!("usage: witchy which <function-name>");
            std::process::exit(1);
        };
        // A module name lists that module's exports.
        if linker::STD_MODULES.contains(&name.as_str()) {
            for s in linker::module_exports(&name) {
                println!("{s}");
            }
            return Ok(());
        }
        let sigs = linker::std_signatures(&name);
        if !sigs.is_empty() {
            for s in sigs {
                println!("{s}");
            }
            return Ok(());
        }
        match linker::closest_std_function(&name) {
            Some((f, m)) => {
                println!("no std function named `{name}` — did you mean `{m}.{f}`?");
                for s in linker::std_signatures(&f) {
                    println!("{s}");
                }
                return Ok(());
            }
            None => {
                eprintln!("no std function matches `{name}`");
                std::process::exit(1);
            }
        }
    }
    // `witchy caps [file]` reports the host-capability footprint. With no
    // file, inside a project, it analyzes the project's entry module (the
    // whole dependency tree's footprint is `witchy audit`).
    // `witchy stats <file>` (RFC-0030) compiles + runs a Console program and
    // prints its deterministic optimization counters under the active WITCHY_OPT
    // setting — exact counts (heap-frontier bytes, in-place re-owns, region
    // copy-out bytes), not timings, so an optimization's effect is a checkable
    // fact. Diff two runs (e.g. `WITCHY_OPT=all` vs `WITCHY_OPT=-inplace`) to see
    // an optimization fire.
    if std::env::args().nth(1).as_deref() == Some("stats") {
        let Some(path) = std::env::args().nth(2) else {
            eprintln!("usage: witchy stats <file>   (counters honor WITCHY_OPT)");
            std::process::exit(1);
        };
        let src = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("witchy stats: {path}: {e}");
                std::process::exit(1);
            }
        };
        // BUG-177: honor `mode opt` like `check`/`run`/`emit` — a copy-cliff or a
        // missing ownership convention is an error, not a silently-measured stat.
        match link_file(&path) {
            Ok((linked, stem)) => {
                if let Err(e) = enforce_performance_modes(&linked, &stem) {
                    eprintln!("witchy stats: {e}");
                    std::process::exit(1);
                }
            }
            Err(e) => {
                eprintln!("witchy stats: {e}");
                std::process::exit(1);
            }
        }
        match witchy::stats::compute(&src) {
            Ok(s) => {
                println!("heap_bytes {}", s.heap_bytes);
                println!("reowns {}", s.reowns);
                println!("region_copy_bytes {}", s.region_copy_bytes);
                println!("rc_reused_bytes {}", s.rc_reused_bytes);
                println!("live_cells {}", s.live_cells);
                println!("rc_alloc_calls {}", s.rc_alloc_calls);
                println!("bump_alloc_calls {}", s.bump_alloc_calls);
                println!("rc_reuse_calls {}", s.rc_reuse_calls);
                println!("rc_free_calls {}", s.rc_free_calls);
                println!("region_rewind_calls {}", s.region_rewind_calls);
                println!("extract_searches {}", s.extract_searches);
                println!("extract_key_comparisons {}", s.extract_key_comparisons);
                println!("extract_copied_bytes {}", s.extract_copied_bytes);
                println!("extract_retains {}", s.extract_retains);
                println!("extract_drops {}", s.extract_drops);
            }
            Err(e) => {
                eprintln!("witchy stats: {e}");
                std::process::exit(1);
            }
        }
        return Ok(());
    }
    if std::env::args().nth(1).as_deref() == Some("caps") {
        let path = match std::env::args().nth(2) {
            Some(p) => p,
            None => match project_entry_file() {
                Some(p) => p,
                None => {
                    eprintln!(
                        "usage: witchy caps <file>  (or run inside a project — no witchy.toml here)"
                    );
                    std::process::exit(1);
                }
            },
        };
        if let Err(e) = report_capabilities(&path) {
            eprintln!("{e}");
            std::process::exit(1);
        }
        return Ok(());
    }
    // `witchy caps-diff <old> <new>` reports whether the newer version widens the
    // capability footprint, exiting 2 on a widening so it can gate CI/installs.
    if std::env::args().nth(1).as_deref() == Some("caps-diff") {
        let (Some(old), Some(new)) = (std::env::args().nth(2), std::env::args().nth(3)) else {
            eprintln!("usage: witchy caps-diff <old.witchy> <new.witchy>");
            std::process::exit(1);
        };
        match report_capability_diff(&old, &new) {
            Ok(widened) => std::process::exit(if widened { 2 } else { 0 }),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
    }
    // `witchy grants-check <prog.witchy> <grants.toml>` (RFC-0013) cross-checks a
    // grant document against the program's computed footprint: an over-request
    // warns, an under-grant exits 2 (the program would fail at the missing cap).
    if std::env::args().nth(1).as_deref() == Some("grants-check") {
        let (Some(prog), Some(grants)) = (std::env::args().nth(2), std::env::args().nth(3)) else {
            eprintln!("usage: witchy grants-check <prog.witchy> <grants.toml>");
            std::process::exit(1);
        };
        match report_grant_check(&prog, &grants) {
            Ok(under_grant) => std::process::exit(if under_grant { 2 } else { 0 }),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
    }
    // `witchy build-step <file> [--out <dir>] [--read <dir>] [--env <KEY>]... [--exec <tool>]... [--net <host:port>]...`
    // runs a rune's `build` entrypoint under confined grants and reports the
    // source it generated. The build step can only use the build capabilities it
    // is granted here (it cannot forge a runtime cap), so this is the build-time
    // half of the capability model, exercised in isolation.
    if std::env::args().nth(1).as_deref() == Some("build-step") {
        let mut out_dir: Option<std::path::PathBuf> = None;
        let mut read_roots: Vec<std::path::PathBuf> = Vec::new();
        let mut env_keys: Vec<String> = Vec::new();
        let mut exec_tools: Vec<String> = Vec::new();
        let mut net_hosts: Vec<String> = Vec::new();
        let mut path: Option<String> = None;
        let mut argv = std::env::args().skip(2);
        while let Some(a) = argv.next() {
            match a.as_str() {
                "--out" => out_dir = argv.next().map(std::path::PathBuf::from),
                "--read" => {
                    if let Some(d) = argv.next() {
                        read_roots.push(std::path::PathBuf::from(d));
                    }
                }
                "--env" => {
                    if let Some(k) = argv.next() {
                        env_keys.push(k);
                    }
                }
                "--exec" => {
                    if let Some(t) = argv.next() {
                        exec_tools.push(t);
                    }
                }
                "--net" => {
                    if let Some(h) = argv.next() {
                        net_hosts.push(h);
                    }
                }
                _ if path.is_none() => path = Some(a),
                _ => {}
            }
        }
        let Some(path) = path else {
            eprintln!("usage: witchy build-step <file.witchy> [--out <dir>] [--read <dir>]... [--env <KEY>]... [--exec <tool>]... [--net <host:port>]...");
            std::process::exit(1);
        };
        match commands::build_steps::run_build_step_file(&path, out_dir, read_roots, env_keys, exec_tools, net_hosts) {
            Ok(files) if files.is_empty() => println!("{path}: no `build` entrypoint, or it generated no files"),
            Ok(files) => {
                println!("build step generated {} file(s):", files.len());
                for f in files {
                    println!("  {f}");
                }
            }
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
        return Ok(());
    }
    // `witchy lsp` starts the language server (stdio), used by editor extensions.
    if std::env::args().nth(1).as_deref() == Some("lsp") {
        if let Err(e) = lsp::run() {
            eprintln!("witchy lsp: {e}");
            std::process::exit(1);
        }
        return Ok(());
    }
    if commands::frontend::run_check() {
        return Ok(());
    }
    if commands::compile::run_compile()? {
        return Ok(());
    }
    // `witchy pm <args...>` runs the EMBEDDED witchy package-manager front-end
    // (projects/pm/src/pm.witchy) — the cargo-equivalent CLI, itself written in
    // witchy and bundled into the toolchain like std. It runs capability-confined:
    // Console, the project `Dir` (cwd, grant ordinal 0), a `Dir` to the
    // toolchain bin (grant ordinal 1, so it can drive the compiler via `Exec`),
    // `Net`, and its argv.
    // This is the additive bootstrap of rfcs/0004-self-hosted-cli.md §5 — `src/pm`
    // is NOT yet removed; this proves the front-end runs as the embedded CLI.
    if std::env::args().nth(1).as_deref() == Some("pm") {
        run_embedded_pm(std::env::args().skip(2).collect());
    }
    // `witchy coven-serve [--addr H:P] [--root DIR] [--trust-issuer iss=pubhex]...
    // [--signing-key <seed>] [--secret-file signing=<path>]` runs the EMBEDDED witchy
    // registry server (projects/coven/src/coven.witchy) — the self-hosted coven, itself
    // written in witchy and bundled into the toolchain like std + the `pm` front-end.
    // It runs capability-confined: Console, Net (the listen addr), the registry `Dir`
    // (grant ordinal 0), a `SecretStore` holding the root signing key, and a Clock. coven uses
    // `compiler.footprint` + the blocking accept loop, so it runs on the interpreter —
    // exactly like the `witchy pm` bootstrap above. This replaces the Rust `coven-serve`
    // (rfcs/0004-self-hosted-cli.md Phase 5); the IdP helpers `coven-gen-issuer` /
    // `coven-mint-token` stay on the Rust path (test-only key tooling).
    if std::env::args().nth(1).as_deref() == Some("coven-serve") {
        use std::collections::{HashSet, VecDeque};
        let mut addr = "127.0.0.1:8787".to_string();
        let mut root: Option<std::path::PathBuf> = None;
        let mut issuers: Vec<String> = Vec::new();
        let mut signing_key: Option<[u8; 32]> = None;
        let mut argv = std::env::args().skip(2);
        while let Some(a) = argv.next() {
            match a.as_str() {
                "--addr" => match argv.next() {
                    Some(v) => addr = v,
                    None => { eprintln!("--addr needs a host:port"); std::process::exit(1); }
                },
                "--root" => match argv.next() {
                    Some(v) => root = Some(std::path::PathBuf::from(v)),
                    None => { eprintln!("--root needs a directory"); std::process::exit(1); }
                },
                "--trust-issuer" => match argv.next() {
                    // `iss=pubhex`, passed verbatim to coven as a trailing arg.
                    Some(v) => issuers.push(v),
                    None => { eprintln!("--trust-issuer needs iss=pubhex"); std::process::exit(1); }
                },
                "--trust-issuer-jwks" => match argv.next() {
                    // `iss=<jwks-file>`: read the JWKS document and hand coven
                    // `iss=jwks:<json>`, from which it selects the signing key by the
                    // token's `kid` — rotation-tolerant, the form a real OIDC provider
                    // (e.g. GitHub Actions) publishes.
                    Some(v) => match v.split_once('=') {
                        Some((iss, path)) => match std::fs::read_to_string(path) {
                            Ok(doc) => {
                                let compact: String = doc.split_whitespace().collect();
                                issuers.push(format!("{iss}=jwks:{compact}"));
                            }
                            Err(e) => { eprintln!("--trust-issuer-jwks: cannot read `{path}`: {e}"); std::process::exit(1); }
                        },
                        None => { eprintln!("--trust-issuer-jwks needs iss=<jwks-file>"); std::process::exit(1); }
                    },
                    None => { eprintln!("--trust-issuer-jwks needs iss=<jwks-file>"); std::process::exit(1); }
                },
                "--signing-key" => match argv.next() {
                    Some(file) => match load_signing_seed(&file) {
                        Ok(seed) => signing_key = Some(seed),
                        Err(e) => { eprintln!("--signing-key: {e}"); std::process::exit(1); }
                    },
                    None => { eprintln!("--signing-key needs a <seed-file>"); std::process::exit(1); }
                },
                // `--secret-file signing=<path>` is the general form; coven only needs
                // its `signing` secret, which the interpreter seeds from `signing_key`.
                "--secret-file" => match argv.next() {
                    Some(spec) => match spec.split_once('=') {
                        Some(("signing", path)) => match load_signing_seed(path) {
                            Ok(seed) => signing_key = Some(seed),
                            Err(e) => { eprintln!("--secret-file signing: {e}"); std::process::exit(1); }
                        },
                        _ => { eprintln!("coven-serve only accepts `--secret-file signing=<path>`"); std::process::exit(1); }
                    },
                    None => { eprintln!("--secret-file needs signing=<path>"); std::process::exit(1); }
                },
                other => { eprintln!("coven-serve: unknown argument `{other}`"); std::process::exit(1); }
            }
        }
        let Some(root) = root else {
            eprintln!("coven-serve requires --root <dir>");
            std::process::exit(1);
        };
        if signing_key.is_none() {
            eprintln!("coven-serve requires --signing-key <seed> (or --secret-file signing=<path>)");
            std::process::exit(1);
        }
        // coven's argv is [addr, "iss1=hex1", ...]; it binds `args[0]` and reads the
        // rest as `iss=pubhex` trusted issuers (parse_issuers / list.drop(args,1)).
        let mut coven_args: Vec<String> = vec![addr.clone()];
        coven_args.extend(issuers);
        // The embedded coven source set: the entry plus its 8 server-side siblings.
        // (coven_client / coven_test are the test client + unit tests — not bundled.)
        let coven_modules: &[(&str, &str)] = &[
            ("coven", include_str!("../projects/coven/src/coven.witchy")),
            ("coven_validate", include_str!("../projects/coven/src/coven_validate.witchy")),
            ("coven_footprint", include_str!("../projects/coven/src/coven_footprint.witchy")),
            ("coven_record", include_str!("../projects/coven/src/coven_record.witchy")),
            ("coven_json", include_str!("../projects/coven/src/coven_json.witchy")),
            ("coven_store", include_str!("../projects/coven/src/coven_store.witchy")),
            ("coven_trust", include_str!("../projects/coven/src/coven_trust.witchy")),
            ("coven_proto", include_str!("../projects/coven/src/coven_proto.witchy")),
            ("coven_meta", include_str!("../projects/coven/src/coven_meta.witchy")),
        ];
        // Whole-pipeline cached like `witchy pm` above (see embedded_wasm_cached).
        let wasm_result = commands::compile::embedded_wasm_cached("coven", || {
            let embedded: std::collections::HashMap<&str, &str> = coven_modules.iter().copied().collect();
            let mut modules: Vec<(String, ast::Module)> = Vec::new();
            let mut loaded: HashSet<String> = HashSet::new();
            let mut queue: VecDeque<(String, String)> = VecDeque::new();
            queue.push_back(("coven".to_string(), embedded["coven"].to_string()));
            while let Some((name, source)) = queue.pop_front() {
                if !loaded.insert(name.clone()) {
                    continue;
                }
                let module = parser::parse_module(&source).map_err(|e| format!("{name}: {e}"))?;
                for imp in &module.imports {
                    if loaded.contains(imp) {
                        continue;
                    }
                    // A coven_* sibling resolves from the embedded set; everything else
                    // (server, json, crypto, ...) is a bundled std module.
                    match embedded.get(imp.as_str()) {
                        Some(s) => queue.push_back((imp.clone(), s.to_string())),
                        None => match bundled_module(imp) {
                            Some(s) => queue.push_back((imp.clone(), s.to_string())),
                            None => return Err(format!("embedded coven imports `{imp}`, not a bundled or coven module")),
                        },
                    }
                }
                modules.push((name, module));
            }
            pipeline::link_checked(modules, "coven").map_err(|e| e.to_string())
        });
        let wasm = match wasm_result {
            Ok(bytes) => bytes,
            Err(e) => { eprintln!("{e}"); std::process::exit(1); }
        };
        // coven runs on the COMPILED WASM tier — the production tier. It has no
        // interpreter-only dependency (`compiler.footprint`/`diff`/`doc` all have host
        // functions; networking and live logging work compiled), so a registry server
        // gets the compiled tier's speed and the same capability confinement.
        let net_allow = vec![addr];
        let mut caps = runtime::Capabilities {
            print: true,
            print_int: true,
            // A server logs as it runs (live stdout), not captured-then-flushed.
            quiet: false,
            args: coven_args,
            clock: true,
            dir_root: Some(root),
            dir_read: true,
            dir_write: true,
            net_allow: Some(net_allow),
            net_connect: true,
            net_listen: true,
            signing_key,
            ..Default::default()
        };
        if let Some(seed) = signing_key {
            // The signing key is the `signing` named secret, also reachable via
            // `SecretStore.get("signing")`; a bare `Secret` is minted from the
            // same host-side bytes as an opaque externref.
            caps.secrets.push(runtime::SecretGrant::new("signing", seed.to_vec()));
        }
        // `batch()` (no preemption): a server blocks in host accept calls, and we never
        // want the preemption watchdog to interrupt a long-running request.
        let mut rt = match runtime::Runtime::batch() {
            Ok(r) => r,
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        };
        let mut vm = match rt.spawn(&wasm, caps, RUN_MEMORY_PAGES) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        };
        match vm.run() {
            Ok(()) => std::process::exit(0),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
    }
    // `witchy test <file|dir>` runs authority-free in-language tests. The typed
    // parser keeps the opt-in integration grant surface in one place instead of
    // growing another collection of positional `args().nth(..)` reads.
    if std::env::args().nth(1).as_deref() == Some("test") {
        let options = match commands::test_runner::TestOptions::parse(std::env::args().skip(2)) {
            Ok(options) => options,
            Err(e) => {
                eprintln!("{e}\n{}", commands::test_runner::TEST_USAGE);
                std::process::exit(1);
            }
        };
        match commands::test_runner::run_tests(&options) {
            Ok(true) => return Ok(()),
            Ok(false) => std::process::exit(1),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
    }
    if commands::execution::run_parity() {
        return Ok(());
    }
    // `witchy sandbox [--dir <root>] [--net <host:port>]... <file> [args...]`
    // compiles the program to WASM and runs it in the capability-sandboxed VM,
    // granted exactly its computed footprint. `--dir` picks the subtree backing
    // a granted Dir (default `.`); each `--net` allowlists an address.
    if std::env::args().nth(1).as_deref() == Some("sandbox") {
        // Multiple `--dir` grants map positionally to `main`'s `Dir` params:
        // the first backs grant ordinal 0, the rest ordinals 1.. (rfcs/0004-self-hosted-cli.md).
        let mut dir_roots: Vec<std::path::PathBuf> = Vec::new();
        let mut file_grants: Vec<std::path::PathBuf> = Vec::new();
        let mut net_allow: Vec<String> = Vec::new();
        let mut signing_key: Option<[u8; 32]> = None;
        let mut named_secrets: Vec<runtime::SecretGrant> = Vec::new();
        let mut grants_doc: Option<String> = None;
        // RFC-0013: pre-approve the grant (skip the interactive confirmation),
        // for non-interactive launches (CI, installers, scripts).
        let mut accept_grants = false;
        let mut path: Option<String> = None;
        let mut prog_args: Vec<String> = Vec::new();
        let mut argv = std::env::args().skip(2);
        while let Some(a) = argv.next() {
            match a.as_str() {
                "--dir" if path.is_none() => match argv.next() {
                    Some(root) => dir_roots.push(std::path::PathBuf::from(root)),
                    None => {
                        eprintln!("--dir needs a directory");
                        std::process::exit(1);
                    }
                },
                // RFC-0013: grant the whole capability set from a document instead
                // of individual flags (cross-checked against the footprint).
                "--grants" if path.is_none() => match argv.next() {
                    Some(doc) => grants_doc = Some(doc),
                    None => {
                        eprintln!("--grants needs a path to a grant document (.toml)");
                        std::process::exit(1);
                    }
                },
                // RFC-0013: accept the grant without the interactive prompt.
                "--accept-grants" if path.is_none() => accept_grants = true,
                // RFC-0012: each `--file` grants one file to a `main` `File` param,
                // positionally (the i-th `--file` backs the i-th `File` parameter).
                "--file" if path.is_none() => match argv.next() {
                    Some(f) => file_grants.push(std::path::PathBuf::from(f)),
                    None => {
                        eprintln!("--file needs a path");
                        std::process::exit(1);
                    }
                },
                "--net" if path.is_none() => match argv.next() {
                    Some(addr) => net_allow.push(addr),
                    None => {
                        eprintln!("--net needs a host:port");
                        std::process::exit(1);
                    }
                },
                "--signing-key" if path.is_none() => match argv.next() {
                    Some(file) => match load_signing_seed(&file) {
                        Ok(seed) => signing_key = Some(seed),
                        Err(e) => {
                            eprintln!("--signing-key: {e}");
                            std::process::exit(1);
                        }
                    },
                    None => {
                        eprintln!("--signing-key needs a <seed-file>");
                        std::process::exit(1);
                    }
                },
                "--secret" if path.is_none() => match argv.next() {
                    Some(spec) => match parse_secret_inline(&spec) {
                        Ok(s) => named_secrets.push(s),
                        Err(e) => { eprintln!("{e}"); std::process::exit(1); }
                    },
                    None => { eprintln!("--secret needs name=value"); std::process::exit(1); }
                },
                "--secret-file" if path.is_none() => match argv.next() {
                    Some(spec) => match parse_secret_file(&spec) {
                        Ok(s) => named_secrets.push(s),
                        Err(e) => { eprintln!("{e}"); std::process::exit(1); }
                    },
                    None => { eprintln!("--secret-file needs name=path"); std::process::exit(1); }
                },
                // (RFC-0037) mode selectors are handled at top of main; skip so they are not
                // mistaken for the program file.
                "--release" | "--debug" if path.is_none() => {}
                _ if path.is_none() => path = Some(a),
                _ => prog_args.push(a),
            }
        }
        let Some(path) = path else {
            eprintln!("usage: witchy sandbox [--grants <doc.toml> [--accept-grants] | [--dir <root>] [--file <path>]... [--net <host:port>]... [--signing-key <seed-file>] [--secret name=value] [--secret-file name=path]] <file.witchy> [args...]");
            std::process::exit(1);
        };
        // A precompiled `.wasm` runs directly (authority from its launch metadata
        // plus imports); a source is compiled then run with its computed grant.
        // With `--grants`, the whole grant comes from the document (cross-checked).
        let result = if let Some(doc) = grants_doc {
            if path.ends_with(".wasm") {
                eprintln!("--grants applies to a `.witchy` source, not a precompiled `.wasm`");
                std::process::exit(1);
            }
            commands::sandbox::run_file_grants(&path, &doc, accept_grants, prog_args)
        } else if path.ends_with(".wasm") {
            // `sandbox` is the strict path: a `Dir`-importing artifact needs an
            // explicit `--dir` (BUG-106), just like the source form.
            run_wasm_file(&path, dir_roots, file_grants, net_allow, prog_args, signing_key, named_secrets, true)
        } else {
            commands::sandbox::run_file_sandboxed(&path, dir_roots, file_grants, net_allow, prog_args, signing_key, named_secrets)
        };
        match result {
            Ok((lines, exit_code)) => {
                for line in lines {
                    println!("{line}");
                }
                if let Some(code) = exit_code {
                    if code != 0 {
                        std::process::exit(code);
                    }
                }
            }
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
        return Ok(());
    }
    if commands::compile::run_emit() {
        return Ok(());
    }
    // `witchy fmt <file>` rewrites a source file in canonical brace-free form.
    if std::env::args().nth(1).as_deref() == Some("fmt") {
        // `witchy fmt [--check] [--cap-methods] <file.witchy>...` formats (or,
        // with `--check`, verifies) EVERY file argument — a shell glob like
        // `witchy fmt std/*.witchy`
        // expands to many paths, and silently dropping all but the first was a
        // no-op that made callers believe files were formatted (BUG-012).
        // `--check` verifies without rewriting (for CI): exit 1 if any file would
        // change; otherwise 0. Every file is processed even if an earlier one
        // fails, and the exit code is 1 iff any file failed.
        let mut check = false;
        let mut cap_methods = false;
        let mut paths = Vec::new();
        for arg in std::env::args().skip(2) {
            match arg.as_str() {
                "--check" => check = true,
                "--cap-methods" => cap_methods = true,
                _ => paths.push(arg),
            }
        }
        if paths.is_empty() {
            eprintln!("usage: witchy fmt [--check] [--cap-methods] <file.witchy>...");
            std::process::exit(1);
        }
        let mut failed = false;
        for path in &paths {
            match std::fs::read_to_string(path) {
                Ok(src) => match if cap_methods {
                    format::reformat_cap_methods(&src)
                } else {
                    format::reformat(&src)
                } {
                    Some(out) => {
                        if check {
                            if out != src {
                                eprintln!("witchy fmt: `{path}` is not formatted");
                                failed = true;
                            }
                        } else if let Err(e) = std::fs::write(path, out) {
                            eprintln!("witchy fmt: `{path}`: {e}");
                            failed = true;
                        }
                    }
                    None => {
                        eprintln!("witchy fmt: cannot format `{path}` (parse error or unsupported construct)");
                        failed = true;
                    }
                },
                Err(e) => {
                    eprintln!("witchy fmt: cannot read `{path}`: {e}");
                    failed = true;
                }
            }
        }
        if failed {
            std::process::exit(1);
        }
        return Ok(());
    }
    // `witchy --bench` compares interpreter vs compiled execution.
    if std::env::args().nth(1).as_deref() == Some("--bench") {
        return run_benchmarks();
    }
    // Package-manager front-end verbs (`witchy add`, `build`, `publish`, ...) run
    // the EMBEDDED witchy front-end — the same program `witchy pm <verb>` runs,
    // so `witchy add foo` and `witchy pm add foo` are identical. They are checked
    // before the file/`--net` runner so they intercept first. The whole argv
    // (verb included) becomes the front-end's args. `coven-serve` intercepts
    // earlier (its own bootstrap), so it never reaches here.
    {
        let mut frontend_args: Vec<String> = std::env::args().skip(1).collect();
        if matches!(frontend_args.first().map(String::as_str), Some("--release" | "--debug"))
            && frontend_args.len() > 1
        {
            let mode = frontend_args.remove(0);
            frontend_args.insert(1, mode);
        }
        let a1 = frontend_args.first().cloned().unwrap_or_default();
        const FRONTEND_VERBS: &[&str] = &[
            "new", "init", "add", "build", "run", "update", "list", "audit", "tree", "outdated",
            "why", "why-cap", "publish", "promote", "yank", "verify", "vendor",
        ];
        if FRONTEND_VERBS.contains(&a1.as_str()) {
            run_embedded_pm(frontend_args);
        }
        // IdP test tooling (trusted-publishing key/token generation) stays a Rust
        // toolchain helper per RFC-0004 §7.
        if a1 == "coven-gen-issuer" || a1 == "coven-mint-token" || a1 == "coven-issuer-jwks" {
            let rest: Vec<String> = std::env::args().skip(2).collect();
            let result = match a1.as_str() {
                "coven-gen-issuer" => idp::gen_issuer(&rest),
                "coven-issuer-jwks" => idp::issuer_jwks(&rest),
                _ => idp::mint_token(&rest),
            };
            if let Err(e) = result {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
            return Ok(());
        }
    }
    // `witchy [--net <host:port>]... <file.witchy>` runs a program, granting the
    // listed hosts to its `Net` capability (the host decides what authority to
    // hand over). With no file argument, show usage.
    {
        let mut net_allow: Vec<String> = Vec::new();
        let mut file: Option<String> = None;
        let mut prog_args: Vec<String> = Vec::new();
        let mut signing_key: Option<[u8; 32]> = None;
        let mut named_secrets: Vec<runtime::SecretGrant> = Vec::new();
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            if file.is_some() {
                // Everything after the program file is the program's own argv —
                // passed through verbatim (flags here belong to the program).
                prog_args.push(arg);
            } else if arg == "--release" || arg == "--debug" {
                // (RFC-0037) mode selectors — handled at top of main; skip here.
            } else if arg == "--secret" || arg.starts_with("--secret=") {
                let spec = flag_value(&arg, "--secret", &mut args);
                match parse_secret_inline(&spec) {
                    Ok(s) => named_secrets.push(s),
                    Err(e) => { eprintln!("{e}"); std::process::exit(1); }
                }
            } else if arg == "--secret-file" || arg.starts_with("--secret-file=") {
                let spec = flag_value(&arg, "--secret-file", &mut args);
                match parse_secret_file(&spec) {
                    Ok(s) => named_secrets.push(s),
                    Err(e) => { eprintln!("{e}"); std::process::exit(1); }
                }
            } else if let Some(host) = arg.strip_prefix("--net=") {
                net_allow.push(host.to_string());
            } else if arg == "--net" {
                match args.next() {
                    Some(host) => net_allow.push(host),
                    None => {
                        eprintln!("--net requires a <host:port> argument");
                        std::process::exit(1);
                    }
                }
            } else if arg == "--signing-key" || arg.starts_with("--signing-key=") {
                // Grant the root `Secret` from a file holding a 64-hex-char
                // (32-byte) Ed25519 seed — the host decides what key to hand over.
                let path = match arg.strip_prefix("--signing-key=") {
                    Some(p) => p.to_string(),
                    None => match args.next() {
                        Some(p) => p,
                        None => {
                            eprintln!("--signing-key requires a <seed-file> argument");
                            std::process::exit(1);
                        }
                    },
                };
                match load_signing_seed(&path) {
                    Ok(seed) => signing_key = Some(seed),
                    Err(e) => {
                        eprintln!("--signing-key: {e}");
                        std::process::exit(1);
                    }
                }
            } else {
                file = Some(arg);
            }
        }
        match file.as_deref() {
            // Standard version flags report the compiler version (RFC-0061 §5).
            Some("--version" | "-V" | "version") => {
                println!(
                    "{}",
                    compiler_version(
                        env!("CARGO_PKG_VERSION"),
                        option_env!("WITCHY_BUILD_COMMIT"),
                    )
                );
                Ok(())
            }
            // Standard help flags show the usage overview.
            Some("--help" | "-h" | "help") => {
                print_usage();
                Ok(())
            }
            Some(path) if !std::path::Path::new(path).is_file() => {
                // A first argument that is neither a known subcommand nor a real
                // file is almost always a mistyped command — point at usage.
                eprintln!("witchy: `{path}` is not a known command or readable file");
                eprintln!("run `witchy` with no arguments for usage");
                std::process::exit(1);
            }
            // A precompiled program module (`witchy app.wasm`): run it directly,
            // granted exactly the authority its imports declare (Dir rooted at cwd).
            Some(path) if path.ends_with(".wasm") => {
                // Dev run: default a `Dir` to the cwd (not strict) — the convenience
                // the source `witchy <file>` run keeps.
                match run_wasm_file(path, Vec::new(), Vec::new(), net_allow, prog_args, signing_key, named_secrets, false) {
                    Ok((lines, code)) => {
                        for line in lines {
                            println!("{line}");
                        }
                        if let Some(c) = code {
                            if c != 0 {
                                std::process::exit(c);
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("{e}");
                        std::process::exit(1);
                    }
                }
                Ok(())
            }
            Some(path) => {
                match execute_file_exit(path, net_allow, prog_args, signing_key, named_secrets) {
                    Ok((output, code)) => {
                        for line in output {
                            println!("{line}");
                        }
                        // `main`'s `Int` return is the process exit status.
                        if code != 0 {
                            std::process::exit(code);
                        }
                    }
                    Err(e) => {
                        eprintln!("{e}");
                        std::process::exit(1);
                    }
                }
                Ok(())
            }
            // Bare `witchy` (or only flags): show usage.
            None => {
                print_usage();
                Ok(())
            }
        }
    }
}

/// Run a `.witchy` file: resolve `import X` to sibling `X.witchy` files
/// (transitively), link, type-check, then run with root capabilities (Console
/// and a Dir rooted at the file's directory). Returns the program's output or a
/// diagnostic.
/// Time the interpreter against the compiled (WASM) backend on a few workloads.
fn run_benchmarks() -> wasmtime::Result<()> {
    use std::time::Instant;

    fn interp_ms(src: &str, runs: u32) -> f64 {
        let start = Instant::now();
        for _ in 0..runs {
            interpreter::run(src).expect("interpreter run");
        }
        start.elapsed().as_secs_f64() * 1000.0 / runs as f64
    }

    fn compiled_ms(src: &str, runs: u32) -> f64 {
        let module = parser::parse_module(src).expect("parse");
        let bytes = codegen::compile_module_binary(&module)
            .expect_lowered("the binary path lowers this benchmark");
        let mut rt = Runtime::new().expect("runtime");
        let start = Instant::now();
        for _ in 0..runs {
            let mut vm = rt
                .spawn(
                    &bytes,
                    runtime::Capabilities {
                        print: true,
                        print_int: true,
                        ..Default::default()
                    },
                    16,
                )
                .expect("spawn");
            vm.run().expect("run");
        }
        start.elapsed().as_secs_f64() * 1000.0 / runs as f64
    }

    let fib = r#"
fn fib(n: Int) -> Int:
    if (n < 2):
        n
    else:
        (fib((n - 1)) + fib((n - 2)))

fn main() -> Int:
    fib(30)
"#;
    // The accumulator is kept under 10^6 (`% 1000000`) so it stays well within
    // the compiled backend's 32-bit `Int`; otherwise this sum would overflow in
    // the compiled run (which wraps at 2^31) while the i64 interpreter would not,
    // making the two columns incomparable. (Compiled `Int` is i32, a deliberate
    // divergence from the interpreter's i64 — see `ty_kind` in codegen.)
    let loop_sum = r#"
fn main() -> Int:
    var sum = 0
    var i = 0
    while (i < 1000000):
        sum = ((sum + i) % 1000000)
        i = (i + 1)
    sum
"#;

    println!("== witchy benchmarks (avg ms/run) ==");
    for (name, src, runs) in [("fib(30)", fib, 5u32), ("loop_sum(1e6)", loop_sum, 5u32)] {
        let i = interp_ms(src, runs);
        let c = compiled_ms(src, runs);
        println!(
            "{name:14}  interpreter {i:8.2} ms   compiled {c:8.3} ms   ({:.0}x)",
            i / c
        );
    }
    Ok(())
}


/// Read a 32-byte Ed25519 signing seed from a file holding 64 hex characters.
fn load_signing_seed(path: &str) -> Result<[u8; 32], String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read `{path}`: {e}"))?;
    let hex = text.trim();
    if hex.len() != 64 {
        return Err(format!("seed must be 64 hex chars (32 bytes), got {}", hex.len()));
    }
    let mut seed = [0u8; 32];
    for (i, b) in seed.iter_mut().enumerate() {
        *b = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)
            .map_err(|_| "seed is not valid hex".to_string())?;
    }
    Ok(seed)
}


// A no-args convenience wrapper over `execute_file_exit` (which the CLI run path
// uses to also get the process exit code), discarding the exit code — used by the
// test suite.
#[cfg(test)]
fn execute_file(path: &str, net_allow: Vec<String>) -> Result<Vec<String>, String> {
    execute_file_exit(path, net_allow, Vec::new(), None, Vec::new()).map(|(output, _)| output)
}

/// Link, type-check, and run `path`, returning its output and the process exit
/// code (`main`'s `Int` return, else 0). `args` populate a `List(String)`
/// parameter; `signing_key` grants the root `Secret` capability.
fn execute_file_exit(
    path: &str,
    net_allow: Vec<String>,
    args: Vec<String>,
    signing_key: Option<[u8; 32]>,
    named_secrets: Vec<runtime::SecretGrant>,
) -> Result<(Vec<String>, i32), String> {
    let (linked, entry_stem) = link_file(path)?;
    // Same file-context prefix as `check_file` (RFC-0072 phase 2): the CLI
    // names the file, the library message stays path-free.
    typeck::check(&linked).map_err(|e| format!("{path}: {e}"))?;
    enforce_performance_modes(&linked, &entry_stem)?;

    // No `main` means there's nothing to run directly — but the file still
    // compiled. Explain rather than failing with "unknown function `main`".
    if !linked_has_main(&linked) {
        let msg = format!(
            "`{entry_stem}` compiled OK — it's a library (no `main`); import it from another module."
        );
        return Ok((vec![msg], 0));
    }

    // One run path: the compiled (WASM) backend. `witchy run` and `witchy sandbox`
    // share one runtime, so dev == deploy by construction. The interpreter is only
    // the differential oracle (`witchy parity`) and the comptime evaluator — never
    // a user-program run path.
    run_linked_compiled(&linked, Vec::new(), Vec::new(), net_allow, args, signing_key, named_secrets, Vec::new(), false, false)
        .map(|(lines, code)| (lines, code.unwrap_or(0)))
}

/// The `witchy.*` host functions a compiled module imports — its executable
/// authority floor. A Witchy-produced artifact also carries its source-derived
/// root contract in `witchy.launch`; imports remain the fallback for legacy and
/// external wasm and are always unioned with that metadata.
///
/// Reads the import section with `wasmparser` — a streaming parse of one
/// section, not a compile. (This used to call `wasmtime::Module::new` on a
/// fresh uncached `Engine`, running the FULL Cranelift compile — ~30% of a warm
/// `witchy pm <verb>` — only to list imports and throw the code away; the real
/// compile happens at `rt.spawn`, which also rejects any malformed module.)
fn witchy_imports(bytes: &[u8]) -> Result<Vec<String>, String> {
    use wasmparser::{Parser, Payload, TypeRef};
    // wasmparser renders some errors (bad magic) multi-line; keep the CLI's
    // message a single line like the wasmtime-sourced one it replaced.
    fn one_line(e: wasmparser::BinaryReaderError) -> String {
        let msg: String = e.to_string().split_whitespace().collect::<Vec<_>>().join(" ");
        format!("not a valid wasm module: {msg}")
    }
    let mut names = Vec::new();
    for payload in Parser::new(0).parse_all(bytes) {
        match payload.map_err(one_line)? {
            Payload::ImportSection(reader) => {
                for imp in reader.into_imports() {
                    let imp = imp.map_err(one_line)?;
                    if imp.module == "witchy" && matches!(imp.ty, TypeRef::Func(_)) {
                        names.push(imp.name.to_string());
                    }
                }
            }
            // Imports live in a single early section; stop at the first code
            // entry so a large module's body is never scanned here.
            Payload::CodeSectionStart { .. } => break,
            _ => {}
        }
    }
    Ok(names)
}

/// Run a PRECOMPILED program module (`app.wasm`) under the capability sandbox,
/// granting exactly the authority its imports declare. `--dir`/`--net` supply the
/// concrete roots; a module that imports a host op it is not granted simply fails
/// to instantiate. This is the Tier-1 distribution runner: ship the `.wasm`, run
/// it with `witchy`.
#[allow(clippy::too_many_arguments)]
fn run_wasm_file(
    path: &str,
    dir_roots: Vec<std::path::PathBuf>,
    file_grants: Vec<std::path::PathBuf>,
    net_allow: Vec<String>,
    args: Vec<String>,
    signing_key: Option<[u8; 32]>,
    named_secrets: Vec<runtime::SecretGrant>,
    strict_dir: bool,
) -> Result<(Vec<String>, Option<i32>), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read `{path}`: {e}"))?;
    run_wasm_module(&bytes, dir_roots, file_grants, net_allow, args, signing_key, named_secrets, strict_dir)
}

/// Detect, from a compiled wasm program, whether its `main` returns an `Int` — so
/// the process boundary can turn the value into the EXIT CODE rather than printing
/// it (BUG-104). The `run` wrapper codegen emits for an Int-returning `main` is
/// `call $main; call $print_int`; no other `run` shape calls the `print_int` import
/// (a program that itself prints an int does so inside `$main`, a different
/// function). So: the `run` export's body calls the `witchy.print_int` import iff
/// `main` returns `Int`. A malformed module (never produced by our codegen) reads as
/// "no" — the value simply stays a trailing line, the pre-fix behavior.
fn wasm_main_returns_int(bytes: &[u8]) -> bool {
    use wasmparser::{ExternalKind, Operator, Parser, Payload, TypeRef};
    let mut func_imports = 0u32;
    let mut print_int_index: Option<u32> = None;
    let mut run_index: Option<u32> = None;
    let mut code_pos = 0u32;
    for payload in Parser::new(0).parse_all(bytes) {
        let Ok(payload) = payload else { return false };
        match payload {
            Payload::ImportSection(reader) => {
                for imp in reader.into_imports() {
                    let Ok(imp) = imp else { return false };
                    if let TypeRef::Func(_) = imp.ty {
                        if imp.module == "witchy" && imp.name == "print_int" {
                            print_int_index = Some(func_imports);
                        }
                        func_imports += 1;
                    }
                }
            }
            Payload::ExportSection(reader) => {
                for ex in reader {
                    let Ok(ex) = ex else { return false };
                    if ex.kind == ExternalKind::Func && ex.name == "run" {
                        run_index = Some(ex.index);
                    }
                }
            }
            Payload::CodeSectionEntry(body) => {
                // Code entries stream in order; the i-th is defined function index
                // `func_imports + i`. Only the `run` wrapper's body is inspected.
                let this_func = func_imports + code_pos;
                code_pos += 1;
                if Some(this_func) == run_index {
                    let (Some(pi), Ok(reader)) = (print_int_index, body.get_operators_reader()) else {
                        continue;
                    };
                    for op in reader {
                        if let Ok(Operator::Call { function_index }) = op {
                            if function_index == pi {
                                return true;
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    false
}

/// Run a precompiled wasm program from in-memory bytes under the capability
/// sandbox — the byte-level core of [`run_wasm_file`]. `strict_dir` mirrors the
/// source path: an announced/strict launch (`witchy sandbox`) requires an explicit
/// `--dir` when the module imports a `Dir` host op, while the dev `witchy <app.wasm>`
/// path defaults a `Dir` to the cwd.
#[allow(clippy::too_many_arguments)]
fn run_wasm_module(
    bytes: &[u8],
    dir_roots: Vec<std::path::PathBuf>,
    file_grants: Vec<std::path::PathBuf>,
    net_allow: Vec<String>,
    args: Vec<String>,
    signing_key: Option<[u8; 32]>,
    named_secrets: Vec<runtime::SecretGrant>,
    strict_dir: bool,
) -> Result<(Vec<String>, Option<i32>), String> {
    use crate::runtime::Capabilities;
    use witchy_wir::wir_prelude::{abi_import_uses_authority, AbiImportAuthority as Authority};

    let needs = witchy_imports(bytes)?;
    let declared = artifact::launch_contract(bytes)?.unwrap_or_default();
    let declares = |name: &str| declared.contains_key(name);
    let declares_right = |name: &str, right: &str| {
        declared.get(name).is_some_and(|rights| rights.contains(right))
    };
    let imports_authority =
        |authority| needs.iter().any(|name| abi_import_uses_authority(name, authority));
    let dir_grant = imports_authority(Authority::DirGrant);
    let dir_read = imports_authority(Authority::DirRead) || declares_right("Dir", "Read");
    let dir_write = imports_authority(Authority::DirWrite) || declares_right("Dir", "Write");
    let net_grant = imports_authority(Authority::NetGrant);
    let net_connect = imports_authority(Authority::NetConnect) || declares_right("Net", "Connect");
    let net_listen = imports_authority(Authority::NetListen) || declares_right("Net", "Listen");
    let uses_secret_host = imports_authority(Authority::Secret);
    if declares("Secret") && signing_key.is_none() {
        return Err(
            "this program's `main` requires a root `Secret` (the signing key), but none was \
             granted (use `--signing-key <seed-file>`)"
                .to_string(),
        );
    }
    if uses_secret_host && signing_key.is_none() && named_secrets.is_empty() {
        return Err(
            "this module imports the Secret host, but none was granted (use `--signing-key <seed-file>`, `--secret name=value`, or `--secret-file name=path`)".to_string(),
        );
    }
    let mut caps = Capabilities {
        print: true,
        print_int: true,
        quiet: true,
        args,
        ..Default::default()
    };
    if imports_authority(Authority::Clock) || declares("Clock") {
        caps.clock = true;
    }
    if imports_authority(Authority::Rand) || declares("Rand") {
        caps.rand = true;
    }
    if imports_authority(Authority::Env) || declares("Env") {
        caps.env = true;
    }
    if imports_authority(Authority::Exec) || declares("Exec") {
        caps.exec = true;
    }
    if dir_grant || dir_read || dir_write || declares("Dir") {
        let mut roots = dir_roots;
        if roots.is_empty() {
            // Deny by omission (BUG-106): a strict/announced launch (`witchy sandbox
            // app.wasm`) must NOT silently hand a `Dir`-importing module the whole
            // cwd — require an explicit `--dir`, exactly as the source `sandbox` path
            // does. Only the dev `witchy <app.wasm>` path keeps the cwd default.
            if strict_dir {
                return Err(
                    "this module requires a `Dir`, but no subtree was granted (use `--dir <root>`)".to_string(),
                );
            }
            roots.push(std::path::PathBuf::from("."));
        }
        caps.dir_root = Some(roots.remove(0));
        caps.dir_roots = roots;
        caps.dir_read = dir_read;
        caps.dir_write = dir_write;
    }
    // RFC-0012 direct `File` grants — fills `main`'s `File` params positionally and
    // pre-populates the files table. (RFC-0005 Stage 2) The run-wrapper mints each
    // as an `externref` via the `mint_file` host import, so a module importing
    // `mint_file` needs at least one `--file` grant, exactly as a `Dir` importer
    // needs `--dir`.
    if (imports_authority(Authority::FileGrant) || declares("File")) && file_grants.is_empty() {
        return Err(
            "this program's `main` requires a `File`, but none was granted (use `--file <path>`)".to_string(),
        );
    }
    caps.file_grants = file_grants;
    if net_grant || net_connect || net_listen || declares("Net") {
        caps.net_allow = Some(net_allow);
        caps.net_connect = net_connect;
        caps.net_listen = net_listen;
    }
    if uses_secret_host || declares("Secret") || declares("SecretStore") {
        caps.signing_key = signing_key;
        if let Some(seed) = signing_key {
            caps.secrets.push(runtime::SecretGrant::new("signing", seed.to_vec()));
        }
        caps.secrets.extend(named_secrets);
    }
    run_prepared_wasm(bytes, caps)
}

/// Run the application embedded in this process with only its checked target
/// bindings. This bypasses every consumer grant/default path: there is no cwd
/// fallback, prompt, or interpretation of application argv as host policy.
fn run_trusted_application(
    application: &trusted_exe::OwnedEmbeddedApplication,
) -> Result<i32, String> {
    let cwd = std::env::current_dir()
        .map_err(|error| format!("cannot determine launch working directory: {error}"))?;
    let resolved = trusted_exe::resolve_binding_plan(
        &application.bindings,
        &application.wasm,
        &cwd,
    )?;
    let declares = |name: &str| resolved.declared.contains_key(name);
    let declares_right = |name: &str, right: &str| {
        resolved.declared.get(name).is_some_and(|rights| rights.contains(right))
    };
    let mut roots = resolved.dir_roots;
    let mut network_grants = resolved.net_grants;
    let caps = runtime::Capabilities {
        print: true,
        print_int: true,
        quiet: true,
        clock: declares("Clock"),
        rand: declares("Rand"),
        env: declares("Env"),
        dir_root: (!roots.is_empty()).then(|| roots.remove(0)),
        dir_roots: roots,
        dir_rights: resolved.dir_rights,
        dir_read: declares_right("Dir", "Read"),
        dir_write: declares_right("Dir", "Write"),
        file_grants: resolved.file_grants,
        file_rights: resolved.file_rights,
        exec: resolved.exec,
        exec_allow: resolved.exec_allow,
        net_allow: (!network_grants.is_empty()).then(|| network_grants.remove(0)),
        net_grants: network_grants,
        net_connect: declares_right("Net", "Connect"),
        net_listen: declares_right("Net", "Listen"),
        args: std::env::args().skip(1).collect(),
        secrets: resolved.secrets,
        ..Default::default()
    };
    let (lines, exit) = run_prepared_wasm(&application.wasm, caps)?;
    for line in lines {
        println!("{line}");
    }
    Ok(exit.unwrap_or(0))
}

fn run_prepared_wasm(
    bytes: &[u8],
    caps: runtime::Capabilities,
) -> Result<(Vec<String>, Option<i32>), String> {
    let mut rt = Runtime::batch().map_err(|e| e.to_string())?;
    let mut vm = rt
        .spawn(bytes, caps, RUN_MEMORY_PAGES)
        .map_err(|e| e.to_string())?;
    vm.run().map_err(|e| e.root_cause().to_string())?;
    // An Int-returning `main` surfaces its value as the final `print_int` line of the
    // `run` wrapper. We can't read the AST here, but we CAN see that shape in the
    // wasm itself (`wasm_main_returns_int`), so pop the trailing line and use it as
    // the process EXIT CODE — matching the source runners (BUG-104). A non-Int `main`
    // leaves its output untouched.
    let mut lines = vm.output();
    let exit_code = if wasm_main_returns_int(bytes) {
        lines.pop().and_then(|s| s.parse::<i32>().ok())
    } else {
        None
    };
    Ok((lines, exit_code))
}


/// Instantiate a compiled WAT module under print/print_int authority, run its
/// `run` export, and return the captured output lines.
/// Linear-memory cap (64 KiB pages) for a run-to-completion program: 1 GiB.
/// wasmtime grows memory lazily, so this is just a ceiling, not a reservation —
/// it lets real programs (lists, strings, recursion) allocate freely while
/// still bounding a runaway. (The tiny per-VM caps used by the scheduler are
/// a separate, deliberate resource-limit demonstration.)
const RUN_MEMORY_PAGES: usize = 16384;

/// Instantiate a compiled wasm binary under the dev grants and return its
/// captured output. The browser playground assembles the same binary and runs
/// *that*; this is the native equivalent.
fn run_wasm_bytes(bytes: &[u8]) -> Result<Vec<String>, String> {
    use crate::runtime::{Capabilities, Runtime};
    // Run-to-completion: no scheduler, so use the non-preempting engine, which
    // omits the per-backedge epoch check and runs tight loops at full speed.
    let mut rt = Runtime::batch().map_err(|e| e.to_string())?;
    let mut vm = rt
        .spawn(
            bytes,
            // The dev/differential path mirrors the interpreter's automatic
            // grants: output plus the read-only ambient capabilities (Clock/Env)
            // a `main` may declare. The `sandbox` command is the strict path
            // that grants exactly the computed footprint.
            Capabilities {
                print: true,
                print_int: true,
                quiet: true,
                clock: true,
                rand: true,
                env: true,
                dir_root: Some(std::path::PathBuf::from(".")),
                dir_read: true,
                dir_write: true,
                net_allow: Some(Vec::new()),
                net_connect: true,
                net_listen: true,
                ..Default::default()
            },
            RUN_MEMORY_PAGES,
        )
        .map_err(|e| e.to_string())?;
    // (RFC-0045) Surface the ROOT CAUSE, not wasmtime's outer "error while
    // executing at wasm backtrace…" wrapper — so a routed `__witchy_abort` reads
    // as the clean, location-prefixed `runtime error` the interpreter produces,
    // which the differential harness (`parity_check`) compares byte-for-byte.
    vm.run().map_err(|e| e.root_cause().to_string())?;
    Ok(vm.output())
}

/// Run a compiled in-language test under exactly the authority a nullary test
/// needs: captured output plus authority-free runtime support (`__witchy_abort`
/// and heap guards are always linked by the runtime). No Clock/Rand/Env/Dir/File/
/// Net/Secret/Exec imports are granted here; a reached real host-capability use
/// therefore fails closed at instantiation.
fn run_wasm_test_bytes(bytes: &[u8]) -> Result<Vec<String>, String> {
    use crate::runtime::{Capabilities, Runtime};
    let mut rt = Runtime::batch().map_err(|e| e.to_string())?;
    let mut vm = rt
        .spawn(
            bytes,
            Capabilities {
                print: true,
                print_int: true,
                quiet: true,
                test_mocks: true,
                ..Default::default()
            },
            RUN_MEMORY_PAGES,
        )
        .map_err(|e| e.to_string())?;
    vm.run().map_err(|e| e.root_cause().to_string())?;
    Ok(vm.output())
}

/// End-to-end coverage: every shipped example must type-check and produce the
/// expected result (interpreted), or type-check and compile to valid WASM.
#[cfg(test)]
mod example_tests;

/// RFC-0072: verbatim diagnostic goldens over the full error surface (parse,
/// layout, link, type, capability, lowering-reject, runtime trap) via `insta`.
#[cfg(test)]
mod diagnostic_golden_tests;
