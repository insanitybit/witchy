//! `witchy doc` — extract a module's public API into Markdown.
//!
//! Public types, traits, trait implementations, and functions are rendered from
//! the AST so their declarations match the code. Leading `//` doc-comment blocks
//! come from the source because comments are not retained in the AST. The
//! module's own top-of-file comment becomes the section description.

use std::fmt::Write;

use crate::ast::{
    Expr, Function, ImplDef, Item, MethodSig, Module, Param, TraitDef, Type, TypeDef, UnOp, Variant,
};
use crate::format::type_str;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssociatedFunctionDoc {
    /// Callable source spelling, for example `Net.tcp(host: String, port: Int) -> NetPolicy`.
    pub signature: String,
    pub docs: String,
}

/// Render Markdown documentation for one module (named `module_name`) from its
/// source. Errors only if the source does not parse.
pub fn render(module_name: &str, source: &str) -> Result<String, String> {
    let module = crate::parser::parse_module(source).map_err(|e| e.to_string())?;
    render_module(module_name, source, &module)
}

/// Find a public self-less inherent function by its owning type and source name.
/// This is the structured API-discovery path shared with editor hover; callers
/// never need to infer ownership from indentation or function-name collisions.
pub fn associated_function(
    source: &str,
    owner: &str,
    name: &str,
) -> Result<Option<AssociatedFunctionDoc>, String> {
    let module = crate::parser::parse_module(source).map_err(|e| e.to_string())?;
    let lines: Vec<&str> = source.lines().collect();
    for item in &module.items {
        let Item::Impl(im) = item else { continue };
        if im.trait_name.is_some() || im.type_name != owner {
            continue;
        }
        for method in &im.methods {
            if !method.public || method.name != name {
                continue;
            }
            let (signature, is_static) = inherent_signature(&im.type_name, method);
            if !is_static {
                continue;
            }
            let marker = format!(
                "pub {}fn {}(",
                fn_qualifier(method.pure, method.is_async, method.is_gen),
                method.name
            );
            return Ok(Some(AssociatedFunctionDoc {
                signature,
                docs: doc_above_indented(&lines, &marker),
            }));
        }
    }
    Ok(None)
}

/// Find a public inherent instance method (a `self` receiver) by its source
/// name, optionally restricted to one owning type. The signature is rendered in
/// its callable Type-qualified spelling (`String.repeat(n: Int) -> String`), so
/// editor hover reports a method under its owner rather than as a free function.
pub fn instance_method(
    source: &str,
    owner: Option<&str>,
    name: &str,
) -> Result<Option<AssociatedFunctionDoc>, String> {
    let module = crate::parser::parse_module(source).map_err(|e| e.to_string())?;
    let lines: Vec<&str> = source.lines().collect();
    for item in &module.items {
        let Item::Impl(im) = item else { continue };
        if im.trait_name.is_some() || owner.is_some_and(|o| im.type_name != o) {
            continue;
        }
        for method in &im.methods {
            if !method.public || method.name != name {
                continue;
            }
            let (signature, is_static) = inherent_signature(&im.type_name, method);
            if is_static {
                continue;
            }
            let marker = format!(
                "pub {}fn {}(",
                fn_qualifier(method.pure, method.is_async, method.is_gen),
                method.name
            );
            return Ok(Some(AssociatedFunctionDoc {
                signature,
                docs: doc_above_indented(&lines, &marker),
            }));
        }
    }
    Ok(None)
}

