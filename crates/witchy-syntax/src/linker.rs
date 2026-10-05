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
use std::sync::{Mutex, OnceLock};

use crate::ast::*;
use crate::lambda_scan::collect_pattern_vars;

#[derive(Debug, Clone, PartialEq)]
pub struct LinkError {
    pub message: String,
    /// Parsed source location when the linker can identify the failing statement.
    /// `None` is reserved for module-wide or generated-source errors.
    pub location: Option<LinkLocation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkLocation {
    pub module: String,
    /// One-based source line within `module`.
    pub line: u32,
}

impl fmt::Display for LinkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "link error: {}", self.message)
    }
}

impl std::error::Error for LinkError {}

pub use crate::source_check::SourceCheckError;

/// Failure from a checked link that validates the complete expanded source set
/// before destructive lowering starts.
#[derive(Debug, Clone, PartialEq)]
pub enum SourceLinkError {
    Link(LinkError),
    Source(SourceCheckError),
}

impl From<LinkError> for SourceLinkError {
    fn from(error: LinkError) -> Self {
        Self::Link(error)
    }
}

impl fmt::Display for SourceLinkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Link(error) => fmt::Display::fmt(error, f),
            Self::Source(error) => fmt::Display::fmt(error, f),
        }
    }
}

impl std::error::Error for SourceLinkError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Link(error) => Some(error),
            Self::Source(error) => Some(error),
        }
    }
}

pub type SourceChecker =
    fn(&crate::source_check::ResolvedSource) -> Result<(), SourceLinkError>;

/// Whether the linked artifact is the ordinary runtime CPS projection or the
/// compiler-only typed-source projection retained by RFC-0161.  Both paths run
/// the same source resolution and linking pipeline; the latter stops before
/// destructive async lowering so a type table can still name source operands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AsyncLoweringStage {
    Runtime,
    TypedSource,
}

fn accept_resolved_source(
    _: &crate::source_check::ResolvedSource,
) -> Result<(), SourceLinkError> {
    Ok(())
}

fn lerr<T>(message: impl Into<String>) -> Result<T, LinkError> {
    Err(LinkError {
        message: message.into(),
        location: None,
    })
}

fn source_lerr<T>(message: impl Into<String>) -> Result<T, SourceLinkError> {
    Err(SourceLinkError::Link(LinkError {
        message: message.into(),
        location: None,
    }))
}

fn lerr_at<T>(message: impl Into<String>, module: &str, line: Option<u32>) -> Result<T, LinkError> {
    Err(LinkError {
        message: message.into(),
        location: line.map(|line| LinkLocation { module: module.to_string(), line }),
    })
}

const BUILTINS: &[&str] = &[
    "print",
    "print_int",
    "print_float",
    GENERATED_RENDER_INTRINSIC,
    crate::intrinsics::GENERATED_LIST_PUSH,
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
    "dict_with_capacity",
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
/// whether it is a unit-returning `var`-procedure (which cannot become a value —
/// see the `Expr::Field` arm of `rewrite_expr`). Membership (`contains_key`) is
/// still the "does module X export `f`" question the rest of the linker asks.
type FnTable = HashMap<String, HashMap<String, EtaSig>>;

/// For each module, the bare function names introduced explicitly by
/// `from X import f`, mapped to their exporting module.
type BareFnImports = HashMap<String, HashMap<String, String>>;

/// Compiler-only marker for a name already resolved in syntax's definition
/// context. `@` is not a source identifier character, so user code cannot mint
/// this spelling; the ordinary link rewrite consumes it before type checking.
const DEFINITION_SITE_PREFIX: &str = "@definition_site:";

/// Compiler-only marker for a name that must be resolved in the syntax
/// consumer's context. It is created directly as AST, never parsed from source.
const CALL_SITE_PREFIX: &str = "@call_site:";
const CALL_SITE_TYPE_PREFIX: &str = "@call_site_type:";
const CALL_SITE_CTOR_PREFIX: &str = "@call_site_ctor:";

pub fn call_site_expr(name: &str) -> Expr {
    Expr::Var(format!("{CALL_SITE_PREFIX}{name}"))
}

pub fn call_site_type(name: &str, args: Vec<Type>) -> Type {
    Type::Named(format!("{CALL_SITE_TYPE_PREFIX}{name}"), args)
}

pub fn call_site_pattern(name: &str, args: Vec<Pattern>) -> Pattern {
    Pattern::Ctor { name: format!("{CALL_SITE_CTOR_PREFIX}{name}"), args }
}

pub fn call_site_type_source(name: &str, args: &[Type]) -> String {
    fn project(ty: &mut Type) {
        match ty {
            Type::Qualified(_, inner) => project(inner),
            Type::Slice(elem) => project(elem),
            Type::Named(name, args) => {
                if let Some(target) = call_site_type_target(name) {
                    *name = target.to_string();
                }
                for arg in args {
                    project(arg);
                }
            }
            Type::Tuple(items) => {
                for item in items {
                    project(item);
                }
            }
            Type::RecordCompose { base, fields } => {
                project(base);
                for (_, field) in fields {
                    project(field);
                }
            }
            Type::Fn(params, ret, _, _) => {
                for param in params {
                    project(param);
                }
                project(ret);
            }
            Type::Dyn(_, args) => {
                for arg in args {
                    project(arg);
                }
            }
        }
    }

    let mut ty = Type::Named(name.to_string(), args.to_vec());
    project(&mut ty);
    crate::format::type_str(&ty)
}

pub fn call_site_pattern_source(name: &str, args: &[Pattern]) -> String {
    fn project(pattern: &mut Pattern) {
        match pattern {
            Pattern::Ctor { name, args } => {
                if let Some(target) = call_site_ctor_target(name) {
                    *name = target.to_string();
                }
                for arg in args {
                    project(arg);
                }
            }
            Pattern::AnonCtor { args, .. }
            | Pattern::Tuple(args)
            | Pattern::Or(args) => {
                for arg in args {
                    project(arg);
                }
            }
            Pattern::List { elems, .. } => {
                for elem in elems {
                    project(elem);
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
    }

    let mut pattern = Pattern::Ctor { name: name.to_string(), args: args.to_vec() };
    project(&mut pattern);
    crate::format::pattern_str(&pattern)
}

pub(crate) fn call_site_type_target(name: &str) -> Option<&str> {
    name.strip_prefix(CALL_SITE_TYPE_PREFIX)
}

pub(crate) fn call_site_ctor_target(name: &str) -> Option<&str> {
    name.strip_prefix(CALL_SITE_CTOR_PREFIX)
}

pub(crate) fn call_site_expr_target(name: &str) -> Option<&str> {
    name.strip_prefix(CALL_SITE_PREFIX)
}

/// Name `module.function` with definition-site authority, for a call a desugar
/// emits into a module that could not have written it itself.
///
/// Source cannot spell the `@` prefix, so this is not forgeable: it says "the
/// compiler generated this", which is what the scheduler-bridge privacy rules
/// need to distinguish from a source-written reference.
pub fn definition_site_name(module: &str, function: &str) -> String {
    format!("{DEFINITION_SITE_PREFIX}{module}.{function}")
}

/// The per-function facts eta-expansion consumes (RFC-0050 Part 2).
#[derive(Clone)]
struct EtaSig {
    /// Number of declared parameters — the arity of the eta-expanded lambda.
    /// Includes RFC-0056 defaulted parameters: a function *value* ignores
    /// defaults, so its value form takes every positional argument.
    arity: usize,
    /// Parameter conventions copied onto the forwarding lambda.
    conventions: Vec<Convention>,
    /// Canonicalized parameter types copied onto a nongeneric forwarding
    /// lambda. Storage qualifiers may be nested or hidden in nominal fields;
    /// inference from an erased forwarding call cannot reconstruct them.
    /// Generic method aliases retain their existing inference path.
    parameter_types: Vec<Option<Type>>,
    /// A generic callable value must retain the declaration identity until its
    /// use site selects concrete type arguments.  Eta-expanding it here would
    /// put source type variables (such as `unique a`) in a non-generic lambda,
    /// where `a` is a nominal spelling rather than the declaration's type
    /// parameter.
    generic: bool,
    /// Declaration purity preserved by function-value eta expansion.
    pure: bool,
    /// Part of this module's public API. Same-module calls may use private
    /// helpers; imported or module-qualified cross-module calls may not.
    public: bool,
    /// An API carries a reference that needs an explicit cross-mode decision.
    /// Ordinary value APIs are always callable across the mode boundary.
    reference_bearing: bool,
    /// An API's only first-class references are RFC-0167 immutable rooted
    /// views.  Their authenticated root-plus-projection carrier is preserved
    /// verbatim by linking, so a normal module may call the API without
    /// erasing its type.  Nested or otherwise unadmitted carriers are still
    /// checked by canonical lowering; this bit is only the source-link policy.
    rooted_view_only: bool,
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

fn type_mentions_reference(ty: &Type) -> bool {
    match ty {
        Type::Qualified(TypeQual::View | TypeQual::Borrow(_) | TypeQual::LegacyBorrow(_) | TypeQual::BorrowMut(_), _) => true,
        Type::Qualified(_, inner) => type_mentions_reference(inner),
        Type::Slice(elem) => type_mentions_reference(elem),
        Type::Named(_, args) | Type::Tuple(args) | Type::Dyn(_, args) => {
            args.iter().any(type_mentions_reference)
        }
        Type::RecordCompose { base, fields } => {
            type_mentions_reference(base)
                || fields.iter().any(|(_, field)| type_mentions_reference(field))
        }
        Type::Fn(params, result, _, _) => {
            params.iter().any(type_mentions_reference) || type_mentions_reference(result)
        }
    }
}

fn type_is_reference_bearing(ty: &Type, bearing: &HashSet<String>) -> bool {
    type_mentions_reference(ty) || type_mentions_named_reference(ty, bearing)
}

/// Returns whether a type contains a reference that is not an immutable
/// RFC-0167 rooted view, directly or through a nominal type in `non_rooted`.
/// View shapes, including nominal records whose fields carry only views,
/// continue to canonical ABI admission without erasure; canonical lowering
/// authenticates each field's root and projection.
fn type_mentions_non_rooted_reference(ty: &Type, bearing: &HashSet<String>) -> bool {
    match ty {
        Type::Qualified(TypeQual::View, inner) => type_mentions_non_rooted_reference(inner, bearing),
        Type::Qualified(TypeQual::Borrow(_) | TypeQual::LegacyBorrow(_) | TypeQual::BorrowMut(_), _) => true,
        Type::Qualified(_, inner) => type_mentions_non_rooted_reference(inner, bearing),
        Type::Slice(elem) => type_mentions_non_rooted_reference(elem, bearing),
        Type::Named(name, args) => {
            bearing.contains(name)
                || args.iter().any(|arg| type_mentions_non_rooted_reference(arg, bearing))
        }
        Type::Tuple(args) | Type::Dyn(_, args) => {
            args.iter().any(|arg| type_mentions_non_rooted_reference(arg, bearing))
        }
        Type::RecordCompose { base, fields } => {
            type_mentions_non_rooted_reference(base, bearing)
                || fields.iter().any(|(_, field)| type_mentions_non_rooted_reference(field, bearing))
        }
        Type::Fn(params, result, _, _) => {
            params.iter().any(|param| type_mentions_non_rooted_reference(param, bearing))
                || type_mentions_non_rooted_reference(result, bearing)
        }
    }
}

fn function_has_only_rooted_views(function: &Function, bearing: &HashSet<String>) -> bool {
    function
        .params
        .iter()
        .filter_map(|param| param.ty.as_ref())
        .all(|ty| !type_mentions_non_rooted_reference(ty, bearing))
        && function
            .ret
            .as_ref()
            .is_none_or(|ty| !type_mentions_non_rooted_reference(ty, bearing))
}

/// Compute, transitively, the nominal types whose fields reach any reference.
/// This feeds the `reference_bearing` signature bit; rooted-view admission is
/// decided separately by [`non_rooted_reference_type_names`].
fn reference_bearing_type_names(modules: &[(String, Module)]) -> HashSet<String> {
    let mut bearing = HashSet::new();
    loop {
        let before = bearing.len();
        for (_, module) in modules {
            for item in &module.items {
                let Item::Type(definition) = item else { continue };
                if definition.variants.iter().any(|variant| {
                    variant.fields.iter().any(|field| {
                        type_mentions_reference(field)
                            || type_mentions_named_reference(field, &bearing)
                    })
                }) {
                    bearing.insert(definition.name.clone());
                }
            }
        }
        if bearing.len() == before {
            return bearing;
        }
    }
}

/// Compute the opt-only nominal interface transitively: the types whose fields
/// (or nested nominal fields) reach a reference that is NOT an RFC-0167
/// immutable rooted view — the retired lifetime/`&mut` carriers only the
/// migration grammar can still spell. A normal importer must not hide such a
/// reference by naming the type. Types carrying only `&T` / `&[T]` views are
/// ordinary values in both modes (RFC-0167 §5-§6) and stay nameable.
fn non_rooted_reference_type_names(modules: &[(String, Module)]) -> HashSet<String> {
    let mut non_rooted = HashSet::new();
    loop {
        let before = non_rooted.len();
        for (_, module) in modules {
            for item in &module.items {
                let Item::Type(definition) = item else { continue };
                if definition.variants.iter().any(|variant| {
                    variant
                        .fields
                        .iter()
                        .any(|field| type_mentions_non_rooted_reference(field, &non_rooted))
                }) {
                    non_rooted.insert(definition.name.clone());
                }
            }
        }
        if non_rooted.len() == before {
            return non_rooted;
        }
    }
}

fn type_mentions_named_reference(ty: &Type, bearing: &HashSet<String>) -> bool {
    match ty {
        Type::Qualified(_, inner) => type_mentions_named_reference(inner, bearing),
        Type::Slice(elem) => type_mentions_named_reference(elem, bearing),
        Type::Named(name, args) => {
            bearing.contains(name) || args.iter().any(|arg| type_mentions_named_reference(arg, bearing))
        }
        Type::Tuple(args) | Type::Dyn(_, args) => {
            args.iter().any(|arg| type_mentions_named_reference(arg, bearing))
        }
        Type::RecordCompose { base, fields } => {
            type_mentions_named_reference(base, bearing)
                || fields.iter().any(|(_, field)| type_mentions_named_reference(field, bearing))
        }
        Type::Fn(params, result, _, _) => {
            params.iter().any(|param| type_mentions_named_reference(param, bearing))
                || type_mentions_named_reference(result, bearing)
        }
    }
}

fn named_reference_type<'a>(ty: &'a Type, bearing: &HashSet<String>) -> Option<&'a str> {
    match ty {
        Type::Qualified(_, inner) => named_reference_type(inner, bearing),
        Type::Slice(elem) => named_reference_type(elem, bearing),
        Type::Named(name, args) => bearing
            .contains(name)
            .then_some(name.as_str())
            .or_else(|| args.iter().find_map(|arg| named_reference_type(arg, bearing))),
        Type::Tuple(args) | Type::Dyn(_, args) => args
            .iter()
            .find_map(|arg| named_reference_type(arg, bearing)),
        Type::RecordCompose { base, fields } => named_reference_type(base, bearing).or_else(|| {
            fields
                .iter()
                .find_map(|(_, field)| named_reference_type(field, bearing))
        }),
        Type::Fn(params, result, _, _) => params
            .iter()
            .find_map(|param| named_reference_type(param, bearing))
            .or_else(|| named_reference_type(result, bearing)),
    }
}

fn check_normal_reference_type_interfaces(modules: &[(String, Module)]) -> Result<(), LinkError> {
    let bearing = non_rooted_reference_type_names(modules);
    let bearing_traits = non_rooted_reference_trait_names(modules, &bearing);
    if bearing.is_empty() && bearing_traits.is_empty() {
        return Ok(());
    }
    for (module_name, module) in modules {
        if module.modes.iter().any(|mode| mode == "opt") {
            continue;
        }
        for item in &module.items {
            let mut types = Vec::new();
            match item {
                Item::Function(function) => {
                    types.extend(function.params.iter().filter_map(|param| param.ty.as_ref()));
                    types.extend(function.ret.iter());
                }
                Item::Type(definition) => for variant in definition.variants.iter() {
                    types.extend(variant.fields.iter());
                },
                Item::Trait(trait_def) => for method in &trait_def.methods {
                    types.extend(method.params.iter().filter_map(|param| param.ty.as_ref()));
                    types.extend(method.ret.iter());
                },
                Item::Impl(implementation) => for method in &implementation.methods {
                    types.extend(method.params.iter().filter_map(|param| param.ty.as_ref()));
                    types.extend(method.ret.iter());
                },
                Item::TypeAlias { ty, .. } => types.push(ty),
                Item::Const { .. } | Item::Comptime(_) => {}
            }
            for ty in types {
                if let Some(name) = named_reference_type(ty, &bearing) {
                    return lerr(format!(
                        "normal module `{module_name}` cannot name non-rooted-reference opt type `{name}`; \
                         only immutable rooted views (`&T`, `&[T]`) cross this boundary"
                    ));
                }
                if let Some(name) = named_reference_trait(ty, &bearing_traits) {
                    return lerr(format!(
                        "normal module `{module_name}` cannot name non-rooted-reference opt trait `{name}`; \
                         only immutable rooted views (`&T`, `&[T]`) cross this boundary"
                    ));
                }
            }
        }
    }
    Ok(())
}

fn non_rooted_reference_trait_names(
    modules: &[(String, Module)],
    non_rooted_types: &HashSet<String>,
) -> HashSet<String> {
    modules
        .iter()
        .flat_map(|(_, module)| &module.items)
        .filter_map(|item| match item {
            Item::Trait(trait_def) if trait_def.methods.iter().any(|method| {
                method
                    .params
                    .iter()
                    .filter_map(|param| param.ty.as_ref())
                    .chain(method.ret.iter())
                    .any(|ty| type_mentions_non_rooted_reference(ty, non_rooted_types))
            }) => Some(trait_def.name.clone()),
            _ => None,
        })
        .collect()
}

fn named_reference_trait<'a>(ty: &'a Type, bearing: &HashSet<String>) -> Option<&'a str> {
    match ty {
        Type::Qualified(_, inner) => named_reference_trait(inner, bearing),
        Type::Slice(elem) => named_reference_trait(elem, bearing),
        Type::Dyn(name, args) => bearing
            .contains(name)
            .then_some(name.as_str())
            .or_else(|| args.iter().find_map(|arg| named_reference_trait(arg, bearing))),
        Type::Named(_, args) | Type::Tuple(args) => args
            .iter()
            .find_map(|arg| named_reference_trait(arg, bearing)),
        Type::RecordCompose { base, fields } => named_reference_trait(base, bearing).or_else(|| {
            fields
                .iter()
                .find_map(|(_, field)| named_reference_trait(field, bearing))
        }),
        Type::Fn(params, result, _, _) => params
            .iter()
            .find_map(|param| named_reference_trait(param, bearing))
            .or_else(|| named_reference_trait(result, bearing)),
    }
}

/// The source of a bundled standard-library module, if `name` is one. This is
/// the canonical std registry: the linker treats it as a built-in search path,
/// and the CLI/test harness resolve `import` against it too.
/// Names of all bundled standard-library modules.
pub const STD_MODULES: &[&str] = &[
    "list", "string", "math", "result", "option", "func", "cmp", "ascii", "set", "server",
    "show", "http", "json", "url", "duration", "prng", "regex", "crypto", "compiler", "toml",
    "iter", "semver", "rights", "fs", "dict", "time", "encoding", "path", "testing",
    "future", "task", "chan", "webauthn", "secretstore", "reflect", "dynamic", "meta", "convert", "exec",
    "policy", "jwt", "oauth", "rand", "vm", "bytes", "error", "public_state", "cbor",
    "transaction", "caperror",
];

/// Bundled modules linked into every program and usable without an explicit
/// import. This is the single prelude registry used by linking and tooling.
pub const PRELUDE_MODULES: &[&str] =
    &["list", "string", "dict", "math", "option", "result", "policy", "show", "caperror"];

/// Bundled playground/package modules. These are compiler-shipped for
/// filesystem-free docs and playgrounds, but retain a package owner distinct
/// from the standard library.
pub const PLAYGROUND_MODULES: &[&str] = &["glamour", "markdown", "highlight"];

/// Compiler-only provenance retained on every callable after it is linked into
/// a whole-program module.  Later lowerings can mangle an inherent method into
/// an unqualified symbol such as `List__map`; consumers must use this source
/// owner rather than that implementation spelling when applying per-module
/// contracts such as `mode opt`.
///
/// This is deliberately not a source declaration attribute.  The parser's
/// deny-by-default attribute registry cannot spell it, and the linker stamps it
/// only after resolving the module graph.
pub const LINKED_SOURCE_MODULE_ATTRIBUTE_PREFIX: &str = "__compiler_linked_source_module=";

pub fn linked_source_module_attribute(module: &str) -> String {
    format!("{LINKED_SOURCE_MODULE_ATTRIBUTE_PREFIX}{module}")
}

pub fn linked_source_module(function: &Function) -> Option<&str> {
    // A source graph can be linked again by a later projection.  The current
    // linker stamp is appended last, so it wins over any stale internal
    // metadata rather than trusting an earlier AST allocation.
    function.attributes.iter().rev().find_map(|attribute| {
        attribute.strip_prefix(LINKED_SOURCE_MODULE_ATTRIBUTE_PREFIX)
    })
}

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
    let Some(src) = bundled_source(module) else {
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

/// The general bundled-module resolver: the standard library plus the
/// playground experiments. Import resolution uses this so a program may
/// `import glamour`, while `std_source` stays a pure standard-library registry.
pub fn bundled_source(name: &str) -> Option<&'static str> {
    std_source(name).or_else(|| playground_source(name))
}

/// Source for a bundled STANDARD-LIBRARY module (`std/*.witchy`). Does NOT
/// include playground experiments; use [`bundled_source`] when those are legal.
pub fn std_source(name: &str) -> Option<&'static str> {
    std_table_source(name)
}

