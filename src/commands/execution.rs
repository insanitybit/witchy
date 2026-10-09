//! Compiled execution services.

use witchy_syntax::ast;
use witchy_caps::capabilities;
use witchy_lower::codegen;
use witchy_runtime::runtime::{self, Runtime};
use crate::{commands, run_memory_pages};
use crate::source::PrecompiledCheckedModule;

/// Run an already-linked module on the compiled (WASM) backend with dev grants
/// derived from its footprint: Console output and argv always, plus Clock / Env /
/// Dir / Net / Secret each granted (at `dir_root` / `net_allow`) iff the footprint
/// shows the program uses it. `Dir`/`Net` rights narrow which host ops are linked.
/// This is the shared core of `witchy run` (dev, root at cwd) and `witchy sandbox`
/// (strict, announced grant) — one runtime for both, so dev == deploy. `strict_dir`
/// is the one host-policy difference: sandbox requires an explicit `--dir` (deny by
/// omission) while dev `run` defaults a `Dir` to the cwd.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_checked_compiled(
    checked: &witchy_types::pipeline::CheckedModule,
    dir_roots: Vec<std::path::PathBuf>,
    file_grants: Vec<std::path::PathBuf>,
    net_grants: Vec<Vec<String>>,
    fetch_origins: Vec<String>,
    env_allow: Option<Vec<String>>,
    exec_allow: Option<Vec<String>>,
    exec_child_paths: Vec<std::path::PathBuf>,
    args: Vec<String>,
    signing_key: Option<[u8; 32]>,
    named_secrets: Vec<runtime::SecretGrant>,
    user_cap_fields: Vec<Vec<String>>,
    strict_dir: bool,
    confinement: witchy_confinement::EnforcementMode,
) -> Result<(Vec<String>, Option<i32>), String> {
    let artifact = match codegen::compile_checked_module_artifact(checked) {
        codegen::LoweringOutcome::Lowered(artifact) => artifact,
        codegen::LoweringOutcome::Unsupported(reason) => return Err(reason.to_string()),
        codegen::LoweringOutcome::Rejected(error) => return Err(error.to_string()),
    };
    run_compiled(
        checked.module(),
        CompiledWasm::Admitted(artifact),
        dir_roots,
        file_grants,
        net_grants,
        fetch_origins,
        env_allow,
        exec_allow,
        exec_child_paths,
        args,
        signing_key,
        named_secrets,
        user_cap_fields,
        strict_dir,
        confinement,
    )
}
/// Launch a checked module whose declaration environment admitted exact
/// separately compiled libraries. This is intentionally not used by `parity`,
/// the interpreter, or packaged-output routes: those routes have no source ABI
/// for external callable Wasm.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_precompiled_checked_compiled(
    linked: &PrecompiledCheckedModule,
    dir_roots: Vec<std::path::PathBuf>,
    file_grants: Vec<std::path::PathBuf>,
    net_grants: Vec<Vec<String>>,
    fetch_origins: Vec<String>,
    env_allow: Option<Vec<String>>,
    exec_allow: Option<Vec<String>>,
    exec_child_paths: Vec<std::path::PathBuf>,
    args: Vec<String>,
    signing_key: Option<[u8; 32]>,
    named_secrets: Vec<runtime::SecretGrant>,
    user_cap_fields: Vec<Vec<String>>,
    strict_dir: bool,
    confinement: witchy_confinement::EnforcementMode,
) -> Result<(Vec<String>, Option<i32>), String> {
    let artifact = match codegen::compile_checked_module_artifact(&linked.checked) {
        codegen::LoweringOutcome::Lowered(artifact) => artifact,
        codegen::LoweringOutcome::Unsupported(reason) => return Err(reason.to_string()),
        codegen::LoweringOutcome::Rejected(error) => return Err(error.to_string()),
    };
    run_compiled(
        linked.checked.module(),
        CompiledWasm::AdmittedWithExternalLibraries(artifact, linked.external_libraries.clone()),
        dir_roots,
        file_grants,
        net_grants,
        fetch_origins,
        env_allow,
        exec_allow,
        exec_child_paths,
        args,
        signing_key,
        named_secrets,
        user_cap_fields,
        strict_dir,
        confinement,
    )
}

