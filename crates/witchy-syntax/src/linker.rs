//! Module linker.
//!
//! Combines a set of named modules into one flat `Module`, qualifying each
//! module's function names (`mod.func`) and rewriting call sites so an
//! unqualified call resolves to the same module and a `mod.func` call resolves
//! to an imported module. Importing is purely declarative: it brings names into
//! scope, runs no code, and confers no authority — a dependency can only act
//! through capabilities the caller passes to its functions (visible in their
//! types).
//!
//! v1: functions are module-scoped; types/constructors share one global
//! namespace.

use std::collections::{HashMap, HashSet};
use std::fmt;

use crate::ast::*;
use crate::lambda_scan::collect_pattern_vars;

#[derive(Debug, Clone, PartialEq)]
pub struct LinkError {
    pub message: String,
}

impl fmt::Display for LinkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "link error: {}", self.message)
    }
}

impl std::error::Error for LinkError {}

fn lerr<T>(message: impl Into<String>) -> Result<T, LinkError> {
    Err(LinkError {
        message: message.into(),
    })
}

const BUILTINS: &[&str] = &[
    "print",
    "print_int",
    "print_float",
    "__render",
    "int_to_string",
    "string_length",
    "to_upper",
    "to_lower",
    "trim",
    "starts_with",
    "contains",
    "ends_with",
    "index_of",
    "split",
    "replace",
    "substring",
    "int_to_float",
    "float_to_int",
    "int_to_duration",
    "duration_to_int",
    "sqrt",
    "string_to_int",
    "length",
    "char_count",
    "at",
    "push",
    "concat",
    "dict_new",
    "insert",
    "get_or",
    "has",
    "keys",
    "values",
    "pairs",
    "size",
    "send",
    "read",
    "exists",
    "subtree",
    "exec",
    "connect",
    "try_connect",
    "send_line",
    "recv_line",
    "send_bytes",
    "recv_all",
    "recv_bytes",
    "listen",
    "accept",
    "close",
];

type FnTable = HashMap<String, HashSet<String>>;

/// The source of a bundled standard-library module, if `name` is one. This is
/// the canonical std registry: the linker treats it as a built-in search path,
/// and the CLI/test harness resolve `import` against it too.
/// Names of all bundled standard-library modules.
pub const STD_MODULES: &[&str] = &[
    "list", "string", "math", "result", "option", "func", "cmp", "ascii", "set", "server",
    "show", "http", "json", "url", "duration", "random", "regex", "crypto", "compiler", "toml",
    "iter", "semver", "rights", "fs", "dict", "csv", "time", "encoding", "path", "testing",
    "future", "task", "chan", "webauthn", "secretstore", "reflect", "meta", "convert", "exec",
    "confine", "jwt", "oauth", "rand", "vm", "bytes",
];

/// The bundled std modules that export a `pub fn` of the given name — used to
/// suggest a missing `import` when a call names an unimported stdlib function.
pub fn std_modules_for_function(fn_name: &str) -> Vec<&'static str> {
    let needle = format!("pub fn {fn_name}(");
    STD_MODULES
        .iter()
        .copied()
        .filter(|m| std_source(m).is_some_and(|s| s.contains(&needle)))
        .collect()
}

/// Every `pub fn` exported by a bundled std module, as `(function, module)`.
fn std_pub_fns() -> Vec<(String, &'static str)> {
    let mut out = Vec::new();
    for m in STD_MODULES {
        if let Some(src) = std_source(m) {
            for line in src.lines() {
                if let Some(rest) = line.trim_start().strip_prefix("pub fn ") {
                    if let Some(paren) = rest.find('(') {
                        out.push((rest[..paren].trim().to_string(), *m));
                    }
                }
            }
        }
    }
    out
}

/// std `pub fn`s whose name matches `query` — exact matches if any, else
/// substring matches — rendered as module-qualified signature lines (with the
/// immediately-preceding doc comment, when there is one) for `witchy which`.
pub fn std_signatures(query: &str) -> Vec<String> {
    let mut exact = Vec::new();
    let mut partial = Vec::new();
    for m in STD_MODULES {
        let Some(src) = std_source(m) else { continue };
        let lines: Vec<&str> = src.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            let t = line.trim_start();
            let Some(rest) = t.strip_prefix("pub fn ") else { continue };
            let Some(paren) = rest.find('(') else { continue };
            let fname = rest[..paren].trim();
            let bucket = if fname == query {
                &mut exact
            } else if fname.contains(query) {
                &mut partial
            } else {
                continue;
            };
            let sig = rest.trim_end().trim_end_matches(':');
            let mut entry = format!("{m}.{sig}");
            // The doc is the contiguous `//` block above; its FIRST line is
            // the sentence that describes the function.
            let mut start = i;
            while start > 0 && lines[start - 1].trim_start().starts_with("//") {
                start -= 1;
            }
            if start < i {
                if let Some(doc) = lines[start].trim_start().strip_prefix("//") {
                    entry.push_str(&format!("\n    {}", doc.trim()));
                }
            }
            bucket.push(entry);
        }
    }
    if !exact.is_empty() {
        return exact;
    }
    if !partial.is_empty() {
        return partial;
    }
    // Abbreviation tier: `to_ms` finds `to_milliseconds` — the query is a
    // subsequence of the name and they share the first few characters.
    let prefix: String = query.chars().take(3).collect();
    let mut abbrev = Vec::new();
    for (cand, m) in std_pub_fns() {
        if cand.starts_with(&prefix) && is_subsequence(query, &cand) {
            abbrev.extend(std_signatures(&cand).into_iter().filter(|s| {
                s.starts_with(&format!("{m}.{cand}("))
            }));
        }
    }
    abbrev.sort();
    abbrev.dedup();
    abbrev.truncate(5);
    abbrev
}

/// Whether the characters of `needle` appear in `hay` in order.
fn is_subsequence(needle: &str, hay: &str) -> bool {
    let mut it = hay.chars();
    needle.chars().all(|c| it.any(|h| h == c))
}

/// Every `pub fn` of one std module, as signature lines — `witchy which time`
/// lists what the `time` module exports.
pub fn module_exports(module: &str) -> Vec<String> {
    let Some(src) = std_source(module) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in src.lines() {
        let t = line.trim_start();
        if let Some(rest) = t.strip_prefix("pub fn ") {
            if rest.contains('(') {
                out.push(format!("{module}.{}", rest.trim_end().trim_end_matches(':')));
            }
        }
    }
    out
}

