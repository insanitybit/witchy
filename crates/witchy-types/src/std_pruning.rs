//! Check only the standard library a program can reach.
//!
//! Every compile links the whole prelude (list, string, dict, math, option,
//! result, show, ... about 2,300 lines) and, before this pass, type-checked,
//! annotated, access-verified and monomorphized ALL of it: a profile of a
//! 2-line `witchy check` put 40% of its time in body checking, 27% in
//! `annotate_checked`, 20% in mono and 17% in access facts, while the
//! reachability judgment (`canonical_scope`) that finally drops unused std
//! code cost 0.5% and ran after all of it (2026-09-23).
//!
//! `prune` removes standard-library free functions and inherent methods that
//! no retained item can name, before the body check. It is conservative by
//! construction rather than by an enumeration of AST forms: an item's
//! references are every string in its `Debug` rendering, so a name that
//! appears anywhere in a reachable item (a call, a method call, a path, even
//! a string literal) keeps every candidate with that name. Trait impls,
//! types, traits, user code and the `show` module are always retained.
//!
//! Pruning happens only inside `with_pruning`, which the production
//! canonical route (`pipeline::link_checked_source_bundle_with`) uses: when
//! anything in that route fails after a pruned check -- the check itself or a
//! later canonical stage that synthesizes a call by name (contextual `?`
//! rewrites to `option.ok_or` / `result.map_err` after type checking) -- the
//! whole route runs again unpruned. A pruned failure is never reported, so
//! every rejection and its diagnostic are the unpruned compiler's.
//! `SYNTHESIZED_ROOTS` only spares the common forms that second run.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};

use witchy_syntax::ast::{Item, Module};

/// Modules whose presence disables pruning: they reflect over, or compile,
/// arbitrary program items at run time.
const WHOLE_PROGRAM_MODULES: &[&str] = &["dynamic", "reflect", "vm", "meta", "compiler"];

/// Names the compiler introduces AFTER type checking (canonical preparation
/// and trait lowering), which no source item names. Missing one only costs
/// the unpruned retry; listing it keeps the common forms single-pass.
const SYNTHESIZED_ROOTS: &[&str] = &[
    "option.ok_or", "ok_or", "result.map_err", "map_err", "task.and_then",
    "task.and_then_step", "and_then", "and_then_step", "set.to_list", "to_list",
    "dict.keys", "dict.values", "dict.pairs", "keys", "values", "pairs",
    "list.length", "length",
];

thread_local! {
    static ENABLED: Cell<bool> = const { Cell::new(false) };
    static PRUNED: Cell<bool> = const { Cell::new(false) };
    static LINK_DEPTH: Cell<usize> = const { Cell::new(0) };
    /// The std functions the current pruned check removed.
    static REMOVED: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
}

/// Restores the thread's pruning state even if the body panics.
struct Restore {
    enabled: bool,
    pruned: bool,
    removed: HashSet<String>,
}

impl Drop for Restore {
    fn drop(&mut self) {
        ENABLED.set(self.enabled);
        PRUNED.set(self.pruned);
        REMOVED.set(std::mem::take(&mut self.removed));
    }
}

/// Run `body` with pruning allowed (`enabled`) or not; also returns whether
/// any check inside actually pruned.
pub fn with_pruning<T>(enabled: bool, body: impl FnOnce() -> T) -> (T, bool) {
    let restore = Restore {
        enabled: ENABLED.replace(enabled),
        pruned: PRUNED.replace(false),
        removed: REMOVED.take(),
    };
    let result = body();
    let pruned = PRUNED.get();
    drop(restore);
    (result, pruned)
}

/// Whether the current pruned check removed the std function `name`. A
/// check that silently tolerates a missing callee (the access check skips
/// conventions it cannot find) must treat this as an error instead, so the
/// route reruns unpruned rather than accepting what the full std rejects.
pub fn removed(name: &str) -> bool {
    REMOVED.with_borrow(|removed| removed.contains(name))
}

/// Prune outside any checked route (the test-only raw link, whose module a
/// caller then checks with no unpruned rerun). The removals stay recorded on
/// this thread afterwards, so that later check still fails closed on a
/// removed callee instead of accepting what the full std rejects.
pub fn prune_detached(module: &Module, std_impls: &HashSet<(String, Vec<String>)>) -> Option<Module> {
    let (pruned, removed) = with_pruning(true, || (prune(module, std_impls), REMOVED.take())).0;
    REMOVED.with_borrow_mut(|recorded| recorded.extend(removed));
    pruned
}