/// Render Markdown documentation for one module from an already-parsed AST.
/// Callers that run frontend expansion passes first use this so generated public
/// APIs are rendered through the same AST path as handwritten APIs.
pub fn render_module(module_name: &str, source: &str, module: &Module) -> Result<String, String> {
    let lines: Vec<&str> = source.lines().collect();

    let mut out = String::new();
    let _ = writeln!(out, "## `{module_name}`\n");
    let header = leading_comment(&lines);
    if !header.is_empty() {
        let _ = writeln!(out, "{header}\n");
    }

    let mut any = false;
    // Types first — they are the vocabulary the functions are written in. A
    // `capability` (RFC-0002/0038) or `sealed type` (RFC-0065) is rendered with
    // its own keyword rather than as an ordinary record `type` (BUG-138), mirroring
    // how `witchy fmt` distinguishes them.
    for item in &module.items {
        let Item::Type(t) = item else { continue };
        any = true;
        let head = type_decl(t);
        let _ = writeln!(out, "#### `{head}`\n");
        let doc = doc_above_type(&lines, t);
        if !doc.is_empty() {
            let _ = writeln!(out, "{doc}\n");
        }
        // A record capability lists its carried state as fields; a `from` brand
        // shows the underlying capability in the heading, so it needs no body. An
        // ordinary/sealed type lists its variants (or record fields).
        if t.is_capability {
            let v = &t.variants[0];
            for (n, ty) in v.field_names.iter().zip(&v.fields) {
                let _ = writeln!(out, "- `{n}: {}`", type_str(ty));
            }
        } else {
            for v in &t.variants {
                let _ = writeln!(out, "- `{}`", variant_str(v));
            }
        }
        let _ = writeln!(out);
    }
    // Type aliases — `type Id = Int` / `type Pair(a) = (a, a)` — are part of
    // the vocabulary too (BUG-170).
    for item in &module.items {
        let Item::TypeAlias { name, params, ty } = item else { continue };
        any = true;
        let head = if params.is_empty() {
            name.clone()
        } else {
            format!("{name}({})", params.join(", "))
        };
        let _ = writeln!(out, "#### `type {head} = {}`\n", type_str(ty));
        let doc = doc_above(&lines, &format!("type {head} ="));
        if !doc.is_empty() {
            let _ = writeln!(out, "{doc}\n");
        }
    }
    // Traits — the interface vocabulary (BUG-073). Traits carry no visibility
    // gate; a module-level `trait` is public API. Render the header (name, type
    // parameters, supertraits) and each method signature.
    for item in &module.items {
        let Item::Trait(t) = item else { continue };
        any = true;
        let _ = writeln!(out, "#### `{}`\n", trait_header(t));
        let doc = doc_above(&lines, &format!("trait {}", t.name));
        if !doc.is_empty() {
            let _ = writeln!(out, "{doc}\n");
        }
        for m in &t.methods {
            let default = if m.default.is_some() { " _(default)_" } else { "" };
            let _ = writeln!(out, "- `{}`{default}", method_sig_str(m));
        }
        let _ = writeln!(out);
    }
    for item in &module.items {
        let Item::Function(f) = item else { continue };
        if !f.public {
            continue;
        }
        any = true;
        let sig = format!("{}{}", fn_qualifier(f.pure, f.is_async, f.is_gen), signature(&f.name, &f.type_params, &f.params, &f.ret, &f.bounds));
        let dynamic = f.attributes.iter().any(|attribute| attribute == "dynamic");
        let sig = if dynamic { format!("@dynamic {sig}") } else { sig };
        let _ = writeln!(out, "#### `{sig}`\n");
        if dynamic {
            let _ = writeln!(
                out,
                "**Dynamic dispatch:** registered for closed-world discovery and checked invocation through `dynamic.methods` and `dynamic.call`.\n",
            );
        }
        // The source line carries the same `async`/`gen` qualifier before `fn`,
        // so the doc-comment marker must reconstruct it to find the block.
        let marker = format!("pub {}fn {}(", fn_qualifier(f.pure, f.is_async, f.is_gen), f.name);
        let doc = doc_above_top_level(&lines, &marker);
        if !doc.is_empty() {
            let _ = writeln!(out, "{doc}\n");
        }
    }
    // Inherent-impl associated (self-less) functions — `Net.tcp(…)` (RFC-0057).
    // These are namespaced under a type, not free functions, so render them
    // qualified: `Type.name(...)`.
    for item in &module.items {
        let Item::Impl(im) = item else { continue };
        if im.trait_name.is_some() {
            continue;
        }
        for m in &im.methods {
            if !m.public {
                continue;
            }
            let (sig, _) = inherent_signature(&im.type_name, m);
            any = true;
            let _ = writeln!(out, "#### `{sig}`\n");
            let marker = format!("pub {}fn {}(", fn_qualifier(m.pure, m.is_async, m.is_gen), m.name);
            let doc = {
                let method_doc = doc_above_indented(&lines, &marker);
                if method_doc.is_empty() {
                    doc_above_top_level(&lines, &marker)
                } else {
                    method_doc
                }
            };
            if !doc.is_empty() {
                let _ = writeln!(out, "{doc}\n");
            }
        }
    }
    // A trait's usable surface includes the set of types implementing it. Keep
    // that inventory after functions/inherent methods so their `####` headings
    // do not become children of this `###` section (BUG-123).
    let mut wrote_trait_impl_heading = false;
    for item in &module.items {
        let Item::Impl(im) = item else { continue };
        if im.trait_name.is_none() {
            continue;
        }
        if !wrote_trait_impl_heading {
            any = true;
            wrote_trait_impl_heading = true;
            let _ = writeln!(out, "### Trait implementations\n");
        }
        let head = impl_header(im);
        let _ = writeln!(out, "#### `{head}`\n");
        let doc = doc_above_impl(&lines, &head);
        if !doc.is_empty() {
            let _ = writeln!(out, "{doc}\n");
        }
        for method in &im.methods {
            let _ = writeln!(
                out,
                "- `{}`",
                signature(&method.name, &method.type_params, &method.params, &method.ret, &method.bounds),
            );
        }
        if !im.methods.is_empty() {
            let _ = writeln!(out);
        }
    }
    if !any {
        let _ = writeln!(out, "_No public API._\n");
    }
    Ok(out)
}

