//! `grantable capability UiRoot:` (book: capabilities-narrowing.md, "grantable"): a sealed
//! capability holding policy data and no host authority, which `main` may take and the run
//! grants with values (`--grant=UiRoot.policy=7`).
//!
//! Desugared after parsing, so nothing later sees anything new:
//! - each grantable type gets `#grant_UiRoot(&#Grants) -> UiRoot`, in the type's own file
//!   (so it may build the sealed record), reading each field from the run by name;
//! - each `main` parameter of a grantable type becomes a `let` at the top of `main`'s body,
//!   built by that function, and `main` takes one `#Grants` handle instead.
//!
//! `#Grants` is a host capability no source can name; what it reads is data, never
//! authority. The checker requires a grantable type to hold no authority and its fields to
//! be numbers or Bools (check.rs).
use super::{lex, Kind, Parser};
use crate::{ast::{Declaration, Type}, diagnostic::Diagnostic};

const TARGET: &str = "WitchyGrantTarget";
const HANDLE_TYPE: &str = "WitchyGrants";
const HANDLE: &str = "witchy_grants";

/// The function that builds a grantable type from the run's grant.
pub(super) fn builder(name: &str) -> String { format!("#grant_{name}") }

fn renamed(text: &str, target: &str, calls: &[String], modules: &[usize]) -> Result<Parser, Diagnostic> {
    let mut tokens = lex(text)?;
    for token in &mut tokens {
        if let Kind::Name(name) = &mut token.kind {
            if name == TARGET { *name = target.to_string(); }
            else if name == HANDLE_TYPE { *name = "#Grants".into(); }
            else if name == HANDLE { *name = "#grants".into(); }
            else if let Some(index) = name.strip_prefix("witchy_grant_").and_then(|i| i.parse::<usize>().ok()) { *name = builder(&calls[index]); }
            else if name == "witchy_grant_builder" { *name = builder(target); }
        }
    }
    Ok(Parser::written(tokens, modules))
}

pub(super) fn desugar(declarations: &mut Vec<Declaration>, grantable: &[String], modules: &[usize]) -> Result<(), Diagnostic> {
    if grantable.is_empty() { return Ok(()); }
    let refuse = |message: String| Diagnostic::new("W0401", 0..0, message);
    // The run grants by name (`--grant=RateLimit.per_minute=3`) and `wasm-caps` reports by
    // name, so a grantable capability's name is one per program even though other types are
    // per file (found by an automated security review).
    let mut seen = std::collections::BTreeSet::new();
    for name in grantable {
        let shown = name.split('@').next().unwrap_or(name);
        if !seen.insert(shown) { return Err(refuse(format!("two files declare a grantable capability `{shown}`: the run grants it by name, so one program has one of each name"))); }
    }
    let mut built = Vec::new();
    for name in grantable {
        let Some((fields, module)) = declarations.iter().find_map(|d| match d {
            Declaration::Record { name: n, fields, module, .. } if n == name => Some((fields.clone(), *module)),
            _ => None,
        }) else { return Err(refuse(format!("grantable capability `{name}` has no fields"))) };
        // Each field by its name as the run grants it: `UiRoot.policy` (without a file suffix).
        let shown = name.split('@').next().unwrap_or(name);
        let mut text = format!("fn witchy_grant_builder({HANDLE}: &{HANDLE_TYPE}) -> {TARGET}:\n");
        let mut values = Vec::new();
        for (index, (field, ty)) in fields.iter().enumerate() {
            for byte in format!("{shown}.{field}").bytes() { text.push_str(&format!("    {HANDLE}.raw_name_byte({byte})\n")); }
            text.push_str(&format!("    let witchy_value{index} = {HANDLE}.raw_lookup()\n"));
            values.push(match ty { Type::Named(n, _) if n == "Bool" => format!("witchy_value{index} == 1"), _ => format!("witchy_value{index}") });
        }
        text.push_str(&format!("    return {TARGET}({})\n", values.join(", ")));
        let mut parser = renamed(&text, name, &[], modules)?;
        let Declaration::Function(mut f) = parser.declaration()? else { unreachable!("a function was written") };
        f.module = module;
        f.public = true;
        built.push(Declaration::Function(f));
    }
    declarations.extend(built);
    // `main`'s grantable parameters.
    let Some(main) = declarations.iter_mut().find_map(|d| match d { Declaration::Function(f) if f.name == "main" => Some(f), _ => None }) else { return Ok(()) };
    let mut taken: Vec<(String, String)> = Vec::new();
    let mut kept = Vec::new();
    for (parameter, ty) in std::mem::take(&mut main.parameters) {
        match &ty {
            Type::Named(n, arguments) if arguments.is_empty() && grantable.contains(n) => {
                if taken.iter().any(|(_, t)| t == n) { return Err(refuse(format!("main accepts only distinct capabilities: `{}` is asked for twice", n.split('@').next().unwrap_or(n)))); }
                taken.push((parameter, n.clone()));
            }
            _ => kept.push((parameter, ty)),
        }
    }
    if taken.is_empty() { main.parameters = kept; return Ok(()); }
    kept.push(("#grants".into(), Type::Named("#Grants".into(), Vec::new())));
    main.parameters = kept;
    let calls: Vec<String> = taken.iter().map(|(_, t)| t.clone()).collect();
    let mut text = format!("fn witchy_main({HANDLE}: {HANDLE_TYPE}):\n");
    for (index, (parameter, _)) in taken.iter().enumerate() { text.push_str(&format!("    let {parameter} = witchy_grant_{index}(&{HANDLE})\n")); }
    let mut parser = renamed(&text, "", &calls, modules)?;
    let Declaration::Function(prefix) = parser.declaration()? else { unreachable!("a function was written") };
    let mut body = prefix.body;
    body.append(&mut main.body);
    main.body = body;
    Ok(())
}
