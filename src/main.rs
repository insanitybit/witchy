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
pub use witchy::{enforce_performance_modes, is_entry_function, ownership_relevant};
pub use witchy::opt;
pub use witchy::pipeline;
mod cli;
mod commands;
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
        match run_build_step_file(&path, out_dir, read_roots, env_keys, exec_tools, net_hosts) {
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
        let options = match TestOptions::parse(std::env::args().skip(2)) {
            Ok(options) => options,
            Err(e) => {
                eprintln!("{e}\n{TEST_USAGE}");
                std::process::exit(1);
            }
        };
        match run_tests(&options) {
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
            run_file_grants(&path, &doc, accept_grants, prog_args)
        } else if path.ends_with(".wasm") {
            // `sandbox` is the strict path: a `Dir`-importing artifact needs an
            // explicit `--dir` (BUG-106), just like the source form.
            run_wasm_file(&path, dir_roots, file_grants, net_allow, prog_args, signing_key, named_secrets, true)
        } else {
            run_file_sandboxed(&path, dir_roots, file_grants, net_allow, prog_args, signing_key, named_secrets)
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

/// Run a program on BOTH backends — the tree-walking interpreter and compiled
/// WebAssembly — and confirm they produce identical output. Witchy's
/// dual-backend equivalence is normally an internal test invariant; `witchy
/// verify` surfaces it as a guarantee you can check on your own code.
/// A failed in-language test: its (qualified) name and the abort message.
type TestFailure = (String, String);

const TEST_USAGE: &str =
    "usage: witchy test [--integration] [--dir <root>]... [--net <addr>]... <file.witchy|dir>";

#[derive(Clone, Debug, Default)]
struct TestGrants {
    dir_roots: Vec<std::path::PathBuf>,
    net_allow: Vec<String>,
}

#[derive(Clone, Debug)]
struct TestOptions {
    path: String,
    integration: bool,
    grants: TestGrants,
}

impl TestOptions {
    fn parse(args: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let mut args = args.into_iter();
        let mut path = None;
        let mut integration = false;
        let mut grants = TestGrants::default();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--integration" => {
                    if integration {
                        return Err("`--integration` may be specified only once".to_string());
                    }
                    integration = true;
                }
                "--dir" => {
                    let root = args
                        .next()
                        .ok_or_else(|| "`--dir` requires a filesystem root".to_string())?;
                    grants.dir_roots.push(std::path::PathBuf::from(root));
                }
                "--net" => {
                    let addr = args
                        .next()
                        .ok_or_else(|| "`--net` requires a host or host:port".to_string())?;
                    grants.net_allow.push(addr);
                }
                flag if flag.starts_with('-') => {
                    return Err(format!("unknown `witchy test` option `{flag}`"));
                }
                value => {
                    if path.replace(value.to_string()).is_some() {
                        return Err("`witchy test` accepts exactly one file or directory".to_string());
                    }
                }
            }
        }
        let path = path.ok_or_else(|| "`witchy test` requires a file or directory".to_string())?;
        if !integration && (!grants.dir_roots.is_empty() || !grants.net_allow.is_empty()) {
            return Err("real `--dir`/`--net` grants require `witchy test --integration`".to_string());
        }
        Ok(Self { path, integration, grants })
    }
}

#[derive(Clone, Copy)]
struct TestRunPolicy<'a> {
    integration: bool,
    real_grants: bool,
    grants: &'a TestGrants,
}

/// Rewrite the placeholder call `witchy_test_target()` in a synthesized test-driver
/// expression to the real (linker-qualified) test name — so the parser never has to
/// re-read `mod.fn` as a method call. The placeholder may sit anywhere in the driver
/// body: bare (`witchy_test_target()`), or as an argument (`task.run(
/// witchy_test_target())`, the async driver), so this recurses through calls,
/// method calls, and unary ops.
fn patch_test_target(expr: &mut ast::Expr, name: &str, params: &[ast::Param]) {
    match expr {
        ast::Expr::Call { name: n, args } => {
            if n == "witchy_test_target" {
                *n = name.to_string();
                *args = params
                    .iter()
                    .map(|param| ast::Expr::Var(param.name.clone()))
                    .collect();
            } else {
                for a in args {
                    patch_test_target(a, name, params);
                }
            }
        }
        ast::Expr::MethodCall { receiver, args, .. } => {
            patch_test_target(receiver, name, params);
            for a in args {
                patch_test_target(a, name, params);
            }
        }
        ast::Expr::Unary { expr, .. } => patch_test_target(expr, name, params),
        _ => {}
    }
}

/// The bare names of every `test_*` function in the UNLOWERED source,
/// split into `(async, gen)` sets. Async lowering (`generators` too) runs during
/// `link`, erasing `is_async`/`is_gen` and rewriting the bodies, so the linked module
/// can no longer tell an async or generator test from a plain one — this recovers
/// that shape from the raw parse. A parse/read failure yields empty sets (the linked
/// module still fails to compile and is reported separately).
fn raw_test_shapes(path: &str) -> (std::collections::HashSet<String>, std::collections::HashSet<String>) {
    let mut async_tests = std::collections::HashSet::new();
    let mut gen_tests = std::collections::HashSet::new();
    if let Ok(src) = std::fs::read_to_string(path) {
        if let Ok(module) = parser::parse_module(&src) {
            for it in &module.items {
                if let ast::Item::Function(f) = it {
                    if f.name.starts_with("test_") {
                        if f.is_async {
                            async_tests.insert(f.name.clone());
                        } else if f.is_gen {
                            gen_tests.insert(f.name.clone());
                        }
                    }
                }
            }
        }
    }
    (async_tests, gen_tests)
}

