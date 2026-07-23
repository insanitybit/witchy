//! Compiled execution and interpreter/Wasm parity command services.

use witchy::enforce_performance_modes;
use witchy_syntax::ast;
use witchy_caps::capabilities;
use witchy_lower::codegen;
use witchy_interp::interpreter;
use witchy_runtime::runtime::{self, Runtime};
use crate::{
    commands, link_file_checked, linked_has_main, run_wasm_bytes,
    RUN_MEMORY_PAGES,
};

/// Run `witchy parity` when it is the requested command.
///
/// The boolean tells the composition root whether this service handled argv.
/// Every handled path retains the command\047s established process exit status.
pub(crate) fn run_parity() -> bool {
    if std::env::args().nth(1).as_deref() != Some("parity") {
        return false;
    }
    let Some(path) = std::env::args().nth(2) else {
        eprintln!("usage: witchy parity <file.witchy>");
        std::process::exit(PARITY_EXIT_UNEXPECTED);
    };
    let outcome = parity_check(&path);
    if outcome.is_pass() {
        println!("{}", outcome.message());
    } else {
        eprintln!("{}", outcome.message());
    }
    println!(
        "parity-stats outcome={} compared={} file={path}",
        outcome.tag(),
        outcome.compared()
    );
    std::process::exit(outcome.exit_code());
}

