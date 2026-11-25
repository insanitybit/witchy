//! Each file's types are its own: two files may each declare an `Entry`, private or `pub`.
//! Before parsing, a type, trait or capability name declared by several files becomes
//! `Entry@file` in each declaring file's tokens, so the rest of the compiler sees distinct
//! types. Elsewhere (no source can spell `@`):
//! - `inventory.Entry` is the `Entry` of the file imported as `inventory`;
//! - `from inventory import Entry` makes the bare `Entry` that one in this file;
//! - a bare `Entry` is the one declaring file this file imports, when there is exactly one
//!   (so a library's own uses never change meaning when a program adds a type of the same
//!   name); with two, it stays as written and is refused as unknown: name the one meant.
//!
//! A private type is never named from another file (`Program::private`), so only `pub`
//! ones are reached by these rules.
use super::{Kind, Token};
use crate::diagnostic::Diagnostic;
use std::collections::{BTreeMap, BTreeSet};

/// Names declared by several files, each with the files that declare it.
pub(super) type Shared = BTreeMap<String, BTreeSet<usize>>;

/// What renaming a file's tokens needs: the shared names, and what each file imports by name,
/// names bare by `from`, and reaches. Kept so the code a derive recipe writes is renamed as its
/// own file's code is (definition-site resolution of recipes, 2026-11-19).
#[derive(Default)]
pub(super) struct Renamer {
    shared: Shared,
    imports: BTreeMap<(usize, String), usize>,
    from: BTreeMap<(usize, String), String>,
    reaches: BTreeMap<usize, BTreeSet<usize>>,
    modules: Vec<usize>,
}

/// The tokens with each such name made its declaring file's own, and the names so treated.
pub(super) fn separate(tokens: Vec<Token>, modules: &[usize]) -> Result<(Vec<Token>, Shared, Renamer), Diagnostic> {
    let (renamer, mut tokens) = tables(tokens, modules)?;
    if renamer.shared.is_empty() { let shared = renamer.shared.clone(); return Ok((tokens, shared, renamer)); }
    renamer.rename(&mut tokens)?;
    let shared = renamer.shared.clone();
    Ok((tokens, shared, renamer))
}

fn tables(tokens: Vec<Token>, modules: &[usize]) -> Result<(Renamer, Vec<Token>), Diagnostic> {
    let file = |start: usize| modules.iter().filter(|first| **first <= start).count().saturating_sub(1);
    let mut declared: BTreeMap<String, BTreeSet<usize>> = BTreeMap::new();
    // What each file imports, by the name it uses (`import NAME FILE`, written by bundling),
    // and what its `from NAME import A, B` lines name bare.
    let mut imports: BTreeMap<(usize, String), usize> = BTreeMap::new();
    let mut from: BTreeMap<(usize, String), String> = BTreeMap::new();
    let mut depth = 0usize;
    let line_start = |tokens: &[Token], at: usize| at == 0 || matches!(tokens[at - 1].kind, Kind::Newline | Kind::Dedent);
    for at in 0..tokens.len() {
        match &tokens[at].kind {
            Kind::Indent => depth += 1,
            Kind::Dedent => depth = depth.saturating_sub(1),
            Kind::Name(word) if depth == 0 && word == "import" && line_start(&tokens, at) => {
                if let (Some(Kind::Name(name)), Some(Kind::Integer(target))) = (tokens.get(at + 1).map(|t| &t.kind), tokens.get(at + 2).map(|t| &t.kind)) {
                    // One name, one module: `parts/a/x.wy` and `parts/b/x.wy` would both be `x`.
                    if let Some(other) = imports.insert((file(tokens[at].span.start), name.clone()), *target as usize) && other != *target as usize {
                        return Err(Diagnostic::new("W0401", tokens[at].span.clone(), format!("two imports of this file are both named `{name}`: a module is named by its file's name, so rename one file")));
                    }
                }
            }
            Kind::Name(word) if depth == 0 && word == "from" && line_start(&tokens, at) => {
                let Some(Kind::Name(module)) = tokens.get(at + 1).map(|t| &t.kind) else { continue };
                let here = file(tokens[at].span.start);
                let mut next = at + 3;
                while let Some(Kind::Name(name)) = tokens.get(next).map(|t| &t.kind) {
                    from.insert((here, name.clone()), module.clone());
                    if !matches!(tokens.get(next + 1).map(|t| &t.kind), Some(Kind::Symbol(","))) { break; }
                    next += 2;
                }
            }
            // `gen fn name` at the start of a line, after `pub` if it is there: its type.
            Kind::Name(word) if depth == 0 && word == "gen" && matches!(tokens.get(at + 1).map(|t| &t.kind), Some(Kind::Name(f)) if f == "fn") => {
                let before = if at > 0 && matches!(&tokens[at - 1].kind, Kind::Name(w) if w == "pub") { at - 1 } else { at };
                if !line_start(&tokens, before) { continue; }
                let Some(Kind::Name(function)) = tokens.get(at + 2).map(|t| &t.kind) else { continue };
                declared.entry(super::generator::type_name(function)).or_default().insert(file(tokens[at].span.start));
            }
            Kind::Name(word) if depth == 0 && matches!(word.as_str(), "type" | "trait" | "capability") => {
                // At the start of a line, after `pub`, `sealed` and `grantable` if they are there.
                let mut before = at;
                while before > 0 && matches!(&tokens[before - 1].kind, Kind::Name(w) if matches!(w.as_str(), "sealed" | "grantable" | "pub")) { before -= 1; }
                let Some(Kind::Name(name)) = tokens.get(at + 1).map(|t| &t.kind) else { continue };
                if !line_start(&tokens, before) { continue; }
                declared.entry(name.clone()).or_default().insert(file(tokens[at].span.start));
            }
            _ => {}
        }
    }
    let shared: Shared = declared.into_iter().filter(|(_, files)| files.len() > 1).collect();
    // What each file reaches through its imports, at any depth: a library file may use a type
    // its own import brings (`heap` uses `Vec` through `string`).
    let mut reaches: BTreeMap<usize, BTreeSet<usize>> = BTreeMap::new();
    let files_present: BTreeSet<usize> = imports.keys().map(|(from_file, _)| *from_file).collect();
    for start in files_present {
        let mut seen = BTreeSet::new();
        let mut pending: Vec<usize> = imports.iter().filter(|((from_file, _), _)| *from_file == start).map(|(_, target)| *target).collect();
        while let Some(next) = pending.pop() {
            if !seen.insert(next) { continue; }
            pending.extend(imports.iter().filter(|((from_file, _), _)| *from_file == next).map(|(_, target)| *target));
        }
        reaches.insert(start, seen);
    }
    Ok((Renamer { shared, imports, from, reaches, modules: modules.to_vec() }, tokens))
}