/// A type's source-faithful declaration head, without the trailing colon.
fn type_decl(t: &TypeDef) -> String {
    let obligation = if t.must_consume { "must " } else { "" };
    if t.is_capability {
        let kw = if t.grantable { "grantable capability" } else { "capability" };
        let v = &t.variants[0];
        // `capability X from U` (RFC-0002): a sealed brand — `field_names` is empty
        // and the single variant's field types ARE the underlying capabilities.
        // Show them in the heading; the source line has no trailing `:`.
        if v.field_names.is_empty() && !v.fields.is_empty() {
            let from = if v.fields.len() == 1 {
                type_str(&v.fields[0])
            } else {
                format!("({})", v.fields.iter().map(type_str).collect::<Vec<_>>().join(", "))
            };
            return format!("{obligation}{kw} {} from {from}", t.name);
        }
        // `capability X:` — a record capability carrying named state.
        return format!("{obligation}{kw} {}", t.name);
    }

    let mut head = if t.sealed {
        format!("{obligation}sealed type {}", t.name)
    } else {
        format!("{obligation}type {}", t.name)
    };
    if !t.params.is_empty() {
        head.push_str(&format!("({})", t.params.join(", ")));
    }
    if t.packed {
        head.push_str(" packed");
    }
    if !t.derives.is_empty() {
        head.push_str(&format!(" derive({})", t.derives.join(", ")));
    }
    head
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

fn signature(name: &str, type_params: &[String], params: &[Param], ret: &Option<Type>, bounds: &[(String, String, Vec<Type>)]) -> String {
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
    let generic = if type_params.is_empty() { String::new() } else { format!("[{}]", type_params.join(", ")) };
    format!("fn {name}{generic}({}){r}{w}", ps.join(", "))
}

/// Render one public inherent method as its callable surface spelling. A
/// self-less method is static (`Type.name(...)`); an instance method drops
/// `self` and reads as `Type.name(...)` in generated API documentation.
fn inherent_signature(owner: &str, method: &Function) -> (String, bool) {
    let is_static = method.params.first().is_none_or(|param| param.name != "self");
    let params: &[Param] = if is_static { &method.params } else { &method.params[1..] };
    let signature = signature(&method.name, &method.type_params, params, &method.ret, &method.bounds);
    let signature = signature
        .strip_prefix("fn ")
        .map(|rest| {
            format!(
                "{}{}.{rest}",
                fn_qualifier(method.pure, method.is_async, method.is_gen),
                owner
            )
        })
        .unwrap_or(signature);
    (signature, is_static)
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
    // (RFC-0056) A closed-constant default (`punct: String = "!"`) is part of the
    // signature's meaning — render it, matching the declaration (BUG-207).
    let def = p
        .default
        .as_ref()
        .map(|e| format!(" = {}", default_str(e)))
        .unwrap_or_default();
    match &p.ty {
        // An `impl Trait` param is stored desugared (a fresh `impltrait_N` type
        // var plus a bound); render it back to the surface `impl Trait`.
        Some(Type::Named(v, args)) if args.is_empty() && is_impl_var(v) => {
            let trait_name = bounds
                .iter()
                .find(|(bv, _, _)| bv == v)
                .map(|(_, t, _)| t.as_str())
                .unwrap_or("?");
            format!("{conv}{}: impl {trait_name}{def}", p.name)
        }
        Some(t) => format!("{conv}{}: {}{def}", p.name, type_str(t)),
        None => format!("{conv}{}{def}", p.name),
    }
}

/// Render a closed-constant parameter default (RFC-0056) — a literal, `None`,
/// `true`/`false`, `[]`/`()`, a constructor of constants, or a unary application
/// thereof (`-1`) — back to its surface form (BUG-207). Anything outside that set
/// can't be a default (the parser rejects it), so a bare fallback suffices.
fn default_str(e: &Expr) -> String {
    match e {
        Expr::Int(n) => n.to_string(),
        Expr::Duration(ms) => format!("{ms}ms"),
        Expr::Float(x) => {
            let t = x.to_string();
            if t.contains('.') || t.contains('e') || t.contains("inf") || t.contains("NaN") {
                t
            } else {
                format!("{t}.0")
            }
        }
        Expr::Str(v) => str_lit(v),
        Expr::Bool(b) => b.to_string(),
        Expr::List(xs) => format!("[{}]", xs.iter().map(default_str).collect::<Vec<_>>().join(", ")),
        Expr::Tuple(xs) => format!("({})", xs.iter().map(default_str).collect::<Vec<_>>().join(", ")),
        Expr::Ctor { name, args } if args.is_empty() => name.clone(),
        Expr::Ctor { name, args } => {
            format!("{name}({})", args.iter().map(default_str).collect::<Vec<_>>().join(", "))
        }
        Expr::Unary { op, expr } => format!("{}{}", unop_str(*op), default_str(expr)),
        _ => "…".to_string(),
    }
}

/// The prefix form of a unary operator, for rendering a defaulted `-1`.
fn unop_str(op: UnOp) -> &'static str {
    match op {
        UnOp::Neg => "-",
        UnOp::Not => "!",
        UnOp::BitNot => "~",
        UnOp::Borrow => "&",
        UnOp::BorrowMut => panic!("cannot document a migration-only mutable borrow as canonical source"),
        UnOp::Deref => panic!("cannot document a migration-only dereference as canonical source"),
        UnOp::Move => "move ",
        UnOp::Await => "await ",
    }
}