fn validate_integration_test_params(
    test: &str,
    params: &[ast::Param],
    policy: TestRunPolicy<'_>,
) -> Result<(), String> {
    if !policy.integration && !params.is_empty() {
        return Err(format!(
            "test `{test}` declares capability parameter(s); run it with `witchy test --integration` and explicit grants"
        ));
    }
    if !policy.integration {
        return Ok(());
    }

    let mut dir_count = 0usize;
    let mut needs_net = false;
    for param in params {
        let Some(ty) = param.ty.as_ref() else {
            return Err(format!(
                "integration test `{test}` parameter `{}` needs an explicit capability type (`Console`, `Dir`, or `Net`)",
                param.name
            ));
        };
        let ast::Type::Named(name, _) = ty.unqualified() else {
            return Err(format!(
                "integration test `{test}` parameter `{}` must be a `Console`, `Dir`, or `Net` capability",
                param.name
            ));
        };
        match name.as_str() {
            "Console" => {}
            "Dir" => dir_count += 1,
            "Net" => needs_net = true,
            other => {
                return Err(format!(
                    "integration test `{test}` parameter `{}` has unsupported capability type `{other}`; this tier currently accepts `Console`, `Dir`, and `Net`",
                    param.name
                ));
            }
        }
    }

    if !policy.real_grants && (dir_count > 0 || needs_net) {
        return Err(format!(
            "dependency test `{test}` requests real authority, but dependency tests receive zero real grants even under `--integration`"
        ));
    }
    if policy.grants.dir_roots.len() < dir_count {
        return Err(format!(
            "integration test `{test}` requires {dir_count} `Dir` grant(s), but {} were provided; repeat `--dir <root>`",
            policy.grants.dir_roots.len()
        ));
    }
    if needs_net && policy.grants.net_allow.is_empty() {
        return Err(format!(
            "integration test `{test}` requires a `Net` grant; provide at least one `--net <addr>`"
        ));
    }
    Ok(())
}

/// Discover and run the tests in an already-linked module (`stem` = the entry file's
/// stem). Every function named `test_*` that the ENTRY file itself declares is
/// invoked through a synthesized `main` on compiled WASM. Plain tests take no
/// parameters and receive no real authority; integration tests forward only their
/// declared capability parameters under the caller's explicit grant policy.
/// `async_tests`/`gen_tests` are the bare names of the entry file's async/gen tests
/// (from `raw_test_shapes`, since lowering erased the AST flags). Returns
/// `(passed, failures)` where each failure is `(name, message)`.
fn run_tests_in_module(
    linked: &ast::Module,
    stem: &str,
    async_tests: &std::collections::HashSet<String>,
    gen_tests: &std::collections::HashSet<String>,
    policy: TestRunPolicy<'_>,
) -> Result<(Vec<String>, Vec<TestFailure>), String> {
    typeck::check(linked).map_err(|e| e.to_string())?;
    // BUG-177: a test run honors `mode opt` like `check`/`run` — a copy-cliff or a
    // missing ownership convention fails the run, it is not silently ignored.
    enforce_performance_modes(linked, stem)?;
    // Post-link names are module-qualified (`suite.test_x`); match on the bare name.
    // BUG-185: run only the ENTRY file's OWN tests. Linking pulls an imported
    // module's `test_*` functions into `linked` too (as `othermod.test_x`); running
    // them here would DOUBLE-count them — they run again when that module's own file
    // is swept. `is_entry_function` keeps just `main` + the `{stem}.`-prefixed items.
    let tests: Vec<(String, bool, bool, Vec<ast::Param>)> = linked
        .items
        .iter()
        .filter_map(|it| match it {
            ast::Item::Function(f)
                if is_entry_function(&f.name, stem)
                    && f.name.rsplit('.').next().unwrap_or(&f.name).starts_with("test_") =>
            {
                let bare = f.name.rsplit('.').next().unwrap_or(&f.name);
                Some((
                    f.name.clone(),
                    async_tests.contains(bare),
                    gen_tests.contains(bare),
                    f.params.clone(),
                ))
            }
            _ => None,
        })
        .collect();
    let mut passed = Vec::new();
    let mut failed = Vec::new();
    for (test, is_async, is_gen, params) in tests {
        // BUG-184: an async/gen test's body does NOT run when the function is merely
        // CALLED — calling an `async fn` yields a `Task` and a `gen fn` yields an
        // iterator, both discarded, so a `fail_with` inside never fires and the test
        // FALSELY passes. An `async fn test_*()` is already lowered (when the file was
        // linked) to a `Task(Nil)`-returning function, so DRIVE it to completion with
        // `task.run` — which surfaces the abort. A `gen fn` yields a sequence rather
        // than running to completion, so it cannot be a test; report it as a failure
        // rather than a silent pass.
        if is_gen {
            failed.push((
                test,
                "a `gen fn` cannot be run as a test — it yields a sequence instead of running to completion".to_string(),
            ));
            continue;
        }
        if let Err(e) = validate_integration_test_params(&test, &params, policy) {
            failed.push((test, e));
            continue;
        }
        // Synthesize a `main` (replacing any real one) that runs the test, and run it.
        // The test name is linker-qualified (`suite.test_x`), which the parser would
        // read as a method call — so parse a placeholder and patch the call in the AST.
        // `task.run` is in scope: async lowering imported `task` into a file with any
        // `async fn`, which is exactly the case an async test needs it.
        let mut m = linked.clone();
        m.items
            .retain(|it| !matches!(it, ast::Item::Function(f) if f.name == "main"));
        let driver_src = if is_async {
            "fn main():\n    task.run(witchy_test_target())\n"
        } else {
            "fn main():\n    witchy_test_target()\n"
        };
        let mut driver = parser::parse_module(driver_src).map_err(|e| e.to_string())?;
        for it in &mut driver.items {
            if let ast::Item::Function(f) = it {
                f.params = params.clone();
                if let Some(ast::Stmt::Expr(e)) = f.body.stmts.first_mut() {
                    patch_test_target(e, &test, &params);
                }
            }
        }
        m.items.extend(driver.items);
        // Run the test on the COMPILED WASM tier — the tier users ship — not the
        // interpreter oracle: a `witchy test` that passes must reflect the backend
        // that actually runs in production. A `testing.assert` / `fail_with` lowers
        // to `__witchy_abort`, which is authority-free and always linked by the
        // runtime. Plain tests run under zero real host capability grants;
        // integration tests use the same explicit runtime-grant path as sandbox/run.
        // The synthesized `main` plus codegen's reachability pruning keep unused
        // effectful production functions out of the test artifact. A module that
        // does not lower is itself a failure: the test cannot run where it ships.
        let outcome = if policy.integration {
            run_linked_compiled(
                &m,
                policy.grants.dir_roots.clone(),
                Vec::new(),
                policy.grants.net_allow.clone(),
                Vec::new(),
                None,
                Vec::new(),
                Vec::new(),
                true,
                true,
            )
            .map(|_| ())
        } else {
            match codegen::compile_module_binary(&m) {
                codegen::LoweringOutcome::Lowered(bytes) => {
                    run_wasm_test_bytes(&bytes).map(|_| ())
                }
                codegen::LoweringOutcome::Unsupported(reason) => Err(reason.to_string()),
                codegen::LoweringOutcome::Rejected(error) => Err(error.to_string()),
            }
        };
        match outcome {
            Ok(()) => passed.push(test),
            Err(msg) => failed.push((test, msg)),
        }
    }
    Ok((passed, failed))
}

