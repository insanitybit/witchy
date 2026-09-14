//! Embedded package-manager (grimoire) launcher: the bundled grimoire rune is
//! compiled and run in-process for `witchy grimoire ...` (and its `witchy pm`
//! compat alias), and its module sources are served to the frontend/doc/expand
//! commands. Extracted from the composition root.

use crate::{ast, bundled_module, commands, parser, pipeline};

// Project workflows have their own source entries, not AST-selected declarations
// or copied witnesses. Each CLI route calls its existing Grimoire handler;
// unrelated registry bodies do not become part of that entry's admission scope.
const PROJECT_RUN_ENTRY: &str = r#"from grimoire import cmd_run
fn main(console: Console, args: List(String), root: Dir, bin: Dir, runner: Exec) -> Int:
    cmd_run(console, runner, root, bin, args)
"#;

const PROJECT_BUILD_ENTRY: &str = r#"from grimoire import cmd_build
fn main(console: Console, args: List(String), root: Dir, bin: Dir, runner: Exec, env: Env) -> Int:
    cmd_build(console, runner, root, bin, env, args)
"#;

const PROJECT_LOCK_ENTRY: &str = r#"from grimoire import cmd_lock
fn main(console: Console, args: List(String), root: Dir) -> Int:
    if args.length() > 1:
        cmd_lock(console, root, args[1])
    else:
        cmd_lock(console, root, "")
"#;

// Keep the unused project Dir before bin: its position preserves the existing
// grant ordinal (cwd = 0, compiler directory = 1). Argument selection remains
// identical to grimoire.main; pm test does not forward extra grant flags.
const PROJECT_TEST_ENTRY: &str = r#"from grimoire import cmd_drive
from gm_core import arg
fn main(console: Console, args: List(String), root: Dir, bin: Dir, runner: Exec) -> Int:
    cmd_drive(console, runner, bin, "test", ["test", arg(args, 1)])
"#;

const PROJECT_DEPS_ENTRY: &str = r#"from grimoire import cmd_deps
from gm_core import arg
fn main(console: Console, args: List(String), root: Dir) -> Int:
    cmd_deps(console, root, arg(args, 1))
    0
"#;

const PROJECT_TREE_ENTRY: &str = r#"from grimoire import cmd_tree
from gm_core import arg
fn main(console: Console, args: List(String), root: Dir) -> Int:
    cmd_tree(console, root, arg(args, 1))
    0
"#;

const PROJECT_ADD_ENTRY: &str = r#"from grimoire import cmd_add
fn main(console: Console, args: List(String), root: Dir, net: Net, env: Env, clock: Clock) -> Int:
    cmd_add(console, net, root, env, clock, args)
"#;

const PROJECT_VERIFY_RUNE_ENTRY: &str = r#"from grimoire import cmd_verify_rune
from gm_core import arg
fn main(console: Console, args: List(String), root: Dir) -> Int:
    cmd_verify_rune(console, root, arg(args, 1), arg(args, 2))
"#;

fn embedded_pm_entry(verb: Option<&str>) -> (&'static str, &'static str) {
    match verb {
        Some("run") => ("__witchy_project_run", PROJECT_RUN_ENTRY),
        Some("build") => ("__witchy_project_build", PROJECT_BUILD_ENTRY),
        Some("lock") => ("__witchy_project_lock", PROJECT_LOCK_ENTRY),
        Some("test") => ("__witchy_project_test", PROJECT_TEST_ENTRY),
        Some("deps") => ("__witchy_project_deps", PROJECT_DEPS_ENTRY),
        Some("tree") => ("__witchy_project_tree", PROJECT_TREE_ENTRY),
        Some("add") => ("__witchy_project_add", PROJECT_ADD_ENTRY),
        Some("verify-rune") => ("__witchy_project_verify_rune", PROJECT_VERIFY_RUNE_ENTRY),
        _ => ("grimoire", include_str!("../../projects/grimoire/src/grimoire.witchy")),
    }
}

