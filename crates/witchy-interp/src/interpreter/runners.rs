//! Public run entry points: the family of `run_*` functions that construct an
//! interpreter, drive a program or module to completion, and adapt the various
//! capability/argument/exit-code envelopes. This is the crate's execution
//! façade over the evaluator.

#![allow(clippy::too_many_arguments)]

use std::path::{Path, PathBuf};

use witchy_syntax::ast::{Module, Type};
use witchy_types::runtime_type::RuntimeDeclarationCatalog;

use super::*;

pub fn run(src: &str) -> Result<Vec<String>, RuntimeError> {
    run_with(src, ".", Vec::new())
}

/// Run with a chosen root directory for the root `Dir` capability.
pub fn run_in(src: &str, root: impl AsRef<Path>) -> Result<Vec<String>, RuntimeError> {
    run_with(src, root, Vec::new())
}

/// Run with the host-provided root capabilities: `root` backs the root `Dir`,
/// and `net_allow` backs the root `Net` (the permitted `host:port` list).
/// `main` is the root entrypoint: it receives the capabilities it declares (the
/// only place authority is minted) and may pass attenuated ones to the functions
/// it calls.
pub fn run_with(
    src: &str,
    root: impl AsRef<Path>,
    net_allow: Vec<String>,
) -> Result<Vec<String>, RuntimeError> {
    let module = parse_module(src).map_err(|e| RuntimeError { message: e.to_string() })?;
    run_module(module, root, net_allow)
}

/// Run an already-built (e.g. linked) module.
pub fn run_module(
    module: Module,
    root: impl AsRef<Path>,
    net_allow: Vec<String>,
) -> Result<Vec<String>, RuntimeError> {
    run_module_args(module, root, net_allow, Vec::new())
}

/// Run a module that crossed the authenticated checked-link boundary. Dynamic
/// descriptor preparation consumes retained loader ownership directly; raw AST
/// runners cannot reconstruct package identity from flattened compiler names.
pub fn run_checked_module(
    checked: witchy_types::pipeline::CheckedModule,
    root: impl AsRef<Path>,
    net_allow: Vec<String>,
) -> Result<Vec<String>, RuntimeError> {
    let runtime_catalog = checked
        .runtime_declaration_catalog()
        .map_err(|error| RuntimeError { message: error.to_string() })?;
    let module = checked.into_module();
    let root = root.as_ref().to_path_buf();
    run_on_deep_stack(move || {
        run_module_inner_limited_with_catalog(
            module,
            root,
            Vec::new(),
            Vec::new(),
            net_allow,
            Vec::new(),
            None,
            Vec::new(),
            UserCapGrants::new(),
            DEFAULT_STEP_LIMIT,
            None,
            None,
            Some(runtime_catalog),
        )
    })
    .map(|outcome| outcome.output)
}

/// Like [`run_module`], but with an evaluation step ceiling — the `comptime:`
/// path, where termination is part of the contract.
pub fn run_module_budgeted(
    module: Module,
    root: impl AsRef<Path>,
    step_limit: u64,
) -> Result<Vec<String>, RuntimeError> {
    run_module_budgeted_in_scope(module, root, step_limit, None)
}

pub(crate) fn run_module_budgeted_in_scope(
    module: Module,
    root: impl AsRef<Path>,
    step_limit: u64,
    fresh_ident_scope: Option<String>,
) -> Result<Vec<String>, RuntimeError> {
    run_comptime_module_budgeted_in_scope(module, root, step_limit, fresh_ident_scope)
        .map(|(output, _)| output)
}

pub(crate) fn run_comptime_module_budgeted_in_scope(
    module: Module,
    root: impl AsRef<Path>,
    step_limit: u64,
    fresh_ident_scope: Option<String>,
) -> Result<(Vec<String>, Vec<PositionedComptimeItem>), RuntimeError> {
    run_comptime_module_outputs_budgeted_in_scope(
        module,
        root,
        step_limit,
        fresh_ident_scope,
    )
    .and_then(|outputs| {
        if !outputs.exprs.is_empty() {
            return err("expression output is valid only during tagged-literal expansion");
        }
        Ok((outputs.output, outputs.items))
    })
}