impl Renamer {
    /// Each shared name in `tokens` made the file's it means, each token read in the file its
    /// span lies in.
    pub(super) fn rename(&self, tokens: &mut [Token]) -> Result<(), Diagnostic> {
    let (shared, imports, from, reaches) = (&self.shared, &self.imports, &self.from, &self.reaches);
    if shared.is_empty() { return Ok(()); }
    let file = |start: usize| self.modules.iter().filter(|first| **first <= start).count().saturating_sub(1);
    for at in 0..tokens.len() {
        let here = file(tokens[at].span.start);
        let Kind::Name(name) = &tokens[at].kind else { continue };
        let Some(files) = shared.get(name.as_str()) else { continue };
        let qualified = at >= 2 && matches!(tokens[at - 1].kind, Kind::Symbol("."));
        let target = if qualified {
            // `module.Entry`: the file imported as `module`; `x.Entry` (a field) is no type.
            let Kind::Name(module) = &tokens[at - 2].kind else { continue };
            match imports.get(&(here, module.clone())) { Some(f) if files.contains(f) => *f, _ => continue }
        } else if files.contains(&here) {
            here
        } else if let Some(module) = from.get(&(here, name.clone())) {
            match imports.get(&(here, module.clone())) { Some(f) if files.contains(f) => *f, _ => continue }
        } else {
            // The declaring files this one imports directly; failing that, any it reaches.
            let direct: Vec<usize> = files.iter().copied().filter(|f| imports.iter().any(|((from_file, _), target)| *from_file == here && target == f)).collect();
            let reached: Vec<usize> = if direct.is_empty() { files.iter().copied().filter(|f| reaches.get(&here).is_some_and(|r| r.contains(f))).collect() } else { direct };
            match reached.as_slice() {
                [only] => *only,
                [] => continue,
                several => {
                    let names: Vec<String> = several.iter().filter_map(|f| imports.iter().find(|((from_file, _), target)| *from_file == here && *target == f).map(|((_, n), _)| format!("`{n}.{name}`"))).collect();
                    return Err(Diagnostic::new("W0401", tokens[at].span.clone(), format!("two imported modules offer `{name}`: write {}, or `from MODULE import {name}`", names.join(" or "))));
                }
            }
        };
        if let Kind::Name(name) = &mut tokens[at].kind { name.push_str(&format!("@{target}")); }
    }
    Ok(())
    }
}
