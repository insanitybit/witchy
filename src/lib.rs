//! The witchy library: the wasm-safe compiler front-end (lexer, parser, type
//! checker, linker, and temporary comptime/build services) plus the pure codegen/
//! format/doc passes. The frozen source tree evaluator is not a production
//! runtime or semantic oracle. The wasmtime sandbox, the package manager, and the LSP
//! live only in the binary (`main.rs`), so this crate compiles to
//! `wasm32-unknown-unknown` — which is what powers the in-browser playground.
//!
//! Build the browser module with:
//!   cargo build --release --lib --no-default-features \
//!       --target wasm32-unknown-unknown

// Mirror the binary crate's lint posture (these collapse-suggestions hurt the
// readability of the nested capability/pattern checks).
#![allow(
    clippy::collapsible_if,
    clippy::collapsible_match,
    clippy::items_after_test_module
)]
#![deny(unsafe_code)]

// The compiler is allocation-heavy: typeck's `Ty`/AST `Expr`/`Type` clone
// deeply on every checker fixpoint round (crates/witchy-types/src/typeck.rs),
// and a `sample` profile of an ordinary compile showed ~45-50% of total CPU
// in the system allocator (malloc/free/memset), not in any compiler
// algorithm. Measured with an interleaved A/B (alternating binaries built
// with each allocator, same fixture, same machine, same few minutes -- this
// box's load average swings 10x within seconds, so a same-binary
// before/after comparison is not trustworthy here): mimalloc won 9/9 rounds,
// averaging ~30-45% less wall time per compile. This is the ONE place it can
// be declared (a binary/cdylib may have at most one `#[global_allocator]` in
// its whole dependency graph) so it covers the CLI binary AND every
// integration test binary that links this lib -- where the gate's wall-clock
// actually lives.
// Native-only: unavailable (and pointless) on the wasm32 playground build,
// which already disables the `native` feature this is gated behind.
#[cfg(feature = "native")]
#[global_allocator]
static GLOBAL_ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

// RFC-0018: AST→WIR lowering (codegen) + its uniqueness analysis live in the
// `witchy-lower` crate.
pub use witchy_lower::codegen;
// RFC-0018: the front-end + AST-level base layer lives in the `witchy-syntax`
// crate; re-export so the rest of the compiler keeps using
// `crate::{ast,parser,…}::…` paths unchanged.
pub use witchy_syntax::{ast, format, opt, parser};
// RFC-0030: deterministic optimization counters (`witchy stats`) — native-only
// (needs the wasmtime sandbox to run a program and read its counters).
#[cfg(feature = "native")]
pub mod stats;
// RFC-0018: footprint analysis + grant docs live in the `witchy-caps` crate.
pub use witchy_caps::capabilities;
pub mod artifact;
#[cfg(test)]
mod capabilities_tests;
#[cfg(feature = "native")]
pub mod trusted_publisher;
// RFC-0018: runtime values + the capability host live in `witchy-runtime`
// (wasm-safe); the wasmtime sandbox `runtime` is native-only.
#[cfg(feature = "native")]
pub use witchy_runtime::runtime;
// RFC-0018: the frozen source evaluator used temporarily for compile-time
// evaluation lives in the `witchy-interp` crate.
pub use witchy_interp::{comptime, interpreter, pipeline};
// RFC-0018: type checking + trait resolution live in the `witchy-types` crate.
pub use witchy_types::typeck;
// RFC-0018: the WIR group lives in the `witchy-wir` crate; re-export it so the
// rest of the compiler keeps using `crate::wir::…` paths unchanged.

/// A failure while loading bundled sources or crossing the checked pipeline.
#[derive(Debug, Clone, PartialEq)]
pub enum ResolveStdError {
    Parse {
        module: String,
        error: witchy_syntax::parser::ParseError,
    },
    UnknownModule {
        name: String,
        suggestion: Option<String>,
    },
    Pipeline(witchy_interp::pipeline::PipelineError),
}

impl std::fmt::Display for ResolveStdError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Parse { error, .. } => std::fmt::Display::fmt(error, f),
            Self::UnknownModule { name, suggestion } => {
                write!(f, "unknown module `{name}`")?;
                if let Some(suggestion) = suggestion {
                    write!(f, " — did you mean `import {suggestion}`?")?;
                }
                f.write_str(" (the browser playground has only the bundled std)")
            }
            Self::Pipeline(error) => std::fmt::Display::fmt(error, f),
        }
    }
}

impl std::error::Error for ResolveStdError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Parse { error, .. } => Some(error),
            Self::Pipeline(error) => Some(error),
            Self::UnknownModule { .. } => None,
        }
    }
}

impl From<witchy_interp::pipeline::PipelineError> for ResolveStdError {
    fn from(error: witchy_interp::pipeline::PipelineError) -> Self {
        Self::Pipeline(error)
    }
}

impl From<witchy_types::runtime_type::RuntimeTypeError> for ResolveStdError {
    fn from(error: witchy_types::runtime_type::RuntimeTypeError) -> Self {
        Self::Pipeline(error.into())
    }
}

/// Resolve a single-source program against the BUNDLED standard library only
/// (no filesystem — the browser has none): parse the entry, then breadth-first
/// load each `import`ed std module from the embedded sources and link them.
#[cfg(test)]
pub(crate) fn resolve_std_only(src: &str) -> Result<witchy_syntax::ast::Module, String> {
    let modules = resolve_std_modules(src).map_err(|error| error.to_string())?;
    witchy_interp::pipeline::link(modules, "main").map_err(|error| error.to_string())
}