pub(crate) fn run_comptime_module_outputs_budgeted_in_scope(
    module: Module,
    root: impl AsRef<Path>,
    step_limit: u64,
    fresh_ident_scope: Option<String>,
) -> Result<ComptimeOutputs, RuntimeError> {
    run_comptime_module_outputs_budgeted_in_scope_with_qualifiers(
        module,
        root,
        step_limit,
        fresh_ident_scope,
        None,
    )
}

pub(crate) fn run_comptime_module_outputs_budgeted_in_scope_with_qualifiers(
    module: Module,
    root: impl AsRef<Path>,
    step_limit: u64,
    fresh_ident_scope: Option<String>,
    compiler_expr_qualifiers: Option<Vec<String>>,
) -> Result<ComptimeOutputs, RuntimeError> {
    let root = root.as_ref().to_path_buf();
    run_on_deep_stack(move || {
        run_module_inner_limited(module, root, Vec::new(), Vec::new(), Vec::new(), Vec::new(), None, Vec::new(), UserCapGrants::new(), step_limit, fresh_ident_scope, compiler_expr_qualifiers)
    })
    .map(|outcome| ComptimeOutputs {
        output: outcome.output,
        items: outcome.comptime_items,
        exprs: outcome.comptime_exprs,
    })
}

/// Run with direct `File` grants (RFC-0012): the i-th `File` parameter of `main`
/// maps to `file_grants[i]`. The differential-test oracle for `--file`.
pub fn run_module_files(
    module: Module,
    root: impl AsRef<Path>,
    file_grants: Vec<PathBuf>,
) -> Result<Vec<String>, RuntimeError> {
    let root = root.as_ref().to_path_buf();
    run_on_deep_stack(move || {
        run_module_inner_limited(module, root, Vec::new(), file_grants, Vec::new(), Vec::new(), None, Vec::new(), UserCapGrants::new(), DEFAULT_STEP_LIMIT, None, None)
    })
    .map(|outcome| outcome.output)
}

/// Like [`run_module`], but also hands command-line `args` to a `main` that
/// declares a `List(String)` parameter to receive them (argv is input data, not
/// authority, so it is an ordinary value parameter — not a capability).
pub fn run_module_args(
    module: Module,
    root: impl AsRef<Path>,
    net_allow: Vec<String>,
    args: Vec<String>,
) -> Result<Vec<String>, RuntimeError> {
    run_module_signed(module, root, net_allow, args, None)
}

/// Like [`run_module_args`], but also grants the root `Secret` capability
/// from `signing_key` (an Ed25519 seed) to a `main` that declares one. Signing is
/// authority, so the key is host-provided, never constructed by the program.
pub fn run_module_signed(
    module: Module,
    root: impl AsRef<Path>,
    net_allow: Vec<String>,
    args: Vec<String>,
    signing_key: Option<[u8; 32]>,
) -> Result<Vec<String>, RuntimeError> {
    run_module_exit(module, root, net_allow, args, signing_key).map(|(output, _)| output)
}

/// Like [`run_module_signed`], but also returns the process exit code (`main`'s
/// `Int` return, or 0). Used by the CLI to set the process status.
pub fn run_module_exit(
    module: Module,
    root: impl AsRef<Path>,
    net_allow: Vec<String>,
    args: Vec<String>,
    signing_key: Option<[u8; 32]>,
) -> Result<(Vec<String>, i32), RuntimeError> {
    // The execution boundary prepares records, traits, and existential
    // witnesses together so both interpreter runtime representations consume
    // one checked module.
    let root = root.as_ref().to_path_buf();
    run_on_deep_stack(move || run_module_inner(module, root, Vec::new(), net_allow, args, signing_key))
        .map(|outcome| (outcome.output, outcome.exit_code))
}