/// Link `path` and run its own tests — the single-file convenience the test suite
/// drives. Mirrors what `run_tests` does per file (link, recover async/gen shapes,
/// dispatch to `run_tests_in_module`).
#[cfg(test)]
fn run_tests_in_file(path: &str) -> Result<(Vec<String>, Vec<TestFailure>), String> {
    let (linked, stem) = link_file_with_mode(path, linker::LinkMode::Test)?;
    let (async_tests, gen_tests) = raw_test_shapes(path);
    let grants = TestGrants::default();
    run_tests_in_module(
        &linked,
        &stem,
        &async_tests,
        &gen_tests,
        TestRunPolicy { integration: false, real_grants: false, grants: &grants },
    )
}

#[cfg(test)]
mod test_mode_link_tests {
    use super::{link_file, run_tests_in_file};

    fn unique_dir(name: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("witchy_{name}_{}_{}", std::process::id(), nanos))
    }

    #[test]
    fn witchy_test_allows_entry_to_construct_foreign_sealed_data() {
        let dir = unique_dir("sealed_test_mode");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("sealed_lib.witchy"),
            "sealed type Version:\n    Version(Int, Int, Int)\n\n\
             pub fn major(v: Version) -> Int:\n    \
             match v:\n        Version(n, _, _) -> n\n",
        )
        .unwrap();
        let suite = dir.join("suite.witchy");
        std::fs::write(
            &suite,
            "import sealed_lib\nimport testing\n\n\
             fn test_constructs_domain_edge_case():\n    \
             let v = sealed_lib.Version(99, 0, 0)\n    \
             testing.assert_int_eq(sealed_lib.major(v), 99)\n",
        )
        .unwrap();

        let prod = link_file(suite.to_str().unwrap()).expect_err("production link must reject");
        assert!(prod.contains("sealed type") && prod.contains("Version"), "{prod}");

        let (passed, failed) = run_tests_in_file(suite.to_str().unwrap()).expect("test mode links");
        assert!(failed.is_empty(), "{failed:?}");
        assert_eq!(passed, vec!["suite.test_constructs_domain_edge_case".to_string()]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn witchy_test_prunes_unused_effectful_main_under_zero_grant() {
        let dir = unique_dir("zero_grant_prunes_unused_effects");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let suite = dir.join("suite.witchy");
        std::fs::write(
            &suite,
            "import testing\n\n\
             fn main(console: Console, root: Dir[Read]):\n    \
             console.print(root.read(\"secret.txt\"))\n\n\
             fn test_pure_logic():\n    \
             testing.assert_int_eq(2 + 2, 4)\n",
        )
        .unwrap();

        let (passed, failed) = run_tests_in_file(suite.to_str().unwrap())
            .expect("unused effectful main is replaced by the test driver");
        assert!(failed.is_empty(), "{failed:?}");
        assert_eq!(passed, vec!["suite.test_pure_logic".to_string()]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn witchy_test_zero_grant_keeps_abort_diagnostics() {
        let dir = unique_dir("zero_grant_abort");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let suite = dir.join("suite.witchy");
        std::fs::write(
            &suite,
            "import testing\n\n\
             fn test_failure_message():\n    testing.fail_with(\"boom\")\n",
        )
        .unwrap();

        let (passed, failed) = run_tests_in_file(suite.to_str().unwrap())
            .expect("abort host import is authority-free under zero grant");
        assert!(passed.is_empty(), "{passed:?}");
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].0, "suite.test_failure_message");
        assert!(failed[0].1.contains("boom"), "{failed:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn testing_mock_dir_is_test_mode_only() {
        let dir = unique_dir("mock_dir_test_only");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let suite = dir.join("suite.witchy");
        std::fs::write(
            &suite,
            "import testing\n\n\
             fn main():\n    \
             let root = testing.mock_dir([(\"config.txt\", \"ok\")])\n    \
             testing.assert_eq(root.read(\"config.txt\"), \"ok\")\n",
        )
        .unwrap();

        let err = link_file(suite.to_str().unwrap()).expect_err("production link must reject");
        assert!(err.contains("testing.mock_dir") && err.contains("witchy test"), "{err}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn witchy_test_mock_dir_reads_in_memory_tree_under_zero_grant() {
        let dir = unique_dir("mock_dir_zero_grant");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let suite = dir.join("suite.witchy");
        std::fs::write(
            &suite,
            "import testing\n\n\
             fn read_config(root: Dir[Read]) -> String:\n    \
             root.read(\"app/config.txt\")\n\n\
             fn test_mock_dir_read_surface():\n    \
             let root = testing.mock_dir([\n        \
             (\"app/config.txt\", \"ok\"),\n        \
             (\"app/nested/name.txt\", \"Ada\"),\n        \
             (\"README.md\", \"top\")\n    \
             ])\n    \
             testing.assert_eq(read_config(root), \"ok\")\n    \
             testing.assert(root.exists(\"app/config.txt\"), \"file exists\")\n    \
             testing.assert(root.is_dir(\"app\"), \"directory exists\")\n    \
             testing.assert(!root.exists(\"missing.txt\"), \"missing path is false\")\n    \
             let app = root.subtree(\"app\")\n    \
             testing.assert_value_eq(app.list(), [\"config.txt\", \"nested\"])\n    \
             let file = app.read_file(\"nested/name.txt\")\n    \
             testing.assert_eq(file.read(), \"Ada\")\n",
        )
        .unwrap();

        let (passed, failed) = run_tests_in_file(suite.to_str().unwrap())
            .expect("mock Dir runs under zero real grants");
        assert!(failed.is_empty(), "{failed:?}");
        assert_eq!(passed, vec!["suite.test_mock_dir_read_surface".to_string()]);

        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[derive(Debug, Default)]
struct TestPackageOwnership {
    dependency_roots: Vec<std::path::PathBuf>,
}

impl TestPackageOwnership {
    fn resolve(target: &str) -> Result<Self, String> {
        let target = std::fs::canonicalize(target)
            .map_err(|e| format!("cannot resolve test target `{target}`: {e}"))?;
        let start = if target.is_dir() {
            target.as_path()
        } else {
            target.parent().unwrap_or_else(|| std::path::Path::new("."))
        };
        let Some(package_root) = package_root_for(start) else {
            return Ok(Self::default());
        };
        let mut roots = std::collections::BTreeSet::new();
        let mut visited = std::collections::HashSet::new();
        collect_resolved_dependency_roots(&package_root, &mut roots, &mut visited)?;
        Ok(Self { dependency_roots: roots.into_iter().collect() })
    }

    fn owns(&self, file: &str) -> Result<bool, String> {
        let file = std::fs::canonicalize(file)
            .map_err(|e| format!("cannot resolve test file `{file}`: {e}"))?;
        Ok(!self.dependency_roots.iter().any(|root| file.starts_with(root)))
    }
}

fn package_root_for(start: &std::path::Path) -> Option<std::path::PathBuf> {
    let mut at = Some(start);
    while let Some(dir) = at {
        if dir.join("witchy.toml").is_file() {
            return std::fs::canonicalize(dir).ok();
        }
        at = dir.parent();
    }
    None
}

fn dependency_alias(name: &str) -> &str {
    name.rsplit('/').next().unwrap_or(name)
}

fn locked_registry_aliases(package_root: &std::path::Path) -> Result<std::collections::HashSet<String>, String> {
    let path = package_root.join("witchy.lock");
    if !path.is_file() {
        return Ok(std::collections::HashSet::new());
    }
    let source = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read `{}`: {e}", path.display()))?;
    let lock: toml::Value = toml::from_str(&source)
        .map_err(|e| format!("cannot parse `{}`: {e}", path.display()))?;
    let mut aliases = std::collections::HashSet::new();
    for entry in lock.get("rune").and_then(toml::Value::as_array).into_iter().flatten() {
        if entry.get("source").and_then(toml::Value::as_str) != Some("coven") {
            continue;
        }
        let alias = entry
            .get("alias")
            .and_then(toml::Value::as_str)
            .or_else(|| entry.get("name").and_then(toml::Value::as_str).map(dependency_alias));
        if let Some(alias) = alias {
            aliases.insert(alias.to_string());
        }
    }
    Ok(aliases)
}

fn resolved_dependency_dirs(package_root: &std::path::Path) -> Result<Vec<std::path::PathBuf>, String> {
    let manifest_path = package_root.join("witchy.toml");
    let source = std::fs::read_to_string(&manifest_path)
        .map_err(|e| format!("cannot read `{}`: {e}", manifest_path.display()))?;
    let manifest: toml::Value = toml::from_str(&source)
        .map_err(|e| format!("cannot parse `{}`: {e}", manifest_path.display()))?;
    let Some(dependencies) = manifest.get("dependencies").and_then(toml::Value::as_table) else {
        return Ok(Vec::new());
    };
    let registry_aliases = locked_registry_aliases(package_root)?;
    let mut dirs = Vec::new();
    for (name, declaration) in dependencies {
        let resolved = declaration
            .as_table()
            .and_then(|inline| inline.get("path"))
            .and_then(toml::Value::as_str)
            .map(|path| package_root.join(path))
            .or_else(|| {
                let alias = dependency_alias(name);
                registry_aliases
                    .contains(alias)
                    .then(|| package_root.join("vendor").join(alias))
            });
        if let Some(dir) = resolved
            && dir.is_dir()
        {
            dirs.push(
                std::fs::canonicalize(&dir)
                    .map_err(|e| format!("cannot resolve dependency `{}`: {e}", dir.display()))?,
            );
        }
    }
    Ok(dirs)
}

fn collect_resolved_dependency_roots(
    package_root: &std::path::Path,
    roots: &mut std::collections::BTreeSet<std::path::PathBuf>,
    visited: &mut std::collections::HashSet<std::path::PathBuf>,
) -> Result<(), String> {
    let package_root = std::fs::canonicalize(package_root)
        .map_err(|e| format!("cannot resolve package `{}`: {e}", package_root.display()))?;
    if !visited.insert(package_root.clone()) {
        return Ok(());
    }
    for dependency in resolved_dependency_dirs(&package_root)? {
        roots.insert(dependency.clone());
        collect_resolved_dependency_roots(&dependency, roots, visited)?;
    }
    Ok(())
}

/// `witchy test <file|dir>`: run in-language tests, print a cargo-style
/// report, and return whether everything passed.
fn run_tests(options: &TestOptions) -> Result<bool, String> {
    let path = &options.path;
    let mut files: Vec<String> = Vec::new();
    let meta = std::fs::metadata(path).map_err(|e| format!("cannot read `{path}`: {e}"))?;
    let ownership = TestPackageOwnership::resolve(path)?;
    if meta.is_dir() {
        // Collect every `.witchy` under the directory recursively, so a rune's
        // `src/` modules (and the nested runes of a multi-rune project) are all
        // discovered — `witchy test <rune-dir>` runs the whole package's tests.
        fn collect(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) -> std::io::Result<()> {
            let mut entries: Vec<_> =
                std::fs::read_dir(dir)?.filter_map(|e| e.ok()).map(|e| e.path()).collect();
            entries.sort();
            for p in entries {
                if p.is_dir() {
                    collect(&p, out)?;
                } else if p.extension().and_then(|s| s.to_str()) == Some("witchy") {
                    out.push(p);
                }
            }
            Ok(())
        }
        let mut paths = Vec::new();
        collect(std::path::Path::new(path), &mut paths)
            .map_err(|e| format!("cannot read `{path}`: {e}"))?;
        files.extend(paths.into_iter().filter_map(|p| p.to_str().map(String::from)));
    } else {
        files.push(path.to_string());
    }
    let mut total_pass = 0usize;
    let mut total_fail = 0usize;
    for file in &files {
        // Distinguish a LINK failure from a post-link (compile) failure (BUG-120).
        // In a directory sweep, a file that can't LINK standalone — a module of a
        // multi-rune project that imports a sibling rune via a path dependency, which
        // resolves no local `<import>.witchy` — is skipped, not fatal. But a file
        // that links yet fails to TYPE-CHECK (or violates `mode opt`) is a genuinely
        // BROKEN test file: it must FAIL the run, never be silently skipped as
        // "ok. 0 passed". An explicit single file surfaces even a link error.
        let (linked, stem) = match link_file_with_mode(file, linker::LinkMode::Test) {
            Ok(v) => v,
            Err(e) if meta.is_dir() => {
                eprintln!("  skipped {file}: {e}");
                continue;
            }
            Err(e) => return Err(e),
        };
        let (async_tests, gen_tests) = raw_test_shapes(file);
        let owns_test = ownership.owns(file)?;
        let no_real_grants = TestGrants::default();
        let grants = if owns_test { &options.grants } else { &no_real_grants };
        let policy = TestRunPolicy {
            integration: options.integration,
            real_grants: options.integration && owns_test,
            grants,
        };
        let (passed, failed) = match run_tests_in_module(
            &linked,
            &stem,
            &async_tests,
            &gen_tests,
            policy,
        ) {
            Ok(r) => r,
            Err(e) => {
                // Linked OK but broken (a type error or a `mode opt` violation): count
                // it as a failure so the run exits non-zero (BUG-120).
                println!("running test(s) in {file}");
                println!("test {file} ... FAILED to compile: {e}");
                total_fail += 1;
                continue;
            }
        };
        if passed.is_empty() && failed.is_empty() {
            continue;
        }
        println!("running {} test(s) in {file}", passed.len() + failed.len());
        for name in &passed {
            println!("test {name} ... ok");
        }
        for (name, msg) in &failed {
            println!("test {name} ... FAILED: {msg}");
        }
        total_pass += passed.len();
        total_fail += failed.len();
    }
    println!(
        "\ntest result: {}. {total_pass} passed; {total_fail} failed",
        if total_fail == 0 { "ok" } else { "FAILED" }
    );
    Ok(total_fail == 0)
}

fn fs_rights_from_type_args(args: &[ast::Type]) -> runtime::FsRights {
    if args.is_empty() {
        return runtime::FsRights::new(true, true);
    }
    let has = |needle: &str| {
        args.iter()
            .any(|a| matches!(a, ast::Type::Named(n, inner) if n == needle && inner.is_empty()))
    };
    runtime::FsRights::new(has("Read"), has("Write"))
}

fn main_fs_param_rights(linked: &ast::Module) -> (Vec<runtime::FsRights>, Vec<runtime::FsRights>) {
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    if let Some(ast::Item::Function(main)) =
        linked.items.iter().find(|it| matches!(it, ast::Item::Function(f) if f.name == "main"))
    {
        for p in &main.params {
            match &p.ty {
                Some(ast::Type::Named(n, args)) if n == "Dir" => dirs.push(fs_rights_from_type_args(args)),
                Some(ast::Type::Named(n, args)) if n == "File" => files.push(fs_rights_from_type_args(args)),
                _ => {}
            }
        }
    }
    (dirs, files)
}

fn grant_fs_rights(label: &str, rights: &[String]) -> Result<runtime::FsRights, String> {
    let mut out = runtime::FsRights::new(false, false);
    for r in rights {
        match r.as_str() {
            "Read" => out.read = true,
            "Write" => out.write = true,
            other => return Err(format!("grant `{label}` names unknown filesystem right `{other}`")),
        }
    }
    Ok(out)
}

fn require_exact_fs_rights(
    label: &str,
    declared: runtime::FsRights,
    required: runtime::FsRights,
) -> Result<(), String> {
    if declared == required {
        Ok(())
    } else {
        Err(format!(
            "grant `{label}` rights {:?} do not match `main` parameter rights {:?}",
            declared, required
        ))
    }
}

/// Compile a program and run it in the WASM VM granted EXACTLY its computed
/// footprint, announcing the grant on stderr. The `Dir` root and `Net` allowlist
/// are host policy (the `--dir`/`--net` flags); the program's footprint decides
/// whether they are granted at all.
fn run_file_sandboxed(
    path: &str,
    dir_roots: Vec<std::path::PathBuf>,
    file_grants: Vec<std::path::PathBuf>,
    net_allow: Vec<String>,
    args: Vec<String>,
    signing_key: Option<[u8; 32]>,
    named_secrets: Vec<runtime::SecretGrant>,
) -> Result<(Vec<String>, Option<i32>), String> {
    let (linked, stem) = link_file(path)?;
    typeck::check(&linked).map_err(|e| e.to_string())?;
    enforce_performance_modes(&linked, &stem)?;
    if !linked_has_main(&linked) {
        return Err(format!("`{path}` has no `main` to run"));
    }
    // The sandbox grants EXACTLY what a run gives `main` (see `run_grant`) — not the
    // whole-program union, so a verify-only program that imports `crypto` is not
    // forced to be handed a `Secret` it never binds.
    let grant = capabilities::run_grant(&linked);
    // (BUG-116/RFC-0005) A bare `Secret` is the root signing-key externref; a
    // named `--secret` populates a `SecretStore`, not the bare `Secret`.
    // Requiring `--signing-key` specifically stops a named value from being
    // revealed as the root key.
    if grant.contains_key("Secret") && signing_key.is_none() {
        return Err(format!(
            "`{path}` needs a root `Secret` (the signing key), but none was granted — provide `--signing-key <seed-file>` (a named `--secret`/`--secret-file` populates a `SecretStore`, not the bare `Secret`)"
        ));
    }
    eprintln!(
        "sandboxing `{path}` \u{2014} granted exactly: {}",
        capabilities::show_caps(&grant)
    );
    run_linked_compiled(&linked, dir_roots, file_grants, net_allow, args, signing_key, named_secrets, Vec::new(), true, false)
}

/// Resolve a `[secrets]` entry's `from = "env:VAR"` to the secret bytes the host
/// holds. The grant document never carries the value — only where to fetch it.
fn resolve_secret_from(from: &str) -> Result<Vec<u8>, String> {
    grants::resolve_secret_provider(from).map_err(|error| format!("grant {error}"))
}

/// `witchy sandbox --grants app.grants.toml <prog.witchy>` (RFC-0013): run a
/// program against a grant document instead of individual flags. The grant is
/// cross-checked against the computed footprint — an over-request warns, an
/// under-grant aborts — and each `Dir`/`File` `main` parameter is bound to the
/// document entry of the SAME NAME (`[files].config` → the `config` parameter).
fn run_file_grants(
    path: &str,
    grants_path: &str,
    accept_grants: bool,
    args: Vec<String>,
) -> Result<(Vec<String>, Option<i32>), String> {
    use std::io::IsTerminal;
    let (linked, stem) = link_file(path)?;
    typeck::check(&linked).map_err(|e| e.to_string())?;
    enforce_performance_modes(&linked, &stem)?;
    let doc_src = std::fs::read_to_string(grants_path)
        .map_err(|e| format!("cannot read `{grants_path}`: {e}"))?;
    let doc = grants::GrantDoc::parse(&doc_src)?;

    // Cross-check the grant against what a RUN actually exercises — `main`'s own
    // parameter row (`run_grant`), not `analyze().total` (the whole-program union
    // over every public entry point). `total` includes a linked library's `pub fn`s
    // (a dep's `pub fn fetch(net)`, std `crypto.sign`'s `Secret`) that this run never
    // reaches, so cross-checking against it can reject a launch grant that is in fact
    // exactly what `main` receives — a wrongly-refused, valid grant (BUG-016).
    let needed = capabilities::run_grant(&linked);
    let check = grants::cross_check(&doc.cap_set(), &needed);
    if !check.over_grant.is_empty() {
        eprintln!(
            "warning: grant `{grants_path}` over-requests {} \u{2014} the code never exercises it",
            capabilities::show_caps(&check.over_grant)
        );
    }
    if !check.sufficient() {
        return Err(format!(
            "grant `{grants_path}` is insufficient: the code needs {} which the grant withholds",
            capabilities::show_caps(&check.under_grant)
        ));
    }

    // Bind each `Dir`/`File` `main` parameter to its same-named document entry, in
    // declaration order (the positional grant the runtime expects). `Net` is one
    // allowlist (all `[net]` addresses); secrets are named and host-resolved.
    let mut dir_roots: Vec<std::path::PathBuf> = Vec::new();
    let mut file_grants: Vec<std::path::PathBuf> = Vec::new();
    // RFC-0038: grantable-cap params, in declaration order, each field pulled from
    // its `[user_caps]` entry in the cap's field order (must match codegen's k / N).
    let grantable: std::collections::HashMap<&str, &ast::TypeDef> = linked
        .items
        .iter()
        .filter_map(|it| match it {
            ast::Item::Type(t) if t.grantable => Some((t.name.as_str(), t)),
            _ => None,
        })
        .collect();
    let mut user_cap_fields: Vec<Vec<String>> = Vec::new();
    let mut net_allow: Vec<String> = doc.net.values().flatten().cloned().collect();
    net_allow.sort();
    net_allow.dedup();
    let mut named_secrets: Vec<runtime::SecretGrant> = Vec::new();
    for (name, s) in &doc.secrets {
        // BUG-146: carry the document's `use-only` modifier through to the runtime
        // grant — otherwise a grant that declares a signing/TLS key as unrevealable
        // was silently lowered to a revealable secret (`crypto.reveal` would leak it).
        named_secrets.push(runtime::SecretGrant {
            name: name.clone(),
            bytes: resolve_secret_from(&s.from)?,
            use_only: s.use_only,
        });
    }
    if let Some(ast::Item::Function(main)) =
        linked.items.iter().find(|it| matches!(it, ast::Item::Function(f) if f.name == "main"))
    {
        for p in &main.params {
            match &p.ty {
                Some(ast::Type::Named(n, _)) if n == "File" => {
                    let g = doc.files.get(&p.name).ok_or_else(|| {
                        format!("grant `{grants_path}` has no `[files].{}` for `main` parameter `{}`", p.name, p.name)
                    })?;
                    let declared = grant_fs_rights(&format!("[files].{}", p.name), &g.rights)?;
                    let required = match &p.ty {
                        Some(ast::Type::Named(_, args)) => fs_rights_from_type_args(args),
                        _ => runtime::FsRights::new(false, false),
                    };
                    require_exact_fs_rights(&format!("[files].{}", p.name), declared, required)?;
                    file_grants.push(std::path::PathBuf::from(&g.path));
                }
                Some(ast::Type::Named(n, _)) if n == "Dir" => {
                    let g = doc.dirs.get(&p.name).ok_or_else(|| {
                        format!("grant `{grants_path}` has no `[dirs].{}` for `main` parameter `{}`", p.name, p.name)
                    })?;
                    let declared = grant_fs_rights(&format!("[dirs].{}", p.name), &g.rights)?;
                    let required = match &p.ty {
                        Some(ast::Type::Named(_, args)) => fs_rights_from_type_args(args),
                        _ => runtime::FsRights::new(false, false),
                    };
                    require_exact_fs_rights(&format!("[dirs].{}", p.name), declared, required)?;
                    dir_roots.push(std::path::PathBuf::from(&g.root));
                }
                Some(ast::Type::Named(n, _)) if grantable.contains_key(n.as_str()) => {
                    // RFC-0038: a bare grantable cap — pull each policy field from the
                    // `[user_caps]` entry in the cap's declared field order.
                    let uc = doc.user_caps.get(&p.name).ok_or_else(|| {
                        format!("grant `{grants_path}` has no `[user_caps].{}` for `main` parameter `{}` (type `{n}`)", p.name, p.name)
                    })?;
                    let t = grantable[n.as_str()];
                    let names: &[String] = t.variants.first().map(|v| v.field_names.as_slice()).unwrap_or(&[]);
                    let mut vals = Vec::with_capacity(names.len());
                    for fname in names {
                        let v = uc.fields.get(fname).ok_or_else(|| {
                            format!("`[user_caps].{}` is missing field `{fname}` required by `{n}`", p.name)
                        })?;
                        let s = v.as_str().ok_or_else(|| {
                            format!("`[user_caps].{}` field `{fname}` must be a string", p.name)
                        })?;
                        vals.push(s.to_string());
                    }
                    user_cap_fields.push(vals);
                }
                _ => {}
            }
        }
    }
    // Show exactly what `main` will receive — a reviewable diff — then, unless
    // pre-accepted (`--accept-grants`) or non-interactive, require confirmation
    // before handing the authority over (RFC-0013's approval step).
    eprintln!("grant `{grants_path}` for `{path}` confers:");
    eprintln!("  capabilities: {}", capabilities::show_caps(&doc.cap_set()));
    for (name, g) in &doc.dirs {
        eprintln!("  dir    {name}: {}", g.root);
    }
    for (name, g) in &doc.files {
        eprintln!("  file   {name}: {}", g.path);
    }
    if !net_allow.is_empty() {
        eprintln!("  net:   {}", net_allow.join(", "));
    }
    for (name, s) in &doc.secrets {
        eprintln!("  secret {name}: {}", s.from);
    }
    if !accept_grants && std::io::stdin().is_terminal() {
        use std::io::Write;
        eprint!("Approve and run? [y/N] ");
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        std::io::stdin()
            .read_line(&mut line)
            .map_err(|e| format!("reading confirmation: {e}"))?;
        if !matches!(line.trim().chars().next(), Some('y' | 'Y')) {
            return Err("grant not approved \u{2014} aborting".to_string());
        }
    }
    // Secrets reach the program by name through the `SecretStore` (`require`/`get`);
    // the bare root `Secret` (`--signing-key`) is not granted via documents here.
    run_linked_compiled(&linked, dir_roots, file_grants, net_allow, args, None, named_secrets, user_cap_fields, true, false)
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


/// Run a deterministic `build` step in the zero-ambient WASM sandbox. This keeps
/// the old BuildOut/BuildRead-only helper shape used by tests and callers; the
/// grantful production path is [`run_build_step_compiled`].
pub fn run_build_step_sandboxed(
    module: ast::Module,
    out_dir: std::path::PathBuf,
    read_roots: Vec<std::path::PathBuf>,
) -> Result<Vec<String>, String> {
    run_build_step_compiled(module, out_dir, read_roots, Vec::new(), Vec::new(), Vec::new())
}

/// Run a `build` step in the **grant-minimal WASM sandbox**: compile it (the
/// `build` entrypoint becomes the `run` export), then instantiate under a
/// `Capabilities` granting only the build output sandbox, read roots, and named
/// BuildEnv/BuildExec/BuildNet allow-lists. The module physically has no
/// `dir_*`/runtime `net_*`/`print` import to call, and every build primitive is
/// confined by the same host-side grant tables as the interpreter oracle.
///
/// This is the production build-step path. The interpreter remains the oracle
/// for parity tests, not a package-manager execution backend.
pub fn run_build_step_compiled(
    module: ast::Module,
    out_dir: std::path::PathBuf,
    read_roots: Vec<std::path::PathBuf>,
    env_keys: Vec<String>,
    exec_tools: Vec<String>,
    net_hosts: Vec<String>,
) -> Result<Vec<String>, String> {
    let env = capture_build_env(&env_keys);
    run_build_step_compiled_with_env(module, out_dir, read_roots, env, exec_tools, net_hosts)
}

fn capture_build_env(
    keys: &[String],
) -> std::collections::BTreeMap<String, Option<String>> {
    let env: std::collections::BTreeMap<_, _> = keys
        .iter()
        .map(|key| (key.clone(), std::env::var(key).ok()))
        .collect();
    debug_assert!(
        keys.iter().all(|key| env.contains_key(key)),
        "every granted env name must be represented in the snapshot"
    );
    env
}

fn run_build_step_compiled_with_env(
    module: ast::Module,
    out_dir: std::path::PathBuf,
    read_roots: Vec<std::path::PathBuf>,
    env: std::collections::BTreeMap<String, Option<String>>,
    exec_tools: Vec<String>,
    net_hosts: Vec<String>,
) -> Result<Vec<String>, String> {
    use runtime::{Capabilities, Runtime};
    std::fs::create_dir_all(&out_dir).map_err(|e| format!("build: output dir: {e}"))?;
    let wasm = match codegen::compile_build_module(&module) {
        codegen::LoweringOutcome::Lowered(bytes) => bytes,
        codegen::LoweringOutcome::Unsupported(reason) => return Err(reason.to_string()),
        codegen::LoweringOutcome::Rejected(error) => return Err(error.message),
    };
    let caps = Capabilities {
        build_out: Some(out_dir.clone()),
        build_read_roots: read_roots,
        build_env: Some(env),
        exec_allow: Some(exec_tools),
        build_net_allow: Some(net_hosts),
        ..Default::default()
    };
    let mut rt = Runtime::batch().map_err(|e| e.to_string())?;
    let mut vm = rt
        .spawn(&wasm, caps, RUN_MEMORY_PAGES)
        .map_err(|e| e.to_string())?;
    vm.run().map_err(|e| e.root_cause().to_string())?;
    let mut generated: Vec<String> = std::fs::read_dir(&out_dir)
        .map_err(|e| format!("build: reading output dir: {e}"))?
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    generated.sort();
    Ok(generated)
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

/// Parse, link, type-check, and run a file's `build` entrypoint under confined
/// grants, returning the names of the files it generated. The output directory
/// defaults to `./build-out`.
fn run_build_step_file(
    path: &str,
    out_dir: Option<std::path::PathBuf>,
    read_roots: Vec<std::path::PathBuf>,
    env_keys: Vec<String>,
    exec_tools: Vec<String>,
    net_hosts: Vec<String>,
) -> Result<Vec<String>, String> {
    let env = capture_build_env(&env_keys);
    run_build_step_file_with_env(path, out_dir, read_roots, env, exec_tools, net_hosts)
}

fn run_build_step_file_with_env(
    path: &str,
    out_dir: Option<std::path::PathBuf>,
    read_roots: Vec<std::path::PathBuf>,
    env: std::collections::BTreeMap<String, Option<String>>,
    exec_tools: Vec<String>,
    net_hosts: Vec<String>,
) -> Result<Vec<String>, String> {
    let (linked, _) = link_file(path)?;
    typeck::check(&linked).map_err(|e| e.to_string())?;
    let out = out_dir.unwrap_or_else(|| std::path::PathBuf::from("build-out"));
    run_build_step_compiled_with_env(linked, out, read_roots, env, exec_tools, net_hosts)
}

#[cfg(test)]
mod compiled_build_step_tests {
    use super::{run_build_step_file, run_build_step_file_with_env};

    fn unique(name: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("witchy_{name}_{}_{}", std::process::id(), nanos))
    }

    fn write_source(dir: &std::path::Path, src: &str) -> std::path::PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join("build.witchy");
        std::fs::write(&path, src).unwrap();
        path
    }

    #[test]
    fn compiled_build_env_reads_only_allow_listed_keys() {
        let dir = unique("compiled_build_env");
        let _ = std::fs::remove_dir_all(&dir);
        let env: std::collections::BTreeMap<String, Option<String>> =
            [("WITCHY_BUILD_ALLOWED".to_string(), Some("yes".to_string()))].into();

        let allowed = write_source(
            &dir,
            "import option\nfn build(out: BuildOut, env: BuildEnv):\n    let v = match env.get_build_env(\"WITCHY_BUILD_ALLOWED\"):\n        Some(x) -> x\n        None -> \"unset\"\n    out.write_out(\"g.txt\", v)\n",
        );
        run_build_step_file_with_env(
            allowed.to_str().unwrap(),
            Some(dir.join("out")),
            vec![],
            env.clone(),
            vec![],
            vec![],
        )
        .expect("allow-listed env key reads");
        assert_eq!(std::fs::read_to_string(dir.join("out/g.txt")).unwrap(), "yes");

        let denied = write_source(
            &dir,
            "import option\nfn build(out: BuildOut, env: BuildEnv):\n    let v = match env.get_build_env(\"WITCHY_BUILD_SECRET\"):\n        Some(x) -> x\n        None -> \"unset\"\n    out.write_out(\"g.txt\", v)\n",
        );
        let err = run_build_step_file_with_env(
            denied.to_str().unwrap(),
            Some(dir.join("out2")),
            vec![],
            env,
            vec![],
            vec![],
        )
        .expect_err("unlisted env key is refused");
        assert!(err.contains("not in this BuildEnv grant's allow-list"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn compiled_build_net_fetches_only_allow_listed_hosts() {
        use std::io::{Read as _, Write as _};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let server = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut buf = [0u8; 1024];
            let _ = sock.read(&mut buf);
            let body = "schema-v1";
            let _ = sock.write_all(
                format!("HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n{body}", body.len()).as_bytes(),
            );
        });

        let dir = unique("compiled_build_net");
        let _ = std::fs::remove_dir_all(&dir);
        let source = write_source(
            &dir,
            &format!(
                "fn build(out: BuildOut, dl: BuildNet):\n    out.write_out(\"got.txt\", dl.fetch_build(\"{addr}\", \"/schema\"))\n"
            ),
        );
        run_build_step_file(
            source.to_str().unwrap(),
            Some(dir.join("out")),
            vec![],
            vec![],
            vec![],
            vec![addr.clone()],
        )
        .expect("allow-listed fetch runs");
        assert_eq!(std::fs::read_to_string(dir.join("out/got.txt")).unwrap(), "schema-v1");
        server.join().unwrap();

        let err = run_build_step_file(
            source.to_str().unwrap(),
            Some(dir.join("out2")),
            vec![],
            vec![],
            vec![],
            vec!["allowed.example:80".to_string()],
        )
        .expect_err("unlisted host is refused");
        assert!(err.contains("not in this BuildNet grant's allow-list"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn compiled_build_exec_runs_only_allow_listed_tools() {
        let dir = unique("compiled_build_exec");
        let _ = std::fs::remove_dir_all(&dir);
        let source = write_source(
            &dir,
            "fn build(out: BuildOut, cc: BuildExec):\n    out.write_out(\"x.txt\", cc.run_tool(\"cat\", \"piped-input\"))\n",
        );
        run_build_step_file(
            source.to_str().unwrap(),
            Some(dir.join("out")),
            vec![],
            vec![],
            vec!["cat".to_string()],
            vec![],
        )
        .expect("cat is allow-listed");
        assert_eq!(std::fs::read_to_string(dir.join("out/x.txt")).unwrap(), "piped-input");

        let denied = write_source(
            &dir,
            "fn build(out: BuildOut, cc: BuildExec):\n    out.write_out(\"x.txt\", cc.run_tool(\"rm\", \"-rf /\"))\n",
        );
        let err = run_build_step_file(
            denied.to_str().unwrap(),
            Some(dir.join("out2")),
            vec![],
            vec![],
            vec!["cat".to_string()],
            vec![],
        )
        .expect_err("unlisted tool is refused");
        assert!(err.contains("not in this BuildExec grant's allow-list"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// End-to-end coverage: every shipped example must type-check and produce the
/// expected result (interpreted), or type-check and compile to valid WASM.
#[cfg(test)]
mod example_tests;

/// RFC-0072: verbatim diagnostic goldens over the full error surface (parse,
/// layout, link, type, capability, lowering-reject, runtime trap) via `insta`.
#[cfg(test)]
mod diagnostic_golden_tests;
