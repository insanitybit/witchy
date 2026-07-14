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
    GENERATED_RENDER_INTRINSIC,
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
#[derive(Clone)]
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
    /// A compiler-provided module-function alias for an inherent method declared
    /// in this module. `list.map(xs, f)` and `list.map` as a value target the
    /// generated method implementation; the method body stays the single
    /// implementation.
    method_alias: bool,
    /// The generated method implementation this alias targets, e.g. `List__map`.
    /// It is appended by trait/impl lowering, so linker output may reference it
    /// before it exists in the item list.
    alias_target: Option<String>,
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
    "policy", "jwt", "oauth", "rand", "vm", "bytes", "error",
];

/// Bundled modules linked into every program and usable without an explicit
/// import. This is the single prelude registry used by linking and tooling.
pub const PRELUDE_MODULES: &[&str] =
    &["list", "string", "dict", "math", "option", "result", "policy", "show"];

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

/// Link-time policy for entry-specific privileges.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LinkMode {
    /// Production/default linking: sealed constructors are available only in the
    /// module that declares them.
    Production,
    /// `witchy test` linking: the entry module may construct foreign `sealed type`
    /// values so tests can exercise malformed domain data. Sealed capabilities
    /// stay production-strict; mock capability backends are a separate RFC-0077
    /// increment.
    Test,
}

/// Link `modules` (each a name + parsed module) into one flat module, with
/// `entry` the module holding `main`. `expand` runs each module's compile-time
/// passes (see [`ComptimeExpander`]).
pub fn link(
    modules: Vec<(String, Module)>,
    entry: &str,
    expand: ComptimeExpander,
) -> Result<Module, LinkError> {
    link_with_mode(modules, entry, expand, LinkMode::Production)
}

pub fn link_with_mode(
    modules: Vec<(String, Module)>,
    entry: &str,
    expand: ComptimeExpander,
    mode: LinkMode,
) -> Result<Module, LinkError> {
    // Safe by default for in-memory callers that have no source-provenance map:
    // a supplied reserved module is canonical only when its parsed AST matches
    // the compiler-bundled source. Loaders with exact source identity should use
    // `link_with_user_modules`; this fallback prevents LSP/library paths from
    // silently replacing std while still accepting explicitly supplied std ASTs.
    let mut user_modules = std::collections::HashSet::new();
    for (name, module) in &modules {
        if !STD_MODULES.contains(&name.as_str()) {
            continue;
        }
        let source = std_source(name).ok_or_else(|| LinkError {
            message: format!("reserved standard-library module `{name}` has no bundled source"),
        })?;
        let canonical = crate::parser::parse_module(source).map_err(|e| LinkError {
            message: format!("bundled std module `{name}` failed to parse: {e}"),
        })?;
        if module != &canonical {
            user_modules.insert(name.clone());
        }
    }
    link_with_user_modules_with_mode(modules, entry, expand, &user_modules, mode)
}

/// Like [`link`], but with the subset of module names that came from user
/// source files rather than the bundled std fallback. This enforces the
/// canonical ownership of reserved standard-library module names.
pub fn link_with_user_modules(
    modules: Vec<(String, Module)>,
    entry: &str,
    expand: ComptimeExpander,
    user_modules: &std::collections::HashSet<String>,
) -> Result<Module, LinkError> {
    link_with_user_modules_with_mode(modules, entry, expand, user_modules, LinkMode::Production)
}