/// Run the EMBEDDED witchy package-manager front-end (`projects/grimoire/src/grimoire.witchy`)
/// — the cargo-equivalent CLI, itself written in witchy and bundled into the
/// toolchain like std (rfcs/0004-self-hosted-cli.md). `raw` is the front-end's
/// argv (everything after the `witchy` subcommand): `--net <host:port>` flags are
/// extracted into the program's `Net` allowlist, the rest become `main`'s `args`.
/// It runs capability-confined: Console, the project `Dir` (cwd, grant ordinal
/// 0), a `Dir` to the toolchain bin (grant ordinal 1, so it can drive the
/// compiler via `Exec`), `Net`, `Env`, and its argv. This is the sole entry for
/// the front-end's client verbs — both `witchy pm <verb>` and the top-level
/// `witchy <verb>` route here.
pub(crate) fn run_embedded_pm(raw: Vec<String>) -> ! {
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
    // (RFC-0095) Host-target auto-detection: `witchy install <pkg>` with no explicit
    // `--target` defaults to THIS build's host triple (from cargo's TARGET), so the
    // common case needs no flag. An explicit `--target` anywhere still wins.
    if pm_args.first().map(String::as_str) == Some("install")
        && !pm_args.iter().any(|a| a == "--target")
    {
        pm_args.push("--target".to_string());
        pm_args.push(env!("WITCHY_HOST_TARGET").to_string());
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
    if let Some(grant) = std::env::var("COVEN_URL").ok().as_deref().and_then(coven_url_net_grant) {
        net_allow.push(grant);
    }
    // Keep the checked source until the admitted launcher compiles it. A byte
    // projection loses the sealed owner bridges needed by captured values.
    let checked = (|| {
        let mut modules: Vec<(String, Vec<u8>)> = Vec::new();
        let mut loaded: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<(String, String)> = VecDeque::new();
        let (entry, source) = embedded_pm_entry(pm_args.first().map(String::as_str));
        queue.push_back((entry.to_string(), source.to_string()));
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
            modules.push((name, source.into_bytes()));
        }
        // These exact bytes come only from the compiler's embedded sources.
        // Parsing to discover imports must not discard canonical source proof.
        witchy_types::pipeline::link_checked_from_source_bundle(
            modules, entry, witchy_interp::comptime::expand_compile_time,
        ).map_err(|e| e.to_string())
    })();
    let checked = match checked {
        Ok(checked) => checked,
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
    // The checked launcher preserves the Dir grant order (ordinal 0 =
    // cwd, ordinal 1 = bin so `Exec` finds the compiler) and surfaces `main`'s
    // `Int` exit code.
    // (RFC-0095 Cut 4) The witchy-home provider: a third `Dir` granted for
    // $WITCHY_HOME (ordinal 2), so `install` can write trusted-publishers into its `bin/`
    // without reaching outside the project `Dir`. Resolve it (WITCHY_HOME, else
    // ~/.witchy) and create it so the grant is valid; other commands ignore it.
    let home = witchy_home_dir();
    let _ = std::fs::create_dir_all(&home);
    match run_embedded_program(&checked, vec![cwd, bin, home], net_allow, pm_args) {
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

/// Use the ordinary source-run grant policy and retain admission authority all
/// the way to `Runtime::spawn_admitted`. External Wasm keeps its separate path.
fn run_embedded_program(
    checked: &pipeline::CheckedModule,
    dir_roots: Vec<std::path::PathBuf>,
    net_allow: Vec<String>,
    args: Vec<String>,
) -> Result<(Vec<String>, Option<i32>), String> {
    commands::execution::run_checked_compiled(
        checked, dir_roots, Vec::new(), net_allow, Vec::new(),
        None, None, Vec::new(), args, None, Vec::new(), Vec::new(),
        false, witchy_confinement::EnforcementMode::Disabled,
    )
}

/// (RFC-0095 Cut 4) The $WITCHY_HOME directory the `witchy-home` provider grants:
/// `WITCHY_HOME` if set (so tests and packagers can redirect it), else `~/.witchy`,
/// else `.witchy` under the cwd as a last resort. The caller creates it before
/// granting so the `Dir` capability is valid.
fn witchy_home_dir() -> std::path::PathBuf {
    if let Some(h) = std::env::var_os("WITCHY_HOME") {
        return std::path::PathBuf::from(h);
    }
    if let Some(h) = std::env::var_os("HOME") {
        return std::path::PathBuf::from(h).join(".witchy");
    }
    std::path::PathBuf::from(".witchy")
}

/// The Net allow-list entry backing a `COVEN_URL` registry address. The scheme is
/// stripped for the grant (a Net grant is `host[:port]`), but it still decides the
/// default port when the URL carries none — `https://coven.example` must grant
/// `coven.example:443` (and `http://…` `:80`), never a portless entry. An explicit
/// port is kept as written; a bare `host:port` passes through unchanged.
fn coven_url_net_grant(u: &str) -> Option<String> {
    let (default_port, rest) = if let Some(rest) = u.strip_prefix("https://") {
        (Some(443u16), rest)
    } else if let Some(rest) = u.strip_prefix("http://") {
        (Some(80u16), rest)
    } else {
        (None, u)
    };
    let hp = rest.trim_end_matches('/');
    if hp.is_empty() {
        return None;
    }
    match default_port {
        Some(p) if !hp.contains(':') => Some(format!("{hp}:{p}")),
        _ => Some(hp.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::coven_url_net_grant;

    #[test]
    fn offline_pm_entries_leave_online_and_unknown_dispatch_unchanged() {
        for (verb, expected) in [
            ("test", ("__witchy_project_test", super::PROJECT_TEST_ENTRY)),
            ("deps", ("__witchy_project_deps", super::PROJECT_DEPS_ENTRY)),
            ("tree", ("__witchy_project_tree", super::PROJECT_TREE_ENTRY)),
        ] {
            assert_eq!(super::embedded_pm_entry(Some(verb)), expected);
        }
        let original = ("grimoire", include_str!("../../projects/grimoire/src/grimoire.witchy"));
        for verb in ["verify", "add", "install", "publish", "promote", "yank",
            "list", "outdated", "update", "help", "not-a-verb", "test --integration"]
        {
            assert_eq!(super::embedded_pm_entry(Some(verb)), original, "{verb}");
        }
        assert_eq!(super::embedded_pm_entry(None), original);
    }

    #[test]
    fn internal_admitted_launch_preserves_embedded_owner_capture_and_args() {
        let source = r#"
fn greeter(prefix: String, console: Console) -> fn(String) -> String:
    fn(name: String):
        console.print(prefix)
        prefix + name

fn main(console: Console, args: List(String)) -> Int:
    let greet = greeter(args[0], console)
    console.print(greet("one"))
    console.print(greet("two"))
    7
"#;
        let checked = witchy::resolve_std_only_checked(source).expect("checked embedded fixture");
        let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
            .expect_lowered("admitted embedded owner capture");
        assert!(!artifact.owner_bridges().is_empty());
        let args = vec!["launch:".to_owned()];
        let expected = ["launch:", "launch:one", "launch:", "launch:two"]
            .map(str::to_owned).to_vec();
        let (oracle_output, oracle_exit) = witchy::interpreter::run_module_exit(
            checked.module().clone(), ".", Vec::new(), args.clone(), None,
        ).expect("independent source execution with launch arguments");
        assert_eq!(oracle_output, expected);
        assert_eq!(oracle_exit, 7);
        let (output, exit_code) = super::run_embedded_program(
            &checked, Vec::new(), Vec::new(), args,
        ).expect("embedded launcher retains admitted owner capture");
        assert_eq!(output, expected);
        assert_eq!(exit_code, Some(7));
    }

    #[test]
    fn coven_url_grant_defaults_the_port_from_the_scheme() {
        assert_eq!(coven_url_net_grant("https://coven.example"), Some("coven.example:443".into()));
        assert_eq!(coven_url_net_grant("https://coven.example/"), Some("coven.example:443".into()));
        assert_eq!(coven_url_net_grant("http://coven.example"), Some("coven.example:80".into()));
    }

    #[test]
    fn coven_url_grant_keeps_an_explicit_port() {
        assert_eq!(coven_url_net_grant("https://localhost:8443"), Some("localhost:8443".into()));
        assert_eq!(coven_url_net_grant("http://127.0.0.1:8787/"), Some("127.0.0.1:8787".into()));
    }

    #[test]
    fn coven_url_grant_passes_a_bare_hostport_through() {
        assert_eq!(coven_url_net_grant("127.0.0.1:8787"), Some("127.0.0.1:8787".into()));
        assert_eq!(coven_url_net_grant(""), None);
        assert_eq!(coven_url_net_grant("https://"), None);
    }
}

fn embedded_pm_module(name: &str) -> Option<&'static str> {
    match name {
        "grimoire" => Some(include_str!("../../projects/grimoire/src/grimoire.witchy")),
        // grimoire's own shared leaf-helper module (arg/path/JSON/registry shims).
        "gm_core" => Some(include_str!("../../projects/grimoire/src/gm_core.witchy")),
        // grimoire's effectful package-manager engine (fetch/resolve/vendor/lock/verify).
        "gm_engine" => Some(include_str!("../../projects/grimoire/src/gm_engine.witchy")),
        "coven_proto" => Some(include_str!("../../projects/coven/src/coven_proto.witchy")),
        "coven_json" => Some(include_str!("../../projects/coven/src/coven_json.witchy")),
        "coven_validate" => Some(include_str!("../../projects/coven/src/coven_validate.witchy")),
        // (RFC-0095) `pm install` parses/verifies the artifact manifest.
        "coven_artifact" => Some(include_str!("../../projects/coven/src/coven_artifact.witchy")),
        _ => None,
    }
}
#[cfg(test)]
#[path = "project_resolver_tests.rs"]
mod project_resolver_tests;