/// One name table, two backends. Each entry maps a bundled module to its file,
/// relative to the `std/` directory and without the `.witchy` extension (a
/// `"module.name"` literal would read as a std reference to the pruning-root
/// census). A release (or wasm32) build embeds every
/// file with `include_str!`, so the shipped binary is self-contained. A dev or
/// test build (`debug_assertions`) reads the same files from disk at run time
/// instead: embedding made every `std/*.witchy` edit an input of
/// `witchy-syntax`, and so recompiled every compiler crate in every agent
/// (~10 CPU-minutes) for a change no Rust code can observe. Both backends
/// expand from this one list, so the set of known modules is identical.
macro_rules! bundled_source_table {
    ($lookup:ident, $paths:ident { $($name:literal => $path:literal,)* }) => {
        #[cfg(any(not(debug_assertions), target_arch = "wasm32"))]
        fn $lookup(name: &str) -> Option<&'static str> {
            match name {
                $($name => Some(include_str!(concat!("../../../std/", $path, ".witchy"))),)*
                _ => None,
            }
        }

        #[cfg(all(debug_assertions, not(target_arch = "wasm32")))]
        const $paths: &[&str] = &[$(concat!($path, ".witchy"),)*];

        #[cfg(all(debug_assertions, not(target_arch = "wasm32")))]
        fn $lookup(name: &str) -> Option<&'static str> {
            match name {
                $($name => Some(bundled_sources_on_disk::read(concat!($path, ".witchy"))),)*
                _ => None,
            }
        }
    };
}

bundled_source_table!(std_table_source, STD_SOURCE_PATHS {
    "list" => "list",
    "string" => "string",
    "math" => "math",
    "result" => "result",
    "option" => "option",
    "func" => "func",
    "convert" => "convert",
    "cmp" => "cmp",
    "testing" => "testing",
    "ascii" => "ascii",
    "set" => "set",
    "server" => "server",
    "show" => "show",
    "http" => "http",
    "json" => "json",
    "url" => "url",
    "duration" => "duration",
    "prng" => "prng",
    "rand" => "rand",
    "regex" => "regex",
    "crypto" => "crypto",
    "secretstore" => "secretstore",
    "webauthn" => "webauthn",
    "compiler" => "compiler",
    "toml" => "toml",
    "iter" => "iter",
    "semver" => "semver",
    "rights" => "rights",
    "fs" => "fs",
    "dict" => "dict",
    "time" => "time",
    "encoding" => "encoding",
    "path" => "path",
    "future" => "future",
    "task" => "task",
    "chan" => "chan",
    "reflect" => "reflect",
    "dynamic" => "dynamic",
    "meta" => "meta",
    "exec" => "exec",
    "policy" => "policy",
    "jwt" => "jwt",
    "oauth" => "oauth",
    "vm" => "vm",
    "bytes" => "bytes",
    "error" => "error",
    "cbor" => "cbor",
    "public_state" => "public_state",
    "transaction" => "transaction",
    "caperror" => "caperror",
});

// (RFC-0041) `glamour` is a published RUNE, not std — deliberately ABSENT from
// `STD_MODULES` so it is not advertised as std and a project still declares it as a
// dependency (the PM/footprint treat it as external). But its source is bundled here so
// its PURE render surface (`element`/`text`/`to_html`) is importable in a STANDALONE
// SNIPPET — the browser playground and the book's runnable cells — where there is no
// filesystem to resolve the rune. Authority-neutral: importing it only exposes types;
// a `UiRoot`/`UiFetch` can still be minted only by the host, never obtained in a cell.
// `markdown` rides on glamour's bundling rationale above: a glamour-provided PURE
// module (it imports only `glamour`), bundled so `import markdown` resolves in a
// standalone snippet and in the glamour examples / `projects/docs`.
bundled_source_table!(playground_source, PLAYGROUND_SOURCE_PATHS {
    "glamour" => "../projects/glamour/src/glamour",
    "markdown" => "../projects/glamour/src/markdown",
    "highlight" => "../projects/glamour/src/highlight",
});

/// The `std/` directory this dev/test build reads bundled sources from, or
/// `None` when the build embeds them. A process that re-executes `witchy` with
/// a cleared environment forwards it as `WITCHY_STD_DIR`, so the child reads
/// the same files even when its executable lives outside the checkout.
pub fn bundled_source_dir() -> Option<&'static std::path::Path> {
    #[cfg(any(not(debug_assertions), target_arch = "wasm32"))]
    return None;
    #[cfg(all(debug_assertions, not(target_arch = "wasm32")))]
    return Some(bundled_sources_on_disk::std_dir());
}

/// Dev/test backend of the bundled-source tables.
///
/// The directory is resolved at RUN time, never baked in at compile time:
/// compiled `witchy-syntax` units are shared across worktrees (the mbx cache
/// remaps the workspace root and restores them through its action cache), so a
/// compile-time path would make one worktree read another's std — silently
/// stale code. Each file is read once per process and
/// leaked, keeping `&'static str` sources and the process-constant premise of
/// the pulled-std expansion cache.
#[cfg(all(debug_assertions, not(target_arch = "wasm32")))]
mod bundled_sources_on_disk {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::{Mutex, OnceLock};

    /// Modules whose files identify a witchy `std/` directory.
    const MARKERS: &[&str] = &["list", "option", "string"];

    fn is_std_dir(dir: &Path) -> bool {
        MARKERS.iter().all(|marker| dir.join(marker).with_extension("witchy").is_file())
    }

    fn resolve() -> PathBuf {
        if let Some(dir) = std::env::var_os("WITCHY_STD_DIR").filter(|dir| !dir.is_empty()) {
            let dir = PathBuf::from(dir);
            assert!(
                is_std_dir(&dir),
                "WITCHY_STD_DIR={} is not a witchy std/ directory (it lacks one of the {} modules)",
                dir.display(),
                MARKERS.join(", "),
            );
            return dir;
        }
        // The executable (`<checkout>/target*/debug/witchy`, or a test binary
        // under `target*/debug/deps`) names the checkout it was built from, so
        // it wins over an inherited CARGO_MANIFEST_DIR that may belong to
        // another checkout. CARGO_MANIFEST_DIR (set by `cargo run` and for test
        // processes, inherited by the `witchy` binaries a test spawns) covers a
        // target dir outside the checkout. The current directory is never
        // consulted: a shell sitting in another checkout would silently pair
        // this compiler with that checkout's std.
        let exe = std::env::current_exe().ok();
        let canonical_exe = exe.as_deref().and_then(|exe| exe.canonicalize().ok());
        let manifest = std::env::var_os("CARGO_MANIFEST_DIR").map(PathBuf::from);
        [exe, canonical_exe, manifest]
            .into_iter()
            .flatten()
            .find_map(|start| {
                start.ancestors().map(|dir| dir.join("std")).find(|dir| is_std_dir(dir))
            })
            .unwrap_or_else(|| {
                panic!(
                    "this debug build of witchy reads the standard library from disk, but no \
                     std/ directory was found above the executable or CARGO_MANIFEST_DIR; set \
                     WITCHY_STD_DIR to the std/ directory of the checkout it was built from, \
                     or use a release build"
                )
            })
    }

    pub(super) fn std_dir() -> &'static Path {
        static DIR: OnceLock<PathBuf> = OnceLock::new();
        DIR.get_or_init(resolve)
    }

    /// The contents of `relative` (a bundled-source table path), read once.
    pub(super) fn read(relative: &'static str) -> &'static str {
        static SOURCES: OnceLock<Mutex<HashMap<&'static str, &'static str>>> = OnceLock::new();
        let mut sources = SOURCES
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *sources.entry(relative).or_insert_with(|| {
            let path = std_dir().join(relative);
            let text = std::fs::read_to_string(&path).unwrap_or_else(|error| {
                panic!(
                    "reading bundled source {}: {error} (WITCHY_STD_DIR overrides the std/ directory)",
                    path.display()
                )
            });
            Box::leak(text.into_boxed_str())
        })
    }

    /// Digest of every bundled source. The persistent std-expansion cache keys
    /// its entries by executable identity, which no longer changes when a std
    /// file does, so this build adds the sources themselves to that key.
    pub(super) fn digest() -> &'static [u8; 32] {
        static DIGEST: OnceLock<[u8; 32]> = OnceLock::new();
        DIGEST.get_or_init(|| {
            let mut hasher = blake3::Hasher::new();
            hasher.update(b"witchy-syntax/bundled-sources/v1\0");
            for path in super::STD_SOURCE_PATHS.iter().chain(super::PLAYGROUND_SOURCE_PATHS) {
                let text = read(path);
                hasher.update(path.as_bytes());
                hasher.update(&[0]);
                hasher.update(&(text.len() as u64).to_le_bytes());
                hasher.update(text.as_bytes());
            }
            *hasher.finalize().as_bytes()
        })
    }
}

/// The per-module compile-time expansion pass the linker invokes (`comptime:`
/// blocks + `tag"…"` literals). Injected as a callback so the linker stays
/// agnostic of how compile-time code is evaluated (RFC-0018): it never names
/// `comptime`/`tagged`. `crate::comptime::expand_compile_time` is the production
/// implementation; `crate::pipeline::link` wires it in.
pub use crate::expansion::ComptimeExpander;

/// The ordinary linked runtime AST plus compiler-only generated-node metadata.
/// Backends keep using [`link`] and therefore consume exactly the same `Module`;
/// tooling and diagnostic frontends opt into [`link_with_origins`].
#[derive(Debug, Clone, PartialEq)]
pub struct LinkedModule {
    pub module: Module,
    pub origins: crate::origin::OriginTable,
    /// Source item positions to final linked item positions, retained for
    /// compiler-owned post-link provenance such as admitted comptime output.
    pub source_item_maps: HashMap<String, Vec<Vec<usize>>>,
    pub(crate) async_surface: crate::async_surface::LinkedAsyncSurface,
    /// Complete linker module-key set, including compiler-pulled modules that
    /// contribute functions but no nominal declarations. Authenticated loaders
    /// validate ownership against this set rather than inferring it from names.
    pub module_names: Vec<String>,
    /// Resolved type/trait declaration provenance retained for RFC-0082 runtime
    /// descriptor identity. Compiler names remain lookup keys only; package
    /// ownership is joined by the loader-facing pipeline.
    pub declarations: crate::type_resolve::ResolvedDeclarations,
}

impl LinkedModule {
    /// Borrow the lossless pre-CPS async metadata tied to this linked module.
    ///
    /// This is compiler-only transport, not an execution plan. A typed runtime
    /// must derive its own shapes, lanes, ownership transitions, and backend
    /// bindings from the checked program before it can execute anything.
    #[doc(hidden)]
    pub fn async_surface(&self) -> &crate::async_surface::LinkedAsyncSurface {
        &self.async_surface
    }
}

