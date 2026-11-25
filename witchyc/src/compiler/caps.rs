//! `witchyc wasm-caps program.wy`: a program's authority, from its source (book:
//! capabilities-authority.md, "Inspecting authority"). Authority enters only through
//! `main`'s parameters and moves only through values, and a capability cannot be a type
//! argument, so the capabilities a function can use are exactly those its parameters
//! hold: directly, through a reference, or inside a record or sum. Nothing here is
//! trusted by the build; it reads the checked program.
use super::{logical_body::{Pointee, Program, Ty}, semantic_types::Types};
use std::{borrow::Cow, collections::{BTreeMap, BTreeSet}};

fn name(ty: Ty) -> Option<&'static str> {
    match ty {
        Ty::Console => Some("Console<Write>"),
        Ty::Input => Some("Console<Read>"),
        Ty::Allocator => Some("Allocator"),
        // What reads a run's grants is data: the grantable type it builds is the authority shown.
        Ty::Host(super::host::Host::Grants) => None,
        Ty::Host(host) => Some(host.name()),
        _ => None,
    }
}

/// Every capability a value of this type holds, at any depth.
pub(super) fn held(types: &Types, ty: Ty, out: &mut BTreeSet<Cow<'static, str>>, seen: &mut BTreeSet<u32>, depth: usize) {
    // Past the limit, say so rather than report nothing.
    if depth > 256 { out.insert("(a type nested too deeply to read)".into()); return; }
    if let Some(found) = name(ty) { out.insert(found.into()); return; }
    // A library's own kind of authority, granted to `main` by the run (book: "user caps").
    if let Ty::Aggregate(_) = ty && let Some(record) = types.nominal(ty) && types.grantable.contains(record) {
        let shown = record.split('@').next().unwrap_or(record);
        out.insert(format!("{shown} (user capability)").into());
        return;
    }
    match ty {
        Ty::Borrow { pointee, .. } => if !matches!(pointee, Pointee::Param(..)) { held(types, pointee.ty(), out, seen, depth + 1) },
        Ty::Aggregate(id) => if seen.insert(id) { for field in types.get(ty).map(|a| a.fields.as_slice()).unwrap_or_default() { held(types, field.ty, out, seen, depth + 1); } },
        Ty::Sum(id) => if seen.insert(u32::MAX - id) { for arm in types.sum(ty).map(|s| s.arms.as_slice()).unwrap_or_default() { held(types, *arm, out, seen, depth + 1); } },
        // Refused by the type checker, and read here anyway.
        Ty::Buffer(id) => if let Some(buffer) = types.buffers().get(id as usize) { held(types, buffer.element, out, seen, depth + 1) },
        _ => {}
    }
}

/// What a parameter of this type holds, each capability named with the record or sum it is
/// held through, as the book reports a refined capability: `Console<Write> (in Logger)`.
fn described(types: &Types, ty: Ty) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    held(types, ty, &mut found, &mut BTreeSet::new(), 0);
    let value = match ty { Ty::Borrow { pointee, .. } if !matches!(pointee, Pointee::Param(..)) => pointee.ty(), other => other };
    let through = match value { Ty::Aggregate(_) | Ty::Sum(_) => types.nominal(value).filter(|n| *n != "Result"), _ => None }
        .map(|n| if n.starts_with("Closure#") { "a closure" } else { n });
    found.into_iter().map(|capability| match through {
        // A user capability is the record itself: `RateLimit (user capability)` says it all.
        Some(record) if capability.strip_prefix(record.split('@').next().unwrap_or(record)).is_some_and(|rest| rest == " (user capability)") => capability.into_owned(),
        Some(record) => format!("{capability} (in {record})"),
        None => capability.into_owned(),
    }).collect()
}

/// What each reachable function can use, by name (`main` included), and how many use nothing.
pub(super) struct Footprint { pub functions: BTreeMap<String, BTreeSet<String>>, pub pure: usize, pub reached: BTreeSet<u32>,
    /// The same by function index, every reached function with authority included, before any name is changed for display.
    pub by_index: BTreeMap<u32, BTreeSet<String>> }

impl Footprint {
    /// Every capability any reachable function holds, without the record it is held in.
    pub fn total(&self) -> BTreeSet<String> {
        self.functions.values().flatten().map(|c| c.split(" (in ").next().unwrap_or(c).split(" (through ").next().unwrap_or(c).to_string()).collect()
    }
}

/// Whether holding `wider` gives at least what holding `narrower` does: the same capability,
/// or a bare `Dir` or `Secret` for one of its one-right views.
fn covers(wider: &str, narrower: &str) -> bool {
    wider == narrower || (matches!(wider, "Dir" | "Secret") && narrower.strip_prefix(wider).is_some_and(|rest| rest.starts_with('<')))
}