/// Held for the duration of one checked link. Only the OUTERMOST link on a
/// thread may prune: a link nested inside the production route (comptime
/// reaching the compiler) turns its errors into values, so the route would
/// never see them and never rerun unpruned.
pub struct LinkScope;

pub fn enter_link() -> LinkScope {
    LINK_DEPTH.set(LINK_DEPTH.get() + 1);
    LinkScope
}

impl Drop for LinkScope {
    fn drop(&mut self) {
        LINK_DEPTH.set(LINK_DEPTH.get() - 1);
    }
}

fn std_module_of(name: &str) -> Option<&str> {
    let (module, rest) = name.split_once('.')?;
    (!rest.is_empty() && witchy_syntax::linker::STD_MODULES.contains(&module)).then_some(module)
}

/// Every string in `rendering` (a `Debug` rendering): the names an item can
/// refer to, over-approximated.
fn strings_in(rendering: &str, into: &mut HashSet<String>) {
    let bytes = rendering.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'"' {
            index += 1;
            continue;
        }
        let mut end = index + 1;
        let mut text = String::new();
        while end < bytes.len() && bytes[end] != b'"' {
            if bytes[end] == b'\\' && end + 1 < bytes.len() {
                end += 1;
            }
            text.push(bytes[end] as char);
            end += 1;
        }
        // A lowered or mangled reference names a method by its last
        // segment: `List__sum`, `Show__Int__show`, `list.sum`.
        if let Some((_, method)) = text.rsplit_once("__") {
            into.insert(method.to_string());
        }
        if let Some((_, member)) = text.rsplit_once('.') {
            into.insert(member.to_string());
        }
        into.insert(text);
        index = end + 1;
    }
}

/// A candidate for removal: (item index, method index within an inherent
/// impl, or None for a free function).
type Candidate = (usize, Option<usize>);