/// Resolve bundled standard-library imports and retain proof that the linked
/// program passed the canonical type checker.
pub fn resolve_std_only_checked(
    src: &str,
) -> Result<witchy_interp::pipeline::CheckedModule, ResolveStdError> {
    use witchy_types::runtime_type::{
        AuthenticatedModuleOwners, ModuleLoadIdentity, PackageCoordinate, PackageSource,
    };

    let sources = resolve_std_sources(src)?;
    let application = PackageCoordinate::new(
        PackageSource::Workspace,
        "witchy/source",
        env!("CARGO_PKG_VERSION"),
    )?;
    let toolchain = PackageCoordinate::new(
        PackageSource::Toolchain,
        "witchy/std",
        env!("CARGO_PKG_VERSION"),
    )?;
    let mut assignments = vec![(
        "main".to_string(),
        ModuleLoadIdentity::new(application, ["source", "main"])?,
    )];
    assignments.extend(
        witchy_syntax::linker::STD_MODULES
            .iter()
            .map(|name| {
                ModuleLoadIdentity::new(toolchain.clone(), ["std", *name])
                    .map(|owner| ((*name).to_string(), owner))
            })
            .collect::<Result<Vec<_>, _>>()?,
    );
    let playground = PackageCoordinate::new(
        PackageSource::Toolchain,
        "witchy/glamour",
        env!("CARGO_PKG_VERSION"),
    )?;
    assignments.extend(
        witchy_syntax::linker::PLAYGROUND_MODULES
            .iter()
            .map(|name| {
                ModuleLoadIdentity::new(playground.clone(), ["src", *name])
                    .map(|owner| ((*name).to_string(), owner))
            })
            .collect::<Result<Vec<_>, _>>()?,
    );
    let owners = AuthenticatedModuleOwners::from_loader_assignments(assignments)?;
    let user_modules = std::collections::HashSet::from(["main".to_string()]);
    witchy_interp::pipeline::link_checked_authenticated_from_source_bundle_with_user_modules(
        sources,
        "main",
        &user_modules,
        owners,
    )
    .map_err(Into::into)
}

fn resolve_std_modules(
    src: &str,
) -> Result<Vec<(String, witchy_syntax::ast::Module)>, ResolveStdError> {
    resolve_std_sources(src)?
        .into_iter()
        .map(|(name, source)| {
            let parsed = witchy_syntax::parser::parse_module(
                std::str::from_utf8(&source).expect("source resolver stores UTF-8 Witchy source"),
            )
            .map_err(|error| ResolveStdError::Parse {
                module: name.clone(),
                error,
            })?;
            Ok((name, parsed))
        })
        .collect()
}

fn resolve_std_sources(src: &str) -> Result<Vec<(String, Vec<u8>)>, ResolveStdError> {
    use std::collections::{HashSet, VecDeque};
    let entry =
        witchy_syntax::parser::parse_module(src).map_err(|error| ResolveStdError::Parse {
            module: "main".to_string(),
            error,
        })?;
    let mut sources = vec![("main".to_string(), src.as_bytes().to_vec())];
    let mut loaded: HashSet<String> = HashSet::from(["main".to_string()]);
    let mut queue: VecDeque<witchy_syntax::ast::Module> = VecDeque::from([entry]);
    while let Some(module) = queue.pop_front() {
        for name in module.imports.clone() {
            if !loaded.insert(name.clone()) {
                continue;
            }
            let source = witchy_syntax::linker::bundled_source(&name).ok_or_else(|| {
                ResolveStdError::UnknownModule {
                    suggestion: witchy_syntax::linker::closest_std_module(&name)
                        .map(str::to_string),
                    name: name.clone(),
                }
            })?;
            let parsed = witchy_syntax::parser::parse_module(source).map_err(|error| {
                ResolveStdError::Parse {
                    module: name.clone(),
                    error,
                }
            })?;
            queue.push_back(parsed.clone());
            sources.push((name, source.as_bytes().to_vec()));
        }
    }
    Ok(sources)
}

/// Whether a linked function originated in the entry file. The linker keeps the
/// entry module's `main` unqualified and qualifies its other functions with the
/// entry stem; linked-in modules carry a different prefix.
pub fn is_entry_function(name: &str, entry_stem: &str) -> bool {
    name == "main" || name.starts_with("main::<") || name.starts_with(&format!("{entry_stem}."))
}

/// Enforce each source declaration's performance mode after linking and type
/// checking, including opt callees imported by normal callers. This lives in
/// the wasm-safe library so CLI, LSP, and browser apply one contract.
pub fn enforce_performance_modes(
    linked: &witchy_syntax::ast::Module,
    entry_stem: &str,
) -> Result<(), String> {
    let source_modes: Vec<&str> = linked
        .modes
        .iter()
        .filter(|mode| !mode.starts_with('@'))
        .map(String::as_str)
        .collect();
    if source_modes.is_empty() && !linked.modes.iter().any(|mode| mode.starts_with("@opt:")) {
        return Ok(());
    }

    let mode_names = source_modes.join(", ");
    let declaration_modes = |function: &str| {
        if !source_modes.is_empty() && is_entry_function(function, entry_stem) {
            Some(mode_names.as_str())
        } else if witchy_lower::analysis::function_is_opt(linked, function) {
            Some("opt")
        } else {
            None
        }
    };
    let mut errors = Vec::new();

    for (func, cliff) in witchy_lower::analysis::module_cliffs(linked) {
        let Some(mode_names) = declaration_modes(&func) else { continue; };
        errors.push(format!(
            "error: in `{func}` (line {}): `{}` is rebuilt by copy on every \
             iteration of this loop — it is {} [mode {}]\n  keep `{}` on the \
             in-place path: certify helper calls with `let`/`own` so they do \
             not alias it out, and do not share it mid-loop",
            cliff.line, cliff.var, cliff.reason, mode_names, cliff.var,
        ));
    }

    for miss in witchy_lower::analysis::try_module_no_copy_misses(linked)? {
        let Some(mode_names) = declaration_modes(&miss.function) else { continue; };
        let advice = if miss.reason.contains("first-class call ABI") {
            "call the function directly so its checked capacity state stays in the compiled ABI, or use normal mode for the copy-correct indirect call".to_string()
        } else {
            format!(
                "keep `{}` uniquely owned: initialize it from fresh storage or a `var` parameter typed `unique`, and do not alias or loan it before this call",
                miss.var
            )
        };
        errors.push(format!(
            "error: in `{}` (line {}): `{}` cannot satisfy the no-copy `var` \
             contract of `{}` — {} [mode {}]\n  {}",
            miss.function, miss.line, miss.var, miss.callee, miss.reason, mode_names, advice,
        ));
    }

    for miss in witchy_lower::analysis::module_fip_misses(linked) {
        let Some(mode_names) = declaration_modes(&miss.function) else { continue; };
        errors.push(format!(
            "error: in `{}` (line {}): functional-in-place contract failed — {} \
             [mode {}]\n  keep recursion in tail position, forward the `own unique` \
             parameter directly, mutate only its fields, and return that owner on every exit",
            miss.function, miss.line, miss.reason, mode_names,
        ));
    }

    for item in &linked.items {
        let witchy_syntax::ast::Item::Function(function) = item else {
            continue;
        };
        let Some(mode_names) = declaration_modes(&function.name) else { continue; };
        for param in &function.params {
            if param.convention == witchy_syntax::ast::Convention::Let
                && ownership_relevant(&param.ty)
            {
                errors.push(format!(
                    "error: in `{}`: parameter `{}` has no ownership convention — \
                     `mode {}` requires an explicit `let` (read-only borrow), `own` \
                     (consumed), or `var` (mutated in place)",
                    function.name, param.name, mode_names,
                ));
            }
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("\n"))
    }
}

