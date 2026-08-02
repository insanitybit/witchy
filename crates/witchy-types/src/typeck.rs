//! A type checker for witchy.
//!
//! Annotation-driven checking with light Hindley-Milner-style unification for
//! the bits that aren't annotated (let bindings, match arms). It is deliberately
//! lenient where it lacks information (e.g. an unknown constructor's arguments)
//! so it never rejects a valid program — it tightens as the type system grows.
//!
//! Capability safety is not a special case: `print` has type
//! `(Console, String) -> Nil`, and the only way to obtain a `Console` is to
//! receive one as a parameter — ultimately from `main`. So "this code may
//! perform output" is simply visible in its type, and code that never received
//! the capability cannot type-check a call that needs it.

// foldhash (not SipHash): every key in these tables is compiler-internal
// (names, spans, expr identities) — never an attacker-chosen collection — so
// the checker's hot maps skip DoS-resistant hashing. The `*Ext` traits supply
// `::new()`/`::with_capacity()` for the aliased types.
use foldhash::{HashMap, HashMapExt as _, HashSet, HashSetExt as _};
use std::fmt;

use witchy_syntax::ast::{
    self, Block, Convention, Expr, Function, Item, MatchArm, Module, Pattern, Stmt, UnOp,
};
use witchy_syntax::build_entry::{build_entrypoint, is_build_capability_type};
use witchy_syntax::{cap_ops, intrinsics};
use witchy_cap_model::{CapabilityClass, CapabilityKind};

use crate::storage::{externref_cap_name, ReferenceLeaf, ReferenceStorageClassifier};

mod coverage;
use coverage::{
    describe_pattern, expand_head_or, flatten_or_arms, pattern_ctor_key, specialize_row, ColCtor,
    LIST_CONS, LIST_NIL, TUPLE_CTOR,
};

mod cap_rights;
mod capability_calls;
pub use cap_rights::{ConsoleRights, DirRights, FileRights, NetRights};
use cap_rights::{console_rights, dir_rights, file_rights, net_rights, validate_cap_markers};

mod uniqueness;
use uniqueness::{
    check_unique_declarations, check_unique_functions, check_unique_parameters,
    UniquenessError,
};

mod existential;
use existential::{check_existential_types, existential_bare};

mod compiler_syntax;
use compiler_syntax::{
    check_compiler_syntax_declarations, compiler_syntax_allowed_module, compiler_syntax_type_name,
    is_compiler_generated_structural_impl,
};

#[derive(Debug, Clone, PartialEq)]
pub enum Ty {
    Int,
    Float,
    /// A length of time (from a duration literal like `30s`). A distinct type:
    /// it does not mix with `Int` under arithmetic.
    Duration,
    String,
    Bytes,
    /// (RFC-0055) The erased message type of the concurrency executor. Opaque and
    /// representationally the universal slot (like a generic type variable, and
    /// like `Bytes` shares `String`'s layout): the `std/chan`/`std/task` executor
    /// buffers, `Step`, and `Slot` are monomorphic over it, while typed channel
    /// endpoints (`Sender(m)`/`Receiver(m)`) erase/unerase at the boundary via the
    /// non-inferable `__erase`/`__unerase` intrinsics, confined to `std/chan`. The
    /// spelling `__Msg` is reserved (double-underscore) so it never collides with a
    /// user `type Msg`.
    Msg,
    Bool,
    Unit,
    Console(ConsoleRights),
    Clock,
    /// The runtime authority to draw cryptographic randomness (`rand_u64`).
    Rand,
    Env,
    Secret,
    /// The runtime authority to spawn a native subprocess. Right-less (one op):
    /// the executable is named and confined through a `Dir[Read]` argument, so
    /// "you can only execute a file you can read". See rfcs/0004-self-hosted-cli.md.
    Exec,
    /// Origin-scoped authority to perform host HTTP(S) requests.
    Fetch,
    Dir(DirRights),
    File(FileRights),
    Net(NetRights),
    Socket,
    Listener,
    /// Build-time capabilities — a parallel set to the runtime caps, granted only
    /// to a rune's `build` entrypoint and enforced in a zero-ambient build
    /// sandbox. Kind-only (the specific tool/host/dir/var is the consumer's grant,
    /// not the type); see rfcs/build-time-execution-plan.md.
    BuildOut,
    BuildRead,
    BuildEnv,
    BuildNet,
    BuildExec,
    List(Box<Ty>),
    Tuple(Vec<Ty>),
    /// A user-declared type, possibly with type arguments: `Option(Int)`,
    /// `Result(String, Error)`. Non-generic types carry an empty argument list.
    Named(String, Vec<Ty>),
    /// (RFC-0081) A first-class existential identity: the RESOLVED bare trait
    /// name plus its fully substituted type arguments (`dyn Render`,
    /// `dyn Convert(Int)`). Never a guessed type name: the head comes from the
    /// checked `ast::Type::Dyn` whose trait was validated against the module's
    /// trait declarations, and the arguments recurse through the ordinary type
    /// resolution, so aliases and cross-module uses of the same instantiation
    /// share one identity. Two existentials unify only on the same head, same
    /// arity, and pairwise-unifying arguments — `dyn Sub` never unifies with
    /// `dyn Super` in this slice.
    Dyn(String, Vec<Ty>),
    /// A function type: parameter types, return type, and parameter conventions.
    Fn(Vec<Ty>, Box<Ty>, Vec<Convention>),
    Var(u32),
}

impl fmt::Display for Ty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Ty::Int => write!(f, "Int"),
            Ty::Float => write!(f, "Float"),
            Ty::Duration => write!(f, "Duration"),
            Ty::String => write!(f, "String"),
            Ty::Bytes => write!(f, "Bytes"),
            Ty::Msg => write!(f, "__Msg"),
            Ty::Bool => write!(f, "Bool"),
            Ty::Unit => write!(f, "()"),
            Ty::Console(rights) => write!(f, "{rights}"),
            Ty::Clock => write!(f, "Clock"),
            Ty::Rand => write!(f, "Rand"),
            Ty::Env => write!(f, "Env"),
            Ty::Secret => write!(f, "Secret"),
            Ty::Exec => write!(f, "Exec"),
            Ty::Fetch => write!(f, "Fetch"),
            Ty::Dir(r) => write!(f, "{r}"),
            Ty::File(r) => write!(f, "{r}"),
            Ty::Net(r) => write!(f, "{r}"),
            Ty::Socket => write!(f, "Socket"),
            Ty::Listener => write!(f, "Listener"),
            Ty::BuildOut => write!(f, "BuildOut"),
            Ty::BuildRead => write!(f, "BuildRead"),
            Ty::BuildEnv => write!(f, "BuildEnv"),
            Ty::BuildNet => write!(f, "BuildNet"),
            Ty::BuildExec => write!(f, "BuildExec"),
            Ty::List(e) => write!(f, "List({e})"),
            Ty::Tuple(ts) => {
                write!(f, "(")?;
                for (i, t) in ts.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{t}")?;
                }
                write!(f, ")")
            }
            Ty::Named(n, args) => {
                write!(f, "{n}")?;
                if !args.is_empty() {
                    write!(f, "(")?;
                    for (i, t) in args.iter().enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{t}")?;
                    }
                    write!(f, ")")?;
                }
                Ok(())
            }
            // (RFC-0081) Canonical existential rendering, matching the
            // formatter: `dyn Render`, `dyn Convert(Int)`.
            Ty::Dyn(n, args) => {
                write!(f, "dyn {n}")?;
                if !args.is_empty() {
                    write!(f, "(")?;
                    for (i, t) in args.iter().enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{t}")?;
                    }
                    write!(f, ")")?;
                }
                Ok(())
            }
            Ty::Fn(params, ret, conventions) => {
                write!(f, "fn(")?;
                for (i, t) in params.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    let prefix = match conventions.get(i).copied().unwrap_or_default() {
                        Convention::Let => "",
                        Convention::Borrow => "let ",
                        Convention::Var => "var ",
                        Convention::Own => "own ",
                    };
                    write!(f, "{prefix}{t}")?;
                }
                write!(f, ") -> {ret}")
            }
            Ty::Var(_) => write!(f, "?"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TypeError {
    pub message: String,
}

impl fmt::Display for TypeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "type error: {}", self.message)
    }
}

impl std::error::Error for TypeError {}

fn terr<T>(message: impl Into<String>) -> Result<T, TypeError> {
    Err(TypeError {
        message: message.into(),
    })
}

fn in_call_context(display: &str, e: TypeError) -> TypeError {
    if e.message.starts_with("expected `") {
        TypeError { message: format!("in call to `{display}`: {}", e.message) }
    } else {
        e
    }
}

fn type_mismatch_context(context: impl FnOnce() -> String, e: TypeError) -> TypeError {
    if e.message.starts_with("expected `") {
        TypeError { message: format!("{}: {}", context(), e.message) }
    } else {
        e
    }
}

/// Prefix a type error with where it occurred — the enclosing function (after
/// linking this is `module.func`, which also names the file) and source line.
/// `line == 0` means no line is available; an empty `func` omits the name.
fn at_loc(e: TypeError, line: u32, func: &str, home: &str) -> TypeError {
    if line == 0 {
        return e;
    }
    let where_ = if func.is_empty() {
        format!("line {line}")
    } else {
        format!("`{}`, line {line}", diagnostic_callable_name(func))
    };
    // The location prefix already names the home module, so render home-module
    // type/variant names bare in the body — the spelling the reader wrote — while
    // keeping cross-module qualifiers (BUG-292).
    TypeError {
        message: format!("{where_}: {}", strip_home_qualifiers(&e.message, home)),
    }
}

/// Non-capability type names known without a declaration. Built-in capability
/// names and their zero-arity/right-bearing shapes come from
/// `witchy-cap-model`.
const BUILTIN_TYPE_NAMES: &[&str] = &[
    "Int", "Float", "Duration", "String", "Bytes", "__Msg", "Bool", "Nil", "List", "Option",
    "Result", "Dict",
];

const AMBIENT_STD_TYPE_NAMES: &[&str] = &["Ordering", "Set", "Iter"];
const AMBIENT_TRAIT_NAMES: &[&str] = &["PartialEq", "Eq", "PartialOrd", "Ord"];

fn builtin_type_arity(name: &str) -> Option<usize> {
    if let Some(kind) = CapabilityKind::from_name(name) {
        return kind.builtin_arity();
    }
    match name {
        "List" | "Option" | "Set" | "Iter" => Some(1),
        "Result" | "Dict" => Some(2),
        "Int" | "Float" | "Duration" | "String" | "Bytes" | "__Msg" | "Bool" | "Nil"
        | "Ordering" => Some(0),
        _ => None,
    }
}

fn is_builtin_type_name(name: &str) -> bool {
    BUILTIN_TYPE_NAMES.contains(&name) || CapabilityKind::from_name(name).is_some()
}

fn is_synthetic_type_name(name: &str) -> bool {
    name.strip_prefix("__anon").is_some_and(|n| {
        !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())
    }) || anon_union_synthetic_arity(name).is_some()
        || name.strip_prefix("Tuple").is_some_and(|n| {
        !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())
    })
}

fn tuple_synthetic_arity(name: &str) -> Option<usize> {
    name.strip_prefix("Tuple")
        .and_then(|n| (!n.is_empty()).then_some(n))
        .and_then(|n| n.parse::<usize>().ok())
}

fn parse_fixed_width_usize(s: &str, pos: &mut usize, width: usize) -> Option<usize> {
    let end = pos.checked_add(width)?;
    let part = s.get(*pos..end)?;
    if !part.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    *pos = end;
    part.parse().ok()
}

fn anon_union_synthetic_arity(name: &str) -> Option<usize> {
    anon_union_synthetic_variants(name).map(|variants| {
        variants
            .into_iter()
            .map(|(_, arity)| arity)
            .sum()
    })
}

pub fn anon_union_synthetic_variants(name: &str) -> Option<Vec<(String, usize)>> {
    let mut pos = "__union".len();
    let rest = name.strip_prefix("__union")?;
    if rest.len() < 10 {
        return None;
    }
    let count = parse_fixed_width_usize(name, &mut pos, 10)?;
    let mut variants = Vec::with_capacity(count);
    for _ in 0..count {
        let len = parse_fixed_width_usize(name, &mut pos, 10)?;
        let mut bytes = Vec::with_capacity(len);
        for _ in 0..len {
            let byte = parse_fixed_width_usize(name, &mut pos, 3)?;
            if byte > u8::MAX as usize {
                return None;
            }
            bytes.push(byte as u8);
        }
        let tag = String::from_utf8(bytes).ok()?;
        let arity = parse_fixed_width_usize(name, &mut pos, 10)?;
        variants.push((tag, arity));
    }
    (pos == name.len()).then_some(variants)
}

/// Validate that every named type in `t` is known — a builtin, a declared type,
/// or a lowercase generic parameter — so a typo like `fn f(x: Flarb)` is a clear
/// "unknown type" error rather than an opaque type that mis-unifies later.
/// Whether a declared type carries a `frozen` qualifier at the top (`frozen T`,
/// possibly under other stacked qualifiers) — used to enforce that a `frozen`
/// (deeply immutable) value is never declared mutable (RFC-0025).
fn is_frozen_type(t: &ast::Type) -> bool {
    match t {
        ast::Type::Qualified(ast::TypeQual::Frozen, _) => true,
        ast::Type::Qualified(_, inner) => is_frozen_type(inner),
        _ => false,
    }
}

/// Whether a declared type carries a `local unique` qualifier at the top — used to
/// reject it in escaping positions (a return type), per RFC-0026.
fn is_local_unique_type(t: &ast::Type) -> bool {
    match t {
        ast::Type::Qualified(ast::TypeQual::LocalUnique, _) => true,
        ast::Type::Qualified(_, inner) => is_local_unique_type(inner),
        _ => false,
    }
}

pub(crate) fn type_param_names<'a>(fields: impl Iterator<Item = &'a ast::Type>) -> Vec<String> {
    ast::effective_type_params(&[], fields)
}

/// Effective type parameters for an ADT, preserving explicit order and then
/// appending implicit lowercase parameters in first field-occurrence order.
/// Every stage that substitutes ADT fields must use this authority.
pub fn type_def_params(t: &ast::TypeDef) -> Vec<String> {
    ast::effective_type_def_params(t)
}

fn type_def_arity(t: &ast::TypeDef) -> usize {
    ast::effective_nominal_type_def_params(t).len()
}

fn lifetime_argument_name(t: &ast::Type) -> Option<&str> {
    let ast::Type::Named(name, arguments) = t else { return None };
    arguments.is_empty().then(|| name.strip_prefix('\'')).flatten()
}

fn collect_parameter_lifetime_binders(t: &ast::Type, lifetimes: &mut HashSet<String>) {
    match t {
        ast::Type::Qualified(ast::TypeQual::Borrow(lifetime), inner) => {
            lifetimes.insert(lifetime.clone());
            collect_parameter_lifetime_binders(inner, lifetimes);
        }
        ast::Type::Qualified(_, inner) => collect_parameter_lifetime_binders(inner, lifetimes),
        ast::Type::Named(_, arguments) | ast::Type::Tuple(arguments) | ast::Type::Dyn(_, arguments) => {
            for argument in arguments {
                collect_parameter_lifetime_binders(argument, lifetimes);
            }
        }
        ast::Type::RecordCompose { base, fields } => {
            collect_parameter_lifetime_binders(base, lifetimes);
            for (_, field) in fields {
                collect_parameter_lifetime_binders(field, lifetimes);
            }
        }
        // A nested function type binds and validates its own relations; those
        // names do not enter the enclosing callable's lifetime scope.
        ast::Type::Fn(_, _, _) => {}
    }
}

fn validate_nominal_lifetime_uses(
    t: &ast::Type,
    lifetimes: &HashSet<String>,
    context: &str,
    borrowed_qualifiers_must_be_bound: bool,
) -> Result<(), TypeError> {
    match t {
        ast::Type::Qualified(ast::TypeQual::Borrow(lifetime), inner) => {
            if borrowed_qualifiers_must_be_bound && !lifetimes.contains(lifetime) {
                return terr(format!(
                    "{context} uses lifetime `'{lifetime}` but does not declare it; add \
                     `'{lifetime}` to the nominal type parameters"
                ));
            }
            validate_nominal_lifetime_uses(
                inner,
                lifetimes,
                context,
                borrowed_qualifiers_must_be_bound,
            )
        }
        ast::Type::Qualified(_, inner) => validate_nominal_lifetime_uses(
            inner,
            lifetimes,
            context,
            borrowed_qualifiers_must_be_bound,
        ),
        ast::Type::Named(_, arguments) | ast::Type::Tuple(arguments) | ast::Type::Dyn(_, arguments) => {
            for argument in arguments {
                if let Some(lifetime) = lifetime_argument_name(argument) {
                    if !lifetimes.contains(lifetime) {
                        return terr(format!(
                            "{context} uses lifetime argument `'{lifetime}`, but no parameter \
                             binds that lifetime"
                        ));
                    }
                } else {
                    validate_nominal_lifetime_uses(
                        argument,
                        lifetimes,
                        context,
                        borrowed_qualifiers_must_be_bound,
                    )?;
                }
            }
            Ok(())
        }
        ast::Type::RecordCompose { base, fields } => {
            validate_nominal_lifetime_uses(
                base,
                lifetimes,
                context,
                borrowed_qualifiers_must_be_bound,
            )?;
            for (_, field) in fields {
                validate_nominal_lifetime_uses(
                    field,
                    lifetimes,
                    context,
                    borrowed_qualifiers_must_be_bound,
                )?;
            }
            Ok(())
        }
        ast::Type::Fn(parameters, result, _) => {
            let mut nested = HashSet::new();
            for parameter in parameters {
                collect_parameter_lifetime_binders(parameter, &mut nested);
            }
            for parameter in parameters {
                validate_nominal_lifetime_uses(parameter, &nested, context, false)?;
            }
            validate_nominal_lifetime_uses(result, &nested, context, false)
        }
    }
}

fn collect_declared_lifetime_uses(t: &ast::Type, used: &mut HashSet<String>) {
    match t {
        ast::Type::Qualified(ast::TypeQual::Borrow(lifetime), inner) => {
            used.insert(lifetime.clone());
            collect_declared_lifetime_uses(inner, used);
        }
        ast::Type::Qualified(_, inner) => collect_declared_lifetime_uses(inner, used),
        ast::Type::Named(_, arguments) | ast::Type::Tuple(arguments) | ast::Type::Dyn(_, arguments) => {
            for argument in arguments {
                if let Some(lifetime) = lifetime_argument_name(argument) {
                    used.insert(lifetime.to_string());
                } else {
                    collect_declared_lifetime_uses(argument, used);
                }
            }
        }
        ast::Type::RecordCompose { base, fields } => {
            collect_declared_lifetime_uses(base, used);
            for (_, field) in fields {
                collect_declared_lifetime_uses(field, used);
            }
        }
        ast::Type::Fn(parameters, result, _) => {
            for parameter in parameters {
                collect_declared_lifetime_uses(parameter, used);
            }
            collect_declared_lifetime_uses(result, used);
        }
    }
}

fn validate_callable_nominal_lifetimes(
    name: &str,
    parameters: &[ast::Param],
    result: Option<&ast::Type>,
) -> Result<(), TypeError> {
    let mut lifetimes = HashSet::new();
    for parameter in parameters {
        if let Some(ty) = &parameter.ty {
            collect_parameter_lifetime_binders(ty, &mut lifetimes);
        }
    }
    let context = format!("callable `{}`", name.rsplit('.').next().unwrap_or(name));
    for parameter in parameters {
        if let Some(ty) = &parameter.ty {
            validate_nominal_lifetime_uses(ty, &lifetimes, &context, false)?;
        }
    }
    if let Some(result) = result {
        validate_nominal_lifetime_uses(result, &lifetimes, &context, false)?;
    }
    Ok(())
}

/// RFC-0112 stage 1: validate lifetime-bearing nominal declarations and their
/// signature relations before any runtime representation work. Lifetimes are
/// compile-time-only and remain restricted to fixed single-variant shells.
fn check_nominal_lifetime_declarations(module: &Module) -> Result<(), TypeError> {
    let opt = module.modes.iter().any(|mode| mode == "opt");
    for item in &module.items {
        match item {
            Item::Type(definition) => {
                let declared = definition
                    .params
                    .iter()
                    .filter_map(|parameter| ast::lifetime_param_name(parameter))
                    .map(str::to_string)
                    .collect::<HashSet<_>>();
                if !declared.is_empty() && !opt && module.linked_entry.is_none() {
                    return terr(format!(
                        "type `{}` declares lifetime parameters, which are only available in a \
                         `mode opt` module; add `mode opt` at the top of the file",
                        definition.name.rsplit('.').next().unwrap_or(&definition.name)
                    ));
                }
                if !declared.is_empty() && definition.variants.len() != 1 {
                    return terr(format!(
                        "borrowed nominal type `{}` has {} variants; RFC-0112's initial scope \
                         supports named-field records and single-variant positional types only",
                        definition.name.rsplit('.').next().unwrap_or(&definition.name),
                        definition.variants.len()
                    ));
                }
                let context = format!(
                    "type `{}`",
                    definition.name.rsplit('.').next().unwrap_or(&definition.name)
                );
                let mut used = HashSet::new();
                for variant in &definition.variants {
                    for field in &variant.fields {
                        validate_nominal_lifetime_uses(field, &declared, &context, true)?;
                        collect_declared_lifetime_uses(field, &mut used);
                    }
                }
                for lifetime in &declared {
                    if !used.contains(lifetime) {
                        return terr(format!(
                            "{context} declares lifetime parameter `'{lifetime}` but no field uses \
                             it; remove the parameter or relate a borrowed field to it"
                        ));
                    }
                }
            }
            Item::Function(function) => validate_callable_nominal_lifetimes(
                &function.name,
                &function.params,
                function.ret.as_ref(),
            )?,
            Item::Trait(definition) => {
                for method in &definition.methods {
                    validate_callable_nominal_lifetimes(
                        &method.name,
                        &method.params,
                        method.ret.as_ref(),
                    )?;
                }
            }
            Item::Impl(definition) => {
                for method in &definition.methods {
                    validate_callable_nominal_lifetimes(
                        &method.name,
                        &method.params,
                        method.ret.as_ref(),
                    )?;
                }
            }
            Item::TypeAlias { name, ty, .. } => {
                validate_nominal_lifetime_uses(
                    ty,
                    &HashSet::new(),
                    &format!("type alias `{}`", name.rsplit('.').next().unwrap_or(name)),
                    true,
                )?;
            }
            Item::Const { .. } | Item::Comptime(_) => {}
        }
    }
    Ok(())
}

fn validate_type(
    t: &ast::Type,
    known: &HashSet<&str>,
    arities: &HashMap<&str, usize>,
    nominal_parameters: &HashMap<&str, Vec<String>>,
) -> Result<(), TypeError> {
    match t {
        ast::Type::Qualified(_, inner) => {
            validate_type(inner, known, arities, nominal_parameters)
        }
        // (RFC-0081) The dyn head is a trait name validated by its own pass;
        // only the type arguments are ordinary types.
        ast::Type::Dyn(_, args) => {
            args.iter()
                .try_for_each(|a| validate_type(a, known, arities, nominal_parameters))
        }
        ast::Type::Tuple(ts) => ts
            .iter()
            .try_for_each(|x| validate_type(x, known, arities, nominal_parameters)),
        ast::Type::RecordCompose { base, fields } => {
            validate_type(base, known, arities, nominal_parameters)?;
            fields
                .iter()
                .try_for_each(|(_, ty)| validate_type(ty, known, arities, nominal_parameters))
        }
        ast::Type::Fn(params, ret, _) => {
            params
                .iter()
                .try_for_each(|p| validate_type(p, known, arities, nominal_parameters))?;
            validate_type(ret, known, arities, nominal_parameters)
        }
        ast::Type::Named(n, args) => {
            if let Some(lifetime) = lifetime_argument_name(t) {
                return terr(format!(
                    "lifetime `'{lifetime}` may only appear as an argument for a declared \
                     lifetime parameter"
                ));
            }
            // `Dir`/`File`/`Net` carry capability *rights* markers (`Dir[Read]`,
            // `Net[Connect]`) in their arguments, not types. Validate the marker
            // vocabulary here (BUG-154) so a typo (`Dir[Reed]`, `Net[Conect]`) or
            // a rejected right (`Net[Tls]` — TLS is an endpoint scheme, not a Net
            // right; RFC-0009) is a clear error instead of a silently-normalized
            // capability whose authority shape no longer matches the source.
            if matches!(n.as_str(), "Console" | "Dir" | "File" | "Net") {
                return validate_cap_markers(n, args);
            }
            if known.contains(n.as_str()) || is_synthetic_type_name(n) {
                if let Some(expected) = arities
                    .get(n.as_str())
                    .copied()
                    .or_else(|| tuple_synthetic_arity(n))
                    .or_else(|| anon_union_synthetic_arity(n))
                {
                    let got = args.len();
                    if got != expected {
                        return terr(format!(
                            "type `{n}` expects {expected} type argument(s) but got {got}"
                        ));
                    }
                }
                if let Some(parameters) = nominal_parameters.get(n.as_str()) {
                    for (index, (argument, parameter)) in
                        args.iter().zip(parameters).enumerate()
                    {
                        let expected_lifetime = ast::is_lifetime_param(parameter);
                        let supplied_lifetime = lifetime_argument_name(argument);
                        match (expected_lifetime, supplied_lifetime) {
                            (true, None) => {
                                return terr(format!(
                                    "type `{n}` expects a lifetime argument for parameter \
                                     `{parameter}` at position {}, but got ordinary type `{}`",
                                    index + 1,
                                    witchy_syntax::format::type_str(argument)
                                ));
                            }
                            (false, Some(lifetime)) => {
                                return terr(format!(
                                    "lifetime argument `'{lifetime}` cannot be used for ordinary \
                                     type parameter `{parameter}` of `{n}`"
                                ));
                            }
                            (true, Some(_)) => {}
                            (false, None) => {
                                validate_type(argument, known, arities, nominal_parameters)?;
                            }
                        }
                    }
                    Ok(())
                } else {
                    for (index, argument) in args.iter().enumerate() {
                        if let Some(lifetime) = lifetime_argument_name(argument) {
                            return terr(format!(
                                "lifetime argument `'{lifetime}` cannot be used in ordinary type \
                                 position {} of `{n}`",
                                index + 1
                            ));
                        }
                        validate_type(argument, known, arities, nominal_parameters)?;
                    }
                    Ok(())
                }
            } else if args.is_empty() && n.chars().next().is_some_and(|c| c.is_lowercase()) && !n.contains('.') {
                // A lowercase, argument-less name is a generic type parameter.
                Ok(())
            } else {
                terr(format!("unknown type `{n}`"))
            }
        }
    }
}

fn is_builtin_authority_type_name(name: &str) -> bool {
    matches!(
        name,
        "Console"
            | "Clock"
            | "Rand"
            | "Env"
            | "Secret"
            | "SecretStore"
            | "Dir"
            | "File"
            | "Net"
            | "Fetch"
            | "Exec"
            | "Socket"
            | "Listener"
            | "BuildOut"
            | "BuildRead"
            | "BuildEnv"
            | "BuildNet"
            | "BuildExec"
    )
}

fn is_anon_record_synthetic_name(name: &str) -> bool {
    name.strip_prefix("__anon")
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

fn structural_type_kind(name: &str) -> Option<&'static str> {
    if is_anon_record_synthetic_name(name) {
        Some("record")
    } else if anon_union_synthetic_variants(name).is_some() {
        Some("union")
    } else {
        None
    }
}

fn authority_taint_type(
    ty: &ast::Type,
    defs: &HashMap<&str, &ast::TypeDef>,
    seen: &mut HashSet<String>,
) -> Option<String> {
    match ty {
        ast::Type::Qualified(_, inner) => authority_taint_type(inner, defs, seen),
        // (RFC-0081) A dyn value is never itself a capability; only its type
        // arguments can carry taint.
        ast::Type::Dyn(_, args) => args.iter().find_map(|a| authority_taint_type(a, defs, seen)),
        ast::Type::Tuple(items) => items.iter().find_map(|t| authority_taint_type(t, defs, seen)),
        ast::Type::Fn(params, ret, _) => params
            .iter()
            .chain(std::iter::once(ret.as_ref()))
            .find_map(|t| authority_taint_type(t, defs, seen)),
        ast::Type::RecordCompose { base, fields } => std::iter::once(base.as_ref())
            .chain(fields.iter().map(|(_, ty)| ty))
            .find_map(|ty| authority_taint_type(ty, defs, seen)),
        ast::Type::Named(name, args) => {
            if is_builtin_authority_type_name(name) {
                return Some(name.clone());
            }
            if let Some(hit) = args.iter().find_map(|a| authority_taint_type(a, defs, seen)) {
                return Some(hit);
            }
            let def = defs.get(name.as_str())?;
            if def.is_capability {
                return Some(name.clone());
            }
            if !seen.insert(name.clone()) {
                return None;
            }
            let hit = def
                .variants
                .iter()
                .flat_map(|v| v.fields.iter())
                .find_map(|field| authority_taint_type(field, defs, seen));
            seen.remove(name);
            hit
        }
    }
}

fn reject_structural_authority_type(
    ty: &ast::Type,
    defs: &HashMap<&str, &ast::TypeDef>,
) -> Result<(), TypeError> {
    match ty {
        ast::Type::Qualified(_, inner) => reject_structural_authority_type(inner, defs),
        ast::Type::Dyn(_, args) => {
            args.iter().try_for_each(|arg| reject_structural_authority_type(arg, defs))
        }
        ast::Type::Tuple(items) => {
            items.iter().try_for_each(|item| reject_structural_authority_type(item, defs))
        }
        ast::Type::Fn(params, ret, _) => {
            params.iter().try_for_each(|param| reject_structural_authority_type(param, defs))?;
            reject_structural_authority_type(ret, defs)
        }
        ast::Type::RecordCompose { base, fields } => {
            let components = || {
                std::iter::once(base.as_ref()).chain(fields.iter().map(|(_, ty)| ty))
            };
            if let Some(cap) = components()
                .find_map(|ty| authority_taint_type(ty, defs, &mut HashSet::new()))
            {
                return terr(format!(
                    "anonymous record types cannot contain capability `{cap}` — structural values \
                     cannot carry authority; name a capability type or pass the capability directly"
                ));
            }
            components().try_for_each(|ty| reject_structural_authority_type(ty, defs))
        }
        ast::Type::Named(name, args) => {
            if let Some(kind) = structural_type_kind(name) {
                if let Some(cap) =
                    args.iter().find_map(|arg| authority_taint_type(arg, defs, &mut HashSet::new()))
                {
                    return terr(format!(
                        "anonymous {kind} types cannot contain capability `{cap}` — structural values \
                         cannot carry authority; name a capability type or pass the capability directly"
                    ));
                }
            }
            args.iter().try_for_each(|arg| reject_structural_authority_type(arg, defs))
        }
    }
}

fn validate_type_model(
    t: &ast::Type,
    known: &HashSet<&str>,
    arities: &HashMap<&str, usize>,
    type_defs: &HashMap<&str, &ast::TypeDef>,
) -> Result<(), TypeError> {
    let nominal_parameters = type_defs
        .iter()
        .map(|(name, definition)| {
            (*name, ast::effective_nominal_type_def_params(definition))
        })
        .collect();
    validate_type(t, known, arities, &nominal_parameters)?;
    reject_structural_authority_type(t, type_defs)
}

/// Reject references to undeclared types in function signatures. The
/// set of known names is the builtins plus every `type` declared in the
/// module; lowercase argument-less names are generic parameters.
fn check_type_names(module: &Module) -> Result<(), TypeError> {
    let mut known: HashSet<&str> = BUILTIN_TYPE_NAMES.iter().copied().collect();
    known.extend(CapabilityKind::ALL.iter().map(|kind| kind.name()));
    let mut arities: HashMap<&str, usize> = BUILTIN_TYPE_NAMES
        .iter()
        .chain(AMBIENT_STD_TYPE_NAMES.iter())
        .copied()
        .chain(CapabilityKind::ALL.iter().map(|kind| kind.name()))
        .filter_map(|name| builtin_type_arity(name).map(|arity| (name, arity)))
        .collect();
    let mut packed_names: HashSet<&str> = HashSet::new();
    // (RFC-0005) User `type`/`capability` declarations used by the stage-specific
    // representation checks. The transitive reference fact itself comes from
    // `ReferenceStorageClassifier` below.
    let mut type_defs: HashMap<&str, &ast::TypeDef> = HashMap::new();
    for item in &module.items {
        if let Item::Type(t) = item {
            known.insert(t.name.as_str());
            arities.insert(t.name.as_str(), type_def_arity(t));
            type_defs.insert(t.name.as_str(), t);
            if t.packed {
                packed_names.insert(t.name.as_str());
            }
        }
    }
    let reference_storage = ReferenceStorageClassifier::new(module);
    for item in &module.items {
        let in_ctx = |e: TypeError, ctx: &str| TypeError {
            message: format!("in `{}`: {}", ctx.rsplit('.').next().unwrap_or(ctx), e.message),
        };
        fn validate_block_types(
            block: &Block,
            known: &HashSet<&str>,
            arities: &HashMap<&str, usize>,
            type_defs: &HashMap<&str, &ast::TypeDef>,
            ctx: &str,
            in_ctx: &impl Fn(TypeError, &str) -> TypeError,
        ) -> Result<(), TypeError> {
            if let Some(region) = &block.region {
                if let Some(ty) = &region.ty {
                    validate_type_model(ty, known, arities, type_defs)
                        .map_err(|e| in_ctx(e, ctx))?;
                }
            }
            for stmt in &block.stmts {
                validate_stmt_types(stmt, known, arities, type_defs, ctx, in_ctx)?;
            }
            Ok(())
        }
        fn validate_stmt_types(
            stmt: &Stmt,
            known: &HashSet<&str>,
            arities: &HashMap<&str, usize>,
            type_defs: &HashMap<&str, &ast::TypeDef>,
            ctx: &str,
            in_ctx: &impl Fn(TypeError, &str) -> TypeError,
        ) -> Result<(), TypeError> {
            match stmt {
                Stmt::Let { ty, value, .. } => {
                    if let Some(ty) = ty {
                        validate_type_model(ty, known, arities, type_defs)
                            .map_err(|e| in_ctx(e, ctx))?;
                    }
                    validate_expr_types(value, known, arities, type_defs, ctx, in_ctx)
                }
                Stmt::Assign { value, .. }
                | Stmt::LetPattern { value, .. }
                | Stmt::Return(Some(value))
                | Stmt::Yield(value)
                | Stmt::Expr(value) => validate_expr_types(value, known, arities, type_defs, ctx, in_ctx),
                Stmt::Return(None) | Stmt::Break | Stmt::Continue => Ok(()),
            }
        }
        fn validate_expr_types(
            expr: &Expr,
            known: &HashSet<&str>,
            arities: &HashMap<&str, usize>,
            type_defs: &HashMap<&str, &ast::TypeDef>,
            ctx: &str,
            in_ctx: &impl Fn(TypeError, &str) -> TypeError,
        ) -> Result<(), TypeError> {
            match expr {
                Expr::Int(_)
                | Expr::Float(_)
                | Expr::Duration(_)
                | Expr::Str(_)
                | Expr::Bool(_)
                | Expr::Var(_)
                | Expr::TaggedLit { .. } => Ok(()),
                Expr::List(values) | Expr::Tuple(values) => {
                    for value in values {
                        validate_expr_types(value, known, arities, type_defs, ctx, in_ctx)?;
                    }
                    Ok(())
                }
                Expr::Call { args, .. } | Expr::Ctor { args, .. }
                | Expr::AnonCtor { args, .. } => {
                    for arg in args {
                        validate_expr_types(arg, known, arities, type_defs, ctx, in_ctx)?;
                    }
                    Ok(())
                }
                Expr::LabeledCall { args, .. } => {
                    for (_, arg) in args {
                        validate_expr_types(arg, known, arities, type_defs, ctx, in_ctx)?;
                    }
                    Ok(())
                }
                Expr::MethodCall { receiver, args, .. } => {
                    validate_expr_types(receiver, known, arities, type_defs, ctx, in_ctx)?;
                    for arg in args {
                        validate_expr_types(arg, known, arities, type_defs, ctx, in_ctx)?;
                    }
                    Ok(())
                }
                Expr::ExistentialCall {
                    receiver,
                    args,
                    ty,
                    result,
                    ..
                } => {
                    validate_expr_types(receiver, known, arities, type_defs, ctx, in_ctx)?;
                    for arg in args {
                        validate_expr_types(arg, known, arities, type_defs, ctx, in_ctx)?;
                    }
                    validate_type_model(ty, known, arities, type_defs)
                        .map_err(|e| in_ctx(e, ctx))?;
                    validate_type_model(result, known, arities, type_defs)
                        .map_err(|e| in_ctx(e, ctx))
                }
                Expr::Apply { func, args } => {
                    validate_expr_types(func, known, arities, type_defs, ctx, in_ctx)?;
                    for arg in args {
                        validate_expr_types(arg, known, arities, type_defs, ctx, in_ctx)?;
                    }
                    Ok(())
                }
                Expr::Unary { expr, .. }
                | Expr::Field { base: expr, .. }
                | Expr::Try(expr) => validate_expr_types(expr, known, arities, type_defs, ctx, in_ctx),
                Expr::As { expr, ty } => {
                    validate_expr_types(expr, known, arities, type_defs, ctx, in_ctx)?;
                    validate_type_model(ty, known, arities, type_defs).map_err(|e| in_ctx(e, ctx))
                }
                Expr::ExistentialPack { expr, ty, .. } => {
                    validate_expr_types(expr, known, arities, type_defs, ctx, in_ctx)?;
                    validate_type_model(ty, known, arities, type_defs).map_err(|e| in_ctx(e, ctx))
                }
                Expr::ExistentialUpcast { expr, ty } => {
                    validate_expr_types(expr, known, arities, type_defs, ctx, in_ctx)?;
                    validate_type_model(ty, known, arities, type_defs).map_err(|e| in_ctx(e, ctx))
                }
                Expr::Lambda { params, body, ret } => {
                    for param in params {
                        if let Some(ty) = &param.ty {
                            validate_type_model(ty, known, arities, type_defs)
                                .map_err(|e| in_ctx(e, ctx))?;
                        }
                    }
                    if let Some(ret) = ret {
                        validate_type_model(ret, known, arities, type_defs)
                            .map_err(|e| in_ctx(e, ctx))?;
                    }
                    validate_block_types(body, known, arities, type_defs, ctx, in_ctx)
                }
                Expr::RecordUpdate { name: _, base, fields } => {
                    validate_expr_types(base, known, arities, type_defs, ctx, in_ctx)?;
                    for (_, value) in fields {
                        validate_expr_types(value, known, arities, type_defs, ctx, in_ctx)?;
                    }
                    Ok(())
                }
                Expr::Record { fields, spread, .. } => {
                    for (_, value) in fields {
                        validate_expr_types(value, known, arities, type_defs, ctx, in_ctx)?;
                    }
                    if let Some(base) = spread {
                        validate_expr_types(base, known, arities, type_defs, ctx, in_ctx)?;
                    }
                    Ok(())
                }
                Expr::Binary { lhs, rhs, .. } | Expr::Range { lo: lhs, hi: rhs, .. } => {
                    validate_expr_types(lhs, known, arities, type_defs, ctx, in_ctx)?;
                    validate_expr_types(rhs, known, arities, type_defs, ctx, in_ctx)
                }
                Expr::If { cond, then_block, else_block } => {
                    validate_expr_types(cond, known, arities, type_defs, ctx, in_ctx)?;
                    validate_block_types(then_block, known, arities, type_defs, ctx, in_ctx)?;
                    if let Some(block) = else_block {
                        validate_block_types(block, known, arities, type_defs, ctx, in_ctx)?;
                    }
                    Ok(())
                }
                Expr::Match { scrutinee, arms } => {
                    validate_expr_types(scrutinee, known, arities, type_defs, ctx, in_ctx)?;
                    for arm in arms {
                        if let Some(guard) = &arm.guard {
                            validate_expr_types(guard, known, arities, type_defs, ctx, in_ctx)?;
                        }
                        validate_expr_types(&arm.body, known, arities, type_defs, ctx, in_ctx)?;
                    }
                    Ok(())
                }
                Expr::Block(block) => validate_block_types(block, known, arities, type_defs, ctx, in_ctx),
                Expr::While { cond, body } => {
                    validate_expr_types(cond, known, arities, type_defs, ctx, in_ctx)?;
                    validate_block_types(body, known, arities, type_defs, ctx, in_ctx)
                }
                Expr::For { iter, body, .. } => {
                    validate_expr_types(iter, known, arities, type_defs, ctx, in_ctx)?;
                    validate_block_types(body, known, arities, type_defs, ctx, in_ctx)
                }
                Expr::Index { base, index } => {
                    validate_expr_types(base, known, arities, type_defs, ctx, in_ctx)?;
                    validate_expr_types(index, known, arities, type_defs, ctx, in_ctx)
                }
                Expr::WhileLet { scrutinee, body, .. } => {
                    validate_expr_types(scrutinee, known, arities, type_defs, ctx, in_ctx)?;
                    validate_block_types(body, known, arities, type_defs, ctx, in_ctx)
                }
            }
        }
        match item {
            Item::Function(f) => {
                for p in &f.params {
                    if let Some(t) = &p.ty {
                        validate_type_model(t, &known, &arities, &type_defs)
                            .map_err(|e| in_ctx(e, &f.name))?;
                        reject_packed_list_boundary(t, &packed_names, &f.name, "a parameter")?;
                        reject_cap_slot_boundary(
                            t,
                            &type_defs,
                            &reference_storage,
                            &f.name,
                            "a parameter",
                        )?;
                    }
                }
                if let Some(t) = &f.ret {
                    validate_type_model(t, &known, &arities, &type_defs)
                        .map_err(|e| in_ctx(e, &f.name))?;
                    reject_packed_list_boundary(t, &packed_names, &f.name, "a return type")?;
                    reject_cap_slot_boundary(
                        t,
                        &type_defs,
                        &reference_storage,
                        &f.name,
                        "a return type",
                    )?;
                    // (RFC-0026) `local unique` is valid only WITHIN the call — it may
                    // not escape — so it cannot be a return type (a return escapes).
                    if is_local_unique_type(t) {
                        return Err(TypeError {
                            message: format!(
                                "`{}`: a `local unique` value cannot escape, so it cannot be a return type — use `unique` (returnable) or drop `local`",
                                f.name
                            ),
                        });
                    }
                }
                validate_block_types(&f.body, &known, &arities, &type_defs, &f.name, &in_ctx)?;
            }
            Item::Trait(tr) => {
                let mut trait_known = known.clone();
                let mut trait_arities = arities.clone();
                trait_known.insert("Self");
                trait_arities.insert("Self", 0);
                for method in &tr.methods {
                    for param in &method.params {
                        if let Some(ty) = &param.ty {
                            validate_type_model(ty, &trait_known, &trait_arities, &type_defs)
                                .map_err(|e| in_ctx(e, &tr.name))?;
                            reject_packed_list_boundary(
                                ty,
                                &packed_names,
                                &method.name,
                                "a parameter",
                            )?;
                            reject_cap_slot_boundary(
                                ty,
                                &type_defs,
                                &reference_storage,
                                &method.name,
                                "a parameter",
                            )?;
                        }
                    }
                    if let Some(ret) = &method.ret {
                        validate_type_model(ret, &trait_known, &trait_arities, &type_defs)
                            .map_err(|e| in_ctx(e, &tr.name))?;
                        reject_packed_list_boundary(
                            ret,
                            &packed_names,
                            &method.name,
                            "a return type",
                        )?;
                        reject_cap_slot_boundary(
                            ret,
                            &type_defs,
                            &reference_storage,
                            &method.name,
                            "a return type",
                        )?;
                    }
                    if let Some(default) = &method.default {
                        validate_block_types(default, &trait_known, &trait_arities, &type_defs, &tr.name, &in_ctx)?;
                    }
                }
            }
            Item::Impl(im) => {
                let target = ast::Type::Named(im.type_name.clone(), im.target_args.clone());
                if let Some(kind) = structural_type_kind(&im.type_name)
                    && !is_compiler_generated_structural_impl(im)
                {
                    return terr(format!(
                        "anonymous {kind} type `{}` cannot be an impl target; structural types \
                         cannot carry user behavior; define a nominal `type Name:` and implement \
                         the trait or methods for that",
                        witchy_syntax::format::type_str(&target)
                    ));
                }
                validate_type_model(&target, &known, &arities, &type_defs)
                    .map_err(|e| in_ctx(e, &im.type_name))?;
                for arg in &im.trait_args {
                    validate_type_model(arg, &known, &arities, &type_defs)
                        .map_err(|e| in_ctx(e, &im.type_name))?;
                }
                for (_, _, trait_args) in &im.bounds {
                    for arg in trait_args {
                        validate_type_model(arg, &known, &arities, &type_defs)
                            .map_err(|e| in_ctx(e, &im.type_name))?;
                    }
                }
                let mut method_known = known.clone();
                let mut method_arities = arities.clone();
                method_known.insert("Self");
                method_arities.insert("Self", 0);
                for method in &im.methods {
                    for param in &method.params {
                        if let Some(ty) = &param.ty {
                            validate_type_model(ty, &method_known, &method_arities, &type_defs)
                                .map_err(|e| in_ctx(e, &method.name))?;
                            reject_packed_list_boundary(
                                ty,
                                &packed_names,
                                &method.name,
                                "a parameter",
                            )?;
                            reject_cap_slot_boundary(
                                ty,
                                &type_defs,
                                &reference_storage,
                                &method.name,
                                "a parameter",
                            )?;
                        }
                    }
                    if let Some(ret) = &method.ret {
                        validate_type_model(ret, &method_known, &method_arities, &type_defs)
                            .map_err(|e| in_ctx(e, &method.name))?;
                        reject_packed_list_boundary(
                            ret,
                            &packed_names,
                            &method.name,
                            "a return type",
                        )?;
                        reject_cap_slot_boundary(
                            ret,
                            &type_defs,
                            &reference_storage,
                            &method.name,
                            "a return type",
                        )?;
                    }
                    for (_, _, trait_args) in &method.bounds {
                        for arg in trait_args {
                            validate_type_model(arg, &method_known, &method_arities, &type_defs)
                                .map_err(|e| in_ctx(e, &method.name))?;
                        }
                    }
                    validate_block_types(&method.body, &method_known, &method_arities, &type_defs, &method.name, &in_ctx)?;
                }
            }
            // A type's variant field types must also be known. The type's own
            // name (and any sibling type) is already in `known`, so recursive and
            // mutually-recursive types check out; lowercase fields are its params.
            Item::Type(t) => {
                for variant in &t.variants {
                    for field in &variant.fields {
                        validate_type_model(field, &known, &arities, &type_defs)
                            .map_err(|e| in_ctx(e, &t.name))?;
                        reject_packed_list_boundary(field, &packed_names, &t.name, "a field")?;
                        reject_cap_slot_boundary(
                            field,
                            &type_defs,
                            &reference_storage,
                            &t.name,
                            "a field",
                        )?;
                    }
                }
                // (RFC-0027) A `packed` type's every field must be packable — a
                // scalar (Int/Float/Bool/Duration) or another `packed` type — so the
                // inline layout has a fixed, statically-known size. A variable-size
                // field (String, List, a non-packed record, …) is a check-time error
                // naming the offending field.
                if t.packed {
                    for variant in &t.variants {
                        for (i, field) in variant.fields.iter().enumerate() {
                            if !is_packable_type(field, &packed_names) {
                                let fname = variant
                                    .field_names
                                    .get(i)
                                    .map(String::as_str)
                                    .unwrap_or("(positional field)");
                                return Err(TypeError {
                                    message: format!(
                                        "`packed` type `{}` has a non-packable field `{}`: \
                                         a packed type's fields must be scalars \
                                         (Int/Float/Bool/Duration) or other `packed` types",
                                        t.name, fname
                                    ),
                                });
                            }
                        }
                    }
                }
            }
            Item::Const { value, .. } => {
                validate_expr_types(value, &known, &arities, &type_defs, "<const>", &in_ctx)?;
            }
            Item::Comptime(block) => {
                validate_block_types(block, &known, &arities, &type_defs, "comptime", &in_ctx)?;
            }
            Item::TypeAlias { ty, name, .. } => {
                validate_type_model(ty, &known, &arities, &type_defs)
                    .map_err(|e| in_ctx(e, name))?;
            }
        }
    }
    Ok(())
}

/// Reject trait names that do not resolve to a declared trait before trait/impl
/// lowering turns those names into dispatch strings. A misspelled trait in a
/// bound or impl head should not become an inert contract.
fn check_trait_names(module: &Module) -> Result<(), TypeError> {
    fn bare(name: &str) -> &str {
        name.rsplit('.').next().unwrap_or(name)
    }

    let mut arities: HashMap<&str, usize> = HashMap::new();
    arities.extend(AMBIENT_TRAIT_NAMES.iter().copied().map(|name| (name, 0)));
    for item in &module.items {
        if let Item::Trait(tr) = item {
            arities.insert(tr.name.as_str(), tr.typarams.len());
        }
    }

    let validate_trait_use =
        |trait_name: &str, arg_count: usize, context: String| -> Result<(), TypeError> {
            match arities.get(trait_name).copied() {
                Some(expected) if expected == arg_count => Ok(()),
                Some(expected) => terr(format!(
                    "trait `{}` expects {expected} type argument(s) but got {arg_count} in {context}",
                    bare(trait_name)
                )),
                None => terr(format!("unknown trait `{}` in {context}", bare(trait_name))),
            }
        };

    for item in &module.items {
        match item {
            Item::Trait(tr) => {
                for supertrait in &tr.supertraits {
                    validate_trait_use(
                        supertrait,
                        0,
                        format!("trait `{}` supertrait list", bare(&tr.name)),
                    )?;
                }
            }
            Item::Impl(im) => {
                if let Some(trait_name) = &im.trait_name {
                    validate_trait_use(
                        trait_name,
                        im.trait_args.len(),
                        format!("impl head for `{}`", bare(&im.type_name)),
                    )?;
                }
                for (_, trait_name, trait_args) in &im.bounds {
                    validate_trait_use(
                        trait_name,
                        trait_args.len(),
                        format!("impl `{}` where clause", bare(&im.type_name)),
                    )?;
                }
                for method in &im.methods {
                    for (_, trait_name, trait_args) in &method.bounds {
                        validate_trait_use(
                            trait_name,
                            trait_args.len(),
                            format!("method `{}` where clause", bare(&method.name)),
                        )?;
                    }
                }
            }
            Item::Function(f) => {
                for (var, trait_name, trait_args) in &f.bounds {
                    let bound_kind = if var.starts_with("impltrait_") {
                        "impl-trait parameter"
                    } else {
                        "where clause"
                    };
                    validate_trait_use(
                        trait_name,
                        trait_args.len(),
                        format!("{bound_kind} of function `{}`", bare(&f.name)),
                    )?;
                }
            }
            Item::Type(_) | Item::Const { .. } | Item::Comptime(_) | Item::TypeAlias { .. } => {}
        }
    }
    Ok(())
}

/// (RFC-0027) Whether `t` is a packable field type for a `packed` layout: a
/// statically-fixed-size scalar (`Int`/`Float`/`Bool`/`Duration`) or another
/// `packed` type. Variable-size types (`String`, `List`, non-packed records, sum
/// types with differing payloads) are not packable.
fn is_packable_type(t: &ast::Type, packed_names: &HashSet<&str>) -> bool {
    matches!(t, ast::Type::Named(n, args)
        if args.is_empty()
            && (matches!(n.as_str(), "Int" | "Float" | "Bool" | "Duration")
                || packed_names.contains(n.as_str())))
}

/// (RFC-0027 declared `packed`) Reject `t` if it contains a `List(P)` where `P` is
/// declared `packed`, in a boundary position (parameter / return / field). A
/// `packed` type's flat inline layout is a CONFINED LOCAL buffer — it has no
/// cross-function or stored-field ABI (that is the deep host-layout change the
/// confined inference deliberately sidesteps), so a `List` of a packed type may
/// only be built and read within one function. This keeps the layout whole-program
/// consistent: every site that handles a packed list either packs it (a confined
/// local) or is a clean compile error here, never a silent boxed fall-back.
fn reject_packed_list_boundary(
    t: &ast::Type,
    packed_names: &HashSet<&str>,
    ctx: &str,
    position: &str,
) -> Result<(), TypeError> {
    if let Some(p) = packed_list_in_type(t, packed_names) {
        return Err(TypeError {
            message: format!(
                "`{ctx}`: a `List({p})` cannot appear in {position} — `{p}` is declared `packed`, so its \
                 list is a confined local buffer with no cross-function or stored layout. Build and read a \
                 `packed` list within one function, or drop `packed` from `{p}` to use the uniform boxed layout"
            ),
        });
    }
    Ok(())
}

/// The name of a declared-`packed` type `P` if `t` contains a `List(P)` anywhere —
/// directly, nested in another list/tuple/function type, or under an ownership
/// qualifier (`frozen`/`unique`). `None` if no packed-typed list appears.
fn packed_list_in_type(t: &ast::Type, packed_names: &HashSet<&str>) -> Option<String> {
    match t {
        ast::Type::Named(n, args) => {
            if n == "List" {
                if let Some(ast::Type::Named(en, eargs)) = args.first().map(ast::Type::unqualified) {
                    if eargs.is_empty() && packed_names.contains(en.as_str()) {
                        return Some(en.clone());
                    }
                }
            }
            args.iter().find_map(|a| packed_list_in_type(a, packed_names))
        }
        ast::Type::Dyn(_, args) => args.iter().find_map(|a| packed_list_in_type(a, packed_names)),
        ast::Type::Tuple(items) => items.iter().find_map(|a| packed_list_in_type(a, packed_names)),
        ast::Type::Fn(args, ret, _) => args
            .iter()
            .find_map(|a| packed_list_in_type(a, packed_names))
            .or_else(|| packed_list_in_type(ret, packed_names)),
        ast::Type::RecordCompose { base, fields } => packed_list_in_type(base, packed_names)
            .or_else(|| {
                fields
                    .iter()
                    .find_map(|(_, ty)| packed_list_in_type(ty, packed_names))
            }),
        ast::Type::Qualified(_, inner) => packed_list_in_type(inner, packed_names),
    }
}

/// (RFC-0005) Names of the capabilities represented as an unforgeable `externref`
/// on the compiled backend — the caps with NO boxed i64-slot representation.
/// `Console`/`Clock`/`Rand` are zero-representation (no runtime handle), while
/// `SecretStore` is a root authority with no guest-held handle to migrate.
/// `Option(cap)` is represented as nullable externref; structural
/// containers/tuples/functions over these caps still need typed GC/closure
/// lowering and are rejected at boundaries.
fn is_externref_cap(name: &str) -> bool {
    externref_cap_name(name).is_some()
}

/// Route every inferred capability variant through the same source-level
/// externref set used by boundary checking and compiled lowering. Adding an
/// existing capability kind to that set therefore updates all three paths.
fn ty_externref_cap_name(ty: &Ty) -> Option<&'static str> {
    let name = match ty {
        Ty::Console(_) => "Console",
        Ty::Clock => "Clock",
        Ty::Rand => "Rand",
        Ty::Env => "Env",
        Ty::Secret => "Secret",
        Ty::Exec => "Exec",
        Ty::Fetch => "Fetch",
        Ty::Dir(_) => "Dir",
        Ty::File(_) => "File",
        Ty::Net(_) => "Net",
        Ty::Socket => "Socket",
        Ty::Listener => "Listener",
        Ty::BuildOut => "BuildOut",
        Ty::BuildRead => "BuildRead",
        Ty::BuildEnv => "BuildEnv",
        Ty::BuildNet => "BuildNet",
        Ty::BuildExec => "BuildExec",
        _ => return None,
    };
    externref_cap_name(name)
}

fn transparent_externref_brand_cap(
    name: &str,
    defs: &HashMap<&str, &ast::TypeDef>,
    seen: &mut HashSet<String>,
) -> Option<String> {
    if is_externref_cap(name) {
        return Some(name.to_string());
    }
    if !seen.insert(name.to_string()) {
        return None;
    }
    let out = (|| {
        let def = defs.get(name)?;
        if !def.is_capability || def.variants.len() != 1 {
            return None;
        }
        let variant = def.variants.first()?;
        if variant.name != def.name || !variant.field_names.is_empty() || variant.fields.len() != 1 {
            return None;
        }
        transparent_externref_brand_field_cap(variant.fields.first()?, defs, seen)
    })();
    seen.remove(name);
    out
}

fn transparent_externref_brand_field_cap(
    t: &ast::Type,
    defs: &HashMap<&str, &ast::TypeDef>,
    seen: &mut HashSet<String>,
) -> Option<String> {
    match t {
        ast::Type::Qualified(_, inner) => transparent_externref_brand_field_cap(inner, defs, seen),
        ast::Type::Named(n, args) if args.is_empty() => transparent_externref_brand_cap(n, defs, seen),
        ast::Type::Named(n, _) if is_externref_cap(n) => Some(n.clone()),
        // (RFC-0081) A dyn type is never a transparent externref brand.
        ast::Type::Named(_, _) | ast::Type::Dyn(_, _) | ast::Type::Tuple(_)
        | ast::Type::Fn(_, _, _) => None,
        ast::Type::RecordCompose { .. } => unreachable!(
            "compiler invariant violated: structural record composition reached externref representation selection before records::lower normalized it"
        ),
    }
}

/// (RFC-0005 stage 4) A non-generic nominal aggregate whose variants
/// transitively carry a migrated externref capability. These lower to one typed
/// wasm GC struct; a transparent one-field capability brand remains the direct
/// externref representation instead.
fn gc_reference_aggregate_supported(
    name: &str,
    defs: &HashMap<&str, &ast::TypeDef>,
    storage: &ReferenceStorageClassifier<'_>,
) -> bool {
    if transparent_externref_brand_cap(name, defs, &mut HashSet::new()).is_some() {
        return false;
    }
    let Some(def) = defs.get(name) else { return false };
    if def.variants.is_empty() {
        return false;
    }
    type_def_params(def).is_empty()
        && storage
            .first_reference(&ast::Type::Named(name.to_string(), Vec::new()))
            .is_some()
}

/// (RFC-0005 stage 4) The module's GC-lowered cap-carrying nominal aggregate
/// names. The representation-neutral reference fact comes from
/// `ReferenceStorageClassifier`; this function adds only the current lowering's
/// non-generic restriction. Typeck and codegen both consume this set, so they
/// cannot disagree about which types have a reference-safe representation.
pub fn gc_cap_aggregate_names(module: &ast::Module) -> Vec<String> {
    let storage = ReferenceStorageClassifier::new(module);
    let type_defs: HashMap<&str, &ast::TypeDef> = module
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Type(t) => Some((t.name.as_str(), t)),
            _ => None,
        })
        .collect();
    module
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Type(t)
                if gc_reference_aggregate_supported(&t.name, &type_defs, &storage) =>
            {
                Some(t.name.clone())
            }
            _ => None,
        })
        .collect()
}

/// (RFC-0005 §4.4/§7) Reject `t` when it would carry a migrated externref capability
/// through a representation the current lowering cannot preserve.
///
/// A bare capability parameter/return stays an `externref`. Closed
/// `Option`/`Result`/`List`/nominal/tuple shapes use typed GC storage. `Dict`
/// remains rejected because its key/value cells still use universal i64 slots.
fn reject_cap_slot_boundary(
    t: &ast::Type,
    _defs: &HashMap<&str, &ast::TypeDef>,
    storage: &ReferenceStorageClassifier<'_>,
    ctx: &str,
    position: &str,
) -> Result<(), TypeError> {
    match t {
        ast::Type::Qualified(_, inner) => {
            reject_cap_slot_boundary(inner, _defs, storage, ctx, position)
        }
        // (RFC-0081) A dyn value is not itself a capability slot; only its type
        // arguments need checking.
        ast::Type::Dyn(_, args) => {
            args.iter()
                .try_for_each(|a| reject_cap_slot_boundary(a, _defs, storage, ctx, position))
        }
        ast::Type::Tuple(items) => {
            items
                .iter()
                .try_for_each(|a| reject_cap_slot_boundary(a, _defs, storage, ctx, position))
        }
        ast::Type::RecordCompose { base, fields } => {
            reject_cap_slot_boundary(base, _defs, storage, ctx, position)?;
            fields.iter().try_for_each(|(_, ty)| {
                reject_cap_slot_boundary(ty, _defs, storage, ctx, position)
            })
        }
        ast::Type::Fn(args, ret, _) => {
            // Function signatures may mention capabilities: the typed closure ABI
            // preserves direct reference parameters and results. Still recurse so
            // unsupported containers nested in the signature are diagnosed at their
            // declaration site.
            args.iter()
                .try_for_each(|a| reject_cap_slot_boundary(a, _defs, storage, ctx, position))?;
            reject_cap_slot_boundary(ret, _defs, storage, ctx, position)
        }
        ast::Type::Named(n, args) => {
            // Dict still stores keys and values in universal i64 slots. Lists
            // use typed GC arrays for reference elements; Option uses null for a
            // direct reference or a typed wrapper; Result and closed nominals use
            // demand-planned GC structs.
            if n == "Dict" {
                for a in args {
                    match storage.first_reference(a) {
                        Some(ReferenceLeaf::Function) => {
                            return Err(TypeError {
                                message: format!(
                                    "`{ctx}`: `Dict` cannot store a function value in {position}; \
                                     its key/value ABI still uses i64 slots"
                                ),
                            });
                        }
                        Some(ReferenceLeaf::ExternRef(cap)) => {
                            return Err(TypeError {
                                message: format!(
                                    "`{ctx}`: a `{cap}` capability cannot be wrapped in `Dict` in {position} — \
                                     it is an unforgeable reference with no boxed representation"
                                ),
                            });
                        }
                        Some(ReferenceLeaf::Existential) => {
                            return Err(TypeError {
                                message: format!(
                                    "`{ctx}`: an owned existential cannot be wrapped in `Dict` in {position} — \
                                     its key/value ABI still uses i64 slots"
                                ),
                            });
                        }
                        None => {}
                    }
                }
            }
            // Recurse so an unsupported Dict nested in an otherwise represented
            // aggregate is still rejected at its declaration boundary.
            args.iter()
                .try_for_each(|a| reject_cap_slot_boundary(a, _defs, storage, ctx, position))
        }
    }
}

/// Whether `t` names a host capability that `main` may receive as a root
/// authority (the rights of `Dir`/`Net` don't matter here — any are grantable).
pub(crate) fn is_capability_type(t: &ast::Type) -> bool {
    matches!(t, ast::Type::Named(n, _)
        if CapabilityKind::from_name(n)
            .is_some_and(|kind| kind.class() == CapabilityClass::Host))
}

/// (RFC-0047) The kind of an un-comparable component of a type: `==`/`!=` reject
/// function, capability, and existential types at every depth (top level or
/// nested inside a container). Returns the FIRST such component found (searching
/// the shared type of an equality's operands), or `None` if the whole type is
/// comparable.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Uncomparable {
    Function,
    Capability,
    Existential,
}

// `uncomparable_kind` is a `Checker` method (it must consult the record/enum type
// tables to reach fn/capability FIELDS, not just generic arguments — BUG-302).

/// (RFC-0047) Which key/member position a `Float` occupies — used for the teaching
/// error suggesting the right escape.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FloatKeyKind {
    DictKey,
    SetMember,
}

/// Whether a resolved [`Ty`] has a concrete `Float` in a `Dict` KEY position or a
/// `Set` MEMBER position, at any depth (a `Dict(k, Dict(Float, v))` counts). Value
/// positions and list elements are fine — only the hashed/compared key set needs
/// `Eq`. `None` when there is no such Float.
fn float_key_position(t: &Ty) -> Option<FloatKeyKind> {
    match t {
        Ty::Named(n, args) if n == "Dict" => {
            // args[0] = key, args[1] = value. A Float directly as the key is the
            // reject; otherwise recurse into BOTH (a nested dict/set anywhere).
            if matches!(args.first(), Some(Ty::Float)) {
                return Some(FloatKeyKind::DictKey);
            }
            args.iter().find_map(float_key_position)
        }
        Ty::Named(n, args) if n == "Set" => {
            if matches!(args.first(), Some(Ty::Float)) {
                return Some(FloatKeyKind::SetMember);
            }
            args.iter().find_map(float_key_position)
        }
        Ty::Named(_, args) => args.iter().find_map(float_key_position),
        Ty::List(e) => float_key_position(e),
        Ty::Tuple(ts) => ts.iter().find_map(float_key_position),
        Ty::Fn(ps, r, _) => ps.iter().chain(std::iter::once(r.as_ref())).find_map(float_key_position),
        _ => None,
    }
}

/// (BUG-395) The argument index of the KEY of a generic `Dict` key operation (the
/// ones that hash/compare the key), or `None` for a non-key-op. Key operations
/// require an `Eq` key; the position lets the checker read the key's type after
/// argument unification.
fn dict_key_op_index(name: &str) -> Option<usize> {
    match name {
        "dict.insert" | "dict.update" | "dict.remove" => Some(1),
        _ => intrinsics::lookup(name).and_then(|spec| {
            spec.signature
                .trait_bounds()
                .iter()
                .find(|bound| bound.trait_name == "Eq")
                .map(|bound| bound.parameter)
        }),
    }
}

/// The first type variable appearing anywhere in `t`, if any.
fn first_type_var(t: &Ty) -> Option<u32> {
    match t {
        Ty::Var(v) => Some(*v),
        Ty::List(e) => first_type_var(e),
        Ty::Tuple(ts) => ts.iter().find_map(first_type_var),
        Ty::Named(_, args) => args.iter().find_map(first_type_var),
        Ty::Fn(ps, r, _) => ps.iter().chain(std::iter::once(r.as_ref())).find_map(first_type_var),
        _ => None,
    }
}

fn float_key_reject_message(kind: FloatKeyKind) -> String {
    match kind {
        FloatKeyKind::DictKey =>
            "`Float` is not a valid `Dict` key — keys require `Eq`, but `Float` is \
             only `PartialEq` (`NaN != NaN`, so a Float key can be unretrievable and \
             `0.1 + 0.2` is a precision trap). Use an `Int` key (scale to a fixed \
             precision) or a `String` rendering of the value"
                .to_string(),
        FloatKeyKind::SetMember =>
            "`Float` is not a valid `Set` member — members require `Eq`, but `Float` \
             is only `PartialEq` (`NaN != NaN`). Use an `Int` (scaled) or a `String` \
             rendering"
                .to_string(),
    }
}

/// The teaching error for an `==`/`!=` on an un-comparable type. `ty` is the whole
/// operand type so the message can point at the container when the offender nests.
fn equality_reject_message(kind: Uncomparable, ty: &Ty) -> String {
    // The offender is nested when the whole operand type is a container (a List,
    // Tuple, or a generic like `Option`/`Result`) rather than the fn/capability
    // itself. `SecretStore` is the one capability spelled as a `Named` type.
    let nested = matches!(ty, Ty::List(_) | Ty::Tuple(_))
        || matches!(ty, Ty::Named(n, _) if n != "SecretStore");
    let where_ = if nested { format!(" (found nested in `{ty}`)") } else { String::new() };
    match kind {
        Uncomparable::Function => format!(
            "`==` is not defined on function types{where_} — there is no meaningful \
             equality for functions (identity is not stable across compilation). \
             Compare the values functions *produce*, not the functions"
        ),
        Uncomparable::Capability => format!(
            "`==` is not defined on capability types{where_} — capabilities are \
             authority, not data; there is no meaningful equality between two \
             authorities"
        ),
        Uncomparable::Existential => format!(
            "`==` is not defined on existential type `{ty}` — `dyn` values expose \
             only their declared trait methods; payload address and witness identity \
             are not observable. Declare an existential-safe comparison method with \
             explicit domain semantics"
        ),
    }
}

/// Whether `t` is `List(String)` — the command-line-arguments parameter `main`
/// may declare.
pub fn is_args_type(t: &ast::Type) -> bool {
    matches!(t, ast::Type::Named(n, args)
        if n == "List"
            && matches!(args.as_slice(), [ast::Type::Named(s, inner)] if s == "String" && inner.is_empty()))
}

/// Validate `main`'s signature: every parameter must be a host capability or the
/// `List(String)` args parameter, since `main` is the program's root entrypoint and
/// only the host's granted authority can enter there. Catches at check time what
/// would otherwise be a runtime error when the root capabilities are minted. A
/// module without `main` is a library and passes.
fn check_main_signature(module: &Module) -> Result<(), TypeError> {
    let Some(main) = module.items.iter().find_map(|it| match it {
        Item::Function(f) if f.name == "main" => Some(f),
        _ => None,
    }) else {
        return Ok(());
    };
    // RFC-0038: a library-defined capability marked `grantable` may also enter at
    // the root (the host mints it from a `[user_caps]` grant). Bareness is enforced
    // separately by `check_grantable_caps`, so here we only recognize the name.
    let grantable: HashSet<&str> = module
        .items
        .iter()
        .filter_map(|it| match it {
            Item::Type(t) if t.grantable => Some(t.name.as_str()),
            _ => None,
        })
        .collect();
    let is_grantable_cap = |t: &ast::Type| {
        matches!(t, ast::Type::Named(n, _) if grantable.contains(n.as_str()))
    };
    for p in &main.params {
        if matches!(&p.ty, Some(t) if is_capability_type(t) || is_args_type(t) || is_grantable_cap(t)) {
            continue;
        }
        let found = match &p.ty {
            Some(t) => format!("has type `{}`", witchy_syntax::format::type_str(t)),
            None => "has no type annotation".to_string(),
        };
        return terr(format!(
            "`main` parameter `{}` {found}, but `main` may only take host capabilities \
             (Console, Clock, Env, Dir, Net, Fetch, Exec, Secret, SecretStore), a `grantable` \
             library capability, or `List(String)` for command-line args",
            p.name
        ));
    }
    // The runtime's value sink surfaces a plain `main` return — an `Int` becomes
    // the process exit code, a `Float`/`String`/… is printed — but it does NOT
    // surface a `Result` or `Option`: their wrapper is dropped, so an `Err`/`None`
    // is silently swallowed (no message, exit stays 0). That makes the Rust
    // `fn main() -> Result(...)` habit a quiet-failure trap. Reject those two
    // specifically and point at handling the outcome in `main`.
    if let Some(ast::Type::Named(n, _)) = &main.ret {
        if n == "Result" || n == "Option" {
            return terr(format!(
                "`main` returns `{}`, but a `Result`/`Option` returned from `main` is \
                 silently discarded — its `Err`/`None` never surfaces and the exit code \
                 stays 0. Handle the outcome inside `main` and return an exit code (or \
                 print it), e.g. `match r: Ok(_) -> 0; Err(e) -> ... ; 1`.",
                witchy_syntax::format::type_str(main.ret.as_ref().unwrap())
            ));
        }
    }
    // (BUG-335, spec §16.3) `main` may return only `Nil` (the default) or `Int` (the
    // process exit code); `Float` is also surfaced (both backends print it). Any
    // OTHER return type — `String`/`Bool`/`List`/a record — is a parity trap: the
    // interpreter echoes it when the program printed nothing, but the compiled run
    // wrapper wires only `print_int`/`print_float` and silently drops the rest. Reject
    // it at check time so the backends agree by construction (fail loud, never a
    // silently different answer).
    if let Some(t) = &main.ret {
        let allowed = matches!(
            t.unqualified(),
            ast::Type::Named(n, _) if n == "Nil" || n == "Int" || n == "Float"
        );
        if !allowed {
            return terr(format!(
                "`main` returns `{}`, but `main` may return only `Nil` (the default) or \
                 `Int` (the process exit code) — a `String`/`Bool`/`List`/record result is \
                 printed by the interpreter but dropped by the compiled backend, so the two \
                 diverge. Print the value inside `main` and return `Nil` or an exit code.",
                witchy_syntax::format::type_str(t)
            ));
        }
    }
    Ok(())
}

fn check_dynamic_method_declarations(module: &Module) -> Result<(), TypeError> {
    let mut registered = HashSet::new();
    for item in &module.items {
        let Item::Function(function) = item else { continue };
        if !function.attributes.iter().any(|attribute| attribute == "dynamic") {
            continue;
        }
        if !function.public {
            return terr(format!("@dynamic function `{}` must be public", function.name));
        }
        if function.comptime_only || function.is_async || function.is_gen {
            return terr(format!(
                "@dynamic function `{}` must be an ordinary runtime function",
                function.name
            ));
        }
        let signature_types = function
            .params
            .iter()
            .filter_map(|parameter| parameter.ty.as_ref())
            .chain(function.ret.iter());
        if !function.bounds.is_empty() || !type_param_names(signature_types).is_empty() {
            return terr(format!(
                "@dynamic function `{}` must have a closed non-generic signature",
                function.name
            ));
        }
        let Some(receiver) = function.params.first() else {
            return terr(format!(
                "@dynamic function `{}` requires `self` as its first parameter",
                function.name
            ));
        };
        if receiver.name != "self" || receiver.ty.is_none() {
            return terr(format!(
                "@dynamic function `{}` requires an explicitly typed `self` first parameter",
                function.name
            ));
        }
        for parameter in &function.params {
            if parameter.ty.is_none() {
                return terr(format!(
                    "@dynamic function `{}` parameter `{}` requires an explicit type",
                    function.name, parameter.name
                ));
            }
            if parameter.default.is_some() {
                return terr(format!(
                    "@dynamic function `{}` cannot use default parameters because runtime arity is exact",
                    function.name
                ));
            }
            if parameter.convention != ast::Convention::Let {
                return terr(format!(
                    "@dynamic function `{}` parameter `{}` must use the `let` convention",
                    function.name, parameter.name
                ));
            }
        }
        let receiver = receiver.ty.as_ref().expect("checked above");
        let method = function
            .name
            .rsplit('.')
            .next()
            .unwrap_or(&function.name);
        let key = (
            witchy_syntax::format::type_str(receiver),
            method.to_string(),
        );
        if !registered.insert(key.clone()) {
            return terr(format!(
                "duplicate @dynamic method `{}` for `{}`",
                key.1, key.0
            ));
        }
    }
    Ok(())
}

/// RFC-0038: a `grantable capability` may be granted to a root entrypoint, so it
/// must be **bare** — carrying zero transitive built-in host authority. Otherwise
/// granting it would be an invisible `Net`/`Dir`/`Secret` grant (and a later
/// version adding a host-cap field would silently widen root authority with no
/// signature change). Reject any grantable cap that reaches a host capability
/// through its fields, directly or through nested user types.
fn check_grantable_caps(module: &Module) -> Result<(), TypeError> {
    let types: HashMap<&str, &ast::TypeDef> = module
        .items
        .iter()
        .filter_map(|it| match it {
            Item::Type(t) => Some((t.name.as_str(), t)),
            _ => None,
        })
        .collect();
    for it in &module.items {
        let Item::Type(t) = it else { continue };
        if !t.grantable {
            continue;
        }
        let mut seen = HashSet::new();
        if let Some(host) = grantable_host_taint(t, &types, &mut seen) {
            return terr(format!(
                "`grantable capability {}` carries host capability `{host}`; a \
                 root-grantable capability must be BARE (zero transitive host \
                 authority), so it cannot disguise a built-in capability. Construct \
                 it inside the program from an explicit `{host}` root instead.",
                t.name
            ));
        }
    }
    Ok(())
}

/// The first built-in host capability reachable from a type def's fields
/// (transitively through user-type fields), or `None` if the type is bare.
fn grantable_host_taint<'a>(
    t: &'a ast::TypeDef,
    types: &HashMap<&'a str, &'a ast::TypeDef>,
    seen: &mut HashSet<&'a str>,
) -> Option<String> {
    if !seen.insert(t.name.as_str()) {
        return None; // cycle guard
    }
    for v in &t.variants {
        for fty in &v.fields {
            if let Some(h) = type_host_taint(fty, types, seen) {
                return Some(h);
            }
        }
    }
    None
}

fn type_host_taint<'a>(
    ty: &'a ast::Type,
    types: &HashMap<&'a str, &'a ast::TypeDef>,
    seen: &mut HashSet<&'a str>,
) -> Option<String> {
    match ty {
        ast::Type::Named(n, args) => {
            if is_capability_type(ty) {
                return Some(n.clone());
            }
            if let Some(inner) = types.get(n.as_str()) {
                if let Some(h) = grantable_host_taint(inner, types, seen) {
                    return Some(h);
                }
            }
            args.iter().find_map(|a| type_host_taint(a, types, seen))
        }
        ast::Type::Qualified(_, inner) => type_host_taint(inner, types, seen),
        // (RFC-0081) A dyn value is never itself a capability; scan args only.
        ast::Type::Dyn(_, args) => args.iter().find_map(|a| type_host_taint(a, types, seen)),
        ast::Type::Tuple(ts) => ts.iter().find_map(|t| type_host_taint(t, types, seen)),
        ast::Type::Fn(params, ret, _) => params
            .iter()
            .chain(std::iter::once(ret.as_ref()))
            .find_map(|t| type_host_taint(t, types, seen)),
        ast::Type::RecordCompose { base, fields } => std::iter::once(base.as_ref())
            .chain(fields.iter().map(|(_, ty)| ty))
            .find_map(|ty| type_host_taint(ty, types, seen)),
    }
}

/// (RFC-0040) A cap-gated string export — `pub fn export_*(cap, String) -> String`,
/// a browser app root — must lead with a BARE grantable capability, since the host
/// mints it at that entry (like `main`). Guard the intended-but-wrong shape (a
/// 2-param `[Named, String] -> String` export whose leading type isn't grantable)
/// with a clear error instead of silently not exporting the function.
fn check_export_signatures(module: &Module) -> Result<(), TypeError> {
    let grantable: HashSet<&str> = module
        .items
        .iter()
        .filter_map(|it| match it {
            Item::Type(t) if t.grantable => Some(t.name.as_str()),
            _ => None,
        })
        .collect();
    let is_string = |t: &Option<ast::Type>| {
        matches!(t, Some(ast::Type::Named(n, a)) if n == "String" && a.is_empty())
    };
    for it in &module.items {
        let Item::Function(f) = it else { continue };
        if !f.public {
            continue;
        }
        let unqual = f.name.rsplit('.').next().unwrap_or(&f.name);
        if !unqual.starts_with("export_") {
            continue;
        }
        if let [p0, p1] = f.params.as_slice() {
            let ret_string = matches!(&f.ret, Some(ast::Type::Named(n, _)) if n == "String");
            if is_string(&p1.ty) && ret_string {
                if let Some(ast::Type::Named(n, _)) = &p0.ty {
                    if n != "String" && !grantable.contains(n.as_str()) {
                        return terr(format!(
                            "the leading parameter of the exported entrypoint `{unqual}` is `{n}`, but a \
                             cap-gated export may lead only with a bare `grantable capability` (RFC-0040) — \
                             `{n}` is not grantable"
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

/// Validate a rune's build entrypoint. The build step is the top-level `fn build`
/// whose first parameter is `BuildOut`; it runs in a zero-ambient build sandbox,
/// so it may take *only* build-time capabilities — a runtime capability (or
/// anything else) in its signature is an error. A `build` function that does not
/// take `BuildOut` is treated as an ordinary function, not the entrypoint, so
/// existing code that happens to define `fn build(...)` is unaffected.
fn check_build_signature(module: &Module) -> Result<(), TypeError> {
    let Some(build) = build_entrypoint(module) else {
        return Ok(());
    };
    for p in &build.params {
        if matches!(&p.ty, Some(t) if is_build_capability_type(t)) {
            continue;
        }
        let found = match &p.ty {
            Some(t) => format!("has type `{}`", witchy_syntax::format::type_str(t)),
            None => "has no type annotation".to_string(),
        };
        return terr(format!(
            "`build` parameter `{}` {found}, but a build step may only take build-time \
             capabilities (BuildOut, BuildRead, BuildEnv, BuildNet, BuildExec)",
            p.name
        ));
    }
    Ok(())
}

/// Collect the type-parameter names (lowercase, argument-less) appearing in a
/// type expression, in order of first appearance. Used to infer the parameters
/// of a generic ADT from its variant field types.
/// A plain `let`-borrowed parameter may not BE the function's result: every
/// block tail and `return` expression is checked for the bare parameter
/// (through if/match/block tails). RFC-0083's explicit lifetime relation is the
/// one exception: `let x: let('a) T -> View(T, 'a)` deliberately returns a
/// checked view, and the loan checker validates that relation below.
fn borrow_escape_check(func: &Function) -> Result<(), TypeError> {
    let returned_lifetime = func.ret.as_ref().and_then(|ty| match ty {
        ast::Type::Qualified(ast::TypeQual::Borrow(lifetime), _) => Some(lifetime.as_str()),
        _ => None,
    });
    let borrowed: Vec<&str> = func
        .params
        .iter()
        .filter(|p| p.convention == Convention::Borrow)
        .filter(|p| {
            let input_lifetime = p.ty.as_ref().and_then(|ty| match ty {
                ast::Type::Qualified(ast::TypeQual::Borrow(lifetime), _) => {
                    Some(lifetime.as_str())
                }
                _ => None,
            });
            input_lifetime != returned_lifetime || returned_lifetime.is_none()
        })
        .map(|p| p.name.as_str())
        .collect();
    if borrowed.is_empty() {
        return Ok(());
    }
    fn check_result_expr(e: &Expr, borrowed: &[&str], fname: &str) -> Result<(), TypeError> {
        match e {
            Expr::Var(v) if borrowed.contains(&v.as_str()) => terr(format!(
                "in `{fname}`: the `let`-borrowed parameter `{v}` cannot be returned — a borrow must not outlive the call (drop the `let`, or take it `own`)"
            )),
            Expr::If { then_block, else_block, .. } => {
                check_result_block(then_block, borrowed, fname)?;
                if let Some(b) = else_block {
                    check_result_block(b, borrowed, fname)?;
                }
                Ok(())
            }
            Expr::Match { arms, .. } => {
                for arm in arms {
                    check_result_expr(&arm.body, borrowed, fname)?;
                }
                Ok(())
            }
            Expr::Block(b) => check_result_block(b, borrowed, fname),
            _ => Ok(()),
        }
    }
    fn check_result_block(b: &Block, borrowed: &[&str], fname: &str) -> Result<(), TypeError> {
        if let Some(Stmt::Expr(tail)) = b.stmts.last() {
            check_result_expr(tail, borrowed, fname)?;
        }
        Ok(())
    }
    // Every `return` expression, anywhere in the body.
    fn scan_returns_block(b: &Block, borrowed: &[&str], fname: &str) -> Result<(), TypeError> {
        for stmt in &b.stmts {
            match stmt {
                Stmt::Return(Some(e)) => check_result_expr(e, borrowed, fname)?,
                Stmt::Let { value, .. }
                | Stmt::Assign { value, .. }
                | Stmt::LetPattern { value, .. }
                | Stmt::Expr(value)
                | Stmt::Yield(value) => scan_returns_expr(value, borrowed, fname)?,
                Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
            }
        }
        Ok(())
    }
    fn scan_returns_expr(e: &Expr, borrowed: &[&str], fname: &str) -> Result<(), TypeError> {
        match e {
            Expr::If { cond, then_block, else_block } => {
                scan_returns_expr(cond, borrowed, fname)?;
                scan_returns_block(then_block, borrowed, fname)?;
                if let Some(b) = else_block {
                    scan_returns_block(b, borrowed, fname)?;
                }
                Ok(())
            }
            Expr::Match { scrutinee, arms } => {
                scan_returns_expr(scrutinee, borrowed, fname)?;
                for arm in arms {
                    scan_returns_expr(&arm.body, borrowed, fname)?;
                }
                Ok(())
            }
            Expr::While { cond, body } => {
                scan_returns_expr(cond, borrowed, fname)?;
                scan_returns_block(body, borrowed, fname)
            }
            Expr::For { iter, body, .. } => {
                scan_returns_expr(iter, borrowed, fname)?;
                scan_returns_block(body, borrowed, fname)
            }
            Expr::Block(b) => scan_returns_block(b, borrowed, fname),
            Expr::Binary { lhs, rhs, .. } => {
                scan_returns_expr(lhs, borrowed, fname)?;
                scan_returns_expr(rhs, borrowed, fname)
            }
            Expr::Unary { expr, .. } | Expr::Try(expr) | Expr::As { expr, .. } => {
                scan_returns_expr(expr, borrowed, fname)
            }
            Expr::Call { args, .. } | Expr::Ctor { args, .. } | Expr::List(args)
            | Expr::Tuple(args) => {
                for a in args {
                    scan_returns_expr(a, borrowed, fname)?;
                }
                Ok(())
            }
            Expr::Apply { func: f2, args } => {
                scan_returns_expr(f2, borrowed, fname)?;
                for a in args {
                    scan_returns_expr(a, borrowed, fname)?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
    check_result_block(&func.body, &borrowed, &func.name)?;
    scan_returns_block(&func.body, &borrowed, &func.name)
}

fn is_unique_capacity_result(ty: &ast::Type) -> bool {
    matches!(
        ty,
        ast::Type::Qualified(ast::TypeQual::Unique, inner)
            if matches!(inner.unqualified(), ast::Type::Named(name, _)
                if matches!(name.as_str(), "List" | "Dict"))
    )
}

/// A `unique List` / `unique Dict` result is also a compiled capacity-token
/// promise. Keep the proof surface deliberately explicit: fresh literals/new
/// storage and direct calls carrying the same result ABI. Broader local-flow
/// proofs can be added without weakening this safe default.
fn check_unique_capacity_results(module: &Module) -> Result<(), TypeError> {
    let unique_functions: HashSet<String> = module
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Function(function)
                if function.ret.as_ref().is_some_and(is_unique_capacity_result) =>
            {
                Some(function.name.clone())
            }
            _ => None,
        })
        .collect();

    fn produces_capacity(expr: &Expr, functions: &HashSet<String>) -> bool {
        match expr {
            Expr::List(_) => true,
            Expr::Call { name, args } => {
                functions.contains(name)
                    || (args.is_empty()
                        && (name == intrinsics::DICT_NEW
                            || name
                                .strip_prefix(intrinsics::DICT_NEW)
                                .is_some_and(|suffix| suffix.starts_with("__"))
                            || name.ends_with(".dict.new")))
            }
            Expr::Unary { op: UnOp::Move, expr } => produces_capacity(expr, functions),
            _ => false,
        }
    }

    fn block_produces_capacity(block: &Block, functions: &HashSet<String>) -> bool {
        matches!(block.stmts.last(), Some(Stmt::Expr(expr)) if produces_capacity(expr, functions))
    }

    fn expr_can_complete(expr: &Expr) -> bool {
        match expr {
            Expr::If { then_block, else_block: Some(else_block), .. } => {
                block_can_complete(then_block) || block_can_complete(else_block)
            }
            Expr::Match { arms, .. } if !arms.is_empty() => {
                arms.iter().any(|arm| expr_can_complete(&arm.body))
            }
            Expr::Block(block) => block_can_complete(block),
            _ => true,
        }
    }

    fn block_can_complete(block: &Block) -> bool {
        let mut reachable = true;
        for stmt in &block.stmts {
            if !reachable {
                break;
            }
            reachable = match stmt {
                Stmt::Return(_) | Stmt::Break | Stmt::Continue => false,
                Stmt::Let { value, .. }
                | Stmt::Assign { value, .. }
                | Stmt::LetPattern { value, .. }
                | Stmt::Expr(value)
                | Stmt::Yield(value) => expr_can_complete(value),
            };
        }
        reachable
    }

    fn check_returns_expr(
        expr: &Expr,
        function: &str,
        functions: &HashSet<String>,
    ) -> Result<(), TypeError> {
        match expr {
            Expr::If { cond, then_block, else_block } => {
                check_returns_expr(cond, function, functions)?;
                check_returns_block(then_block, function, functions)?;
                if let Some(block) = else_block {
                    check_returns_block(block, function, functions)?;
                }
            }
            Expr::Match { scrutinee, arms } => {
                check_returns_expr(scrutinee, function, functions)?;
                for arm in arms {
                    if let Some(guard) = &arm.guard {
                        check_returns_expr(guard, function, functions)?;
                    }
                    check_returns_expr(&arm.body, function, functions)?;
                }
            }
            Expr::Block(block) => check_returns_block(block, function, functions)?,
            Expr::While { cond, body } => {
                check_returns_expr(cond, function, functions)?;
                check_returns_block(body, function, functions)?;
            }
            Expr::WhileLet { scrutinee, body, .. } => {
                check_returns_expr(scrutinee, function, functions)?;
                check_returns_block(body, function, functions)?;
            }
            Expr::For { iter, body, .. } => {
                check_returns_expr(iter, function, functions)?;
                check_returns_block(body, function, functions)?;
            }
            Expr::Call { args, .. }
            | Expr::Ctor { args, .. }
            | Expr::AnonCtor { args, .. }
            | Expr::List(args)
            | Expr::Tuple(args) => {
                for arg in args {
                    check_returns_expr(arg, function, functions)?;
                }
            }
            Expr::LabeledCall { args, .. } => {
                for (_, arg) in args {
                    check_returns_expr(arg, function, functions)?;
                }
            }
            Expr::MethodCall { receiver, args, .. } => {
                check_returns_expr(receiver, function, functions)?;
                for arg in args {
                    check_returns_expr(arg, function, functions)?;
                }
            }
            Expr::ExistentialCall { receiver, args, .. } => {
                check_returns_expr(receiver, function, functions)?;
                for arg in args {
                    check_returns_expr(arg, function, functions)?;
                }
            }
            Expr::Apply { func, args } => {
                check_returns_expr(func, function, functions)?;
                for arg in args {
                    check_returns_expr(arg, function, functions)?;
                }
            }
            Expr::Unary { expr, .. }
            | Expr::Try(expr)
            | Expr::As { expr, .. }
            | Expr::Field { base: expr, .. } => check_returns_expr(expr, function, functions)?,
            Expr::Binary { lhs, rhs, .. }
            | Expr::Range { lo: lhs, hi: rhs, .. }
            | Expr::Index { base: lhs, index: rhs } => {
                check_returns_expr(lhs, function, functions)?;
                check_returns_expr(rhs, function, functions)?;
            }
            Expr::RecordUpdate { base, fields, .. } => {
                check_returns_expr(base, function, functions)?;
                for (_, value) in fields {
                    check_returns_expr(value, function, functions)?;
                }
            }
            Expr::Record { fields, spread, .. } => {
                for (_, value) in fields {
                    check_returns_expr(value, function, functions)?;
                }
                if let Some(spread) = spread {
                    check_returns_expr(spread, function, functions)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn check_returns_block(
        block: &Block,
        function: &str,
        functions: &HashSet<String>,
    ) -> Result<(), TypeError> {
        for stmt in &block.stmts {
            match stmt {
                Stmt::Return(Some(expr)) if !produces_capacity(expr, functions) => {
                    return terr(format!(
                        "in `{function}`: this `unique` collection return has no capacity-token proof — return fresh list/dict storage or the direct result of another `unique` collection function"
                    ));
                }
                Stmt::Return(Some(expr))
                | Stmt::Let { value: expr, .. }
                | Stmt::Assign { value: expr, .. }
                | Stmt::LetPattern { value: expr, .. }
                | Stmt::Expr(expr)
                | Stmt::Yield(expr) => check_returns_expr(expr, function, functions)?,
                Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
            }
        }
        Ok(())
    }

    for item in &module.items {
        let Item::Function(function) = item else { continue };
        if !function.ret.as_ref().is_some_and(is_unique_capacity_result) {
            continue;
        }
        check_returns_block(&function.body, &function.name, &unique_functions)?;
        if block_can_complete(&function.body)
            && !block_produces_capacity(&function.body, &unique_functions)
        {
            return terr(format!(
                "in `{}`: the tail of a `unique` collection function has no capacity-token proof — return fresh list/dict storage or the direct result of another `unique` collection function",
                function.name
            ));
        }
    }
    Ok(())
}

/// Types a region assignment may freely write through to the outer scope:
/// copied by value, never pointer-backed.
fn is_scalar_ty(t: &Ty) -> bool {
    matches!(t, Ty::Int | Ty::Float | Ty::Bool | Ty::Duration)
}

fn collect_trait_method_names(module: &Module) -> HashSet<String> {
    let mut names = HashSet::new();
    for item in &module.items {
        if let Item::Trait(tr) = item {
            for method in &tr.methods {
                let bare = method.name.rsplit('.').next().unwrap_or(&method.name);
                names.insert(bare.to_string());
            }
        }
    }
    names
}

/// A record type's layout: its type-parameter var ids (in order) and its fields
/// as `(name, type)`. Field types may mention the parameters, instantiated with
/// the value's actual type arguments on access.
type RecordInfo = (Vec<u32>, Vec<(String, Ty)>);
type CallObligations = Vec<(Ty, String)>;
type UserCallSig = (Vec<Ty>, Ty, CallObligations);
type AnonUnionVariants = Vec<(String, Vec<Ty>)>;

#[derive(Clone, Debug, PartialEq, Eq)]
enum VarPlaceStep {
    Field(String),
    IndexConst(i64),
    IndexDynamic,
}

#[derive(Clone, Debug)]
struct VarPlace {
    root: String,
    steps: Vec<VarPlaceStep>,
}

fn var_place(expr: &Expr) -> Option<VarPlace> {
    fn walk(expr: &Expr, steps: &mut Vec<VarPlaceStep>) -> Option<String> {
        match expr {
            Expr::Var(root) => Some(root.clone()),
            Expr::Field { base, field } => {
                let root = walk(base, steps)?;
                steps.push(VarPlaceStep::Field(field.clone()));
                Some(root)
            }
            Expr::Index { base, index } => {
                let root = walk(base, steps)?;
                let step = match index.as_ref() {
                    Expr::Int(value) => VarPlaceStep::IndexConst(*value),
                    _ => VarPlaceStep::IndexDynamic,
                };
                steps.push(step);
                Some(root)
            }
            Expr::Call { name, args }
                if matches!(name.as_str(), intrinsics::LIST_AT | intrinsics::DICT_AT)
                    && args.len() == 2 =>
            {
                let root = walk(&args[0], steps)?;
                let step = match &args[1] {
                    Expr::Int(value) => VarPlaceStep::IndexConst(*value),
                    _ => VarPlaceStep::IndexDynamic,
                };
                steps.push(step);
                Some(root)
            }
            _ => None,
        }
    }

    let mut steps = Vec::new();
    let root = walk(expr, &mut steps)?;
    Some(VarPlace { root, steps })
}

fn places_overlap(left: &VarPlace, right: &VarPlace) -> bool {
    if left.root != right.root {
        return false;
    }
    for (left, right) in left.steps.iter().zip(&right.steps) {
        if left == right {
            continue;
        }
        return match (left, right) {
            (VarPlaceStep::Field(a), VarPlaceStep::Field(b)) => a == b,
            (VarPlaceStep::IndexConst(a), VarPlaceStep::IndexConst(b)) => a == b,
            _ => true,
        };
    }
    true
}

struct Checker {
    /// When annotating (see `annotate`): expression identity -> inferred type,
    /// finalized against the ending substitution. Key = `&Expr as *const _`.
    type_record: Option<HashMap<usize, Ty>>,
    /// Directed concrete-to-existential coercions discovered while checking.
    /// The types remain checker types until the ending substitution is known.
    /// [`TypeTable`] preserves unresolved requests so final existential
    /// preparation rejects them loudly instead of silently omitting a pack.
    existential_pack_record: Option<HashMap<usize, (Ty, Ty)>>,
    /// Directed existential supertrait conversions. The pair is `(target,
    /// source)`; lowering converts the source witness to the target witness
    /// without exposing the concrete payload.
    existential_upcast_record: Option<HashMap<usize, (Ty, Ty)>>,
    /// Directed richer-to-poorer anonymous-record conversions discovered at
    /// explicit expected-type sites. The pair is `(target, source)` and is
    /// finalized only after the ending substitution is known.
    record_projection_record: Option<HashMap<usize, (Ty, Ty)>>,
    fn_sigs: HashMap<String, (Vec<Ty>, Ty)>,
    /// Functions selected by trait lowering as actual `From.from` impls.
    /// Generated function names are an ABI detail, not semantic evidence.
    from_conversion_fns: HashSet<String>,
    ctor_sigs: HashMap<String, (Vec<Ty>, Ty)>,
    /// Type-parameter var ids per constructor, so a generic ADT's constructors
    /// are instantiated fresh at each use (e.g. `Some(1)` vs `Some("x")`).
    ctor_typarams: HashMap<String, HashSet<u32>>,
    /// Record types: name -> (type-parameter var ids in order, fields). A field
    /// type may mention the parameters, which are instantiated with the value's
    /// actual type arguments on access.
    record_fields: HashMap<String, RecordInfo>,
    /// Sealed record capabilities (`capability X:` with named fields). Their
    /// fields are opaque: `.field` access is rejected so the only way to reach a
    /// carried capability is `match`, which the linker confines to the home
    /// module — otherwise an alias would leak the underlying authority.
    sealed_types: HashSet<String>,
    /// Every `sealed type` and `capability` record. Rebuilding one through a
    /// record update is construction, so it is legal only while checking a
    /// function in the type's defining module.
    construction_sealed_types: HashSet<String>,
    /// Single-field brands whose compiled representation is the same externref
    /// as their single underlying migrated host capability.
    transparent_externref_brands: HashMap<String, String>,
    /// Named non-generic aggregates that the compiled backend lowers to typed GC
    /// structs, so they may directly carry migrated externref capabilities.
    gc_cap_aggregates: HashSet<String>,
    adt_variants: HashMap<String, Vec<String>>,
    fn_conventions: HashMap<String, Vec<Convention>>,
    /// Source parameter names paired with [`Self::fn_conventions`]. Names are
    /// diagnostic metadata only; function-type identity remains positional.
    fn_param_names: HashMap<String, Vec<String>>,
    /// Per-function type parameters (name, var id), from lowercase type names in
    /// signatures. Generalized: instantiated fresh at each call site.
    fn_typarams: HashMap<String, Vec<(String, u32)>>,
    /// Per-function call obligations from `where` clauses, keyed to the same
    /// signature type-parameter var ids as `fn_typarams`. These are checked at a
    /// generic call site so wrapper functions cannot erase a callee's public
    /// protocol bounds.
    fn_bounds: HashMap<String, Vec<(u32, String)>>,
    /// Bare trait method names declared in the linked module. Trait methods do
    /// not have one first-class function value; value-position references need a
    /// targeted RFC-0050 diagnostic instead of the generic unbound-variable one.
    trait_method_names: HashSet<String>,
    /// The source trait graph survives trait lowering only through this checker
    /// context. It lets source checking reject an unrelated `dyn` conversion
    /// before feature staging; final existential preparation validates the same
    /// edge again before either runtime sees a compiler-owned upcast node.
    trait_supertraits: HashMap<String, Vec<String>>,
    /// (BUG-308) The type parameters (name -> var id) of the function whose body is
    /// currently being checked, so a body `let`/`var` ascription's lowercase name
    /// (`let out: List(a) = …`) resolves to the SAME type-parameter var as the
    /// signature rather than a fresh concrete `Named("a")` — which would pin the
    /// generic parameter and trip the "isn't generic" soundness check.
    current_typarams: HashMap<String, u32>,
    /// Trait bounds declared on the function currently being checked.
    current_bounds: Vec<(String, String)>,
    subst: HashMap<u32, Ty>,
    next_var: u32,
    /// Each binding carries its type and whether it is mutable.
    scopes: Vec<HashMap<String, (Ty, bool)>>,
    /// Bindings that have been consumed (moved out via an `own` parameter) and
    /// may not be used again until reassigned. Flow-sensitive within a body.
    consumed: HashSet<String>,
    /// One entry per ACTIVE `region:` block, holding the names declared
    /// inside it — an assignment to a name outside the innermost region must
    /// be scalar (a region's only pointer-escape is its value).
    region_locals: Vec<HashSet<String>>,
    /// The declared return type of the function currently being checked, so `?`
    /// can require the enclosing function to return a matching Result/Option.
    current_ret: Option<Ty>,
    /// The callback parameter of an isolated-worker stdlib reference body. The
    /// wrapper may invoke this parameter directly; the worker adapter still has
    /// its own capability-bearing callback boundary.
    current_isolated_callback: Option<String>,
    /// (BUG-395 / RFC-0047) The key types of the generic `Dict` key operations
    /// (`dict.insert`/`get_or`/`update`/`contains_key`/`remove`) invoked in the
    /// body currently being checked, with the source line. A dict key is hashed and
    /// compared, so it must be `Eq`; validated once the body is fully inferred so
    /// an unbounded generic key (a type parameter with no `where k: Eq` bound)
    /// is rejected rather than silently accepted.
    dict_key_ops: Vec<(Ty, u32)>,
    /// Source line of the statement currently being checked, attached to errors
    /// so diagnostics point at a location. 0 means "no line known".
    cur_line: u32,
    /// The module (file stem) of the function currently being checked, so a
    /// diagnostic can render a home-module type/variant with its bare name — the
    /// spelling the reader wrote — while keeping cross-module qualifiers that
    /// disambiguate (RFC-0042; BUG-292). Empty means "unknown".
    cur_module: String,
    /// The entry module's name (the home of the unqualified `main`), used as the
    /// home for a bare function whose name carries no `module.` prefix (BUG-292).
    entry_module: String,
    /// True only while checking the isolated program used to execute a
    /// `comptime:` block; ordinary runtime modules cannot traffic in compiler
    /// syntax values such as `meta.ItemSyntax`.
    compiler_syntax_allowed: bool,
}

/// Render a canonical `module.Name` for a diagnostic: strip the qualifier when it
/// names `home` (the reader wrote the bare name and the error location already
/// names the module), but keep a cross-module qualifier (it disambiguates a
/// same-named type from another module).
fn dequalify_home(name: &str, home: &str) -> String {
    match name.split_once('.') {
        Some((module, bare)) if module == home => bare.to_string(),
        _ => name.to_string(),
    }
}

/// Strip the `home.` qualifier from every canonical type/constructor name embedded
/// in a diagnostic message, leaving cross-module qualifiers intact (RFC-0042;
/// BUG-292). A canonical qualifier is `home.` at a word boundary followed by an
/// uppercase letter (`t_file.Point`) — never an incidental substring, and never a
/// lowercase-suffixed name like the `module.fn` location prefix.
fn strip_home_qualifiers(message: &str, home: &str) -> String {
    if home.is_empty() {
        return message.to_string();
    }
    let needle = format!("{home}.");
    let bytes = message.as_bytes();
    let mut out = String::with_capacity(message.len());
    let mut i = 0;
    while i < message.len() {
        let boundary = i == 0 || {
            let prev = bytes[i - 1];
            !(prev.is_ascii_alphanumeric() || prev == b'_' || prev == b'.')
        };
        if boundary
            && message[i..].starts_with(&needle)
            && message[i + needle.len()..].chars().next().is_some_and(|c| c.is_ascii_uppercase())
        {
            i += needle.len(); // drop the `home.` qualifier, keep the bare name
            continue;
        }
        let ch = message[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn diagnostic_segment(segment: &str) -> &str {
    segment.rsplit('.').next().unwrap_or(segment)
}

fn starts_lowercase(segment: &str) -> bool {
    segment
        .chars()
        .next()
        .is_some_and(|ch| ch.is_ascii_lowercase())
}

/// Render a lowered function/method symbol as a source-facing callable name.
///
/// Method and trait dispatch names use the compiler-private `__` namespace
/// (`Type__method`, `Trait__Type__method`). User source is not allowed to spell
/// those names, so diagnostics should translate them back to dotted surface
/// names rather than exposing lowering artifacts. Generic free-function
/// specializations such as `smallest__Int` are rendered as the original function.
fn diagnostic_callable_name(name: &str) -> String {
    let name = cap_ops::surface_name(name);
    if !name.contains("__") {
        return name.to_string();
    }
    let parts: Vec<&str> = name
        .split("__")
        .map(diagnostic_segment)
        .filter(|part| !part.is_empty())
        .collect();
    match parts.as_slice() {
        [] => name.to_string(),
        [only] => (*only).to_string(),
        // `fn_name__Int` is a monomorphized free function, not a method.
        [func, _] if starts_lowercase(func) => (*func).to_string(),
        [ty, method] => format!("{ty}.{method}"),
        // Prefer the concrete receiver/implementor when a lowered trait method
        // carries both the trait and type arguments: `Trait__Type__method`.
        many => {
            let owner = many[many.len() - 2];
            let method = many[many.len() - 1];
            format!("{owner}.{method}")
        }
    }
}

/// Return the callback position and diagnostic for an API whose semantics require
/// execution in an isolated worker VM. Both type checking and codegen consume this
/// contract so a new worker API cannot silently acquire a parent-VM fallback.
pub fn isolated_vm_callback_contract(
    name: &str,
    arity: usize,
) -> Option<(usize, &'static str)> {
    match (cap_ops::surface_name(name), arity) {
        ("vm.with_dir", 3) => Some((
            1,
            "`vm.with_dir` requires a bare top-level function callback; closures and local function values cannot cross the isolated worker-VM boundary",
        )),
        ("vm.serve", 3) => Some((
            2,
            "`vm.serve` requires a bare top-level function callback; closures and local function values cannot cross the isolated worker-VM boundary",
        )),
        _ => None,
    }
}

fn bare_cap_op_error(name: &str, arity: usize) -> Option<String> {
    if !cap_ops::is_op_name(name) {
        return None;
    }
    match cap_ops::diagnostic_suggestion(name, arity) {
        Some(suggestion) => Some(format!(
            "capability operation `{name}` is method-only; write `{suggestion}` instead"
        )),
        None => Some(format!(
            "capability operation `{name}` is method-only; call it as `cap.{name}(…)`"
        )),
    }
}

impl Checker {
    fn fresh(&mut self) -> Ty {
        let v = self.next_var;
        self.next_var += 1;
        Ty::Var(v)
    }

    /// The one builtin-name table (RFC-0073): resolve a named type against the
    /// language's builtins, or `None` for a user/parameter name. Shared by
    /// `to_ty` and `to_ty_generic` — which differ only in how they recurse into
    /// generic arguments (`elem`) and in their non-builtin fallback — so a new
    /// builtin type or capability is added in exactly one place.
    fn named_builtin(
        &mut self,
        name: &str,
        args: &[ast::Type],
        elem: &mut dyn FnMut(&mut Self, &ast::Type) -> Ty,
    ) -> Option<Ty> {
        Some(match name {
            "Int" => Ty::Int,
            "Float" => Ty::Float,
            "Duration" => Ty::Duration,
            "String" => Ty::String,
            "Bytes" => Ty::Bytes,
            "__Msg" => Ty::Msg,
            "Bool" => Ty::Bool,
            "Nil" => Ty::Unit,
            "Console" => Ty::Console(console_rights(args)),
            "Clock" => Ty::Clock,
            "Rand" => Ty::Rand,
            "Env" => Ty::Env,
            "Secret" => Ty::Secret,
            "Exec" => Ty::Exec,
            "Fetch" => Ty::Fetch,
            "Dir" => Ty::Dir(dir_rights(args)),
            "File" => Ty::File(file_rights(args)),
            "Net" => Ty::Net(net_rights(args)),
            "Socket" => Ty::Socket,
            "Listener" => Ty::Listener,
            "BuildOut" => Ty::BuildOut,
            "BuildRead" => Ty::BuildRead,
            "BuildEnv" => Ty::BuildEnv,
            "BuildNet" => Ty::BuildNet,
            "BuildExec" => Ty::BuildExec,
            "List" => {
                let e = match args.first() {
                    Some(a) => elem(self, a),
                    None => self.fresh(),
                };
                Ty::List(Box::new(e))
            }
            _ => return None,
        })
    }

    // `&mut self` because resolving an unknown type can mint fresh type vars via
    // `self.fresh()`, despite the `to_` name.
    #[allow(clippy::wrong_self_convention)]
    fn to_ty(&mut self, t: &ast::Type) -> Ty {
        let (name, args) = match t {
            // (RFC-0025/0026) Ownership/immutability qualifiers are compile-time
            // contracts with no runtime type — lower to the inner type.
            ast::Type::Qualified(_, inner) => return self.to_ty(inner),
            ast::Type::Named(name, args) => (name, args),
            ast::Type::Tuple(ts) => {
                if ts.is_empty() {
                    return Ty::Unit;
                }
                return Ty::Tuple(ts.iter().map(|t| self.to_ty(t)).collect());
            }
            ast::Type::Fn(params, ret, conventions) => {
                return Ty::Fn(
                    params.iter().map(|t| self.to_ty(t)).collect(),
                    Box::new(self.to_ty(ret)),
                    conventions.clone(),
                );
            }
            // (RFC-0081) A first-class existential identity: the bare trait
            // name plus its argument types (which recurse through ordinary
            // resolution, so aliases normalize to the same identity).
            ast::Type::Dyn(name, args) => {
                return Ty::Dyn(
                    name.clone(),
                    args.iter().map(|a| self.to_ty(a)).collect(),
                );
            }
            ast::Type::RecordCompose { .. } => unreachable!(
                "compiler invariant violated: structural record composition reached type conversion before records::lower normalized it"
            ),
        };
        if let Some(t) = self.named_builtin(name, args, &mut |c, a| c.to_ty(a)) {
            return t;
        }
        // (BUG-308) A lowercase, argument-less name that names one of the
        // enclosing function's type parameters is that parameter's var — so a
        // body ascription refines the generic parameter instead of pinning it
        // to a distinct concrete `Named`. (Matches the signature-only rule in
        // `to_ty_generic`; outside a generic fn `current_typarams` is empty, so
        // top-level `let` and non-parameter names are unaffected.)
        if args.is_empty()
            && name.chars().next().is_some_and(|c| c.is_lowercase())
            && !name.contains('.')
            && self.current_typarams.contains_key(name.as_str())
        {
            return Ty::Var(self.current_typarams[name.as_str()]);
        }
        Ty::Named(
            name.clone(),
            args.iter()
                .filter(|argument| lifetime_argument_name(argument).is_none())
                .map(|a| self.to_ty(a))
                .collect(),
        )
    }

    /// Like `to_ty`, but a lowercase, argument-less type name becomes a type
    /// *variable* (a parameter), shared within one signature via `vars`.
    #[allow(clippy::wrong_self_convention)]
    fn to_ty_generic(&mut self, t: &ast::Type, vars: &mut HashMap<String, Ty>) -> Ty {
        match t {
            ast::Type::Qualified(_, inner) => self.to_ty_generic(inner, vars),
            ast::Type::Tuple(ts) => {
                if ts.is_empty() {
                    return Ty::Unit;
                }
                Ty::Tuple(ts.iter().map(|t| self.to_ty_generic(t, vars)).collect())
            }
            ast::Type::Fn(params, ret, conventions) => Ty::Fn(
                params.iter().map(|t| self.to_ty_generic(t, vars)).collect(),
                Box::new(self.to_ty_generic(ret, vars)),
                conventions.clone(),
            ),
            // (RFC-0081) First-class existential identity; arguments recurse so
            // a signature's generic vars inside `dyn T(a)` stay shared (the
            // existential validation pass separately rejects unresolved args).
            ast::Type::Dyn(name, args) => Ty::Dyn(
                name.clone(),
                args.iter().map(|a| self.to_ty_generic(a, vars)).collect(),
            ),
            ast::Type::RecordCompose { .. } => unreachable!(
                "compiler invariant violated: structural record composition reached generic type conversion before records::lower normalized it"
            ),
            ast::Type::Named(name, args) => {
                if let Some(t) =
                    self.named_builtin(name, args, &mut |c, a| c.to_ty_generic(a, vars))
                {
                    return t;
                }
                if args.is_empty()
                    && name.chars().next().is_some_and(|c| c.is_lowercase())
                    && !name.contains('.')
                {
                    if let Some(v) = vars.get(name.as_str()) {
                        return v.clone();
                    }
                    let v = self.fresh();
                    vars.insert(name.clone(), v.clone());
                    return v;
                }
                Ty::Named(
                    name.clone(),
                    args.iter()
                        .filter(|argument| lifetime_argument_name(argument).is_none())
                        .map(|a| self.to_ty_generic(a, vars))
                        .collect(),
                )
            }
        }
    }

    /// Instantiate a polymorphic signature: replace its generalized type
    /// parameters with fresh vars, so each call site is independent. Other
    /// (inference) vars stay shared, keeping un-annotated functions monomorphic.
    fn instantiate(&mut self, params: &[Ty], ret: &Ty, typarams: &HashSet<u32>) -> (Vec<Ty>, Ty) {
        let fresh_map = self.fresh_typeparam_map(typarams);
        let p = params.iter().map(|t| self.subst_vars(t, &fresh_map)).collect();
        let r = self.subst_vars(ret, &fresh_map);
        (p, r)
    }

    fn instantiate_with_bounds(
        &mut self,
        params: &[Ty],
        ret: &Ty,
        typarams: &HashSet<u32>,
        bounds: &[(u32, String)],
    ) -> UserCallSig {
        let fresh_map = self.fresh_typeparam_map(typarams);
        let p = params.iter().map(|t| self.subst_vars(t, &fresh_map)).collect();
        let r = self.subst_vars(ret, &fresh_map);
        let bs = bounds
            .iter()
            .map(|(var, tr)| (self.subst_vars(&Ty::Var(*var), &fresh_map), tr.clone()))
            .collect();
        (p, r, bs)
    }

    fn fresh_typeparam_map(&mut self, typarams: &HashSet<u32>) -> HashMap<u32, Ty> {
        let mut fresh_map: HashMap<u32, Ty> = HashMap::new();
        for &v in typarams {
            // Checking the function's body may have *bound* the type-param var to
            // another (still-unbound) var — e.g. matching on the param. Key the
            // fresh substitution by that resolved representative, since
            // `subst_vars` resolves before it looks up the map; otherwise the
            // substitution would never apply and the function would behave
            // monomorphically across call sites. A param resolved to a concrete
            // type isn't generic, so skip it.
            if let Ty::Var(rv) = self.resolve(&Ty::Var(v)) {
                fresh_map.entry(rv).or_insert_with(|| self.fresh());
            }
        }
        fresh_map
    }

    fn subst_vars(&self, t: &Ty, map: &HashMap<u32, Ty>) -> Ty {
        match self.resolve(t) {
            Ty::Var(v) => map.get(&v).cloned().unwrap_or(Ty::Var(v)),
            Ty::List(e) => Ty::List(Box::new(self.subst_vars(&e, map))),
            Ty::Tuple(ts) => Ty::Tuple(ts.iter().map(|x| self.subst_vars(x, map)).collect()),
            Ty::Named(n, args) => {
                Ty::Named(n, args.iter().map(|x| self.subst_vars(x, map)).collect())
            }
            Ty::Dyn(n, args) => {
                Ty::Dyn(n, args.iter().map(|x| self.subst_vars(x, map)).collect())
            }
            Ty::Fn(params, ret, conventions) => Ty::Fn(
                params.iter().map(|x| self.subst_vars(x, map)).collect(),
                Box::new(self.subst_vars(&ret, map)),
                conventions,
            ),
            other => other,
        }
    }

    fn resolve(&self, t: &Ty) -> Ty {
        match t {
            Ty::Var(v) => match self.subst.get(v) {
                Some(bound) => self.resolve(bound),
                None => t.clone(),
            },
            Ty::List(e) => Ty::List(Box::new(self.resolve(e))),
            Ty::Tuple(ts) => Ty::Tuple(ts.iter().map(|t| self.resolve(t)).collect()),
            Ty::Named(n, args) => Ty::Named(n.clone(), args.iter().map(|t| self.resolve(t)).collect()),
            Ty::Dyn(n, args) => Ty::Dyn(n.clone(), args.iter().map(|t| self.resolve(t)).collect()),
            Ty::Fn(params, ret, conventions) => Ty::Fn(
                params.iter().map(|t| self.resolve(t)).collect(),
                Box::new(self.resolve(ret)),
                conventions.clone(),
            ),
            _ => t.clone(),
        }
    }

    fn same_resolved_type(&self, a: &Ty, b: &Ty) -> bool {
        self.resolve(a) == self.resolve(b)
    }

    fn has_from_conversion(&self, dst: &Ty, src: &Ty) -> bool {
        self.from_conversion_fns.iter().any(|name| {
            self.fn_sigs.get(name).is_some_and(|(params, ret)| {
                params.len() == 1
                    && self.same_resolved_type(ret, dst)
                    && self.same_resolved_type(&params[0], src)
            })
        })
    }

    fn result_error_compatible(&mut self, dst: &Ty, src: &Ty) -> Result<bool, TypeError> {
        if self.same_resolved_type(dst, src) || self.has_from_conversion(dst, src) {
            return Ok(true);
        }
        self.anon_union_widening_ok(dst, src)
    }

    fn anon_union_widening_ok(&mut self, dst: &Ty, src: &Ty) -> Result<bool, TypeError> {
        let (Some((_, dst_variants)), Some((_, src_variants))) = (
            self.anon_union_variants_for_ty(dst),
            self.anon_union_variants_for_ty(src),
        ) else {
            return Ok(false);
        };
        if src_variants.len() > dst_variants.len() {
            return Ok(false);
        }

        let saved_subst = self.subst.clone();
        for (src_tag, src_fields) in &src_variants {
            let Some((_, dst_fields)) = dst_variants
                .iter()
                .find(|(dst_tag, dst_fields)| {
                    dst_tag == src_tag && dst_fields.len() == src_fields.len()
                })
            else {
                self.subst = saved_subst;
                return Ok(false);
            };
            for (dst_field, src_field) in dst_fields.iter().zip(src_fields) {
                if self.unify(dst_field, src_field).is_err() {
                    self.subst = saved_subst;
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }

    /// The first migrated externref capability carried by `t`, if any. Kept in
    /// lockstep with `is_externref_cap`. This is used for expression-level shapes
    /// (not just annotated `ast::Type`s), such as closure captures inferred from
    /// local bindings.
    fn ty_carries_externref_cap(&self, t: &Ty) -> Option<&'static str> {
        fn go(c: &Checker, t: &Ty, seen: &mut HashSet<String>) -> Option<&'static str> {
            let resolved = c.resolve(t);
            if let Some(cap) = ty_externref_cap_name(&resolved) {
                return Some(cap);
            }
            match resolved {
                Ty::List(inner) => go(c, &inner, seen),
                Ty::Tuple(items) => items.iter().find_map(|i| go(c, i, seen)),
                Ty::Fn(params, ret, _) => params
                    .iter()
                    .find_map(|p| go(c, p, seen))
                    .or_else(|| go(c, &ret, seen)),
                // (RFC-0081) An existential VALUE never leaks a capability —
                // capability-carrying payloads are rejected at construction —
                // so only its type arguments need checking.
                Ty::Dyn(_, args) => args.iter().find_map(|a| go(c, a, seen)),
                Ty::Named(n, args) => {
                    if let Some(cap) = c.transparent_externref_brands.get(&n) {
                        return externref_cap_name(cap);
                    }
                    if let Some(hit) = args.iter().find_map(|a| go(c, a, seen)) {
                        return Some(hit);
                    }
                    if !seen.insert(n.clone()) {
                        return None;
                    }
                    let fields = c.record_fields.get(&n).map(|(_, fields)| fields.clone());
                    let mut hit = fields
                        .into_iter()
                        .flatten()
                        .find_map(|(_, field)| go(c, &field, seen));
                    if hit.is_none()
                        && let Some(variants) = c.adt_variants.get(&n)
                    {
                        hit = variants.iter().find_map(|variant| {
                            c.ctor_sigs.get(variant).and_then(|(payloads, _)| {
                                payloads.iter().find_map(|payload| go(c, payload, seen))
                            })
                        });
                    }
                    seen.remove(&n);
                    hit
                }
                Ty::Int | Ty::Float | Ty::Duration | Ty::String | Ty::Bytes | Ty::Msg
                | Ty::Bool | Ty::Unit | Ty::Console(_) | Ty::Clock | Ty::Rand | Ty::Env
                | Ty::Secret | Ty::Exec | Ty::Fetch | Ty::Dir(_) | Ty::File(_) | Ty::Net(_)
                | Ty::Socket | Ty::Listener | Ty::BuildOut | Ty::BuildRead | Ty::BuildEnv
                | Ty::BuildNet | Ty::BuildExec | Ty::Var(_) => None,
            }
        }
        go(self, t, &mut HashSet::new())
    }

    fn ty_carries_function_value(&self, t: &Ty) -> bool {
        fn go(c: &Checker, t: &Ty, seen: &mut HashSet<String>) -> bool {
            match c.resolve(t) {
                Ty::Fn(_, _, _) => true,
                Ty::List(inner) => go(c, &inner, seen),
                Ty::Tuple(items) | Ty::Dyn(_, items) => {
                    items.iter().any(|item| go(c, item, seen))
                }
                Ty::Named(name, args) => {
                    if args.iter().any(|arg| go(c, arg, seen)) {
                        return true;
                    }
                    if !seen.insert(name.clone()) {
                        return false;
                    }
                    let record_hit = c.record_fields.get(&name).is_some_and(|(_, fields)| {
                        fields.iter().any(|(_, field)| go(c, field, seen))
                    });
                    let variant_hit = c.adt_variants.get(&name).is_some_and(|variants| {
                        variants.iter().any(|variant| {
                            c.ctor_sigs.get(variant).is_some_and(|(payloads, _)| {
                                payloads.iter().any(|payload| go(c, payload, seen))
                            })
                        })
                    });
                    seen.remove(&name);
                    record_hit || variant_hit
                }
                Ty::Int | Ty::Float | Ty::Duration | Ty::String | Ty::Bytes | Ty::Msg
                | Ty::Bool | Ty::Unit | Ty::Console(_) | Ty::Clock | Ty::Rand | Ty::Env
                | Ty::Secret | Ty::Exec | Ty::Fetch | Ty::Dir(_) | Ty::File(_) | Ty::Net(_)
                | Ty::Socket | Ty::Listener | Ty::BuildOut | Ty::BuildRead | Ty::BuildEnv
                | Ty::BuildNet | Ty::BuildExec | Ty::Var(_) => false,
            }
        }
        go(self, t, &mut HashSet::new())
    }

    /// (RFC-0081) The first capability of ANY kind carried by `t`, transitively:
    /// the full direct capability set (every variant `ty_externref_cap_name`
    /// matches on, whether or not it has migrated to externref, plus the
    /// `SecretStore` root), recursing through lists, tuples, function types,
    /// generic arguments, record fields, and ADT variant payloads exactly like
    /// `ty_carries_externref_cap`. Used to reject capability-carrying
    /// existential payloads at directed and explicit construction sites — v1
    /// has no authority envelope, so a payload capability would hide authority
    /// behind the existential.
    fn ty_carries_capability(&self, t: &Ty) -> Option<&'static str> {
        fn direct(ty: &Ty) -> Option<&'static str> {
            Some(match ty {
                Ty::Console(_) => "Console",
                Ty::Clock => "Clock",
                Ty::Rand => "Rand",
                Ty::Env => "Env",
                Ty::Secret => "Secret",
                Ty::Exec => "Exec",
                Ty::Fetch => "Fetch",
                Ty::Dir(_) => "Dir",
                Ty::File(_) => "File",
                Ty::Net(_) => "Net",
                Ty::Socket => "Socket",
                Ty::Listener => "Listener",
                Ty::BuildOut => "BuildOut",
                Ty::BuildRead => "BuildRead",
                Ty::BuildEnv => "BuildEnv",
                Ty::BuildNet => "BuildNet",
                Ty::BuildExec => "BuildExec",
                Ty::Named(n, _) if n == "SecretStore" => "SecretStore",
                _ => return None,
            })
        }
        fn go(c: &Checker, t: &Ty, seen: &mut HashSet<String>) -> Option<&'static str> {
            let resolved = c.resolve(t);
            if let Some(cap) = direct(&resolved) {
                return Some(cap);
            }
            match resolved {
                Ty::List(inner) => go(c, &inner, seen),
                Ty::Tuple(items) => items.iter().find_map(|i| go(c, i, seen)),
                Ty::Fn(params, ret, _) => params
                    .iter()
                    .find_map(|p| go(c, p, seen))
                    .or_else(|| go(c, &ret, seen)),
                // A nested existential's payload was itself cap-checked at its
                // own construction; only its type arguments remain.
                Ty::Dyn(_, args) => args.iter().find_map(|a| go(c, a, seen)),
                Ty::Named(n, args) => {
                    if let Some(cap) = c.transparent_externref_brands.get(&n) {
                        return externref_cap_name(cap);
                    }
                    if let Some(hit) = args.iter().find_map(|a| go(c, a, seen)) {
                        return Some(hit);
                    }
                    if !seen.insert(n.clone()) {
                        return None;
                    }
                    let fields = c.record_fields.get(&n).map(|(_, fields)| fields.clone());
                    let mut hit = fields
                        .into_iter()
                        .flatten()
                        .find_map(|(_, field)| go(c, &field, seen));
                    if hit.is_none()
                        && let Some(variants) = c.adt_variants.get(&n)
                    {
                        hit = variants.iter().find_map(|variant| {
                            c.ctor_sigs.get(variant).and_then(|(payloads, _)| {
                                payloads.iter().find_map(|payload| go(c, payload, seen))
                            })
                        });
                    }
                    seen.remove(&n);
                    hit
                }
                Ty::Int | Ty::Float | Ty::Duration | Ty::String | Ty::Bytes | Ty::Msg
                | Ty::Bool | Ty::Unit | Ty::Var(_) => None,
                // Direct capabilities were handled by `direct` above.
                Ty::Console(_) | Ty::Clock | Ty::Rand | Ty::Env | Ty::Secret | Ty::Exec | Ty::Fetch
                | Ty::Dir(_) | Ty::File(_) | Ty::Net(_) | Ty::Socket | Ty::Listener
                | Ty::BuildOut | Ty::BuildRead | Ty::BuildEnv | Ty::BuildNet
                | Ty::BuildExec => None,
            }
        }
        go(self, t, &mut HashSet::new())
    }

    /// The same authority closure as `ty_carries_capability`, with the nominal
    /// field/constructor edge that retains the capability. Generic declaration
    /// fields are instantiated before traversal so diagnostics identify the
    /// source shape rather than an otherwise opaque type argument.
    fn ty_capability_retention(&self, t: &Ty) -> Option<(&'static str, Vec<String>)> {
        fn direct(ty: &Ty) -> Option<&'static str> {
            Some(match ty {
                Ty::Console(_) => "Console",
                Ty::Clock => "Clock",
                Ty::Rand => "Rand",
                Ty::Env => "Env",
                Ty::Secret => "Secret",
                Ty::Exec => "Exec",
                Ty::Fetch => "Fetch",
                Ty::Dir(_) => "Dir",
                Ty::File(_) => "File",
                Ty::Net(_) => "Net",
                Ty::Socket => "Socket",
                Ty::Listener => "Listener",
                Ty::BuildOut => "BuildOut",
                Ty::BuildRead => "BuildRead",
                Ty::BuildEnv => "BuildEnv",
                Ty::BuildNet => "BuildNet",
                Ty::BuildExec => "BuildExec",
                Ty::Named(name, _) if name == "SecretStore" => "SecretStore",
                _ => return None,
            })
        }
        fn go(
            checker: &Checker,
            ty: &Ty,
            visiting: &mut Vec<(String, Vec<Ty>)>,
            path: &[String],
        ) -> Option<(&'static str, Vec<String>)> {
            let resolved = checker.resolve(ty);
            if let Some(capability) = direct(&resolved) {
                return Some((capability, path.to_vec()));
            }
            match resolved {
                Ty::List(item) => {
                    let mut child = path.to_vec();
                    child.push("list item".into());
                    go(checker, &item, visiting, &child)
                }
                Ty::Tuple(items) => items.iter().enumerate().find_map(|(index, item)| {
                    let mut child = path.to_vec();
                    child.push(format!("tuple[{index}]"));
                    go(checker, item, visiting, &child)
                }),
                Ty::Fn(params, result, _) => params
                    .iter()
                    .enumerate()
                    .find_map(|(index, param)| {
                        let mut child = path.to_vec();
                        child.push(format!("parameter[{index}]"));
                        go(checker, param, visiting, &child)
                    })
                    .or_else(|| {
                        let mut child = path.to_vec();
                        child.push("result".into());
                        go(checker, &result, visiting, &child)
                    }),
                Ty::Dyn(name, arguments) => arguments.iter().enumerate().find_map(|(index, arg)| {
                    let mut child = path.to_vec();
                    child.push(format!("{name}<arg {index}>"));
                    go(checker, arg, visiting, &child)
                }),
                Ty::Named(name, arguments) => {
                    if let Some(capability) = checker.transparent_externref_brands.get(&name) {
                        return externref_cap_name(capability)
                            .map(|capability| (capability, path.to_vec()));
                    }
                    let key = (name.clone(), arguments.clone());
                    if visiting.contains(&key) {
                        return None;
                    }
                    visiting.push(key);
                    let bare = name.rsplit('.').next().unwrap_or(&name);
                    if let Some((parameters, fields)) = checker.record_fields.get(&name) {
                        let substitution = parameters
                            .iter()
                            .copied()
                            .zip(arguments.iter().cloned())
                            .collect::<HashMap<_, _>>();
                        for (field_name, field_type) in fields {
                            let mut child = path.to_vec();
                            child.push(format!("{bare}.{field_name}"));
                            let field_type = checker.subst_vars(field_type, &substitution);
                            if let Some(found) = go(checker, &field_type, visiting, &child) {
                                visiting.pop();
                                return Some(found);
                            }
                        }
                    }
                    if let Some(variants) = checker.adt_variants.get(&name) {
                        for variant in variants {
                            let Some((payloads, result)) = checker.ctor_sigs.get(variant) else {
                                continue;
                            };
                            let result_arguments = match checker.resolve(result) {
                                Ty::Named(_, result_arguments) => result_arguments,
                                _ => Vec::new(),
                            };
                            let substitution = result_arguments
                                .iter()
                                .zip(&arguments)
                                .filter_map(|(parameter, argument)| match parameter {
                                    Ty::Var(id) => Some((*id, argument.clone())),
                                    _ => None,
                                })
                                .collect::<HashMap<_, _>>();
                            let variant = variant.rsplit('.').next().unwrap_or(variant);
                            for (index, payload) in payloads.iter().enumerate() {
                                let mut child = path.to_vec();
                                child.push(format!("{variant}[{index}]"));
                                let payload = checker.subst_vars(payload, &substitution);
                                if let Some(found) = go(checker, &payload, visiting, &child) {
                                    visiting.pop();
                                    return Some(found);
                                }
                            }
                        }
                    }
                    visiting.pop();
                    None
                }
                Ty::Int | Ty::Float | Ty::Duration | Ty::String | Ty::Bytes | Ty::Msg
                | Ty::Bool | Ty::Unit | Ty::Var(_) => None,
                Ty::Console(_) | Ty::Clock | Ty::Rand | Ty::Env | Ty::Secret | Ty::Exec | Ty::Fetch
                | Ty::Dir(_) | Ty::File(_) | Ty::Net(_) | Ty::Socket | Ty::Listener
                | Ty::BuildOut | Ty::BuildRead | Ty::BuildEnv | Ty::BuildNet
                | Ty::BuildExec => None,
            }
        }
        let found = go(self, t, &mut Vec::new(), &[]);
        debug_assert_eq!(
            found.as_ref().map(|(capability, _)| *capability),
            self.ty_carries_capability(t),
        );
        found
    }

    fn ty_is_direct_externref_value(&self, t: &Ty) -> bool {
        let resolved = self.resolve(t);
        if ty_externref_cap_name(&resolved).is_some() {
            return true;
        }
        match resolved {
            Ty::Named(n, args) => args.is_empty() && self.transparent_externref_brands.contains_key(&n),
            _ => false,
        }
    }

    /// The first authority type carried by `t`, independent of the current
    /// RFC-0005 representation stage. This is for RFC-0078's structural-tier
    /// firewall: anonymous records/unions are ordinary data and may not carry
    /// capabilities, even when a capability is still i32-backed today.
    fn ty_authority_taint(&self, t: &Ty, seen: &mut HashSet<String>) -> Option<String> {
        match self.resolve(t) {
            Ty::Console(_) => Some("Console".to_string()),
            Ty::Clock => Some("Clock".to_string()),
            Ty::Rand => Some("Rand".to_string()),
            Ty::Env => Some("Env".to_string()),
            Ty::Secret => Some("Secret".to_string()),
            Ty::Exec => Some("Exec".to_string()),
            Ty::Fetch => Some("Fetch".to_string()),
            Ty::Dir(_) => Some("Dir".to_string()),
            Ty::File(_) => Some("File".to_string()),
            Ty::Net(_) => Some("Net".to_string()),
            Ty::Socket => Some("Socket".to_string()),
            Ty::Listener => Some("Listener".to_string()),
            Ty::BuildOut => Some("BuildOut".to_string()),
            Ty::BuildRead => Some("BuildRead".to_string()),
            Ty::BuildEnv => Some("BuildEnv".to_string()),
            Ty::BuildNet => Some("BuildNet".to_string()),
            Ty::BuildExec => Some("BuildExec".to_string()),
            Ty::List(inner) => self.ty_authority_taint(&inner, seen),
            Ty::Tuple(items) => items.iter().find_map(|item| self.ty_authority_taint(item, seen)),
            Ty::Fn(params, ret, _) => params
                .iter()
                .chain(std::iter::once(ret.as_ref()))
                .find_map(|item| self.ty_authority_taint(item, seen)),
            // (RFC-0081) An existential value never carries authority (payload
            // capabilities are rejected at construction); check its args only.
            Ty::Dyn(_, args) => args.iter().find_map(|arg| self.ty_authority_taint(arg, seen)),
            Ty::Named(name, args) => {
                if name == "SecretStore" || self.sealed_types.contains(&name) {
                    return Some(name);
                }
                if let Some(hit) = args.iter().find_map(|arg| self.ty_authority_taint(arg, seen)) {
                    return Some(hit);
                }
                if !seen.insert(name.clone()) {
                    return None;
                }
                let record_hit = self.record_fields.get(&name).and_then(|(_, fields)| {
                    fields.iter().find_map(|(_, field)| self.ty_authority_taint(field, seen))
                });
                let hit = record_hit.or_else(|| {
                    self.adt_variants.get(&name).and_then(|variants| {
                        variants.iter().find_map(|variant| {
                            self.ctor_sigs.get(variant).and_then(|(payloads, _)| {
                                payloads.iter().find_map(|payload| self.ty_authority_taint(payload, seen))
                            })
                        })
                    })
                });
                seen.remove(&name);
                hit
            }
            Ty::Int | Ty::Float | Ty::Duration | Ty::String | Ty::Bytes | Ty::Msg
            | Ty::Bool | Ty::Unit | Ty::Var(_) => None,
        }
    }

    fn reject_structural_authority_ty(&self, t: &Ty, ctx: &str) -> Result<(), TypeError> {
        match self.resolve(t) {
            Ty::List(inner) => self.reject_structural_authority_ty(&inner, ctx),
            Ty::Dyn(_, args) => {
                args.iter().try_for_each(|arg| self.reject_structural_authority_ty(arg, ctx))
            }
            Ty::Tuple(items) => {
                items.iter().try_for_each(|item| self.reject_structural_authority_ty(item, ctx))
            }
            Ty::Fn(params, ret, _) => {
                params.iter().try_for_each(|param| self.reject_structural_authority_ty(param, ctx))?;
                self.reject_structural_authority_ty(&ret, ctx)
            }
            Ty::Named(name, args) => {
                if let Some(kind) = structural_type_kind(&name) {
                    if let Some(cap) =
                        args.iter().find_map(|arg| self.ty_authority_taint(arg, &mut HashSet::new()))
                    {
                        return terr(format!(
                            "`{ctx}` builds an anonymous {kind} containing capability `{cap}`; \
                             structural values cannot carry authority — name a capability type \
                             or pass the capability directly"
                        ));
                    }
                }
                args.iter().try_for_each(|arg| self.reject_structural_authority_ty(arg, ctx))
            }
            Ty::Int | Ty::Float | Ty::Duration | Ty::String | Ty::Bytes | Ty::Msg
            | Ty::Bool | Ty::Unit | Ty::Console(_) | Ty::Clock | Ty::Rand | Ty::Env
            | Ty::Secret | Ty::Exec | Ty::Fetch | Ty::Dir(_) | Ty::File(_) | Ty::Net(_)
            | Ty::Socket | Ty::Listener | Ty::BuildOut | Ty::BuildRead | Ty::BuildEnv
            | Ty::BuildNet | Ty::BuildExec | Ty::Var(_) => Ok(()),
        }
    }

    fn compiler_syntax_ty(&self, t: &Ty) -> Option<&'static str> {
        match self.resolve(t) {
            Ty::List(inner) => self.compiler_syntax_ty(&inner),
            Ty::Dyn(_, args) => args.iter().find_map(|arg| self.compiler_syntax_ty(arg)),
            Ty::Tuple(items) => items.iter().find_map(|item| self.compiler_syntax_ty(item)),
            Ty::Fn(params, ret, _) => params
                .iter()
                .chain(std::iter::once(ret.as_ref()))
                .find_map(|item| self.compiler_syntax_ty(item)),
            Ty::Named(name, args) => {
                compiler_syntax_type_name(&name).or_else(|| args.iter().find_map(|arg| self.compiler_syntax_ty(arg)))
            }
            Ty::Int | Ty::Float | Ty::Duration | Ty::String | Ty::Bytes | Ty::Msg
            | Ty::Bool | Ty::Unit | Ty::Console(_) | Ty::Clock | Ty::Rand | Ty::Env
            | Ty::Secret | Ty::Exec | Ty::Fetch | Ty::Dir(_) | Ty::File(_) | Ty::Net(_)
            | Ty::Socket | Ty::Listener | Ty::BuildOut | Ty::BuildRead | Ty::BuildEnv
            | Ty::BuildNet | Ty::BuildExec | Ty::Var(_) => None,
        }
    }

    fn reject_runtime_compiler_syntax_ty(&self, t: &Ty, ctx: &str) -> Result<(), TypeError> {
        if self.compiler_syntax_allowed || compiler_syntax_allowed_module(&self.cur_module) {
            return Ok(());
        }
        if let Some(name) = self.compiler_syntax_ty(t) {
            return terr(format!(
                "`{ctx}` has compiler syntax type `{name}`, which is compile-time-only; \
                 use it only inside `comptime:`/`std/meta` helpers and pass generated items to `emit_item`"
            ));
        }
        Ok(())
    }

    fn reject_externref_cap_aggregate_ty(&self, t: &Ty, ctx: &str) -> Result<(), TypeError> {
        match self.resolve(t) {
            Ty::List(inner) => {
                self.reject_externref_cap_aggregate_ty(&inner, ctx)
            }
            Ty::Tuple(items) => {
                items
                    .iter()
                    .try_for_each(|item| self.reject_externref_cap_aggregate_ty(item, ctx))
            }
            Ty::Named(n, args)
                if n == "Option" && args.len() == 1 && self.ty_is_direct_externref_value(&args[0]) =>
            {
                Ok(())
            }
            Ty::Named(n, args) if n == "Dict" => {
                if args.iter().any(|arg| self.ty_carries_function_value(arg)) {
                    return terr(format!(
                        "`{ctx}` stores a function value in `Dict`, whose key/value ABI \
                         still uses i64 slots"
                    ));
                }
                if let Some(cap) = args.iter().find_map(|a| self.ty_carries_externref_cap(a)) {
                    return terr(format!(
                        "`{ctx}` wraps a `{cap}` capability in `Dict`; \
                         an externref capability has no boxed i64-slot representation — \
                         pass it directly"
                    ));
                }
                Ok(())
            }
            Ty::Fn(params, ret, _) => {
                params
                    .iter()
                    .try_for_each(|param| self.reject_externref_cap_aggregate_ty(param, ctx))?;
                self.reject_externref_cap_aggregate_ty(&ret, ctx)
            }
            Ty::Named(_, args) => args
                .iter()
                .try_for_each(|arg| self.reject_externref_cap_aggregate_ty(arg, ctx)),
            _ => Ok(()),
        }
    }

    fn unify(&mut self, a: &Ty, b: &Ty) -> Result<(), TypeError> {
        let a = self.resolve(a);
        let b = self.resolve(b);
        match (&a, &b) {
            (Ty::Var(x), Ty::Var(y)) if x == y => Ok(()),
            (Ty::Var(x), other) | (other, Ty::Var(x)) => {
                // Occurs check: binding x := T where x appears inside T would
                // build an infinite type (e.g. unifying `a` with `List(a)`).
                if self.occurs(*x, other) {
                    return terr(format!(
                        "cannot construct the infinite type: a type variable occurs within `{other}`"
                    ));
                }
                self.subst.insert(*x, other.clone());
                Ok(())
            }
            (Ty::List(x), Ty::List(y)) => self.unify(x, y),
            (Ty::Tuple(xs), Ty::Tuple(ys)) if xs.len() == ys.len() => {
                for (x, y) in xs.iter().zip(ys) {
                    self.unify(x, y)?;
                }
                Ok(())
            }
            (Ty::Named(x, xa), Ty::Named(y, ya)) if x == y && xa.len() == ya.len() => {
                for (p, q) in xa.iter().zip(ya) {
                    self.unify(p, q)?;
                }
                Ok(())
            }
            // (RFC-0081) Two existentials unify only on the same trait head,
            // the same arity, and pairwise-unifying arguments. Mismatched heads
            // fall through to the ordinary mismatch error: `dyn Sub` never
            // unifies with `dyn Super` in this slice (supertrait upcasts are
            // the witness slice's directed coercion, not unification).
            (Ty::Dyn(x, xa), Ty::Dyn(y, ya)) if x == y && xa.len() == ya.len() => {
                for (p, q) in xa.iter().zip(ya) {
                    self.unify(p, q)?;
                }
                Ok(())
            }
            (Ty::Fn(xp, xr, xc), Ty::Fn(yp, yr, yc))
                if xp.len() == yp.len() && xc == yc =>
            {
                for (p, q) in xp.iter().zip(yp) {
                    self.unify(p, q)?;
                }
                self.unify(xr, yr)
            }
            _ if a == b => Ok(()),
            _ => terr(format!("expected `{a}`, found `{b}`")),
        }
    }

    /// Whether type variable `x` occurs anywhere inside `t` (after resolving
    /// intermediate variables) — the standard infinite-type guard.
    fn occurs(&self, x: u32, t: &Ty) -> bool {
        match self.resolve(t) {
            Ty::Var(y) => x == y,
            Ty::List(inner) => self.occurs(x, &inner),
            Ty::Tuple(items) => items.iter().any(|i| self.occurs(x, i)),
            Ty::Named(_, args) | Ty::Dyn(_, args) => args.iter().any(|a| self.occurs(x, a)),
            Ty::Fn(params, ret, _) => {
                params.iter().any(|p| self.occurs(x, p)) || self.occurs(x, &ret)
            }
            _ => false,
        }
    }

    // --- scope helpers ---
    fn push(&mut self) {
        self.scopes.push(HashMap::new());
    }
    fn pop(&mut self) {
        self.scopes.pop();
    }
    /// The (name, type) bindings introduced in the innermost scope frame — used to
    /// compare what each or-pattern alternative binds (RFC-0052 binding-consistency).
    fn scope_bindings(&self) -> Vec<(String, Ty)> {
        self.scopes
            .last()
            .map(|f| f.iter().map(|(n, (t, _))| (n.clone(), t.clone())).collect())
            .unwrap_or_default()
    }
    fn define(&mut self, name: String, ty: Ty, mutable: bool) {
        if let Some(r) = self.region_locals.last_mut() {
            r.insert(name.clone());
        }
        self.scopes.last_mut().unwrap().insert(name, (ty, mutable));
    }
    /// Walk frames inner→outer, returning the binding at the first frame that
    /// defines the name (inner bindings shadow outer ones).
    fn resolve_binding(&self, name: &str) -> Option<&(Ty, bool)> {
        for vars in self.scopes.iter().rev() {
            if let Some(b) = vars.get(name) {
                return Some(b);
            }
        }
        None
    }
    fn lookup(&self, name: &str) -> Option<Ty> {
        self.resolve_binding(name).map(|(t, _)| t.clone())
    }
    fn is_mutable(&self, name: &str) -> Option<bool> {
        self.resolve_binding(name).map(|(_, m)| *m)
    }

    fn user_call_sig_with_bounds(&mut self, name: &str) -> Option<UserCallSig> {
        let (params, ret) = self.fn_sigs.get(name).cloned()?;
        let typarams: HashSet<u32> = self
            .fn_typarams
            .get(name)
            .into_iter()
            .flatten()
            .map(|(_, id)| *id)
            .collect();
        let bounds = self.fn_bounds.get(name).cloned().unwrap_or_default();
        Some(self.instantiate_with_bounds(&params, &ret, &typarams, &bounds))
    }

    fn unknown_call(&self, name: &str, args: &[Expr]) -> Result<Ty, TypeError> {
        // `to_string` was removed from the surface: interpolation IS the
        // rendering (it desugars to the internal render intrinsic).
        if name == "to_string" || name == "int_to_string" {
            return terr(format!(
                "`{name}` was removed — render with `\"${{x}}\"` \
                 interpolation (it works on every value), or \
                 `say(console, x)` to print a `Show` value"
            ));
        }
        // `show` is the `Show` trait method, not a free function: a bare
        // `show(x)` resolves only when x's concrete type is statically known
        // here. Point at the renderers that always work, rather than the
        // misleading `import set` near-miss (`set.show` is an unrelated
        // same-named module function).
        if name == "show" && args.len() == 1 {
            return terr(
                "could not resolve the `Show` method `show` on this \
                 value — a bare `show(x)` needs x's concrete type to be \
                 statically known. Render any value with `\"${x}\"` \
                 interpolation or `say(console, x)`, or bind x via a \
                 `for` loop or a typed parameter so dispatch resolves",
            );
        }
        // A retired global builtin: name the module-qualified spelling that
        // replaced it (the one-cut migration).
        if let Some(moved) = witchy_syntax::aliases::moved_builtin(name) {
            return terr(format!(
                "`{name}` moved to `{moved}` — pure data operations are \
                 module-qualified now (no import needed; the core modules \
                 are always available)"
            ));
        }
        // `<` `<=` `>` `>=` desugar to these trait-method calls; an unresolved one
        // means the operand's type lacks `Ord`, so name the operator and the fix
        // rather than leaking the desugar name.
        if let Some(op) = match name {
            "less" => Some("<"),
            "greater" => Some(">"),
            "less_equal" => Some("<="),
            "greater_equal" => Some(">="),
            _ => None,
        } {
            return terr(format!(
                "`{op}` is not defined for this type — it requires `Ord`; derive it \
                 with `derive(PartialEq, Eq, PartialOrd, Ord)` or implement it"
            ));
        }
        // If the name is an unimported stdlib function, point the way; otherwise
        // suggest a near-miss stdlib name (a likely typo).
        let hint = match witchy_syntax::linker::std_modules_for_function(name).as_slice() {
            [m] => format!(" — did you forget `import {m}`?"),
            many if !many.is_empty() => {
                format!(" — did you forget to import one of: {}?", many.join(", "))
            }
            _ => match witchy_syntax::linker::closest_std_function(name) {
                Some((cand, m)) => format!(" — did you mean `{cand}` (`import {m}`)?"),
                None => String::new(),
            },
        };
        terr(format!("call to unknown function `{name}`{hint}"))
    }

    fn intrinsic_call_sig(&mut self, name: &str) -> Option<(Vec<Ty>, Ty)> {
        use witchy_syntax::intrinsics::IntrinsicSignature as S;

        let spec = intrinsics::lookup(name)?;
        let signature = match spec.signature {
            S::GenericRender => {
                let a = self.fresh();
                Some((vec![a], Ty::String))
            }
            S::GenericListPush => {
                let elem = self.fresh();
                Some((
                    vec![Ty::List(Box::new(elem.clone())), elem.clone()],
                    Ty::List(Box::new(elem)),
                ))
            }
            S::CompilerQuoteItem => Some((
                vec![Ty::String, Ty::String],
                Ty::Named("meta.ItemSyntax".into(), Vec::new()),
            )),
            S::CompilerQuoteItemHoles => Some((
                vec![
                    Ty::String,
                    Ty::List(Box::new(Ty::String)),
                    Ty::List(Box::new(Ty::Named("meta.SyntaxHole".into(), Vec::new()))),
                ],
                Ty::Named("meta.ItemSyntax".into(), Vec::new()),
            )),
            S::CompilerQuoteExpr => Some((
                vec![Ty::String, Ty::String],
                Ty::Named("meta.ExprSyntax".into(), Vec::new()),
            )),
            S::CompilerQuoteExprHoles => Some((
                vec![
                    Ty::String,
                    Ty::List(Box::new(Ty::String)),
                    Ty::List(Box::new(Ty::Named("meta.SyntaxHole".into(), Vec::new()))),
                ],
                Ty::Named("meta.ExprSyntax".into(), Vec::new()),
            )),
            S::CompilerQuoteType => Some((
                vec![Ty::String, Ty::String],
                Ty::Named("meta.TypeSyntax".into(), Vec::new()),
            )),
            S::CompilerQuoteTypeHoles => Some((
                vec![
                    Ty::String,
                    Ty::List(Box::new(Ty::String)),
                    Ty::List(Box::new(Ty::Named("meta.TypeSyntax".into(), Vec::new()))),
                ],
                Ty::Named("meta.TypeSyntax".into(), Vec::new()),
            )),
            S::CompilerQuotePattern => Some((
                vec![Ty::String, Ty::String],
                Ty::Named("meta.PatternSyntax".into(), Vec::new()),
            )),
            S::CompilerQuotePatternHoles => Some((
                vec![
                    Ty::String,
                    Ty::List(Box::new(Ty::String)),
                    Ty::List(Box::new(Ty::Named("meta.PatternSyntax".into(), Vec::new()))),
                ],
                Ty::Named("meta.PatternSyntax".into(), Vec::new()),
            )),
            S::CompilerQuoteStmt => Some((
                vec![Ty::String, Ty::String],
                Ty::Named("meta.StmtSyntax".into(), Vec::new()),
            )),
            S::CompilerQuoteStmtHoles => Some((
                vec![
                    Ty::String,
                    Ty::List(Box::new(Ty::String)),
                    Ty::List(Box::new(Ty::Named("meta.SyntaxHole".into(), Vec::new()))),
                ],
                Ty::Named("meta.StmtSyntax".into(), Vec::new()),
            )),
            S::CompilerQuoteBlock => Some((
                vec![Ty::String, Ty::String],
                Ty::Named("meta.BlockSyntax".into(), Vec::new()),
            )),
            S::CompilerQuoteBlockHoles => Some((
                vec![
                    Ty::String,
                    Ty::List(Box::new(Ty::String)),
                    Ty::List(Box::new(Ty::Named("meta.SyntaxHole".into(), Vec::new()))),
                ],
                Ty::Named("meta.BlockSyntax".into(), Vec::new()),
            )),
            S::CompilerEmitItem => Some((
                vec![Ty::Named("meta.ItemSyntax".into(), Vec::new())],
                Ty::Unit,
            )),
            S::CompilerEmitExpr => Some((
                vec![Ty::Named("meta.ExprSyntax".into(), Vec::new())],
                Ty::Unit,
            )),
            S::GenericToMessage => {
                let m = self.fresh();
                Some((vec![m], Ty::Msg))
            }
            S::MessageToGeneric => {
                let m = self.fresh();
                Some((vec![Ty::Msg], m))
            }
            S::StringStringToRuntimeType => Some((
                vec![Ty::String, Ty::String],
                Ty::Named("dynamic.RuntimeType".into(), Vec::new()),
            )),
            S::GenericToRuntimeType => {
                let value = self.fresh();
                Some((
                    vec![value],
                    Ty::Named("dynamic.RuntimeType".into(), Vec::new()),
                ))
            }
            S::GenericToInt => {
                let value = self.fresh();
                Some((vec![value], Ty::Int))
            }
            S::RuntimeTypeToListRuntimeField => Some((
                vec![Ty::Named("dynamic.RuntimeType".into(), Vec::new())],
                Ty::List(Box::new(Ty::Named(
                    "dynamic.RuntimeField".into(),
                    Vec::new(),
                ))),
            )),
            S::RuntimeTypeStringToDynamicFieldStatus => Some((
                vec![
                    Ty::Named("dynamic.RuntimeType".into(), Vec::new()),
                    Ty::String,
                ],
                Ty::Named("dynamic.DynamicFieldStatus".into(), Vec::new()),
            )),
            S::RuntimeTypeToListRuntimeMethod => Some((
                vec![Ty::Named("dynamic.RuntimeType".into(), Vec::new())],
                Ty::List(Box::new(Ty::Named(
                    "dynamic.RuntimeMethod".into(),
                    Vec::new(),
                ))),
            )),
            S::DynamicStringListDynamicToResultDynamicDynamicError => {
                let dynamic = Ty::Named("dynamic.Dynamic".into(), Vec::new());
                Some((
                    vec![dynamic.clone(), Ty::String, Ty::List(Box::new(dynamic.clone()))],
                    Ty::Named(
                        "Result".into(),
                        vec![
                            dynamic,
                            Ty::Named("dynamic.DynamicError".into(), Vec::new()),
                        ],
                    ),
                ))
            }
            S::DynamicStringListDynamicGenericToResultDynamicDynamicError => {
                let dynamic = Ty::Named("dynamic.Dynamic".into(), Vec::new());
                let capabilities = self.fresh();
                Some((
                    vec![
                        dynamic.clone(),
                        Ty::String,
                        Ty::List(Box::new(dynamic.clone())),
                        capabilities,
                    ],
                    Ty::Named(
                        "Result".into(),
                        vec![
                            dynamic,
                            Ty::Named("dynamic.DynamicError".into(), Vec::new()),
                        ],
                    ),
                ))
            }
            S::DynamicRuntimeTypeToBool => Some((
                vec![
                    Ty::Named("dynamic.Dynamic".into(), Vec::new()),
                    Ty::Named("dynamic.RuntimeType".into(), Vec::new()),
                ],
                Ty::Bool,
            )),
            S::DynamicRuntimeTypeToResultDynamicDynamicError => {
                let dynamic = Ty::Named("dynamic.Dynamic".into(), Vec::new());
                Some((
                    vec![
                        dynamic.clone(),
                        Ty::Named("dynamic.RuntimeType".into(), Vec::new()),
                    ],
                    Ty::Named(
                        "Result".into(),
                        vec![
                            dynamic,
                            Ty::Named("dynamic.DynamicError".into(), Vec::new()),
                        ],
                    ),
                ))
            }
            S::DynamicToOptionGeneric => {
                let value = self.fresh();
                Some((
                    vec![Ty::Named("dynamic.Dynamic".into(), Vec::new())],
                    Ty::Named("Option".into(), vec![value]),
                ))
            }
            S::DynamicIntToOptionGeneric => {
                let value = self.fresh();
                Some((
                    vec![Ty::Named("dynamic.Dynamic".into(), Vec::new()), Ty::Int],
                    Ty::Named("Option".into(), vec![value]),
                ))
            }
            S::StringToBytes => Some((vec![Ty::String], Ty::Bytes)),
            S::ListIntToBytes => Some((vec![Ty::List(Box::new(Ty::Int))], Ty::Bytes)),
            S::BytesToString => Some((vec![Ty::Bytes], Ty::String)),
            S::BytesToInt => Some((vec![Ty::Bytes], Ty::Int)),
            S::BytesIntToInt => Some((vec![Ty::Bytes, Ty::Int], Ty::Int)),
            S::BytesBytesToBytes => Some((vec![Ty::Bytes, Ty::Bytes], Ty::Bytes)),
            S::BytesIntIntToBytes => {
                Some((vec![Ty::Bytes, Ty::Int, Ty::Int], Ty::Bytes))
            }
            S::SecretStoreStringToOptionSecret => Some((
                vec![Ty::Named("SecretStore".into(), Vec::new()), Ty::String],
                Ty::Named("Option".into(), vec![Ty::Secret]),
            )),
            S::SecretStoreStringToSecret => Some((
                vec![Ty::Named("SecretStore".into(), Vec::new()), Ty::String],
                Ty::Secret,
            )),
            S::StringToString => Some((vec![Ty::String], Ty::String)),
            S::StringStringToString => Some((vec![Ty::String, Ty::String], Ty::String)),
            S::StringToInt => Some((vec![Ty::String], Ty::Int)),
            S::StringStringToInt => Some((vec![Ty::String, Ty::String], Ty::Int)),
            S::StringStringToBool => Some((vec![Ty::String, Ty::String], Ty::Bool)),
            S::StringToListString => {
                Some((vec![Ty::String], Ty::List(Box::new(Ty::String))))
            }
            S::StringStringToListString => Some((
                vec![Ty::String, Ty::String],
                Ty::List(Box::new(Ty::String)),
            )),
            S::StringStringStringToString => {
                Some((vec![Ty::String, Ty::String, Ty::String], Ty::String))
            }
            S::StringStringStringToInt => {
                Some((vec![Ty::String, Ty::String, Ty::String], Ty::Int))
            }
            S::StringIntIntToString => {
                Some((vec![Ty::String, Ty::Int, Ty::Int], Ty::String))
            }
            S::ListStringListStringToString => Some((
                vec![
                    Ty::List(Box::new(Ty::String)),
                    Ty::List(Box::new(Ty::String)),
                ],
                Ty::String,
            )),
            S::SecretStringToString => Some((vec![Ty::Secret, Ty::String], Ty::String)),
            S::SecretToString => Some((vec![Ty::Secret], Ty::String)),
            S::IntToString => Some((vec![Ty::Int], Ty::String)),
            S::IntToFloat => Some((vec![Ty::Int], Ty::Float)),
            S::FloatToInt => Some((vec![Ty::Float], Ty::Int)),
            S::FloatToFloat => Some((vec![Ty::Float], Ty::Float)),
            S::GenericListToInt => {
                let elem = self.fresh();
                Some((vec![Ty::List(Box::new(elem))], Ty::Int))
            }
            S::GenericListIndex => {
                let elem = self.fresh();
                Some((vec![Ty::List(Box::new(elem.clone())), Ty::Int], elem))
            }
            S::GenericListSetAt => {
                let elem = self.fresh();
                let list = Ty::List(Box::new(elem.clone()));
                Some((vec![list.clone(), Ty::Int, elem], list))
            }
            S::GenericListConcat => {
                let elem = self.fresh();
                let list = Ty::List(Box::new(elem));
                Some((vec![list.clone(), list.clone()], list))
            }
            S::GenericListPopExtract => {
                let elem = self.fresh();
                Some((
                    vec![Ty::List(Box::new(elem.clone()))],
                    Ty::Named("Option".into(), vec![elem]),
                ))
            }
            S::GenericDictNew => {
                let key = self.fresh();
                let value = self.fresh();
                Some((vec![], Ty::Named("Dict".into(), vec![key, value])))
            }
            S::GenericDictInsert => {
                let key = self.fresh();
                let value = self.fresh();
                let dict = Ty::Named("Dict".into(), vec![key.clone(), value.clone()]);
                Some((vec![dict.clone(), key, value], dict))
            }
            S::GenericDictInsertExtract => {
                let key = self.fresh();
                let value = self.fresh();
                let dict = Ty::Named("Dict".into(), vec![key.clone(), value.clone()]);
                Some((
                    vec![dict, key, value.clone()],
                    Ty::Named("Option".into(), vec![value]),
                ))
            }
            S::GenericDictGetOr => {
                let key = self.fresh();
                let value = self.fresh();
                let dict = Ty::Named("Dict".into(), vec![key.clone(), value.clone()]);
                Some((vec![dict, key, value.clone()], value))
            }
            S::GenericDictIndex => {
                let key = self.fresh();
                let value = self.fresh();
                let dict = Ty::Named("Dict".into(), vec![key.clone(), value.clone()]);
                Some((vec![dict, key], value))
            }
            S::GenericDictUpdate => {
                let key = self.fresh();
                let value = self.fresh();
                let dict = Ty::Named("Dict".into(), vec![key.clone(), value.clone()]);
                let update = Ty::Fn(
                    vec![value.clone()],
                    Box::new(value.clone()),
                    vec![Convention::Let],
                );
                Some((vec![dict.clone(), key, value, update], dict))
            }
            S::GenericDictContainsKey => {
                let key = self.fresh();
                let value = self.fresh();
                let dict = Ty::Named("Dict".into(), vec![key.clone(), value]);
                Some((vec![dict, key], Ty::Bool))
            }
            S::GenericDictRemove => {
                let key = self.fresh();
                let value = self.fresh();
                let dict = Ty::Named("Dict".into(), vec![key.clone(), value]);
                Some((vec![dict.clone(), key], dict))
            }
            S::GenericDictRemoveExtract => {
                let key = self.fresh();
                let value = self.fresh();
                let dict = Ty::Named("Dict".into(), vec![key.clone(), value.clone()]);
                Some((
                    vec![dict, key],
                    Ty::Named("Option".into(), vec![value]),
                ))
            }
            S::GenericDictKeys => {
                let key = self.fresh();
                let value = self.fresh();
                let dict = Ty::Named("Dict".into(), vec![key.clone(), value]);
                Some((vec![dict], Ty::List(Box::new(key))))
            }
            S::GenericDictValues => {
                let key = self.fresh();
                let value = self.fresh();
                let dict = Ty::Named("Dict".into(), vec![key, value.clone()]);
                Some((vec![dict], Ty::List(Box::new(value))))
            }
            S::GenericDictPairs => {
                let key = self.fresh();
                let value = self.fresh();
                let dict = Ty::Named("Dict".into(), vec![key.clone(), value.clone()]);
                Some((
                    vec![dict],
                    Ty::List(Box::new(Ty::Tuple(vec![key, value]))),
                ))
            }
            S::GenericDictToInt => {
                let key = self.fresh();
                let value = self.fresh();
                Some((vec![Ty::Named("Dict".into(), vec![key, value])], Ty::Int))
            }
            // These calls are checked by their dedicated frontend rule or by
            // the linked source declaration, not by the builtin signature path.
            S::TryContext | S::DeclaredInSource => None,
        };
        debug_assert!(
            signature.as_ref().is_none_or(|(params, _)| params.len() == spec.arity),
            "intrinsic catalog arity disagrees with the type recipe for {}",
            spec.name
        );
        signature
    }

    fn cap_op_call_sig(operation: &cap_ops::CapOp) -> (Vec<Ty>, Ty) {
        use cap_ops::{ArgumentShape as A, ReceiverKind as R, ResultShape as O};

        let receiver = match operation.receiver {
            R::Console => Ty::Console(ConsoleRights::full()),
            R::Clock => Ty::Clock,
            R::Rand => Ty::Rand,
            R::Env => Ty::Env,
            R::Exec => Ty::Exec,
            R::BuildOut => Ty::BuildOut,
            R::BuildRead => Ty::BuildRead,
            R::BuildEnv => Ty::BuildEnv,
            R::BuildNet => Ty::BuildNet,
            R::BuildExec => Ty::BuildExec,
            R::File => Ty::File(FileRights::full()),
            R::Dir => Ty::Dir(DirRights::full()),
            R::Net => Ty::Net(NetRights::full()),
            R::Fetch => Ty::Fetch,
            R::Socket => Ty::Socket,
            R::Listener => Ty::Listener,
        };
        let mut parameters = Vec::with_capacity(operation.total_arity());
        parameters.push(receiver.clone());
        parameters.extend(operation.arguments.iter().map(|argument| match argument {
            A::String => Ty::String,
            A::Int => Ty::Int,
            A::Bool => Ty::Bool,
            A::Secret => Ty::Secret,
            A::ListString => Ty::List(Box::new(Ty::String)),
            A::Dir => Ty::Dir(DirRights::full()),
            A::DirPolicy => Ty::Named("DirPolicy".into(), Vec::new()),
            A::NetPolicy => Ty::Named("NetPolicy".into(), Vec::new()),
        }));
        let result = match operation.result {
            O::SameReceiver => receiver,
            O::Nil => Ty::Unit,
            O::Int => Ty::Int,
            O::String => Ty::String,
            O::Bool => Ty::Bool,
            O::ListString => Ty::List(Box::new(Ty::String)),
            O::OptionString => Ty::Named("Option".into(), vec![Ty::String]),
            O::Dir => Ty::Dir(DirRights::full()),
            O::File => Ty::File(FileRights::full()),
            O::Fetch => Ty::Fetch,
            O::Socket => Ty::Socket,
            O::OptionSocket => Ty::Named("Option".into(), vec![Ty::Socket]),
            O::Listener => Ty::Listener,
        };
        (parameters, result)
    }

    fn call_sig(&mut self, name: &str) -> Option<(Vec<Ty>, Ty)> {
        if let Some(signature) = self.intrinsic_call_sig(name) {
            return Some(signature);
        }
        if let Some(operation) = cap_ops::unique_operation(name) {
            return Some(Self::cap_op_call_sig(operation));
        }
        match name {
            // Abort with a message (the primitive behind std/testing).
            "fail" => Some((vec![Ty::String], Ty::Unit)),
            // Duration <-> Int(milliseconds) bridge for the std `duration` module.
            "int_to_duration" => Some((vec![Ty::Int], Ty::Duration)),
            "duration_to_int" => Some((vec![Ty::Duration], Ty::Int)),
            // User functions: instantiate generic type parameters fresh per call.
            _ => match self.fn_sigs.get(name).cloned() {
                Some((params, ret)) => {
                    let typarams: HashSet<u32> = self
                        .fn_typarams
                        .get(name)
                        .into_iter()
                        .flatten()
                        .map(|(_, id)| *id)
                        .collect();
                    Some(self.instantiate(&params, &ret, &typarams))
                }
                None => None,
            },
        }
    }

    /// `__try_ctx(value, msg)` — the `e ? "msg"` desugar. Generic over the operand:
    /// `Option(T)` or `Result(T, String)`, both yielding `Result(T, String)` so the
    /// enclosing `?` unwraps `T` and propagates an `Err(String)`. The message is a
    /// `String`; a `Result`'s error must already be `String` (the message is
    /// prepended to it, so it stays `String`).
    fn check_try_ctx(&mut self, name: &str, args: &[Expr]) -> Result<Option<Ty>, TypeError> {
        if name != intrinsics::TRY_CONTEXT {
            return Ok(None);
        }
        let arity = intrinsics::lookup(name)
            .expect("try-context intrinsic is cataloged")
            .arity;
        if args.len() != arity {
            return terr(format!("`? \"msg\"` expects (value, message) but got {}", args.len()));
        }
        let mty = self.infer(&args[1])?;
        self.unify(&Ty::String, &mty).map_err(|e| TypeError {
            message: format!("a `? \"msg\"` context message must be a String: {}", e.message),
        })?;
        let oty = self.infer(&args[0])?;
        let elem = self.fresh();
        let resolved = self.resolve(&oty);
        match &resolved {
            Ty::Named(n, _) if n == "Option" => {
                self.unify(&Ty::Named("Option".into(), vec![elem.clone()]), &oty).map_err(|e| {
                    TypeError { message: format!("in `? \"msg\"`: {}", e.message) }
                })?;
            }
            Ty::Named(n, _) if n == "Result" => {
                self.unify(
                    &Ty::Named("Result".into(), vec![elem.clone(), Ty::String]),
                    &oty,
                )
                .map_err(|_| TypeError {
                    message: "`? \"msg\"` prepends to a String error, so the `Result`'s error type must be `String`".to_string(),
                })?;
            }
            other => {
                return terr(format!(
                    "`? \"msg\"` applies to an `Option` or `Result`, not `{other}`"
                ));
            }
        }
        Ok(Some(Ty::Named("Result".into(), vec![elem, Ty::String])))
    }

    /// Check a capability narrowing ascription (`src as target`): the target must
    /// be the *same* capability with a subset of `src`'s rights. Narrowing only
    /// drops rights — you can never ascribe a right the source doesn't hold. An
    /// unconstrained source is pinned to the target.
    fn check_narrow(&mut self, src: &Ty, target: &Ty) -> Result<(), TypeError> {
        let resolved = self.resolve(src);
        let ok = match (&resolved, target) {
            (Ty::Dir(s), Ty::Dir(t)) => (!t.read || s.read) && (!t.write || s.write),
            (Ty::File(s), Ty::File(t)) => (!t.read || s.read) && (!t.write || s.write),
            (Ty::Net(s), Ty::Net(t)) => {
                (!t.connect || s.connect)
                    && (!t.listen || s.listen)
                    && (!t.tcp || s.tcp)
                    && (!t.udp || s.udp)
                    && (!t.uds || s.uds)
            }
            (Ty::Console(s), Ty::Console(t)) => {
                (!t.read || s.read) && (!t.write || s.write)
            }
            (Ty::Exec, Ty::Exec) => true,
            (Ty::Fetch, Ty::Fetch) => true,
            // An unconstrained source: pin it to the ascribed capability.
            (
                Ty::Var(_),
                Ty::Dir(_) | Ty::File(_) | Ty::Net(_) | Ty::Console(_) | Ty::Exec | Ty::Fetch,
            ) => {
                return self.unify(src, target).map_err(|e| TypeError {
                    message: format!("in `as` ascription: {}", e.message),
                });
            }
            _ => {
                return terr(format!(
                    "`as` narrows a capability to a subset of its rights; cannot ascribe `{resolved}` as `{target}`"
                ));
            }
        };
        if !ok {
            return terr(format!(
                "`as` can only drop rights: `{target}` is not a subset of `{resolved}`"
            ));
        }
        Ok(())
    }

    /// Bind a call argument (`actual`) to a parameter (`expected`), allowing a
    /// broader capability to stand in for a narrower one — implicit directional
    /// narrowing, so a full `Net` satisfies a `Net[Connect]` parameter (more
    /// authority can always be used where less is asked). The callee is still
    /// type-bounded to the parameter's rights, so this stays sound: it cannot do
    /// (or re-pass) more than its declared type permits. Everything else (Vars,
    /// non-caps, exact caps, a too-narrow argument) falls back to unification.
    fn coerce_arg(&mut self, expected: &Ty, actual: &Ty) -> Result<(), TypeError> {
        if self.existential_coercion(expected, actual)? {
            return Ok(());
        }
        let coercible = match (self.resolve(expected), self.resolve(actual)) {
            // `want`'s rights must be a subset of what the argument `has`.
            (Ty::Dir(want), Ty::Dir(has)) => (!want.read || has.read) && (!want.write || has.write),
            (Ty::File(want), Ty::File(has)) => (!want.read || has.read) && (!want.write || has.write),
            (Ty::Net(want), Ty::Net(has)) => {
                (!want.connect || has.connect)
                    && (!want.listen || has.listen)
                    && (!want.tcp || has.tcp)
                    && (!want.udp || has.udp)
                    && (!want.uds || has.uds)
            }
            (Ty::Console(want), Ty::Console(has)) => {
                (!want.read || has.read) && (!want.write || has.write)
            }
            _ => false,
        };
        if coercible
            || self.anon_union_widening_ok(expected, actual)?
            || self.record_width_conformance(expected, actual)?
        {
            Ok(())
        } else {
            self.unify(expected, actual)
        }
    }

    /// Check one directed structural-record conversion. Only compiler-owned
    /// anonymous record heads participate: nominal records and all other types
    /// fall through to exact unification. Same-shape records also fall through,
    /// preserving ordinary generic inference without manufacturing a projection.
    fn record_width_conformance(
        &mut self,
        expected: &Ty,
        actual: &Ty,
    ) -> Result<bool, TypeError> {
        let expected = self.resolve(expected);
        let actual = self.resolve(actual);
        let (Ty::Named(expected_name, expected_types), Ty::Named(actual_name, actual_types)) =
            (&expected, &actual)
        else {
            return Ok(false);
        };
        let Some(expected_fields) = witchy_syntax::ast::anon_record_field_names(expected_name)
        else {
            return Ok(false);
        };
        let Some(actual_fields) = witchy_syntax::ast::anon_record_field_names(actual_name) else {
            return Ok(false);
        };
        if expected_fields == actual_fields {
            return Ok(false);
        }
        if expected_fields.len() != expected_types.len()
            || actual_fields.len() != actual_types.len()
        {
            return terr("malformed compiler-owned anonymous record type");
        }

        let saved_subst = self.subst.clone();
        for (field, expected_type) in expected_fields.iter().zip(expected_types) {
            let Some(index) = actual_fields.iter().position(|candidate| candidate == field) else {
                self.subst = saved_subst;
                return terr(format!(
                    "structural record `{actual}` does not conform to `{expected}`: missing required field `{field}`"
                ));
            };
            if let Err(error) = self.unify(expected_type, &actual_types[index]) {
                self.subst = saved_subst;
                return terr(format!(
                    "structural record `{actual}` does not conform to `{expected}`: field `{field}` has incompatible type: {}",
                    error.message
                ));
            }
        }
        Ok(true)
    }

    fn record_record_projection(&mut self, expr: &Expr, expected: &Ty, actual: &Ty) {
        let target = self.resolve(expected);
        let source = self.resolve(actual);
        let (Ty::Named(target_name, _), Ty::Named(source_name, _)) = (&target, &source) else {
            return;
        };
        let (Some(target_fields), Some(source_fields)) = (
            witchy_syntax::ast::anon_record_field_names(target_name),
            witchy_syntax::ast::anon_record_field_names(source_name),
        ) else {
            return;
        };
        if source_fields.len() <= target_fields.len()
            || !target_fields
                .iter()
                .all(|field| source_fields.contains(field))
        {
            return;
        }
        if let Some(record) = &mut self.record_projection_record {
            record.insert(expr as *const Expr as usize, (target, source));
        }
    }

    /// Whether this pair is one directed existential conversion.
    ///
    /// An unresolved concrete type is accepted here so generic bodies can be
    /// checked before monomorphization; final existential preparation requires
    /// a fully resolved pair.
    fn existential_coercion(
        &self,
        expected: &Ty,
        actual: &Ty,
    ) -> Result<bool, TypeError> {
        let expected = self.resolve(expected);
        let Ty::Dyn(dyn_name, expected_args) = &expected else {
            return Ok(false);
        };
        let resolved_actual = self.resolve(actual);
        if let Ty::Dyn(actual_name, actual_args) = &resolved_actual {
            // Trait lowering has already erased declarations by the final
            // annotation pass. Source checking retains the declaration graph,
            // while final preparation validates the same edge against its
            // retained WitnessCatalog before creating a runtime node.
            let structural_ok = actual_name != dyn_name
                && expected_args.is_empty()
                && actual_args.is_empty();
            if structural_ok
                && !self.trait_supertraits.is_empty()
                && !self.trait_has_supertrait(actual_name, dyn_name)
            {
                return terr(format!(
                    "cannot convert `dyn {actual_name}` to unrelated `dyn {dyn_name}`; \
                     `{dyn_name}` is not a supertrait of `{actual_name}`"
                ));
            }
            return Ok(structural_ok);
        }
        if let Some((cap, path)) = self.ty_capability_retention(&resolved_actual) {
            let path = if path.is_empty() {
                String::new()
            } else {
                format!(" through `{}`", path.join(" -> "))
            };
            return terr(format!(
                "conversion to `dyn {}`: the concrete payload type `{resolved_actual}` \
                 carries a `{cap}` capability{path} — capability-carrying existential \
                 payloads are rejected (RFC-0081); pass the capability explicitly \
                 in method signatures instead",
                existential_bare(dyn_name)
            ));
        }
        Ok(true)
    }

    fn trait_has_supertrait(&self, child: &str, target: &str) -> bool {
        let mut seen = HashSet::new();
        let mut pending = self
            .trait_supertraits
            .get(child)
            .cloned()
            .unwrap_or_default();
        while let Some(current) = pending.pop() {
            if !seen.insert(current.clone()) {
                continue;
            }
            if current == target {
                return true;
            }
            if let Some(next) = self.trait_supertraits.get(&current) {
                pending.extend(next.iter().cloned());
            }
        }
        false
    }

    fn record_existential_pack(
        &mut self,
        expr: &Expr,
        expected: &Ty,
        actual: &Ty,
    ) -> Result<(), TypeError> {
        let is_conversion = self.existential_coercion(expected, actual)?;
        let source = self.resolve(actual);
        if is_conversion && !matches!(source, Ty::Dyn(_, _)) {
            let target = self.resolve(expected);
            if let Some(record) = &mut self.existential_pack_record {
            record.insert(
                expr as *const Expr as usize,
                    (target, source),
            );
            }
        }
        Ok(())
    }

    fn record_existential_upcast(
        &mut self,
        expr: &Expr,
        expected: &Ty,
        actual: &Ty,
    ) -> Result<(), TypeError> {
        let is_conversion = self.existential_coercion(expected, actual)?;
        let source = self.resolve(actual);
        if is_conversion && matches!(source, Ty::Dyn(_, _)) {
            let target = self.resolve(expected);
            if let Some(record) = &mut self.existential_upcast_record {
                record.insert(expr as *const Expr as usize, (target, source));
            }
        }
        Ok(())
    }

    fn reject_var_directed_coercion(
        &mut self,
        callable: &str,
        index: usize,
        convention: Option<&Convention>,
        expected: &Ty,
        actual: &Ty,
    ) -> Result<(), TypeError> {
        if !matches!(convention, Some(Convention::Var)) {
            return Ok(());
        }
        if self.existential_coercion(expected, actual)? {
            return terr(format!(
                "argument {} to `var` parameter of `{callable}` cannot implicitly convert \
                 `{}` to `{}` — the callee may replace the existential with a different \
                 concrete witness, which cannot be written back into the concrete caller \
                 place; bind a `var` of the existential type before this call",
                index + 1,
                self.resolve(actual),
                self.resolve(expected)
            ));
        }
        if self.record_width_conformance(expected, actual)? {
            return terr(format!(
                "argument {} to `var` parameter of `{callable}` cannot project `{}` to `{}` — \
                 `var` arguments are invariant because the callee may replace target fields and \
                 write-back cannot reconstruct the caller's omitted fields; bind a `var` of the \
                 exact target shape before this call",
                index + 1,
                self.resolve(actual),
                self.resolve(expected)
            ));
        }
        Ok(())
    }

    /// (RFC-0047 / RFC-0081 / BUG-302) Whether `t` (a resolved [`Ty`]) contains a
    /// function, capability, or existential type at any depth — the three kinds
    /// `==`/`!=` refuse. Containers
    /// (List/Tuple/Dict/Result/Option) are transparent through their generic
    /// arguments; a record or enum is transparent through its DECLARED FIELD /
    /// variant-payload types too (a `type H: run: fn(Int) -> Int` is `Named("H",
    /// [])` with no generic argument, so walking only arguments — the old bug —
    /// let it escape the net, then the compiled backend rejected it while the
    /// interpreter compared by closure identity: a parity divergence). `seen`
    /// guards against recursive types. A bare type variable is comparable here (a
    /// bounded generic resolves after monomorphization; an unbounded one is caught
    /// elsewhere).
    fn uncomparable_kind(&self, t: &Ty, seen: &mut HashSet<String>) -> Option<Uncomparable> {
        match t {
            Ty::Fn(_, _, _) => Some(Uncomparable::Function),
            Ty::Dyn(_, _) => Some(Uncomparable::Existential),
            Ty::Console(_)
            | Ty::Clock
            | Ty::Rand
            | Ty::Env
            | Ty::Secret
            | Ty::Exec
            | Ty::Fetch
            | Ty::Socket
            | Ty::Listener | Ty::Dir(_) | Ty::File(_) | Ty::Net(_) | Ty::BuildOut | Ty::BuildRead
            | Ty::BuildEnv | Ty::BuildNet | Ty::BuildExec => Some(Uncomparable::Capability),
            Ty::List(e) => self.uncomparable_kind(e, seen),
            Ty::Tuple(ts) => ts.iter().find_map(|x| self.uncomparable_kind(x, seen)),
            Ty::Named(n, args) => {
                // `SecretStore` is a capability the type checker models as a Named type.
                if n == "SecretStore" {
                    return Some(Uncomparable::Capability);
                }
                // Generic arguments (`List(fn)`, `Option(File)`, `H(fn)`).
                if let Some(k) = args.iter().find_map(|a| self.uncomparable_kind(a, seen)) {
                    return Some(k);
                }
                // Declared record fields / enum variant payloads. Guard against a
                // recursive type revisiting itself (which cannot make it
                // uncomparable — the offending fn/cap would be found on the first
                // visit).
                if !seen.insert(n.clone()) {
                    return None;
                }
                let found = self.named_field_uncomparable(n, seen);
                seen.remove(n);
                found
            }
            _ => None,
        }
    }

    /// The un-comparable kind carried by any DECLARED field of record/enum `n`
    /// (payload types are walked with the type's own parameters left as vars — a
    /// concrete fn/cap field triggers; a generic field is caught via the generic
    /// argument walk in [`Self::uncomparable_kind`]).
    fn named_field_uncomparable(&self, n: &str, seen: &mut HashSet<String>) -> Option<Uncomparable> {
        // A record: its named fields.
        if let Some((_, fields)) = self.record_fields.get(n) {
            if let Some(k) = fields.iter().find_map(|(_, ty)| self.uncomparable_kind(ty, seen)) {
                return Some(k);
            }
        }
        // An enum (possibly the same record type, which is a one-variant enum):
        // every variant's payload types.
        if let Some(variants) = self.adt_variants.get(n) {
            for v in variants {
                if let Some((payloads, _)) = self.ctor_sigs.get(v) {
                    if let Some(k) = payloads.iter().find_map(|ty| self.uncomparable_kind(ty, seen)) {
                        return Some(k);
                    }
                }
            }
        }
        None
    }

    // --- inference ---

    fn infer_block(&mut self, block: &Block) -> Result<Ty, TypeError> {
        self.infer_block_with_expected_tail(block, None)
    }

    fn infer_block_tail_expected(&mut self, block: &Block, expected: &Ty) -> Result<Ty, TypeError> {
        self.infer_block_with_expected_tail(block, Some(expected))
    }

    fn infer_block_with_expected_tail(
        &mut self,
        block: &Block,
        expected: Option<&Ty>,
    ) -> Result<Ty, TypeError> {
        let is_region = block.region.is_some();
        if is_region {
            self.region_locals.push(HashSet::new());
        }
        let result = self.infer_block_inner(block, expected);
        if is_region {
            self.region_locals.pop();
        }
        let ty = result?;
        if is_region
            && !self.ty_is_direct_externref_value(&ty)
            && let Some(cap) = self.ty_carries_externref_cap(&ty)
        {
            return terr(format!(
                "a `region` value cannot be a reference-backed aggregate carrying capability `{cap}` until \
                 region copy-out understands GC references"
            ));
        }
        if let Some(ann) = &block.region {
            if let Some(want) = &ann.ty {
                let want_ty = self.to_ty(want);
                self.unify(&ty, &want_ty).map_err(|e| TypeError {
                    message: format!("region value: {}", e.message),
                })?;
            }
        }
        Ok(ty)
    }

    fn infer_block_inner(&mut self, block: &Block, expected: Option<&Ty>) -> Result<Ty, TypeError> {
        self.push();
        let mut ty = Ty::Unit;
        let tail = block.stmts.len().saturating_sub(1);
        for (i, stmt) in block.stmts.iter().enumerate() {
            if let Some(line) = block.lines.get(i) {
                self.cur_line = *line;
            }
            match stmt {
                Stmt::Let { name, ty: decl, mutable, value } => {
                    // An ascription remains a unification constraint except
                    // for RFC-0081's directed concrete-to-existential erasure.
                    // It pins variables the RHS leaves open and reports
                    // disagreement at THIS line.
                    let vt = if let Some(decl) = decl {
                        // (RFC-0025) `frozen` asserts deep immutability, so a `frozen`
                        // binding cannot also be mutable — `var x: frozen T` is a
                        // contradiction the checker rejects (the contract has teeth).
                        if *mutable && is_frozen_type(decl) {
                            return Err(TypeError {
                                message: format!(
                                    "`{name}` is declared `frozen` (deeply immutable) but also `var` (mutable) — drop the `var` (use `let`), or drop `frozen`"
                                ),
                            });
                        }
                        let want = self.to_ty(decl);
                        let vt = self.infer_expected(value, &want).map_err(|e| {
                            type_mismatch_context(
                                || format!("`{name}` is declared `{want}` but the value disagrees"),
                                e,
                            )
                        })?;
                        let directed = self.existential_coercion(&want, &vt)?
                            || self.record_width_conformance(&want, &vt)?;
                        if directed {
                            want
                        } else {
                            self.unify(&want, &vt).map_err(|e| TypeError {
                                message: format!(
                                    "`{name}` is declared `{want}` but the value disagrees: {}",
                                    e.message
                                ),
                            })?;
                            vt
                        }
                    } else {
                        self.infer(value)?
                    };
                    self.define(name.clone(), vt, *mutable);
                    ty = Ty::Unit;
                }
                Stmt::Assign { name, value } => {
                    let Some(existing) = self.lookup(name) else {
                        self.pop();
                        return terr(format!("assignment to unbound variable `{name}`"));
                    };
                    if self.is_mutable(name) == Some(false) {
                        self.pop();
                        return terr(format!(
                            "cannot assign to `{name}`: it is immutable (declared with `let`)"
                        ));
                    }
                    if let Some(scope) = self.region_locals.last() {
                        if !scope.contains(name) && !is_scalar_ty(&self.resolve(&existing)) {
                            self.pop();
                            return terr(format!(
                                "cannot assign `{name}` inside `region:`: it is declared outside the region and is not a scalar — a region's only escape is its value"
                            ));
                        }
                    }
                    let vt = self.infer_expected(value, &existing)?;
                    if !self.existential_coercion(&existing, &vt)?
                        && !self.record_width_conformance(&existing, &vt)?
                    {
                        self.unify(&existing, &vt)?;
                    }
                    self.consumed.remove(name); // reassignment re-initializes
                    ty = Ty::Unit;
                }
                Stmt::LetPattern { pattern, value } => {
                    // (RFC-0052) `let PAT = e` — ONE pattern grammar for every
                    // binding position. A `let`/`for`/comprehension pattern must be
                    // IRREFUTABLE (proven to always match); a refutable one errors,
                    // pointing at `if let`.
                    if let Some(dup) = pattern_dup_binding(pattern) {
                        return terr(format!(
                            "pattern binds `{dup}` more than once — each binding in a \
                             pattern must have a distinct name"
                        ));
                    }
                    let vt = self.infer(value)?;
                    if let Some(reason) = self.pattern_refutable(pattern, &vt) {
                        return terr(reason);
                    }
                    self.check_pattern(pattern, &vt)?;
                    ty = Ty::Unit;
                }
                Stmt::Return(opt) => {
                    let t = match opt {
                        Some(e) => match self.current_ret.clone() {
                            Some(ret) => self.infer_expected(e, &ret)?,
                            None => self.infer(e)?,
                        },
                        None => Ty::Unit,
                    };
                    if let Some(ret) = self.current_ret.clone() {
                        self.coerce_arg(&ret, &t).map_err(|e| TypeError {
                            message: format!("`return` value: {}", e.message),
                        })?;
                    }
                    // A return diverges: its position can satisfy any expected
                    // type, so contribute a fresh var (which unifies with anything).
                    ty = self.fresh();
                }
                Stmt::Expr(e) => {
                    ty = if i == tail {
                        match expected {
                            Some(expected) => self.infer_expected(e, expected)?,
                            None => self.infer(e)?,
                        }
                    } else {
                        self.infer(e)?
                    };
                }
                Stmt::Yield(e) => {
                    if !self.region_locals.is_empty() {
                        self.pop();
                        return terr(
                            "cannot `yield` inside `region:`: the generator frame outlives the region".to_string(),
                        );
                    }
                    ty = self.infer(e)?;
                }
                // `break`/`continue` diverge (control leaves the block), so like
                // `return` they contribute a fresh var that unifies with any
                // expected type — letting `match x { _ -> { break } ... }` work.
                // Misuse outside a loop is caught by codegen (no enclosing label).
                Stmt::Break | Stmt::Continue => {
                    ty = self.fresh();
                }
            }
        }
        self.pop();
        Ok(ty)
    }

    fn infer(&mut self, expr: &Expr) -> Result<Ty, TypeError> {
        let t = self.infer_inner(expr)?;
        self.finish_infer(expr, t)
    }

    fn finish_infer(&mut self, expr: &Expr, t: Ty) -> Result<Ty, TypeError> {
        // (RFC-0047) `Dict` keys and `Set` members require `Eq`; `Float` is
        // `PartialEq` but not `Eq` (NaN != NaN), so a `Float`-keyed dict has an
        // unretrievable NaN entry and a `Float` key is a precision trap. Reject
        // it at check time — once any expression's type concretely has a `Float`
        // in key position, the whole program is refused (a type-level rule, not a
        // per-NaN runtime trap). Only fires when the key type is concretely Float
        // (an unresolved key var never triggers).
        if let Some(kind) = float_key_position(&self.resolve(&t)) {
            return terr(float_key_reject_message(kind));
        }
        self.reject_runtime_compiler_syntax_ty(&t, "expression")?;
        if let Some(rec) = &mut self.type_record {
            rec.insert(expr as *const Expr as usize, t.clone());
        }
        Ok(t)
    }

    fn infer_expected(&mut self, expr: &Expr, expected: &Ty) -> Result<Ty, TypeError> {
        let actual = self.infer_expected_inner(expr, expected)?;
        self.record_existential_pack(expr, expected, &actual)?;
        self.record_existential_upcast(expr, expected, &actual)?;
        self.record_record_projection(expr, expected, &actual);
        Ok(actual)
    }

    fn infer_expected_inner(&mut self, expr: &Expr, expected: &Ty) -> Result<Ty, TypeError> {
        match expr {
            Expr::AnonCtor { tag, args } => {
                self.check_anon_ctor(tag, args, expected)?;
                self.finish_infer(expr, expected.clone())
            }
            Expr::List(items) => {
                if let Ty::List(elem) = self.resolve(expected) {
                    for item in items {
                        let at = self.infer_expected(item, &elem)?;
                        self.coerce_arg(&elem, &at)?;
                    }
                    return self.finish_infer(expr, expected.clone());
                }
                let at = self.infer(expr)?;
                self.coerce_arg(expected, &at)?;
                Ok(at)
            }
            Expr::Tuple(items) => {
                if let Ty::Tuple(slots) = self.resolve(expected) {
                    if slots.len() == items.len() {
                        for (item, slot) in items.iter().zip(&slots) {
                            let at = self.infer_expected(item, slot)?;
                            self.coerce_arg(slot, &at)?;
                        }
                        return self.finish_infer(expr, expected.clone());
                    }
                }
                let at = self.infer(expr)?;
                self.coerce_arg(expected, &at)?;
                Ok(at)
            }
            Expr::Call { name, args } => {
                let at = self.infer_call(name, args, Some(expected))?;
                self.coerce_arg(expected, &at)?;
                self.finish_infer(expr, at)
            }
            Expr::Ctor { name, args } => {
                if name != "Nil" && let Some((fields, result)) = self.ctor_sigs.get(name).cloned() {
                    let typarams = self.ctor_typarams.get(name).cloned().unwrap_or_default();
                    let (fields, result) = self.instantiate(&fields, &result, &typarams);
                    self.coerce_arg(expected, &result).map_err(|e| TypeError {
                        message: format!("in constructor `{name}`: {}", e.message),
                    })?;
                    if fields.len() != args.len() {
                        return terr(format!(
                            "constructor `{name}` takes {} field(s) but got {}",
                            fields.len(),
                            args.len()
                        ));
                    }
                    for (arg, fty) in args.iter().zip(&fields) {
                        let at = if self.ty_is_direct_externref_value(fty) {
                            self.infer(arg)?
                        } else {
                            self.infer_expected(arg, fty)?
                        };
                        self.coerce_arg(fty, &at).map_err(|e| TypeError {
                            message: format!("in constructor `{name}`: {}", e.message),
                        })?;
                    }
                    self.reject_externref_cap_aggregate_ty(&result, &format!("constructor `{name}`"))?;
                    self.reject_structural_authority_ty(&result, &format!("constructor `{name}`"))?;
                    return self.finish_infer(expr, result);
                }
                let at = self.infer(expr)?;
                self.coerce_arg(expected, &at)?;
                Ok(at)
            }
            Expr::Match { scrutinee, arms } => {
                let at = self.infer_match_expected(scrutinee, arms, expected)?;
                self.finish_infer(expr, at)
            }
            Expr::If { cond, then_block, else_block } => {
                let ct = self.infer(cond)?;
                self.unify(&Ty::Bool, &ct)?;
                let tt = self.infer_block_expected(then_block, expected)?;
                self.coerce_arg(expected, &tt)?;
                if let Some(else_block) = else_block {
                    let et = self.infer_block_expected(else_block, expected)?;
                    self.coerce_arg(expected, &et)?;
                } else if matches!(self.resolve(expected), Ty::Dyn(_, _)) {
                    return terr(
                        "`if` without `else` has an implicit `Nil` path that cannot produce \
                         an existential value — add an explicit `else` branch"
                    );
                } else {
                    self.coerce_arg(expected, &Ty::Unit).map_err(|e| TypeError {
                        message: format!("`if` without `else` produces `Nil`: {}", e.message),
                    })?;
                }
                self.finish_infer(expr, expected.clone())
            }
            Expr::Block(block) => {
                let at = self.infer_block_expected(block, expected)?;
                self.coerce_arg(expected, &at)?;
                self.finish_infer(expr, expected.clone())
            }
            _ => {
                let at = self.infer(expr)?;
                self.coerce_arg(expected, &at)?;
                Ok(at)
            }
        }
    }

    fn infer_call(
        &mut self,
        name: &str,
        args: &[Expr],
        expected: Option<&Ty>,
    ) -> Result<Ty, TypeError> {
        let is_cap_op = cap_ops::is_marked(name);
        let call_name = cap_ops::surface_name(name);
        if expected.is_none()
            && matches!(call_name, "dynamic.try_decode" | "dynamic.decode")
        {
            let result = if call_name.ends_with("try_decode") {
                "Option(T)"
            } else {
                "Result(T, dynamic.DynamicError)"
            };
            return terr(format!(
                "`{call_name}` requires an expected `{result}` result type; add a type annotation"
            ));
        }
        if let Some((callback_index, diagnostic)) =
            isolated_vm_callback_contract(call_name, args.len())
        {
            let callback = &args[callback_index];
            let is_bare_top_level = matches!(callback, Expr::Var(function)
                if self.lookup(function).is_none() && self.fn_sigs.contains_key(function));
            if !is_bare_top_level {
                return terr(diagnostic);
            }
            if let Expr::Var(function) = callback
                && let Some((params, ret)) = self.fn_sigs.get(function).cloned()
            {
                let conventions = self
                    .fn_conventions
                    .get(function)
                    .cloned()
                    .unwrap_or_else(|| vec![Convention::Let; params.len()]);
                let function_ty = Ty::Fn(params, Box::new(ret), conventions);
                if call_name != "vm.with_dir"
                    && let Some(cap) = self.ty_carries_externref_cap(&function_ty)
                {
                    return terr(format!(
                        "`{call_name}` cannot accept function value `{function}` carrying `{cap}` \
                         through the isolated-worker callback adapter; that adapter still uses a \
                         scalar cross-instance ABI and typed callback lowering is not implemented"
                    ));
                }
            }
        }
        // A local binding (parameter or `let`) holding a function value:
        // apply it. Handles both an explicit `fn(..)->..` type and an as
        // yet unconstrained variable (which we pin to a function type).
        if !is_cap_op && let Some(vty) = self.lookup(name) {
            match self.resolve(&vty) {
                Ty::Fn(param_tys, ret, conventions) => {
                    if self.current_isolated_callback.as_deref() != Some(name) {
                        self.reject_externref_cap_aggregate_ty(
                            &vty,
                            &format!("function value `{name}`"),
                        )?;
                    }
                    if param_tys.len() != args.len() {
                        let display = diagnostic_callable_name(name);
                        return terr(format!(
                            "`{display}` expects {} argument(s) but got {}",
                            param_tys.len(),
                            args.len()
                        ));
                    }
                    let display = diagnostic_callable_name(name);
                    if let Some(expected) = expected {
                        self.coerce_arg(expected, ret.as_ref()).map_err(|e| TypeError {
                            message: format!("in call to `{display}`: {}", e.message),
                        })?;
                    }
                    for (index, (arg, pty)) in args.iter().zip(&param_tys).enumerate() {
                        let at = self.infer_expected(arg, pty)
                            .map_err(|e| in_call_context(&display, e))?;
                        self.reject_var_directed_coercion(
                            &display,
                            index,
                            conventions.get(index),
                            pty,
                            &at,
                        )?;
                        self.coerce_arg(pty, &at).map_err(|e| TypeError {
                            message: format!("in call to `{display}`: {}", e.message),
                        })?;
                    }
                    // Generic function values can acquire unsupported aggregate
                    // shapes only after argument inference. Recheck the resolved
                    // type so `let f = id; f(file)` cannot bypass that boundary.
                    if self.current_isolated_callback.as_deref() != Some(name) {
                        self.reject_externref_cap_aggregate_ty(
                            &vty,
                            &format!("function value `{name}`"),
                        )?;
                    }
                    self.enforce_function_value_conventions(name, args, &conventions)?;
                    return Ok(*ret);
                }
                Ty::Var(_) => {
                    let mut argtys = Vec::new();
                    for arg in args {
                        argtys.push(self.infer(arg)?);
                    }
                    let ret = expected.cloned().unwrap_or_else(|| self.fresh());
                    self.unify(
                        &vty,
                        &Ty::Fn(
                            argtys,
                            Box::new(ret.clone()),
                            vec![Convention::Let; args.len()],
                        ),
                    )?;
                    self.reject_externref_cap_aggregate_ty(
                        &vty,
                        &format!("function value `{name}`"),
                    )?;
                    return Ok(ret);
                }
                _ => {} // a non-function local with this name: fall through
            }
        }
        // A concrete catalog type recipe outranks the linked std placeholder.
        // Source declarations remain present for documentation, parameter
        // conventions, and ownership qualifiers, while generic trait bounds
        // travel with the catalog recipe itself.
        let catalog_contract = (!is_cap_op)
            .then(|| self.intrinsic_call_sig(call_name))
            .flatten()
            .map(|(params, ret)| {
                let signature = intrinsics::lookup(call_name)
                    .expect("catalog type recipe has a catalog row")
                    .signature;
                let bounds = signature
                    .trait_bounds()
                    .iter()
                    .map(|bound| {
                        (
                            params
                                .get(bound.parameter)
                                .unwrap_or_else(|| {
                                    panic!(
                                        "catalog trait bound parameter {} is outside {} arguments for {call_name}",
                                        bound.parameter,
                                        params.len()
                                    )
                                })
                                .clone(),
                            bound.trait_name.to_string(),
                        )
                    })
                    .collect();
                (params, ret, bounds)
            });
        let user_sig = catalog_contract
            .is_none()
            .then(|| (!is_cap_op).then(|| self.user_call_sig_with_bounds(name)).flatten())
            .flatten();
        let exact_generic_positions = if user_sig.is_some()
            && self
                .fn_typarams
                .get(name)
                .is_some_and(|parameters| !parameters.is_empty())
        {
            self.fn_sigs
                .get(name)
                .map(|(parameters, _)| parameters.iter().map(ty_has_var).collect::<Vec<_>>())
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        if catalog_contract.is_none() && user_sig.is_none() {
            if !is_cap_op && let Some(msg) = bare_cap_op_error(call_name, args.len()) {
                return terr(msg);
            }
            if let Some(t) = self.check_console_op(call_name, args)? {
                return Ok(t);
            }
            if let Some(t) = self.check_file_op(call_name, args)? {
                return Ok(t);
            }
            if let Some(t) = self.check_env_op(call_name, args)? {
                return Ok(t);
            }
            if let Some(t) = self.check_fetch_op(call_name, args)? {
                return Ok(t);
            }
            if let Some(t) = self.check_dir_op(call_name, args)? {
                return Ok(t);
            }
            if let Some(t) = self.check_exec_op(call_name, args)? {
                return Ok(t);
            }
            if let Some(t) = self.check_net_op(call_name, args)? {
                return Ok(t);
            }
        }
        if let Some(t) = self.check_try_ctx(call_name, args)? {
            return Ok(t);
        }
        let (params, ret, call_bounds) = match catalog_contract {
            Some(contract) => contract,
            None => match user_sig {
                Some(sig) => sig,
                None => {
                    let Some((params, ret)) = self.call_sig(call_name) else {
                        return self.unknown_call(call_name, args);
                    };
                    (params, ret, Vec::new())
                }
            },
        };
        if params.len() != args.len() {
            let display = diagnostic_callable_name(name);
            // (RFC-0072 phase 2) Show WHAT the arguments are, not just
            // how many — the reader shouldn't have to hunt the signature.
            // (Zero-arg callees skip the empty parenthetical.)
            let sig = if params.is_empty() {
                String::new()
            } else {
                let types = params
                    .iter()
                    .map(|p| self.resolve(p).to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(" ({types})")
            };
            return terr(format!(
                "`{display}` expects {} argument(s){sig} but got {}",
                params.len(),
                args.len()
            ));
        }
        let display = diagnostic_callable_name(name);
        let bind_expected_before_args = expected.is_some() && call_bounds.is_empty();
        if bind_expected_before_args && let Some(expected) = expected {
            self.coerce_arg(expected, &ret).map_err(|e| TypeError {
                message: format!("in call to `{display}`: {}", e.message),
            })?;
        }
        let typed_gc_list_intrinsic = matches!(
            call_name,
            intrinsics::LIST_LENGTH
                | intrinsics::LIST_AT
                | intrinsics::LIST_PUSH
                | intrinsics::GENERATED_LIST_PUSH
                | intrinsics::LIST_SET_AT
                | intrinsics::LIST_CONCAT
        ) || intrinsics::is_list_pop_extract(call_name)
            || call_name == "list.pop";
        let call_conventions = self.fn_conventions.get(name).cloned();
        for (index, (arg, param_ty)) in args.iter().zip(&params).enumerate() {
            let typed_vm_callback = call_name == "vm.with_dir" && index == 1;
            let exact_generic = exact_generic_positions.get(index).copied().unwrap_or(false);
            let at = if typed_vm_callback {
                let Expr::Var(function) = arg else {
                    unreachable!("the isolated callback contract rejects non-function values")
                };
                let (callback_params, callback_ret) = self
                    .fn_sigs
                    .get(function)
                    .cloned()
                    .expect("the isolated callback contract requires a known function");
                let typarams: HashSet<u32> = self
                    .fn_typarams
                    .get(function)
                    .into_iter()
                    .flatten()
                    .map(|(_, id)| *id)
                    .collect();
                let (callback_params, callback_ret) =
                    self.instantiate(&callback_params, &callback_ret, &typarams);
                let conventions = self
                    .fn_conventions
                    .get(function)
                    .cloned()
                    .unwrap_or_else(|| vec![Convention::Let; callback_params.len()]);
                Ok(Ty::Fn(
                    callback_params,
                    Box::new(callback_ret),
                    conventions,
                ))
            } else if exact_generic {
                self.infer(arg)
            } else {
                self.infer_expected(arg, param_ty)
            }
            .map_err(|e| in_call_context(&display, e))?;
            self.reject_var_directed_coercion(
                &display,
                index,
                call_conventions
                    .as_ref()
                    .and_then(|conventions| conventions.get(index)),
                param_ty,
                &at,
            )?;
            // (BUG-305) `"${f}"` on a function value is rejected HERE, at
            // check time, so BOTH backends refuse it identically — rather
            // than the interpreter rendering `<function/N>` while the
            // compiled backend rejected at codegen with a misleading
            // "generic record such as `Set`" diagnostic. A function has no
            // printable form; interpolation renders DATA. The render intrinsic is the
            // desugaring of `"${…}"`, so the message speaks in the user's
            // terms and never names `Set`/records for a function operand.
            if ast::is_render_intrinsic(call_name) {
                if let Ty::Fn(..) = self.resolve(&at) {
                    return terr(
                        "cannot render a function value with `\"${…}\"` — a \
                         function has no printable form. Interpolation renders \
                         data; call the function (e.g. `f(x)`) and interpolate \
                         its result instead",
                    );
                }
                if let Some(cap) = self.ty_carries_externref_cap(&at) {
                    return terr(format!(
                        "cannot render a value carrying capability `{cap}` with `\"${{…}}\"` — \
                         capabilities are authority, not printable data"
                    ));
                }
            }
            if ty_has_var(param_ty)
                && !typed_gc_list_intrinsic
                && let Some(cap) = self.ty_carries_externref_cap(&at)
            {
                return terr(format!(
                    "argument to `{display}` instantiates a generic parameter with a value \
                     carrying `{cap}`; capability-bearing generics require a typed \
                     specialization ABI"
                ));
            }
            if !typed_vm_callback {
                self.reject_externref_cap_aggregate_ty(
                    &at,
                    &format!("argument to `{display}`"),
                )?;
            }
            let result = if exact_generic {
                self.unify(param_ty, &at)
            } else {
                self.coerce_arg(param_ty, &at)
            };
            result.map_err(|e| TypeError {
                message: format!("in call to `{display}`: {}", e.message),
            })?;
        }
        for (bound_ty, trait_name) in &call_bounds {
            self.require_call_bound(call_name, bound_ty, trait_name)?;
        }
        if !bind_expected_before_args && let Some(expected) = expected {
            self.coerce_arg(expected, &ret).map_err(|e| TypeError {
                message: format!("in call to `{display}`: {}", e.message),
            })?;
        }
        // (BUG-395) A generic `Dict` key operation's key must be `Eq` — record
        // the (post-unification) key type and its line; validated once the
        // whole body is inferred (so a key var pinned later is seen concrete).
        if let Some(i) = dict_key_op_index(call_name) {
            if let Some(key_ty) = params.get(i) {
                self.dict_key_ops.push((key_ty.clone(), self.cur_line));
            }
        }
        // Enforce conventions: a `var` parameter needs a mutable variable;
        // `own` consumes its argument (use-after-move becomes an error).
        if !is_cap_op && let Some(convs) = self.fn_conventions.get(name).cloned() {
            let mut var_places = Vec::new();
            for (i, (arg, conv)) in args.iter().zip(&convs).enumerate() {
                match conv {
                    Convention::Var => {
                        let parameter = self.var_parameter_context(name, i);
                        if matches!(arg, Expr::Unary { op: UnOp::Move, .. }) {
                            return terr(format!(
                                "argument {} to {parameter} uses `move`; write-back requires a live mutable place in the caller",
                                i + 1
                            ));
                        }
                        match var_place(arg) {
                        Some(place) if self.is_mutable(&place.root) == Some(true) => {
                            for (previous_index, previous) in &var_places {
                                if places_overlap(previous, &place) {
                                    return terr(format!(
                                        "arguments {} and {} to `{name}` are overlapping `var` places rooted in `{}`",
                                        previous_index + 1,
                                        i + 1,
                                        place.root
                                    ));
                                }
                            }
                            self.reject_later_writeback_conflict(name, args, i, &place)?;
                            var_places.push((i, place));
                        }
                        Some(place) => {
                            return terr(format!(
                                "argument {} to {parameter} has immutable root `{}`; root `{}` must be a mutable `var` for write-back",
                                i + 1,
                                place.root,
                                place.root
                            ))
                        }
                        None => {
                            return terr(format!(
                                "argument {} to {parameter} must be a mutable place; bind the expression to a mutable `var` before the call",
                                i + 1
                            ))
                        }
                        }
                    }
                    Convention::Own => {
                        if let Expr::Var(v) = arg {
                            self.consumed.insert(v.clone());
                        }
                    }
                    // An owned value or an immutable borrow: no call-site
                    // obligation (a borrow's no-escape rule is enforced at
                    // native-lowering time by Rust's borrow checker).
                    Convention::Let | Convention::Borrow => {}
                }
            }
        }
        self.reject_externref_cap_aggregate_ty(&ret, &format!("call to `{call_name}`"))?;
        self.reject_structural_authority_ty(&ret, &format!("call to `{call_name}`"))?;
        self.reject_runtime_compiler_syntax_ty(&ret, &format!("call to `{call_name}`"))?;
        Ok(ret)
    }

    fn var_parameter_context(&self, name: &str, index: usize) -> String {
        match self
            .fn_param_names
            .get(name)
            .and_then(|names| names.get(index))
        {
            Some(parameter) => {
                format!("`var` parameter `{parameter}` of `{name}`")
            }
            None => format!("`var` parameter {} of `{name}`", index + 1),
        }
    }

    fn enforce_function_value_conventions(
        &mut self,
        name: &str,
        args: &[Expr],
        conventions: &[Convention],
    ) -> Result<(), TypeError> {
        let mut var_places = Vec::new();
        for (index, (arg, convention)) in args.iter().zip(conventions).enumerate() {
            match convention {
                Convention::Var => {
                    let parameter = self.var_parameter_context(name, index);
                    if matches!(arg, Expr::Unary { op: UnOp::Move, .. }) {
                        return terr(format!(
                            "argument {} to {parameter} uses `move`; write-back requires a live mutable place in the caller",
                            index + 1
                        ));
                    }
                    match var_place(arg) {
                    Some(place) if self.is_mutable(&place.root) == Some(true) => {
                        for (previous_index, previous) in &var_places {
                            if places_overlap(previous, &place) {
                                return terr(format!(
                                    "arguments {} and {} to `{name}` are overlapping `var` places rooted in `{}`",
                                    previous_index + 1,
                                    index + 1,
                                    place.root
                                ));
                            }
                        }
                        self.reject_later_writeback_conflict(name, args, index, &place)?;
                        var_places.push((index, place));
                    }
                    Some(place) => {
                        return terr(format!(
                            "argument {} to {parameter} has immutable root `{}`; root `{}` must be a mutable `var` for write-back",
                            index + 1,
                            place.root,
                            place.root
                        ));
                    }
                    None => {
                        return terr(format!(
                            "argument {} to {parameter} must be a mutable place; bind the expression to a mutable `var` before the call",
                            index + 1
                        ));
                    }
                    }
                }
                Convention::Own => {
                    if let Expr::Var(var) = arg {
                        self.consumed.insert(var.clone());
                    }
                }
                Convention::Let | Convention::Borrow => {}
            }
        }
        Ok(())
    }

    /// Re-validate the convention contract carried by a compiler-owned dynamic
    /// call. Final existential preparation resolves the method into a
    /// compiler-owned node, so this is the point where both backends must
    /// receive identical place, alias, and move guarantees.
    fn enforce_existential_conventions(
        &mut self,
        owner_trait: &str,
        method: &str,
        receiver: &Expr,
        args: &[Expr],
        conventions: &[Convention],
    ) -> Result<(), TypeError> {
        let callee = format!("{owner_trait}.{method}");
        let mut operands = Vec::with_capacity(args.len() + 1);
        operands.push(receiver.clone());
        operands.extend(args.iter().cloned());
        if operands.len() != conventions.len() {
            return terr(format!(
                "internal: existential `{callee}` has {} operand(s) but {} convention(s)",
                operands.len(),
                conventions.len()
            ));
        }

        let mut var_places = Vec::new();
        for (index, (operand, convention)) in operands.iter().zip(conventions).enumerate() {
            match convention {
                Convention::Var => {
                    if matches!(operand, Expr::Unary { op: UnOp::Move, .. }) {
                        return terr(format!(
                            "operand {} to existential `{callee}` uses `move`; write-back requires a live mutable place in the caller",
                            index + 1
                        ));
                    }
                    match var_place(operand) {
                        Some(place) if self.is_mutable(&place.root) == Some(true) => {
                            for (previous_index, previous) in &var_places {
                                if places_overlap(previous, &place) {
                                    return terr(format!(
                                        "operands {} and {} to existential `{callee}` are overlapping `var` places rooted in `{}`",
                                        previous_index + 1,
                                        index + 1,
                                        place.root
                                    ));
                                }
                            }
                            self.reject_later_writeback_conflict(
                                &callee,
                                &operands,
                                index,
                                &place,
                            )?;
                            var_places.push((index, place));
                        }
                        Some(place) => {
                            return terr(format!(
                                "operand {} to existential `{callee}` has immutable root `{}`; root `{}` must be a mutable `var` for write-back",
                                index + 1,
                                place.root,
                                place.root
                            ));
                        }
                        None => {
                            return terr(format!(
                                "operand {} to existential `{callee}` must be a mutable place; bind the expression to a mutable `var` before the call",
                                index + 1
                            ));
                        }
                    }
                }
                Convention::Own => {
                    if let Expr::Var(name) = operand {
                        self.consumed.insert(name.clone());
                    }
                }
                Convention::Let | Convention::Borrow => {}
            }
        }
        Ok(())
    }

    fn call_conventions_for_expr(&self, name: &str) -> Option<Vec<Convention>> {
        self.fn_conventions.get(name).cloned().or_else(|| {
            let ty = self.lookup(name)?;
            match self.resolve(&ty) {
                Ty::Fn(_, _, conventions) => Some(conventions.clone()),
                _ => None,
            }
        })
    }

    fn collect_var_writebacks_in_expr(
        &self,
        expr: &Expr,
        out: &mut Vec<(String, usize, VarPlace)>,
    ) {
        match expr {
            Expr::Call { name, args } => {
                if let Some(conventions) = self.call_conventions_for_expr(name) {
                    for (index, (argument, convention)) in
                        args.iter().zip(&conventions).enumerate()
                    {
                        if *convention == Convention::Var
                            && let Some(place) = var_place(argument)
                        {
                            out.push((name.clone(), index, place));
                        }
                    }
                }
                for argument in args {
                    self.collect_var_writebacks_in_expr(argument, out);
                }
            }
            Expr::ExistentialCall {
                receiver,
                args,
                owner_trait,
                method,
                conventions,
                ..
            } => {
                let callee = format!("{owner_trait}.{method}");
                for (index, (argument, convention)) in std::iter::once(receiver.as_ref())
                    .chain(args.iter())
                    .zip(conventions)
                    .enumerate()
                {
                    if *convention == Convention::Var
                        && let Some(place) = var_place(argument)
                    {
                        out.push((callee.clone(), index, place));
                    }
                }
                self.collect_var_writebacks_in_expr(receiver, out);
                for argument in args {
                    self.collect_var_writebacks_in_expr(argument, out);
                }
            }
            Expr::Apply { func, args } => {
                if let Expr::Var(name) = func.as_ref()
                    && let Some(conventions) = self.call_conventions_for_expr(name)
                {
                    for (index, (argument, convention)) in
                        args.iter().zip(&conventions).enumerate()
                    {
                        if *convention == Convention::Var
                            && let Some(place) = var_place(argument)
                        {
                            out.push((name.clone(), index, place));
                        }
                    }
                }
                self.collect_var_writebacks_in_expr(func, out);
                for argument in args {
                    self.collect_var_writebacks_in_expr(argument, out);
                }
            }
            Expr::Ctor { args, .. }
            | Expr::AnonCtor { args, .. }
            | Expr::List(args)
            | Expr::Tuple(args) => {
                for argument in args {
                    self.collect_var_writebacks_in_expr(argument, out);
                }
            }
            Expr::Unary { expr, .. }
            | Expr::Try(expr)
            | Expr::As { expr, .. }
            | Expr::ExistentialPack { expr, .. }
            | Expr::ExistentialUpcast { expr, .. }
            | Expr::Field { base: expr, .. } => self.collect_var_writebacks_in_expr(expr, out),
            Expr::RecordUpdate { base, fields, .. } => {
                self.collect_var_writebacks_in_expr(base, out);
                for (_, value) in fields {
                    self.collect_var_writebacks_in_expr(value, out);
                }
            }
            Expr::Binary { lhs, rhs, .. }
            | Expr::Index { base: lhs, index: rhs }
            | Expr::Range { lo: lhs, hi: rhs, .. } => {
                self.collect_var_writebacks_in_expr(lhs, out);
                self.collect_var_writebacks_in_expr(rhs, out);
            }
            Expr::If { cond, then_block, else_block } => {
                self.collect_var_writebacks_in_expr(cond, out);
                self.collect_var_writebacks_in_block(then_block, out);
                if let Some(block) = else_block {
                    self.collect_var_writebacks_in_block(block, out);
                }
            }
            Expr::Match { scrutinee, arms } => {
                self.collect_var_writebacks_in_expr(scrutinee, out);
                for arm in arms {
                    if let Some(guard) = &arm.guard {
                        self.collect_var_writebacks_in_expr(guard, out);
                    }
                    self.collect_var_writebacks_in_expr(&arm.body, out);
                }
            }
            Expr::Block(block) => self.collect_var_writebacks_in_block(block, out),
            Expr::While { cond, body } => {
                self.collect_var_writebacks_in_expr(cond, out);
                self.collect_var_writebacks_in_block(body, out);
            }
            Expr::WhileLet { scrutinee, body, .. } => {
                self.collect_var_writebacks_in_expr(scrutinee, out);
                self.collect_var_writebacks_in_block(body, out);
            }
            Expr::For { iter, body, .. } => {
                self.collect_var_writebacks_in_expr(iter, out);
                self.collect_var_writebacks_in_block(body, out);
            }
            // Constructing a lambda does not execute its body, so calls in it do
            // not conflict with a reservation held by the enclosing call.
            Expr::Lambda { .. }
            | Expr::MethodCall { .. }
            | Expr::Record { .. }
            | Expr::LabeledCall { .. }
            | Expr::Int(_)
            | Expr::Duration(_)
            | Expr::Float(_)
            | Expr::Str(_)
            | Expr::Bool(_)
            | Expr::Var(_)
            | Expr::TaggedLit { .. } => {}
        }
    }

    fn collect_var_writebacks_in_block(
        &self,
        block: &Block,
        out: &mut Vec<(String, usize, VarPlace)>,
    ) {
        for statement in &block.stmts {
            match statement {
                Stmt::Let { value, .. }
                | Stmt::Assign { value, .. }
                | Stmt::LetPattern { value, .. }
                | Stmt::Return(Some(value))
                | Stmt::Expr(value)
                | Stmt::Yield(value) => self.collect_var_writebacks_in_expr(value, out),
                Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
            }
        }
    }

    fn reject_later_writeback_conflict(
        &self,
        callee: &str,
        args: &[Expr],
        reserved_index: usize,
        reserved: &VarPlace,
    ) -> Result<(), TypeError> {
        for (later_index, argument) in args.iter().enumerate().skip(reserved_index + 1) {
            let mut writebacks = Vec::new();
            self.collect_var_writebacks_in_expr(argument, &mut writebacks);
            for (nested_callee, nested_index, nested_place) in writebacks {
                if places_overlap(reserved, &nested_place) {
                    return terr(format!(
                        "argument {} to `{callee}` reserves `var` place rooted in `{}` until the call returns, but later argument {} writes back to an overlapping place through argument {} of `{nested_callee}`; written evaluation order keeps the earlier reservation live",
                        reserved_index + 1,
                        reserved.root,
                        later_index + 1,
                        nested_index + 1,
                    ));
                }
            }
        }
        Ok(())
    }

    fn infer_block_expected(&mut self, block: &Block, expected: &Ty) -> Result<Ty, TypeError> {
        self.infer_block_tail_expected(block, expected)
    }

    fn anon_union_variants_for_ty(&self, ty: &Ty) -> Option<(Ty, AnonUnionVariants)> {
        let resolved = self.resolve(ty);
        let Ty::Named(name, union_args) = resolved else {
            return None;
        };
        let variants = anon_union_synthetic_variants(&name)?;
        let mut offset = 0usize;
        let mut out = Vec::with_capacity(variants.len());
        for (variant, arity) in variants {
            let end = offset.checked_add(arity)?;
            if end > union_args.len() {
                return None;
            }
            out.push((variant, union_args[offset..end].to_vec()));
            offset = end;
        }
        (offset == union_args.len()).then_some((Ty::Named(name, union_args), out))
    }

    fn check_anon_ctor(&mut self, tag: &str, args: &[Expr], expected: &Ty) -> Result<(), TypeError> {
        let Some((union_ty, variants)) = self.anon_union_variants_for_ty(expected) else {
            return terr(format!(
                "anonymous union injection `.{tag}` needs an expected anonymous union type; annotate it as `.[{tag}]` or pass it to a parameter/return position with that type"
            ));
        };
        for (variant, fields) in &variants {
            if variant == tag {
                let arity = fields.len();
                if arity != args.len() {
                    return terr(format!(
                        "anonymous union tag `.{tag}` takes {arity} payload value(s) but got {}",
                        args.len()
                    ));
                }
                for (arg, fty) in args.iter().zip(fields) {
                    let at = self.infer_expected(arg, fty)?;
                    self.coerce_arg(fty, &at).map_err(|e| TypeError {
                        message: format!("in anonymous union tag `.{tag}`: {}", e.message),
                    })?;
                }
                return Ok(());
            }
        }
        terr(format!("anonymous union type `{union_ty}` has no tag `.{tag}`"))
    }

    /// Infer a TRANSIENT (desugar-temp) expression WITHOUT recording any of its
    /// sub-node types into the address-keyed `type_record`. A desugared subtree
    /// (a `range`/index/`while let` lowering, built locally and dropped as soon
    /// as inference returns) is never walked by a table consumer — they walk the
    /// original AST — so recording its node addresses is useless, and a soundness
    /// hazard: once the temp is freed, a later allocation reusing its address
    /// would read a stale, wrong type from the table (BUG-004). Suppressing the
    /// recording for these subtrees closes that hole at the source; the persistent
    /// node that desugared (the `Range`/`Index`/`WhileLet` itself) is still
    /// recorded by the enclosing `infer`, with the result type this returns.
    fn infer_transient(&mut self, e: &Expr) -> Result<Ty, TypeError> {
        let saved_types = self.type_record.take();
        let saved_packs = self.existential_pack_record.take();
        let r = self.infer(e);
        self.type_record = saved_types;
        self.existential_pack_record = saved_packs;
        r
    }

    fn infer_inner(&mut self, expr: &Expr) -> Result<Ty, TypeError> {
        match expr {
            // Expanded away by `crate::tagged` during linking, before checking.
            Expr::TaggedLit { tag, .. } => {
                unreachable!("unexpanded tagged literal `{tag}` reached the type checker")
            }
            Expr::Int(_) => Ok(Ty::Int),
            Expr::Float(_) => Ok(Ty::Float),
            Expr::Duration(_) => Ok(Ty::Duration),
            Expr::Str(_) => Ok(Ty::String),
            Expr::Bool(_) => Ok(Ty::Bool),
            Expr::AnonCtor { tag, .. } => terr(format!(
                "anonymous union injection `.{tag}` needs an expected anonymous union type; annotate the binding/return type or pass it to a typed parameter"
            )),
            // A range lowers to a list-building block; type it as that block.
            Expr::Range { lo, hi, inclusive } => {
                let d = witchy_syntax::parser::desugar_range((**lo).clone(), (**hi).clone(), *inclusive);
                self.infer_transient(&d)
            }
            // A subscript lowers to a type-directed read: `list.at(xs, i)` for
            // lists, `dict.at(d, k)` for dicts. Keep the call-shaped transient so
            // the ordinary function signature enforces index/key types and bounds.
            Expr::Index { base, index } => {
                let base_ty = self.infer(base)?;
                let d = if matches!(base_ty, Ty::Named(ref name, _) if name == "Dict") {
                    Expr::Call {
                        name: intrinsics::DICT_AT.to_string(),
                        args: vec![(**base).clone(), (**index).clone()],
                    }
                } else {
                    witchy_syntax::parser::desugar_index((**base).clone(), (**index).clone())
                };
                self.infer_transient(&d)
            }
            Expr::MethodCall { receiver, method, .. } => {
                // Trait lowering resolves every method call (impl, trait
                // bound, or static); one that survives is unresolvable.
                let receiver_ty = self.infer(receiver)?;
                // Trait lowering resolves every valid existential call into a
                // compiler-owned `ExistentialCall`. A dyn receiver surviving
                // here therefore names a method outside its statically declared
                // trait surface; do not fall back to reflection or a guessed
                // method-name lookup.
                if let dyn_ty @ Ty::Dyn(_, _) = self.resolve(&receiver_ty) {
                    return terr(format!(
                        "cannot resolve `.{method}(…)` on `{dyn_ty}` — existential calls are limited to the trait's declared method surface"
                    ));
                }
                terr(format!(
                    "cannot resolve the method call `.{method}(…)` — methods come from \
                     `impl` blocks; a plain function is called as `{method}(value, …)`"
                ))
            }
            Expr::ExistentialCall {
                receiver,
                args,
                result,
                owner_trait,
                method,
                conventions,
                ..
            } => {
                self.infer(receiver)?;
                for arg in args {
                    self.infer(arg)?;
                }
                self.enforce_existential_conventions(
                    owner_trait,
                    method,
                    receiver,
                    args,
                    conventions,
                )?;
                Ok(self.to_ty(result))
            }
            Expr::ExistentialPack { expr, ty, .. } => {
                self.infer(expr)?;
                Ok(self.to_ty(ty))
            }
            Expr::ExistentialUpcast { expr, ty } => {
                self.infer(expr)?;
                Ok(self.to_ty(ty))
            }
            // Named-field record construction is lowered by `witchy_syntax::records`
            // before type-checking.
            Expr::Record { .. } => {
                unreachable!("Expr::Record is lowered by witchy_syntax::records before typeck")
            }
            // (RFC-0056) Labeled calls are resolved to positional `Call`s by
            // `witchy_syntax::keyword_args` at the link layer, before type-checking.
            Expr::LabeledCall { .. } => {
                unreachable!("Expr::LabeledCall is lowered by witchy_syntax::keyword_args before typeck")
            }
            // `while let` lowers to a `while true` over a match; type that.
            Expr::WhileLet { pattern, scrutinee, body } => {
                let d = witchy_syntax::parser::desugar_while_let(
                    pattern.clone(),
                    (**scrutinee).clone(),
                    body.clone(),
                );
                self.infer_transient(&d)
            }
            Expr::List(items) => {
                let elem = self.fresh();
                for it in items {
                    let t = self.infer(it)?;
                    self.unify(&elem, &t)?;
                }
                let ty = Ty::List(Box::new(elem));
                self.reject_externref_cap_aggregate_ty(&ty, "list literal")?;
                self.reject_structural_authority_ty(&ty, "list literal")?;
                Ok(ty)
            }
            Expr::Tuple(items) => {
                let tys = items
                    .iter()
                    .map(|e| self.infer(e))
                    .collect::<Result<Vec<_>, _>>()?;
                let ty = Ty::Tuple(tys);
                self.reject_externref_cap_aggregate_ty(&ty, "tuple literal")?;
                self.reject_structural_authority_ty(&ty, "tuple literal")?;
                Ok(ty)
            }
            Expr::Var(name) => {
                if self.consumed.contains(name) {
                    return terr(format!(
                        "use of `{name}` after it was moved (consumed by an `own` parameter)"
                    ));
                }
                if let Some(t) = self.lookup(name) {
                    return Ok(t);
                }
                // A bare top-level function name used as a value retains its
                // parameter conventions in the function type.
                if let Some((params, ret)) = self.fn_sigs.get(name).cloned() {
                    let typarams: HashSet<u32> = self
                        .fn_typarams
                        .get(name)
                        .into_iter()
                        .flatten()
                        .map(|(_, id)| *id)
                        .collect();
                    let (params, ret) = self.instantiate(&params, &ret, &typarams);
                    let conventions = self
                        .fn_conventions
                        .get(name)
                        .cloned()
                        .unwrap_or_else(|| vec![Convention::Let; params.len()]);
                    let function_ty = Ty::Fn(params, Box::new(ret), conventions);
                    self.reject_externref_cap_aggregate_ty(
                        &function_ty,
                        &format!("function value `{name}`"),
                    )?;
                    return Ok(function_ty);
                }
                if self.trait_method_names.contains(name) {
                    return terr(format!(
                        "trait method `{name}` has no single function value to reference — wrap the receiver dispatch in a lambda, e.g. `fn(x): x.{name}()`"
                    ));
                }
                terr(format!("unbound variable `{name}`"))
            }
            Expr::Lambda { params, body, ret } => {
                // Closures capture by value, so an assignment to a captured
                // (outer) variable cannot propagate out: the interpreter would
                // silently mutate a private copy while the compiled backends can't
                // express it at all. Reject it uniformly here (using the shared
                // AST capture/assignment scan) so every backend agrees.
                let scan = witchy_syntax::lambda_scan::scan_lambda(params, body);
                let outer = scan.assigns_outer();
                if !outer.is_empty() {
                    return terr(format!(
                        "a closure cannot assign to the captured variable `{}` (captures are by value, so the write would be lost) — return the new value or use a `var` parameter instead",
                        outer.join("`, `")
                    ));
                }
                self.push();
                let param_tys: Vec<Ty> = params
                    .iter()
                    .map(|p| match &p.ty {
                        Some(t) => self.to_ty(t),
                        None => self.fresh(),
                    })
                    .collect();
                for (p, ty) in params.iter().zip(&param_tys) {
                    self.define(p.name.clone(), ty.clone(), p.convention.binds_mutable());
                }
                // The closure is its OWN `?` boundary: a `?` in its body propagates
                // to the closure's return type, not the enclosing function's. Use
                // the declared return type if given (`fn(x) -> Result(..): ...`),
                // else a fresh var the body pins. Save/restore the outer return so
                // a `?` after the closure still targets the enclosing function.
                let declared = ret.as_ref().map(|t| self.to_ty(t));
                let lambda_ret = declared.clone().unwrap_or_else(|| self.fresh());
                let saved_ret = self.current_ret.replace(lambda_ret.clone());
                let body_ty = if declared.is_some() {
                    self.infer_block_tail_expected(body, &lambda_ret).map_err(|e| {
                        type_mismatch_context(
                            || "closure body type does not match its declared return type".to_string(),
                            e,
                        )
                    })?
                } else {
                    self.infer_block(body)?
                };
                self.unify(&lambda_ret, &body_ty).map_err(|e| TypeError {
                    message: format!("closure body type does not match its declared return type: {}", e.message),
                })?;
                self.current_ret = saved_ret;
                self.pop();
                let conventions = params.iter().map(|p| p.convention).collect();
                let function_ty = Ty::Fn(param_tys, Box::new(lambda_ret), conventions);
                self.reject_externref_cap_aggregate_ty(&function_ty, "closure value")?;
                Ok(function_ty)
            }
            Expr::Call { name, args } => self.infer_call(name, args, None),
            Expr::Apply { func, args } => {
                // The callee is an arbitrary expression of function type; unify
                // it with `fn(argtys) -> r` and yield `r`.
                let fty = self.infer(func)?;
                if let Ty::Fn(param_tys, ret, conventions) = self.resolve(&fty) {
                    self.reject_externref_cap_aggregate_ty(&fty, "function value")?;
                    if param_tys.len() != args.len() {
                        return terr(format!(
                            "function application expects {} argument(s) but got {}",
                            param_tys.len(),
                            args.len()
                        ));
                    }
                    for (index, (arg, pty)) in args.iter().zip(&param_tys).enumerate() {
                        let at = self.infer_expected(arg, pty)?;
                        self.reject_var_directed_coercion(
                            "function value",
                            index,
                            conventions.get(index),
                            pty,
                            &at,
                        )?;
                        self.coerce_arg(pty, &at).map_err(|e| TypeError {
                            message: format!("in function application: {}", e.message),
                        })?;
                    }
                    self.reject_externref_cap_aggregate_ty(&fty, "function value")?;
                    self.enforce_function_value_conventions(
                        "function value",
                        args,
                        &conventions,
                    )?;
                    return Ok(*ret);
                }
                let mut argtys = Vec::new();
                for arg in args {
                    argtys.push(self.infer(arg)?);
                }
                let ret = self.fresh();
                self.unify(
                    &fty,
                    &Ty::Fn(
                        argtys,
                        Box::new(ret.clone()),
                        vec![Convention::Let; args.len()],
                    ),
                )
                    .map_err(|e| TypeError {
                        message: format!("in function application: {}", e.message),
                    })?;
                self.reject_externref_cap_aggregate_ty(&fty, "function value")?;
                Ok(ret)
            }
            Expr::Ctor { name, args } => {
                if name == "Nil" {
                    if !args.is_empty() {
                        return terr(format!(
                            "constructor `{name}` takes 0 field(s) but got {}",
                            args.len()
                        ));
                    }
                    return Ok(Ty::Unit);
                }
                if let Some((fields, result)) = self.ctor_sigs.get(name).cloned() {
                    let typarams = self.ctor_typarams.get(name).cloned().unwrap_or_default();
                    let (fields, result) = self.instantiate(&fields, &result, &typarams);
                    if fields.len() != args.len() {
                        return terr(format!(
                            "constructor `{name}` takes {} field(s) but got {}",
                            fields.len(),
                            args.len()
                        ));
                    }
                    for (arg, fty) in args.iter().zip(&fields) {
                        let at = if self.ty_is_direct_externref_value(fty) {
                            self.infer(arg)?
                        } else {
                            self.infer_expected(arg, fty)?
                        };
                        // A capability field accepts a broader argument (a full
                        // `Net` into a `Net[Connect]` field), like a call boundary.
                        self.coerce_arg(fty, &at).map_err(|e| TypeError {
                            message: format!("in constructor `{name}`: {}", e.message),
                        })?;
                    }
                    self.reject_externref_cap_aggregate_ty(&result, &format!("constructor `{name}`"))?;
                    self.reject_structural_authority_ty(&result, &format!("constructor `{name}`"))?;
                    Ok(result)
                } else {
                    if self.adt_variants.contains_key(name)
                        || is_builtin_type_name(name)
                        || AMBIENT_STD_TYPE_NAMES.contains(&name.as_str())
                        || is_synthetic_type_name(name)
                    {
                        return terr(format!(
                            "type `{name}` is not a value; use it in a type annotation or call a real constructor/function"
                        ));
                    }
                    // Unknown constructor: still check its arguments, but don't
                    // constrain the result type.
                    for arg in args {
                        self.infer(arg)?;
                    }
                    Ok(self.fresh())
                }
            }
            Expr::Unary { op, expr } => {
                let t = self.infer(expr)?;
                match op {
                    // `move x` has the type of `x`, and consumes it: it's a
                    // use-site ownership transfer, so any later use of the moved
                    // variable is an error on every backend (matching native's
                    // actual move). `infer(expr)` above already rejected moving an
                    // already-consumed binding.
                    UnOp::Move => {
                        if let Expr::Var(v) = expr.as_ref() {
                            self.consumed.insert(v.clone());
                        }
                        Ok(t)
                    }
                    // `await e` has the type of `e` (Phase 1: the awaited value is
                    // the value itself; suspension is invisible to the type).
                    UnOp::Await => Ok(t),
                    UnOp::Not => {
                        self.unify(&Ty::Bool, &t)?;
                        Ok(Ty::Bool)
                    }
                    UnOp::Neg => match self.resolve(&t) {
                        Ty::Float => {
                            self.unify(&t, &Ty::Float)?;
                            Ok(Ty::Float)
                        }
                        // (RFC-0052) `-1s` negates a Duration to a Duration — a
                        // Duration is an exact i64 of milliseconds, so unary minus
                        // is well-defined and both backends negate the i64. This
                        // is why `let d: Duration = -1s` types (the sign folds into
                        // the literal's meaning, not into an Int).
                        Ty::Duration => {
                            self.unify(&t, &Ty::Duration)?;
                            Ok(Ty::Duration)
                        }
                        _ => {
                            self.unify(&t, &Ty::Int)?;
                            Ok(Ty::Int)
                        }
                    },
                    UnOp::BitNot => {
                        self.unify(&Ty::Int, &t)?;
                        Ok(Ty::Int)
                    }
                }
            }
            Expr::Field { base, field } => {
                let bt = self.infer(base)?;
                let resolved = self.resolve(&bt);
                // `pair.0` — a tuple element, by position.
                if let Ok(i) = field.parse::<usize>() {
                    let Ty::Tuple(ts) = &resolved else {
                        return terr(format!(
                            "element access `.{field}` requires a tuple, found `{resolved}`"
                        ));
                    };
                    let Some(t) = ts.get(i) else {
                        return terr(format!(
                            "tuple `{resolved}` has no element `.{i}` (it has {})",
                            ts.len()
                        ));
                    };
                    return Ok(t.clone());
                }
                let Ty::Named(tyname, args) = &resolved else {
                    // (RFC-0072 phase 2) `?` is the renderer's spelling of "not
                    // yet known" — for a user that almost always means an
                    // unbounded generic parameter, so say that instead of
                    // leaking the placeholder alone.
                    if matches!(resolved, Ty::Var(_)) {
                        return terr(format!(
                            "field access `.{field}` requires a record, but this \
                             value's type is not known here — if it is a generic \
                             parameter, witchy has no field constraints on generics: \
                             take the concrete record type (or pass the field's \
                             value) instead"
                        ));
                    }
                    return terr(format!(
                        "field access `.{field}` requires a record, found `{resolved}`"
                    ));
                };
                if self.sealed_types.contains(tyname) {
                    return terr(format!(
                        "`{tyname}` is a sealed capability — its fields are private; \
                         destructure it with `match` inside its own module"
                    ));
                }
                let Some((params, fields)) = self.record_fields.get(tyname).cloned() else {
                    return terr(format!("type `{tyname}` is not a record, so it has no field `{field}`"));
                };
                let Some((_, fty)) = fields.iter().find(|(n, _)| n == field) else {
                    return terr(format!("record `{tyname}` has no field `{field}`"));
                };
                // Instantiate the field type with the value's actual type args.
                let map: HashMap<u32, Ty> =
                    params.iter().cloned().zip(args.iter().cloned()).collect();
                Ok(self.subst_vars(fty, &map))
            }
            Expr::RecordUpdate { name, base, fields } => {
                let bt = self.infer(base)?;
                let resolved = self.resolve(&bt);
                let (base_tyname, base_args) = match &resolved {
                    Ty::Named(n, a) => (n.clone(), a.clone()),
                    other => {
                        return terr(format!("`update` requires a record, found `{other}`"))
                    }
                };
                let tyname = name.clone().unwrap_or_else(|| base_tyname.clone());
                if tyname != base_tyname {
                    return terr(format!(
                        "`{tyname}(..base)` requires a `{tyname}` base, found `{base_tyname}`"
                    ));
                }
                if self.sealed_types.contains(&tyname) {
                    return terr(format!(
                        "`{tyname}` is a sealed capability and cannot be `update`d — \
                         only its own module may construct it"
                    ));
                }
                if self.construction_sealed_types.contains(&tyname)
                    && let Some(home) = tyname
                        .rsplit_once('.')
                        .map(|(home, _)| home)
                        .or_else(|| witchy_syntax::type_resolve::ambient_type_owner(&tyname))
                    && home != self.cur_module
                {
                    return terr(format!(
                        "`{tyname}` is sealed and cannot be `update`d outside its defining \
                         module `{home}`"
                    ));
                }
                let Some((params, rec_fields)) = self.record_fields.get(&tyname).cloned() else {
                    return terr(format!("type `{tyname}` is not a record"));
                };
                let map: HashMap<u32, Ty> =
                    params.into_iter().zip(base_args.iter().cloned()).collect();
                for (fname, vexpr) in fields {
                    let Some((_, fty)) = rec_fields.iter().find(|(n, _)| n == fname) else {
                        return terr(format!("record `{tyname}` has no field `{fname}`"));
                    };
                    let expected = self.subst_vars(fty, &map);
                    let vt = self.infer_expected(vexpr, &expected)?;
                    if !self.existential_coercion(&expected, &vt)? {
                        self.unify(&expected, &vt).map_err(|e| TypeError {
                            message: format!("`update` of field `{fname}`: {}", e.message),
                        })?;
                    }
                }
                Ok(Ty::Named(tyname, base_args))
            }
            Expr::Try(inner) => {
                let it = self.infer(inner)?;
                let resolved = self.resolve(&it);
                let Some(ret) = self.current_ret.clone() else {
                    return terr("`?` can only be used inside a function returning Result or Option");
                };
                let ret = self.resolve(&ret);
                let value_ty = match &resolved {
                    Ty::Named(n, args) if n == "Result" && args.len() == 2 => {
                        match &ret {
                            Ty::Named(rn, rargs) if rn == "Result" && rargs.len() == 2 => {
                                if !self.result_error_compatible(&rargs[1], &args[1])? {
                                    return terr(format!(
                                        "`?` propagates a `{}` error, but the enclosing function returns `{}` \
                                         and no `From({}) for {}` impl exists",
                                        args[1], rargs[1], args[1], rargs[1]
                                    ));
                                }
                                args[0].clone()
                            }
                            _ => {
                                return terr(format!(
                                    "`?` propagates from a `{resolved}`, but the enclosing function returns `{ret}`"
                                ))
                            }
                        }
                    }
                    Ty::Named(n, args) if n == "Option" && args.len() == 1 => {
                        let r = self.fresh();
                        let expected_ret = Ty::Named("Option".into(), vec![r]);
                        self.unify(&ret, &expected_ret).map_err(|e| TypeError {
                            message: format!(
                                "`?` propagates from a `{resolved}`, but the enclosing function returns a different type: {}",
                                e.message
                            ),
                        })?;
                        args[0].clone()
                    }
                    other => {
                        return terr(format!(
                            "`?` expects a Result or Option, found `{other}`"
                        ))
                    }
                };
                Ok(value_ty)
            }
            Expr::As { expr: payload, ty } => {
                let src = self.infer(payload)?;
                let target = self.to_ty(ty);
                // (RFC-0081) Explicit erasure `value as dyn Trait` is a legal
                // cast form of its own — NOT capability narrowing, so
                // `check_narrow` is skipped — with one authority rule: the
                // concrete payload must not carry any capability, directly or
                // nested (v1 has no authority envelope). `to_ty` lowers
                // `Qualified` to the inner type, so a qualified dyn target is
                // already `Ty::Dyn` here.
                if let Ty::Dyn(dyn_name, _) = &target {
                    let resolved_src = self.resolve(&src);
                    if matches!(&resolved_src, Ty::Dyn(_, _))
                        && !self.existential_coercion(&target, &resolved_src)?
                    {
                        return terr(format!(
                            "cannot cast `{resolved_src}` to `{target}` — an existential may only \
                             upcast to one of its transitive supertraits"
                        ));
                    }
                    if matches!(&resolved_src, Ty::Dyn(_, _)) {
                        self.record_existential_upcast(expr, &target, &resolved_src)?;
                        return Ok(target);
                    }
                    if let Some((cap, path)) = self.ty_capability_retention(&resolved_src) {
                        let path = if path.is_empty() {
                            String::new()
                        } else {
                            format!(" through `{}`", path.join(" -> "))
                        };
                        let display_name = existential_bare(dyn_name);
                        return terr(format!(
                            "`as dyn {display_name}`: the concrete payload type `{resolved_src}` \
                             carries a `{cap}` capability{path} — capability-carrying existential \
                             payloads are rejected (RFC-0081); pass the capability explicitly \
                             in method signatures instead"
                        ));
                    }
                    self.record_existential_pack(expr, &target, &src)?;
                    return Ok(target);
                }
                if self.record_width_conformance(&target, &src)? {
                    self.record_record_projection(expr, &target, &src);
                    return Ok(target);
                }
                self.check_narrow(&src, &target)?;
                Ok(target)
            }
            Expr::Binary { op, lhs, rhs } => self.infer_binary(*op, lhs, rhs),
            Expr::If {
                cond,
                then_block,
                else_block,
            } => {
                let ct = self.infer(cond)?;
                self.unify(&Ty::Bool, &ct)
                    .map_err(|e| TypeError { message: format!("`if` condition: {}", e.message) })?;
                let before = self.consumed.clone();
                let tt = self.infer_block(then_block)?;
                let consumed_then = std::mem::replace(&mut self.consumed, before.clone());
                match else_block {
                    Some(eb) => {
                        let et = self.infer_block(eb)?;
                        self.unify(&tt, &et).map_err(|e| TypeError {
                            message: format!("`if` branches disagree: {}", e.message),
                        })?;
                    }
                    None => {
                        self.unify(&tt, &Ty::Unit)?;
                    }
                }
                // A binding consumed on either path is treated as consumed after.
                self.consumed = &consumed_then | &self.consumed;
                Ok(tt)
            }
            Expr::Block(b) => self.infer_block(b),
            Expr::While { cond, body } => {
                let ct = self.infer(cond)?;
                self.unify(&Ty::Bool, &ct).map_err(|e| TypeError {
                    message: format!("`while` condition: {}", e.message),
                })?;
                self.infer_block(body)?;
                Ok(Ty::Unit)
            }
            Expr::For { var, iter, body } => {
                let it = self.infer(iter)?;
                let elem = self.fresh();
                self.unify(&Ty::List(Box::new(elem.clone())), &it).map_err(|e| TypeError {
                    message: format!("`for` expects a List to iterate: {}", e.message),
                })?;
                self.push();
                self.define(var.clone(), elem, false);
                self.infer_block(body)?;
                self.pop();
                Ok(Ty::Unit)
            }
            Expr::Match { scrutinee, arms } => self.infer_match(scrutinee, arms),
        }
    }

    fn infer_binary(&mut self, op: ast::BinOp, lhs: &Expr, rhs: &Expr) -> Result<Ty, TypeError> {
        use ast::BinOp::*;
        let lt = self.infer(lhs)?;
        let rt = self.infer(rhs)?;
        match op {
            Add | Sub | Mul | Div => {
                let lr = self.resolve(&lt);
                let rr = self.resolve(&rt);
                // `+` concatenates strings — type-directed, never coercing: a
                // String operand demands a String on the other side, and a
                // mixed `+` names interpolation as the fix.
                if matches!(lr, Ty::String) || matches!(rr, Ty::String) {
                    if !matches!(op, Add) {
                        return terr("only `+` is defined on String (it concatenates)");
                    }
                    for other in [&lr, &rr] {
                        if !matches!(other, Ty::String | Ty::Var(_)) {
                            return terr(format!(
                                "cannot `+` a String and `{other}` — render the value \
                                 into the string instead: `\"${{a}}${{b}}\"`"
                            ));
                        }
                    }
                    self.unify(&Ty::String, &lt)?;
                    self.unify(&Ty::String, &rt)?;
                    return Ok(Ty::String);
                }
                // Duration arithmetic: durations add/subtract with each other,
                // scale by an Int (multiply / divide), and Duration / Duration is
                // their Int ratio. Mixing a Duration with a bare Int under +/-
                // is a type error.
                if matches!(lr, Ty::Duration) || matches!(rr, Ty::Duration) {
                    return match op {
                        Add | Sub => {
                            self.unify(&Ty::Duration, &lt)?;
                            self.unify(&Ty::Duration, &rt)?;
                            Ok(Ty::Duration)
                        }
                        Mul => {
                            if matches!(lr, Ty::Duration) {
                                self.unify(&Ty::Int, &rt)?;
                            } else {
                                self.unify(&Ty::Int, &lt)?;
                            }
                            Ok(Ty::Duration)
                        }
                        Div => {
                            if matches!(lr, Ty::Duration) && matches!(rr, Ty::Duration) {
                                Ok(Ty::Int)
                            } else if matches!(lr, Ty::Duration) {
                                self.unify(&Ty::Int, &rt)?;
                                Ok(Ty::Duration)
                            } else {
                                terr("an Int cannot be divided by a Duration")
                            }
                        }
                        _ => unreachable!(),
                    };
                }
                let either_float = matches!(lr, Ty::Float) || matches!(rr, Ty::Float);
                let num = if either_float { Ty::Float } else { Ty::Int };
                self.unify(&lt, &num)?;
                self.unify(&rt, &num)?;
                Ok(num)
            }
            Mod | BitAnd | BitOr | BitXor | Shl | Shr => {
                self.unify(&Ty::Int, &lt)?;
                self.unify(&Ty::Int, &rt)?;
                Ok(Ty::Int)
            }
            Concat => {
                // Surface `<>` is gone; the OP survives as compiler-internal IR
                // (codegen flips string `+` to it after annotation), so the
                // annotating run types it and only the user-facing check
                // teaches.
                if self.type_record.is_some() {
                    self.unify(&Ty::String, &lt)?;
                    self.unify(&Ty::String, &rt)?;
                    return Ok(Ty::String);
                }
                terr("`<>` was removed — `+` concatenates strings, and `\"${a}${b}\"` interpolates")
            }
            Eq | NotEq => {
                self.unify(&lt, &rt)?;
                // (RFC-0047) `==`/`!=` desugar through `PartialEq` at every depth.
                // Function and capability types have no meaningful equality — a
                // function's identity is an implementation accident (monomorphization
                // /inlining), and a capability is authority, not data — so comparing
                // them (directly or nested in a container) is a compile-time error,
                // not a backend-dependent answer. This deletes the confirmed
                // `f == f` parity divergence by construction.
                let resolved = self.resolve(&lt);
                if let Some(kind) = self.uncomparable_kind(&resolved, &mut HashSet::new()) {
                    return terr(equality_reject_message(kind, &resolved));
                }
                Ok(Ty::Bool)
            }
            Lt | LtEq | Gt | GtEq => {
                self.unify(&lt, &rt)?;
                // Ordering is defined only for the totally-ordered primitives.
                // Without a type-class mechanism, allowing it on arbitrary types
                // would type-check but crash at runtime, so reject it here.
                match self.resolve(&lt) {
                    Ty::Int | Ty::Float | Ty::String | Ty::Duration => Ok(Ty::Bool),
                    // A `Ty::Var` operand is an unbounded generic type parameter
                    // (it renders as `?`): `<` on it needs an `Ord` bound to
                    // dispatch through the trait, so point at that instead of a
                    // bare "found `?`".
                    other @ Ty::Var(_) if self.type_var_has_bound(&other, &["Ord"]) => {
                        Ok(Ty::Bool)
                    }
                    other @ Ty::Var(_) => terr(format!(
                        "ordering comparison on `{other}` — if this is a generic type \
                         parameter, bound it with `where T: Ord` so `<` dispatches through \
                         the Ord trait"
                    )),
                    other => terr(format!(
                        "ordering comparison requires Int, Float, String, Duration, or a type \
                         that derives `Ord` — found `{other}` (derive it with \
                         `derive(PartialEq, Eq, PartialOrd, Ord)`)"
                    )),
                }
            }
            And => {
                self.unify(&Ty::Bool, &lt)?;
                self.unify(&Ty::Bool, &rt)?;
                Ok(Ty::Bool)
            }
            Or => {
                // `a || b` is Bool-only logical-or (RFC-0048). A non-Bool operand
                // gets a teaching error pointing at `??`, the fallback operator
                // that took over the old truthy/unwrap meanings.
                for t in [self.resolve(&lt), self.resolve(&rt)] {
                    if !matches!(t, Ty::Bool | Ty::Var(_)) {
                        return terr(format!(
                            "`||` is logical-or on Bool, found `{t}`. For a fallback \
                             value use `??`: `name ?? \"anon\"` (Option), \
                             `parse(s) ?? 0` (Result)"
                        ));
                    }
                }
                self.unify(&Ty::Bool, &lt)?;
                self.unify(&Ty::Bool, &rt)?;
                Ok(Ty::Bool)
            }
            Coalesce => {
                // `a ?? b` (RFC-0048): the fallback operator. The left side must
                // be an Option(T) or a Result(T, e); the right side is a T
                // (evaluated only on None/Err); the expression is a T. Nothing
                // else is admissible — no truthiness, no same-typed fallback —
                // so the result type is never ambiguous.
                match self.resolve(&lt) {
                    Ty::Named(ref n, ref args) if n == "Option" && args.len() == 1 => {
                        let t = args[0].clone();
                        self.unify(&t, &rt).map_err(|e| TypeError {
                            message: format!(
                                "`??` fallback: the right side must have the Option's \
                                 payload type: {}",
                                e.message
                            ),
                        })?;
                        Ok(t)
                    }
                    Ty::Named(ref n, ref args) if n == "Result" && args.len() == 2 => {
                        let t = args[0].clone();
                        self.unify(&t, &rt).map_err(|e| TypeError {
                            message: format!(
                                "`??` fallback: the right side must have the Result's \
                                 Ok type: {}",
                                e.message
                            ),
                        })?;
                        Ok(t)
                    }
                    Ty::Var(_) => terr(
                        "the left side of `??` must be an Option or a Result — \
                         annotate it (`let x: Option(Int) = …`) so `??` knows what \
                         to unwrap",
                    ),
                    other => terr(format!(
                        "`??` unwraps an Option or a Result (`opt ?? default`), \
                         found `{other}` on the left"
                    )),
                }
            }
        }
    }

    fn infer_match(&mut self, scrutinee: &Expr, arms: &[MatchArm]) -> Result<Ty, TypeError> {
        self.infer_match_with_expected(scrutinee, arms, None)
    }

    fn infer_match_expected(
        &mut self,
        scrutinee: &Expr,
        arms: &[MatchArm],
        expected: &Ty,
    ) -> Result<Ty, TypeError> {
        self.infer_match_with_expected(scrutinee, arms, Some(expected))
    }

    fn infer_match_with_expected(
        &mut self,
        scrutinee: &Expr,
        arms: &[MatchArm],
        expected: Option<&Ty>,
    ) -> Result<Ty, TypeError> {
        let st = self.infer(scrutinee)?;
        let result = expected.cloned().unwrap_or_else(|| self.fresh());
        let before = self.consumed.clone();
        let mut merged = before.clone();
        for arm in arms {
            self.consumed = before.clone();
            self.push();
            if let Some(dup) = pattern_dup_binding(&arm.pattern) {
                return terr(format!(
                    "pattern binds `{dup}` more than once — each binding in a pattern \
                     must have a distinct name (witchy has no equality patterns)"
                ));
            }
            self.check_pattern(&arm.pattern, &st)?;
            if let Some(guard) = &arm.guard {
                let gt = self.infer(guard)?;
                self.unify(&Ty::Bool, &gt)
                    .map_err(|e| TypeError { message: format!("match guard: {}", e.message) })?;
            }
            let bt = match expected {
                Some(expected) => self.infer_expected(&arm.body, expected)?,
                None => self.infer(&arm.body)?,
            };
            if expected.is_some() {
                self.coerce_arg(&result, &bt).map_err(|e| TypeError {
                    message: format!("match arm disagrees with the expected type: {}", e.message),
                })?;
            } else {
                self.unify(&result, &bt).map_err(|e| TypeError {
                    message: format!("match arms produce different types: {}", e.message),
                })?;
            }
            self.pop();
            merged = &merged | &self.consumed;
        }
        self.consumed = merged;
        // Coverage analysis (exhaustiveness + unreachability) reasons per
        // alternative, so flatten a top-level `Pattern::Or` arm into one synthetic
        // arm per alternative (sharing the guard) — exactly what the old
        // parse-time or-expansion produced, so these checks are unchanged.
        let flat = flatten_or_arms(arms);
        self.check_unreachable(&flat)?;
        self.check_exhaustive(&st, &flat)?;
        Ok(result)
    }

    /// (RFC-0052) Whether a pattern is REFUTABLE in an irrefutable context
    /// (`let`/`for`/comprehension) — returns `Some(teaching-error)` if so, `None`
    /// if it provably always matches. Irrefutable: `_`, a variable, a tuple of
    /// irrefutable patterns (any nesting), and a constructor/record pattern for a
    /// SINGLE-variant type whose fields are all irrefutable. Everything else
    /// (literals, ranges, or-patterns, list patterns, multi-variant constructors)
    /// is refutable — the message points at `if let`.
    fn pattern_refutable(&self, pat: &Pattern, ty: &Ty) -> Option<String> {
        match pat {
            Pattern::Wildcard | Pattern::Var(_) => None,
            Pattern::Tuple(ps) => {
                // Recover each slot type if the expected type is a concrete tuple;
                // otherwise a placeholder (`Ty::Unit`) — only used to type sub-tuple
                // slots, and refutability never depends on a slot's exact type.
                let slots = match self.resolve(ty) {
                    Ty::Tuple(ts) if ts.len() == ps.len() => Some(ts),
                    _ => None,
                };
                for (i, sub) in ps.iter().enumerate() {
                    let sub_ty = slots.as_ref().map(|s| s[i].clone()).unwrap_or(Ty::Unit);
                    if let Some(r) = self.pattern_refutable(sub, &sub_ty) {
                        return Some(r);
                    }
                }
                None
            }
            Pattern::Ctor { name, args } => {
                // The variant's ADT and how many variants it has.
                let adt = self
                    .ctor_sigs
                    .get(name)
                    .and_then(|(_, res)| match res {
                        Ty::Named(adt, _) => Some(adt.clone()),
                        _ => None,
                    });
                let variant_count = adt
                    .as_ref()
                    .and_then(|a| self.adt_variants.get(a))
                    .map(|v| v.len());
                match variant_count {
                    // A multi-variant type's constructor pattern can fail.
                    Some(n) if n > 1 => Some(format!(
                        "`let {} = …` — `{name}` is one of {n} variants of `{}`, so this \
                         pattern can fail. Use `if let {} = …:` (with an else), or `match`.",
                        describe_pattern(pat),
                        adt.as_deref().unwrap_or("?"),
                        describe_pattern(pat),
                    )),
                    // A single-variant record/wrapper is irrefutable IF its fields
                    // are; recurse (field types recovered via check_pattern's own
                    // machinery would need instantiation, so be permissive: use
                    // fresh vars, which never themselves report refutable).
                    _ => {
                        for sub in args {
                            if let Some(r) = self.pattern_refutable(sub, &Ty::Unit) {
                                return Some(r);
                            }
                        }
                        None
                    }
                }
            }
            Pattern::AnonCtor { tag, args } => {
                let Some((_union_ty, variants)) = self.anon_union_variants_for_ty(ty) else {
                    return Some(format!(
                        "`let {} = …` — an anonymous union pattern needs an anonymous union \
                         scrutinee. Use `match` or add a type annotation.",
                        describe_pattern(pat)
                    ));
                };
                if variants.len() > 1 {
                    return Some(format!(
                        "`let {} = …` — `.{tag}` is one of {} tags, so this pattern can fail. \
                         Use `if let {} = …:` (with an else), or `match`.",
                        describe_pattern(pat),
                        variants.len(),
                        describe_pattern(pat),
                    ));
                }
                let fields = variants
                    .iter()
                    .find(|(variant, _)| variant == tag)
                    .map(|(_, fields)| fields.as_slice())
                    .unwrap_or(&[]);
                for (sub, sub_ty) in args.iter().zip(fields) {
                    if let Some(r) = self.pattern_refutable(sub, sub_ty) {
                        return Some(r);
                    }
                }
                None
            }
            // Everything below is refutable in an irrefutable context.
            Pattern::Int(_) | Pattern::Str(_) | Pattern::Bool(_) | Pattern::Duration(_) => {
                Some(format!(
                    "`let {} = …` — a literal pattern can fail to match. Use `if let` \
                     (with an else) or `match`.",
                    describe_pattern(pat)
                ))
            }
            Pattern::IntRange { .. } => Some(
                "a range pattern can fail to match — use `if let` (with an else) or `match`"
                    .to_string(),
            ),
            Pattern::Or(_) => Some(
                "an or-pattern can fail to match — use `if let` (with an else) or `match`"
                    .to_string(),
            ),
            Pattern::List { .. } => Some(
                "a list pattern can fail to match (a list's length is never statically \
                 known) — use `if let` (with an else) or `match`"
                    .to_string(),
            ),
        }
    }

    fn check_pattern(&mut self, pat: &Pattern, expected: &Ty) -> Result<(), TypeError> {
        match pat {
            Pattern::Wildcard => Ok(()),
            Pattern::Var(name) => {
                self.define(name.clone(), expected.clone(), false);
                Ok(())
            }
            Pattern::Int(_) => self.unify(expected, &Ty::Int),
            Pattern::Str(_) => self.unify(expected, &Ty::String),
            Pattern::Bool(_) => self.unify(expected, &Ty::Bool),
            Pattern::Tuple(pats) => {
                let elem_tys: Vec<Ty> = (0..pats.len()).map(|_| self.fresh()).collect();
                self.unify(expected, &Ty::Tuple(elem_tys.clone()))?;
                for (p, t) in pats.iter().zip(elem_tys) {
                    self.check_pattern(p, &t)?;
                }
                Ok(())
            }
            Pattern::List { elems, rest } => {
                let elem = self.fresh();
                self.unify(expected, &Ty::List(Box::new(elem.clone())))?;
                for p in elems {
                    self.check_pattern(p, &elem)?;
                }
                if let Some(Some(name)) = rest {
                    self.define(name.clone(), Ty::List(Box::new(elem)), false);
                }
                Ok(())
            }
            Pattern::Ctor { name, args } => {
                if let Some((fields, result)) = self.ctor_sigs.get(name).cloned() {
                    let typarams = self.ctor_typarams.get(name).cloned().unwrap_or_default();
                    let (fields, result) = self.instantiate(&fields, &result, &typarams);
                    self.unify(expected, &result)?;
                    if fields.len() != args.len() {
                        return terr(format!(
                            "pattern `{name}` takes {} field(s) but matched {}",
                            fields.len(),
                            args.len()
                        ));
                    }
                    for (p, fty) in args.iter().zip(&fields) {
                        self.check_pattern(p, fty)?;
                    }
                    Ok(())
                } else {
                    // Unknown constructor pattern: bind sub-patterns freely.
                    for p in args {
                        let v = self.fresh();
                        self.check_pattern(p, &v)?;
                    }
                    Ok(())
                }
            }
            Pattern::AnonCtor { tag, args } => {
                let Some((union_ty, variants)) = self.anon_union_variants_for_ty(expected) else {
                    return terr(format!(
                        "anonymous union pattern `.{tag}` needs an anonymous union scrutinee, found `{}`",
                        self.resolve(expected)
                    ));
                };
                let Some((_, fields)) = variants.iter().find(|(variant, _)| variant == tag) else {
                    return terr(format!("anonymous union type `{union_ty}` has no tag `.{tag}`"));
                };
                if fields.len() != args.len() {
                    return terr(format!(
                        "anonymous union pattern `.{tag}` takes {} payload pattern(s) but got {}",
                        fields.len(),
                        args.len()
                    ));
                }
                for (p, fty) in args.iter().zip(fields) {
                    self.check_pattern(p, fty)?;
                }
                Ok(())
            }
            // (RFC-0052) A duration literal pattern matches a `Duration` scrutinee.
            Pattern::Duration(_) => self.unify(expected, &Ty::Duration),
            // (RFC-0052) An integer range pattern matches an `Int` scrutinee.
            Pattern::IntRange { lo, hi, inclusive } => {
                self.unify(expected, &Ty::Int)?;
                // A backwards range never matches anything — almost always a typo.
                let empty = if *inclusive { lo > hi } else { lo >= hi };
                if empty {
                    return terr(format!(
                        "range pattern `{lo}..{}{hi}` is empty (matches nothing) — \
                         its low bound is not below its high bound",
                        if *inclusive { "=" } else { "" }
                    ));
                }
                Ok(())
            }
            // (RFC-0052) An or-pattern: every alternative checks against the same
            // expected type AND must bind the SAME names at the SAME types
            // (binding-consistency). We check the first alternative in the real
            // scope (defining its bindings), then verify each further alternative
            // binds an identical name→type set in a throwaway scope.
            Pattern::Or(alts) => {
                let Some((first, rest)) = alts.split_first() else {
                    return terr("empty or-pattern");
                };
                // Snapshot the bindings the first alternative introduces.
                self.push();
                self.check_pattern(first, expected)?;
                let mut first_binds = self.scope_bindings();
                self.pop();
                first_binds.sort_by(|a, b| a.0.cmp(&b.0));
                // Define the first alternative's bindings for real (the arm body /
                // let sees them).
                self.check_pattern(first, expected)?;
                for alt in rest {
                    self.push();
                    self.check_pattern(alt, expected)?;
                    let mut alt_binds = self.scope_bindings();
                    self.pop();
                    alt_binds.sort_by(|a, b| a.0.cmp(&b.0));
                    let names_a: Vec<&String> = first_binds.iter().map(|(n, _)| n).collect();
                    let names_b: Vec<&String> = alt_binds.iter().map(|(n, _)| n).collect();
                    if names_a != names_b {
                        return terr(format!(
                            "or-pattern alternatives must bind the same names — `{}` \
                             binds {{{}}} but another alternative binds {{{}}}",
                            describe_pattern(alt),
                            names_b.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", "),
                            names_a.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", "),
                        ));
                    }
                    for ((n, ta), (_, tb)) in first_binds.iter().zip(&alt_binds) {
                        self.unify(ta, tb).map_err(|e| TypeError {
                            message: format!(
                                "or-pattern binding `{n}` has inconsistent types across \
                                 alternatives: {}",
                                e.message
                            ),
                        })?;
                    }
                }
                Ok(())
            }
        }
    }

    /// Flag arms that can never match because an earlier arm already covers
    /// them — dead code that is almost always a bug (a typo'd duplicate, or arms
    /// placed after a catch-all). Conservative: a guarded arm never establishes
    /// coverage (its guard may fail at runtime), and a constructor arm only
    /// covers its variant when all its fields are irrefutable (`_`/binding), so
    /// `Some(0)` followed by `Some(n)` is correctly left reachable.
    fn check_unreachable(&self, arms: &[MatchArm]) -> Result<(), TypeError> {
        let mut saturated = false;
        let mut ctors: HashSet<String> = HashSet::new();
        let mut ints: HashSet<i64> = HashSet::new();
        let mut strs: HashSet<&str> = HashSet::new();
        let mut bools: HashSet<bool> = HashSet::new();
        // (BUG-294) Duration literal patterns are ordinary `i64`-of-milliseconds
        // literals (RFC-0052), so a duplicate `1s` arm is dead code just like a
        // duplicate `1` — track them the same way Int/Str/Bool are tracked.
        let mut durations: HashSet<i64> = HashSet::new();
        for (i, arm) in arms.iter().enumerate() {
            let already = saturated
                || match &arm.pattern {
                    Pattern::Ctor { name, .. } => ctors.contains(name),
                    Pattern::AnonCtor { tag, .. } => ctors.contains(&format!(".{tag}")),
                    Pattern::Int(n) => ints.contains(n),
                    Pattern::Str(s) => strs.contains(s.as_str()),
                    Pattern::Bool(b) => bools.contains(b),
                    Pattern::Duration(ms) => durations.contains(ms),
                    _ => false,
                };
            // (BUG-295, spec §6: `if let`/`while let` accept ANY pattern) A trailing
            // bare `_` arm is idiomatic AND is exactly the synthesized else-arm an
            // irrefutable `if let x = e:` / `while let x = e:` desugars to
            // (`match e: <irrefutable> -> …; _ -> …`). So a redundant FINAL wildcard is
            // not an error — otherwise `if let x = 3` rejects while the equally-
            // irrefutable `if let (a, b) = p` passes (an inconsistent split). A
            // non-final or non-wildcard duplicate (real dead code, e.g. `1s` then `1s`)
            // is still flagged.
            let is_trailing_catchall =
                i + 1 == arms.len() && arm.guard.is_none() && matches!(arm.pattern, Pattern::Wildcard);
            if already && !is_trailing_catchall {
                return terr(format!(
                    "unreachable match arm: `{}` is already covered by an earlier arm",
                    describe_pattern(&arm.pattern)
                ));
            }
            if arm.guard.is_none() {
                match &arm.pattern {
                    Pattern::Wildcard | Pattern::Var(_) => saturated = true,
                    Pattern::Ctor { name, args }
                        if args
                            .iter()
                            .all(|p| matches!(p, Pattern::Wildcard | Pattern::Var(_))) =>
                    {
                        ctors.insert(name.clone());
                    }
                    Pattern::AnonCtor { tag, args }
                        if args
                            .iter()
                            .all(|p| matches!(p, Pattern::Wildcard | Pattern::Var(_))) =>
                    {
                        // The dotted key keeps anonymous tags disjoint from declared
                        // constructors with the same spelling.
                        ctors.insert(format!(".{tag}"));
                    }
                    Pattern::Int(n) => {
                        ints.insert(*n);
                    }
                    Pattern::Str(s) => {
                        strs.insert(s.as_str());
                    }
                    Pattern::Bool(b) => {
                        bools.insert(*b);
                    }
                    Pattern::Duration(ms) => {
                        durations.insert(*ms);
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    /// If the scrutinee is a known sum type, every variant must be covered (or a
    /// wildcard/variable arm must catch the rest). `Bool` is treated as a
    /// two-variant sum (`true`/`false`), so an incomplete Bool match is rejected
    /// just like an incomplete ADT match.
    fn check_exhaustive(&self, scrut: &Ty, arms: &[MatchArm]) -> Result<(), TypeError> {
        let resolved = self.resolve(scrut);
        let has_catchall = arms.iter().any(|a| {
            a.guard.is_none() && matches!(a.pattern, Pattern::Wildcard | Pattern::Var(_))
        });
        if has_catchall {
            return Ok(());
        }
        if matches!(resolved, Ty::Bool) {
            let covers = |want: bool| {
                arms.iter()
                    .any(|a| a.guard.is_none() && matches!(a.pattern, Pattern::Bool(b) if b == want))
            };
            if covers(true) && covers(false) {
                return Ok(());
            }
            return terr(
                "non-exhaustive match on `Bool`: cover both `true` and `false` (or add `_`)",
            );
        }
        // Infinite/large scalar domains can only be matched exhaustively with a
        // catch-all (an unguarded `_`/variable) — which we'd have accepted above.
        // Reaching here means there is none, so a literal-only or guard-only match
        // is non-exhaustive and would trap at runtime on an unlisted value.
        let scalar = match resolved {
            Ty::Int => Some("Int"),
            Ty::Float => Some("Float"),
            Ty::Duration => Some("Duration"),
            Ty::String => Some("String"),
            _ => None,
        };
        if let Some(kind) = scalar {
            return terr(format!(
                "non-exhaustive match on `{kind}`: it has no finite set of cases, \
                 so add a catch-all `_` arm (a guard does not make a match exhaustive)"
            ));
        }
        // (BUG-293) Tuple/list scrutinees are compound, not `Ty::Named`, so the ADT
        // path below never saw them and a non-exhaustive tuple/list match passed
        // `check` then TRAPPED at runtime. Cover them with the general
        // constructor-matrix algorithm (`rows_exhaustive`): a tuple is a
        // single-constructor product; a list is `[]` (nil) + `[head, ..tail]`
        // (cons). Only unguarded arms count (a guard can fail).
        if matches!(resolved, Ty::Tuple(_) | Ty::List(_)) {
            let rows: Vec<Vec<Pattern>> = arms
                .iter()
                .filter(|a| a.guard.is_none())
                .map(|a| vec![a.pattern.clone()])
                .collect();
            if self.rows_exhaustive(std::slice::from_ref(&resolved), &rows) {
                return Ok(());
            }
            return match resolved {
                Ty::List(_) => terr(
                    "non-exhaustive match on list: cover both the empty list `[]` and a \
                     non-empty list (`[head, ..tail]`), or add a catch-all `_` arm",
                ),
                _ => terr(
                    "non-exhaustive match on tuple: the arms don't cover every combination \
                     of component cases — add the missing case(s) or a catch-all `_` arm",
                ),
            };
        }
        if let Some((union_ty, variants)) = self.anon_union_variants_for_ty(&resolved) {
            let rows: Vec<Vec<Pattern>> = arms
                .iter()
                .filter(|a| a.guard.is_none())
                .map(|a| vec![a.pattern.clone()])
                .collect();
            if self.rows_exhaustive(std::slice::from_ref(&resolved), &rows) {
                return Ok(());
            }
            let covered: HashSet<String> = arms
                .iter()
                .filter(|a| a.guard.is_none())
                .filter_map(|a| match &a.pattern {
                    Pattern::AnonCtor { tag, .. } => Some(format!(".{tag}")),
                    _ => None,
                })
                .collect();
            let missing: Vec<String> = variants
                .iter()
                .filter_map(|(tag, _)| {
                    let tag = format!(".{tag}");
                    (!covered.contains(&tag)).then_some(format!("`{tag}`"))
                })
                .collect();
            if !missing.is_empty() {
                return terr(format!(
                    "non-exhaustive match on `{union_ty}`: missing {}",
                    missing.join(", ")
                ));
            }
            return terr(format!(
                "non-exhaustive match on `{union_ty}`: a tag payload pattern doesn't cover every case \
                 — add a wholesale `.Tag(_)` arm for that tag or a catch-all `_`"
            ));
        }
        let Ty::Named(adt, _) = resolved else {
            return Ok(());
        };
        let Some(variants) = self.adt_variants.get(&adt) else {
            return Ok(());
        };
        // Top-level: every variant of the sum type must appear in an unguarded arm.
        let covered: HashSet<&str> = arms
            .iter()
            .filter(|a| a.guard.is_none())
            .filter_map(|a| match &a.pattern {
                Pattern::Ctor { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        let missing: Vec<&String> = variants.iter().filter(|v| !covered.contains(v.as_str())).collect();
        if !missing.is_empty() {
            // Render home-module names bare (`Blue`, not `t_file.Blue`) and backtick
            // each missing variant (BUG-292).
            let adt_disp = dequalify_home(&adt, &self.cur_module);
            let names = missing
                .iter()
                .map(|s| format!("`{}`", dequalify_home(s, &self.cur_module)))
                .collect::<Vec<_>>()
                .join(", ");
            return terr(format!("non-exhaustive match on `{adt_disp}`: missing {names}"));
        }
        // ...and each present variant must ALSO cover its fields, so a *nested*
        // non-exhaustive match (`Circle(Red)` without `Circle(Blue)`) is caught at
        // check time instead of trapping at runtime. `patterns_cover` reads each
        // nested type from the sub-patterns' own constructors and is permissive on
        // shapes it can't analyze, so it only ever rejects a provably-incomplete
        // match — never a valid one (which is what the earlier shortcut got wrong).
        for v in variants {
            let v_arms: Vec<&[Pattern]> = arms
                .iter()
                .filter(|a| a.guard.is_none())
                .filter_map(|a| match &a.pattern {
                    Pattern::Ctor { name, args } if name == v => Some(args.as_slice()),
                    _ => None,
                })
                .collect();
            if !v_arms.is_empty() && !self.variant_fields_covered(&v_arms) {
                let adt_disp = dequalify_home(&adt, &self.cur_module);
                let v_disp = dequalify_home(v, &self.cur_module);
                return terr(format!(
                    "non-exhaustive match on `{adt_disp}`: `{v_disp}` is matched but its fields \
                     don't cover every case — add a wholesale `{v_disp}(_)` arm or a `_`"
                ));
            }
        }
        Ok(())
    }

    /// Whether one variant's arms jointly cover all its field values. 0 fields →
    /// yes; 1 field → recurse on that column (the common `Some(_)`/`Circle(c)`
    /// case, checked precisely); ≥2 fields → permissive (a full product matrix is
    /// rare and error-prone, so we don't risk rejecting a valid enumeration).
    fn variant_fields_covered(&self, arms: &[&[Pattern]]) -> bool {
        match arms.first().map(|a| a.len()).unwrap_or(0) {
            0 => true,
            1 => {
                let col: Vec<&Pattern> = arms.iter().map(|a| &a[0]).collect();
                self.patterns_cover(&col)
            }
            _ => true,
        }
    }

    /// (BUG-293) The constructors a value of `ty` can take, each with the types of
    /// its fields — the finite signature the exhaustiveness matrix enumerates.
    /// `None` for an *open* type (Int/Float/String/Duration, a type variable, a
    /// capability/function, or an unknown ADT): such a column can only be covered
    /// by a wildcard, so the algorithm stays SOUND (it never rejects a valid match,
    /// only one with a provable hole).
    fn column_ctors(&self, ty: &Ty) -> Option<Vec<ColCtor>> {
        match ty {
            Ty::Bool => Some(vec![
                ColCtor { key: "true".into(), args: Vec::new() },
                ColCtor { key: "false".into(), args: Vec::new() },
            ]),
            Ty::Tuple(comps) => {
                Some(vec![ColCtor { key: TUPLE_CTOR.into(), args: comps.clone() }])
            }
            Ty::List(elem) => Some(vec![
                ColCtor { key: LIST_NIL.into(), args: Vec::new() },
                ColCtor { key: LIST_CONS.into(), args: vec![(**elem).clone(), ty.clone()] },
            ]),
            Ty::Named(adt, args) => {
                if let Some((_union_ty, variants)) = self.anon_union_variants_for_ty(ty) {
                    return Some(
                        variants
                            .into_iter()
                            .map(|(tag, fields)| ColCtor { key: format!(".{tag}"), args: fields })
                            .collect(),
                    );
                }
                let variants = self.adt_variants.get(adt)?;
                Some(
                    variants
                        .iter()
                        .map(|v| ColCtor { key: v.clone(), args: self.variant_arg_types(v, args) })
                        .collect(),
                )
            }
            _ => None,
        }
    }

    /// The field types of variant `v` of a `Named(adt, actual_args)` type, with the
    /// ADT's formal type parameters substituted by `actual_args` — so a
    /// `Pair(a): P(a, a)` matched at `Pair(Bool)` yields fields `[Bool, Bool]`, not
    /// the un-substituted parameter vars (which would look opaque). Any leftover
    /// var stays a var (→ opaque → needs a wildcard), keeping the check sound.
    fn variant_arg_types(&self, v: &str, actual_args: &[Ty]) -> Vec<Ty> {
        let Some((fields, result)) = self.ctor_sigs.get(v) else {
            return Vec::new();
        };
        // The variant's result type carries its ADT's formal parameter vars in
        // order (`Named(adt, [Var(p0), Var(p1), …])`), so zip them to the actual
        // arguments to build the substitution.
        let map: HashMap<u32, Ty> = match result {
            Ty::Named(_, formals) => formals
                .iter()
                .zip(actual_args)
                .filter_map(|(f, a)| match f {
                    Ty::Var(id) => Some((*id, a.clone())),
                    _ => None,
                })
                .collect(),
            _ => HashMap::new(),
        };
        fields.iter().map(|f| self.subst_vars(f, &map)).collect()
    }

    /// (BUG-293) Type-directed exhaustiveness: whether the `rows` (each a pattern
    /// per column) provably cover every value of `types`. The classic
    /// constructor-matrix algorithm — for a column whose type has a finite ctor
    /// signature that the rows mention completely, split per constructor; otherwise
    /// only wildcard rows can cover the rest, so recurse on the default matrix. It
    /// terminates on recursive types (List) because an all-wildcard column takes
    /// the default branch instead of expanding `cons` forever.
    fn rows_exhaustive(&self, types: &[Ty], rows: &[Vec<Pattern>]) -> bool {
        // Expand a head-position or-pattern into one row per alternative, so
        // coverage reasons per alternative (a nested `(1 | 2, x)` contributes both
        // `1` and `2`). Deeper ors surface as column heads after specialization and
        // are expanded by the same step on the recursive call.
        let rows: Vec<Vec<Pattern>> = rows.iter().flat_map(|r| expand_head_or(r)).collect();
        let Some((col_ty, rest)) = types.split_first() else {
            // No columns left: the (empty) value is covered iff a row remains.
            return !rows.is_empty();
        };
        let col_ty = self.resolve(col_ty);
        let full = self.column_ctors(&col_ty);
        let present: HashSet<String> =
            rows.iter().filter_map(|r| pattern_ctor_key(&r[0])).collect();
        let complete = matches!(&full, Some(cs) if cs.iter().all(|c| present.contains(&c.key)));
        if complete {
            // Every constructor is mentioned — the match is exhaustive iff each
            // constructor's specialized sub-matrix is exhaustive.
            for c in full.expect("complete implies Some") {
                let sub_rows: Vec<Vec<Pattern>> =
                    rows.iter().filter_map(|r| specialize_row(r, &c)).collect();
                let mut sub_types = c.args.clone();
                sub_types.extend_from_slice(rest);
                if !self.rows_exhaustive(&sub_types, &sub_rows) {
                    return false;
                }
            }
            true
        } else {
            // Open or incomplete column: only wildcard rows can cover the missing
            // constructors. Drop this column from them and recurse on the rest.
            let default: Vec<Vec<Pattern>> = rows
                .iter()
                .filter(|r| pattern_ctor_key(&r[0]).is_none())
                .map(|r| r[1..].to_vec())
                .collect();
            self.rows_exhaustive(rest, &default)
        }
    }

    /// Whether a set of patterns at one position covers every value of its type.
    /// The type is read from the patterns themselves — a constructor names its
    /// ADT, a `Bool`/`Int`/`Str` literal names a scalar. Returns `true` whenever
    /// coverage cannot be DISPROVEN (lists, tuples, foreign constructors, mixed or
    /// empty rows), so it never rejects a valid match — only one it can prove is
    /// incomplete (a missing variant, or a scalar with no catch-all).
    fn patterns_cover(&self, pats: &[&Pattern]) -> bool {
        if pats.iter().any(|p| matches!(p, Pattern::Wildcard | Pattern::Var(_))) {
            return true;
        }
        if pats.iter().any(|p| matches!(p, Pattern::Bool(_))) {
            let has = |b: bool| pats.iter().any(|p| matches!(p, Pattern::Bool(x) if *x == b));
            return has(true) && has(false);
        }
        // A scalar literal column with no catch-all is an infinite domain — it can
        // never be exhaustively enumerated.
        if pats.iter().any(|p| matches!(p, Pattern::Int(_) | Pattern::Str(_))) {
            return false;
        }
        if !pats.is_empty() && pats.iter().all(|p| matches!(p, Pattern::Ctor { .. })) {
            let Pattern::Ctor { name: first, .. } = pats[0] else {
                return true;
            };
            let Some((_, result)) = self.ctor_sigs.get(first) else {
                return true;
            };
            let Ty::Named(adt, _) = result else {
                return true;
            };
            let Some(variants) = self.adt_variants.get(adt) else {
                return true;
            };
            for v in variants {
                let v_arms: Vec<&[Pattern]> = pats
                    .iter()
                    .filter_map(|p| match p {
                        Pattern::Ctor { name, args } if name == v => Some(args.as_slice()),
                        _ => None,
                    })
                    .collect();
                if v_arms.is_empty() || !self.variant_fields_covered(&v_arms) {
                    return false;
                }
            }
            return true;
        }
        true
    }

    /// The source name of the type parameter appearing in a (rejected) `Dict` key
    /// type, for the "add a `where <k>: Eq`" hint — falls back to `k`.
    fn key_var_name(&self, ty: &Ty) -> String {
        if let Some(id) = first_type_var(ty) {
            for (name, v) in &self.current_typarams {
                if *v == id {
                    return name.clone();
                }
            }
        }
        "k".to_string()
    }

    fn type_var_has_bound(&self, ty: &Ty, traits: &[&str]) -> bool {
        let Some(id) = first_type_var(ty) else { return false };
        let Some(name) = self
            .current_typarams
            .iter()
            .find_map(|(name, v)| (*v == id).then_some(name.as_str()))
        else {
            return false;
        };
        self.current_bounds.iter().any(|(var, tr)| {
            let bare = tr.rsplit('.').next().unwrap_or(tr);
            var == name && traits.contains(&bare)
        })
    }

    fn trait_alternatives(trait_name: &str) -> &'static [&'static str] {
        match trait_name.rsplit('.').next().unwrap_or(trait_name) {
            "PartialEq" => &["PartialEq", "Eq", "PartialOrd", "Ord"],
            "Eq" => &["Eq", "Ord"],
            "PartialOrd" => &["PartialOrd", "Ord"],
            "Ord" => &["Ord"],
            "Show" => &["Show"],
            "Reflect" => &["Reflect"],
            "Deserialize" => &["Deserialize"],
            "From" => &["From"],
            "Into" => &["Into"],
            _ => &[],
        }
    }

    fn require_call_bound(
        &self,
        callee: &str,
        bound_ty: &Ty,
        trait_name: &str,
    ) -> Result<(), TypeError> {
        let resolved = self.resolve(bound_ty);
        if !ty_has_var(&resolved) {
            return Ok(());
        }
        let Some(id) = first_type_var(&resolved) else {
            return Ok(());
        };
        let Some(var_name) = self
            .current_typarams
            .iter()
            .find_map(|(name, v)| (*v == id).then_some(name.as_str()))
        else {
            return Ok(());
        };
        let alternatives = Self::trait_alternatives(trait_name);
        let ok = self.current_bounds.iter().any(|(var, tr)| {
            let bare = tr.rsplit('.').next().unwrap_or(tr);
            var == var_name && alternatives.contains(&bare)
        });
        if ok {
            return Ok(());
        }
        let bare_trait = trait_name.rsplit('.').next().unwrap_or(trait_name);
        terr(format!(
            "`{}` requires `{var_name}: {bare_trait}` at this call to `{}` — add \
             `where {var_name}: {bare_trait}` to the enclosing generic function",
            callee.rsplit('.').next().unwrap_or(callee),
            callee
        ))
    }

    fn check_function(&mut self, func: &Function) -> Result<(), TypeError> {
        borrow_escape_check(func)?;
        let prev_compiler_syntax_allowed = self.compiler_syntax_allowed;
        if func.comptime_only {
            self.compiler_syntax_allowed = true;
        }
        let (params, ret) = self.fn_sigs.get(&func.name).cloned().unwrap();
        self.scopes = vec![HashMap::new()];
        self.consumed.clear();
        self.current_ret = Some(ret.clone());
        self.current_isolated_callback = isolated_vm_callback_contract(
            &func.name,
            func.params.len(),
        )
        .and_then(|(index, _)| func.params.get(index))
        .map(|param| param.name.clone());
        // (BUG-308) Make this function's type parameters visible to `to_ty` so body
        // ascriptions resolve `a` to the signature's parameter var.
        self.current_typarams = self
            .fn_typarams
            .get(&func.name)
            .map(|ps| ps.iter().map(|(n, v)| (n.clone(), *v)).collect())
            .unwrap_or_default();
        self.current_bounds = func
            .bounds
            .iter()
            .map(|(var, tr, _)| (var.clone(), tr.clone()))
            .collect();
        self.dict_key_ops.clear();
        self.cur_line = 0;
        // `func.name` is the canonical `module.fn`; remember the module so home
        // types render bare in this function's diagnostics (BUG-292). The entry
        // module's `main` is left unqualified by the linker, so fall back to the
        // detected entry module for it.
        self.cur_module = func
            .name
            .rsplit_once('.')
            .map_or_else(|| self.entry_module.clone(), |(m, _)| m.to_string());
        for (param, ty) in func.params.iter().zip(&params) {
            self.reject_runtime_compiler_syntax_ty(
                ty,
                &format!("parameter `{}` of `{}`", param.name, diagnostic_callable_name(&func.name)),
            )?;
        }
        self.reject_runtime_compiler_syntax_ty(
            &ret,
            &format!("return type of `{}`", diagnostic_callable_name(&func.name)),
        )?;
        for (param, ty) in func.params.iter().zip(&params) {
            // (RFC-0025) A `frozen` parameter is deeply immutable, so a mutable
            // convention (`var`/`own`, which exist to mutate/consume the argument)
            // contradicts it.
            if param.convention.binds_mutable() {
                if let Some(decl) = &param.ty {
                    if is_frozen_type(decl) {
                        return Err(TypeError {
                            message: format!(
                                "parameter `{}` of `{}` is `frozen` (deeply immutable) but its convention is mutable (`var`/`own`) — a frozen value cannot be mutated; use a plain (read-only) parameter",
                                param.name, func.name
                            ),
                        });
                    }
                }
            }
            self.define(param.name.clone(), ty.clone(), param.convention.binds_mutable());
        }
        let body = if func.ret.is_some() {
            self.infer_block_tail_expected(&func.body, &ret).map_err(|e| {
                type_mismatch_context(
                    || format!("function `{}` body", diagnostic_callable_name(&func.name)),
                    e,
                )
            })?
        } else {
            self.infer_block(&func.body)?
        };
        // A broader capability may be returned where a narrower one is declared
        // (`-> Net[Connect]` returning a full `Net`), mirroring call-argument
        // narrowing; `coerce_arg` falls back to unification for everything else.
        self.coerce_arg(&ret, &body).map_err(|e| TypeError {
            message: format!(
                "function `{}` body: {}",
                diagnostic_callable_name(&func.name),
                e.message
            ),
        })?;
        // (BUG-395 / RFC-0047) A generic `Dict` key operation performed in this
        // (unbounded) body must have an `Eq` key. Now that the body is fully
        // inferred, a key type that still carries a type variable is an UNBOUNDED
        // generic key — the function needs a `where <k>: Eq` bound (a bounded
        // function is checked through monomorphization instead, where the key is
        // concrete). Concrete keys (Int/String/records that derive Eq) are already
        // fine; `Float` is caught separately by `float_key_position`.
        // The std `dict` module's own compositional helpers (`map_values`, `filter`,
        // `merge`, …) key their output dicts with keys drawn from an INPUT `Dict`
        // parameter — whose existence already guarantees the key is `Eq` (a dict
        // can only be built through the bounded insert wrappers). Exempting the
        // trusted std API layer keeps those helpers unbounded (bounding them makes
        // them templates that regress result inference), while USER code that does a
        // generic key op is still enforced.
        let is_std = func
            .name
            .rsplit_once('.')
            .is_some_and(|(m, _)| witchy_syntax::linker::STD_MODULES.contains(&m));
        for (key_ty, line) in std::mem::take(&mut self.dict_key_ops) {
            if is_std {
                continue;
            }
            let resolved = self.resolve(&key_ty);
            if ty_has_var(&resolved) && !self.type_var_has_bound(&resolved, &["Eq", "Ord"]) {
                self.cur_line = line;
                return terr(format!(
                    "`{}`: a generic `Dict` key must be `Eq` — the key type is used to hash \
                     and compare entries, so an unbounded type parameter can't be a key. Add a \
                     `where {}: Eq` bound (or use a concrete key type)",
                    func.name.rsplit('.').next().unwrap_or(&func.name),
                    self.key_var_name(&resolved)
                ));
            }
        }
        // Soundness: a declared type parameter must stay free (truly generic).
        // If the body pinned it to a concrete type, the signature is misleading.
        if let Some(typarams) = self.fn_typarams.get(&func.name).cloned() {
            for (pname, v) in typarams {
                let resolved = self.resolve(&Ty::Var(v));
                if !matches!(resolved, Ty::Var(_)) {
                    // (RFC-0072 phase 2) Say what to DO, not just what's wrong:
                    // the two real fixes are "you meant the concrete type" or
                    // "the body over-constrained the parameter".
                    return terr(format!(
                        "function `{}`: type parameter `{pname}` is used as `{resolved}`, \
                         so it isn't generic — write `{resolved}` in the signature if that \
                         is what you meant, or remove the body expression that pins \
                         `{pname}` to `{resolved}`",
                        func.name
                    ));
                }
            }
        }
        self.compiler_syntax_allowed = prev_compiler_syntax_allowed;
        Ok(())
    }

}

/// Type-check a whole module. Returns the first error found.
pub fn check(module: &Module) -> Result<(), TypeError> {
    check_with_compiler_syntax(module, false)
}

/// Validate declaration-level semantics on the complete expanded source set
/// before the linker lowers generators, async functions, records, or impls.
pub fn check_linked_source_headers(
    source: &witchy_syntax::source_check::ResolvedSource,
) -> Result<(), witchy_syntax::source_check::SourceCheckError> {
    fn source_error(
        module_name: &str,
        module: &Module,
        error: UniquenessError,
    ) -> witchy_syntax::source_check::SourceCheckError {
        let line = module
            .item_lines
            .get(error.item_index)
            .copied()
            .filter(|line| *line != 0);
        let source_error =
            witchy_syntax::source_check::SourceCheckError::new(error.error.message);
        match line {
            Some(line) => source_error.with_location(module_name, line),
            None => source_error,
        }
    }

    for (module_name, module) in source.modules() {
        check_unique_functions(module)
            .map_err(|error| source_error(module_name, module, error))?;
        check_unique_declarations(module)
            .map_err(|error| source_error(module_name, module, error))?;
        check_unique_parameters(module)
            .map_err(|error| source_error(module_name, module, error))?;
        check_nominal_lifetime_declarations(module).map_err(|error| {
            witchy_syntax::source_check::SourceCheckError::new(error.message)
        })?;
    }
    Ok(())
}

/// Type-check the isolated program used to execute a `comptime:` block.
///
/// This mode allows compiler syntax values such as `meta.ItemSyntax` to flow
/// through helper expressions while the ordinary public checker still rejects
/// them from runtime modules.
pub fn check_comptime(module: &Module) -> Result<(), TypeError> {
    check_with_compiler_syntax(module, true)
}

fn check_with_compiler_syntax(module: &Module, compiler_syntax_allowed: bool) -> Result<(), TypeError> {
    // Catch duplicate top-level functions before lowering, while `impl` methods
    // are still distinct from free functions (so overloaded methods aren't
    // mistaken for duplicates) and source lines are still available.
    check_unique_functions(module).map_err(UniquenessError::into_type_error)?;

    // (BUG-230) Duplicate type / constructor / method declarations get the same
    // "defined more than once" error the function namespace already gets. Runs
    // pre-lowering, while `impl`/`type` items are still present and distinct.
    check_unique_declarations(module).map_err(UniquenessError::into_type_error)?;

    // (BUG-444) Parameter names are binding labels, not an overloadable surface:
    // duplicates silently shadow in the checker scope and make keyword labels
    // incoherent. Validate before lowering for source-quality diagnostics.
    check_unique_parameters(module).map_err(UniquenessError::into_type_error)?;

    // RFC-0112 stage 1 runs on the source declaration shape so named-field
    // records, positional shells, and the opt-mode boundary retain precise
    // diagnostics before generator/record lowering.
    check_nominal_lifetime_declarations(module)?;

    // Lower named-field record construction (a no-op once the linker has done so,
    // but covers single-module paths like `check_str`).
    let checked = witchy_syntax::source_check::check(module.clone())
        .map_err(|error| TypeError { message: error.message })?;
    let checked = witchy_syntax::generators::lower(checked)
        .map_err(|message| TypeError { message })?;
    let checked = witchy_syntax::async_lower::lower(checked)
        .map_err(|message| TypeError { message })?;
    let recs = witchy_syntax::records::lower(checked)
        .map_err(|message| TypeError { message })?
        .into_module();
    check_type_names(&recs)?;
    if !compiler_syntax_allowed {
        check_compiler_syntax_declarations(&recs)?;
    }
    check_trait_names(&recs)?;
    // Validate every `dyn Trait` occurrence (identity, existential safety, and
    // v1 exclusions) while trait declarations are still present.
    check_existential_types(&recs.items)?;
    let trait_method_names = collect_trait_method_names(&recs);
    let trait_supertraits = recs
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Trait(definition) => {
                Some((definition.name.clone(), definition.supertraits.clone()))
            }
            _ => None,
        })
        .collect::<HashMap<_, _>>();

    // Trait/impl declarations are desugared to ordinary functions first, so the
    // checker only ever sees plain functions (a no-op for trait-free modules).
    // The checked flavor surfaces unsatisfiable dispatch ("`Float` does not
    // implement `Show`") instead of a post-lowering unknown-function error.
    match crate::traits::lower_checked(recs.clone()) {
        Ok(lowered) => {
            check_unique_parameters(&lowered)
                .map_err(UniquenessError::into_type_error)?;
            run_check_with_trait_methods(
                &lowered,
                false,
                &trait_method_names,
                &trait_supertraits,
                compiler_syntax_allowed,
            )?;
            check_unique_capacity_results(&lowered)?;
            // (RFC-0083) Static lifetime/loan check for borrowed views. Runs after
            // type checking (so a genuine type error is reported first) on the
            // lowered module (method calls are plain `Call`s and the borrow
            // signatures survive lowering as `Qualified(Borrow, _)`).
            crate::loans::check(&lowered)?;
            crate::access::verify_module(&lowered).map_err(|error| TypeError {
                message: error.to_string(),
            })?;
            Ok(())
        }
        Err(message) => {
            // (BUG-307) Mono's "cannot infer the result type" fallback fires when
            // `annotate` returned an EMPTY TypeTable — which is itself a symptom of
            // a REAL checker error elsewhere in the module (a body type error breaks
            // annotate, so every result-position bounded call loses its inferred
            // type and mono then misdiagnoses it). Run the checker on the plainly
            // lowered module first: its genuine error takes priority. The inference
            // fallback only stands when the module otherwise type-checks. Other
            // (genuine) dispatch errors are surfaced unchanged, preserving their
            // teaching message.
            if message.contains("cannot infer the result type") {
                if let Err(real) =
                    run_check_with_trait_methods(
                        &crate::traits::lower(recs),
                        false,
                        &trait_method_names,
                        &trait_supertraits,
                        compiler_syntax_allowed,
                    )
                {
                    // Plain lowering can't resolve the un-inferable bounded call and
                    // leaves it as an unknown function — that artifact IS the same
                    // problem the mono message already describes (and better), so
                    // ignore it and keep the mono message. Any OTHER checker error is
                    // the real one mono masked (a body type error, etc.) — surface it.
                    if !real.message.contains("call to unknown function") {
                        return Err(real);
                    }
                }
            }
            Err(TypeError { message })
        }
    }
}

/// The semantic side tables owned by [`TypedModule`]: expression identity
/// (`&Expr as *const _`) maps to the inferred type and any directed
/// existential-construction request, both finalized against the ending
/// substitution. Ordinary type entries exist only where the type is fully
/// concrete; unresolved existential requests remain present so their final
/// consumer fails loudly.
///
/// This table is deliberately not produced independently of its AST. Raw node
/// addresses are valid identities only while that exact module allocation is
/// alive, so [`annotate`] returns both values in one ownership unit.
#[derive(Default)]
pub struct TypeTable {
    types: HashMap<usize, Ty>,
    functions: HashMap<String, (Vec<Ty>, Ty)>,
    existential_packs: HashMap<usize, (Ty, Ty)>,
    existential_upcasts: HashMap<usize, (Ty, Ty)>,
    record_projections: HashMap<usize, (Ty, Ty)>,
}

impl TypeTable {
    pub fn type_of(&self, e: &Expr) -> Option<&Ty> {
        self.types.get(&(e as *const Expr as usize))
    }

    /// Every concrete expression type in this table. Consumers use this only
    /// while the owning [`TypedModule`] is alive; unlike the address keys, the
    /// returned semantic types are independent of expression identity.
    pub fn concrete_types(&self) -> impl Iterator<Item = &Ty> {
        self.types.values()
    }

    /// The finalized declaration signature for a non-polymorphic function.
    /// Generic declarations remain use-site-specialized and therefore have no
    /// single concrete entry; their concrete function-value expressions still
    /// appear in [`Self::type_of`].
    pub fn function_type(&self, name: &str) -> Option<ast::Type> {
        let (params, result) = self.functions.get(name)?;
        let params = params.iter().map(ty_to_ast).collect::<Option<Vec<_>>>()?;
        let result = ty_to_ast(result)?;
        Some(ast::Type::Fn(params, Box::new(result), Vec::new()))
    }

    /// The finalized `(existential, concrete)` construction requested at this
    /// exact expression node.
    pub fn existential_pack(&self, e: &Expr) -> Option<&(Ty, Ty)> {
        self.existential_packs
            .get(&(e as *const Expr as usize))
    }

    /// The finalized `(target, source)` supertrait conversion requested at this
    /// exact expression node.
    pub fn existential_upcast(&self, e: &Expr) -> Option<&(Ty, Ty)> {
        self.existential_upcasts
            .get(&(e as *const Expr as usize))
    }

    /// The finalized `(target, source)` exact-record projection requested at
    /// this expression node.
    pub fn record_projection(&self, e: &Expr) -> Option<&(Ty, Ty)> {
        self.record_projections
            .get(&(e as *const Expr as usize))
    }

    pub(crate) fn record_projection_count(&self) -> usize {
        self.record_projections.len()
    }
}

/// An AST and the type facts keyed to that exact AST instance.
///
/// Keeping the table under the same owner as the module prevents callers from
/// moving the table away, dropping the annotated nodes, and accidentally
/// consulting it for freshly allocated nodes that reuse an old address.
pub struct TypedModule {
    module: Module,
    table: TypeTable,
}

impl TypedModule {
    pub fn module(&self) -> &Module {
        &self.module
    }

    pub fn table(&self) -> &TypeTable {
        &self.table
    }

    pub fn into_module(self) -> Module {
        self.module
    }

    /// Mutate fields that preserve every expression allocation (for example a
    /// resolved call name or binary-op tag) while consulting the existing facts.
    /// Structural rewrites must instead consume the typed module and re-annotate.
    pub(crate) fn rewrite_preserving_nodes<R>(
        &mut self,
        rewrite: impl FnOnce(&TypeTable, &mut Module) -> R,
    ) -> R {
        rewrite(&self.table, &mut self.module)
    }

    /// Apply a potentially structural rewrite, then unconditionally rebuild
    /// the address-keyed side table. The caller cannot assert that arbitrary
    /// `&mut Module` access preserved node identity.
    pub fn rewrite_and_reannotate(
        mut self,
        rewrite: impl FnOnce(&TypeTable, &mut Module),
    ) -> Self {
        rewrite(&self.table, &mut self.module);
        annotate(self.module)
    }

    /// Perform a final, potentially structural rewrite while the old facts are
    /// available. The table is consumed with this wrapper and cannot be observed
    /// after nodes have been replaced or removed.
    pub(crate) fn rewrite_into_module<R>(
        mut self,
        rewrite: impl FnOnce(&TypeTable, &mut Module) -> R,
    ) -> (Module, TypeTable, R) {
        let result = rewrite(&self.table, &mut self.module);
        (self.module, self.table, result)
    }
}

/// Convert a resolved checker type to the surface `ast::Type` shape the
/// backends' type-directed machinery (eq/to_string shapes, valtypes)
/// consumes. None where no surface form exists (free variables).
pub fn ty_to_ast(t: &Ty) -> Option<witchy_syntax::ast::Type> {
    use witchy_syntax::ast::Type as T;
    let marker = |name: &str| T::Named(name.to_string(), Vec::new());
    Some(match t {
        Ty::Int => T::Named("Int".into(), Vec::new()),
        Ty::Float => T::Named("Float".into(), Vec::new()),
        Ty::Duration => T::Named("Duration".into(), Vec::new()),
        Ty::String => T::Named("String".into(), Vec::new()),
        Ty::Bytes => T::Named("Bytes".into(), Vec::new()),
        Ty::Msg => T::Named("__Msg".into(), Vec::new()),
        Ty::Bool => T::Named("Bool".into(), Vec::new()),
        Ty::Unit => T::Named("Nil".into(), Vec::new()),
        Ty::Console(rights) => {
            if !rights.read && !rights.write {
                return None;
            }
            let mut args = Vec::new();
            if *rights != ConsoleRights::full() {
                if rights.read {
                    args.push(marker("Read"));
                }
                if rights.write {
                    args.push(marker("Write"));
                }
            }
            T::Named("Console".into(), args)
        }
        Ty::Clock => T::Named("Clock".into(), Vec::new()),
        Ty::Rand => T::Named("Rand".into(), Vec::new()),
        Ty::Env => T::Named("Env".into(), Vec::new()),
        Ty::Secret => T::Named("Secret".into(), Vec::new()),
        Ty::Exec => T::Named("Exec".into(), Vec::new()),
        Ty::Fetch => T::Named("Fetch".into(), Vec::new()),
        Ty::Dir(rights) => {
            if !rights.read && !rights.write {
                return None;
            }
            let mut args = Vec::new();
            if *rights != DirRights::full() {
                if rights.read {
                    args.push(marker("Read"));
                }
                if rights.write {
                    args.push(marker("Write"));
                }
            }
            T::Named("Dir".into(), args)
        }
        Ty::File(rights) => {
            if !rights.read && !rights.write {
                return None;
            }
            let mut args = Vec::new();
            if *rights != FileRights::full() {
                if rights.read {
                    args.push(marker("Read"));
                }
                if rights.write {
                    args.push(marker("Write"));
                }
            }
            T::Named("File".into(), args)
        }
        Ty::Net(rights) => {
            if (!rights.connect && !rights.listen)
                || (!rights.tcp && !rights.udp && !rights.uds)
            {
                return None;
            }
            let mut args = Vec::new();
            if !rights.verbs_full() {
                if rights.connect {
                    args.push(marker("Connect"));
                }
                if rights.listen {
                    args.push(marker("Listen"));
                }
            }
            if !rights.transports_full() {
                if rights.tcp {
                    args.push(marker("Tcp"));
                }
                if rights.udp {
                    args.push(marker("Udp"));
                }
                if rights.uds {
                    args.push(marker("Uds"));
                }
            }
            T::Named("Net".into(), args)
        }
        Ty::Socket => T::Named("Socket".into(), Vec::new()),
        Ty::Listener => T::Named("Listener".into(), Vec::new()),
        Ty::BuildOut | Ty::BuildRead | Ty::BuildEnv | Ty::BuildNet | Ty::BuildExec => {
            return None
        }
        Ty::List(e) => T::Named("List".into(), vec![ty_to_ast(e)?]),
        Ty::Tuple(ts) => {
            T::Tuple(ts.iter().map(ty_to_ast).collect::<Option<Vec<_>>>()?)
        }
        Ty::Named(n, args) => T::Named(
            n.clone(),
            args.iter().map(ty_to_ast).collect::<Option<Vec<_>>>()?,
        ),
        // (RFC-0081) The existential surface form: `dyn Render(args…)`.
        Ty::Dyn(n, args) => T::Dyn(
            n.clone(),
            args.iter().map(ty_to_ast).collect::<Option<Vec<_>>>()?,
        ),
        Ty::Fn(params, ret, conventions) => T::Fn(
            params.iter().map(ty_to_ast).collect::<Option<Vec<_>>>()?,
            Box::new(ty_to_ast(ret)?),
            conventions.clone(),
        ),
        Ty::Var(_) => return None,
    })
}

fn ty_has_var(t: &Ty) -> bool {
    match t {
        Ty::Var(_) => true,
        Ty::List(e) => ty_has_var(e),
        Ty::Tuple(ts) => ts.iter().any(ty_has_var),
        Ty::Named(_, args) | Ty::Dyn(_, args) => args.iter().any(ty_has_var),
        Ty::Fn(ps, r, _) => ps.iter().any(ty_has_var) || ty_has_var(r),
        _ => false,
    }
}

/// Annotate an ALREADY-LOWERED module instance (the exact AST a consumer will
/// keep walking): the typed-lowering keystone (rfcs/language-evolution.md
/// Phase 0). Best-effort by contract — consumers only annotate modules that
/// already passed `check`, so any error here yields an empty table and the
/// consumer's own fallbacks apply.
pub fn annotate(module: Module) -> TypedModule {
    annotate_with_conversion_fns(module, None)
}

/// Annotate an already-lowered module while preserving checker failures.
///
/// Backend preparation uses this after compiler-owned nodes such as
/// `ExistentialCall` have been introduced. Those nodes can carry new semantic
/// obligations that did not exist during source checking, so silently replacing
/// their diagnostics with an empty table would turn a required rejection into a
/// later generic "unsupported lowering" error.
pub fn annotate_checked(module: Module) -> Result<TypedModule, TypeError> {
    // Frontend checking already enforced the compiler-syntax boundary before
    // linking and comptime expansion. The executable AST can legitimately retain
    // std/meta declarations (and an isolated `comptime` entry needs their values),
    // so backend reannotation must not reapply that phase-sensitive source rule.
    // All ordinary type checks, including existential obligations introduced by
    // compiler-owned nodes, still run and their failures remain observable.
    let table = run_check_selected(
        &module,
        true,
        None,
        None,
        None,
        None,
        true,
    )?
        .unwrap_or_default();
    Ok(TypedModule { module, table })
}

/// Reannotate a build module after its checked `build` entrypoint has been
/// renamed to the runtime's internal `main` export.
///
/// Node identities survive the temporary name restoration, so the resulting
/// table still belongs to `module`. This keeps the ordinary public `main`
/// contract strict while validating the generated entrypoint under the build
/// capability contract.
pub fn annotate_checked_build(mut module: Module) -> Result<TypedModule, TypeError> {
    let mut renamed_entry = None;
    for (index, item) in module.items.iter_mut().enumerate() {
        if let Item::Function(function) = item
            && function.name == "main"
        {
            function.name = "build".to_string();
            renamed_entry = Some(index);
            break;
        }
    }
    let mut table = run_check_selected(
        &module,
        true,
        None,
        None,
        None,
        None,
        true,
    )?
        .unwrap_or_default();
    if let Some(index) = renamed_entry {
        if let Item::Function(function) = &mut module.items[index] {
            function.name = "main".to_string();
        }
        if let Some(signature) = table.functions.remove("build") {
            table.functions.insert("main".to_string(), signature);
        }
    }
    Ok(TypedModule { module, table })
}

fn annotate_with_conversion_fns(
    module: Module,
    from_conversion_fns: Option<&HashSet<String>>,
) -> TypedModule {
    let table = match run_check_selected(
        &module,
        true,
        None,
        None,
        None,
        from_conversion_fns,
        false,
    ) {
        Ok(Some(table)) => table,
        Err(e) => {
            if std::env::var_os("WITCHY_DEBUG_ANNOTATE").is_some() {
                eprintln!("annotate: checker error on lowered module: {e}");
            }
            TypeTable::default()
        }
        _ => TypeTable::default(),
    };
    TypedModule { module, table }
}

/// Annotate a trait-lowering intermediate while preserving the semantic
/// identity of resolved `From.from` implementations. The ordinary public
/// annotation path sees an already-lowered module and needs no such context.
pub(crate) fn annotate_with_from_conversions(
    module: Module,
    from_conversion_fns: &HashSet<String>,
) -> TypedModule {
    annotate_with_conversion_fns(module, Some(from_conversion_fns))
}

/// The entry module — the home of the unqualified `main` — for home-module
/// de-qualification in `main`'s own diagnostics (BUG-292). The linker emits the
/// entry module's items FIRST and qualifies every declaration except the bare
/// `main`, so the first `module.`-qualified item appearing BEFORE `main` belongs
/// to the entry module. An entry that declares only `main` (no qualified item
/// before it) yields "" — there are no home types to strip, and `main`'s
/// references to imported types must keep their qualifiers. When there is no bare
/// `main` at all (a library/comptime unit), fall back to the first qualified item.
fn detect_entry_module(module: &Module) -> String {
    let prefix = |name: &str| name.rsplit_once('.').map(|(m, _)| m.to_string());
    let mut before: Option<String> = None;
    for item in &module.items {
        match item {
            Item::Function(f) if f.name == "main" => return before.unwrap_or_default(),
            Item::Type(t) => before = before.or_else(|| prefix(&t.name)),
            Item::Function(f) => before = before.or_else(|| prefix(&f.name)),
            _ => {}
        }
    }
    before.unwrap_or_default()
}

pub(crate) fn check_selected_lowered(
    module: &Module,
    names: &HashSet<String>,
    from_conversion_fns: &HashSet<String>,
) -> Result<(), TypeError> {
    run_check_selected(
        module,
        false,
        Some(names),
        None,
        None,
        Some(from_conversion_fns),
        false,
    )
        .map(|_| ())
}

fn run_check_with_trait_methods(
    module: &Module,
    record: bool,
    trait_method_names: &HashSet<String>,
    trait_supertraits: &HashMap<String, Vec<String>>,
    compiler_syntax_allowed: bool,
) -> Result<Option<TypeTable>, TypeError> {
    run_check_selected(
        module,
        record,
        None,
        Some(trait_method_names),
        Some(trait_supertraits),
        None,
        compiler_syntax_allowed,
    )
}

fn run_check_selected(
    module: &Module,
    record: bool,
    selected_functions: Option<&HashSet<String>>,
    trait_method_names: Option<&HashSet<String>>,
    trait_supertraits: Option<&HashMap<String, Vec<String>>>,
    from_conversion_fns: Option<&HashSet<String>>,
    compiler_syntax_allowed: bool,
) -> Result<Option<TypeTable>, TypeError> {
    let module = &module;
    let mut c = Checker {
        type_record: if record { Some(HashMap::new()) } else { None },
        // Construction requests are also retained during ordinary checking so
        // capability payloads resolved only by the ending substitution are
        // rejected before annotation or lowering.
        existential_pack_record: Some(HashMap::new()),
        existential_upcast_record: Some(HashMap::new()),
        record_projection_record: Some(HashMap::new()),
        fn_sigs: HashMap::new(),
        from_conversion_fns: from_conversion_fns.cloned().unwrap_or_default(),
        fn_conventions: HashMap::new(),
        fn_param_names: HashMap::new(),
        ctor_sigs: HashMap::new(),
        ctor_typarams: HashMap::new(),
        record_fields: HashMap::new(),
        sealed_types: HashSet::new(),
        construction_sealed_types: HashSet::new(),
        transparent_externref_brands: HashMap::new(),
        gc_cap_aggregates: HashSet::new(),
        adt_variants: HashMap::new(),
        fn_typarams: HashMap::new(),
        fn_bounds: HashMap::new(),
        trait_method_names: trait_method_names.cloned().unwrap_or_else(|| collect_trait_method_names(module)),
        trait_supertraits: trait_supertraits.cloned().unwrap_or_else(|| {
            module
                .items
                .iter()
                .filter_map(|item| match item {
                    Item::Trait(definition) => {
                        Some((definition.name.clone(), definition.supertraits.clone()))
                    }
                    _ => None,
                })
                .collect()
        }),
        current_typarams: HashMap::new(),
        current_bounds: Vec::new(),
        subst: HashMap::new(),
        next_var: 0,
        scopes: vec![HashMap::new()],
        consumed: HashSet::new(),
        region_locals: Vec::new(),
        current_ret: None,
        current_isolated_callback: None,
        dict_key_ops: Vec::new(),
        cur_line: 0,
        cur_module: String::new(),
        entry_module: module
            .linked_entry
            .clone()
            .unwrap_or_else(|| detect_entry_module(module)),
        compiler_syntax_allowed,
    };

    let option_a = c.fresh();
    let option_id = match option_a {
        Ty::Var(id) => id,
        _ => unreachable!("fresh type variable"),
    };
    let option_result = Ty::Named("Option".into(), vec![option_a.clone()]);
    let mut option_typarams = HashSet::new();
    option_typarams.insert(option_id);
    c.ctor_sigs.insert("Some".into(), (vec![option_a.clone()], option_result.clone()));
    c.ctor_sigs.insert("None".into(), (Vec::new(), option_result));
    c.ctor_typarams.insert("Some".into(), option_typarams.clone());
    c.ctor_typarams.insert("None".into(), option_typarams);
    c.adt_variants.insert("Option".into(), vec!["Some".into(), "None".into()]);

    let result_ok = c.fresh();
    let result_err = c.fresh();
    let (result_ok_id, result_err_id) = match (&result_ok, &result_err) {
        (Ty::Var(ok), Ty::Var(err)) => (*ok, *err),
        _ => unreachable!("fresh type variable"),
    };
    let result = Ty::Named("Result".into(), vec![result_ok.clone(), result_err.clone()]);
    let mut result_typarams = HashSet::new();
    result_typarams.insert(result_ok_id);
    result_typarams.insert(result_err_id);
    c.ctor_sigs.insert("Ok".into(), (vec![result_ok], result.clone()));
    c.ctor_sigs.insert("Err".into(), (vec![result_err], result));
    c.ctor_typarams.insert("Ok".into(), result_typarams.clone());
    c.ctor_typarams.insert("Err".into(), result_typarams);
    c.adt_variants.insert("Result".into(), vec!["Ok".into(), "Err".into()]);

    // Pass 1: collect all signatures so definitions can refer to each other.
    let type_defs: HashMap<&str, &ast::TypeDef> = module
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Type(t) => Some((t.name.as_str(), t)),
            _ => None,
        })
        .collect();
    for item in &module.items {
        match item {
            Item::Function(f) => {
                let mut vars: HashMap<String, Ty> = HashMap::new();
                let params: Vec<Ty> = f
                    .params
                    .iter()
                    .map(|p| match &p.ty {
                        Some(t) => c.to_ty_generic(t, &mut vars),
                        None => c.fresh(),
                    })
                    .collect();
                let ret = match &f.ret {
                    Some(t) => c.to_ty_generic(t, &mut vars),
                    None => c.fresh(),
                };
                c.fn_sigs.insert(f.name.clone(), (params, ret));
                let typarams: Vec<(String, u32)> = vars
                    .iter()
                    .filter_map(|(name, ty)| match ty {
                        Ty::Var(v) => Some((name.clone(), *v)),
                        _ => None,
                    })
                    .collect();
                let bounds: Vec<(u32, String)> = f
                    .bounds
                    .iter()
                    .filter_map(|(var, tr, _)| match vars.get(var) {
                        Some(Ty::Var(v)) => Some((*v, tr.clone())),
                        _ => None,
                    })
                    .collect();
                c.fn_typarams.insert(f.name.clone(), typarams);
                c.fn_bounds.insert(f.name.clone(), bounds);
                c.fn_conventions
                    .insert(f.name.clone(), f.params.iter().map(|p| p.convention).collect());
                c.fn_param_names
                    .insert(f.name.clone(), f.params.iter().map(|p| p.name.clone()).collect());
            }
            Item::Type(t) => {
                // A type's parameters: explicit ones (`type Step(m, a):`) FIX the
                // order; otherwise infer them from the variant field types in order
                // of first appearance (so `type Option { Some(a) None }` has one
                // param `a`). Explicit params are required when a constructor omits
                // one (e.g. `Done(a)` for `Step(m, a)`): inference would drop the
                // omitted `m` from that constructor's result type, mis-aligning it.
                let param_names = type_def_params(t);
                let mut vars: HashMap<String, Ty> = HashMap::new();
                let mut typaram_ids: HashSet<u32> = HashSet::new();
                let mut params_in_order: Vec<u32> = Vec::new();
                let mut result_args: Vec<Ty> = Vec::new();
                for pn in &param_names {
                    let v = c.fresh();
                    if let Ty::Var(id) = v {
                        typaram_ids.insert(id);
                        params_in_order.push(id);
                    }
                    vars.insert(pn.clone(), v.clone());
                    result_args.push(v);
                }
                let result = Ty::Named(t.name.clone(), result_args);
                if t.sealed {
                    c.construction_sealed_types.insert(t.name.clone());
                }
                let mut names = Vec::new();
                for variant in &t.variants {
                    let fields: Vec<Ty> = variant
                        .fields
                        .iter()
                        .map(|ft| c.to_ty_generic(ft, &mut vars))
                        .collect();
                    // A record variant carries field names: remember them (with
                    // the type's parameters) so `value.field` can be typed.
                    if !variant.field_names.is_empty() {
                        let rec: Vec<(String, Ty)> = variant
                            .field_names
                            .iter()
                            .cloned()
                            .zip(fields.iter().cloned())
                            .collect();
                        c.record_fields
                            .insert(t.name.clone(), (params_in_order.clone(), rec));
                        // Field privacy is a CAPABILITY property (its carried
                        // authority is secret), not a general sealing one. A
                        // `sealed type` (RFC-0065) seals only CONSTRUCTION; its
                        // fields stay readable/matchable (DoD item 4), so only a
                        // `capability` goes in `sealed_types`.
                        if t.sealed && t.is_capability {
                            c.sealed_types.insert(t.name.clone());
                        }
                    }
                    c.ctor_sigs
                        .insert(variant.name.clone(), (fields, result.clone()));
                    c.ctor_typarams
                        .insert(variant.name.clone(), typaram_ids.clone());
                    names.push(variant.name.clone());
                }
                c.adt_variants.insert(t.name.clone(), names);
            }
            // Desugared to functions by `traits::lower` and constants inlined by
            // `witchy_syntax::consts` before this point.
            Item::Trait(_) | Item::Impl(_) | Item::Const { .. } | Item::TypeAlias { .. } | Item::Comptime(_) => {}
        }
    }
    for item in &module.items {
        if let Item::Type(t) = item
            && let Some(cap) = transparent_externref_brand_cap(&t.name, &type_defs, &mut HashSet::new())
            && !is_externref_cap(&t.name)
        {
            c.transparent_externref_brands.insert(t.name.clone(), cap);
        }
    }
    for name in gc_cap_aggregate_names(module) {
        c.gc_cap_aggregates.insert(name);
    }

    // Reject typo'd / undeclared type names in signatures before they become
    // opaque types that mis-unify with a confusing message later.
    check_type_names(module)?;

    // `main` is the root entrypoint: its parameters are where the host's authority
    // enters, so they must be capabilities (or the args list) — validate before
    // diving into bodies so a malformed entry point is reported up front.
    check_dynamic_method_declarations(module)?;
    check_main_signature(module)?;
    // RFC-0038: a `grantable` capability must be BARE (no transitive host authority),
    // checked module-wide (a grantable cap is invalid regardless of `main`).
    check_grantable_caps(module)?;
    // RFC-0040: a cap-gated string export must lead with a bare grantable capability.
    check_export_signatures(module)?;
    // A rune's `build` entrypoint is the root of the build sandbox; its parameters
    // are where build-time authority enters, so they must be build capabilities.
    check_build_signature(module)?;

    // Pass 2: check bodies. Ordinary runs skip bounded generic templates because
    // their trait-dispatch placeholders are resolved by monomorphization. The
    // selected-template path above opts back in for declaration-time sanity
    // checks before trait lowering drops uninstantiated templates.
    for item in &module.items {
        match item {
            Item::Function(f)
                if selected_functions.is_some_and(|names| !names.contains(&f.name)) => {}
            Item::Function(f) if selected_functions.is_none() && !f.bounds.is_empty() => {}
            Item::Function(f) => c
                .check_function(f)
                .map_err(|e| at_loc(e, c.cur_line, &f.name, &c.cur_module))?,
            Item::Type(_) | Item::Trait(_) | Item::Impl(_) | Item::Const { .. } | Item::TypeAlias { .. } | Item::Comptime(_) => {}
        }
    }
    let mut existential_packs = HashMap::new();
    if let Some(records) = c.existential_pack_record.take() {
        for (key, (existential, concrete)) in records {
            let existential = c.resolve(&existential);
            let concrete = c.resolve(&concrete);
            if let Some((cap, path)) = c.ty_capability_retention(&concrete) {
                let path = if path.is_empty() {
                    String::new()
                } else {
                    format!(" through `{}`", path.join(" -> "))
                };
                let dyn_name = match &existential {
                    Ty::Dyn(name, _) => existential_bare(name),
                    _ => "existential",
                };
                return terr(format!(
                    "conversion to `dyn {dyn_name}`: the concrete payload type `{concrete}` \
                     carries a `{cap}` capability{path} — capability-carrying existential \
                     payloads are rejected (RFC-0081); pass the capability explicitly \
                     in method signatures instead"
                ));
            }
            if record {
                existential_packs.insert(key, (existential, concrete));
            }
        }
    }
    let mut existential_upcasts = HashMap::new();
    if let Some(records) = c.existential_upcast_record.take() {
        for (key, (target, source)) in records {
            let target = c.resolve(&target);
            let source = c.resolve(&source);
            if record {
                existential_upcasts.insert(key, (target, source));
            }
        }
    }
    let mut record_projections = HashMap::new();
    if let Some(records) = c.record_projection_record.take() {
        for (key, (target, source)) in records {
            let target = c.resolve(&target);
            let source = c.resolve(&source);
            if record {
                record_projections.insert(key, (target, source));
            }
        }
    }
    if let Some(rec) = c.type_record.take() {
        let mut types = HashMap::new();
        for (k, ty) in rec {
            let resolved = c.resolve(&ty);
            if !ty_has_var(&resolved) {
                types.insert(k, resolved);
            }
        }
        let functions = c
            .fn_sigs
            .iter()
            .filter_map(|(name, (params, result))| {
                let params = params.iter().map(|param| c.resolve(param)).collect::<Vec<_>>();
                let result = c.resolve(result);
                (!params.iter().any(ty_has_var) && !ty_has_var(&result))
                    .then(|| (name.clone(), (params, result)))
            })
            .collect();
        return Ok(Some(TypeTable {
            types,
            functions,
            existential_packs,
            existential_upcasts,
            record_projections,
        }));
    }
    Ok(None)
}

/// The first name a pattern binds twice, if any. A pattern like `P(x, x)` must be
/// a compile error: witchy has no non-linear patterns (binding the same name in
/// two positions would silently shadow rather than constrain the two to be
/// equal), so we reject it rather than pick a winner.
fn pattern_dup_binding(p: &Pattern) -> Option<String> {
    fn walk(p: &Pattern, seen: &mut HashSet<String>, dup: &mut Option<String>) {
        if dup.is_some() {
            return;
        }
        match p {
            Pattern::Var(n) => {
                if !seen.insert(n.clone()) {
                    *dup = Some(n.clone());
                }
            }
            Pattern::Tuple(ps) => ps.iter().for_each(|q| walk(q, seen, dup)),
            Pattern::Ctor { args, .. } | Pattern::AnonCtor { args, .. } => {
                args.iter().for_each(|q| walk(q, seen, dup));
            }
            Pattern::List { elems, rest } => {
                elems.iter().for_each(|q| walk(q, seen, dup));
                if let Some(Some(name)) = rest {
                    if !seen.insert(name.clone()) {
                        *dup = Some(name.clone());
                    }
                }
            }
            // (RFC-0052) Each or-pattern alternative is an INDEPENDENT binding set
            // (they bind the same names by the consistency rule), so a name reused
            // ACROSS alternatives is not a duplicate — check each alternative with
            // its own `seen`, catching only a within-alternative repeat.
            Pattern::Or(alts) => {
                for alt in alts {
                    let mut alt_seen = HashSet::new();
                    walk(alt, &mut alt_seen, dup);
                    if dup.is_some() {
                        return;
                    }
                }
            }
            _ => {}
        }
    }
    let mut seen = HashSet::new();
    let mut dup = None;
    walk(p, &mut seen, &mut dup);
    dup
}

/// Module-qualified native primitives: declared in std as signature-bearing
/// placeholders, intercepted by name on both backends, never templated by
/// monomorphization, and never compiled as source bodies.
pub fn intrinsic(name: &str) -> bool {
    matches!(
        name,
        witchy_syntax::intrinsics::GENERATED_LIST_PUSH
    ) || witchy_syntax::intrinsics::is_list_operation(name)
        || witchy_syntax::intrinsics::is_dict_operation(name)
        || witchy_syntax::intrinsics::is_string_operation(name)
        || witchy_syntax::intrinsics::is_math_operation(name)
        || witchy_syntax::intrinsics::is_crypto_operation(name)
        || witchy_syntax::intrinsics::is_regex_operation(name)
        || witchy_syntax::intrinsics::is_secretstore_operation(name)
}


/// Convenience: parse then type-check.
pub fn check_str(src: &str) -> Result<(), String> {
    let module = witchy_syntax::parser::parse_module(src).map_err(|e| e.to_string())?;
    check(&module).map_err(|e| e.to_string())
}

#[cfg(test)]
#[path = "typeck_tests.rs"]
mod tests;
