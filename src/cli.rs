//! Command-line presentation and argument decoding.

use witchy_runtime::runtime;

pub(crate) fn compiler_version(version: &str, build_commit: Option<&str>) -> String {
    match build_commit.filter(|commit| !commit.is_empty()) {
        Some(commit) => format!("witchy {version} (commit {commit})"),
        None => format!("witchy {version}"),
    }
}

/// One-screen overview of the command-line interface, shown for bare `witchy`.
pub(crate) fn print_usage() {
    println!(
        "\
witchy — a capability-secure language with twin interpreter and WASM backends

USAGE:
    witchy [--net <host:port>]... [--fetch <origin>]... <file.witchy>
                                                  run a program
    witchy check    <file.witchy>                 check + verify compiled acceptance without running
    witchy parity   <file.witchy>                 run on both backends, confirm identical output
                                                  (a verify-the-compiler tool, not a workflow step)
    witchy test [--fixtures <plan.json>] [--backend interpreter|wasm|both] [--filter <text>] [--list] [--show-output] [--seed <u64>] [--format human|json] [--integration] [--dir <root>]... [--net <addr>]... <file.witchy|dir>
                                                  run zero-authority fixture tests on one or both backends, or explicit integration tests
    witchy new --web <directory>                  create a capability-safe Glamour project
    witchy test --web [directory]                 validate a Glamour project and optimized browser artifact
    witchy build --web [--out <directory>] [directory]
                                                  create deterministic production web artifacts
    witchy doctor --web [--format human|json] [--deployment <url>] [directory]
                                                  audit web project, target, ABI, and reproducibility
    witchy dev [--host 127.0.0.1] [--port 3000] [directory]
                                                  run the integrated Glamour development loop
    witchy sandbox [--confine <best-effort|required>] [--dir <root>] [--net <addr>]... [--fetch <origin>]... <file.witchy> [args...]
                                                  compile and run in a VM granted exactly its footprint
    witchy emit-wat <file.witchy>                 print the compiled WebAssembly text (the module sandbox runs)
    witchy expand  <file.witchy>                  print canonical source after comptime/tag expansion
    witchy caps     [--csp] [file.witchy]         report the capability footprint (defaults to the project entry)
    witchy caps-diff <old.witchy> <new.witchy>    fail if the footprint widened
    witchy which    <name>                        find a function in the standard library by (partial) name
    witchy doc     <file.witchy>...                generate stdlib documentation from doc-comments
    witchy compile <entry.witchy> [--dep name=path]... [--package-owner source package version module] [--dep-owner alias source package version module]... [--out <file.wasm>]
                                                  compile to a standalone .wasm module
    witchy --release build --target trusted-exe [--out <file>]
                                                  build a trusted self-contained native application
    witchy build-step <file.witchy> [--out <dir>] [--read <dir>]...
                                                  run a build-time entrypoint with declared grants
    witchy grants-diff <old.grants.toml> <new.grants.toml>
                                                  fail if a secret's reveal policy loosened
    witchy grants-check <prog.witchy> <grants.toml>
                                                  verify a program's footprint fits declared grants
    witchy fmt [--check] [--cap-methods] <file.witchy>
                                                   reformat in place (--check: verify only, --cap-methods: RFC-0076 migration)
    witchy lsp                                    run the language server

Package commands: new, init, add, build, run [args...], update, audit, tree,
outdated, why, why-cap, verify, vendor, publish, promote, yank, list — run
`witchy pm` for the full package-manager help. All of them accept
`-C <dir>`; `witchy run` passes everything after `run` (or after `--`) to the
program as `main`'s `args`, including `--help`."
    );
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
pub(crate) fn leading_opt_mode(args: &[String]) -> Option<&'static str> {
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
pub(crate) fn flag_value(arg: &str, flag: &str, rest: &mut impl Iterator<Item = String>) -> String {
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

pub(crate) fn parse_confinement_mode(
    value: &str,
) -> Result<witchy_confinement::EnforcementMode, String> {
    value.parse().map_err(
        |error: witchy_confinement::ParseEnforcementModeError| error.to_string(),
    )
}

/// Parse a `--secret name=value[,sealed]` spec into a named secret. The value is
/// taken literally (UTF-8 bytes) — a token, password, or connection string. The
/// name must be non-empty and contain no `=` (everything after the first `=`, up
/// to any trailing `,sealed`, is the value, so values may contain `=`). A trailing
/// `,sealed` (RFC-0060/0121) grants the secret as `Secret[Seal]`: usable by opaque
/// handle but never revealable. The default is revealable (`Secret[Reveal, Seal]`).
pub(crate) fn parse_secret_inline(spec: &str) -> Result<runtime::SecretGrant, String> {
    let (body, sealed) = split_sealed(spec);
    match body.split_once('=') {
        Some((name, value)) if !name.is_empty() => {
            Ok(runtime::SecretGrant { name: name.to_string(), bytes: value.as_bytes().to_vec(), sealed })
        }
        _ => Err(format!("`--secret` expects `name=value[,sealed]`, got `{spec}`")),
    }
}

/// Parse a `--secret-file name=path[,sealed]` spec, reading the secret's bytes
/// from the file. Whitespace is NOT trimmed (a secret file holds exactly its
/// bytes). A trailing `,sealed` (RFC-0060/0121) grants it as `Secret[Seal]` —
/// usable by handle, never revealable — the shape a TLS private key should take.
pub(crate) fn parse_secret_file(spec: &str) -> Result<runtime::SecretGrant, String> {
    let (body, sealed) = split_sealed(spec);
    match body.split_once('=') {
        Some((name, path)) if !name.is_empty() => {
            let bytes = std::fs::read(path).map_err(|e| format!("`--secret-file {name}`: cannot read `{path}`: {e}"))?;
            Ok(runtime::SecretGrant { name: name.to_string(), bytes, sealed })
        }
        _ => Err(format!("`--secret-file` expects `name=path[,sealed]`, got `{spec}`")),
    }
}

/// (RFC-0060/0121) Peel a single trailing `,sealed` grant modifier off a secret
/// spec, returning the `name=…` body and whether sealing was requested. Only the
/// exact trailing token is recognized, so a `name=value` whose value happens to
/// contain commas is unaffected unless it literally ends in `,sealed`.
fn split_sealed(spec: &str) -> (&str, bool) {
    match spec.strip_suffix(",sealed") {
        Some(body) => (body, true),
        None => (spec, false),
    }
}

/// BUG-108 / BUG-114: the global mode selector (`--release`/`--debug`) is a LEADING
/// flag; a mode flag in the guest's argv (after the program file) must not flip the
/// compiler's optimization mode nor be double-consumed.
#[cfg(test)]
mod version_tests {
    use super::compiler_version;

    #[test]
    fn local_builds_report_the_package_version() {
        assert_eq!(compiler_version("0.1.0", None), "witchy 0.1.0");
        assert_eq!(compiler_version("0.1.0", Some("")), "witchy 0.1.0");
    }

    #[test]
    fn release_builds_report_the_exact_embedded_commit() {
        assert_eq!(
            compiler_version("0.1.0", Some("0123456789abcdef")),
            "witchy 0.1.0 (commit 0123456789abcdef)",
        );
    }
}

#[cfg(test)]
mod cli_flag_tests {
    use super::{leading_opt_mode, parse_confinement_mode};

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
    fn confinement_mode_accepts_only_public_launch_modes() {
        assert_eq!(
            parse_confinement_mode("best-effort"),
            Ok(witchy_confinement::EnforcementMode::BestEffort)
        );
        assert_eq!(
            parse_confinement_mode("required"),
            Ok(witchy_confinement::EnforcementMode::Required)
        );
        assert!(parse_confinement_mode("disabled")
            .unwrap_err()
            .contains("expected `best-effort` or `required`"));
    }

    #[test]
    fn mode_flags_in_guest_argv_are_ignored() {
        assert_eq!(leading_opt_mode(&argv(&["foo.witchy", "--release"])), None);
        assert_eq!(leading_opt_mode(&argv(&["app.wasm", "--debug", "hello"])), None);
        assert_eq!(leading_opt_mode(&argv(&["foo.witchy"])), None);
        assert_eq!(leading_opt_mode(&argv(&[])), None);
    }
}

#[cfg(test)]
mod secret_arg_tests {
    use super::{parse_secret_inline, split_sealed};

    #[test]
    fn inline_secret_preserves_equals_and_sealed() {
        let secret = parse_secret_inline("token=a=b,sealed").expect("valid secret");
        assert_eq!(secret.name, "token");
        assert_eq!(secret.bytes, b"a=b");
        assert!(secret.sealed);
    }

    #[test]
    fn sealed_is_only_an_exact_trailing_modifier() {
        assert_eq!(split_sealed("token=a,sealed"), ("token=a", true));
        assert_eq!(split_sealed("token=sealed,tail"), ("token=sealed,tail", false));
    }
}