/// The closest std-library function name to `name` within a small edit distance —
/// used to suggest a likely-misspelled stdlib call. Returns `(function, module)`.
pub fn closest_std_function(name: &str) -> Option<(String, &'static str)> {
    if name.len() < 3 {
        return None; // too short for a meaningful suggestion
    }
    let mut best: Option<(usize, String, &'static str)> = None;
    for (cand, m) in std_pub_fns() {
        if cand == name {
            continue;
        }
        let d = levenshtein(name, &cand);
        // Require the edit to be small relative to the name, so short names don't
        // match everything.
        if d <= 2 && d < name.len() && best.as_ref().is_none_or(|(bd, _, _)| d < *bd) {
            best = Some((d, cand, m));
        }
    }
    best.map(|(_, c, m)| (c, m))
}

/// The closest bundled std-module name to `name` within a small edit distance —
/// used to suggest a correction for a misspelled `import`.
pub fn closest_std_module(name: &str) -> Option<&'static str> {
    if name.len() < 3 {
        return None; // too short for a meaningful suggestion
    }
    let mut best: Option<(usize, &'static str)> = None;
    for m in STD_MODULES {
        let d = levenshtein(name, m);
        if d <= 2 && d < name.len() && best.as_ref().is_none_or(|(bd, _)| d < *bd) {
            best = Some((d, m));
        }
    }
    best.map(|(_, m)| m)
}

/// Levenshtein edit distance (two-row dynamic programming).
fn levenshtein(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr = vec![0; b.len() + 1];
    for (i, ca) in a.chars().enumerate() {
        curr[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let cost = if ca == cb { 0 } else { 1 };
            curr[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(curr[j] + 1);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
}

pub fn std_source(name: &str) -> Option<&'static str> {
    match name {
        "list" => Some(include_str!("../../../std/list.witchy")),
        "string" => Some(include_str!("../../../std/string.witchy")),
        "math" => Some(include_str!("../../../std/math.witchy")),
        "result" => Some(include_str!("../../../std/result.witchy")),
        "option" => Some(include_str!("../../../std/option.witchy")),
        "func" => Some(include_str!("../../../std/func.witchy")),
        "convert" => Some(include_str!("../../../std/convert.witchy")),
        "cmp" => Some(include_str!("../../../std/cmp.witchy")),
        "testing" => Some(include_str!("../../../std/testing.witchy")),
        "ascii" => Some(include_str!("../../../std/ascii.witchy")),
        "set" => Some(include_str!("../../../std/set.witchy")),
        "server" => Some(include_str!("../../../std/server.witchy")),
        "show" => Some(include_str!("../../../std/show.witchy")),
        "http" => Some(include_str!("../../../std/http.witchy")),
        "json" => Some(include_str!("../../../std/json.witchy")),
        "url" => Some(include_str!("../../../std/url.witchy")),
        "duration" => Some(include_str!("../../../std/duration.witchy")),
        "random" => Some(include_str!("../../../std/random.witchy")),
        "rand" => Some(include_str!("../../../std/rand.witchy")),
        "regex" => Some(include_str!("../../../std/regex.witchy")),
        "crypto" => Some(include_str!("../../../std/crypto.witchy")),
        "secretstore" => Some(include_str!("../../../std/secretstore.witchy")),
        "webauthn" => Some(include_str!("../../../std/webauthn.witchy")),
        "compiler" => Some(include_str!("../../../std/compiler.witchy")),
        "toml" => Some(include_str!("../../../std/toml.witchy")),
        "iter" => Some(include_str!("../../../std/iter.witchy")),
        "semver" => Some(include_str!("../../../std/semver.witchy")),
        "rights" => Some(include_str!("../../../std/rights.witchy")),
        "fs" => Some(include_str!("../../../std/fs.witchy")),
        "dict" => Some(include_str!("../../../std/dict.witchy")),
        "csv" => Some(include_str!("../../../std/csv.witchy")),
        "time" => Some(include_str!("../../../std/time.witchy")),
        "encoding" => Some(include_str!("../../../std/encoding.witchy")),
        "path" => Some(include_str!("../../../std/path.witchy")),
        "future" => Some(include_str!("../../../std/future.witchy")),
        "task" => Some(include_str!("../../../std/task.witchy")),
        "chan" => Some(include_str!("../../../std/chan.witchy")),
        "reflect" => Some(include_str!("../../../std/reflect.witchy")),
        "meta" => Some(include_str!("../../../std/meta.witchy")),
        "exec" => Some(include_str!("../../../std/exec.witchy")),
        "confine" => Some(include_str!("../../../std/confine.witchy")),
        "jwt" => Some(include_str!("../../../std/jwt.witchy")),
        "oauth" => Some(include_str!("../../../std/oauth.witchy")),
        "vm" => Some(include_str!("../../../std/vm.witchy")),
        "bytes" => Some(include_str!("../../../std/bytes.witchy")),
        // (RFC-0041) `glamour` is a published RUNE, not std — deliberately ABSENT from
        // `STD_MODULES` so it is not advertised as std and a project still declares it as a
        // dependency (the PM/footprint treat it as external). But its source is bundled here so
        // its PURE render surface (`element`/`text`/`to_html`) is importable in a STANDALONE
        // SNIPPET — the browser playground and the book's runnable cells — where there is no
        // filesystem to resolve the rune. Authority-neutral: importing it only exposes types;
        // a `UiRoot`/`UiFetch` can still be minted only by the host, never obtained in a cell.
        "glamour" => Some(include_str!("../../../projects/glamour/src/glamour.witchy")),
        _ => None,
    }
}

/// The per-module compile-time expansion pass the linker invokes (`comptime:`
/// blocks + `tag"…"` literals). Injected as a callback so the linker stays
/// agnostic of how compile-time code is evaluated (RFC-0018): it never names
/// `comptime`/`tagged`. `crate::comptime::expand_compile_time` is the production
/// implementation; `crate::pipeline::link` wires it in.
pub type ComptimeExpander = fn(&str, &mut Module, &[(String, Module)]) -> Result<(), String>;

/// Link `modules` (each a name + parsed module) into one flat module, with
/// `entry` the module holding `main`. `expand` runs each module's compile-time
/// passes (see [`ComptimeExpander`]).
pub fn link(
    mut modules: Vec<(String, Module)>,
    entry: &str,
    expand: ComptimeExpander,
) -> Result<Module, LinkError> {
    // Lower `gen fn`/`yield` to ordinary functions over `std/iter` first — this
    // adds `import iter`/`import option` to any generator module, so the std
    // pull-in below resolves them.
    modules = modules
        .into_iter()
        .map(|(n, m)| (n, crate::generators::lower(m)))
        .collect();

    // Lower `async fn`/`await` to ordinary functions over `std/task` (CPS over
    // closures), also before typeck — adds `import task`/`import chan` to any
    // async module.
    modules = modules
        .into_iter()
        .map(|(n, m)| crate::async_lower::lower(m).map(|m| (n, m)))
        .collect::<Result<_, _>>()
        .map_err(|message| LinkError { message })?;

    // Lower named-field record construction (`Point(x: 1, ..p)`) to positional
    // constructors / record updates, so later stages never see `Expr::Record`.
    modules = modules
        .into_iter()
        .map(|(n, m)| crate::records::lower(m).map(|m| (n, m)))
        .collect::<Result<_, _>>()
        .map_err(|message| LinkError { message })?;

    // Compile-time expansion happens here, per module, BEFORE name resolution
    // and type checking see it. `expand` runs `comptime:` blocks (zero
    // capabilities, print = the emit channel) and then `tag"…${e}…"` tagged
    // literals; both append/splice items so the expanded AST is identical on
    // both backends (RFC-0006). Additive only. Tagged expansion needs the OTHER
    // modules so an IMPORTED tag resolves, so each call gets a snapshot of the
    // rest of the link set (those already expanded this pass, plus the raw later
    // ones). To run a tag, the expander `link`s a pruned comptime program — which
    // re-enters this pass; the reachable-set prune keeps that finite. The linker
    // invokes `expand` as an injected callback and never names comptime/tagged
    // itself (RFC-0018).
    {
        let names: Vec<String> = modules.iter().map(|(n, _)| n.clone()).collect();
        for (i, name) in names.iter().enumerate() {
            let siblings: Vec<(String, Module)> = modules
                .iter()
                .enumerate()
                .filter(|(j, _)| *j != i)
                .map(|(_, m)| m.clone())
                .collect();
            expand(name, &mut modules[i].1, &siblings).map_err(|message| LinkError { message })?;
        }
    }

    // Modules pulled in by the prelude/import passes below post-date the
    // records-lowering + compile-time expansion above; index from here so we can
    // run those same passes on just the new ones (see after the pull-in loop).
    let pulled_std_start = modules.len();

    // THE PRELUDE: the core data modules are always in the link set, so the
    // module-qualified spellings (`list.push`, `string.split`, `dict.insert`,
    // `math.sqrt`) resolve without an import line. Locally provided modules
    // still take precedence; dead-code elimination strips what goes unused.
    for prelude in ["list", "string", "dict", "math", "option", "result"] {
        if !modules.iter().any(|(n, _)| n == prelude) {
            if let Some(src) = std_source(prelude) {
                if let Ok(m) = crate::parser::parse_module(src) {
                    modules.push((prelude.to_string(), m));
                }
            }
        }
    }

    // Pull in any imported standard-library module not already provided (the
    // std registry is a built-in search path), transitively — so a std module
    // can import another (e.g. `list` importing `option`) and callers need not
    // list the dependency explicitly. Locally provided modules take precedence:
    // a name already present is never overridden by the bundled copy.
    let mut i = 0;
    while i < modules.len() {
        let imports = modules[i].1.imports.clone();
        for imp in imports {
            if !modules.iter().any(|(n, _)| n == &imp) {
                if let Some(src) = std_source(&imp) {
                    let m = crate::parser::parse_module(src).map_err(|e| LinkError {
                        message: format!("std module `{imp}`: {e}"),
                    })?;
                    modules.push((imp.clone(), m));
                }
            }
        }
        i += 1;
    }

    // The std modules pulled in just above were appended AFTER the
    // records-lowering + compile-time expansion passes ran, so run those same
    // passes on them now — otherwise a std type's `derive(...)` (e.g. `semver`'s
    // `Version`) would never desugar to its `meta.derive_*` comptime call, nor
    // run it to generate the impl. `records::lower` (which runs `derive::expand`)
    // is idempotent — it consumes the annotation — and comptime auto-imports
    // `meta`, so this is a no-op for the derive-free modules and needs nothing
    // extra in the link set. The entry modules already ran both passes above, so
    // restrict to `pulled_std_start..` to avoid re-running comptime expansion
    // (which, unlike derive desugaring, is not idempotent).
    for (_, m) in modules[pulled_std_start..].iter_mut() {
        *m = crate::records::lower(m.clone()).map_err(|message| LinkError { message })?;
    }
    for k in pulled_std_start..modules.len() {
        let name = modules[k].0.clone();
        let siblings: Vec<(String, Module)> = modules
            .iter()
            .enumerate()
            .filter(|(j, _)| *j != k)
            .map(|(_, m)| m.clone())
            .collect();
        expand(&name, &mut modules[k].1, &siblings).map_err(|message| LinkError { message })?;
    }

    // MethodCall nodes survive linking: `x.f(a)` resolves to a REAL method
    // (impl/trait/static) during trait lowering, not to arbitrary free
    // functions (rfcs/language-evolution.md Phase 3).

    // Reject cyclic constant/alias definitions with a clear message before
    // resolution turns them into dangling self-references.
    for (name, m) in &modules {
        if let Some(c) = crate::consts::find_cycle(m) {
            return lerr(format!("module `{name}`: constant `{c}` is defined cyclically"));
        }
        if let Some(c) = crate::aliases::find_cycle(m) {
            return lerr(format!("module `{name}`: type alias `{c}` is defined cyclically"));
        }
    }

    // RFC-0002 sealing: a `capability` (a sealed type) may be CONSTRUCTED or
    // DESTRUCTURED only inside the module that declares it. Run here, while each
    // item still knows its home module (before merge flattens the namespace).
    check_sealing(&modules)?;

    // Expand type aliases and inline top-level constants per module before
    // merging, so their use sites (and any function calls inside constant values)
    // are qualified along with the bodies they expand into — no `Item::TypeAlias`
    // or `Item::Const` reaches later stages.
    modules = modules
        .into_iter()
        .map(|(n, m)| (n, crate::aliases::resolve(crate::consts::inline(m))))
        .collect();

    let mut fns: FnTable = HashMap::new();
    for (name, m) in &modules {
        let mut names = HashSet::new();
        for item in &m.items {
            if let Item::Function(f) = item {
                names.insert(f.name.clone());
            }
        }
        fns.insert(name.clone(), names);
    }

    if !modules.iter().any(|(n, _)| n == entry) {
        return lerr(format!("entry module `{entry}` not found"));
    }
    for (name, m) in &modules {
        for imp in &m.imports {
            if !fns.contains_key(imp) {
                return lerr(format!("module `{name}` imports unknown module `{imp}`"));
            }
        }
    }

    // `mode opt` is transitive: an `opt` module may depend only on other `opt`
    // modules, so the guarantee covers the whole reachable user graph, not just
    // one file. The bundled standard library is the compiler's optimized
    // substrate and is exempt; any USER import of an `opt` module must itself be
    // `opt`. (A non-`opt` module may freely import `opt` ones — the rule is
    // one-directional.)
    let is_opt = |m: &Module| m.modes.iter().any(|x| x == "opt");
    let opt_of: HashMap<&str, bool> =
        modules.iter().map(|(n, m)| (n.as_str(), is_opt(m))).collect();
    for (name, m) in &modules {
        if !is_opt(m) {
            continue;
        }
        for imp in &m.imports {
            if STD_MODULES.contains(&imp.as_str()) {
                continue;
            }
            if opt_of.get(imp.as_str()) == Some(&false) {
                return lerr(format!(
                    "`mode opt` module `{name}` imports `{imp}`, which is not `mode opt` — \
                     optimization mode is transitive, so an `opt` module may depend only on \
                     other `opt` modules (the bundled std library is exempt). Add `mode opt` \
                     to `{imp}`, or drop `mode opt` from `{name}`."
                ));
            }
        }
    }

    let mut items = Vec::new();
    for (mname, m) in &modules {
        for item in &m.items {
            match item {
                Item::Function(f) => {
                    let mut f2 = f.clone();
                    f2.name = if mname == entry && f.name == "main" {
                        "main".to_string()
                    } else {
                        format!("{mname}.{}", f.name)
                    };
                    let mut bound = HashSet::new();
                    for p in &f2.params {
                        bound.insert(p.name.clone());
                    }
                    collect_bound_block(&f2.body, &mut bound);
                    rewrite_block(&mut f2.body, mname, &m.imports, &fns, &bound)?;
                    items.push(Item::Function(f2));
                }
                Item::Type(t) => items.push(Item::Type(t.clone())),
                // Constants and aliases were resolved per-module above, so none
                // remain here.
                Item::Const { .. } | Item::TypeAlias { .. } | Item::Comptime(_) => {}
                // Traits/impls are carried into the merged module and desugared
                // after linking (see `crate::traits`). Their method bodies are
                // rewritten here, in their defining module's context, so calls
                // inside them resolve like any other function body.
                Item::Trait(t) => {
                    let mut t2 = t.clone();
                    for ms in &mut t2.methods {
                        if let Some(body) = &mut ms.default {
                            let mut bound = HashSet::new();
                            for p in &ms.params {
                                bound.insert(p.name.clone());
                            }
                            collect_bound_block(body, &mut bound);
                            rewrite_block(body, mname, &m.imports, &fns, &bound)?;
                        }
                    }
                    items.push(Item::Trait(t2));
                }
                Item::Impl(im) => {
                    let mut im2 = im.clone();
                    for method in &mut im2.methods {
                        let mut bound = HashSet::new();
                        for p in &method.params {
                            bound.insert(p.name.clone());
                        }
                        collect_bound_block(&method.body, &mut bound);
                        rewrite_block(&mut method.body, mname, &m.imports, &fns, &bound)?;
                    }
                    items.push(Item::Impl(im2));
                }
            }
        }
    }
    let mut module = Module {
        // The entry module's performance modes carry onto the linked module;
        // enforcement applies to the entry file's own (unqualified) functions.
        modes: modules
            .iter()
            .find(|(n, _)| n == entry)
            .map(|(_, m)| m.modes.clone())
            .unwrap_or_default(),
        imports: Vec::new(),
        items,
        import_lines: Vec::new(),
        item_lines: Vec::new(),
    };
    resolve_methods(&mut module);
    // Semantics-preserving constant folding over the single linked module both
    // backends consume (parity-free by construction). See src/optimize.rs. Gated
    // on the `fold` lever (RFC-0030) so the differential de-opt sweep covers it:
    // `WITCHY_OPT=-fold` leaves constant expressions unfolded.
    if crate::opt::enabled(crate::opt::Opt::Fold) {
        crate::optimize::optimize(&mut module);
    }
    // RFC-0028: turn statement-position mutating-method calls into write-backs
    // (`nodes.push(x)` => `nodes = nodes.push(x)`), once, on the shared module.
    rewrite_mut_method_stmts(&mut module);
    Ok(module)
}

/// RFC-0028 mutating-method statements: rewrite a statement-position
/// `place.method(args)` into a write-back `place = place.method(args)` when
/// `method` is a *self-returning* operation — every free function with that name
/// returns its first parameter's type — and the name is not also an `impl`/`trait`
/// method (whose overload would need the receiver type we don't infer here).
///
/// Sound because whichever overload `place.method` resolves to is self-returning,
/// so reassigning the place is type-correct; conservative because `length`
/// (`-> Int`) and any genuinely-discarded result are left as bare statements. One
/// edit on the single linked module, so both backends agree by construction and
/// the uniqueness pass then keeps the reassignment in place.
fn rewrite_mut_method_stmts(module: &mut Module) {
    // Method names an impl/trait declares — skip auto-rewrite (the right overload
    // depends on the receiver type, which is not resolved at link time).
    let mut shadowed: HashSet<String> = HashSet::new();
    for item in &module.items {
        match item {
            Item::Impl(im) => {
                for m in &im.methods {
                    shadowed.insert(bare_method_name(&m.name));
                }
            }
            Item::Trait(tr) => {
                for m in &tr.methods {
                    shadowed.insert(m.name.clone());
                }
            }
            _ => {}
        }
    }
    // bare method name -> (any free function seen, all of them self-returning).
    let mut sigs: HashMap<String, (bool, bool)> = HashMap::new();
    for item in &module.items {
        if let Item::Function(f) = item {
            let entry = sigs.entry(bare_method_name(&f.name)).or_insert((false, true));
            entry.0 = true;
            entry.1 = entry.1 && fn_is_self_returning(f);
        }
    }
    let eligible = |method: &str| -> bool {
        !shadowed.contains(method)
            && sigs.get(method).is_some_and(|&(seen, all_self_ret)| seen && all_self_ret)
    };
    for item in &mut module.items {
        if let Item::Function(f) = item {
            // A write-back targets a mutable place, so scope starts with the `var`
            // parameters (only those are reassignable) and tracks `var` lets.
            let mutables: HashSet<String> = f
                .params
                .iter()
                .filter(|p| p.convention == Convention::Var)
                .map(|p| p.name.clone())
                .collect();
            // The function body's tail expression IS its return value, so the
            // final statement is in value position (must not be rewritten).
            rewrite_mut_block(&mut f.body, &eligible, &mutables, true);
        }
    }
}

/// The last `.`-separated segment of a function name (`list.push` -> `push`).
fn bare_method_name(name: &str) -> String {
    name.rsplit('.').next().unwrap_or(name).to_string()
}

/// Does `f` return exactly its first parameter's (declared) type? Both must be
/// declared; generic `list.push(xs: List(a), …) -> List(a)` qualifies.
fn fn_is_self_returning(f: &Function) -> bool {
    matches!(
        (f.params.first().and_then(|p| p.ty.as_ref()), f.ret.as_ref()),
        (Some(pt), Some(rt)) if pt == rt
    )
}

/// A place whose base variable is a known mutable (`var`) binding — the only
/// kind a write-back can target. `let`/`Borrow`/`Own` bases, loop variables,
/// pattern bindings, and lambda parameters are all immutable, so a statement on
/// one stays a bare discard (its prior meaning), never a wrong assignment.
fn place_base_is_mutable(e: &Expr, mutables: &HashSet<String>) -> bool {
    match e {
        Expr::Var(x) => mutables.contains(x),
        Expr::Index { base, .. } | Expr::Field { base, .. } => {
            place_base_is_mutable(base, mutables)
        }
        _ => false,
    }
}

fn rewrite_mut_block(
    b: &mut Block,
    eligible: &impl Fn(&str) -> bool,
    outer: &HashSet<String>,
    tail_is_value: bool,
) {
    let mut mutables = outer.clone();
    let last = b.stmts.len().wrapping_sub(1);
    for (i, stmt) in b.stmts.iter_mut().enumerate() {
        // The final statement of a value-position block IS the block's value
        // (a function's return, an `if`/`match` arm used as an expression). Its
        // result is consumed, so it must NOT be turned into a (Nil-valued)
        // write-back; only genuinely-discarded statements are rewritten.
        let value_used = i == last && tail_is_value;
        let replacement = match stmt {
            Stmt::Expr(Expr::MethodCall { receiver, method, args })
                if !value_used
                    && place_base_is_mutable(receiver, &mutables)
                    && eligible(&bare_method_name(method)) =>
            {
                let place = (**receiver).clone();
                let call = Expr::MethodCall {
                    receiver: receiver.clone(),
                    method: method.clone(),
                    args: args.clone(),
                };
                crate::parser::desugar_place_assign(place, call).ok()
            }
            _ => None,
        };
        if let Some(new_stmt) = replacement {
            *stmt = new_stmt;
        }
        rewrite_mut_stmt_children(stmt, eligible, &mutables, value_used);
        // A `var` let makes its name mutable for the rest of this block; any other
        // binding form shadows (un-mutables) an outer name of the same name.
        match stmt {
            Stmt::Let { name, mutable: true, .. } => {
                mutables.insert(name.clone());
            }
            Stmt::Let { name, mutable: false, .. } => {
                mutables.remove(name);
            }
            Stmt::LetPattern { pattern, .. } => {
                let mut names = Vec::new();
                crate::ast::pattern_binds(pattern, &mut names);
                for n in &names {
                    mutables.remove(n);
                }
            }
            _ => {}
        }
    }
}

fn rewrite_mut_stmt_children(
    s: &mut Stmt,
    eligible: &impl Fn(&str) -> bool,
    mutables: &HashSet<String>,
    value_used: bool,
) {
    match s {
        // An expression-statement's value is used only when it is the block's
        // consumed tail; every other statement's value expression is always used.
        Stmt::Expr(value) => rewrite_mut_expr(value, eligible, mutables, value_used),
        Stmt::Let { value, .. }
        | Stmt::Assign { value, .. }
        | Stmt::LetPattern { value, .. }
        | Stmt::Yield(value) => rewrite_mut_expr(value, eligible, mutables, true),
        Stmt::Return(Some(e)) => rewrite_mut_expr(e, eligible, mutables, true),
        Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
    }
}

/// `scope` with `bound` removed — entering a sub-scope that binds those names
/// immutably (loop variable, pattern, lambda parameter) shadows any outer `var`.
fn without(scope: &HashSet<String>, bound: &HashSet<String>) -> HashSet<String> {
    scope.difference(bound).cloned().collect()
}

/// `value_position` is whether this expression's value is consumed — it flows to
/// the nested blocks so each one knows if its tail statement is a value (an
/// `if`/`match` arm inherits the surrounding position; a loop body never is).
fn rewrite_mut_expr(
    e: &mut Expr,
    eligible: &impl Fn(&str) -> bool,
    mutables: &HashSet<String>,
    value_position: bool,
) {
    match e {
        Expr::If { cond, then_block, else_block } => {
            rewrite_mut_expr(cond, eligible, mutables, true);
            rewrite_mut_block(then_block, eligible, mutables, value_position);
            if let Some(b) = else_block {
                rewrite_mut_block(b, eligible, mutables, value_position);
            }
        }
        Expr::Match { scrutinee, arms } => {
            rewrite_mut_expr(scrutinee, eligible, mutables, true);
            for a in arms {
                let mut bound = HashSet::new();
                collect_pattern_vars(&a.pattern, &mut bound);
                let arm_scope = without(mutables, &bound);
                if let Some(g) = &mut a.guard {
                    rewrite_mut_expr(g, eligible, &arm_scope, true);
                }
                rewrite_mut_expr(&mut a.body, eligible, &arm_scope, value_position);
            }
        }
        Expr::Block(b) => rewrite_mut_block(b, eligible, mutables, value_position),
        Expr::While { cond, body } => {
            rewrite_mut_expr(cond, eligible, mutables, true);
            // A loop evaluates to Nil, so its body's tail value is discarded.
            rewrite_mut_block(body, eligible, mutables, false);
        }
        Expr::For { var, iter, body } => {
            rewrite_mut_expr(iter, eligible, mutables, true);
            // The loop variable is an immutable fresh binding each iteration.
            let body_scope = without(mutables, &HashSet::from([var.clone()]));
            rewrite_mut_block(body, eligible, &body_scope, false);
        }
        Expr::WhileLet { pattern, scrutinee, body } => {
            rewrite_mut_expr(scrutinee, eligible, mutables, true);
            let mut bound = HashSet::new();
            collect_pattern_vars(pattern, &mut bound);
            rewrite_mut_block(body, eligible, &without(mutables, &bound), false);
        }
        Expr::Lambda { params, body, .. } => {
            // A closure captures by value (captures are immutable) and its params
            // are immutable, so only its own `var` lets become mutable inside it;
            // its tail IS the closure's return value.
            let bound: HashSet<String> = params.iter().map(|p| p.name.clone()).collect();
            rewrite_mut_block(body, eligible, &without(&HashSet::new(), &bound), true);
        }
        Expr::Call { args, .. }
        | Expr::Ctor { args, .. }
        | Expr::List(args)
        | Expr::Tuple(args) => {
            for a in args {
                rewrite_mut_expr(a, eligible, mutables, true);
            }
        }
        Expr::Apply { func, args } => {
            rewrite_mut_expr(func, eligible, mutables, true);
            for a in args {
                rewrite_mut_expr(a, eligible, mutables, true);
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            rewrite_mut_expr(lhs, eligible, mutables, true);
            rewrite_mut_expr(rhs, eligible, mutables, true);
        }
        Expr::Range { lo, hi, .. } => {
            rewrite_mut_expr(lo, eligible, mutables, true);
            rewrite_mut_expr(hi, eligible, mutables, true);
        }
        Expr::Index { base, index } => {
            rewrite_mut_expr(base, eligible, mutables, true);
            rewrite_mut_expr(index, eligible, mutables, true);
        }
        Expr::MethodCall { receiver, args, .. } => {
            rewrite_mut_expr(receiver, eligible, mutables, true);
            for a in args {
                rewrite_mut_expr(a, eligible, mutables, true);
            }
        }
        Expr::Unary { expr, .. }
        | Expr::Try(expr)
        | Expr::As { expr, .. }
        | Expr::Field { base: expr, .. } => rewrite_mut_expr(expr, eligible, mutables, true),
        Expr::RecordUpdate { base, fields } => {
            rewrite_mut_expr(base, eligible, mutables, true);
            for (_, v) in fields {
                rewrite_mut_expr(v, eligible, mutables, true);
            }
        }
        _ => {}
    }
}

/// The (nominal) type name of a `Type`, if it has one.
fn type_name(t: &Type) -> Option<String> {
    match t {
        Type::Named(n, _) => Some(n.clone()),
        _ => None,
    }
}

/// Per-function nominal signature: first-parameter type name and return type
/// name, used to resolve overloaded UFCS method calls by the receiver's type.
struct FnSig {
    first_param: Option<String>,
    ret: Option<String>,
}

/// Resolve UFCS method calls the linker left unqualified because the bare name is
/// provided by several imported modules (e.g. `get` in http/server/json). For
/// each such `name(receiver, ...)`, pick the `mod.name` whose first parameter
/// type matches the receiver's nominal type. The receiver's type is read from
/// the function it calls (its return type) or a literal — so chains like
/// `router().get(...).layer(...)` resolve left to right. A receiver whose type
/// can't be determined (e.g. a plain variable) is left for the type checker.
fn resolve_methods(module: &mut Module) {
    let mut sig: HashMap<String, FnSig> = HashMap::new();
    // base method name -> the qualified function names providing it.
    let mut by_base: HashMap<String, Vec<String>> = HashMap::new();
    for item in &module.items {
        if let Item::Function(f) = item {
            let first_param = f.params.first().and_then(|p| p.ty.as_ref()).and_then(type_name);
            let ret = f.ret.as_ref().and_then(type_name);
            sig.insert(f.name.clone(), FnSig { first_param, ret });
            if let Some((_, base)) = f.name.rsplit_once('.') {
                by_base.entry(base.to_string()).or_default().push(f.name.clone());
            }
        }
    }
    for item in &mut module.items {
        match item {
            Item::Function(f) => {
                let mut vars = param_vars(&f.params);
                resolve_in_block(&mut f.body, &sig, &by_base, &mut vars);
            }
            Item::Trait(t) => {
                for ms in &mut t.methods {
                    if let Some(body) = &mut ms.default {
                        let mut vars = param_vars(&ms.params);
                        resolve_in_block(body, &sig, &by_base, &mut vars);
                    }
                }
            }
            Item::Impl(im) => {
                for method in &mut im.methods {
                    let mut vars = param_vars(&method.params);
                    resolve_in_block(&mut method.body, &sig, &by_base, &mut vars);
                }
            }
            Item::Type(_) | Item::Const { .. } | Item::TypeAlias { .. } | Item::Comptime(_) => {}
        }
    }
}

/// Seed a variable-type scope from a function's parameters (nominal types only).
fn param_vars(params: &[Param]) -> HashMap<String, String> {
    let mut m = HashMap::new();
    for p in params {
        if let Some(n) = p.ty.as_ref().and_then(type_name) {
            m.insert(p.name.clone(), n);
        }
    }
    m
}

/// The nominal type an expression evaluates to, where the linker can tell: a
/// call's return type, a literal's type, or a variable whose type was tracked.
fn expr_nominal_type(
    e: &Expr,
    sig: &HashMap<String, FnSig>,
    vars: &HashMap<String, String>,
) -> Option<String> {
    match e {
        Expr::Call { name, .. } => sig.get(name).and_then(|s| s.ret.clone()),
        Expr::Var(n) => vars.get(n).cloned(),
        Expr::Int(_) => Some("Int".to_string()),
        Expr::Float(_) => Some("Float".to_string()),
        Expr::Duration(_) => Some("Duration".to_string()),
        Expr::Str(_) => Some("String".to_string()),
        Expr::Bool(_) => Some("Bool".to_string()),
        _ => None,
    }
}

fn resolve_in_block(
    b: &mut Block,
    sig: &HashMap<String, FnSig>,
    by_base: &HashMap<String, Vec<String>>,
    vars: &mut HashMap<String, String>,
) {
    for stmt in &mut b.stmts {
        match stmt {
            Stmt::Let { name, value, .. } => {
                resolve_in_expr(value, sig, by_base, vars);
                // Track the binding's nominal type so later `name.method(...)`
                // calls (a step-by-step builder) resolve by it.
                if let Some(t) = expr_nominal_type(value, sig, vars) {
                    vars.insert(name.clone(), t);
                }
            }
            Stmt::Assign { name, value } => {
                resolve_in_expr(value, sig, by_base, vars);
                match expr_nominal_type(value, sig, vars) {
                    Some(t) => {
                        vars.insert(name.clone(), t);
                    }
                    None => {
                        vars.remove(name);
                    }
                }
            }
            Stmt::LetPattern { value, .. } => resolve_in_expr(value, sig, by_base, vars),
            Stmt::Return(Some(e)) | Stmt::Expr(e) | Stmt::Yield(e) => resolve_in_expr(e, sig, by_base, vars),
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
    }
}

fn resolve_in_expr(
    e: &mut Expr,
    sig: &HashMap<String, FnSig>,
    by_base: &HashMap<String, Vec<String>>,
    vars: &mut HashMap<String, String>,
) {
    match e {
        Expr::Call { name, args } => {
            // Resolve nested receivers/arguments first, so a chained receiver's
            // call is already resolved when we read its return type.
            for a in args.iter_mut() {
                resolve_in_expr(a, sig, by_base, vars);
            }
            if !name.contains('.') && !sig.contains_key(name.as_str()) {
                if let Some(cands) = by_base.get(name.as_str()) {
                    if cands.len() > 1 {
                        if let Some(recv) = args.first().and_then(|a| expr_nominal_type(a, sig, vars))
                        {
                            let matches: Vec<&String> = cands
                                .iter()
                                .filter(|c| {
                                    sig.get(*c).and_then(|s| s.first_param.as_deref())
                                        == Some(recv.as_str())
                                })
                                .collect();
                            if let [only] = matches.as_slice() {
                                *name = (*only).clone();
                            }
                        }
                    }
                }
            }
        }
        Expr::Apply { func, args } => {
            resolve_in_expr(func, sig, by_base, vars);
            for a in args.iter_mut() {
                resolve_in_expr(a, sig, by_base, vars);
            }
        }
        Expr::Ctor { args, .. } | Expr::List(args) | Expr::Tuple(args) => {
            for a in args.iter_mut() {
                resolve_in_expr(a, sig, by_base, vars);
            }
        }
        Expr::Unary { expr, .. } | Expr::Try(expr) | Expr::As { expr, .. } | Expr::Field { base: expr, .. } => {
            resolve_in_expr(expr, sig, by_base, vars)
        }
        Expr::Binary { lhs, rhs, .. } => {
            resolve_in_expr(lhs, sig, by_base, vars);
            resolve_in_expr(rhs, sig, by_base, vars);
        }
        Expr::Range { lo, hi, .. } => {
            resolve_in_expr(lo, sig, by_base, vars);
            resolve_in_expr(hi, sig, by_base, vars);
        }
        Expr::Index { base, index } => {
            resolve_in_expr(base, sig, by_base, vars);
            resolve_in_expr(index, sig, by_base, vars);
        }
        // Lowered to a plain `Call` before resolution; recurse for safety.
        Expr::MethodCall { receiver, args, .. } => {
            resolve_in_expr(receiver, sig, by_base, vars);
            for a in args.iter_mut() {
                resolve_in_expr(a, sig, by_base, vars);
            }
        }
        Expr::WhileLet { scrutinee, body, .. } => {
            resolve_in_expr(scrutinee, sig, by_base, vars);
            resolve_in_block(body, sig, by_base, vars);
        }
        Expr::RecordUpdate { base, fields } => {
            resolve_in_expr(base, sig, by_base, vars);
            for (_, v) in fields.iter_mut() {
                resolve_in_expr(v, sig, by_base, vars);
            }
        }
        Expr::Record { fields, spread, .. } => {
            for (_, v) in fields.iter_mut() {
                resolve_in_expr(v, sig, by_base, vars);
            }
            if let Some(s) = spread {
                resolve_in_expr(s, sig, by_base, vars);
            }
        }
        Expr::If { cond, then_block, else_block } => {
            resolve_in_expr(cond, sig, by_base, vars);
            resolve_in_block(then_block, sig, by_base, vars);
            if let Some(b) = else_block {
                resolve_in_block(b, sig, by_base, vars);
            }
        }
        Expr::Block(b) => resolve_in_block(b, sig, by_base, vars),
        Expr::While { cond, body } => {
            resolve_in_expr(cond, sig, by_base, vars);
            resolve_in_block(body, sig, by_base, vars);
        }
        Expr::For { iter, body, .. } => {
            resolve_in_expr(iter, sig, by_base, vars);
            resolve_in_block(body, sig, by_base, vars);
        }
        Expr::Match { scrutinee, arms } => {
            resolve_in_expr(scrutinee, sig, by_base, vars);
            for arm in arms.iter_mut() {
                if let Some(g) = &mut arm.guard {
                    resolve_in_expr(g, sig, by_base, vars);
                }
                resolve_in_expr(&mut arm.body, sig, by_base, vars);
            }
        }
        Expr::Lambda { body, .. } => resolve_in_block(body, sig, by_base, vars),
        Expr::Int(_)
        | Expr::Duration(_)
        | Expr::Float(_)
        | Expr::Str(_)
        | Expr::Bool(_)
        | Expr::Var(_)
        | Expr::TaggedLit { .. } => {}
    }
}

/// Collect every name bound as a local within a block — `let`/`var` bindings,
/// tuple destructurings, `for` loop variables, lambda parameters, and `match`
/// pattern bindings (recursively, including nested blocks/expressions). Used so
/// the linker never mistakes a local that shadows a same-module function name
/// for a first-class reference to that function.
fn collect_bound_block(b: &Block, out: &mut HashSet<String>) {
    for stmt in &b.stmts {
        match stmt {
            Stmt::Let { name, value, .. } => {
                out.insert(name.clone());
                collect_bound_expr(value, out);
            }
            Stmt::LetPattern { pattern, value } => {
                let mut names = Vec::new();
                crate::ast::pattern_binds(pattern, &mut names);
                for n in names {
                    out.insert(n);
                }
                collect_bound_expr(value, out);
            }
            Stmt::Assign { value, .. } => collect_bound_expr(value, out),
            Stmt::Return(Some(e)) | Stmt::Expr(e) | Stmt::Yield(e) => collect_bound_expr(e, out),
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
    }
}

fn collect_bound_expr(e: &Expr, out: &mut HashSet<String>) {
    match e {
        Expr::Lambda { params, body, .. } => {
            for p in params {
                out.insert(p.name.clone());
            }
            collect_bound_block(body, out);
        }
        Expr::For { var, iter, body } => {
            out.insert(var.clone());
            collect_bound_expr(iter, out);
            collect_bound_block(body, out);
        }
        Expr::Match { scrutinee, arms } => {
            collect_bound_expr(scrutinee, out);
            for arm in arms {
                collect_pattern_vars(&arm.pattern, out);
                if let Some(g) = &arm.guard {
                    collect_bound_expr(g, out);
                }
                collect_bound_expr(&arm.body, out);
            }
        }
        Expr::If { cond, then_block, else_block } => {
            collect_bound_expr(cond, out);
            collect_bound_block(then_block, out);
            if let Some(b) = else_block {
                collect_bound_block(b, out);
            }
        }
        Expr::While { cond, body } => {
            collect_bound_expr(cond, out);
            collect_bound_block(body, out);
        }
        Expr::Block(b) => collect_bound_block(b, out),
        Expr::Call { args, .. }
        | Expr::Ctor { args, .. }
        | Expr::List(args)
        | Expr::Tuple(args) => {
            for a in args {
                collect_bound_expr(a, out);
            }
        }
        Expr::Apply { func, args } => {
            collect_bound_expr(func, out);
            for a in args {
                collect_bound_expr(a, out);
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            collect_bound_expr(lhs, out);
            collect_bound_expr(rhs, out);
        }
        Expr::Range { lo, hi, .. } => {
            collect_bound_expr(lo, out);
            collect_bound_expr(hi, out);
        }
        Expr::Index { base, index } => {
            collect_bound_expr(base, out);
            collect_bound_expr(index, out);
        }
        Expr::MethodCall { receiver, args, .. } => {
            collect_bound_expr(receiver, out);
            for a in args {
                collect_bound_expr(a, out);
            }
        }
        Expr::WhileLet { pattern, scrutinee, body } => {
            collect_bound_expr(scrutinee, out);
            collect_pattern_vars(pattern, out);
            collect_bound_block(body, out);
        }
        Expr::Unary { expr, .. } | Expr::Try(expr) | Expr::As { expr, .. } | Expr::Field { base: expr, .. } => {
            collect_bound_expr(expr, out)
        }
        Expr::RecordUpdate { base, fields } => {
            collect_bound_expr(base, out);
            for (_, v) in fields {
                collect_bound_expr(v, out);
            }
        }
        Expr::Record { fields, spread, .. } => {
            for (_, v) in fields {
                collect_bound_expr(v, out);
            }
            if let Some(s) = spread {
                collect_bound_expr(s, out);
            }
        }
        Expr::Var(_) | Expr::Int(_) | Expr::Duration(_) | Expr::Float(_) | Expr::Str(_) | Expr::Bool(_) | Expr::TaggedLit { .. } => {}
    }
}

fn rewrite_block(
    b: &mut Block,
    m: &str,
    imps: &[String],
    fns: &FnTable,
    bound: &HashSet<String>,
) -> Result<(), LinkError> {
    for stmt in &mut b.stmts {
        match stmt {
            Stmt::Let { value, .. }
            | Stmt::Assign { value, .. }
            | Stmt::LetPattern { value, .. } => rewrite_expr(value, m, imps, fns, bound)?,
            Stmt::Return(Some(e)) | Stmt::Expr(e) | Stmt::Yield(e) => rewrite_expr(e, m, imps, fns, bound)?,
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
    }
    Ok(())
}

fn rewrite_expr(
    e: &mut Expr,
    m: &str,
    imps: &[String],
    fns: &FnTable,
    bound: &HashSet<String>,
) -> Result<(), LinkError> {
    match e {
        Expr::Call { name, args } => {
            *name = resolve_call(name, m, imps, fns, bound)?;
            for a in args {
                rewrite_expr(a, m, imps, fns, bound)?;
            }
        }
        // A bare name matching a same-module function is a first-class reference
        // to it; qualify it like a call — unless it is shadowed by a local of the
        // same name (a parameter, `let`, loop variable, or pattern binding).
        Expr::Var(name) => {
            if !bound.contains(name.as_str())
                && fns.get(m).is_some_and(|s| s.contains(name.as_str()))
            {
                *name = format!("{m}.{name}");
            }
        }
        Expr::Apply { func, args } => {
            rewrite_expr(func, m, imps, fns, bound)?;
            for a in args {
                rewrite_expr(a, m, imps, fns, bound)?;
            }
        }
        Expr::Ctor { args, .. } | Expr::List(args) | Expr::Tuple(args) => {
            for a in args {
                rewrite_expr(a, m, imps, fns, bound)?;
            }
        }
        Expr::Unary { expr, .. } | Expr::Try(expr) | Expr::As { expr, .. } | Expr::Field { base: expr, .. } => {
            rewrite_expr(expr, m, imps, fns, bound)?
        }
        Expr::RecordUpdate { base, fields } => {
            rewrite_expr(base, m, imps, fns, bound)?;
            for (_, value) in fields {
                rewrite_expr(value, m, imps, fns, bound)?;
            }
        }
        Expr::Record { fields, spread, .. } => {
            for (_, value) in fields {
                rewrite_expr(value, m, imps, fns, bound)?;
            }
            if let Some(s) = spread {
                rewrite_expr(s, m, imps, fns, bound)?;
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            rewrite_expr(lhs, m, imps, fns, bound)?;
            rewrite_expr(rhs, m, imps, fns, bound)?;
        }
        Expr::Range { lo, hi, .. } => {
            rewrite_expr(lo, m, imps, fns, bound)?;
            rewrite_expr(hi, m, imps, fns, bound)?;
        }
        Expr::Index { base, index } => {
            rewrite_expr(base, m, imps, fns, bound)?;
            rewrite_expr(index, m, imps, fns, bound)?;
        }
        // Lowered to a plain `Call` before this runs; recurse for safety.
        Expr::MethodCall { receiver, args, .. } => {
            rewrite_expr(receiver, m, imps, fns, bound)?;
            for a in args {
                rewrite_expr(a, m, imps, fns, bound)?;
            }
        }
        Expr::WhileLet { scrutinee, body, .. } => {
            rewrite_expr(scrutinee, m, imps, fns, bound)?;
            rewrite_block(body, m, imps, fns, bound)?;
        }
        Expr::If {
            cond,
            then_block,
            else_block,
        } => {
            rewrite_expr(cond, m, imps, fns, bound)?;
            rewrite_block(then_block, m, imps, fns, bound)?;
            if let Some(b) = else_block {
                rewrite_block(b, m, imps, fns, bound)?;
            }
        }
        Expr::Lambda { body, .. } => rewrite_block(body, m, imps, fns, bound)?,
        Expr::Block(b) => rewrite_block(b, m, imps, fns, bound)?,
        Expr::While { cond, body } => {
            rewrite_expr(cond, m, imps, fns, bound)?;
            rewrite_block(body, m, imps, fns, bound)?;
        }
        Expr::For { iter, body, .. } => {
            rewrite_expr(iter, m, imps, fns, bound)?;
            rewrite_block(body, m, imps, fns, bound)?;
        }
        Expr::Match { scrutinee, arms } => {
            rewrite_expr(scrutinee, m, imps, fns, bound)?;
            for arm in arms {
                if let Some(g) = &mut arm.guard {
                    rewrite_expr(g, m, imps, fns, bound)?;
                }
                rewrite_expr(&mut arm.body, m, imps, fns, bound)?;
            }
        }
        Expr::Int(_) | Expr::Duration(_) | Expr::Float(_) | Expr::Str(_) | Expr::Bool(_) | Expr::TaggedLit { .. } => {}
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// RFC-0002 sealing: a `capability` (sealed) type may only be constructed
// (`X(..)`) or destructured (`match _: X(..)`) inside the module that declares
// it. Every other module may hold, pass, and return a value of `X` — but cannot
// mint or unwrap one, so the brand is un-forgeable like the host capabilities it
// refines. Checked here (a read-only walk mirroring `rewrite_expr`'s complete
// traversal, plus pattern walking the rewriter skips) before names are merged.
// ---------------------------------------------------------------------------

fn check_sealing(modules: &[(String, Module)]) -> Result<(), LinkError> {
    let mut sealed: HashMap<String, String> = HashMap::new();
    for (mname, m) in modules {
        for item in &m.items {
            if let Item::Type(t) = item {
                if t.sealed {
                    sealed.insert(t.name.clone(), mname.clone());
                }
            }
        }
    }
    if sealed.is_empty() {
        return Ok(());
    }
    for (mname, m) in modules {
        for item in &m.items {
            match item {
                Item::Function(f) => seal_block(&f.body, &sealed, mname)?,
                Item::Impl(im) => {
                    for method in &im.methods {
                        seal_block(&method.body, &sealed, mname)?;
                    }
                }
                _ => {}
            }
        }
    }
    Ok(())
}

fn seal_use(
    name: &str,
    sealed: &HashMap<String, String>,
    home: &str,
    verb: &str,
) -> Result<(), LinkError> {
    if let Some(decl) = sealed.get(name) {
        if decl != home {
            return lerr(format!(
                "`{name}` is a sealed capability declared in module `{decl}`; module \
                 `{home}` may hold and pass a `{name}` but cannot {verb} one — only \
                 `{decl}` can mint or unwrap it (use the functions `{decl}` exports)"
            ));
        }
    }
    Ok(())
}

fn seal_block(b: &Block, sealed: &HashMap<String, String>, home: &str) -> Result<(), LinkError> {
    for stmt in &b.stmts {
        match stmt {
            Stmt::Let { value, .. } | Stmt::Assign { value, .. } | Stmt::LetPattern { value, .. } => {
                seal_expr(value, sealed, home)?
            }
            Stmt::Return(Some(e)) | Stmt::Expr(e) | Stmt::Yield(e) => seal_expr(e, sealed, home)?,
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
    }
    Ok(())
}

fn seal_pattern(p: &Pattern, sealed: &HashMap<String, String>, home: &str) -> Result<(), LinkError> {
    match p {
        Pattern::Ctor { name, args } => {
            seal_use(name, sealed, home, "destructure")?;
            for a in args {
                seal_pattern(a, sealed, home)?;
            }
        }
        Pattern::Tuple(ps) => {
            for q in ps {
                seal_pattern(q, sealed, home)?;
            }
        }
        Pattern::List { elems, .. } => {
            for q in elems {
                seal_pattern(q, sealed, home)?;
            }
        }
        Pattern::Or(alts) => {
            for q in alts {
                seal_pattern(q, sealed, home)?;
            }
        }
        Pattern::Wildcard
        | Pattern::Var(_)
        | Pattern::Int(_)
        | Pattern::Str(_)
        | Pattern::Bool(_)
        | Pattern::Duration(_)
        | Pattern::IntRange { .. } => {}
    }
    Ok(())
}

fn seal_expr(e: &Expr, sealed: &HashMap<String, String>, home: &str) -> Result<(), LinkError> {
    match e {
        Expr::Ctor { name, args } => {
            seal_use(name, sealed, home, "construct")?;
            for a in args {
                seal_expr(a, sealed, home)?;
            }
        }
        Expr::Call { args, .. } | Expr::List(args) | Expr::Tuple(args) => {
            for a in args {
                seal_expr(a, sealed, home)?;
            }
        }
        Expr::Apply { func, args } => {
            seal_expr(func, sealed, home)?;
            for a in args {
                seal_expr(a, sealed, home)?;
            }
        }
        Expr::MethodCall { receiver, args, .. } => {
            seal_expr(receiver, sealed, home)?;
            for a in args {
                seal_expr(a, sealed, home)?;
            }
        }
        Expr::Unary { expr, .. }
        | Expr::Try(expr)
        | Expr::As { expr, .. }
        | Expr::Field { base: expr, .. } => seal_expr(expr, sealed, home)?,
        Expr::RecordUpdate { base, fields } => {
            seal_expr(base, sealed, home)?;
            for (_, v) in fields {
                seal_expr(v, sealed, home)?;
            }
        }
        Expr::Record { fields, spread, .. } => {
            for (_, v) in fields {
                seal_expr(v, sealed, home)?;
            }
            if let Some(s) = spread {
                seal_expr(s, sealed, home)?;
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            seal_expr(lhs, sealed, home)?;
            seal_expr(rhs, sealed, home)?;
        }
        Expr::Range { lo, hi, .. } => {
            seal_expr(lo, sealed, home)?;
            seal_expr(hi, sealed, home)?;
        }
        Expr::Index { base, index } => {
            seal_expr(base, sealed, home)?;
            seal_expr(index, sealed, home)?;
        }
        Expr::WhileLet { pattern, scrutinee, body } => {
            seal_pattern(pattern, sealed, home)?;
            seal_expr(scrutinee, sealed, home)?;
            seal_block(body, sealed, home)?;
        }
        Expr::If { cond, then_block, else_block } => {
            seal_expr(cond, sealed, home)?;
            seal_block(then_block, sealed, home)?;
            if let Some(b) = else_block {
                seal_block(b, sealed, home)?;
            }
        }
        Expr::Lambda { body, .. } => seal_block(body, sealed, home)?,
        Expr::Block(b) => seal_block(b, sealed, home)?,
        Expr::While { cond, body } => {
            seal_expr(cond, sealed, home)?;
            seal_block(body, sealed, home)?;
        }
        Expr::For { iter, body, .. } => {
            seal_expr(iter, sealed, home)?;
            seal_block(body, sealed, home)?;
        }
        Expr::Match { scrutinee, arms } => {
            seal_expr(scrutinee, sealed, home)?;
            for arm in arms {
                seal_pattern(&arm.pattern, sealed, home)?;
                if let Some(g) = &arm.guard {
                    seal_expr(g, sealed, home)?;
                }
                seal_expr(&arm.body, sealed, home)?;
            }
        }
        Expr::Var(_)
        | Expr::Int(_)
        | Expr::Duration(_)
        | Expr::Float(_)
        | Expr::Str(_)
        | Expr::Bool(_)
        | Expr::TaggedLit { .. } => {}
    }
    Ok(())
}

fn resolve_call(
    name: &str,
    m: &str,
    imps: &[String],
    fns: &FnTable,
    bound: &HashSet<String>,
) -> Result<String, LinkError> {
    if let Some((modname, fname)) = name.split_once('.') {
        // The prelude modules are importable-by-default everywhere (the link
        // set always carries them), including from inside one another.
        let prelude = matches!(modname, "list" | "string" | "dict" | "math" | "option" | "result");
        if !prelude && !imps.iter().any(|i| i == modname) {
            return lerr(format!(
                "module `{m}` calls `{modname}.{fname}` but does not `import {modname}`"
            ));
        }
        return match fns.get(modname) {
            Some(s) if s.contains(fname) => Ok(name.to_string()),
            _ => lerr(format!("module `{modname}` has no function `{fname}`")),
        };
    }
    // A function defined in THIS module wins over a builtin of the same name, so
    // e.g. `list.contains` is reachable as a bare `contains` inside `list` (a
    // builtin would otherwise shadow it). Checked before BUILTINS for that
    // reason.
    if fns.get(m).is_some_and(|s| s.contains(name)) {
        return Ok(format!("{m}.{name}"));
    }
    if BUILTINS.contains(&name) {
        return Ok(name.to_string());
    }
    // A bare name (e.g. from `recv.method(...)` UFCS sugar) may name a function
    // in an imported module. Resolve it when exactly one import provides it and
    // it isn't shadowed by a local binding; ambiguity across imports is an error.
    if !bound.contains(name) {
        let mut providers: Vec<&str> = Vec::new();
        for imp in imps {
            if fns.get(imp).is_some_and(|s| s.contains(name)) {
                providers.push(imp.as_str());
            }
        }
        // Exactly one import provides it -> resolve. If several do, it's an
        // overloaded method name (e.g. http.get / server.get / json.get): leave
        // it unqualified for the post-link `resolve_methods` pass to pick by the
        // receiver's type.
        if providers.len() == 1 {
            return Ok(format!("{}.{name}", providers[0]));
        }
    }
    // Not a function here and not a builtin: a local binding being applied (e.g.
    // a lambda parameter). Leave it unqualified; the type checker decides.
    Ok(name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggests_near_miss_std_module_and_function() {
        assert_eq!(closest_std_module("lst"), Some("list"));
        assert_eq!(closest_std_module("str/ng"), Some("string"));
        assert_eq!(closest_std_module("zzz"), None); // not close to anything
        assert_eq!(closest_std_module("qq"), None); // too short to over-match

        // `map` lives in list (and option); a near miss resolves to a real name.
        assert!(closest_std_function("mep").is_some());
        assert_eq!(closest_std_function("zzzzzz"), None);
    }

    #[test]
    fn which_finds_functions_by_name_fragment_and_abbreviation() {
        // Exact: module-qualified signature with its doc line.
        let split = std_signatures("split");
        assert!(split.iter().any(|s| s.starts_with("string.split(")), "{split:?}");
        // Substring: `pad` lists both pads.
        let pad = std_signatures("pad");
        assert!(pad.iter().any(|s| s.starts_with("string.pad_left(")), "{pad:?}");
        assert!(pad.iter().any(|s| s.starts_with("string.pad_right(")), "{pad:?}");
        // Abbreviation: the round-3 learner guessed `to_ms`.
        let ms = std_signatures("to_ms");
        assert!(
            ms.iter().any(|s| s.starts_with("duration.to_milliseconds(")),
            "{ms:?}"
        );
        assert!(std_signatures("zzz_nothing").is_empty());
    }
}