/// Whether an ownership convention changes generated code for this parameter.
/// Qualifiers refine the contract but do not hide the underlying heap type.
pub fn ownership_relevant(ty: &Option<witchy_syntax::ast::Type>) -> bool {
    ty.as_ref().is_some_and(ownership_relevant_type)
}

fn ownership_relevant_type(ty: &witchy_syntax::ast::Type) -> bool {
    match ty {
        witchy_syntax::ast::Type::Named(name, _) => {
            !matches!(name.as_str(), "Int" | "Float" | "Bool" | "Duration")
                && !witchy_caps::capabilities::is_capability_type_name(name)
        }
        witchy_syntax::ast::Type::Tuple(_) => true,
        // Record composition is normalized before ownership analysis; keep the
        // raw syntax conservatively ownership-relevant if tooling asks early.
        witchy_syntax::ast::Type::RecordCompose { .. } => true,
        // (RFC-0081) A dyn value is a heap value, so its convention matters.
        witchy_syntax::ast::Type::Dyn(_, _) => true,
        witchy_syntax::ast::Type::Fn(_, _, _, _) => false,
        witchy_syntax::ast::Type::Slice(inner) => ownership_relevant_type(inner),
        witchy_syntax::ast::Type::Qualified(qualifier, inner) => match qualifier {
            // Explicit reference syntax already declares shared/exclusive access.
            // Requiring a second `let` convention would create two spellings for
            // the same contract and incorrectly reject canonical RFC-0167 source.
            witchy_syntax::ast::TypeQual::View
            | witchy_syntax::ast::TypeQual::Borrow(_)
            | witchy_syntax::ast::TypeQual::BorrowMut(_)
            | witchy_syntax::ast::TypeQual::LegacyBorrow(_) => false,
            witchy_syntax::ast::TypeQual::Frozen | witchy_syntax::ast::TypeQual::Unique => {
                ownership_relevant_type(inner)
            }
        },
    }
}

/// Compile a witchy program to a WebAssembly **binary** the browser's own engine
/// can instantiate: resolve against the bundled std, type-check, codegen to WAT,
/// then assemble with the pure-Rust `wat` crate. This is the codegen path that
/// replaces the in-browser interpreter — the page runs the SAME module `witchy
/// sandbox` would, so the playground is now dev == deploy too. The produced module
/// imports `witchy.print` plus, only if used, the pure helpers below; the page
/// supplies them (capabilities are granted as trapping stubs — the browser has
/// none).
pub fn compile_source(src: &str) -> Result<Vec<u8>, String> {
    let checked = resolve_std_only_checked(src).map_err(|error| error.to_string())?;
    let linked = checked.module();
    enforce_performance_modes(linked, "main")?;
    // Compile through the WIR → wasm-binary pipeline (`wasmparser`/`wasm-encoder`,
    // pure Rust, so this runs on EVERY target including the browser playground).
    let bytes = match witchy_lower::codegen::compile_checked_module_binary(&checked) {
        witchy_lower::codegen::LoweringOutcome::Lowered(bytes) => bytes,
        witchy_lower::codegen::LoweringOutcome::Unsupported(reason) => {
            return Err(format!("cannot compile to WASM: {reason}"));
        }
        witchy_lower::codegen::LoweringOutcome::Rejected(error) => {
            return Err(format!("cannot compile to WASM: {error}"));
        }
    };
    // Canonical admission already emitted and authenticated `witchy.launch`.
    Ok(bytes)
}

#[cfg(test)]
mod canonical_source_witness_tests {
    #[test]
    fn browser_resolver_mints_an_exact_source_witness() {
        let checked = super::resolve_std_only_checked(
            "pub fn export_echo(input: Bytes) -> Bytes:\n    input\n",
        )
        .expect("browser source resolves through canonical type checking");
        let digest = checked
            .canonical_source_witness()
            .expect("checked browser source carries an exact-source witness")
            .source_bundle_digest();
        let changed = super::resolve_std_only_checked(
            "pub fn export_echo(input: Bytes) -> Bytes:\n    input\n\n",
        )
        .expect("byte-distinct browser source also resolves")
        .canonical_source_witness()
        .expect("byte-distinct source carries a witness")
        .source_bundle_digest();

        assert_ne!(digest, changed, "the witness must bind exact source bytes");
    }