pub fn link_with_user_modules_with_mode(
    mut modules: Vec<(String, Module)>,
    entry: &str,
    expand: ComptimeExpander,
    user_modules: &std::collections::HashSet<String>,
    mode: LinkMode,
) -> Result<Module, LinkError> {
    check_reserved_source_names(&modules)?;
    if let Some(name) = user_modules
        .iter()
        .filter(|name| STD_MODULES.contains(&name.as_str()))
        .min()
    {
        return lerr(format!(
            "module `{name}` uses a reserved standard-library name — rename the local module; \
             bundled std modules have one canonical owner"
        ));
    }

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
    // `math.sqrt`) resolve without an import line. Their names are reserved;
    // dead-code elimination strips what goes unused.
    //
    // `show` is part of the prelude because interpolation is one protocol, not
    // behavior selected by an otherwise-unused import. User modules cannot
    // shadow bundled std names, so its dependencies always resolve canonically.
    for prelude in PRELUDE_MODULES {
        if !modules.iter().any(|(n, _)| n == prelude) {
            let src = std_source(prelude).ok_or_else(|| LinkError {
                message: format!("prelude module `{prelude}` has no bundled source"),
            })?;
            let m = crate::parser::parse_module(src).map_err(|e| LinkError {
                message: format!("prelude module `{prelude}` failed to parse: {e}"),
            })?;
            modules.push((prelude.to_string(), m));
        }
    }

    // Pull in any imported standard-library module not already present (the
    // std registry is a built-in search path), transitively — so a std module
    // can import another (e.g. `list` importing `option`) and callers need not
    // list the dependency explicitly. User modules cannot claim these reserved
    // names, so every standard-library reference has one canonical owner.
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
    crate::type_resolve::resolve_with_user_modules(&mut modules, user_modules)?;

    // RFC-0002 sealing: a `capability` (a sealed type) may be CONSTRUCTED or
    // DESTRUCTURED only inside the module that declares it. Run AFTER
    // `type_resolve` canonicalizes every constructor and pattern — bare,
    // module-qualified (`lib.Vault(…)`), and from-imported alike — to a single
    // `Expr::Ctor { name: "module.Ctor" }`, so a qualified spelling can no longer
    // slip past the name-keyed check (BUG-313, fail-closed). Modules are still
    // unmerged here, so each item knows its home module.
    check_sealing(&modules, mode, entry)?;

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
                        method_alias: false,
                        alias_target: None,
                    },
                );
            }
        }
        for item in &m.items {
            let Item::Impl(im) = item else { continue };
            if im.trait_name.is_some() {
                continue;
            }
            for method in &im.methods {
                let is_instance_method =
                    method.params.first().is_some_and(|p| p.name == "self");
                if method.public && is_instance_method && !names.contains_key(&method.name) {
                    names.insert(
                        method.name.clone(),
                        EtaSig {
                            arity: method.params.len(),
                            is_var_procedure: method.is_var_procedure(),
                            public: true,
                            method_alias: true,
                            alias_target: Some(inherent_method_symbol(im, &method.name)),
                        },
                    );
                }
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
    let access = LinkAccess { mode, entry };
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
                        access,
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
                                access,
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
                            access,
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
        Expr::Ctor { args, .. } | Expr::AnonCtor { args, .. }
        | Expr::List(args) | Expr::Tuple(args) => {
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
        | Expr::AnonCtor { args, .. }
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

#[derive(Clone, Copy)]
struct LinkAccess<'a> {
    mode: LinkMode,
    entry: &'a str,
}

fn rewrite_block(
    b: &mut Block,
    m: &str,
    imps: &[String],
    bare_imports: Option<&HashMap<String, String>>,
    fns: &FnTable,
    bound: &HashSet<String>,
    access: LinkAccess<'_>,
) -> Result<(), LinkError> {
    for stmt in &mut b.stmts {
        match stmt {
            Stmt::Let { value, .. }
            | Stmt::Assign { value, .. }
            | Stmt::LetPattern { value, .. } => rewrite_expr(value, m, imps, bare_imports, fns, bound, access)?,
            Stmt::Return(Some(e)) | Stmt::Expr(e) | Stmt::Yield(e) => {
                rewrite_expr(e, m, imps, bare_imports, fns, bound, access)?
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
    access: LinkAccess<'_>,
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
                        rewrite_expr(a, m, imps, bare_imports, fns, bound, access)?;
                    }
                    *e = Expr::MethodCall { receiver, method, args: call_args };
                    return Ok(());
                }
            }
            let resolved = resolve_call(name, m, imps, bare_imports, fns, bound, access)?;
            if let Some(sig) = fn_sig(fns, &resolved).filter(|sig| sig.method_alias) {
                *name = sig
                    .alias_target
                    .expect("method aliases carry the generated implementation name");
                for a in args {
                    rewrite_expr(a, m, imps, bare_imports, fns, bound, access)?;
                }
                return Ok(());
            }
            *name = resolved;
            for a in args {
                rewrite_expr(a, m, imps, bare_imports, fns, bound, access)?;
            }
        }
        // (RFC-0056) A labeled direct call: qualify the callee exactly like a plain
        // call so `keyword_args::resolve` can look up its declaration, and rewrite
        // the argument values. The labels ride along untouched until then.
        Expr::LabeledCall { name, args } => {
            let resolved = resolve_call(name, m, imps, bare_imports, fns, bound, access)?;
            if fn_sig(fns, &resolved).is_some_and(|sig| sig.method_alias) {
                return lerr(format!(
                    "`{resolved}` is a method alias; call it as `receiver.{}(...)` \
                     or use the positional module form",
                    resolved.rsplit_once('.').map_or(resolved.as_str(), |(_, name)| name)
                ));
            }
            *name = resolved;
            for (_, a) in args {
                rewrite_expr(a, m, imps, bare_imports, fns, bound, access)?;
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
            rewrite_expr(func, m, imps, bare_imports, fns, bound, access)?;
            for a in args {
                rewrite_expr(a, m, imps, bare_imports, fns, bound, access)?;
            }
        }
        Expr::Ctor { args, .. } | Expr::AnonCtor { args, .. }
        | Expr::List(args) | Expr::Tuple(args) => {
            for a in args {
                rewrite_expr(a, m, imps, bare_imports, fns, bound, access)?;
            }
        }
        Expr::Unary { expr, .. } | Expr::Try(expr) | Expr::As { expr, .. } => {
            rewrite_expr(expr, m, imps, bare_imports, fns, bound, access)?
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
                    resolve_call(&format!("{modname}.{field}"), m, imps, bare_imports, fns, bound, access)?;
                let sig = fns
                    .get(&modname)
                    .and_then(|s| s.get(&field))
                    .cloned()
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
                *e = eta_lambda(&qualified, sig);
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
            rewrite_expr(base, m, imps, bare_imports, fns, bound, access)?;
        }
        Expr::RecordUpdate { name: _, base, fields } => {
            rewrite_expr(base, m, imps, bare_imports, fns, bound, access)?;
            for (_, value) in fields {
                rewrite_expr(value, m, imps, bare_imports, fns, bound, access)?;
            }
        }
        Expr::Record { fields, spread, .. } => {
            for (_, value) in fields {
                rewrite_expr(value, m, imps, bare_imports, fns, bound, access)?;
            }
            if let Some(s) = spread {
                rewrite_expr(s, m, imps, bare_imports, fns, bound, access)?;
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            rewrite_expr(lhs, m, imps, bare_imports, fns, bound, access)?;
            rewrite_expr(rhs, m, imps, bare_imports, fns, bound, access)?;
        }
        Expr::Range { lo, hi, .. } => {
            rewrite_expr(lo, m, imps, bare_imports, fns, bound, access)?;
            rewrite_expr(hi, m, imps, bare_imports, fns, bound, access)?;
        }
        Expr::Index { base, index } => {
            rewrite_expr(base, m, imps, bare_imports, fns, bound, access)?;
            rewrite_expr(index, m, imps, bare_imports, fns, bound, access)?;
        }
        // Lowered to a plain `Call` before this runs; recurse for safety.
        Expr::MethodCall { receiver, args, .. } => {
            rewrite_expr(receiver, m, imps, bare_imports, fns, bound, access)?;
            for a in args {
                rewrite_expr(a, m, imps, bare_imports, fns, bound, access)?;
            }
        }
        Expr::WhileLet { scrutinee, body, .. } => {
            rewrite_expr(scrutinee, m, imps, bare_imports, fns, bound, access)?;
            rewrite_block(body, m, imps, bare_imports, fns, bound, access)?;
        }
        Expr::If {
            cond,
            then_block,
            else_block,
        } => {
            rewrite_expr(cond, m, imps, bare_imports, fns, bound, access)?;
            rewrite_block(then_block, m, imps, bare_imports, fns, bound, access)?;
            if let Some(b) = else_block {
                rewrite_block(b, m, imps, bare_imports, fns, bound, access)?;
            }
        }
        Expr::Lambda { body, .. } => rewrite_block(body, m, imps, bare_imports, fns, bound, access)?,
        Expr::Block(b) => rewrite_block(b, m, imps, bare_imports, fns, bound, access)?,
        Expr::While { cond, body } => {
            rewrite_expr(cond, m, imps, bare_imports, fns, bound, access)?;
            rewrite_block(body, m, imps, bare_imports, fns, bound, access)?;
        }
        Expr::For { iter, body, .. } => {
            rewrite_expr(iter, m, imps, bare_imports, fns, bound, access)?;
            rewrite_block(body, m, imps, bare_imports, fns, bound, access)?;
        }
        Expr::Match { scrutinee, arms } => {
            rewrite_expr(scrutinee, m, imps, bare_imports, fns, bound, access)?;
            for arm in arms {
                if let Some(g) = &mut arm.guard {
                    rewrite_expr(g, m, imps, bare_imports, fns, bound, access)?;
                }
                rewrite_expr(&mut arm.body, m, imps, bare_imports, fns, bound, access)?;
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
    PRELUDE_MODULES.contains(&name)
}

/// (RFC-0050 Part 2) Build the eta-expansion of a module-function reference in
/// value position: `list.length` (arity 1) becomes `fn(__eta0): list.length(__eta0)`.
/// The lambda captures nothing and its parameters carry no type annotation, so the
/// ordinary checker infers them — for a generic callee, RFC-0046's annotate/mono
/// fixpoint resolves the type-var parameters. A source-to-source rewrite on the
/// single linked AST before either backend lowers: parity by construction. The
/// arity is the callee's FULL declared parameter count; RFC-0056 defaults never
/// attach to a function value, so every positional argument is present.
fn eta_lambda(qualified: &str, sig: EtaSig) -> Expr {
    let arity = sig.arity;
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
    let call = if sig.method_alias {
        Expr::Call {
            name: sig
                .alias_target
                .expect("method aliases carry the generated implementation name"),
            args,
        }
    } else {
        Expr::Call { name: qualified.to_string(), args }
    };
    Expr::Lambda {
        params,
        body: Block { stmts: vec![Stmt::Expr(call)], lines: vec![0], region: None },
        ret: None,
    }
}

fn fn_sig(fns: &FnTable, qualified: &str) -> Option<EtaSig> {
    let (modname, fname) = qualified.split_once('.')?;
    fns.get(modname).and_then(|s| s.get(fname)).cloned()
}

fn inherent_method_symbol(im: &ImplDef, method: &str) -> String {
    format!("{}__{method}", im.type_name)
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

fn check_sealing(modules: &[(String, Module)], mode: LinkMode, entry: &str) -> Result<(), LinkError> {
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
        let allow_test_sealed_type_construction = mode == LinkMode::Test && mname == entry;
        for item in &m.items {
            match item {
                Item::Function(f) => {
                    seal_block(&f.body, &sealed, mname, allow_test_sealed_type_construction)?;
                }
                Item::Impl(im) => {
                    for method in &im.methods {
                        seal_block(&method.body, &sealed, mname, allow_test_sealed_type_construction)?;
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
    sealed: &SealMap,
    home: &str,
    verb: &str,
    allow_test_sealed_type_construction: bool,
) -> Result<(), LinkError> {
    if let Some((decl, is_capability)) = sealed.get(name) {
        // A `sealed type` (RFC-0065) restricts only CONSTRUCTION; matching/reading
        // are unaffected (DoD item 4). A `capability` (RFC-0002) additionally seals
        // DESTRUCTURING — its carried authority must not be unwrapped elsewhere.
        if verb == "destructure" && !is_capability {
            return Ok(());
        }
        if decl != home {
            if verb == "construct" && !is_capability && allow_test_sealed_type_construction {
                return Ok(());
            }
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

fn seal_block(
    b: &Block,
    sealed: &SealMap,
    home: &str,
    allow_test_sealed_type_construction: bool,
) -> Result<(), LinkError> {
    for stmt in &b.stmts {
        match stmt {
            Stmt::Let { value, .. } | Stmt::Assign { value, .. } | Stmt::LetPattern { value, .. } => {
                seal_expr(value, sealed, home, allow_test_sealed_type_construction)?
            }
            Stmt::Return(Some(e)) | Stmt::Expr(e) | Stmt::Yield(e) => {
                seal_expr(e, sealed, home, allow_test_sealed_type_construction)?
            }
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
    }
    Ok(())
}

fn seal_pattern(
    p: &Pattern,
    sealed: &SealMap,
    home: &str,
    allow_test_sealed_type_construction: bool,
) -> Result<(), LinkError> {
    match p {
        Pattern::Ctor { name, args } => {
            seal_use(name, sealed, home, "destructure", allow_test_sealed_type_construction)?;
            for a in args {
                seal_pattern(a, sealed, home, allow_test_sealed_type_construction)?;
            }
        }
        Pattern::AnonCtor { args, .. } => {
            for a in args {
                seal_pattern(a, sealed, home, allow_test_sealed_type_construction)?;
            }
        }
        Pattern::Tuple(ps) => {
            for q in ps {
                seal_pattern(q, sealed, home, allow_test_sealed_type_construction)?;
            }
        }
        Pattern::List { elems, .. } => {
            for q in elems {
                seal_pattern(q, sealed, home, allow_test_sealed_type_construction)?;
            }
        }
        Pattern::Or(alts) => {
            for q in alts {
                seal_pattern(q, sealed, home, allow_test_sealed_type_construction)?;
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

fn seal_expr(
    e: &Expr,
    sealed: &SealMap,
    home: &str,
    allow_test_sealed_type_construction: bool,
) -> Result<(), LinkError> {
    match e {
        Expr::Ctor { name, args } => {
            seal_use(name, sealed, home, "construct", allow_test_sealed_type_construction)?;
            for a in args {
                seal_expr(a, sealed, home, allow_test_sealed_type_construction)?;
            }
        }
        Expr::Call { args, .. } | Expr::AnonCtor { args, .. }
        | Expr::List(args) | Expr::Tuple(args) => {
            for a in args {
                seal_expr(a, sealed, home, allow_test_sealed_type_construction)?;
            }
        }
        Expr::LabeledCall { args, .. } => {
            for (_, a) in args {
                seal_expr(a, sealed, home, allow_test_sealed_type_construction)?;
            }
        }
        Expr::Apply { func, args } => {
            seal_expr(func, sealed, home, allow_test_sealed_type_construction)?;
            for a in args {
                seal_expr(a, sealed, home, allow_test_sealed_type_construction)?;
            }
        }
        Expr::MethodCall { receiver, args, .. } => {
            seal_expr(receiver, sealed, home, allow_test_sealed_type_construction)?;
            for a in args {
                seal_expr(a, sealed, home, allow_test_sealed_type_construction)?;
            }
        }
        Expr::Unary { expr, .. }
        | Expr::Try(expr)
        | Expr::As { expr, .. }
        | Expr::Field { base: expr, .. } => {
            seal_expr(expr, sealed, home, allow_test_sealed_type_construction)?;
        }
        Expr::RecordUpdate { name: _, base, fields } => {
            seal_expr(base, sealed, home, allow_test_sealed_type_construction)?;
            for (_, v) in fields {
                seal_expr(v, sealed, home, allow_test_sealed_type_construction)?;
            }
        }
        Expr::Record { fields, spread, .. } => {
            for (_, v) in fields {
                seal_expr(v, sealed, home, allow_test_sealed_type_construction)?;
            }
            if let Some(s) = spread {
                seal_expr(s, sealed, home, allow_test_sealed_type_construction)?;
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            seal_expr(lhs, sealed, home, allow_test_sealed_type_construction)?;
            seal_expr(rhs, sealed, home, allow_test_sealed_type_construction)?;
        }
        Expr::Range { lo, hi, .. } => {
            seal_expr(lo, sealed, home, allow_test_sealed_type_construction)?;
            seal_expr(hi, sealed, home, allow_test_sealed_type_construction)?;
        }
        Expr::Index { base, index } => {
            seal_expr(base, sealed, home, allow_test_sealed_type_construction)?;
            seal_expr(index, sealed, home, allow_test_sealed_type_construction)?;
        }
        Expr::WhileLet { pattern, scrutinee, body } => {
            seal_pattern(pattern, sealed, home, allow_test_sealed_type_construction)?;
            seal_expr(scrutinee, sealed, home, allow_test_sealed_type_construction)?;
            seal_block(body, sealed, home, allow_test_sealed_type_construction)?;
        }
        Expr::If { cond, then_block, else_block } => {
            seal_expr(cond, sealed, home, allow_test_sealed_type_construction)?;
            seal_block(then_block, sealed, home, allow_test_sealed_type_construction)?;
            if let Some(b) = else_block {
                seal_block(b, sealed, home, allow_test_sealed_type_construction)?;
            }
        }
        Expr::Lambda { body, .. } => seal_block(body, sealed, home, allow_test_sealed_type_construction)?,
        Expr::Block(b) => seal_block(b, sealed, home, allow_test_sealed_type_construction)?,
        Expr::While { cond, body } => {
            seal_expr(cond, sealed, home, allow_test_sealed_type_construction)?;
            seal_block(body, sealed, home, allow_test_sealed_type_construction)?;
        }
        Expr::For { iter, body, .. } => {
            seal_expr(iter, sealed, home, allow_test_sealed_type_construction)?;
            seal_block(body, sealed, home, allow_test_sealed_type_construction)?;
        }
        Expr::Match { scrutinee, arms } => {
            seal_expr(scrutinee, sealed, home, allow_test_sealed_type_construction)?;
            for arm in arms {
                seal_pattern(&arm.pattern, sealed, home, allow_test_sealed_type_construction)?;
                if let Some(g) = &arm.guard {
                    seal_expr(g, sealed, home, allow_test_sealed_type_construction)?;
                }
                seal_expr(&arm.body, sealed, home, allow_test_sealed_type_construction)?;
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
    access: LinkAccess<'_>,
) -> Result<String, LinkError> {
    let accept = |resolved: String| -> Result<String, LinkError> {
        check_test_only_call(&resolved, m, access)?;
        Ok(resolved)
    };
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
            Some(_) if modname == m => accept(name.to_string()),
            Some(_) if private_intrinsic_friend_call(modname, fname, m) => {
                accept(name.to_string())
            }
            Some(sig) if sig.public => accept(name.to_string()),
            Some(_) => lerr(format!("function `{modname}.{fname}` is private to module `{modname}`")),
            None => lerr(format!("module `{modname}` has no function `{fname}`")),
        };
    }
    // A function defined in THIS module wins over a builtin of the same name, so
    // e.g. `list.contains` is reachable as a bare `contains` inside `list` (a
    // builtin would otherwise shadow it). Checked before BUILTINS for that
    // reason.
    if fns.get(m).is_some_and(|s| s.contains_key(name)) {
        return accept(format!("{m}.{name}"));
    }
    if BUILTINS.contains(&name) {
        return Ok(name.to_string());
    }
    if !bound.contains(name) {
        if let Some(srcmod) = bare_imports.and_then(|imports| imports.get(name)) {
            return accept(format!("{srcmod}.{name}"));
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

fn check_test_only_call(
    resolved: &str,
    module_name: &str,
    access: LinkAccess<'_>,
) -> Result<(), LinkError> {
    if resolved != "testing.mock_dir" {
        return Ok(());
    }
    if access.mode == LinkMode::Test && module_name == access.entry {
        return Ok(());
    }
    lerr(
        "`testing.mock_dir` is available only inside the entry module run by \
         `witchy test`; production code and dependency test modules cannot mint \
         mock capabilities",
    )
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

fn is_generated_anon_union_name(name: &str) -> bool {
    name.strip_prefix("__union")
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

fn is_synthetic_source_line(line: Option<u32>) -> bool {
    matches!(line, Some(0 | u32::MAX))
}

fn check_reserved_source_names(modules: &[(String, Module)]) -> Result<(), LinkError> {
    for (module_name, module) in modules {
        if STD_MODULES.contains(&module_name.as_str()) {
            continue;
        }
        let mut generated_anon_types: HashSet<&str> = HashSet::new();
        for (idx, item) in module.items.iter().enumerate() {
            let line = module.item_lines.get(idx).copied();
            if let Item::Type(t) = item {
                if is_generated_anon_type(&t.name, line) {
                    generated_anon_types.insert(t.name.as_str());
                }
            }
        }
        for (idx, item) in module.items.iter().enumerate() {
            let line = module.item_lines.get(idx).copied();
            check_reserved_item(module_name, item, line, &generated_anon_types)?;
        }
    }
    Ok(())
}

fn check_reserved_item(
    module_name: &str,
    item: &Item,
    line: Option<u32>,
    generated_anon_types: &HashSet<&str>,
) -> Result<(), LinkError> {
    match item {
        Item::Function(f) => {
            if is_reserved_user_identifier(&f.name) {
                return reserved_name_error(module_name, "function", &f.name);
            }
            check_reserved_function(module_name, f, generated_anon_types)
        }
        Item::Type(t) => {
            let generated_anon = is_generated_anon_type(&t.name, line);
            if is_reserved_user_identifier(&t.name) && !generated_anon {
                return reserved_name_error(module_name, "type", &t.name);
            }
            for param in &t.params {
                check_reserved_binding(module_name, "type parameter", param, line)?;
            }
            for variant in &t.variants {
                if is_reserved_user_identifier(&variant.name) && !generated_anon {
                    return reserved_name_error(module_name, "constructor", &variant.name);
                }
                for field_name in &variant.field_names {
                    check_reserved_binding(module_name, "field", field_name, line)?;
                }
            }
            Ok(())
        }
        Item::Trait(t) => {
            if is_reserved_user_identifier(&t.name) {
                return reserved_name_error(module_name, "trait", &t.name);
            }
            for param in &t.typarams {
                check_reserved_binding(module_name, "type parameter", param, line)?;
            }
            for method in &t.methods {
                if is_reserved_user_identifier(&method.name) {
                    return reserved_name_error(module_name, "trait method", &method.name);
                }
                for p in &method.params {
                    check_reserved_param(module_name, p, generated_anon_types)?;
                    if let Some(default) = &p.default {
                        check_reserved_expr(module_name, default, line, generated_anon_types)?;
                    }
                }
                if let Some(body) = &method.default {
                    check_reserved_block(module_name, body, generated_anon_types)?;
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
                check_reserved_function(module_name, method, generated_anon_types)?;
            }
            Ok(())
        }
        Item::Const { name, value } => {
            check_reserved_binding(module_name, "constant", name, line)?;
            check_reserved_expr(module_name, value, line, generated_anon_types)
        }
        Item::TypeAlias { name, ty, .. } => {
            check_reserved_binding(module_name, "type alias", name, line)?;
            check_reserved_type(module_name, ty, generated_anon_types)
        }
        Item::Comptime(body) => check_reserved_block(module_name, body, generated_anon_types),
    }
}

fn check_reserved_function(
    module_name: &str,
    f: &Function,
    generated_anon_types: &HashSet<&str>,
) -> Result<(), LinkError> {
    for p in &f.params {
        check_reserved_param(module_name, p, generated_anon_types)?;
    }
    check_reserved_block(module_name, &f.body, generated_anon_types)
}

fn check_reserved_param(
    module_name: &str,
    p: &Param,
    generated_anon_types: &HashSet<&str>,
) -> Result<(), LinkError> {
    check_reserved_binding(module_name, "parameter", &p.name, None)?;
    if let Some(ty) = &p.ty {
        check_reserved_type(module_name, ty, generated_anon_types)?;
    }
    if let Some(default) = &p.default {
        check_reserved_expr(module_name, default, None, generated_anon_types)?;
    }
    Ok(())
}

fn check_reserved_binding(
    module_name: &str,
    kind: &str,
    name: &str,
    line: Option<u32>,
) -> Result<(), LinkError> {
    let generated_local = is_generated_local_name(name) && is_synthetic_source_line(line);
    if is_reserved_user_identifier(name) && !generated_local {
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

fn check_reserved_type(
    module_name: &str,
    ty: &Type,
    generated_anon_types: &HashSet<&str>,
) -> Result<(), LinkError> {
    match ty {
        Type::Named(name, args) => {
            // Anonymous record type-position syntax is parsed into the same
            // shape-keyed synthetic generic record names that value-position
            // `.{...}` already uses. They are compiler-private names, but they
            // are generated by the parser for this module and must survive this
            // reserved-name source walk so the ordinary type/linker machinery can
            // use them.
            if is_reserved_user_identifier(name)
                && !generated_anon_types.contains(name.as_str())
                && !is_generated_anon_union_name(name)
            {
                return reserved_name_error(module_name, "type", name);
            }
            for arg in args {
                check_reserved_type(module_name, arg, generated_anon_types)?;
            }
        }
        Type::Tuple(items) => {
            for item in items {
                check_reserved_type(module_name, item, generated_anon_types)?;
            }
        }
        Type::Fn(params, ret, _) => {
            for param in params {
                check_reserved_type(module_name, param, generated_anon_types)?;
            }
            check_reserved_type(module_name, ret, generated_anon_types)?;
        }
        Type::Qualified(_, inner) => check_reserved_type(module_name, inner, generated_anon_types)?,
    }
    Ok(())
}

fn check_reserved_block(
    module_name: &str,
    block: &Block,
    generated_anon_types: &HashSet<&str>,
) -> Result<(), LinkError> {
    for (idx, stmt) in block.stmts.iter().enumerate() {
        check_reserved_stmt(module_name, stmt, block.lines.get(idx).copied(), generated_anon_types)?;
    }
    Ok(())
}

fn check_reserved_stmt(
    module_name: &str,
    stmt: &Stmt,
    line: Option<u32>,
    generated_anon_types: &HashSet<&str>,
) -> Result<(), LinkError> {
    match stmt {
        Stmt::Let { name, ty, value, .. } => {
            check_reserved_binding(module_name, "binding", name, line)?;
            if let Some(ty) = ty {
                check_reserved_type(module_name, ty, generated_anon_types)?;
            }
            check_reserved_expr(module_name, value, line, generated_anon_types)
        }
        Stmt::Assign { name, value } => {
            check_reserved_binding(module_name, "assignment target", name, line)?;
            check_reserved_expr(module_name, value, line, generated_anon_types)
        }
        Stmt::LetPattern { pattern, value } => {
            check_reserved_pattern(module_name, pattern, line)?;
            check_reserved_expr(module_name, value, line, generated_anon_types)
        }
        Stmt::Return(Some(value)) | Stmt::Expr(value) | Stmt::Yield(value) => {
            check_reserved_expr(module_name, value, line, generated_anon_types)
        }
        Stmt::Return(None) | Stmt::Break | Stmt::Continue => Ok(()),
    }
}

fn check_reserved_expr(
    module_name: &str,
    expr: &Expr,
    line: Option<u32>,
    generated_anon_types: &HashSet<&str>,
) -> Result<(), LinkError> {
    match expr {
        Expr::List(items) | Expr::Tuple(items) | Expr::Ctor { args: items, .. }
        | Expr::AnonCtor { args: items, .. } => {
            for item in items {
                check_reserved_expr(module_name, item, line, generated_anon_types)?;
            }
        }
        Expr::Call { args, .. } => {
            for arg in args {
                check_reserved_expr(module_name, arg, line, generated_anon_types)?;
            }
        }
        Expr::LabeledCall { args, .. } => {
            for (_, arg) in args {
                check_reserved_expr(module_name, arg, line, generated_anon_types)?;
            }
        }
        Expr::MethodCall { receiver, args, .. } => {
            check_reserved_expr(module_name, receiver, line, generated_anon_types)?;
            for arg in args {
                check_reserved_expr(module_name, arg, line, generated_anon_types)?;
            }
        }
        Expr::Apply { func, args } => {
            check_reserved_expr(module_name, func, line, generated_anon_types)?;
            for arg in args {
                check_reserved_expr(module_name, arg, line, generated_anon_types)?;
            }
        }
        Expr::Lambda { params, body, ret } => {
            for param in params {
                check_reserved_param(module_name, param, generated_anon_types)?;
            }
            if let Some(ret) = ret {
                check_reserved_type(module_name, ret, generated_anon_types)?;
            }
            check_reserved_block(module_name, body, generated_anon_types)?;
        }
        Expr::RecordUpdate { name: _, base, fields } => {
            check_reserved_expr(module_name, base, line, generated_anon_types)?;
            for (field, value) in fields {
                check_reserved_binding(module_name, "field", field, line)?;
                check_reserved_expr(module_name, value, line, generated_anon_types)?;
            }
        }
        Expr::Record { fields, spread, .. } => {
            for (field, value) in fields {
                check_reserved_binding(module_name, "field", field, line)?;
                check_reserved_expr(module_name, value, line, generated_anon_types)?;
            }
            if let Some(spread) = spread {
                check_reserved_expr(module_name, spread, line, generated_anon_types)?;
            }
        }
        Expr::Unary { expr, .. } | Expr::Try(expr) | Expr::As { expr, .. } | Expr::Field { base: expr, .. } => {
            check_reserved_expr(module_name, expr, line, generated_anon_types)?
        }
        Expr::Binary { lhs, rhs, .. } => {
            check_reserved_expr(module_name, lhs, line, generated_anon_types)?;
            check_reserved_expr(module_name, rhs, line, generated_anon_types)?;
        }
        Expr::If { cond, then_block, else_block } => {
            check_reserved_expr(module_name, cond, line, generated_anon_types)?;
            check_reserved_block(module_name, then_block, generated_anon_types)?;
            if let Some(block) = else_block {
                check_reserved_block(module_name, block, generated_anon_types)?;
            }
        }
        Expr::Match { scrutinee, arms } => {
            check_reserved_expr(module_name, scrutinee, line, generated_anon_types)?;
            for arm in arms {
                check_reserved_pattern(module_name, &arm.pattern, line)?;
                if let Some(guard) = &arm.guard {
                    check_reserved_expr(module_name, guard, line, generated_anon_types)?;
                }
                check_reserved_expr(module_name, &arm.body, line, generated_anon_types)?;
            }
        }
        Expr::Block(block) => check_reserved_block(module_name, block, generated_anon_types)?,
        Expr::While { cond, body } => {
            check_reserved_expr(module_name, cond, line, generated_anon_types)?;
            check_reserved_block(module_name, body, generated_anon_types)?;
        }
        Expr::For { var, iter, body } => {
            let loop_line = if is_generated_local_name(var) { Some(0) } else { line };
            check_reserved_binding(module_name, "loop binding", var, loop_line)?;
            check_reserved_expr(module_name, iter, line, generated_anon_types)?;
            check_reserved_block(module_name, body, generated_anon_types)?;
        }
        Expr::Range { lo, hi, .. } => {
            check_reserved_expr(module_name, lo, line, generated_anon_types)?;
            check_reserved_expr(module_name, hi, line, generated_anon_types)?;
        }
        Expr::Index { base, index } => {
            check_reserved_expr(module_name, base, line, generated_anon_types)?;
            check_reserved_expr(module_name, index, line, generated_anon_types)?;
        }
        Expr::WhileLet { pattern, scrutinee, body } => {
            check_reserved_pattern(module_name, pattern, line)?;
            check_reserved_expr(module_name, scrutinee, line, generated_anon_types)?;
            check_reserved_block(module_name, body, generated_anon_types)?;
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

fn check_reserved_pattern(
    module_name: &str,
    pattern: &Pattern,
    line: Option<u32>,
) -> Result<(), LinkError> {
    match pattern {
        Pattern::Var(name) => check_reserved_binding(module_name, "pattern binding", name, line),
        Pattern::Ctor { args, .. } | Pattern::AnonCtor { args, .. } | Pattern::Tuple(args) => {
            for arg in args {
                check_reserved_pattern(module_name, arg, line)?;
            }
            Ok(())
        }
        Pattern::List { elems, rest } => {
            for elem in elems {
                check_reserved_pattern(module_name, elem, line)?;
            }
            if let Some(Some(name)) = rest {
                check_reserved_binding(module_name, "pattern binding", name, line)?;
            }
            Ok(())
        }
        Pattern::Or(alts) => {
            for alt in alts {
                check_reserved_pattern(module_name, alt, line)?;
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

fn check_private_intrinsic_call(name: &str, module_name: &str) -> Result<(), LinkError> {
    let intrinsic = name.rsplit_once('.').map_or(name, |(_, bare)| bare);
    if intrinsic == crate::intrinsics::RETIRED_SOURCE_RENDER {
        return lerr(
            "`__render` is compiler-private; use string interpolation (`\"${value}\"`) \
             or `show.render(value)` instead",
        );
    }
    let Some(owners) = crate::intrinsics::private_intrinsic_callers(intrinsic) else {
        return Ok(());
    };
    if owners.contains(&module_name) {
        return Ok(());
    }
    lerr(format!(
        "`{intrinsic}` is a compiler-private intrinsic for std/{}`; use the public stdlib surface instead",
        owners.join(" or std/")
    ))
}

fn private_intrinsic_friend_call(provider: &str, name: &str, caller: &str) -> bool {
    provider == "task"
        && crate::intrinsics::is_channel_bridge(name)
        && crate::intrinsics::private_intrinsic_callers(name)
            .is_some_and(|callers| callers.contains(&caller))
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

    #[test]
    fn std_registry_matches_bundled_source_files() {
        let std_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std");
        let files: std::collections::BTreeSet<String> = std::fs::read_dir(std_dir)
            .expect("std directory")
            .filter_map(|entry| {
                let path = entry.ok()?.path();
                if path.extension().and_then(|ext| ext.to_str()) != Some("witchy") {
                    return None;
                }
                path.file_stem()?.to_str().map(str::to_string)
            })
            .collect();
        let registered: std::collections::BTreeSet<String> =
            STD_MODULES.iter().map(|name| (*name).to_string()).collect();
        assert_eq!(registered.len(), STD_MODULES.len(), "STD_MODULES contains a duplicate");
        assert_eq!(registered, files, "STD_MODULES must match std/*.witchy");
        for module in STD_MODULES {
            assert!(std_source(module).is_some(), "missing bundled source for `{module}`");
        }
    }

    fn noop_expand(_: &str, _: &mut Module, _: &[(String, Module)]) -> Result<(), String> {
        Ok(())
    }

    /// Link `lib` (module `sealed_lib`) with `user` (module `user`, the entry) and
    /// return the link error message, if any.
    fn link_lib_user(lib: &str, user: &str) -> Result<(), String> {
        link_lib_user_with_mode(lib, user, LinkMode::Production)
    }

    fn link_lib_user_with_mode(lib: &str, user: &str, mode: LinkMode) -> Result<(), String> {
        let libm = crate::parser::parse_module(lib).expect("lib parses");
        let userm = crate::parser::parse_module(user).expect("user parses");
        link_with_mode(
            vec![("sealed_lib".to_string(), libm), ("user".to_string(), userm)],
            "user",
            noop_expand,
            mode,
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
    fn user_module_cannot_shadow_bundled_std_module() {
        let bytes = crate::parser::parse_module("pub fn unrelated() -> Int:\n    1\n")
            .expect("local bytes parses");
        let main = crate::parser::parse_module(
            "import bytes\n\nfn main(console: Console):\n    let b = bytes.from_string(\"hi\")\n    console.print(\"${bytes.length(b)}\")\n",
        )
        .expect("main parses");
        let err = link(
            vec![("bytes".to_string(), bytes), ("main".to_string(), main)],
            "main",
            noop_expand,
        )
        .expect_err("default in-memory linking must reserve bundled std module names")
        .message;
        assert!(
            err.contains("module `bytes` uses a reserved standard-library name")
                && err.contains("one canonical owner"),
            "{err}"
        );
    }

    #[test]
    fn user_source_cannot_declare_compiler_private_names() {
        let err = link_main("type __Hidden:\n    Hidden(Int)\n").unwrap_err();
        assert!(err.contains("type `__Hidden`") && err.contains("reserved for the compiler"), "{err}");

        let err = link_main("type Foo__Int:\n    Foo__Int(Int)\n").unwrap_err();
        assert!(err.contains("type `Foo__Int`") && err.contains("reserved for the compiler"), "{err}");

        let err = link_main("type Sneak = __anon00000000010000000001120\n").unwrap_err();
        assert!(err.contains("type `__anon00000000010000000001120`") && err.contains("reserved for the compiler"), "{err}");

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
            "fn main(console: Console):\n    let __target = 1\n    console.print(\"${__target}\")\n",
        )
        .unwrap_err();
        assert!(err.contains("binding `__target`"), "{err}");

        let err = link_main(
            "fn main(console: Console):\n    let user__target = 1\n    console.print(\"${user__target}\")\n",
        )
        .unwrap_err();
        assert!(err.contains("binding `user__target`"), "{err}");

        let err = link_main(
            "fn main(console: Console):\n    let __compr0 = 1\n    console.print(\"${__compr0}\")\n",
        )
        .unwrap_err();
        assert!(err.contains("binding `__compr0`"), "{err}");

        let err = crate::parser::parse_module(
            "fn main(console: Console):\n    for __fv0 in [1]:\n        console.print(\"${__fv0}\")\n",
        )
        .expect_err("source loop variables cannot use generated compiler names");
        assert!(err.message.contains("identifier `__fv0` is reserved for the compiler"), "{err:?}");

        let err = crate::parser::parse_module(
            "fn main(console: Console):\n    var xs = [1]\n    for var __fv0 in xs:\n        __fv0 = __fv0 + 1\n",
        )
        .expect_err("source for-var variables cannot use generated compiler names");
        assert!(err.message.contains("identifier `__fv0` is reserved for the compiler"), "{err:?}");

        let err = link_main(
            "fn main(console: Console):\n    let (__fortuple0, n) = (1, 2)\n    console.print(\"${n}\")\n",
        )
        .unwrap_err();
        assert!(err.contains("pattern binding `__fortuple0`"), "{err}");

        link_main(
            "fn main(console: Console):\n    let xs = [n * 2 for n in [1, 2, 3]]\n    console.print(\"${xs}\")\n",
        )
        .expect("parser-generated list-comprehension names stay legal");

        link_main(
            "fn main(console: Console):\n    var xs = [1, 2, 3]\n    for var n in xs:\n        n = n + 1\n    console.print(\"ok\")\n",
        )
        .expect("parser-generated for-var index names stay legal");
    }

    #[test]
    fn private_bridge_intrinsics_are_std_only() {
        let err = link_main(
            "fn main(console: Console):\n    let s = __unerase(__erase(1))\n    console.print(\"${s}\")\n",
        )
        .unwrap_err();
        assert!(
            err.contains("`__unerase` is a compiler-private intrinsic")
                || err.contains("`__erase` is a compiler-private intrinsic"),
            "{err}"
        );

        let err = link_main(
            "fn main(console: Console):\n    let b = __bytes_from_string(\"x\")\n    console.print(\"x\")\n",
        )
        .unwrap_err();
        assert!(err.contains("`__bytes_from_string` is a compiler-private intrinsic"), "{err}");

        let err = link_main(
            "import task\n\nfn main(console: Console):\n    let _raw = task.__channel_open(0)\n    console.print(\"x\")\n",
        )
        .unwrap_err();
        assert!(err.contains("`__channel_open` is a compiler-private intrinsic"), "{err}");

        let bytes = crate::parser::parse_module(std_source("bytes").expect("std bytes")).expect("bytes parses");
        link(vec![("bytes".to_string(), bytes)], "bytes", noop_expand)
            .expect("std/bytes may use the private bytes bridge");

        let task = crate::parser::parse_module(std_source("task").expect("std task")).expect("task parses");
        let chan = crate::parser::parse_module(std_source("chan").expect("std chan")).expect("chan parses");
        link(
            vec![("task".to_string(), task), ("chan".to_string(), chan)],
            "chan",
            noop_expand,
        )
        .expect("std/chan may use task's private channel bridge");
    }

    #[test]
    fn source_render_intrinsic_is_private() {
        let err = link_main("fn main(console: Console):\n    console.print(__render(1))\n")
            .expect_err("source-spellable render intrinsic is compiler-private");
        assert!(
            err.contains("`__render` is compiler-private")
                && err.contains("string interpolation")
                && err.contains("show.render"),
            "{err}"
        );

        link_main("fn main(console: Console):\n    console.print(\"${1}\")\n")
            .expect("interpolation still emits the generated render intrinsic");
    }

    #[test]
    fn module_private_functions_are_not_cross_module_api() {
        let lib = "fn hidden(n: Int) -> Int:\n    n + 1\n\n\
                   pub fn shown(n: Int) -> Int:\n    hidden(n)\n";

        let ok = "import sealed_lib\n\n\
                  fn main(console: Console):\n    console.print(\"${sealed_lib.shown(1)}\")\n";
        link_lib_user(lib, ok).expect("public function may call its private helper");

        let hidden_call = "import sealed_lib\n\n\
                           fn main(console: Console):\n    console.print(\"${sealed_lib.hidden(1)}\")\n";
        let err = link_lib_user(lib, hidden_call).expect_err("private function must not be module-callable");
        assert!(err.contains("function `sealed_lib.hidden` is private"), "{err}");

        let hidden_ref = "import sealed_lib\n\n\
                          fn main(console: Console):\n    let f = sealed_lib.hidden\n    console.print(\"x\")\n";
        let err = link_lib_user(lib, hidden_ref).expect_err("private function must not be a module value");
        assert!(err.contains("function `sealed_lib.hidden` is private"), "{err}");
    }

    #[test]
    fn plain_import_does_not_bind_bare_functions() {
        // RFC-0042 / BUG-452: `import X` keeps functions qualified. A bare
        // imported call exists only when the source wrote `from X import f`.
        let lib = "pub fn shown(n: Int) -> Int:\n    n + 1\n";

        let plain = "import sealed_lib\n\n\
                     fn main(console: Console):\n    console.print(\"${shown(1)}\")\n";
        let err = link_lib_user(lib, plain).expect_err("plain import must not bind `shown` bare");
        assert!(
            err.contains("`shown(...)` is not in scope as a bare function")
                && err.contains("sealed_lib.shown(...)")
                && err.contains("from sealed_lib import shown"),
            "{err}"
        );

        let from_import = "from sealed_lib import shown\n\n\
                           fn main(console: Console):\n    console.print(\"${shown(1)}\")\n";
        link_lib_user(lib, from_import).expect("from-imported public function is callable bare");

        let qualified = "import sealed_lib\n\n\
                         fn main(console: Console):\n    console.print(\"${sealed_lib.shown(1)}\")\n";
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
                     let forged = sealed_lib.Vault(net)\n    console.print(\"x\")\n";
        let err = link_lib_user(lib, forge).expect_err("qualified construct must be rejected");
        assert!(err.contains("sealed capability") && err.contains("Vault"), "{err}");

        // DESTRUCTURE via `match v: lib.Vault(...)` — rejected.
        let destr = "import sealed_lib\n\n\
                     fn main(console: Console, net: Net):\n    \
                     let v = sealed_lib.make(net)\n    \
                     match v:\n        sealed_lib.Vault(inner) -> console.print(\"leak\")\n";
        let err = link_lib_user(lib, destr).expect_err("qualified destructure must be rejected");
        assert!(err.contains("sealed capability") && err.contains("destructure"), "{err}");

        // Legit: hold and pass a Vault through the lib's exported functions — allowed.
        let holder = "from sealed_lib import Vault\nimport sealed_lib\n\n\
                      fn use_it(v: Vault, console: Console):\n    console.print(sealed_lib.zone(v))\n\n\
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
                     let forged = sealed_lib.UiRoot(\"admin\")\n    console.print(\"x\")\n";
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
                     let b = sealed_lib.BoxData(1)\n    console.print(\"x\")\n";
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
                     match b:\n        sealed_lib.BoxData(inner) -> console.print(\"${inner}\")\n";
        assert!(link_lib_user(lib, destr).is_ok(), "sealed-type match must be allowed");

        // Legit: build via the smart constructor, hold, pass, and read the value out.
        let ok = "import sealed_lib\n\n\
                  fn main(console: Console):\n    \
                  let b = sealed_lib.wrap(41)\n    \
                  console.print(\"${sealed_lib.unwrap(b)}\")\n";
        assert!(link_lib_user(lib, ok).is_ok(), "smart-constructor use must be allowed");
    }

    #[test]
    fn test_mode_allows_entry_to_construct_foreign_sealed_type() {
        let lib = "sealed type Version:\n    Version(Int, Int, Int)\n\n\
                   pub fn major(v: Version) -> Int:\n    \
                   match v:\n        Version(n, _, _) -> n\n";
        let user = "import sealed_lib\n\n\
                    fn main(console: Console):\n    \
                    let v = sealed_lib.Version(99, 0, 0)\n    \
                    console.print(\"${sealed_lib.major(v)}\")\n";

        let err = link_lib_user(lib, user).expect_err("production link must keep sealed types strict");
        assert!(err.contains("sealed type") && err.contains("Version"), "{err}");

        link_lib_user_with_mode(lib, user, LinkMode::Test)
            .expect("entry test module may construct foreign sealed data");
    }

    #[test]
    fn test_mode_keeps_sealed_capabilities_strict() {
        let lib = "capability Vault from Net\n\n\
                   pub fn make(net: Net) -> Vault:\n    Vault(net)\n";

        let forge = "import sealed_lib\n\n\
                     fn main(console: Console, net: Net):\n    \
                     let forged = sealed_lib.Vault(net)\n    console.print(\"x\")\n";
        let err = link_lib_user_with_mode(lib, forge, LinkMode::Test)
            .expect_err("test mode must not forge sealed capabilities");
        assert!(err.contains("sealed capability") && err.contains("construct"), "{err}");

        let destr = "import sealed_lib\n\n\
                     fn main(console: Console, net: Net):\n    \
                     let v = sealed_lib.make(net)\n    \
                     match v:\n        sealed_lib.Vault(inner) -> console.print(\"leak\")\n";
        let err = link_lib_user_with_mode(lib, destr, LinkMode::Test)
            .expect_err("test mode must not unwrap sealed capabilities");
        assert!(err.contains("sealed capability") && err.contains("destructure"), "{err}");
    }

    #[test]
    fn test_mode_does_not_relax_imported_dependency_modules() {
        let lib = crate::parser::parse_module(
            "sealed type Box:\n    BoxData(Int)\n",
        )
        .expect("lib parses");
        let helper = crate::parser::parse_module(
            "import sealed_lib\n\n\
             pub fn fake() -> sealed_lib.Box:\n    sealed_lib.BoxData(1)\n",
        )
        .expect("helper parses");
        let user = crate::parser::parse_module(
            "import helper\n\n\
             fn main(console: Console):\n    let b = helper.fake()\n    console.print(\"x\")\n",
        )
        .expect("user parses");

        let err = link_with_mode(
            vec![
                ("sealed_lib".to_string(), lib),
                ("helper".to_string(), helper),
                ("user".to_string(), user),
            ],
            "user",
            noop_expand,
            LinkMode::Test,
        )
        .expect_err("only the entry test module receives sealed data construction privilege")
        .message;
        assert!(err.contains("sealed type") && err.contains("BoxData") && err.contains("helper"), "{err}");
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
                   let list = R(x: 1)\n    console.print(\"${list.get(2)}\")\n";
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
                    let fi = FieldInfo(name: \"x\", type_name: \"Int\")\n    console.print(fi.name)\n";
        let qualified = "import rec_lib\n\n\
                         fn main(console: Console):\n    \
                         let fi = rec_lib.FieldInfo(name: \"x\", type_name: \"Int\")\n    console.print(fi.name)\n";
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
        let src = "fn main(console: Console):\n    let f = iter.count\n    console.print(\"x\")\n";
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