/// Run a generated test driver through its task-specific checked boundary.
/// Unlike `run_checked_compiled`, this cannot accept or manufacture a general
/// production `CheckedModule` from the post-link AST rewrite.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_checked_test_driver_compiled(
    checked: &witchy_types::pipeline::CheckedTestDriverModule,
    dir_roots: Vec<std::path::PathBuf>,
    file_grants: Vec<std::path::PathBuf>,
    net_grants: Vec<Vec<String>>,
    fetch_origins: Vec<String>,
    env_allow: Option<Vec<String>>,
    exec_allow: Option<Vec<String>>,
    exec_child_paths: Vec<std::path::PathBuf>,
    args: Vec<String>,
    signing_key: Option<[u8; 32]>,
    named_secrets: Vec<runtime::SecretGrant>,
    user_cap_fields: Vec<Vec<String>>,
    strict_dir: bool,
    confinement: witchy_confinement::EnforcementMode,
) -> Result<(Vec<String>, Option<i32>), String> {
    let wasm = match codegen::compile_checked_test_driver_artifact(checked) {
        codegen::LoweringOutcome::Lowered(wasm) => wasm,
        codegen::LoweringOutcome::Unsupported(reason) => return Err(reason.to_string()),
        codegen::LoweringOutcome::Rejected(error) => return Err(error.to_string()),
    };
    run_compiled(
        checked.module(),
        CompiledWasm::Admitted(wasm),
        dir_roots,
        file_grants,
        net_grants,
        fetch_origins,
        env_allow,
        exec_allow,
        exec_child_paths,
        args,
        signing_key,
        named_secrets,
        user_cap_fields,
        strict_dir,
        confinement,
    )
}

enum CompiledWasm {
    Admitted(witchy_wir::AdmittedCanonicalArtifact),
    AdmittedWithExternalLibraries(
        witchy_wir::AdmittedCanonicalArtifact,
        Vec<runtime::AuthenticatedExternalLibrary>,
    ),
}

#[allow(clippy::too_many_arguments)]
fn run_compiled(
    linked: &ast::Module,
    wasm: CompiledWasm,
    dir_roots: Vec<std::path::PathBuf>,
    file_grants: Vec<std::path::PathBuf>,
    net_grants: Vec<Vec<String>>,
    fetch_origins: Vec<String>,
    env_allow: Option<Vec<String>>,
    exec_allow: Option<Vec<String>>,
    exec_child_paths: Vec<std::path::PathBuf>,
    args: Vec<String>,
    signing_key: Option<[u8; 32]>,
    named_secrets: Vec<runtime::SecretGrant>,
    user_cap_fields: Vec<Vec<String>>,
    strict_dir: bool,
    confinement: witchy_confinement::EnforcementMode,
) -> Result<(Vec<String>, Option<i32>), String> {
    // Install the compiler-service natives (compiler.footprint/diff/doc),
    // which live above the runtime kernel in witchy-compiler-services. This is THE compiled
    // chokepoint (execute_file / run / sandbox all route here), so no compiled
    // caller can resolve compiler.* without the vtable present.
    witchy_compiler_services::install();
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
        caps.exec_child_paths = exec_child_paths;
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
        // One allowlist per `Net` parameter of `main`, in declaration order: a
        // document binds each parameter to its own `[net]` entry, and a flag-built
        // grant passes one shared list.
        let mut grants = net_grants.into_iter();
        caps.net_allow = Some(grants.next().unwrap_or_default());
        caps.net_grants = grants.collect();
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
    commands::confinement::arm(&caps, confinement)?;
    let mut rt = Runtime::batch().map_err(|e| e.to_string())?;
    let mut vm = match &wasm {
        CompiledWasm::Admitted(artifact) => {
            rt.spawn_admitted(artifact, caps, run_memory_pages(artifact))
        }
        CompiledWasm::AdmittedWithExternalLibraries(artifact, libraries) => {
            rt.spawn_admitted_with_external_libraries(artifact, caps, run_memory_pages(artifact), libraries)
        }
    }
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
    // A module encoded under WITCHY_COUNT_MEMORY_OPS reports its exact
    // allocation, retain and release counts for the kernel A/B harness.
    if let Some(counts) = vm.canonical_memory_op_counts() {
        eprintln!("memory_ops allocs={} retains={} releases={}", counts.allocs, counts.retains, counts.releases);
    }
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
