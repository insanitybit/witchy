//! Compiler-service natives (`compiler.footprint`/`diff`/`doc`/`try_doc`).
//!
//! Moved out of `witchy-runtime` so the runtime kernel sheds its parser/type/
//! caps implementation dependencies (RFC-0018 stage-DAG). The interpreter
//! dispatches these directly; the compiled backend reaches the same code via
//! the injected `CompilerServices` seam, whose default reads the vtable this
//! module installs into `witchy_runtime`.

use witchy_runtime::value::{NativeError as RuntimeError, NativeValue as Value};
use witchy_syntax::ast::{Item, Module};
use witchy_syntax::intrinsics;

fn type_error(msg: impl Into<String>) -> RuntimeError {
    RuntimeError { message: msg.into() }
}

/// Install the compiler-native vtable into `witchy_runtime` so the compiled
/// backend's default `CompilerServices` can dispatch. Idempotent.
pub fn install() {
    witchy_runtime::native::install_compiler_natives(footprint, diff, doc, doc_result_json);
}

/// Compute the capability footprint of witchy `source`, returned as JSON:
/// `{"total":[..],"build":[..],"user_caps":[..],"entries":[{"name":..,"capabilities":[..],"brands":[..]}]}`,
/// or `{"error":".."}` if the source does not parse. `build` is the build-time
/// footprint — the build capabilities (`BuildOut`/`BuildRead`/…) the rune's
/// `build` entrypoint demands, which a build tool gates separately from the
/// runtime `total`. Pairs with `std/json`.
pub fn footprint(args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::Str(src)] = args else {
        return Err(type_error("compiler.footprint expects a String"));
    };
    let json = match checked_source_only_module(src, "compiler.footprint") {
        Ok(module) => {
            let fp = witchy_caps::capabilities::analyze(&module);
            let total = arr(fp.total.iter().map(|(n, r)| witchy_caps::capabilities::show_cap(n, r)));
            let build = arr(fp.build.iter().map(|(n, r)| witchy_caps::capabilities::show_cap(n, r)));
            // RFC-0038: the grantable user-capability axis (bare policy tokens).
            let user_caps = arr(fp.user_caps.iter().cloned());
            let entries: Vec<String> = fp
                .entries
                .iter()
                .map(|e| {
                    let caps =
                        arr(e.capabilities.iter().map(|(n, r)| witchy_caps::capabilities::show_cap(n, r)));
                    let brands = arr(e.brands.iter().cloned());
                    format!(
                        "{{\"name\":{},\"capabilities\":{},\"brands\":{}}}",
                        string(&e.name),
                        caps,
                        brands
                    )
                })
                .collect();
            format!(
                "{{\"total\":{},\"build\":{},\"user_caps\":{},\"entries\":[{}]}}",
                total,
                build,
                user_caps,
                entries.join(",")
            )
        }
        Err(e) => format!("{{\"error\":{}}}", string(&e.to_string())),
    };
    Ok(Value::Str(json))
}

/// Render witchy `source` to Markdown API documentation — the same output as the
/// `witchy doc` CLI: the module's public types, traits, trait implementations,
/// and functions with their signatures and doc-comments. `name` titles the module
/// heading. Lets a registry generate browsable docs from a rune's stored source,
/// on either backend. This native
/// source-string entry point expands `comptime:` sources through the same
/// bounded evaluator used by the source-file linker. A parse/expansion-boundary
/// error is returned as an HTML comment rather than trapping.
pub fn doc(args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::Str(name), Value::Str(src)] = args else {
        return Err(type_error("compiler.doc expects (name, source) strings"));
    };
    let md = render_doc_result(name, src, "compiler.doc")
        .unwrap_or_else(|e| format!("<!-- doc error: {e} -->"));
    Ok(Value::Str(md))
}

/// Fallible docs as inspectable JSON for `compiler.try_doc`:
/// `{"ok":"markdown"}` or `{"error":"message"}`.
pub fn doc_result_json(args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::Str(name), Value::Str(src)] = args else {
        return Err(type_error(format!(
            "{} expects (name, source) strings",
            intrinsics::COMPILER_DOC_RESULT_JSON
        )));
    };
    let json = match render_doc_result(name, src, "compiler.try_doc") {
        Ok(md) => format!("{{\"ok\":{}}}", string(&md)),
        Err(e) => format!("{{\"error\":{}}}", string(&e)),
    };
    Ok(Value::Str(json))
}

