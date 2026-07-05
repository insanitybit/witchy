//! `witchy doc` — extract a module's public API into Markdown.
//!
//! Each `pub fn` becomes a heading with its signature (rendered from the AST, so
//! it always matches the code) and its leading `//` doc-comment block (read from
//! the source, since comments are not retained in the AST). The module's own
//! top-of-file comment becomes the section description.

use std::fmt::Write;

use crate::ast::{Item, Param, Type, Variant};
use crate::format::type_str;

/// Render Markdown documentation for one module (named `module_name`) from its
/// source. Errors only if the source does not parse.
pub fn render(module_name: &str, source: &str) -> Result<String, String> {
    let module = crate::parser::parse_module(source).map_err(|e| e.to_string())?;
    let lines: Vec<&str> = source.lines().collect();

    let mut out = String::new();
    let _ = writeln!(out, "## `{module_name}`\n");
    let header = leading_comment(&lines);
    if !header.is_empty() {
        let _ = writeln!(out, "{header}\n");
    }

    let mut any = false;
    // Types first — they are the vocabulary the functions are written in.
    for item in &module.items {
        let Item::Type(t) = item else { continue };
        any = true;
        let _ = writeln!(out, "#### `type {}`\n", t.name);
        let doc = doc_above(&lines, &format!("type {}:", t.name));
        if !doc.is_empty() {
            let _ = writeln!(out, "{doc}\n");
        }
        for v in &t.variants {
            let _ = writeln!(out, "- `{}`", variant_str(v));
        }
        let _ = writeln!(out);
    }
    for item in &module.items {
        let Item::Function(f) = item else { continue };
        if !f.public {
            continue;
        }
        any = true;
        let _ = writeln!(out, "#### `{}`\n", signature(&f.name, &f.params, &f.ret, &f.bounds));
        let doc = doc_above(&lines, &format!("pub fn {}(", f.name));
        if !doc.is_empty() {
            let _ = writeln!(out, "{doc}\n");
        }
    }
    // Inherent-impl associated (self-less) functions — `Net.tcp(…)` (RFC-0057).
    // These are namespaced under a type, not free functions, so render them
    // qualified: `Type.name(...)`. Trait impls carry no new public surface.
    for item in &module.items {
        let Item::Impl(im) = item else { continue };
        if im.trait_name.is_some() {
            continue;
        }
        for m in &im.methods {
            if !m.public {
                continue;
            }
            // A `self`-less method is a static (`Type.name(…)`); an instance
            // method drops `self` and reads as `value.name(…)`.
            let is_static = m.params.first().is_none_or(|p| p.name != "self");
            let params: &[Param] = if is_static { &m.params } else { &m.params[1..] };
            let sig = signature(&m.name, params, &m.ret, &m.bounds);
            let sig = sig
                .strip_prefix("fn ")
                .map(|s| format!("{}.{s}", im.type_name))
                .unwrap_or(sig);
            any = true;
            let _ = writeln!(out, "#### `{sig}`\n");
            let doc = doc_above(&lines, &format!("pub fn {}(", m.name));
            if !doc.is_empty() {
                let _ = writeln!(out, "{doc}\n");
            }
        }
    }
    if !any {
        let _ = writeln!(out, "_No public API._\n");
    }
    Ok(out)
}

fn variant_str(v: &Variant) -> String {
    if !v.field_names.is_empty() {
        let fs: Vec<String> = v
            .field_names
            .iter()
            .zip(&v.fields)
            .map(|(n, t)| format!("{n}: {}", type_str(t)))
            .collect();
        format!("{} {{ {} }}", v.name, fs.join(", "))
    } else if v.fields.is_empty() {
        v.name.clone()
    } else {
        let fs: Vec<String> = v.fields.iter().map(type_str).collect();
        format!("{}({})", v.name, fs.join(", "))
    }
}

fn signature(name: &str, params: &[Param], ret: &Option<Type>, bounds: &[(String, String, Vec<Type>)]) -> String {
    let ps: Vec<String> = params.iter().map(|p| param_str(p, bounds)).collect();
    let r = ret
        .as_ref()
        .map(|t| format!(" -> {}", type_str(t)))
        .unwrap_or_default();
    // `where` bounds, minus the synthetic `impl Trait` ones (rendered inline on
    // the param), matching `witchy fmt`.
    let visible: Vec<&(String, String, Vec<Type>)> =
        bounds.iter().filter(|(v, _, _)| !is_impl_var(v)).collect();
    let w = if visible.is_empty() {
        String::new()
    } else {
        let bs: Vec<String> = visible
            .iter()
            .map(|(v, t, ta)| format!("{v}: {}", bound_trait_str(t, ta)))
            .collect();
        format!(" where {}", bs.join(", "))
    };
    format!("fn {name}({}){r}{w}", ps.join(", "))
}