/// Failure diagnostics are observable backend behavior, so "both errored" is
/// agreement only when the complete messages agree. This rejects bare Wasm
/// traps, missing source locations, and backend-specific host failures alike.
fn backend_errors_agree(interp: &str, compiled: &str) -> bool {
    interp == compiled
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
pub(crate) enum ParityOutcome {
    /// Both backends produced equal output. Carries the compared line count.
    Agree { compared: usize, message: String },
    /// Both backends errored with byte-for-byte identical diagnostics.
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
    pub(crate) fn message(&self) -> &str {
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
/// exact error comparison closes the routed-diagnostic gap (RFC-0045). This is the
/// oracle the differential fuzzer and the example sweep drive as `witchy parity`.
pub(crate) fn parity_check(path: &str) -> ParityOutcome {
    use std::path::Path;
    macro_rules! unexpected {
        ($($arg:tt)*) => {
            return ParityOutcome::Unexpected { message: format!($($arg)*) }
        };
    }
    let (checked, stem) = match link_file_checked(path) {
        Ok(v) => v,
        Err(e) => unexpected!("{e}"),
    };
    let linked = checked.module();
    // Honor `mode opt` here too (BUG-119): a copy-cliff or a missing ownership
    // convention is a hard error under `mode opt` on every other path
    // (check/run/sandbox/emit) — a program `check` rejects must not slip through
    // `parity` as an "unexpected error" masquerading as a compile miss.
    if let Err(e) = enforce_performance_modes(linked, &stem) {
        unexpected!("{e}");
    }
    if !linked_has_main(linked) {
        unexpected!("`{path}` has no `main` to run");
    }
    // Compile first (borrows `linked`), then run the interpreter (consumes it).
    let bytes = match codegen::compile_checked_module_binary(&checked) {
        codegen::LoweringOutcome::Lowered(bytes) => bytes,
        codegen::LoweringOutcome::Unsupported(reason) => {
            unexpected!("cannot compile to WASM: {reason}")
        }
        codegen::LoweringOutcome::Rejected(error) => {
            unexpected!("cannot compile to WASM: {error}")
        }
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
    let interp = interpreter::run_checked_module(&checked, Path::new("."), Vec::new())
        .map_err(|e| e.to_string());
    let compiled = match &unmintable {
        Some(msg) => Err(witchy_syntax::diag::runtime_error("", 0, msg)),
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
        // Both fail: exact diagnostic parity is mandatory. A bare Wasm trap,
        // missing source location, or different host refusal is a divergence,
        // not "both error" agreement (RFC-0045 strict).
        (Err(i), Err(c)) => {
            if !backend_errors_agree(&i, &c) {
                return ParityOutcome::Diverge {
                    compared: 0,
                    message: format!(
                        "\u{2717} {path}: the two backends DIVERGE on the error diagnostic\n  \
                         interpreter: {i:?}\n  compiled:    {c:?}"
                    ),
                };
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

#[cfg(test)]
mod runtime_parity_tests {
    use super::{ParityOutcome, backend_errors_agree, parity_check};

    #[test]
    fn backend_errors_require_full_diagnostic_parity() {
        let full = "runtime error: `main`, line 3: division by zero";
        assert!(backend_errors_agree(full, full));
        assert!(!backend_errors_agree(full, "runtime error: division by zero"));
        assert!(!backend_errors_agree(full, "wasm trap: integer divide by zero"));
        assert!(backend_errors_agree("host refusal", "host refusal"));
        assert!(!backend_errors_agree("host refusal", "different refusal"));
    }

    #[test]
    fn parity_authenticates_user_dynamic_declarations() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "witchy-rfc0082-parity-{}-{nonce}.witchy",
            std::process::id(),
        ));
        std::fs::write(
            &path,
            r#"import reflect

type Counter derive(Reflect):
    value: Int

@dynamic
pub fn add(self: Counter, amount: Int) -> Counter:
    Counter(self.value + amount)

fn main(console: Console):
    console.print("dynamic-ready")
"#,
        )
        .expect("write dynamic parity fixture");

        let outcome = parity_check(path.to_str().expect("UTF-8 fixture path"));
        std::fs::remove_file(&path).expect("remove dynamic parity fixture");
        assert!(
            matches!(&outcome, ParityOutcome::Agree { compared: 1, .. }),
            "{}",
            outcome.message(),
        );
    }
}

/// Run an already-linked module on the compiled (WASM) backend with dev grants
/// derived from its footprint: Console output and argv always, plus Clock / Env /
/// Dir / Net / Secret each granted (at `dir_root` / `net_allow`) iff the footprint
/// shows the program uses it. `Dir`/`Net` rights narrow which host ops are linked.
/// This is the shared core of `witchy run` (dev, root at cwd) and `witchy sandbox`
/// (strict, announced grant) — one runtime for both, so dev == deploy. `strict_dir`
/// is the one host-policy difference: sandbox requires an explicit `--dir` (deny by
/// omission) while dev `run` defaults a `Dir` to the cwd. `test_mocks` enables only
/// the authority-free `testing.mock_*` host backends; it never mints a real grant.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_linked_compiled(
    linked: &ast::Module,
    dir_roots: Vec<std::path::PathBuf>,
    file_grants: Vec<std::path::PathBuf>,
    net_allow: Vec<String>,
    fetch_origins: Vec<String>,
    env_allow: Option<Vec<String>>,
    exec_allow: Option<Vec<String>>,
    args: Vec<String>,
    signing_key: Option<[u8; 32]>,
    named_secrets: Vec<runtime::SecretGrant>,
    user_cap_fields: Vec<Vec<String>>,
    strict_dir: bool,
    test_mocks: bool,
) -> Result<(Vec<String>, Option<i32>), String> {
    run_compiled(
        linked,
        None,
        dir_roots,
        file_grants,
        net_allow,
        fetch_origins,
        env_allow,
        exec_allow,
        args,
        signing_key,
        named_secrets,
        user_cap_fields,
        strict_dir,
        test_mocks,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn run_checked_compiled(
    checked: &witchy_types::pipeline::CheckedModule,
    dir_roots: Vec<std::path::PathBuf>,
    file_grants: Vec<std::path::PathBuf>,
    net_allow: Vec<String>,
    fetch_origins: Vec<String>,
    args: Vec<String>,
    signing_key: Option<[u8; 32]>,
    named_secrets: Vec<runtime::SecretGrant>,
    user_cap_fields: Vec<Vec<String>>,
    strict_dir: bool,
    test_mocks: bool,
) -> Result<(Vec<String>, Option<i32>), String> {
    let wasm = commands::compile::compile_checked_to_wasm_cached(checked)?;
    run_compiled(
        checked.module(),
        Some(wasm),
        dir_roots,
        file_grants,
        net_allow,
        fetch_origins,
        None,
        None,
        args,
        signing_key,
        named_secrets,
        user_cap_fields,
        strict_dir,
        test_mocks,
    )
}

#[allow(clippy::too_many_arguments)]
fn run_compiled(
    linked: &ast::Module,
    precompiled_wasm: Option<Vec<u8>>,
    dir_roots: Vec<std::path::PathBuf>,
    file_grants: Vec<std::path::PathBuf>,
    net_allow: Vec<String>,
    fetch_origins: Vec<String>,
    env_allow: Option<Vec<String>>,
    exec_allow: Option<Vec<String>>,
    args: Vec<String>,
    signing_key: Option<[u8; 32]>,
    named_secrets: Vec<runtime::SecretGrant>,
    user_cap_fields: Vec<Vec<String>>,
    strict_dir: bool,
    test_mocks: bool,
) -> Result<(Vec<String>, Option<i32>), String> {
    // Install the compiler-service natives (compiler.footprint/diff/doc),
    // which live above the runtime kernel in witchy-interp. This is THE compiled
    // chokepoint (execute_file / run / sandbox all route here), so no compiled
    // caller can resolve compiler.* without the vtable present.
    witchy_interp::compiler_natives::install();
    use witchy_runtime::runtime::Capabilities;
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
    //
    // (BUG-116/RFC-0005) A bare `Secret` is the ROOT signing-key externref; only
    // `--signing-key` mints it. A `--secret name=value` populates the
    // `SecretStore`, so accepting a named secret as the bare root would let
    // `crypto.reveal(key)` read an arbitrary named value as the root key. A bare
    // `Secret` therefore requires `--signing-key` specifically, independent of
    // any named secrets.
    if grant.contains_key("Secret") && signing_key.is_none() {
        return Err(
            "this program needs a root `Secret` (the signing key), but none was granted — provide `--signing-key <seed-file>` (a named `--secret`/`--secret-file` populates a `SecretStore`, not the bare `Secret`)".to_string(),
        );
    }
    // Quiet: the captured lines are printed once by the caller (and an
    // Int-returning `main` surfaces its value as the LAST line, which the
    // caller turns into the process exit code, like the interpreter CLI).
    let mut caps = Capabilities {
        print: grant
            .get("Console")
            .is_some_and(|rights| rights.contains("Write")),
        console_read: grant
            .get("Console")
            .is_some_and(|rights| rights.contains("Read")),
        print_int: true,
        quiet: true,
        args,
        user_cap_fields,
        test_mocks,
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
        caps.env_allow = env_allow;
    }
    if grant.contains_key("Exec") {
        caps.exec = true;
        caps.exec_allow = exec_allow;
    }
    let (dir_param_rights, file_param_rights) = crate::commands::sandbox::main_fs_param_rights(linked);
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
        caps.dir_rights = dir_param_rights;
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
        caps.file_rights = file_param_rights;
    }
    if let Some(rights) = grant.get("Net") {
        caps.net_allow = Some(net_allow);
        caps.net_connect = rights.contains("Connect");
        caps.net_listen = rights.contains("Listen");
    }
    if grant.contains_key("Fetch") {
        if fetch_origins.is_empty() {
            return Err(
                "this program's `main` requires `Fetch`, but no origins were granted \
                 (use `--fetch <scheme://host:port>`)"
                    .to_string(),
            );
        }
        witchy_runtime::fetch::FetchPolicy::allow(fetch_origins.clone())
            .map_err(|error| format!("invalid `--fetch` grant: {error}"))?;
        let fetch_params = linked
            .items
            .iter()
            .find_map(|item| match item {
                ast::Item::Function(function) if function.name == "main" => Some(
                    function
                        .params
                        .iter()
                        .filter(|param| {
                            matches!(
                                &param.ty,
                                Some(ast::Type::Named(name, _)) if name == "Fetch"
                            )
                        })
                        .count(),
                ),
                _ => None,
            })
            .unwrap_or(0);
        caps.fetch_grants = vec![fetch_origins; fetch_params];
    }
    if grant.contains_key("Secret") || grant.contains_key("SecretStore") {
        caps.signing_key = signing_key;
        // The signing key is also the named `signing` secret, so a bare root
        // `Secret` and `SecretStore.get("signing")` agree by identity while
        // still reaching the guest only as opaque refs. The `--secret` /
        // `--secret-file` grants follow, each reachable by
        // `SecretStore.get(name)` / `.require(name)`.
        if let Some(seed) = signing_key {
            caps.secrets.push(runtime::SecretGrant::new("signing", seed.to_vec()));
        }
        caps.secrets.extend(named_secrets);
    }
    let wasm = match precompiled_wasm {
        Some(wasm) => wasm,
        None => commands::compile::compile_linked_to_wasm_cached(linked)?,
    };
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
            e.root_cause().to_string()
        } else {
            // (RFC-0072 phase 2) Advertise the switch where it's needed: on the
            // trap itself. One line, only when the var is unset, only at the
            // CLI boundary — the message CORE stays identical across backends
            // (the parity harness compares the core, not this trailer).
            format!(
                "{}\n(re-run with WITCHY_WASM_BACKTRACE=1 for a full backtrace)",
                e.root_cause()
            )
        }
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