/// How the authority of `new` differs from `old` (book: "The supply-chain payoff",
/// `caps-diff`), and whether `new` as a whole can do something `old` could not.
pub(super) fn diff(old: &Footprint, new: &Footprint) -> (String, bool) {
    let (old_total, new_total) = (old.total(), new.total());
    let gained: Vec<&String> = new_total.iter().filter(|c| !old_total.iter().any(|o| covers(o, c))).collect();
    let mut out = String::new();
    for capability in &gained { out.push_str(&format!("widened: the program now holds {capability}
")); }
    for capability in old_total.iter().filter(|c| !new_total.iter().any(|n| covers(n, c))) { out.push_str(&format!("narrowed: the program no longer holds {capability}
")); }
    let names: BTreeSet<&String> = old.functions.keys().chain(new.functions.keys()).collect();
    let empty = BTreeSet::new();
    for name in names {
        let (before, after) = (old.functions.get(name).unwrap_or(&empty), new.functions.get(name).unwrap_or(&empty));
        let added: Vec<&str> = after.difference(before).map(String::as_str).collect();
        let removed: Vec<&str> = before.difference(after).map(String::as_str).collect();
        if !added.is_empty() { out.push_str(&format!("{name}: + {}
", added.join(", "))); }
        if !removed.is_empty() { out.push_str(&format!("{name}: - {}
", removed.join(", "))); }
    }
    if out.is_empty() { out.push_str("no change in authority
"); }
    (out, !gained.is_empty())
}

pub(super) fn report(program: &Program, main_parameters: usize) -> String {
    let footprint = footprint(program, main_parameters);
    let mut lines = footprint.functions.iter().map(|(f, c)| (f.clone(), c.iter().cloned().collect::<Vec<_>>().join(", "))).collect::<BTreeMap<_, _>>();
    let pure = footprint.pure;
    let main = lines.remove("main").unwrap_or_default();
    let mut out = format!("main: {}\n", if main.is_empty() { "nothing" } else { &main });
    for (function, capabilities) in &lines { out.push_str(&format!("{function}: {capabilities}\n")); }
    out.push_str(&format!("{pure} reachable functions hold no capability\n"));
    let total = footprint.total();
    out.push_str(&format!("total: {}\n", if total.is_empty() { "nothing".to_string() } else { total.into_iter().collect::<Vec<_>>().join(", ") }));
    out
}

/// The same, by the file each function is written in: what the functions `main` reaches in that
/// file can use, and which of them perform memory operations that are not checked when they run
/// (each is in a function whose proof shows it safe; the compiler refuses one that is not). This
/// is what a program's author reviews of a package it imports: the authority the package can
/// reach is only what the program passes it, and the operations it is trusted for are listed.
/// One file's part of a program: what the functions `main` reaches there can use. Every file of
/// the bundle has one, with `functions: 0` where `main` reaches none.
pub(super) struct FileUse { pub file: String, pub functions: usize, pub authority: BTreeSet<String>, pub unchecked: Vec<String>, pub contracts: usize }

pub(super) fn file_uses(program: &Program, main_parameters: usize, files: &[String]) -> Vec<FileUse> {
    let footprint = footprint(program, main_parameters);
    let mut out = Vec::new();
    for (module, file) in files.iter().enumerate() {
        // A trait's own method has no body: what it is given to run is counted where that is written.
        let here = program.functions.iter().filter(|f| f.module == module as u32 && !f.required && footprint.reached.contains(&f.definition.0)).collect::<Vec<_>>();
        // A file with no function `main` reaches (constants, types) is listed all the same, using
        // nothing: it is part of the program, so it is held to a policy line and a draft pins it.
        // Left out, it could change with no line and no review.
        let authority = here.iter().flat_map(|f| footprint.by_index.get(&f.definition.0).into_iter().flatten())
            .map(|c| c.split(" (in ").next().unwrap_or(c).split(" (through ").next().unwrap_or(c).to_string()).collect::<BTreeSet<_>>();
        // Read proof-only storage operations and UTF-8 refinements from the typed body.
        let unchecked = here.iter().filter(|f| super::record_state::has_unchecked_operations(&f.body)).map(|f| f.name.clone()).collect::<Vec<_>>();
        let contracts = here.iter().filter(|f| f.contract.is_some()).count();
        out.push(FileUse { file: file.clone(), functions: here.len(), authority, unchecked, contracts });
    }
    out
}

pub(super) fn by_file(program: &Program, main_parameters: usize, files: &[String]) -> String {
    let mut out = String::new();
    for used in file_uses(program, main_parameters, files) {
        out.push_str(&format!("{}: {} reachable functions; can use {}\n", used.file, used.functions, if used.authority.is_empty() { "nothing".to_string() } else { used.authority.iter().cloned().collect::<Vec<_>>().join(", ") }));
        if used.contracts > 0 { out.push_str(&format!("  {} with a contract proved by Lean\n", used.contracts)); }
        if used.unchecked.is_empty() { out.push_str("  no unchecked memory operations\n"); }
        else { out.push_str(&format!("  unchecked memory operations, each shown safe by its function's proof: {}\n", used.unchecked.join(", "))); }
    }
    out
}

/// `wasm-caps --draft-policy`: a policy the program fits as it is, one line per file with exactly
/// what it uses, for its author to review, narrow and keep beside the program.
pub(super) fn draft_policy(uses: &[FileUse], digests: &[(String, String)]) -> Result<String, String> {
    let mut out = String::from("# Drafted from what each file uses now: review it, narrow it, keep it beside the program.\n");
    for used in uses {
        if used.file.contains(['#', ';', '\n', '\r']) || used.file.contains(": ") || used.file.ends_with(':') {
            return Err(format!("{}: a file name a policy line cannot name exactly", used.file));
        }
        let authority = if used.authority.is_empty() { "nothing".to_string() } else { used.authority.iter().cloned().collect::<Vec<_>>().join(", ") };
        let digest = digests.iter().find(|(name, _)| *name == used.file).map(|(_, digest)| format!(" ; digest {digest}")).unwrap_or_default();
        out.push_str(&format!("{}: {authority}{}{digest}\n", used.file, if used.unchecked.is_empty() { " ; no unchecked" } else { "" }));
    }
    Ok(out)
}

/// `wasm-caps --policy`: each file is held to the line of the policy that matches it most
/// closely (a line ending in `/` matches what is under it, any other line one path exactly): `prefix: Allocator, Console<Write>`, `prefix: nothing`, or `prefix: *` (anything), and
/// `; no unchecked` forbids unchecked memory operations there. A file no line covers is a
/// violation itself. Returns the violations, one line each; none means the program fits.
/// One line of a policy: the files it covers, what they may use (`None`: anything), whether
/// unchecked operations are forbidden there, and the digest of the code it was reviewed for.
struct Rule { prefix: String, allowed: Option<BTreeSet<String>>, no_unchecked: bool, digest: Option<String> }
/// A policy's lines. A digest is a `sha384:` pin (the `witchy-2` profile). A bare 64-digit one,
/// the SHA-256 pins written before it, is refused, never compared, unless `repinning`: a repin
/// replaces it with the pin of the code as it is now, after its review.
fn rules(policy: &str) -> Result<Vec<Rule>, String> { rules_of(policy, false) }
fn rules_of(policy: &str, repinning: bool) -> Result<Vec<Rule>, String> {
    let mut rules = Vec::new();
    for (number, line) in policy.lines().enumerate() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() { continue; }
        let Some((prefix, rest)) = line.rsplit_once(": ").or_else(|| line.strip_suffix(':').map(|p| (p, ""))) else { return Err(format!("policy line {}: `prefix: capabilities`", number + 1)); };
        let mut sections = rest.split(';');
        let allowed = sections.next().unwrap_or("").trim();
        let (mut no_unchecked, mut digest) = (false, None);
        for flag in sections.map(str::trim) {
            match flag.strip_prefix("digest ") {
                Some(pin) if super::proof_digest::is_pin(pin) && digest.is_none() => digest = Some(pin.to_string()),
                Some(hex) if hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) && digest.is_none() => {
                    if !repinning { return Err(format!("policy line {}: a SHA-256 digest, from before the witchy-2 profile: review the code the line covers, then `wasm-caps --repin` pins it as sha384:", number + 1)); }
                    digest = Some(hex.to_string());
                }
                _ if flag == "no unchecked" && !no_unchecked => no_unchecked = true,
                _ => return Err(format!("policy line {}: the flags are `; no unchecked` and `; digest sha384:<96 lowercase hex digits>`", number + 1)),
            }
        }
        let set: Option<BTreeSet<String>> = match allowed { "*" => None, "nothing" | "" => Some(BTreeSet::new()), list => Some(list.split(", ").map(|c| c.trim().to_string()).collect()) };
        rules.push(Rule { prefix: prefix.trim().to_string(), allowed: set, no_unchecked, digest });
    }
    Ok(rules)
}
/// The line a file is held to: of those that match it (a line ending in `/` covers what is under
/// it; any other line exactly one path, so a drafted `deps/config.wy` does not cover
/// `deps/config.wy.evil`: automated security review), the longest.
fn rule_for<'a>(rules: &'a [Rule], file: &str) -> Option<&'a Rule> {
    // `@name/` covers the whole package: its first file, named `@name`, and `@name/PATH`. Only a
    // package's own line does: `@name/sub/` covers what is under `sub/`, not a file named `sub`.
    let package_line = |prefix: &str| prefix.strip_prefix('@').and_then(|p| p.strip_suffix('/')).is_some_and(|name| !name.contains('/'));
    let covers = |prefix: &str| file.starts_with(prefix) || (package_line(prefix) && Some(file) == prefix.strip_suffix('/'));
    rules.iter().filter(|rule| if rule.prefix.ends_with('/') { covers(&rule.prefix) } else { file == rule.prefix }).max_by_key(|rule| rule.prefix.len())
}
pub(super) fn check_policy(uses: &[FileUse], policy: &str) -> Result<Vec<String>, String> {
    let rules = rules(policy)?;
    let mut violations = Vec::new();
    for used in uses {
        // A file the policy does not mention is refused whatever it does: a new dependency is
        // reviewed before it is allowed anything, unchecked operations included.
        let Some(rule) = rule_for(&rules, &used.file) else { violations.push(format!("{} is in no line of the policy", used.file)); continue; };
        if let Some(allowed) = &rule.allowed {
            let extra = used.authority.iter().filter(|c| !allowed.iter().any(|a| covers(a, c))).cloned().collect::<Vec<_>>();
            if !extra.is_empty() { violations.push(format!("{} can use {}, which the policy does not allow", used.file, extra.join(", "))); }
        }
        if rule.no_unchecked && !used.unchecked.is_empty() { violations.push(format!("{} performs unchecked memory operations (in {}), which the policy forbids there", used.file, used.unchecked.join(", "))); }
    }
    Ok(violations)
}
/// The digest a line covering `files` (name and pin of each, every file of the bundle the line
/// governs, reached or not) is checked against: one file's own for a line naming it, and for a
/// directory the pin (`sha384:`) of `name-under-it NUL pin LF` for each, in name order.
pub(super) fn covered_digest(rule_prefix: &str, files: &[(String, String)]) -> String {
    if !rule_prefix.ends_with('/') { return files.first().map(|(_, digest)| digest.clone()).unwrap_or_default(); }
    let mut sorted: Vec<&(String, String)> = files.iter().collect();
    sorted.sort();
    let mut listing = String::new();
    for (name, digest) in sorted { listing.push_str(&format!("{}\0{digest}\n", name.strip_prefix(rule_prefix).unwrap_or(name))); }
    super::proof_digest::pin(listing.as_bytes()).unwrap_or_default()
}
/// `covered_digest` as pins were before the witchy-2 profile: SHA-256, bare hex.
fn legacy_digest(rule_prefix: &str, files: &[(String, String)]) -> String {
    if !rule_prefix.ends_with('/') { return files.first().map(|(_, digest)| digest.clone()).unwrap_or_default(); }
    let mut sorted: Vec<&(String, String)> = files.iter().collect();
    sorted.sort();
    let mut listing = String::new();
    for (name, digest) in sorted { listing.push_str(&format!("{}\0{digest}\n", name.strip_prefix(rule_prefix).unwrap_or(name))); }
    super::proof_digest::bytes(listing.as_bytes()).unwrap_or_default()
}
/// The policy with each `; digest` set to the code its line now covers, and the lines changed:
/// `wasm-caps --repin`, run after reviewing a package's update. Comments and every other part
/// of each line are kept as written.
/// A line's SHA-256 pin from before the witchy-2 profile is replaced only when it is the code as
/// it is now (`legacy`: each file's SHA-256, as pins were then): one that no longer matches is
/// refused, so a migration never passes a change off as reviewed (review of the cutover).
pub(super) fn repin(files: &[(String, String)], legacy: &[(String, String)], policy: &str) -> Result<(String, Vec<String>), String> {
    let rules = rules_of(policy, true)?;
    let (mut out, mut changed) = (String::new(), Vec::new());
    for line in policy.split_inclusive('\n') {
        let code = line.split('#').next().unwrap_or("");
        let rule = code.trim().rsplit_once(": ").and_then(|(prefix, _)| rules.iter().find(|rule| rule.prefix == prefix.trim() && rule.digest.is_some()));
        let Some(rule) = rule else { out.push_str(line); continue; };
        let covered: Vec<(String, String)> = files.iter().filter(|(name, _)| rule_for(&rules, name).is_some_and(|best| std::ptr::eq(best, rule))).cloned().collect();
        if covered.is_empty() { return Err(format!("{} has a digest but covers no file of the program", rule.prefix)); }
        let now = covered_digest(&rule.prefix, &covered);
        let old = rule.digest.as_deref().unwrap_or_default();
        if !super::proof_digest::is_pin(old) {
            let before: Vec<(String, String)> = legacy.iter().filter(|(name, _)| covered.iter().any(|(c, _)| c == name)).cloned().collect();
            if legacy_digest(&rule.prefix, &before) != old {
                return Err(format!("{} has changed since its SHA-256 pin was reviewed: review it, then replace its `; digest` with the one `wasm-caps --draft-policy` prints", rule.prefix));
            }
        }
        if old != now { changed.push(format!("{}: {old} -> {now}", rule.prefix)); }
        out.push_str(&line.replacen(&format!("digest {old}"), &format!("digest {now}"), 1));
    }
    Ok((out, changed))
}
/// `; digest HEX` lines: the code each covers must be what it was reviewed as (a package
/// updated since is a violation, naming the digest it has now, until its line is reviewed again).
pub(super) fn check_digests(files: &[(String, String)], policy: &str) -> Result<Vec<String>, String> {
    let rules = rules(policy)?;
    let mut violations = Vec::new();
    for rule in rules.iter().filter(|rule| rule.digest.is_some()) {
        let covered: Vec<(String, String)> = files.iter().filter(|(name, _)| rule_for(&rules, name).is_some_and(|best| std::ptr::eq(best, rule))).cloned().collect();
        if covered.is_empty() { violations.push(format!("{} has a digest but covers no file of the program", rule.prefix)); continue; }
        let now = covered_digest(&rule.prefix, &covered);
        if Some(&now) != rule.digest.as_ref() { violations.push(format!("{} has changed since its line was reviewed: its digest is now {now}", rule.prefix)); }
    }
    Ok(violations)
}

/// The same as JSON: `{"functions": {"name": ["Capability", ...]}, "pure": n, "total": [...]}`.
pub(super) fn json(program: &Program, main_parameters: usize) -> String {
    let quote = |s: &str| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""));
    let list = |items: &mut dyn Iterator<Item = &String>| items.map(|c| quote(c)).collect::<Vec<_>>().join(",");
    let footprint = footprint(program, main_parameters);
    let functions = footprint.functions.iter().map(|(f, c)| format!("{}:[{}]", quote(f), list(&mut c.iter()))).collect::<Vec<_>>().join(",");
    let total = footprint.total();
    format!("{{\"functions\":{{{functions}}},\"pure\":{},\"total\":[{}]}}\n", footprint.pure, list(&mut total.iter()))
}

/// How many of `main`'s first locals are its grant: its parameters, and the grantable
/// capabilities built at its top from the run's grants (parser/grants.rs).
pub(super) fn main_grant(syntax: &crate::ast::Program) -> usize {
    use crate::ast::{Declaration, Expression, Statement};
    syntax.declarations.iter().find_map(|d| match d {
        Declaration::Function(f) if f.name == "main" => Some(f.parameters.len() + f.body.iter().take_while(|s| matches!(s,
            Statement::Bind { value, .. } if matches!(&value.kind, Expression::Call(callee, _) if matches!(&callee.kind, Expression::Name(n) if n.starts_with("#grant_"))))).count()),
        _ => None,
    }).unwrap_or(0)
}

/// The type parameters a type mentions, at any depth (`&F`, `Vec<F>`).
fn parameters_in(types: &Types, ty: Ty, out: &mut BTreeSet<(u32, u32)>, seen: &mut BTreeSet<u32>, depth: usize) {
    if depth > 256 { return; }
    match ty {
        Ty::Param { owner, index } => { out.insert((owner.0, index)); }
        Ty::Borrow { pointee: Pointee::Param(owner, index), .. } => { out.insert((owner.0, index)); }
        Ty::Borrow { pointee, .. } => parameters_in(types, pointee.ty(), out, seen, depth + 1),
        Ty::Aggregate(id) => if seen.insert(id) { for field in types.get(ty).map(|a| a.fields.as_slice()).unwrap_or_default() { parameters_in(types, field.ty, out, seen, depth + 1); } },
        Ty::Sum(id) => if seen.insert(u32::MAX - id) { for arm in types.sum(ty).map(|s| s.arms.as_slice()).unwrap_or_default() { parameters_in(types, *arm, out, seen, depth + 1); } },
        _ => {}
    }
}

pub(super) fn footprint(program: &Program, main_parameters: usize) -> Footprint {
    let types = &program.types;
    let functions = &program.functions;
    let by_name: BTreeMap<&str, u32> = functions.iter().enumerate().map(|(i, f)| (f.name.as_str(), i as u32)).collect();
    // Only what `main` can reach is part of the program's footprint, and a generic function
    // holds what its type arguments hold: a callback given authority by its maker
    // (docs/design-closure-authority.md). For each type parameter, the types it is given,
    // followed through calls until nothing new is found. A trait's method reached with a
    // given `Self` reaches that type's own method.
    let mut reached = BTreeSet::from([program.main.0]);
    let mut given: BTreeMap<(u32, u32), Vec<Ty>> = BTreeMap::new();
    let mut changed = true;
    // Until nothing new is reached: the sets only grow, and are finite (review: a limit of 64
    // rounds left deeper calls out of every report).
    while changed {
        changed = false;
        for at in reached.clone() {
            let f = &functions[at as usize];
            for call in &f.calls {
                changed |= reached.insert(call.definition.0);
                for (index, argument) in call.substitutions.iter().enumerate() {
                    let mut found: Vec<Ty> = Vec::new();
                    let mut inner = BTreeSet::new();
                    parameters_in(types, *argument, &mut inner, &mut BTreeSet::new(), 0);
                    if inner.is_empty() { found.push(*argument); }
                    // A type built from the caller's own parameters holds what they are given.
                    for key in inner { if let Some(types) = given.get(&key) { found.extend(types.iter().copied()); } }
                    let entry = given.entry((call.definition.0, index as u32)).or_default();
                    for t in found { if !entry.contains(&t) { entry.push(t); changed = true; } }
                }
            }
            // `WitchyFn1.call` given `Self = Logger` runs `Logger.call`.
            if let Some((_, method)) = f.name.split_once('.') && let Some(selves) = given.get(&(at, 0)) {
                for t in selves.clone() {
                    let Some(owner) = types.nominal(t) else { continue };
                    if let Some(&target) = by_name.get(format!("{owner}.{method}").as_str()) { changed |= reached.insert(target); }
                }
            }
        }
    }
    let mut lines = BTreeMap::new();
    let mut by_index = BTreeMap::new();
    let mut pure = 0;
    for index in &reached {
        let f = &functions[*index as usize];
        let mut found = BTreeSet::new();
        // `main`'s grant is its first locals; it has no parameters of its own.
        let parameters: Vec<Ty> = if *index == program.main.0 { f.locals.iter().take(main_parameters).map(|l| l.ty).collect() } else { f.params.clone() };
        for ty in &parameters { found.extend(described(types, *ty)); }
        let mut mentioned = BTreeSet::new();
        for ty in &parameters { parameters_in(types, *ty, &mut mentioned, &mut BTreeSet::new(), 0); }
        for key in mentioned {
            for t in given.get(&key).into_iter().flatten() {
                for capability in described(types, *t) { found.insert(format!("{capability} (through a type argument)")); }
            }
        }
        if found.is_empty() { pure += 1; continue; }
        by_index.insert(*index, found.clone());
        // A private function shares its name with one in another file: `name@file`.
        let shown = if f.name.starts_with('#') { continue }
            // A closure's own `call`, which holds what the closure took over.
            else if let Some(rest) = f.name.strip_prefix("Closure#") { let (at, method) = rest.rsplit_once('.').unwrap_or((rest, "call")); format!("a closure (at {at}).{method}") }
            else { f.name.split_once('@').map_or(f.name.clone(), |(name, file)| format!("{name} (file {file})")) };
        lines.insert(shown, found);
    }
    Footprint { functions: lines, pure, reached, by_index }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_function_holds_what_its_parameters_hold_and_nothing_else() {
        let source = "type Holder:\n    out: Console<Write>\n\nfn pure(x: U64) -> U64:\n    return x + 1\n\nfn via(holder: &Holder) -> U64:\n    return 1\n\nfn direct(console: &Console<Write>):\n    console.print_int(pure(1))\n\nfn main(console: Console<Write>, dir: Dir<Read>):\n    direct(&console)\n    let holder = Holder(move console)\n    console_free(via(&holder))\n\nfn console_free(x: U64):\n    return\n";
        let syntax = crate::compiler::parse(source).expect("parses");
        let logical = super::super::check::program(&syntax).expect("checks");
        let report = super::report(&logical, 2);
        assert!(report.starts_with("main: Console<Write>, Dir<Read>\n"), "{report}");
        assert!(report.contains("direct: Console<Write>\n") && report.contains("via: Console<Write> (in Holder)\n"), "{report}");
        assert!(!report.contains("pure:") && report.contains("2 reachable functions hold no capability"), "{report}");
    }
}

#[cfg(test)]
mod files_tests {
    /// `wasm-caps` reads a program of several files as a build does: with each file's bounds,
    /// so per-file functions and module-qualified calls resolve the same way.
    #[test]
    fn several_files_are_read_with_their_bounds() {
        let root = std::env::temp_dir().join(format!("witchy-caps-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("other.wy"), "fn helper(x: U64) -> U64:\n    return x\n\npub fn twice(x: U64) -> U64:\n    return helper(x) + helper(x)\n").unwrap();
        std::fs::write(root.join("main.wy"), "import \"other.wy\"\n\nfn helper(x: U64) -> U64:\n    return x + 1\n\nfn main():\n    let y = other.twice(helper(1))\n").unwrap();
        let report = crate::compiler::caps_file(&root.join("main.wy"));
        let _ = std::fs::remove_dir_all(&root);
        let report = report.unwrap();
        assert!(report.contains("hold no capability"), "{report}");
    }
    /// `wasm-caps --by-file`: an imported file given nothing can use nothing, whatever `main` holds.
    #[test]
    fn each_file_reports_what_its_reached_functions_can_use() {
        let root = std::env::temp_dir().join(format!("witchy-caps-by-file-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("package.wy"), "pub fn twice(x: U64) -> U64:\n    return x + x\n\nfn unused(console: &Console<Write>):\n    console.line(\"x\")\n").unwrap();
        // A private function named like one in another file, and a closure, each holding the console.
        std::fs::write(root.join("logger.wy"), "import console\n\nfn say(console: &Console<Write>):\n    console.line(\"logged\")\n\npub fn log(console: &Console<Write>):\n    say(console)\n    let shout = fn() -> (): console.line(\"!\")\n    shout()\n").unwrap();
        std::fs::write(root.join("main.wy"), "import console\nimport \"package.wy\"\nimport \"logger.wy\"\n\nfn say(console: &Console<Write>):\n    console.line(\"main\")\n\nfn main(console: Console<Write>):\n    console.print_int(twice(2))\n    say(&console)\n    log(&console)\n").unwrap();
        let report = crate::compiler::caps_by_file(&root.join("main.wy"));
        let _ = std::fs::remove_dir_all(&root);
        let report = report.unwrap();
        let line = |file: &str| report.lines().find(|line| line.contains(file)).unwrap_or_default().to_owned();
        assert!(line("package.wy").ends_with("1 reachable functions; can use nothing"), "{report}");
        assert!(line("main.wy").contains("Console<Write>"), "{report}");
        assert!(line("logger.wy").ends_with("3 reachable functions; can use Console<Write>"), "{report}");
    }
    #[test]
    fn no_unchecked_refuses_a_proof_only_utf8_view_but_allows_validation() {
        let root = std::env::temp_dir().join(format!("witchy-caps-utf8-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let main = root.join("main.wy");
        let policy = root.join("main.policy");
        let caller = r#"
fn main(allocator: Allocator):
    let allocated: Result<Vec<U8>, U64> = Vec.new(&allocator, 0)
    match move allocated:
        Ok(vector):
            let text: &str = "hello"
            let viewed = reborrow(&vector, text)
            let length = viewed.len()
            vector.close()
        Err(code):
            return
"#;
        let unchecked = r#"import vec

fn reborrow(buffer: &Vec<U8>, text: &str) -> &str:
    ensures result.len() == text.len()
    proof:
        auto
    return memory.utf8_unchecked(text.as_bytes())
"#;
        let checked = r#"import vec

fn reborrow(buffer: &Vec<U8>, text: &str) -> &str:
    match str.from_utf8_bytes(text.as_bytes()):
        Ok(viewed):
            return viewed
        Err(code):
            return text
"#;
        std::fs::write(&policy, "<standard>/wasm-vec.wy: Allocator\nmain.wy: Allocator ; no unchecked\n").unwrap();
        std::fs::write(&main, format!("{unchecked}{caller}")).unwrap();
        let unchecked_violations = crate::compiler::caps_policy(&policy, &main);
        std::fs::write(&main, format!("{checked}{caller}")).unwrap();
        let checked_violations = crate::compiler::caps_policy(&policy, &main);
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(
            unchecked_violations.unwrap(),
            ["main.wy performs unchecked memory operations (in reborrow), which the policy forbids there"]
        );
        assert!(checked_violations.unwrap().is_empty());
    }
    /// A file that holds only constants and types has no function `main` reaches. It is listed,
    /// a policy that does not name it is refused, and a draft pins it: so it cannot change unseen.
    #[test]
    fn a_file_of_constants_alone_is_held_to_the_policy() {
        let root = std::env::temp_dir().join(format!("witchy-caps-constants-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let constants = |limit: u64| format!("pub let LIMIT = {limit}\n\npub type Pair:\n    a: U64\n    b: U64\n");
        std::fs::write(root.join("limits.wy"), constants(42)).unwrap();
        std::fs::write(root.join("main.wy"), "import console\nimport \"limits.wy\"\n\nfn main(console: Console<Write>):\n    console.print_int(LIMIT)\n").unwrap();
        let main = root.join("main.wy");
        let report = crate::compiler::caps_by_file(&main).unwrap();
        assert!(report.lines().any(|line| line.ends_with("limits.wy: 0 reachable functions; can use nothing")), "{report}");
        // A policy with no line for it is refused, whatever it holds.
        let policy = root.join("main.policy");
        std::fs::write(&policy, "<standard>/: *\nmain.wy: *\n").unwrap();
        assert_eq!(crate::compiler::caps_policy(&policy, &main).unwrap(), vec!["limits.wy is in no line of the policy".to_string()]);
        // The draft names and pins it, and the program fits its draft.
        let draft = crate::compiler::caps_draft_policy(&main).unwrap();
        assert!(draft.lines().any(|line| line.starts_with("limits.wy: nothing ; no unchecked ; digest sha384:")), "{draft}");
        std::fs::write(&policy, &draft).unwrap();
        assert!(crate::compiler::caps_policy(&policy, &main).unwrap().is_empty());
        // Changed, it no longer fits the reviewed line.
        std::fs::write(root.join("limits.wy"), constants(600)).unwrap();
        let found = crate::compiler::caps_policy(&policy, &main).unwrap();
        let _ = std::fs::remove_dir_all(&root);
        assert!(found.len() == 1 && found[0].starts_with("limits.wy has changed since its line was reviewed"), "{found:?}");
    }
    /// A digest pins a line to the code it was reviewed for: one file, or every file under a directory.
    #[test]
    fn a_digest_pins_the_code_a_line_was_reviewed_for() {
        let pin = |digit: &str| format!("sha384:{}", digit.repeat(96));
        let files = vec![("deps/c/a.wy".to_string(), pin("1")), ("deps/c/b.wy".to_string(), pin("2")), ("app.wy".to_string(), pin("3"))];
        let whole = super::covered_digest("deps/c/", &files[..2]);
        assert!(super::check_digests(&files, &format!("deps/c/: * ; digest {whole}\napp.wy: * ; digest {}", pin("3"))).unwrap().is_empty());
        let mut changed = files.clone();
        changed[1].1 = pin("4");
        assert!(super::check_digests(&changed, &format!("deps/c/: * ; digest {whole}")).unwrap()[0].starts_with("deps/c/ has changed"));
        // A file added under the directory changes it too, and a more specific line takes a file out of it.
        let mut added = files.clone();
        added.push(("deps/c/new.wy".to_string(), pin("5")));
        assert_eq!(super::check_digests(&added, &format!("deps/c/: * ; digest {whole}")).unwrap().len(), 1);
        assert!(super::check_digests(&added, &format!("deps/c/: * ; digest {whole}\ndeps/c/new.wy: *")).unwrap().is_empty());
        assert!(super::check_digests(&files, &format!("gone/: * ; digest {}", pin("0"))).unwrap()[0].contains("covers no file"));
        assert!(super::check_digests(&files, "deps/c/: * ; digest ABC").is_err());
        assert!(super::covered_digest("deps/c/", &files[..2]).starts_with("sha384:"));
        // A SHA-256 pin, from before the witchy-2 profile, is refused with what to do, never
        // compared: not even when its hex is the SHA-256 of the code (no downgrade).
        let old = format!("deps/c/: * ; digest {}", "1".repeat(64));
        assert!(super::check_digests(&files, &old).unwrap_err().contains("SHA-256 digest, from before the witchy-2 profile"));
        assert!(super::check_digests(&files, &format!("deps/c/: * ; digest sha256:{}", "1".repeat(64))).is_err());
    }
    /// Repinning sets each digest to what its line covers now, and keeps everything else.
    #[test]
    fn repinning_updates_only_digests() {
        let pin = |digit: &str| format!("sha384:{}", digit.repeat(96));
        let files = vec![("deps/c/a.wy".to_string(), pin("1")), ("app.wy".to_string(), pin("3"))];
        let policy = format!("# reviewed\ndeps/c/: Allocator ; no unchecked ; digest {}  # c\napp.wy: *\n", pin("0"));
        let (pinned, changed) = super::repin(&files, &[], &policy).unwrap();
        assert_eq!(changed.len(), 1);
        assert!(pinned.starts_with("# reviewed\ndeps/c/: Allocator ; no unchecked ; digest ") && pinned.ends_with("  # c\napp.wy: *\n"), "{pinned}");
        assert!(super::check_digests(&files, &pinned).unwrap().is_empty());
        assert!(super::repin(&files, &[], &pinned).unwrap().1.is_empty());
        // A SHA-256 pin from before the witchy-2 profile is replaced by a repin only when it is
        // the code as it is now; one the code no longer matches is refused (review of the cutover).
        let legacy = vec![("deps/c/a.wy".to_string(), "e".repeat(64)), ("app.wy".to_string(), "f".repeat(64))];
        let reviewed = super::legacy_digest("deps/c/", &legacy[..1]);
        assert_eq!(reviewed.len(), 64);
        let stale = format!("deps/c/: Allocator ; digest {}\napp.wy: *\n", "0".repeat(64));
        assert!(super::repin(&files, &legacy, &stale).unwrap_err().contains("has changed since its SHA-256 pin was reviewed"));
        let old = format!("deps/c/: Allocator ; digest {reviewed}\napp.wy: *\n");
        let (repinned, changed) = super::repin(&files, &legacy, &old).unwrap();
        assert_eq!(changed.len(), 1);
        assert!(super::check_digests(&files, &repinned).unwrap().is_empty() && repinned.contains("digest sha384:"), "{repinned}");
    }
    /// A file imported through `..` is named by where it is, and a call chain of any depth is followed.
    #[test]
    fn imports_through_parent_directories_and_deep_calls_are_reported_where_they_are() {
        let root = std::env::temp_dir().join(format!("witchy-caps-paths-{}", std::process::id()));
        std::fs::create_dir_all(root.join("app")).unwrap();
        std::fs::create_dir_all(root.join("deps")).unwrap();
        let mut chain = String::from("import console\n\npub fn f0(console: &Console<Write>):\n    console.line(\"deep\")\n");
        for i in 1..80 { chain.push_str(&format!("\npub fn f{i}(console: &Console<Write>):\n    f{}(console)\n", i - 1)); }
        std::fs::write(root.join("deps/deep.wy"), chain).unwrap();
        std::fs::write(root.join("app/main.wy"), "import console\nimport \"../deps/deep.wy\"\n\nfn main(console: Console<Write>):\n    f79(&console)\n").unwrap();
        let uses = super::super::caps_by_file(&root.join("app/main.wy"));
        let _ = std::fs::remove_dir_all(&root);
        let report = uses.unwrap();
        assert!(report.lines().any(|line| line.contains("deps/deep.wy: 80 reachable functions; can use Console<Write>") && !line.contains("..")), "{report}");
    }
    /// A policy holds each file to its longest matching line; a file no line covers may use nothing.
    #[test]
    fn a_policy_names_each_file_that_reaches_too_far() {
        let used = |file: &str, authority: &[&str], unchecked: &[&str]| super::FileUse { file: file.into(), functions: 1, authority: authority.iter().map(|s| s.to_string()).collect(), unchecked: unchecked.iter().map(|s| s.to_string()).collect(), contracts: 0 };
        let uses = vec![used("<standard>/wasm-vec.wy", &["Allocator"], &["Vec.get"]), used("app/main.wy", &["Console<Write>", "Dir<Read>"], &[]), used("deps/config.wy", &["Allocator"], &[]), used("deps/net.wy", &["Net"], &[])];
        let policy = "<standard>/: *\napp/: *\ndeps/: Allocator ; no unchecked\n";
        assert_eq!(super::check_policy(&uses, policy).unwrap(), vec!["deps/net.wy can use Net, which the policy does not allow".to_string()]);
        let strict = "app/: Console<Write>\n";
        let found = super::check_policy(&uses, strict).unwrap();
        assert!(found.iter().any(|v| v.starts_with("app/main.wy can use Dir<Read>")) && found.iter().any(|v| v == "deps/config.wy is in no line of the policy"), "{found:?}");
        // Uncovered and using nothing is still refused: it was not reviewed.
        assert_eq!(super::check_policy(&[used("deps/quiet.wy", &[], &["Vec.get"])], "app/: *").unwrap(), vec!["deps/quiet.wy is in no line of the policy".to_string()]);
        assert!(super::check_policy(&uses[..1], "<standard>/: Allocator ; no unchecked").unwrap()[0].contains("unchecked"));
        assert!(super::check_policy(&uses, "nonsense").is_err());
        // A drafted policy is one the program fits.
        assert!(super::check_policy(&uses, &super::draft_policy(&uses, &[]).unwrap()).unwrap().is_empty());
        // A line for one file covers only that file.
        let evil = vec![used("deps/config.wy.evil", &["Net"], &[])];
        assert_eq!(super::check_policy(&evil, "deps/config.wy: Net").unwrap(), vec!["deps/config.wy.evil is in no line of the policy".to_string()]);
        assert!(super::draft_policy(&[used("odd; name.wy", &[], &[])], &[]).is_err());
    }
    /// `@name/` covers a package by name: its first file `@name` and its files `@name/PATH`, and
    /// no other package whose name starts the same way.
    #[test]
    fn a_package_line_covers_the_package_and_nothing_else() {
        let used = |file: &str| super::FileUse { file: file.into(), functions: 1, authority: ["Net".to_string()].into(), unchecked: Default::default(), contracts: 0 };
        let uses = vec![used("@cfg"), used("@cfg/util.wy"), used("@cfg-evil"), used("@cfgx/a.wy"), used("cfg")];
        let found = super::check_policy(&uses, "@cfg/: Net\ncfg/: Net\n").unwrap();
        assert_eq!(found, ["@cfg-evil", "@cfgx/a.wy", "cfg"].map(|f| format!("{f} is in no line of the policy")).to_vec());
        // A subdirectory's line is not the package's: it does not cover a file named like it.
        let net = vec![used("@cfg/net")];
        assert_eq!(super::check_policy(&net, "@cfg/: nothing\n@cfg/net/: Net\n").unwrap(), vec!["@cfg/net can use Net, which the policy does not allow".to_string()]);
    }
}

#[cfg(test)]
mod diff_tests {
    fn footprint(source: &str) -> super::Footprint {
        let syntax = crate::compiler::parse(source).expect("parses");
        super::footprint(&super::super::check::program(&syntax).expect("checks"), super::main_grant(&syntax))
    }
    /// `main(connection: Socket)` is the program for one connection (the engine accepts, and
    /// starts a new instance for each): the footprint is a `Socket`, and it takes nothing held
    /// once or across connections beside it, in either order.
    #[test]
    fn a_main_that_takes_a_socket_is_the_program_for_one_connection() {
        let each = footprint("fn main(connection: Socket, dir: Dir, allocator: Allocator, clock: Clock):\n    let closed = connection.raw_close()\n");
        let (report, widened) = super::diff(&footprint("fn main(dir: Dir, allocator: Allocator, clock: Clock):\n    return\n"), &each);
        assert!(widened && report.contains("Socket"), "{report}");
        // Granted again whole for each connection: a read file and the input (read once before
        // the first), Net and Exec. Refused: the rest.
        for beside in ["net: Net", "exec: Exec", "input: Console<Read>", "config: File<Read>"] {
            let source = format!("fn main(connection: Socket, {beside}):\n    let closed = connection.raw_close()\n");
            assert!(super::super::check::program(&crate::compiler::parse(&source).expect("parses")).is_ok(), "{source}");
        }
        for beside in ["listener: Listener", "store: SecretStore", "key: Secret<Seal>", "signer: Signer", "log: File<Write>"] {
            for source in [format!("fn main(connection: Socket, {beside}):\n    let closed = connection.raw_close()\n"), format!("fn main({beside}, connection: Socket):\n    let closed = connection.raw_close()\n")] {
                let syntax = crate::compiler::parse(&source).expect("parses");
                let refused = super::super::check::program(&syntax).err().map(|error| format!("{error:?}")).unwrap_or_default();
                assert!(refused.contains("run once for each connection"), "{source}: {refused}");
            }
        }
        let syntax = crate::compiler::parse("fn main(one: Socket, other: Socket):\n    let a = one.raw_close()\n    let b = other.raw_close()\n").expect("parses");
        assert!(super::super::check::program(&syntax).is_err());
    }
    /// A new capability anywhere widens the program; giving rights up does not.
    #[test]
    fn widening_is_what_a_new_version_can_do_that_the_old_could_not() {
        let read = footprint("fn look(dir: &Dir<Read>) -> U64:\n    return 1\n\nfn main(dir: Dir<Read>):\n    let n = look(&dir)\n");
        let full = footprint("fn look(dir: &Dir<Read>) -> U64:\n    return 1\n\nfn main(dir: Dir):\n    let n = look(&dir)\n");
        let clock = footprint("fn look(dir: &Dir<Read>, clock: &Clock) -> U64:\n    return 1\n\nfn main(dir: Dir<Read>, clock: Clock):\n    let n = look(&dir, &clock)\n");
        let (report, widened) = super::diff(&read, &clock);
        assert!(widened && report.contains("widened: the program now holds Clock") && report.contains("look: + Clock"), "{report}");
        assert!(super::diff(&read, &full).1, "Dir<Read> to Dir is wider");
        let (report, widened) = super::diff(&full, &read);
        assert!(!widened && report.contains("narrowed"), "{report}");
        assert_eq!(super::diff(&read, &read), ("no change in authority\n".to_string(), false));
    }
    /// A grantable capability is reported as itself, and requiring a new one widens.
    #[test]
    fn a_new_grantable_capability_widens() {
        let library = "grantable capability Quota:\n    bytes: U64\n\nfn spend(quota: &Quota) -> U64:\n    return (*quota).bytes\n\n";
        let before = footprint(&format!("{library}fn main(console: Console<Write>):\n    console.print_int(1)\n"));
        let after = footprint(&format!("{library}fn main(console: Console<Write>, quota: Quota):\n    console.print_int(spend(&quota))\n"));
        assert_eq!(after.functions.get("main").map(|c| c.iter().cloned().collect::<Vec<_>>()), Some(vec!["Console<Write>".to_string(), "Quota (user capability)".to_string()]));
        assert!(after.functions.get("spend").is_some_and(|c| c.contains("Quota (user capability)")));
        let (report, widened) = super::diff(&before, &after);
        assert!(widened && report.contains("widened: the program now holds Quota (user capability)"), "{report}");
    }
    /// A lent callback's authority is the receiver's, through its type argument.
    #[test]
    fn a_callback_carries_its_makers_authority_to_the_receiver() {
        let receiver = "fn each_of(f: &fn(U64) -> (), n: U64):\n    for i in 0..n:\n        f(i)\n\n";
        let quiet = footprint(&format!("{receiver}fn main(console: Console<Write>):\n    let f = fn(n: U64) -> (): ()\n    each_of(&f, 3)\n    console.print_int(1)\n"));
        let loud = footprint(&format!("{receiver}fn main(console: Console<Write>):\n    let f = fn(n: U64) -> (): console.print_int(n)\n    each_of(&f, 3)\n"));
        assert!(!quiet.functions.contains_key("each_of"), "{:?}", quiet.functions);
        assert!(loud.functions.get("each_of").is_some_and(|c| c.contains("Console<Write> (in a closure) (through a type argument)")), "{:?}", loud.functions);
        let (report, widened) = super::diff(&quiet, &loud);
        assert!(!widened && report.contains("each_of: + Console<Write> (in a closure) (through a type argument)"), "{report}");
    }
    #[test]
    fn json_lists_each_function_and_the_total() {
        let syntax = crate::compiler::parse("fn look(dir: &Dir<Read>) -> U64:\n    return 1\n\nfn main(dir: Dir<Read>):\n    let n = look(&dir)\n").unwrap();
        let program = super::super::check::program(&syntax).unwrap();
        assert_eq!(super::json(&program, 1), "{\"functions\":{\"look\":[\"Dir<Read>\"],\"main\":[\"Dir<Read>\"]},\"pure\":0,\"total\":[\"Dir<Read>\"]}\n");
    }
}