/// A witchy string literal, escaping the characters `witchy fmt` escapes, so the
/// rendered default round-trips.
fn str_lit(v: &str) -> String {
    let mut s = String::from("\"");
    for c in v.chars() {
        match c {
            '"' => s.push_str("\\\""),
            '\\' => s.push_str("\\\\"),
            '\n' => s.push_str("\\n"),
            '\t' => s.push_str("\\t"),
            '\r' => s.push_str("\\r"),
            '\0' => s.push_str("\\0"),
            '$' => s.push_str("\\$"),
            _ => s.push(c),
        }
    }
    s.push('"');
    s
}

/// The `async`/`gen` prefix a function or method carries before `fn`, matching
/// `witchy fmt`'s order (`async gen fn`). Empty for a plain function (BUG-167).
fn fn_qualifier(pure: bool, is_async: bool, is_gen: bool) -> String {
    let mut q = String::new();
    if pure {
        q.push_str("pure ");
    }
    if is_async {
        q.push_str("async ");
    }
    if is_gen {
        q.push_str("gen ");
    }
    q
}

/// A trait's declaration head: `trait Name`, plus type parameters
/// (`trait From(a)`) and supertraits (`trait Ord: Eq + PartialOrd`), matching the
/// source form (BUG-073).
fn trait_header(t: &TraitDef) -> String {
    let mut h = format!("trait {}", t.name);
    if !t.typarams.is_empty() {
        h.push_str(&format!("({})", t.typarams.join(", ")));
    }
    if !t.supertraits.is_empty() {
        h.push_str(&format!(": {}", t.supertraits.join(" + ")));
    }
    h
}