    #[test]
    fn browser_compile_preserves_the_single_admitted_launch_section() {
        let wasm = super::compile_source("fn main() -> Int:\n    42\n")
            .expect("canonical scalar source compiles");
        let launch_sections = wasmparser::Parser::new(0)
            .parse_all(&wasm)
            .filter_map(|payload| match payload.expect("admitted Wasm parses") {
                wasmparser::Payload::CustomSection(section)
                    if section.name() == "witchy.launch" => Some(section.data().to_vec()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(launch_sections.len(), 1);
    }
}

#[cfg(test)]
mod performance_mode_tests {
    #[test]
    fn browser_compile_exports_a_pure_bytes_entrypoint() {
        let wasm = super::compile_source("pub fn export_echo(input: Bytes) -> Bytes:\n    input\n")
            .expect("Bytes export compiles");
        let mut exports = Vec::new();
        for payload in wasmparser::Parser::new(0).parse_all(&wasm) {
            if let wasmparser::Payload::ExportSection(section) = payload.expect("parse wasm") {
                for export in section {
                    exports.push(export.expect("parse export").name.to_owned());
                }
            }
        }
        for expected in ["memory", "__galloc", "__export_export_echo"] {
            assert!(
                exports.iter().any(|name| name == expected),
                "missing {expected} from {exports:?}"
            );
        }
    }

    #[test]
    fn browser_compile_enforces_fip_contracts() {
        let error = super::compile_source(
            r#"mode opt

type State:
    count: Int

fn run(own state: unique State, n: Int) -> unique State:
    if n == 0:
        return state
    let next = run(state, n - 1)
    next

fn main(console: Console):
    console.print("${run(State(0), 2).count}")
"#,
        )
        .expect_err("mode opt must reject non-tail FIP recursion");

        // The canonical route defers the `own unique` entry contract
        // (spec/language.md, "Functional-in-place state kernels"): the call
        // that would enter it is refused fail-closed before the tail-position
        // judgment runs, so the program still cannot compile.
        assert!(
            error.contains("`unique` parameter in `mode opt`"),
            "{error}"
        );
    }

    #[test]
    fn browser_compile_enforces_no_copy_contracts() {
        let error = super::compile_source(
            r#"mode opt

import dict

fn main(console: Console):
    var d = dict.new()
    let snapshot = d
    let _ = d.insert("a", 2)
    console.print("${snapshot.length()}")
"#,
        )
        .expect_err("mode opt must reject an aliased no-copy receiver");

        // `let snapshot = d` consumes the fresh unique handle (RFC-0167 §4), so
        // the later mutation is a use after transfer.
        assert!(
            error.contains("after it was transferred")
                || error.contains("cannot repair ordinary storage into a `unique` parameter"),
            "{error}"
        );
    }

    #[test]
    fn browser_compile_enforces_place_assignment_no_copy_contract() {
        let error = super::compile_source(
            r#"mode opt

import dict

fn main(console: Console):
    var d = dict.new()
    let snapshot = d
    d["a"] = 2
    console.print("${snapshot.length()}")
"#,
        )
        .expect_err("place assignment must retain the discarded Dict contract");

        // `let snapshot = d` consumes the fresh unique handle (RFC-0167 §4), so
        // the later mutation is a use after transfer.
        assert!(
            error.contains("after it was transferred")
                || error.contains("cannot repair ordinary storage into a `unique` parameter"),
            "{error}"
        );
    }

    #[test]
    fn performance_gate_includes_entry_lambdas() {
        let linked = super::resolve_std_only(
            r#"mode opt

fn take(var xs: unique List(Int)) -> ():
    return

fn main() -> Int:
    let work = fn() -> ():
        var xs = [1]
        let snapshot = xs
        take(xs)
        let _ = snapshot
        return
    work()
    0
"#,
        )
        .expect("resolve lambda program");
        // The no-copy judgment moved into type checking and reaches the body
        // of an entry lambda: `snapshot` aliases `xs` inside `work`.
        let error = witchy_types::typeck::check(&linked)
            .expect_err("an entry lambda is part of the entry module contract")
            .to_string();
        assert!(
            error.contains("cannot repair ordinary storage into a `unique` parameter"),
            "{error}"
        );
        assert!(error.contains("line 10"), "{error}");
    }

    fn linked_performance_boundary(helper: &str, caller: &str) -> witchy_syntax::ast::Module {
        let mut modules = super::resolve_std_modules(helper).expect("resolve helper dependencies");
        modules.iter_mut().find(|(name, _)| name == "main").unwrap().0 = "helper".into();
        modules.push(("main".into(), witchy_syntax::parser::parse_module(caller).expect("parse caller")));
        witchy_interp::pipeline::link(modules, "main").expect("link and type-check boundary")
    }

    #[test]
    fn performance_gate_checks_imported_opt_bodies_for_normal_and_opt_callers() {
        let helper = "mode opt\n\npub fn accumulate() -> Int:\n    var xs = []\n    var snapshots = []\n    for i in [1, 2, 3]:\n        list.push(snapshots, xs)\n        list.push(xs, i)\n    list.length(xs) + list.length(snapshots[0])\n";
        for mode in ["", "mode opt\n"] {
            let caller = format!("{mode}import helper\n\nfn main() -> Int:\n    helper.accumulate()\n");
            let linked = linked_performance_boundary(helper, &caller);
            let error = super::enforce_performance_modes(&linked, "main")
                .expect_err("an imported opt body's existing copy cliff must reject");
            assert!(error.contains("in `helper.accumulate`"), "{error}");
            assert!(error.contains("rebuilt by copy on every iteration"), "{error}");
            assert!(error.contains("[mode opt]"), "{error}");
        }
        let linked = linked_performance_boundary(helper.strip_prefix("mode opt\n").unwrap(),
            "import helper\nfn main() -> Int:\n    helper.accumulate()\n");
        super::enforce_performance_modes(&linked, "main")
            .expect("the same copy-correct body remains permitted in normal mode");
    }

    #[test]
    fn performance_gate_keeps_normal_caller_costs_separate_from_opt_callee() {
        let linked = linked_performance_boundary(
            "mode opt\n\npub fn update(var xs: List(Int)) -> ():\n    xs[0] = 2\n",
            "import helper\n\nfn main() -> Int:\n    var xs = [1]\n    let snapshot = xs\n    helper.update(xs)\n    snapshot[0] + xs[0]\n");
        super::enforce_performance_modes(&linked, "main")
            .expect("normal caller may retain a snapshot while calling an ordinary-var opt helper");
    }

    #[test]
    fn explicit_reference_parameters_need_no_redundant_convention() {
        let linked = super::resolve_std_only(
            "mode opt\n\nfn shared(value: &String) -> &String:\n    value\n\nfn main() -> Int:\n    0\n",
        )
        .expect("resolve canonical reference signatures");
        witchy_types::typeck::check(&linked).expect("type-check canonical reference signatures");
        super::enforce_performance_modes(&linked, "main")
            .expect("reference access is already explicit in the type");
    }
}

/// The exact float formatting both backends share. The playground's host shim
/// delegates `float_to_str` here so it never reimplements (and so never diverges
/// from) Rust's float `Display`.
pub fn render_float(x: f64) -> String {
    witchy_syntax::fmt::render_float(x)
}

/// `string.from_code` via the shared native registry — the same `char::from_u32`
/// both backends use. Out-of-range/surrogate becomes U+FFFD.
pub fn string_from_code(cp: i64) -> String {
    native_str(
        "string.from_code",
        witchy_runtime::value::NativeValue::Int(cp),
    )
    .unwrap_or_else(|_| "\u{FFFD}".to_string())
}

/// `encoding.*` via the shared native registry (hex/base64), selected by the
/// same op table the native WASM runtime uses. Text ops read `input` lossily as
/// UTF-8; byte ops preserve the raw slice and return raw flat-buffer bytes.
/// The playground host shim delegates here.
pub fn encoding(op: i32, input: &[u8]) -> Result<Vec<u8>, String> {
    use witchy_runtime::value::NativeValue;
    enum Input {
        String,
        Bytes,
        LossyString,
    }
    let (name, input_kind) = match op {
        0 => ("encoding.hex_encode", Input::String),
        1 => ("encoding.hex_decode_lossy", Input::String),
        2 => ("encoding.base64_encode", Input::String),
        3 => ("encoding.base64_decode_lossy", Input::String),
        4 => ("encoding.hex_to_base64url_lossy", Input::String),
        5 => ("encoding.base64url_decode_lossy", Input::String),
        6 => ("encoding.base64url_to_hex_lossy", Input::String),
        7 => ("encoding.utf8_lossy", Input::LossyString),
        8 => ("encoding.hex_encode_bytes", Input::Bytes),
        9 => ("encoding.base64_encode_bytes", Input::Bytes),
        10 => ("encoding.base64url_encode_bytes", Input::Bytes),
        11 => ("encoding.hex_decode_bytes_raw", Input::String),
        12 => ("encoding.base64_decode_bytes_raw", Input::String),
        13 => ("encoding.base64url_decode_bytes_raw", Input::String),
        _ => return Err(format!("unknown encoding op {op}")),
    };
    let arg = match input_kind {
        Input::String => NativeValue::Str(String::from_utf8_lossy(input).into_owned()),
        Input::Bytes => NativeValue::Bytes(input.to_vec()),
        Input::LossyString => NativeValue::Str(String::from_utf8_lossy(input).into_owned()),
    };
    let f =
        witchy_runtime::native::lookup(name).ok_or_else(|| format!("{name} is not registered"))?;
    match f(&[arg]).map_err(|e| e.message)? {
        NativeValue::Str(s) => Ok(s.into_bytes()),
        NativeValue::Bytes(bytes) => Ok(bytes),
        _ => Err(format!("{name} did not return a flat buffer")),
    }
}

fn native_str(name: &str, arg: witchy_runtime::value::NativeValue) -> Result<String, String> {
    let f =
        witchy_runtime::native::lookup(name).ok_or_else(|| format!("{name} is not registered"))?;
    match f(&[arg]).map_err(|e| e.message)? {
        witchy_runtime::value::NativeValue::Str(s) => Ok(s),
        _ => Err(format!("{name} did not return a String")),
    }
}

fn native_call(name: &str, args: &[&str]) -> Result<witchy_runtime::value::NativeValue, String> {
    let f =
        witchy_runtime::native::lookup(name).ok_or_else(|| format!("{name} is not registered"))?;
    let vals: Vec<witchy_runtime::value::NativeValue> = args
        .iter()
        .map(|s| witchy_runtime::value::NativeValue::Str(s.to_string()))
        .collect();
    f(&vals).map_err(|e| e.message)
}

/// SHA-256 / SHA-512 / SHA3-256 of `input` as lowercase hex (op 0/1/2), via the
/// shared native registry. The playground host shim delegates here so a pasted
/// hashing program runs in the browser instead of trapping — the bundled `regex`/
/// `crypto` std modules are native-backed, and the browser has no filesystem
/// sibling to fall back to.
pub fn crypto_hash(op: i32, input: &str) -> String {
    let name = match op {
        0 => "crypto.sha256",
        1 => "crypto.sha512",
        2 => "crypto.sha3_256",
        _ => return String::new(),
    };
    match native_call(name, &[input]) {
        Ok(witchy_runtime::value::NativeValue::Str(s)) => s,
        _ => String::new(),
    }
}

/// HMAC-SHA256(key, message) as hex.
pub fn hmac_sha256(key: &str, msg: &str) -> String {
    match native_call("crypto.hmac_sha256", &[key, msg]) {
        Ok(witchy_runtime::value::NativeValue::Str(s)) => s,
        _ => String::new(),
    }
}

/// `regex.match_spans(pattern, text)` — the packed match-span string both backends
/// share; the host shim stages it through `fill_pending` like the native runtime.
pub fn regex_spans(pattern: &str, text: &str) -> String {
    match native_call(
        witchy_syntax::intrinsics::REGEX_MATCH_SPANS,
        &[pattern, text],
    ) {
        Ok(witchy_runtime::value::NativeValue::Str(s)) => s,
        // The browser host calls this through the C-style wasm export, whose ABI
        // can only return bytes. Prefix host-visible errors with an impossible
        // span byte; web/witchy-host.js turns it back into a thrown error.
        Err(e) => format!("\x1fregex-error:{e}"),
        _ => format!(
            "\x1fregex-error:{} did not return a String",
            witchy_syntax::intrinsics::REGEX_MATCH_SPANS
        ),
    }
}

/// Signature verification status (op 0 ed25519, 1 ecdsa_p256, 2 ecdsa_p256_hex);
/// private std wrappers map the status into `Result(Bool, String)`.
pub fn crypto_verify_status(op: i32, pk: &str, msg: &str, sig: &str) -> i64 {
    let name = match op {
        0 => "crypto.__ed25519_verify_status",
        1 => "crypto.__ecdsa_p256_verify_status",
        2 => "crypto.__ecdsa_p256_verify_hex_status",
        _ => return -4,
    };
    match native_call(name, &[pk, msg, sig]) {
        Ok(witchy_runtime::value::NativeValue::Int(n)) => n,
        _ => -4,
    }
}

/// Back-compat for older playground hosts that still ask only for a bool.
pub fn crypto_verify(op: i32, pk: &str, msg: &str, sig: &str) -> bool {
    crypto_verify_status(op, pk, msg, sig) == 1
}

// --- the browser ABI (no wasm-bindgen; hand-marshaled UTF-8) -----------------
//
// JS writes the source into memory it got from `witchy_alloc`, calls
// `witchy_compile(ptr, len)` to get the program's wasm binary, then instantiates
// THAT module on the browser's own WebAssembly engine — the interpreter never
// runs a user program in the browser. During the program's run its `witchy.*`
// host imports call back into the page, which delegates the pure ones
// (`float_to_str`, `string_from_code`, `encoding`) to the exports below so they
// match the native backend byte-for-byte.
//
// Result framing: `witchy_compile` returns `[u32 status][u32 len][payload]`
// (status 0 → wasm bytes, 1 → utf-8 error). The helper exports return
// `[u32 len][payload]` (always succeed; bad input already folds to a sentinel).
// The caller frees each block (`8 + len` or `4 + len`) with `witchy_free`.

#[cfg(any(
    all(target_arch = "wasm32", feature = "browser-fixtures"),
    all(test, feature = "browser-fixtures")
))]
mod fixture_sessions {
    use std::collections::BTreeMap;