/// Process-lifetime cache of bundled std modules pulled into a link set, in
/// their fully prepared source form: parsed, derive-scheduled, and compile-time
/// expanded, but not destructively lowered. The expansion of a `derive`-carrying std module (json/meta/
/// reflect/semver) runs comptime programs that recursively link — ~200ms per
/// link that imports `semver` — and recomputes byte-identical results every
/// time, because the inputs are process-constants: the bundled sources are
/// `&'static str`s compiled in or read once per process, comptime programs are zero-capability and
/// deterministic, and a std module's imports resolve std-only (user modules
/// cannot claim reserved names). The single sibling-reading pass — tagged-
/// literal expansion — is excluded by construction: a std source that even
/// mentions `tag"` is never cached (see `pulled_std_cache_insert`). Keyed by
/// the expander and context-request identities so a non-standard `ComptimeExpander` gets its own
/// entries, never another expander's output. Native cargo test executables add
/// a second, process-shared tier below this map; both tiers store the exact
/// expanded source `Module` and its compiler-only `OriginTable` in an executable-
/// identity-keyed, integrity-checked envelope. Every cache hit therefore re-enters
/// linked-source proof construction before destructive lowering.
#[cfg_attr(not(target_arch = "wasm32"), derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone)]
struct CachedStdExpansion {
    module: Module,
    origins: crate::origin::OriginTable,
}

static PULLED_STD_EXPANSION_CACHE: std::sync::OnceLock<
    std::sync::Mutex<HashMap<((usize, usize), String), CachedStdExpansion>>,
> = std::sync::OnceLock::new();

fn pulled_std_cache_get(expand: ComptimeExpander, name: &str) -> Option<CachedStdExpansion> {
    if let Some(expansion) = PULLED_STD_EXPANSION_CACHE
        .get()
        .and_then(|cache| cache.lock().ok())
        .and_then(|map| map.get(&(expand.cache_identity(), name.to_string())).cloned())
    {
        return Some(expansion);
    }

    let expansion = persistent_std_cache_get(expand, name)?;
    let cache =
        PULLED_STD_EXPANSION_CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    if let Ok(mut map) = cache.lock() {
        map.insert((expand.cache_identity(), name.to_string()), expansion.clone());
    }
    Some(expansion)
}

fn pulled_std_cache_insert(
    expand: ComptimeExpander,
    name: &str,
    module: &Module,
    origins: &crate::origin::OriginTable,
) {
    // Tagged-literal expansion reads sibling modules, whose expansion state at
    // that point depends on the link set's pull order — so a std source that
    // even mentions `tag"` (in code, an emitted string, or a comment) is
    // conservatively left uncached. No bundled module does today.
    if bundled_source(name).is_none_or(|s| s.contains("tag\"")) {
        return;
    }
    let cache =
        PULLED_STD_EXPANSION_CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    let expansion = CachedStdExpansion { module: module.clone(), origins: origins.clone() };
    if let Ok(mut map) = cache.lock() {
        map.insert((expand.cache_identity(), name.to_string()), expansion.clone());
    }
    persistent_std_cache_insert(expand, name, &expansion);
}

#[cfg(not(target_arch = "wasm32"))]
// Version 4 invalidates AST enum ordinals from before the retired qualifier removal.
const PERSISTENT_STD_CACHE_MAGIC: &[u8] = b"witchy-std-expansion-cache\x04";

#[cfg(not(target_arch = "wasm32"))]
const PERSISTENT_STD_CACHE_MAX_BYTES: usize = 8 * 1024 * 1024;

#[cfg(not(target_arch = "wasm32"))]
const PERSISTENT_STD_CACHE_MAX_GENERATIONS: usize = 64;

#[cfg(not(target_arch = "wasm32"))]
static PERSISTENT_STD_CACHE_WRITE_ID: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

#[cfg(not(target_arch = "wasm32"))]
static PRUNED_PERSISTENT_STD_CACHE_ROOTS: OnceLock<Mutex<HashSet<std::path::PathBuf>>> =
    OnceLock::new();

/// A stable address inside the current executable. Subtracting it from the
/// expander address cancels ASLR while keeping alternate test expanders in
/// separate namespaces.
#[cfg(not(target_arch = "wasm32"))]
#[inline(never)]
fn persistent_std_cache_anchor() {}

#[cfg(not(target_arch = "wasm32"))]
fn persistent_std_cache_path(expand: ComptimeExpander, name: &str) -> Option<std::path::PathBuf> {
    if !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
        return None;
    }

    // Two on-disk homes, chosen by an explicit test override first and the
    // running binary's layout second. Older Cargo/nextest test executables
    // (parent dir `deps`) share a cache under the target dir. Every other binary
    // — the installed CLI — shares a cache under the user cache dir, beside the
    // runtime's other witchy caches (`witchy/wasm`, `witchy/optimized-wasm`,
    // ...). The executable-identity key below invalidates either on a new
    // binary, so enabling the CLI home can never serve a stale prelude. Each
    // process also prunes old executable identities once per cache root.
    let executable = std::env::current_exe().ok()?;
    let parent = executable.parent()?;
    // Cargo 1.100's per-unit target layout no longer guarantees that a test
    // executable lives directly under a directory named `deps`. An explicit
    // test-cache root is authoritative regardless of Cargo's output layout.
    let explicit_test_root = std::env::var_os("WITCHY_TEST_STDLIB_CACHE_DIR")
        .filter(|path| !path.is_empty())
        .map(std::path::PathBuf::from);
    let root = if let Some(root) = explicit_test_root {
        root
    } else if parent.file_name()?.to_str()? == "deps" {
        let profile = parent.parent()?;
        let target = profile.parent()?;
        target.join("witchy-testcache")
    } else {
        let base = std::env::var_os("XDG_CACHE_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".cache"))
            })?;
        base.join("witchy").join("stdlib")
    };

    let metadata = std::fs::metadata(&executable).ok()?;
    let modified = metadata
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos();
    let anchor = persistent_std_cache_anchor as *const () as usize;
    let (callback, request) = expand.cache_identity();
    let expander_offset = callback.wrapping_sub(anchor);
    let request_offset = request.wrapping_sub(anchor);
    let mut identity = blake3::Hasher::new();
    identity.update(executable.file_name()?.to_string_lossy().as_bytes());
    identity.update(&metadata.len().to_le_bytes());
    identity.update(&modified.to_le_bytes());
    identity.update(&expander_offset.to_le_bytes());
    identity.update(&request_offset.to_le_bytes());
    #[cfg(debug_assertions)]
    identity.update(bundled_sources_on_disk::digest());
    let identity = identity.finalize().to_hex();
    let version_root = root.join("v1");
    persistent_std_cache_prune_once(&version_root, identity.as_str());

    Some(version_root.join(identity.as_str()).join(format!("{name}.wstd")))
}