/// `module` with the unreachable standard-library functions and inherent
/// methods removed, or None when pruning does not apply (nothing to remove,
/// or a whole-program module is linked). `std_impls` identifies inherent
/// impls declared by standard-library modules, as (type name, method names).
pub fn prune(module: &Module, std_impls: &HashSet<(String, Vec<String>)>) -> Option<Module> {
    // The reference scan cannot see a std call the checker or a later pass
    // desugars implicitly (`d[i] = v` reaches a std insert whose `unique`
    // parameter makes `mode opt` reject an aliased owner). Every std name the
    // compiler spells in its own source is therefore a root
    // (`COMPILER_STD_NAMES`, kept complete by a drift test). A wrong
    // acceptance has no fallback, so `WITCHY_PRUNE_STD=off` stays as the
    // kill switch.
    if !ENABLED.get()
        || LINK_DEPTH.get() > 1
        || (!cfg!(test) && std::env::var_os("WITCHY_PRUNE_STD").is_some_and(|value| value == "off"))
    {
        return None;
    }
    let mut candidates_by_name: HashMap<String, Vec<Candidate>> = HashMap::new();
    let mut roots = Vec::new();
    for (index, item) in module.items.iter().enumerate() {
        match item {
            Item::Function(function) => match std_module_of(&function.name) {
                Some(owner) if WHOLE_PROGRAM_MODULES.contains(&owner) => return None,
                // `show`, and internal `mod.__*` helpers the compiler may name
                // through a constructed string, are always retained.
                Some("show") | None => roots.push(index),
                Some(_) if function.name.split_once('.').is_some_and(|(_, rest)| rest.starts_with("__")) => {
                    roots.push(index)
                }
                Some(_) => {
                    // A call may name it qualified (`list.map`) or, from
                    // inside its own module, by its bare suffix.
                    let bare = function.name.split_once('.').map(|(_, rest)| rest);
                    candidates_by_name.entry(function.name.clone()).or_default().push((index, None));
                    if let Some(bare) = bare {
                        candidates_by_name.entry(bare.to_string()).or_default().push((index, None));
                    }
                }
            },
            Item::Impl(implementation) if implementation.trait_name.is_none() => {
                let key = (
                    implementation.type_name.clone(),
                    implementation.methods.iter().map(|method| method.name.clone()).collect(),
                );
                if std_impls.contains(&key) {
                    for (method_index, method) in implementation.methods.iter().enumerate() {
                        candidates_by_name
                            .entry(method.name.clone())
                            .or_default()
                            .push((index, Some(method_index)));
                    }
                } else {
                    roots.push(index);
                }
            }
            _ => roots.push(index),
        }
    }
    if candidates_by_name.is_empty() {
        return None;
    }

    let mut kept: HashSet<Candidate> = HashSet::new();
    let mut seen_names: HashSet<String> = HashSet::new();
    let mut frontier: Vec<String> = Vec::new();
    let mut names = HashSet::new();
    for &index in &roots {
        strings_in(&format!("{:?}", module.items[index]), &mut names);
    }
    names.extend(SYNTHESIZED_ROOTS.iter().map(|name| name.to_string()));
    // Every std function the compiler itself spells (implicit desugarings
    // such as `d[i] = v`), generated from its sources. The bare suffix keeps
    // the inherent method of that name too, since methods are keyed bare.
    for name in crate::std_pruning_roots::COMPILER_STD_NAMES {
        names.insert(name.to_string());
        if let Some((_, bare)) = name.split_once('.') {
            names.insert(bare.to_string());
        }
    }
    frontier.extend(names.drain());
    while let Some(name) = frontier.pop() {
        if !seen_names.insert(name.clone()) {
            continue;
        }
        let Some(candidates) = candidates_by_name.get(&name) else { continue };
        for &candidate in candidates {
            if !kept.insert(candidate) {
                continue;
            }
            let rendering = match (candidate, &module.items[candidate.0]) {
                ((_, None), item) => format!("{item:?}"),
                ((_, Some(method)), Item::Impl(implementation)) => {
                    format!("{:?}", implementation.methods[method])
                }
                _ => continue,
            };
            strings_in(&rendering, &mut names);
            frontier.extend(names.drain().filter(|name| !seen_names.contains(name)));
        }
    }

    let mut pruned = module.clone();
    let mut removed = 0usize;
    let mut removed_names = HashSet::new();
    let lines_aligned = pruned.item_lines.len() == pruned.items.len();
    let old_lines = std::mem::take(&mut pruned.item_lines);
    let mut items = Vec::with_capacity(pruned.items.len());
    for (index, item) in pruned.items.drain(..).enumerate() {
        let before = items.len();
        match item {
            Item::Function(function)
                if std_module_of(&function.name).is_some_and(|owner| owner != "show")
                    && !function.name.split_once('.').is_some_and(|(_, rest)| rest.starts_with("__")) =>
            {
                if kept.contains(&(index, None)) {
                    items.push(Item::Function(function));
                } else {
                    removed += 1;
                    removed_names.insert(function.name);
                }
            }
            Item::Impl(mut implementation)
                if implementation.trait_name.is_none() && !roots.contains(&index) =>
            {
                let count = implementation.methods.len();
                let mut method_index = 0;
                implementation.methods.retain(|_| {
                    let keep = kept.contains(&(index, Some(method_index)));
                    method_index += 1;
                    keep
                });
                removed += count - implementation.methods.len();
                if !implementation.methods.is_empty() {
                    items.push(Item::Impl(implementation));
                }
            }
            other => items.push(other),
        }
        if lines_aligned && items.len() > before {
            pruned.item_lines.push(old_lines[index]);
        }
    }
    pruned.items = items;
    if removed == 0 {
        return None;
    }
    // Two whole-module judgments read the checked module and choose the
    // analysis every later stage uses, with no fallback after the route
    // returns Ok: whether it contains canonical views, and legacy loans.
    // Standard-library `&str` methods make the first true today; a pruned
    // module must never flip either, whatever the roots happen to keep.
    if crate::views::module_contains_canonical_view(&pruned)
        != crate::views::module_contains_canonical_view(module)
        || crate::views::module_contains_legacy_loan(&pruned)
            != crate::views::module_contains_legacy_loan(module)
    {
        return None;
    }
    PRUNED.set(true);
    REMOVED.set(removed_names);
    if std::env::var_os("WITCHY_PRUNE_STD_TRACE").is_some() {
        eprintln!("std_pruning: removed {removed} std functions/methods, kept {}", kept.len());
    }
    Some(pruned)
}

/// Append one line to `$WITCHY_PRUNE_STD_LOG`, if set: how often the pruned
/// check is trusted versus re-run in full.
pub fn log(line: String) {
    if let Some(path) = std::env::var_os("WITCHY_PRUNE_STD_LOG") {
        use std::io::Write as _;
        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(file, "{line}");
        }
    }
}