    use witchy_test_host::FixtureHost;
    use witchy_testkit::TestResult;

    #[derive(Default)]
    pub struct FixtureSessionStore {
        sessions: BTreeMap<u32, FixtureHost>,
        next_session: u32,
    }

    impl FixtureSessionStore {
        pub fn open(&mut self, plan: &[u8]) -> Result<String, String> {
            let host = FixtureHost::from_plan_json(plan).map_err(|error| error.to_string())?;
            let roots: serde_json::Value =
                serde_json::from_str(&host.roots_json().map_err(|error| error.to_string())?)
                    .map_err(|error| {
                        format!("fixture host returned invalid roots JSON: {error}")
                    })?;
            let session = self.allocate_session()?;
            self.sessions.insert(session, host);
            Ok(serde_json::json!({
                "version": 1,
                "session": session.to_string(),
                "host": roots,
            })
            .to_string())
        }

        pub fn invoke(&mut self, session: u32, request: &[u8]) -> Result<String, String> {
            self.sessions
                .get_mut(&session)
                .ok_or_else(|| format!("unknown or finished fixture session {session}"))?
                .invoke_json(request)
                .map_err(|error| error.to_string())
        }

        pub fn finish(&mut self, session: u32, result: TestResult) -> Result<String, String> {
            self.sessions
                .remove(&session)
                .ok_or_else(|| format!("unknown or finished fixture session {session}"))?
                .finish_json(result)
                .map_err(|error| error.to_string())
        }