/// Like [`run_module_exit`], but also grants NAMED secrets to a `main` that binds
/// a `SecretStore` — each `(name, bytes, use_only)`; a use-only secret (RFC-0060)
/// is consumable by handle (`crypto.sign`, `server.serve_tls`) but `crypto.reveal`
/// on it errors. This is the interpreter twin of the compiled runtime's
/// `Capabilities.secrets`, for differential tests of secret-consuming servers.
pub fn run_module_exit_secrets(
    module: Module,
    root: impl AsRef<Path>,
    net_allow: Vec<String>,
    args: Vec<String>,
    signing_key: Option<[u8; 32]>,
    named_secrets: Vec<(String, Vec<u8>, bool)>,
) -> Result<(Vec<String>, i32), RuntimeError> {
    let root = root.as_ref().to_path_buf();
    run_on_deep_stack(move || {
        run_module_inner_limited(
            module,
            root,
            Vec::new(),
            Vec::new(),
            net_allow,
            args,
            signing_key,
            named_secrets,
            UserCapGrants::new(),
            DEFAULT_STEP_LIMIT,
            None,
            None,
        )
    })
    .map(|outcome| (outcome.output, outcome.exit_code))
}

/// Like [`run_module_exit`], but grants several `Dir` capabilities: `roots[0]`
/// backs the first `Dir` param of `main` (handle 0), `roots[1..]` the rest, in
/// order. For multi-directory programs — e.g. the witchy CLI holding both a
/// project `Dir` and a toolchain-`bin` `Dir`. See rfcs/0004-self-hosted-cli.md.
pub fn run_module_exit_dirs(
    module: Module,
    roots: Vec<PathBuf>,
    net_allow: Vec<String>,
    args: Vec<String>,
    signing_key: Option<[u8; 32]>,
) -> Result<(Vec<String>, i32), RuntimeError> {
    let mut roots = roots;
    let root = if roots.is_empty() { PathBuf::from(".") } else { roots.remove(0) };
    run_on_deep_stack(move || run_module_inner(module, root, roots, net_allow, args, signing_key))
        .map(|outcome| (outcome.output, outcome.exit_code))
}

/// Parse and run `src` with several `Dir` grants (the multi-`Dir` analog of
/// [`run_in`]); test/CLI helper for [`run_module_exit_dirs`].
pub fn run_in_dirs(src: &str, roots: &[PathBuf]) -> Result<Vec<String>, RuntimeError> {
    let module = parse_module(src).map_err(|e| RuntimeError { message: e.to_string() })?;
    run_module_exit_dirs(module, roots.to_vec(), Vec::new(), Vec::new(), None).map(|(out, _)| out)
}

/// Run the tree-walker on a thread with a large stack. The interpreter recurses
/// in Rust for nested calls, so deep (but bounded) recursion would otherwise
/// overflow the default stack and *abort the host*. The big stack accommodates
/// legitimate depth; `depth_limit` is the graceful guard against runaway
/// recursion well before this stack is exhausted. `join` contains any panic, so
/// even an unforeseen one becomes a graceful error rather than aborting the host.
#[cfg(not(target_arch = "wasm32"))]
fn run_on_deep_stack<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, RuntimeError> + Send + 'static,
) -> Result<T, RuntimeError> {
    let handle = std::thread::Builder::new()
        .stack_size(4 * 1024 * 1024 * 1024)
        .spawn(f)
        .map_err(|e| RuntimeError {
            message: format!("could not start the interpreter thread: {e}"),
        })?;
    match handle.join() {
        Ok(result) => result,
        Err(_) => Err(RuntimeError {
            message: "internal error: the interpreter thread panicked".into(),
        }),
    }
}

/// wasm32 (the browser playground) has neither threads nor a 4 GiB stack, so we
/// run the evaluator inline. The host stack is small, so very deep recursion can
/// trap the module — `depth_limit` still guards the common cases.
#[cfg(target_arch = "wasm32")]
fn run_on_deep_stack<T>(
    f: impl FnOnce() -> Result<T, RuntimeError>,
) -> Result<T, RuntimeError> {
    f()
}

/// Whether a `main` parameter type is `List(String)` — the slot that receives the
/// command-line arguments.
fn is_args_param(ty: &Option<Type>) -> bool {
    matches!(
        ty,
        Some(Type::Named(n, targs))
            if n == "List"
                && matches!(targs.as_slice(), [Type::Named(e, ea)] if e == "String" && ea.is_empty())
    )
}

