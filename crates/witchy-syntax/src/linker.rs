//! Module linker.
//!
//! Combines a set of named modules into one flat `Module`, qualifying each
//! module's function names (`mod.func`) and rewriting call sites so an
//! unqualified call resolves to the same module or an explicit `from` import,
//! and a `mod.func` call resolves to an imported module. Importing is purely
//! declarative: it brings names into scope, runs no code, and confers no
//! authority — a dependency can only act through capabilities the caller passes
//! to its functions (visible in their types).
//!
//! v1: functions are module-scoped; types/constructors share one global
//! namespace.

// foldhash: compiler-internal keys only — see witchy-types/src/typeck.rs.
use foldhash::{HashMap, HashMapExt as _, HashSet, HashSetExt as _};
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

/// For each module, its exported function names mapped to the signature facts
/// RFC-0050 Part-2 eta-expansion needs: the function's FULL declared arity and
/// whether it is a Nil-returning `var`-procedure (which cannot become a value —
/// see the `Expr::Field` arm of `rewrite_expr`). Membership (`contains_key`) is
/// still the "does module X export `f`" question the rest of the linker asks.
type FnTable = HashMap<String, HashMap<String, EtaSig>>;

/// For each module, the bare function names introduced explicitly by
/// `from X import f`, mapped to their exporting module.
type BareFnImports = HashMap<String, HashMap<String, String>>;

/// The per-function facts eta-expansion consumes (RFC-0050 Part 2).
#[derive(Clone, Copy)]
struct EtaSig {
    /// Number of declared parameters — the arity of the eta-expanded lambda.
    /// Includes RFC-0056 defaulted parameters: a function *value* ignores
    /// defaults, so its value form takes every positional argument.
    arity: usize,
    /// A `var`-procedure (a `var` parameter, returns `Nil`): eta-expanding it
    /// would bind a `let` lambda parameter where a `var` is demanded, so it is
    /// excluded from value position with an error that names the real cause.
    is_var_procedure: bool,
    /// Part of this module's public API. Same-module calls may use private
    /// helpers; imported or module-qualified cross-module calls may not.
    public: bool,
}