        pub fn discard(&mut self, session: u32) -> bool {
            self.sessions.remove(&session).is_some()
        }

        fn allocate_session(&mut self) -> Result<u32, String> {
            for _ in 0..=self.sessions.len() {
                let candidate = self.next_session.max(1);
                self.next_session = candidate.checked_add(1).unwrap_or(1);
                if !self.sessions.contains_key(&candidate) {
                    return Ok(candidate);
                }
            }
            Err("fixture session identifier space exhausted".to_owned())
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        const PLAN: &[u8] = br#"{"version":1,"argv":["browser"]}"#;

        #[test]
        fn sessions_are_isolated_consumed_once_and_explicitly_discardable() {
            let mut store = FixtureSessionStore::default();
            let first: serde_json::Value =
                serde_json::from_str(&store.open(PLAN).expect("first session")).unwrap();
            let second: serde_json::Value =
                serde_json::from_str(&store.open(PLAN).expect("second session")).unwrap();
            let first = first["session"].as_str().unwrap().parse::<u32>().unwrap();
            let second = second["session"].as_str().unwrap().parse::<u32>().unwrap();
            assert_ne!(first, second);

            let request = br#"{"version":1,"request":{"operation":"argv"}}"#;
            assert!(store.invoke(first, request).unwrap().contains("browser"));
            assert!(store.invoke(second, request).unwrap().contains("browser"));
            assert!(store
                .invoke(u32::MAX, request)
                .expect_err("forged session must fail")
                .contains("unknown"));

            let transcript = store.finish(first, TestResult::Passed).unwrap();
            assert!(transcript.contains("\"result\":{\"kind\":\"passed\"}"));
            assert!(store.finish(first, TestResult::Passed).is_err());
            assert!(store.discard(second));
            assert!(!store.discard(second));
        }
    }
}

/// Owned browser-ABI buffers. JavaScript sees only the address of each boxed
/// slice; Rust retains ownership and validates the exact base/length before a
/// read or free. That turns the foreign pointer contract into safe, testable
/// map lookups instead of relying on `from_raw_parts`/manual deallocation.
#[cfg(any(target_arch = "wasm32", test))]
mod abi_buffers {
    #[cfg(target_arch = "wasm32")]
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    #[derive(Default)]
    struct BufferStore {
        live: BTreeMap<usize, Box<[u8]>>,
    }

    impl BufferStore {
        fn store(&mut self, bytes: Vec<u8>) -> *mut u8 {
            if bytes.is_empty() {
                return std::ptr::null_mut();
            }
            let mut bytes = bytes.into_boxed_slice();
            let ptr = bytes.as_mut_ptr();
            let old = self.live.insert(ptr as usize, bytes);
            debug_assert!(
                old.is_none(),
                "two live boxes cannot have the same base address"
            );
            ptr
        }

        fn allocate(&mut self, len: usize) -> *mut u8 {
            if len == 0 {
                std::ptr::null_mut()
            } else {
                self.store(vec![0; len])
            }
        }

        fn read(&self, ptr: *const u8, len: usize) -> Option<Vec<u8>> {
            if ptr.is_null() {
                return (len == 0).then(Vec::new);
            }
            self.live
                .get(&(ptr as usize))
                .and_then(|bytes| bytes.get(..len))
                .map(<[u8]>::to_vec)
        }

        fn free(&mut self, ptr: *mut u8, len: usize) -> bool {
            if ptr.is_null() {
                return len == 0;
            }
            let key = ptr as usize;
            if !matches!(self.live.get(&key), Some(bytes) if bytes.len() == len) {
                return false;
            }
            let removed = self.live.remove(&key);
            debug_assert_eq!(removed.as_deref().map(<[u8]>::len), Some(len));
            true
        }
    }

    #[cfg(target_arch = "wasm32")]
    thread_local! {
        static BUFFERS: RefCell<BufferStore> = RefCell::new(BufferStore::default());
    }

    #[cfg(target_arch = "wasm32")]
    pub fn allocate(len: usize) -> *mut u8 {
        BUFFERS.with(|buffers| buffers.borrow_mut().allocate(len))
    }

    #[cfg(target_arch = "wasm32")]
    pub fn store(bytes: Vec<u8>) -> *mut u8 {
        BUFFERS.with(|buffers| buffers.borrow_mut().store(bytes))
    }

    #[cfg(target_arch = "wasm32")]
    pub fn read(ptr: *const u8, len: usize) -> Option<Vec<u8>> {
        BUFFERS.with(|buffers| buffers.borrow().read(ptr, len))
    }