#[cfg(not(target_arch = "wasm32"))]
fn persistent_std_cache_prune_once(version_root: &std::path::Path, active_generation: &str) {
    let roots = PRUNED_PERSISTENT_STD_CACHE_ROOTS.get_or_init(|| Mutex::new(HashSet::new()));
    let should_prune = roots.lock().is_ok_and(|mut roots| roots.insert(version_root.to_path_buf()));
    if should_prune {
        persistent_std_cache_prune(version_root, active_generation);
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn persistent_std_cache_prune(version_root: &std::path::Path, active_generation: &str) {
    let Ok(entries) = std::fs::read_dir(version_root) else {
        return;
    };
    let mut generations = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|entry| {
            let name = entry.file_name();
            let name = name.to_str()?;
            let is_identity = name.len() == 64
                && name.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
            (is_identity && name != active_generation).then(|| {
                let modified = entry
                    .metadata()
                    .and_then(|metadata| metadata.modified())
                    .unwrap_or(std::time::UNIX_EPOCH);
                (modified, entry.path())
            })
        })
        .collect::<Vec<_>>();
    generations.sort_unstable_by(|left, right| right.cmp(left));

    // Reserve one slot for the current executable even if its cache directory
    // has not been created yet. Cache deletion is best-effort and never affects
    // compiler correctness.
    for (_, path) in generations
        .into_iter()
        .skip(PERSISTENT_STD_CACHE_MAX_GENERATIONS.saturating_sub(1))
    {
        let _ = std::fs::remove_dir_all(path);
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn persistent_std_cache_decode(bytes: &[u8]) -> Option<CachedStdExpansion> {
    let header = PERSISTENT_STD_CACHE_MAGIC.len().checked_add(32)?;
    if bytes.len() < header
        || bytes.len() > header + PERSISTENT_STD_CACHE_MAX_BYTES
        || !bytes.starts_with(PERSISTENT_STD_CACHE_MAGIC)
    {
        return None;
    }
    let (stored_hash, payload) = bytes[PERSISTENT_STD_CACHE_MAGIC.len()..].split_at(32);
    if stored_hash != blake3::hash(payload).as_bytes() {
        return None;
    }
    postcard::from_bytes(payload).ok()
}

#[cfg(not(target_arch = "wasm32"))]
fn persistent_std_cache_get(expand: ComptimeExpander, name: &str) -> Option<CachedStdExpansion> {
    let path = persistent_std_cache_path(expand, name)?;
    let max_envelope = PERSISTENT_STD_CACHE_MAGIC.len() + 32 + PERSISTENT_STD_CACHE_MAX_BYTES;
    if std::fs::metadata(&path).ok()?.len() > max_envelope as u64 {
        let _ = std::fs::remove_file(path);
        return None;
    }
    let bytes = std::fs::read(&path).ok()?;
    if let Some(module) = persistent_std_cache_decode(&bytes) {
        return Some(module);
    }
    // A malformed entry must never become sticky. Best effort only: failure to
    // remove it still falls back to ordinary expansion in this process.
    let _ = std::fs::remove_file(path);
    None
}

#[cfg(not(target_arch = "wasm32"))]
fn persistent_std_cache_insert(
    expand: ComptimeExpander,
    name: &str,
    expansion: &CachedStdExpansion,
) {
    let Some(path) = persistent_std_cache_path(expand, name) else {
        return;
    };
    let Ok(payload) = postcard::to_stdvec(expansion) else {
        return;
    };
    if payload.len() > PERSISTENT_STD_CACHE_MAX_BYTES {
        return;
    }

    let mut envelope = Vec::with_capacity(PERSISTENT_STD_CACHE_MAGIC.len() + 32 + payload.len());
    envelope.extend_from_slice(PERSISTENT_STD_CACHE_MAGIC);
    envelope.extend_from_slice(blake3::hash(&payload).as_bytes());
    envelope.extend_from_slice(&payload);

    let Some(parent) = path.parent() else {
        return;
    };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    let write_id = PERSISTENT_STD_CACHE_WRITE_ID
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let temp = path.with_extension(format!("wstd.{}.{}.tmp", std::process::id(), write_id));
    if std::fs::write(&temp, envelope).is_ok() {
        let _ = std::fs::rename(&temp, &path);
    }
    let _ = std::fs::remove_file(temp);
}

#[cfg(target_arch = "wasm32")]
fn persistent_std_cache_get(_: ComptimeExpander, _: &str) -> Option<CachedStdExpansion> {
    None
}

#[cfg(target_arch = "wasm32")]
fn persistent_std_cache_insert(_: ComptimeExpander, _: &str, _: &CachedStdExpansion) {}

/// Link-time policy for entry-specific privileges.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LinkMode {
    /// Production/default linking: sealed constructors are available only in the
    /// module that declares them.
    Production,
    /// `witchy test` linking: handwritten items in the entry test module may
    /// construct foreign `sealed type` values so tests can exercise malformed
    /// domain data. Generated items and items containing generated syntax stay
    /// production-strict, as do sealed capabilities.
    Test,
    /// Compiler-owned static evaluation only. The source recipe has already
    /// authenticated Glamour and inserted one of its two origin-bearing
    /// constructors before this linker runs; ordinary source cannot select
    /// this mode.
    StaticEvaluation,
}

thread_local! {
    static AST_FOLD_SUPPRESSION_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

struct AstFoldSuppression;

impl Drop for AstFoldSuppression {
    fn drop(&mut self) {
        AST_FOLD_SUPPRESSION_DEPTH.with(|depth| {
            depth.set(depth.get().checked_sub(1).expect("AST fold suppression depth"));
        });
    }
}

fn without_ast_fold<R>(operation: impl FnOnce() -> R) -> R {
    AST_FOLD_SUPPRESSION_DEPTH.with(|depth| {
        depth.set(depth.get().checked_add(1).expect("AST fold suppression depth"));
    });
    let _suppression = AstFoldSuppression;
    operation()
}

fn ast_fold_is_suppressed() -> bool {
    AST_FOLD_SUPPRESSION_DEPTH.with(|depth| depth.get() != 0)
}

/// Whether calling `function` yields a borrowed view. An `async fn` never
/// does: its call produces the `Task`, which carries a direct view result by
/// value.
fn returns_borrowed_view(function: &crate::ast::Function) -> bool {
    !function.is_async && function.ret.as_ref().is_some_and(|ty| {
        matches!(ty, Type::Qualified(TypeQual::View | TypeQual::Borrow(_) | TypeQual::LegacyBorrow(_) | TypeQual::BorrowMut(_), _))
    })
}

fn returns_view_callable(ty: Option<&Type>) -> bool {
    ty.is_some_and(|ty| {
        matches!(
            ty.unqualified(),
            Type::Fn(_, result, _, _)
                if matches!(
                    result.as_ref(),
                    Type::Qualified(TypeQual::View | TypeQual::Borrow(_) | TypeQual::LegacyBorrow(_) | TypeQual::BorrowMut(_), _)
                )
        )
    })
}

fn published_inherent_aliases(module: &Module) -> HashMap<&str, &crate::ast::Function> {
    let top_level: HashSet<&str> = module
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Function(function) => Some(function.name.as_str()),
            _ => None,
        })
        .collect();
    let mut aliases = HashMap::new();
    for item in &module.items {
        let Item::Impl(definition) = item else { continue };
        if definition.trait_name.is_some() {
            continue;
        }
        for method in &definition.methods {
            if method.public
                && method.params.first().is_some_and(|param| param.name == "self")
                && !top_level.contains(method.name.as_str())
            {
                aliases.entry(method.name.as_str()).or_insert(method);
            }
        }
    }
    aliases
}

fn expanded_view_fns(
    modules: &[(String, Module)],
    current_name: &str,
    current: &Module,
) -> HashSet<String> {
    let mut known = HashSet::new();
    for (module_name, stored) in modules {
        let module = if module_name == current_name { current } else { stored };
        for item in &module.items {
            match item {
                Item::Function(function) => {
                    if returns_borrowed_view(function) {
                        known.insert(format!("{module_name}.{}", function.name));
                    }
                    if returns_view_callable(function.ret.as_ref()) {
                        known.insert(format!("@callable:{module_name}.{}", function.name));
                    }
                }
                Item::Impl(definition) => {
                    for method in &definition.methods {
                        if returns_borrowed_view(method) {
                            known.insert(format!("@method:{}", method.name));
                        }
                        if returns_view_callable(method.ret.as_ref()) {
                            known.insert(format!("@callable-method:{}", method.name));
                        }
                    }
                }
                _ => {}
            }
        }
        for (name, method) in published_inherent_aliases(module) {
            if returns_borrowed_view(method) {
                known.insert(format!("{module_name}.{name}"));
            }
            if returns_view_callable(method.ret.as_ref()) {
                known.insert(format!("@callable:{module_name}.{name}"));
            }
        }
    }
    for item in &current.items {
        match item {
            Item::Function(function) => {
                if returns_borrowed_view(function) {
                    known.insert(function.name.clone());
                }
                if returns_view_callable(function.ret.as_ref()) {
                    known.insert(format!("@callable:{}", function.name));
                }
            }
            Item::Impl(_) => {}
            _ => {}
        }
    }
    for (name, method) in published_inherent_aliases(current) {
        if returns_borrowed_view(method) {
            known.insert(name.to_string());
        }
        if returns_view_callable(method.ret.as_ref()) {
            known.insert(format!("@callable:{name}"));
        }
    }
    for (source, names) in &current.from_imports {
        for name in names {
            if known.contains(&format!("{source}.{name}")) {
                known.insert(name.clone());
            }
            if known.contains(&format!("@callable:{source}.{name}")) {
                known.insert(format!("@callable:{name}"));
            }
        }
    }
    known
}

fn prepare_source_for_expansion(
    module_name: &str,
    mut module: Module,
) -> Result<Module, LinkError> {
    crate::derive::expand_linked(&mut module)
        .map_err(|message| LinkError { message, location: None })?;
    let checked = crate::source_check::check(module.clone()).map_err(|error| {
        let error = error.with_module(module_name);
        LinkError {
            location: error.link_location(),
            message: error.message,
        }
    })?;
    let checked = crate::generators::lower(checked)
        .map_err(|message| LinkError { message, location: None })?;
    let checked = crate::async_lower::lower_for_linker(checked)
        .map_err(|message| LinkError { message, location: None })?;
    let projected = crate::records::lower_lenient(checked)
        .map_err(|message| LinkError { message, location: None })?
        .into_module();
    module.imports = projected.imports;
    module.import_lines = projected.import_lines;
    module.from_imports = projected.from_imports;
    Ok(module)
}

fn lower_expanded_source(
    checked: crate::source_check::SemanticallyCheckedSource,
    origin_tables: &mut HashMap<String, crate::origin::OriginTable>,
    async_stage: AsyncLoweringStage,
) -> Result<
    (
        Vec<(String, Module)>,
        crate::type_resolve::ResolvedDeclarations,
        crate::async_surface::LinkedAsyncSurface,
    ),
    LinkError,
> {
    let view_fns = checked
        .modules()
        .iter()
        .map(|(name, module)| expanded_view_fns(checked.modules(), name, module))
        .collect::<Vec<_>>();
    let (checked_modules, declarations) = checked.into_checked_modules();
    let mut lowered_modules = Vec::with_capacity(checked_modules.len());
    let mut async_surface = crate::async_surface::LinkedAsyncSurface::default();
    for ((module_name, checked), view_fns) in checked_modules.into_iter().zip(view_fns) {
        // Typed-source projections never consult or populate the runtime cache.
        // Avoid formatting their complete AST into an unused cache key.
        let cache_key = (async_stage == AsyncLoweringStage::Runtime)
            .then(|| lowered_source_cache_key(&module_name, checked.module(), &view_fns));
        if let Some(key) = &cache_key
            && let Some(cached) = lowered_source_cache().lock().unwrap().get(key).cloned()
        {
            origin_tables.insert(module_name.clone(), cached.origin_table);
            lowered_modules.push((module_name, cached.module));
            async_surface.push_module(cached.async_surface);
            continue;
        }
        let generator_mapping = crate::generators::lowered_item_mapping(checked.module());
        let checked = lower_generators_with_canonical_impls(&module_name, checked).map_err(
            |message| LinkError {
                message: format!("module `{module_name}`: expanded source: {message}"),
                location: None,
            },
        )?;
        let module_async_surface = crate::async_surface::AsyncSurfaceModule::from_module(
            &module_name,
            checked.module(),
        );
        let (lowered, final_mapping) = match async_stage {
            AsyncLoweringStage::Runtime => {
                let (checked, async_mapping) =
                    crate::async_lower::lower_with_view_fns_and_item_mapping(checked, &view_fns)
                        .map_err(|message| LinkError {
                        message: format!("module `{module_name}`: expanded source: {message}"),
                        location: None,
                    })?;
                let mut final_mapping = Vec::with_capacity(generator_mapping.len());
                for generator_targets in &generator_mapping {
                    let mut targets = Vec::new();
                    for intermediate in generator_targets {
                        let Some(async_targets) = async_mapping.get(*intermediate) else {
                            return Err(LinkError {
                                message: format!(
                                    "module `{module_name}`: expanded source origins: generator mapping targets missing async input {intermediate}"
                                ),
                                location: None,
                            });
                        };
                        targets.extend(async_targets.iter().copied());
                    }
                    final_mapping.push(targets);
                }
                let lowered = crate::records::lower_lenient(checked)
                    .map(|module| module.into_module())
                    .map_err(|message| LinkError {
                    message: format!("module `{module_name}`: expanded source: {message}"),
                    location: None,
                })?;
                (lowered, final_mapping)
            }
            AsyncLoweringStage::TypedSource => {
                let mut module = checked.into_module();
                mark_async_source_identities(&mut module);
                let lowered = crate::records::lower_for_async_typeck(module).map_err(|message| LinkError {
                    message: format!("module `{module_name}`: expanded source: {message}"),
                    location: None,
                })?;
                (lowered, generator_mapping)
            }
        };
        let origin_table = origin_tables
            .entry(module_name.clone())
            .or_default();
        origin_table
            .rebuild_item_trees(&module_name, &final_mapping, &lowered.items)
            .map_err(|message| LinkError {
                message: format!(
                    "module `{module_name}`: expanded source origins: {message}"
                ),
                location: None,
            })?;
        if let Some(cache_key) = cache_key {
            let mut cache = lowered_source_cache().lock().unwrap();
            if cache.len() >= 64 {
                cache.clear();
            }
            cache.insert(
                cache_key,
                CachedLoweredSource {
                    module: lowered.clone(),
                    origin_table: origin_table.clone(),
                    async_surface: module_async_surface.clone(),
                },
            );
        }
        lowered_modules.push((module_name, lowered));
        async_surface.push_module(module_async_surface);
    }
    Ok((lowered_modules, declarations, async_surface))
}

#[derive(Clone)]
struct CachedLoweredSource {
    module: Module,
    origin_table: crate::origin::OriginTable,
    async_surface: crate::async_surface::AsyncSurfaceModule,
}

fn lowered_source_cache() -> &'static Mutex<std::collections::HashMap<String, CachedLoweredSource>> {
    static CACHE: OnceLock<Mutex<std::collections::HashMap<String, CachedLoweredSource>>> =
        OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

fn lowered_source_cache_key(
    module_name: &str,
    module: &Module,
    view_fns: &HashSet<String>,
) -> String {
    format!("{module_name}\n{module:?}\n{view_fns:?}")
}

/// Preserve a direct source callable identity on the still-async projection.
/// Runtime lowering attaches the same identity to generated frame states; this
/// variant instead attaches it to the original callable so the type table and
/// source-expression coordinates refer to one AST allocation.
fn mark_async_source_identities(module: &mut Module) {
    for (item, source) in module.items.iter_mut().enumerate() {
        let item = usize::try_from(item).expect("source item count exceeds usize");
        match source {
            Item::Function(function) if function.is_async => {
                function.attributes.push(crate::suspension::async_surface_attribute(item, None));
            }
            Item::Impl(definition) => {
                for (method, function) in definition.methods.iter_mut().enumerate() {
                    if function.is_async {
                        function.attributes.push(crate::suspension::async_surface_attribute(item, Some(method)));
                    }
                }
            }
            Item::Function(_) | Item::Type(_) | Item::Trait(_) | Item::Const { .. }
            | Item::TypeAlias { .. } | Item::Comptime(_) => {}
        }
    }
}

fn lower_generators_with_canonical_impls(
    module_name: &str,
    mut checked: crate::source_check::SourceCheckedModule,
) -> Result<crate::source_check::GeneratorsLoweredModule, String> {
    let prefix = format!("{module_name}.");
    let mut canonical_impls = Vec::new();
    for item in &mut checked.module_mut().items {
        let Item::Impl(definition) = item else {
            continue;
        };
        if definition.trait_name.is_some() || !definition.methods.iter().any(|method| method.is_gen)
        {
            continue;
        }
        let Some(local_name) = definition.type_name.strip_prefix(&prefix) else {
            continue;
        };
        let helper_names = definition
            .methods
            .iter()
            .filter(|method| method.is_gen)
            .map(|method| format!("__gen_{local_name}_{}", method.name))
            .collect::<Vec<_>>();
        canonical_impls.push((
            local_name.to_string(),
            definition.type_name.clone(),
            helper_names,
        ));
        definition.type_name = local_name.to_string();
    }

    let mut lowered = crate::generators::lower(checked)?;
    for item in &mut lowered.module_mut().items {
        match item {
            Item::Impl(definition) => {
                if let Some((_, canonical, _)) = canonical_impls
                    .iter()
                    .find(|(local, _, _)| local == &definition.type_name)
                {
                    definition.type_name.clone_from(canonical);
                }
            }
            Item::Function(function) => {
                let Some((local, canonical, _)) = canonical_impls
                    .iter()
                    .find(|(_, _, helpers)| helpers.contains(&function.name))
                else {
                    continue;
                };
                for parameter in &mut function.params {
                    if let Some(ty) = &mut parameter.ty {
                        canonicalize_generator_receiver_type(ty, local, canonical);
                    }
                }
                if let Some(ty) = &mut function.ret {
                    canonicalize_generator_receiver_type(ty, local, canonical);
                }
                for (_, _, arguments) in &mut function.bounds {
                    for argument in arguments {
                        canonicalize_generator_receiver_type(argument, local, canonical);
                    }
                }
                for statement in &mut function.body.stmts {
                    if let Stmt::Let { ty: Some(ty), .. } = statement {
                        canonicalize_generator_receiver_type(ty, local, canonical);
                    }
                }
            }
            _ => {}
        }
    }
    Ok(lowered)
}

fn canonicalize_generator_receiver_type(ty: &mut Type, local: &str, canonical: &str) {
    match ty {
        Type::Named(name, arguments) => {
            if name == local {
                *name = canonical.to_string();
            }
            for argument in arguments {
                canonicalize_generator_receiver_type(argument, local, canonical);
            }
        }
        Type::Dyn(_, arguments) | Type::Tuple(arguments) => {
            for argument in arguments {
                canonicalize_generator_receiver_type(argument, local, canonical);
            }
        }
        Type::RecordCompose { base, fields } => {
            canonicalize_generator_receiver_type(base, local, canonical);
            for (_, field) in fields {
                canonicalize_generator_receiver_type(field, local, canonical);
            }
        }
        Type::Fn(parameters, result, _, _) => {
            for parameter in parameters {
                canonicalize_generator_receiver_type(parameter, local, canonical);
            }
            canonicalize_generator_receiver_type(result, local, canonical);
        }
        Type::Qualified(_, inner) => {
            canonicalize_generator_receiver_type(inner, local, canonical);
        }
        Type::Slice(elem) => {
            canonicalize_generator_receiver_type(elem, local, canonical);
        }
    }
}

/// Resolve parser-ambiguous `module.member(...)` expressions against this
/// module's declared imports.
///
/// The ordinary linker runs this before sealing a linked module. Compiler
/// front ends that synthesize an item-only module from parsed source can run it
/// before discarding the synthetic import declarations and appending the items
/// to an already linked application module.
pub fn reclassify_parsed_module_members(module: &mut Module) {
    let imports: HashSet<String> = module
        .imports
        .iter()
        .cloned()
        .chain(PRELUDE_MODULES.iter().map(|name| (*name).to_string()))
        .collect();
    for item in &mut module.items {
        match item {
            Item::Function(function) => {
                let bound = function
                    .params
                    .iter()
                    .map(|param| param.name.clone())
                    .collect();
                for param in &mut function.params {
                    if let Some(default) = &mut param.default {
                        reclassify_expr_members(default, &imports, &bound);
                    }
                }
                reclassify_block_members(&mut function.body, &imports, &bound);
            }
            Item::Trait(definition) => {
                for method in &mut definition.methods {
                    let bound = method
                        .params
                        .iter()
                        .map(|param| param.name.clone())
                        .collect();
                    for param in &mut method.params {
                        if let Some(default) = &mut param.default {
                            reclassify_expr_members(default, &imports, &bound);
                        }
                    }
                    if let Some(body) = &mut method.default {
                        reclassify_block_members(body, &imports, &bound);
                    }
                }
            }
            Item::Impl(definition) => {
                for method in &mut definition.methods {
                    let bound = method
                        .params
                        .iter()
                        .map(|param| param.name.clone())
                        .collect();
                    for param in &mut method.params {
                        if let Some(default) = &mut param.default {
                            reclassify_expr_members(default, &imports, &bound);
                        }
                    }
                    reclassify_block_members(&mut method.body, &imports, &bound);
                }
            }
            Item::Const { value, .. } => {
                reclassify_expr_members(value, &imports, &HashSet::new());
            }
            Item::Comptime(body) => {
                reclassify_block_members(body, &imports, &HashSet::new());
            }
            Item::Type(_) | Item::TypeAlias { .. } => {}
        }
    }
}

fn reclassify_block_members(
    block: &mut Block,
    imports: &HashSet<String>,
    inherited: &HashSet<String>,
) {
    let mut bound = inherited.clone();
    for statement in &mut block.stmts {
        match statement {
            Stmt::Let { name, value, .. } => {
                reclassify_expr_members(value, imports, &bound);
                bound.insert(name.clone());
            }
            Stmt::LetPattern { pattern, value } => {
                reclassify_expr_members(value, imports, &bound);
                collect_pattern_vars(pattern, &mut bound);
            }
            Stmt::Assign { value, .. } => reclassify_expr_members(value, imports, &bound),
            Stmt::Return(Some(value)) | Stmt::Expr(value) | Stmt::Yield(value) => {
                reclassify_expr_members(value, imports, &bound);
            }
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
    }
}

fn reclassify_expr_members(
    expression: &mut Expr,
    imports: &HashSet<String>,
    bound: &HashSet<String>,
) {
    match expression {
        Expr::MethodCall { receiver, method, args } => {
            if let Expr::Var(module) = receiver.as_ref()
                && imports.contains(module)
                && !bound.contains(module)
            {
                for argument in args.iter_mut() {
                    reclassify_expr_members(argument, imports, bound);
                }
                let args = std::mem::take(args);
                *expression = Expr::Call {
                    name: format!("{module}.{method}"),
                    args,
                };
                return;
            }
            reclassify_expr_members(receiver, imports, bound);
            for argument in args {
                reclassify_expr_members(argument, imports, bound);
            }
        }
        Expr::LabeledMethodCall { receiver, args, .. } => {
            reclassify_expr_members(receiver, imports, bound);
            for (_, argument) in args {
                reclassify_expr_members(argument, imports, bound);
            }
        }
        Expr::Call { args, .. }
        | Expr::Ctor { args, .. }
        | Expr::AnonCtor { args, .. }
        | Expr::List(args)
        | Expr::Tuple(args) => {
            for argument in args {
                reclassify_expr_members(argument, imports, bound);
            }
        }
        Expr::LabeledCall { args, .. } => {
            for (_, argument) in args {
                reclassify_expr_members(argument, imports, bound);
            }
        }
        Expr::Apply { func, args } => {
            reclassify_expr_members(func, imports, bound);
            for argument in args {
                reclassify_expr_members(argument, imports, bound);
            }
        }
        Expr::Unary { expr, .. } | Expr::Try(expr) | Expr::Field { base: expr, .. } => {
            reclassify_expr_members(expr, imports, bound);
        }
        Expr::As { expr, .. }
        | Expr::ExistentialPack { expr, .. }
        | Expr::ExistentialUpcast { expr, .. } => {
            reclassify_expr_members(expr, imports, bound);
        }
        Expr::ExistentialCall { receiver, args, .. } => {
            reclassify_expr_members(receiver, imports, bound);
            for argument in args {
                reclassify_expr_members(argument, imports, bound);
            }
        }
        Expr::RecordUpdate { base, fields, .. } => {
            reclassify_expr_members(base, imports, bound);
            for (_, value) in fields {
                reclassify_expr_members(value, imports, bound);
            }
        }
        Expr::Record { fields, spread, .. } => {
            for (_, value) in fields {
                reclassify_expr_members(value, imports, bound);
            }
            if let Some(spread) = spread {
                reclassify_expr_members(spread, imports, bound);
            }
        }
        Expr::Binary { lhs, rhs, .. } | Expr::Range { lo: lhs, hi: rhs, .. } => {
            reclassify_expr_members(lhs, imports, bound);
            reclassify_expr_members(rhs, imports, bound);
        }
        Expr::Index { base, index } => {
            reclassify_expr_members(base, imports, bound);
            reclassify_expr_members(index, imports, bound);
        }
        Expr::WhileLet { pattern, scrutinee, body } => {
            reclassify_expr_members(scrutinee, imports, bound);
            let mut nested = bound.clone();
            collect_pattern_vars(pattern, &mut nested);
            reclassify_block_members(body, imports, &nested);
        }
        Expr::If { cond, then_block, else_block } => {
            reclassify_expr_members(cond, imports, bound);
            reclassify_block_members(then_block, imports, bound);
            if let Some(else_block) = else_block {
                reclassify_block_members(else_block, imports, bound);
            }
        }
        Expr::Lambda { params, body, .. } => {
            let mut nested = bound.clone();
            nested.extend(params.iter().map(|param| param.name.clone()));
            for param in params {
                if let Some(default) = &mut param.default {
                    reclassify_expr_members(default, imports, &nested);
                }
            }
            reclassify_block_members(body, imports, &nested);
        }
        Expr::Block(block) => reclassify_block_members(block, imports, bound),
        Expr::While { cond, body } => {
            reclassify_expr_members(cond, imports, bound);
            reclassify_block_members(body, imports, bound);
        }
        Expr::For { var, iter, body } => {
            reclassify_expr_members(iter, imports, bound);
            let mut nested = bound.clone();
            nested.insert(var.clone());
            reclassify_block_members(body, imports, &nested);
        }
        Expr::Match { scrutinee, arms } => {
            reclassify_expr_members(scrutinee, imports, bound);
            for arm in arms {
                let mut nested = bound.clone();
                collect_pattern_vars(&arm.pattern, &mut nested);
                if let Some(guard) = &mut arm.guard {
                    reclassify_expr_members(guard, imports, &nested);
                }
                reclassify_expr_members(&mut arm.body, imports, &nested);
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
}

/// Link `modules` (each a name + parsed module) into one flat module, with
/// `entry` the module holding `main`. `expand` runs each module's compile-time
/// passes (see [`ComptimeExpander`]).
pub fn link(
    modules: Vec<(String, Module)>,
    entry: &str,
    expand: ComptimeExpander,
) -> Result<Module, LinkError> {
    link_with_origins(modules, entry, expand).map(|linked| linked.module)
}

pub fn link_with_origins(
    modules: Vec<(String, Module)>,
    entry: &str,
    expand: ComptimeExpander,
) -> Result<LinkedModule, LinkError> {
    link_with_mode_and_origins(modules, entry, expand, LinkMode::Production)
}

pub fn link_with_mode(
    modules: Vec<(String, Module)>,
    entry: &str,
    expand: ComptimeExpander,
    mode: LinkMode,
) -> Result<Module, LinkError> {
    link_with_mode_and_origins(modules, entry, expand, mode).map(|linked| linked.module)
}

pub fn link_with_mode_and_origins(
    modules: Vec<(String, Module)>,
    entry: &str,
    expand: ComptimeExpander,
    mode: LinkMode,
) -> Result<LinkedModule, LinkError> {
    match link_with_mode_and_origins_and_source_check(
        modules,
        entry,
        expand,
        mode,
        accept_resolved_source,
    ) {
        Ok(linked) => Ok(linked),
        Err(SourceLinkError::Link(error)) => Err(error),
        Err(SourceLinkError::Source(error)) => Err(LinkError {
            location: error.link_location(),
            message: error.message,
        }),
    }
}

pub fn link_with_mode_and_origins_and_source_check<F>(
    modules: Vec<(String, Module)>,
    entry: &str,
    expand: ComptimeExpander,
    mode: LinkMode,
    check_source: F,
) -> Result<LinkedModule, SourceLinkError>
where
    F: Fn(&crate::source_check::ResolvedSource) -> Result<(), SourceLinkError>,
{
    link_with_mode_and_origins_and_source_check_fold_policy(
        modules,
        entry,
        expand,
        mode,
        check_source,
        true,
    )
}

/// Link a canonical production source graph without running configured AST
/// optimizations before its checked-source witness is created.
pub fn link_canonical_with_origins_and_source_check<F>(
    modules: Vec<(String, Module)>,
    entry: &str,
    expand: ComptimeExpander,
    mode: LinkMode,
    check_source: F,
) -> Result<LinkedModule, SourceLinkError>
where
    F: Fn(&crate::source_check::ResolvedSource) -> Result<(), SourceLinkError>,
{
    without_ast_fold(|| {
        link_with_mode_and_origins_and_source_check_fold_policy(
            modules,
            entry,
            expand,
            mode,
            check_source,
            false,
        )
    })
}

fn link_with_mode_and_origins_and_source_check_fold_policy<F>(
    modules: Vec<(String, Module)>,
    entry: &str,
    expand: ComptimeExpander,
    mode: LinkMode,
    check_source: F,
    allow_ast_fold: bool,
) -> Result<LinkedModule, SourceLinkError>
where
    F: Fn(&crate::source_check::ResolvedSource) -> Result<(), SourceLinkError>,
{
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
            location: None,
        })?;
        let canonical = crate::parser::parse_module(source).map_err(|e| LinkError {
            message: format!("bundled std module `{name}` failed to parse: {e}"),
            location: None,
        })?;
        if module != &canonical {
            user_modules.insert(name.clone());
        }
    }
    link_with_user_modules_with_mode_and_origins_and_source_check_fold_policy(
        modules,
        entry,
        expand,
        &user_modules,
        mode,
        check_source,
        allow_ast_fold,
    )
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
    link_with_user_modules_with_mode_and_origins(
        modules,
        entry,
        expand,
        user_modules,
        LinkMode::Production,
    ).map(|linked| linked.module)
}

pub fn link_with_user_modules_with_mode(
    modules: Vec<(String, Module)>,
    entry: &str,
    expand: ComptimeExpander,
    user_modules: &std::collections::HashSet<String>,
    mode: LinkMode,
) -> Result<Module, LinkError> {
    link_with_user_modules_with_mode_and_origins(modules, entry, expand, user_modules, mode)
        .map(|linked| linked.module)
}

fn link_with_user_modules_with_mode_and_origins(
    modules: Vec<(String, Module)>,
    entry: &str,
    expand: ComptimeExpander,
    user_modules: &std::collections::HashSet<String>,
    mode: LinkMode,
) -> Result<LinkedModule, LinkError> {
    match link_with_user_modules_with_mode_and_origins_and_source_check(
        modules,
        entry,
        expand,
        user_modules,
        mode,
        accept_resolved_source,
    ) {
        Ok(linked) => Ok(linked),
        Err(SourceLinkError::Link(error)) => Err(error),
        Err(SourceLinkError::Source(error)) => Err(LinkError {
            location: error.link_location(),
            message: error.message,
        }),
    }
}

/// Link while invoking `check_source` after expansion, transitive module
/// discovery, and source name resolution, but before any source node is
/// destructively lowered.
pub fn link_with_user_modules_with_mode_and_origins_and_source_check<F>(
    modules: Vec<(String, Module)>,
    entry: &str,
    expand: ComptimeExpander,
    user_modules: &std::collections::HashSet<String>,
    mode: LinkMode,
    check_source: F,
) -> Result<LinkedModule, SourceLinkError>
where
    F: Fn(&crate::source_check::ResolvedSource) -> Result<(), SourceLinkError>,
{
    link_with_user_modules_with_mode_and_origins_and_source_check_fold_policy(
        modules,
        entry,
        expand,
        user_modules,
        mode,
        check_source,
        true,
    )
}

/// Link a loader-authenticated canonical production source graph without
/// applying configured AST optimizations before SafeWIR lowering.
pub fn link_canonical_with_user_modules_and_origins_and_source_check<F>(
    modules: Vec<(String, Module)>,
    entry: &str,
    expand: ComptimeExpander,
    user_modules: &std::collections::HashSet<String>,
    mode: LinkMode,
    check_source: F,
) -> Result<LinkedModule, SourceLinkError>
where
    F: Fn(&crate::source_check::ResolvedSource) -> Result<(), SourceLinkError>,
{
    without_ast_fold(|| {
        link_with_user_modules_with_mode_and_origins_and_source_check_fold_policy(
            modules,
            entry,
            expand,
            user_modules,
            mode,
            check_source,
            false,
        )
    })
}

fn link_with_user_modules_with_mode_and_origins_and_source_check_fold_policy<F>(
    modules: Vec<(String, Module)>,
    entry: &str,
    expand: ComptimeExpander,
    user_modules: &std::collections::HashSet<String>,
    mode: LinkMode,
    check_source: F,
    allow_ast_fold: bool,
) -> Result<LinkedModule, SourceLinkError>
where
    F: Fn(&crate::source_check::ResolvedSource) -> Result<(), SourceLinkError>,
{
    link_with_user_modules_with_mode_and_origins_and_source_check_stage_fold_policy(
        modules,
        entry,
        expand,
        true,
        user_modules,
        mode,
        check_source,
        AsyncLoweringStage::Runtime,
        allow_ast_fold,
    )
}

pub(crate) fn link_with_user_modules_with_mode_and_origins_and_source_check_stage_fold_policy<F>(
    modules: Vec<(String, Module)>,
    entry: &str,
    expand: ComptimeExpander,
    // Whether `expand` reads its sibling snapshots. Building them costs a
    // whole-module clone per (module, sibling) pair; the source projections
    // re-link already-expanded modules with an expander that ignores them.
    expansion_uses_siblings: bool,
    user_modules: &std::collections::HashSet<String>,
    mode: LinkMode,
    check_source: F,
    async_stage: AsyncLoweringStage,
    allow_ast_fold: bool,
) -> Result<LinkedModule, SourceLinkError>
where
    F: Fn(&crate::source_check::ResolvedSource) -> Result<(), SourceLinkError>,
{
    let mut origin_tables: HashMap<String, crate::origin::OriginTable> = HashMap::new();
    check_reserved_source_names(&modules)?;
    if let Some(name) = user_modules
        .iter()
        .filter(|name| STD_MODULES.contains(&name.as_str()))
        .min()
    {
        return source_lerr(format!(
            "module `{name}` uses a reserved standard-library name — rename the local module; \
             bundled std modules have one canonical owner"
        ));
    }
    // User modules that shadow std module names (always empty after the check
    // above rejects them, but the rewrite context still carries the set for
    // diagnostic precision in `missing_module_function_message`).
    let user_std_shadows: HashSet<String> = user_modules
        .iter()
        .filter(|name| STD_MODULES.contains(&name.as_str()))
        .cloned()
        .collect();

    // Check source-only semantics before expansion, but retain the source AST.
    // A lowered projection contributes implicit imports needed for discovery;
    // the real destructive sequence runs once, after the complete expanded link
    // set exists and borrowed-view facts are final (RFC-0101).
    let mut modules: Vec<(String, Module)> = modules
        .into_iter()
        .map(|(name, mut module)| {
            reclassify_parsed_module_members(&mut module);
            crate::source_check::check(module.clone()).map_err(|error| {
                let error = error.with_module(&name);
                LinkError {
                    location: error.link_location(),
                    message: error.message,
                }
            })?;
            prepare_source_for_expansion(&name, module).map(|module| (name, module))
        })
        .collect::<Result<Vec<_>, LinkError>>()?;

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
        // Expansion strips a module's `comptime fn` declarations and compiler-
        // owned syntax tables from its runtime result. Later modules still need
        // that compile-time API when they invoke an imported tag, so preserve the
        // pre-expansion modules and overlay their compile-time-only pieces onto
        // sibling snapshots. Runtime modules remain stripped after their own pass.
        let expansion_sources = modules.clone();
        let names: Vec<String> = modules.iter().map(|(n, _)| n.clone()).collect();
        for (i, name) in names.iter().enumerate() {
            let needs_siblings = expansion_uses_siblings && expand.needs_siblings(&modules[i].1);
            let siblings: Vec<(String, Module)> = modules
                .iter()
                .enumerate()
                .filter(|(j, _)| needs_siblings && *j != i)
                .map(|(j, (module_name, module))| {
                    (
                        module_name.clone(),
                        expansion_sibling(module, &expansion_sources[j].1),
                    )
                })
                .collect();
            let origins = expand.run(name, &mut modules[i].1, &siblings)
                .map_err(|message| LinkError { message, location: None })?;
            origin_tables.insert(name.clone(), origins);
        }
    }

    // Modules pulled in by the prelude/import passes below post-date initial
    // source preparation and compile-time expansion; index from here so those
    // passes run on just the new ones. Cached indices already carry both and are
    // skipped until every module rejoins linked-source proof construction.
    let pulled_std_start = modules.len();
    let mut cached_pull_indices: HashSet<usize> = HashSet::new();

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
            if let Some(cached) = pulled_std_cache_get(expand, prelude) {
                cached_pull_indices.insert(modules.len());
                origin_tables.insert(prelude.to_string(), cached.origins);
                modules.push((prelude.to_string(), cached.module));
                continue;
            }
            let src = std_source(prelude).ok_or_else(|| LinkError {
                message: format!("prelude module `{prelude}` has no bundled source"),
                location: None,
            })?;
            let m = crate::parser::parse_module(src).map_err(|e| LinkError {
                message: format!("prelude module `{prelude}` failed to parse: {e}"),
                location: None,
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
                if let Some(cached) = pulled_std_cache_get(expand, &imp) {
                    cached_pull_indices.insert(modules.len());
                    origin_tables.insert(imp.clone(), cached.origins);
                    modules.push((imp.clone(), cached.module));
                } else if let Some(src) = bundled_source(&imp) {
                    let m = crate::parser::parse_module(src).map_err(|e| LinkError {
                        message: format!("std module `{imp}`: {e}"),
                        location: None,
                    })?;
                    modules.push((imp.clone(), m));
                }
            }
        }
        i += 1;
    }

    // Prepare newly discovered std source exactly like the initial modules:
    // schedule derives and project implicit lowering imports while retaining
    // every source node for the final whole-link lowering.
    for (k, (name, module)) in modules.iter_mut().enumerate().skip(pulled_std_start) {
        if cached_pull_indices.contains(&k) {
            continue;
        }
        *module = prepare_source_for_expansion(name, module.clone())?;
    }
    let mut pulled_imports_before = HashMap::new();
    for k in pulled_std_start..modules.len() {
        if cached_pull_indices.contains(&k) {
            continue;
        }
        let name = modules[k].0.clone();
        let needs_siblings = expansion_uses_siblings && expand.needs_siblings(&modules[k].1);
        let siblings: Vec<(String, Module)> = modules
            .iter()
            .enumerate()
            .filter(|(j, _)| needs_siblings && *j != k)
            .map(|(_, m)| m.clone())
            .collect();
        let imports_before =
            (modules[k].1.imports.clone(), modules[k].1.from_imports.clone());
        let origins = expand.run(&name, &mut modules[k].1, &siblings)
            .map_err(|message| LinkError { message, location: None })?;
        origin_tables.insert(name, origins);
        pulled_imports_before.insert(k, imports_before);
    }
    // Cache expanded source, never lowered output. A comptime block may EMIT
    // `import` lines after the pull-in loop has run, so cache only expansions
    // whose imports stayed fixed. Warm hits then traverse the same linked-source
    // proof and destructive lowering as cold source.
    for (k, (name, module)) in modules.iter().enumerate().skip(pulled_std_start) {
        if cached_pull_indices.contains(&k) {
            continue;
        }
        let imports_before = pulled_imports_before
            .get(&k)
            .expect("every uncached pulled module recorded its pre-expansion imports");
        if imports_before.0 == module.imports
            && imports_before.1 == module.from_imports
        {
            let origins = origin_tables
                .get(name)
                .expect("every expanded module retains its origin table");
            pulled_std_cache_insert(expand, name, module, origins);
        }
    }

    // The proof is built only after expansion and transitive std discovery are
    // complete. It validates the full namespace and canonicalizes all
    // source-visible identities while generators, async functions, records,
    // traits, impls, aliases, and their method signatures are still intact.
    for (_, module) in &mut modules {
        reclassify_parsed_module_members(module);
    }
    let resolved = crate::source_check::resolve_linked_source(
        modules,
        user_modules,
        entry,
        mode,
        allow_ast_fold,
    )?;
    if !matches!(async_stage, AsyncLoweringStage::Runtime) {
        check_source(&resolved)?;
        return lower_checked_source(
            resolved.after_semantic_check(), origin_tables, async_stage, mode, entry, &user_std_shadows, allow_ast_fold,
        );
    }
    // The source check asks for the runtime projection, and relinking the
    // resolved source reproduces exactly this link's own lowering. Lower a
    // copy once, before the check, and lend it to the check: the speculative
    // result is returned only after the check accepts. A failed lowering lends
    // nothing, so the check relinks and reports what it reported before, and
    // the lowering error reaches the caller only if the check passes.
    let mut resolved = resolved;
    let lowered = lower_checked_source(
        resolved.clone().after_semantic_check(), origin_tables, async_stage, mode, entry, &user_std_shadows, allow_ast_fold,
    );
    let rest = match lowered {
        Ok(LinkedModule { module, origins, source_item_maps, async_surface, module_names, declarations }) => {
            resolved.install_runtime_projection(module);
            Ok((origins, source_item_maps, async_surface, module_names, declarations))
        }
        Err(error) => Err(error),
    };
    check_source(&resolved)?;
    let (origins, source_item_maps, async_surface, module_names, declarations) = rest?;
    let module = resolved.take_runtime_projection()
        .expect("a successful lowering installed its projection");
    Ok(LinkedModule { module, origins, source_item_maps, async_surface, module_names, declarations })
}