/// A source-faithful trait implementation head, without the trailing colon.
fn impl_header(im: &ImplDef) -> String {
    let trait_name = im.trait_name.as_deref().expect("trait impl has a trait name");
    let target = if im
        .type_name
        .strip_prefix("Tuple")
        .and_then(|arity| arity.parse::<usize>().ok())
        .is_some()
    {
        Type::Tuple(im.target_args.clone())
    } else {
        Type::Named(im.type_name.clone(), im.target_args.clone())
    };
    let mut head = format!(
        "impl {} for {}",
        bound_trait_str(trait_name, &im.trait_args),
        type_str(&target),
    );
    if !im.bounds.is_empty() {
        let bounds: Vec<String> = im
            .bounds
            .iter()
            .map(|(var, trait_name, args)| {
                format!("{var}: {}", bound_trait_str(trait_name, args))
            })
            .collect();
        head.push_str(&format!(" where {}", bounds.join(", ")));
    }
    head
}

/// A trait method signature (`fn from(value: a) -> Self`). A trait method has no
/// `where` bounds of its own here, so render with an empty bound set (BUG-073).
fn method_sig_str(m: &MethodSig) -> String {
    format!(
        "{}{}",
        if m.pure { "pure " } else { "" },
        signature(&m.name, &m.type_params, &m.params, &m.ret, &[]),
    )
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
    comment_before(lines, i)
}

/// Implementation sections often use comments such as
/// `// --- primitive impls ---` as source navigation. They are not API prose.
fn doc_above_impl(lines: &[&str], marker: &str) -> String {
    let doc = doc_above(lines, marker);
    let trimmed = doc.trim();
    if trimmed.starts_with("---") && trimmed.ends_with("---") {
        String::new()
    } else {
        doc
    }
}

/// Like [`doc_above`], but only matches a declaration that starts at column 0.
/// Top-level functions and inherent methods can share the same surface name; a
/// method must not steal the module function's doc-comment.
fn doc_above_top_level(lines: &[&str], marker: &str) -> String {
    let Some(i) = lines.iter().position(|l| l.starts_with(marker)) else {
        return String::new();
    };
    comment_before(lines, i)
}

/// Find a type declaration by keyword + exact name boundary. The suffix varies
/// across `type T:`, `type T(a):`, `type T packed:`, and `type T derive(...):`.
fn doc_above_type(lines: &[&str], t: &TypeDef) -> String {
    let keyword = if t.is_capability {
        if t.grantable { "grantable capability" } else { "capability" }
    } else if t.sealed {
        "sealed type"
    } else {
        "type"
    };
    let obligation = if t.must_consume { "must " } else { "" };
    let prefix = format!("{obligation}{keyword} {}", t.name);
    let Some(i) = lines.iter().position(|line| {
        line.strip_prefix(&prefix).is_some_and(|rest| {
            rest.as_bytes()
                .first()
                .is_some_and(|byte| matches!(*byte, b':' | b'(') || byte.is_ascii_whitespace())
        })
    }) else {
        return String::new();
    };
    comment_before(lines, i)
}

/// Like [`doc_above`], but only matches an indented declaration. Used for
/// inherent methods, whose source line is nested under `impl`.
fn doc_above_indented(lines: &[&str], marker: &str) -> String {
    let Some(i) = lines
        .iter()
        .position(|l| !l.starts_with(marker) && l.trim_start().starts_with(marker))
    else {
        return String::new();
    };
    comment_before(lines, i)
}

fn comment_before(lines: &[&str], i: usize) -> String {
    let mut start = i;
    while start > 0 && lines[start - 1].trim_start().starts_with("//") {
        start -= 1;
    }
    // A comment block beginning at source line zero is the module description,
    // already rendered by `leading_comment`. Do not repeat it on the first item.
    if start == 0 {
        return String::new();
    }
    join_comment(lines[start..i].iter())
}

/// Strip `//` markers and join a comment block into flowing prose, treating a
/// bare `//` line as a paragraph break.
fn join_comment<'a>(it: impl Iterator<Item = &'a &'a str>) -> String {
    let mut paragraphs: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut in_separator = false;
    for l in it {
        let t = l.trim_start();
        match t.strip_prefix("//") {
            Some(rest) => {
                let rest = rest.strip_prefix(' ').unwrap_or(rest);
                let trimmed = rest.trim();
                if in_separator {
                    if trimmed.ends_with("---") {
                        in_separator = false;
                    }
                    continue;
                }
                if trimmed.starts_with("---") {
                    if !current.is_empty() {
                        paragraphs.push(std::mem::take(&mut current));
                    }
                    in_separator = !trimmed.ends_with("---");
                    continue;
                }
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