    #[cfg(target_arch = "wasm32")]
    pub fn free(ptr: *mut u8, len: usize) -> bool {
        BUFFERS.with(|buffers| buffers.borrow_mut().free(ptr, len))
    }

    #[cfg(test)]
    mod tests {
        use super::BufferStore;

        #[test]
        fn wasm_export_lint_exception_contains_no_unsafe_operations() {
            // `wasm_abi` must allow Rust 2024's `#[unsafe(no_mangle)]`
            // attributes, so guard that narrow lint exception against growing
            // actual unsafe functions, impls, extern blocks, or operations.
            let unsafe_operation =
                regex::Regex::new(r"\bunsafe\s*(?:\{|fn\b|impl\b|extern\b|trait\b)").unwrap();
            assert!(
                !unsafe_operation.is_match(include_str!("lib.rs")),
                "the browser ABI must use safe Rust despite its export-attribute lint exception"
            );
        }

        #[test]
        fn reads_only_live_base_bounded_ranges() {
            let mut buffers = BufferStore::default();
            let ptr = buffers.store(b"witchy".to_vec());
            assert_eq!(buffers.read(ptr, 6).as_deref(), Some(b"witchy".as_slice()));
            assert_eq!(buffers.read(ptr, 3).as_deref(), Some(b"wit".as_slice()));
            assert!(
                buffers.read(ptr, 7).is_none(),
                "an oversized range must be rejected"
            );
            assert!(
                buffers.read(std::ptr::dangling(), 1).is_none(),
                "a forged base must be rejected"
            );
            assert_eq!(buffers.read(std::ptr::null(), 0), Some(Vec::new()));
            assert!(buffers.read(std::ptr::null(), 1).is_none());
        }

        #[test]
        fn free_requires_exact_live_allocation() {
            let mut buffers = BufferStore::default();
            let ptr = buffers.allocate(8);
            assert!(
                !buffers.free(ptr, 7),
                "a mismatched layout must not free the box"
            );
            assert!(
                buffers.read(ptr, 8).is_some(),
                "a rejected free must leave the allocation live"
            );
            assert!(buffers.free(ptr, 8));
            assert!(!buffers.free(ptr, 8), "a double free must be rejected");
            assert!(buffers.free(std::ptr::null_mut(), 0));
            assert!(!buffers.free(std::ptr::null_mut(), 1));
        }
    }
}

#[cfg(target_arch = "wasm32")]
#[allow(
    unsafe_code,
    reason = "Rust 2024 classifies exported no_mangle symbols as unsafe attributes; a source-level test forbids unsafe operations in this module"
)]
mod wasm_abi {
    use super::abi_buffers;
    #[cfg(feature = "browser-fixtures")]
    use std::cell::RefCell;

    #[cfg(feature = "browser-fixtures")]
    thread_local! {
        static FIXTURE_SESSIONS: RefCell<super::fixture_sessions::FixtureSessionStore> =
            RefCell::new(super::fixture_sessions::FixtureSessionStore::default());
    }

    /// Allocate `len` bytes of guest memory for JS to write into.
    #[unsafe(no_mangle)]
    pub extern "C" fn witchy_alloc(len: usize) -> *mut u8 {
        abi_buffers::allocate(len)
    }

    /// Free a buffer previously returned by `witchy_alloc` / `witchy_compile` /
    /// the helper exports.
    #[unsafe(no_mangle)]
    pub extern "C" fn witchy_free(ptr: *mut u8, len: usize) {
        // Invalid, mismatched, forged, and already-freed pairs fail closed. The
        // host has no authority to make Rust deallocate an unowned address.
        let _ = abi_buffers::free(ptr, len);
    }

    /// `[u32 status][u32 len][payload]` in a fresh buffer handed to JS.
    fn pack_tagged(status: u32, payload: &[u8]) -> *mut u8 {
        let mut out = Vec::with_capacity(8 + payload.len());
        out.extend_from_slice(&status.to_le_bytes());
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(payload);
        abi_buffers::store(out)
    }

    /// `[u32 len][payload]` in a fresh buffer handed to JS.
    fn pack(payload: &[u8]) -> *mut u8 {
        let mut out = Vec::with_capacity(4 + payload.len());
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(payload);
        abi_buffers::store(out)
    }

    /// Compile the source at `ptr[..len]` to a wasm binary; status 0 → bytes,
    /// 1 → error message. Free with `witchy_free(p, 8 + len)`.
    #[unsafe(no_mangle)]
    pub extern "C" fn witchy_compile(ptr: *const u8, len: usize) -> *mut u8 {
        let Some(src) = abi_buffers::read(ptr, len) else {
            return pack_tagged(1, b"browser ABI rejected an invalid source buffer");
        };
        let src = String::from_utf8_lossy(&src).into_owned();
        match super::compile_source(&src) {
            Ok(binary) => pack_tagged(0, &binary),
            Err(message) => pack_tagged(1, message.as_bytes()),
        }
    }

    /// Open a deterministic fixture session from canonical plan JSON.
    ///
    /// Returns tagged JSON containing the nonzero session identifier and roots
    /// minted from the plan. The session is scoped to this compiler-Wasm
    /// instance, which the book runs in a fresh opaque-origin frame.
    #[cfg(feature = "browser-fixtures")]
    #[unsafe(no_mangle)]
    pub extern "C" fn witchy_fixture_open(ptr: *const u8, len: usize) -> *mut u8 {
        let Some(plan) = abi_buffers::read(ptr, len) else {
            return pack_tagged(1, b"browser ABI rejected an invalid fixture plan buffer");
        };
        match FIXTURE_SESSIONS.with(|sessions| sessions.borrow_mut().open(&plan)) {
            Ok(response) => pack_tagged(0, response.as_bytes()),
            Err(message) => pack_tagged(1, message.as_bytes()),
        }
    }

    /// Dispatch one versioned fixture-host request and return its tagged JSON
    /// response. Provider failures are successful protocol responses.
    #[cfg(feature = "browser-fixtures")]
    #[unsafe(no_mangle)]
    pub extern "C" fn witchy_fixture_invoke(session: u32, ptr: *const u8, len: usize) -> *mut u8 {
        let Some(request) = abi_buffers::read(ptr, len) else {
            return pack_tagged(1, b"browser ABI rejected an invalid fixture request buffer");
        };
        match FIXTURE_SESSIONS.with(|sessions| sessions.borrow_mut().invoke(session, &request)) {
            Ok(response) => pack_tagged(0, response.as_bytes()),
            Err(message) => pack_tagged(1, message.as_bytes()),
        }
    }

