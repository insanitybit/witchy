//! Witchy runtime spike — proving the core thesis: a program in an isolated
//! WASM VM can do nothing beyond the capabilities it was explicitly granted.
//!
//! The modules here are hand-written WebAssembly standing in for compiled
//! witchy code; the point of the spike is the security substrate, not the
//! language surface yet.

// This crate is hand-indented, not rustfmt-managed. Clippy's "collapse nested
// conditionals" lints would rewrite explicit `if { if let ... }` nesting into
// `let`-chains without re-indenting, hurting readability; the nested form is an
// intentional style choice here.
#![allow(clippy::collapsible_if, clippy::collapsible_match, clippy::items_after_test_module)]

pub use witchy::analysis;
pub use witchy::aliases;
pub use witchy::ast;
pub use witchy::capabilities;
pub use witchy::codegen;
pub use witchy::confine;
pub use witchy::async_lower;
pub use witchy::consts;
pub use witchy::comptime;
pub use witchy::derive;
pub use witchy::doc;
pub use witchy::fmt;
pub use witchy::format;
pub use witchy::generators;
pub use witchy::grants;
pub use witchy::interpreter;
pub use witchy::lexer;
pub use witchy::linker;
pub use witchy::opt;
pub use witchy::pipeline;
mod lsp;
pub use witchy::native;
pub use witchy::net;
pub use witchy::optimize;
pub use witchy::parser;
mod idp;
pub use witchy::records;
pub use witchy::runtime;
pub use witchy::traits;
pub use witchy::typeck;
pub use witchy::value;
pub use witchy::wir;
pub use witchy::wir_encode;
pub use witchy::wir_helpers;
pub use witchy::wir_opt;
#[cfg(feature = "native")]
pub use witchy::wir_prelude;


use runtime::Runtime;
/// One-screen overview of the command-line interface, shown for bare `witchy`.
fn print_usage() {
    println!(
        "\
witchy — a capability-secure language with twin interpreter and WASM backends

USAGE:
    witchy [--net <host:port>]... <file.witchy>   run a program
    witchy check    <file.witchy>                 type-check without running
    witchy parity   <file.witchy>                 run on both backends, confirm identical output
                                                  (a verify-the-compiler tool, not a workflow step)
    witchy test     <file.witchy|dir>             run in-language tests (zero-param `test_*` functions)
    witchy sandbox [--dir <root>] [--net <addr>]... <file.witchy> [args...]
                                                  compile and run in a VM granted exactly its footprint
    witchy emit-wat <file.witchy>                 print the compiled WebAssembly text (the module sandbox runs)
    witchy caps     [file.witchy]                 report the capability footprint (defaults to the project entry)
    witchy caps-diff <old.witchy> <new.witchy>    fail if the footprint widened
    witchy which    <name>                        find a function in the standard library by (partial) name
    witchy fmt [--check] <file.witchy>            reformat in place (--check: verify only, exit 1 if not)
    witchy lsp                                    run the language server

Package commands: new, init, add, build, run [args...], update, audit, tree,
outdated, why, why-cap, verify, vendor, publish, promote, yank, list — run
`witchy pm` for the full package-manager help. All of them accept
`-C <dir>`; `witchy run` passes everything after `run` (or after `--`) to the
program as `main`'s `args`, including `--help`."
    );
}

/// The current directory's project entry source file (`src/<module>.witchy`,
/// where `<module>` is the manifest's rune name with `/`-prefixes stripped and
/// `-` mapped to `_`), if we're inside a project. Lets file-oriented commands
/// (`witchy caps`) default to the project entry. Reads the `name = "..."` line
/// from `witchy.toml` directly so no package-manager code is needed.
fn project_entry_file() -> Option<String> {
    let dir = std::path::Path::new(".");
    let toml = std::fs::read_to_string(dir.join("witchy.toml")).ok()?;
    let name = toml.lines().find_map(|line| {
        let line = line.trim();
        let rest = line.strip_prefix("name")?.trim_start();
        let rest = rest.strip_prefix('=')?.trim();
        rest.strip_prefix('"').and_then(|r| r.strip_suffix('"')).map(|s| s.to_string())
    })?;
    let module = name.rsplit('/').next().unwrap_or("").replace('-', "_");
    let path = dir.join("src").join(format!("{module}.witchy"));
    path.exists().then(|| path.display().to_string())
}

