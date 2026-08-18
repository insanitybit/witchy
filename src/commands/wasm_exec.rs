//! Compiled-Wasm module execution: run a `.wasm` file/module under the
//! capability sandbox, trusted-application launch, and the raw byte-buffer
//! runners used across the differential suite. Extracted from the composition root.

use crate::runtime::Runtime;
use crate::{artifact, runtime, trusted_exe};

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
pub(crate) fn witchy_imports(bytes: &[u8]) -> Result<Vec<String>, String> {
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
pub(crate) fn run_wasm_file(
    path: &str,
    dir_roots: Vec<std::path::PathBuf>,
    file_grants: Vec<std::path::PathBuf>,
    net_allow: Vec<String>,
    fetch_origins: Vec<String>,
    args: Vec<String>,
    signing_key: Option<[u8; 32]>,
    named_secrets: Vec<runtime::SecretGrant>,
    strict_dir: bool,
    confinement: witchy_confinement::EnforcementMode,
) -> Result<(Vec<String>, Option<i32>), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read `{path}`: {e}"))?;
    run_wasm_module(&bytes, dir_roots, file_grants, net_allow, fetch_origins, args, signing_key, named_secrets, strict_dir, confinement)
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
pub(crate) fn run_wasm_module(
    bytes: &[u8],
    dir_roots: Vec<std::path::PathBuf>,
    file_grants: Vec<std::path::PathBuf>,
    net_allow: Vec<String>,
    fetch_origins: Vec<String>,
    args: Vec<String>,
    signing_key: Option<[u8; 32]>,
    named_secrets: Vec<runtime::SecretGrant>,
    strict_dir: bool,
    confinement: witchy_confinement::EnforcementMode,
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
    let fetch_grant = imports_authority(Authority::FetchGrant) || declares("Fetch");
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
        // A listening program (e.g. `coven-serve`) is a long-running server: its
        // `main` never returns, so the buffered output path (drained only after
        // the run) would swallow its log/readiness lines. Stream them live and
        // flushed instead. Non-listening programs keep the buffered path, which
        // the differential/parity harness depends on.
        live_output: net_listen,
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
    // A Fetch host import does not imply a root Fetch grant: `Net[Connect,
    // Tcp].fetch(...)` deliberately mints a bounded Fetch inside the program.
    // The launch contract and `mint_fetch` import identify actual roots. An
    // explicitly supplied grant also remains available to legacy artifacts.
    if fetch_grant || !fetch_origins.is_empty() {
        if fetch_origins.is_empty() {
            return Err(
                "this module requires `Fetch`, but no origins were granted \
                 (use `--fetch <scheme://host:port>`)"
                    .to_string(),
            );
        }
        witchy_runtime::fetch::FetchPolicy::allow(fetch_origins.clone())
            .map_err(|error| format!("invalid `--fetch` grant: {error}"))?;
        caps.fetch_grants = vec![fetch_origins];
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
    run_prepared_wasm(bytes, caps, confinement)
}

/// Run the application embedded in this process with only its checked target
/// bindings. This bypasses every consumer grant/default path: there is no cwd
/// fallback, prompt, or interpretation of application argv as host policy.
pub(crate) fn run_trusted_application(
    application: &trusted_exe::OwnedEmbeddedApplication,
) -> Result<i32, String> {
    let cwd = std::env::current_dir()
        .map_err(|error| format!("cannot determine launch working directory: {error}"))?;
    let resolved = trusted_exe::resolve_binding_plan(
        &application.bindings,
        &application.wasm,
        &cwd,
    )?;
    let confinement = resolved.confinement;
    let declares = |name: &str| resolved.declared.contains_key(name);
    let declares_right = |name: &str, right: &str| {
        resolved.declared.get(name).is_some_and(|rights| rights.contains(right))
    };
    let mut network_grants = resolved.net_grants;
    let caps = runtime::Capabilities {
        print: true,
        print_int: true,
        quiet: true,
        clock: declares("Clock"),
        rand: declares("Rand"),
        env: declares("Env"),
        env_allow: resolved.env_allow,
        preopened_dirs: resolved.dir_authorities,
        dir_rights: resolved.dir_rights,
        dir_read: declares_right("Dir", "Read"),
        dir_write: declares_right("Dir", "Write"),
        preopened_files: resolved.file_authorities,
        file_rights: resolved.file_rights,
        exec: resolved.exec,
        exec_allow: resolved.exec_allow,
        exec_child_paths: resolved.exec_child_paths,
        net_allow: (!network_grants.is_empty()).then(|| network_grants.remove(0)),
        net_grants: network_grants,
        net_connect: declares_right("Net", "Connect"),
        net_listen: declares_right("Net", "Listen"),
        args: std::env::args().skip(1).collect(),
        secrets: resolved.secrets,
        ..Default::default()
    };
    let (lines, exit) = run_prepared_wasm(
        &application.wasm,
        caps,
        confinement,
    )?;
    for line in lines {
        println!("{line}");
    }
    Ok(exit.unwrap_or(0))
}

pub(crate) fn run_prepared_wasm(
    bytes: &[u8],
    caps: runtime::Capabilities,
    confinement: witchy_confinement::EnforcementMode,
) -> Result<(Vec<String>, Option<i32>), String> {
    crate::commands::confinement::arm(&caps, confinement)?;
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
pub(crate) const RUN_MEMORY_PAGES: usize = 16384;

/// Instantiate a compiled wasm binary under the dev grants and return its
/// captured output. The browser playground assembles the same binary and runs
/// *that*; this is the native equivalent.
pub(crate) fn run_wasm_bytes(bytes: &[u8]) -> Result<Vec<String>, String> {
    use crate::runtime::{Capabilities, Runtime};
    use std::cell::RefCell;
    // Run-to-completion: no scheduler, so use the non-preempting engine, which
    // omits the per-backedge epoch check and runs tight loops at full speed.
    // Reuse the engine across calls in this thread — the differential suite
    // invokes this hundreds of times and Engine::new is Cranelift ISA setup.
    thread_local! {
        static RUNTIME: RefCell<Runtime> = RefCell::new(Runtime::batch().expect("runtime"));
    }
    RUNTIME.with(|rt| {
    let mut rt = rt.borrow_mut();
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
    })
}

/// Run a compiled in-language test under exactly the authority a nullary test
/// needs: captured output plus authority-free runtime support (`__witchy_abort`
/// and heap guards are always linked by the runtime). No Clock/Rand/Env/Dir/File/
/// Net/Secret/Exec imports are granted here; a reached real host-capability use
/// therefore fails closed at instantiation.
pub(crate) fn run_wasm_test_bytes(bytes: &[u8]) -> Result<Vec<String>, String> {
    use crate::runtime::{Capabilities, Runtime};
    let mut rt = Runtime::batch().map_err(|e| e.to_string())?;
    let mut vm = rt
        .spawn(
            bytes,
            Capabilities {
                print: true,
                print_int: true,
                quiet: true,
                ..Default::default()
            },
            RUN_MEMORY_PAGES,
        )
        .map_err(|e| e.to_string())?;
    vm.run().map_err(|e| e.root_cause().to_string())?;
    Ok(vm.output())
}