/// Destructive lowering of a semantically checked source set: everything the
/// link does after its injected source check accepted the resolved proof.
fn lower_checked_source(
    checked: crate::source_check::SemanticallyCheckedSource,
    mut origin_tables: HashMap<String, crate::origin::OriginTable>,
    async_stage: AsyncLoweringStage,
    mode: LinkMode,
    entry: &str,
    user_std_shadows: &HashSet<String>,
    allow_ast_fold: bool,
) -> Result<LinkedModule, SourceLinkError> {
    let (mut modules, declarations, mut async_surface) =
        lower_expanded_source(checked, &mut origin_tables, async_stage)?;

    // MethodCall nodes survive linking: `x.f(a)` resolves to a REAL method
    // (impl/trait/static) during trait lowering, not to arbitrary free
    // functions (rfcs/language-evolution.md Phase 3).

    // Expand type aliases and inline top-level constants per module before
    // merging. The linked-source proof already canonicalized each declaration,
    // target, use site, and constant expression without erasing either item.
    modules = modules
        .into_iter()
        .map(|(n, m)| {
            if let Some(origins) = origin_tables.get_mut(&n) {
                let retained: Vec<bool> = m.items.iter().map(|item| {
                    !matches!(item, Item::Const { .. } | Item::TypeAlias { .. })
                }).collect();
                origins.retain_items(&n, &retained);
            }
            crate::aliases::resolve(crate::consts::inline(m))
                .map(|module| (n, module))
                .map_err(|message| LinkError { message, location: None })
        })
        .collect::<Result<Vec<_>, _>>()?;

    // Type aliases are expanded above, so this sees the same canonical nominal
    // identities that function signatures and the later checker consume.
    check_normal_reference_type_interfaces(&modules)?;

    // RFC-0002 sealing: a `capability` (a sealed type) may be CONSTRUCTED or
    // DESTRUCTURED only inside the module that declares it. Run AFTER
    // `type_resolve` canonicalizes every constructor and pattern — bare,
    // module-qualified (`lib.Vault(…)`), and from-imported alike — to a single
    // `Expr::Ctor { name: "module.Ctor" }`, so a qualified spelling can no longer
    // slip past the name-keyed check (BUG-313, fail-closed). Modules are still
    // unmerged here, so each item knows its home module.
    check_sealing(&modules, &origin_tables, mode, entry)?;
    restore_compiler_type_info(&mut modules);

    let reference_bearing = reference_bearing_type_names(&modules);
    let non_rooted = non_rooted_reference_type_names(&modules);
    let mut fns: FnTable = HashMap::new();
    for (name, m) in &modules {
        let mut names: HashMap<String, EtaSig> = HashMap::new();
        for item in &m.items {
            if let Item::Function(f) = item {
                names.insert(
                    f.name.clone(),
                    EtaSig {
                        arity: f.params.len(),
                        conventions: f.params.iter().map(|param| param.convention).collect(),
                        parameter_types: eta_parameter_types(f),
                        generic: function_has_type_parameters(f),
                        pure: f.pure,
                        public: f.public,
                        reference_bearing: f
                            .params
                            .iter()
                            .filter_map(|param| param.ty.as_ref())
                            .any(|ty| type_is_reference_bearing(ty, &reference_bearing))
                            || f
                            .ret
                            .as_ref()
                            .is_some_and(|ty| type_is_reference_bearing(ty, &reference_bearing)),
                        rooted_view_only: function_has_only_rooted_views(f, &non_rooted),
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
                            conventions: method
                                .params
                                .iter()
                                .map(|param| param.convention)
                                .collect(),
                            parameter_types: eta_parameter_types(method),
                            generic: function_has_type_parameters(method),
                            pure: method.pure,
                            public: true,
                            reference_bearing: method
                                .params
                                .iter()
                                .filter_map(|param| param.ty.as_ref())
                                .any(|ty| type_is_reference_bearing(ty, &reference_bearing))
                                || method.ret.as_ref().is_some_and(|ty| {
                                    type_is_reference_bearing(ty, &reference_bearing)
                                }),
                            rooted_view_only: function_has_only_rooted_views(method, &non_rooted),
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
        return source_lerr(format!("entry module `{entry}` not found"));
    }
    for (name, m) in &modules {
        for imp in &m.imports {
            if !fns.contains_key(imp) {
                return source_lerr(format!("module `{name}` imports unknown module `{imp}`"));
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
                return source_lerr(format!(
                    "`mode opt` module `{name}` imports `{imp}`, which is not `mode opt` — \
                     optimization mode is transitive, so an `opt` module may depend only on \
                     other `opt` modules (the bundled std library is exempt). Add `mode opt` \
                     to `{imp}`, or drop `mode opt` from `{name}`."
                ));
            }
        }
    }

    let mut items = Vec::new();
    let mut origin_item_maps: HashMap<String, Vec<Vec<usize>>> = HashMap::new();
    let mut seen_anon_types: HashMap<String, TypeDef> = HashMap::new();
    let mut seen_anon_trait_impls: HashSet<(String, Vec<String>, String, Vec<String>)> = HashSet::new();
    // Each module's items move into the linked module; nothing below reads a
    // source module's items again, so none is copied.
    for (mname, m) in &mut modules {
        let source_items = std::mem::take(&mut m.items);
        let m = &*m;
        let mut item_map = vec![Vec::new(); source_items.len()];
        for (source_index, item) in source_items.into_iter().enumerate() {
            match item {
                Item::Function(mut f2) => {
                    f2.name = if mname == entry && f2.name == "main" {
                        "main".to_string()
                    } else {
                        format!("{mname}.{}", f2.name)
                    };
                    f2.attributes
                        .push(linked_source_module_attribute(mname));
                    let mut bound = HashSet::new();
                    for p in &f2.params {
                        bound.insert(p.name.clone());
                    }
                    rewrite_block(
                        &mut f2.body,
                        mname,
                        &m.imports,
                        bare_fn_imports.get(mname),
                        &fns,
                        &bound,
                        user_std_shadows,
                        mode,
                        entry,
                        is_opt(m),
                    )?;
                    let linked_item = u32::try_from(items.len())
                        .expect("linked item count exceeds async-surface location range");
                    if crate::suspension::async_surface(&f2).is_some() {
                        f2.attributes.push(crate::suspension::async_surface_module_attribute(mname));
                    }
                    if let (Some((surface_item, surface_method)), Some(local_state)) = (
                        crate::suspension::async_surface(&f2),
                        crate::suspension::frame_state(&f2),
                    ) {
                        async_surface.push_binding(
                            crate::async_surface::LinkedAsyncCallableBinding {
                                module: mname.clone(),
                                surface: crate::async_surface::AsyncSurfaceId {
                                    item: u32::try_from(surface_item).expect(
                                        "source item count exceeds async-surface identity range",
                                    ),
                                    method: surface_method.map(|method| {
                                        u32::try_from(method).expect(
                                            "method count exceeds async-surface identity range",
                                        )
                                    }),
                                },
                                local_state: u32::try_from(local_state).expect(
                                    "async state count exceeds async-surface state range",
                                ),
                                location: crate::async_surface::LinkedAsyncCallableLocation::Function {
                                    item: linked_item,
                                },
                            },
                        );
                    }
                    item_map[source_index].push(items.len());
                    items.push(Item::Function(f2));
                }
                Item::Type(t) => {
                    if is_generated_anon_name(&t.name) {
                        if let Some(prev) = seen_anon_types.get(&t.name) {
                            if *prev == t {
                                continue;
                            }
                        } else {
                            seen_anon_types.insert(t.name.clone(), t.clone());
                        }
                    }
                    item_map[source_index].push(items.len());
                    items.push(Item::Type(t));
                }
                // Constants and aliases were resolved per-module above, so none
                // remain here.
                Item::Const { .. } | Item::TypeAlias { .. } | Item::Comptime(_) => {}
                // Traits/impls are carried into the merged module and desugared
                // after linking (see `crate::traits`). Their method bodies are
                // rewritten here, in their defining module's context, so calls
                // inside them resolve like any other function body.
                Item::Trait(mut t2) => {
                    for ms in &mut t2.methods {
                        if let Some(body) = &mut ms.default {
                            let mut bound = HashSet::new();
                            for p in &ms.params {
                                bound.insert(p.name.clone());
                            }
                            rewrite_block(
                                body,
                                mname,
                                &m.imports,
                                bare_fn_imports.get(mname),
                                &fns,
                                &bound,
                                user_std_shadows,
                        mode,
                        entry,
                        is_opt(m),
                            )?;
                        }
                    }
                    item_map[source_index].push(items.len());
                    items.push(Item::Trait(t2));
                }
                Item::Impl(mut im2) => {
                    if let Some(key) = generated_anon_trait_impl_key(&im2) {
                        if !seen_anon_trait_impls.insert(key) {
                            continue;
                        }
                    }
                    for method in &mut im2.methods {
                        method
                            .attributes
                            .push(linked_source_module_attribute(mname));
                        let mut bound = HashSet::new();
                        for p in &method.params {
                            bound.insert(p.name.clone());
                        }
                        rewrite_block(
                            &mut method.body,
                            mname,
                            &m.imports,
                            bare_fn_imports.get(mname),
                            &fns,
                            &bound,
                            user_std_shadows,
                        mode,
                        entry,
                        is_opt(m),
                        )?;
                    }
                    let linked_item = u32::try_from(items.len())
                        .expect("linked item count exceeds async-surface location range");
                    for (method_index, method) in im2.methods.iter_mut().enumerate() {
                        if crate::suspension::async_surface(method).is_some() {
                            method.attributes.push(crate::suspension::async_surface_module_attribute(mname));
                        }
                        if let (Some((surface_item, surface_method)), Some(local_state)) = (
                            crate::suspension::async_surface(method),
                            crate::suspension::frame_state(method),
                        ) {
                            let Some(surface_method) = surface_method else {
                                return source_lerr(format!(
                                    "async surface function marker on linked method `{}`",
                                    method.name,
                                ));
                            };
                            async_surface.push_binding(
                                crate::async_surface::LinkedAsyncCallableBinding {
                                    module: mname.clone(),
                                    surface: crate::async_surface::AsyncSurfaceId {
                                        item: u32::try_from(surface_item).expect(
                                            "source item count exceeds async-surface identity range",
                                        ),
                                        method: Some(u32::try_from(surface_method).expect(
                                            "method count exceeds async-surface identity range",
                                        )),
                                    },
                                    local_state: u32::try_from(local_state).expect(
                                        "async state count exceeds async-surface state range",
                                    ),
                                    location: crate::async_surface::LinkedAsyncCallableLocation::InherentMethod {
                                        item: linked_item,
                                        method: u32::try_from(method_index).expect(
                                            "linked method count exceeds async-surface location range",
                                        ),
                                    },
                                },
                            );
                        }
                    }
                    item_map[source_index].push(items.len());
                    items.push(Item::Impl(im2));
                }
            }
        }
        origin_item_maps.insert(mname.clone(), item_map);
    }
    let mut linked_modes = modules
        .iter()
        .find(|(name, _)| name == entry)
        .map(|(_, module)| module.modes.clone())
        .unwrap_or_default();
    // RFC-0083: preserve the origin of borrowed APIs when a normal entry imports
    // an opt module. These are linked-AST metadata, never source modes; the
    // formatter filters them and the loan checker consumes them per function.
    linked_modes.extend(
        modules
            .iter()
            .filter(|(_, module)| is_opt(module))
            .map(|(name, _)| format!("@opt:{name}")),
    );
    let compiler_item_syntax = modules
        .iter()
        .flat_map(|(_, module)| module.compiler_item_syntax.iter().cloned())
        .collect();
    let compiler_expr_syntax = modules
        .iter()
        .flat_map(|(_, module)| module.compiler_expr_syntax.iter().cloned())
        .collect();
    let compiler_type_syntax = modules
        .iter()
        .flat_map(|(_, module)| module.compiler_type_syntax.iter().cloned())
        .collect();
    let compiler_pattern_syntax = modules
        .iter()
        .flat_map(|(_, module)| module.compiler_pattern_syntax.iter().cloned())
        .collect();
    let compiler_stmt_syntax = modules
        .iter()
        .flat_map(|(_, module)| module.compiler_stmt_syntax.iter().cloned())
        .collect();
    let compiler_block_syntax = modules
        .iter()
        .flat_map(|(_, module)| module.compiler_block_syntax.iter().cloned())
        .collect();
    let mut origins = crate::origin::OriginTable::default();
    for (name, _) in &modules {
        let Some(mut table) = origin_tables.remove(name) else { continue };
        if let Some(mapping) = origin_item_maps.get(name) {
            table.remap_items(name, mapping);
            origins.append_shifted(table, 0);
        }
    }
    let module_names = modules.iter().map(|(name, _)| name.clone()).collect();
    let mut module = Module {
        // The entry module's performance modes carry onto the linked module;
        // enforcement applies to the entry file's own (unqualified) functions.
        modes: linked_modes,
        imports: Vec::new(),
        from_imports: Vec::new(),
        items,
        import_lines: Vec::new(),
        item_lines: Vec::new(),
        compiler_item_syntax,
        compiler_expr_syntax,
        compiler_type_syntax,
        compiler_pattern_syntax,
        compiler_stmt_syntax,
        compiler_block_syntax,
        linked_entry: Some(entry.to_string()),
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
    crate::keyword_args::resolve(&mut module).map_err(|message| LinkError { message, location: None })?;
    // Semantics-preserving constant folding over the single linked module both
    // backends consume (parity-free by construction). See src/optimize.rs. Gated
    // on the `fold` lever (RFC-0030) so the differential de-opt sweep covers it:
    // `WITCHY_OPT=-fold` leaves constant expressions unfolded.
    if allow_ast_fold
        && !ast_fold_is_suppressed()
        && crate::opt::enabled(crate::opt::Opt::Fold)
    {
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
    let checked = crate::source_check::check(module).map_err(|error| LinkError {
        location: error.link_location(),
        message: error.message,
    })?;
    let checked = crate::generators::lower(checked)
        .map_err(|message| LinkError { message, location: None })?;
    let module = match async_stage {
        AsyncLoweringStage::Runtime => {
            let checked = crate::async_lower::lower_for_linker(checked)
                .map_err(|message| LinkError { message, location: None })?;
            crate::records::lower(checked)
                .map_err(|message| LinkError { message, location: None })?
                .into_module()
        }
        AsyncLoweringStage::TypedSource => crate::records::lower_for_async_typeck(checked.into_module())
            .map_err(|message| LinkError { message, location: None })?,
    };
    Ok(LinkedModule {
        module,
        origins,
        source_item_maps: origin_item_maps,
        async_surface,
        module_names,
        declarations,
    })
}

fn expansion_sibling(runtime: &Module, source: &Module) -> Module {
    let mut sibling = runtime.clone();
    // Expansion siblings execute compile-time code only. Runtime descriptor
    // requests belong to the enclosing executable module and may name types
    // intentionally absent from the synthetic `comptime` namespace.
    sibling
        .compiler_type_syntax
        .retain(|syntax| !syntax.runtime_identity);
    let existing: HashSet<String> = sibling
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Function(function) => Some(function.name.clone()),
            _ => None,
        })
        .collect();
    let restored = source
        .items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| match item {
            Item::Function(function)
                if function.comptime_only && !existing.contains(&function.name) =>
            {
                Some((
                    Item::Function(function.clone()),
                    source.item_lines.get(index).copied().unwrap_or(0),
                ))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    if !restored.is_empty() {
        sibling.item_lines.resize(sibling.items.len(), 0);
        for (item, line) in restored {
            sibling.items.push(item);
            sibling.item_lines.push(line);
        }
    }

    if sibling.compiler_item_syntax.is_empty() {
        sibling.compiler_item_syntax = source.compiler_item_syntax.clone();
    }
    if sibling.compiler_expr_syntax.is_empty() {
        sibling.compiler_expr_syntax = source.compiler_expr_syntax.clone();
    }
    if sibling.compiler_type_syntax.is_empty() {
        sibling.compiler_type_syntax = source
            .compiler_type_syntax
            .iter()
            .filter(|syntax| !syntax.runtime_identity)
            .cloned()
            .collect();
    }
    if sibling.compiler_pattern_syntax.is_empty() {
        sibling.compiler_pattern_syntax = source.compiler_pattern_syntax.clone();
    }
    if sibling.compiler_stmt_syntax.is_empty() {
        sibling.compiler_stmt_syntax = source.compiler_stmt_syntax.clone();
    }
    if sibling.compiler_block_syntax.is_empty() {
        sibling.compiler_block_syntax = source.compiler_block_syntax.clone();
    }
    sibling
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
            Stmt::Assign { name, value, .. } => {
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
        Expr::LabeledMethodCall { receiver, args, .. } => {
            resolve_in_expr(receiver, sig, by_base, vars);
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
        Expr::Unary { expr, .. }
        | Expr::Try(expr)
        | Expr::As { expr, .. }
        | Expr::ExistentialPack { expr, .. }
        | Expr::ExistentialUpcast { expr, .. }
        | Expr::Field { base: expr, .. } => {
            resolve_in_expr(expr, sig, by_base, vars)
        }
        Expr::ExistentialCall { receiver, args, .. } => {
            resolve_in_expr(receiver, sig, by_base, vars);
            for arg in args { resolve_in_expr(arg, sig, by_base, vars); }
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

#[derive(Clone, Copy)]
struct RewriteContext<'a> {
    module: &'a str,
    imports: &'a [String],
    bare_imports: Option<&'a HashMap<String, String>>,
    fns: &'a FnTable,
    bound: &'a HashSet<String>,
    user_std_shadows: &'a HashSet<String>,
    definition_site: bool,
    mode: LinkMode,
    opt: bool,
}

/// Resolve the direct function names in compiler-owned expression syntax
/// against the module that defined the syntax. The result remains an ordinary
/// expression AST, with unspellable markers that the final consumer link strips
/// after validating the already-resolved target. Hole expressions are inserted
/// after this pass and therefore retain their call-site context.
pub fn mark_definition_site_expr(
    expr: &mut Expr,
    definition_module: &str,
    modules: &[(String, Module)],
) -> Result<(), LinkError> {
    let Some((_, owner)) = modules.iter().find(|(name, _)| name == definition_module) else {
        return lerr(format!(
            "compiler-owned syntax refers to unknown definition module `{definition_module}`"
        ));
    };

    let mut available = modules.to_vec();
    for imported in owner
        .imports
        .iter()
        .map(String::as_str)
        .chain(PRELUDE_MODULES.iter().copied())
    {
        if available.iter().any(|(name, _)| name == imported) {
            continue;
        }
        let Some(source) = bundled_source(imported) else { continue };
        let module = crate::parser::parse_module(source).map_err(|error| LinkError {
            message: format!("standard-library module `{imported}` failed to parse: {error}"),
            location: None,
        })?;
        available.push((imported.to_string(), module));
    }

    crate::type_resolve::resolve_definition_site_expr(
        expr,
        definition_module,
        &available,
    )?;

    let reference_bearing = reference_bearing_type_names(&available);
    let non_rooted = non_rooted_reference_type_names(&available);
    let mut fns = FnTable::new();
    for (module_name, module) in &available {
        let mut names = HashMap::new();
        for item in &module.items {
            if let Item::Function(function) = item {
                names.insert(
                    function.name.clone(),
                    EtaSig {
                        arity: function.params.len(),
                        conventions: function
                            .params
                            .iter()
                            .map(|param| param.convention)
                            .collect(),
                        parameter_types: eta_parameter_types(function),
                        generic: function_has_type_parameters(function),
                        pure: function.pure,
                        public: function.public,
                        reference_bearing: function
                            .params
                            .iter()
                            .filter_map(|param| param.ty.as_ref())
                            .any(|ty| type_is_reference_bearing(ty, &reference_bearing))
                            || function
                            .ret
                            .as_ref()
                            .is_some_and(|ty| type_is_reference_bearing(ty, &reference_bearing)),
                        rooted_view_only: function_has_only_rooted_views(function, &non_rooted),
                        method_alias: false,
                        alias_target: None,
                    },
                );
            }
        }
        fns.insert(module_name.clone(), names);
    }

    let mut bare_imports = HashMap::new();
    for (source, names) in &owner.from_imports {
        for name in names {
            if fns
                .get(source)
                .and_then(|functions| functions.get(name))
                .is_some_and(|signature| signature.public)
            {
                bare_imports.insert(name.clone(), source.clone());
            }
        }
    }

    let bound = HashSet::new();
    let user_std_shadows = HashSet::new();
    let context = RewriteContext {
        module: definition_module,
        imports: &owner.imports,
        bare_imports: Some(&bare_imports),
        fns: &fns,
        bound: &bound,
        user_std_shadows: &user_std_shadows,
        definition_site: true,
        mode: LinkMode::Production,
        opt: true,
    };
    rewrite_expr(expr, &context, None)
}

#[allow(clippy::too_many_arguments)]
fn rewrite_block(
    b: &mut Block,
    m: &str,
    imps: &[String],
    bare_imports: Option<&HashMap<String, String>>,
    fns: &FnTable,
    bound: &HashSet<String>,
    user_std_shadows: &HashSet<String>,
    mode: LinkMode,
    _entry: &str,
    opt: bool,
) -> Result<(), LinkError> {
    let context = RewriteContext {
        module: m, imports: imps, bare_imports, fns, bound,
        user_std_shadows, definition_site: false,
        mode,
        opt,
    };
    rewrite_block_with_context(b, &context)
}

fn rewrite_block_with_context(b: &mut Block, context: &RewriteContext<'_>) -> Result<(), LinkError> {
    let mut bound = context.bound.clone();
    for (index, stmt) in b.stmts.iter_mut().enumerate() {
        let line = b.lines.get(index).copied().filter(|line| *line > 0);
        let scoped = RewriteContext { bound: &bound, ..*context };
        match stmt {
            Stmt::Let { name, value, .. } => {
                rewrite_expr(value, &scoped, line)?;
                bound.insert(name.clone());
            }
            Stmt::LetPattern { pattern, value } => {
                rewrite_expr(value, &scoped, line)?;
                let mut names = Vec::new();
                crate::ast::pattern_binds(pattern, &mut names);
                bound.extend(names);
            }
            Stmt::Assign { value, .. } => rewrite_expr(value, &scoped, line)?,
            Stmt::Return(Some(e)) | Stmt::Expr(e) | Stmt::Yield(e) => {
                rewrite_expr(e, &scoped, line)?
            }
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
    }
    Ok(())
}

fn rewrite_expr(
    e: &mut Expr,
    context: &RewriteContext<'_>,
    line: Option<u32>,
) -> Result<(), LinkError> {
    let RewriteContext {
        module: m,
        imports: imps,
        fns,
        bound,
        ..
    } = *context;
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
                        rewrite_expr(a, context, line)?;
                    }
                    *e = Expr::MethodCall { receiver, method, args: call_args };
                    return Ok(());
                }
            }
            let resolved = resolve_call(name, context, line)?;
            if let Some(sig) = fn_sig(fns, &resolved).filter(|sig| sig.method_alias) {
                let target = sig
                    .alias_target
                    .expect("method aliases carry the generated implementation name");
                *name = if context.definition_site {
                    let module = resolved
                        .split_once('.')
                        .map_or(m, |(module, _)| module);
                    format!("{DEFINITION_SITE_PREFIX}{module}.{target}")
                } else {
                    target
                };
                for a in args {
                    rewrite_expr(a, context, line)?;
                }
                return Ok(());
            }
            *name = if context.definition_site && resolved.contains('.') {
                format!("{DEFINITION_SITE_PREFIX}{resolved}")
            } else {
                resolved
            };
            for a in args {
                rewrite_expr(a, context, line)?;
            }
        }
        // (RFC-0056) A labeled direct call: qualify the callee exactly like a plain
        // call so `keyword_args::resolve` can look up its declaration, and rewrite
        // the argument values. The labels ride along untouched until then.
        Expr::LabeledCall { name, args } => {
            let resolved = resolve_call(name, context, line)?;
            if fn_sig(fns, &resolved).is_some_and(|sig| sig.method_alias) {
                return lerr_at(
                    format!(
                        "`{resolved}` is a method alias; call it as `receiver.{}(...)` \
                         or use the positional module form",
                        resolved.rsplit_once('.').map_or(resolved.as_str(), |(_, name)| name)
                    ),
                    m,
                    line,
                );
            }
            *name = if context.definition_site && resolved.contains('.') {
                format!("{DEFINITION_SITE_PREFIX}{resolved}")
            } else {
                resolved
            };
            for (_, a) in args {
                rewrite_expr(a, context, line)?;
            }
        }
        // A bare name matching a same-module function is a first-class reference
        // to it; qualify it like a call — unless it is shadowed by a local of the
        // same name (a parameter, `let`, loop variable, or pattern binding).
        Expr::Var(name) => {
            if let Some(target) = call_site_expr_target(name).map(str::to_owned) {
                if context.definition_site {
                    return Ok(());
                }
                *name = target;
            }
            if let Some(resolved) = definition_site_target(name, fns, m, line)? {
                *name = resolved;
            } else if !bound.contains(name.as_str())
                && (fns.get(m).is_some_and(|s| s.contains_key(name.as_str()))
                    || context.bare_imports.is_some_and(|imports| imports.contains_key(name.as_str())))
            {
                // Function values cross the same declaration/privacy boundary
                // as direct calls, including unqualified `from` imports.
                let resolved = resolve_call(name, context, line)?;
                *name = if context.definition_site {
                    format!("{DEFINITION_SITE_PREFIX}{resolved}")
                } else {
                    resolved
                };
            }
        }
        Expr::Apply { func, args } => {
            rewrite_expr(func, context, line)?;
            for a in args {
                rewrite_expr(a, context, line)?;
            }
        }
        Expr::TaggedLit { tag, .. } => {
            if context.definition_site && definition_site_tag_target(tag).is_none() {
                let target_module = definition_site_tag_module(tag, context, line)?;
                *tag = format!("{DEFINITION_SITE_PREFIX}{target_module}.{tag}");
            }
        }
        Expr::Ctor { args, .. } | Expr::AnonCtor { args, .. }
        | Expr::List(args) | Expr::Tuple(args) => {
            for a in args {
                rewrite_expr(a, context, line)?;
            }
        }
        Expr::Unary { expr, .. }
        | Expr::Try(expr)
        | Expr::As { expr, .. }
        | Expr::ExistentialPack { expr, .. }
        | Expr::ExistentialUpcast { expr, .. } => {
            rewrite_expr(expr, context, line)?
        }
        Expr::ExistentialCall { receiver, args, .. } => {
            rewrite_expr(receiver, context, line)?;
            for arg in args { rewrite_expr(arg, context, line)?; }
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
                let qualified = resolve_call(&format!("{modname}.{field}"), context, line)?;
                let sig = fns
                    .get(&modname)
                    .and_then(|s| s.get(&field))
                    .cloned()
                    .expect("resolve_call accepted the reference, so the function exists");
                // Generic function values are not monomorphized by a rendered
                // spelling. Keep the checked declaration identity so type
                // checking instantiates its signature at the application site;
                // lowering already synthesizes the same forwarding closure from
                // a top-level `Var` value. Inherent method aliases still need
                // their generated implementation target, so they retain the
                // ordinary eta rewrite.
                *e = if sig.generic && !sig.method_alias {
                    Expr::Var(qualified)
                } else {
                    eta_lambda(&qualified, sig)
                };
                if context.definition_site {
                    rewrite_expr(e, context, line)?;
                }
                return Ok(());
            }
            // (BUG-303) A value-position `iter.count` whose base is a KNOWN std
            // module that is simply not imported would otherwise fall through to
            // an ordinary field read and die as "unbound variable `iter`". Emit
            // the same missing-import teaching diagnostic the call position gives,
            // so "unbound variable" never names a module.
            if let Expr::Var(modname) = base.as_ref() {
                if !bound.contains(modname.as_str()) && STD_MODULES.contains(&modname.as_str()) {
                    return lerr_at(
                        format!(
                            "`{modname}.{field}` looks like a module-qualified reference, but \
                             `{modname}` is not imported — add `import {modname}`"
                        ),
                        m,
                        line,
                    );
                }
            }
            rewrite_expr(base, context, line)?;
        }
        Expr::RecordUpdate { name: _, base, fields } => {
            rewrite_expr(base, context, line)?;
            for (_, value) in fields {
                rewrite_expr(value, context, line)?;
            }
        }
        Expr::Record { fields, spread, .. } => {
            for (_, value) in fields {
                rewrite_expr(value, context, line)?;
            }
            if let Some(s) = spread {
                rewrite_expr(s, context, line)?;
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            rewrite_expr(lhs, context, line)?;
            rewrite_expr(rhs, context, line)?;
        }
        Expr::Range { lo, hi, .. } => {
            rewrite_expr(lo, context, line)?;
            rewrite_expr(hi, context, line)?;
        }
        Expr::Index { base, index } => {
            rewrite_expr(base, context, line)?;
            rewrite_expr(index, context, line)?;
        }
        // Module/member classification has already been repaired against the
        // final import scope before source checking and sealing.
        Expr::MethodCall { receiver, args, .. } => {
            rewrite_expr(receiver, context, line)?;
            for a in args {
                rewrite_expr(a, context, line)?;
            }
        }
        Expr::LabeledMethodCall { receiver, args, .. } => {
            rewrite_expr(receiver, context, line)?;
            for (_, a) in args {
                rewrite_expr(a, context, line)?;
            }
        }
        Expr::WhileLet { pattern, scrutinee, body } => {
            rewrite_expr(scrutinee, context, line)?;
            let mut nested_bound = bound.clone();
            collect_pattern_vars(pattern, &mut nested_bound);
            let nested = RewriteContext { bound: &nested_bound, ..*context };
            rewrite_block_with_context(body, &nested)?;
        }
        Expr::If {
            cond,
            then_block,
            else_block,
        } => {
            rewrite_expr(cond, context, line)?;
            rewrite_block_with_context(then_block, context)?;
            if let Some(b) = else_block {
                rewrite_block_with_context(b, context)?;
            }
        }
        Expr::Lambda { params, body, .. } => {
            let mut nested_bound = bound.clone();
            nested_bound.extend(params.iter().map(|param| param.name.clone()));
            let nested = RewriteContext { bound: &nested_bound, ..*context };
            rewrite_block_with_context(body, &nested)?;
        }
        Expr::Block(b) => rewrite_block_with_context(b, context)?,
        Expr::While { cond, body } => {
            rewrite_expr(cond, context, line)?;
            rewrite_block_with_context(body, context)?;
        }
        Expr::For { var, iter, body } => {
            rewrite_expr(iter, context, line)?;
            let mut nested_bound = bound.clone();
            nested_bound.insert(var.clone());
            let nested = RewriteContext { bound: &nested_bound, ..*context };
            rewrite_block_with_context(body, &nested)?;
        }
        Expr::Match { scrutinee, arms } => {
            rewrite_expr(scrutinee, context, line)?;
            for arm in arms {
                let mut nested_bound = bound.clone();
                collect_pattern_vars(&arm.pattern, &mut nested_bound);
                let nested = RewriteContext { bound: &nested_bound, ..*context };
                if let Some(g) = &mut arm.guard {
                    rewrite_expr(g, &nested, line)?;
                }
                rewrite_expr(&mut arm.body, &nested, line)?;
            }
        }
        Expr::Int(_) | Expr::Duration(_) | Expr::Float(_) | Expr::Str(_) | Expr::Bool(_) => {}
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
/// The lambda captures nothing, but its parameters retain the declaration's
/// authenticated type facts: `var unique List(Int)` must remain `var unique
/// List(Int)` when the function is named as a value. A source-to-source rewrite
/// on the single linked AST before either backend lowers: parity by construction.
/// The arity is the callee's FULL declared parameter count; RFC-0056 defaults
/// never attach to a function value, so every positional argument is present.
fn eta_lambda(qualified: &str, sig: EtaSig) -> Expr {
    let arity = sig.arity;
    let names: Vec<String> = (0..arity).map(|i| format!("__eta{i}")).collect();
    let params = names
        .iter()
        .enumerate()
        .map(|(index, n)| Param {
            name: n.clone(),
            ty: sig.parameter_types.get(index).cloned().unwrap_or(None),
            convention: sig.conventions.get(index).copied().unwrap_or(Convention::Let),
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
        qualifiers: CallableQualifiers::new(sig.pure, false),
    }
}

fn fn_sig(fns: &FnTable, qualified: &str) -> Option<EtaSig> {
    let (modname, fname) = qualified.split_once('.')?;
    fns.get(modname).and_then(|s| s.get(fname)).cloned()
}

/// Nongeneric declarations already have canonicalized type identities. Retain
/// their entire parameter storage contract, not just a top-level qualifier.
/// Generic function values bypass eta expansion; generic method aliases keep
/// their existing inference behavior rather than placing free type variables
/// into a nongeneric lambda annotation.
fn eta_parameter_types(function: &Function) -> Vec<Option<Type>> {
    let generic = function_has_type_parameters(function);
    function.params.iter().map(|param| match param.ty.as_ref() {
        Some(ty) if !generic || matches!(ty, Type::Qualified(_, _)) => Some(ty.clone()),
        _ => None,
    }).collect()
}

fn function_has_type_parameters(function: &Function) -> bool {
    if !function.bounds.is_empty() {
        return true;
    }
    let mut parameters = Vec::new();
    for parameter in &function.params {
        if let Some(ty) = &parameter.ty {
            collect_type_vars(ty, &mut parameters);
        }
    }
    if let Some(result) = &function.ret {
        collect_type_vars(result, &mut parameters);
    }
    !parameters.is_empty()
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

// Reflection and derive insert this unspellable marker after parsing source.
// Restore the ordinary constructor only once user-written constructors have
// passed the sealing check; subsequent type checking sees the real TypeInfo.
fn restore_compiler_type_info(modules: &mut [(String, Module)]) {
    fn restore(body: &mut Block) {
        crate::ast::visit::visit_block_mut(body, &mut |expression| {
            if let Expr::Call { name, args } = expression
                && name == "@compiler:type-info"
            {
                *expression = Expr::Ctor {
                    name: "meta.TypeInfo".into(),
                    args: std::mem::take(args),
                };
            }
            Ok::<_, ()>(())
        }).expect("compiler type-info restoration is infallible");
    }
    for (_, module) in modules {
        for item in &mut module.items {
            match item {
                Item::Function(function) => restore(&mut function.body),
                Item::Impl(implementation) => {
                    for method in &mut implementation.methods {
                        restore(&mut method.body);
                    }
                }
                _ => {}
            }
        }
    }
}

fn check_sealing(
    modules: &[(String, Module)],
    origin_tables: &HashMap<String, crate::origin::OriginTable>,
    mode: LinkMode,
    entry: &str,
) -> Result<(), LinkError> {
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
                    for v in t.variants.iter() {
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
        for (item_index, item) in m.items.iter().enumerate() {
            // Test-only construction is authenticated at the source definition
            // site. A comptime-emitted item, or a handwritten item containing
            // tag-generated syntax, is not wholly source-authored and cannot
            // inherit that privilege merely by landing in the entry module.
            let source_authored = origin_tables.get(mname).is_none_or(|origins| {
                origins
                    .nodes()
                    .iter()
                    .all(|origin| origin.node.item as usize != item_index)
            });
            let allow_test_sealed_type_construction =
                mode == LinkMode::Test && mname == entry && source_authored;
            match item {
                Item::Function(f) => {
                    // This unspellable function is built entirely by tagged::expand.
                    // Give only its compiler-issued TagHole constructors a distinct
                    // sealing identity; ordinary source in this module keeps mname.
                    let seal_home = if f.name == "@compiler:tag-bridge" {
                        "@compiler:tag-bridge"
                    } else {
                        mname.as_str()
                    };
                    seal_block(&f.body, &sealed, seal_home, allow_test_sealed_type_construction)?;
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
            if verb == "construct"
                && home == "@compiler:tag-bridge"
                && decl == "meta"
                && (name == "meta.TagHole" || name == "TagHole")
            {
                return Ok(());
            }
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
        Expr::LabeledMethodCall { receiver, args, .. } => {
            seal_expr(receiver, sealed, home, allow_test_sealed_type_construction)?;
            for (_, a) in args {
                seal_expr(a, sealed, home, allow_test_sealed_type_construction)?;
            }
        }
        Expr::Unary { expr, .. }
        | Expr::Try(expr)
        | Expr::As { expr, .. }
        | Expr::ExistentialPack { expr, .. }
        | Expr::ExistentialUpcast { expr, .. }
        | Expr::Field { base: expr, .. } => {
            seal_expr(expr, sealed, home, allow_test_sealed_type_construction)?;
        }
        Expr::ExistentialCall { receiver, args, .. } => {
            seal_expr(receiver, sealed, home, allow_test_sealed_type_construction)?;
            for arg in args { seal_expr(arg, sealed, home, allow_test_sealed_type_construction)?; }
        }
        Expr::RecordUpdate { name, base, fields } => {
            if let Some(name) = name {
                seal_use(name, sealed, home, "construct", allow_test_sealed_type_construction)?;
            }
            seal_expr(base, sealed, home, allow_test_sealed_type_construction)?;
            for (_, v) in fields {
                seal_expr(v, sealed, home, allow_test_sealed_type_construction)?;
            }
        }
        Expr::Record { name, fields, spread } => {
            seal_use(name, sealed, home, "construct", allow_test_sealed_type_construction)?;
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
    context: &RewriteContext<'_>,
    line: Option<u32>,
) -> Result<String, LinkError> {
    let RewriteContext {
        module: m,
        imports: imps,
        bare_imports,
        fns,
        bound,
        user_std_shadows,
        definition_site: _,
        mode,
        opt,
    } = *context;
    if let Some(target) = call_site_expr_target(name) {
        if context.definition_site {
            return Ok(name.to_string());
        }
        return resolve_call(target, context, line);
    }
    if let Some(resolved) = definition_site_target(name, fns, m, line)? {
        return Ok(resolved);
    }
    let accept = |resolved: String| -> Result<String, LinkError> {
        if let Some((provider, function)) = resolved.split_once('.')
            && !user_std_shadows.contains(provider)
            && let Some(owners) = crate::intrinsics::private_source_bridge_callers(&resolved)
            && !owners.contains(&m)
        {
            return lerr_at(
                format!("`{function}` is a compiler-private intrinsic for std/{}`; use the public stdlib surface instead", owners.join(" or std/")),
                m, line,
            );
        }
        Ok(resolved)
    };
    check_private_intrinsic_call(name, m, line)?;
    if let Some((modname, fname)) = name.split_once('.') {
        // The prelude modules are importable-by-default everywhere (the link
        // set always carries them), including from inside one another.
        let prelude = is_prelude_module(modname);
        if !prelude && !imps.iter().any(|i| i == modname) {
            return lerr_at(
                format!("module `{m}` calls `{modname}.{fname}` but does not `import {modname}`"),
                m,
                line,
            );
        }
        return match fns.get(modname).and_then(|s| s.get(fname)) {
            Some(_) if modname == m => accept(name.to_string()),
            Some(_) if private_intrinsic_friend_call(modname, fname, m) => {
                accept(name.to_string())
            }
            Some(sig) if sig.public
                && (modname == m || opt || !sig.reference_bearing || sig.rooted_view_only) => {
                accept(name.to_string())
            }
            Some(sig) if sig.public && sig.reference_bearing => lerr_at(
                format!(
                    "normal module `{m}` cannot use non-rooted-reference opt API `{modname}.{fname}`; \
                     only immutable rooted-view APIs cross this boundary"
                ),
                m,
                line,
            ),
            Some(_) if static_evaluation_origin_constructor(mode, modname, fname) => {
                accept(name.to_string())
            }
            Some(_) => lerr_at(
                format!("function `{modname}.{fname}` is private to module `{modname}`"),
                m,
                line,
            ),
            None => lerr_at(
                missing_module_function_message(modname, fname, user_std_shadows),
                m,
                line,
            ),
        };
    }
    // A lexical binding wins over every function namespace. `f(...)` is an
    // indirect call when a parameter or local closure named `f` is in scope,
    // even if the module also declares a top-level `f`. Resolve this before the
    // same-module and builtin cases so checked linking preserves source
    // shadowing instead of silently turning the call into self-recursion.
    if bound.contains(name) {
        return Ok(name.to_string());
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
    {
        if let Some(srcmod) = bare_imports.and_then(|imports| imports.get(name)) {
            if !opt
                && fns
                    .get(srcmod)
                    .and_then(|functions| functions.get(name))
                    .is_some_and(|signature| {
                        signature.reference_bearing && !signature.rooted_view_only
                    })
            {
                return lerr_at(
                    format!(
                        "normal module `{m}` cannot use non-rooted-reference opt API `{srcmod}.{name}`; \
                         only immutable rooted-view APIs cross this boundary"
                    ),
                    m,
                    line,
                );
            }
            return accept(format!("{srcmod}.{name}"));
        }
        if let Some(srcmod) = imps
            .iter()
            .find(|imp| fns.get(*imp).and_then(|s| s.get(name)).is_some_and(|sig| sig.public))
        {
            return lerr_at(
                format!(
                    "`{name}(...)` is not in scope as a bare function. `import {srcmod}` keeps \
                     functions qualified; write `{srcmod}.{name}(...)` or add \
                     `from {srcmod} import {name}`"
                ),
                m,
                line,
            );
        }
    }
    // Not a function here and not a builtin: a local binding being applied (e.g.
    // a lambda parameter). Leave it unqualified; the type checker decides.
    Ok(name.to_string())
}

/// The pre-link static-evaluation recipe is the only producer of these calls.
/// Keep this name-level allowance in the linker deliberately smaller than a
/// general private-access mode; the recipe separately verifies that the raw
/// `glamour` module has the authenticated toolchain identity before selecting
/// [`LinkMode::StaticEvaluation`].
fn static_evaluation_origin_constructor(mode: LinkMode, module: &str, function: &str) -> bool {
    mode == LinkMode::StaticEvaluation
        && module == "glamour"
        && matches!(function, "interactive_with_origin" | "client_region_with_origin")
}

fn definition_site_target(
    name: &str,
    fns: &FnTable,
    module_name: &str,
    line: Option<u32>,
) -> Result<Option<String>, LinkError> {
    let Some(target) = name.strip_prefix(DEFINITION_SITE_PREFIX) else {
        return Ok(None);
    };
    let Some((target_module, target_name)) = target.split_once('.') else {
        return lerr_at(
            "compiler-owned definition-site name lost its qualified target",
            module_name,
            line,
        );
    };
    let exists = fns
        .get(target_module)
        .is_some_and(|functions| {
            functions.contains_key(target_name)
                || functions.values().any(|signature| {
                    signature.alias_target.as_deref() == Some(target_name)
                })
        });
    if !exists {
        return lerr_at(
            format!(
                "compiler-owned definition-site reference targets unknown function `{target}`"
            ),
            module_name,
            line,
        );
    }
    Ok(Some(target.to_string()))
}

/// Decode the compiler-owned identity attached to a nested tagged literal.
/// Source cannot spell the `@` prefix, so this may authorize a private tag in
/// the module whose generated AST introduced it.
pub fn definition_site_tag_target(tag: &str) -> Option<(&str, &str)> {
    tag.strip_prefix(DEFINITION_SITE_PREFIX)?.split_once('.')
}

fn definition_site_tag_module(
    tag: &str,
    context: &RewriteContext<'_>,
    line: Option<u32>,
) -> Result<String, LinkError> {
    if context
        .fns
        .get(context.module)
        .is_some_and(|functions| functions.contains_key(tag))
    {
        return Ok(context.module.to_string());
    }
    let mut imported = context
        .imports
        .iter()
        .filter(|module| {
            context
                .fns
                .get(*module)
                .and_then(|functions| functions.get(tag))
                .is_some_and(|signature| signature.public)
        })
        .cloned()
        .collect::<Vec<_>>();
    imported.sort();
    imported.dedup();
    match imported.as_slice() {
        [] => Ok(context.module.to_string()),
        [module] => Ok(module.clone()),
        modules => lerr_at(
            format!(
                "definition-site tagged literal `{tag}` is ambiguous across directly imported modules: {}",
                modules.join(", ")
            ),
            context.module,
            line,
        ),
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

pub fn is_generated_anon_name(name: &str) -> bool {
    name.strip_prefix("__anon")
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

fn is_generated_anon_union_name(name: &str) -> bool {
    name.strip_prefix("__union")
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

fn is_generated_anon_type(definition: &TypeDef) -> bool {
    let Some(fields) = crate::ast::anon_record_field_names(&definition.name) else {
        return false;
    };
    matches!(definition.variants.as_slice(), [variant]
        if variant.name == definition.name
            && variant.line == u32::MAX
            && variant.field_names == fields)
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
        for item in &module.items {
            if let Item::Type(t) = item {
                if is_generated_anon_type(t) {
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
            if f.attributes.iter().any(|attribute| {
                attribute == crate::source_check::GENERATED_ITEM_ATTRIBUTE
            }) {
                // Typed `emit_item` owns this complete function subtree. The
                // parser cannot construct this internal attribute, so private
                // fresh parameters, locals, and references remain compiler
                // syntax when semantic checking recursively links a runtime
                // projection.
                return Ok(());
            }
            if is_reserved_user_identifier(&f.name) {
                return reserved_name_error(module_name, "function", &f.name);
            }
            check_reserved_function(module_name, f, generated_anon_types)
        }
        Item::Type(t) => {
            let generated_anon = is_generated_anon_type(t);
            if is_reserved_user_identifier(&t.name) && !generated_anon {
                return reserved_name_error(module_name, "type", &t.name);
            }
            for param in &t.params {
                check_reserved_binding(module_name, "type parameter", param, line)?;
            }
            for variant in t.variants.iter() {
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
            let generated_impl = im.origin == ImplOrigin::CompilerGenerated;
            if let Some(trait_name) = &im.trait_name {
                if is_reserved_user_identifier(trait_name) {
                    return reserved_name_error(module_name, "trait", trait_name);
                }
            }
            let generated_anon_target = generated_impl
                && crate::ast::anon_record_field_names(&im.type_name).is_some();
            if is_reserved_user_identifier(&im.type_name) && !generated_anon_target {
                return reserved_name_error(module_name, "type", &im.type_name);
            }
            for method in &im.methods {
                if generated_impl {
                    // A typed `emit_item` owns the complete impl subtree. Its
                    // private method, parameter, local, and intrinsic names are
                    // compiler syntax rather than user-spellable declarations.
                    continue;
                }
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
        // (RFC-0081; mechanical arm only — this file is owned by another lane)
        // The head is a trait name: apply the same reserved-identifier check as
        // `Named`, then walk the trait arguments.
        Type::Dyn(name, args) => {
            if is_reserved_user_identifier(name) {
                return reserved_name_error(module_name, "trait", name);
            }
            for arg in args {
                check_reserved_type(module_name, arg, generated_anon_types)?;
            }
        }
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
        Type::RecordCompose { base, fields } => {
            check_reserved_type(module_name, base, generated_anon_types)?;
            for (_, field) in fields {
                check_reserved_type(module_name, field, generated_anon_types)?;
            }
        }
        Type::Fn(params, ret, _, _) => {
            for param in params {
                check_reserved_type(module_name, param, generated_anon_types)?;
            }
            check_reserved_type(module_name, ret, generated_anon_types)?;
        }
        Type::Qualified(_, inner) => check_reserved_type(module_name, inner, generated_anon_types)?,
        Type::Slice(inner) => check_reserved_type(module_name, inner, generated_anon_types)?,
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
        Stmt::Assign { name, value, .. } => {
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
        Expr::LabeledMethodCall { receiver, args, .. } => {
            check_reserved_expr(module_name, receiver, line, generated_anon_types)?;
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
        Expr::Lambda { params, body, ret, .. } => {
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
        Expr::Unary { expr, .. }
        | Expr::Try(expr)
        | Expr::As { expr, .. }
        | Expr::ExistentialPack { expr, .. }
        | Expr::ExistentialUpcast { expr, .. }
        | Expr::Field { base: expr, .. } => {
            check_reserved_expr(module_name, expr, line, generated_anon_types)?
        }
        Expr::ExistentialCall { receiver, args, .. } => {
            check_reserved_expr(module_name, receiver, line, generated_anon_types)?;
            for arg in args { check_reserved_expr(module_name, arg, line, generated_anon_types)?; }
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

fn check_private_intrinsic_call(name: &str, module_name: &str, line: Option<u32>) -> Result<(), LinkError> {
    let intrinsic = name.rsplit_once('.').map_or(name, |(_, bare)| bare);
    if intrinsic == crate::intrinsics::RETIRED_SOURCE_RENDER {
        return lerr_at(
            "`__render` is compiler-private; use string interpolation (`\"${value}\"`) \
             or `show.render(value)` instead",
            module_name,
            line,
        );
    }
    let Some(owners) = crate::intrinsics::private_intrinsic_callers(intrinsic) else {
        return Ok(());
    };
    if owners.contains(&module_name) {
        return Ok(());
    }
    lerr_at(
        format!(
            "`{intrinsic}` is a compiler-private intrinsic for std/{}`; use the public stdlib surface instead",
            owners.join(" or std/")
        ),
        module_name,
        line,
    )
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

fn private_intrinsic_friend_call(provider: &str, name: &str, caller: &str) -> bool {
    provider == "task"
        && crate::intrinsics::is_channel_bridge(name)
        && crate::intrinsics::private_intrinsic_callers(name)
            .is_some_and(|callers| callers.contains(&caller))
}