/// The source of a bundled standard-library module, if `name` is one. This is
/// the canonical std registry: the linker treats it as a built-in search path,
/// and the CLI/test harness resolve `import` against it too.
/// Names of all bundled standard-library modules.
pub const STD_MODULES: &[&str] = &[
    "list", "string", "math", "result", "option", "func", "cmp", "ascii", "set", "server",
    "show", "http", "json", "url", "duration", "prng", "regex", "crypto", "compiler", "toml",
    "iter", "semver", "rights", "fs", "dict", "time", "encoding", "path", "testing",
    "future", "task", "chan", "webauthn", "secretstore", "reflect", "meta", "convert", "exec",
    "policy", "jwt", "oauth", "rand", "vm", "bytes",
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
        let mut current_impl: Option<&str> = None;
        for (i, line) in lines.iter().enumerate() {
            let indent = line.len() - line.trim_start().len();
            let t = line.trim_start();
            // Track an INHERENT `impl <Type>:` block at column 0, so a `pub fn` inside
            // it is surfaced under its Type (`Net.tcp`) — the callable, `check`-accepted
            // form — not the module (`policy.tcp`), which `check` rejects (BUG-160). A
            // TRAIT impl (`impl Trait for Type:`) is not a `Type.method` call site, so it
            // leaves methods module-qualified; any other top-level line ends the block.
            if indent == 0 && !t.is_empty() && !t.starts_with("//") {
                current_impl = t
                    .strip_prefix("impl ")
                    .and_then(|r| r.trim_end().strip_suffix(':'))
                    .map(str::trim)
                    .filter(|s| !s.contains(" for ") && !s.contains(" where ") && !s.contains('('));
            }
            let Some(rest) = t.strip_prefix("pub fn ") else { continue };
            let Some(paren) = rest.find('(') else { continue };
            let fname = rest[..paren].trim();
            let qualifier = if indent > 0 { current_impl.unwrap_or(m) } else { m };
            let qualified = format!("{qualifier}.{fname}");
            let bucket = if fname == query || qualified == query {
                &mut exact
            } else if fname.contains(query) || qualified.contains(query) {
                &mut partial
            } else {
                continue;
            };
            let sig = rest.trim_end().trim_end_matches(':');
            let mut entry = format!("{qualifier}.{sig}");
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
    let mut current_impl: Option<&str> = None;
    for line in src.lines() {
        let indent = line.len() - line.trim_start().len();
        let t = line.trim_start();
        // Surface inherent-`impl <Type>:` methods under their Type (`Net.tcp`), the
        // callable form, rather than the module (`policy.tcp`) — see BUG-160.
        if indent == 0 && !t.is_empty() && !t.starts_with("//") {
            current_impl = t
                .strip_prefix("impl ")
                .and_then(|r| r.trim_end().strip_suffix(':'))
                .map(str::trim)
                .filter(|s| !s.contains(" for ") && !s.contains(" where ") && !s.contains('('));
        }
        if let Some(rest) = t.strip_prefix("pub fn ") {
            if rest.contains('(') {
                let qualifier = if indent > 0 { current_impl.unwrap_or(module) } else { module };
                out.push(format!("{qualifier}.{}", rest.trim_end().trim_end_matches(':')));
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
    if name == "random" {
        return Some("prng");
    }
    if name.len() < 3 {
        return None; // too short for a meaningful suggestion
    }
    let mut best: Option<(usize, &'static str)> = None;
    for m in STD_MODULES {
        let d = levenshtein(name, m);
        let max_distance = if name.len() <= 3 { 1 } else { 2 };
        if d <= max_distance && d < name.len() && best.as_ref().is_none_or(|(bd, _)| d < *bd) {
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
        "prng" => Some(include_str!("../../../std/prng.witchy")),
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
        "time" => Some(include_str!("../../../std/time.witchy")),
        "encoding" => Some(include_str!("../../../std/encoding.witchy")),
        "path" => Some(include_str!("../../../std/path.witchy")),
        "future" => Some(include_str!("../../../std/future.witchy")),
        "task" => Some(include_str!("../../../std/task.witchy")),
        "chan" => Some(include_str!("../../../std/chan.witchy")),
        "reflect" => Some(include_str!("../../../std/reflect.witchy")),
        "meta" => Some(include_str!("../../../std/meta.witchy")),
        "exec" => Some(include_str!("../../../std/exec.witchy")),
        "policy" => Some(include_str!("../../../std/policy.witchy")),
        "jwt" => Some(include_str!("../../../std/jwt.witchy")),
        "oauth" => Some(include_str!("../../../std/oauth.witchy")),
        "vm" => Some(include_str!("../../../std/vm.witchy")),
        "bytes" => Some(include_str!("../../../std/bytes.witchy")),
        "error" => Some(include_str!("../../../std/error.witchy")),
        // (RFC-0041) `glamour` is a published RUNE, not std — deliberately ABSENT from
        // `STD_MODULES` so it is not advertised as std and a project still declares it as a
        // dependency (the PM/footprint treat it as external). But its source is bundled here so
        // its PURE render surface (`element`/`text`/`to_html`) is importable in a STANDALONE
        // SNIPPET — the browser playground and the book's runnable cells — where there is no
        // filesystem to resolve the rune. Authority-neutral: importing it only exposes types;
        // a `UiRoot`/`UiFetch` can still be minted only by the host, never obtained in a cell.
        "glamour" => Some(include_str!("../../../projects/glamour/src/glamour.witchy")),
        // `markdown` rides on glamour's bundling rationale above: a glamour-provided PURE
        // module (it imports only `glamour`), bundled so `import markdown` resolves in a
        // standalone snippet and in the glamour examples / `projects/docs`.
        "markdown" => Some(include_str!("../../../projects/glamour/src/markdown.witchy")),
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
    modules: Vec<(String, Module)>,
    entry: &str,
    expand: ComptimeExpander,
) -> Result<Module, LinkError> {
    link_with_user_modules(modules, entry, expand, &std::collections::HashSet::new())
}

/// Like [`link`], but with the subset of module names that came from user
/// source files rather than the bundled std fallback. This lets diagnostics
/// explain true local-std shadowing without mislabeling ordinary std imports.
pub fn link_with_user_modules(
    mut modules: Vec<(String, Module)>,
    entry: &str,
    expand: ComptimeExpander,
    user_modules: &std::collections::HashSet<String>,
) -> Result<Module, LinkError> {
    check_reserved_source_names(&modules)?;
    let user_std_shadows: HashSet<String> = user_modules
        .iter()
        .filter(|name| STD_MODULES.contains(&name.as_str()))
        .cloned()
        .collect();

    // Lower `gen fn`/`yield` to ordinary functions over `std/iter` first — this
    // adds `import iter`/`import option` to any generator module, so the std
    // pull-in below resolves them.
    modules = modules
        .into_iter()
        .map(|(n, m)| crate::generators::lower(m).map(|m| (n, m)))
        .collect::<Result<_, _>>()
        .map_err(|message| LinkError { message })?;

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
        .map(|(n, m)| crate::records::lower_lenient(m).map(|m| (n, m)))
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
    //
    // (RFC-0053) `show` is deliberately NOT a prelude module. It was tried, so the
    // interpolation flip (`"${90000ms}"` -> `1m30s`) would fire on programs that
    // never link `show` — but `std/show` transitively imports `result`/`set`/
    // `duration`/`string`, and a program is allowed to define its OWN module by one
    // of those names (a local module SHADOWS the std one). Forcing `show` in then
    // resolved `std/show`'s internal `result.ok` against such a local `result` that
    // lacks it, a spurious link error (e.g. `examples/result`). So the flip honors
    // `Show` only where the program already links it (an `impl Show`, a `say`, an
    // `import show` — every real Show-using program); a bare `"${90000ms}"` with
    // none of those keeps the structural millisecond render.
    for prelude in ["list", "string", "dict", "math", "option", "result", "policy"] {
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
        *m = crate::records::lower_lenient(m.clone()).map_err(|message| LinkError { message })?;
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

    // Expand type aliases and inline top-level constants per module before
    // merging, so their use sites (and any function calls inside constant values)
    // are qualified along with the bodies they expand into — no `Item::TypeAlias`
    // or `Item::Const` reaches later stages.
    modules = modules
        .into_iter()
        .map(|(n, m)| (n, crate::aliases::resolve(crate::consts::inline(m))))
        .collect();

    // (RFC-0042) Canonicalize every user TYPE and CONSTRUCTOR name to
    // `module.Name`, per module — the type-side twin of the function
    // qualification below. Runs after alias expansion (so `type Id = iter.Step`
    // is already a concrete reference) and before the merge, while each item
    // still knows its home module and imports. Dissolves the flat-type-namespace
    // collisions (iter+chan's `Step`, task+future's `Step`/`Task`).
    crate::type_resolve::resolve(&mut modules)?;

    // RFC-0002 sealing: a `capability` (a sealed type) may be CONSTRUCTED or
    // DESTRUCTURED only inside the module that declares it. Run AFTER
    // `type_resolve` canonicalizes every constructor and pattern — bare,
    // module-qualified (`lib.Vault(…)`), and from-imported alike — to a single
    // `Expr::Ctor { name: "module.Ctor" }`, so a qualified spelling can no longer
    // slip past the name-keyed check (BUG-313, fail-closed). Modules are still
    // unmerged here, so each item knows its home module.
    check_sealing(&modules)?;

    let mut fns: FnTable = HashMap::new();
    for (name, m) in &modules {
        let mut names: HashMap<String, EtaSig> = HashMap::new();
        for item in &m.items {
            if let Item::Function(f) = item {
                names.insert(
                    f.name.clone(),
                    EtaSig {
                        arity: f.params.len(),
                        is_var_procedure: f.is_var_procedure(),
                        public: f.public,
                    },
                );
            }
        }
        fns.insert(name.clone(), names);
    }
    let mut bare_fn_imports: BareFnImports = HashMap::new();
    for (name, m) in &modules {
        let mut bare = HashMap::new();
        for (srcmod, names) in &m.from_imports {
            for imported in names {
                if fns
                    .get(srcmod)
                    .and_then(|s| s.get(imported))
                    .is_some_and(|sig| sig.public)
                {
                    bare.insert(imported.clone(), srcmod.clone());
                }
            }
        }
        bare_fn_imports.insert(name.clone(), bare);
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
    let mut seen_anon_types: HashMap<String, TypeDef> = HashMap::new();
    let mut seen_anon_trait_impls: HashSet<(String, Vec<String>, String, Vec<String>)> = HashSet::new();
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
                    rewrite_block(
                        &mut f2.body,
                        mname,
                        &m.imports,
                        bare_fn_imports.get(mname),
                        &fns,
                        &bound,
                        &user_std_shadows,
                    )?;
                    items.push(Item::Function(f2));
                }
                Item::Type(t) => {
                    if is_generated_anon_name(&t.name) {
                        if let Some(prev) = seen_anon_types.get(&t.name) {
                            if prev == t {
                                continue;
                            }
                        } else {
                            seen_anon_types.insert(t.name.clone(), t.clone());
                        }
                    }
                    items.push(Item::Type(t.clone()));
                }
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
                            rewrite_block(
                                body,
                                mname,
                                &m.imports,
                                bare_fn_imports.get(mname),
                                &fns,
                                &bound,
                                &user_std_shadows,
                            )?;
                        }
                    }
                    items.push(Item::Trait(t2));
                }
                Item::Impl(im) => {
                    if let Some(key) = generated_anon_trait_impl_key(im) {
                        if !seen_anon_trait_impls.insert(key) {
                            continue;
                        }
                    }
                    let mut im2 = im.clone();
                    for method in &mut im2.methods {
                        let mut bound = HashSet::new();
                        for p in &method.params {
                            bound.insert(p.name.clone());
                        }
                        collect_bound_block(&method.body, &mut bound);
                        rewrite_block(
                            &mut method.body,
                            mname,
                            &m.imports,
                            bare_fn_imports.get(mname),
                            &fns,
                            &bound,
                            &user_std_shadows,
                        )?;
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
        from_imports: Vec::new(),
        items,
        import_lines: Vec::new(),
        item_lines: Vec::new(),
    };
    resolve_methods(&mut module);
    // (RFC-0042) Fix up residual bare constructor PATTERNS — a plain `import iter`
    // + `match … Item(x)`, where the per-module pass could not see the scrutinee's
    // module. Now that every type is merged, a bare variant whose unqualified name
    // is unique across the program resolves to it; an ambiguous one is a loud
    // error asking for a qualifier (never a silent runtime tag mismatch).
    crate::type_resolve::resolve_residual_patterns(&mut module)?;
    // (RFC-0056) Resolve keyword-labeled direct calls and splice constant
    // parameter defaults, using each callee's now-qualified declaration. Runs
    // AFTER method resolution (so every direct callee is statically known) and
    // BEFORE folding/typeck, which only ever see the positional calls it emits —
    // labels and defaults never reach either backend (parity by construction).
    crate::keyword_args::resolve(&mut module).map_err(|message| LinkError { message })?;
    // Semantics-preserving constant folding over the single linked module both
    // backends consume (parity-free by construction). See src/optimize.rs. Gated
    // on the `fold` lever (RFC-0030) so the differential de-opt sweep covers it:
    // `WITCHY_OPT=-fold` leaves constant expressions unfolded.
    if crate::opt::enabled(crate::opt::Opt::Fold) {
        crate::optimize::optimize(&mut module);
    }
    // (RFC-0043) Statement-position mutating-method write-back is NO LONGER a
    // link-time name census. It moved to `witchy_types::traits::lower` (the
    // method-resolution pass), where the receiver's type is known and the
    // decision reads the RESOLVED CALLEE's `var`-receiver declaration — killing
    // the shadowing (Failure 1) and filter/map (Failure 2) silent-misbehavior
    // classes the census produced. See traits.rs's module doc.
    //
    // (BUG-342/BUG-316) The per-module records pass above ran LENIENT, deferring
    // any named-field construction whose record type that module could not yet
    // see (an imported record) to this point. Now that every module is merged and
    // every type is visible, the strict pass resolves those leftover imported
    // constructions AND rejects a genuinely-unknown constructor name
    // (`Bogus(x: 9, ..p)` → "not a record type") — so the merge is the single
    // point where an unknown record type is caught, whether or not a later stage
    // (typeck/backend) re-runs the idempotent lowering.
    let module = crate::records::lower(module).map_err(|message| LinkError { message })?;
    Ok(module)
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
        // (RFC-0056) A labeled direct call names its callee statically (the parser
        // only labels a direct call), so no overload resolution is needed — just
        // recurse into the argument values.
        Expr::LabeledCall { args, .. } => {
            for (_, a) in args.iter_mut() {
                resolve_in_expr(a, sig, by_base, vars);
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
        Expr::RecordUpdate { name: _, base, fields } => {
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
        Expr::LabeledCall { args, .. } => {
            for (_, a) in args {
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
        Expr::RecordUpdate { name: _, base, fields } => {
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
    bare_imports: Option<&HashMap<String, String>>,
    fns: &FnTable,
    bound: &HashSet<String>,
    user_std_shadows: &HashSet<String>,
) -> Result<(), LinkError> {
    for stmt in &mut b.stmts {
        match stmt {
            Stmt::Let { value, .. }
            | Stmt::Assign { value, .. }
            | Stmt::LetPattern { value, .. } => rewrite_expr(value, m, imps, bare_imports, fns, bound, user_std_shadows)?,
            Stmt::Return(Some(e)) | Stmt::Expr(e) | Stmt::Yield(e) => {
                rewrite_expr(e, m, imps, bare_imports, fns, bound, user_std_shadows)?
            }
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
    }
    Ok(())
}

fn rewrite_expr(
    e: &mut Expr,
    m: &str,
    imps: &[String],
    bare_imports: Option<&HashMap<String, String>>,
    fns: &FnTable,
    bound: &HashSet<String>,
    user_std_shadows: &HashSet<String>,
) -> Result<(), LinkError> {
    match e {
        Expr::Call { name, args } => {
            // `local.method(args)` where `local` is a bound variable is a METHOD
            // CALL on the local, not a module-qualified function call — the local
            // shadows any prelude/imported module of the same name (BUG-216).
            // Rewrite to a `MethodCall` so trait/UFCS lowering resolves it by the
            // local's type, matching the value-position rule for `local.field`
            // (the `Expr::Field` arm below). The module stays reachable while
            // shadowed via an alias or by renaming the local.
            if let Some((base, method)) = name.split_once('.') {
                if bound.contains(base) && !method.contains('.') {
                    let receiver = Box::new(Expr::Var(base.to_string()));
                    let method = method.to_string();
                    let mut call_args = Vec::new();
                    std::mem::swap(args, &mut call_args);
                    for a in &mut call_args {
                        rewrite_expr(a, m, imps, bare_imports, fns, bound, user_std_shadows)?;
                    }
                    *e = Expr::MethodCall { receiver, method, args: call_args };
                    return Ok(());
                }
            }
            *name = resolve_call(name, m, imps, bare_imports, fns, bound, user_std_shadows)?;
            for a in args {
                rewrite_expr(a, m, imps, bare_imports, fns, bound, user_std_shadows)?;
            }
        }
        // (RFC-0056) A labeled direct call: qualify the callee exactly like a plain
        // call so `keyword_args::resolve` can look up its declaration, and rewrite
        // the argument values. The labels ride along untouched until then.
        Expr::LabeledCall { name, args } => {
            *name = resolve_call(name, m, imps, bare_imports, fns, bound, user_std_shadows)?;
            for (_, a) in args {
                rewrite_expr(a, m, imps, bare_imports, fns, bound, user_std_shadows)?;
            }
        }
        // A bare name matching a same-module function is a first-class reference
        // to it; qualify it like a call — unless it is shadowed by a local of the
        // same name (a parameter, `let`, loop variable, or pattern binding).
        Expr::Var(name) => {
            if !bound.contains(name.as_str())
                && fns.get(m).is_some_and(|s| s.contains_key(name.as_str()))
            {
                *name = format!("{m}.{name}");
            }
        }
        Expr::Apply { func, args } => {
            rewrite_expr(func, m, imps, bare_imports, fns, bound, user_std_shadows)?;
            for a in args {
                rewrite_expr(a, m, imps, bare_imports, fns, bound, user_std_shadows)?;
            }
        }
        Expr::Ctor { args, .. } | Expr::List(args) | Expr::Tuple(args) => {
            for a in args {
                rewrite_expr(a, m, imps, bare_imports, fns, bound, user_std_shadows)?;
            }
        }
        Expr::Unary { expr, .. } | Expr::Try(expr) | Expr::As { expr, .. } => {
            rewrite_expr(expr, m, imps, bare_imports, fns, bound, user_std_shadows)?
        }
        // (RFC-0050 Part 2) A bare `module.fn` in value (non-call) position is a
        // first-class function value, produced by eta-expansion: `list.length`
        // becomes `fn(__eta0): list.length(__eta0)` at the callee's FULL declared
        // arity. Parsed as a `Field` read; only rewritten when the base is a
        // module name in scope (not a local shadowing it) that actually exports
        // the function — otherwise it is an ordinary field/tuple access and the
        // base is recursed into as before.
        Expr::Field { base, field } => {
            // Is the base a bare module name that is actually in scope here — a
            // prelude module or one this module imports — and not shadowed by a
            // local of the same name? Then `module.field` is a module-qualified
            // *value* reference (modules are not values, so it is never an
            // ordinary field read).
            let module_ref = match base.as_ref() {
                Expr::Var(modname)
                    if !bound.contains(modname.as_str())
                        && (is_prelude_module(modname) || imps.iter().any(|i| i == modname)) =>
                {
                    Some((modname.clone(), field.clone()))
                }
                _ => None,
            };
            if let Some((modname, field)) = module_ref {
                // Validate it exactly as a *call* would be. Scope is already
                // checked, so `resolve_call` returns the qualified callee or the
                // precise "module `X` has no function `Y`" error — "unbound
                // variable" never names a module again.
                let qualified =
                    resolve_call(&format!("{modname}.{field}"), m, imps, bare_imports, fns, bound, user_std_shadows)?;
                let sig = fns
                    .get(&modname)
                    .and_then(|s| s.get(&field))
                    .copied()
                    .expect("resolve_call accepted the reference, so the function exists");
                // A Nil-returning `var`-procedure has no value form: eta-expanding
                // it would bind a `let` lambda parameter where a `var` is demanded
                // (RFC-0043). Name the real cause rather than let a later pass
                // mislead. RFC-0043 mutators (they return `self`) are fine — their
                // value form is an ordinary pure call.
                if sig.is_var_procedure {
                    return lerr(format!(
                        "`{modname}.{field}` is a `var`-procedure (it mutates its argument in \
                         place and returns Nil), so it has no value form: an eta-expanded lambda \
                         would bind a `let` parameter where a `var` is required. Call it directly, \
                         or wrap it in your own `fn(var x): {modname}.{field}(x)`."
                    ));
                }
                *e = eta_lambda(&qualified, sig.arity);
                return Ok(());
            }
            // (BUG-303) A value-position `iter.count` whose base is a KNOWN std
            // module that is simply not imported would otherwise fall through to
            // an ordinary field read and die as "unbound variable `iter`". Emit
            // the same missing-import teaching diagnostic the call position gives,
            // so "unbound variable" never names a module.
            if let Expr::Var(modname) = base.as_ref() {
                if !bound.contains(modname.as_str()) && STD_MODULES.contains(&modname.as_str()) {
                    return lerr(format!(
                        "`{modname}.{field}` looks like a module-qualified reference, but \
                         `{modname}` is not imported — add `import {modname}`"
                    ));
                }
            }
            rewrite_expr(base, m, imps, bare_imports, fns, bound, user_std_shadows)?;
        }
        Expr::RecordUpdate { name: _, base, fields } => {
            rewrite_expr(base, m, imps, bare_imports, fns, bound, user_std_shadows)?;
            for (_, value) in fields {
                rewrite_expr(value, m, imps, bare_imports, fns, bound, user_std_shadows)?;
            }
        }
        Expr::Record { fields, spread, .. } => {
            for (_, value) in fields {
                rewrite_expr(value, m, imps, bare_imports, fns, bound, user_std_shadows)?;
            }
            if let Some(s) = spread {
                rewrite_expr(s, m, imps, bare_imports, fns, bound, user_std_shadows)?;
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            rewrite_expr(lhs, m, imps, bare_imports, fns, bound, user_std_shadows)?;
            rewrite_expr(rhs, m, imps, bare_imports, fns, bound, user_std_shadows)?;
        }
        Expr::Range { lo, hi, .. } => {
            rewrite_expr(lo, m, imps, bare_imports, fns, bound, user_std_shadows)?;
            rewrite_expr(hi, m, imps, bare_imports, fns, bound, user_std_shadows)?;
        }
        Expr::Index { base, index } => {
            rewrite_expr(base, m, imps, bare_imports, fns, bound, user_std_shadows)?;
            rewrite_expr(index, m, imps, bare_imports, fns, bound, user_std_shadows)?;
        }
        // Lowered to a plain `Call` before this runs; recurse for safety.
        Expr::MethodCall { receiver, args, .. } => {
            rewrite_expr(receiver, m, imps, bare_imports, fns, bound, user_std_shadows)?;
            for a in args {
                rewrite_expr(a, m, imps, bare_imports, fns, bound, user_std_shadows)?;
            }
        }
        Expr::WhileLet { scrutinee, body, .. } => {
            rewrite_expr(scrutinee, m, imps, bare_imports, fns, bound, user_std_shadows)?;
            rewrite_block(body, m, imps, bare_imports, fns, bound, user_std_shadows)?;
        }
        Expr::If {
            cond,
            then_block,
            else_block,
        } => {
            rewrite_expr(cond, m, imps, bare_imports, fns, bound, user_std_shadows)?;
            rewrite_block(then_block, m, imps, bare_imports, fns, bound, user_std_shadows)?;
            if let Some(b) = else_block {
                rewrite_block(b, m, imps, bare_imports, fns, bound, user_std_shadows)?;
            }
        }
        Expr::Lambda { body, .. } => rewrite_block(body, m, imps, bare_imports, fns, bound, user_std_shadows)?,
        Expr::Block(b) => rewrite_block(b, m, imps, bare_imports, fns, bound, user_std_shadows)?,
        Expr::While { cond, body } => {
            rewrite_expr(cond, m, imps, bare_imports, fns, bound, user_std_shadows)?;
            rewrite_block(body, m, imps, bare_imports, fns, bound, user_std_shadows)?;
        }
        Expr::For { iter, body, .. } => {
            rewrite_expr(iter, m, imps, bare_imports, fns, bound, user_std_shadows)?;
            rewrite_block(body, m, imps, bare_imports, fns, bound, user_std_shadows)?;
        }
        Expr::Match { scrutinee, arms } => {
            rewrite_expr(scrutinee, m, imps, bare_imports, fns, bound, user_std_shadows)?;
            for arm in arms {
                if let Some(g) = &mut arm.guard {
                    rewrite_expr(g, m, imps, bare_imports, fns, bound, user_std_shadows)?;
                }
                rewrite_expr(&mut arm.body, m, imps, bare_imports, fns, bound, user_std_shadows)?;
            }
        }
        Expr::Int(_) | Expr::Duration(_) | Expr::Float(_) | Expr::Str(_) | Expr::Bool(_) | Expr::TaggedLit { .. } => {}
    }
    Ok(())
}

/// The prelude modules that are importable-by-default everywhere (the link set
/// always carries them), so a call or a `module.fn` value reference to one needs
/// no explicit `import`.
fn is_prelude_module(name: &str) -> bool {
    matches!(name, "list" | "string" | "dict" | "math" | "option" | "result")
}

/// (RFC-0050 Part 2) Build the eta-expansion of a module-function reference in
/// value position: `list.length` (arity 1) becomes `fn(__eta0): list.length(__eta0)`.
/// The lambda captures nothing and its parameters carry no type annotation, so the
/// ordinary checker infers them — for a generic callee, RFC-0046's annotate/mono
/// fixpoint resolves the type-var parameters. A source-to-source rewrite on the
/// single linked AST before either backend lowers: parity by construction. The
/// arity is the callee's FULL declared parameter count; RFC-0056 defaults never
/// attach to a function value, so every positional argument is present.
fn eta_lambda(qualified: &str, arity: usize) -> Expr {
    let names: Vec<String> = (0..arity).map(|i| format!("__eta{i}")).collect();
    let params = names
        .iter()
        .map(|n| Param {
            name: n.clone(),
            ty: None,
            convention: Convention::Let,
            default: None,
        })
        .collect();
    let args = names.into_iter().map(Expr::Var).collect();
    let call = Expr::Call { name: qualified.to_string(), args };
    Expr::Lambda {
        params,
        body: Block { stmts: vec![Stmt::Expr(call)], lines: vec![0], region: None },
        ret: None,
    }
}

// ---------------------------------------------------------------------------
// RFC-0002 sealing: a `capability` (sealed) type may only be constructed
// (`X(..)`) or destructured (`match _: X(..)`) inside the module that declares
// it. Every other module may hold, pass, and return a value of `X` — but cannot
// mint or unwrap one, so the brand is un-forgeable like the host capabilities it
// refines. Checked here (a read-only walk mirroring `rewrite_expr`'s complete
// traversal, plus pattern walking the rewriter skips) before names are merged.
// ---------------------------------------------------------------------------

/// constructor name (bare or canonical `module.Ctor`) -> (home module, is_capability).
type SealMap = HashMap<String, (String, bool)>;

fn check_sealing(modules: &[(String, Module)]) -> Result<(), LinkError> {
    // Each CONSTRUCTOR of a sealed type is registered under both its bare name and
    // its canonical `module.Ctor` spelling (whichever form reaches `seal_use` after
    // `type_resolve` — a qualified `m.Ctor` is the BUG-313 bypass). The value is the
    // home module plus whether the seal came from a `capability` (RFC-0002) or a
    // `sealed type` (RFC-0065), which only changes the diagnostic noun. Keying on
    // the constructor (not the type name) is what generalizes RFC-0002's mechanism:
    // a `capability`'s single variant is named after the type, so this is a strict
    // superset of the old type-name keying.
    let mut sealed: SealMap = HashMap::new();
    for (mname, m) in modules {
        for item in &m.items {
            if let Item::Type(t) = item {
                if t.sealed {
                    for v in &t.variants {
                        let info = (mname.clone(), t.is_capability);
                        sealed.insert(v.name.clone(), info.clone());
                        sealed.insert(format!("{mname}.{}", v.name), info);
                    }
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

fn seal_use(name: &str, sealed: &SealMap, home: &str, verb: &str) -> Result<(), LinkError> {
    if let Some((decl, is_capability)) = sealed.get(name) {
        // A `sealed type` (RFC-0065) restricts only CONSTRUCTION; matching/reading
        // are unaffected (DoD item 4). A `capability` (RFC-0002) additionally seals
        // DESTRUCTURING — its carried authority must not be unwrapped elsewhere.
        if verb == "destructure" && !is_capability {
            return Ok(());
        }
        if decl != home {
            // Names may be bare or canonical `module.Ctor` here; show the bare ctor.
            let bare = name.rsplit('.').next().unwrap_or(name);
            let noun = if *is_capability { "sealed capability" } else { "sealed type" };
            return lerr(format!(
                "`{bare}` is a {noun} declared in module `{decl}`; module \
                 `{home}` may hold and pass a `{bare}` but cannot {verb} one — only \
                 `{decl}` can mint or unwrap it (use the functions `{decl}` exports)"
            ));
        }
    }
    Ok(())
}

fn seal_block(b: &Block, sealed: &SealMap, home: &str) -> Result<(), LinkError> {
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

fn seal_pattern(p: &Pattern, sealed: &SealMap, home: &str) -> Result<(), LinkError> {
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

fn seal_expr(e: &Expr, sealed: &SealMap, home: &str) -> Result<(), LinkError> {
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
        Expr::LabeledCall { args, .. } => {
            for (_, a) in args {
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
        Expr::RecordUpdate { name: _, base, fields } => {
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
    bare_imports: Option<&HashMap<String, String>>,
    fns: &FnTable,
    bound: &HashSet<String>,
    user_std_shadows: &HashSet<String>,
) -> Result<String, LinkError> {
    check_private_intrinsic_call(name, m)?;
    if let Some((modname, fname)) = name.split_once('.') {
        // The prelude modules are importable-by-default everywhere (the link
        // set always carries them), including from inside one another.
        let prelude = is_prelude_module(modname);
        if !prelude && !imps.iter().any(|i| i == modname) {
            return lerr(format!(
                "module `{m}` calls `{modname}.{fname}` but does not `import {modname}`"
            ));
        }
        return match fns.get(modname).and_then(|s| s.get(fname)) {
            Some(_) if modname == m => Ok(name.to_string()),
            Some(sig) if sig.public => Ok(name.to_string()),
            Some(_) => lerr(format!("function `{modname}.{fname}` is private to module `{modname}`")),
            None => lerr(missing_module_function_message(modname, fname, user_std_shadows)),
        };
    }
    // A function defined in THIS module wins over a builtin of the same name, so
    // e.g. `list.contains` is reachable as a bare `contains` inside `list` (a
    // builtin would otherwise shadow it). Checked before BUILTINS for that
    // reason.
    if fns.get(m).is_some_and(|s| s.contains_key(name)) {
        return Ok(format!("{m}.{name}"));
    }
    if BUILTINS.contains(&name) {
        return Ok(name.to_string());
    }
    if !bound.contains(name) {
        if let Some(srcmod) = bare_imports.and_then(|imports| imports.get(name)) {
            return Ok(format!("{srcmod}.{name}"));
        }
        if let Some(srcmod) = imps
            .iter()
            .find(|imp| fns.get(*imp).and_then(|s| s.get(name)).is_some_and(|sig| sig.public))
        {
            return lerr(format!(
                "`{name}(...)` is not in scope as a bare function. `import {srcmod}` keeps \
                 functions qualified; write `{srcmod}.{name}(...)` or add \
                 `from {srcmod} import {name}`"
            ));
        }
    }
    // Not a function here and not a builtin: a local binding being applied (e.g.
    // a lambda parameter). Leave it unqualified; the type checker decides.
    Ok(name.to_string())
}

fn missing_module_function_message(
    modname: &str,
    fname: &str,
    user_std_shadows: &HashSet<String>,
) -> String {
    if user_std_shadows.contains(modname) {
        format!(
            "module `{modname}` is provided by this program and shadows the bundled \
             standard-library module `{modname}`; it has no function `{fname}`"
        )
    } else {
        format!("module `{modname}` has no function `{fname}`")
    }
}

// ---------------------------------------------------------------------------
// Compiler-private namespace boundary.
// ---------------------------------------------------------------------------

fn is_compiler_private_name(name: &str) -> bool {
    name.starts_with("__")
}

fn is_user_spellable_lowered_method_name(name: &str) -> bool {
    // Trait/inherent method lowering historically generated source-spellable
    // `Type__method` / `Trait__Type__method` function names. Reserve that shape
    // for top-level functions so a handwritten helper cannot collide with a
    // lowered method after the duplicate-name census has already run.
    name.contains("__")
}

fn is_reserved_user_identifier(name: &str) -> bool {
    is_compiler_private_name(name) || is_user_spellable_lowered_method_name(name)
}

fn is_generated_anon_name(name: &str) -> bool {
    name.strip_prefix("__anon")
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

fn is_generated_anon_type(name: &str, line: Option<u32>) -> bool {
    line == Some(u32::MAX) && is_generated_anon_name(name)
}

fn generated_anon_trait_impl_key(im: &ImplDef) -> Option<(String, Vec<String>, String, Vec<String>)> {
    let trait_name = im.trait_name.as_ref()?;
    if !is_generated_anon_name(&im.type_name) {
        return None;
    }
    Some((
        trait_name.clone(),
        im.trait_args.iter().map(crate::format::type_str).collect(),
        im.type_name.clone(),
        im.target_args.iter().map(crate::format::type_str).collect(),
    ))
}

fn is_generated_local_name(name: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "__fv",
        "__fortuple",
        "__compr",
        "__range",
        "__ri",
        "__rend",
        "__kw",
        "__eta",
        "__await",
    ];
    PREFIXES.iter().any(|prefix| {
        name.strip_prefix(prefix)
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    })
}

fn check_reserved_source_names(modules: &[(String, Module)]) -> Result<(), LinkError> {
    for (module_name, module) in modules {
        if STD_MODULES.contains(&module_name.as_str()) {
            continue;
        }
        for (idx, item) in module.items.iter().enumerate() {
            let line = module.item_lines.get(idx).copied();
            check_reserved_item(module_name, item, line)?;
        }
    }
    Ok(())
}

fn check_reserved_item(module_name: &str, item: &Item, line: Option<u32>) -> Result<(), LinkError> {
    match item {
        Item::Function(f) => {
            if is_reserved_user_identifier(&f.name) {
                return reserved_name_error(module_name, "function", &f.name);
            }
            check_reserved_function(module_name, f)
        }
        Item::Type(t) => {
            let generated_anon = is_generated_anon_type(&t.name, line);
            if is_reserved_user_identifier(&t.name) && !generated_anon {
                return reserved_name_error(module_name, "type", &t.name);
            }
            for param in &t.params {
                check_reserved_binding(module_name, "type parameter", param)?;
            }
            for variant in &t.variants {
                if is_reserved_user_identifier(&variant.name) && !generated_anon {
                    return reserved_name_error(module_name, "constructor", &variant.name);
                }
                for field_name in &variant.field_names {
                    check_reserved_binding(module_name, "field", field_name)?;
                }
            }
            Ok(())
        }
        Item::Trait(t) => {
            if is_reserved_user_identifier(&t.name) {
                return reserved_name_error(module_name, "trait", &t.name);
            }
            for param in &t.typarams {
                check_reserved_binding(module_name, "type parameter", param)?;
            }
            for method in &t.methods {
                if is_reserved_user_identifier(&method.name) {
                    return reserved_name_error(module_name, "trait method", &method.name);
                }
                for p in &method.params {
                    check_reserved_param(module_name, p)?;
                    if let Some(default) = &p.default {
                        check_reserved_expr(module_name, default)?;
                    }
                }
                if let Some(body) = &method.default {
                    check_reserved_block(module_name, body)?;
                }
            }
            Ok(())
        }
        Item::Impl(im) => {
            if let Some(trait_name) = &im.trait_name {
                if is_reserved_user_identifier(trait_name) {
                    return reserved_name_error(module_name, "trait", trait_name);
                }
            }
            if is_reserved_user_identifier(&im.type_name) {
                return reserved_name_error(module_name, "type", &im.type_name);
            }
            for method in &im.methods {
                if is_reserved_user_identifier(&method.name) {
                    return reserved_name_error(module_name, "method", &method.name);
                }
                check_reserved_function(module_name, method)?;
            }
            Ok(())
        }
        Item::Const { name, value } => {
            check_reserved_binding(module_name, "constant", name)?;
            check_reserved_expr(module_name, value)
        }
        Item::TypeAlias { name, ty, .. } => {
            check_reserved_binding(module_name, "type alias", name)?;
            check_reserved_type(module_name, ty)
        }
        Item::Comptime(body) => check_reserved_block(module_name, body),
    }
}

fn check_reserved_function(module_name: &str, f: &Function) -> Result<(), LinkError> {
    for p in &f.params {
        check_reserved_param(module_name, p)?;
    }
    check_reserved_block(module_name, &f.body)
}

fn check_reserved_param(module_name: &str, p: &Param) -> Result<(), LinkError> {
    check_reserved_binding(module_name, "parameter", &p.name)?;
    if let Some(ty) = &p.ty {
        check_reserved_type(module_name, ty)?;
    }
    if let Some(default) = &p.default {
        check_reserved_expr(module_name, default)?;
    }
    Ok(())
}

fn check_reserved_binding(module_name: &str, kind: &str, name: &str) -> Result<(), LinkError> {
    if is_reserved_user_identifier(name) && !is_generated_local_name(name) {
        return reserved_name_error(module_name, kind, name);
    }
    Ok(())
}

fn reserved_name_error(module_name: &str, kind: &str, name: &str) -> Result<(), LinkError> {
    lerr(format!(
        "module `{module_name}` declares {kind} `{name}`, but identifiers containing `__` are \
         reserved for the compiler"
    ))
}

fn check_reserved_type(module_name: &str, ty: &Type) -> Result<(), LinkError> {
    match ty {
        Type::Named(name, args) => {
            if is_reserved_user_identifier(name) {
                return reserved_name_error(module_name, "type", name);
            }
            for arg in args {
                check_reserved_type(module_name, arg)?;
            }
        }
        Type::Tuple(items) => {
            for item in items {
                check_reserved_type(module_name, item)?;
            }
        }
        Type::Fn(params, ret) => {
            for param in params {
                check_reserved_type(module_name, param)?;
            }
            check_reserved_type(module_name, ret)?;
        }
        Type::Qualified(_, inner) => check_reserved_type(module_name, inner)?,
    }
    Ok(())
}

fn check_reserved_block(module_name: &str, block: &Block) -> Result<(), LinkError> {
    for stmt in &block.stmts {
        check_reserved_stmt(module_name, stmt)?;
    }
    Ok(())
}

fn check_reserved_stmt(module_name: &str, stmt: &Stmt) -> Result<(), LinkError> {
    match stmt {
        Stmt::Let { name, ty, value, .. } => {
            check_reserved_binding(module_name, "binding", name)?;
            if let Some(ty) = ty {
                check_reserved_type(module_name, ty)?;
            }
            check_reserved_expr(module_name, value)
        }
        Stmt::Assign { name, value } => {
            check_reserved_binding(module_name, "assignment target", name)?;
            check_reserved_expr(module_name, value)
        }
        Stmt::LetPattern { pattern, value } => {
            check_reserved_pattern(module_name, pattern)?;
            check_reserved_expr(module_name, value)
        }
        Stmt::Return(Some(value)) | Stmt::Expr(value) | Stmt::Yield(value) => {
            check_reserved_expr(module_name, value)
        }
        Stmt::Return(None) | Stmt::Break | Stmt::Continue => Ok(()),
    }
}

fn check_reserved_expr(module_name: &str, expr: &Expr) -> Result<(), LinkError> {
    match expr {
        Expr::List(items) | Expr::Tuple(items) | Expr::Ctor { args: items, .. } => {
            for item in items {
                check_reserved_expr(module_name, item)?;
            }
        }
        Expr::Call { args, .. } => {
            for arg in args {
                check_reserved_expr(module_name, arg)?;
            }
        }
        Expr::LabeledCall { args, .. } => {
            for (_, arg) in args {
                check_reserved_expr(module_name, arg)?;
            }
        }
        Expr::MethodCall { receiver, args, .. } => {
            check_reserved_expr(module_name, receiver)?;
            for arg in args {
                check_reserved_expr(module_name, arg)?;
            }
        }
        Expr::Apply { func, args } => {
            check_reserved_expr(module_name, func)?;
            for arg in args {
                check_reserved_expr(module_name, arg)?;
            }
        }
        Expr::Lambda { params, body, ret } => {
            for param in params {
                check_reserved_param(module_name, param)?;
            }
            if let Some(ret) = ret {
                check_reserved_type(module_name, ret)?;
            }
            check_reserved_block(module_name, body)?;
        }
        Expr::RecordUpdate { name: _, base, fields } => {
            check_reserved_expr(module_name, base)?;
            for (field, value) in fields {
                check_reserved_binding(module_name, "field", field)?;
                check_reserved_expr(module_name, value)?;
            }
        }
        Expr::Record { fields, spread, .. } => {
            for (field, value) in fields {
                check_reserved_binding(module_name, "field", field)?;
                check_reserved_expr(module_name, value)?;
            }
            if let Some(spread) = spread {
                check_reserved_expr(module_name, spread)?;
            }
        }
        Expr::Unary { expr, .. } | Expr::Try(expr) | Expr::As { expr, .. } | Expr::Field { base: expr, .. } => {
            check_reserved_expr(module_name, expr)?
        }
        Expr::Binary { lhs, rhs, .. } => {
            check_reserved_expr(module_name, lhs)?;
            check_reserved_expr(module_name, rhs)?;
        }
        Expr::If { cond, then_block, else_block } => {
            check_reserved_expr(module_name, cond)?;
            check_reserved_block(module_name, then_block)?;
            if let Some(block) = else_block {
                check_reserved_block(module_name, block)?;
            }
        }
        Expr::Match { scrutinee, arms } => {
            check_reserved_expr(module_name, scrutinee)?;
            for arm in arms {
                check_reserved_pattern(module_name, &arm.pattern)?;
                if let Some(guard) = &arm.guard {
                    check_reserved_expr(module_name, guard)?;
                }
                check_reserved_expr(module_name, &arm.body)?;
            }
        }
        Expr::Block(block) => check_reserved_block(module_name, block)?,
        Expr::While { cond, body } => {
            check_reserved_expr(module_name, cond)?;
            check_reserved_block(module_name, body)?;
        }
        Expr::For { var, iter, body } => {
            check_reserved_binding(module_name, "loop binding", var)?;
            check_reserved_expr(module_name, iter)?;
            check_reserved_block(module_name, body)?;
        }
        Expr::Range { lo, hi, .. } => {
            check_reserved_expr(module_name, lo)?;
            check_reserved_expr(module_name, hi)?;
        }
        Expr::Index { base, index } => {
            check_reserved_expr(module_name, base)?;
            check_reserved_expr(module_name, index)?;
        }
        Expr::WhileLet { pattern, scrutinee, body } => {
            check_reserved_pattern(module_name, pattern)?;
            check_reserved_expr(module_name, scrutinee)?;
            check_reserved_block(module_name, body)?;
        }
        Expr::Int(_)
        | Expr::Float(_)
        | Expr::Duration(_)
        | Expr::Str(_)
        | Expr::Bool(_)
        | Expr::Var(_)
        | Expr::TaggedLit { .. } => {}
    }
    Ok(())
}

fn check_reserved_pattern(module_name: &str, pattern: &Pattern) -> Result<(), LinkError> {
    match pattern {
        Pattern::Var(name) => check_reserved_binding(module_name, "pattern binding", name),
        Pattern::Ctor { args, .. } | Pattern::Tuple(args) => {
            for arg in args {
                check_reserved_pattern(module_name, arg)?;
            }
            Ok(())
        }
        Pattern::List { elems, rest } => {
            for elem in elems {
                check_reserved_pattern(module_name, elem)?;
            }
            if let Some(Some(name)) = rest {
                check_reserved_binding(module_name, "pattern binding", name)?;
            }
            Ok(())
        }
        Pattern::Or(alts) => {
            for alt in alts {
                check_reserved_pattern(module_name, alt)?;
            }
            Ok(())
        }
        Pattern::Wildcard
        | Pattern::Int(_)
        | Pattern::Str(_)
        | Pattern::Bool(_)
        | Pattern::Duration(_)
        | Pattern::IntRange { .. } => Ok(()),
    }
}

fn private_intrinsic_owner(name: &str) -> Option<&'static [&'static str]> {
    match name {
        "__erase" | "__unerase" => Some(&["chan", "task"]),
        "__bytes_from_string"
        | "__bytes_to_string"
        | "__bytes_length"
        | "__bytes_at"
        | "__bytes_concat"
        | "__bytes_slice" => Some(&["bytes"]),
        _ => None,
    }
}

fn check_private_intrinsic_call(name: &str, module_name: &str) -> Result<(), LinkError> {
    let Some(owners) = private_intrinsic_owner(name) else {
        return Ok(());
    };
    if owners.contains(&module_name) {
        return Ok(());
    }
    lerr(format!(
        "`{name}` is a compiler-private intrinsic for std/{}`; use the public stdlib surface instead",
        owners.join(" or std/")
    ))
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
        assert!(!STD_MODULES.contains(&"csv"));
        assert_eq!(std_source("csv"), None);
        assert_eq!(closest_std_module("csv"), None);
        assert!(STD_MODULES.contains(&"prng"));
        assert_eq!(std_source("random"), None);
        assert_eq!(closest_std_module("random"), Some("prng"));

        // `map` lives in list (and option); a near miss resolves to a real name.
        assert!(closest_std_function("mep").is_some());
        assert_eq!(closest_std_function("zzzzzz"), None);
    }

    fn noop_expand(_: &str, _: &mut Module, _: &[(String, Module)]) -> Result<(), String> {
        Ok(())
    }

    /// Link `lib` (module `sealed_lib`) with `user` (module `user`, the entry) and
    /// return the link error message, if any.
    fn link_lib_user(lib: &str, user: &str) -> Result<(), String> {
        let libm = crate::parser::parse_module(lib).expect("lib parses");
        let userm = crate::parser::parse_module(user).expect("user parses");
        link(
            vec![("sealed_lib".to_string(), libm), ("user".to_string(), userm)],
            "user",
            noop_expand,
        )
        .map(|_| ())
        .map_err(|e| e.message)
    }

    fn link_main(src: &str) -> Result<(), String> {
        let module = crate::parser::parse_module(src).expect("parses");
        link(vec![("main".to_string(), module)], "main", noop_expand)
            .map(|_| ())
            .map_err(|e| e.message)
    }

    #[test]
    fn std_shadowing_module_missing_function_names_shadow() {
        let bytes = crate::parser::parse_module("pub fn unrelated() -> Int:\n    1\n")
            .expect("local bytes parses");
        let main = crate::parser::parse_module(
            "import bytes\n\nfn main(console: Console):\n    let b = bytes.from_string(\"hi\")\n    print(console, \"${bytes.length(b)}\")\n",
        )
        .expect("main parses");
        let user_modules = std::collections::HashSet::from(["bytes".to_string(), "main".to_string()]);
        let err = link_with_user_modules(
            vec![("bytes".to_string(), bytes), ("main".to_string(), main)],
            "main",
            noop_expand,
            &user_modules,
        )
        .expect_err("local bytes should not be mistaken for std/bytes")
        .message;
        assert!(
            err.contains("module `bytes` is provided by this program")
                && err.contains("shadows the bundled standard-library module `bytes`")
                && err.contains("has no function `from_string`"),
            "{err}"
        );
    }

    #[test]
    fn user_source_cannot_declare_compiler_private_names() {
        let err = link_main("type __Hidden:\n    Hidden(Int)\n").unwrap_err();
        assert!(err.contains("type `__Hidden`") && err.contains("reserved for the compiler"), "{err}");

        let err = link_main("type Foo__Int:\n    Foo__Int(Int)\n").unwrap_err();
        assert!(err.contains("type `Foo__Int`") && err.contains("reserved for the compiler"), "{err}");

        let err = link_main("type Box(value__type):\n    Box(value__type)\n").unwrap_err();
        assert!(
            err.contains("type parameter `value__type`") && err.contains("reserved for the compiler"),
            "{err}"
        );

        let err = link_main("type Record:\n    field__name: Int\n").unwrap_err();
        assert!(err.contains("field `field__name`") && err.contains("reserved for the compiler"), "{err}");

        let err = link_main("fn Point__show(x: Int) -> String:\n    \"x\"\n").unwrap_err();
        assert!(
            err.contains("function `Point__show`") && err.contains("reserved for the compiler"),
            "{err}"
        );

        let err = link_main(
            "fn main(console: Console):\n    let __target = 1\n    print(console, __render(__target))\n",
        )
        .unwrap_err();
        assert!(err.contains("binding `__target`"), "{err}");

        let err = link_main(
            "fn main(console: Console):\n    let user__target = 1\n    print(console, __render(user__target))\n",
        )
        .unwrap_err();
        assert!(err.contains("binding `user__target`"), "{err}");
    }

    #[test]
    fn private_bridge_intrinsics_are_std_only() {
        let err = link_main(
            "fn main(console: Console):\n    let s = __unerase(__erase(1))\n    print(console, __render(s))\n",
        )
        .unwrap_err();
        assert!(
            err.contains("`__unerase` is a compiler-private intrinsic")
                || err.contains("`__erase` is a compiler-private intrinsic"),
            "{err}"
        );

        let err = link_main(
            "fn main(console: Console):\n    let b = __bytes_from_string(\"x\")\n    print(console, \"x\")\n",
        )
        .unwrap_err();
        assert!(err.contains("`__bytes_from_string` is a compiler-private intrinsic"), "{err}");

        let bytes = crate::parser::parse_module(std_source("bytes").expect("std bytes")).expect("bytes parses");
        link(vec![("bytes".to_string(), bytes)], "bytes", noop_expand)
            .expect("std/bytes may use the private bytes bridge");
    }

    #[test]
    fn render_intrinsic_remains_available_for_interpolation_oracles() {
        link_main("fn main(console: Console):\n    print(console, __render(1))\n")
            .expect("__render is still the interpolation/oracle spelling");
    }

    #[test]
    fn module_private_functions_are_not_cross_module_api() {
        let lib = "fn hidden(n: Int) -> Int:\n    n + 1\n\n\
                   pub fn shown(n: Int) -> Int:\n    hidden(n)\n";

        let ok = "import sealed_lib\n\n\
                  fn main(console: Console):\n    print(console, __render(sealed_lib.shown(1)))\n";
        link_lib_user(lib, ok).expect("public function may call its private helper");

        let hidden_call = "import sealed_lib\n\n\
                           fn main(console: Console):\n    print(console, __render(sealed_lib.hidden(1)))\n";
        let err = link_lib_user(lib, hidden_call).expect_err("private function must not be module-callable");
        assert!(err.contains("function `sealed_lib.hidden` is private"), "{err}");

        let hidden_ref = "import sealed_lib\n\n\
                          fn main(console: Console):\n    let f = sealed_lib.hidden\n    print(console, \"x\")\n";
        let err = link_lib_user(lib, hidden_ref).expect_err("private function must not be a module value");
        assert!(err.contains("function `sealed_lib.hidden` is private"), "{err}");
    }

    #[test]
    fn plain_import_does_not_bind_bare_functions() {
        // RFC-0042 / BUG-452: `import X` keeps functions qualified. A bare
        // imported call exists only when the source wrote `from X import f`.
        let lib = "pub fn shown(n: Int) -> Int:\n    n + 1\n";

        let plain = "import sealed_lib\n\n\
                     fn main(console: Console):\n    print(console, __render(shown(1)))\n";
        let err = link_lib_user(lib, plain).expect_err("plain import must not bind `shown` bare");
        assert!(
            err.contains("`shown(...)` is not in scope as a bare function")
                && err.contains("sealed_lib.shown(...)")
                && err.contains("from sealed_lib import shown"),
            "{err}"
        );

        let from_import = "from sealed_lib import shown\n\n\
                           fn main(console: Console):\n    print(console, __render(shown(1)))\n";
        link_lib_user(lib, from_import).expect("from-imported public function is callable bare");

        let qualified = "import sealed_lib\n\n\
                         fn main(console: Console):\n    print(console, __render(sealed_lib.shown(1)))\n";
        link_lib_user(lib, qualified).expect("plain import keeps qualified calls available");
    }

    #[test]
    fn sealing_rejects_module_qualified_constructor_and_pattern() {
        // BUG-313: a module-qualified constructor/pattern must not evade the RFC-0002
        // seal check — a sealed capability may be minted/destructured only in its
        // declaring module, on EVERY spelling (fail-closed).
        let lib = "capability Vault from Net\n\n\
                   pub fn make(net: Net) -> Vault:\n    Vault(net)\n\n\
                   pub fn zone(v: Vault) -> String:\n    match v:\n        Vault(n) -> \"z\"\n";

        // CONSTRUCT via `lib.Vault(...)` — rejected.
        let forge = "import sealed_lib\n\n\
                     fn main(console: Console, net: Net):\n    \
                     let forged = sealed_lib.Vault(net)\n    print(console, \"x\")\n";
        let err = link_lib_user(lib, forge).expect_err("qualified construct must be rejected");
        assert!(err.contains("sealed capability") && err.contains("Vault"), "{err}");

        // DESTRUCTURE via `match v: lib.Vault(...)` — rejected.
        let destr = "import sealed_lib\n\n\
                     fn main(console: Console, net: Net):\n    \
                     let v = sealed_lib.make(net)\n    \
                     match v:\n        sealed_lib.Vault(inner) -> print(console, \"leak\")\n";
        let err = link_lib_user(lib, destr).expect_err("qualified destructure must be rejected");
        assert!(err.contains("sealed capability") && err.contains("destructure"), "{err}");

        // Legit: hold and pass a Vault through the lib's exported functions — allowed.
        let holder = "from sealed_lib import Vault\nimport sealed_lib\n\n\
                      fn use_it(v: Vault, console: Console):\n    print(console, sealed_lib.zone(v))\n\n\
                      fn main(console: Console, net: Net):\n    \
                      let v = sealed_lib.make(net)\n    use_it(v, console)\n";
        assert!(link_lib_user(lib, holder).is_ok(), "hold+pass must be allowed");
    }

    #[test]
    fn sealing_rejects_grantable_mint_from_paramless_main() {
        // BUG-313: a grantable root capability minted by `lib.RootCap(...)` from a
        // param-less main forges authority from nothing — rejected.
        let lib = "grantable capability UiRoot:\n    policy: String\n";
        let forge = "import sealed_lib\n\n\
                     fn main(console: Console):\n    \
                     let forged = sealed_lib.UiRoot(\"admin\")\n    print(console, \"x\")\n";
        let err = link_lib_user(lib, forge).expect_err("grantable mint must be rejected");
        assert!(err.contains("sealed capability") && err.contains("UiRoot"), "{err}");
    }

    #[test]
    fn sealed_type_seals_construction_only_not_matching_or_reading() {
        // RFC-0065: a `sealed type` seals only CONSTRUCTION (the smart-constructor
        // choke point) — home-module only, even the qualified `m.Ctor` spelling
        // (BUG-313). Unlike a `capability`, MATCHING/reading are UNAFFECTED
        // (DoD item 4): inspection can't forge an invalid value. The ctor
        // (`BoxData`) is NOT named after the type (`Box`), so this exercises the
        // generalization past the capability case (ctor == type name).
        let lib = "sealed type Box(a):\n    BoxData(a)\n\n\
                   pub fn wrap(x: a) -> Box(a):\n    BoxData(x)\n\n\
                   pub fn unwrap(b: Box(a)) -> a:\n    match b:\n        BoxData(inner) -> inner\n";

        // CONSTRUCT the sealed data ctor from another module — rejected, and named a
        // "sealed type" (not a "sealed capability").
        let forge = "import sealed_lib\n\n\
                     fn main(console: Console):\n    \
                     let b = sealed_lib.BoxData(1)\n    print(console, \"x\")\n";
        let err = link_lib_user(lib, forge).expect_err("sealed-type construct must be rejected");
        assert!(
            err.contains("sealed type") && err.contains("BoxData") && err.contains("construct"),
            "{err}"
        );

        // DESTRUCTURE from another module — ALLOWED (matching is inspection, not
        // construction; it cannot forge an invalid Box). This is the key difference
        // from a capability, whose destructure IS sealed.
        let destr = "import sealed_lib\n\n\
                     fn main(console: Console):\n    \
                     let b = sealed_lib.wrap(1)\n    \
                     match b:\n        sealed_lib.BoxData(inner) -> print(console, \"${inner}\")\n";
        assert!(link_lib_user(lib, destr).is_ok(), "sealed-type match must be allowed");

        // Legit: build via the smart constructor, hold, pass, and read the value out.
        let ok = "import sealed_lib\n\n\
                  fn main(console: Console):\n    \
                  let b = sealed_lib.wrap(41)\n    \
                  print(console, \"${sealed_lib.unwrap(b)}\")\n";
        assert!(link_lib_user(lib, ok).is_ok(), "smart-constructor use must be allowed");
    }

    /// Find the first `MethodCall` whose receiver is `Var(recv)` and method is
    /// `method`, anywhere in the linked entry module.
    fn has_method_call_on(m: &Module, recv: &str, method: &str) -> bool {
        fn in_expr(e: &Expr, recv: &str, method: &str) -> bool {
            match e {
                Expr::MethodCall { receiver, method: mm, args } => {
                    (matches!(receiver.as_ref(), Expr::Var(v) if v == recv) && mm == method)
                        || in_expr(receiver, recv, method)
                        || args.iter().any(|a| in_expr(a, recv, method))
                }
                Expr::Call { args, .. } | Expr::List(args) | Expr::Tuple(args) | Expr::Ctor { args, .. } => {
                    args.iter().any(|a| in_expr(a, recv, method))
                }
                Expr::Binary { lhs, rhs, .. } => in_expr(lhs, recv, method) || in_expr(rhs, recv, method),
                Expr::Block(b) => in_block(b, recv, method),
                _ => false,
            }
        }
        fn in_block(b: &Block, recv: &str, method: &str) -> bool {
            b.stmts.iter().any(|s| match s {
                Stmt::Let { value, .. } | Stmt::Assign { value, .. } | Stmt::LetPattern { value, .. } => {
                    in_expr(value, recv, method)
                }
                Stmt::Return(Some(e)) | Stmt::Expr(e) | Stmt::Yield(e) => in_expr(e, recv, method),
                _ => false,
            })
        }
        m.items.iter().any(|it| matches!(it, Item::Function(f) if in_block(&f.body, recv, method)))
    }

    #[test]
    fn local_named_after_prelude_module_keeps_its_method_call() {
        // BUG-216: `x.f(a)` where `x` is a local named after a prelude module
        // (`list`, `string`, …) is a METHOD CALL on the local, not a hijacked
        // module call — the call position must agree with the value position.
        let src = "type R:\n    x: Int\n\n\
                   impl R:\n    fn get(self, n: Int) -> Int:\n        self.x + n\n\n\
                   fn main(console: Console):\n    \
                   let list = R(x: 1)\n    print(console, __render(list.get(2)))\n";
        let parsed = crate::parser::parse_module(src).expect("parses");
        let linked = link(vec![("main".to_string(), parsed)], "main", noop_expand)
            .expect("links");
        assert!(
            has_method_call_on(&linked, "list", "get"),
            "`list.get(2)` on a shadowing local must stay a method call, not a `list.get` module call"
        );
    }

    #[test]
    fn named_field_construction_of_imported_record_resolves() {
        // BUG-342: `from lib import FieldInfo; FieldInfo(name: ..)` used to fail at
        // link ("not a record type") because the per-module records pass ran
        // before the imported type was visible. It now leaves the construction for
        // the merged pass, which resolves it.
        let lib = "type FieldInfo:\n    name: String\n    type_name: String\n";
        let user = "import rec_lib\nfrom rec_lib import FieldInfo\n\n\
                    fn main(console: Console):\n    \
                    let fi = FieldInfo(name: \"x\", type_name: \"Int\")\n    print(console, fi.name)\n";
        let qualified = "import rec_lib\n\n\
                         fn main(console: Console):\n    \
                         let fi = rec_lib.FieldInfo(name: \"x\", type_name: \"Int\")\n    print(console, fi.name)\n";
        let libm = crate::parser::parse_module(lib).expect("lib parses");
        let userm = crate::parser::parse_module(user).expect("user parses");
        let qualm = crate::parser::parse_module(qualified).expect("qualified user parses");
        fn has_record(m: &Module) -> bool {
            fn e(x: &Expr) -> bool {
                match x {
                    Expr::Record { .. } => true,
                    Expr::Call { args, .. } | Expr::Ctor { args, .. } => args.iter().any(e),
                    Expr::Block(b) => b.stmts.iter().any(|s| matches!(s,
                        Stmt::Let { value, .. } | Stmt::Expr(value) if e(value))),
                    _ => false,
                }
            }
            m.items.iter().any(|it| matches!(it, Item::Function(f)
                if f.body.stmts.iter().any(|s| matches!(s,
                    Stmt::Let { value, .. } | Stmt::Expr(value) if e(value)))))
        }
        for (entry, userm) in [("user", userm), ("qualified", qualm)] {
            let linked = link(
                vec![("rec_lib".to_string(), libm.clone()), (entry.to_string(), userm)],
                entry,
                noop_expand,
            )
            .expect("links without a false named-field construction error");
            // The merged strict pass (run by typeck/backends) must resolve the
            // leftover `Expr::Record` to a positional constructor.
            let lowered = crate::records::lower(linked).expect("merged records pass lowers it");
            assert!(!has_record(&lowered), "{entry}: imported named-field construction must lower to a Ctor");
        }
    }

    #[test]
    fn unimported_std_module_value_gives_missing_import_diagnostic() {
        // BUG-303: `let f = iter.count` without `import iter` must give the
        // missing-import teaching diagnostic (like the call position), not a bare
        // "unbound variable `iter`".
        let src = "fn main(console: Console):\n    let f = iter.count\n    print(console, \"x\")\n";
        let parsed = crate::parser::parse_module(src).expect("parses");
        let err = link(vec![("main".to_string(), parsed)], "main", noop_expand)
            .expect_err("must be a link error")
            .message;
        assert!(err.contains("`iter` is not imported") && err.contains("import iter"), "{err}");
        assert!(!err.contains("unbound variable"), "must not fall through to unbound: {err}");
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

    /// (BUG-160) A `pub fn` inside an inherent `impl <Type>:` block (a capability
    /// policy constructor) is surfaced under its Type — the callable, `check`-accepted
    /// form `Net.tcp(...)` — not the module form `policy.tcp(...)` that `check` rejects.
    /// `which` resolves both the bare name and the qualified `Type.method` query.
    #[test]
    fn which_reports_impl_methods_under_their_type() {
        // The bare name resolves to the Type-qualified, callable form.
        let tcp = std_signatures("tcp");
        assert!(tcp.iter().any(|s| s.starts_with("Net.tcp(")), "bare `tcp` -> Net.tcp: {tcp:?}");
        assert!(!tcp.iter().any(|s| s.starts_with("policy.tcp(")), "no unusable policy.tcp: {tcp:?}");

        // The qualified `Type.method` query resolves.
        let qualified = std_signatures("Net.tcp");
        assert!(
            qualified.iter().any(|s| s.starts_with("Net.tcp(")),
            "qualified `Net.tcp` must resolve: {qualified:?}"
        );
        assert!(std_signatures("Dir.ext").iter().any(|s| s.starts_with("Dir.ext(")));

        // Listing the `policy` module reports every constructor under its Type.
        let exports = module_exports("policy");
        assert!(exports.iter().any(|s| s.starts_with("Net.tcp(")), "{exports:?}");
        assert!(exports.iter().any(|s| s.starts_with("Dir.ext(")), "{exports:?}");
        assert!(!exports.iter().any(|s| s.starts_with("policy.")), "no module-qualified form: {exports:?}");

        // Ordinary module functions stay module-qualified.
        assert!(std_signatures("split").iter().any(|s| s.starts_with("string.split(")));
    }
}