/// Run a `main` that binds bare grantable capabilities (RFC-0038): each grantable
/// parameter mints a sealed record from `user_caps` (parameter name → field
/// values). The oracle for `witchy sandbox --grants` with a `[user_caps]` section.
pub fn run_module_user_caps(
    module: Module,
    root: impl AsRef<Path>,
    net_allow: Vec<String>,
    args: Vec<String>,
    file_grants: Vec<PathBuf>,
    user_caps: UserCapGrants,
) -> Result<Vec<String>, RuntimeError> {
    let root = root.as_ref().to_path_buf();
    run_on_deep_stack(move || {
        run_module_inner_limited(module, root, Vec::new(), file_grants, net_allow, args, None, Vec::new(), user_caps, DEFAULT_STEP_LIMIT, None, None)
    })
    .map(|outcome| outcome.output)
}

fn run_module_inner(
    module: Module,
    root: PathBuf,
    dir_roots: Vec<PathBuf>,
    net_allow: Vec<String>,
    args: Vec<String>,
    signing_key: Option<[u8; 32]>,
) -> Result<InterpreterOutcome, RuntimeError> {
    run_module_inner_limited(module, root, dir_roots, Vec::new(), net_allow, args, signing_key, Vec::new(), UserCapGrants::new(), DEFAULT_STEP_LIMIT, None, None)
}

/// Prepare the interpreter's executable AST through the same typed existential
/// contract as the compiled backend. The catalog must be captured before trait
/// lowering erases declarations; the resulting plan is then carried alongside
/// the lowered module instead of rediscovering dispatch at evaluation time.
fn prepare_runtime_module(
    module: Module,
    runtime_catalog: Option<&RuntimeDeclarationCatalog>,
) -> Result<(Module, WitnessPlan), RuntimeError> {
    let checked = witchy_syntax::source_check::check(module)
        .map_err(|message| RuntimeError { message })?;
    let checked = witchy_syntax::generators::lower(checked)
        .map_err(|message| RuntimeError { message })?;
    let checked = witchy_syntax::async_lower::lower(checked)
        .map_err(|message| RuntimeError { message })?;
    let module = witchy_syntax::records::lower(checked)
        .map_err(|message| RuntimeError { message })?
        .into_module();
    let catalog = witchy_types::witness::WitnessCatalog::from_module(&module);
    // Keep this ordering aligned with the compiled backend: trait lowering
    // resolves dynamic method slots before the type table records the concrete
    // construction sites that need compiler-owned packs.
    let mut module = witchy_types::traits::lower_for_wasm(module);
    witchy_syntax::parser::lower_sugar_module(&mut module);
    let typed = witchy_types::typeck::annotate_checked(module)
        .map_err(|error| RuntimeError { message: error.to_string() })?;
    let prepared = match runtime_catalog {
        Some(runtime_catalog) => witchy_types::existential::lower_explicit_packs_with_runtime_types(
            typed,
            &catalog,
            runtime_catalog,
        ),
        None => witchy_types::existential::lower_explicit_packs(typed, &catalog),
    }
    .map_err(|message| RuntimeError { message })?;
    let (module, _, witnesses) = prepared.into_parts();
    Ok((module, witnesses))
}

