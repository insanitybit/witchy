//! Embedded package-manager (pm) launcher: the bundled pm rune is compiled
//! and run in-process for `witchy pm ...`, and its module sources are served to
//! the frontend/doc/expand commands. Extracted from the composition root.

use crate::{ast, bundled_module, commands, parser, pipeline};

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
    // The embedded front-end's wasm: whole-pipeline cached (parse+link+check+
    // codegen all skipped on a warm hit — the sources are include_str! constants,
    // so the binary fingerprint keys them exactly; see embedded_wasm_cached).
    let wasm = commands::compile::embedded_wasm_cached("pm", || {
        let mut modules: Vec<(String, ast::Module)> = Vec::new();
        let mut loaded: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<(String, String)> = VecDeque::new();
        queue.push_back(("pm".to_string(), include_str!("../../projects/pm/src/pm.witchy").to_string()));
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
    // (RFC-0095 Cut 4) The witchy-home provider: a third `Dir` granted for
    // $WITCHY_HOME (ordinal 2), so `install` can write trusted-exes into its `bin/`
    // without reaching outside the project `Dir`. Resolve it (WITCHY_HOME, else
    // ~/.witchy) and create it so the grant is valid; other commands ignore it.
    let home = witchy_home_dir();
    let _ = std::fs::create_dir_all(&home);
    match commands::wasm_exec::run_wasm_module(&wasm, vec![cwd, bin, home], Vec::new(), net_allow, Vec::new(), pm_args, None, Vec::new(), false, witchy_confinement::EnforcementMode::Disabled) {
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
        "coven_proto" => Some(include_str!("../../projects/coven/src/coven_proto.witchy")),
        "coven_json" => Some(include_str!("../../projects/coven/src/coven_json.witchy")),
        "coven_validate" => Some(include_str!("../../projects/coven/src/coven_validate.witchy")),
        // (RFC-0095) `pm install` parses/verifies the artifact manifest.
        "coven_artifact" => Some(include_str!("../../projects/coven/src/coven_artifact.witchy")),
        _ => None,
    }
}