/// Run the EMBEDDED witchy package-manager front-end (`projects/pm/src/pm.witchy`)
/// — the cargo-equivalent CLI, itself written in witchy and bundled into the
/// toolchain like std (rfcs/0004-self-hosted-cli.md). `raw` is the front-end's
/// argv (everything after the `witchy` subcommand): `--net <host:port>` flags are
/// extracted into the program's `Net` allowlist, the rest become `main`'s `args`.
/// It runs capability-confined: Console, the project `Dir` (cwd, handle 0), a
/// `Dir` to the toolchain bin (handle 1, so it can drive the compiler via `Exec`),
/// `Net`, `Env`, and its argv. This is the sole entry for the front-end's client
/// verbs — both `witchy pm <verb>` and the top-level `witchy <verb>` route here.
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
    // Link the embedded front-end against the bundled std modules.
    let link_result = (|| -> Result<ast::Module, String> {
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
                    match bundled_module(imp) {
                        Some(s) => queue.push_back((imp.clone(), s.to_string())),
                        None => return Err(format!("embedded front-end imports `{imp}`, not a bundled module")),
                    }
                }
            }
            modules.push((name, module));
        }
        pipeline::link(modules, "pm").map_err(|e| e.to_string())
    })();
    let module = match link_result {
        Ok(m) => m,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    if let Err(e) = typeck::check(&module) {
        eprintln!("{e}");
        std::process::exit(1);
    }
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
    // `run_wasm_module` grants exactly the same authority (handle 0 = cwd, handle
    // 1 = bin so `Exec` finds the compiler) and surfaces `main`'s `Int` exit code.
    let wasm = match codegen::compile_module_binary(&module) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => {
            eprintln!("the pm front-end does not lower to the compiled backend");
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
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

fn main() -> wasmtime::Result<()> {
    // (RFC-0037) `--release` / `--debug` — thin WITCHY_OPT mode selectors, usable with any
    // subcommand. `--debug` compiles with NO optimizations (maximal debuggability); `--release`
    // is the optimized shipping set (also the default when neither is given). Set the mode here,
    // before any codegen reads WITCHY_OPT; an explicit `WITCHY_OPT` env still wins only if
    // neither flag is present. The user-facing run/sandbox arg loops skip these tokens so they
    // aren't mistaken for the program file.
    {
        let a: Vec<String> = std::env::args().skip(1).collect();
        // SAFETY: this runs at the very top of `main`, before any thread is spawned, so there is
        // no concurrent env access to race with (the requirement `set_var` is unsafe for).
        if let Some(m) = leading_opt_mode(&a) {
            unsafe { std::env::set_var("WITCHY_OPT", m) };
        }
    }
    // `witchy doc <file>...` prints Markdown API docs (one section per file) to
    // stdout — public functions, their signatures, and their doc comments.
    if std::env::args().nth(1).as_deref() == Some("doc") {
        use std::path::Path;
        let files: Vec<String> = std::env::args().skip(2).collect();
        if files.is_empty() {
            eprintln!("usage: witchy doc <file.witchy>...");
            std::process::exit(2);
        }
        let mut out = String::from("# API reference\n\n");
        for f in &files {
            let stem = Path::new(f)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or(f);
            let src = match std::fs::read_to_string(f) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("cannot read `{f}`: {e}");
                    std::process::exit(1);
                }
            };
            match doc::render(stem, &src) {
                Ok(md) => out.push_str(&md),
                Err(e) => {
                    eprintln!("{f}: {e}");
                    std::process::exit(1);
                }
            }
        }
        print!("{out}");
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
    // `witchy build-step <file> [--out <dir>] [--read <dir>] [--env <KEY>]...`
    // runs a rune's `build` entrypoint under confined grants and reports the
    // source it generated. The build step can only use the build capabilities it
    // is granted here (it cannot forge a runtime cap), so this is the build-time
    // half of the capability model, exercised in isolation.
    if std::env::args().nth(1).as_deref() == Some("build-step") {
        let mut out_dir: Option<std::path::PathBuf> = None;
        let mut read_roots: Vec<std::path::PathBuf> = Vec::new();
        let mut env_keys: Vec<String> = Vec::new();
        let mut exec_tools: Vec<String> = Vec::new();
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
                _ if path.is_none() => path = Some(a),
                _ => {}
            }
        }
        let Some(path) = path else {
            eprintln!("usage: witchy build-step <file.witchy> [--out <dir>] [--read <dir>]... [--env <KEY>]... [--exec <tool>]...");
            std::process::exit(1);
        };
        match run_build_step_file(&path, out_dir, read_roots, env_keys, exec_tools) {
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
    // `witchy check <file>` parses, links, and type-checks without running —
    // exits non-zero on any error. Validates programs you can't run (servers).
    if std::env::args().nth(1).as_deref() == Some("check") {
        let Some(path) = std::env::args().nth(2) else {
            eprintln!("usage: witchy check <file.witchy>");
            std::process::exit(1);
        };
        match check_file(&path) {
            Ok(()) => println!("{path}: ok"),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
        return Ok(());
    }
    // `witchy compile <entry> [--dep name=path]... [--out <file.wasm>]` links the
    // entry with explicitly-provided dependency sources, type-checks, and compiles
    // to a wasm binary — the low-level surface the witchy CLI front-end drives to
    // build a multi-rune project (rfcs/0004-self-hosted-cli.md §4). Without `--out`
    // it just verifies the program compiles.
    if std::env::args().nth(1).as_deref() == Some("compile") {
        let mut entry: Option<String> = None;
        let mut deps: std::collections::HashMap<String, std::path::PathBuf> =
            std::collections::HashMap::new();
        let mut out: Option<String> = None;
        let mut argv = std::env::args().skip(2);
        while let Some(a) = argv.next() {
            match a.as_str() {
                "--dep" => match argv.next().and_then(|s| s.split_once('=').map(|(n, p)| (n.to_string(), std::path::PathBuf::from(p)))) {
                    Some((n, p)) => {
                        deps.insert(n, p);
                    }
                    None => {
                        eprintln!("--dep needs name=path");
                        std::process::exit(1);
                    }
                },
                "--out" => match argv.next() {
                    Some(f) => out = Some(f),
                    None => {
                        eprintln!("--out needs a file");
                        std::process::exit(1);
                    }
                },
                _ if entry.is_none() => entry = Some(a),
                _ => {}
            }
        }
        let Some(entry) = entry else {
            eprintln!("usage: witchy compile <entry.witchy> [--dep name=path]... [--out <file.wasm>]");
            std::process::exit(1);
        };
        let result = (|| -> Result<(), String> {
            let (linked, _stem) = link_file_with_deps(&entry, &deps)?;
            typeck::check(&linked).map_err(|e| e.to_string())?;
            let bytes = compile_linked_to_wasm(&linked)?;
            if let Some(f) = &out {
                std::fs::write(f, &bytes).map_err(|e| format!("cannot write `{f}`: {e}"))?;
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                match &out {
                    Some(f) => println!("{entry}: compiled -> {f}"),
                    None => println!("{entry}: ok"),
                }
                return Ok(());
            }
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
    }
    // `witchy pm <args...>` runs the EMBEDDED witchy package-manager front-end
    // (projects/pm/src/pm.witchy) — the cargo-equivalent CLI, itself written in
    // witchy and bundled into the toolchain like std. It runs capability-confined:
    // Console, the project `Dir` (cwd, handle 0), a `Dir` to the toolchain bin
    // (handle 1, so it can drive the compiler via `Exec`), `Net`, and its argv.
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
    // (handle 0), a `SecretStore` holding the root signing key, and a Clock. coven uses
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
        let link_result = (|| -> Result<ast::Module, String> {
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
            pipeline::link(modules, "coven").map_err(|e| e.to_string())
        })();
        let module = match link_result {
            Ok(m) => m,
            Err(e) => { eprintln!("{e}"); std::process::exit(1); }
        };
        if let Err(e) = typeck::check(&module) {
            eprintln!("{e}");
            std::process::exit(1);
        }
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
            // The signing key is the `signing` secret at handle 0 (a bare `Secret`),
            // also reachable via `SecretStore.get("signing")`.
            caps.secrets.push(runtime::SecretGrant::new("signing", seed.to_vec()));
        }
        let wasm = match codegen::compile_module_binary(&module) {
            Ok(Some(bytes)) => bytes,
            Ok(None) => {
                eprintln!("coven does not lower to the compiled backend");
                std::process::exit(1);
            }
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        };
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
    // `witchy test <file|dir>` runs in-language tests: zero-parameter `test_*`
    // functions, passing unless they abort (std/testing's assertions).
    if std::env::args().nth(1).as_deref() == Some("test") {
        let Some(path) = std::env::args().nth(2) else {
            eprintln!("usage: witchy test <file.witchy|dir>");
            std::process::exit(1);
        };
        match run_tests(&path) {
            Ok(true) => return Ok(()),
            Ok(false) => std::process::exit(1),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
    }
    // `witchy parity <file>` runs the program on both the interpreter and the
    // compiled WASM backend and confirms they produce identical output. (Named
    // `parity`, not `verify` — `verify` is the package-manager's TUF/signature
    // command.)
    if std::env::args().nth(1).as_deref() == Some("parity") {
        let Some(path) = std::env::args().nth(2) else {
            eprintln!("usage: witchy parity <file.witchy>");
            std::process::exit(PARITY_EXIT_UNEXPECTED);
        };
        let outcome = parity_check(&path);
        // Human-readable detail: agreements to stdout, failures to stderr.
        if outcome.is_pass() {
            println!("{}", outcome.message());
        } else {
            eprintln!("{}", outcome.message());
        }
        // Final MACHINE-READABLE stats line (always, stdout). Consumers (the sweep,
        // the fuzzer) branch on `outcome=`/`compared=` and the exit code — never on
        // human text (RFC-0058 §2).
        println!(
            "parity-stats outcome={} compared={} file={path}",
            outcome.tag(),
            outcome.compared()
        );
        std::process::exit(outcome.exit_code());
    }
    // `witchy sandbox [--dir <root>] [--net <host:port>]... <file> [args...]`
    // compiles the program to WASM and runs it in the capability-sandboxed VM,
    // granted exactly its computed footprint. `--dir` picks the subtree backing
    // a granted Dir (default `.`); each `--net` allowlists an address.
    if std::env::args().nth(1).as_deref() == Some("sandbox") {
        // Multiple `--dir` grants map positionally to `main`'s `Dir` params: the
        // first backs handle 0, the rest handles 1.. (rfcs/0004-self-hosted-cli.md).
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
        // A precompiled `.wasm` runs directly (authority from its imports); a
        // `.witchy` source is compiled then run, granted its computed footprint.
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
        return Ok(());
    }
    // `witchy emit-wasm <file.witchy> [-o out.wasm]` compiles a program to a wasm
    // BINARY — the Tier-1 distribution artifact. Run it with `witchy <out.wasm>`,
    // which grants exactly the authority the module's imports declare.
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
        return Ok(());
    }
    // `witchy fmt <file>` rewrites a source file in canonical brace-free form.
    if std::env::args().nth(1).as_deref() == Some("fmt") {
        // `witchy fmt [--check] <file.witchy>...` formats (or, with `--check`,
        // verifies) EVERY file argument — a shell glob like `witchy fmt std/*.witchy`
        // expands to many paths, and silently dropping all but the first was a
        // no-op that made callers believe files were formatted (BUG-012).
        // `--check` verifies without rewriting (for CI): exit 1 if any file would
        // change; otherwise 0. Every file is processed even if an earlier one
        // fails, and the exit code is 1 iff any file failed.
        let check = std::env::args().nth(2).as_deref() == Some("--check");
        let paths: Vec<String> = std::env::args().skip(if check { 3 } else { 2 }).collect();
        if paths.is_empty() {
            eprintln!("usage: witchy fmt [--check] <file.witchy>...");
            std::process::exit(1);
        }
        let mut failed = false;
        for path in &paths {
            match std::fs::read_to_string(path) {
                Ok(src) => match format::reformat(&src) {
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
    if let Some(a1) = std::env::args().nth(1) {
        const FRONTEND_VERBS: &[&str] = &[
            "new", "init", "add", "build", "run", "update", "list", "audit", "tree", "outdated",
            "why", "why-cap", "publish", "promote", "yank", "verify", "vendor",
        ];
        if FRONTEND_VERBS.contains(&a1.as_str()) {
            run_embedded_pm(std::env::args().skip(1).collect());
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
                println!("witchy {}", env!("CARGO_PKG_VERSION"));
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
            .expect("compile")
            .expect("the binary path lowers this benchmark");
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

/// Source for a bundled standard-library module, shipped with the compiler so
/// `import list` works without a local file. A local file of the same name
/// takes precedence (see `execute_file`).
fn bundled_module(name: &str) -> Option<&'static str> {
    crate::linker::std_source(name)
}

/// Parse and link a source file, resolving each `import` from a sibling
/// `<name>.witchy` (preferred) or the bundled std. Returns the linked module and
/// the entry module's stem. Shared by `execute_file` and `check_file`.
fn link_file(path: &str) -> Result<(ast::Module, String), String> {
    link_file_with_deps(path, &std::collections::HashMap::new())
}

/// Like `link_file`, but resolves named imports from an explicit dependency map
/// (`import X` → `deps["X"]`) before the sibling-`<name>.witchy` / bundled-std
/// fallback. This is the hook the witchy CLI front-end uses to hand the compiler
/// resolved coven-dependency sources via `witchy compile <entry> --dep name=path`
/// (rfcs/0004-self-hosted-cli.md §4).
fn link_file_with_deps(
    path: &str,
    deps: &std::collections::HashMap<String, std::path::PathBuf>,
) -> Result<(ast::Module, String), String> {
    use std::collections::{HashSet, VecDeque};
    use std::path::{Path, PathBuf};

    let entry_path = Path::new(path);
    let dir: &Path = entry_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let entry_stem = entry_path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| format!("invalid file name: {path}"))?
        .to_string();

    let mut modules: Vec<(String, ast::Module)> = Vec::new();
    let mut loaded: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<(String, PathBuf)> = VecDeque::new();
    queue.push_back((entry_stem.clone(), entry_path.to_path_buf()));

    while let Some((name, p)) = queue.pop_front() {
        if !loaded.insert(name.clone()) {
            continue; // already loaded (cycle-safe)
        }
        // A local `<name>.witchy` wins; otherwise fall back to a bundled
        // standard-library module (e.g. `import list`).
        let src = match std::fs::read_to_string(&p) {
            Ok(s) => s,
            Err(e) => match bundled_module(&name) {
                Some(s) => s.to_string(),
                None => {
                    // A misspelled `import` of a std module gets a suggestion.
                    let hint = if name != entry_stem {
                        crate::linker::closest_std_module(&name)
                            .map(|m| format!(" — did you mean `import {m}`?"))
                            .unwrap_or_default()
                    } else {
                        String::new()
                    };
                    return Err(format!("cannot read `{}`: {e}{hint}", p.display()));
                }
            },
        };
        let module = parser::parse_module(&src).map_err(|e| format!("{name}: {e}"))?;
        for imp in &module.imports {
            if !loaded.contains(imp) {
                let dep_path = deps
                    .get(imp)
                    .cloned()
                    .unwrap_or_else(|| dir.join(format!("{imp}.witchy")));
                queue.push_back((imp.clone(), dep_path));
            }
        }
        modules.push((name, module));
    }

    let linked = pipeline::link(modules, &entry_stem).map_err(|e| e.to_string())?;
    Ok((linked, entry_stem))
}

/// Parse, link, and type-check a file WITHOUT running it (`witchy check`). Useful
/// for CI and for validating programs you don't want to run — e.g. servers,
/// which never return.
fn check_file(path: &str) -> Result<(), String> {
    let (linked, stem) = link_file(path)?;
    typeck::check(&linked).map_err(|e| e.to_string())?;
    enforce_performance_modes(&linked, &stem)?;
    Ok(())
}

/// Whether a linked function originated in the entry file. The linker keeps the
/// entry module's `main` unqualified and qualifies everything else as
/// `{stem}.name`, so the entry file's functions are exactly `main` and the
/// `{stem}.`-prefixed ones; linked-in modules carry a different prefix.
fn is_entry_function(name: &str, entry_stem: &str) -> bool {
    name == "main" || name.starts_with(&format!("{entry_stem}."))
}

/// Performance-mode enforcement. The uniqueness analysis flags accumulation that
/// reverts to the copying path inside a loop (O(n²)). In an ordinary file this is
/// a check-time *note* — the copying path IS the semantics, so a perf-shape
/// warning must never block a build. In a file that declares `mode opt`
/// the cliff is a hard error, AND every ownership-relevant parameter must carry
/// an explicit `let`/`own`/`var` convention — so the interprocedural summaries
/// are declared contracts rather than inferences, and the optimization is powered
/// by the annotation, not the fixpoint. Only the entry file's own functions are
/// judged; linked-in modules keep their own policy. See rfcs/performance-modes.md.
fn enforce_performance_modes(linked: &ast::Module, entry_stem: &str) -> Result<(), String> {
    let enforce = !linked.modes.is_empty();
    let mut errors = Vec::new();

    // Body contract: accumulators must stay on the in-place fast path.
    for (func, c) in analysis::module_cliffs(linked) {
        if !is_entry_function(&func, entry_stem) {
            continue;
        }
        if enforce {
            errors.push(format!(
                "error: in `{func}` (line {}): `{}` is rebuilt by copy on every \
                 iteration of this loop — it is {} [mode {}]\n  keep `{}` on the \
                 in-place path: certify helper calls with `let`/`own` so they do \
                 not alias it out, and do not share it mid-loop",
                c.line, c.var, c.reason, linked.modes.join(", "), c.var,
            ));
        } else {
            eprintln!(
                "note: in `{func}` (line {}): `{}` is rebuilt by copy on every \
                 iteration of this loop — it is {}",
                c.line, c.var, c.reason
            );
        }
    }

    // Signature contract (mode files only): ownership-relevant parameters must
    // declare their convention, so the summaries are facts, not inferences.
    if enforce {
        for item in &linked.items {
            if let ast::Item::Function(f) = item {
                if !is_entry_function(&f.name, entry_stem) {
                    continue;
                }
                for p in &f.params {
                    if p.convention == ast::Convention::Let && ownership_relevant(&p.ty) {
                        errors.push(format!(
                            "error: in `{}`: parameter `{}` has no ownership \
                             convention — `mode {}` requires an explicit `let` \
                             (read-only borrow), `own` (consumed), or `var` \
                             (mutated in place)",
                            f.name,
                            p.name,
                            linked.modes.join(", "),
                        ));
                    }
                }
            }
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("\n"))
    }
}

/// Whether a parameter type is one where an ownership convention changes the
/// generated code (a heap buffer: String/List/Dict/tuple/record/ADT). Scalars,
/// capabilities, and function values are exempt — annotating them is noise.
fn ownership_relevant(ty: &Option<ast::Type>) -> bool {
    match ty {
        Some(ast::Type::Named(n, _)) => !matches!(
            n.as_str(),
            "Int" | "Float" | "Bool" | "Duration" | "Console" | "Dir" | "Net" | "Clock"
                | "Env" | "Secret" | "SecretStore"
        ),
        Some(ast::Type::Tuple(_)) => true,
        _ => false,
    }
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

/// (RFC-0037) The optimization mode a LEADING global flag selects (`--release` /
/// `--debug`), or `None`. `args` is the argv WITHOUT the program name. Only the
/// flags BEFORE the program file — the first `.witchy`/`.wasm` token, which is where
/// the guest's own argv begins — are consulted: a mode flag sitting in the guest's
/// argv must neither flip the compiler's optimization mode nor be double-consumed
/// (BUG-108 / BUG-114). Every other global flag already obeys this "before the file"
/// rule via the per-command arg loops; the top-of-`main` mode scan is the one that
/// used to read the whole argv, guest args included. `--debug` wins over `--release`
/// when both lead (maximal debuggability), matching the prior precedence.
fn leading_opt_mode(args: &[String]) -> Option<&'static str> {
    let mut debug = false;
    let mut release = false;
    for a in args {
        if a.ends_with(".witchy") || a.ends_with(".wasm") {
            break;
        }
        match a.as_str() {
            "--debug" => debug = true,
            "--release" => release = true,
            _ => {}
        }
    }
    if debug {
        Some("debug")
    } else if release {
        Some("release")
    } else {
        None
    }
}

/// The value of a `--flag value` / `--flag=value` option: the inline form if
/// present, else the next argument. Exits with a usage error if neither is given.
fn flag_value(arg: &str, flag: &str, rest: &mut impl Iterator<Item = String>) -> String {
    match arg.strip_prefix(&format!("{flag}=")) {
        Some(v) => v.to_string(),
        None => match rest.next() {
            Some(v) => v,
            None => {
                eprintln!("{flag} requires a value");
                std::process::exit(1);
            }
        },
    }
}

/// Parse a `--secret name=value[,use-only]` spec into a named secret. The value is
/// taken literally (UTF-8 bytes) — a token, password, or connection string. The
/// name must be non-empty and contain no `=` (everything after the first `=`, up
/// to any trailing `,use-only`, is the value, so values may contain `=`). A
/// trailing `,use-only` (RFC-0060) marks the secret usable by handle but not
/// revealable (`crypto.reveal` errors); the default is revealable.
fn parse_secret_inline(spec: &str) -> Result<runtime::SecretGrant, String> {
    let (body, use_only) = split_use_only(spec);
    match body.split_once('=') {
        Some((name, value)) if !name.is_empty() => {
            Ok(runtime::SecretGrant { name: name.to_string(), bytes: value.as_bytes().to_vec(), use_only })
        }
        _ => Err(format!("`--secret` expects `name=value[,use-only]`, got `{spec}`")),
    }
}

/// Parse a `--secret-file name=path[,use-only]` spec, reading the secret's bytes
/// from the file. Whitespace is NOT trimmed (a secret file holds exactly its
/// bytes). A trailing `,use-only` (RFC-0060) marks it usable by handle but not
/// revealable — the shape a TLS private key should take.
fn parse_secret_file(spec: &str) -> Result<runtime::SecretGrant, String> {
    let (body, use_only) = split_use_only(spec);
    match body.split_once('=') {
        Some((name, path)) if !name.is_empty() => {
            let bytes = std::fs::read(path).map_err(|e| format!("`--secret-file {name}`: cannot read `{path}`: {e}"))?;
            Ok(runtime::SecretGrant { name: name.to_string(), bytes, use_only })
        }
        _ => Err(format!("`--secret-file` expects `name=path[,use-only]`, got `{spec}`")),
    }
}

/// (RFC-0060) Peel a single trailing `,use-only` grant modifier off a secret spec,
/// returning the `name=…` body and whether use-only was requested. Only the exact
/// trailing token is recognized, so a `name=value` whose value happens to contain
/// commas is unaffected unless it literally ends in `,use-only`.
fn split_use_only(spec: &str) -> (&str, bool) {
    match spec.strip_suffix(",use-only") {
        Some(body) => (body, true),
        None => (spec, false),
    }
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
    typeck::check(&linked).map_err(|e| e.to_string())?;
    enforce_performance_modes(&linked, &entry_stem)?;

    // No `main` means there's nothing to run directly — but the file still
    // compiled. Explain rather than failing with "unknown function `main`".
    let has_main = linked
        .items
        .iter()
        .any(|it| matches!(it, ast::Item::Function(f) if f.name == "main"));
    if !has_main {
        let msg = format!(
            "`{entry_stem}` compiled OK — it's a library (no `main`); import it from another module."
        );
        return Ok((vec![msg], 0));
    }

    // One run path: the compiled (WASM) backend. `witchy run` and `witchy sandbox`
    // share one runtime, so dev == deploy by construction. The interpreter is only
    // the differential oracle (`witchy parity`) and the comptime evaluator — never
    // a user-program run path.
    run_linked_compiled(&linked, Vec::new(), Vec::new(), net_allow, args, signing_key, named_secrets, Vec::new(), false)
        .map(|(lines, code)| (lines, code.unwrap_or(0)))
}

/// Run a program on BOTH backends — the tree-walking interpreter and compiled
/// WebAssembly — and confirm they produce identical output. Witchy's
/// dual-backend equivalence is normally an internal test invariant; `witchy
/// verify` surfaces it as a guarantee you can check on your own code.
/// A failed in-language test: its (qualified) name and the abort message.
type TestFailure = (String, String);

/// Rewrite the placeholder call `witchy_test_target()` in a synthesized test-driver
/// expression to the real (linker-qualified) test name — so the parser never has to
/// re-read `mod.fn` as a method call. The placeholder may sit anywhere in the driver
/// body: bare (`witchy_test_target()`), or as an argument (`task.run(
/// witchy_test_target())`, the async driver), so this recurses through calls,
/// method calls, and unary ops.
fn patch_test_target(expr: &mut ast::Expr, name: &str) {
    match expr {
        ast::Expr::Call { name: n, args } => {
            if n == "witchy_test_target" {
                *n = name.to_string();
            } else {
                for a in args {
                    patch_test_target(a, name);
                }
            }
        }
        ast::Expr::MethodCall { receiver, args, .. } => {
            patch_test_target(receiver, name);
            for a in args {
                patch_test_target(a, name);
            }
        }
        ast::Expr::Unary { expr, .. } => patch_test_target(expr, name),
        _ => {}
    }
}

/// The bare names of every zero-parameter `test_*` function in the UNLOWERED source,
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
                    if f.name.starts_with("test_") && f.params.is_empty() {
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

/// Discover and run the tests in an already-linked module (`stem` = the entry file's
/// stem). Every ZERO-parameter function named `test_*` that the ENTRY file itself
/// declares is invoked through a synthesized `main` in a fresh interpreter. A test
/// passes by returning and fails by aborting (which `std/testing`'s assertions do,
/// with a message). Tests take no capabilities, so a suite provably has no effects.
/// `async_tests`/`gen_tests` are the bare names of the entry file's async/gen tests
/// (from `raw_test_shapes`, since lowering erased the AST flags). Returns
/// `(passed, failures)` where each failure is `(name, message)`.
fn run_tests_in_module(
    linked: &ast::Module,
    stem: &str,
    async_tests: &std::collections::HashSet<String>,
    gen_tests: &std::collections::HashSet<String>,
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
    let tests: Vec<(String, bool, bool)> = linked
        .items
        .iter()
        .filter_map(|it| match it {
            ast::Item::Function(f)
                if is_entry_function(&f.name, stem)
                    && f.name.rsplit('.').next().unwrap_or(&f.name).starts_with("test_")
                    && f.params.is_empty() =>
            {
                let bare = f.name.rsplit('.').next().unwrap_or(&f.name);
                Some((f.name.clone(), async_tests.contains(bare), gen_tests.contains(bare)))
            }
            _ => None,
        })
        .collect();
    let mut passed = Vec::new();
    let mut failed = Vec::new();
    for (test, is_async, is_gen) in tests {
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
                if let Some(ast::Stmt::Expr(e)) = f.body.stmts.first_mut() {
                    patch_test_target(e, &test);
                }
            }
        }
        m.items.extend(driver.items);
        // Run the test on the COMPILED WASM tier — the tier users ship — not the
        // interpreter oracle: a `witchy test` that passes must reflect the backend
        // that actually runs in production. A `test_*` is nullary (no capability
        // params), so the synthesized `main` needs no grants. A `testing.assert` /
        // `fail_with` lowers to `__witchy_abort`, which `run_wasm_bytes` surfaces as
        // the same `runtime error: <core>` the interpreter produced (RFC-0045 message
        // parity), so a failure reads identically. A module that does not lower is
        // itself a failure: the test cannot run where it ships.
        let outcome = match codegen::compile_module_binary(&m) {
            Ok(Some(bytes)) => run_wasm_bytes(&bytes).map(|_| ()),
            Ok(None) => Err("does not lower to the compiled backend (WASM)".to_string()),
            Err(e) => Err(e.to_string()),
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
    let (linked, stem) = link_file(path)?;
    let (async_tests, gen_tests) = raw_test_shapes(path);
    run_tests_in_module(&linked, &stem, &async_tests, &gen_tests)
}

/// `witchy test <file|dir>`: run in-language tests, print a cargo-style
/// report, and return whether everything passed.
fn run_tests(path: &str) -> Result<bool, String> {
    let mut files: Vec<String> = Vec::new();
    let meta = std::fs::metadata(path).map_err(|e| format!("cannot read `{path}`: {e}"))?;
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
        let (linked, stem) = match link_file(file) {
            Ok(v) => v,
            Err(e) if meta.is_dir() => {
                eprintln!("  skipped {file}: {e}");
                continue;
            }
            Err(e) => return Err(e),
        };
        let (async_tests, gen_tests) = raw_test_shapes(file);
        let (passed, failed) = match run_tests_in_module(&linked, &stem, &async_tests, &gen_tests) {
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

/// (RFC-0045) Extract the *core* of a routed runtime-abort message for message
/// parity: the text after the `runtime error: ` marker, with the interpreter's
/// `` `func`, line N: `` location prefix (which the compiled backend does not yet
/// reproduce — the site table is a deferred channel) stripped. Returns `None` for
/// a message that is not a `runtime error: …` (a bare wasm trap, a capability
/// refusal, a parse/type error), so only genuinely routed aborts are compared.
fn abort_core(msg: &str) -> Option<String> {
    let rest = msg.strip_prefix("runtime error: ")?;
    Some(strip_location_prefix(rest).to_string())
}

/// Strip the interpreter's EXACT `rt_at_line` location prefix — `` `<func>`, line
/// <N>: `` (func nonempty) or `line <N>: ` (func empty) — from the front of a
/// runtime-error body, leaving the message core. The match is precise (a
/// backtick-delimited name, the literal `, line `, one-or-more digits, then `: `)
/// so a `fail` message that itself contains backticks or `": "` is never
/// mis-stripped. Returns the input unchanged when no such prefix is present (the
/// compiled backend does not yet emit one — the site table is deferred).
fn strip_location_prefix(rest: &str) -> &str {
    // `` `<func>`, line <N>: ``
    if let Some(after_tick) = rest.strip_prefix('`') {
        if let Some((_func, tail)) = after_tick.split_once('`') {
            if let Some(after_line) = tail.strip_prefix(", line ") {
                if let Some(core) = split_after_line_number(after_line) {
                    return core;
                }
            }
        }
    }
    // `line <N>: `
    if let Some(after_line) = rest.strip_prefix("line ") {
        if let Some(core) = split_after_line_number(after_line) {
            return core;
        }
    }
    rest
}

/// Given the text right after `line ` (i.e. starting with the line number),
/// consume the digits and a following `: `, returning the remaining core. `None`
/// if the shape doesn't match (no digit, or no `: ` after the number).
fn split_after_line_number(s: &str) -> Option<&str> {
    let ndigits = s.bytes().take_while(u8::is_ascii_digit).count();
    if ndigits == 0 {
        return None;
    }
    s[ndigits..].strip_prefix(": ")
}

/// Exit codes for `witchy parity` — distinct so gate scripts branch on the code,
/// never on human text (RFC-0058 §2). `0` = agree or both-error-agree (a pass);
/// `2` = unexpected-error (a compile/link/lower failure or a missing file — a
/// regression for the known-good example corpus, a tolerated generator miss for
/// the fuzzer); `3` = a value/behavior divergence.
const PARITY_EXIT_UNEXPECTED: i32 = 2;
const PARITY_EXIT_DIVERGE: i32 = 3;

/// The positive-control sentinel (RFC-0058 §1). A control-char-delimited line no
/// real program can print; appending it to the *compiled* result under
/// `WITCHY_SEEDED_DIVERGENCE=1` forces a guaranteed DIVERGE — the self-test that
/// the parity gate can still fail. Read ONLY on the `witchy parity` path
/// (`parity_check`); the program-run path never consults it, so the injected fault
/// is inert in release execution (no divergent fixture ever lives in-repo).
const SEEDED_DIVERGENCE_SENTINEL: &str = "\u{1}witchy-seeded-divergence\u{1}";

/// Is the seeded-divergence positive control armed? The ONE reader of the env var
/// (RFC-0058 §1) — kept here on the parity path so release execution stays inert.
fn seeded_divergence_armed() -> bool {
    std::env::var_os("WITCHY_SEEDED_DIVERGENCE").is_some_and(|v| v == "1")
}

/// The four mechanical outcomes of a parity check. "Intended trap" is NOT judged
/// here (RFC-0058 §2) — parity reports only what it observed; a generator decides
/// whether a both-error-agree or an unexpected-error is acceptable.
enum ParityOutcome {
    /// Both backends produced equal output. Carries the compared line count.
    Agree { compared: usize, message: String },
    /// Both backends errored and agree (same routed abort core, or unrouted).
    BothErrorAgree { message: String },
    /// The backends diverge. `compared` is the matched-prefix line count.
    Diverge { compared: usize, message: String },
    /// A compile/link/lower failure, a missing `main`, or a missing file.
    Unexpected { message: String },
}

impl ParityOutcome {
    /// The `outcome=` token of the machine-readable stats line.
    fn tag(&self) -> &'static str {
        match self {
            ParityOutcome::Agree { .. } => "agree",
            ParityOutcome::BothErrorAgree { .. } => "both-error-agree",
            ParityOutcome::Diverge { .. } => "diverge",
            ParityOutcome::Unexpected { .. } => "unexpected-error",
        }
    }
    /// The `compared=` token: output lines actually compared (0 when none were).
    fn compared(&self) -> usize {
        match self {
            ParityOutcome::Agree { compared, .. } | ParityOutcome::Diverge { compared, .. } => {
                *compared
            }
            ParityOutcome::BothErrorAgree { .. } | ParityOutcome::Unexpected { .. } => 0,
        }
    }
    fn exit_code(&self) -> i32 {
        match self {
            ParityOutcome::Agree { .. } | ParityOutcome::BothErrorAgree { .. } => 0,
            ParityOutcome::Diverge { .. } => PARITY_EXIT_DIVERGE,
            ParityOutcome::Unexpected { .. } => PARITY_EXIT_UNEXPECTED,
        }
    }
    /// The human-readable line: agreements go to stdout, failures to stderr.
    fn message(&self) -> &str {
        match self {
            ParityOutcome::Agree { message, .. }
            | ParityOutcome::BothErrorAgree { message }
            | ParityOutcome::Diverge { message, .. }
            | ParityOutcome::Unexpected { message } => message,
        }
    }
    fn is_pass(&self) -> bool {
        matches!(self, ParityOutcome::Agree { .. } | ParityOutcome::BothErrorAgree { .. })
    }
}

/// Run `path` on both backends and classify the result into one of the four
/// `ParityOutcome`s. The compiled and interpreter runs happen regardless of either
/// failing (a trap on one side and a value on the other is itself a divergence), and
/// the abort-core comparison closes the routed-message gap (RFC-0045). This is the
/// oracle the differential fuzzer and the example sweep drive as `witchy parity`.
fn parity_check(path: &str) -> ParityOutcome {
    use std::path::Path;
    macro_rules! unexpected {
        ($($arg:tt)*) => {
            return ParityOutcome::Unexpected { message: format!($($arg)*) }
        };
    }
    let (linked, stem) = match link_file(path) {
        Ok(v) => v,
        Err(e) => unexpected!("{e}"),
    };
    if let Err(e) = typeck::check(&linked) {
        unexpected!("{e}");
    }
    // Honor `mode opt` here too (BUG-119): a copy-cliff or a missing ownership
    // convention is a hard error under `mode opt` on every other path
    // (check/run/sandbox/emit) — a program `check` rejects must not slip through
    // `parity` as an "unexpected error" masquerading as a compile miss.
    if let Err(e) = enforce_performance_modes(&linked, &stem) {
        unexpected!("{e}");
    }
    let has_main = linked
        .items
        .iter()
        .any(|it| matches!(it, ast::Item::Function(f) if f.name == "main"));
    if !has_main {
        unexpected!("`{path}` has no `main` to run");
    }
    // Compile first (borrows `linked`), then run the interpreter (consumes it).
    let bytes = match codegen::compile_module_binary(&linked) {
        Ok(Some(b)) => b,
        Ok(None) => unexpected!(
            "cannot compile to WASM: the program reached a construct the compiled backend \
             does not support (an interpreter-only feature?)"
        ),
        Err(e) => unexpected!("cannot compile to WASM (an interpreter-only feature?): {e}"),
    };
    // Run BOTH backends regardless of either failing: a program that errors on
    // one backend but produces a value on the other is itself a divergence (a
    // trap and a clean result are not the same behavior), so we must not return
    // early on the interpreter's error before observing what WASM does.
    // The compiled run export surfaces an Int-returning `main` as a final
    // print_int line (the sandbox/CLI turn it into the process exit code); the
    // interpreter's output has no such line, so drop it before comparing.
    let main_returns_int = linked.items.iter().any(|it| {
        matches!(it, ast::Item::Function(f) if f.name == "main"
            && matches!(&f.ret, Some(ast::Type::Named(n, _)) if n == "Int"))
    });
    // The root grant is concrete on BOTH backends (spec §13): if `main` binds a
    // `Secret` and `verify` was given no signing key, neither backend may run.
    // The interpreter already refuses (in `root_cap_for`); make the compiled side
    // refuse identically so they AGREE (both error) instead of diverging — the
    // interpreter rejecting while WASM mints a null secret was a real parity hole.
    let unmintable = linked.items.iter().find_map(|it| match it {
        ast::Item::Function(f) if f.name == "main" => {
            capabilities::unmintable_main_cap(&f.params, false)
        }
        _ => None,
    });
    let interp = interpreter::run_module(linked, Path::new("."), Vec::new()).map_err(|e| e.to_string());
    let compiled = match &unmintable {
        Some(msg) => Err(msg.clone()),
        None => run_wasm_bytes(&bytes).map(|mut lines| {
            if main_returns_int {
                lines.pop();
            }
            lines
        }),
    };
    // Positive control (RFC-0058 §1): when armed, deliberately perturb the COMPILED
    // side so the oracle MUST report a divergence — proving the gate can fail. An
    // `Ok` gains an impossible sentinel line; an `Err` becomes an `Ok` carrying only
    // the sentinel, so the value, both-error, and one-sided cases ALL diverge. Read
    // only here; the program-run path never applies it, so release execution is inert.
    let compiled = if seeded_divergence_armed() {
        match compiled {
            Ok(mut lines) => {
                lines.push(SEEDED_DIVERGENCE_SENTINEL.to_string());
                Ok(lines)
            }
            Err(_) => Ok(vec![SEEDED_DIVERGENCE_SENTINEL.to_string()]),
        }
    } else {
        compiled
    };
    match (interp, compiled) {
        (Ok(i), Ok(c)) if i == c => ParityOutcome::Agree {
            compared: i.len(),
            message: format!(
                "\u{2713} {path}: interpreter and compiled WASM agree ({} line(s) of output)",
                i.len()
            ),
        },
        (Ok(i), Ok(c)) => {
            let compared = i.iter().zip(c.iter()).take_while(|(a, b)| a == b).count();
            ParityOutcome::Diverge {
                compared,
                message: format!(
                    "\u{2717} {path}: the two backends DIVERGE\n  interpreter: {i:?}\n  compiled:    {c:?}"
                ),
            }
        }
        // Both fail: they agree on rejecting this input. (RFC-0045, message parity
        // — lenient notch) When the compiled backend surfaced a ROUTED abort
        // (`runtime error: <core>` via `__witchy_abort`), its message core must
        // MATCH the interpreter's core (same abort class, same dynamic data) — a
        // compiled trap at the wrong site or for the wrong reason now diverges
        // loudly, closing the occurrence-vs-semantics gap. An unrouted site (a bare
        // wasm `unreachable` trap, or a non-`runtime error:` message) still passes,
        // so sites become load-bearing as they are routed.
        (Err(i), Err(c)) => {
            if let (Some(ic), Some(cc)) = (abort_core(&i), abort_core(&c)) {
                if ic != cc {
                    return ParityOutcome::Diverge {
                        compared: 0,
                        message: format!(
                            "\u{2717} {path}: the two backends DIVERGE on the abort message\n  \
                             interpreter core: {ic:?}\n  compiled core:    {cc:?}"
                        ),
                    };
                }
            }
            ParityOutcome::BothErrorAgree {
                message: format!("\u{2713} {path}: interpreter and compiled WASM agree (both error)"),
            }
        }
        (Ok(i), Err(c)) => ParityOutcome::Diverge {
            compared: 0,
            message: format!(
                "\u{2717} {path}: the two backends DIVERGE\n  interpreter: Ok({i:?})\n  compiled:    Err({c})"
            ),
        },
        (Err(i), Ok(c)) => ParityOutcome::Diverge {
            compared: 0,
            message: format!(
                "\u{2717} {path}: the two backends DIVERGE\n  interpreter: Err({i})\n  compiled:    Ok({c:?})"
            ),
        },
    }
}

/// Compile a program to WASM and run it inside the capability-sandboxed VM,
/// granting exactly the authority its footprint declares. The compiled sandbox
/// currently links only the console (`print`) host, so it supports Console-only
/// (or pure) programs; anything needing `Dir`/`Net` is reported, not run.
/// Returns the program's output lines.
/// Compile a program to WebAssembly text (WAT) and return it — the same module
/// `sandbox` would run. For inspecting and optimizing the generated code.
fn emit_wat_file(path: &str) -> Result<String, String> {
    let (linked, stem) = link_file(path)?;
    typeck::check(&linked).map_err(|e| e.to_string())?;
    // Honor `mode opt` (BUG-163): `emit-wat` renders the SAME module `sandbox` runs,
    // so a copy-cliff / missing-convention that `check`, `emit-wasm`, and `sandbox`
    // reject must not be quietly rendered here (exit 0 with a copy-cliff file).
    enforce_performance_modes(&linked, &stem)?;
    // The WIR-as-WAT: the actual module the backend encodes and runs
    // (optimization passes included), rendered back to text for inspection —
    // a display of the real WIR, not a separately generated WAT string.
    let mut wir = codegen::assemble_wir_module(&linked)
        .map_err(|e| format!("cannot compile to WASM (an interpreter-only feature?): {e}"))?
        .ok_or_else(|| {
            "cannot compile to WASM: the program reached a construct the compiled backend \
             does not support (an interpreter-only feature?)"
                .to_string()
        })?;
    wir_opt::optimize(&mut wir);
    Ok(wir::to_wat(&wir))
}

/// Run an already-linked module on the compiled (WASM) backend with dev grants
/// derived from its footprint: Console output and argv always, plus Clock / Env /
/// Dir / Net / Secret each granted (at `dir_root` / `net_allow`) iff the footprint
/// shows the program uses it. `Dir`/`Net` rights narrow which host ops are linked.
/// This is the shared core of `witchy run` (dev, root at cwd) and `witchy sandbox`
/// (strict, announced grant) — one runtime for both, so dev == deploy. `strict_dir`
/// is the one host-policy difference: sandbox requires an explicit `--dir` (deny by
/// omission) while dev `run` defaults a `Dir` to the cwd.
#[allow(clippy::too_many_arguments)]
fn run_linked_compiled(
    linked: &ast::Module,
    dir_roots: Vec<std::path::PathBuf>,
    file_grants: Vec<std::path::PathBuf>,
    net_allow: Vec<String>,
    args: Vec<String>,
    signing_key: Option<[u8; 32]>,
    named_secrets: Vec<runtime::SecretGrant>,
    user_cap_fields: Vec<Vec<String>>,
    strict_dir: bool,
) -> Result<(Vec<String>, Option<i32>), String> {
    use crate::runtime::{Capabilities, Runtime};
    // The grant is what `main` RECEIVES — authority originates only there (witchy
    // has no ambient caps), so a linked library's public fns (a dependency's `pub fn
    // fetch(net)`, std `crypto.sign`'s `Secret`) are not entry points of THIS run
    // and must not widen the grant. `analyze().total` is the whole-program union
    // (the supply-chain surface the package gate diffs); a run wants only main's row
    // — which also means a `Secret` in some unreached signing path needs no key here.
    let grant = capabilities::run_grant(linked);
    // Only a BARE `Secret` is unmintable without a key — a `Secret` *is* its key, so
    // there is no empty one to hand over. A `SecretStore` with no secrets is a real,
    // mintable capability (the interpreter mints an empty store; see
    // `capabilities::unmintable_main_cap`), so binding one must NOT force a
    // `--secret`. This keeps `run`/`sandbox` aligned with `parity` and both backends,
    // which run a `main(…, SecretStore)` fine with an empty store (BUG-112).
    if grant.contains_key("Secret") && signing_key.is_none() && named_secrets.is_empty() {
        return Err(
            "this program needs a Secret, but the host granted none (provide `--signing-key <seed-file>`, `--secret name=value`, or `--secret-file name=path`)".to_string(),
        );
    }
    // Quiet: the captured lines are printed once by the caller (and an
    // Int-returning `main` surfaces its value as the LAST line, which the
    // caller turns into the process exit code, like the interpreter CLI).
    let mut caps = Capabilities {
        print: true,
        print_int: true,
        quiet: true,
        args,
        user_cap_fields,
        ..Default::default()
    };
    if grant.contains_key("Clock") {
        caps.clock = true;
    }
    if grant.contains_key("Rand") {
        caps.rand = true;
    }
    if grant.contains_key("Env") {
        caps.env = true;
    }
    if grant.contains_key("Exec") {
        caps.exec = true;
    }
    if let Some(rights) = grant.get("Dir") {
        let mut roots = dir_roots;
        if roots.is_empty() {
            // Deny by omission: an announced/strict run (`witchy sandbox`, `--grants`)
            // for untrusted code must NOT silently hand over the whole cwd — require an
            // explicit `--dir`, exactly as a `File`-binding `main` requires `--file`.
            // Only the dev `run` path keeps the cwd convenience (not a security boundary).
            if strict_dir {
                return Err(
                    "this program's `main` requires a `Dir`, but no subtree was granted (use `--dir <root>`)".to_string(),
                );
            }
            roots.push(std::path::PathBuf::from("."));
        }
        caps.dir_root = Some(roots.remove(0));
        caps.dir_roots = roots;
        caps.dir_read = rights.contains("Read");
        caps.dir_write = rights.contains("Write");
    }
    // RFC-0012: direct `File` grants — `main`'s `File` params are filled from
    // `--file` positionally (read/write is the param's compile-time right).
    if grant.contains_key("File") {
        if file_grants.is_empty() {
            return Err(
                "this program's `main` requires a `File`, but none was granted (use `--file <path>`)".to_string(),
            );
        }
        caps.file_grants = file_grants;
    }
    if let Some(rights) = grant.get("Net") {
        caps.net_allow = Some(net_allow);
        caps.net_connect = rights.contains("Connect");
        caps.net_listen = rights.contains("Listen");
    }
    if grant.contains_key("Secret") || grant.contains_key("SecretStore") {
        caps.signing_key = signing_key;
        // The signing key is the `signing` secret at handle 0, so a `Secret`
        // capability (always handle 0) and `SecretStore.get("signing")` agree.
        // The `--secret`/`--secret-file` grants follow, each a named `Secret`
        // reachable by `SecretStore.get(name)` / `.require(name)`.
        if let Some(seed) = signing_key {
            caps.secrets.push(runtime::SecretGrant::new("signing", seed.to_vec()));
        }
        caps.secrets.extend(named_secrets);
    }
    let wasm = compile_linked_to_wasm_cached(linked)?;
    let mut rt = Runtime::batch().map_err(|e| e.to_string())?;
    let mut vm = rt
        .spawn(&wasm, caps, RUN_MEMORY_PAGES)
        .map_err(|e| e.to_string())?;
    // Surface the *root cause*, not wasmtime's outer "error while executing at
    // wasm backtrace…" wrapper: a confinement violation then reads as the same
    // clean "`..` escapes the Dir capability" both backends print, and a genuine
    // trap reads as "wasm trap: …" rather than a stack dump.
    vm.run().map_err(|e| {
        // Default: the clean root-cause message both backends agree on. With
        // WITCHY_WASM_BACKTRACE set, also dump the full named wasm backtrace
        // (the emitted name section makes frames readable) for debugging traps.
        if std::env::var_os("WITCHY_WASM_BACKTRACE").is_some() {
            eprintln!("{e:?}");
        }
        e.root_cause().to_string()
    })?;
    let mut lines = vm.output();
    // At the process boundary an Int-returning `main` is the exit code (the
    // run export surfaces it as the final print_int line; pop and convert).
    let main_returns_int = linked.items.iter().any(|it| {
        matches!(it, ast::Item::Function(f) if f.name == "main"
            && matches!(&f.ret, Some(ast::Type::Named(n, _)) if n == "Int"))
    });
    let exit_code = if main_returns_int {
        lines.pop().and_then(|s| s.parse::<i32>().ok())
    } else {
        None
    };
    Ok((lines, exit_code))
}

/// Compile a linked module to a wasm BINARY through the WIR → wasm-binary
/// pipeline (`compile_module_binary`). A program that doesn't fully lower
/// surfaces as a hard "cannot compile" error — there is no WAT fallback.
fn compile_linked_to_wasm(linked: &ast::Module) -> Result<Vec<u8>, String> {
    codegen::compile_module_binary(linked)
        .map_err(|e| format!("cannot compile to WASM (an interpreter-only feature?): {e}"))?
        .ok_or_else(|| {
            "cannot compile to WASM: the program reached a construct the compiled backend \
             does not support (an interpreter-only feature?)"
                .to_string()
        })
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
    use crate::opt::{self, Opt};
    Opt::ALL
        .iter()
        .filter(|o| opt::enabled(**o))
        .map(|o| o.name())
        .collect::<Vec<_>>()
        .join(",")
}

/// Compile `linked` to wasm, reusing a SOURCE-keyed cache to skip codegen on warm
/// runs. The key hashes the full linked AST + the compiler fingerprint + the active
/// opt set — every input that determines the emitted wasm — so it is sound by
/// construction: a key that fails to reflect some input simply MISSES and recompiles,
/// it can never serve wrong code. Distinct from the runtime's post-Cranelift module
/// cache (`~/.cache/witchy/aot`); this one (`~/.cache/witchy/src`) caches the wasm
/// bytes so the front-end's codegen is skipped, not just the native compile. The
/// capability grant and every security check still run from `linked` on every run —
/// only the wasm is cached.
fn compile_linked_to_wasm_cached(linked: &ast::Module) -> Result<Vec<u8>, String> {
    use sha2::{Digest, Sha256};
    let key = {
        let mut h = Sha256::new();
        h.update(format!("{linked:?}").as_bytes());
        h.update(b"\0");
        h.update(compiler_fingerprint().as_bytes());
        h.update(b"\0");
        h.update(active_opt_key().as_bytes());
        h.finalize().iter().map(|b| format!("{b:02x}")).collect::<String>()
    };
    let path = (|| -> Option<std::path::PathBuf> {
        let base = std::env::var_os("XDG_CACHE_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".cache")))?;
        let dir = base.join("witchy").join("src");
        std::fs::create_dir_all(&dir).ok()?;
        Some(dir.join(format!("{key}.wasm")))
    })();
    if let Some(p) = &path {
        if let Ok(bytes) = std::fs::read(p) {
            return Ok(bytes);
        }
    }
    let wasm = compile_linked_to_wasm(linked)?;
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
    let has_main = linked
        .items
        .iter()
        .any(|it| matches!(it, ast::Item::Function(f) if f.name == "main"));
    if !has_main {
        return Err(format!("`{path}` has no `main` to run"));
    }
    // The sandbox grants EXACTLY what a run gives `main` (see `run_grant`) — not the
    // whole-program union, so a verify-only program that imports `crypto` is not
    // forced to be handed a `Secret` it never binds.
    let grant = capabilities::run_grant(&linked);
    if grant.contains_key("Secret") && signing_key.is_none() && named_secrets.is_empty() {
        return Err(format!(
            "`{path}` needs a Secret, but the host granted none (provide `--signing-key <seed-file>`, `--secret name=value`, or `--secret-file name=path`)"
        ));
    }
    eprintln!(
        "sandboxing `{path}` \u{2014} granted exactly: {}",
        capabilities::show_caps(&grant)
    );
    run_linked_compiled(&linked, dir_roots, file_grants, net_allow, args, signing_key, named_secrets, Vec::new(), true)
}

/// Resolve a `[secrets]` entry's `from = "env:VAR"` to the secret bytes the host
/// holds. The grant document never carries the value — only where to fetch it.
fn resolve_secret_from(from: &str) -> Result<Vec<u8>, String> {
    if let Some(var) = from.strip_prefix("env:") {
        std::env::var(var)
            .map(String::into_bytes)
            .map_err(|_| format!("grant secret resolver `env:{var}`: ${var} is not set"))
    } else {
        Err(format!("unsupported grant secret resolver `{from}` (expected `env:VAR`)"))
    }
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
    let (linked, _stem) = link_file(path)?;
    typeck::check(&linked).map_err(|e| e.to_string())?;
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
                    file_grants.push(std::path::PathBuf::from(&g.path));
                }
                Some(ast::Type::Named(n, _)) if n == "Dir" => {
                    let g = doc.dirs.get(&p.name).ok_or_else(|| {
                        format!("grant `{grants_path}` has no `[dirs].{}` for `main` parameter `{}`", p.name, p.name)
                    })?;
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
    // the bare `Secret` handle (`--signing-key`) is not granted via documents here.
    run_linked_compiled(&linked, dir_roots, file_grants, net_allow, args, None, named_secrets, user_cap_fields, true)
}

/// The `witchy.*` host functions a compiled module imports — its authority
/// surface. For a *precompiled* program (`app.wasm`) the imports ARE the
/// footprint: there is no source to analyze, but a module physically cannot call
/// a host op it does not import, so granting exactly the imported families is the
/// distribution counterpart of `capabilities::analyze`.
fn witchy_imports(bytes: &[u8]) -> Result<Vec<String>, String> {
    use wasmtime::{Engine, Module};
    let engine = Engine::default();
    let module = Module::new(&engine, bytes).map_err(|e| format!("not a valid wasm module: {e}"))?;
    Ok(module
        .imports()
        .filter(|i| i.module() == "witchy")
        .map(|i| i.name().to_string())
        .collect())
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
    use crate::runtime::{Capabilities, Runtime};
    let needs = witchy_imports(bytes)?;
    let has = |n: &str| needs.iter().any(|i| i == n);
    let dir_read = [
        "dir_subdir", "dir_read_len", "dir_exists", "dir_is_dir", "dir_list_size",
        // BUG-013: the runtime links these too — omitting them left a precompiled `.wasm`
        // failing `unknown import: witchy::dir_*` under `--dir`.
        "dir_only", "dir_open",
    ]
    .iter()
    .any(|n| has(n));
    let dir_write = ["dir_write", "dir_append", "dir_make_dir", "dir_create"].iter().any(|n| has(n));
    let net_connect = [
        "net_connect", "net_try_connect", "net_restrict", "net_send_line", "net_send_bytes",
        "net_recv_line_len", "net_recv_all_len", "net_recv_bytes_len", "net_close",
        // BUG-013: pinned/resolve/deny variants the runtime links but the classifier missed.
        "net_deny", "net_resolve_size", "net_connect_pinned", "net_try_connect_pinned",
    ]
    .iter()
    .any(|n| has(n));
    let net_listen = ["net_listen", "net_accept", "serve_pool"].iter().any(|n| has(n));
    let needs_secret =
        has("crypto.sign") || has("crypto.public_key") || has("crypto.reveal") || has("secretstore_lookup");
    if needs_secret && signing_key.is_none() && named_secrets.is_empty() {
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
    if has("now") {
        caps.clock = true;
    }
    if has("rand_u64") {
        caps.rand = true;
    }
    if has("env_len") || has("env_fill") {
        caps.env = true;
    }
    if has("exec_run") {
        caps.exec = true;
    }
    if dir_read || dir_write {
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
    if has("mint_file") && file_grants.is_empty() {
        return Err(
            "this program's `main` requires a `File`, but none was granted (use `--file <path>`)".to_string(),
        );
    }
    caps.file_grants = file_grants;
    if net_connect || net_listen {
        caps.net_allow = Some(net_allow);
        caps.net_connect = net_connect;
        caps.net_listen = net_listen;
    }
    if needs_secret {
        caps.signing_key = signing_key;
        if let Some(seed) = signing_key {
            caps.secrets.push(runtime::SecretGrant::new("signing", seed.to_vec()));
        }
        caps.secrets.extend(named_secrets);
    }
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

/// Compile a `.witchy` program to a wasm binary and write it to `out`. The
/// produced module is the Tier-1 distribution artifact: run it with
/// `witchy <out>` (authority granted from its imports).
fn emit_wasm_file(path: &str, out: &str) -> Result<(), String> {
    let (linked, _stem) = link_file(path)?;
    typeck::check(&linked).map_err(|e| e.to_string())?;
    enforce_performance_modes(&linked, _stem.as_str())?;
    let binary = compile_linked_to_wasm(&linked)?;
    std::fs::write(out, &binary).map_err(|e| format!("cannot write `{out}`: {e}"))?;
    Ok(())
}

/// Run a `build` step in the **zero-ambient WASM sandbox**: compile it (the
/// `build` entrypoint becomes the `run` export), then instantiate under a
/// `Capabilities` granting *only* the build output sandbox and read roots — so
/// the module physically has no `dir_*`/`net_*`/`print` import to call, and a
/// `..` write traps via the same confinement as a runtime `Dir`. Returns the
/// generated source files written into `out_dir`.
///
/// Used for deterministic steps (BuildOut/BuildRead only). It is hard isolation
/// for untrusted codegen logic: a bug in the interpreter could not help a build
/// step here, because the dangerous host functions simply are not linked.
pub fn run_build_step_sandboxed(
    module: ast::Module,
    out_dir: std::path::PathBuf,
    read_roots: Vec<std::path::PathBuf>,
) -> Result<Vec<String>, String> {
    use runtime::{Capabilities, Runtime};
    std::fs::create_dir_all(&out_dir).map_err(|e| format!("build: output dir: {e}"))?;
    let wasm = codegen::compile_build_module(&module).map_err(|e| e.message)?;
    let caps = Capabilities {
        build_out: Some(out_dir.clone()),
        build_read_roots: read_roots,
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
    // as the clean `runtime error: <core>` the interpreter produces, which the
    // differential harness (`parity_check`) compares for message parity.
    vm.run().map_err(|e| e.root_cause().to_string())?;
    Ok(vm.output())
}

/// Read, parse, and compute the host-capability footprint of a source file.
fn analyze_file(path: &str) -> Result<capabilities::Footprint, String> {
    // BUG-179: a footprint computed over code that doesn't type-check is meaningless
    // (an undefined-function call, a type error). Link + type-check the whole program
    // first, so `caps`/`caps-diff` refuse a source that `check` would reject rather
    // than reporting a footprint for it.
    let (linked, _stem) = link_file(path)?;
    typeck::check(&linked).map_err(|e| e.to_string())?;
    // Report the footprint of the ENTRY file's own items (unprefixed names, matching
    // the existing per-function output) — but with its `comptime:` blocks EXPANDED
    // (BUG-178). A `comptime:` block that `emit`s `pub fn generated(net: Net)` adds a
    // real capability-bearing API; `capabilities::analyze` treats generated code
    // exactly like handwritten code, so it must see the expanded items. This is the
    // same additive per-module pass the linker runs, applied to the single module.
    let src = std::fs::read_to_string(path).map_err(|e| format!("cannot read `{path}`: {e}"))?;
    let mut module = parser::parse_module(&src).map_err(|e| e.to_string())?;
    let stem = std::path::Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(path);
    comptime::expand(stem, &mut module).map_err(|e| format!("{path}: {e}"))?;
    Ok(capabilities::analyze(&module))
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
) -> Result<Vec<String>, String> {
    let (linked, _) = link_file(path)?;
    typeck::check(&linked).map_err(|e| e.to_string())?;
    let out = out_dir.unwrap_or_else(|| std::path::PathBuf::from("build-out"));
    // Hard isolation when it adds value (matching the package manager): a
    // *deterministic* step — only BuildOut/BuildRead, no granted env/exec/net —
    // is a pure function of its inputs, so it runs in the zero-ambient WASM
    // sandbox where a `..` write traps with no host import to call. Steps needing
    // BuildExec/BuildNet/BuildEnv run on the capability-sound interpreter: their
    // host process/socket/env I/O is confined by the grant allow-list, and the
    // compiled backend has no per-tool exec / per-key env allow-list to enforce it
    // (only all-or-nothing bools; net already has one). This is the last deliberate
    // interpreter use in a production path — RFC-0068 proposes closing it by giving
    // `Capabilities` exec/env allow-lists so every build step runs compiled.
    let footprint = capabilities::analyze(&linked);
    let sandboxable = env_keys.is_empty()
        && exec_tools.is_empty()
        && !footprint.build.is_empty()
        && footprint.build.keys().all(|k| *k == "BuildOut" || *k == "BuildRead");
    if sandboxable {
        return run_build_step_sandboxed(linked, out, read_roots);
    }
    let grants = interpreter::BuildGrants {
        out_dir: out,
        read_roots,
        env_keys,
        exec_tools,
        ..Default::default()
    };
    interpreter::run_build_step(linked, grants).map_err(|e| e.message)
}

/// Print the host-capability footprint of a single source file: every
/// capability-touching function (entry points and private helpers), and the
/// union over the entry points.
fn report_capabilities(path: &str) -> Result<(), String> {
    let fp = analyze_file(path)?;
    let show = capabilities::show_caps;
    println!("Host-capability footprint of {path}:");
    let width = fp
        .per_function
        .iter()
        .map(|e| e.name.len())
        .max()
        .unwrap_or(0)
        .max("total".len());
    for e in &fp.per_function {
        let refined = if e.brands.is_empty() {
            String::new()
        } else {
            let names: Vec<&str> = e.brands.iter().map(String::as_str).collect();
            format!("  (refined: {})", names.join(", "))
        };
        println!("  {:<width$}  {}{}", e.name, show(&e.capabilities), refined);
    }
    println!("  {:<width$}  {}", "total", show(&fp.total));
    // RFC-0038: the grantable user-capability axis — bare policy tokens `main`
    // receives (e.g. `UiRoot`), carrying no host authority but reviewable as a
    // widening (a new package in the policy TCB / new library-effect authority).
    if !fp.user_caps.is_empty() {
        let names: Vec<&str> = fp.user_caps.iter().map(String::as_str).collect();
        println!("  {:<width$}  {}", "user caps", names.join(", "));
    }
    // The build axis (only when the rune ships a `build` step). Runtime authority
    // is enforced by the type system; the build footprint is the supply-chain
    // signal — what a rune's build step is allowed to do, outside the consumer's
    // type-checked call graph.
    if !fp.build.is_empty() {
        println!("Build-time footprint of {path}:");
        println!("  {:<width$}  {}", "build", show(&fp.build));
    }
    Ok(())
}

/// Compare the capability footprints of two versions of a module and report any
/// *widening* — host authority the newer version demands that the older did not.
/// Returns whether it widened so the caller can fail the supply-chain gate. This
/// is what makes `witchy` dependency updates auditable: unlike Go, where a
/// version bump can silently start touching the network, a widening is visible
/// and blockable here.
fn report_capability_diff(old_path: &str, new_path: &str) -> Result<bool, String> {
    let old = analyze_file(old_path)?;
    let new = analyze_file(new_path)?;
    let d = capabilities::diff(&old, &new);
    println!("Capability footprint diff {old_path} -> {new_path}:");
    println!("  old total:  {}", capabilities::show_caps(&old.total));
    println!("  new total:  {}", capabilities::show_caps(&new.total));
    println!("  added:      {}", capabilities::show_caps(&d.added));
    println!("  removed:    {}", capabilities::show_caps(&d.removed));
    if !old.build.is_empty() || !new.build.is_empty() {
        println!("  build old:  {}", capabilities::show_caps(&old.build));
        println!("  build new:  {}", capabilities::show_caps(&new.build));
        println!("  build +:    {}", capabilities::show_caps(&d.build_added));
        println!("  build -:    {}", capabilities::show_caps(&d.build_removed));
    }
    let join = |s: &std::collections::BTreeSet<String>| {
        if s.is_empty() {
            "(none)".to_string()
        } else {
            s.iter().cloned().collect::<Vec<_>>().join(", ")
        }
    };
    if !d.refinements_dropped.is_empty() || !d.refinements_gained.is_empty() {
        println!(
            "  refinements: dropped {} / gained {}",
            join(&d.refinements_dropped),
            join(&d.refinements_gained)
        );
    }
    if !old.user_caps.is_empty() || !new.user_caps.is_empty() {
        println!("  user caps +: {}", join(&d.user_caps_added));
        println!("  user caps -: {}", join(&d.user_caps_removed));
    }
    let mut flagged = false;
    if !d.user_caps_added.is_empty() {
        // A new grantable (user) capability carries no host authority, but it IS a
        // widening: `main` now receives a policy token it did not before, expanding
        // the policy TCB — and `FootprintDiff::widened` counts it, so the exit code
        // is 2. Surface it in the message too, so the two agree (BUG-314): previously
        // this printed "OK: no widening" yet exited 2.
        println!(
            "USER-CAP WIDENING: the newer version's `main` receives new grantable capabilities ({}). \
             They confer no host authority but widen the policy TCB — review before trusting.",
            join(&d.user_caps_added)
        );
        flagged = true;
    }
    if d.build_widened() {
        // The high-signal supply-chain event: build-time execution is outside the
        // consumer's type-checked call graph, so a new build cap is the thing the
        // gate must catch.
        println!(
            "BUILD WIDENING: the newer version's build step demands new build-time authority ({}). \
             It cannot run until you grant it (`--allow-build-cap` + a `[build.grants]` entry).",
            capabilities::show_caps(&d.build_added)
        );
        flagged = true;
    }
    if !d.added.is_empty() {
        println!(
            "WIDENING: the newer version demands new host authority ({}). Review before trusting.",
            capabilities::show_caps(&d.added)
        );
        flagged = true;
    }
    if !flagged {
        if !d.refinements_dropped.is_empty() {
            // Same authority on both axes, but a brand was dropped — a confined
            // capability loosened to its bare form. Not a widening, but an intent
            // change worth surfacing.
            println!(
                "OK on authority, but a refinement was dropped ({}): a confined capability loosened to its bare form. Worth a look.",
                join(&d.refinements_dropped)
            );
        } else {
            println!("OK: no widening — the newer version demands no new authority on either axis.");
        }
    }
    Ok(d.widened())
}

/// RFC-0013: cross-check a grant document against a program's computed footprint.
/// Returns `true` when there is an UNDER-grant (the fatal case): the code needs
/// authority the grant withholds, so the program would fail at the missing
/// capability anyway. An over-grant (authority the code never uses) only warns.
fn report_grant_check(prog_path: &str, grants_path: &str) -> Result<bool, String> {
    let footprint = analyze_file(prog_path)?;
    let doc_src = std::fs::read_to_string(grants_path)
        .map_err(|e| format!("cannot read `{grants_path}`: {e}"))?;
    let doc = crate::grants::GrantDoc::parse(&doc_src)?;
    let grant = doc.cap_set();
    let check = crate::grants::cross_check(&grant, &footprint.total);
    println!("Grant cross-check: `{grants_path}` vs the footprint of `{prog_path}`");
    println!("  code needs:  {}", capabilities::show_caps(&footprint.total));
    println!("  grant gives: {}", capabilities::show_caps(&grant));
    if check.clean() {
        println!("  OK: the grant matches what the code exercises exactly.");
    }
    if !check.over_grant.is_empty() {
        println!(
            "  WARN over-grant (authority the code never exercises): {}",
            capabilities::show_caps(&check.over_grant)
        );
    }
    if !check.under_grant.is_empty() {
        println!(
            "  ERROR under-grant (authority the code needs but the grant withholds): {}",
            capabilities::show_caps(&check.under_grant)
        );
    }
    Ok(!check.sufficient())
}

/// BUG-108 / BUG-114: the global mode selector (`--release`/`--debug`) is a LEADING
/// flag; a mode flag in the guest's argv (after the program file) must not flip the
/// compiler's optimization mode nor be double-consumed.
#[cfg(test)]
mod cli_flag_tests {
    use super::leading_opt_mode;

    fn argv(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn mode_flags_before_the_file_are_global() {
        assert_eq!(leading_opt_mode(&argv(&["--release", "foo.witchy"])), Some("release"));
        assert_eq!(leading_opt_mode(&argv(&["--debug", "sandbox", "foo.witchy"])), Some("debug"));
        // `--debug` wins over `--release` when both lead (maximal debuggability).
        assert_eq!(leading_opt_mode(&argv(&["--release", "--debug", "foo.witchy"])), Some("debug"));
    }

    #[test]
    fn mode_flags_in_guest_argv_are_ignored() {
        assert_eq!(leading_opt_mode(&argv(&["foo.witchy", "--release"])), None);
        assert_eq!(leading_opt_mode(&argv(&["app.wasm", "--debug", "hello"])), None);
        assert_eq!(leading_opt_mode(&argv(&["foo.witchy"])), None);
        assert_eq!(leading_opt_mode(&argv(&[])), None);
    }
}

/// End-to-end coverage: every shipped example must type-check and produce the
/// expected result (interpreted), or type-check and compile to valid WASM.
#[cfg(test)]
mod example_tests;