#[allow(clippy::too_many_arguments)]
fn run_module_inner_limited(
    module: Module,
    root: PathBuf,
    dir_roots: Vec<PathBuf>,
    file_grants: Vec<PathBuf>,
    net_allow: Vec<String>,
    args: Vec<String>,
    signing_key: Option<[u8; 32]>,
    named_secrets: Vec<(String, Vec<u8>, bool)>,
    user_caps: UserCapGrants,
    step_limit: u64,
    fresh_ident_scope: Option<String>,
    compiler_expr_qualifiers: Option<Vec<String>>,
) -> Result<InterpreterOutcome, RuntimeError> {
    run_module_inner_limited_with_catalog(
        module,
        root,
        dir_roots,
        file_grants,
        net_allow,
        args,
        signing_key,
        named_secrets,
        user_caps,
        step_limit,
        fresh_ident_scope,
        compiler_expr_qualifiers,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn run_module_inner_limited_with_catalog(
    module: Module,
    root: PathBuf,
    dir_roots: Vec<PathBuf>,
    file_grants: Vec<PathBuf>,
    net_allow: Vec<String>,
    args: Vec<String>,
    signing_key: Option<[u8; 32]>,
    named_secrets: Vec<(String, Vec<u8>, bool)>,
    user_caps: UserCapGrants,
    step_limit: u64,
    fresh_ident_scope: Option<String>,
    compiler_expr_qualifiers: Option<Vec<String>>,
    runtime_catalog: Option<RuntimeDeclarationCatalog>,
) -> Result<InterpreterOutcome, RuntimeError> {
    let (module, witnesses) = prepare_runtime_module(module, runtime_catalog.as_ref())?;
    let mut interp = Interpreter::new_with_witnesses(module, witnesses);
    interp.step_limit = step_limit;
    interp.fresh_ident_scope = fresh_ident_scope;
    interp.compiler_expr_qualifiers = compiler_expr_qualifiers;
    interp.root = root;
    interp.dir_roots = dir_roots;
    interp.file_grants = file_grants;
    interp.net_allow = net_allow;
    interp.signing_key = signing_key;
    interp.user_cap_grants = user_caps;
    // The signing key is the `signing` secret in the store, so a program may take
    // either a `Secret` (the key directly) or a `SecretStore` and `get("signing")`.
    if let Some(seed) = signing_key {
        // The signing key is never revealable, but its non-revealability is enforced
        // by the signing-key identity check, so it is stored use_only=false.
        interp.secrets.insert("signing".to_string(), (seed.to_vec(), false));
    }
    // Named `--secret`/`--secret-file` grants, each `(name, bytes, use_only)` —
    // a use-only secret (RFC-0060) is consumable by handle; `crypto.reveal` errors.
    for (name, bytes, use_only) in named_secrets {
        interp.secrets.insert(name, (bytes, use_only));
    }
    let root_args = match interp.functions.get("main").cloned() {
        Some(f) => {
            // Each `Dir` param maps positionally to a grant: the first to `root`
            // (handle 0), the rest to `dir_roots` in order (mirrors the compiled
            // backend's handle assignment). Other caps mint normally.
            let mut vals = Vec::with_capacity(f.params.len());
            let mut dir_idx = 0usize;
            let mut file_idx = 0usize;
            for p in &f.params {
                if is_args_param(&p.ty) {
                    vals.push(Value::list(args.iter().map(Value::str).collect()));
                } else if matches!(&p.ty, Some(Type::Named(n, _)) if n == "Dir") {
                    let r = if dir_idx == 0 {
                        interp.root.clone()
                    } else {
                        interp.dir_roots.get(dir_idx - 1).cloned().unwrap_or_else(|| interp.root.clone())
                    };
                    dir_idx += 1;
                    vals.push(Value::Dir(DirValue::Fs(r), String::new()));
                } else if matches!(&p.ty, Some(Type::Named(n, _)) if n == "File") {
                    // The i-th `File` param maps to the i-th `--file` grant (RFC-0012).
                    let path = interp.file_grants.get(file_idx).cloned().ok_or_else(|| RuntimeError {
                        message: "`main` requires a `File`, but the host granted none (provide `--file <path>`)".into(),
                    })?;
                    file_idx += 1;
                    vals.push(Value::File(FileValue::Fs(path)));
                } else if let Some(Type::Named(tn, _)) = &p.ty {
                    // RFC-0038: a record-typed `main` param is a bare grantable cap
                    // (typeck guarantees it); mint the sealed record from the grant.
                    // Any other named type falls through to the host-cap minter.
                    if interp.record_fields.contains_key(tn) {
                        vals.push(interp.mint_user_cap(&p.name, tn)?);
                    } else {
                        vals.push(interp.root_cap_for(&p.ty)?);
                    }
                } else {
                    vals.push(interp.root_cap_for(&p.ty)?);
                }
            }
            vals
        }
        None => vec![],
    };
    let ret = interp
        .call("main", root_args)
        .map_err(|e| rt_at_line(e, interp.cur_line, &interp.cur_fn))?;
    // `main` returning an `Int` sets the process exit code (the C/Go/Rust
    // convention) — it is *not* printed; a program shows output via `print`.
    let exit_code = match ret {
        Value::Int(n) => n as i32,
        _ => 0,
    };
    // A non-`Int`, non-`Nil` result (e.g. a `Float`) is still surfaced when the
    // program printed nothing — for a compiled `main -> Float` this is the only
    // way to show the value (the WASM backend has no float `to_string`).
    if interp.output.is_empty() && !matches!(ret, Value::Unit | Value::Int(_)) {
        interp.output.push(format!("{ret}"));
    }
    Ok(InterpreterOutcome {
        output: interp.output,
        exit_code,
        comptime_items: interp.comptime_item_output,
        comptime_exprs: interp.comptime_expr_output,
    })
}

/// The attenuated grants a build step runs under: a confined output directory
/// (always present — it is `BuildOut`), an optional confined read root, and
/// an immutable env snapshot, and allow-lists for the net/exec caps. Safe by
/// default — anything not granted here cannot be minted, so a build step
/// demanding it fails before running.
#[derive(Debug, Clone, Default)]
pub struct BuildGrants {
    pub out_dir: PathBuf,
    pub read_roots: Vec<PathBuf>,
    /// Granted environment values captured by the host before the build starts.
    /// Map membership is the allow-list; `None` represents an allowed but unset
    /// variable. The interpreter never consults mutable process-global env state.
    pub env: BTreeMap<String, Option<String>>,
    pub net_hosts: Vec<String>,
    pub exec_tools: Vec<String>,
}

/// Run a rune's `build` entrypoint under `grants`, on the interpreter, and return
/// the (sorted) names of the files it generated into `grants.out_dir`. The build
/// step's authority is exactly the build capabilities minted here: it cannot forge
/// a runtime capability (the type checker forbids a build step from taking one),
/// so even without the WASM sandbox it can only touch what these confined grants
/// permit. A module with no `build` entrypoint generates nothing.
pub fn run_build_step(module: Module, grants: BuildGrants) -> Result<Vec<String>, RuntimeError> {
    std::fs::create_dir_all(&grants.out_dir)
        .map_err(|e| RuntimeError { message: format!("build: cannot create output dir: {e}") })?;
    // Find the entrypoint before moving the module in — `build_entrypoint` is
    // robust to the linker's `mod.build` qualification.
    let Some(build) = witchy_syntax::build_entry::build_entrypoint(&module).cloned() else {
        return Ok(Vec::new());
    };
    let (module, witnesses) = prepare_runtime_module(module, None)?;
    let mut interp = Interpreter::new_with_witnesses(module, witnesses);
    let argv = build
        .params
        .iter()
        .map(|p| interp.mint_build_cap(&p.ty, &grants))
        .collect::<Result<Vec<_>, _>>()?;
    interp
        .call(&build.name, argv)
        .map_err(|e| rt_at_line(e, interp.cur_line, &interp.cur_fn))?;
    let mut generated: Vec<String> = std::fs::read_dir(&grants.out_dir)
        .map_err(|e| RuntimeError { message: format!("build: cannot read output dir: {e}") })?
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    generated.sort();
    Ok(generated)
}

/// Parse and link a multi-module program, then run it. `entry` is the module
/// holding `main`. Importing a module grants no authority — only `main`'s root
/// capabilities (and what it passes on) flow in.
pub fn run_program(sources: &[(&str, &str)], entry: &str) -> Result<Vec<String>, RuntimeError> {
    let mut modules = Vec::new();
    for (name, src) in sources {
        let m = parse_module(src).map_err(|e| RuntimeError {
            message: format!("{name}: {e}"),
        })?;
        modules.push((name.to_string(), m));
    }
    let linked = crate::pipeline::link(modules, entry)
        .map_err(|e| RuntimeError { message: e.message })?;
    run_module(linked, ".", Vec::new())
}