/// Compare two witchy sources by capability footprint, as JSON:
/// `{"widened":bool,"added":[..],"removed":[..],"build_added":[..],
/// "build_removed":[..],"user_caps_added":[..],"user_caps_removed":[..]}` —
/// the rights-precise block-on-widening gate (the package manager's core
/// safety check), exposed to witchy. `{"error":".."}` if either source does
/// not parse.
pub fn diff(args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::Str(old_src), Value::Str(new_src)] = args else {
        return Err(type_error("compiler.diff expects (old_source, new_source) strings"));
    };
    let json = match (
        checked_source_only_module(old_src, "compiler.diff"),
        checked_source_only_module(new_src, "compiler.diff"),
    ) {
        (Ok(old), Ok(new)) => {
            let old_fp = witchy_caps::capabilities::analyze(&old);
            let new_fp = witchy_caps::capabilities::analyze(&new);
            let d = witchy_caps::capabilities::diff(&old_fp, &new_fp);
            let added = arr(d.added.iter().map(|(n, r)| witchy_caps::capabilities::show_cap(n, r)));
            let removed = arr(d.removed.iter().map(|(n, r)| witchy_caps::capabilities::show_cap(n, r)));
            let build_added = arr(d.build_added.iter().map(|(n, r)| witchy_caps::capabilities::show_cap(n, r)));
            let build_removed = arr(d.build_removed.iter().map(|(n, r)| witchy_caps::capabilities::show_cap(n, r)));
            let user_caps_added = arr(d.user_caps_added.iter().cloned());
            let user_caps_removed = arr(d.user_caps_removed.iter().cloned());
            format!(
                "{{\"widened\":{},\"added\":{},\"removed\":{},\"build_added\":{},\"build_removed\":{},\"user_caps_added\":{},\"user_caps_removed\":{}}}",
                d.widened(),
                added,
                removed,
                build_added,
                build_removed,
                user_caps_added,
                user_caps_removed
            )
        }
        (Err(e), _) | (_, Err(e)) => format!("{{\"error\":{}}}", string(&e.to_string())),
    };
    Ok(Value::Str(json))
}

fn parse_source_only_module(src: &str, op: &str) -> Result<Module, String> {
    let module = witchy_syntax::parser::parse_module(src).map_err(|e| e.to_string())?;
    if module.items.iter().any(|item| matches!(item, Item::Comptime(_))) {
        return Err(format!(
            "{op} does not support comptime source strings; use the source-file CLI path for expanded introspection"
        ));
    }
    Ok(module)
}

fn checked_source_only_module(src: &str, op: &str) -> Result<Module, String> {
    use std::collections::{HashSet, VecDeque};

    fn expand_source(
        name: &str,
        module: &mut Module,
        siblings: &[(String, Module)],
    ) -> Result<witchy_syntax::origin::OriginTable, String> {
        crate::comptime::expand_compile_time(name, module, siblings)
    }

    let entry = parse_source_only_module(src, op)?;
    let mut modules: Vec<(String, Module)> = vec![("main".to_string(), entry.clone())];
    let mut loaded: HashSet<String> = HashSet::from(["main".to_string()]);
    let mut queue: VecDeque<Module> = VecDeque::from([entry.clone()]);
    while let Some(module) = queue.pop_front() {
        for name in module.imports.clone() {
            if !loaded.insert(name.clone()) {
                continue;
            }
            let Some(source) = witchy_syntax::linker::std_source(&name) else {
                if let Some(suggestion) = witchy_syntax::linker::closest_std_module(&name) {
                    return Err(format!(
                        "unknown module `{name}` — did you mean `import {suggestion}`?"
                    ));
                }
                // Source-string introspection has no caller-provided module map.
                // Project tools such as pm feed it one file at a time, so a
                // non-std import is treated as a project-local module and keeps
                // the historical source-only footprint shape. Source-file CLI
                // gates own strict multi-module validation.
                return Ok(entry);
            };
            let parsed = parse_source_only_module(source, op)?;
            queue.push_back(parsed.clone());
            modules.push((name, parsed));
        }
    }

    witchy_types::pipeline::link_checked(modules, "main", expand_source)
        .map_err(|e| e.to_string())?;
    // The public source-string footprint is the submitted module's footprint,
    // not the linked program's transitive std footprint. Linking/type-checking
    // above is the validity gate; analyzing `entry` preserves the established
    // JSON shape and unqualified entry names (`load`, not `main.load`).
    Ok(entry)
}

fn render_doc_result(name: &str, src: &str, op: &str) -> Result<String, String> {
    parse_source_only_module(src, op).and_then(|module| witchy_syntax::doc::render_module(name, src, &module))
}

/// A JSON string literal (quoted, with `"` and `\` escaped).
fn string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A JSON array of strings.
fn arr(items: impl Iterator<Item = String>) -> String {
    let parts: Vec<String> = items.map(|s| string(&s)).collect();
    format!("[{}]", parts.join(","))
}
