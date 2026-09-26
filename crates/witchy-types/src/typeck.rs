//! A type checker for witchy.
//!
//! Annotation-driven checking with light Hindley-Milner-style unification for
//! the bits that aren't annotated (let bindings, match arms). It is deliberately
//! lenient where it lacks information (e.g. an unknown constructor's arguments)
//! so it never rejects a valid program — it tightens as the type system grows.
//!
//! Capability safety is not a special case: `print` has type
//! `(Console, String) -> ()`, and the only way to obtain a `Console` is to
//! receive one as a parameter — ultimately from `main`. So "this code may
//! perform output" is simply visible in its type, and code that never received
//! the capability cannot type-check a call that needs it.

// foldhash (not SipHash): every key in these tables is compiler-internal
// (names, spans, expr identities) — never an attacker-chosen collection — so
// the checker's hot maps skip DoS-resistant hashing. The `*Ext` traits supply
// `::new()`/`::with_capacity()` for the aliased types.
use foldhash::{HashMap, HashMapExt as _, HashSet, HashSetExt as _};
use std::fmt;
use std::rc::Rc;

use witchy_syntax::ast::{
    self, Block, CallableQualifiers, Convention, Expr, Function, Item, MatchArm, Module, Pattern,
    Stmt, UnOp,
};
use witchy_syntax::build_entry::{build_entrypoint, is_build_capability_type};
use witchy_syntax::synthetic_names::anon_union_synthetic_variants;
use witchy_canonical_source::proper_tail::{
    canonical_proper_tail_manifest_digest, ProperTailCallableId, ProperTailDispatchKind, ProperTailExprKind,
    ProperTailPathStep,
};
use witchy_syntax::{cap_ops, intrinsics};
use witchy_cap_model::{CapabilityClass, CapabilityKind};

use crate::storage::{ReferenceLeaf, ReferenceStorageClassifier};
use witchy_canonical_source::capabilities::externref_cap_name;
use witchy_canonical_source::storage::StoredObligationCatalog;

mod coverage;
use coverage::{
    describe_pattern, expand_head_or, flatten_or_arms, pattern_ctor_key, specialize_row, ColCtor,
    LIST_CONS, LIST_NIL, TUPLE_CTOR,
};

mod cap_rights;
mod capability_calls;
pub use cap_rights::{ConsoleRights, DirRights, FileRights, NetRights, SecretRights};
use cap_rights::{
    console_rights, dir_rights, file_rights, net_rights, secret_rights, validate_cap_markers,
};

mod uniqueness;
use uniqueness::{
    check_unique_declarations, check_unique_functions, check_unique_parameters,
    UniquenessError,
};

mod existential;
use existential::{check_existential_types, existential_bare};

mod ty;
pub use ty::*;
mod declarations;
pub use declarations::*;
mod names;
pub use names::*;
mod module_checks;
pub use module_checks::*;
mod checker;
use checker::Checker;
mod compiler_syntax;
use compiler_syntax::{
    check_compiler_syntax_declarations, compiler_syntax_allowed_module, compiler_syntax_type_name,
    is_compiler_generated_structural_impl,
};

/// `&mut` is a reference capability, not the public `var` convention, but the
/// referent is writable inside the callee.  The loan checker proves that this
/// local mutability is exclusive; ordinary `let` parameters remain immutable.
fn parameter_binds_exclusive_reference(param: &ast::Param) -> bool {
    param.ty.as_ref().is_some_and(type_is_exclusive_reference)
}

fn type_is_exclusive_reference(ty: &ast::Type) -> bool {
    match ty {
        ast::Type::Qualified(ast::TypeQual::BorrowMut(_), _) => true,
        // Ownership qualifiers apply to the reference handle. They do not
        // change the exclusive capability of the underlying `&mut` relation.
        ast::Type::Qualified(
            ast::TypeQual::Frozen | ast::TypeQual::Unique,
            inner,
        ) => type_is_exclusive_reference(inner),
        _ => false,
    }
}

/// RFC-0167 §5: the compiler-known method that explicitly materializes an
/// independent owned value (`view.owned()`). It needs no import.
pub(crate) const OWNED_MATERIALIZATION: &str = "owned";

fn type_is_explicit_reference(ty: &ast::Type) -> bool {
    match ty {
        ast::Type::Qualified(
            ast::TypeQual::Borrow(_) | ast::TypeQual::LegacyBorrow(_) | ast::TypeQual::BorrowMut(_),
            _,
        ) => true,
        ast::Type::Qualified(
            ast::TypeQual::Frozen | ast::TypeQual::Unique,
            inner,
        ) => type_is_explicit_reference(inner),
        _ => false,
    }
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

/// A record type's layout: its type-parameter var ids (in order) and its fields
/// as `(name, type)`. Field types may mention the parameters, instantiated with
/// the value's actual type arguments on access.
type RecordInfo = (Vec<u32>, Vec<(String, Ty)>);
type CallObligations = Vec<(Ty, String)>;
#[derive(Clone, Debug, PartialEq, Eq)]
enum StorageObligation {
    RootedView(Ty),
    ReplayCapture(Ty),
}
type StorageObligations = Vec<StorageObligation>;
type UserCallSig = (Vec<Ty>, Ty, CallObligations, StorageObligations);
fn replay_snapshot_reason(reason: witchy_canonical_source::storage::SnapshotRefusal) -> &'static str {
    use witchy_canonical_source::storage::SnapshotRefusal;
    match reason {
        SnapshotRefusal::UniqueOwner => "affine unique storage",
        SnapshotRefusal::MustConsume => "must-consume storage",
        SnapshotRefusal::OnceCallable => "affine once-callable storage",
        SnapshotRefusal::Authority => "unproved capability storage",
        SnapshotRefusal::ResourceLimit => "storage beyond the recursive proof limit",
        SnapshotRefusal::UnprovedStorage => "storage without a checked replay snapshot proof",
    }
}
type AnonUnionVariants = Vec<(String, Vec<Ty>)>;

/// Qualifier-bearing callable facts that are intentionally absent from [`Ty`]'s
/// runtime function identity. Result guarantees require every source path;
/// parameter demands are retained if any source path needs them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct UniqueCallableContract {
    unique_params: Vec<bool>,
    unique_result: bool,
    /// `Ty` deliberately erases `frozen`, but an indirect `var` call still
    /// needs to distinguish whole-value replacement from a potentially deep
    /// mutation. Retain the source contract beside the callable identity.
    frozen_params: Vec<bool>,
    /// An inferred binding receiving this result remains frozen; otherwise a
    /// direct or indirect call would turn an immutable payload into writable
    /// storage merely by passing through a qualifier-erasing `Ty`.
    frozen_result: bool,
}

/// Checked provenance for a first-class callable whose runtime identity is
/// intentionally just its convention-preserving function ABI.  `mode opt`
/// does not introduce an `opt fn` spelling: an opt callable parameter instead
/// requires this source fact at its entry boundary.  Keeping it out of `Ty`
/// prevents an erased function value or a separate-compilation ABI from
/// accidentally manufacturing the permission.
#[derive(Clone, Debug, PartialEq, Eq)]
struct OptCallableContract {
    /// Nested callable arguments must retain compatible provenance as well.
    params: Vec<Option<Box<OptCallableContract>>>,
    /// A returned callable carries the contract declared by its producer, not
    /// a body-derived guess at the consuming module.
    result: Option<Box<OptCallableContract>>,
}

impl OptCallableContract {
    fn from_function_type(ty: &ast::Type) -> Option<Self> {
        match ty {
            ast::Type::Fn(params, result, _, _) => Some(Self {
                params: params
                    .iter()
                    .map(|param| Self::from_function_type(param).map(Box::new))
                    .collect(),
                result: Self::from_function_type(result).map(Box::new),
            }),
            ast::Type::Qualified(_, inner) => Self::from_function_type(inner),
            _ => None,
        }
    }

    fn from_function(function: &Function) -> Self {
        Self {
            params: function
                .params
                .iter()
                .map(|param| param.ty.as_ref().and_then(Self::from_function_type).map(Box::new))
                .collect(),
            result: function.ret.as_ref().and_then(Self::from_function_type).map(Box::new),
        }
    }

    fn from_lambda(params: &[ast::Param], result: Option<&ast::Type>) -> Self {
        Self {
            params: params
                .iter()
                .map(|param| param.ty.as_ref().and_then(Self::from_function_type).map(Box::new))
                .collect(),
            result: result.and_then(Self::from_function_type).map(Box::new),
        }
    }

    /// A leaf callable has no callable precondition or recursively higher-order
    /// result.  Normal code may receive it through an ordinary ABI: losing its
    /// origin cannot enable a later opt HOF call because `require_opt...` still
    /// rejects it at that stricter slot.  Everything else is load-bearing.
    fn has_higher_order_boundary(&self) -> bool {
        self.params.iter().any(Option::is_some)
            || self
                .result
                .as_deref()
                .is_some_and(Self::has_higher_order_boundary)
    }
}

/// A callable-producing control-flow path either yields a value contract or
/// does not reach the join at all.  The latter is neutral: treating it as an
/// empty, ordinary contract would erase the guarantees of the surviving path.
#[derive(Clone, Debug, PartialEq, Eq)]
enum CallableContractPath {
    Completes(UniqueCallableContract),
    Diverges,
}

impl UniqueCallableContract {
    fn from_function_type(ty: &ast::Type) -> Option<Self> {
        match ty {
            ast::Type::Fn(params, result, _, _) => Some(Self {
                unique_params: params.iter().map(is_unique_value_type).collect(),
                unique_result: is_unique_value_type(result),
                frozen_params: params.iter().map(is_frozen_value_type).collect(),
                frozen_result: is_frozen_value_type(result),
            }),
            ast::Type::Qualified(_, inner) => Self::from_function_type(inner),
            _ => None,
        }
    }

    fn join_all(contracts: impl IntoIterator<Item = Self>) -> Option<Self> {
        let mut contracts = contracts.into_iter();
        let mut joined = contracts.next()?;
        for contract in contracts {
            if joined.unique_params.len() != contract.unique_params.len() {
                return None;
            }
            for (joined_param, param) in joined.unique_params.iter_mut().zip(contract.unique_params) {
                *joined_param |= param;
            }
            joined.unique_result &= contract.unique_result;
            if joined.frozen_params.len() != contract.frozen_params.len() {
                return None;
            }
            for (joined_param, param) in joined.frozen_params.iter_mut().zip(contract.frozen_params) {
                // A caller may pass frozen storage through an indirect `var`
                // call only when every completing callee promises whole-value
                // replacement. One deep-mutating branch invalidates that
                // permission; unlike unique demand, this is a guarantee.
                *joined_param &= param;
            }
            joined.frozen_result &= contract.frozen_result;
        }
        Some(joined)
    }
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
    // Generator helpers occupy the compiler-private `__gen_` namespace. The
    // source checker has already reserved that spelling, so stripping it here
    // cannot rename a user function. Keep type failures phrased against the
    // `gen fn` declaration rather than exposing the replay/frame helper.
    if let Some(source) = name
        .rsplit('.')
        .next()
        .and_then(|local| local.strip_prefix("__gen_"))
    {
        return source.to_string();
    }
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
    /// The canonical storage plan of `ty` over this module's definitions,
    /// memoized: the plan depends only on the type and the (fixed) map.
    fn storage_plan(&self, ty: &ast::Type) -> witchy_canonical_source::storage::StoragePlan {
        if let Some(plan) = self.storage_plans.borrow().get(ty) {
            return plan.clone();
        }
        let plan = witchy_canonical_source::storage::StoragePlan::classify(
            ty, &self.canonical_definitions, |_| Some(0),
        );
        self.storage_plans.borrow_mut().insert(ty.clone(), plan.clone());
        plan
    }

    /// A view payload with a carrier that crosses a callable boundary: the
    /// scalar and `&String` carriers, or a record/sum/`Option`/`Result` or
    /// scalar List accepted by the canonical carrier rules.
    fn admitted_view_capture(&self, payload: &Ty) -> bool {
        use witchy_canonical_source::rooted_view::{RetainedRootStorage, RootedViewStorage};
        use witchy_canonical_source::storage::StoragePlan;
        let Some(payload) = ty_to_ast(payload) else { return false };
        let view = ast::Type::Qualified(ast::TypeQual::View, Box::new(payload));
        let StoragePlan::RootedView(storage) = self.storage_plan(&view) else { return false };
        let ast::Type::Qualified(_, payload) = &view else { unreachable!() };
        match storage {
            RootedViewStorage::ScalarCell(_) => true,
            RootedViewStorage::InlineScalar(_)
            | RootedViewStorage::RetainedRoot(RetainedRootStorage::ByteBuffer(_)) => {
                !matches!(payload.as_ref(), ast::Type::Qualified(..))
            }
            RootedViewStorage::RetainedProjection(_) => {
                // `Slice(T)` is the checker's private retained-view payload.
                matches!(payload.as_ref(), ast::Type::Named(name, arguments)
                    if name == "Slice" && arguments.len() == 1)
            }
            RootedViewStorage::RetainedRoot(RetainedRootStorage::Sequence(_)
                | RetainedRootStorage::CheckedAggregate(_)) => true,
            RootedViewStorage::RetainedRoot(RetainedRootStorage::StructuralRecord(_)) => false,
        }
    }

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
            "Secret" => Ty::Secret(secret_rights(args)),
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
                Ty::List(Box::new(e).into())
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
            ast::Type::Qualified(ast::TypeQual::View, inner) => {
                return Ty::View(Box::new(self.to_ty(inner)).into());
            }
            ast::Type::Qualified(_, inner) => return self.to_ty(inner),
            ast::Type::Slice(elem) => {
                let inner = self.to_ty(elem);
                return Ty::Named("Slice".into(), vec![inner].into());
            }
            ast::Type::Named(name, args) => (name, args),
            ast::Type::Tuple(ts) => {
                if ts.is_empty() {
                    return Ty::Unit;
                }
                return Ty::Tuple(ts.iter().map(|t| self.to_ty(t)).collect());
            }
            ast::Type::Fn(params, ret, conventions, qualifiers) => {
                return Ty::Fn(
                    params.iter().map(|t| self.to_ty(t)).collect(),
                    Box::new(self.to_ty(ret)).into(),
                    conventions.clone().into(),
                    params.iter().map(type_is_exclusive_reference).collect(),
                    *qualifiers,
                );
            }
            // (RFC-0081) A first-class existential identity: the bare trait
            // name plus its argument types (which recurse through ordinary
            // resolution, so aliases normalize to the same identity).
            ast::Type::Dyn(name, args) => {
                return Ty::Dyn(
                    name.clone().into(),
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
        // (BUG-308) An argument-less name that names one of the enclosing
        // function's type parameters is that parameter's var — so a
        // body ascription refines the generic parameter instead of pinning it
        // to a distinct concrete `Named`. (Matches the signature-only rule in
        // `to_ty_generic`; outside a generic fn `current_typarams` is empty, so
        // top-level `let` and non-parameter names are unaffected.)
        if args.is_empty()
            && !name.contains('.')
            && self.current_typarams.contains_key(name.as_str())
        {
            return Ty::Var(self.current_typarams[name.as_str()]);
        }
        Ty::Named(name.clone().into(), args.iter().map(|a| self.to_ty(a)).collect())
    }

    /// Like `to_ty`, but a lowercase, argument-less type name becomes a type
    /// *variable* (a parameter), shared within one signature via `vars`.
    #[allow(clippy::wrong_self_convention)]
    fn to_ty_generic(&mut self, t: &ast::Type, vars: &mut HashMap<String, Ty>) -> Ty {
        match t {
            ast::Type::Qualified(ast::TypeQual::View, inner) => {
                Ty::View(Box::new(self.to_ty_generic(inner, vars)).into())
            }
            ast::Type::Qualified(_, inner) => self.to_ty_generic(inner, vars),
            ast::Type::Slice(elem) => {
                let inner = self.to_ty_generic(elem, vars);
                Ty::Named("Slice".into(), vec![inner].into())
            }
            ast::Type::Tuple(ts) => {
                if ts.is_empty() {
                    return Ty::Unit;
                }
                Ty::Tuple(ts.iter().map(|t| self.to_ty_generic(t, vars)).collect())
            }
            ast::Type::Fn(params, ret, conventions, qualifiers) => Ty::Fn(
                params.iter().map(|t| self.to_ty_generic(t, vars)).collect(),
                Box::new(self.to_ty_generic(ret, vars)).into(),
                conventions.clone().into(),
                params.iter().map(type_is_exclusive_reference).collect(),
                *qualifiers,
            ),
            // (RFC-0081) First-class existential identity; arguments recurse so
            // a signature's generic vars inside `dyn T(a)` stay shared (the
            // existential validation pass separately rejects unresolved args).
            ast::Type::Dyn(name, args) => Ty::Dyn(
                name.clone().into(),
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
                    && !name.contains('.')
                    && let Some(v) = vars.get(name.as_str())
                {
                    return v.clone();
                }
                if args.is_empty()
                    && name.chars().next().is_some_and(|c| c.is_lowercase())
                    && !name.contains('.')
                    && name != "str"
                {
                    if let Some(v) = vars.get(name.as_str()) {
                        return v.clone();
                    }
                    let v = self.fresh();
                    vars.insert(name.clone(), v.clone());
                    return v;
                }
                Ty::Named(
                    name.clone().into(),
                    args.iter()
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
        storage_obligations: &[StorageObligation],
    ) -> UserCallSig {
        let fresh_map = self.fresh_typeparam_map(typarams);
        let p = params.iter().map(|t| self.subst_vars(t, &fresh_map)).collect();
        let r = self.subst_vars(ret, &fresh_map);
        let bs = bounds
            .iter()
            .map(|(var, tr)| (self.subst_vars(&Ty::Var(*var), &fresh_map), tr.clone()))
            .collect();
        let storage = storage_obligations
            .iter()
            .map(|obligation| match obligation {
                StorageObligation::RootedView(payload) => StorageObligation::RootedView(self.subst_vars(payload, &fresh_map)),
                StorageObligation::ReplayCapture(capture) => StorageObligation::ReplayCapture(self.subst_vars(capture, &fresh_map)),
            })
            .collect();
        (p, r, bs, storage)
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
            Ty::List(e) => Ty::List(Box::new(self.subst_vars(&e, map)).into()),
            Ty::View(payload) => Ty::View(Box::new(self.subst_vars(&payload, map)).into()),
            Ty::Tuple(ts) => Ty::Tuple(ts.iter().map(|x| self.subst_vars(x, map)).collect()),
            Ty::Named(n, args) => {
                Ty::Named(n, args.iter().map(|x| self.subst_vars(x, map)).collect())
            }
            Ty::Dyn(n, args) => {
                Ty::Dyn(n, args.iter().map(|x| self.subst_vars(x, map)).collect())
            }
            Ty::Fn(params, ret, conventions, reference_params, qualifiers) => Ty::Fn(
                params.iter().map(|x| self.subst_vars(x, map)).collect(),
                Box::new(self.subst_vars(&ret, map)).into(),
                conventions,
                reference_params,
                qualifiers,
            ),
            other => other,
        }
    }

    // RFC-0164 Phase 3: `resolve` is the checker's single hottest call after
    // Phase 1's Ty interning (sample-profiled, ~2nd-costliest leaf on an
    // ordinary compile) -- it recurses over the WHOLE structure of every type
    // it's handed and rebuilds a fresh value at every level, even for a type
    // with no unresolved variable anywhere in it (the common case once
    // unification has settled for that subtree). Phase 1 made `Ty::clone()`
    // an O(1) Rc bump, but `resolve` still paid an O(size) *reconstruction*
    // regardless of whether anything actually changed.
    //
    // First cut of this cache keyed on the whole input `Ty` (any structural
    // duplicate hits): measured WORSE on a fresh profile -- hashing a large
    // compound `Ty` costs about as much as rebuilding it, and most calls
    // reach `resolve` with a structurally distinct argument (a different
    // expression's type), so the miss path pays hashing on top of the
    // unchanged rebuild cost with little hit-rate to amortize it.
    //
    // Keyed on the bound type VARIABLE instead: `Ty::Var(v)` is the only
    // variant that can actually contain something unification changed, and a
    // shared variable (a let-binding's inferred type, a callee's return type
    // variable) is resolved from many call sites over the checker's
    // lifetime, so THIS cache sees real repeat traffic. The key is a `u32`
    // (hashes for free) rather than the type it's bound to, so a miss costs
    // nothing beyond what resolving would have cost anyway.
    fn resolve(&self, t: &Ty) -> Ty {
        self.resolved_if_changed(t).unwrap_or_else(|| t.clone())
    }

    /// `Some` only when resolution actually substituted something.
    ///
    /// Resolution rebuilt the whole type on every call: a `Named`, `Tuple` or
    /// `Fn` allocated a fresh `Rc<[Ty]>` for its arguments even when no
    /// variable inside it was bound and the answer was the type it started
    /// with. Profiled on a real 1,824-line program, `Ty::allocate_for_slice`
    /// under `resolve` was 6.7% of the compile, inside a type-check that is
    /// 85% of it and three quarters allocator traffic.
    ///
    /// Reporting "unchanged" upward lets every level keep the `Rc` it already
    /// holds, so an unchanged subtree costs a refcount bump instead of a
    /// rebuild. The result is identical either way: this only chooses between
    /// a fresh structure and the equal one already in hand.
    fn resolved_if_changed(&self, t: &Ty) -> Option<Ty> {
        match t {
            Ty::Var(v) => {
                if let Some(cached) = self.resolve_cache.borrow().get(v) {
                    return (cached != t).then(|| cached.clone());
                }
                let result = match self.subst.get(v) {
                    Some(bound) => self.resolve(bound),
                    None => t.clone(),
                };
                self.resolve_cache.borrow_mut().insert(*v, result.clone());
                (result != *t).then_some(result)
            }
            Ty::List(e) => self.resolved_if_changed(e).map(|e| Ty::List(Rc::new(e))),
            Ty::View(payload) => self.resolved_if_changed(payload).map(|p| Ty::View(Rc::new(p))),
            Ty::Tuple(ts) => self.resolved_slice(ts).map(Ty::Tuple),
            Ty::Named(n, args) => self.resolved_slice(args).map(|args| Ty::Named(n.clone(), args)),
            Ty::Dyn(n, args) => self.resolved_slice(args).map(|args| Ty::Dyn(n.clone(), args)),
            Ty::Fn(params, ret, conventions, reference_params, qualifiers) => {
                let resolved_params = self.resolved_slice(params);
                let resolved_ret = self.resolved_if_changed(ret);
                if resolved_params.is_none() && resolved_ret.is_none() {
                    return None;
                }
                Some(Ty::Fn(
                    resolved_params.unwrap_or_else(|| params.clone()),
                    resolved_ret.map_or_else(|| ret.clone(), Rc::new),
                    conventions.clone(),
                    reference_params.clone(),
                    *qualifiers,
                ))
            }
            _ => None,
        }
    }

    /// `Some` only when at least one element resolved to something else.
    fn resolved_slice(&self, types: &Rc<[Ty]>) -> Option<Rc<[Ty]>> {
        let first_change = types
            .iter()
            .position(|t| self.resolved_if_changed(t).is_some())?;
        let resolved = types
            .iter()
            .enumerate()
            .map(|(index, t)| {
                if index < first_change { t.clone() } else { self.resolve(t) }
            })
            .collect();
        Some(resolved)
    }

    /// The only two ways `self.subst` may change: bind one more variable, or
    /// roll back to an earlier snapshot (speculative unification's undo).
    /// Both invalidate substitution-dependent query caches -- a stale entry
    /// would answer with a type variable's PREVIOUS binding. Route mutations through these
    /// rather than `self.subst.insert`/`self.subst = ...` directly, so a
    /// future write can't forget to invalidate.
    fn bind_subst_var(&mut self, v: u32, ty: Ty) {
        self.subst.insert(v, ty);
        self.resolve_cache.borrow_mut().clear();
        self.unique_storage_types.borrow_mut().clear();
    }

    fn restore_subst(&mut self, saved: HashMap<u32, Ty>) {
        self.subst = saved;
        self.resolve_cache.borrow_mut().clear();
        self.unique_storage_types.borrow_mut().clear();
    }

    fn same_resolved_type(&self, a: &Ty, b: &Ty) -> bool {
        let a = self.resolve(a);
        let b = self.resolve(b);
        match (&a, &b) {
            (Ty::Named(x, xa), Ty::Named(y, ya))
                if same_builtin_nominal_head(x, y) && xa.len() == ya.len() =>
            {
                xa.iter().zip(ya.iter()).all(|(x, y)| self.same_resolved_type(x, y))
            }
            _ => a == b,
        }
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
                self.restore_subst(saved_subst);
                return Ok(false);
            };
            for (dst_field, src_field) in dst_fields.iter().zip(src_fields) {
                if self.unify(dst_field, src_field).is_err() {
                    self.restore_subst(saved_subst);
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
                Ty::View(payload) => go(c, &payload, seen),
                Ty::Tuple(items) => items.iter().find_map(|i| go(c, i, seen)),
                Ty::Fn(params, ret, _, _, _) => params
                    .iter()
                    .find_map(|p| go(c, p, seen))
                    .or_else(|| go(c, &ret, seen)),
                // (RFC-0081) An existential VALUE never leaks a capability —
                // capability-carrying payloads are rejected at construction —
                // so only its type arguments need checking.
                Ty::Dyn(_, args) => args.iter().find_map(|a| go(c, a, seen)),
                Ty::Named(n, args) => {
                    if let Some(cap) = c.transparent_externref_brands.get(n.as_ref()) {
                        return externref_cap_name(cap);
                    }
                    if let Some(hit) = args.iter().find_map(|a| go(c, a, seen)) {
                        return Some(hit);
                    }
                    if !seen.insert(n.to_string()) {
                        return None;
                    }
                    let fields = c.record_fields.get(n.as_ref()).map(|(_, fields)| fields.clone());
                    let mut hit = fields
                        .into_iter()
                        .flatten()
                        .find_map(|(_, field)| go(c, &field, seen));
                    if hit.is_none()
                        && let Some(variants) = c.adt_variants.get(n.as_ref())
                    {
                        hit = variants.iter().find_map(|variant| {
                            c.ctor_sigs.get(variant).and_then(|(payloads, _)| {
                                payloads.iter().find_map(|payload| go(c, payload, seen))
                            })
                        });
                    }
                    seen.remove(n.as_ref());
                    hit
                }
                Ty::Int | Ty::Float | Ty::Duration | Ty::String | Ty::Bytes | Ty::Msg
                | Ty::Bool | Ty::Unit | Ty::Console(_) | Ty::Clock | Ty::Rand | Ty::Env
                | Ty::Secret(_) | Ty::Exec | Ty::Fetch | Ty::Dir(_) | Ty::File(_) | Ty::Net(_)
                | Ty::Socket | Ty::Listener | Ty::BuildOut | Ty::BuildRead | Ty::BuildEnv
                | Ty::BuildNet | Ty::BuildExec | Ty::Var(_) => None,
            }
        }
        go(self, t, &mut HashSet::new())
    }

    fn ty_carries_function_value(&self, t: &Ty) -> bool {
        fn go(c: &Checker, t: &Ty, seen: &mut HashSet<String>) -> bool {
            match c.resolve(t) {
                Ty::Fn(..) => true,
                Ty::List(inner) => go(c, &inner, seen),
                Ty::View(payload) => go(c, &payload, seen),
                Ty::Tuple(items) | Ty::Dyn(_, items) => {
                    items.iter().any(|item| go(c, item, seen))
                }
                Ty::Named(name, args) => {
                    if args.iter().any(|arg| go(c, arg, seen)) {
                        return true;
                    }
                    if !seen.insert(name.to_string()) {
                        return false;
                    }
                    let record_hit = c.record_fields.get(name.as_ref()).is_some_and(|(_, fields)| {
                        fields.iter().any(|(_, field)| go(c, field, seen))
                    });
                    let variant_hit = c.adt_variants.get(name.as_ref()).is_some_and(|variants| {
                        variants.iter().any(|variant| {
                            c.ctor_sigs.get(variant).is_some_and(|(payloads, _)| {
                                payloads.iter().any(|payload| go(c, payload, seen))
                            })
                        })
                    });
                    seen.remove(name.as_ref());
                    record_hit || variant_hit
                }
                Ty::Int | Ty::Float | Ty::Duration | Ty::String | Ty::Bytes | Ty::Msg
                | Ty::Bool | Ty::Unit | Ty::Console(_) | Ty::Clock | Ty::Rand | Ty::Env
                | Ty::Secret(_) | Ty::Exec | Ty::Fetch | Ty::Dir(_) | Ty::File(_) | Ty::Net(_)
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
                Ty::Secret(_) => "Secret",
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
                Ty::Named(n, _) if **n == *"SecretStore" => "SecretStore",
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
                Ty::View(payload) => go(c, &payload, seen),
                Ty::Tuple(items) => items.iter().find_map(|i| go(c, i, seen)),
                Ty::Fn(params, ret, _, _, _) => params
                    .iter()
                    .find_map(|p| go(c, p, seen))
                    .or_else(|| go(c, &ret, seen)),
                // A nested existential's payload was itself cap-checked at its
                // own construction; only its type arguments remain.
                Ty::Dyn(_, args) => args.iter().find_map(|a| go(c, a, seen)),
                Ty::Named(n, args) => {
                    if let Some(cap) = c.transparent_externref_brands.get(n.as_ref()) {
                        return externref_cap_name(cap);
                    }
                    if let Some(hit) = args.iter().find_map(|a| go(c, a, seen)) {
                        return Some(hit);
                    }
                    if !seen.insert(n.to_string()) {
                        return None;
                    }
                    let fields = c.record_fields.get(n.as_ref()).map(|(_, fields)| fields.clone());
                    let mut hit = fields
                        .into_iter()
                        .flatten()
                        .find_map(|(_, field)| go(c, &field, seen));
                    if hit.is_none()
                        && let Some(variants) = c.adt_variants.get(n.as_ref())
                    {
                        hit = variants.iter().find_map(|variant| {
                            c.ctor_sigs.get(variant).and_then(|(payloads, _)| {
                                payloads.iter().find_map(|payload| go(c, payload, seen))
                            })
                        });
                    }
                    seen.remove(n.as_ref());
                    hit
                }
                Ty::Int | Ty::Float | Ty::Duration | Ty::String | Ty::Bytes | Ty::Msg
                | Ty::Bool | Ty::Unit | Ty::Var(_) => None,
                // Direct capabilities were handled by `direct` above.
                Ty::Console(_) | Ty::Clock | Ty::Rand | Ty::Env | Ty::Secret(_) | Ty::Exec | Ty::Fetch
                | Ty::Dir(_) | Ty::File(_) | Ty::Net(_) | Ty::Socket | Ty::Listener
                | Ty::BuildOut | Ty::BuildRead | Ty::BuildEnv | Ty::BuildNet
                | Ty::BuildExec => None,
            }
        }
        go(self, t, &mut HashSet::new())
    }

    /// Ask the shared checked-storage judgment about the actual captured
    /// value. A free inference variable becomes an explicit refusal marker;
    /// callable signatures and phantom arguments remain outside storage.
    fn pure_capture_hazard(
        &self, ty: &Ty,
    ) -> Option<witchy_canonical_source::storage::PureCaptureHazard> {
        use witchy_canonical_source::storage::{PureCaptureHazard, pure_capture_hazard};
        let source = ty_to_ast_with_unresolved(
            &self.resolve(ty), UnresolvedAst::UnknownPureCapture,
        );
        source.map_or(Some(PureCaptureHazard::UnprovedStorage), |source| {
            pure_capture_hazard(&source, &self.canonical_definitions, &self.stored_obligations)
        })
    }

    fn reject_pure_capture(&self, capture: &str, ty: &Ty) -> Result<(), TypeError> {
        use witchy_canonical_source::storage::PureCaptureHazard;
        let Some(hazard) = self.pure_capture_hazard(ty) else {
            return Ok(());
        };
        let reason = match hazard {
            PureCaptureHazard::Capability(capability) => {
                format!("it carries capability `{capability:?}`")
            }
            PureCaptureHazard::DeclaredCapability => {
                "its nominal type is declared as a capability".to_string()
            }
            PureCaptureHazard::OrdinaryCallable => {
                "it carries an ordinary callable with opaque effects".to_string()
            }
            PureCaptureHazard::Existential => {
                "it carries existential behavior that is not statically effect-classified"
                    .to_string()
            }
            PureCaptureHazard::UnresolvedGeneric => {
                "its generic type may store authority or an ordinary callable".to_string()
            }
            PureCaptureHazard::UnprovedStorage => {
                "its stored fields have no complete pure-capture proof".to_string()
            }
        };
        terr(format!(
            "closure capture `{capture}` cannot cross a pure boundary because {reason}"
        ))
    }

    fn reject_pure_intrinsic_call(&self, name: &str) -> Result<(), TypeError> {
        if !self.current_pure {
            return Ok(());
        }
        let Some(spec) = intrinsics::lookup(name) else {
            return Ok(());
        };
        if spec.capability_effect != intrinsics::CapabilityEffect::None {
            return terr(format!(
                "pure callable cannot invoke `{}` because it exercises capability authority",
                diagnostic_callable_name(name)
            ));
        }
        match spec.effect {
            intrinsics::IntrinsicEffect::Pure | intrinsics::IntrinsicEffect::ControlFlow => Ok(()),
            intrinsics::IntrinsicEffect::Dynamic => terr(format!(
                "pure callable cannot invoke `{}` because dynamic behavior is not statically effect-classified",
                diagnostic_callable_name(name)
            )),
            // The three WriteBack rows are collection extract computations.
            // Their `var` argument selects whether the repaired collection is
            // stored in a local or a caller-owned place; the call-site check
            // below rejects only the latter.
            intrinsics::IntrinsicEffect::WriteBack => Ok(()),
            intrinsics::IntrinsicEffect::Task => terr(format!(
                "pure callable cannot invoke `{}` because task scheduling is an observable effect",
                diagnostic_callable_name(name)
            )),
            intrinsics::IntrinsicEffect::Toolchain => terr(format!(
                "pure callable cannot invoke `{}` because compiler/toolchain access is an observable effect",
                diagnostic_callable_name(name)
            )),
        }
    }

    fn reject_pure_caller_writeback(&self, name: &str, args: &[Expr]) -> Result<(), TypeError> {
        if !self.current_pure
            || !intrinsics::lookup(name)
                .is_some_and(|spec| spec.effect == intrinsics::IntrinsicEffect::WriteBack)
        {
            return Ok(());
        }
        let Some(place) = args.first().and_then(witchy_canonical_source::access::checked_place) else {
            // Catalog/source conventions still require a mutable place. There
            // is no caller-visible write-back to classify until one exists.
            return Ok(());
        };
        if self
            .must_binding_key(place.root())
            .is_some_and(|binding| self.pure_var_parameters.contains(&binding))
        {
            return terr(format!(
                "pure callable cannot pass `var` parameter `{}` to `{}` because the collection repair writes back to the caller",
                place.root(),
                diagnostic_callable_name(name)
            ));
        }
        Ok(())
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
                Ty::Secret(_) => "Secret",
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
                Ty::Named(name, _) if **name == *"SecretStore" => "SecretStore",
                _ => return None,
            })
        }
        fn go(
            checker: &Checker,
            ty: &Ty,
            visiting: &mut Vec<(Rc<str>, Rc<[Ty]>)>,
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
                Ty::View(payload) => go(checker, &payload, visiting, path),
                Ty::Tuple(items) => items.iter().enumerate().find_map(|(index, item)| {
                    let mut child = path.to_vec();
                    child.push(format!("tuple[{index}]"));
                    go(checker, item, visiting, &child)
                }),
                Ty::Fn(params, result, _, _, _) => params
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
                    if let Some(capability) = checker.transparent_externref_brands.get(name.as_ref()) {
                        return externref_cap_name(capability)
                            .map(|capability| (capability, path.to_vec()));
                    }
                    let key = (name.clone(), arguments.clone());
                    if visiting.contains(&key) {
                        return None;
                    }
                    visiting.push(key);
                    let bare = name.rsplit('.').next().unwrap_or(&name);
                    // Type arguments retain a capability even for builtin
                    // containers with no field/variant table (`Dict`, `Set`,
                    // `Iter`): mirror `ty_carries_capability`, which recurses
                    // into `arguments` here. Without this the two closures
                    // disagree (the `debug_assert_eq!` below) and a
                    // `Dict(String, Dir)` payload could erase through the
                    // RFC-0081 firewall the moment caps-in-containers are
                    // permitted.
                    for (index, argument) in arguments.iter().enumerate() {
                        let mut child = path.to_vec();
                        child.push(format!("{bare}<arg {index}>"));
                        if let Some(found) = go(checker, argument, visiting, &child) {
                            visiting.pop();
                            return Some(found);
                        }
                    }
                    if let Some((parameters, fields)) = checker.record_fields.get(name.as_ref()) {
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
                    if let Some(variants) = checker.adt_variants.get(name.as_ref()) {
                        for variant in variants {
                            let Some((payloads, result)) = checker.ctor_sigs.get(variant) else {
                                continue;
                            };
                            let result_arguments = match checker.resolve(result) {
                                Ty::Named(_, result_arguments) => result_arguments,
                                _ => Vec::new().into(),
                            };
                            let substitution = result_arguments
                                .iter()
                                .zip(arguments.iter())
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
                Ty::Console(_) | Ty::Clock | Ty::Rand | Ty::Env | Ty::Secret(_) | Ty::Exec | Ty::Fetch
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
            Ty::Named(n, args) => args.is_empty() && self.transparent_externref_brands.contains_key(n.as_ref()),
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
            Ty::Secret(_) => Some("Secret".to_string()),
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
            Ty::View(payload) => self.ty_authority_taint(&payload, seen),
            Ty::Tuple(items) => items.iter().find_map(|item| self.ty_authority_taint(item, seen)),
            Ty::Fn(params, ret, _, _, _) => params
                .iter()
                .chain(std::iter::once(ret.as_ref()))
                .find_map(|item| self.ty_authority_taint(item, seen)),
            // (RFC-0081) An existential value never carries authority (payload
            // capabilities are rejected at construction); check its args only.
            Ty::Dyn(_, args) => args.iter().find_map(|arg| self.ty_authority_taint(arg, seen)),
            Ty::Named(name, args) => {
                if name == "SecretStore".into() || self.sealed_types.contains(name.as_ref()) {
                    return Some(name.to_string());
                }
                if let Some(hit) = args.iter().find_map(|arg| self.ty_authority_taint(arg, seen)) {
                    return Some(hit);
                }
                if !seen.insert(name.to_string()) {
                    return None;
                }
                let record_hit = self.record_fields.get(name.as_ref()).and_then(|(_, fields)| {
                    fields.iter().find_map(|(_, field)| self.ty_authority_taint(field, seen))
                });
                let hit = record_hit.or_else(|| {
                    self.adt_variants.get(name.as_ref()).and_then(|variants| {
                        variants.iter().find_map(|variant| {
                            self.ctor_sigs.get(variant).and_then(|(payloads, _)| {
                                payloads.iter().find_map(|payload| self.ty_authority_taint(payload, seen))
                            })
                        })
                    })
                });
                seen.remove(name.as_ref());
                hit
            }
            Ty::Int | Ty::Float | Ty::Duration | Ty::String | Ty::Bytes | Ty::Msg
            | Ty::Bool | Ty::Unit | Ty::Var(_) => None,
        }
    }

    fn reject_structural_authority_ty(&self, t: &Ty, ctx: &str) -> Result<(), TypeError> {
        match self.resolve(t) {
            Ty::List(inner) => self.reject_structural_authority_ty(&inner, ctx),
            Ty::View(payload) => self.reject_structural_authority_ty(&payload, ctx),
            Ty::Dyn(_, args) => {
                args.iter().try_for_each(|arg| self.reject_structural_authority_ty(arg, ctx))
            }
            Ty::Tuple(items) => {
                items.iter().try_for_each(|item| self.reject_structural_authority_ty(item, ctx))
            }
            Ty::Fn(params, ret, _, _, _) => {
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
            | Ty::Secret(_) | Ty::Exec | Ty::Fetch | Ty::Dir(_) | Ty::File(_) | Ty::Net(_)
            | Ty::Socket | Ty::Listener | Ty::BuildOut | Ty::BuildRead | Ty::BuildEnv
            | Ty::BuildNet | Ty::BuildExec | Ty::Var(_) => Ok(()),
        }
    }

    fn compiler_syntax_ty(&self, t: &Ty) -> Option<&'static str> {
        match self.resolve(t) {
            Ty::List(inner) => self.compiler_syntax_ty(&inner),
            Ty::View(payload) => self.compiler_syntax_ty(&payload),
            Ty::Dyn(_, args) => args.iter().find_map(|arg| self.compiler_syntax_ty(arg)),
            Ty::Tuple(items) => items.iter().find_map(|item| self.compiler_syntax_ty(item)),
            Ty::Fn(params, ret, _, _, _) => params
                .iter()
                .chain(std::iter::once(ret.as_ref()))
                .find_map(|item| self.compiler_syntax_ty(item)),
            Ty::Named(name, args) => {
                compiler_syntax_type_name(&name).or_else(|| args.iter().find_map(|arg| self.compiler_syntax_ty(arg)))
            }
            Ty::Int | Ty::Float | Ty::Duration | Ty::String | Ty::Bytes | Ty::Msg
            | Ty::Bool | Ty::Unit | Ty::Console(_) | Ty::Clock | Ty::Rand | Ty::Env
            | Ty::Secret(_) | Ty::Exec | Ty::Fetch | Ty::Dir(_) | Ty::File(_) | Ty::Net(_)
            | Ty::Socket | Ty::Listener | Ty::BuildOut | Ty::BuildRead | Ty::BuildEnv
            | Ty::BuildNet | Ty::BuildExec | Ty::Var(_) => None,
        }
    }

    fn borrowed_nominal_name(&self, ty: &Ty) -> Option<String> {
        match self.resolve(ty) {
            Ty::View(payload) => self.borrowed_nominal_name(&payload),
            Ty::List(element) => self.borrowed_nominal_name(&element),
            Ty::Tuple(items) => items
                .iter()
                .find_map(|item| self.borrowed_nominal_name(item)),
            Ty::Named(name, arguments) => {
                let has_lifetime_argument = arguments.iter().any(|argument| {
                    matches!(self.resolve(argument), Ty::Named(lifetime, args)
                        if args.is_empty() && lifetime.starts_with('\''))
                });
                if self.borrowed_nominal_types.contains(name.as_ref()) || has_lifetime_argument {
                    Some(dequalify_home(&name, &self.cur_module))
                } else {
                    arguments
                        .iter()
                        .find_map(|argument| self.borrowed_nominal_name(argument))
                }
            }
            Ty::Dyn(_, arguments) => arguments
                .iter()
                .find_map(|argument| self.borrowed_nominal_name(argument)),
            // A callable carries independently quantified relations in its type;
            // it is not itself a borrowed shell. Invocation checks its operands
            // and results at the corresponding runtime boundary.
            Ty::Fn(..)
            | Ty::Int
            | Ty::Float
            | Ty::Duration
            | Ty::String
            | Ty::Bytes
            | Ty::Msg
            | Ty::Bool
            | Ty::Unit
            | Ty::Console(_)
            | Ty::Clock
            | Ty::Rand
            | Ty::Env
            | Ty::Secret(_)
            | Ty::Exec
            | Ty::Fetch
            | Ty::Dir(_)
            | Ty::File(_)
            | Ty::Net(_)
            | Ty::Socket
            | Ty::Listener
            | Ty::BuildOut
            | Ty::BuildRead
            | Ty::BuildEnv
            | Ty::BuildNet
            | Ty::BuildExec
            | Ty::Var(_) => None,
        }
    }

    fn is_direct_borrowed_nominal(&self, ty: &Ty) -> bool {
        match self.resolve(ty) {
            Ty::Named(name, arguments) => {
                self.borrowed_nominal_types.contains(name.as_ref())
                    || arguments.iter().any(|argument| {
                        matches!(self.resolve(argument), Ty::Named(lifetime, args)
                            if args.is_empty() && lifetime.starts_with('\''))
                    })
            }
            _ => false,
        }
    }

    fn contains_unsupported_borrowed_nominal(&self, ty: &Ty) -> bool {
        match self.resolve(ty) {
            Ty::View(payload) => self.contains_unsupported_borrowed_nominal(&payload),
            Ty::List(element) => self.contains_unsupported_borrowed_nominal(&element),
            Ty::Tuple(items) => items
                .iter()
                .any(|item| self.contains_unsupported_borrowed_nominal(item)),
            Ty::Named(name, arguments) => {
                let explicit = self.explicit_reference_nominal_types.contains(name.as_ref());
                let direct_legacy = !explicit
                    && (self.borrowed_nominal_types.contains(name.as_ref())
                        || arguments.iter().any(|argument| {
                            matches!(self.resolve(argument), Ty::Named(lifetime, args)
                                if args.is_empty() && lifetime.starts_with('\''))
                        }));
                direct_legacy
                    || arguments
                        .iter()
                        .any(|argument| self.contains_unsupported_borrowed_nominal(argument))
            }
            Ty::Dyn(_, arguments) => arguments
                .iter()
                .any(|argument| self.contains_unsupported_borrowed_nominal(argument)),
            _ => false,
        }
    }

    fn is_direct_borrowed_nominal_list(&self, ty: &Ty) -> bool {
        matches!(self.resolve(ty), Ty::List(element) if self.is_direct_borrowed_nominal(&element))
    }

    fn is_nested_borrowed_nominal_list(&self, ty: &Ty) -> bool {
        match self.resolve(ty) {
            Ty::List(element) => {
                self.is_direct_borrowed_nominal(&element)
                    || matches!(self.resolve(&element), Ty::List(_))
                        && self.is_nested_borrowed_nominal_list(&element)
            }
            _ => false,
        }
    }

    fn borrowed_shell_binding_source(value: &Expr) -> bool {
        matches!(value, Expr::Ctor { .. } | Expr::Call { .. } | Expr::List(_))
    }

    fn is_borrowed_shell_binding(&self, name: &str) -> bool {
        self.borrowed_shell_bindings
            .iter()
            .rev()
            .any(|bindings| bindings.contains(name))
    }

    fn authorize_borrowed_shell_binding(&mut self, name: String) {
        self.borrowed_shell_bindings
            .last_mut()
            .expect("borrowed shell bindings track type scopes")
            .insert(name);
    }

    fn authorize_frozen_binding(&mut self, name: String) {
        self.frozen_bindings
            .last_mut()
            .expect("frozen bindings track type scopes")
            .insert(name);
    }

    fn is_frozen_binding(&self, name: &str) -> bool {
        self.frozen_bindings
            .iter()
            .rev()
            .any(|bindings| bindings.contains(name))
    }

    fn authorize_unique_value_binding(&mut self, name: String) {
        self.unique_value_bindings
            .last_mut()
            .expect("unique bindings track type scopes")
            .insert(name);
    }

    fn authorize_unique_storage_binding(&mut self, name: String) {
        self.unique_storage_bindings
            .last_mut()
            .expect("unique storage bindings track type scopes")
            .insert(name);
    }

    fn remember_unique_binding_type(&mut self, name: String, ty: ast::Type) {
        self.unique_binding_types
            .last_mut()
            .expect("unique binding types track type scopes")
            .insert(name, ty);
    }

    fn unique_binding_type(&self, name: &str) -> Option<ast::Type> {
        self.unique_binding_types
            .iter()
            .rev()
            .find_map(|types| types.get(name).cloned())
    }


    fn remember_callable_unique_contract(&mut self, name: String, contract: UniqueCallableContract) {
        self.callable_unique_contracts
            .last_mut()
            .expect("callable unique contracts track type scopes")
            .insert(name, contract);
    }

    fn callable_unique_contract(&self, name: &str) -> Option<UniqueCallableContract> {
        self.callable_unique_contracts
            .iter()
            .rev()
            .find_map(|contracts| contracts.get(name).cloned())
            .or_else(|| self.fn_unique_contracts.get(name).cloned())
    }

    fn callable_unique_contract_path_for_block(&self, block: &Block) -> Option<CallableContractPath> {
        if !block_can_complete(block) {
            return Some(CallableContractPath::Diverges);
        }
        match block.stmts.last() {
            Some(Stmt::Expr(expression)) => self
                .callable_unique_contract_path_for_expr(expression),
            _ => None,
        }
    }

    fn join_callable_unique_contract_paths(
        paths: impl IntoIterator<Item = CallableContractPath>,
    ) -> Option<CallableContractPath> {
        let contracts = paths
            .into_iter()
            .filter_map(|path| match path {
                CallableContractPath::Completes(contract) => Some(contract),
                CallableContractPath::Diverges => None,
            })
            .collect::<Vec<_>>();
        if contracts.is_empty() {
            Some(CallableContractPath::Diverges)
        } else {
            UniqueCallableContract::join_all(contracts).map(CallableContractPath::Completes)
        }
    }

    fn callable_unique_contract_path_for_expr(
        &self,
        expression: &Expr,
    ) -> Option<CallableContractPath> {
        match expression {
            Expr::Var(name) => self
                .callable_unique_contract(name)
                .map(CallableContractPath::Completes),
            Expr::Lambda { params, ret, .. } => Some(CallableContractPath::Completes(UniqueCallableContract {
                unique_params: params
                    .iter()
                    .map(|parameter| parameter.ty.as_ref().is_some_and(is_unique_value_type))
                    .collect(),
                unique_result: ret.as_ref().is_some_and(is_unique_value_type),
                frozen_params: params
                    .iter()
                    .map(|parameter| parameter.ty.as_ref().is_some_and(is_frozen_value_type))
                    .collect(),
                frozen_result: ret.as_ref().is_some_and(is_frozen_value_type),
            })),
            // An ascription is an explicit function-value weakening boundary:
            // use its declared source contract, never the payload's stronger
            // erased identity.
            Expr::As { ty, .. } => UniqueCallableContract::from_function_type(ty)
                .map(CallableContractPath::Completes),
            Expr::If { then_block, else_block: Some(else_block), .. } => {
                Self::join_callable_unique_contract_paths([
                    self.callable_unique_contract_path_for_block(then_block)?,
                    self.callable_unique_contract_path_for_block(else_block)?,
                ])
            }
            Expr::Match { arms, .. } if !arms.is_empty() => {
                Self::join_callable_unique_contract_paths(
                    arms.iter()
                        .map(|arm| self.callable_unique_contract_path_for_expr(&arm.body))
                        .collect::<Option<Vec<_>>>()?,
                )
            }
            Expr::Block(block) => self.callable_unique_contract_path_for_block(block),
            _ => None,
        }
    }

    fn callable_unique_contract_for_expr(&self, expression: &Expr) -> Option<UniqueCallableContract> {
        match self.callable_unique_contract_path_for_expr(expression)? {
            CallableContractPath::Completes(contract) => Some(contract),
            CallableContractPath::Diverges => None,
        }
    }

    fn remember_opt_callable_provenance(&mut self, name: String, contract: OptCallableContract) {
        self.callable_opt_provenance
            .last_mut()
            .expect("opt callable provenance tracks type scopes")
            .insert(name, contract);
    }

    fn callable_opt_contract(&self, name: &str) -> Option<OptCallableContract> {
        self.callable_opt_provenance
            .iter()
            .rev()
            .find_map(|provenance| provenance.get(name).cloned())
            .or_else(|| self.fn_opt_provenance.get(name).cloned())
    }

    fn remember_container_opt_provenance(
        &mut self,
        name: String,
        contract: Option<OptCallableContract>,
    ) {
        self.container_opt_provenance
            .last_mut()
            .expect("container opt provenance tracks type scopes")
            .insert(name, contract);
    }

    fn container_opt_contract(&self, name: &str) -> Option<Option<OptCallableContract>> {
        self.container_opt_provenance
            .iter()
            .rev()
            .find_map(|provenance| provenance.get(name).cloned())
    }

    fn opt_callable_contract_from_ty(&self, ty: &Ty) -> Option<OptCallableContract> {
        let Ty::Fn(params, result, ..) = self.resolve(ty) else { return None };
        Some(OptCallableContract {
            params: params
                .iter()
                .map(|parameter| self.opt_callable_contract_from_ty(parameter).map(Box::new))
                .collect(),
            result: self.opt_callable_contract_from_ty(&result).map(Box::new),
        })
    }

    fn container_opt_contract_for_expr(&self, expression: &Expr) -> Option<Option<OptCallableContract>> {
        match expression {
            Expr::Var(name) => self.container_opt_contract(name),
            Expr::List(items) => {
                let mut contracts = items.iter().map(|item| self.expression_opt_callable_contract(item));
                let first = contracts.next()?;
                contracts.all(|contract| contract == first).then_some(first)
            }
            Expr::Ctor { name, args } if matches!(name.rsplit('.').next(), Some("None") | Some("Err")) => {
                Some(None)
            }
            Expr::Ctor { name, args } if matches!(name.rsplit('.').next(), Some("Some") | Some("Ok")) => {
                (args.len() == 1).then(|| self.expression_opt_callable_contract(&args[0]))
            }
            Expr::As { expr, .. } => self.container_opt_contract_for_expr(expr),
            Expr::If { then_block, else_block: Some(else_block), .. } => {
                let then_contract = self.block_container_opt_contract(then_block)?;
                let else_contract = self.block_container_opt_contract(else_block)?;
                (then_contract == else_contract).then_some(then_contract)
            }
            Expr::Match { arms, .. } if !arms.is_empty() => {
                let mut contracts = arms.iter().map(|arm| self.container_opt_contract_for_expr(&arm.body));
                let first = contracts.next()??;
                contracts.all(|contract| contract.as_ref() == Some(&first)).then_some(first)
            }
            _ => None,
        }
    }

    fn block_container_opt_contract(&self, block: &Block) -> Option<Option<OptCallableContract>> {
        match block.stmts.last() {
            Some(Stmt::Expr(expr)) | Some(Stmt::Return(Some(expr))) => {
                self.container_opt_contract_for_expr(expr)
            }
            _ => None,
        }
    }

    /// Pattern binding is a projection, not a new provenance source.  Carry a
    /// fact only along the exact source expression/pattern slot that supplied
    /// it; a same-shaped pattern over an unknown aggregate remains untrusted.
    fn remember_pattern_opt_provenance(&mut self, pattern: &Pattern, value: &Expr) {
        match (pattern, value) {
            (Pattern::Var(name), value) => {
                if let Some(contract) = self.expression_opt_callable_contract(value) {
                    self.remember_opt_callable_provenance(name.clone(), contract);
                }
            }
            (Pattern::Tuple(patterns), Expr::Tuple(values)) => {
                for (pattern, value) in patterns.iter().zip(values) {
                    self.remember_pattern_opt_provenance(pattern, value);
                }
            }
            _ => {}
        }
    }

    /// Recognize only the linker's zero-source-line eta adapter for an imported
    /// function value.  This carries the target declaration's contract into
    /// both the adapter value and its generated parameter bindings.
    fn linker_eta_opt_contract(
        &self,
        params: &[ast::Param],
        body: &Block,
    ) -> Option<Option<OptCallableContract>> {
        (params.iter().all(|param| param.name.starts_with("__eta"))
            && body.lines.iter().all(|line| *line == 0)
            && matches!(body.stmts.as_slice(), [Stmt::Expr(Expr::Call { args, .. })] if args.len() == params.len()
                && args.iter().zip(params).all(|(arg, param)| matches!(arg, Expr::Var(name) if name == &param.name))))
            .then(|| match body.stmts.first() {
                Some(Stmt::Expr(Expr::Call { name, .. })) => self.callable_opt_contract(name),
                _ => None,
            })
    }

    /// A callable result from a compatible callable is compatible too: the
    /// originating callable's signature recursively carries its input/result
    /// callable contract.  Every other producer is rejected closed.
    fn expression_opt_callable_contract(&self, expression: &Expr) -> Option<OptCallableContract> {
        match expression {
            Expr::Var(name) => self.callable_opt_contract(name),
            Expr::Lambda { params, body, ret, .. } => {
                // The linker eta-expands imported function values so that the
                // target lowering never needs an import-specific closure ABI.
                // This exact compiler-generated adapter preserves (rather than
                // invents) the origin's declaration contract.  User closures
                // do not have zero-source-line `__eta` parameters and therefore
                // cannot use this route to bless a normal callback.
                // A recognized compiler eta adapter preserves its exact target
                // fact, including absence.  Falling through from a normal
                // target to the enclosing opt callable would manufacture
                // provenance for an ordinary imported function.
                if let Some(target_contract) = self.linker_eta_opt_contract(params, body) {
                    return target_contract;
                }
                self.current_opt_callable.then(|| {
                    // Inferred lambda results still have a checked source
                    // expression.  Preserve that fact instead of dropping it
                    // merely because the author omitted an annotation.
                    let mut contract = OptCallableContract::from_lambda(params, ret.as_ref());
                    if ret.is_none() {
                        contract.result = self.block_opt_callable_contract(body).map(Box::new);
                    }
                    contract
                })
            }
            // An ascription may weaken source-visible *type* qualifiers, but
            // cannot add a missing opt callable origin.
            Expr::As { expr, .. } => self.expression_opt_callable_contract(expr),
            Expr::Call { name, .. } => self
                .callable_opt_contract(name)
                .and_then(|contract| contract.result.map(|result| *result)),
            Expr::Apply { func, .. } => self
                .expression_opt_callable_contract(func)
                .and_then(|contract| contract.result.map(|result| *result)),
            Expr::Binary { op: ast::BinOp::Coalesce, lhs, rhs } => {
                let fallback = self.expression_opt_callable_contract(rhs)?;
                match self.container_opt_contract_for_expr(lhs) {
                    // An authenticated None/Err always selects the fallback.
                    Some(None) => Some(fallback),
                    // A potentially present payload must carry the same
                    // contract as the fallback: either branch may be returned.
                    Some(Some(payload)) if payload == fallback => Some(payload),
                    _ => None,
                }
            }
            // A field or existential result's type describes its ABI, not the
            // origin of the value stored there. Neither operation can mint an
            // opt callable contract from the current body's source mode.
            Expr::Field { .. } | Expr::ExistentialCall { .. } => None,
            Expr::If { then_block, else_block: Some(else_block), .. } => {
                let then_contract = self.block_opt_callable_contract(then_block)?;
                let else_contract = self.block_opt_callable_contract(else_block)?;
                (then_contract == else_contract).then_some(then_contract)
            }
            Expr::Match { arms, .. } if !arms.is_empty() => {
                let mut contracts = arms
                    .iter()
                    .map(|arm| self.expression_opt_callable_contract(&arm.body));
                let first = contracts.next()??;
                contracts.all(|contract| contract.as_ref() == Some(&first)).then_some(first)
            }
            Expr::Block(block) => self.block_opt_callable_contract(block),
            _ => None,
        }
    }

    fn block_opt_callable_contract(&self, block: &Block) -> Option<OptCallableContract> {
        match block.stmts.last() {
            Some(Stmt::Expr(expr)) | Some(Stmt::Return(Some(expr))) => {
                self.expression_opt_callable_contract(expr)
            }
            _ => None,
        }
    }

    fn require_opt_callable_provenance(
        &self,
        argument: &Expr,
        callee: &str,
        index: usize,
    ) -> Result<(), TypeError> {
        if self.expression_opt_callable_contract(argument).is_some() {
            return Ok(());
        }
        terr(format!(
            "argument {} to `{}` requires an opt-compatible callable; normal closures and functions cannot satisfy an opt higher-order contract",
            index + 1,
            diagnostic_callable_name(callee),
        ))
    }

    /// No ordinary function-value, generic, aggregate, or nominal-field ABI
    /// carries opt callable provenance today.  Letting an opt function enter
    /// one would erase the contract and permit a later normal callback to
    /// re-enter an opt higher-order call.  Reject at that first erasing edge;
    /// direct normal use of an opt function (with non-callable arguments) is
    /// unaffected.
    fn reject_opt_callable_erasure(
        &self,
        argument: &Expr,
        context: &str,
    ) -> Result<(), TypeError> {
        if self
            .expression_opt_callable_contract(argument)
            .is_some_and(|contract| contract.has_higher_order_boundary())
        {
            return terr(format!(
                "{context} cannot erase an opt-compatible callable into an ordinary callable contract; pass it directly through an opt-compatible interface"
            ));
        }
        Ok(())
    }

    /// Recover the erased source `frozen` result contract for a value binding.
    /// Unlike `unique`, freezing is not affine, so a frozen local may be copied
    /// into another frozen snapshot. What must never happen is qualifier loss
    /// at a call boundary followed by a deep write through the new binding.
    fn expression_returns_frozen_value(&self, expression: &Expr) -> bool {
        match expression {
            Expr::Var(name) => self.is_frozen_binding(name),
            Expr::Call { name, .. } => self
                .callable_unique_contract(name)
                .is_some_and(|contract| contract.frozen_result),
            Expr::Apply { func, .. } => self
                .callable_unique_contract_for_expr(func)
                .is_some_and(|contract| contract.frozen_result),
            Expr::As { ty, .. } => is_frozen_value_type(ty),
            Expr::Block(block) => block
                .stmts
                .last()
                .is_some_and(|statement| matches!(statement, Stmt::Expr(expression) if self.expression_returns_frozen_value(expression))),
            Expr::If { then_block, else_block: Some(else_block), .. } => {
                self.block_returns_frozen_value(then_block) && self.block_returns_frozen_value(else_block)
            }
            Expr::Match { arms, .. } if !arms.is_empty() => arms
                .iter()
                .all(|arm| self.expression_returns_frozen_value(&arm.body)),
            _ => false,
        }
    }

    fn block_returns_frozen_value(&self, block: &Block) -> bool {
        block.stmts.last().is_some_and(
            |statement| matches!(statement, Stmt::Expr(expression) if self.expression_returns_frozen_value(expression)),
        )
    }

    fn is_unique_value_binding(&self, name: &str) -> bool {
        self.unique_value_bindings
            .iter()
            .rev()
            .any(|bindings| bindings.contains(name))
    }

    fn is_unique_storage_binding(&self, name: &str) -> bool {
        self.unique_storage_bindings
            .iter()
            .rev()
            .any(|bindings| bindings.contains(name))
    }

    fn is_inferred_owner_binding(&self, name: &str) -> bool {
        self.inferred_owner_bindings
            .iter()
            .rev()
            .any(|bindings| bindings.contains(name))
    }

    /// Whether `value` is a call whose result is inferred fresh without a
    /// declared `unique` result: the only shape that yields an inferred owner.
    fn call_yields_inferred_owner(&self, value: &Expr) -> bool {
        self.opt_mode
            && matches!(value, Expr::Call { name, .. }
                if self.fresh_result_fns.contains(name)
                    && !self.callable_unique_contract(name).is_some_and(|contract| contract.unique_result))
    }

    fn forget_inferred_owner_binding(&mut self, name: &str) {
        if let Some(bindings) = self
            .inferred_owner_bindings
            .iter_mut()
            .rev()
            .find(|bindings| bindings.contains(name))
        {
            bindings.remove(name);
        }
    }

    fn declared_type_carries_unique_storage(&self, ty: &ast::Type) -> bool {
        witchy_canonical_source::storage::contains_unique_storage(
            ty, &self.canonical_definitions,
        ).unwrap_or(true)
    }

    fn ty_carries_unique_storage(&self, ty: &Ty) -> bool {
        let resolved = self.resolve(ty);
        if let Some(answer) = self.unique_storage_types.borrow().get(&resolved) {
            return *answer;
        }
        // A free variable contributes no *known* unique field. Keep its
        // position in the source shape so fixed fields of a partially known
        // nominal are still inspected by the shared stored-child judgment.
        // The memo is keyed by the resolved type and is invalidated as
        // inference binds variables. Failed stored-child proofs remain affine.
        let answer = ty_to_ast_with_unresolved(&resolved, UnresolvedAst::NeutralStorage).map_or(true, |source| {
            witchy_canonical_source::storage::contains_unique_storage(
                &source, &self.canonical_definitions,
            ).unwrap_or(true)
        });
        self.unique_storage_types.borrow_mut().insert(resolved, answer);
        answer
    }

    /// Whether evaluating `expression` yields storage that must follow affine
    /// `unique` flow.  A unique whole value is affine; a non-unique record is
    /// only affine below a field declared `unique`, preserving ordinary sibling
    /// fields.  This works exclusively from checked declaration/provenance
    /// facts, never rendered type strings.
    fn expression_carries_unique_storage(&self, expression: &Expr, ty: &Ty) -> bool {
        match expression {
            Expr::Var(name) => {
                self.is_unique_value_binding(name)
                    || self.is_unique_storage_binding(name)
                    || self.ty_carries_unique_storage(ty)
                    || (!self.is_consumed_binding(name)
                        && ty_to_ast(&self.resolve(ty)).is_some_and(|source| {
                            self.storage_plan(&source).requires_affine_owner_token()
                        }))
            }
            Expr::As { expr, .. } | Expr::Unary { op: UnOp::Move, expr } => {
                self.expression_carries_unique_storage(expr, ty)
            }
            Expr::Unary { op: UnOp::Borrow | UnOp::BorrowMut | UnOp::Deref, .. } => false,
            Expr::Field { base, field } => {
                if self
                    .unique_source_type_for_place(expression)
                    .is_some_and(|source| self.declared_type_carries_unique_storage(&source))
                {
                    return true;
                }
                let Some(Ty::Named(record, _)) = self.place_ty(base) else {
                    return false;
                };
                self.unique_record_fields
                    .get(record.as_ref())
                    .is_some_and(|fields| fields.contains(field))
                    || matches!(base.as_ref(), Expr::Field { .. } | Expr::Index { .. })
                        && self.expression_carries_unique_storage(
                            base,
                            &self.place_ty(base).unwrap_or_else(|| ty.clone()),
                        )
            }
            // An outer `unique List(T)` qualifies the list handle, not every
            // `T` extracted from it. The element is affine only when its own
            // checked type carries unique storage.
            Expr::Index { .. } => self
                .unique_source_type_for_place(expression)
                .is_some_and(|source| self.declared_type_carries_unique_storage(&source))
                || self.ty_carries_unique_storage(ty),
            Expr::List(items) | Expr::Tuple(items) => items.iter().any(|item| {
                self.expression_carries_unique_storage(item, &self.fresh_for_unique_probe(item))
            }),
            Expr::Ctor { args, .. } | Expr::AnonCtor { args, .. } => {
                self.ty_carries_unique_storage(ty)
                    || args.iter().any(|arg| self.expression_carries_unique_storage(arg, &self.fresh_for_unique_probe(arg)))
            }
            Expr::RecordUpdate { base, fields, .. } => {
                self.expression_carries_unique_storage(base, &self.fresh_for_unique_probe(base))
                    || fields.iter().any(|(_, value)| self.expression_carries_unique_storage(value, &self.fresh_for_unique_probe(value)))
            }
            // (RFC-0167 §6) A fresh-allocator result is a proven owner only
            // where a proof is required; normal code keeps its copy-correct
            // fallback and the binding stays copyable.
            Expr::Call { name, .. } => {
                (self.opt_mode && self.fresh_result_fns.contains(name))
                    || self
                        .callable_unique_contract(name)
                        .is_some_and(|contract| contract.unique_result)
            }
            Expr::Apply { func, .. } => self
                .callable_unique_contract_for_expr(func)
                .is_some_and(|contract| contract.unique_result),
            _ => self.ty_carries_unique_storage(ty),
        }
    }

    fn reject_opt_unique_call_after_view(&self, argument: &Expr, context: &str) -> Result<(), TypeError> {
        if !self.opt_mode {
            return Ok(());
        }
        if let Some(root) = self.affine_place_root(argument)
            && self.opt_viewed_roots.contains(&root)
        {
            return terr(format!(
                "{context} cannot pass `unique` storage rooted in `{root}` in `mode opt` after forming an immutable view of that root; preserving the view would require a hidden copy"
            ));
        }
        Ok(())
    }

    /// A positive, source-authenticated proof that this expression forms or
    /// carries an outer unique owner. Component contracts are checked by
    /// `expression_establishes_declared_unique_storage` below; a fresh shell
    /// alone never blesses an ordinary child as `unique`.
    fn expression_establishes_unique_storage(&self, expression: &Expr, ty: &Ty) -> bool {
        match expression {
            Expr::Var(_) | Expr::Field { .. } | Expr::Index { .. } => {
                self.expression_carries_unique_storage(expression, ty)
            }
            // `move` transfers an already-proven owner; it cannot manufacture
            // one from an ordinary root.
            Expr::Unary { op: UnOp::Move, expr } => {
                self.expression_establishes_unique_storage(expr, ty)
            }
            // Ascribing a weaker source type deliberately drops provenance.
            // An outer unique ascription still needs the payload proof.
            Expr::As { expr, ty: target } => {
                self.declared_type_carries_unique_storage(target)
                    && self.expression_establishes_unique_storage(expr, ty)
            }
            Expr::Int(_) | Expr::Float(_) | Expr::Duration(_) | Expr::Str(_) | Expr::Bool(_) => true,
            // Each of these allocates a new shell. Its components are checked
            // for affine transfer separately; their ordinary provenance cannot
            // make the freshly formed aggregate ordinary.
            Expr::List(_) | Expr::Tuple(_) | Expr::Ctor { .. } | Expr::AnonCtor { .. } => true,
            Expr::Record { spread: None, .. } => true,
            Expr::Record { spread: Some(_), .. } | Expr::RecordUpdate { .. } => false,
            Expr::Call { name, .. } => {
                (self.opt_mode && self.fresh_result_fns.contains(name))
                    || self
                        .callable_unique_contract(name)
                        .is_some_and(|contract| contract.unique_result)
            }
            Expr::Apply { func, .. } => self
                .callable_unique_contract_for_expr(func)
                .is_some_and(|contract| contract.unique_result),
            Expr::If { then_block, else_block: Some(else_block), .. } => {
                self.block_establishes_unique_storage(then_block)
                    && self.block_establishes_unique_storage(else_block)
            }
            Expr::Match { arms, .. } if !arms.is_empty() => arms
                .iter()
                .all(|arm| self.expression_establishes_unique_storage(
                    &arm.body,
                    &self.fresh_for_unique_probe(&arm.body),
                )),
            Expr::Block(block) => self.block_establishes_unique_storage(block),
            _ => false,
        }
    }

    fn block_establishes_unique_storage(&self, block: &Block) -> bool {
        if !block_can_complete(block) {
            return true;
        }
        match block.stmts.last() {
            Some(Stmt::Expr(expression)) => self.expression_establishes_unique_storage(
                expression,
                &self.fresh_for_unique_probe(expression),
            ),
            _ => false,
        }
    }

    /// Does a qualifier-bearing source declaration prove every `unique`
    /// position required by `target`? `Ty` cannot answer this: it deliberately
    /// erases these qualifiers, including qualifiers nested in aggregates.
    fn source_type_proves_unique_storage(source: &ast::Type, target: &ast::Type) -> bool {
        match target {
            ast::Type::Qualified(ast::TypeQual::Unique, target_inner) => {
                matches!(source, ast::Type::Qualified(ast::TypeQual::Unique, source_inner)
                    if Self::source_type_proves_unique_storage(source_inner, target_inner))
            }
            ast::Type::Qualified(ast::TypeQual::Frozen, target_inner) => match source {
                ast::Type::Qualified(ast::TypeQual::Frozen, source_inner) => {
                    Self::source_type_proves_unique_storage(source_inner, target_inner)
                }
                _ => false,
            },
            ast::Type::Qualified(ast::TypeQual::View, _) => false,
            ast::Type::Qualified(
                ast::TypeQual::Borrow(_)
                | ast::TypeQual::LegacyBorrow(_)
                | ast::TypeQual::BorrowMut(_),
                _,
            ) => false,
            ast::Type::Slice(target_inner) => match source {
                ast::Type::Slice(source_inner) => {
                    Self::source_type_proves_unique_storage(source_inner, target_inner)
                }
                _ => false,
            },
            ast::Type::Named(target_name, target_arguments) => match source {
                ast::Type::Named(source_name, source_arguments)
                    if source_name == target_name && source_arguments.len() == target_arguments.len() =>
                {
                    source_arguments
                        .iter()
                        .zip(target_arguments)
                        .all(|(source, target)| Self::source_type_proves_unique_storage(source, target))
                }
                _ => false,
            },
            ast::Type::Tuple(target_items) => match source {
                ast::Type::Tuple(source_items) if source_items.len() == target_items.len() => source_items
                    .iter()
                    .zip(target_items)
                    .all(|(source, target)| Self::source_type_proves_unique_storage(source, target)),
                _ => false,
            },
            ast::Type::RecordCompose { base: target_base, fields: target_fields } => match source {
                ast::Type::RecordCompose { base: source_base, fields: source_fields } => {
                    Self::source_type_proves_unique_storage(source_base, target_base)
                        && target_fields.iter().all(|(target_name, target_field)| {
                            source_fields
                                .iter()
                                .find(|(source_name, _)| source_name == target_name)
                                .is_some_and(|(_, source_field)| {
                                    Self::source_type_proves_unique_storage(source_field, target_field)
                                })
                        })
                }
                _ => false,
            },
            // These leaves cannot contain a nested storage qualifier.
            ast::Type::Fn(..) | ast::Type::Dyn(..) => true,
        }
    }

    fn expression_source_type_for_unique_storage(&self, expression: &Expr) -> Option<ast::Type> {
        match expression {
            Expr::Var(_) | Expr::Field { .. } | Expr::Index { .. } => {
                self.unique_source_type_for_place(expression)
            }
            Expr::Unary { op: UnOp::Move, expr } | Expr::As { expr, .. } => {
                self.expression_source_type_for_unique_storage(expr)
            }
            _ => None,
        }
    }

    fn declared_type_requires_only_outer_unique(&self, declared: &ast::Type) -> bool {
        match declared {
            ast::Type::Qualified(ast::TypeQual::Frozen, inner) => {
                self.declared_type_requires_only_outer_unique(inner)
            }
            ast::Type::Qualified(ast::TypeQual::Unique, inner) => {
                !self.declared_type_carries_unique_storage(inner)
            }
            _ => false,
        }
    }

    fn expression_establishes_declared_unique_storage(
        &self,
        expression: &Expr,
        declared: &ast::Type,
        ty: &Ty,
    ) -> bool {
        if !self.declared_type_carries_unique_storage(declared) {
            return true;
        }
        if self
            .expression_source_type_for_unique_storage(expression)
            .is_some_and(|source| Self::source_type_proves_unique_storage(&source, declared))
        {
            return true;
        }

        match (expression, declared) {
            (Expr::Unary { op: UnOp::Move, expr }, _) | (Expr::As { expr, .. }, _) => self
                .expression_establishes_declared_unique_storage(expr, declared, ty),
            (_, ast::Type::Qualified(ast::TypeQual::Unique, inner)) => {
                self.expression_establishes_unique_storage(expression, ty)
                    && self.expression_establishes_declared_unique_storage(expression, inner, ty)
            }
            (_, ast::Type::Qualified(ast::TypeQual::Frozen, inner)) => {
                self.expression_establishes_declared_unique_storage(expression, inner, ty)
            }
            (Expr::List(items), ast::Type::Named(name, arguments))
                if name.rsplit('.').next() == Some("List") && arguments.len() == 1 =>
            {
                items.iter().all(|item| {
                    self.expression_establishes_declared_unique_storage(
                        item,
                        &arguments[0],
                        &self.fresh_for_unique_probe(item),
                    )
                })
            }
            (Expr::Tuple(items), ast::Type::Tuple(declared_items)) if items.len() == declared_items.len() => {
                items.iter().zip(declared_items).all(|(item, declared)| {
                    self.expression_establishes_declared_unique_storage(
                        item,
                        declared,
                        &self.fresh_for_unique_probe(item),
                    )
                })
            }
            (Expr::Ctor { name, args }, ast::Type::Named(_, arguments)) => {
                let Some(field_types) = self.unique_ctor_field_types.get(name) else {
                    return false;
                };
                let Some(parameters) = self.ctor_source_type_params.get(name) else {
                    return false;
                };
                if field_types.len() != args.len() || parameters.len() != arguments.len() {
                    return false;
                }
                let substitutions = parameters.iter().cloned().zip(arguments.iter().cloned()).collect();
                args.iter().zip(field_types).all(|(argument, field)| {
                    let field = Self::substitute_unique_source_type(field, &substitutions);
                    self.expression_establishes_declared_unique_storage(
                        argument,
                        &field,
                        &self.fresh_for_unique_probe(argument),
                    )
                })
            }
            (Expr::Record { fields, spread: None, .. }, ast::Type::Named(record, arguments)) => {
                let Some(field_types) = self.unique_source_record_fields.get(record) else {
                    return false;
                };
                let Some(parameters) = self.unique_source_record_params.get(record) else {
                    return false;
                };
                if parameters.len() != arguments.len() {
                    return false;
                }
                let substitutions = parameters.iter().cloned().zip(arguments.iter().cloned()).collect();
                field_types.iter().all(|(name, field)| {
                    let field = Self::substitute_unique_source_type(field, &substitutions);
                    if !self.declared_type_carries_unique_storage(&field) {
                        return true;
                    }
                    fields.iter().find(|(candidate, _)| candidate == name).is_some_and(|(_, value)| {
                        self.expression_establishes_declared_unique_storage(
                            value,
                            &field,
                            &self.fresh_for_unique_probe(value),
                        )
                    })
                })
            }
            (Expr::Call { name, .. }, _) => self.declared_type_requires_only_outer_unique(declared)
                && self.callable_unique_contract(name).is_some_and(|contract| contract.unique_result),
            (Expr::Apply { func, .. }, _) => self.declared_type_requires_only_outer_unique(declared)
                && self
                    .callable_unique_contract_for_expr(func)
                    .is_some_and(|contract| contract.unique_result),
            (Expr::If { then_block, else_block: Some(else_block), .. }, _) => {
                self.block_establishes_declared_unique_storage(then_block, declared)
                    && self.block_establishes_declared_unique_storage(else_block, declared)
            }
            (Expr::Match { arms, .. }, _) if !arms.is_empty() => arms.iter().all(|arm| {
                self.expression_establishes_declared_unique_storage(
                    &arm.body,
                    declared,
                    &self.fresh_for_unique_probe(&arm.body),
                )
            }),
            (Expr::Block(block), _) => self.block_establishes_declared_unique_storage(block, declared),
            _ => false,
        }
    }

    fn block_establishes_declared_unique_storage(&self, block: &Block, declared: &ast::Type) -> bool {
        if !block_can_complete(block) {
            return true;
        }
        match block.stmts.last() {
            Some(Stmt::Expr(expression)) => self.expression_establishes_declared_unique_storage(
                expression,
                declared,
                &self.fresh_for_unique_probe(expression),
            ),
            _ => false,
        }
    }

    /// Unique provenance probes only need a conservative checked type.  Local
    /// roots retain their exact type; compound forms are handled structurally
    /// above, so an unresolved placeholder cannot manufacture affinity.
    fn fresh_for_unique_probe(&self, expression: &Expr) -> Ty {
        match expression {
            Expr::Var(name) => self.lookup(name).unwrap_or(Ty::Unit),
            Expr::Field { .. } | Expr::Index { .. } => self.place_ty(expression).unwrap_or(Ty::Unit),
            _ => Ty::Unit,
        }
    }

    fn unique_source_type_for_place(&self, expression: &Expr) -> Option<ast::Type> {
        match expression {
            Expr::Var(name) => self.unique_binding_type(name),
            Expr::Index { base, .. } => match self.unique_source_type_for_place(base)? {
                ast::Type::Named(name, arguments) if name.rsplit('.').next() == Some("List") => {
                    arguments.into_iter().next()
                }
                _ => None,
            },
            Expr::Field { base, field } => {
                let ast::Type::Named(record, arguments) = self.unique_source_type_for_place(base)? else {
                    return None;
                };
                let source_field = self.unique_source_record_fields.get(&record)?.get(field)?;
                let params = self.unique_source_record_params.get(&record)?;
                let substitutions = params.iter().cloned().zip(arguments).collect();
                Some(Self::substitute_unique_source_type(source_field, &substitutions))
            }
            _ => None,
        }
    }

    fn transfer_unique_value(
        &mut self,
        expression: &Expr,
        ty: &Ty,
        context: &str,
    ) -> Result<(), TypeError> {
        let into_unique = self.ty_carries_unique_storage(ty);
        self.transfer_unique_value_into(expression, ty, into_unique, context)
    }

    /// `into_unique` says whether the receiving position is itself declared
    /// `unique`. `Ty` erases source qualifiers, so an argument site that knows
    /// the callee's contract passes the flag explicitly.
    fn transfer_unique_value_into(
        &mut self,
        expression: &Expr,
        ty: &Ty,
        into_unique: bool,
        context: &str,
    ) -> Result<(), TypeError> {
        if !self.expression_carries_unique_storage(expression, ty) {
            return Ok(());
        }
        let Some(root) = self.affine_place_root(expression) else {
            return Ok(());
        };
        if self.is_consumed_binding(&root) {
            return terr(format!(
                "use of unique value `{root}` after it was transferred ({context})"
            ));
        }
        // (RFC-0167 §4) A declared `unique` value is affine: transferring it
        // consumes the binding. An owner that is only *proven* fresh (an
        // inferred allocator result) instead demotes: coercing it into an
        // ordinary position gives up the guarantee, and ordinary copies may
        // be made afterward — a later `unique`-requiring use is what fails.
        if self.is_unique_value_binding(&root) || into_unique || !self.is_inferred_owner_binding(&root) {
            self.mark_consumed_binding(&root);
        } else {
            if let Some(bindings) = self
                .unique_storage_bindings
                .iter_mut()
                .rev()
                .find(|bindings| bindings.contains(&root))
            {
                bindings.remove(&root);
            }
            self.forget_inferred_owner_binding(&root);
        }
        Ok(())
    }

    /// Substitute nominal source parameters before walking qualifier storage.
    /// `Ty` deliberately erases those qualifiers, so this source shape remains
    /// the authority for extraction provenance. The substitution is structural:
    /// `List(a)`, `(a, Int)`, and `Pair(a)` retain a supplied `unique` argument.
    fn substitute_unique_source_type(
        source: &ast::Type,
        substitutions: &HashMap<String, ast::Type>,
    ) -> ast::Type {
        match source {
            ast::Type::Named(name, arguments) if arguments.is_empty() => substitutions
                .get(name)
                .cloned()
                .unwrap_or_else(|| source.clone()),
            ast::Type::Named(name, arguments) => ast::Type::Named(
                name.clone(),
                arguments
                    .iter()
                    .map(|argument| Self::substitute_unique_source_type(argument, substitutions))
                    .collect(),
            ),
            ast::Type::Tuple(items) => ast::Type::Tuple(
                items
                    .iter()
                    .map(|item| Self::substitute_unique_source_type(item, substitutions))
                    .collect(),
            ),
            ast::Type::Slice(item) => ast::Type::Slice(Box::new(
                Self::substitute_unique_source_type(item, substitutions),
            )),
            ast::Type::RecordCompose { base, fields } => ast::Type::RecordCompose {
                base: Box::new(Self::substitute_unique_source_type(base, substitutions)),
                fields: fields
                    .iter()
                    .map(|(name, field)| {
                        (
                            name.clone(),
                            Self::substitute_unique_source_type(field, substitutions),
                        )
                    })
                    .collect(),
            },
            ast::Type::Fn(inputs, output, conventions, qualifiers) => ast::Type::Fn(
                inputs
                    .iter()
                    .map(|input| Self::substitute_unique_source_type(input, substitutions))
                    .collect(),
                Box::new(Self::substitute_unique_source_type(output, substitutions)),
                conventions.clone(),
                *qualifiers,
            ),
            ast::Type::Qualified(qualifier, inner) => ast::Type::Qualified(
                qualifier.clone(),
                Box::new(Self::substitute_unique_source_type(inner, substitutions)),
            ),
            ast::Type::Dyn(name, arguments) => ast::Type::Dyn(
                name.clone(),
                arguments
                    .iter()
                    .map(|argument| Self::substitute_unique_source_type(argument, substitutions))
                    .collect(),
            ),
        }
    }

    fn bind_unique_source_parameters(
        source: &ast::Type,
        value: &ast::Type,
        parameters: &[String],
        bindings: &mut HashMap<String, ast::Type>,
    ) {
        match source {
            ast::Type::Named(name, arguments)
                if arguments.is_empty() && parameters.contains(name) =>
            {
                bindings.entry(name.clone()).or_insert_with(|| value.clone());
            }
            ast::Type::Named(_, source_arguments) => {
                if let ast::Type::Named(_, value_arguments) = value {
                    for (source_argument, value_argument) in
                        source_arguments.iter().zip(value_arguments)
                    {
                        Self::bind_unique_source_parameters(
                            source_argument,
                            value_argument,
                            parameters,
                            bindings,
                        );
                    }
                }
            }
            ast::Type::Tuple(source_items) => {
                if let ast::Type::Tuple(value_items) = value {
                    for (source_item, value_item) in source_items.iter().zip(value_items) {
                        Self::bind_unique_source_parameters(
                            source_item,
                            value_item,
                            parameters,
                            bindings,
                        );
                    }
                }
            }
            ast::Type::Qualified(_, source_inner) | ast::Type::Slice(source_inner) => {
                let value_inner = match value {
                    ast::Type::Qualified(_, value_inner) | ast::Type::Slice(value_inner) => value_inner,
                    _ => value,
                };
                Self::bind_unique_source_parameters(
                    source_inner,
                    value_inner,
                    parameters,
                    bindings,
                );
            }
            ast::Type::RecordCompose { .. } | ast::Type::Fn(..) | ast::Type::Dyn(..) => {}
        }
    }

    fn inferred_unique_source_type(&self, expression: &Expr, ty: &Ty) -> Option<ast::Type> {
        match expression {
            Expr::Var(name) => self.unique_binding_type(name),
            Expr::As { ty, .. } => Some(ty.clone()),
            Expr::Tuple(items) => Some(ast::Type::Tuple(
                items
                    .iter()
                    .map(|item| {
                        self.inferred_unique_source_type(item, &self.fresh_for_unique_probe(item))
                            .unwrap_or_else(|| ast::Type::Named("Unit".into(), Vec::new()))
                    })
                    .collect(),
            )),
            Expr::List(items) => items.iter().find_map(|item| {
                self.inferred_unique_source_type(item, &self.fresh_for_unique_probe(item))
                    .map(|item| ast::Type::Named("List".into(), vec![item]))
            }),
            Expr::Ctor { name, args } => {
                let source_fields = self.unique_ctor_field_types.get(name)?;
                let parameters = self.ctor_source_type_params.get(name)?;
                let Ty::Named(result_name, _) = self.resolve(ty) else {
                    return None;
                };
                let mut bindings = HashMap::new();
                for (source_field, argument) in source_fields.iter().zip(args) {
                    if let Some(argument_source) = self.inferred_unique_source_type(
                        argument,
                        &self.fresh_for_unique_probe(argument),
                    ) {
                        Self::bind_unique_source_parameters(
                            source_field,
                            &argument_source,
                            parameters,
                            &mut bindings,
                        );
                    }
                }
                (!bindings.is_empty()).then(|| {
                    ast::Type::Named(
                        result_name.to_string(),
                        parameters
                            .iter()
                            .map(|parameter| {
                                bindings
                                    .get(parameter)
                                    .cloned()
                                    .unwrap_or_else(|| ast::Type::Named("Unit".into(), Vec::new()))
                            })
                            .collect(),
                    )
                })
            }
            _ => None,
        }
    }

    fn authorize_unique_pattern_storage(&mut self, pattern: &Pattern, declared: &ast::Type) {
        if let Pattern::Or(alternatives) = pattern {
            for alternative in alternatives {
                self.authorize_unique_pattern_storage(alternative, declared);
            }
            return;
        }
        match declared {
            // Moving a unique whole value into a single binding preserves the
            // handle contract. Structural extraction instead follows the
            // payload declaration, so ordinary sibling fields stay copyable.
            ast::Type::Qualified(ast::TypeQual::Unique, inner) => match pattern {
                Pattern::Var(name) => self.authorize_unique_storage_binding(name.clone()),
                _ => self.authorize_unique_pattern_storage(pattern, inner),
            },
            ast::Type::Qualified(_, inner) | ast::Type::Slice(inner) => {
                self.authorize_unique_pattern_storage(pattern, inner)
            }
            ast::Type::Tuple(types) => {
                if let Pattern::Tuple(items) = pattern {
                    for (item, item_type) in items.iter().zip(types) {
                        self.authorize_unique_pattern_storage(item, item_type);
                    }
                }
            }
            ast::Type::Named(name, arguments) => {
                if let Pattern::Var(binding) = pattern
                    && self.declared_type_carries_unique_storage(declared)
                {
                    self.authorize_unique_storage_binding(binding.clone());
                } else if name.rsplit('.').next() == Some("List")
                    && let (Some(element), Pattern::List { elems, rest }) =
                        (arguments.first(), pattern)
                {
                    for element_pattern in elems {
                        self.authorize_unique_pattern_storage(element_pattern, element);
                    }
                    if let Some(Some(rest)) = rest
                        && self.declared_type_carries_unique_storage(element)
                    {
                        self.authorize_unique_storage_binding(rest.clone());
                    }
                } else if let Pattern::Ctor { name, args } = pattern
                    && let Some(field_types) = self.unique_ctor_field_types.get(name).cloned()
                {
                    let parameters = self
                        .ctor_source_type_params
                        .get(name)
                        .cloned()
                        .unwrap_or_default();
                    let substitutions = parameters
                        .iter()
                        .cloned()
                        .zip(arguments.iter().cloned())
                        .collect();
                    for (argument, field_type) in args.iter().zip(&field_types) {
                        let field_type =
                            Self::substitute_unique_source_type(field_type, &substitutions);
                        self.authorize_unique_pattern_storage(argument, &field_type);
                    }
                } else if let Pattern::AnonCtor { tag, args } = pattern
                    && let Some(variants) = anon_union_synthetic_variants(name)
                {
                    // Anonymous-union payload shapes live in the synthetic
                    // head while their source-qualified payload types remain
                    // ordinary type arguments.  Recover the selected tag's
                    // argument range before `Ty` erasure so a unique payload
                    // is affine without making an ordinary sibling affine.
                    let mut offset = 0usize;
                    for (variant, arity) in variants {
                        let end = offset.saturating_add(arity);
                        if variant == *tag {
                            for (argument, field_type) in args
                                .iter()
                                .zip(arguments.get(offset..end).unwrap_or_default())
                            {
                                self.authorize_unique_pattern_storage(argument, field_type);
                            }
                            break;
                        }
                        offset = end;
                    }
                }
            }
            ast::Type::Dyn(..) | ast::Type::Fn(..) | ast::Type::RecordCompose { .. } => {}
        }
    }

    fn place_root(expr: &Expr) -> Option<&str> {
        match expr {
            Expr::Var(name) => Some(name),
            Expr::Field { base, .. } | Expr::Index { base, .. } => Self::place_root(base),
            Expr::Unary { op: UnOp::Deref, expr } => Self::place_root(expr),
            _ => None,
        }
    }

    fn place_ty(&self, expr: &Expr) -> Option<Ty> {
        match expr {
            Expr::Var(name) => self.lookup(name).map(|ty| self.resolve(&ty)),
            Expr::Field { base, field } => {
                let Ty::Named(name, args) = self.place_ty(base)? else {
                    return None;
                };
                let (parameters, fields) = self.record_fields.get(name.as_ref())?;
                let (_, field_ty) = fields.iter().find(|(name, _)| name == field)?;
                let substitutions: HashMap<u32, Ty> = parameters
                    .iter()
                    .copied()
                    .zip(args.iter().cloned())
                    .collect();
                Some(self.resolve(&self.subst_vars(field_ty, &substitutions)))
            }
            Expr::Index { base, .. } => match self.place_ty(base)? {
                Ty::List(element) => Some(element.as_ref().clone()),
                _ => None,
            },
            Expr::Unary { op: UnOp::Deref, expr } => self.place_ty(expr),
            _ => None,
        }
    }

    fn frozen_binding_root<'a>(&self, expr: &'a Expr) -> Option<&'a str> {
        match expr {
            Expr::Var(name) if self.is_frozen_binding(name) => Some(name),
            Expr::Field { base, field } => self.frozen_binding_root(base).or_else(|| {
                let Ty::Named(record, _) = self.place_ty(base)? else {
                    return None;
                };
                self.frozen_record_fields
                    .get(record.as_ref())
                    .filter(|fields| fields.contains(field))
                    .and_then(|_| Self::place_root(base))
            }),
            Expr::Index { base, .. } | Expr::Unary { op: UnOp::Deref, expr: base } => {
                self.frozen_binding_root(base)
            }
            _ => None,
        }
    }

    /// Classify the exact destination recovered from RFC-0022 assignment
    /// desugaring. A frozen field is local to that projection: sibling fields
    /// stay writable, while every index/field below the frozen field remains
    /// immutable. If a non-root assignment has lost its canonical place shape,
    /// reject rather than silently treating it as a root-only replacement.
    fn assignment_targets_frozen_storage(
        &self,
        root: &str,
        value: &Expr,
        place_depth: u32,
    ) -> Result<bool, ()> {
        if place_depth == 0 {
            return Ok(false);
        }
        if self.is_frozen_binding(root) {
            return Ok(true);
        }
        let place = witchy_canonical_source::access::checked_assignment_place(root, value, place_depth);
        if place.steps().len() != place_depth as usize {
            return Err(());
        }
        let mut current = self.lookup(root).map(|ty| self.resolve(&ty)).ok_or(())?;
        for step in place.steps() {
            match (self.resolve(&current), step) {
                (Ty::Named(record, args), witchy_canonical_source::access::CheckedPlaceStep::Field(field)) => {
                    if self
                        .frozen_record_fields
                        .get(record.as_ref())
                        .is_some_and(|fields| fields.contains(field))
                    {
                        return Ok(true);
                    }
                    let (parameters, fields) = self.record_fields.get(record.as_ref()).ok_or(())?;
                    let (_, field_ty) = fields.iter().find(|(name, _)| name == field).ok_or(())?;
                    let substitutions: HashMap<u32, Ty> = parameters
                        .iter()
                        .copied()
                        .zip(args.iter().cloned())
                        .collect();
                    current = self.subst_vars(field_ty, &substitutions);
                }
                (Ty::List(element), witchy_canonical_source::access::CheckedPlaceStep::Index(_)
                    | witchy_canonical_source::access::CheckedPlaceStep::DynamicIndex) => {
                    current = element.as_ref().clone();
                }
                (Ty::Named(name, arguments), witchy_canonical_source::access::CheckedPlaceStep::DynamicIndex)
                    if name.as_ref() == "Dict" && arguments.len() == 2 => {
                    current = arguments[1].clone();
                }
                _ => return Err(()),
            }
        }
        Ok(false)
    }

    fn exclusive_borrow_targets_frozen_storage(&self, expr: &Expr) -> bool {
        match expr {
            Expr::Var(name) => self.is_frozen_binding(name),
            Expr::Field { base, .. } | Expr::Index { base, .. } => {
                self.exclusive_borrow_targets_frozen_storage(base)
            }
            Expr::Unary { op: UnOp::Deref, expr } => {
                self.exclusive_borrow_targets_frozen_storage(expr)
            }
            _ => false,
        }
    }

    fn is_authorized_borrowed_shell_value(&self, value: &Expr, ty: &Ty) -> bool {
        self.is_direct_borrowed_nominal(ty)
            && matches!(value, Expr::Var(name) if self.is_borrowed_shell_binding(name))
    }

    fn is_nested_borrowed_nominal_projection(&self, value: &Expr, ty: &Ty) -> bool {
        if !self.is_direct_borrowed_nominal(ty) {
            return false;
        }
        match value {
            Expr::Call { name, .. } => {
                witchy_syntax::intrinsics::canonical_operation_name(name)
                    == witchy_syntax::intrinsics::LIST_AT
            }
            _ => false,
        }
    }

    fn is_borrowed_shell_self_update(&self, name: &str, existing: &Ty, value: &Expr) -> bool {
        self.is_direct_borrowed_nominal(existing)
            && self.is_borrowed_shell_binding(name)
            && matches!(value, Expr::RecordUpdate { base, .. }
                if matches!(base.as_ref(), Expr::Var(base) if base == name))
    }

    fn is_must_preserving_self_update(&self, name: &str, value: &Expr) -> bool {
        matches!(value, Expr::Call { name: operation, args }
            if witchy_syntax::intrinsics::canonical_operation_name(operation)
                == witchy_syntax::intrinsics::LIST_PUSH
                && matches!(args.first(), Some(Expr::Var(base)) if base == name))
    }

    fn is_authorized_borrowed_shell_update(&self, base: &Expr, ty: &Ty) -> bool {
        self.is_authorized_borrowed_shell_value(base, ty)
            && self.borrowed_shell_update_target.as_ref().is_some_and(|target| {
                matches!(base, Expr::Var(base) if base == target)
            })
    }

    /// RFC-0112 stage 1 freezes the type-level contract before any value-level
    /// owner-root ABI exists. Keeping this one typed guard shared by field,
    /// pattern, update, and call paths prevents a new expression lowering from
    /// silently treating a borrowed shell as an ordinary owning aggregate.
    fn reject_borrowed_nominal_runtime_ty(
        &self,
        ty: &Ty,
        operation: &str,
    ) -> Result<(), TypeError> {
        if !self.contains_unsupported_borrowed_nominal(ty) {
            return Ok(());
        }
        let Some(name) = self.borrowed_nominal_name(ty) else { return Ok(()) };
        terr(format!(
            "{operation} uses borrowed nominal type `{name}`, but RFC-0112 stage 1 preserves \
             syntax, kinds, signatures, and reflection only. Wait for projection-aware loans \
             and runtime owner-root lowering"
        ))
    }

    fn reject_borrowed_nominal_container_type(
        &self,
        ty: &ast::Type,
        context: &str,
    ) -> Result<(), TypeError> {
        reject_borrowed_nominal_containers(ty, &self.borrowed_nominal_types, context)
    }

    /// An admitted reference-bearing carrier is not permission to hide its
    /// relation behind an existential or an unannotated callable result.
    fn reject_borrowed_nominal_erasure(
        &self,
        ty: &Ty,
        operation: &str,
    ) -> Result<(), TypeError> {
        self.reject_borrowed_nominal_runtime_ty(ty, operation)?;
        if let Some(name) = self.borrowed_nominal_name(ty) {
            return terr(format!(
                "{operation} would erase borrowed nominal type `{name}` and its owner/lifetime relation; materialize a reference-free value or use an explicit relation-preserving contract"
            ));
        }
        Ok(())
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
                if n == "Option".into() && args.len() == 1 && self.ty_is_direct_externref_value(&args[0]) =>
            {
                Ok(())
            }
            Ty::Named(n, args) if n == "Dict".into() => {
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
            Ty::Fn(params, ret, _, _, _) => {
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
                self.bind_subst_var(*x, other.clone());
                Ok(())
            }
            (Ty::List(x), Ty::List(y)) => self.unify(x, y),
            (Ty::View(x), Ty::View(y)) => self.unify(x, y),
            (Ty::Tuple(xs), Ty::Tuple(ys)) if xs.len() == ys.len() => {
                for (x, y) in xs.iter().zip(ys.iter()) {
                    self.unify(x, y)?;
                }
                Ok(())
            }
            (Ty::Named(x, xa), Ty::Named(y, ya))
                if same_builtin_nominal_head(x, y) && xa.len() == ya.len() =>
            {
                for (p, q) in xa.iter().zip(ya.iter()) {
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
                for (p, q) in xa.iter().zip(ya.iter()) {
                    self.unify(p, q)?;
                }
                Ok(())
            }
            (Ty::Fn(xp, xr, xc, xrefs, xq), Ty::Fn(yp, yr, yc, yrefs, yq))
                if xp.len() == yp.len() && xc == yc && xq == yq =>
            {
                let reference_positions_match = (0..xp.len()).all(|index| {
                    xrefs.get(index).copied().unwrap_or(false)
                        == yrefs.get(index).copied().unwrap_or(false)
                });
                if !reference_positions_match {
                    return terr(
                        "function type erases or changes its borrow/convention relation",
                    );
                }
                // Lifetime names in a function type are local universal binders,
                // so `fn(...'a...)` and `fn(...'b...)` have the same identity
                // when their relation positions agree. Normalize each callable
                // independently before ordinary structural unification; nested
                // functions normalize in their own scope.
                let (xp, xr) = alpha_normalize_callable(xp, xr);
                let (yp, yr) = alpha_normalize_callable(yp, yr);
                for (p, q) in xp.iter().zip(&yp) {
                    self.unify(p, q)?;
                }
                self.unify(&xr, &yr)
            }
            _ if a == b => Ok(()),
            _ => terr(format!("expected `{a}`, found `{b}`")),
        }
    }

    /// Least upper bound for branch results. Callable purity may widen away at
    /// a join; invocation cardinality never does.
    fn branch_lub(&mut self, left: &Ty, right: &Ty) -> Result<Ty, TypeError> {
        let left = self.resolve(left);
        let right = self.resolve(right);
        if let (
            Ty::Fn(lp, lr, lc, lrefs, lq),
            Ty::Fn(rp, rr, rc, rrefs, rq),
        ) = (&left, &right)
        {
            if lq.once != rq.once {
                return terr(format!(
                    "callable branches disagree on invocation cardinality: `{left}` versus `{right}`"
                ));
            }
            let qualifiers = CallableQualifiers::new(lq.pure && rq.pure, lq.once);
            let widened_left = Ty::Fn(
                lp.clone(),
                lr.clone(),
                lc.clone(),
                lrefs.clone(),
                qualifiers,
            );
            let widened_right = Ty::Fn(
                rp.clone(),
                rr.clone(),
                rc.clone(),
                rrefs.clone(),
                qualifiers,
            );
            self.unify(&widened_left, &widened_right)?;
            return Ok(self.resolve(&widened_left));
        }
        self.unify(&left, &right)?;
        Ok(self.resolve(&left))
    }

    /// Whether type variable `x` occurs anywhere inside `t` (after resolving
    /// intermediate variables) — the standard infinite-type guard.
    fn occurs(&self, x: u32, t: &Ty) -> bool {
        match self.resolve(t) {
            Ty::Var(y) => x == y,
            Ty::List(inner) => self.occurs(x, &inner),
            Ty::View(payload) => self.occurs(x, &payload),
            Ty::Tuple(items) => items.iter().any(|i| self.occurs(x, i)),
            Ty::Named(_, args) | Ty::Dyn(_, args) => args.iter().any(|a| self.occurs(x, a)),
            Ty::Fn(params, ret, _, _, _) => {
                params.iter().any(|p| self.occurs(x, p)) || self.occurs(x, &ret)
            }
            _ => false,
        }
    }

    // --- scope helpers ---
    fn push(&mut self) {
        self.scopes.push(HashMap::new());
        self.borrowed_shell_bindings.push(HashSet::new());
        self.explicit_reference_bindings.push(HashSet::new());
        self.frozen_bindings.push(HashSet::new());
        self.unique_value_bindings.push(HashSet::new());
        self.unique_storage_bindings.push(HashSet::new());
        self.inferred_owner_bindings.push(HashSet::new());
        self.unique_binding_types.push(HashMap::new());
        self.callable_unique_contracts.push(HashMap::new());
        self.callable_opt_provenance.push(HashMap::new());
        self.container_opt_provenance.push(HashMap::new());
    }
    fn pop(&mut self) {
        let scope = self.scopes.len().saturating_sub(1);
        self.consumed
            .retain(|(binding_scope, _)| *binding_scope != scope);
        self.once_invoked
            .retain(|(binding_scope, _)| *binding_scope != scope);
        self.must_live.retain(|(binding_scope, _)| *binding_scope != scope);
        self.must_borrowed
            .retain(|(binding_scope, _)| *binding_scope != scope);
        self.once_borrowed
            .retain(|(binding_scope, _)| *binding_scope != scope);
        self.scopes.pop();
        self.borrowed_shell_bindings.pop();
        self.explicit_reference_bindings.pop();
        self.frozen_bindings.pop();
        self.unique_value_bindings.pop();
        self.unique_storage_bindings.pop();
        self.inferred_owner_bindings.pop();
        self.unique_binding_types.pop();
        self.callable_unique_contracts.pop();
        self.callable_opt_provenance.pop();
        self.container_opt_provenance.pop();
    }
    fn is_explicit_reference_binding(&self, name: &str) -> bool {
        self.explicit_reference_bindings
            .iter()
            .rev()
            .any(|bindings| bindings.contains(name))
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

    fn ty_carries_must_consume(&self, ty: &Ty) -> bool {
        match self.resolve(ty) {
            // A view may be copyable, but it may never erase the payload's
            // consumption obligation. Formation rejects such a carrier below.
            Ty::View(payload) => self.ty_carries_must_consume(&payload),
            Ty::List(element) => self.ty_carries_must_consume(&element),
            Ty::Tuple(items) => items.iter().any(|item| self.ty_carries_must_consume(item)),
            Ty::Named(name, arguments) | Ty::Dyn(name, arguments) => {
                self.stored_obligations.must_types.contains(name.as_ref())
                    || self
                        .stored_obligations.stored_parameters
                        .get(name.as_ref())
                        .is_some_and(|positions| {
                            positions.iter().zip(arguments.iter()).any(|(stored, argument)| {
                                *stored && self.ty_carries_must_consume(argument)
                            })
                        })
            }
            // A callable's parameter/result contract does not mean the function
            // value owns either value. Captured obligations are rejected at the
            // closure-capture boundary instead.
            Ty::Fn(..) => false,
            _ => false,
        }
    }

    fn collect_storage_variables(&self, ty: &Ty, variables: &mut HashSet<u32>) {
        match self.resolve(ty) {
            Ty::Var(variable) => { variables.insert(variable); }
            Ty::View(payload) | Ty::List(payload) => self.collect_storage_variables(&payload, variables),
            Ty::Tuple(items) | Ty::Named(_, items) | Ty::Dyn(_, items) => {
                for item in items.iter() { self.collect_storage_variables(item, variables); }
            }
            Ty::Fn(params, result, _, _, _) => {
                for parameter in params.iter() { self.collect_storage_variables(parameter, variables); }
                self.collect_storage_variables(&result, variables);
            }
            _ => {}
        }
    }

    /// Admit a payload for a copyable rooted-view carrier. Generic variables
    /// belonging to the current declaration become retained call-site
    /// obligations; every other unresolved variable is rejected now. This
    /// prevents a generic body from manufacturing `&a` and later specializing
    /// `a` to authority, must-consume, or affine storage.
    fn admit_view_payload(&mut self, ty: &Ty, context: &str) -> Result<(), TypeError> {
        use witchy_canonical_source::storage::{SnapshotRefusal, StoragePlan, snapshot_eligibility};
        let resolved = self.resolve(ty);
        let mut variables = HashSet::new();
        self.collect_storage_variables(&resolved, &mut variables);
        let current: HashSet<u32> = self.current_typarams.values().copied().collect();
        let mut deferred = HashMap::new();
        for variable in variables {
            if current.contains(&variable) {
                deferred.insert(variable, Ty::Int);
            } else {
                return terr(format!(
                    "{context} forms an immutable view with unresolved payload type; authenticate a concrete payload or declare it as a function type parameter",
                ));
            }
        }
        if !deferred.is_empty()
            && !self.current_storage_obligations.contains(&StorageObligation::RootedView(resolved.clone()))
        {
            self.current_storage_obligations.push(StorageObligation::RootedView(resolved.clone()));
        }
        // A declared generic variable is proved again at specialization. Use
        // an inline scalar representative now so all other instantiated
        // fields are checked by the same source-storage judgment as SafeWIR.
        let representative = self.subst_vars(&resolved, &deferred);
        let source = ty_to_ast(&representative).ok_or_else(|| TypeError {
            message: format!("{context} cannot authenticate its immutable view payload"),
        })?;
        if let Err(reason) = snapshot_eligibility(&source, &self.canonical_definitions) {
            let explanation = match reason {
                SnapshotRefusal::Authority => "authority storage has no lawful retained snapshot",
                SnapshotRefusal::UniqueOwner => "affine unique storage has no lawful retained snapshot",
                SnapshotRefusal::MustConsume => "must-consume storage cannot be copied into a persistent view",
                SnapshotRefusal::OnceCallable => "affine once-callable storage cannot be copied into a persistent view",
                SnapshotRefusal::ResourceLimit => "view storage exceeds the recursive proof limit",
                SnapshotRefusal::UnprovedStorage => "view storage has no recursive checked carrier with a lawful snapshot proof",
            };
            return terr(format!("{context} cannot form an immutable view of `{resolved}`: {explanation}"));
        }
        let view = ast::Type::Qualified(ast::TypeQual::View, Box::new(source));
        if !matches!(self.storage_plan(&view),
            StoragePlan::RootedView(_))
        {
            return terr(format!("{context} cannot form an immutable view without an authenticated storage carrier"));
        }
        Ok(())
    }

    fn admit_replay_capture_source(
        &mut self,
        source: &ast::Type,
        inferred: &Ty,
        context: &str,
    ) -> Result<(), TypeError> {
        use witchy_canonical_source::storage::{SnapshotRefusal, replay_capture_eligibility};
        let initial = replay_capture_eligibility(source, &self.canonical_definitions);
        if initial.is_ok() { return Ok(()); }
        let resolved = self.resolve(inferred);
        let mut variables = HashSet::new();
        self.collect_storage_variables(&resolved, &mut variables);
        let current: HashSet<u32> = self.current_typarams.values().filter_map(|variable| {
            match self.resolve(&Ty::Var(*variable)) { Ty::Var(representative) => Some(representative), _ => None }
        }).collect();
        if matches!(initial, Err(SnapshotRefusal::UnprovedStorage))
            && !variables.is_empty()
            && variables.iter().all(|variable| current.contains(variable))
        {
            let substitutions = self.current_typarams.keys()
                .map(|name| (name.clone(), ast::Type::Named("Int".into(), Vec::new())))
                .collect();
            let representative = Self::substitute_unique_source_type(source, &substitutions);
            if replay_capture_eligibility(&representative, &self.canonical_definitions).is_ok() {
                let obligation = StorageObligation::ReplayCapture(resolved);
                if !self.current_storage_obligations.contains(&obligation) {
                    self.current_storage_obligations.push(obligation);
                }
                return Ok(());
            }
        }
        terr(format!("{context} has no copyable snapshot: {}", replay_snapshot_reason(initial.err().unwrap())))
    }

    fn admit_replay_capture_ty(&mut self, ty: &Ty, context: &str) -> Result<(), TypeError> {
        use witchy_canonical_source::storage::replay_capture_eligibility;
        let resolved = self.resolve(ty);
        let mut variables = HashSet::new();
        self.collect_storage_variables(&resolved, &mut variables);
        let current: HashSet<u32> = self.current_typarams.values().filter_map(|variable| {
            match self.resolve(&Ty::Var(*variable)) { Ty::Var(representative) => Some(representative), _ => None }
        }).collect();
        let mut representatives = HashMap::new();
        for variable in variables {
            if !current.contains(&variable) {
                return terr(format!("{context} has an unresolved replay capture type"));
            }
            representatives.insert(variable, Ty::Int);
        }
        let representative = self.subst_vars(&resolved, &representatives);
        let source = ty_to_ast(&representative).ok_or_else(|| TypeError {
            message: format!("{context} has no checked replay capture type"),
        })?;
        replay_capture_eligibility(&source, &self.canonical_definitions).map_err(|reason| TypeError {
            message: format!("{context} has no copyable snapshot: {}", replay_snapshot_reason(reason)),
        })?;
        if !representatives.is_empty() {
            let obligation = StorageObligation::ReplayCapture(resolved);
            if !self.current_storage_obligations.contains(&obligation) {
                self.current_storage_obligations.push(obligation);
            }
        }
        Ok(())
    }

    fn is_declared_type_parameter_representative(&self, variable: u32) -> bool {
        self.current_typarams.values().any(|parameter| {
            matches!(self.resolve(&Ty::Var(*parameter)), Ty::Var(representative)
                if representative == variable)
        })
    }

    /// Defer a generic `.owned()` storage proof only when every unresolved
    /// variable is one of the function's declared type parameters. Such
    /// templates have no executable fallback and their concrete bodies are
    /// checked again after specialization.
    fn has_only_declared_deferred_parameters(&self, ty: &Ty) -> bool {
        let mut pending = vec![(ty.clone(), 0usize)];
        let mut found_parameter = false;
        let mut visited = 0usize;
        while let Some((ty, depth)) = pending.pop() {
            visited += 1;
            if visited > 4_096 || depth > 128 { return false; }
            let child_depth = depth + 1;
            match self.resolve(&ty) {
                Ty::Var(variable) => {
                    if !self.is_declared_type_parameter_representative(variable) {
                        return false;
                    }
                    found_parameter = true;
                }
                Ty::View(inner) | Ty::List(inner) => pending.push((inner.as_ref().clone(), child_depth)),
                Ty::Tuple(items) | Ty::Named(_, items) | Ty::Dyn(_, items) => {
                    pending.extend(items.iter().cloned().map(|item| (item, child_depth)));
                }
                Ty::Fn(parameters, result, _, _, _) => {
                    pending.extend(parameters.iter().cloned().map(|item| (item, child_depth)));
                    pending.push((result.as_ref().clone(), child_depth));
                }
                _ => {}
            }
        }
        found_parameter
    }

    /// RFC-0167 §5 `.owned()`: the result type of explicitly materializing a
    /// value. A view `&T` materializes its referent `T`; an owned value is
    /// its own materialization. Only lawfully copyable payloads qualify: no
    /// authority, no `must` obligation, no once-callable, and no nested view
    /// (an aggregate holding a view is not deeply owned). An admitted slice
    /// explicitly materializes its projection as a fresh `List(T)`.
    fn owned_materialization_ty(&mut self, receiver: &Ty) -> Result<Ty, TypeError> {
        use witchy_canonical_source::rooted_view::RootedViewStorage;
        use witchy_canonical_source::storage::StoragePlan;

        let payload = match self.resolve(receiver) {
            Ty::View(payload) => self.resolve(&payload),
            // A `dyn` receiver resolves `.owned()` only through its trait
            // surface (an existential call), never as a copy.
            dyn_ty @ Ty::Dyn(_, _) => {
                return terr(format!(
                    "cannot resolve `.owned()` on `{dyn_ty}` — existential calls are limited to the trait's declared method surface"
                ));
            }
            owned => owned,
        };
        // Lowering picks `*view` or identity from the receiver's checked
        // type, so an inference variable (not a declared type parameter)
        // must be pinned first rather than judged copyable by default.
        if let Ty::Var(variable) = payload
            && !self.is_declared_type_parameter_representative(variable)
        {
            return terr(
                "`.owned()` needs the receiver's type to be known here; annotate the receiver",
            );
        }
        if let Ty::Named(name, arguments) = &payload
            && name.as_ref() == "Slice"
            && arguments.len() == 1
        {
            let element = self.resolve(&arguments[0]);
            if ty_to_ast(&payload).is_some_and(|ty| {
                let view = ast::Type::Qualified(ast::TypeQual::View, Box::new(ty));
                matches!(
                    self.storage_plan(&view),
                    StoragePlan::RootedView(RootedViewStorage::RetainedProjection(element))
                        if element.within_resource_bounds()
                )
            })
                || matches!(&element, Ty::Var(variable)
                    if self.is_declared_type_parameter_representative(*variable))
            {
                return Ok(Ty::List(Rc::new(element)));
            }
            return terr(format!(
                "`.owned()` cannot materialize the slice view `{payload}`: its element lacks \
                 a recursive checked carrier with retain, clone, and release into List(T)"
            ));
        }
        if let Some(capability) = self.ty_authority_taint(&payload, &mut HashSet::new()) {
            return terr(format!(
                "`.owned()` cannot copy authority `{capability}`; a capability has no lawful copy operation"
            ));
        }
        if self.ty_carries_must_consume(&payload) {
            return terr(format!(
                "`.owned()` cannot copy must-consume `{payload}`; a copy would duplicate its obligation"
            ));
        }
        if self.ty_carries_once_callable(&payload) {
            return terr(format!(
                "`.owned()` cannot copy once-callable `{payload}`; it may be invoked at most once"
            ));
        }
        if self.ty_carries_rooted_view(&payload) {
            return terr(format!(
                "`.owned()` cannot materialize `{payload}`: it contains an immutable view, and \
                 `.owned()` does not make nested views deeply owned; map the aggregate explicitly"
            ));
        }
        // A declared type parameter is checked again after specialization.
        // All concrete copies need an owned source-storage proof; in particular,
        // an unknown nominal or unsupported callable must not inherit one from
        // the absence of the hazards above.
        if !self.has_only_declared_deferred_parameters(&payload)
            && !ty_to_ast(&payload).as_ref().is_some_and(|ty| {
                self.storage_plan(ty)
                    .admits_owned_copy()
            })
        {
            return terr(format!(
                "`.owned()` cannot materialize `{payload}`: its owned storage has no recursive checked copy proof"
            ));
        }
        Ok(payload)
    }

    fn ty_carries_once_callable(&self, ty: &Ty) -> bool {
        fn go(checker: &Checker, ty: &Ty, seen: &mut HashSet<String>) -> bool {
            match checker.resolve(ty) {
                Ty::Fn(_, _, _, _, qualifiers) => qualifiers.once,
                Ty::View(payload) => go(checker, &payload, seen),
                Ty::List(item) => go(checker, &item, seen),
                Ty::Tuple(items) => items.iter().any(|item| go(checker, item, seen)),
                // An existential's type arguments describe its method surface;
                // they are not stored payload fields. The concrete payload is
                // checked at the packing boundary instead.
                Ty::Dyn(..) => false,
                Ty::Named(name, arguments) => {
                    let parameter_hit = checker
                        .stored_obligations.stored_parameters
                        .get(name.as_ref())
                        .map(|positions| {
                            positions.iter().zip(arguments.iter()).any(|(stored, argument)| {
                                *stored && go(checker, argument, seen)
                            })
                        })
                        // An unavailable declaration cannot prove a phantom
                        // position. Treat every argument as stored until the
                        // source contract is present.
                        .unwrap_or_else(|| arguments.iter().any(|argument| go(checker, argument, seen)));
                    if parameter_hit {
                        return true;
                    }
                    // Owning arguments are checked before this name-only guard:
                    // that detects `Grow(Grow(once fn()))` without following an
                    // argument-growing recursive field forever.
                    if !seen.insert(name.to_string()) {
                        return false;
                    }
                    let field_hit = checker.record_fields.get(name.as_ref()).is_some_and(|(params, fields)| {
                        let bindings: HashMap<u32, Ty> = params
                            .iter()
                            .copied()
                            .zip(arguments.iter().cloned())
                            .collect();
                        fields.iter().any(|(_, field)| {
                            go(checker, &checker.subst_vars(field, &bindings), seen)
                        })
                    });
                    let variant_hit = checker.adt_variants.get(name.as_ref()).is_some_and(|variants| {
                        variants.iter().any(|variant| {
                            checker.ctor_sigs.get(variant).is_some_and(|(payloads, result)| {
                                let result_arguments = match checker.resolve(result) {
                                    Ty::Named(_, result_arguments) => result_arguments,
                                    _ => Vec::new().into(),
                                };
                                let bindings = result_arguments
                                    .iter()
                                    .zip(arguments.iter())
                                    .filter_map(|(parameter, argument)| match parameter {
                                        Ty::Var(id) => Some((*id, argument.clone())),
                                        _ => None,
                                    })
                                    .collect::<HashMap<_, _>>();
                                payloads.iter().any(|payload| {
                                    go(checker, &checker.subst_vars(payload, &bindings), seen)
                                })
                            })
                        })
                    });
                    seen.remove(name.as_ref());
                    field_hit || variant_hit
                }
                _ => false,
            }
        }
        // A variable-free type is a fixed input: its answer cannot change as
        // the substitution grows, so it is computed once per checker.
        let resolved = self.resolve(ty);
        if ty_has_var(&resolved) {
            return go(self, &resolved, &mut HashSet::new());
        }
        if let Some(answer) = self.once_callable_types.borrow().get(&resolved) {
            return *answer;
        }
        let answer = go(self, &resolved, &mut HashSet::new());
        self.once_callable_types.borrow_mut().insert(resolved, answer);
        answer
    }

    fn mark_borrowed_once_binding(&mut self, name: &str) {
        if let Some(binding) = self.must_binding_key(name) {
            self.once_borrowed.insert(binding);
        }
    }

    fn is_borrowed_once_binding(&self, name: &str) -> bool {
        self.must_binding_key(name)
            .is_some_and(|binding| self.once_borrowed.contains(&binding))
    }

    fn reject_explicit_once_reference_type(
        &mut self,
        declared: &ast::Type,
        context: &str,
    ) -> Result<(), TypeError> {
        fn contains(checker: &mut Checker, declared: &ast::Type) -> bool {
            match declared {
                ast::Type::Qualified(
                    ast::TypeQual::View
                        | ast::TypeQual::Borrow(_)
                        | ast::TypeQual::LegacyBorrow(_)
                        | ast::TypeQual::BorrowMut(_),
                    referent,
                ) => {
                    let referent_ty = checker.to_ty(referent);
                    checker.ty_carries_once_callable(&referent_ty)
                        || contains(checker, referent)
                }
                ast::Type::Qualified(_, inner) | ast::Type::Slice(inner) => {
                    contains(checker, inner)
                }
                ast::Type::Named(name, arguments) => {
                    let positions = checker.stored_obligations.stored_parameters.get(name).cloned();
                    match positions {
                        Some(positions) => positions
                            .iter()
                            .zip(arguments)
                            .any(|(stored, argument)| *stored && contains(checker, argument)),
                        None => arguments
                            .iter()
                            .any(|argument| contains(checker, argument)),
                    }
                }
                ast::Type::Tuple(arguments) => {
                    arguments.iter().any(|argument| contains(checker, argument))
                }
                // Existential arguments describe the method surface rather than
                // fields stored by the value. Concrete method declarations are
                // validated as function signatures after trait lowering.
                ast::Type::Dyn(..) => false,
                ast::Type::Fn(parameters, result, _, _) => {
                    parameters.iter().any(|parameter| contains(checker, parameter))
                        || contains(checker, result)
                }
                ast::Type::RecordCompose { base, fields } => {
                    contains(checker, base)
                        || fields.iter().any(|(_, field)| contains(checker, field))
                }
            }
        }

        if contains(self, declared) {
            return terr(format!(
                "{context} contains an explicit reference to affine once-callable storage; shared and exclusive reference handles are copyable but the current loan facts cannot consume their referent, so pass ownership or use an explicit `let` parameter that does not invoke it"
            ));
        }
        Ok(())
    }

    fn affine_place(&self, expression: &Expr) -> Option<(String, bool)> {
        fn go(expression: &Expr, projected: bool) -> Option<(String, bool)> {
            match expression {
                Expr::Var(root) => Some((root.clone(), projected)),
                // A type ascription does not manufacture a new value and is
                // transparent even when nested below a field/index projection.
                Expr::As { expr, .. } => go(expr, projected),
                Expr::Unary { op: UnOp::Deref, expr } => go(expr, projected),
                Expr::Field { base, .. } | Expr::Index { base, .. } => go(base, true),
                Expr::Call { name, args }
                    if matches!(
                        name.as_str(),
                        intrinsics::LIST_AT | intrinsics::DICT_AT
                    ) && args.len() == 2 =>
                {
                    go(&args[0], true)
                }
                _ => None,
            }
        }
        go(expression, false)
    }

    fn affine_place_root(&self, expression: &Expr) -> Option<String> {
        self.affine_place(expression).map(|(root, _)| root)
    }

    fn reject_affine_loop_backedge(
        &self,
        before: &HashSet<(usize, String)>,
        context: &str,
    ) -> Result<(), TypeError> {
        for binding @ (scope, name) in self.consumed.difference(before) {
            let Some((ty, _)) = self.scopes.get(*scope).and_then(|frame| frame.get(name)) else {
                continue;
            };
            if self.ty_carries_once_callable(ty) {
                let action = if self.once_invoked.contains(binding) {
                    "invoked"
                } else {
                    "transferred"
                };
                return terr(format!(
                    "once-callable `{name}` is {action} in {context}; a loop backedge could repeat that affine consumption"
                ));
            }
            if self.ty_carries_must_consume(ty) {
                return terr(format!(
                    "must-consume value `{name}` is transferred in {context}; a loop backedge could repeat that affine consumption"
                ));
            }
        }
        Ok(())
    }

    fn reject_once_match_guard_consumption(
        &self,
        before: &HashSet<(usize, String)>,
    ) -> Result<(), TypeError> {
        for (scope, name) in self.consumed.difference(before) {
            let Some((ty, _)) = self.scopes.get(*scope).and_then(|frame| frame.get(name)) else {
                continue;
            };
            if self.ty_carries_once_callable(ty) {
                return terr(format!(
                    "match guard cannot consume affine value `{name}` because a false guard falls through to later arms"
                ));
            }
        }
        Ok(())
    }

    fn reject_implicit_once_copy(
        &self,
        expression: &Expr,
        ty: &Ty,
        context: &str,
    ) -> Result<(), TypeError> {
        if !self.ty_carries_once_callable(ty)
            || matches!(expression, Expr::Unary { op: UnOp::Move, .. })
        {
            return Ok(());
        }
        let Some(root) = self.affine_place_root(expression) else {
            return Ok(());
        };
        if self.lookup(&root).is_some() {
            return terr(format!(
                "once-callable `{root}` would be copied by {context}; pass it to an `own` parameter or use `move {root}`"
            ));
        }
        Ok(())
    }

    fn consume_once_callee(&mut self, expression: &Expr, ty: &Ty) -> Result<(), TypeError> {
        let Ty::Fn(_, _, _, _, qualifiers) = self.resolve(ty) else { return Ok(()) };
        if !qualifiers.once {
            return Ok(());
        }
        let Some(root) = self.affine_place_root(expression) else {
            return Ok(());
        };
        if self.is_explicit_reference_binding(&root) {
            return terr(format!(
                "cannot invoke once-callable through explicit reference `{root}`; the reference handle does not carry referent-consumption state"
            ));
        }
        if self.is_borrowed_once_binding(&root) {
            return terr(format!(
                "borrowed once-callable `{root}` cannot be invoked because invocation consumes the caller-owned value; take it by value or `own`"
            ));
        }
        if self.is_consumed_binding(&root) {
            let action = if self
                .must_binding_key(&root)
                .is_some_and(|binding| self.once_invoked.contains(&binding))
            {
                "consumed by invocation"
            } else {
                "moved or transferred"
            };
            return terr(format!("use of once-callable `{root}` after it was {action}"));
        }
        self.mark_consumed_binding(&root);
        if let Some(binding) = self.must_binding_key(&root) {
            self.once_invoked.insert(binding);
        }
        Ok(())
    }

    fn transfer_once_match_scrutinee(
        &mut self,
        expression: &Expr,
        ty: &Ty,
    ) -> Result<(), TypeError> {
        if !self.ty_carries_once_callable(ty)
            || matches!(expression, Expr::Unary { op: UnOp::Move, .. })
        {
            return Ok(());
        }
        let Some(root) = self.affine_place_root(expression) else {
            return Ok(());
        };
        if self.is_borrowed_once_binding(&root) {
            return terr(format!(
                "cannot destructure borrowed affine value `{root}`; matching transfers its once-callable payloads"
            ));
        }
        self.mark_consumed_binding(&root);
        Ok(())
    }

    fn enforce_once_argument(
        &mut self,
        expression: &Expr,
        parameter: &Ty,
        convention: Convention,
        context: &str,
    ) -> Result<(), TypeError> {
        if !self.ty_carries_once_callable(parameter) {
            return Ok(());
        }
        match convention {
            Convention::Var => terr(format!(
                "{context} cannot use `var` with a once-callable: invocation could leave an empty write-back slot; use `own` or reinitialize explicitly before a later transfer"
            )),
            Convention::Borrow => Ok(()),
            Convention::Let => self.reject_implicit_once_copy(expression, parameter, context),
            Convention::Own => {
                if let Some(root) = self.affine_place_root(expression) {
                    if self.is_borrowed_once_binding(&root) {
                        return terr(format!(
                            "{context} cannot transfer borrowed once-callable `{root}` to an `own` parameter"
                        ));
                    }
                    if self.is_consumed_binding(&root) {
                        let reason = if self
                            .must_binding_key(&root)
                            .is_some_and(|binding| self.once_invoked.contains(&binding))
                        {
                            "after it was consumed by invocation"
                        } else {
                            "after it was already transferred"
                        };
                        return terr(format!("use of once-callable `{root}` {reason}"));
                    }
                    self.mark_consumed_binding(&root);
                }
                Ok(())
            }
        }
    }

    fn mark_must_consume_binding(&mut self, name: &str, ty: &Ty) {
        if !self.ty_carries_must_consume(ty) {
            return;
        }
        self.must_live
            .insert((self.scopes.len().saturating_sub(1), name.to_string()));
    }

    fn must_binding_key(&self, name: &str) -> Option<(usize, String)> {
        self.scopes
            .iter()
            .rposition(|scope| scope.contains_key(name))
            .map(|scope| (scope, name.to_string()))
    }

    fn is_consumed_binding(&self, name: &str) -> bool {
        self.must_binding_key(name)
            .is_some_and(|binding| self.consumed.contains(&binding))
    }

    fn mark_consumed_binding(&mut self, name: &str) {
        if let Some(binding) = self.must_binding_key(name) {
            self.consumed.insert(binding);
        }
    }

    /// `own` transfers the source binding in every source mode. Copy repair may
    /// establish storage uniqueness, but cannot undo this source-level move.
    fn transfer_owned_argument_binding(&mut self, name: &str) {
        self.mark_consumed_binding(name);
        self.consume_must_binding(name);
    }

    fn reject_consumed_own_argument(
        &self,
        argument: &Expr,
        convention: Convention,
    ) -> Result<(), TypeError> {
        if convention == Convention::Own
            && let Expr::Var(name) = argument
            && self.is_consumed_binding(name)
        {
            return terr(format!(
                "use of `{name}` after it was already transferred (moved by an `own` parameter)"
            ));
        }
        Ok(())
    }

    fn reinitialize_binding(&mut self, name: &str) {
        if let Some(binding) = self.must_binding_key(name) {
            self.consumed.remove(&binding);
            self.once_invoked.remove(&binding);
        }
    }

    fn is_live_must_binding(&self, name: &str) -> bool {
        self.must_binding_key(name)
            .is_some_and(|binding| self.must_live.contains(&binding))
    }

    fn is_borrowed_must_binding(&self, name: &str) -> bool {
        self.must_binding_key(name)
            .is_some_and(|binding| self.must_borrowed.contains(&binding))
    }

    fn is_must_binding(&self, name: &str) -> bool {
        self.is_live_must_binding(name) || self.is_borrowed_must_binding(name)
    }

    fn mark_borrowed_must_binding(&mut self, name: &str) {
        if let Some(binding) = self.must_binding_key(name) {
            self.must_borrowed.insert(binding);
        }
    }

    fn reject_own_of_borrowed_must(&self, name: &str, context: &str) -> Result<(), TypeError> {
        if self.is_borrowed_must_binding(name) {
            return terr(format!(
                "{context} cannot consume borrowed must-consume value `{name}`; only its caller-owned binding may cross an `own` boundary"
            ));
        }
        Ok(())
    }

    fn consume_must_binding(&mut self, name: &str) {
        if let Some(binding) = self.must_binding_key(name) {
            self.must_live.remove(&binding);
        }
    }

    fn reject_implicit_must_copy(&self, expression: &Expr, context: &str) -> Result<(), TypeError> {
        if let Expr::Var(name) = expression
            && self.is_must_binding(name)
        {
            return terr(format!(
                "{context} would copy must-consume value `{name}`; transfer it with `move {name}` or pass it to an `own` parameter"
            ));
        }
        Ok(())
    }

    fn reject_must_borrowed_temporary(
        &self,
        expression: &Expr,
        parameter: &Ty,
        context: &str,
    ) -> Result<(), TypeError> {
        if !self.ty_carries_must_consume(parameter) {
            return Ok(());
        }
        // Every ordinary owned or borrowed local is tracked separately. An
        // untracked variable here is an `own` operation parameter: the call
        // boundary already discharged its caller's obligation, and the
        // operation implementation may forward the value into generic storage.
        if matches!(expression, Expr::Var(_)) {
            return Ok(());
        }
        terr(format!(
            "{context} borrows a temporary must-consume value whose obligation would be lost after the call; bind it, borrow the binding, then consume or return it"
        ))
    }

    fn expression_introduces_must_obligation(&self, expression: &Expr, ty: &Ty) -> bool {
        self.ty_carries_must_consume(ty)
            && !matches!(expression, Expr::Var(name) if !self.is_live_must_binding(name))
    }

    fn reject_live_must_in_current_scope(&self) -> Result<(), TypeError> {
        let scope = self.scopes.len().saturating_sub(1);
        if let Some((_, name)) = self
            .must_live
            .iter()
            .find(|(binding_scope, _)| *binding_scope == scope)
        {
            return terr(format!(
                "must-consume value `{name}` reaches the end of its scope without being consumed; pass it to an `own` parameter or return it"
            ));
        }
        Ok(())
    }

    fn reject_all_live_must_before_return(&self) -> Result<(), TypeError> {
        if let Some((_, name)) = self.must_live.iter().next() {
            return terr(format!(
                "return leaves must-consume value `{name}` undisposed; consume it on this path or return that value"
            ));
        }
        Ok(())
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
        let storage_obligations = self
            .fn_storage_obligations
            .get(name)
            .cloned()
            .unwrap_or_default();
        Some(self.instantiate_with_bounds(
            &params,
            &ret,
            &typarams,
            &bounds,
            &storage_obligations,
        ))
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
                    vec![Ty::List(Box::new(elem.clone()).into()), elem.clone()],
                    Ty::List(Box::new(elem).into()),
                ))
            }
            S::CompilerQuoteItem => Some((
                vec![Ty::String, Ty::String],
                Ty::Named("meta.ItemSyntax".into(), Vec::new().into()),
            )),
            S::CompilerQuoteItemHoles => Some((
                vec![
                    Ty::String,
                    Ty::List(Box::new(Ty::String).into()),
                    Ty::List(Box::new(Ty::Named("meta.SyntaxHole".into(), Vec::new().into())).into()),
                ],
                Ty::Named("meta.ItemSyntax".into(), Vec::new().into()),
            )),
            S::CompilerQuoteExpr => Some((
                vec![Ty::String, Ty::String],
                Ty::Named("meta.ExprSyntax".into(), Vec::new().into()),
            )),
            S::CompilerQuoteExprHoles => Some((
                vec![
                    Ty::String,
                    Ty::List(Box::new(Ty::String).into()),
                    Ty::List(Box::new(Ty::Named("meta.SyntaxHole".into(), Vec::new().into())).into()),
                ],
                Ty::Named("meta.ExprSyntax".into(), Vec::new().into()),
            )),
            S::CompilerQuoteType => Some((
                vec![Ty::String, Ty::String],
                Ty::Named("meta.TypeSyntax".into(), Vec::new().into()),
            )),
            S::CompilerQuoteTypeHoles => Some((
                vec![
                    Ty::String,
                    Ty::List(Box::new(Ty::String).into()),
                    Ty::List(Box::new(Ty::Named("meta.TypeSyntax".into(), Vec::new().into())).into()),
                ],
                Ty::Named("meta.TypeSyntax".into(), Vec::new().into()),
            )),
            S::CompilerQuotePattern => Some((
                vec![Ty::String, Ty::String],
                Ty::Named("meta.PatternSyntax".into(), Vec::new().into()),
            )),
            S::CompilerQuotePatternHoles => Some((
                vec![
                    Ty::String,
                    Ty::List(Box::new(Ty::String).into()),
                    Ty::List(Box::new(Ty::Named("meta.PatternSyntax".into(), Vec::new().into())).into()),
                ],
                Ty::Named("meta.PatternSyntax".into(), Vec::new().into()),
            )),
            S::CompilerQuoteStmt => Some((
                vec![Ty::String, Ty::String],
                Ty::Named("meta.StmtSyntax".into(), Vec::new().into()),
            )),
            S::CompilerQuoteStmtHoles => Some((
                vec![
                    Ty::String,
                    Ty::List(Box::new(Ty::String).into()),
                    Ty::List(Box::new(Ty::Named("meta.SyntaxHole".into(), Vec::new().into())).into()),
                ],
                Ty::Named("meta.StmtSyntax".into(), Vec::new().into()),
            )),
            S::CompilerQuoteBlock => Some((
                vec![Ty::String, Ty::String],
                Ty::Named("meta.BlockSyntax".into(), Vec::new().into()),
            )),
            S::CompilerQuoteBlockHoles => Some((
                vec![
                    Ty::String,
                    Ty::List(Box::new(Ty::String).into()),
                    Ty::List(Box::new(Ty::Named("meta.SyntaxHole".into(), Vec::new().into())).into()),
                ],
                Ty::Named("meta.BlockSyntax".into(), Vec::new().into()),
            )),
            S::CompilerEmitItem => Some((
                vec![Ty::Named("meta.ItemSyntax".into(), Vec::new().into())],
                Ty::Unit,
            )),
            S::CompilerEmitExpr => Some((
                vec![Ty::Named("meta.ExprSyntax".into(), Vec::new().into())],
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
                Ty::Named("dynamic.RuntimeType".into(), Vec::new().into()),
            )),
            S::GenericToRuntimeType => {
                let value = self.fresh();
                Some((
                    vec![value],
                    Ty::Named("dynamic.RuntimeType".into(), Vec::new().into()),
                ))
            }
            S::GenericToInt => {
                let value = self.fresh();
                Some((vec![value], Ty::Int))
            }
            S::RuntimeTypeToListRuntimeField => Some((
                vec![Ty::Named("dynamic.RuntimeType".into(), Vec::new().into())],
                Ty::List(Box::new(Ty::Named(
                    "dynamic.RuntimeField".into(),
                    Vec::new().into(),
                )).into()),
            )),
            S::RuntimeTypeStringToDynamicFieldStatus => Some((
                vec![
                    Ty::Named("dynamic.RuntimeType".into(), Vec::new().into()),
                    Ty::String,
                ],
                Ty::Named("dynamic.DynamicFieldStatus".into(), Vec::new().into()),
            )),
            S::RuntimeTypeToListRuntimeMethod => Some((
                vec![Ty::Named("dynamic.RuntimeType".into(), Vec::new().into())],
                Ty::List(Box::new(Ty::Named(
                    "dynamic.RuntimeMethod".into(),
                    Vec::new().into(),
                )).into()),
            )),
            S::DynamicStringListDynamicToResultDynamicDynamicError => {
                let dynamic = Ty::Named("dynamic.Dynamic".into(), Vec::new().into());
                Some((
                    vec![dynamic.clone(), Ty::String, Ty::List(Box::new(dynamic.clone()).into())],
                    Ty::Named(
                        "Result".into(),
                        vec![
                            dynamic,
                            Ty::Named("dynamic.DynamicError".into(), Vec::new().into()),
                        ].into(),
                    ),
                ))
            }
            S::DynamicStringListDynamicGenericToResultDynamicDynamicError => {
                let dynamic = Ty::Named("dynamic.Dynamic".into(), Vec::new().into());
                let capabilities = self.fresh();
                Some((
                    vec![
                        dynamic.clone(),
                        Ty::String,
                        Ty::List(Box::new(dynamic.clone()).into()),
                        capabilities,
                    ],
                    Ty::Named(
                        "Result".into(),
                        vec![
                            dynamic,
                            Ty::Named("dynamic.DynamicError".into(), Vec::new().into()),
                        ].into(),
                    ),
                ))
            }
            S::DynamicRuntimeTypeToBool => Some((
                vec![
                    Ty::Named("dynamic.Dynamic".into(), Vec::new().into()),
                    Ty::Named("dynamic.RuntimeType".into(), Vec::new().into()),
                ],
                Ty::Bool,
            )),
            S::DynamicRuntimeTypeToResultDynamicDynamicError => {
                let dynamic = Ty::Named("dynamic.Dynamic".into(), Vec::new().into());
                Some((
                    vec![
                        dynamic.clone(),
                        Ty::Named("dynamic.RuntimeType".into(), Vec::new().into()),
                    ],
                    Ty::Named(
                        "Result".into(),
                        vec![
                            dynamic,
                            Ty::Named("dynamic.DynamicError".into(), Vec::new().into()),
                        ].into(),
                    ),
                ))
            }
            S::DynamicToOptionGeneric => {
                let value = self.fresh();
                Some((
                    vec![Ty::Named("dynamic.Dynamic".into(), Vec::new().into())],
                    Ty::Named("Option".into(), vec![value].into()),
                ))
            }
            S::DynamicIntToOptionGeneric => {
                let value = self.fresh();
                Some((
                    vec![Ty::Named("dynamic.Dynamic".into(), Vec::new().into()), Ty::Int],
                    Ty::Named("Option".into(), vec![value].into()),
                ))
            }
            S::StringToBytes => Some((vec![Ty::String], Ty::Bytes)),
            S::ListIntToBytes => Some((vec![Ty::List(Box::new(Ty::Int).into())], Ty::Bytes)),
            S::BytesToString => Some((vec![Ty::Bytes], Ty::String)),
            S::BytesToInt => Some((vec![Ty::Bytes], Ty::Int)),
            S::BytesIntToInt => Some((vec![Ty::Bytes, Ty::Int], Ty::Int)),
            S::BytesBytesToBytes => Some((vec![Ty::Bytes, Ty::Bytes], Ty::Bytes)),
            S::BytesIntIntToBytes => {
                Some((vec![Ty::Bytes, Ty::Int, Ty::Int], Ty::Bytes))
            }
            S::BytesIntToBytes => Some((vec![Ty::Bytes, Ty::Int], Ty::Bytes)),
            S::IntToBytes => Some((vec![Ty::Int], Ty::Bytes)),
            S::IntIntToBytes => Some((vec![Ty::Int, Ty::Int], Ty::Bytes)),
            S::SecretStoreStringToOptionSecret => Some((
                vec![Ty::Named("SecretStore".into(), Vec::new().into()), Ty::String],
                Ty::Named("Option".into(), vec![Ty::Secret(SecretRights::full())].into()),
            )),
            S::SecretStoreStringToSecret => Some((
                vec![Ty::Named("SecretStore".into(), Vec::new().into()), Ty::String],
                Ty::Secret(SecretRights::full()),
            )),
            S::StringToString => Some((vec![Ty::String], Ty::String)),
            S::StringStringToString => Some((vec![Ty::String, Ty::String], Ty::String)),
            S::StringToInt => Some((vec![Ty::String], Ty::Int)),
            S::StringStringToInt => Some((vec![Ty::String, Ty::String], Ty::Int)),
            S::StringStringToBool => Some((vec![Ty::String, Ty::String], Ty::Bool)),
            S::StringToListString => {
                Some((vec![Ty::String], Ty::List(Box::new(Ty::String).into())))
            }
            S::StringStringToListString => Some((
                vec![Ty::String, Ty::String],
                Ty::List(Box::new(Ty::String).into()),
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
            S::StringToStr => {
                let str_ty = Ty::Named("str".into(), Vec::new().into());
                Some((
                    vec![Ty::View(Box::new(Ty::String).into())],
                    Ty::View(Box::new(str_ty).into()),
                ))
            }
            S::StrIntIntToStr => {
                let str_ty = Ty::Named("str".into(), Vec::new().into());
                let str_view = Ty::View(Box::new(str_ty).into());
                Some((vec![str_view.clone(), Ty::Int, Ty::Int], str_view))
            }
            S::StrToString => {
                let str_ty = Ty::Named("str".into(), Vec::new().into());
                Some((vec![Ty::View(Box::new(str_ty).into())], Ty::String))
            }
            S::StrToInt => {
                let str_ty = Ty::Named("str".into(), Vec::new().into());
                Some((vec![Ty::View(Box::new(str_ty).into())], Ty::Int))
            }
            S::ListStringListStringToString => Some((
                vec![
                    Ty::List(Box::new(Ty::String).into()),
                    Ty::List(Box::new(Ty::String).into()),
                ],
                Ty::String,
            )),
            // (RFC-0121) By-handle ops ask only for `Seal`. `coerce_arg` lets a bare
            // `Secret` stand in, so a sealed handle and a full one both work.
            S::SealedSecretStringToString => {
                Some((vec![Ty::Secret(SecretRights::sealed()), Ty::String], Ty::String))
            }
            S::SealedSecretToString => {
                Some((vec![Ty::Secret(SecretRights::sealed())], Ty::String))
            }
            // `reveal` asks for the full set, so a `Secret[Seal]` argument fails to
            // coerce and the diagnostic names the rights mismatch.
            S::RevealSecretToString => {
                Some((vec![Ty::Secret(SecretRights::full())], Ty::String))
            }
            S::IntToString => Some((vec![Ty::Int], Ty::String)),
            S::IntToFloat => Some((vec![Ty::Int], Ty::Float)),
            S::FloatToInt => Some((vec![Ty::Float], Ty::Int)),
            S::FloatToFloat => Some((vec![Ty::Float], Ty::Float)),
            S::GenericListToInt => {
                let elem = self.fresh();
                Some((vec![Ty::List(Box::new(elem).into())], Ty::Int))
            }
            S::GenericListIndex => {
                let elem = self.fresh();
                Some((vec![Ty::List(Box::new(elem.clone()).into()), Ty::Int], elem))
            }
            S::GenericListSetAt => {
                let elem = self.fresh();
                let list = Ty::List(Box::new(elem.clone()).into());
                Some((vec![list.clone(), Ty::Int, elem], list))
            }
            S::GenericListConcat => {
                let elem = self.fresh();
                let list = Ty::List(Box::new(elem).into());
                Some((vec![list.clone(), list.clone()], list))
            }
            S::GenericListPopExtract => {
                let elem = self.fresh();
                Some((
                    vec![Ty::List(Box::new(elem.clone()).into())],
                    Ty::Named("Option".into(), vec![elem].into()),
                ))
            }
            S::GenericListWithCapacity => {
                let elem = self.fresh();
                let list = Ty::List(Box::new(elem).into());
                Some((vec![Ty::Int], list))
            }
            S::GenericDictNew => {
                let key = self.fresh();
                let value = self.fresh();
                Some((vec![], Ty::Named("Dict".into(), vec![key, value].into())))
            }
            S::GenericDictWithCapacity => {
                let key = self.fresh();
                let value = self.fresh();
                Some((vec![Ty::Int], Ty::Named("Dict".into(), vec![key, value].into())))
            }
            S::GenericDictInsert => {
                let key = self.fresh();
                let value = self.fresh();
                let dict = Ty::Named("Dict".into(), vec![key.clone(), value.clone()].into());
                Some((vec![dict.clone(), key, value], dict))
            }
            S::GenericDictInsertExtract => {
                let key = self.fresh();
                let value = self.fresh();
                let dict = Ty::Named("Dict".into(), vec![key.clone(), value.clone()].into());
                Some((
                    vec![dict, key, value.clone()],
                    Ty::Named("Option".into(), vec![value].into()),
                ))
            }
            S::GenericDictGetOr => {
                let key = self.fresh();
                let value = self.fresh();
                let dict = Ty::Named("Dict".into(), vec![key.clone(), value.clone()].into());
                Some((vec![dict, key, value.clone()], value))
            }
            S::GenericDictIndex => {
                let key = self.fresh();
                let value = self.fresh();
                let dict = Ty::Named("Dict".into(), vec![key.clone(), value.clone()].into());
                Some((vec![dict, key], value))
            }
            S::GenericDictUpdate => {
                let key = self.fresh();
                let value = self.fresh();
                let dict = Ty::Named("Dict".into(), vec![key.clone(), value.clone()].into());
                let update = Ty::Fn(
                    vec![value.clone()].into(),
                    Box::new(value.clone()).into(),
                    vec![Convention::Let].into(),
                    vec![false].into(),
                    CallableQualifiers::ORDINARY,
                );
                Some((vec![dict.clone(), key, value, update], dict))
            }
            S::GenericDictContainsKey => {
                let key = self.fresh();
                let value = self.fresh();
                let dict = Ty::Named("Dict".into(), vec![key.clone(), value].into());
                Some((vec![dict, key], Ty::Bool))
            }
            S::GenericDictRemove => {
                let key = self.fresh();
                let value = self.fresh();
                let dict = Ty::Named("Dict".into(), vec![key.clone(), value].into());
                Some((vec![dict.clone(), key], dict))
            }
            S::GenericDictRemoveExtract => {
                let key = self.fresh();
                let value = self.fresh();
                let dict = Ty::Named("Dict".into(), vec![key.clone(), value.clone()].into());
                Some((
                    vec![dict, key],
                    Ty::Named("Option".into(), vec![value].into()),
                ))
            }
            S::GenericDictKeys => {
                let key = self.fresh();
                let value = self.fresh();
                let dict = Ty::Named("Dict".into(), vec![key.clone(), value].into());
                Some((vec![dict], Ty::List(Box::new(key).into())))
            }
            S::GenericDictValues => {
                let key = self.fresh();
                let value = self.fresh();
                let dict = Ty::Named("Dict".into(), vec![key, value.clone()].into());
                Some((vec![dict], Ty::List(Box::new(value).into())))
            }
            S::GenericDictPairs => {
                let key = self.fresh();
                let value = self.fresh();
                let dict = Ty::Named("Dict".into(), vec![key.clone(), value.clone()].into());
                Some((
                    vec![dict],
                    Ty::List(Box::new(Ty::Tuple(vec![key, value].into())).into()),
                ))
            }
            S::GenericDictToInt => {
                let key = self.fresh();
                let value = self.fresh();
                Some((vec![Ty::Named("Dict".into(), vec![key, value].into())], Ty::Int))
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
            A::Bytes => Ty::Bytes,
            A::Int => Ty::Int,
            A::Bool => Ty::Bool,
            A::Secret => Ty::Secret(SecretRights::full()),
            A::ListString => Ty::List(Box::new(Ty::String).into()),
            A::Dir => Ty::Dir(DirRights::full()),
            A::DirPolicy => Ty::Named("DirPolicy".into(), Vec::new().into()),
            A::NetPolicy => Ty::Named("NetPolicy".into(), Vec::new().into()),
        }));
        let result = match operation.result {
            O::SameReceiver => receiver,
            O::Nil => Ty::Unit,
            O::Int => Ty::Int,
            O::String => Ty::String,
            O::Bytes => Ty::Bytes,
            O::Bool => Ty::Bool,
            O::ListString => Ty::List(Box::new(Ty::String).into()),
            O::OptionString => Ty::Named("Option".into(), vec![Ty::String].into()),
            O::Dir => Ty::Dir(DirRights::full()),
            O::File => Ty::File(FileRights::full()),
            O::Fetch => Ty::Fetch,
            O::Socket => Ty::Socket,
            O::OptionSocket => Ty::Named("Option".into(), vec![Ty::Socket].into()),
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
            Ty::Named(n, _) if **n == *"Option" => {
                self.unify(&Ty::Named("Option".into(), vec![elem.clone()].into()), &oty).map_err(|e| {
                    TypeError { message: format!("in `? \"msg\"`: {}", e.message) }
                })?;
            }
            Ty::Named(n, _) if **n == *"Result" => {
                self.unify(
                    &Ty::Named("Result".into(), vec![elem.clone(), Ty::String].into()),
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
        Ok(Some(Ty::Named("Result".into(), vec![elem, Ty::String].into())))
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
            // (RFC-0121) `Secret[Reveal]`/bare `Secret` narrows to `Secret[Seal]`,
            // never the reverse: `as` can only drop the reveal right.
            (Ty::Secret(s), Ty::Secret(t)) => {
                (!t.reveal || s.reveal) && (!t.seal || s.seal)
            }
            (Ty::Exec, Ty::Exec) => true,
            (Ty::Fetch, Ty::Fetch) => true,
            // An unconstrained source: pin it to the ascribed capability.
            (
                Ty::Var(_),
                Ty::Dir(_)
                | Ty::File(_)
                | Ty::Net(_)
                | Ty::Console(_)
                | Ty::Secret(_)
                | Ty::Exec
                | Ty::Fetch,
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
        let expected_resolved = self.resolve(expected);
        let actual_resolved = self.resolve(actual);
        if let (
            Ty::Fn(params, result, conventions, references, expected_qualifiers),
            Ty::Fn(actual_params, actual_result, actual_conventions, actual_references, actual_qualifiers),
        ) = (&expected_resolved, &actual_resolved)
            && expected_qualifiers.once == actual_qualifiers.once
            && !expected_qualifiers.pure
            && actual_qualifiers.pure
        {
            let widened = Ty::Fn(
                actual_params.clone(),
                actual_result.clone(),
                actual_conventions.clone(),
                actual_references.clone(),
                *expected_qualifiers,
            );
            let expected_callable = Ty::Fn(
                params.clone(),
                result.clone(),
                conventions.clone(),
                references.clone(),
                *expected_qualifiers,
            );
            return self.unify(&expected_callable, &widened);
        }
        let coercible = match (&expected_resolved, &actual_resolved) {
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
            // (RFC-0121) A bare `Secret` satisfies a `Secret[Seal]` parameter — more
            // authority stands in for less — while the callee stays bounded to `Seal`.
            (Ty::Secret(want), Ty::Secret(has)) => {
                (!want.reveal || has.reveal) && (!want.seal || has.seal)
            }
            _ => false,
        };
        if coercible
            || self.anon_union_widening_ok(expected, actual)?
            || self.record_width_conformance(expected, actual)?
        {
            return Ok(());
        }
        // (RFC-0121) A `Secret` that is too narrow for the parameter is a rights
        // failure, not a type mismatch. Name the missing right, in the same shape as
        // the `Dir`/`Console` rights diagnostics, so `reveal` on a sealed secret reads
        // as the policy decision it is rather than as "expected Secret, found
        // Secret[Seal]".
        if let (Ty::Secret(want), Ty::Secret(has)) = (&expected_resolved, &actual_resolved)
            && want.reveal
            && !has.reveal
        {
            return terr(format!(
                "this needs `Reveal` but the capability is `{has}`; a sealed secret is \
                 usable by handle (`crypto.sign`, `crypto.public_key`, `server.serve_tls`) \
                 but its bytes are never readable"
            ));
        }
        self.unify(expected, actual)
    }

    /// A rooted slice is a retained immutable carrier, not an owned `List`.
    /// Only catalogued read-only sequence operations may consume this
    /// compatibility at their receiver position; list mutation never reaches
    /// this helper.  Keep the element variable unified with the carrier payload
    /// so generic `list.length` retains its ordinary type contract.
    fn coerce_rooted_slice_read_arg(&mut self, expected: &Ty, actual: &Ty) -> Result<bool, TypeError> {
        let Ty::List(expected_element) = self.resolve(expected) else {
            return Ok(false);
        };
        let Ty::View(payload) = self.resolve(actual) else {
            return Ok(false);
        };
        let Ty::Named(name, arguments) = self.resolve(&payload) else {
            return Ok(false);
        };
        if name.as_ref() != "Slice" || arguments.len() != 1 {
            return Ok(false);
        }
        self.unify(&expected_element, &arguments[0])?;
        Ok(true)
    }

    fn callable_purity_widening(&self, expected: &Ty, actual: &Ty) -> bool {
        matches!(
            (self.resolve(expected), self.resolve(actual)),
            (
                Ty::Fn(_, _, _, _, CallableQualifiers { pure: false, once: expected_once }),
                Ty::Fn(_, _, _, _, CallableQualifiers { pure: true, once: actual_once }),
            ) if expected_once == actual_once
        )
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
        for (field, expected_type) in expected_fields.iter().zip(expected_types.iter()) {
            let Some(index) = actual_fields.iter().position(|candidate| candidate == field) else {
                self.restore_subst(saved_subst);
                return terr(format!(
                    "structural record `{actual}` does not conform to `{expected}`: missing required field `{field}`"
                ));
            };
            if let Err(error) = self.unify(expected_type, &actual_types[index]) {
                self.restore_subst(saved_subst);
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
            Ty::Fn(..) => Some(Uncomparable::Function),
            Ty::Dyn(_, _) => Some(Uncomparable::Existential),
            Ty::Console(_)
            | Ty::Clock
            | Ty::Rand
            | Ty::Env
            | Ty::Secret(_)
            | Ty::Exec
            | Ty::Fetch
            | Ty::Socket
            | Ty::Listener | Ty::Dir(_) | Ty::File(_) | Ty::Net(_) | Ty::BuildOut | Ty::BuildRead
            | Ty::BuildEnv | Ty::BuildNet | Ty::BuildExec => Some(Uncomparable::Capability),
            Ty::List(e) => self.uncomparable_kind(e, seen),
            Ty::Tuple(ts) => ts.iter().find_map(|x| self.uncomparable_kind(x, seen)),
            Ty::Named(n, args) => {
                // `SecretStore` is a capability the type checker models as a Named type.
                if **n == *"SecretStore" {
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
                if !seen.insert(n.to_string()) {
                    return None;
                }
                let found = self.named_field_uncomparable(n, seen);
                seen.remove(n.as_ref());
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
        // Reject this before ordinary reference-relation inference loses the
        // lexical owner and reports a misleading generic "unbound" error.
        // A local *handle* rooted in a parent remains legal; only a local
        // owner root is the region-born escape.
        let region_annotation_has_reference = block
            .region
            .as_ref()
            .and_then(|region| region.ty.as_ref())
            .is_some_and(type_contains_explicit_reference_relation);
        let region_direct_borrow = matches!(
            region_value_tail(block),
            Some(Expr::Unary {
                op: UnOp::Borrow | UnOp::BorrowMut,
                ..
            })
        );
        if is_region && (region_annotation_has_reference || region_direct_borrow) {
            let owner_root = region_reference_root(block);
            if let Some(root) = owner_root.as_ref()
                && region_declared_local_names(block).contains(root)
            {
                return terr(format!(
                    "cannot copy out region-born reference `{root}` from `region:`; its owner is rewound at region exit"
                ));
            }
            // We admit only a direct, checked reference carrier rooted in a
            // parent binding.  An aggregate reference needs a per-field owner
            // witness which this contract deliberately does not mint yet.
            // A view-carrying aggregate whose every view is syntactically
            // parent-rooted is proven again, against the region's complete
            // local set, in `region_promotion_fact`.
            if region_annotation_has_reference
                && !region_aggregate_views_parent_rooted(block, &region_declared_local_names(block))
                && (!block
                    .region
                    .as_ref()
                    .and_then(|region| region.ty.as_ref())
                    .is_some_and(type_is_explicit_reference)
                    || owner_root.is_none())
            {
                return terr(
                    "cannot copy out a region reference aggregate without a supported parent-owner carrier proof"
                        .to_string(),
                );
            }
        }
        if is_region {
            self.region_locals.push(HashSet::new());
        }
        let result = self.infer_block_inner(block, expected);
        let locals = is_region
            .then(|| self.region_locals.pop().expect("active region has locals"));
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
        if let Some(locals) = locals {
            let fact = self.region_promotion_fact(block, &ty, &locals)?;
            self.region_promotions.insert(block as *const Block as usize, fact);
        }
        Ok(ty)
    }

    /// Classify exactly what the checked region boundary promises.  Normal
    /// code may use the existing value/snapshot representation; opt code gets
    /// no implicit promotion copy and therefore records either a direct
    /// parent-owned pass-through or an obligation the canonical path must
    /// reject.  Reference handles are permitted only when their root is
    /// outside the region.
    fn region_promotion_fact(
        &self,
        block: &Block,
        ty: &Ty,
        locals: &HashSet<String>,
    ) -> Result<RegionPromotionFact, TypeError> {
        let tail = region_value_tail(block);
        let result = ty_to_ast(&self.resolve(ty)).ok_or_else(|| TypeError {
            message: "region value has no concrete checked representation".to_string(),
        })?;
        let annotation_is_reference = block
            .region
            .as_ref()
            .and_then(|region| region.ty.as_ref())
            .is_some_and(type_contains_explicit_reference_relation);
        let direct_borrow = matches!(tail, Some(Expr::Unary { op: UnOp::Borrow | UnOp::BorrowMut, .. }));
        let direct_view = matches!(self.resolve(ty), Ty::View(_));
        let carries_view = self.ty_carries_rooted_view(ty);
        let parent_rooted_aggregate =
            carries_view && !direct_view && region_aggregate_views_parent_rooted(block, locals);
        if carries_view && !direct_view && !parent_rooted_aggregate {
            return terr(
                "cannot copy out a region aggregate carrying immutable views without per-field parent-root provenance"
                    .to_string(),
            );
        }
        // A callable or task recipe hides what it captured behind its type, so
        // the type test above cannot see a region-born view inside it
        // (BUG-646). Refuse the copy-out whenever such a value is formed in a
        // region that also forms a view of one of its own locals.
        if ty_carries_callable_or_task(&self.resolve(ty), self)
            && block_forms_view_of_local(block, locals)
        {
            return terr(
                "cannot copy out a callable or task from `region:` that may retain a view of a region-born root; its backing root is rewound at region exit"
                    .to_string(),
            );
        }
        if (annotation_is_reference && !parent_rooted_aggregate) || direct_borrow || direct_view {
            let Some(root) = region_reference_root(block) else {
                return terr(
                    "cannot copy out an immutable view from `region:` without a parent-root provenance proof".to_string(),
                );
            };
            if locals.contains(&root) {
                return terr(format!(
                    "cannot copy out region-born view rooted in `{root}` from `region:`; its backing root is rewound at region exit"
                ));
            }
        }

        if is_scalar_ty(&self.resolve(ty)) {
            return Ok(RegionPromotionFact::Scalar { result });
        }
        if let Some(descriptor) = self.region_packed_layout_descriptor(&result) {
            return Ok(RegionPromotionFact::PackedInline { result, descriptor });
        }
        if !self.current_opt_callable {
            return Ok(RegionPromotionFact::NormalOwnedSnapshot {
                result,
                // A must result crosses this boundary only through the exact
                // fresh constructor expression that introduced its obligation.
                // A boolean would let a stale or forged fact claim transfer.
                must_disposition: if self.ty_carries_must_consume(ty) {
                    self.region_must_transfer(tail, ty)
                        .map(RegionMustDisposition::FreshConstructor)
                        .unwrap_or(RegionMustDisposition::Unproven)
                } else {
                    RegionMustDisposition::NoObligation
                },
            });
        }
        let owner_root = if annotation_is_reference || direct_borrow || direct_view {
            region_reference_root(block)
        } else {
            match tail {
                Some(Expr::Var(root)) if !locals.contains(root) && self.lookup(root).is_some() => {
                    Some(root.clone())
                }
                _ => None,
            }
        };
        match owner_root {
            Some(owner_root) if !locals.contains(&owner_root) => {
                let explicit_reference = annotation_is_reference || direct_borrow || direct_view;
                Ok(RegionPromotionFact::OptParentPassThrough {
                    result,
                    qualifier: self.region_promotion_qualifier(&owner_root, explicit_reference),
                    reference_loan: explicit_reference.then(|| RegionPromotionLoan {
                        owner_root: owner_root.clone(),
                    }),
                    owner_root,
                })
            }
            _ => Ok(RegionPromotionFact::OptNeedsCopyFreeProof { result }),
        }
    }

    /// Whether a runtime value stores an immutable rooted-view carrier. Function
    /// signatures are intentionally opaque here: parameter and result types are
    /// contracts, not captured storage. Closure captures are checked separately.
    fn ty_carries_rooted_view(&self, ty: &Ty) -> bool {
        fn go(checker: &Checker, ty: &Ty, seen: &mut HashSet<String>) -> bool {
            match checker.resolve(ty) {
                Ty::View(_) => true,
                Ty::List(item) => go(checker, &item, seen),
                Ty::Tuple(items) => items.iter().any(|item| go(checker, item, seen)),
                Ty::Dyn(_, arguments) => arguments.iter().any(|argument| go(checker, argument, seen)),
                Ty::Named(name, arguments) => {
                    let argument_hit = checker
                        .stored_obligations.stored_parameters
                        .get(name.as_ref())
                        .map(|stored| stored.iter().zip(arguments.iter()).any(|(stored, argument)| {
                            *stored && go(checker, argument, seen)
                        }))
                        .unwrap_or_else(|| arguments.iter().any(|argument| go(checker, argument, seen)));
                    if argument_hit || !seen.insert(name.to_string()) {
                        return argument_hit;
                    }
                    let field_hit = checker.record_fields.get(name.as_ref()).is_some_and(|(_, fields)| {
                        fields.iter().any(|(_, field)| go(checker, field, seen))
                    });
                    let variant_hit = checker.adt_variants.get(name.as_ref()).is_some_and(|variants| {
                        variants.iter().any(|variant| checker.ctor_sigs.get(variant).is_some_and(
                            |(payloads, _)| payloads.iter().any(|payload| go(checker, payload, seen)),
                        ))
                    });
                    seen.remove(name.as_ref());
                    field_hit || variant_hit
                }
                Ty::Fn(..)
                | Ty::Int | Ty::Float | Ty::Duration | Ty::String | Ty::Bytes | Ty::Msg
                | Ty::Bool | Ty::Unit | Ty::Console(_) | Ty::Clock | Ty::Rand | Ty::Env
                | Ty::Secret(_) | Ty::Exec | Ty::Fetch | Ty::Dir(_) | Ty::File(_) | Ty::Net(_)
                | Ty::Socket | Ty::Listener | Ty::BuildOut | Ty::BuildRead | Ty::BuildEnv
                | Ty::BuildNet | Ty::BuildExec | Ty::Var(_) => false,
            }
        }
        go(self, ty, &mut HashSet::new())
    }

    fn region_promotion_qualifier(
        &self,
        root: &str,
        explicit_reference: bool,
    ) -> RegionPromotionQualifier {
        if explicit_reference || self.is_explicit_reference_binding(root) {
            RegionPromotionQualifier::ExplicitReference
        } else if self.is_unique_value_binding(root) || self.is_unique_storage_binding(root) {
            RegionPromotionQualifier::Unique
        } else if self.is_frozen_binding(root) {
            RegionPromotionQualifier::Frozen
        } else {
            RegionPromotionQualifier::Ordinary
        }
    }

    fn region_packed_layout_descriptor(
        &self,
        result: &ast::Type,
    ) -> Option<RegionPackedLayoutDescriptor> {
        let ast::Type::Named(name, arguments) = result.unqualified() else { return None };
        if !arguments.is_empty() {
            return None;
        }
        self.region_packed_layouts
            .get(name)
            .cloned()
    }

    /// This is deliberately narrower than "the result carries must".  A
    /// direct constructor is the existing checker operation that mints one
    /// fresh obligation at this exact tail node.  A local, projection, call,
    /// or aggregate would require a separate disposition carrier and remains
    /// refused by canonical lowering.
    fn region_must_transfer(
        &self,
        tail: Option<&Expr>,
        result: &Ty,
    ) -> Option<RegionMustTransfer> {
        if !self.ty_carries_must_consume(result) {
            return None;
        }
        let tail = tail?;
        let Expr::Ctor { name, .. } = tail else { return None };
        self.ctor_sigs.contains_key(name).then(|| RegionMustTransfer {
            tail: tail as *const Expr as usize,
            constructor: name.clone(),
        })
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
                        self.reject_borrowed_nominal_container_type(
                            decl,
                            &format!("local type ascription for `{name}`"),
                        )?;
                        self.reject_explicit_once_reference_type(
                            decl,
                            &format!("local type ascription for `{name}`"),
                        )?;
                        let want = self.to_ty(decl);
                        let vt = self.infer_expected(value, &want).map_err(|e| {
                            type_mismatch_context(
                                || format!("`{name}` is declared `{want}` but the value disagrees"),
                                e,
                            )
                        })?;
                        let directed = self.existential_coercion(&want, &vt)?
                            || self.record_width_conformance(&want, &vt)?
                            || self.callable_purity_widening(&want, &vt);
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
                    if self.ty_carries_must_consume(&vt) {
                        self.reject_implicit_must_copy(value, &format!("binding `{name}`"))?;
                    }
                    self.reject_implicit_once_copy(value, &vt, &format!("binding `{name}`"))?;
                    self.transfer_unique_value(value, &vt, &format!("binding `{name}`"))?;
                    if self.opt_mode
                        && decl
                            .as_ref()
                            .is_some_and(|ty| self.declared_type_carries_unique_storage(ty))
                        && !self.expression_establishes_declared_unique_storage(
                            value,
                            decl.as_ref().expect("unique declaration was checked above"),
                            &vt,
                        )
                    {
                        return terr(format!(
                            "binding `{name}` cannot repair ordinary storage into `unique` in `mode opt`; pass an existing unique owner or construct fresh storage"
                        ));
                    }
                    let borrowed_shell_binding = self.is_direct_borrowed_nominal(&vt)
                        && Self::borrowed_shell_binding_source(value);
                    let borrowed_list_binding = self.is_direct_borrowed_nominal_list(&vt)
                        && (matches!(value, Expr::List(_))
                            || matches!(value, Expr::Var(source) if self.is_borrowed_shell_binding(source)));
                    let borrowed_shell_binding = borrowed_shell_binding || borrowed_list_binding;
                    if !borrowed_shell_binding {
                        self.reject_borrowed_nominal_runtime_ty(
                            &vt,
                            &format!("binding/copy into `{name}`"),
                        )?;
                    }
                    self.define(name.clone(), vt, *mutable);
                    let binding_ty = self.lookup(name).expect("new binding retains its type");
                    self.mark_must_consume_binding(name, &binding_ty);
                    if decl.as_ref().is_some_and(is_frozen_value_type)
                        || self.expression_returns_frozen_value(value)
                    {
                        self.authorize_frozen_binding(name.clone());
                    }
                    if decl.as_ref().is_some_and(is_unique_value_type) {
                        self.authorize_unique_value_binding(name.clone());
                    }
                    if decl.as_ref().map_or_else(
                        || self.expression_carries_unique_storage(value, &binding_ty)
                            || self.ty_carries_unique_storage(&binding_ty),
                        |ty| {
                            self.declared_type_carries_unique_storage(ty)
                                // (RFC-0167) `var d: Dict(k, v) = dict.new()` — an
                                // ordinary annotation on an inferred-fresh result is
                                // the unification constraint the spec describes, not
                                // an `as` coercion. Annotating a declared `unique`
                                // result with an ordinary type is a weakening and
                                // drops the proof.
                                || self.call_yields_inferred_owner(value)
                        },
                    )
                    {
                        self.authorize_unique_storage_binding(name.clone());
                        if self.call_yields_inferred_owner(value)
                            && !decl.as_ref().is_some_and(|ty| self.declared_type_carries_unique_storage(ty))
                        {
                            self.inferred_owner_bindings
                                .last_mut()
                                .expect("inferred owner bindings track type scopes")
                                .insert(name.clone());
                        }
                        if let Some(declared) = decl {
                            self.remember_unique_binding_type(name.clone(), declared.clone());
                        } else if let Some(source_type) =
                            self.inferred_unique_source_type(value, &binding_ty)
                        {
                            self.remember_unique_binding_type(name.clone(), source_type);
                        }
                    }
                    // A declared function type is the source-visible contract
                    // of this binding. It may explicitly weaken a stronger
                    // value contract, or give an inferred lambda its declared
                    // unique result fact after `Ty` erases it.
                    if let Some(contract) = decl
                        .as_ref()
                        .and_then(UniqueCallableContract::from_function_type)
                        .or_else(|| self.callable_unique_contract_for_expr(value))
                    {
                        self.remember_callable_unique_contract(name.clone(), contract);
                    }
                    // Unlike ordinary function type compatibility, opt
                    // provenance is never created by an ascription.  An alias
                    // carries it only when its source expression already did.
                    if matches!(self.resolve(&binding_ty), Ty::Fn(..))
                        && self.expression_opt_callable_contract(value).is_some()
                    {
                        self.remember_opt_callable_provenance(
                            name.clone(),
                            self.expression_opt_callable_contract(value)
                                .expect("checked opt callable expression has a contract"),
                        );
                    }
                    if matches!(
                        self.resolve(&binding_ty),
                        Ty::List(_) | Ty::Tuple(_) | Ty::Named(_, _)
                    )
                        && let Some(contract) = self.container_opt_contract_for_expr(value)
                    {
                        self.remember_container_opt_provenance(name.clone(), contract);
                    }
                    if borrowed_shell_binding {
                        self.authorize_borrowed_shell_binding(name.clone());
                    }
                    // Reference qualifiers erase to their payload in `Ty`, but
                    // a local formed by borrowing (or copying another explicit
                    // reference) still carries a loan into a closure capture.
                    // Keep that provenance alongside the ordinary local type.
                    if decl.as_ref().is_some_and(type_is_explicit_reference)
                        || matches!(value, Expr::Unary { op: UnOp::Borrow | UnOp::BorrowMut, .. })
                        || matches!(value, Expr::Var(source) if self.is_explicit_reference_binding(source))
                    {
                        self.explicit_reference_bindings
                            .last_mut()
                            .expect("reference bindings track type scopes")
                            .insert(name.clone());
                    }
                    ty = Ty::Unit;
                }
                Stmt::Assign { name, value, place_depth } => {
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
                    match self.assignment_targets_frozen_storage(name, value, *place_depth) {
                        Ok(true) => {
                            self.pop();
                            return terr(format!(
                                "cannot mutate through frozen value `{name}`; replace the whole value instead"
                            ));
                        }
                        Ok(false) => {}
                        Err(()) => {
                            self.pop();
                            return terr(format!(
                                "cannot verify deep assignment place rooted in `{name}`; reject to preserve frozen storage contracts"
                            ));
                        }
                    }
                    if self.current_pure
                        && self
                            .must_binding_key(name)
                            .is_some_and(|binding| self.pure_var_parameters.contains(&binding))
                    {
                        self.pop();
                        return terr(format!(
                            "pure callable cannot assign to `var` parameter `{name}` because that writes back to the caller"
                        ));
                    }
                    let must_preserving_self_update =
                        self.is_must_preserving_self_update(name, value);
                    if self.ty_carries_must_consume(&existing)
                        && self.is_live_must_binding(name)
                        && !must_preserving_self_update
                    {
                        self.pop();
                        return terr(format!(
                            "assignment would overwrite live must-consume value `{name}`; consume the old value before assigning a replacement"
                        ));
                    }
                    if self.is_borrowed_must_binding(name) && !must_preserving_self_update {
                        self.pop();
                        return terr(format!(
                            "assignment would replace borrowed must-consume value `{name}` and erase its caller-owned obligation"
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
                    let saved_shell_target = self.borrowed_shell_update_target.take();
                    if self.is_borrowed_shell_binding(name) {
                        self.borrowed_shell_update_target = Some(name.clone());
                    }
                    let saved_must_target = self.must_self_update_target.take();
                    if must_preserving_self_update {
                        self.must_self_update_target = Some(name.clone());
                    }
                    let inferred = self.infer_expected(value, &existing);
                    self.borrowed_shell_update_target = saved_shell_target;
                    self.must_self_update_target = saved_must_target;
                    let vt = inferred?;
                    if self.ty_carries_must_consume(&vt) {
                        self.reject_implicit_must_copy(value, &format!("assignment to `{name}`"))?;
                    }
                    self.reject_implicit_once_copy(
                        value,
                        &vt,
                        &format!("assignment to `{name}`"),
                    )?;
                    self.transfer_unique_value(value, &vt, &format!("assignment to `{name}`"))?;
                    if !self.is_borrowed_shell_self_update(name, &existing, value) {
                        self.reject_borrowed_nominal_runtime_ty(
                            &vt,
                            &format!("assignment/copy into `{name}`"),
                        )?;
                    }
                    if !self.existential_coercion(&existing, &vt)?
                        && !self.record_width_conformance(&existing, &vt)?
                        && !self.callable_purity_widening(&existing, &vt)
                    {
                        self.unify(&existing, &vt)?;
                    }
                    self.reinitialize_binding(name); // reassignment re-initializes
                    // Only a whole-binding reassignment replaces the root's
                    // storage. Assigning into a place (`d[k] = v`, `r.f = v`)
                    // mutates the storage the binding already owns, so its
                    // owner proof survives (RFC-0167 §4).
                    // A self-update `x = f(x, …)` (the place-assignment
                    // desugar, `x = dict.__insert(x, k, v)`) threads the
                    // binding's own storage through the call, so its owner
                    // proof survives too.
                    let self_update = matches!(
                        value,
                        Expr::Call { args, .. }
                            if args.first().is_some_and(|arg| matches!(arg, Expr::Var(root) if root == name))
                    );
                    if *place_depth == 0 && !self_update && !self.is_unique_value_binding(name) {
                        if let Some(bindings) = self
                            .unique_storage_bindings
                            .iter_mut()
                            .rev()
                            .find(|bindings| bindings.contains(name))
                        {
                            bindings.remove(name);
                        }
                        self.forget_inferred_owner_binding(name);
                        if self.expression_carries_unique_storage(value, &vt)
                            || self.ty_carries_unique_storage(&existing)
                        {
                            self.authorize_unique_storage_binding(name.clone());
                            if self.call_yields_inferred_owner(value) {
                                self.inferred_owner_bindings
                                    .last_mut()
                                    .expect("inferred owner bindings track type scopes")
                                    .insert(name.clone());
                            }
                        }
                    }
                    if self.ty_carries_must_consume(&existing)
                        && !self.is_borrowed_must_binding(name)
                    {
                        if let Some(binding) = self.must_binding_key(name) {
                            self.must_live.insert(binding);
                        }
                    }
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
                    self.reject_implicit_once_copy(value, &vt, "pattern destructuring")?;
                    self.transfer_unique_value(value, &vt, "pattern destructuring")?;
                    if self.ty_carries_must_consume(&vt) {
                        self.reject_implicit_must_copy(value, "pattern destructuring")?;
                        if self.expression_introduces_must_obligation(value, &vt) {
                            return terr(
                                "pattern destructuring would erase a must-consume obligation; transfer the value to an `own` operation before inspecting it",
                            );
                        }
                    }
                    if let Some(reason) = self.pattern_refutable(pattern, &vt) {
                        return terr(reason);
                    }
                    self.check_pattern(pattern, &vt)?;
                    self.remember_pattern_opt_provenance(pattern, value);
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
                    if self.ty_carries_must_consume(&t)
                        && let Some(Expr::Var(name)) = opt
                    {
                        if self.is_borrowed_must_binding(name) {
                            return terr(format!(
                                "cannot return borrowed must-consume value `{name}`; the caller retains its obligation"
                            ));
                        }
                        self.consume_must_binding(name);
                    }
                    if let Some(expression) = opt {
                        self.reject_implicit_once_copy(expression, &t, "return")?;
                        self.transfer_unique_value(expression, &t, "return")?;
                        if self.ty_carries_once_callable(&t)
                            && let Some(root) = self.affine_place_root(expression)
                        {
                            self.mark_consumed_binding(&root);
                        }
                    }
                    self.reject_all_live_must_before_return()?;
                    // A return diverges: its position can satisfy any expected
                    // type, so contribute a fresh var (which unifies with anything).
                    ty = self.fresh();
                }
                Stmt::Expr(e) => {
                    // A non-tail `&name` has no surviving handle. Preserve an
                    // earlier view of the same root, but do not let this
                    // discarded expression make a later opt call look aliased.
                    // Other places may evaluate effects or escape through a
                    // nested expression, so they retain the conservative mark.
                    let discarded_direct_view_root = if self.opt_mode && i != tail {
                        match e {
                            Expr::Unary { op: UnOp::Borrow, expr }
                                if matches!(expr.as_ref(), Expr::Var(_)) => self
                                    .affine_place_root(expr)
                                    .filter(|root| !self.opt_viewed_roots.contains(root)),
                            _ => None,
                        }
                    } else {
                        None
                    };
                    ty = if i == tail {
                        match expected {
                            Some(expected) => self.infer_expected(e, expected)?,
                            None => self.infer(e)?,
                        }
                    } else {
                        self.infer(e)?
                    };
                    if i != tail {
                        self.reject_implicit_once_copy(
                            e,
                            &ty,
                            "discarded expression",
                        )?;
                        if self.ty_carries_must_consume(&ty) {
                            return terr(
                                "a must-consume expression result cannot be discarded; bind it and consume it, pass it to an `own` parameter, or return it",
                            );
                        }
                        self.reject_borrowed_nominal_runtime_ty(
                            &ty,
                            "non-tail expression statement",
                        )?;
                        if let Some(root) = discarded_direct_view_root {
                            self.opt_viewed_roots.remove(&root);
                        }
                    }
                }
                Stmt::Yield(e) => {
                    if !self.region_locals.is_empty() {
                        self.pop();
                        return terr(
                            "cannot `yield` inside `region:`: the generator frame outlives the region".to_string(),
                        );
                    }
                    ty = self.infer(e)?;
                    self.reject_implicit_once_copy(e, &ty, "generator yield")?;
                    self.transfer_unique_value(e, &ty, "generator yield")?;
                    if self.ty_carries_once_callable(&ty)
                        && let Some(root) = self.affine_place_root(e)
                    {
                        self.mark_consumed_binding(&root);
                    }
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
        if let Some(Stmt::Expr(Expr::Var(name))) = block.stmts.last()
            && self.ty_carries_must_consume(&ty)
        {
            if self.is_borrowed_must_binding(name) {
                return terr(format!(
                    "cannot return borrowed must-consume value `{name}`; the caller retains its obligation"
                ));
            }
            self.consume_must_binding(name);
        }
        if let Some(Stmt::Expr(expression)) = block.stmts.last()
            && self.ty_carries_once_callable(&ty)
        {
            self.reject_implicit_once_copy(expression, &ty, "block result")?;
            if let Some(root) = self.affine_place_root(expression) {
                self.mark_consumed_binding(&root);
            }
        }
        if let Some(Stmt::Expr(expression)) = block.stmts.last() {
            self.transfer_unique_value(expression, &ty, "block result")?;
        }
        self.reject_live_must_in_current_scope()?;
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
        // Named local calls have no Expr::Var child. Preserve the exact
        // lexical callable type alongside the result, under the same
        // persistent-node recording boundary as every other expression fact.
        let local_callee = match expr {
            Expr::Call { name, .. } | Expr::LabeledCall { name, .. } => self.lookup(name),
            _ => None,
        };
        if let Some(rec) = &mut self.type_record {
            rec.insert(expr as *const Expr as usize, (t.clone(), !expr_can_complete(expr), local_callee));
        }
        Ok(t)
    }

    fn infer_expected(&mut self, expr: &Expr, expected: &Ty) -> Result<Ty, TypeError> {
        let actual = self.infer_expected_inner(expr, expected)?;
        // A typed storage/call boundary inside `mode opt` cannot turn an
        // ordinary callable into a trusted carrier merely because `Ty::Fn`
        // erases the mode marker.  Structural paths below recover only facts
        // admitted here (or by an already authenticated opt call).
        if self.current_opt_callable
            && self.opt_callable_contract_from_ty(expected).is_some()
        {
            self.require_opt_callable_provenance(expr, "opt callable storage", 0)?;
        }
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
                    if self.ty_carries_must_consume(expected) {
                        for item in items {
                            self.reject_implicit_must_copy(item, "list construction")?;
                        }
                    }
                    if self.ty_carries_once_callable(expected) {
                        for item in items {
                            self.reject_implicit_once_copy(
                                item,
                                &elem,
                                "list construction",
                            )?;
                        }
                    }
                    return self.finish_infer(expr, expected.clone());
                }
                let at = self.infer(expr)?;
                self.coerce_arg(expected, &at)?;
                Ok(at)
            }
            Expr::Tuple(items) => {
                if items.is_empty() && matches!(self.resolve(expected), Ty::Unit) {
                    return self.finish_infer(expr, Ty::Unit);
                }
                if let Ty::Tuple(slots) = self.resolve(expected) {
                    if slots.len() == items.len() {
                        for (item, slot) in items.iter().zip(slots.iter()) {
                            let at = self.infer_expected(item, slot)?;
                            self.coerce_arg(slot, &at)?;
                        }
                        if self.ty_carries_must_consume(expected) {
                            for item in items {
                                self.reject_implicit_must_copy(item, "tuple construction")?;
                            }
                        }
                        if self.ty_carries_once_callable(expected) {
                            for (item, slot) in items.iter().zip(slots.iter()) {
                                self.reject_implicit_once_copy(
                                    item,
                                    slot,
                                    "tuple construction",
                                )?;
                            }
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
                    let (fields, mut result) = self.instantiate(&fields, &result, &typarams);
                    if self.is_direct_borrowed_nominal(expected)
                        && matches!((self.resolve(expected), self.resolve(&result)),
                            (Ty::Named(expected_name, _), Ty::Named(result_name, _))
                                if expected_name == result_name)
                    {
                        // Lifetime arguments are relations, not runtime constructor
                        // operands. The annotated result supplies them while the
                        // constructor continues to check the ordinary field values.
                        result = expected.clone();
                    }
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
                    if self.ty_carries_must_consume(&result) {
                        for argument in args {
                            self.reject_implicit_must_copy(
                                argument,
                                &format!("constructor `{name}`"),
                            )?;
                        }
                    }
                    if self.ty_carries_once_callable(&result) {
                        for (argument, field) in args.iter().zip(&fields) {
                            self.reject_implicit_once_copy(
                                argument,
                                field,
                                &format!("constructor `{name}`"),
                            )?;
                        }
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
                let before = self.consumed.clone();
                let must_before = self.must_live.clone();
                let once_before = self.once_invoked.clone();
                let then_completes = block_can_complete(then_block);
                let tt = self.infer_block_expected(then_block, expected)?;
                let consumed_then = std::mem::replace(&mut self.consumed, before.clone());
                let must_then = std::mem::replace(&mut self.must_live, must_before.clone());
                let once_then =
                    std::mem::replace(&mut self.once_invoked, once_before.clone());
                self.coerce_arg(expected, &tt)?;
                let else_completes = if let Some(else_block) = else_block {
                    let et = self.infer_block_expected(else_block, expected)?;
                    self.coerce_arg(expected, &et)?;
                    block_can_complete(else_block)
                } else if matches!(self.resolve(expected), Ty::Dyn(_, _)) {
                    return terr(
                        "`if` without `else` has an implicit `()` path that cannot produce \
                         an existential value — add an explicit `else` branch"
                    );
                } else {
                    self.coerce_arg(expected, &Ty::Unit).map_err(|e| TypeError {
                        message: format!("`if` without `else` produces `()`: {}", e.message),
                    })?;
                    true
                };
                let branches = [
                    (then_completes, consumed_then, must_then, once_then),
                    (
                        else_completes,
                        self.consumed.clone(),
                        self.must_live.clone(),
                        self.once_invoked.clone(),
                    ),
                ];
                (self.consumed, self.must_live, self.once_invoked) =
                    join_reachable_binding_facts(
                        &before,
                        &must_before,
                        &once_before,
                        &branches,
                    );
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
        // `*reference = value` is represented after parsing as a private
        // place-write call so every later AST traversal sees one ordinary
        // expression shape. It is not source-callable (the `@` spelling is
        // unlexable by user code); the loan pass separately proves that its
        // first argument is a live exclusive reference.
        if name == intrinsics::REFERENCE_WRITE {
            if self.current_pure {
                return terr(
                    "pure callable cannot write through an explicit reference because that mutates caller-owned state",
                );
            }
            if args.len() != 2 {
                return terr("internal: reference write requires a reference and a value");
            }
            let reference = self.infer(&args[0])?;
            let value = self.infer_expected(&args[1], &reference)?;
            self.unify(&reference, &value)?;
            return Ok(Ty::Unit);
        }
        let is_cap_op = cap_ops::is_marked(name);
        let call_name = cap_ops::surface_name(name);
        if self.current_pure && is_cap_op {
            return terr(format!(
                "pure callable cannot invoke capability operation `{call_name}`"
            ));
        }
        self.reject_pure_caller_writeback(call_name, args)?;
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
                let qualifiers = self
                    .fn_qualifiers
                    .get(function)
                    .copied()
                    .unwrap_or(CallableQualifiers::ORDINARY);
                let function_ty = Ty::Fn(
                    params.into(),
                    Box::new(ret).into(),
                    conventions.into(),
                    vec![false; args.len()].into(),
                    qualifiers,
                );
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
            // Named application bypasses Expr::Var inference, but it still
            // reads the callable binding and cannot revive an own/move transfer.
            if self.is_consumed_binding(name) {
                let callee = Expr::Var(name.to_string());
                self.consume_once_callee(&callee, &vty)?;
                self.infer(&callee)?;
            }
            match self.resolve(&vty) {
                Ty::Fn(param_tys, ret, conventions, reference_params, qualifiers) => {
                    if self.current_pure && !qualifiers.pure {
                        return terr(format!(
                            "pure callable cannot invoke ordinary function value `{}`; require a `pure fn` parameter or binding",
                            diagnostic_callable_name(name)
                        ));
                    }
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
                    let unique_contract = self.callable_unique_contract(name);
                    let opt_callable = self.callable_opt_contract(name).is_some();
                    if let Some(expected) = expected {
                        self.coerce_arg(expected, ret.as_ref()).map_err(|e| TypeError {
                            message: format!("in call to `{display}`: {}", e.message),
                        })?;
                    }
                    for (index, (arg, pty)) in args.iter().zip(param_tys.iter()).enumerate() {
                        if opt_callable && matches!(self.resolve(pty), Ty::Fn(..)) {
                            self.require_opt_callable_provenance(arg, &display, index)?;
                        } else if !opt_callable {
                            self.reject_opt_callable_erasure(
                                arg,
                                &format!("argument {} to function value `{display}`", index + 1),
                            )?;
                        }
                        let at = self
                            .infer_expected(arg, pty)
                            .map_err(|e| in_call_context(&display, e))?;
                        self.reject_borrowed_nominal_runtime_ty(
                            &at,
                            &format!("call to function value `{display}`"),
                        )?;
                        self.reject_var_directed_coercion(
                            &display,
                            index,
                            conventions.get(index),
                            pty,
                            &at,
                        )?;
                        let unique_requested = self.opt_mode
                            && unique_contract
                                .as_ref()
                                .and_then(|contract| contract.unique_params.get(index))
                                .copied()
                                .unwrap_or(false);
                        if unique_requested {
                            if !self.expression_carries_unique_storage(arg, pty) {
                                return terr(format!(
                                    "argument {} to function value `{display}` cannot repair ordinary storage into a `unique` parameter in `mode opt`; pass an existing unique owner",
                                    index + 1
                                ));
                            }
                            self.reject_opt_unique_call_after_view(
                                arg, &format!("argument {} to function value `{display}`", index + 1),
                            )?;
                        }
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
                    self.enforce_function_value_conventions(
                        name,
                        args,
                        &param_tys,
                        &conventions,
                        &reference_params,
                        unique_contract.as_ref(),
                    )?;
                    if qualifiers.once {
                        self.consume_once_callee(&Expr::Var(name.to_string()), &vty)?;
                    }
                    self.reject_borrowed_nominal_runtime_ty(
                        ret.as_ref(),
                        &format!("call to function value `{display}` result"),
                    )?;
                    return Ok((*ret).clone());
                }
                Ty::Var(_) => {
                    if self.current_pure {
                        return terr(format!(
                            "pure callable cannot invoke `{}` without a statically declared `pure fn` contract",
                            diagnostic_callable_name(name)
                        ));
                    }
                    let mut argtys = Vec::new();
                    for arg in args {
                        let arg_ty = self.infer(arg)?;
                        self.reject_borrowed_nominal_runtime_ty(
                            &arg_ty,
                            &format!("call to function value `{name}`"),
                        )?;
                        argtys.push(arg_ty);
                    }
                    let ret = expected.cloned().unwrap_or_else(|| self.fresh());
                    self.unify(
                        &vty,
                        &Ty::Fn(
                            argtys.into(),
                            Box::new(ret.clone()).into(),
                            vec![Convention::Let; args.len()].into(),
                            vec![false; args.len()].into(),
                            CallableQualifiers::ORDINARY,
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
        self.reject_pure_intrinsic_call(call_name)?;
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
        if self.current_pure
            && catalog_contract.is_none()
            && user_sig.is_some()
            && !self
                .fn_qualifiers
                .get(name)
                .copied()
                .unwrap_or(CallableQualifiers::ORDINARY)
                .pure
        {
            return terr(format!(
                "pure callable cannot invoke ordinary function `{}`; the callee must be declared `pure fn`",
                diagnostic_callable_name(name)
            ));
        }
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
        let (params, ret, call_bounds, storage_obligations) = match catalog_contract {
            Some((params, ret, bounds)) => (params, ret, bounds, Vec::new()),
            None => match user_sig {
                Some(sig) => sig,
                None => {
                    let Some((params, ret)) = self.call_sig(call_name) else {
                        return self.unknown_call(call_name, args);
                    };
                    (params, ret, Vec::new(), Vec::new())
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
        let rooted_slice_read_intrinsic = intrinsics::lookup(call_name)
            .is_some_and(|spec| spec.signature.accepts_rooted_slice_view());
        let call_conventions = self.fn_conventions.get(name).cloned();
        for (index, (arg, param_ty)) in args.iter().zip(&params).enumerate() {
            let expects_opt_callable = self
                .fn_opt_provenance
                .get(name)
                .and_then(|contract| contract.params.get(index))
                .is_some_and(Option::is_some);
            if expects_opt_callable {
                self.require_opt_callable_provenance(arg, &display, index)?;
            } else {
                // An opt declaration can still have an ordinary or generic
                // parameter at this position.  Such a slot has no carrier for
                // a higher-order contract, so allowing it to accept one would
                // erase provenance through `fn id(a)(a) -> a`.  Only an
                // explicit callable declaration slot receives the opt entry
                // contract above.
                self.reject_opt_callable_erasure(
                    arg,
                    &format!("argument {} to `{display}`", index + 1),
                )?;
            }
            let typed_vm_callback = (call_name == "vm.with_dir" && index == 1)
                || (call_name == "vm.serve" && index == 2);
            let exact_generic = exact_generic_positions.get(index).copied().unwrap_or(false);
            let inferred = if typed_vm_callback {
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
                let callback_param_count = callback_params.len();
                Ok(Ty::Fn(
                    callback_params.into(),
                    Box::new(callback_ret).into(),
                    conventions.into(),
                    vec![false; callback_param_count].into(),
                    self.fn_qualifiers
                        .get(function)
                        .copied()
                        .unwrap_or(CallableQualifiers::ORDINARY),
                ))
            } else if exact_generic || (index == 0 && rooted_slice_read_intrinsic) {
                self.infer(arg)
            } else {
                self.infer_expected(arg, param_ty)
            };
            let at = inferred.map_err(|e| in_call_context(&display, e))?;
            let borrowed_list_read = self.is_nested_borrowed_nominal_list(&at)
                && matches!(
                    witchy_syntax::intrinsics::canonical_operation_name(&call_name),
                    witchy_syntax::intrinsics::LIST_AT | witchy_syntax::intrinsics::LIST_LENGTH
                );
            let borrowed_list_nested_access = index == 0
                && matches!(
                    witchy_syntax::intrinsics::canonical_operation_name(&call_name),
                    witchy_syntax::intrinsics::LIST_AT
                )
                && self.is_nested_borrowed_nominal_projection(arg, &at);
            if !borrowed_list_read && !borrowed_list_nested_access {
                self.reject_borrowed_nominal_runtime_ty(
                    &at,
                    &format!("call to `{display}`"),
                )?;
            }
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
            let result = if index == 0
                && rooted_slice_read_intrinsic
                && self.coerce_rooted_slice_read_arg(param_ty, &at)?
            {
                Ok(())
            } else if name == "vm.serve" && index == 2 {
                // Check a copied-byte invocation, not a conversion to the std
                // implementation's function-value parameter type. Preserve the
                // declaration's actual conventions in `at` and the type table.
                let access = |ty: &Ty| ty_to_ast(ty).and_then(|ty|
                    witchy_canonical_source::access::AccessSignature::from_function_type(&ty).ok());
                let compatible = access(&self.resolve(param_ty))
                    .zip(access(&self.resolve(&at)))
                    .is_some_and(|(expected, actual)|
                        crate::loans::vm_service_copy_callback_matches(&expected, &actual));
                if compatible {
                    Ok(())
                } else {
                    terr("`vm.serve` callback requires two default or own Bytes inputs and an owned Bytes result")
                }
            } else if exact_generic {
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
        for obligation in &storage_obligations {
            match obligation {
                StorageObligation::RootedView(payload) => {
                    self.admit_view_payload(payload, &format!("generic call to `{display}`"))?;
                }
                StorageObligation::ReplayCapture(capture) => {
                    self.admit_replay_capture_ty(capture, &format!("generic call to `{display}`"))?;
                }
            }
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
            let mut var_places: Vec<(usize, witchy_canonical_source::access::CheckedPlace)> = Vec::new();
            for (i, (arg, conv)) in args.iter().zip(&convs).enumerate() {
                self.reject_consumed_own_argument(arg, *conv)?;
                if let Some(parameter) = params.get(i) {
                    self.enforce_once_argument(
                        arg,
                        parameter,
                        *conv,
                        &format!("argument {} to `{name}`", i + 1),
                    )?;
                }
                if *conv == Convention::Var
                    && let Some(root) = self.frozen_binding_root(arg)
                    && !self
                        .fn_frozen_value_params
                        .get(name)
                        .and_then(|params| params.get(i))
                        .copied()
                        .unwrap_or(false)
                {
                    return terr(format!(
                        "argument {} to `{name}` cannot pass frozen value `{root}` to a non-frozen `var` parameter; declare that parameter `var frozen` to permit replacement",
                        i + 1
                    ));
                }
                let unique_requested = self.opt_mode
                    && self
                        .fn_unique_contracts
                        .get(name)
                        .and_then(|contract| contract.unique_params.get(i))
                        .copied()
                        .unwrap_or(false);
                if unique_requested {
                    if !params.get(i).is_some_and(|parameter| {
                        self.expression_carries_unique_storage(arg, parameter)
                    }) {
                        return terr(format!(
                            "argument {} to `{name}` cannot repair ordinary storage into a `unique` parameter in `mode opt`; pass an existing unique owner",
                            i + 1
                        ));
                    }
                    self.reject_opt_unique_call_after_view(arg, &format!("argument {} to `{name}`", i + 1))?;
                }
                let explicit_exclusive = self
                    .fn_exclusive_reference_params
                    .get(name)
                    .and_then(|params| params.get(i))
                    .copied()
                    .unwrap_or(false);
                // Preserve the existing reborrow/write-through handling for
                // non-consuming reference parameters. `own` must still reach
                // binding transfer when its argument is an existing handle.
                if explicit_exclusive
                    && *conv != Convention::Own
                    && !matches!(arg, Expr::Unary { op: UnOp::BorrowMut, .. })
                {
                    continue;
                }
                if explicit_exclusive
                    && let Expr::Unary { op: UnOp::BorrowMut, expr } = arg
                    && let Some(place) = witchy_canonical_source::access::checked_place(expr)
                {
                    for (previous_index, previous) in &var_places {
                        if previous.overlaps(&place) {
                            return terr(format!(
                                "arguments {} and {} to `{name}` are overlapping exclusive reference places rooted in `{}`",
                                previous_index + 1,
                                i + 1,
                                place.root()
                            ));
                        }
                    }
                    var_places.push((i, place));
                }
                match conv {
                    Convention::Var => {
                        let parameter = self.var_parameter_context(name, i);
                        // `var &'a mut T` writes through its exclusive place;
                        // it does not require a mutable *reference-handle*
                        // binding. The preceding exclusive-reference check has
                        // already recorded the referent place for aliasing.
                        if explicit_exclusive
                            && let Expr::Unary { op: UnOp::BorrowMut, expr } = arg
                            && let Some(place) = witchy_canonical_source::access::checked_place(expr)
                        {
                            if self.is_mutable(place.root()) == Some(true) {
                                continue;
                            }
                            return terr(format!(
                                "argument {} to {parameter} has immutable root `{}`; root `{}` must be a mutable `var` for exclusive write access",
                                i + 1,
                                place.root(),
                                place.root()
                            ));
                        }
                        if matches!(arg, Expr::Unary { op: UnOp::Move, .. }) {
                            return terr(format!(
                                "argument {} to {parameter} uses `move`; write-back requires a live mutable place in the caller",
                                i + 1
                            ));
                        }
                        match witchy_canonical_source::access::checked_place(arg) {
                            Some(place) if self.is_mutable(place.root()) == Some(true) => {
                                for (previous_index, previous) in &var_places {
                                    if previous.overlaps(&place) {
                                        return terr(format!(
                                            "arguments {} and {} to `{name}` are overlapping `var` places rooted in `{}`",
                                            previous_index + 1,
                                            i + 1,
                                            place.root()
                                        ));
                                    }
                                }
                                self.reject_later_writeback_conflict(name, args, i, &place)?;
                                self.reject_region_var_escape(
                                    place.root(),
                                    &format!("argument {} to {parameter}", i + 1),
                                )?;
                                var_places.push((i, place));
                            }
                            Some(place) => {
                                return terr(format!(
                                    "argument {} to {parameter} has immutable root `{}`; root `{}` must be a mutable `var` for write-back",
                                    i + 1,
                                    place.root(),
                                    place.root()
                                ));
                            }
                            None => {
                                return terr(format!(
                                    "argument {} to {parameter} must be a mutable place; bind the expression to a mutable `var` before the call",
                                    i + 1
                                ));
                            }
                        }
                    }
                    Convention::Own => {
                        if let Expr::Var(v) = arg {
                            if self.must_self_update_target.as_deref() != Some(v.as_str()) {
                                self.reject_own_of_borrowed_must(
                                    v,
                                    &format!("argument {} to `{name}`", i + 1),
                                )?;
                            }
                            self.transfer_owned_argument_binding(v);
                        }
                    }
                    // An owned value or an immutable borrow: no call-site
                    // obligation (a borrow's no-escape rule is enforced at
                    // native-lowering time by Rust's borrow checker).
                    Convention::Let => {
                        if let Some(parameter) = params.get(i) {
                            let into_unique = self
                                .callable_unique_contract(name)
                                .and_then(|contract| contract.unique_params.get(i).copied())
                                .unwrap_or(false);
                            self.transfer_unique_value_into(
                                arg,
                                parameter,
                                into_unique,
                                &format!("argument {} to `{name}`", i + 1),
                            )?;
                        }
                        self.reject_implicit_must_copy(
                            arg,
                            &format!("argument {} to `{name}`", i + 1),
                        )?;
                        if let Some(parameter) = params.get(i) {
                            self.reject_must_borrowed_temporary(
                                arg,
                                parameter,
                                &format!("argument {} to `{name}`", i + 1),
                            )?;
                        }
                    }
                    Convention::Borrow => {
                        if let Some(parameter) = params.get(i) {
                            self.reject_must_borrowed_temporary(
                                arg,
                                parameter,
                                &format!("argument {} to `{name}`", i + 1),
                            )?;
                        }
                    }
                }
            }
        }
        self.reject_externref_cap_aggregate_ty(&ret, &format!("call to `{call_name}`"))?;
        self.reject_structural_authority_ty(&ret, &format!("call to `{call_name}`"))?;
        self.reject_runtime_compiler_syntax_ty(&ret, &format!("call to `{call_name}`"))?;
        let operation_is_nested_list_projection = matches!(
            witchy_syntax::intrinsics::canonical_operation_name(&call_name),
            witchy_syntax::intrinsics::LIST_AT
        ) && self.is_nested_borrowed_nominal_list(&ret);
        if witchy_syntax::intrinsics::canonical_operation_name(&call_name)
            == witchy_syntax::intrinsics::LIST_AT
            && ty_to_ast(&self.resolve(&ret)).is_some_and(|source| {
                self.storage_plan(&source).requires_affine_owner_token()
            })
        {
            return terr("list.at reads by copy; use a consuming extraction for move-only elements");
        }
        if !self.is_direct_borrowed_nominal(&ret) && !operation_is_nested_list_projection {
            self.reject_borrowed_nominal_runtime_ty(
                &ret,
                &format!("call to `{display}` result"),
            )?;
        }
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
        parameter_types: &[Ty],
        conventions: &[Convention],
        reference_params: &[bool],
        source_contract: Option<&UniqueCallableContract>,
    ) -> Result<(), TypeError> {
        let mut var_places: Vec<(usize, witchy_canonical_source::access::CheckedPlace)> = Vec::new();
        for (index, (arg, convention)) in args.iter().zip(conventions).enumerate() {
            self.reject_consumed_own_argument(arg, *convention)?;
            if let Some(parameter) = parameter_types.get(index) {
                self.enforce_once_argument(
                    arg,
                    parameter,
                    *convention,
                    &format!("argument {} to function value `{name}`", index + 1),
                )?;
            }
            if *convention == Convention::Var
                && let Some(root) = self.frozen_binding_root(arg)
                && !source_contract
                    .and_then(|contract| contract.frozen_params.get(index))
                    .copied()
                    .unwrap_or(false)
            {
                return terr(format!(
                    "argument {} to function value `{name}` cannot pass frozen value `{root}` to a non-frozen or erased `var` parameter contract",
                    index + 1
                ));
            }
            if reference_params.get(index).copied().unwrap_or(false)
                && let Expr::Unary { op: UnOp::BorrowMut, expr } = arg
                && let Some(place) = witchy_canonical_source::access::checked_place(expr)
            {
                for (previous_index, previous) in &var_places {
                    if previous.overlaps(&place) {
                        return terr(format!(
                            "arguments {} and {} to `{name}` are overlapping exclusive reference places rooted in `{}`",
                            previous_index + 1,
                            index + 1,
                            place.root()
                        ));
                    }
                }
                var_places.push((index, place));
            }
            match convention {
                Convention::Var => {
                    let parameter = self.var_parameter_context(name, index);
                    // See the named-call path above: a direct exclusive borrow
                    // is the writable place for `var &'a mut T`; requiring a
                    // mutable reference-handle local would make the signature
                    // impossible to call.
                    if reference_params.get(index).copied().unwrap_or(false)
                        && let Expr::Unary { op: UnOp::BorrowMut, expr } = arg
                        && let Some(place) = witchy_canonical_source::access::checked_place(expr)
                    {
                        if self.is_mutable(place.root()) == Some(true) {
                            continue;
                        }
                        return terr(format!(
                            "argument {} to {parameter} has immutable root `{}`; root `{}` must be a mutable `var` for exclusive write access",
                            index + 1,
                            place.root(),
                            place.root()
                        ));
                    }
                    if matches!(arg, Expr::Unary { op: UnOp::Move, .. }) {
                        return terr(format!(
                            "argument {} to {parameter} uses `move`; write-back requires a live mutable place in the caller",
                            index + 1
                        ));
                    }
                    match witchy_canonical_source::access::checked_place(arg) {
                        Some(place) if self.is_mutable(place.root()) == Some(true) => {
                            for (previous_index, previous) in &var_places {
                                if previous.overlaps(&place) {
                                    return terr(format!(
                                        "arguments {} and {} to `{name}` are overlapping `var` places rooted in `{}`",
                                        previous_index + 1,
                                        index + 1,
                                        place.root()
                                    ));
                                }
                            }
                            self.reject_later_writeback_conflict(name, args, index, &place)?;
                            self.reject_region_var_escape(
                                place.root(),
                                &format!("argument {} to {parameter}", index + 1),
                            )?;
                            var_places.push((index, place));
                        }
                        Some(place) => {
                            return terr(format!(
                                "argument {} to {parameter} has immutable root `{}`; root `{}` must be a mutable `var` for write-back",
                                index + 1,
                                place.root(),
                                place.root()
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
                        self.reject_own_of_borrowed_must(
                            var,
                            &format!("argument {} to function value `{name}`", index + 1),
                        )?;
                        self.transfer_owned_argument_binding(var);
                    }
                }
                Convention::Let => {
                    if let Some(parameter) = parameter_types.get(index) {
                        let into_unique = self
                            .callable_unique_contract(name)
                            .and_then(|contract| contract.unique_params.get(index).copied())
                            .unwrap_or(false);
                        self.transfer_unique_value_into(
                            arg,
                            parameter,
                            into_unique,
                            &format!("argument {} to function value `{name}`", index + 1),
                        )?;
                    }
                    self.reject_implicit_must_copy(
                        arg,
                        &format!("argument {} to function value `{name}`", index + 1),
                    )?;
                    if let Some(parameter) = parameter_types.get(index) {
                        self.reject_must_borrowed_temporary(
                            arg,
                            parameter,
                            &format!("argument {} to function value `{name}`", index + 1),
                        )?;
                    }
                }
                Convention::Borrow => {
                    if let Some(parameter) = parameter_types.get(index) {
                        self.reject_must_borrowed_temporary(
                            arg,
                            parameter,
                            &format!("argument {} to function value `{name}`", index + 1),
                        )?;
                    }
                }
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
        parameter_types: &[Ty],
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

        let mut var_places: Vec<(usize, witchy_canonical_source::access::CheckedPlace)> = Vec::new();
        for (index, (operand, convention)) in operands.iter().zip(conventions).enumerate() {
            self.reject_consumed_own_argument(operand, *convention)?;
            if index > 0
                && let Some(parameter) = parameter_types.get(index - 1)
            {
                self.enforce_once_argument(
                    operand,
                    parameter,
                    *convention,
                    &format!("operand {} to existential `{callee}`", index + 1),
                )?;
            }
            match convention {
                Convention::Var => {
                    if matches!(operand, Expr::Unary { op: UnOp::Move, .. }) {
                        return terr(format!(
                            "operand {} to existential `{callee}` uses `move`; write-back requires a live mutable place in the caller",
                            index + 1
                        ));
                    }
                    match witchy_canonical_source::access::checked_place(operand) {
                        Some(place) if self.is_mutable(place.root()) == Some(true) => {
                            for (previous_index, previous) in &var_places {
                                if previous.overlaps(&place) {
                                    return terr(format!(
                                        "operands {} and {} to existential `{callee}` are overlapping `var` places rooted in `{}`",
                                        previous_index + 1,
                                        index + 1,
                                        place.root()
                                    ));
                                }
                            }
                            self.reject_later_writeback_conflict(
                                &callee,
                                &operands,
                                index,
                                &place,
                            )?;
                            self.reject_region_var_escape(
                                place.root(),
                                &format!("operand {} to existential `{callee}`", index + 1),
                            )?;
                            var_places.push((index, place));
                        }
                        Some(place) => {
                            return terr(format!(
                                "operand {} to existential `{callee}` has immutable root `{}`; root `{}` must be a mutable `var` for write-back",
                                index + 1,
                                place.root(),
                                place.root()
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
                        self.reject_own_of_borrowed_must(
                            name,
                            &format!("operand {} to existential `{callee}`", index + 1),
                        )?;
                        self.transfer_owned_argument_binding(name);
                    }
                }
                Convention::Let => {
                    if index > 0 && let Some(parameter) = parameter_types.get(index - 1) {
                        self.transfer_unique_value(
                            operand,
                            parameter,
                            &format!("operand {} to existential `{callee}`", index + 1),
                        )?;
                    }
                    self.reject_implicit_must_copy(
                        operand,
                        &format!("operand {} to existential `{callee}`", index + 1),
                    )?;
                }
                Convention::Borrow => {}
            }
        }
        Ok(())
    }

    fn call_conventions_for_expr(&self, name: &str) -> Option<Vec<Convention>> {
        self.fn_conventions.get(name).cloned().or_else(|| {
            let ty = self.lookup(name)?;
            match self.resolve(&ty) {
                Ty::Fn(_, _, conventions, _, _) => Some(conventions.to_vec()),
                _ => None,
            }
        })
    }

    fn collect_var_writebacks_in_expr(
        &self,
        expr: &Expr,
        out: &mut Vec<(String, usize, witchy_canonical_source::access::CheckedPlace)>,
    ) {
        match expr {
            Expr::Call { name, args } => {
                if let Some(conventions) = self.call_conventions_for_expr(name) {
                    for (index, (argument, convention)) in
                        args.iter().zip(&conventions).enumerate()
                    {
                        if *convention == Convention::Var
                            && let Some(place) = witchy_canonical_source::access::checked_place(argument)
                        {
                            out.push((name.clone(), index, place));
                        }
                    }
                } else {
                    // A callable introduced inside a later argument has not
                    // entered this outer inference scope yet. Absence of its
                    // convention cannot erase an earlier live `var`
                    // reservation, so every assignable argument is a
                    // potential write-back until its call is checked locally.
                    for (index, argument) in args.iter().enumerate() {
                        if let Some(place) = witchy_canonical_source::access::checked_place(argument) {
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
                        && let Some(place) = witchy_canonical_source::access::checked_place(argument)
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
                if let Expr::Var(name) = func.as_ref() {
                    if let Some(conventions) = self.call_conventions_for_expr(name) {
                        for (index, (argument, convention)) in
                            args.iter().zip(&conventions).enumerate()
                        {
                            if *convention == Convention::Var
                                && let Some(place) = witchy_canonical_source::access::checked_place(argument)
                            {
                                out.push((name.clone(), index, place));
                            }
                        }
                    } else {
                        for (index, argument) in args.iter().enumerate() {
                            if let Some(place) = witchy_canonical_source::access::checked_place(argument) {
                                out.push((name.clone(), index, place));
                            }
                        }
                    }
                } else {
                    // A computed callable can carry a `var` convention even
                    // though this outer reservation walk has no stable name
                    // with which to query its signature. Treat every
                    // assignable argument as a potential write-back; the
                    // call's own inference later determines its exact ABI.
                    for (index, argument) in args.iter().enumerate() {
                        if let Some(place) = witchy_canonical_source::access::checked_place(argument) {
                            out.push(("computed callable".to_string(), index, place));
                        }
                    }
                }
                self.collect_var_writebacks_in_expr(func, out);
                for argument in args {
                    self.collect_var_writebacks_in_expr(argument, out);
                }
            }
            Expr::LabeledCall { name, args } => {
                // Keyword resolution normally makes this positional before
                // type checking. Keep the reservation census fail-closed if
                // pipeline ordering changes: labels do not identify the
                // convention order here, so any assignable argument may be
                // the callee's `var` lane.
                if self.call_conventions_for_expr(name)
                    .is_none_or(|conventions| conventions.contains(&Convention::Var))
                {
                    for (index, (_, argument)) in args.iter().enumerate() {
                        if let Some(place) = witchy_canonical_source::access::checked_place(argument) {
                            out.push((name.clone(), index, place));
                        }
                    }
                }
                for (_, argument) in args {
                    self.collect_var_writebacks_in_expr(argument, out);
                }
            }
            Expr::LabeledMethodCall { receiver, args, .. } => {
                self.collect_var_writebacks_in_expr(receiver, out);
                for (_, argument) in args {
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
        out: &mut Vec<(String, usize, witchy_canonical_source::access::CheckedPlace)>,
    ) {
        for statement in &block.stmts {
            match statement {
                Stmt::Assign { name, value, place_depth } => {
                    out.push((
                        "assignment".to_string(),
                        0,
                        witchy_canonical_source::access::checked_assignment_place(name, value, *place_depth),
                    ));
                    self.collect_var_writebacks_in_expr(value, out);
                }
                Stmt::Let { value, .. }
                | Stmt::LetPattern { value, .. }
                | Stmt::Return(Some(value))
                | Stmt::Expr(value)
                | Stmt::Yield(value) => self.collect_var_writebacks_in_expr(value, out),
                Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
            }
        }
    }

    /// A `var` argument is a write-back sink: the callee's replacement lands
    /// in the caller's binding.  Inside `region:` a binding declared outside
    /// the region outlives it, so a write-back whose type can hold a view (or
    /// a callable/task hiding one, BUG-646) could carry a view of a
    /// region-born root past the region's exit (BUG-648).  Refuse it exactly
    /// as direct assignment to such a binding is refused: a region's only
    /// escape is its value.
    fn reject_region_var_escape(&self, root: &str, context: &str) -> Result<(), TypeError> {
        let Some(scope) = self.region_locals.last() else { return Ok(()) };
        if scope.contains(root) {
            return Ok(());
        }
        let Some(ty) = self.lookup(root) else { return Ok(()) };
        if self.ty_carries_rooted_view(&ty) || ty_carries_callable_or_task(&self.resolve(&ty), self) {
            return terr(format!(
                "{context} cannot write back into `{root}` inside `region:`: it is declared outside the region and can hold a view — a region's only escape is its value"
            ));
        }
        Ok(())
    }

    fn reject_later_writeback_conflict(
        &self,
        callee: &str,
        args: &[Expr],
        reserved_index: usize,
        reserved: &witchy_canonical_source::access::CheckedPlace,
    ) -> Result<(), TypeError> {
        for (later_index, argument) in args.iter().enumerate().skip(reserved_index + 1) {
            let mut writebacks = Vec::new();
            self.collect_var_writebacks_in_expr(argument, &mut writebacks);
            for (nested_callee, nested_index, nested_place) in writebacks {
                if reserved.overlaps(&nested_place) {
                    return terr(format!(
                        "argument {} to `{callee}` reserves `var` place rooted in `{}` until the call returns, but later argument {} writes back to an overlapping place through argument {} of `{nested_callee}`; written evaluation order keeps the earlier reservation live",
                        reserved_index + 1,
                        reserved.root(),
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
                    self.reject_implicit_once_copy(
                        arg,
                        fty,
                        &format!("anonymous union tag `.{tag}`"),
                    )?;
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

    fn infer_index_parts(&mut self, base: &Expr, index: &Expr) -> Result<(Ty, Ty), TypeError> {
        let base_ty = self.infer(base)?;
        // A rooted view carries a checked immutable projection, not an owned
        // list handle.  Reads through it keep the payload type; formation of a
        // further `&` projection is authenticated separately by `views`.
        let payload = match self.resolve(&base_ty) {
            Ty::View(payload) => payload.as_ref().clone(),
            other => other,
        };
        if let Expr::Range { lo, hi, .. } = index {
            let lo_ty = self.infer(lo)?;
            let hi_ty = self.infer(hi)?;
            self.unify(&lo_ty, &Ty::Int)?;
            self.unify(&hi_ty, &Ty::Int)?;
            let element = match self.resolve(&payload) {
                Ty::List(element) => element.as_ref().clone(),
                Ty::Named(name, arguments)
                    if (name == "List".into() || name == "Slice".into()) && arguments.len() == 1 =>
                {
                    arguments[0].clone()
                }
                other => return terr(format!("cannot slice `{other}`")),
            };
            return Ok((base_ty, Ty::Named("Slice".into(), vec![element].into())));
        }
        if let Ty::Named(name, arguments) = self.resolve(&payload)
            && name == "Slice".into()
            && arguments.len() == 1
        {
            let index_ty = self.infer(index)?;
            self.unify(&index_ty, &Ty::Int)?;
            return Ok((base_ty, arguments[0].clone()));
        }
        if let Ty::List(element) = self.resolve(&payload) {
            let index_ty = self.infer(index)?;
            self.unify(&index_ty, &Ty::Int)?;
            if ty_to_ast(&self.resolve(&element)).is_some_and(|source| {
                self.storage_plan(&source).requires_affine_owner_token()
            }) {
                return terr("list indexing reads by copy; use a consuming extraction for move-only elements");
            }
            return Ok((base_ty, element.as_ref().clone()));
        }
        // The transient below types clones; infer the source index directly so
        // its persistent node retains the fact canonical lowering consumes.
        let _ = self.infer(index)?;
        let desugared = if matches!(payload, Ty::Named(ref name, _) if **name == *"Dict") {
            Expr::Call {
                name: intrinsics::DICT_AT.to_string(),
                args: vec![base.clone(), index.clone()],
            }
        } else {
            witchy_syntax::parser::desugar_index(base.clone(), index.clone())
        };
        let result = self.infer_transient(&desugared)?;
        Ok((base_ty, result))
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
                // Record facts on the retained source operands, not on clones
                // whose addresses disappear after this inference. Infer each
                // operand once so ownership/call effects are not duplicated.
                let lower = self.infer(lo)?;
                let upper = self.infer(hi)?;
                self.push();
                let lower_name = "__checked_range_lo".to_owned();
                let upper_name = "__checked_range_hi".to_owned();
                self.define(lower_name.clone(), lower, false);
                self.define(upper_name.clone(), upper, false);
                // Keep the existing desugared range constraints, including
                // comparison and increment typing, over these private locals.
                let d = witchy_syntax::parser::desugar_range(
                    Expr::Var(lower_name), Expr::Var(upper_name), *inclusive);
                let result = self.infer_transient(&d);
                self.pop();
                result
            }
            // A retained subscript gets a type-directed read. Slice views use
            // their checked element projection; other collection forms keep
            // the existing transient call and its index/key constraints.
            Expr::Index { base, index } => {
                self.infer_index_parts(base, index).map(|(_, result)| result)
            }
            // RFC-0167 §5: `.owned()` is a compiler-known operation, not a
            // library method. Trait lowering leaves it in place until the
            // receiver's checked type is in the table, then lowers it to the
            // view read `*view` (or identity on an owned value). An admitted
            // slice keeps the call for its checked projection-copy lowering.
            Expr::MethodCall { receiver, method, args }
                if method == OWNED_MATERIALIZATION && args.is_empty() =>
            {
                let receiver_ty = self.infer(receiver)?;
                let result = self.owned_materialization_ty(&receiver_ty)?;
                if self.type_record.is_some() {
                    self.compiler_owned_materializations.insert(expr as *const Expr as usize);
                }
                Ok(result)
            }
            Expr::MethodCall { receiver, method, .. } => {
                // Trait lowering resolves every method call (impl, trait
                // bound, or static); one that survives is unresolvable.
                let receiver_ty = self.infer(receiver)?;
                self.reject_borrowed_nominal_runtime_ty(
                    &receiver_ty,
                    &format!("method call `.{method}(…)`"),
                )?;
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
                params,
                ..
            } => {
                if self.current_pure {
                    return terr(format!(
                        "pure callable cannot invoke existential method `{owner_trait}.{method}` because its implementation is not statically effect-classified"
                    ));
                }
                let receiver_ty = self.infer(receiver)?;
                self.reject_borrowed_nominal_runtime_ty(
                    &receiver_ty,
                    &format!("existential method call `{owner_trait}.{method}`"),
                )?;
                for arg in args {
                    let arg_ty = self.infer(arg)?;
                    self.reject_borrowed_nominal_runtime_ty(
                        &arg_ty,
                        &format!("existential method call `{owner_trait}.{method}`"),
                    )?;
                }
                let parameter_types = params
                    .iter()
                    .map(|parameter| self.to_ty(parameter))
                    .collect::<Vec<_>>();
                self.enforce_existential_conventions(
                    owner_trait,
                    method,
                    receiver,
                    args,
                    conventions,
                    &parameter_types,
                )?;
                let result = self.to_ty(result);
                self.reject_borrowed_nominal_runtime_ty(
                    &result,
                    &format!("existential method call `{owner_trait}.{method}`"),
                )?;
                Ok(result)
            }
            Expr::ExistentialPack { expr, ty, .. } => {
                let source = self.infer(expr)?;
                self.reject_borrowed_nominal_runtime_ty(
                    &source,
                    "existential erasure",
                )?;
                if self.ty_carries_once_callable(&source) {
                    return terr(format!(
                        "existential erasure cannot hide affine once-callable payload `{}`",
                        self.resolve(&source),
                    ));
                }
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
            Expr::LabeledMethodCall { .. } => {
                unreachable!(
                    "Expr::LabeledMethodCall is lowered by witchy_syntax::keyword_args before typeck"
                )
            }
            // `while let` lowers to a `while true` over a match; type that.
            Expr::WhileLet { pattern, scrutinee, body } => {
                // The source scrutinee, pattern bindings, and body are
                // inferred directly so the table carries their types for the
                // canonical lowering (the transient below types clones); the
                // desugared `while true` over a `match` then keeps the loop's
                // remaining checks.
                let consumed_before = self.consumed.clone();
                let scrutinee_ty = self.infer(scrutinee)?;
                let must_before = self.must_live.clone();
                self.push();
                self.check_pattern(pattern, &scrutinee_ty)?;
                self.infer_block(body)?;
                self.pop();
                self.reject_affine_loop_backedge(&consumed_before, "a `while let` loop")?;
                self.must_live = &must_before | &self.must_live;
                let d = witchy_syntax::parser::desugar_while_let(
                    pattern.clone(),
                    (**scrutinee).clone(),
                    body.clone(),
                );
                self.infer_transient(&d)
            }
            Expr::List(items) => {
                for item in items {
                    self.reject_opt_callable_erasure(item, "list construction")?;
                }
                let elem = self.fresh();
                for it in items {
                    let t = self.infer(it)?;
                    self.unify(&elem, &t)?;
                }
                let ty = Ty::List(Box::new(elem.clone()).into());
                for item in items {
                    self.transfer_unique_value(item, &elem, "list construction")?;
                }
                if self.ty_carries_must_consume(&ty) {
                    for item in items {
                        self.reject_implicit_must_copy(item, "list construction")?;
                    }
                }
                if self.ty_carries_once_callable(&ty) {
                    for item in items {
                        self.reject_implicit_once_copy(item, &elem, "list construction")?;
                    }
                }
                self.reject_externref_cap_aggregate_ty(&ty, "list literal")?;
                self.reject_structural_authority_ty(&ty, "list literal")?;
                Ok(ty)
            }
            Expr::Tuple(items) => {
                if items.is_empty() {
                    return Ok(Ty::Unit);
                }
                for item in items {
                    self.reject_opt_callable_erasure(item, "tuple construction")?;
                }
                let tys = items
                    .iter()
                    .map(|e| self.infer(e))
                    .collect::<Result<Vec<_>, _>>()?;
                let ty = Ty::Tuple(tys.into());
                if let Ty::Tuple(item_tys) = &ty {
                    for (item, item_ty) in items.iter().zip(item_tys.iter()) {
                        self.transfer_unique_value(item, item_ty, "tuple construction")?;
                    }
                }
                if self.ty_carries_must_consume(&ty) {
                    for item in items {
                        self.reject_implicit_must_copy(item, "tuple construction")?;
                    }
                }
                if self.ty_carries_once_callable(&ty) {
                    for (item, item_ty) in items.iter().zip(match &ty {
                        Ty::Tuple(types) => types.as_ref(),
                        _ => &[],
                    }) {
                        self.reject_implicit_once_copy(item, item_ty, "tuple construction")?;
                    }
                }
                self.reject_externref_cap_aggregate_ty(&ty, "tuple literal")?;
                self.reject_structural_authority_ty(&ty, "tuple literal")?;
                Ok(ty)
            }
            Expr::Var(name) => {
                if self.is_consumed_binding(name) {
                    if self.is_unique_value_binding(name) || self.is_unique_storage_binding(name) {
                        return terr(format!(
                            "use of unique value `{name}` after it was transferred"
                        ));
                    }
                    if self
                        .must_binding_key(name)
                        .is_some_and(|binding| self.once_invoked.contains(&binding))
                    {
                        return terr(format!(
                            "use of once-callable `{name}` after it was consumed by invocation"
                        ));
                    }
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
                    if self
                        .fn_storage_obligations
                        .get(name)
                        .is_some_and(|constraints| !constraints.is_empty())
                    {
                        return terr(format!(
                            "generic function `{name}` has storage obligations and cannot be transported as a function value; they require a direct checked call",
                        ));
                    }
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
                    let param_count = params.len();
                    let reference_params = self
                        .fn_exclusive_reference_params
                        .get(name)
                        .cloned()
                        .unwrap_or_else(|| vec![false; param_count]);
                    let qualifiers = self
                        .fn_qualifiers
                        .get(name)
                        .copied()
                        .unwrap_or(CallableQualifiers::ORDINARY);
                    let function_ty = Ty::Fn(
                        params.into(),
                        Box::new(ret).into(),
                        conventions.into(),
                        reference_params.into(),
                        qualifiers,
                    );
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
            Expr::Lambda { params, body, ret, qualifiers } => {
                validate_callable_nominal_lifetimes(
                    "lambda",
                    params,
                    ret.as_ref(),
                    &self.borrowed_nominal_types,
                )?;
                for parameter in params {
                    if let Some(declared) = &parameter.ty {
                        self.reject_explicit_once_reference_type(
                            declared,
                            &format!("lambda parameter `{}`", parameter.name),
                        )?;
                    }
                }
                if let Some(result) = ret {
                    self.reject_explicit_once_reference_type(
                        result,
                        "lambda return type",
                    )?;
                }
                // Closures capture by value, so an assignment to a captured
                // (outer) variable cannot propagate out: the interpreter would
                // silently mutate a private copy while the compiled backends can't
                // express it at all. Reject it uniformly here (using the shared
                // AST capture/assignment scan) so every backend agrees.
                let scan = witchy_syntax::lambda_scan::scan_lambda(params, body);
                let pure_capture_boundary = self.current_pure || qualifiers.pure;
                let outer = scan.assigns_outer();
                if !outer.is_empty() {
                    return terr(format!(
                        "a closure cannot assign to the captured variable `{}` (captures are by value, so the write would be lost) — return the new value or use a `var` parameter instead",
                        outer.join("`, `")
                    ));
                }
                let captures = scan.captures();
                let mut copied_captures = Vec::new();
                for capture in captures {
                    let Some(captured_ty) = self.lookup(&capture) else { continue };
                    if self.is_consumed_binding(&capture) {
                        return terr(format!(
                            "closure capture `{capture}` uses a binding after it was moved"
                        ));
                    }
                    if self.ty_carries_once_callable(&captured_ty) {
                        return terr(format!(
                            "closure environment captures affine once-callable `{capture}`; pass it explicitly to an `own` parameter instead"
                        ));
                    }
                    if self.is_must_binding(&capture) {
                        return terr(format!(
                            "closure capture `{capture}` carries a must-consume obligation; an ordinary callable cannot hide it or enter a replayable task"
                        ));
                    }
                    // RFC-0167 §views: "a view is a value-semantic snapshot handle,
                    // not a lexical loan ... may be returned, stored in an aggregate,
                    // transported through a generic, and captured by a callable
                    // without naming a lifetime", because the carrier retains the
                    // root while any copy of the view is live. So capture is not a
                    // loan escape, and the blanket refusal that stood here reasoned
                    // in the model that section retires.
                    //
                    // What a capture still must not do is carry a payload the ABI has
                    // no carrier for past the boundary that would refuse it, so the
                    // refusal is kept for exactly that. Row G of the acceptance ledger
                    // records the admitted carrier and the matching `&Float` refusal.
                    if self
                        .explicit_reference_bindings
                        .iter()
                        .rev()
                        .any(|bindings| bindings.contains(&capture))
                        && !matches!(
                            self.resolve(&captured_ty),
                            Ty::View(ref payload) if self.admitted_view_capture(payload)
                        )
                    {
                        return terr(format!(
                            "closure capture `{capture}` carries a view whose payload has no admitted carrier; callable capture requires a checked scalar, String, List, aggregate, or recursive slice carrier"
                        ));
                    }
                    if let Some(borrowed) = self.borrowed_nominal_name(&captured_ty) {
                        return terr(format!(
                            "closure capture `{capture}` carries borrowed nominal type \
                             `{borrowed}`, but RFC-0112 stage 1 cannot prove this closure \
                             non-escaping; wait for projection-aware closure loan facts"
                        ));
                    }
                    if pure_capture_boundary {
                        self.reject_pure_capture(&capture, &captured_ty)?;
                    }
                    if !self.ty_carries_must_consume(&captured_ty) {
                        let mutable = self.is_mutable(&capture).unwrap_or(false);
                        let frozen = self.is_frozen_binding(&capture);
                        copied_captures.push((capture, captured_ty, mutable, frozen));
                    }
                }
                let saved_pure = std::mem::replace(&mut self.current_pure, qualifiers.pure);
                let saved_pure_var_parameters =
                    std::mem::take(&mut self.pure_var_parameters);
                self.push();
                // Value captures are snapshots, not the enclosing bindings.
                // An `own` call in the body consumes that invocation's
                // private binding. Known obligation-bearing capture types
                // are left to the final checked closure witness; unresolved
                // captures are rechecked below after body inference.
                for (capture, ty, mutable, frozen) in &copied_captures {
                    // These analysis-only bindings do not allocate storage in
                    // the enclosing lexical region, unlike a source `let`.
                    self.scopes.last_mut().unwrap()
                        .insert(capture.clone(), (ty.clone(), *mutable));
                    if *frozen {
                        self.authorize_frozen_binding(capture.clone());
                    }
                }
                let param_tys: Vec<Ty> = params
                    .iter()
                    .map(|p| match &p.ty {
                        Some(t) => self.to_ty(t),
                        None => self.fresh(),
                    })
                    .collect();
                let eta_contract = self.linker_eta_opt_contract(params, body);
                for (index, (p, ty)) in params.iter().zip(&param_tys).enumerate() {
                    self.define(
                        p.name.clone(),
                        ty.clone(),
                        p.convention.binds_mutable(),
                    );
                    if let Some(contract) = eta_contract
                        .as_ref()
                        .and_then(Option::as_ref)
                        .and_then(|contract| contract.params.get(index))
                        .and_then(|contract| contract.as_deref())
                        .cloned()
                        .or_else(|| {
                            (eta_contract.is_none() && self.current_opt_callable)
                                .then(|| p.ty.as_ref().and_then(OptCallableContract::from_function_type))
                                .flatten()
                        })
                    {
                        self.remember_opt_callable_provenance(p.name.clone(), contract);
                    }
                    if qualifiers.pure && p.convention == Convention::Var {
                        let binding = self
                            .must_binding_key(&p.name)
                            .expect("a just-defined lambda parameter has a binding identity");
                        self.pure_var_parameters.insert(binding);
                    }
                    if self.ty_carries_must_consume(ty) {
                        match p.convention {
                            Convention::Own => self.mark_must_consume_binding(&p.name, ty),
                            Convention::Borrow | Convention::Var => {
                                self.mark_borrowed_must_binding(&p.name)
                            }
                            Convention::Let => {
                                return terr(format!(
                                    "lambda parameter `{}` receives must-consume `{ty}` by copying; use `own` to transfer the obligation or explicit `let` to borrow it",
                                    p.name,
                                ));
                            }
                        }
                    }
                    if self.ty_carries_once_callable(ty) {
                        match p.convention {
                            Convention::Var => {
                                return terr(format!(
                                    "lambda parameter `{}` cannot use `var` with once-callable `{ty}`; invocation could leave an empty write-back slot",
                                    p.name,
                                ));
                            }
                            Convention::Borrow => self.mark_borrowed_once_binding(&p.name),
                            Convention::Let | Convention::Own => {}
                        }
                    }
                    if p.ty.as_ref().is_some_and(type_is_explicit_reference) {
                        self.explicit_reference_bindings
                            .last_mut()
                            .expect("reference bindings track type scopes")
                            .insert(p.name.clone());
                    }
                    if p.ty.as_ref().is_some_and(is_frozen_value_type) {
                        self.authorize_frozen_binding(p.name.clone());
                    }
                    if p.ty.as_ref().is_some_and(is_unique_value_type) {
                        self.authorize_unique_value_binding(p.name.clone());
                    }
                    if p
                        .ty
                        .as_ref()
                        .is_some_and(|ty| self.declared_type_carries_unique_storage(ty))
                        || self.ty_carries_unique_storage(ty)
                    {
                        self.authorize_unique_storage_binding(p.name.clone());
                    }
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
                // Body inference can refine a generic capture. Recheck the
                // resolved storage before granting a reusable callable: even
                // a root continuation is wrapped in a replayable Task recipe.
                for (capture, captured_ty, _, _) in &copied_captures {
                    if self.ty_carries_must_consume(captured_ty) {
                        return terr(format!(
                            "closure capture `{capture}` carries a must-consume obligation; an ordinary callable cannot hide it or enter a replayable task"
                        ));
                    }
                    if self.ty_carries_once_callable(captured_ty) {
                        return terr(format!(
                            "closure environment captures affine once-callable `{capture}`; pass it explicitly to an `own` parameter instead"
                        ));
                    }
                    self.reject_borrowed_nominal_erasure(
                        captured_ty,
                        &format!("closure capture `{capture}`"),
                    )?;
                }
                self.current_ret = saved_ret;
                self.reject_live_must_in_current_scope()?;
                self.pop();
                self.current_pure = saved_pure;
                self.pure_var_parameters = saved_pure_var_parameters;
                let conventions = params.iter().map(|p| p.convention).collect();
                let function_ty = Ty::Fn(
                    param_tys.into(),
                    Box::new(lambda_ret).into(),
                    conventions,
                    vec![false; params.len()].into(),
                    *qualifiers,
                );
                if ret.is_none()
                    && let Ty::Fn(_, result, _, _, _) = &function_ty
                {
                    self.reject_borrowed_nominal_erasure(
                        result,
                        "inferred closure result",
                    )?;
                }
                self.reject_externref_cap_aggregate_ty(&function_ty, "closure value")?;
                Ok(function_ty)
            }
            Expr::Call { name, args } => self.infer_call(name, args, None),
            Expr::Apply { func, args } => {
                // The callee is an arbitrary expression of function type; unify
                // it with `fn(argtys) -> r` and yield `r`.
                let unique_contract = self.callable_unique_contract_for_expr(func);
                let opt_callable = self.expression_opt_callable_contract(func).is_some();
                let fty = self.infer(func)?;
                if let Ty::Fn(param_tys, ret, conventions, reference_params, qualifiers) =
                    self.resolve(&fty)
                {
                    if self.current_pure && !qualifiers.pure {
                        return terr(
                            "pure callable cannot apply an ordinary function value; the callee expression must have a `pure fn` type",
                        );
                    }
                    self.reject_externref_cap_aggregate_ty(&fty, "function value")?;
                    if param_tys.len() != args.len() {
                        return terr(format!(
                            "function application expects {} argument(s) but got {}",
                            param_tys.len(),
                            args.len()
                        ));
                    }
                    for (index, (arg, pty)) in args.iter().zip(param_tys.iter()).enumerate() {
                        if opt_callable && matches!(self.resolve(pty), Ty::Fn(..)) {
                            self.require_opt_callable_provenance(arg, "function value", index)?;
                        } else if !opt_callable {
                            self.reject_opt_callable_erasure(
                                arg,
                                &format!("argument {} to function value", index + 1),
                            )?;
                        }
                        let at = self.infer_expected(arg, pty)?;
                        self.reject_borrowed_nominal_runtime_ty(
                            &at,
                            "function-value application",
                        )?;
                        self.reject_var_directed_coercion(
                            "function value",
                            index,
                            conventions.get(index),
                            pty,
                            &at,
                        )?;
                        let unique_requested = self.opt_mode
                            && unique_contract
                                .as_ref()
                                .and_then(|contract| contract.unique_params.get(index))
                                .copied()
                                .unwrap_or(false);
                        if unique_requested {
                            if !self.expression_carries_unique_storage(arg, pty) {
                                return terr(format!(
                                    "argument {} to function value cannot repair ordinary storage into a `unique` parameter in `mode opt`; pass an existing unique owner",
                                    index + 1
                                ));
                            }
                            self.reject_opt_unique_call_after_view(
                                arg, &format!("argument {} to function value", index + 1),
                            )?;
                        }
                        self.coerce_arg(pty, &at).map_err(|e| TypeError {
                            message: format!("in function application: {}", e.message),
                        })?;
                    }
                    self.reject_externref_cap_aggregate_ty(&fty, "function value")?;
                    self.enforce_function_value_conventions(
                        "function value",
                        args,
                        &param_tys,
                        &conventions,
                        &reference_params,
                        unique_contract.as_ref(),
                    )?;
                    if qualifiers.once {
                        self.consume_once_callee(func, &fty)?;
                    }
                    self.reject_borrowed_nominal_runtime_ty(
                        ret.as_ref(),
                        "function-value application result",
                    )?;
                    return Ok((*ret).clone());
                }
                if self.current_pure {
                    return terr(
                        "pure callable cannot apply a value without a statically declared `pure fn` contract",
                    );
                }
                let mut argtys = Vec::new();
                for arg in args {
                    let arg_ty = self.infer(arg)?;
                    self.reject_borrowed_nominal_runtime_ty(
                        &arg_ty,
                        "function-value application",
                    )?;
                    argtys.push(arg_ty);
                }
                let ret = self.fresh();
                self.unify(
                    &fty,
                    &Ty::Fn(
                        argtys.into(),
                        Box::new(ret.clone()).into(),
                        vec![Convention::Let; args.len()].into(),
                        vec![false; args.len()].into(),
                        CallableQualifiers::ORDINARY,
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
                    if !self.is_direct_borrowed_nominal(&result) {
                        self.reject_borrowed_nominal_runtime_ty(
                            &result,
                            &format!("constructor `{name}`"),
                        )?;
                    }
                    if fields.len() != args.len() {
                        return terr(format!(
                            "constructor `{name}` takes {} field(s) but got {}",
                            fields.len(),
                            args.len()
                        ));
                    }
                    for (arg, fty) in args.iter().zip(&fields) {
                        self.reject_opt_callable_erasure(
                            arg,
                            &format!("constructor `{name}`"),
                        )?;
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
                        self.transfer_unique_value(arg, fty, &format!("constructor `{name}`"))?;
                    }
                    if self.ty_carries_must_consume(&result) {
                        for argument in args {
                            self.reject_implicit_must_copy(
                                argument,
                                &format!("constructor `{name}`"),
                            )?;
                        }
                    }
                    if self.ty_carries_once_callable(&result) {
                        for (argument, field) in args.iter().zip(&fields) {
                            self.reject_implicit_once_copy(
                                argument,
                                field,
                                &format!("constructor `{name}`"),
                            )?;
                        }
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
                let t = if matches!(op, UnOp::Borrow)
                    && let Expr::Index { base, index } = expr.as_ref()
                {
                    let (_, result) = self.infer_index_parts(base, index)?;
                    self.finish_infer(expr, result)?
                } else {
                    self.infer(expr)?
                };
                match op {
                    // `move x` has the type of `x`, and consumes it: it's a
                    // use-site ownership transfer, so any later use of the moved
                    // variable is an error on every backend (matching native's
                    // actual move). `infer(expr)` above already rejected moving an
                    // already-consumed binding.
                    UnOp::Move => {
                        self.reject_borrowed_nominal_runtime_ty(&t, "unary `move`")?;
                        if let Some((root, projected)) = self.affine_place(expr) {
                            self.reject_own_of_borrowed_must(&root, "`move`")?;
                            if projected
                                && self
                                    .lookup(&root)
                                    .is_some_and(|root_ty| self.ty_carries_must_consume(&root_ty))
                            {
                                return terr(format!(
                                    "cannot partially move `{root}` because its aggregate carries a must-consume obligation; transfer the whole value to an `own` operation"
                                ));
                            }
                            if self.is_borrowed_once_binding(&root)
                                && self.ty_carries_once_callable(&t)
                            {
                                return terr(format!(
                                    "cannot move borrowed once-callable `{root}`; only its owner may transfer it"
                                ));
                            }
                            if matches!(expr.as_ref(), Expr::Var(_))
                                || self.ty_carries_once_callable(&t)
                            {
                                self.mark_consumed_binding(&root);
                                if !projected {
                                    self.consume_must_binding(&root);
                                }
                            }
                        }
                        Ok(t)
                    }
                    UnOp::Await if !self.region_locals.is_empty() => terr(
                        "cannot `await` inside `region:`: the async frame outlives the region".to_string(),
                    ),
                    UnOp::Await if self.current_pure => terr(
                        "pure callable cannot suspend with `await`; task scheduling is an observable effect",
                    ),
                    UnOp::Await => match self.resolve(&t) {
                        Ty::Named(name, arguments)
                            if (name == "Task".into() || name == "task.Task".into()) && arguments.len() == 1 =>
                        {
                            // Starting the exact task operand discharges that
                            // token's must-start obligation. Every other live
                            // obligation would be abandoned if execution is
                            // cancelled at this suspension point.
                            let started = self.affine_place(expr)
                                .and_then(|(root, projected)| (!projected).then_some(root));
                            if let Some((_, name)) = self.must_live.iter()
                                .filter(|(_, name)| started.as_deref() != Some(name.as_str()))
                                .min()
                            {
                                return terr(format!(
                                    "cannot `await` while must-consume value `{name}` is live: cancellation at this suspension has no checked disposition path; consume or transfer it before awaiting"
                                ));
                            }
                            let replayable = ty_to_ast(&t).is_some_and(|source| {
                                witchy_canonical_source::storage::replay_capture_eligibility(
                                    &source, &self.canonical_definitions,
                                ).is_ok()
                            }) && !self.ty_carries_unique_storage(&t)
                                && !self.ty_carries_must_consume(&t)
                                && !self.ty_carries_once_callable(&t);
                            if let Some((root, projected)) = self.affine_place(expr) {
                                if projected && !replayable {
                                    return terr(format!(
                                        "cannot await a projected affine task from `{root}`; transfer the whole Task value before starting it"
                                    ));
                                }
                                self.reject_own_of_borrowed_must(&root, "`await`")?;
                                if self.is_consumed_binding(&root) {
                                    return terr(format!(
                                        "cannot await task `{root}` after it was already started or transferred"
                                    ));
                                }
                                if !replayable {
                                    self.mark_consumed_binding(&root);
                                    self.consume_must_binding(&root);
                                }
                            }
                            Ok(arguments[0].clone())
                        }
                        actual => terr(format!(
                            "`await` requires `Task(T)`, but this expression has type `{actual}`"
                        )),
                    },
                    // RFC-0167 views are rooted immutable carrier values in
                    // both modes. Their root/projection authentication is
                    // published by `CheckedViewFacts`, not LoanFacts.
                    UnOp::Borrow => {
                        self.admit_view_payload(&t, "immutable view formation")?;
                        if self.opt_mode
                            && let Some(root) = self.affine_place_root(expr)
                        {
                            self.opt_viewed_roots.insert(root);
                        }
                        Ok(Ty::View(Box::new(t).into()))
                    }
                    UnOp::Deref => match self.resolve(&t) {
                        Ty::View(payload) => Ok(payload.as_ref().clone()),
                        actual => terr(format!(
                            "immutable read dereference requires a rooted view, but this expression has type `{actual}`"
                        )),
                    },
                    UnOp::BorrowMut if self.opt_mode => {
                        if self.ty_carries_once_callable(&t) {
                            return terr(
                                "cannot create an explicit exclusive reference to affine once-callable storage; referent-consuming invocation through reference handles is not supported",
                            );
                        }
                        if self.exclusive_borrow_targets_frozen_storage(expr) {
                            return terr(
                                "cannot create an exclusive reference to `frozen` storage; frozen values permit shared reads only",
                            );
                        }
                        Ok(t)
                    }
                    UnOp::BorrowMut => terr(
                        "explicit references are available only in `mode opt` files; normal Witchy uses owned values and does not require lifetime annotations",
                    ),
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
                if !self.is_authorized_borrowed_shell_value(base, &bt)
                    && !self.is_nested_borrowed_nominal_projection(base, &bt)
                {
                    self.reject_borrowed_nominal_runtime_ty(
                        &bt,
                        &format!("field projection `.{field}`"),
                    )?;
                }
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
                if self.sealed_types.contains(tyname.as_ref()) {
                    return terr(format!(
                        "`{tyname}` is a sealed capability — its fields are private; \
                         destructure it with `match` inside its own module"
                    ));
                }
                let Some((params, fields)) = self.record_fields.get(tyname.as_ref()).cloned() else {
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
                self.reject_implicit_once_copy(base, &bt, "record spread/update")?;
                // The base is consumed to build the replacement, but that
                // happens AFTER the field expressions are evaluated. Consuming
                // it here refused `c.field = c.field + 1` on an owned `unique`
                // record — the place-assignment desugar is
                // `c = C(field: c.field + 1, ..c)`, so the field expression
                // read a binding this had already marked transferred.
                let borrowed_shell = self.is_authorized_borrowed_shell_update(base, &bt);
                if !borrowed_shell {
                    self.reject_borrowed_nominal_runtime_ty(&bt, "record spread/update")?;
                }
                let resolved = self.resolve(&bt);
                let (base_tyname, base_args) = match &resolved {
                    Ty::Named(n, a) => (n.clone(), a.clone()),
                    other => {
                        return terr(format!("`update` requires a record, found `{other}`"))
                    }
                };
                let tyname = name.clone().unwrap_or_else(|| base_tyname.to_string());
                if *tyname != *base_tyname {
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
                    if borrowed_shell {
                        let relation_field = self
                            .borrowed_nominal_relation_fields
                            .get(&tyname)
                            .is_some_and(|borrowed| borrowed.contains(fname));
                        if !relation_field && !is_scalar_ty(&self.resolve(&expected)) {
                            return terr(format!(
                                "`update` of field `{fname}` on borrowed shell `{tyname}` is \
                                 limited to owned scalar fields or declared borrowed-relation \
                                 fields"
                            ));
                        }
                    }
                    let vt = self.infer_expected(vexpr, &expected)?;
                    self.transfer_unique_value(
                        vexpr,
                        &expected,
                        &format!("update of field `{fname}`"),
                    )?;
                    self.reject_implicit_once_copy(
                        vexpr,
                        &expected,
                        &format!("update of field `{fname}`"),
                    )?;
                    if !self.existential_coercion(&expected, &vt)? {
                        self.unify(&expected, &vt).map_err(|e| TypeError {
                            message: format!("`update` of field `{fname}`: {}", e.message),
                        })?;
                    }
                }
                // Now the fields are evaluated, the base is consumed. A field
                // expression that itself consumed the base still fails here,
                // which is the affine rule in written evaluation order.
                self.transfer_unique_value(base, &bt, "record spread/update")?;
                Ok(Ty::Named(tyname.into(), base_args))
            }
            Expr::Try(inner) => {
                let it = self.infer(inner)?;
                let resolved = self.resolve(&it);
                let Some(ret) = self.current_ret.clone() else {
                    return terr("`?` can only be used inside a function returning Result or Option");
                };
                let ret = self.resolve(&ret);
                let value_ty = match &resolved {
                    Ty::Named(n, args) if **n == *"Result" && args.len() == 2 => {
                        match &ret {
                            Ty::Named(rn, rargs) if **rn == *"Result" && rargs.len() == 2 => {
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
                    Ty::Named(n, args) if **n == *"Option" && args.len() == 1 => {
                        let r = self.fresh();
                        let expected_ret = Ty::Named("Option".into(), vec![r].into());
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
                // `?` has an implicit error-return edge after its operand has
                // been evaluated. Check obligations at exactly that point: an
                // `own` operand call has already discharged its argument, while
                // every unrelated live resource would be abandoned on Err/None.
                self.reject_all_live_must_before_return()?;
                if self.ty_carries_once_callable(&value_ty) {
                    self.reject_implicit_once_copy(inner, &resolved, "`?` extraction")?;
                }
                Ok(value_ty)
            }
            Expr::As { expr: payload, ty } => {
                self.reject_borrowed_nominal_container_type(ty, "type ascription")?;
                self.reject_explicit_once_reference_type(ty, "type ascription")?;
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
                    self.reject_borrowed_nominal_erasure(
                        &resolved_src,
                        &format!("erasure to `dyn {}`", existential_bare(dyn_name)),
                    )?;
                    if self.ty_carries_once_callable(&resolved_src) {
                        return terr(format!(
                            "`as dyn {}` cannot erase affine once-callable payload `{resolved_src}`; keep the concrete owning type or consume it through an explicit `own` operation",
                            existential_bare(dyn_name),
                        ));
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
                let must_before = self.must_live.clone();
                let once_before = self.once_invoked.clone();
                let then_completes = block_can_complete(then_block);
                let tt = self.infer_block(then_block)?;
                let consumed_then = std::mem::replace(&mut self.consumed, before.clone());
                let must_then = std::mem::replace(&mut self.must_live, must_before.clone());
                let once_then =
                    std::mem::replace(&mut self.once_invoked, once_before.clone());
                let (else_completes, result) = match else_block {
                    Some(eb) => {
                        let et = self.infer_block(eb)?;
                        let joined = self.branch_lub(&tt, &et).map_err(|e| TypeError {
                            message: format!("`if` branches disagree: {}", e.message),
                        })?;
                        (block_can_complete(eb), joined)
                    }
                    None => {
                        self.unify(&tt, &Ty::Unit)?;
                        (true, self.resolve(&tt))
                    }
                };
                let branches = [
                    (then_completes, consumed_then, must_then, once_then),
                    (
                        else_completes,
                        self.consumed.clone(),
                        self.must_live.clone(),
                        self.once_invoked.clone(),
                    ),
                ];
                (self.consumed, self.must_live, self.once_invoked) =
                    join_reachable_binding_facts(
                        &before,
                        &must_before,
                        &once_before,
                        &branches,
                    );
                Ok(result)
            }
            Expr::Block(b) => self.infer_block(b),
            Expr::While { cond, body } => {
                let consumed_before = self.consumed.clone();
                let ct = self.infer(cond)?;
                self.unify(&Ty::Bool, &ct).map_err(|e| TypeError {
                    message: format!("`while` condition: {}", e.message),
                })?;
                let must_before = self.must_live.clone();
                self.infer_block(body)?;
                self.reject_affine_loop_backedge(&consumed_before, "a `while` loop")?;
                self.must_live = &must_before | &self.must_live;
                Ok(Ty::Unit)
            }
            Expr::For { var, iter, body } => {
                let it = self.infer(iter)?;
                // A `&[T]` view iterates its elements by value, as `xs[i]` reads them.
                let slice_element = match self.resolve(&it) {
                    Ty::View(payload) => match payload.as_ref() {
                        Ty::Named(name, args) if name.as_ref() == "Slice" && args.len() == 1 => Some(args[0].clone()),
                        _ => None,
                    },
                    _ => None,
                };
                let elem = match slice_element {
                    Some(element) => element,
                    None => {
                        let elem = self.fresh();
                        self.unify(&Ty::List(Box::new(elem.clone()).into()), &it).map_err(|e| TypeError {
                            message: format!("`for` expects a List or a `&[T]` view to iterate: {}", e.message),
                        })?;
                        elem
                    }
                };
                self.reject_implicit_once_copy(iter, &it, "`for` iteration")?;
                let consumed_before_body = self.consumed.clone();
                self.push();
                let borrowed_element = self.is_direct_borrowed_nominal(&elem);
                self.define(var.clone(), elem.clone(), false);
                if let Some(contract) = self.container_opt_contract_for_expr(iter).flatten() {
                    self.remember_opt_callable_provenance(var.clone(), contract);
                }
                // A borrowed-list loop binder is a read-only borrowed shell.
                // Its owner roots remain live through the enclosing iterator
                // expression; authorizing the binder admits only checked field
                // projections, never list mutation or an owning escape.
                if borrowed_element {
                    self.authorize_borrowed_shell_binding(var.clone());
                }
                self.infer_block(body)?;
                self.pop();
                self.reject_affine_loop_backedge(&consumed_before_body, "a `for` loop")?;
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
                    Ty::Named(ref n, ref args) if **n == *"Option" && args.len() == 1 => {
                        let t = args[0].clone();
                        self.unify(&t, &rt).map_err(|e| TypeError {
                            message: format!(
                                "`??` fallback: the right side must have the Option's \
                                 payload type: {}",
                                e.message
                            ),
                        })?;
                        if self.ty_carries_once_callable(&t) {
                            self.reject_implicit_once_copy(lhs, &lt, "`??` left operand")?;
                            self.reject_implicit_once_copy(rhs, &t, "`??` fallback")?;
                        }
                        Ok(t)
                    }
                    Ty::Named(ref n, ref args) if **n == *"Result" && args.len() == 2 => {
                        let t = args[0].clone();
                        self.unify(&t, &rt).map_err(|e| TypeError {
                            message: format!(
                                "`??` fallback: the right side must have the Result's \
                                 Ok type: {}",
                                e.message
                            ),
                        })?;
                        if self.ty_carries_once_callable(&t) {
                            self.reject_implicit_once_copy(lhs, &lt, "`??` left operand")?;
                            self.reject_implicit_once_copy(rhs, &t, "`??` fallback")?;
                        }
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
        self.transfer_once_match_scrutinee(scrutinee, &st)?;
        self.transfer_unique_value(scrutinee, &st, "match scrutinee")?;
        if self.ty_carries_must_consume(&st) {
            self.reject_implicit_must_copy(scrutinee, "match scrutinee")?;
            if self.expression_introduces_must_obligation(scrutinee, &st) {
                let consuming_aggregate_match = ty_to_ast(&self.resolve(&st)).is_some_and(|source| {
                    witchy_canonical_source::storage::consuming_aggregate_match_shape(
                        &source, arms, &self.canonical_definitions,
                    ).is_some()
                }) && matches!(scrutinee,
                    Expr::Call { .. } | Expr::Ctor { .. } | Expr::AnonCtor { .. }
                        | Expr::Unary { op: UnOp::Move, .. });
                if !consuming_aggregate_match {
                    return terr(
                        "matching would erase a must-consume obligation; transfer the value to an `own` operation before inspecting it",
                    );
                }
            }
        }
        let mut result = expected.cloned();
        let before = self.consumed.clone();
        let must_before = self.must_live.clone();
        let once_before = self.once_invoked.clone();
        let mut branch_facts = Vec::with_capacity(arms.len());
        for arm in arms {
            self.consumed = before.clone();
            self.must_live = must_before.clone();
            self.once_invoked = once_before.clone();
            self.push();
            if let Some(dup) = pattern_dup_binding(&arm.pattern) {
                return terr(format!(
                    "pattern binds `{dup}` more than once — each binding in a pattern \
                     must have a distinct name (witchy has no equality patterns)"
                ));
            }
            self.check_pattern(&arm.pattern, &st)?;
            if let Expr::Var(binding) = scrutinee
                && let Some(declared) = self.unique_binding_type(binding)
            {
                self.authorize_unique_pattern_storage(&arm.pattern, &declared);
            }
            if let Some(guard) = &arm.guard {
                let guard_before = self.consumed.clone();
                let gt = self.infer(guard)?;
                self.unify(&Ty::Bool, &gt)
                    .map_err(|e| TypeError { message: format!("match guard: {}", e.message) })?;
                self.reject_once_match_guard_consumption(&guard_before)?;
            }
            let bt = match expected {
                Some(expected) => self.infer_expected(&arm.body, expected)?,
                None => self.infer(&arm.body)?,
            };
            self.reject_implicit_once_copy(&arm.body, &bt, "match arm result")?;
            if expected.is_some() {
                self.coerce_arg(result.as_ref().expect("expected match type"), &bt).map_err(|e| TypeError {
                    message: format!("match arm disagrees with the expected type: {}", e.message),
                })?;
            } else {
                result = Some(match result {
                    Some(ref current) => self.branch_lub(current, &bt).map_err(|e| TypeError {
                        message: format!("match arms produce different types: {}", e.message),
                    })?,
                    None => bt,
                });
            }
            self.reject_live_must_in_current_scope()?;
            self.pop();
            branch_facts.push((
                expr_can_complete(&arm.body),
                self.consumed.clone(),
                self.must_live.clone(),
                self.once_invoked.clone(),
            ));
        }
        (self.consumed, self.must_live, self.once_invoked) =
            join_reachable_binding_facts(
                &before,
                &must_before,
                &once_before,
                &branch_facts,
            );
        // Coverage analysis (exhaustiveness + unreachability) reasons per
        // alternative, so flatten a top-level `Pattern::Or` arm into one synthetic
        // arm per alternative (sharing the guard) — exactly what the old
        // parse-time or-expansion produced, so these checks are unchanged.
        let flat = flatten_or_arms(arms);
        self.check_unreachable(&flat)?;
        self.check_exhaustive(&st, &flat)?;
        Ok(result.unwrap_or(Ty::Unit))
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
                    .and_then(|a| self.adt_variants.get(a.as_ref()))
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
        if !matches!(pat, Pattern::Wildcard | Pattern::Var(_)) {
            self.reject_borrowed_nominal_runtime_ty(expected, "pattern destructuring")?;
        }
        match pat {
            Pattern::Wildcard if self.ty_carries_must_consume(expected) => terr(
                "a wildcard pattern would discard a must-consume value; bind and consume it",
            ),
            Pattern::Wildcard => Ok(()),
            Pattern::Var(name) => {
                self.define(name.clone(), expected.clone(), false);
                self.mark_must_consume_binding(name, expected);
                Ok(())
            }
            Pattern::Int(_) => self.unify(expected, &Ty::Int),
            Pattern::Str(_) => self.unify(expected, &Ty::String),
            Pattern::Bool(_) => self.unify(expected, &Ty::Bool),
            Pattern::Tuple(pats) => {
                let elem_tys: Vec<Ty> = (0..pats.len()).map(|_| self.fresh()).collect();
                self.unify(expected, &Ty::Tuple(elem_tys.clone().into()))?;
                for (p, t) in pats.iter().zip(elem_tys) {
                    self.check_pattern(p, &t)?;
                }
                Ok(())
            }
            Pattern::List { elems, rest } => {
                let elem = self.fresh();
                self.unify(expected, &Ty::List(Box::new(elem.clone()).into()))?;
                for p in elems {
                    self.check_pattern(p, &elem)?;
                }
                if let Some(Some(name)) = rest {
                    let rest_ty = Ty::List(Box::new(elem).into());
                    self.define(name.clone(), rest_ty.clone(), false);
                    self.mark_must_consume_binding(name, &rest_ty);
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
                    let source_fields = self.unique_ctor_field_types.get(name).cloned();
                    for (index, (p, fty)) in args.iter().zip(&fields).enumerate() {
                        self.check_pattern(p, fty)?;
                        if let Some(source_field) = source_fields
                            .as_ref()
                            .and_then(|source_fields| source_fields.get(index))
                        {
                            self.authorize_unique_pattern_storage(p, source_field);
                        }
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
        let Some(variants) = self.adt_variants.get(adt.as_ref()) else {
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
                Some(vec![ColCtor { key: TUPLE_CTOR.into(), args: comps.to_vec() }])
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
                let variants = self.adt_variants.get(adt.as_ref())?;
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
            let Some(variants) = self.adt_variants.get(adt.as_ref()) else {
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
            "PublicState" => &["PublicState"],
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
        let source_callable = witchy_syntax::suspension::source_callable_name(func);
        let prev_compiler_syntax_allowed = self.compiler_syntax_allowed;
        if func.comptime_only {
            self.compiler_syntax_allowed = true;
        }
        let (params, ret) = self.fn_sigs.get(&func.name).cloned().unwrap();
        self.scopes = vec![HashMap::new()];
        self.borrowed_shell_bindings = vec![HashSet::new()];
        self.explicit_reference_bindings = vec![HashSet::new()];
        self.frozen_bindings = vec![HashSet::new()];
        self.unique_value_bindings = vec![HashSet::new()];
        self.unique_storage_bindings = vec![HashSet::new()];
        self.opt_viewed_roots.clear();
        self.inferred_owner_bindings = vec![HashSet::new()];
        self.unique_binding_types = vec![HashMap::new()];
        self.callable_unique_contracts = vec![HashMap::new()];
        self.callable_opt_provenance = vec![HashMap::new()];
        self.container_opt_provenance = vec![HashMap::new()];
        self.consumed.clear();
        self.once_invoked.clear();
        self.must_live.clear();
        self.must_borrowed.clear();
        self.once_borrowed.clear();
        self.current_storage_obligations.clear();
        let body_ret =
            self.async_body_returns
                .get(&func.name)
                .cloned()
                .unwrap_or(ret.clone());
        self.current_ret = Some(body_ret.clone());
        self.current_pure = func.pure;
        let saved_opt_callable = std::mem::replace(
            &mut self.current_opt_callable,
            self.fn_opt_provenance.contains_key(&func.name),
        );
        self.pure_var_parameters.clear();
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
                &format!("parameter `{}` of `{}`", param.name, diagnostic_callable_name(&source_callable)),
            )?;
        }
        self.reject_runtime_compiler_syntax_ty(
            &ret,
            &format!("return type of `{}`", diagnostic_callable_name(&source_callable)),
        )?;
        let suspension_frame = func.attributes.iter().any(|attribute| {
            attribute == witchy_syntax::suspension::FRAME_FUNCTION_ATTRIBUTE
        });
        for (param, ty) in func.params.iter().zip(&params) {
            let lazy_recipe = func.is_async && func.name != "main";
            if lazy_recipe && param.convention == Convention::Borrow {
                return terr(format!(
                    "async function `{}` cannot capture call-scoped `let` parameter `{}` in a lazy Task recipe; pass an ordinary owned value or materialize one explicitly",
                    diagnostic_callable_name(&source_callable),
                    param.name,
                ));
            }
            // The source async entry captures this value once in `task.lazy`;
            // each execution clones its invocation state from that snapshot.
            // Check instantiated stored fields, not a second name-based list
            // of unique/must/once shapes. `main` runs directly, with no recipe.
            if lazy_recipe {
                if param.convention == Convention::Var {
                    return terr(format!(
                        "async function `{}` cannot defer `var` write-back for parameter `{}` in a lazy Task recipe",
                        diagnostic_callable_name(&source_callable), param.name,
                    ));
                }
                let source_ty = param.ty.clone().or_else(|| ty_to_ast(ty)).ok_or_else(|| TypeError {
                    message: format!("async parameter `{}` has no checked replay capture type", param.name),
                })?;
                self.admit_replay_capture_source(
                    &source_ty,
                    ty,
                    &format!(
                        "async function `{}` cannot capture parameter `{}` into its replayable Task recipe",
                        diagnostic_callable_name(&source_callable), param.name,
                    ),
                )?;
            }
            // `var` replaces the parameter handle and `own` transfers it; neither
            // is a field/deep mutation of a frozen payload.
            self.define(
                param.name.clone(),
                ty.clone(),
                param.convention.binds_mutable(),
            );
            if self.current_opt_callable
                && let Some(contract) = param.ty.as_ref().and_then(OptCallableContract::from_function_type)
            {
                // The declaration is the checked entry boundary: callers must
                // supply provenance before this local becomes callable.
                self.remember_opt_callable_provenance(param.name.clone(), contract);
            }
            if func.pure && param.convention == Convention::Var {
                let binding = self
                    .must_binding_key(&param.name)
                    .expect("a just-defined parameter has a binding identity");
                self.pure_var_parameters.insert(binding);
            }
            if self.ty_carries_must_consume(ty) {
                // Calling a source `async fn` constructs a lazy, replayable
                // `Task`; it does not execute the body or establish a checked
                // cancellation cleanup.  Moving a must-consume parameter into
                // that recipe would let the caller discard or replay the
                // obligation before the first suspension.  Compiler-generated
                // frame functions are checked separately below and retain the
                // one exact non-cancellable root-transfer exception.
                match param.convention {
                    // An `own` parameter is the declaration of a consuming
                    // operation. The caller's obligation is discharged at the
                    // call boundary; a must value returned by this function
                    // creates a fresh obligation for its caller.
                    Convention::Own if suspension_frame => {
                        self.mark_must_consume_binding(&param.name, ty);
                    }
                    Convention::Own => {}
                    Convention::Borrow | Convention::Var => {
                        self.mark_borrowed_must_binding(&param.name);
                    }
                    Convention::Let => {
                        return terr(format!(
                            "parameter `{}` of `{}` receives must-consume `{ty}` by copying; use `own` to transfer the obligation or explicit `let` to borrow it",
                            param.name,
                            diagnostic_callable_name(&source_callable),
                        ));
                    }
                }
            }
            if self.ty_carries_once_callable(ty) {
                match param.convention {
                    Convention::Var => {
                        return terr(format!(
                            "parameter `{}` of `{}` cannot use `var` with once-callable `{ty}`; invocation could leave an empty write-back slot",
                            param.name,
                            diagnostic_callable_name(&source_callable),
                        ));
                    }
                    Convention::Borrow => self.mark_borrowed_once_binding(&param.name),
                    Convention::Let | Convention::Own => {}
                }
            }
            if param.ty.as_ref().is_some_and(type_is_explicit_reference) {
                self.explicit_reference_bindings
                    .last_mut()
                    .expect("reference bindings track type scopes")
                    .insert(param.name.clone());
            }
            if param.ty.as_ref().is_some_and(is_frozen_value_type) {
                self.authorize_frozen_binding(param.name.clone());
            }
            if param.ty.as_ref().is_some_and(is_unique_value_type) {
                self.authorize_unique_value_binding(param.name.clone());
            }
            if param
                .ty
                .as_ref()
                .is_some_and(|ty| self.declared_type_carries_unique_storage(ty))
                || self.ty_carries_unique_storage(ty)
            {
                self.authorize_unique_storage_binding(param.name.clone());
                if let Some(declared) = &param.ty {
                    self.remember_unique_binding_type(param.name.clone(), declared.clone());
                }
            }
        }
        let body = if func.ret.is_some() {
            self.infer_block_tail_expected(&func.body, &body_ret).map_err(|e| {
                type_mismatch_context(
                    || format!("function `{}` body", diagnostic_callable_name(&source_callable)),
                    e,
                )
            })?
        } else {
            self.infer_block(&func.body)?
        };
        // An opt declaration that returns a callable publishes a recursive
        // declaration contract.  Its completing tail must therefore carry the
        // checked origin too; a normal imported function cannot be relabelled
        // merely by returning it from an opt body.
        if self.current_opt_callable
            && func.ret.as_ref().and_then(OptCallableContract::from_function_type).is_some()
            && self.block_opt_callable_contract(&func.body).is_none()
        {
            return terr(format!(
                "opt function `{}` returns a callable without opt-compatible provenance",
                diagnostic_callable_name(&source_callable),
            ));
        }
        // A broader capability may be returned where a narrower one is declared
        // (`-> Net[Connect]` returning a full `Net`), mirroring call-argument
        // narrowing; `coerce_arg` falls back to unification for everything else.
        self.coerce_arg(&body_ret, &body).map_err(|e| TypeError {
            message: format!(
                "function `{}` body: {}",
                diagnostic_callable_name(&source_callable),
                e.message
            ),
        })?;
        self.reject_all_live_must_before_return()?;
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
        self.fn_storage_obligations
            .insert(func.name.clone(), self.current_storage_obligations.clone());
        self.compiler_syntax_allowed = prev_compiler_syntax_allowed;
        self.current_opt_callable = saved_opt_callable;
        Ok(())
    }

    fn check_public_return_annotations(&mut self, module: &Module) -> Result<(), TypeError> {
        // Check only after every body has contributed its constraints. A public
        // unit wrapper may call a later declaration whose fresh result variable
        // has not resolved while the wrapper itself is being checked.
        for item in &module.items {
            let Item::Function(function) = item else { continue };
            if !function.public || function.ret.is_some() {
                continue;
            }
            let inferred = if function.is_async {
                self.async_body_returns.get(&function.name).cloned()
            } else {
                self.fn_sigs.get(&function.name).map(|(_, ret)| ret.clone())
            }
            .expect("every checked function has a collected result signature");
            let inferred = self.resolve(&inferred);
            if inferred != Ty::Unit {
                self.cur_line = function.line;
                let source_callable = witchy_syntax::suspension::source_callable_name(function);
                return terr(format!(
                    "public function `{}` has inferred non-unit return type `{inferred}`; add an explicit return annotation `-> {inferred}`",
                    diagnostic_callable_name(&source_callable),
                ));
            }
        }
        Ok(())
    }

}

/// Type-check a whole module. Returns the first error found.
pub fn check(module: &Module) -> Result<(), TypeError> {
    let checked = check_with_compiler_syntax(module, false)?;
    // Canonical views are authenticated during ordinary type checking too.
    // `check` cannot retain pointer facts because it borrows the caller's AST,
    // but it must still reject a raw/non-place `&` rather than deferring that
    // safety boundary to a lowering consumer.
    if crate::views::module_contains_canonical_view(checked.module()) {
        crate::views::checked_facts(checked.module(), checked.table())?;
    }
    Ok(())
}

/// Validate complete semantics on the expanded source set before the linker
/// lowers generators, async functions, records, traits, or impls.
pub fn check_linked_source_semantics(
    source: &witchy_syntax::source_check::ResolvedSource,
) -> Result<(), witchy_syntax::linker::SourceLinkError> {
    check_linked_source_semantics_typed(source).map(|_| ())
}

/// Validate expanded linked-source semantics and return both typed projections
/// produced by that check. The source async projection retains exact `await`
/// and capture identities before CPS erases them; the runtime projection is
/// the executable input to later canonical preparation.
pub fn check_linked_source_semantics_typed(
    source: &witchy_syntax::source_check::ResolvedSource,
) -> Result<(TypedModule, Option<TypedModule>), witchy_syntax::linker::SourceLinkError> {
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
        check_unique_functions(module).map_err(|error| {
            witchy_syntax::linker::SourceLinkError::Source(source_error(
                module_name,
                module,
                error,
            ))
        })?;
        check_unique_declarations(module).map_err(|error| {
            witchy_syntax::linker::SourceLinkError::Source(source_error(
                module_name,
                module,
                error,
            ))
        })?;
        check_unique_parameters(module).map_err(|error| {
            witchy_syntax::linker::SourceLinkError::Source(source_error(
                module_name,
                module,
                error,
            ))
        })?;
        check_nominal_lifetime_declarations(module).map_err(|error| {
            witchy_syntax::linker::SourceLinkError::Source(
                witchy_syntax::source_check::SourceCheckError::new(error.message),
            )
        })?;
    }

    // Resolve-wide semantics run over one read-only aggregate so imported
    // canonical type, trait, callable, and method identities are visible to
    // every declaration and body before the production lowering sequence can
    // consume its source representation.
    let mut modules = source.modules().iter();
    let Some((_, first)) = modules.next() else {
        return Err(witchy_syntax::linker::SourceLinkError::Link(
            witchy_syntax::linker::LinkError {
                message: "resolved source set is empty".to_string(),
                location: None,
            },
        ));
    };
    let mut aggregate = first.clone();
    aggregate.imports.clear();
    aggregate.import_lines.clear();
    aggregate.from_imports.clear();
    aggregate.item_lines.clear();
    for (_, module) in modules {
        aggregate.items.extend(module.items.iter().cloned());
        aggregate
            .compiler_item_syntax
            .extend(module.compiler_item_syntax.iter().cloned());
        aggregate
            .compiler_expr_syntax
            .extend(module.compiler_expr_syntax.iter().cloned());
        aggregate
            .compiler_type_syntax
            .extend(module.compiler_type_syntax.iter().cloned());
        aggregate
            .compiler_pattern_syntax
            .extend(module.compiler_pattern_syntax.iter().cloned());
        aggregate
            .compiler_stmt_syntax
            .extend(module.compiler_stmt_syntax.iter().cloned());
        aggregate
            .compiler_block_syntax
            .extend(module.compiler_block_syntax.iter().cloned());
    }
    let source_error = |error: TypeError| {
        witchy_syntax::linker::SourceLinkError::Source(
            witchy_syntax::source_check::SourceCheckError::new(error.message),
        )
    };
    check_type_names(&aggregate).map_err(source_error)?;
    check_trait_names(&aggregate).map_err(source_error)?;
    check_existential_types(&aggregate.items).map_err(source_error)?;
    check_public_state_impls(&aggregate).map_err(source_error)?;
    let has_async = source.modules().iter().any(|(_, module)| module.items.iter().any(|item| match item {
        Item::Function(function) => function.is_async,
        Item::Impl(definition) => definition.methods.iter().any(|method| method.is_async),
        _ => false,
    }));
    let typed_async_source = if has_async {
        let projection = source.typed_async_projection()
            .map_err(witchy_syntax::linker::SourceLinkError::Link)?;
        Some(annotate_typed_async_projection(projection.module).map_err(source_error)?)
    } else {
        None
    };
    let body_projection = source
        .runtime_projection()
        .map_err(witchy_syntax::linker::SourceLinkError::Link)?;
    // Check only the standard library this program can reach (see
    // std_pruning; a failure anywhere in the production route after pruning
    // reruns the route unpruned, so a pruned error is never reported).
    let std_impls = source
        .modules()
        .iter()
        .filter(|(name, _)| witchy_syntax::linker::STD_MODULES.contains(&name.as_str()))
        .flat_map(|(_, module)| module.items.iter())
        .filter_map(|item| match item {
            Item::Impl(implementation) if implementation.trait_name.is_none() => Some((
                implementation.type_name.clone(),
                implementation.methods.iter().map(|method| method.name.clone()).collect(),
            )),
            _ => None,
        })
        .collect();
    if let Some(pruned) = crate::std_pruning::prune(&body_projection, &std_impls) {
        return check_with_compiler_syntax(&pruned, false)
            .map(|typed| (typed, typed_async_source)).map_err(source_error);
    }
    check_with_compiler_syntax(&body_projection, false)
        .map(|typed| (typed, typed_async_source)).map_err(source_error)
}

/// Type-check the isolated program used to execute a `comptime:` block.
///
/// This mode allows compiler syntax values such as `meta.ItemSyntax` to flow
/// through helper expressions while the ordinary public checker still rejects
/// them from runtime modules.
pub fn check_comptime(module: &Module) -> Result<(), TypeError> {
    check_with_compiler_syntax(module, true).map(|_| ())
}

fn check_with_compiler_syntax(
    module: &Module,
    compiler_syntax_allowed: bool,
) -> Result<TypedModule, TypeError> {
    crate::views::reject_const_view_formation(module)?;
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
    check_public_state_impls(&recs)?;
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
            let type_table = run_check_with_trait_methods(
                &lowered,
                true,
                &trait_method_names,
                &trait_supertraits,
                compiler_syntax_allowed,
            )?
            .unwrap_or_default();
            check_unique_root_owner_results(&lowered)?;
            // (RFC-0083) Static lifetime/loan check for borrowed views. Runs after
            // type checking (so a genuine type error is reported first) on the
            // lowered module (method calls are plain `Call`s and the borrow
            // signatures survive lowering as `Qualified(Borrow, _)`).
            typed_module_checked(lowered, type_table)
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
/// A region promotion classification minted by the checker for one exact
/// `Block`.  It is deliberately not reconstructible from a type alone:
/// qualifiers, loans, and lexical origin decide whether opt may pass a managed
/// value through without materializing a hidden copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegionPromotionQualifier {
    Ordinary,
    Frozen,
    Unique,
    ExplicitReference,
}

/// The checked owner root carried by an explicit-reference promotion.  This is
/// deliberately a direct carrier only: aggregate and projected reference
/// shapes stay rejected until the retained loan graph can mint their exact
/// projection witness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegionPromotionLoan {
    pub owner_root: String,
}

/// The checked source identity of a fixed inline packed layout.  The lowerer
/// must rederive this from the exact checked result and match it to the decoded
/// layout bundle; this data alone never authorizes a layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegionPackedLayoutDescriptor {
    pub record: String,
    pub fields: Vec<ast::Type>,
}

/// A single fresh must-consume obligation produced at a region tail.  `tail`
/// is an AST identity under the same `CanonicalCheckedSource` that owns the
/// enclosing promotion fact; canonical lowering verifies both it and the
/// constructor spelling before consuming this proof once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegionMustTransfer {
    pub tail: usize,
    pub constructor: String,
}

/// The checker-visible disposition state for a normal region promotion.  An
/// obligation cannot disappear into an ordinary snapshot: it is either the
/// one constructor identity below or an explicit canonical refusal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegionMustDisposition {
    NoObligation,
    FreshConstructor(RegionMustTransfer),
    Unproven,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegionPromotionFact {
    Scalar { result: ast::Type },
    /// A normal-mode owned promotion.  `unique`/`frozen` remain ordinary
    /// qualifier contracts and do not by themselves prohibit normal snapshot
    /// representation choice.  A must payload has an exact one-shot transfer
    /// identity rather than an unauthenticated `true` flag.
    NormalOwnedSnapshot { result: ast::Type, must_disposition: RegionMustDisposition },
    /// A fixed inline packed result.  Its descriptor is derived from the
    /// checked source definition and must match the canonical layout bundle.
    PackedInline { result: ast::Type, descriptor: RegionPackedLayoutDescriptor },
    OptParentPassThrough {
        result: ast::Type,
        owner_root: String,
        qualifier: RegionPromotionQualifier,
        reference_loan: Option<RegionPromotionLoan>,
    },
    OptNeedsCopyFreeProof { result: ast::Type },
}

impl RegionPromotionFact {
    pub fn result(&self) -> &ast::Type {
        match self {
            Self::Scalar { result }
            | Self::NormalOwnedSnapshot { result, .. }
            | Self::PackedInline { result, .. }
            | Self::OptParentPassThrough { result, .. }
            | Self::OptNeedsCopyFreeProof { result } => result,
        }
    }
}

#[derive(Debug, Default, PartialEq)]
pub struct TypeTable {
    types: HashMap<usize, Ty>,
    /// Expression identity produced by the checker branch for the
    /// compiler-known `.owned()` operation. Structural rewrites must rebuild
    /// this table before any authority or tail-manifest consumer uses it.
    compiler_owned_materializations: HashSet<usize>,
    /// Canonical view expressions retain their carrier type even while a
    /// generic payload variable remains unresolved. These are authenticated
    /// formation facts, not general permission for downstream consumers to
    /// treat arbitrary unresolved expressions as concrete runtime values.
    view_types: HashMap<usize, Ty>,
    /// Fully resolved lexical callee signatures at named call expressions.
    /// A source spelling alone never supplies a callable type.
    local_callees: HashMap<usize, Ty>,
    /// Checked expressions with no normal continuation. Unlike value types,
    /// this fact survives an unresolved result variable; it supplies no value
    /// representation and does not exempt descendants from type checking.
    non_completing: HashSet<usize>,
    /// Complete callable identity for each concrete named declaration.
    functions: HashMap<String, Ty>,
    existential_packs: HashMap<usize, (Ty, Ty)>,
    existential_upcasts: HashMap<usize, (Ty, Ty)>,
    record_projections: HashMap<usize, (Ty, Ty)>,
    region_promotions: HashMap<usize, RegionPromotionFact>,
}

/// The canonical source operation key for one proper-tail candidate.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProperTailSiteKey {
    pub callable: ProperTailCallableId,
    pub path: Vec<ProperTailPathStep>,
    pub dispatch: ProperTailDispatchKind,
}

/// An opaque deterministic identity for a proper-tail source operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProperTailSiteId([u8; 32]);

impl ProperTailSiteId {
    pub fn as_bytes(self) -> [u8; 32] {
        self.0
    }

    fn from_key(key: &ProperTailSiteKey) -> Self {
        let mut bytes = Vec::new();
        encode_site_key(key, &mut bytes);
        Self(*blake3::hash(&bytes).as_bytes())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProperTailManifestError {
    PathOverflow,
    DuplicateId,
}

impl fmt::Display for ProperTailManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PathOverflow => f.write_str("proper-tail structural path exceeds u32 range"),
            Self::DuplicateId => f.write_str("proper-tail manifest contains a duplicate site ID"),
        }
    }
}

/// Checked source obligations, sorted by their canonical keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProperTailSite {
    pub id: ProperTailSiteId,
    pub key: ProperTailSiteKey,
    /// Ordinal in the canonical linked-function sequence. This is not the
    /// source item index: non-function and excluded items must not perturb the
    /// identity consumed by SafeWIR lowering.
    pub canonical_function: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProperTailSiteManifest {
    sites: Vec<ProperTailSite>,
}

impl ProperTailSiteManifest {
    pub fn sites(&self) -> &[ProperTailSite] {
        &self.sites
    }

    pub fn digest(&self) -> [u8; 32] {
        canonical_proper_tail_manifest_digest(
            &self
                .sites
                .iter()
                .map(|site| (site.id.as_bytes(), site.canonical_function))
                .collect::<Vec<_>>(),
        )
    }

    /// Retain obligations in the finalized executable callable sequence.
    /// Generic templates have no runtime ordinal; their concrete instances do.
    /// This filtering is performed
    /// while the frontend still owns the checked typed module; lowering only
    /// transports the resulting witness and cannot choose its expected digest.
    pub fn for_canonical_functions(&self, module: &Module, runtime_functions: &[String]) -> Self {
        let runtime_ordinals = runtime_functions.iter().enumerate()
            .map(|(ordinal, name)| (name.as_str(), u32::try_from(ordinal).expect("validated function count")))
            .collect::<HashMap<_, _>>();
        let mut canonical_ordinals = HashMap::default();
        for (item_index, item) in module.items.iter().enumerate() {
            let Item::Function(function) = item else { continue };
            if let Some(&ordinal) = runtime_ordinals.get(function.name.as_str()) {
                let item_index = u32::try_from(item_index).expect("validated item index");
                canonical_ordinals.insert(item_index, ordinal);
            }
        }
        Self {
            sites: self
                .sites
                .iter()
                .filter_map(|site| {
                    canonical_ordinals
                        .get(&site.key.callable.item)
                        .map(|&canonical_function| ProperTailSite {
                            canonical_function,
                            ..site.clone()
                        })
                })
                .collect(),
        }
    }
}

impl TypedModule {
    /// Derive proper-tail obligations from this exact checked AST allocation.
    /// The result contains no AST pointers and remains valid after this owner is
    /// consumed by a later lowering stage.
    pub fn proper_tail_manifest(&self) -> Result<ProperTailSiteManifest, ProperTailManifestError> {
        derive_proper_tail_manifest(&self.module, self.table())
    }
}

fn derive_proper_tail_manifest(module: &Module, table: &TypeTable) -> Result<ProperTailSiteManifest, ProperTailManifestError> {
    let mut keys = Vec::new();
    let shadowed_parameters = HashSet::default();
    let functions = module.items.iter().filter_map(|item| match item {
        Item::Function(function) => Some((function.name.as_str(), function)),
        _ => None,
    }).collect::<HashMap<_, _>>();
    for (item, item_def) in module.items.iter().enumerate() {
        let item = u32::try_from(item).map_err(|_| ProperTailManifestError::PathOverflow)?;
        match item_def {
            Item::Function(function) => {
                let callable = ProperTailCallableId { item, method: None, lambda_path: Vec::new() };
                let caller = TailCaller { callable: &callable, params: &function.params, ret: function.ret.as_ref(), functions: &functions, table, checked_signature: table.functions.get(function.name.as_str()), shadowed_parameters: &shadowed_parameters };
                collect_callable(&function.body, &caller, &mut keys)?;
            }
            Item::Impl(definition) => {
                for (method, function) in definition.methods.iter().enumerate() {
                    let method = u32::try_from(method).map_err(|_| ProperTailManifestError::PathOverflow)?;
                    let callable = ProperTailCallableId { item, method: Some(method), lambda_path: Vec::new() };
                    let caller = TailCaller { callable: &callable, params: &function.params, ret: function.ret.as_ref(), functions: &functions, table, checked_signature: table.functions.get(function.name.as_str()), shadowed_parameters: &shadowed_parameters };
                    collect_callable(&function.body, &caller, &mut keys)?;
                }
            }
            Item::Trait(definition) => {
                for (method, signature) in definition.methods.iter().enumerate() {
                    let Some(body) = &signature.default else { continue };
                    let method = u32::try_from(method).map_err(|_| ProperTailManifestError::PathOverflow)?;
                    let callable = ProperTailCallableId { item, method: Some(method), lambda_path: Vec::new() };
                    let caller = TailCaller { callable: &callable, params: &signature.params, ret: signature.ret.as_ref(), functions: &functions, table, checked_signature: None, shadowed_parameters: &shadowed_parameters };
                    collect_callable(body, &caller, &mut keys)?;
                }
            }
            Item::Type(_) | Item::TypeAlias { .. } | Item::Const { .. } | Item::Comptime(_) => {}
        }
    }
    keys.sort();
    u32::try_from(keys.len()).map_err(|_| ProperTailManifestError::PathOverflow)?;
    let mut ids = HashSet::default();
    let mut sites = Vec::with_capacity(keys.len());
    for key in keys {
        u32::try_from(key.path.len()).map_err(|_| ProperTailManifestError::PathOverflow)?;
        u32::try_from(key.callable.lambda_path.len()).map_err(|_| ProperTailManifestError::PathOverflow)?;
        let id = ProperTailSiteId::from_key(&key);
        if !ids.insert(id) {
            return Err(ProperTailManifestError::DuplicateId);
        }
        sites.push(ProperTailSite {
            id,
            canonical_function: key.callable.item,
            key,
        });
    }
    Ok(ProperTailSiteManifest { sites })
}

#[derive(Clone, Copy)]
struct TailCaller<'a> {
    callable: &'a ProperTailCallableId,
    params: &'a [ast::Param],
    ret: Option<&'a ast::Type>,
    functions: &'a HashMap<&'a str, &'a Function>,
    table: &'a TypeTable,
    checked_signature: Option<&'a Ty>,
    /// Hidden write-back parameter spellings shadowed by a nearer lexical
    /// binding at this site.
    /// Assignment preserves a binding; declarations and patterns replace it.
    shadowed_parameters: &'a HashSet<String>,
}

/// A plain direct `&mut T` parameter is a hidden write-back lane in the
/// canonical ABI, just as `var T` is. Outer ownership qualifiers deliberately
/// stay outside this predicate: their transport has a distinct contract.
fn plain_exclusive_reference(ty: &ast::Type) -> bool {
    matches!(ty, ast::Type::Qualified(ast::TypeQual::BorrowMut(_), _))
}

fn plain_direct_exclusive_write_back(ty: &ast::Type) -> bool {
    matches!(ty, ast::Type::Qualified(ast::TypeQual::BorrowMut(_), inner)
        if matches!(inner.unqualified(), ast::Type::Named(name, arguments)
            if matches!(name.as_str(), "String" | "Int") && arguments.is_empty()))
}

/// RFC-0090 proper tails forward the complete callable ABI envelope. `var`
/// and plain direct `&mut` parameters are hidden result lanes, so a textually
/// final direct call is proper only when every such lane is forwarded
/// unchanged. Checked callable types resolve indirect targets; ordinary input
/// parameters need not match because they are not returned write-back lanes.
/// Unknown indirect ABI shapes remain candidates only when the caller itself
/// has no pending write-back lane; the independent SafeWIR checker still
/// requires exact result signatures before accepting the edge.
fn direct_tail_envelope_is_forwarded(expr: &Expr, caller: TailCaller<'_>) -> bool {
    let checked_target = match expr {
        Expr::Call { name, args } => {
            // A local callable may shadow a declaration. Never replace its
            // checked convention with that of a same-named global function.
            let signature = match caller.table.local_callee_type(expr) {
                Some(signature) => ty_to_ast(signature),
                None => caller.table.function_type(name),
            };
            signature.map(|signature| (signature, args.as_slice()))
        }
        Expr::Apply { func, args } => caller.table.type_of(func).and_then(ty_to_ast)
            .map(|signature| (signature, args.as_slice())),
        _ => None,
    };
    if let (
        Some(ast::Type::Fn(source_types, source_result, source_conventions, _)),
        Some((ast::Type::Fn(target_types, target_result, target_conventions, _), arguments)),
    ) = (caller.checked_signature.and_then(ty_to_ast), checked_target) {
        if source_types.len() != caller.params.len() || target_types.len() != arguments.len()
            || (!source_conventions.is_empty() && source_conventions.len() != source_types.len())
            || (!target_conventions.is_empty() && target_conventions.len() != target_types.len())
            || caller.params.iter().enumerate().any(|(index, parameter)|
                (parameter.convention == Convention::Var)
                    != (source_conventions.get(index) == Some(&Convention::Var)))
        {
            return false;
        }
        let declared_target = match expr {
            Expr::Call { name, .. } if caller.table.local_callee_type(expr).is_none() => {
                caller.functions.get(name.as_str()).copied()
            }
            _ => None,
        };
        let source_has_unsupported_reference = caller.params.iter().any(|parameter| {
            parameter.convention == Convention::Let
                && parameter.ty.as_ref().is_some_and(|ty| {
                    plain_exclusive_reference(ty) && !plain_direct_exclusive_write_back(ty)
                })
        });
        let target_has_unsupported_reference = declared_target.is_some_and(|target| {
            target.params.iter().any(|parameter| {
                parameter.convention == Convention::Let
                    && parameter.ty.as_ref().is_some_and(|ty| {
                        plain_exclusive_reference(ty) && !plain_direct_exclusive_write_back(ty)
                    })
            })
        });
        if source_has_unsupported_reference || target_has_unsupported_reference {
            return false;
        }
        let source_write_backs = caller.params.iter().enumerate().filter_map(|(index, parameter)| {
            (parameter.convention == Convention::Var).then_some((index, false)).or_else(|| {
                (parameter.convention == Convention::Let
                    && parameter.ty.as_ref().is_some_and(plain_direct_exclusive_write_back))
                    .then_some((index, true))
            })
        }).collect::<Vec<_>>();
        let target_write_backs: Vec<(usize, bool)> = declared_target.map(|target| {
            target.params.iter().enumerate().filter_map(|(index, parameter)| {
                (parameter.convention == Convention::Var).then_some((index, false)).or_else(|| {
                    (parameter.convention == Convention::Let
                        && parameter.ty.as_ref().is_some_and(plain_direct_exclusive_write_back))
                        .then_some((index, true))
                })
            }).collect()
        }).unwrap_or_else(|| {
            target_types.iter().enumerate().filter_map(|(index, ty)| {
                let convention = target_conventions.get(index).copied().unwrap_or(Convention::Let);
                (convention == Convention::Var).then_some((index, false)).or_else(|| {
                    (convention == Convention::Let && plain_direct_exclusive_write_back(ty))
                        .then_some((index, true))
                })
            }).collect()
        });
        return source_result == target_result && source_write_backs.len() == target_write_backs.len()
            && source_write_backs.iter().zip(&target_write_backs).all(|(&(source, source_reference), &(target, target_reference))| {
                source_reference == target_reference
                    && source_types[source] == target_types[target]
                    && !caller.shadowed_parameters.contains(&caller.params[source].name)
                    && matches!(&arguments[target], Expr::Var(name) if name == &caller.params[source].name)
            });
    }
    // An unresolved local signature cannot fall back to a same-spelled
    // declaration. That declaration is not the callee at this lexical site.
    if caller.table.local_callee_type(expr).is_some() {
        return false;
    }
    let caller_write_backs = caller.params.iter().enumerate().filter_map(|(index, parameter)| {
        (parameter.convention == Convention::Var).then_some((index, false)).or_else(|| {
            (parameter.convention == Convention::Let
                && parameter.ty.as_ref().is_some_and(plain_direct_exclusive_write_back))
                .then_some((index, true))
        })
    }).collect::<Vec<_>>();
    let Expr::Call { name, args } = expr else {
        return caller_write_backs.is_empty();
    };
    let Some(target) = caller.functions.get(name.as_str()).copied() else {
        return caller_write_backs.is_empty();
    };
    let target_write_backs = target.params.iter().enumerate().filter_map(|(index, parameter)| {
        (parameter.convention == Convention::Var).then_some((index, false)).or_else(|| {
            (parameter.convention == Convention::Let
                && parameter.ty.as_ref().is_some_and(plain_direct_exclusive_write_back))
                .then_some((index, true))
        })
    }).collect::<Vec<_>>();
    let has_unsupported_reference = |parameters: &[ast::Param]| {
        parameters.iter().any(|parameter| {
            parameter.convention == Convention::Let
                && parameter.ty.as_ref().is_some_and(|ty| {
                    plain_exclusive_reference(ty) && !plain_direct_exclusive_write_back(ty)
                })
        })
    };
    if has_unsupported_reference(caller.params) || has_unsupported_reference(&target.params) {
        return false;
    }
    if caller_write_backs.is_empty() {
        return target_write_backs.is_empty();
    }
    caller.ret == target.ret.as_ref()
        && caller_write_backs.len() == target_write_backs.len()
        && caller_write_backs.iter().zip(&target_write_backs).all(|(&source, &target_lane)| {
            source.1 == target_lane.1
                && caller.params[source.0].ty == target.params[target_lane.0].ty
                && !caller.shadowed_parameters.contains(&caller.params[source.0].name)
                && matches!(&args[target_lane.0], Expr::Var(name) if name == &caller.params[source.0].name)
        })
}

fn shadow_tail_parameter(name: &str, params: &[ast::Param], shadowed: &mut HashSet<String>) {
    if params.iter().any(|parameter| {
        parameter.name == name
            && (parameter.convention == Convention::Var
                || (parameter.convention == Convention::Let
                    && parameter.ty.as_ref().is_some_and(plain_direct_exclusive_write_back)))
    }) {
        shadowed.insert(name.to_owned());
    }
}

fn shadow_tail_pattern(pattern: &ast::Pattern, params: &[ast::Param], shadowed: &mut HashSet<String>) {
    let mut names = Vec::new();
    ast::pattern_binds(pattern, &mut names);
    for name in names {
        shadow_tail_parameter(&name, params, shadowed);
    }
}

fn collect_callable(
    body: &Block,
    caller: &TailCaller<'_>,
    keys: &mut Vec<ProperTailSiteKey>,
) -> Result<(), ProperTailManifestError> {
    collect_block(body, caller, true, &mut Vec::new(), keys)
}

fn collect_block(
    block: &Block,
    caller: &TailCaller<'_>,
    value_tail: bool,
    path: &mut Vec<ProperTailPathStep>,
    keys: &mut Vec<ProperTailSiteKey>,
) -> Result<(), ProperTailManifestError> {
    let parent = caller;
    let mut shadowed = parent.shadowed_parameters.clone();
    for (index, statement) in block.stmts.iter().enumerate() {
        let caller = &TailCaller { shadowed_parameters: &shadowed, ..*parent };
        path.push(ProperTailPathStep::Statement(u32::try_from(index).map_err(|_| ProperTailManifestError::PathOverflow)?));
        match statement {
            Stmt::Let { value, .. } | Stmt::Assign { value, .. } | Stmt::LetPattern { value, .. } => {
                collect_child(value, false, caller, path, 0, keys)?;
            }
            Stmt::Return(Some(value)) => {
                path.push(ProperTailPathStep::ReturnValue);
                collect_expr(value, true, caller, path, keys)?;
                path.pop();
            }
            Stmt::Expr(value) => {
                path.push(ProperTailPathStep::Value);
                collect_expr(value, value_tail && index + 1 == block.stmts.len(), caller, path, keys)?;
                path.pop();
            }
            Stmt::Yield(value) => {
                path.push(ProperTailPathStep::YieldValue);
                collect_expr(value, false, caller, path, keys)?;
                path.pop();
            }
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
        path.pop();
        // Initializers see the preceding binding. Only subsequent statements
        // see the declaration, and leaving this block restores the parent.
        match statement {
            Stmt::Let { name, .. } => shadow_tail_parameter(name, parent.params, &mut shadowed),
            Stmt::LetPattern { pattern, .. } => shadow_tail_pattern(pattern, parent.params, &mut shadowed),
            _ => {}
        }
    }
    Ok(())
}

fn collect_child(
    expr: &Expr,
    tail: bool,
    caller: &TailCaller<'_>,
    path: &mut Vec<ProperTailPathStep>,
    child: usize,
    keys: &mut Vec<ProperTailSiteKey>,
) -> Result<(), ProperTailManifestError> {
    path.push(ProperTailPathStep::Child(u32::try_from(child).map_err(|_| ProperTailManifestError::PathOverflow)?));
    collect_expr(expr, tail, caller, path, keys)?;
    path.pop();
    Ok(())
}

fn collect_expr(
    expr: &Expr,
    tail: bool,
    caller: &TailCaller<'_>,
    path: &mut Vec<ProperTailPathStep>,
    keys: &mut Vec<ProperTailSiteKey>,
) -> Result<(), ProperTailManifestError> {
    path.push(ProperTailPathStep::Node(expr_kind(expr)));
    if tail && direct_tail_envelope_is_forwarded(expr, *caller)
        && !compiler_known_owned_tail(expr, caller.table)
    {
        if let Some(dispatch) = proper_tail_dispatch(expr) {
            keys.push(ProperTailSiteKey { callable: caller.callable.clone(), path: path.clone(), dispatch });
        }
    }
    match expr {
        Expr::Call { args, .. } | Expr::Ctor { args, .. } | Expr::AnonCtor { args, .. } | Expr::List(args) | Expr::Tuple(args) => {
            for (index, arg) in args.iter().enumerate() {
                collect_child(arg, false, caller, path, index, keys)?;
            }
        }
        Expr::LabeledCall { args, .. } => {
            for (index, (_, arg)) in args.iter().enumerate() {
                collect_child(arg, false, caller, path, index, keys)?;
            }
        }
        Expr::LabeledMethodCall { receiver, args, .. } => {
            collect_child(receiver, false, caller, path, 0, keys)?;
            for (index, (_, arg)) in args.iter().enumerate() {
                collect_child(arg, false, caller, path, index + 1, keys)?;
            }
        }
        Expr::MethodCall { receiver, args, .. } | Expr::ExistentialCall { receiver, args, .. } => {
            collect_child(receiver, false, caller, path, 0, keys)?;
            for (index, arg) in args.iter().enumerate() {
                collect_child(arg, false, caller, path, index + 1, keys)?;
            }
        }
        Expr::Apply { func, args } => {
            collect_child(func, false, caller, path, 0, keys)?;
            for (index, arg) in args.iter().enumerate() {
                collect_child(arg, false, caller, path, index + 1, keys)?;
            }
        }
        Expr::Unary { expr, .. } | Expr::Field { base: expr, .. } | Expr::Try(expr) | Expr::As { expr, .. }
        | Expr::ExistentialPack { expr, .. } | Expr::ExistentialUpcast { expr, .. } => {
            collect_child(expr, false, caller, path, 0, keys)?;
        }
        Expr::RecordUpdate { base, fields, .. } => {
            collect_child(base, false, caller, path, 0, keys)?;
            for (index, (_, value)) in fields.iter().enumerate() {
                collect_child(value, false, caller, path, index + 1, keys)?;
            }
        }
        Expr::Record { fields, spread, .. } => {
            for (index, (_, value)) in fields.iter().enumerate() {
                collect_child(value, false, caller, path, index, keys)?;
            }
            if let Some(spread) = spread {
                collect_child(spread, false, caller, path, fields.len(), keys)?;
            }
        }
        Expr::Binary { op, lhs, rhs } => {
            collect_child(lhs, false, caller, path, 0, keys)?;
            collect_child(rhs, *op == ast::BinOp::Coalesce && tail, caller, path, 1, keys)?;
        }
        Expr::If { cond, then_block, else_block } => {
            collect_child(cond, false, caller, path, 0, keys)?;
            path.push(ProperTailPathStep::Child(1));
            collect_block(then_block, caller, tail, path, keys)?;
            path.pop();
            if let Some(else_block) = else_block {
                path.push(ProperTailPathStep::Child(2));
                collect_block(else_block, caller, tail, path, keys)?;
                path.pop();
            }
        }
        Expr::Match { scrutinee, arms } => {
            collect_child(scrutinee, false, caller, path, 0, keys)?;
            for (index, arm) in arms.iter().enumerate() {
                let mut shadowed = caller.shadowed_parameters.clone();
                shadow_tail_pattern(&arm.pattern, caller.params, &mut shadowed);
                let arm_caller = TailCaller { shadowed_parameters: &shadowed, ..*caller };
                path.push(ProperTailPathStep::Arm(u32::try_from(index).map_err(|_| ProperTailManifestError::PathOverflow)?));
                if let Some(guard) = &arm.guard {
                    path.push(ProperTailPathStep::Guard);
                    collect_expr(guard, false, &arm_caller, path, keys)?;
                    path.pop();
                }
                path.push(ProperTailPathStep::Value);
                collect_expr(&arm.body, tail, &arm_caller, path, keys)?;
                path.pop();
                path.pop();
            }
        }
        Expr::Block(block) => collect_block(block, caller, tail, path, keys)?,
        Expr::Lambda { params, ret, body, .. } => {
            let nested = ProperTailCallableId { lambda_path: path.clone(), ..caller.callable.clone() };
            let shadowed_parameters = HashSet::default();
            let nested_caller = TailCaller {
                callable: &nested,
                params,
                ret: ret.as_ref(),
                functions: caller.functions,
                table: caller.table,
                checked_signature: caller.table.type_of(expr),
                shadowed_parameters: &shadowed_parameters,
            };
            path.push(ProperTailPathStep::Child(0));
            collect_block(body, &nested_caller, true, path, keys)?;
            path.pop();
        }
        Expr::While { cond, body } => {
            collect_child(cond, false, caller, path, 0, keys)?;
            path.push(ProperTailPathStep::Child(1));
            collect_block(body, caller, false, path, keys)?;
            path.pop();
        }
        Expr::For { var, iter, body } => {
            collect_child(iter, false, caller, path, 0, keys)?;
            let mut shadowed = caller.shadowed_parameters.clone();
            shadow_tail_parameter(var, caller.params, &mut shadowed);
            let loop_caller = TailCaller { shadowed_parameters: &shadowed, ..*caller };
            path.push(ProperTailPathStep::Child(1));
            collect_block(body, &loop_caller, false, path, keys)?;
            path.pop();
        }
        Expr::WhileLet { pattern, scrutinee, body } => {
            collect_child(scrutinee, false, caller, path, 0, keys)?;
            let mut shadowed = caller.shadowed_parameters.clone();
            shadow_tail_pattern(pattern, caller.params, &mut shadowed);
            let loop_caller = TailCaller { shadowed_parameters: &shadowed, ..*caller };
            path.push(ProperTailPathStep::Child(1));
            collect_block(body, &loop_caller, false, path, keys)?;
            path.pop();
        }
        Expr::Range { lo, hi, .. } | Expr::Index { base: lo, index: hi } => {
            collect_child(lo, false, caller, path, 0, keys)?;
            collect_child(hi, false, caller, path, 1, keys)?;
        }
        Expr::Int(_) | Expr::Float(_) | Expr::Duration(_) | Expr::Str(_) | Expr::Bool(_) | Expr::Var(_) | Expr::TaggedLit { .. } => {}
    }
    path.pop();
    Ok(())
}

fn proper_tail_dispatch(expr: &Expr) -> Option<ProperTailDispatchKind> {
    match expr {
        // Effect-only canonical host operations are not callable tail
        // transfers. In particular, console.print must lower to HostCall
        // followed by Return, not to a TailCall terminator.
        // An intrinsic-named call (the catalog, plus the duration conversions
        // the linker resolves by name) is computed in place and returned as a
        // value: there is no callee to transfer to.
        Expr::Call { name, .. }
            if matches!(name.as_str(), "fail" | "vm.serve" | "__capop.print" | "int_to_duration" | "duration_to_int")
                // The parser-only reference write is an effect terminator,
                // not a source-callable callee. A proper-tail obligation here
                // would require SafeWIR to invent a TailCall for `*r = v`.
                || name == intrinsics::REFERENCE_WRITE
                || (name.starts_with("__capop.") && witchy_syntax::cap_ops::is_op_name(name))
                || witchy_syntax::intrinsics::lookup(name).is_some()
                || witchy_syntax::intrinsics::lookup(name.strip_prefix("list.__").map_or(name.as_str(), |_| name.as_str())).is_some() => None,
        Expr::Call { .. } | Expr::LabeledCall { .. } | Expr::LabeledMethodCall { .. } | Expr::MethodCall { .. } => Some(ProperTailDispatchKind::Direct),
        Expr::Apply { .. } | Expr::ExistentialCall { .. } => Some(ProperTailDispatchKind::Indirect),
        _ => None,
    }
}

/// A checked compiler-owned materialization is emitted inline, not as a
/// callable transfer. Declared `owned` methods resolve to ordinary calls.
fn compiler_known_owned_tail(expr: &Expr, table: &TypeTable) -> bool {
    let Expr::MethodCall { method, args, .. } = expr else { return false };
    method == "owned"
        && args.is_empty()
        && table.compiler_owned_materializations.contains(&(expr as *const Expr as usize))
}

fn expr_kind(expr: &Expr) -> ProperTailExprKind {
    match expr {
        Expr::Int(_) => ProperTailExprKind::Int,
        Expr::Float(_) => ProperTailExprKind::Float,
        Expr::Duration(_) => ProperTailExprKind::Duration,
        Expr::Str(_) => ProperTailExprKind::Str,
        Expr::Bool(_) => ProperTailExprKind::Bool,
        Expr::List(_) => ProperTailExprKind::List,
        Expr::Tuple(_) => ProperTailExprKind::Tuple,
        Expr::Var(_) => ProperTailExprKind::Var,
        Expr::Call { .. } => ProperTailExprKind::Call,
        Expr::LabeledCall { .. } => ProperTailExprKind::LabeledCall,
        Expr::LabeledMethodCall { .. } => ProperTailExprKind::LabeledMethodCall,
        Expr::MethodCall { .. } => ProperTailExprKind::MethodCall,
        Expr::Apply { .. } => ProperTailExprKind::Apply,
        Expr::Ctor { .. } => ProperTailExprKind::Ctor,
        Expr::AnonCtor { .. } => ProperTailExprKind::AnonCtor,
        Expr::Unary { .. } => ProperTailExprKind::Unary,
        Expr::Field { .. } => ProperTailExprKind::Field,
        Expr::Lambda { .. } => ProperTailExprKind::Lambda,
        Expr::RecordUpdate { .. } => ProperTailExprKind::RecordUpdate,
        Expr::Record { .. } => ProperTailExprKind::Record,
        Expr::Try(_) => ProperTailExprKind::Try,
        Expr::As { .. } => ProperTailExprKind::As,
        Expr::ExistentialPack { .. } => ProperTailExprKind::ExistentialPack,
        Expr::ExistentialUpcast { .. } => ProperTailExprKind::ExistentialUpcast,
        Expr::ExistentialCall { .. } => ProperTailExprKind::ExistentialCall,
        Expr::Binary { .. } => ProperTailExprKind::Binary,
        Expr::If { .. } => ProperTailExprKind::If,
        Expr::Match { .. } => ProperTailExprKind::Match,
        Expr::Block(_) => ProperTailExprKind::Block,
        Expr::While { .. } => ProperTailExprKind::While,
        Expr::For { .. } => ProperTailExprKind::For,
        Expr::Range { .. } => ProperTailExprKind::Range,
        Expr::Index { .. } => ProperTailExprKind::Index,
        Expr::WhileLet { .. } => ProperTailExprKind::WhileLet,
        Expr::TaggedLit { .. } => ProperTailExprKind::TaggedLit,
    }
}

fn put_u32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn encode_path(path: &[ProperTailPathStep], bytes: &mut Vec<u8>) {
    put_u32(bytes, u32::try_from(path.len()).expect("validated proper-tail path length"));
    for step in path {
        match step {
            ProperTailPathStep::Statement(index) => { bytes.push(0); put_u32(bytes, *index); }
            ProperTailPathStep::Value => bytes.push(1),
            ProperTailPathStep::ReturnValue => bytes.push(2),
            ProperTailPathStep::YieldValue => bytes.push(3),
            ProperTailPathStep::Node(kind) => { bytes.push(4); bytes.push(*kind as u8); }
            ProperTailPathStep::Child(index) => { bytes.push(5); put_u32(bytes, *index); }
            ProperTailPathStep::Arm(index) => { bytes.push(6); put_u32(bytes, *index); }
            ProperTailPathStep::Guard => bytes.push(7),
        }
    }
}

fn encode_site_key(key: &ProperTailSiteKey, bytes: &mut Vec<u8>) {
    bytes.extend_from_slice(b"witchy.proper-tail-site.v1");
    put_u32(bytes, key.callable.item);
    match key.callable.method {
        Some(method) => { bytes.push(1); put_u32(bytes, method); }
        None => bytes.push(0),
    }
    encode_path(&key.callable.lambda_path, bytes);
    encode_path(&key.path, bytes);
    bytes.push(match key.dispatch { ProperTailDispatchKind::Direct => 0, ProperTailDispatchKind::Indirect => 1 });
}

impl TypeTable {
    /// Whether this exact checked expression cannot complete normally.
    /// Absence means unknown or completing, never permission to omit a type.
    pub fn is_non_completing(&self, e: &Expr) -> bool {
        self.non_completing.contains(&(e as *const Expr as usize))
    }

    pub fn type_of(&self, e: &Expr) -> Option<&Ty> {
        self.types.get(&(e as *const Expr as usize))
    }

    pub fn is_compiler_owned_materialization(&self, e: &Expr) -> bool {
        self.compiler_owned_materializations.contains(&(e as *const Expr as usize))
    }

    pub(crate) fn view_type_of(&self, e: &Expr) -> Option<&Ty> {
        self.view_types.get(&(e as *const Expr as usize))
    }

    pub fn local_callee_type(&self, e: &Expr) -> Option<&Ty> {
        self.local_callees.get(&(e as *const Expr as usize))
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
        ty_to_ast(self.functions.get(name)?)
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

    /// The checker-authenticated promotion contract for this exact `region:`
    /// block.  Absence is never equivalent to an ordinary lexical block.
    pub fn region_promotion(&self, block: &Block) -> Option<&RegionPromotionFact> {
        self.region_promotions.get(&(block as *const Block as usize))
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
#[derive(Debug)]
pub struct TypedModule {
    // Address-keyed type and loan facts must outlive moves of this wrapper.
    // A box gives the retained AST one stable allocation for the lifetime of
    // every checked side product.
    module: Box<Module>,
    table: TypeTable,
    loan_facts: Option<RetainedLoanFacts>,
    access_facts: Option<RetainedAccessFacts>,
    view_facts: Option<RetainedViewFacts>,
    /// Which checked annotation produced exactly this table and these facts
    /// for exactly this AST, if any. Cleared by every in-place rewrite, so it
    /// is never a claim about a module the checker did not see.
    checked_as: Option<CheckedAnnotation>,
}

/// The checked annotation entry point that produced a [`TypedModule`]:
/// [`annotate_checked`] or [`annotate_checked_build`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CheckedAnnotation {
    Runtime,
    BuildEntrypoint,
}

impl CheckedAnnotation {
    fn annotate(self, module: Module) -> Result<TypedModule, TypeError> {
        match self {
            Self::Runtime => annotate_checked(module),
            Self::BuildEntrypoint => annotate_checked_build(module),
        }
    }
}

#[derive(Debug)]
struct RetainedLoanFacts {
    module: *const Module,
    facts: crate::loans::LoanFacts,
}

#[derive(Debug)]
struct RetainedAccessFacts {
    module: *const Module,
    facts: crate::access::CheckedAccessFacts,
}

#[derive(Debug)]
struct RetainedViewFacts {
    module: *const Module,
    facts: crate::views::CheckedViewFacts,
}

/// Why the retained loan graph cannot be used for this typed AST.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckedLoanFactsError {
    Missing,
    Stale,
}

impl std::fmt::Display for CheckedLoanFactsError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing => write!(formatter, "checked typed module lacks its loan graph"),
            Self::Stale => write!(formatter, "checked loan graph belongs to a different AST allocation"),
        }
    }
}

impl std::error::Error for CheckedLoanFactsError {}

/// Why the retained access-signature graph cannot be used for this typed AST.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckedAccessFactsError {
    Missing,
    Stale,
}

/// Why rooted-view formation facts cannot be used for this typed AST.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckedViewFactsError {
    Missing,
    Stale,
}

impl std::fmt::Display for CheckedViewFactsError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing => write!(formatter, "checked typed module lacks rooted-view facts"),
            Self::Stale => write!(formatter, "checked rooted-view facts belong to a different AST allocation"),
        }
    }
}

impl std::error::Error for CheckedViewFactsError {}

impl std::fmt::Display for CheckedAccessFactsError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing => write!(formatter, "checked typed module lacks its access-signature graph"),
            Self::Stale => write!(formatter, "checked access-signature graph belongs to a different AST allocation"),
        }
    }
}

impl std::error::Error for CheckedAccessFactsError {}

impl PartialEq for TypedModule {
    fn eq(&self, other: &Self) -> bool {
        self.module == other.module && self.table == other.table
    }
}

fn typed_module_checked(module: Module, table: TypeTable) -> Result<TypedModule, TypeError> {
    let module = Box::new(module);
    let has_views = crate::views::module_contains_canonical_view(&module);
    let has_legacy_loans = crate::views::module_contains_legacy_loan(&module);
    if has_views && has_legacy_loans {
        return terr(
            "canonical immutable views cannot be mixed with legacy lifetime loans in one checked module; migrate the legacy references or split the module",
        );
    }
    let loan_facts = (!has_views)
        .then(|| crate::loans::facts_with_types(&module, &table))
        .transpose()?;
    let view_facts = has_views
        .then(|| crate::views::checked_facts(&module, &table))
        .transpose()?;
    let access_facts = crate::access::checked_facts(&module, &table)
        .map_err(|error| TypeError { message: error.to_string() })?;
    crate::call_scoped::verify(&module, &table, &access_facts)?;
    Ok(TypedModule {
        loan_facts: loan_facts.map(|facts| RetainedLoanFacts { module: std::ptr::from_ref(module.as_ref()), facts }),
        access_facts: Some(RetainedAccessFacts { module: std::ptr::from_ref(module.as_ref()), facts: access_facts }),
        view_facts: view_facts.map(|facts| RetainedViewFacts { module: std::ptr::from_ref(module.as_ref()), facts }),
        module,
        table,
        checked_as: None,
    })
}

fn typed_module_best_effort(module: Module, table: TypeTable) -> TypedModule {
    let module = Box::new(module);
    let has_views = crate::views::module_contains_canonical_view(&module);
    let has_legacy_loans = crate::views::module_contains_legacy_loan(&module);
    // Best-effort annotation cannot report the mixed-mode error, but must not
    // hand either stale/incomplete fact product to a downstream consumer.
    let mixed_view_and_loan = has_views && has_legacy_loans;
    let loan_facts = (!has_views && !mixed_view_and_loan).then(|| crate::loans::facts_with_types(&module, &table))
        .and_then(Result::ok)
        .map(|facts| RetainedLoanFacts { module: std::ptr::from_ref(module.as_ref()), facts });
    let access_facts = crate::access::checked_facts(&module, &table)
        .ok()
        .map(|facts| RetainedAccessFacts { module: std::ptr::from_ref(module.as_ref()), facts });
    let view_facts = (has_views && !mixed_view_and_loan).then(|| crate::views::checked_facts(&module, &table))
        .and_then(Result::ok)
        .map(|facts| RetainedViewFacts { module: std::ptr::from_ref(module.as_ref()), facts });
    TypedModule { module, table, loan_facts, access_facts, view_facts, checked_as: None }
}

impl TypedModule {
    pub fn module(&self) -> &Module {
        &self.module
    }

    pub fn table(&self) -> &TypeTable {
        &self.table
    }

    pub fn into_module(self) -> Module {
        *self.module
    }

    /// Borrow the exact pointer-keyed loan graph checked for this AST.
    ///
    /// This is deliberately fallible: best-effort annotation has no authority
    /// to invent an empty graph, and any structural rewrite must re-check it.
    pub fn loan_facts(&self) -> Result<&crate::loans::LoanFacts, CheckedLoanFactsError> {
        let retained = self.loan_facts.as_ref().ok_or(CheckedLoanFactsError::Missing)?;
        (std::ptr::eq(retained.module, self.module.as_ref()))
            .then_some(&retained.facts)
            .ok_or(CheckedLoanFactsError::Stale)
    }

    /// Borrow exact checked rooted-view formation facts.  Consumers that do
    /// not understand this carrier receive `Missing` and must reject rather
    /// than recreating root/projection authority from source syntax.
    pub fn view_facts(&self) -> Result<&crate::views::CheckedViewFacts, CheckedViewFactsError> {
        let retained = self.view_facts.as_ref().ok_or(CheckedViewFactsError::Missing)?;
        (std::ptr::eq(retained.module, self.module.as_ref()))
            .then_some(&retained.facts)
            .ok_or(CheckedViewFactsError::Stale)
    }

    /// Borrow the exact pointer-keyed access-signature graph checked for this AST.
    ///
    /// This graph authenticates declarations, calls, callable expressions, and
    /// places; canonical consumers must reject rather than rebuild it.
    pub fn access_facts(&self) -> Result<&crate::access::CheckedAccessFacts, CheckedAccessFactsError> {
        let retained = self.access_facts.as_ref().ok_or(CheckedAccessFactsError::Missing)?;
        (std::ptr::eq(retained.module, self.module.as_ref()))
            .then_some(&retained.facts)
            .ok_or(CheckedAccessFactsError::Stale)
    }

    /// Mutate fields that preserve every expression allocation (for example a
    /// resolved call name or binary-op tag) while consulting the existing facts.
    /// Structural rewrites must instead consume the typed module and re-annotate.
    pub(crate) fn rewrite_preserving_nodes<R>(
        &mut self,
        rewrite: impl FnOnce(&TypeTable, &mut Module) -> R,
    ) -> R {
        self.checked_as = None;
        rewrite(&self.table, &mut self.module)
    }

    /// Apply a rewrite that reports whether it changed the AST, and rebuild the
    /// address-keyed facts only when it did (or when this wrapper was not
    /// produced by the `checked` annotation the caller requires).
    ///
    /// `rewrite` must return `false` only when it mutated nothing: then every
    /// node allocation and value is exactly what `checked` already annotated,
    /// so re-running it would recompute this same table and these same facts
    /// for the same addresses. `WITCHY_REANNOTATE_UNCHANGED=1` re-annotates
    /// regardless (a kill switch for comparing the two routes).
    pub(crate) fn rewrite_reannotating_if_changed<E>(
        mut self,
        checked: CheckedAnnotation,
        rewrite: impl FnOnce(&TypeTable, &mut Module) -> Result<bool, E>,
    ) -> Result<Result<TypedModule, TypeError>, E> {
        let reusable = self.reusable_as(checked);
        let changed = rewrite(&self.table, &mut self.module)?;
        if reusable && !changed {
            return Ok(Ok(self));
        }
        Ok(checked.annotate(*self.module))
    }

    /// The checked annotation of `candidate`, a module a pass rebuilt from a
    /// copy of this one. When the rebuild reproduced this module exactly and
    /// this wrapper is itself the `checked` annotation of it, that annotation
    /// is the answer: the checker is a deterministic function of the module,
    /// so a fresh run over an equal AST computes an equal table and equal
    /// facts (keyed to `candidate`'s nodes instead of these). Equality is
    /// structural AND debug-exact, because the hand-written `PartialEq` of
    /// functions, arms and variants ignores the source lines diagnostics
    /// report. Both walks are far cheaper than the whole-module check they
    /// replace. `WITCHY_REANNOTATE_UNCHANGED=1` always re-annotates.
    pub(crate) fn reannotate_unless_equal(
        self,
        checked: CheckedAnnotation,
        candidate: Module,
    ) -> Result<TypedModule, TypeError> {
        if self.reusable_as(checked)
            && *self.module == candidate
            && format!("{:?}", self.module) == format!("{candidate:?}")
        {
            return Ok(self);
        }
        drop(self);
        checked.annotate(candidate)
    }

    fn reusable_as(&self, checked: CheckedAnnotation) -> bool {
        self.checked_as == Some(checked)
            && std::env::var_os("WITCHY_REANNOTATE_UNCHANGED").is_none()
    }

    /// Apply a potentially structural rewrite that reports whether it changed
    /// the AST (`true` whenever it mutated anything), then rebuild the
    /// address-keyed side table best-effort.
    ///
    /// A no-op on a wrapper produced by [`annotate_checked`] for this
    /// allocation keeps that wrapper, and its producer tag, whenever the
    /// best-effort check succeeds. The two runs differ only in the
    /// compiler-syntax rejection the best-effort one re-enables, which reads
    /// no state and records nothing; once it passes, the checker computes the
    /// identical table, and the retained facts are the ones the best-effort
    /// wrapper would derive from it. The tag lets a later
    /// `rewrite_reannotating_if_changed` skip its recheck of this same module.
    /// A failing best-effort check keeps the empty-table route exactly.
    /// `WITCHY_REANNOTATE_UNCHANGED=1` always re-annotates.
    pub fn rewrite_and_reannotate_if_changed(
        mut self,
        rewrite: impl FnOnce(&TypeTable, &mut Module) -> bool,
    ) -> Self {
        let reusable = self.reusable_as(CheckedAnnotation::Runtime);
        if !rewrite(&self.table, &mut self.module)
            && reusable
            && run_check_selected(&self.module, true, None, None, None, None, false).is_ok()
        {
            return self;
        }
        annotate(*self.module)
    }

    /// Perform a final, potentially structural rewrite while the old facts are
    /// available. The table is consumed with this wrapper and cannot be observed
    /// after nodes have been replaced or removed.
    pub(crate) fn rewrite_into_module<R>(
        mut self,
        rewrite: impl FnOnce(&TypeTable, &mut Module) -> R,
    ) -> (Module, TypeTable, R) {
        let result = rewrite(&self.table, &mut self.module);
        (*self.module, self.table, result)
    }
}

/// Convert a resolved checker type to the surface `ast::Type` shape the
/// backends' type-directed machinery (eq/to_string shapes, valtypes)
/// consumes. None where no surface form exists (free variables).
pub fn ty_to_ast(t: &Ty) -> Option<witchy_syntax::ast::Type> {
    ty_to_ast_with_unresolved(t, UnresolvedAst::Absent)
}

#[derive(Clone, Copy)]
enum UnresolvedAst { Absent, NeutralStorage, UnknownPureCapture }

fn ty_to_ast_with_unresolved(t: &Ty, unresolved: UnresolvedAst) -> Option<witchy_syntax::ast::Type> {
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
        // `()` is the one surface representation of unit.  This conversion
        // feeds checked declaration signatures and generated source, so
        // emitting the legacy `Nil` name here would make it re-enter the
        // canonical route after parsing has already normalized it away.
        Ty::Unit => T::Tuple(Vec::new()),
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
        Ty::Secret(rights) => {
            if !rights.seal && !rights.reveal {
                return None;
            }
            let mut args = Vec::new();
            if *rights != SecretRights::full() {
                if rights.reveal {
                    args.push(marker("Reveal"));
                }
                if rights.seal {
                    args.push(marker("Seal"));
                }
            }
            T::Named("Secret".into(), args)
        }
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
        Ty::BuildOut => T::Named("BuildOut".into(), Vec::new()),
        Ty::BuildRead => T::Named("BuildRead".into(), Vec::new()),
        Ty::BuildEnv => T::Named("BuildEnv".into(), Vec::new()),
        Ty::BuildNet => T::Named("BuildNet".into(), Vec::new()),
        Ty::BuildExec => T::Named("BuildExec".into(), Vec::new()),
        Ty::List(e) => T::Named("List".into(), vec![ty_to_ast_with_unresolved(e, unresolved)?]),
        Ty::View(payload) => T::Qualified(
            witchy_syntax::ast::TypeQual::View,
            Box::new(ty_to_ast_with_unresolved(payload, unresolved)?),
        ),
        Ty::Tuple(ts) => {
            T::Tuple(ts.iter().map(|ty| ty_to_ast_with_unresolved(ty, unresolved)).collect::<Option<Vec<_>>>()?)
        }
        Ty::Named(n, args) => T::Named(
            n.to_string(),
            args.iter().map(|ty| ty_to_ast_with_unresolved(ty, unresolved)).collect::<Option<Vec<_>>>()?,
        ),
        // (RFC-0081) The existential surface form: `dyn Render(args…)`.
        Ty::Dyn(n, args) => T::Dyn(
            n.to_string(),
            args.iter().map(|ty| ty_to_ast_with_unresolved(ty, unresolved)).collect::<Option<Vec<_>>>()?,
        ),
        Ty::Fn(params, ret, conventions, _, qualifiers) => T::Fn(
            params.iter().map(|ty| ty_to_ast_with_unresolved(ty, unresolved)).collect::<Option<Vec<_>>>()?,
            Box::new(ty_to_ast_with_unresolved(ret, unresolved)?),
            conventions.to_vec(),
            *qualifiers,
        ),
        Ty::Var(_) => match unresolved {
            UnresolvedAst::Absent => return None,
            UnresolvedAst::NeutralStorage => T::Tuple(Vec::new()),
            UnresolvedAst::UnknownPureCapture => T::Named(
                witchy_canonical_source::storage::PURE_CAPTURE_UNKNOWN.into(), Vec::new(),
            ),
        },
    })
}

fn ty_has_var(t: &Ty) -> bool {
    match t {
        Ty::Var(_) => true,
        Ty::List(e) => ty_has_var(e),
        Ty::View(payload) => ty_has_var(payload),
        Ty::Tuple(ts) => ts.iter().any(ty_has_var),
        Ty::Named(_, args) | Ty::Dyn(_, args) => args.iter().any(ty_has_var),
        Ty::Fn(ps, r, _, _, _) => ps.iter().any(ty_has_var) || ty_has_var(r),
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
    let mut typed = typed_module_checked(module, table)?;
    typed.checked_as = Some(CheckedAnnotation::Runtime);
    Ok(typed)
}

/// Lower an already checked linked source module to the ordinary typed runtime
/// AST while retaining expression-level type facts for a compiler metadata
/// consumer. Unlike [`annotate_checked`], this accepts the pre-lowering module
/// held by [`crate::pipeline::CheckedModule`].
pub fn annotate_checked_source(module: Module) -> Result<TypedModule, TypeError> {
    let checked = witchy_syntax::source_check::check(module)
        .map_err(|error| TypeError { message: error.message })?;
    let checked = witchy_syntax::generators::lower(checked)
        .map_err(|message| TypeError { message })?;
    let checked = witchy_syntax::async_lower::lower(checked)
        .map_err(|message| TypeError { message })?;
    let records = witchy_syntax::records::lower(checked)
        .map_err(|message| TypeError { message })?
        .into_module();
    let mut lowered = crate::traits::lower_checked(records)
        .map_err(|message| TypeError { message })?;
    witchy_syntax::parser::lower_sugar_module_retaining_subscripts(&mut lowered);
    annotate_checked(lowered)
}

/// Annotate the compiler-only linked source projection retained for RFC-0161.
/// The linker has already applied ordinary name/method/label resolution and
/// record normalization, but deliberately left `async` and `await` intact so
/// expression identities remain valid for typed resumable lowering.
pub fn annotate_typed_async_projection(module: Module) -> Result<TypedModule, TypeError> {
    let mut lowered = crate::traits::lower_checked(module)
        .map_err(|message| TypeError { message })?;
    witchy_syntax::parser::lower_sugar_module_retaining_subscripts(&mut lowered);
    annotate_checked(lowered)
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
    let mut typed = typed_module_checked(module, table)?;
    typed.checked_as = Some(CheckedAnnotation::BuildEntrypoint);
    Ok(typed)
}

fn annotate_with_conversion_fns(
    module: Module,
    from_conversion_fns: Option<&HashSet<String>>,
) -> TypedModule {
    let table = best_effort_table(&module, from_conversion_fns);
    typed_module_best_effort(module, table)
}

/// The recorded type table of a whole-module check, or an empty table when
/// the module does not check: best-effort annotation never reports an error.
fn best_effort_table(module: &Module, from_conversion_fns: Option<&HashSet<String>>) -> TypeTable {
    match run_check_selected(
        module,
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
    }
}

/// The checked type table of a trait-lowering intermediate, and nothing else.
///
/// `traits::lower_with` annotates the module once before trait lowering and
/// again after every monomorphization round. It reads only the table (through
/// `rewrite_preserving_nodes` / `rewrite_into_module`, which consumes the
/// wrapper), never the loan, access or view facts `typed_module_best_effort`
/// derives -- whole-module walks that were rebuilt every round and dropped
/// unread. A wrapper built here carries none, so every fact accessor answers
/// `Missing` rather than a stale or invented graph.
pub(crate) fn annotate_types_with_from_conversions(
    module: Module,
    from_conversion_fns: &HashSet<String>,
) -> TypedModule {
    let table = best_effort_table(&module, Some(from_conversion_fns));
    TypedModule {
        module: Box::new(module),
        table,
        loan_facts: None,
        access_facts: None,
        view_facts: None,
        checked_as: None,
    }
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
    // Source-check entry points carry the pre-lowering trait catalog. Internal
    // re-annotation and trait-lowering probes do not, and must not reapply a
    // source-only exported-signature rule to their generated functions.
    let enforce_public_return_annotations = trait_method_names.is_some();
    // Task itself requires disposition, while its future result is not a
    // stored recipe field. Every generic storage mask comes from the shared
    // source judgment, including collection and declared aggregate children.
    let stored_obligations = StoredObligationCatalog::from_module(module)
        .ok_or_else(|| TypeError {
            message: "checked Task declaration has inconsistent execution-obligation arity".into(),
        })?;
    let canonical_definitions = witchy_canonical_source::aggregates::type_definitions(module);
    let storage_plans = checker::shared_storage_plans(&canonical_definitions);
    let mut c = Checker {
        canonical_definitions,
        resolve_cache: std::cell::RefCell::new(HashMap::new()),
        storage_plans,
        once_callable_types: std::cell::RefCell::new(HashMap::new()),
        unique_storage_types: std::cell::RefCell::new(HashMap::new()),
        type_record: if record { Some(HashMap::new()) } else { None },
        compiler_owned_materializations: HashSet::new(),
        // Construction requests are also retained during ordinary checking so
        // capability payloads resolved only by the ending substitution are
        // rejected before annotation or lowering.
        existential_pack_record: Some(HashMap::new()),
        existential_upcast_record: Some(HashMap::new()),
        record_projection_record: Some(HashMap::new()),
        fn_sigs: HashMap::new(),
        async_body_returns: HashMap::new(),
        fn_qualifiers: HashMap::new(),
        from_conversion_fns: from_conversion_fns.cloned().unwrap_or_default(),
        fn_conventions: HashMap::new(),
        fn_exclusive_reference_params: HashMap::new(),
        fn_frozen_value_params: HashMap::new(),
        fn_unique_contracts: HashMap::new(),
        fresh_result_fns: HashSet::new(),
        inferred_owner_bindings: vec![HashSet::new()],
        fn_opt_provenance: HashMap::new(),
        frozen_record_fields: HashMap::new(),
        unique_record_fields: HashMap::new(),
        unique_source_record_fields: HashMap::new(),
        unique_source_record_params: HashMap::new(),
        unique_ctor_field_types: HashMap::new(),
        ctor_source_type_params: HashMap::new(),
        fn_param_names: HashMap::new(),
        ctor_sigs: HashMap::new(),
        ctor_typarams: HashMap::new(),
        record_fields: HashMap::new(),
        borrowed_nominal_types: module
            .items
            .iter()
            .filter_map(|item| {
                let Item::Type(definition) = item else { return None };
                definition
                    .params
                    .iter()
                    .any(|parameter| ast::is_lifetime_param(parameter))
                    .then(|| definition.name.clone())
            })
            .collect(),
        borrowed_nominal_relation_fields: module
            .items
            .iter()
            .filter_map(|item| {
                let Item::Type(definition) = item else { return None };
                if !definition
                    .params
                    .iter()
                    .any(|parameter| ast::is_lifetime_param(parameter))
                {
                    return None;
                }
                let fields = definition
                    .variants
                    .iter()
                    .flat_map(|variant| variant.field_names.iter().zip(&variant.fields))
                    .filter(|(_, field)| type_contains_nominal_lifetime_relation(field))
                    .map(|(name, _)| name.clone())
                    .collect();
                Some((definition.name.clone(), fields))
            })
            .collect(),
        explicit_reference_nominal_types: module
            .items
            .iter()
            .filter_map(|item| {
                let Item::Type(definition) = item else { return None };
                definition
                    .params
                    .iter()
                    .any(|parameter| ast::is_lifetime_param(parameter))
                    .then_some(definition)
            })
            .filter(|definition| {
                definition
                    .variants
                    .iter()
                    .flat_map(|variant| &variant.fields)
                    .any(type_contains_explicit_reference_relation)
            })
            .map(|definition| definition.name.clone())
            .collect(),
        borrowed_shell_update_target: None,
        must_self_update_target: None,
        borrowed_shell_bindings: vec![HashSet::new()],
        explicit_reference_bindings: vec![HashSet::new()],
        frozen_bindings: vec![HashSet::new()],
        unique_value_bindings: vec![HashSet::new()],
        unique_storage_bindings: vec![HashSet::new()],
        opt_viewed_roots: HashSet::new(),
        unique_binding_types: vec![HashMap::new()],
        callable_unique_contracts: vec![HashMap::new()],
        callable_opt_provenance: vec![HashMap::new()],
        container_opt_provenance: vec![HashMap::new()],
        sealed_types: HashSet::new(),
        construction_sealed_types: HashSet::new(),
        transparent_externref_brands: HashMap::new(),
        gc_cap_aggregates: HashSet::new(),
        adt_variants: HashMap::new(),
        fn_typarams: HashMap::new(),
        fn_storage_obligations: HashMap::new(),
        current_storage_obligations: Vec::new(),
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
        once_invoked: HashSet::new(),
        stored_obligations,
        must_live: HashSet::new(),
        must_borrowed: HashSet::new(),
        once_borrowed: HashSet::new(),
        region_locals: Vec::new(),
        region_promotions: HashMap::new(),
        region_packed_layouts: region_packed_layout_descriptors(module),
        current_ret: None,
        current_pure: false,
        pure_var_parameters: HashSet::new(),
        current_isolated_callback: None,
        dict_key_ops: Vec::new(),
        cur_line: 0,
        cur_module: String::new(),
        entry_module: module
            .linked_entry
            .clone()
            .unwrap_or_else(|| detect_entry_module(module)),
        compiler_syntax_allowed,
        opt_mode: false,
        current_opt_callable: false,
    };

    let option_a = c.fresh();
    let option_id = match option_a {
        Ty::Var(id) => id,
        _ => unreachable!("fresh type variable"),
    };
    let option_result = Ty::Named("Option".into(), vec![option_a.clone()].into());
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
    let result = Ty::Named("Result".into(), vec![result_ok.clone(), result_err.clone()].into());
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
    // Linking qualifies stdlib nominals (`task.Task`), while a standalone
    // source-check fixture can still carry the prelude spelling (`Task`). An
    // async callable's public result must use the declaration identity present
    // in this checked module; hard-coding either spelling makes the retained
    // pre-CPS projection disagree with ordinary linked calls.
    let async_task_type = if type_defs.contains_key("task.Task") {
        "task.Task"
    } else {
        "Task"
    };
    for item in &module.items {
        match item {
            Item::Function(f) => {
                let mut vars: HashMap<String, Ty> = HashMap::new();
                // Bracketed declaration parameters bind these exact signature
                // variables before any inferred lowercase parameter is seen.
                // They are not formatter-only AST metadata.
                for parameter in &f.type_params {
                    let variable = c.fresh();
                    vars.insert(parameter.clone(), variable);
                }
                let params: Vec<Ty> = f
                    .params
                    .iter()
                    .map(|p| match &p.ty {
                        Some(t) => c.to_ty_generic(t, &mut vars),
                        None => c.fresh(),
                    })
                    .collect();
                let body_ret = match &f.ret {
                    Some(t) => c.to_ty_generic(t, &mut vars),
                    None => c.fresh(),
                };
                let ret = if f.is_async {
                    c.async_body_returns.insert(f.name.clone(), body_ret.clone());
                    Ty::Named(async_task_type.into(), vec![body_ret].into())
                } else {
                    body_ret
                };
                c.fn_sigs.insert(f.name.clone(), (params, ret));
                c.fn_qualifiers.insert(
                    f.name.clone(),
                    CallableQualifiers::new(f.pure, false),
                );
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
                c.fn_exclusive_reference_params.insert(
                    f.name.clone(),
                    f.params.iter().map(parameter_binds_exclusive_reference).collect(),
                );
                c.fn_frozen_value_params.insert(
                    f.name.clone(),
                    f.params
                        .iter()
                        .map(|parameter| parameter.ty.as_ref().is_some_and(is_frozen_value_type))
                        .collect(),
                );
                c.fn_unique_contracts.insert(
                    f.name.clone(),
                    UniqueCallableContract {
                        unique_params: f
                            .params
                            .iter()
                            .map(|parameter| parameter.ty.as_ref().is_some_and(is_unique_value_type))
                            .collect(),
                        unique_result: f.ret.as_ref().is_some_and(is_unique_value_type),
                        frozen_params: f
                            .params
                            .iter()
                            .map(|parameter| parameter.ty.as_ref().is_some_and(is_frozen_value_type))
                            .collect(),
                        frozen_result: f.ret.as_ref().is_some_and(is_frozen_value_type),
                    },
                );
                if witchy_canonical_source::source::source_execution_mode(f, &module.modes).is_opt() {
                    c.fn_opt_provenance
                        .insert(f.name.clone(), OptCallableContract::from_function(f));
                }
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
                // Keep lifetime parameters in constructor result identities as
                // well as ordinary type parameters.  They are erased from
                // field value types, but a nominal value such as `Pair('a)`
                // must still unify with its constructor pattern and call ABI.
                let param_names = ast::effective_nominal_type_def_params(t);
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
                let result = Ty::Named(t.name.clone().into(), result_args.into());
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
                        let frozen_fields = variant
                            .field_names
                            .iter()
                            .zip(&variant.fields)
                            .filter_map(|(name, ty)| {
                                is_frozen_value_type(ty).then_some(name.clone())
                            })
                            .collect();
                        c.frozen_record_fields.insert(t.name.clone(), frozen_fields);
                        let unique_fields = variant
                            .field_names
                            .iter()
                            .zip(&variant.fields)
                            .filter_map(|(name, ty)| {
                                is_unique_value_type(ty).then_some(name.clone())
                            })
                            .collect();
                        c.unique_record_fields.insert(t.name.clone(), unique_fields);
                        c.unique_source_record_fields.insert(
                            t.name.clone(),
                            variant
                                .field_names
                                .iter()
                                .cloned()
                                .zip(variant.fields.iter().cloned())
                                .collect(),
                        );
                        c.unique_source_record_params
                            .insert(t.name.clone(), param_names.clone());
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
                    c.ctor_source_type_params
                        .insert(variant.name.clone(), param_names.clone());
                    c.unique_ctor_field_types.insert(
                        variant.name.clone(),
                        variant.fields.clone(),
                    );
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
    // (RFC-0167) Fresh-storage results are inferred by fixpoint over bodies:
    // a wrapper around a fresh allocator is itself a fresh allocator.
    let mut rounds = 0;
    loop {
        let mut changed = false;
        for item in &module.items {
            let Item::Function(f) = item else { continue };
            if c.fresh_result_fns.contains(&f.name) {
                continue;
            }
            if block_yields_fresh_storage(&f.body, &c.fresh_result_fns) {
                c.fresh_result_fns.insert(f.name.clone());
                changed = true;
            }
        }
        rounds += 1;
        if !changed || rounds > 8 {
            break;
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

    // RFC-0138 first cut: explicit reference handles do not retain the
    // referent-root consumption fact required by `once fn`. Reject these
    // relations module-wide before a skipped template or unused declaration can
    // preserve an unsound signature.
    for item in &module.items {
        match item {
            Item::Function(function) => {
                c.current_typarams = c
                    .fn_typarams
                    .get(&function.name)
                    .map(|parameters| parameters.iter().cloned().collect())
                    .unwrap_or_default();
                for parameter in &function.params {
                    if let Some(declared) = &parameter.ty {
                        c.reject_explicit_once_reference_type(
                            declared,
                            &format!(
                                "parameter `{}` of `{}`",
                                parameter.name,
                                diagnostic_callable_name(&function.name),
                            ),
                        )?;
                    }
                }
                if let Some(result) = &function.ret {
                    c.reject_explicit_once_reference_type(
                        result,
                        &format!(
                            "return type of `{}`",
                            diagnostic_callable_name(&function.name),
                        ),
                    )?;
                }
            }
            Item::Type(definition) => {
                c.current_typarams.clear();
                for field in definition
                    .variants
                    .iter()
                    .flat_map(|variant| &variant.fields)
                {
                    c.reject_explicit_once_reference_type(
                        field,
                        &format!("type `{}`", definition.name),
                    )?;
                }
            }
            Item::TypeAlias { name, ty, .. } => {
                c.current_typarams.clear();
                c.reject_explicit_once_reference_type(
                    ty,
                    &format!("type alias `{name}`"),
                )?;
            }
            Item::Trait(_) | Item::Impl(_) | Item::Const { .. } | Item::Comptime(_) => {}
        }
    }
    c.current_typarams.clear();

    // `main` is the root entrypoint: its parameters are where the host's authority
    // enters, so they must be capabilities (or the args list) — validate before
    // diving into bodies so a malformed entry point is reported up front.
    check_dynamic_method_declarations(module)?;
    check_target_availability(module)?;
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
            Item::Function(f) => {
                let source_callable = witchy_syntax::suspension::source_callable_name(f);
                c.opt_mode = witchy_canonical_source::source::source_execution_mode(f, &module.modes).is_opt();
                c.check_function(f)
                    .map_err(|e| at_loc(e, c.cur_line, &source_callable, &c.cur_module))?
            }
            Item::Type(_) | Item::Trait(_) | Item::Impl(_) | Item::Const { .. } | Item::TypeAlias { .. } | Item::Comptime(_) => {}
        }
    }
    if enforce_public_return_annotations {
        c.check_public_return_annotations(module)?;
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
        let mut view_types = HashMap::new();
        let mut local_callees = HashMap::new();
        let mut non_completing = HashSet::new();
        for (k, (ty, cannot_complete, local_callee)) in rec {
            if cannot_complete {
                non_completing.insert(k);
            }
            let resolved = c.resolve(&ty);
            if !ty_has_var(&resolved) {
                types.insert(k, resolved);
            } else if matches!(resolved, Ty::View(_)) {
                view_types.insert(k, resolved);
            }
            if let Some(callee) = local_callee {
                let resolved = c.resolve(&callee);
                if matches!(resolved, Ty::Fn(..)) && !ty_has_var(&resolved) {
                    local_callees.insert(k, resolved);
                }
            }
        }
        let functions = c
            .fn_sigs
            .iter()
            .filter_map(|(name, (params, result))| {
                let params = params.iter().map(|param| c.resolve(param)).collect::<Vec<_>>();
                let result = c.resolve(result);
                let conventions = c
                    .fn_conventions
                    .get(name)
                    .cloned()
                    .unwrap_or_else(|| vec![Convention::Let; params.len()]);
                let references = c
                    .fn_exclusive_reference_params
                    .get(name)
                    .cloned()
                    .unwrap_or_else(|| vec![false; params.len()]);
                let qualifiers = c
                    .fn_qualifiers
                    .get(name)
                    .copied()
                    .unwrap_or(CallableQualifiers::ORDINARY);
                (!params.iter().any(ty_has_var) && !ty_has_var(&result)).then(|| {
                    (
                        name.clone(),
                        Ty::Fn(params.into(), Box::new(result).into(), conventions.into(), references.into(), qualifiers),
                    )
                })
            })
            .collect();
        return Ok(Some(TypeTable {
            types,
            compiler_owned_materializations: c.compiler_owned_materializations,
            view_types,
            local_callees,
            non_completing,
            functions,
            existential_packs,
            existential_upcasts,
            record_projections,
            region_promotions: c.region_promotions,
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

/// Compatibility-only parser/check boundary for historical reference syntax.
/// Ordinary source must use [`check_str`], which deliberately rejects explicit
/// lifetimes, `View(T, 'a)`, and `&mut`. Migration tooling and legacy semantic
/// fixtures may opt into this parser explicitly while the source is rewritten.
#[doc(hidden)]
pub fn check_reference_migration_str(src: &str) -> Result<(), String> {
    let module = witchy_syntax::parser::parse_reference_migration(src)
        .map_err(|e| e.to_string())?;
    check(&module).map_err(|e| e.to_string())
}