    /// Consume one fixture session and return its canonical transcript JSON.
    ///
    /// Status 0 is passed, 1 is failed, and 2 is infrastructure error. Failed
    /// statuses carry their message in `ptr[..len]`; passed requires no message.
    #[cfg(feature = "browser-fixtures")]
    #[unsafe(no_mangle)]
    pub extern "C" fn witchy_fixture_finish(
        session: u32,
        status: i32,
        ptr: *const u8,
        len: usize,
    ) -> *mut u8 {
        let Some(message) = abi_buffers::read(ptr, len) else {
            return pack_tagged(1, b"browser ABI rejected an invalid fixture result buffer");
        };
        let result = match status {
            0 if message.is_empty() => witchy_testkit::TestResult::Passed,
            0 => {
                return pack_tagged(
                    1,
                    b"a passed fixture result must not carry an error message",
                );
            }
            1 | 2 => {
                let Ok(message) = String::from_utf8(message) else {
                    return pack_tagged(1, b"fixture result message must be valid UTF-8");
                };
                if status == 1 {
                    witchy_testkit::TestResult::Failed { message }
                } else {
                    witchy_testkit::TestResult::InfrastructureError { message }
                }
            }
            _ => return pack_tagged(1, b"unknown fixture result status"),
        };
        match FIXTURE_SESSIONS.with(|sessions| sessions.borrow_mut().finish(session, result)) {
            Ok(transcript) => pack_tagged(0, transcript.as_bytes()),
            Err(message) => pack_tagged(1, message.as_bytes()),
        }
    }

    /// Drop an unfinished session after an adapter exception. Returns 1 only
    /// when a live registry entry was removed; arbitrary identifiers do nothing.
    #[cfg(feature = "browser-fixtures")]
    #[unsafe(no_mangle)]
    pub extern "C" fn witchy_fixture_discard(session: u32) -> i32 {
        FIXTURE_SESSIONS
            .with(|sessions| sessions.borrow_mut().discard(session))
            .into()
    }

    /// `float_to_str(x)` → `[u32 len][utf-8]`. Free with `witchy_free(p, 4 + len)`.
    #[unsafe(no_mangle)]
    pub extern "C" fn witchy_render_float(x: f64) -> *mut u8 {
        pack(super::render_float(x).as_bytes())
    }

    /// `string_from_code(cp)` → `[u32 len][utf-8]`.
    #[unsafe(no_mangle)]
    pub extern "C" fn witchy_string_from_code(cp: i64) -> *mut u8 {
        pack(super::string_from_code(cp).as_bytes())
    }

    /// `encoding(op, input[..in_len])` → `[u32 len][bytes]`. Text results use
    /// UTF-8; byte decoders preserve their raw output. Public witchy decoders
    /// validate before calling a raw op, so the pointer-only bridge has no error
    /// variant; an impossible host-table error becomes an empty result.
    #[unsafe(no_mangle)]
    pub extern "C" fn witchy_encoding(op: i32, in_ptr: *const u8, in_len: usize) -> *mut u8 {
        let Some(input) = abi_buffers::read(in_ptr, in_len) else {
            return pack(&[]);
        };
        let out = super::encoding(op, &input).unwrap_or_default();
        pack(&out)
    }

    fn text(ptr: *const u8, len: usize) -> Option<String> {
        abi_buffers::read(ptr, len).map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
    }

    /// `crypto.sha256/sha512/sha3_256(op, input)` → `[u32 len][hex]`.
    #[unsafe(no_mangle)]
    pub extern "C" fn witchy_crypto_hash(op: i32, in_ptr: *const u8, in_len: usize) -> *mut u8 {
        let Some(input) = text(in_ptr, in_len) else {
            return pack(&[]);
        };
        pack(super::crypto_hash(op, &input).as_bytes())
    }

    /// `crypto.hmac_sha256(key, msg)` → `[u32 len][hex]`.
    #[unsafe(no_mangle)]
    pub extern "C" fn witchy_hmac_sha256(
        k_ptr: *const u8,
        k_len: usize,
        m_ptr: *const u8,
        m_len: usize,
    ) -> *mut u8 {
        let (Some(key), Some(message)) = (text(k_ptr, k_len), text(m_ptr, m_len)) else {
            return pack(&[]);
        };
        pack(super::hmac_sha256(&key, &message).as_bytes())
    }

    /// `regex.match_spans(pattern, text)` → `[u32 len][packed spans]`.
    #[unsafe(no_mangle)]
    pub extern "C" fn witchy_regex(
        p_ptr: *const u8,
        p_len: usize,
        t_ptr: *const u8,
        t_len: usize,
    ) -> *mut u8 {
        let (Some(pattern), Some(text)) = (text(p_ptr, p_len), text(t_ptr, t_len)) else {
            return pack(&[]);
        };
        pack(super::regex_spans(&pattern, &text).as_bytes())
    }

    /// Signature verify status (op 0 ed25519, 1/2 ecdsa): 1 valid, 0 invalid
    /// signature, negative malformed input/unavailable.
    #[unsafe(no_mangle)]
    pub extern "C" fn witchy_verify_status(
        op: i32,
        pk_ptr: *const u8,
        pk_len: usize,
        m_ptr: *const u8,
        m_len: usize,
        s_ptr: *const u8,
        s_len: usize,
    ) -> i64 {
        let (Some(pk), Some(message), Some(signature)) =
            (text(pk_ptr, pk_len), text(m_ptr, m_len), text(s_ptr, s_len))
        else {
            return -4;
        };
        super::crypto_verify_status(op, &pk, &message, &signature)
    }

    /// Signature verify (op 0 ed25519, 1/2 ecdsa) → 1 if valid, else 0.
    #[unsafe(no_mangle)]
    pub extern "C" fn witchy_verify(
        op: i32,
        pk_ptr: *const u8,
        pk_len: usize,
        m_ptr: *const u8,
        m_len: usize,
        s_ptr: *const u8,
        s_len: usize,
    ) -> i32 {
        let (Some(pk), Some(message), Some(signature)) =
            (text(pk_ptr, pk_len), text(m_ptr, m_len), text(s_ptr, s_len))
        else {
            return 0;
        };
        (super::crypto_verify_status(op, &pk, &message, &signature) == 1) as i32
    }
}