/// Render a bound's trait with its type arguments: `Ord`, `FromIterator(a)`.
fn bound_trait_str(t: &str, args: &[Type]) -> String {
    if args.is_empty() {
        t.to_string()
    } else {
        let rendered: Vec<String> = args.iter().map(type_str).collect();
        format!("{t}({})", rendered.join(", "))
    }
}

fn is_impl_var(v: &str) -> bool {
    v.starts_with("impltrait_")
}

fn param_str(p: &Param, bounds: &[(String, String, Vec<Type>)]) -> String {
    // (RFC-0043) The parameter convention is part of the signature's meaning — a
    // `var` receiver marks a mutator (its statement form writes back), so render
    // it, matching `witchy fmt`. `let` (explicit borrow) prints its keyword too.
    let conv = match p.convention {
        crate::ast::Convention::Let => "",
        crate::ast::Convention::Borrow => "let ",
        crate::ast::Convention::Var => "var ",
        crate::ast::Convention::Own => "own ",
    };
    match &p.ty {
        // An `impl Trait` param is stored desugared (a fresh `impltrait_N` type
        // var plus a bound); render it back to the surface `impl Trait`.
        Some(Type::Named(v, args)) if args.is_empty() && is_impl_var(v) => {
            let trait_name = bounds
                .iter()
                .find(|(bv, _, _)| bv == v)
                .map(|(_, t, _)| t.as_str())
                .unwrap_or("?");
            format!("{conv}{}: impl {trait_name}", p.name)
        }
        Some(t) => format!("{conv}{}: {}", p.name, type_str(t)),
        None => format!("{conv}{}", p.name),
    }
}

/// The contiguous `//` block at the top of the file (the module description).
/// Stops at the first blank or non-comment line, so it doesn't swallow the doc
/// comment of the first function.
fn leading_comment(lines: &[&str]) -> String {
    let block: Vec<&&str> = lines
        .iter()
        .take_while(|l| l.trim_start().starts_with("//"))
        .collect();
    join_comment(block.into_iter())
}

/// The `//` block immediately above the first line starting with `marker`
/// (e.g. `pub fn name(` or `type Name:`), if any.
fn doc_above(lines: &[&str], marker: &str) -> String {
    let Some(i) = lines
        .iter()
        .position(|l| l.trim_start().starts_with(marker))
    else {
        return String::new();
    };
    let mut start = i;
    while start > 0 && lines[start - 1].trim_start().starts_with("//") {
        start -= 1;
    }
    join_comment(lines[start..i].iter())
}

/// Strip `//` markers and join a comment block into flowing prose, treating a
/// bare `//` line as a paragraph break.
fn join_comment<'a>(it: impl Iterator<Item = &'a &'a str>) -> String {
    let mut paragraphs: Vec<String> = Vec::new();
    let mut current = String::new();
    for l in it {
        let t = l.trim_start();
        match t.strip_prefix("//") {
            Some(rest) => {
                let rest = rest.strip_prefix(' ').unwrap_or(rest);
                if rest.trim().is_empty() {
                    if !current.is_empty() {
                        paragraphs.push(std::mem::take(&mut current));
                    }
                } else {
                    if !current.is_empty() {
                        current.push(' ');
                    }
                    current.push_str(rest.trim_end());
                }
            }
            None => {
                // A blank (non-comment) line also breaks a paragraph.
                if !current.is_empty() {
                    paragraphs.push(std::mem::take(&mut current));
                }
            }
        }
    }
    if !current.is_empty() {
        paragraphs.push(current);
    }
    paragraphs.join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::render;

    #[test]
    fn renders_signature_and_doc() {
        let src = "// greet — a tiny module.\n\n// Say hello to `name`.\npub fn hello(name: String) -> String:\n    \"hi \" + name\n\nfn private_helper() -> Int:\n    0\n";
        let md = render("greet", src).unwrap();
        assert!(md.contains("## `greet`"), "module heading: {md}");
        assert!(md.contains("greet — a tiny module."), "module doc: {md}");
        assert!(md.contains("#### `fn hello(name: String) -> String`"), "signature: {md}");
        assert!(md.contains("Say hello to `name`."), "fn doc: {md}");
        assert!(!md.contains("private_helper"), "private fn must be omitted: {md}");
    }
}
