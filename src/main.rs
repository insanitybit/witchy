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
pub use witchy::interpreter;
pub use witchy::lexer;
pub use witchy::linker;
mod lsp;
pub use witchy::native;
pub use witchy::optimize;
pub use witchy::parser;
mod pm;
pub use witchy::records;
mod runtime;
pub use witchy::traits;
pub use witchy::typeck;
pub use witchy::value;
pub use witchy::wir;
pub use witchy::wir_encode;
pub use witchy::wir_opt;
#[cfg(feature = "native")]
pub use witchy::wir_prelude;

use std::time::Duration;

use runtime::{Capabilities, Runtime};

/// A well-behaved VM that was granted `print`.
const GREETER: &str = r#"
(module
  (import "witchy" "print" (func $print (param i32 i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "hello from inside the sandbox\n")
  (func (export "run")
    (call $print (i32.const 0) (i32.const 30))))
"#;

/// A "malicious library": it *tries* to import `print`, but we will grant it
/// nothing. It must fail to even come up.
const MALICIOUS: &str = r#"
(module
  (import "witchy" "print" (func $print (param i32 i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "I am exfiltrating your secrets\n")
  (func (export "run")
    (call $print (i32.const 0) (i32.const 31))))
"#;

/// A greedy VM: it declares 4 pages of initial memory. We will cap it at 1,
/// so it must be denied at instantiation.
const GREEDY: &str = r#"
(module
  (memory (export "memory") 4)
  (func (export "run")))
"#;

/// A runaway VM: an infinite loop that never yields. The scheduler must be
/// able to preempt it.
const RUNAWAY: &str = r#"
(module
  (memory (export "memory") 1)
  (func (export "run")
    (loop $forever (br $forever))))
"#;

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
    witchy demo                                   built-in capability/runtime demonstration

Package commands: new, init, add, build, run [args...], update, audit, tree,
outdated, why, why-cap, verify, vendor, publish, promote, yank, list — run
`witchy coven` for the full package-manager help. All of them accept
`-C <dir>`; `witchy run` passes everything after `run` (or after `--`) to the
program as `main`'s `args`, including `--help`."
    );
}

fn main() -> wasmtime::Result<()> {
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
    if std::env::args().nth(1).as_deref() == Some("caps") {
        let path = match std::env::args().nth(2) {
            Some(p) => p,
            None => match pm::project_entry_file() {
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
        use std::collections::{HashSet, VecDeque};
        let mut net_allow: Vec<String> = Vec::new();
        let mut pm_args: Vec<String> = Vec::new();
        let mut argv = std::env::args().skip(2);
        while let Some(a) = argv.next() {
            if a == "--net" {
                match argv.next() {
                    Some(addr) => net_allow.push(addr),
                    None => {
                        eprintln!("--net needs a host:port");
                        std::process::exit(1);
                    }
                }
            } else {
                pm_args.push(a);
            }
        }
        // Auto-grant Net to the configured registry (COVEN_URL), matching the Rust
        // CLI, so registry commands need no explicit `--net`. The front-end reads
        // COVEN_URL itself (via Env) when no host:port argument is given.
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
            linker::link(modules, "pm").map_err(|e| e.to_string())
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
        let bin = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        match interpreter::run_module_exit_dirs(module, vec![cwd, bin], net_allow, pm_args, None) {
            Ok((lines, code)) => {
                for l in &lines {
                    println!("{l}");
                }
                std::process::exit(code);
            }
            Err(e) => {
                eprintln!("{}", e.message);
                std::process::exit(1);
            }
        }
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
            linker::link(modules, "coven").map_err(|e| e.to_string())
        })();
        let module = match link_result {
            Ok(m) => m,
            Err(e) => { eprintln!("{e}"); std::process::exit(1); }
        };
        if let Err(e) = typeck::check(&module) {
            eprintln!("{e}");
            std::process::exit(1);
        }
        // The listen addr is the only Net authority coven is granted.
        let net_allow = vec![addr];
        match interpreter::run_module_exit_dirs(module, vec![root], net_allow, coven_args, signing_key) {
            Ok((lines, code)) => {
                for l in &lines {
                    println!("{l}");
                }
                std::process::exit(code);
            }
            Err(e) => {
                eprintln!("{}", e.message);
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
            std::process::exit(1);
        };
        if let Err(e) = verify_file(&path) {
            eprintln!("{e}");
            std::process::exit(1);
        }
        return Ok(());
    }
    // `witchy sandbox [--dir <root>] [--net <host:port>]... <file> [args...]`
    // compiles the program to WASM and runs it in the capability-sandboxed VM,
    // granted exactly its computed footprint. `--dir` picks the subtree backing
    // a granted Dir (default `.`); each `--net` allowlists an address.
    if std::env::args().nth(1).as_deref() == Some("sandbox") {
        // Multiple `--dir` grants map positionally to `main`'s `Dir` params: the
        // first backs handle 0, the rest handles 1.. (rfcs/0004-self-hosted-cli.md).
        let mut dir_roots: Vec<std::path::PathBuf> = Vec::new();
        let mut net_allow: Vec<String> = Vec::new();
        let mut signing_key: Option<[u8; 32]> = None;
        let mut named_secrets: Vec<(String, Vec<u8>)> = Vec::new();
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
                _ if path.is_none() => path = Some(a),
                _ => prog_args.push(a),
            }
        }
        let Some(path) = path else {
            eprintln!("usage: witchy sandbox [--dir <root>] [--net <host:port>]... [--signing-key <seed-file>] [--secret name=value] [--secret-file name=path] <file.witchy> [args...]");
            std::process::exit(1);
        };
        // A precompiled `.wasm` runs directly (authority from its imports); a
        // `.witchy` source is compiled then run, granted its computed footprint.
        let result = if path.ends_with(".wasm") {
            run_wasm_file(&path, dir_roots, net_allow, prog_args, signing_key, named_secrets)
        } else {
            run_file_sandboxed(&path, dir_roots, net_allow, prog_args, signing_key, named_secrets)
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
        // `witchy fmt --check <file>` verifies formatting without rewriting (for
        // CI): exit 0 if already canonical, 1 if it would change.
        let check = std::env::args().nth(2).as_deref() == Some("--check");
        let path = std::env::args().nth(if check { 3 } else { 2 });
        let Some(path) = path else {
            eprintln!("usage: witchy fmt [--check] <file.witchy>");
            std::process::exit(1);
        };
        match std::fs::read_to_string(&path) {
            Ok(src) => match format::reformat(&src) {
                Some(out) => {
                    if check {
                        if out != src {
                            eprintln!("witchy fmt: `{path}` is not formatted");
                            std::process::exit(1);
                        }
                    } else if let Err(e) = std::fs::write(&path, out) {
                        eprintln!("witchy fmt: {e}");
                        std::process::exit(1);
                    }
                }
                None => {
                    eprintln!("witchy fmt: cannot format `{path}` (parse error or unsupported construct)");
                    std::process::exit(1);
                }
            },
            Err(e) => {
                eprintln!("witchy fmt: cannot read `{path}`: {e}");
                std::process::exit(1);
            }
        }
        return Ok(());
    }
    // `witchy --bench` compares interpreter vs compiled execution.
    if std::env::args().nth(1).as_deref() == Some("--bench") {
        return run_benchmarks();
    }
    // coven package-manager subcommands (`witchy add`, `build`, `publish`, ...)
    // are checked before the file/`--net` runner so they intercept first.
    if let Some(a1) = std::env::args().nth(1) {
        if pm::cli::is_command(&a1) {
            let args: Vec<String> = std::env::args().skip(1).collect();
            if let Err(e) = pm::cli::run(&args) {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
            return Ok(());
        }
    }
    // `witchy [--net <host:port>]... <file.witchy>` runs a program, granting the
    // listed hosts to its `Net` capability (the host decides what authority to
    // hand over). With no file argument, run the demos.
    {
        let mut net_allow: Vec<String> = Vec::new();
        let mut file: Option<String> = None;
        let mut prog_args: Vec<String> = Vec::new();
        let mut signing_key: Option<[u8; 32]> = None;
        let mut named_secrets: Vec<(String, Vec<u8>)> = Vec::new();
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            if file.is_some() {
                // Everything after the program file is the program's own argv —
                // passed through verbatim (flags here belong to the program).
                prog_args.push(arg);
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
            // Standard help flags show the usage overview.
            Some("--help" | "-h" | "help") => {
                print_usage();
                return Ok(());
            }
            // `witchy demo` runs the built-in capability/runtime demonstration
            // below; fall through to it.
            Some("demo") => {}
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
                match run_wasm_file(path, Vec::new(), net_allow, prog_args, signing_key, named_secrets) {
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
                return Ok(());
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
                return Ok(());
            }
            // Bare `witchy` (or only flags): show usage rather than the demo.
            None => {
                print_usage();
                return Ok(());
            }
        }
    }

    let mut rt = Runtime::new()?;

    println!("== M2: capability gating ==");

    // Granted the `print` capability — works.
    let mut greeter = rt.spawn(GREETER, Capabilities { print: true, ..Default::default() }, 4)?;
    greeter.run()?;

    // Granted nothing — must fail to instantiate because `witchy.print` is not
    // linked into its VM.
    match rt.spawn(MALICIOUS, Capabilities::none(), 4) {
        Ok(_) => println!("!! SECURITY FAILURE: ungranted VM was allowed to instantiate"),
        Err(e) => println!("DENIED (as designed): {e}"),
    }

    println!("\n== M3: containment ==");

    // Memory budget: the greedy VM wants 4 pages but is capped at 1.
    match rt.spawn(GREEDY, Capabilities::none(), 1) {
        Ok(_) => println!("!! BUDGET FAILURE: over-budget VM was allowed to start"),
        Err(e) => println!("memory budget enforced: {e}"),
    }

    // Preemption: the runaway VM loops forever; the scheduler interrupts it.
    let mut runaway = rt.spawn(RUNAWAY, Capabilities::none(), 4)?;
    match rt.run_with_budget(&mut runaway, Duration::from_millis(50)) {
        Ok(_) => println!("!! PREEMPTION FAILURE: runaway VM finished on its own"),
        Err(e) => {
            let reason = e
                .downcast_ref::<wasmtime::Trap>()
                .map(|t| t.to_string())
                .unwrap_or_else(|| e.to_string());
            println!("PREEMPTED (as designed): {reason}");
        }
    }

    run_witchy("witchy language (interpreter)", include_str!("../examples/hello/src/hello.witchy"));
    run_witchy("witchy mutable value semantics", include_str!("../examples/mutate/src/mutate.witchy"));
    run_witchy("witchy ownership", include_str!("../examples/ownership/src/ownership.witchy"));
    run_witchy("witchy features combined", include_str!("../examples/commands/src/commands.witchy"));
    run_witchy("witchy fizzbuzz (while, %, if/else)", include_str!("../examples/fizzbuzz/src/fizzbuzz.witchy"));
    run_witchy("witchy tuples (multiple return values)", include_str!("../examples/tuples/src/tuples.witchy"));
    run_witchy("witchy generics (swap any pair)", include_str!("../examples/generics/src/generics.witchy"));
    run_witchy("witchy generic ADTs (Result)", include_str!("../examples/result/src/result.witchy"));
    run_witchy("witchy ? error propagation", include_str!("../examples/try/src/try.witchy"));
    run_witchy("witchy for-in loops over lists", include_str!("../examples/loops/src/loops.witchy"));
    run_witchy("witchy list patterns (head/tail)", include_str!("../examples/listmatch/src/listmatch.witchy"));
    run_witchy("witchy records (named fields)", include_str!("../examples/records/src/records.witchy"));
    run_witchy("witchy record update", include_str!("../examples/record_update/src/record_update.witchy"));
    run_witchy("witchy expression evaluator (recursive ADT)", include_str!("../examples/eval/src/eval.witchy"));
    run_witchy("witchy bank (records + lists + Result)", include_str!("../examples/bank/src/bank.witchy"));
    run_witchy("witchy higher-order functions (closures)", include_str!("../examples/higher_order/src/higher_order.witchy"));
    run_witchy("witchy list combinators (map/filter via push)", include_str!("../examples/list_ops/src/list_ops.witchy"));
    run_witchy("witchy dictionaries (word count)", include_str!("../examples/wordcount/src/wordcount.witchy"));
    run_witchy("witchy dict iteration (values/pairs)", include_str!("../examples/inventory/src/inventory.witchy"));
    run_witchy("witchy early return (guard clauses)", include_str!("../examples/guard/src/guard.witchy"));
    run_witchy("witchy negative-literal patterns", include_str!("../examples/signs/src/signs.witchy"));
    run_witchy("witchy string slicing (substring/index_of)", include_str!("../examples/parse_kv/src/parse_kv.witchy"));
    run_witchy("witchy filesystem capability", include_str!("../examples/files/src/files.witchy"));
    run_compiled(&mut rt, "witchy compiled to WASM (ints)", include_str!("../examples/compute/src/compute.witchy"));
    run_compiled(&mut rt, "witchy compiled to WASM (ADTs)", include_str!("../examples/shapes/src/shapes.witchy"));
    run_compiled(&mut rt, "witchy compiled to WASM (record field access)", include_str!("../examples/record_compiled/src/record_compiled.witchy"));
    run_compiled(&mut rt, "witchy compiled to WASM (strings)", include_str!("../examples/strings/src/strings.witchy"));
    run_net_demo("witchy network capability");
    run_program_demo(
        "witchy modules (import)",
        &[
            ("strutil", include_str!("../examples/app/src/strutil.witchy")),
            ("app", include_str!("../examples/app/src/app.witchy")),
        ],
        "app",
    );
    run_program_demo(
        "witchy standard library (import list)",
        &[
            ("list", include_str!("../std/list.witchy")),
            ("std_demo", include_str!("../examples/std_demo/src/std_demo.witchy")),
        ],
        "std_demo",
    );
    run_compiled_program(
        &mut rt,
        "witchy list combinators compiled to WASM (map/filter/fold/sort_by)",
        &[
            ("list", include_str!("../std/list.witchy")),
            ("list_pipeline", include_str!("../examples/list_pipeline/src/list_pipeline.witchy")),
        ],
        "list_pipeline",
    );
    run_program_demo(
        "witchy list search/slice (contains/index_of/take/drop)",
        &[
            ("list", include_str!("../std/list.witchy")),
            ("list_more", include_str!("../examples/list_more/src/list_more.witchy")),
        ],
        "list_more",
    );
    run_program_demo(
        "witchy list zip/enumerate",
        &[
            ("list", include_str!("../std/list.witchy")),
            ("string", include_str!("../std/string.witchy")),
            ("zip", include_str!("../examples/zip/src/zip.witchy")),
        ],
        "zip",
    );
    run_program_demo(
        "witchy list any/all (predicates)",
        &[
            ("list", include_str!("../std/list.witchy")),
            ("predicates", include_str!("../examples/predicates/src/predicates.witchy")),
        ],
        "predicates",
    );
    run_program_demo(
        "witchy text processing (split/map/join)",
        &[
            ("list", include_str!("../std/list.witchy")),
            ("string", include_str!("../std/string.witchy")),
            ("text", include_str!("../examples/text/src/text.witchy")),
        ],
        "text",
    );
    run_program_demo(
        "witchy sorting (sort_by with a comparator)",
        &[
            ("list", include_str!("../std/list.witchy")),
            ("string", include_str!("../std/string.witchy")),
            ("sort", include_str!("../examples/sort/src/sort.witchy")),
        ],
        "sort",
    );
    run_program_demo(
        "witchy standard library (import math)",
        &[
            ("math", include_str!("../std/math.witchy")),
            ("math_demo", include_str!("../examples/math_demo/src/math_demo.witchy")),
        ],
        "math_demo",
    );
    run_program_demo(
        "witchy float math (sqrt + float_abs/float_min/float_max)",
        &[
            ("math", include_str!("../std/math.witchy")),
            ("floats", include_str!("../examples/floats/src/floats.witchy")),
        ],
        "floats",
    );
    run_program_demo(
        "witchy standard Result (import result + ?)",
        &[
            ("result", include_str!("../std/result.witchy")),
            (
                "rclient",
                r#"
import result

fn checked_div(a: Int, b: Int) -> Result(Int, String):
    match b:
        0 -> Err("divide by zero")
        _ -> Ok((a / b))

fn compute(x: Int, y: Int) -> Result(Int, String):
    let q = (checked_div(x, y))?
    Ok((q + 1))

fn main(console: Console):
    print(console, __render(result.unwrap_or(compute(10, 2), (0 - 1))))
    print(console, __render(result.unwrap_or(compute(10, 0), (0 - 1))))
"#,
            ),
        ],
        "rclient",
    );
    run_program_demo(
        "witchy standard Option (import option)",
        &[
            ("option", include_str!("../std/option.witchy")),
            ("option_std", include_str!("../examples/option_std/src/option_std.witchy")),
        ],
        "option_std",
    );

    println!("\nspike OK");
    Ok(())
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

    let linked = linker::link(modules, &entry_stem).map_err(|e| e.to_string())?;
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

/// Parse a `--secret name=value` spec into a named secret. The value is taken
/// literally (UTF-8 bytes) — a token, password, or connection string. The name
/// must be non-empty and contain no `=` (everything after the first `=` is the
/// value, so values may contain `=`).
fn parse_secret_inline(spec: &str) -> Result<(String, Vec<u8>), String> {
    match spec.split_once('=') {
        Some((name, value)) if !name.is_empty() => Ok((name.to_string(), value.as_bytes().to_vec())),
        _ => Err(format!("`--secret` expects `name=value`, got `{spec}`")),
    }
}

/// Parse a `--secret-file name=path` spec, reading the secret's bytes from the
/// file. Whitespace is NOT trimmed (a secret file holds exactly its bytes).
fn parse_secret_file(spec: &str) -> Result<(String, Vec<u8>), String> {
    match spec.split_once('=') {
        Some((name, path)) if !name.is_empty() => {
            let bytes = std::fs::read(path).map_err(|e| format!("`--secret-file {name}`: cannot read `{path}`: {e}"))?;
            Ok((name.to_string(), bytes))
        }
        _ => Err(format!("`--secret-file` expects `name=path`, got `{spec}`")),
    }
}

// Convenience wrapper (no command-line args) — used by the test suite; the CLI
// run path calls `execute_file_exit` to also get the process exit code.
#[cfg_attr(not(test), allow(dead_code))]
fn execute_file(path: &str, net_allow: Vec<String>) -> Result<Vec<String>, String> {
    execute_file_args(path, net_allow, Vec::new(), None)
}

/// Like [`execute_file`] but with command-line `args` and an optional signing
/// key, discarding the process exit code (used by the test suite).
#[cfg_attr(not(test), allow(dead_code))]
fn execute_file_args(
    path: &str,
    net_allow: Vec<String>,
    args: Vec<String>,
    signing_key: Option<[u8; 32]>,
) -> Result<Vec<String>, String> {
    execute_file_exit(path, net_allow, args, signing_key, Vec::new()).map(|(output, _)| output)
}

/// Link, type-check, and run `path`, returning its output and the process exit
/// code (`main`'s `Int` return, else 0). `args` populate a `List(String)`
/// parameter; `signing_key` grants the root `Secret` capability.
fn execute_file_exit(
    path: &str,
    net_allow: Vec<String>,
    args: Vec<String>,
    signing_key: Option<[u8; 32]>,
    named_secrets: Vec<(String, Vec<u8>)>,
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
    run_linked_compiled(&linked, Vec::new(), net_allow, args, signing_key, named_secrets)
        .map(|(lines, code)| (lines, code.unwrap_or(0)))
}

/// Run a program on BOTH backends — the tree-walking interpreter and compiled
/// WebAssembly — and confirm they produce identical output. Witchy's
/// dual-backend equivalence is normally an internal test invariant; `witchy
/// verify` surfaces it as a guarantee you can check on your own code.
/// A failed in-language test: its (qualified) name and the abort message.
type TestFailure = (String, String);

/// Discover and run the tests in a source file: every ZERO-parameter function
/// named `test_*`, each invoked through a synthesized `main` in a fresh
/// interpreter. A test passes by returning and fails by aborting (which
/// `std/testing`'s assertions do, with a message). Tests take no capabilities,
/// so a suite provably has no effects. Returns the failures as (name, message).
fn run_tests_in_file(path: &str) -> Result<(Vec<String>, Vec<TestFailure>), String> {
    use std::path::Path;
    let (linked, _stem) = link_file(path)?;
    typeck::check(&linked).map_err(|e| e.to_string())?;
    // Post-link names are module-qualified (`suite.test_x`); match on the bare
    // name, call the qualified one.
    let tests: Vec<String> = linked
        .items
        .iter()
        .filter_map(|it| match it {
            ast::Item::Function(f)
                if f.name.rsplit('.').next().unwrap_or(&f.name).starts_with("test_")
                    && f.params.is_empty() =>
            {
                Some(f.name.clone())
            }
            _ => None,
        })
        .collect();
    let root = Path::new(path).parent().unwrap_or(Path::new("."));
    let mut passed = Vec::new();
    let mut failed = Vec::new();
    for test in tests {
        // Synthesize `fn main(): <test>()` (replacing any real main) and run.
        // The test name is already linker-qualified (`suite.test_x`), which the
        // parser would read as a method call — so parse a placeholder and patch
        // the call name in the AST.
        let mut m = linked.clone();
        m.items
            .retain(|it| !matches!(it, ast::Item::Function(f) if f.name == "main"));
        let mut driver = parser::parse_module("fn main():\n    witchy_test_target()\n")
            .map_err(|e| e.to_string())?;
        for it in &mut driver.items {
            if let ast::Item::Function(f) = it {
                if let Some(ast::Stmt::Expr(ast::Expr::Call { name, .. })) = f.body.stmts.first_mut()
                {
                    *name = test.clone();
                }
            }
        }
        m.items.extend(driver.items);
        match interpreter::run_module(m, root, Vec::new()) {
            Ok(_) => passed.push(test),
            Err(e) => failed.push((test, e.message)),
        }
    }
    Ok((passed, failed))
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
        let (passed, failed) = match run_tests_in_file(file) {
            Ok(r) => r,
            // In a directory sweep, a file that can't link standalone — e.g. a
            // module of a multi-rune project that imports a sibling rune via a
            // path dependency — is skipped, not fatal. An explicit single file
            // still surfaces the error.
            Err(e) if meta.is_dir() => {
                eprintln!("  skipped {file}: {e}");
                continue;
            }
            Err(e) => return Err(e),
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

fn verify_file(path: &str) -> Result<(), String> {
    use std::path::Path;
    let (linked, _stem) = link_file(path)?;
    typeck::check(&linked).map_err(|e| e.to_string())?;
    let has_main = linked
        .items
        .iter()
        .any(|it| matches!(it, ast::Item::Function(f) if f.name == "main"));
    if !has_main {
        return Err(format!("`{path}` has no `main` to run"));
    }
    // Compile first (borrows `linked`), then run the interpreter (consumes it).
    let bytes = codegen::compile_module_binary(&linked)
        .map_err(|e| format!("cannot compile to WASM (an interpreter-only feature?): {e}"))?
        .ok_or_else(|| {
            "cannot compile to WASM: the program reached a construct the compiled backend \
             does not support (an interpreter-only feature?)"
                .to_string()
        })?;
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
    match (interp, compiled) {
        (Ok(i), Ok(c)) if i == c => {
            println!(
                "\u{2713} {path}: interpreter and compiled WASM agree ({} line(s) of output)",
                i.len()
            );
            Ok(())
        }
        (Ok(i), Ok(c)) => Err(format!(
            "\u{2717} {path}: the two backends DIVERGE\n  interpreter: {i:?}\n  compiled:    {c:?}"
        )),
        // Both fail: they agree on rejecting this input (the messages differ — a
        // readable interpreter error vs. a WASM trap — but the behavior matches).
        (Err(_), Err(_)) => {
            println!("\u{2713} {path}: interpreter and compiled WASM agree (both error)");
            Ok(())
        }
        (Ok(i), Err(c)) => Err(format!(
            "\u{2717} {path}: the two backends DIVERGE\n  interpreter: Ok({i:?})\n  compiled:    Err({c})"
        )),
        (Err(i), Ok(c)) => Err(format!(
            "\u{2717} {path}: the two backends DIVERGE\n  interpreter: Err({i})\n  compiled:    Ok({c:?})"
        )),
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
    let (linked, _stem) = link_file(path)?;
    typeck::check(&linked).map_err(|e| e.to_string())?;
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
/// (strict, announced grant) — one runtime for both, so dev == deploy.
fn run_linked_compiled(
    linked: &ast::Module,
    dir_roots: Vec<std::path::PathBuf>,
    net_allow: Vec<String>,
    args: Vec<String>,
    signing_key: Option<[u8; 32]>,
    named_secrets: Vec<(String, Vec<u8>)>,
) -> Result<(Vec<String>, Option<i32>), String> {
    use crate::runtime::{Capabilities, Runtime};
    let footprint = capabilities::analyze(linked);
    // A Secret in the footprint is the whole-program union (e.g. coven CAN sign
    // when publishing) — it does NOT mean THIS run signs. Mirror the interpreter,
    // which requires a key only when `main` actually binds a `Secret` parameter;
    // an unreached signing path needs none.
    let main_binds_secret = linked.items.iter().any(|it| {
        matches!(it, ast::Item::Function(f) if f.name == "main"
            && f.params.iter().any(|p| matches!(&p.ty,
                Some(ast::Type::Named(n, _)) if n == "Secret" || n == "SecretStore")))
    });
    if main_binds_secret && signing_key.is_none() && named_secrets.is_empty() {
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
        ..Default::default()
    };
    if footprint.total.contains_key("Clock") {
        caps.clock = true;
    }
    if footprint.total.contains_key("Env") {
        caps.env = true;
    }
    if footprint.total.contains_key("Exec") {
        caps.exec = true;
    }
    if let Some(rights) = footprint.total.get("Dir") {
        let mut roots = dir_roots;
        if roots.is_empty() {
            roots.push(std::path::PathBuf::from("."));
        }
        caps.dir_root = Some(roots.remove(0));
        caps.dir_roots = roots;
        caps.dir_read = rights.contains("Read");
        caps.dir_write = rights.contains("Write");
    }
    if let Some(rights) = footprint.total.get("Net") {
        caps.net_allow = Some(net_allow);
        caps.net_connect = rights.contains("Connect");
        caps.net_listen = rights.contains("Listen");
    }
    if footprint.total.contains_key("Secret") || footprint.total.contains_key("SecretStore") {
        caps.signing_key = signing_key;
        // The signing key is the `signing` secret at handle 0, so a `Secret`
        // capability (always handle 0) and `SecretStore.get("signing")` agree.
        // The `--secret`/`--secret-file` grants follow, each a named `Secret`
        // reachable by `SecretStore.get(name)` / `.require(name)`.
        if let Some(seed) = signing_key {
            caps.secrets.push(("signing".to_string(), seed.to_vec()));
        }
        caps.secrets.extend(named_secrets);
    }
    let wasm = compile_linked_to_wasm(linked)?;
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

/// Compile a program and run it in the WASM VM granted EXACTLY its computed
/// footprint, announcing the grant on stderr. The `Dir` root and `Net` allowlist
/// are host policy (the `--dir`/`--net` flags); the program's footprint decides
/// whether they are granted at all.
fn run_file_sandboxed(
    path: &str,
    dir_roots: Vec<std::path::PathBuf>,
    net_allow: Vec<String>,
    args: Vec<String>,
    signing_key: Option<[u8; 32]>,
    named_secrets: Vec<(String, Vec<u8>)>,
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
    let footprint = capabilities::analyze(&linked);
    if footprint.total.contains_key("Secret") && signing_key.is_none() && named_secrets.is_empty() {
        return Err(format!(
            "`{path}` needs a Secret, but the host granted none (provide `--signing-key <seed-file>`, `--secret name=value`, or `--secret-file name=path`)"
        ));
    }
    eprintln!(
        "sandboxing `{path}` \u{2014} granted exactly: {}",
        capabilities::show_caps(&footprint.total)
    );
    run_linked_compiled(&linked, dir_roots, net_allow, args, signing_key, named_secrets)
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
fn run_wasm_file(
    path: &str,
    dir_roots: Vec<std::path::PathBuf>,
    net_allow: Vec<String>,
    args: Vec<String>,
    signing_key: Option<[u8; 32]>,
    named_secrets: Vec<(String, Vec<u8>)>,
) -> Result<(Vec<String>, Option<i32>), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read `{path}`: {e}"))?;
    run_wasm_module(&bytes, dir_roots, net_allow, args, signing_key, named_secrets)
}

/// Run a precompiled wasm program from in-memory bytes under the capability
/// sandbox — the byte-level core of [`run_wasm_file`].
fn run_wasm_module(
    bytes: &[u8],
    dir_roots: Vec<std::path::PathBuf>,
    net_allow: Vec<String>,
    args: Vec<String>,
    signing_key: Option<[u8; 32]>,
    named_secrets: Vec<(String, Vec<u8>)>,
) -> Result<(Vec<String>, Option<i32>), String> {
    use crate::runtime::{Capabilities, Runtime};
    let needs = witchy_imports(bytes)?;
    let has = |n: &str| needs.iter().any(|i| i == n);
    let dir_read = ["dir_subdir", "dir_read_len", "dir_exists", "dir_is_dir", "dir_list_size"]
        .iter()
        .any(|n| has(n));
    let dir_write = ["dir_write", "dir_append", "dir_make_dir"].iter().any(|n| has(n));
    let net_connect = [
        "net_connect", "net_try_connect", "net_restrict", "net_send_line", "net_send_bytes",
        "net_recv_line_len", "net_recv_all_len", "net_recv_bytes_len", "net_close",
    ]
    .iter()
    .any(|n| has(n));
    let net_listen = ["net_listen", "net_accept"].iter().any(|n| has(n));
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
    if has("env_len") || has("env_fill") {
        caps.env = true;
    }
    if has("exec_run") {
        caps.exec = true;
    }
    if dir_read || dir_write {
        let mut roots = dir_roots;
        if roots.is_empty() {
            roots.push(std::path::PathBuf::from("."));
        }
        caps.dir_root = Some(roots.remove(0));
        caps.dir_roots = roots;
        caps.dir_read = dir_read;
        caps.dir_write = dir_write;
    }
    if net_connect || net_listen {
        caps.net_allow = Some(net_allow);
        caps.net_connect = net_connect;
        caps.net_listen = net_listen;
    }
    if needs_secret {
        caps.signing_key = signing_key;
        if let Some(seed) = signing_key {
            caps.secrets.push(("signing".to_string(), seed.to_vec()));
        }
        caps.secrets.extend(named_secrets);
    }
    let mut rt = Runtime::batch().map_err(|e| e.to_string())?;
    let mut vm = rt
        .spawn(bytes, caps, RUN_MEMORY_PAGES)
        .map_err(|e| e.to_string())?;
    vm.run().map_err(|e| e.root_cause().to_string())?;
    // We can't see `main`'s return type from a bare binary, so an Int `main`'s
    // value surfaces as a trailing output line rather than the process exit code
    // (the source runners pop it because they have the AST). Acceptable for Tier 1.
    Ok((vm.output(), None))
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
    vm.run().map_err(|e| e.to_string())?;
    Ok(vm.output())
}

/// Read, parse, and compute the host-capability footprint of a source file.
fn analyze_file(path: &str) -> Result<capabilities::Footprint, String> {
    let src = std::fs::read_to_string(path).map_err(|e| format!("cannot read `{path}`: {e}"))?;
    let module = parser::parse_module(&src).map_err(|e| e.to_string())?;
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
    // host process/socket/env I/O is confined by the grant allow-list, which the
    // WASM boundary cannot itself enforce.
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
    let mut flagged = false;
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

/// Parse, link, and run a multi-module program through the interpreter.
fn run_program_demo(title: &str, sources: &[(&str, &str)], entry: &str) {
    println!("\n== {title} ==");
    match interpreter::run_program(sources, entry) {
        Ok(out) => {
            for line in out {
                println!("{line}");
            }
        }
        Err(e) => println!("error: {e}"),
    }
}

/// Demonstrate the Net capability against a loopback echo server: a granted
/// address can be reached; an address outside the allow-list is denied.
fn run_net_demo(title: &str) {
    use std::io::{BufRead, BufReader, Write};
    println!("\n== {title} ==");
    let listener = match std::net::TcpListener::bind("127.0.0.1:0") {
        Ok(l) => l,
        Err(e) => {
            println!("could not bind loopback: {e}");
            return;
        }
    };
    let addr = listener.local_addr().unwrap().to_string();
    let server = std::thread::spawn(move || {
        if let Ok((stream, _)) = listener.accept() {
            let mut r = BufReader::new(stream);
            let mut line = String::new();
            let _ = r.read_line(&mut line);
            let _ = r.get_mut().write_all(format!("echo: {line}").as_bytes());
        }
    });

    let program = format!(
        r#"
        fn main(console: Console, net: Net) {{
          let s = connect(net, "{addr}")
          send_line(s, "hello over the wire")
          print(console, recv_line(s))
        }}
    "#
    );
    match interpreter::run_with(&program, ".", vec![addr.clone()]) {
        Ok(out) => {
            for line in out {
                println!("{line}");
            }
        }
        Err(e) => println!("error: {e}"),
    }
    server.join().ok();

    let denied = r#"
fn main(console: Console, net: Net):
    let s = connect(net, "10.255.255.1:80")
    send_line(s, "x")
"#;
    if let Err(e) = interpreter::run_with(denied, ".", vec![addr]) {
        println!("DENIED outside the allow-list (as designed): {e}");
    }
}


/// End-to-end coverage: every shipped example must type-check and produce the
/// expected result (interpreted), or type-check and compile to valid WASM.
#[cfg(test)]
mod example_tests;

/// Compile a witchy program to WASM and run it on the runtime, demonstrating
/// that the capability gate now applies to *compiled* witchy: granted, it runs;
/// ungranted, the module cannot instantiate.
fn run_compiled(rt: &mut Runtime, title: &str, program: &str) {
    println!("\n== {title} ==");
    if let Err(e) = typeck::check_str(program) {
        println!("{e}");
        return;
    }
    let module = match parser::parse_module(program) {
        Ok(m) => m,
        Err(e) => {
            println!("{e}");
            return;
        }
    };
    let bytes = match codegen::compile_module_binary(&module) {
        Ok(Some(b)) => b,
        Ok(None) => {
            println!("cannot compile to WASM (an interpreter-only feature?)");
            return;
        }
        Err(e) => {
            println!("{e}");
            return;
        }
    };

    // Granted the output capabilities: the compiled module runs and prints.
    match rt.spawn(
        &bytes,
        Capabilities {
            print: true,
            print_int: true,
            ..Default::default()
        },
        4,
    ) {
        Ok(mut vm) => {
            if let Err(e) = vm.run() {
                println!("error: {e}");
            }
        }
        Err(e) => println!("spawn failed: {e}"),
    }

    // Denied: the same compiled module cannot even instantiate.
    match rt.spawn(&bytes, Capabilities::none(), 4) {
        Ok(_) => println!("!! SECURITY FAILURE: compiled module ran without the capability"),
        Err(e) => println!("DENIED without capability (as designed): {e}"),
    }
}

/// Link a multi-module program, compile the flat module to WASM, and run it on
/// its own VM — so an imported library (e.g. `list`) is genuinely compiled, not
/// just the entry module.
fn run_compiled_program(rt: &mut Runtime, title: &str, sources: &[(&str, &str)], entry: &str) {
    println!("\n== {title} ==");
    let mods: Result<Vec<(String, ast::Module)>, String> = sources
        .iter()
        .map(|(n, s)| {
            parser::parse_module(s)
                .map(|m| ((*n).to_string(), m))
                .map_err(|e| e.to_string())
        })
        .collect();
    let linked = match mods.and_then(|m| linker::link(m, entry).map_err(|e| e.to_string())) {
        Ok(m) => m,
        Err(e) => {
            println!("{e}");
            return;
        }
    };
    if let Err(e) = typeck::check(&linked) {
        println!("{e}");
        return;
    }
    let bytes = match codegen::compile_module_binary(&linked) {
        Ok(Some(b)) => b,
        Ok(None) => {
            println!("cannot compile to WASM (an interpreter-only feature?)");
            return;
        }
        Err(e) => {
            println!("{e}");
            return;
        }
    };
    match rt.spawn(
        &bytes,
        Capabilities {
            print: true,
            print_int: true,
            ..Default::default()
        },
        4,
    ) {
        Ok(mut vm) => {
            if let Err(e) = vm.run() {
                println!("error: {e}");
            }
        }
        Err(e) => println!("spawn failed: {e}"),
    }
}

fn run_witchy(title: &str, program: &str) {
    println!("\n== {title} ==");
    if let Err(e) = typeck::check_str(program) {
        println!("{e}");
        return;
    }
    match interpreter::run(program) {
        Ok(output) => {
            for line in output {
                println!("{line}");
            }
        }
        Err(e) => println!("error: {e}"),
    }
}

