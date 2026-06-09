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
        let _ = writeln!(out, "#### `{}`\n", signature(&f.name, &f.params, &f.ret));
        let doc = doc_above(&lines, &format!("pub fn {}(", f.name));
        if !doc.is_empty() {
            let _ = writeln!(out, "{doc}\n");
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

fn signature(name: &str, params: &[Param], ret: &Option<Type>) -> String {
    let ps: Vec<String> = params.iter().map(param_str).collect();
    let r = ret
        .as_ref()
        .map(|t| format!(" -> {}", type_str(t)))
        .unwrap_or_default();
    format!("fn {name}({}){r}", ps.join(", "))
}

fn param_str(p: &Param) -> String {
    match &p.ty {
        Some(t) => format!("{}: {}", p.name, type_str(t)),
        None => p.name.clone(),
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
        let src = "// greet — a tiny module.\n\n// Say hello to `name`.\npub fn hello(name: String) -> String:\n    \"hi \" <> name\n\nfn private_helper() -> Int:\n    0\n";
        let md = render("greet", src).unwrap();
        assert!(md.contains("## `greet`"), "module heading: {md}");
        assert!(md.contains("greet — a tiny module."), "module doc: {md}");
        assert!(md.contains("#### `fn hello(name: String) -> String`"), "signature: {md}");
        assert!(md.contains("Say hello to `name`."), "fn doc: {md}");
        assert!(!md.contains("private_helper"), "private fn must be omitted: {md}");
    }
}
