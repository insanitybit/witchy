//! Source-facing checker diagnostics and suggestions.
use super::{Diagnostic, a, error};
use crate::compiler::{files, semantic_types::Field};

/// Edits (insertions, deletions, substitutions) from one name to another.
pub(super) fn distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut previous = row[0]; row[0] = i;
        for j in 1..=b.len() {
            let here = row[j];
            row[j] = (row[j] + 1).min(row[j - 1] + 1).min(previous + usize::from(a[i - 1] != b[j - 1]));
            previous = here;
        }
    }
    row[b.len()]
}

/// A field the record does not have: the nearest it does, or all of a small record's.
pub(super) fn unknown_field(fields: &[Field], written: &str) -> Diagnostic {
    let mut near: Vec<(usize, &str)> = fields.iter().map(|f| (distance(&f.name, written), f.name.as_str()))
        .filter(|(d, _)| *d <= 2.max(written.len() / 4)).collect();
    near.sort();
    let named = |names: Vec<&str>| names.iter().map(|n| format!("`{n}`")).collect::<Vec<_>>().join(", ");
    let message = if !near.is_empty() {
        format!("unknown record field: the nearest it has is {}", named(near.into_iter().take(2).map(|(_, n)| n).collect()))
    } else if !fields.is_empty() && fields.len() <= 6 {
        format!("unknown record field: it has {}", named(fields.iter().map(|f| f.name.as_str()).collect()))
    } else {
        return error("unknown record field");
    };
    error(message)
}

/// A local named twice in one function: no name is bound again while it is in scope, so the
/// message names it and says what to do (it said only "duplicate or shadowed local").
pub(super) fn shadowed(name: &str, first: Option<usize>) -> String {
    let shown = name.split('#').next().unwrap_or(name);
    let place = first.map(|at| format!(" (bound at ⟪at {at}⟫)")).unwrap_or_default();
    format!("duplicate or shadowed local: `{shown}` is already a name here{place}, and a name is not bound again while it is in scope; give this one a name of its own")
}

/// A use-after-move refusal from `Flow` (which knows slots, not names), with the value's name.
pub(super) fn moved_named(mut d: Diagnostic, name: &str, moved: Option<usize>) -> Diagnostic {
    if d.message.starts_with("use of a moved value") && !d.message.contains('`') {
        let why = d.message.split_once(": ").map(|(_, w)| w).unwrap_or("it was moved away");
        let why = why.strip_prefix("it ").unwrap_or(why);
        // Where (the latest move of it; on some paths only, one of them).
        let place = match moved { Some(at) if why.contains("some path") => format!(" (a move at ⟪at {at}⟫)"), Some(at) => format!(" (moved at ⟪at {at}⟫)"), None => String::new() };
        d.message = format!("use of a moved value: `{name}` {why}{place}; use it before the move, or move a copy of what is needed").into();
    }
    d
}

/// Why there is no `main`, as far as can be told: a misspelling of it, or a file of functions
/// for others to import compiled on its own (its own functions are the last module's: imports
/// are read in before a file's body).
pub(super) fn missing_main(program: &a::Program) -> std::borrow::Cow<'static, str> {
    let functions: Vec<&a::Function> = program.declarations.iter().filter_map(|d| match d { a::Declaration::Function(f) => Some(f), _ => None }).collect();
    // Library functions are public; a program's own misspelt `main` is not (`min` is one edit away).
    if let Some(near) = functions.iter().find(|f| !f.public && f.name != "main" && distance(&f.name, "main") <= 2) {
        return format!("missing main function: a program starts at `fn main(...)`; the nearest declared is `{}`", near.name).into();
    }
    let own = functions.iter().map(|f| f.module).max();
    let offered: Vec<&str> = functions.iter().filter(|f| Some(f.module) == own && f.public && !f.name.contains(['.', '#']) && !f.name.starts_with("Witchy")).map(|f| f.name.as_str()).take(3).collect();
    if !offered.is_empty() {
        let listed = offered.iter().map(|n| format!("`{n}`")).collect::<Vec<_>>().join(", ");
        return format!("missing main function: this file offers {listed} for others to import and has no `fn main(...)`: it is a library, which `witchyc build` does not build by itself. Compile the program that imports it; a package is built and checked with `grimoire build` (and `grimoire test`) in its directory. Or add a `main`").into();
    }
    "missing main function: a program starts at `fn main(...)`, which is given the capabilities it asks for (`console: Console<Write>`, ...)".into()
}

/// `dir.write_text(..)` on a `Dir<Read>`: the method is the same type's with `Write`, so the
/// value was granted for reading. Said before any hint about names or imports.
pub(super) fn read_only(shown: &str, method: &str) -> Option<String> {
    let base = shown.strip_suffix("<Read>")?;
    if !files::standard_impl_has(&format!("{base}<Write>"), method) { return None; }
    Some(format!("`{method}` writes, and this `{base}` was granted read-only (`{base}<Read>`): `{method}` is a method of `{base}<Write>`. Take a `{base}<Write>` where this value comes from (for one of `main`'s parameters, the parameter's type and the launcher's grant of it)"))
}
