//! Canonical source-level callable ABI identities.
//!
//! This module deliberately knows nothing about WIR.  It projects the final
//! checked type representation into a deterministic, tagged description that
//! can be bound to a later candidate by a consumer.  Function names and
//! backend-assigned IDs are never part of the projection.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use witchy_syntax::ast::{self, Convention, Item, Type};

use crate::typeck::{Ty, TypedModule};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CanonicalScalarKind {
    Int,
    Float,
    Duration,
    Bool,
    Unit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CanonicalCapabilityKind {
    Console,
    Clock,
    Rand,
    Env,
    Secret,
    Exec,
    Fetch,
    Dir,
    File,
    Net,
    Socket,
    Listener,
    BuildOut,
    BuildRead,
    BuildEnv,
    BuildNet,
    BuildExec,
    SecretStore,
}

/// A structural identity for a checked source type used by a reference or
/// aggregate ABI entry.  It is not a WIR layout identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CanonicalTypeKey([u8; 32]);

impl CanonicalTypeKey {
    pub fn as_bytes(self) -> [u8; 32] {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CanonicalReferenceKind {
    String,
    Bytes,
    Aggregate(CanonicalTypeKey),
    Capability(CanonicalCapabilityKind),
    /// A local rooted-view lane.  The payload key binds the checked payload
    /// identity; the carrier kind says how the retained root/projection is
    /// represented.  This is deliberately not an open, separately compiled
    /// reference ABI.
    RootedView {
        payload: CanonicalTypeKey,
        carrier: CanonicalRootedViewCarrier,
    },
}

/// Physical family selected for an authenticated rooted view.  New payloads
/// require a new explicit carrier family; they may not reuse the scalar cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CanonicalRootedViewCarrier {
    IntScalarRoot,
    /// Retained `List(Int)` root plus checked `{start, len}` projection.
    /// This remains local-only: it is not an open List ABI and exposes no
    /// by-value element read.
    IntListSliceRoot,
    /// A whole immutable `String` value.  The carrier is the retained
    /// reference-counted String object itself: a whole-value view has no
    /// projection, and the String representation is already immutable and
    /// shared by retain, so the view is a value-semantic snapshot of its root.
    StringRoot,
    /// A whole immutable user record or sum (argument-less nominal).  Like
    /// `StringRoot`, the carrier is the retained aggregate object itself: a
    /// record is an immutable value shared by retain, so the token is the
    /// snapshot.  The payload identity is the declaration's nominal ordinal.
    RecordRoot,
    /// A whole immutable `List` of scalars or `String`s.  The carrier is the
    /// retained list object: a later mutation of the owner finds it shared and
    /// copies, so the view keeps its snapshot.
    ListRoot,
    /// A view of an immutable `Float`, `Bool`, or `Duration` value is carried
    /// as that value: an immutable scalar read at formation is exactly what the
    /// view observes, the RFC's proven-equivalent representation.  `&Int`
    /// keeps its `IntScalarRoot` cell, which also carries list element views.
    ScalarValue,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CanonicalValueKind {
    Scalar(CanonicalScalarKind),
    Reference(CanonicalReferenceKind),
    Callable(CanonicalCallableAbiKey),
    Value(CanonicalTypeKey),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CanonicalParameterConvention {
    Let,
    Borrow,
    Var,
    Own,
    VarUnique,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CanonicalParameterAbi {
    pub convention: CanonicalParameterConvention,
    pub value: CanonicalValueKind,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CanonicalResultKind {
    Scalar(CanonicalScalarKind),
    Reference(CanonicalReferenceKind),
    Value(CanonicalValueKind),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CanonicalCallableQualifiers {
    pub pure: bool,
    pub once: bool,
}

/// A source-structural local target.  The ordinal is assigned only from the
/// final checked function sequence, never from a function spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CanonicalLocalTarget {
    pub function_ordinal: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CanonicalTargetPolicy {
    ClosedLocal(CanonicalLocalTarget),
    OpenAbi,
}

/// Hash identity of a complete source-level callable ABI shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CanonicalCallableAbiKey([u8; 32]);

impl CanonicalCallableAbiKey {
    pub fn as_bytes(self) -> [u8; 32] {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct CanonicalCallableAbi {
    pub key: CanonicalCallableAbiKey,
    pub parameters: Vec<CanonicalParameterAbi>,
    pub result: CanonicalResultKind,
    pub qualifiers: CanonicalCallableQualifiers,
    pub target_policy: CanonicalTargetPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalCallableAbiManifest {
    entries: Vec<CanonicalCallableAbi>,
    digest: [u8; 32],
}

impl CanonicalCallableAbiManifest {
    /// The manifest of a checked source whose runtime declarations could not
    /// be encoded (an RFC-0165 obligation recorded by the pipeline). It binds
    /// no callable and its digest is the digest of no entries, so nothing can
    /// mistake it for a derived manifest of the same program.
    pub fn empty() -> Self {
        let entries = Vec::new();
        let digest = manifest_digest(&entries);
        Self { entries, digest }
    }

    pub fn entries(&self) -> &[CanonicalCallableAbi] {
        &self.entries
    }

    pub fn digest(&self) -> [u8; 32] {
        self.digest
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CanonicalAbiError {
    NonConcrete(String),
    Unsupported(String),
    InvalidCallable(String),
}

impl fmt::Display for CanonicalAbiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonConcrete(context) => write!(f, "canonical callable ABI is not concrete: {context}"),
            Self::Unsupported(context) => write!(f, "canonical callable ABI has unsupported type: {context}"),
            Self::InvalidCallable(context) => write!(f, "canonical callable ABI is invalid: {context}"),
        }
    }
}

impl std::error::Error for CanonicalAbiError {}

struct Context {
    nominal_ordinals: BTreeMap<String, u32>,
    type_declarations: std::collections::BTreeSet<String>,
    /// The module's type definitions, so a record-root view is admitted by the
    /// same rule lowering and authority apply (`is_record_root_payload`).
    definitions: BTreeMap<String, ast::TypeDef>,
}

impl Context {
    fn from_module(module: &ast::Module) -> Self {
        let nominal_ordinals = module
            .items
            .iter()
            .enumerate()
            .filter_map(|(ordinal, item)| match item {
                Item::Type(definition) => Some((definition.name.clone(), ordinal as u32)),
                Item::TypeAlias { name, .. } => Some((name.clone(), ordinal as u32)),
                _ => None,
            })
            .collect();
        let type_declarations = module
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Type(definition) => Some(definition.name.clone()),
                _ => None,
            })
            .collect();
        let definitions = crate::canonical_aggregates::type_definitions(module);
        Self { nominal_ordinals, type_declarations, definitions }
    }

    /// A `type` declaration (not a `type X = ...` alias): only these can be
    /// whole-record view roots.
    fn declares_type(&self, name: &str) -> bool {
        self.type_declarations.contains(name)
    }

    fn nominal(&self, name: &str) -> Result<u32, CanonicalAbiError> {
        self.nominal_ordinals.get(name).copied().ok_or_else(|| {
            CanonicalAbiError::Unsupported(format!("unresolved nominal type `{name}`"))
        })
    }
}

pub(crate) fn canonical_checked_ty_bytes(
    typed: &TypedModule,
    ty: &Ty,
) -> Result<Vec<u8>, CanonicalAbiError> {
    let context = Context::from_module(typed.module());
    let mut bytes = Vec::new();
    encode_ty(ty, &context, &mut bytes)?;
    Ok(bytes)
}

pub(crate) fn canonical_ast_type_bytes(
    typed: &TypedModule,
    ty: &Type,
) -> Result<Vec<u8>, CanonicalAbiError> {
    let context = Context::from_module(typed.module());
    let mut bytes = Vec::new();
    encode_ast(ty, &context, &mut bytes)?;
    Ok(bytes)
}

/// Derive the complete callable ABI manifest from the final checked module.
/// Declarations contribute closed local targets. Concrete expression types
/// contribute open ABI entries, including nested function types.
/// Preserve the checked declaration's resource qualifier, erased by HM value
/// types. This never substitutes annotation payloads for checked closed types.
pub(crate) fn checked_declaration_signature(typed: &TypedModule, name: &str) -> Result<Type, CanonicalAbiError> {
    let mut signature = typed.table().function_type(name).ok_or_else(||
        CanonicalAbiError::InvalidCallable(format!("checked declaration `{name}` has no finalized signature")))?;
    let declaration = typed.module().items.iter().find_map(|item| match item {
        Item::Function(function) if function.name == name => Some(function),
        _ => None,
    }).ok_or_else(|| CanonicalAbiError::InvalidCallable(format!("missing checked declaration `{name}`")))?;
    let Type::Fn(parameters, result, conventions, _) = &mut signature else {
        return Err(CanonicalAbiError::InvalidCallable("checked declaration is not callable".into()));
    };
    if parameters.len() != declaration.params.len() {
        return Err(CanonicalAbiError::InvalidCallable("checked declaration parameter arity".into()));
    }
    for (index, (parameter, declared)) in parameters.iter_mut().zip(&declaration.params).enumerate() {
        if declared.convention == Convention::Var
            && matches!(&declared.ty, Some(Type::Qualified(ast::TypeQual::Unique, _)))
        {
            if conventions.get(index) != Some(&Convention::Var) {
                return Err(CanonicalAbiError::InvalidCallable("checked unique write-back convention".into()));
            }
            *parameter = Type::Qualified(ast::TypeQual::Unique, Box::new(parameter.unqualified().clone()));
        }
    }
    if let Some(declared) = declaration.ret.as_ref()
        && let Some(physical) = ast::direct_unique_result_ok_collection_physical_type(declared)
        && result.as_ref() == &physical
    {
        // The finalized value type establishes the physical Result shape;
        // restore only the exact checked source carrier relation that its
        // canonical ABI needs to authenticate the returned owned wrapper.
        *result = Box::new(declared.clone());
    }
    Ok(signature)
}

pub fn derive_manifest(
    typed: &TypedModule,
    closed_functions: &[String],
) -> Result<CanonicalCallableAbiManifest, CanonicalAbiError> {
    let context = Context::from_module(typed.module());
    let mut entries = BTreeSet::new();
    for (ordinal, name) in closed_functions.iter().enumerate() {
        let function_ordinal = u32::try_from(ordinal).map_err(|_| {
            CanonicalAbiError::InvalidCallable("function ordinal overflow".into())
        })?;
        let signature = checked_declaration_signature(typed, name)?;
        let entry = callable_from_ast(
            &signature,
            CanonicalTargetPolicy::ClosedLocal(CanonicalLocalTarget { function_ordinal }),
            &context,
        )?;
        entries.insert(entry);
    }
    for ty in typed.table().concrete_types() {
        collect_ty_callables(ty, CanonicalTargetPolicy::OpenAbi, &context, &mut entries)?;
    }
    let entries = entries.into_iter().collect::<Vec<_>>();
    let digest = manifest_digest(&entries);
    Ok(CanonicalCallableAbiManifest { entries, digest })
}

fn collect_ty_callables(
    ty: &Ty,
    policy: CanonicalTargetPolicy,
    context: &Context,
    entries: &mut BTreeSet<CanonicalCallableAbi>,
) -> Result<(), CanonicalAbiError> {
    match ty {
        Ty::Fn(..) => {
            // A concrete checked function type can describe an external
            // callable slot.  A rooted view has no separately compiled ABI:
            // only the closed declaration entry, source-bound below, can
            // install its retained root-plus-projection carrier.  Do not
            // accidentally promote a local view callable into OpenAbi.
            if !contains_rooted_view(ty) {
                entries.insert(callable_from_ty(ty, policy, context)?);
            }
        }
        Ty::List(element) => collect_ty_callables(element, CanonicalTargetPolicy::OpenAbi, context, entries)?,
        Ty::Tuple(items) | Ty::Named(_, items) | Ty::Dyn(_, items) => {
            for item in items.iter() {
                collect_ty_callables(item, CanonicalTargetPolicy::OpenAbi, context, entries)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn callable_from_ty(
    ty: &Ty,
    target_policy: CanonicalTargetPolicy,
    context: &Context,
) -> Result<CanonicalCallableAbi, CanonicalAbiError> {
    let Ty::Fn(params, result, conventions, _, qualifiers) = ty else {
        return Err(CanonicalAbiError::InvalidCallable("expected function type".into()));
    };
    if !conventions.is_empty() && conventions.len() != params.len() {
        return Err(CanonicalAbiError::InvalidCallable("parameter convention arity does not match parameter type arity".into()));
    }
    let parameters = params
        .iter()
        .enumerate()
        .map(|(index, ty)| {
            Ok(CanonicalParameterAbi {
                convention: convention(conventions.get(index).copied().unwrap_or_default()),
                value: value_from_ty(ty, context)?,
            })
        })
        .collect::<Result<Vec<_>, CanonicalAbiError>>()?;
    let result = result_from_value(value_from_ty(result, context)?)?;
    let qualifiers = CanonicalCallableQualifiers { pure: qualifiers.pure, once: qualifiers.once };
    let bytes = encode_callable_parts(&parameters, &result, qualifiers);
    Ok(CanonicalCallableAbi {
        key: CanonicalCallableAbiKey(*blake3::hash(&bytes).as_bytes()),
        parameters,
        result,
        qualifiers,
        target_policy,
    })
}

fn callable_from_ast(
    ty: &Type,
    target_policy: CanonicalTargetPolicy,
    context: &Context,
) -> Result<CanonicalCallableAbi, CanonicalAbiError> {
    let Type::Fn(params, result, conventions, qualifiers) = ty.unqualified() else {
        return Err(CanonicalAbiError::InvalidCallable("finalized declaration is not a function type".into()));
    };
    if !conventions.is_empty() && conventions.len() != params.len() {
        return Err(CanonicalAbiError::InvalidCallable("parameter convention arity does not match parameter type arity".into()));
    }
    let parameters = params
        .iter()
        .enumerate()
        .map(|(index, ty)| {
            Ok(CanonicalParameterAbi {
                convention: if conventions.get(index) == Some(&Convention::Var)
                    && matches!(ty, Type::Qualified(ast::TypeQual::Unique, _))
                { CanonicalParameterConvention::VarUnique }
                else { convention(conventions.get(index).copied().unwrap_or_default()) },
                value: value_from_ast(ty, context)?,
            })
        })
        .collect::<Result<Vec<_>, CanonicalAbiError>>()?;
    let result = result_from_value(value_from_ast(result, context)?)?;
    let qualifiers = CanonicalCallableQualifiers { pure: qualifiers.pure, once: qualifiers.once };
    let bytes = encode_callable_parts(&parameters, &result, qualifiers);
    Ok(CanonicalCallableAbi {
        key: CanonicalCallableAbiKey(*blake3::hash(&bytes).as_bytes()),
        parameters,
        result,
        qualifiers,
        target_policy,
    })
}

fn result_from_value(value: CanonicalValueKind) -> Result<CanonicalResultKind, CanonicalAbiError> {
    match value {
        CanonicalValueKind::Scalar(kind) => Ok(CanonicalResultKind::Scalar(kind)),
        CanonicalValueKind::Reference(kind) => Ok(CanonicalResultKind::Reference(kind)),
        other @ (CanonicalValueKind::Callable(_) | CanonicalValueKind::Value(_)) => {
            Ok(CanonicalResultKind::Value(other))
        }
    }
}

fn convention(convention: Convention) -> CanonicalParameterConvention {
    match convention {
        Convention::Let => CanonicalParameterConvention::Let,
        Convention::Borrow => CanonicalParameterConvention::Borrow,
        Convention::Var => CanonicalParameterConvention::Var,
        Convention::Own => CanonicalParameterConvention::Own,
    }
}

fn value_from_ty(ty: &Ty, context: &Context) -> Result<CanonicalValueKind, CanonicalAbiError> {
    let mut bytes = Vec::new();
    encode_ty(ty, context, &mut bytes)?;
    let key = CanonicalTypeKey(*blake3::hash(&bytes).as_bytes());
    Ok(match ty {
        Ty::Int => CanonicalValueKind::Scalar(CanonicalScalarKind::Int),
        Ty::Float => CanonicalValueKind::Scalar(CanonicalScalarKind::Float),
        Ty::Duration => CanonicalValueKind::Scalar(CanonicalScalarKind::Duration),
        Ty::Bool => CanonicalValueKind::Scalar(CanonicalScalarKind::Bool),
        Ty::Unit => CanonicalValueKind::Scalar(CanonicalScalarKind::Unit),
        Ty::String => CanonicalValueKind::Reference(CanonicalReferenceKind::String),
        Ty::Bytes => CanonicalValueKind::Reference(CanonicalReferenceKind::Bytes),
        Ty::Fn(..) => CanonicalValueKind::Callable(CanonicalCallableAbiKey(*blake3::hash(&bytes).as_bytes())),
        Ty::View(payload) => CanonicalValueKind::Reference(rooted_view_from_ty(payload, context)?),
        Ty::Console(_) | Ty::Clock | Ty::Rand | Ty::Env | Ty::Secret(_) | Ty::Exec | Ty::Fetch
        | Ty::Dir(_) | Ty::File(_) | Ty::Net(_) | Ty::Socket | Ty::Listener | Ty::BuildOut
        | Ty::BuildRead | Ty::BuildEnv | Ty::BuildNet | Ty::BuildExec => {
            CanonicalValueKind::Reference(CanonicalReferenceKind::Capability(capability_kind(ty)))
        }
        Ty::Named(name, arguments) if name.as_ref() == "SecretStore" && arguments.is_empty() => {
            CanonicalValueKind::Reference(CanonicalReferenceKind::Capability(CanonicalCapabilityKind::SecretStore))
        }
        Ty::List(_) | Ty::Named(_, _) | Ty::Dyn(_, _) => {
            CanonicalValueKind::Reference(CanonicalReferenceKind::Aggregate(key))
        }
        Ty::Tuple(_) => CanonicalValueKind::Value(key),
        Ty::Var(id) => return Err(CanonicalAbiError::NonConcrete(format!("type variable {id}"))),
        Ty::Msg => CanonicalValueKind::Value(key),
    })
}

fn value_from_ast(ty: &Type, context: &Context) -> Result<CanonicalValueKind, CanonicalAbiError> {
    if let Type::Qualified(ast::TypeQual::View, payload) = ty {
        return Ok(CanonicalValueKind::Reference(rooted_view_from_ast(payload, context)?));
    }
    let mut bytes = Vec::new();
    encode_ast(ty, context, &mut bytes)?;
    let key = CanonicalTypeKey(*blake3::hash(&bytes).as_bytes());
    let ty = ty.unqualified();
    Ok(match ty {
        Type::Named(name, args) if name == "Int" && args.is_empty() => CanonicalValueKind::Scalar(CanonicalScalarKind::Int),
        Type::Named(name, args) if name == "Float" && args.is_empty() => CanonicalValueKind::Scalar(CanonicalScalarKind::Float),
        Type::Named(name, args) if name == "Duration" && args.is_empty() => CanonicalValueKind::Scalar(CanonicalScalarKind::Duration),
        Type::Named(name, args) if name == "Bool" && args.is_empty() => CanonicalValueKind::Scalar(CanonicalScalarKind::Bool),
        Type::Named(name, args) if (name == "Nil" || name == "Unit") && args.is_empty() => CanonicalValueKind::Scalar(CanonicalScalarKind::Unit),
        Type::Tuple(items) if items.is_empty() => CanonicalValueKind::Scalar(CanonicalScalarKind::Unit),
        Type::Named(name, args) if name == "String" && args.is_empty() => CanonicalValueKind::Reference(CanonicalReferenceKind::String),
        Type::Named(name, args) if name == "Bytes" && args.is_empty() => CanonicalValueKind::Reference(CanonicalReferenceKind::Bytes),
        Type::Fn(..) => CanonicalValueKind::Callable(CanonicalCallableAbiKey(*blake3::hash(&bytes).as_bytes())),
        Type::Named(name, _) if is_capability_name(name) => CanonicalValueKind::Reference(CanonicalReferenceKind::Capability(capability_name(name))),
        Type::Named(_, _) | Type::Tuple(_) | Type::Slice(_) | Type::Dyn(_, _) => CanonicalValueKind::Reference(CanonicalReferenceKind::Aggregate(key)),
        Type::RecordCompose { .. } => return Err(CanonicalAbiError::Unsupported("record composition survived type checking".into())),
        Type::Qualified(_, _) => unreachable!("Type::unqualified removes qualifiers"),
    })
}

fn rooted_view_from_ty(
    payload: &Ty,
    context: &Context,
) -> Result<CanonicalReferenceKind, CanonicalAbiError> {
    match payload {
        Ty::Int => Ok(CanonicalReferenceKind::RootedView {
            payload: rooted_scalar_payload_key(CanonicalScalarKind::Int),
            carrier: CanonicalRootedViewCarrier::IntScalarRoot,
        }),
        Ty::Named(name, arguments)
            if name.as_ref() == "Slice"
                && arguments.len() == 1
                && matches!(arguments.get(0), Some(Ty::Int)) =>
        {
            Ok(CanonicalReferenceKind::RootedView {
            payload: rooted_slice_int_payload_key(),
            carrier: CanonicalRootedViewCarrier::IntListSliceRoot,
            })
        }
        Ty::String => Ok(CanonicalReferenceKind::RootedView {
            payload: rooted_string_payload_key(),
            carrier: CanonicalRootedViewCarrier::StringRoot,
        }),
        Ty::Float | Ty::Bool | Ty::Duration => Ok(CanonicalReferenceKind::RootedView {
            payload: rooted_scalar_value_payload_key(match payload {
                Ty::Float => CanonicalScalarKind::Float,
                Ty::Bool => CanonicalScalarKind::Bool,
                _ => CanonicalScalarKind::Duration,
            }),
            carrier: CanonicalRootedViewCarrier::ScalarValue,
        }),
        Ty::List(element) => {
            let name = match element.as_ref() {
                Ty::Int => "Int", Ty::Float => "Float", Ty::Duration => "Duration",
                Ty::Bool => "Bool", Ty::String => "String",
                _ => return Err(CanonicalAbiError::Unsupported(
                    "rooted view payload has no admitted authenticated carrier".into(),
                )),
            };
            rooted_list_view(name)
        }
        Ty::Named(..) => {
            if !crate::typeck::ty_to_ast(payload).is_some_and(|payload| {
                crate::canonical_aggregates::is_record_root_payload(&payload, &context.definitions)
            }) {
                return Err(CanonicalAbiError::Unsupported(
                    "rooted view payload has no admitted authenticated carrier".into(),
                ));
            }
            let mut identity = Vec::new();
            record_identity_ty(payload, context, &mut identity)?;
            Ok(CanonicalReferenceKind::RootedView {
                payload: rooted_record_payload_key(&identity),
                carrier: CanonicalRootedViewCarrier::RecordRoot,
            })
        }
        _ => Err(CanonicalAbiError::Unsupported(
            "rooted view payload has no admitted authenticated carrier".into(),
        )),
    }
}

fn rooted_view_from_ast(
    payload: &Type,
    context: &Context,
) -> Result<CanonicalReferenceKind, CanonicalAbiError> {
    match payload.unqualified() {
        Type::Named(name, arguments) if name == "Int" && arguments.is_empty() => Ok(CanonicalReferenceKind::RootedView {
            payload: rooted_scalar_payload_key(CanonicalScalarKind::Int),
            carrier: CanonicalRootedViewCarrier::IntScalarRoot,
        }),
        Type::Slice(element)
            if matches!(element.unqualified(), Type::Named(name, arguments) if name == "Int" && arguments.is_empty()) =>
        {
            Ok(CanonicalReferenceKind::RootedView {
                payload: rooted_slice_int_payload_key(),
                carrier: CanonicalRootedViewCarrier::IntListSliceRoot,
            })
        }
        Type::Named(name, arguments)
            if name == "Slice"
                && matches!(arguments.as_slice(), [element]
                    if matches!(element.unqualified(), Type::Named(name, arguments)
                        if name == "Int" && arguments.is_empty())) =>
        {
            Ok(CanonicalReferenceKind::RootedView {
                payload: rooted_slice_int_payload_key(),
                carrier: CanonicalRootedViewCarrier::IntListSliceRoot,
            })
        }
        Type::Named(name, arguments) if name == "String" && arguments.is_empty() => Ok(CanonicalReferenceKind::RootedView {
            payload: rooted_string_payload_key(),
            carrier: CanonicalRootedViewCarrier::StringRoot,
        }),
        Type::Named(name, arguments)
            if arguments.is_empty() && matches!(name.as_str(), "Float" | "Bool" | "Duration") =>
        {
            Ok(CanonicalReferenceKind::RootedView {
                payload: rooted_scalar_value_payload_key(match name.as_str() {
                    "Float" => CanonicalScalarKind::Float,
                    "Bool" => CanonicalScalarKind::Bool,
                    _ => CanonicalScalarKind::Duration,
                }),
                carrier: CanonicalRootedViewCarrier::ScalarValue,
            })
        }
        Type::Named(name, arguments) if name == "List" && arguments.len() == 1 => {
            let Type::Named(element, element_arguments) = arguments[0].unqualified() else {
                return Err(CanonicalAbiError::Unsupported(
                    "rooted view payload has no admitted authenticated carrier".into(),
                ));
            };
            if !element_arguments.is_empty() {
                return Err(CanonicalAbiError::Unsupported(
                    "rooted view payload has no admitted authenticated carrier".into(),
                ));
            }
            rooted_list_view(element)
        }
        Type::Named(name, arguments)
            if !(arguments.is_empty() && matches!(name.as_str(), "Int" | "Float" | "Duration" | "Bool" | "String")) =>
        {
            // A record root is a head; scalar and String leaves appear only as
            // its type arguments.
            if !crate::canonical_aggregates::is_record_root_payload(payload.unqualified(), &context.definitions) {
                return Err(CanonicalAbiError::Unsupported(
                    "rooted view payload has no admitted authenticated carrier".into(),
                ));
            }
            let mut identity = Vec::new();
            record_identity_ast(payload.unqualified(), context, &mut identity)?;
            Ok(CanonicalReferenceKind::RootedView {
                payload: rooted_record_payload_key(&identity),
                carrier: CanonicalRootedViewCarrier::RecordRoot,
            })
        }
        _ => Err(CanonicalAbiError::Unsupported(
            "rooted view payload has no admitted authenticated carrier".into(),
        )),
    }
}

/// Rooted views need one payload identity regardless of whether the caller
/// reached the boundary through a checked HM type or a checked declaration
/// annotation.  Those encodings deliberately differ elsewhere, so never hash
/// either representation here.  Each admitted carrier family gets an explicit
/// source-level payload identity instead.
fn rooted_scalar_payload_key(kind: CanonicalScalarKind) -> CanonicalTypeKey {
    let mut bytes = b"witchy.canonical-rooted-view-payload.v1".to_vec();
    bytes.push(kind as u8);
    CanonicalTypeKey(*blake3::hash(&bytes).as_bytes())
}

/// Builtin heads and capabilities are never record roots; only a declared
/// user type (resolved through the nominal ordinal table) is.
fn is_user_record_name(name: &str) -> bool {
    !matches!(name, "Int" | "Float" | "Duration" | "Bool" | "Nil" | "Unit" | "String" | "Bytes"
        | "List" | "Dict" | "Option" | "Result" | "Slice" | "SecretStore")
        && !is_capability_name(name)
        && crate::typeck::anon_union_synthetic_variants(name).is_none()
}

fn rooted_record_payload_key(identity: &[u8]) -> CanonicalTypeKey {
    let mut bytes = b"witchy.canonical-rooted-view-payload.v2/record".to_vec();
    bytes.extend_from_slice(identity);
    CanonicalTypeKey(*blake3::hash(&bytes).as_bytes())
}

/// Record-root payload identity, written identically from a checked type and
/// from a declaration annotation (the general encoders deliberately differ).
/// A head is a declared user type (its nominal ordinal) or builtin `Option` /
/// `Result`; an argument is a scalar, `String`, or another such head. Anything
/// else has no admitted record-root identity.
const RECORD_IDENTITY_USER: u8 = 1;
const RECORD_IDENTITY_OPTION: u8 = 2;
const RECORD_IDENTITY_RESULT: u8 = 3;
const RECORD_IDENTITY_LEAF: u8 = 4;

fn record_head(name: &str, arity: usize, context: &Context, bytes: &mut Vec<u8>) -> Result<(), CanonicalAbiError> {
    match (name, arity) {
        ("Option", 1) => bytes.push(RECORD_IDENTITY_OPTION),
        ("Result", 2) => bytes.push(RECORD_IDENTITY_RESULT),
        _ if is_user_record_name(name) && context.declares_type(name) => {
            bytes.push(RECORD_IDENTITY_USER);
            bytes.extend_from_slice(&context.nominal(name)?.to_le_bytes());
        }
        _ => return Err(CanonicalAbiError::Unsupported(
            "rooted view payload has no admitted authenticated carrier".into(),
        )),
    }
    bytes.extend_from_slice(&(arity as u32).to_le_bytes());
    Ok(())
}

fn record_leaf(name: &str, bytes: &mut Vec<u8>) -> bool {
    let tag = match name {
        "Int" => 1, "Float" => 2, "Duration" => 3, "Bool" => 4, "String" => 5,
        _ => return false,
    };
    bytes.extend_from_slice(&[RECORD_IDENTITY_LEAF, tag]);
    true
}

fn record_identity_ty(ty: &Ty, context: &Context, bytes: &mut Vec<u8>) -> Result<(), CanonicalAbiError> {
    match ty {
        Ty::Int => { record_leaf("Int", bytes); }
        Ty::Float => { record_leaf("Float", bytes); }
        Ty::Duration => { record_leaf("Duration", bytes); }
        Ty::Bool => { record_leaf("Bool", bytes); }
        Ty::String => { record_leaf("String", bytes); }
        Ty::Named(name, arguments) => {
            record_head(name, arguments.len(), context, bytes)?;
            for argument in arguments.iter() {
                record_identity_ty(argument, context, bytes)?;
            }
        }
        _ => return Err(CanonicalAbiError::Unsupported(
            "rooted view payload has no admitted authenticated carrier".into(),
        )),
    }
    Ok(())
}

fn record_identity_ast(ty: &Type, context: &Context, bytes: &mut Vec<u8>) -> Result<(), CanonicalAbiError> {
    let Type::Named(name, arguments) = ty.unqualified() else {
        return Err(CanonicalAbiError::Unsupported(
            "rooted view payload has no admitted authenticated carrier".into(),
        ));
    };
    if arguments.is_empty() && record_leaf(name, bytes) {
        return Ok(());
    }
    record_head(name, arguments.len(), context, bytes)?;
    for argument in arguments {
        record_identity_ast(argument, context, bytes)?;
    }
    Ok(())
}

/// A `ListRoot` view of a list whose elements are one scalar kind or String.
fn rooted_list_view(element: &str) -> Result<CanonicalReferenceKind, CanonicalAbiError> {
    let mut bytes = b"witchy.canonical-rooted-view-payload.v1/list".to_vec();
    if !record_leaf(element, &mut bytes) {
        return Err(CanonicalAbiError::Unsupported(
            "rooted view payload has no admitted authenticated carrier".into(),
        ));
    }
    Ok(CanonicalReferenceKind::RootedView {
        payload: CanonicalTypeKey(*blake3::hash(&bytes).as_bytes()),
        carrier: CanonicalRootedViewCarrier::ListRoot,
    })
}

fn rooted_scalar_value_payload_key(kind: CanonicalScalarKind) -> CanonicalTypeKey {
    let mut bytes = b"witchy.canonical-rooted-view-payload.v1/scalar-value".to_vec();
    bytes.push(kind as u8);
    CanonicalTypeKey(*blake3::hash(&bytes).as_bytes())
}

fn rooted_string_payload_key() -> CanonicalTypeKey {
    CanonicalTypeKey(*blake3::hash(b"witchy.canonical-rooted-view-payload.v1/string").as_bytes())
}

fn rooted_slice_int_payload_key() -> CanonicalTypeKey {
    CanonicalTypeKey(*blake3::hash(b"witchy.canonical-rooted-view-payload.v1/slice-int").as_bytes())
}

fn contains_rooted_view(ty: &Ty) -> bool {
    match ty {
        Ty::View(_) => true,
        Ty::List(item) => contains_rooted_view(item),
        Ty::Tuple(items) | Ty::Named(_, items) | Ty::Dyn(_, items) => {
            items.iter().any(contains_rooted_view)
        }
        Ty::Fn(parameters, result, _, _, _) => {
            parameters.iter().any(contains_rooted_view) || contains_rooted_view(result)
        }
        _ => false,
    }
}

fn encode_callable_parts(parameters: &[CanonicalParameterAbi], result: &CanonicalResultKind, qualifiers: CanonicalCallableQualifiers) -> Vec<u8> {
    let mut bytes = b"witchy.canonical-callable-abi.v1".to_vec();
    put_len(&mut bytes, parameters.len());
    for parameter in parameters {
        bytes.push(match parameter.convention { CanonicalParameterConvention::Let => 0, CanonicalParameterConvention::Borrow => 1, CanonicalParameterConvention::Var => 2, CanonicalParameterConvention::Own => 3, CanonicalParameterConvention::VarUnique => 4 });
        encode_value(&mut bytes, &parameter.value);
    }
    encode_result(&mut bytes, result);
    bytes.push(u8::from(qualifiers.pure));
    bytes.push(u8::from(qualifiers.once));
    bytes
}

fn encode_value(bytes: &mut Vec<u8>, value: &CanonicalValueKind) {
    match value {
        CanonicalValueKind::Scalar(kind) => { bytes.push(0); bytes.push(*kind as u8); }
        CanonicalValueKind::Reference(kind) => { bytes.push(1); encode_reference(bytes, kind); }
        CanonicalValueKind::Callable(key) => { bytes.push(2); bytes.extend_from_slice(&key.0); }
        CanonicalValueKind::Value(key) => { bytes.push(3); bytes.extend_from_slice(&key.0); }
    }
}

fn encode_result(bytes: &mut Vec<u8>, result: &CanonicalResultKind) {
    match result {
        CanonicalResultKind::Scalar(kind) => { bytes.push(0); bytes.push(*kind as u8); }
        CanonicalResultKind::Reference(kind) => { bytes.push(1); encode_reference(bytes, kind); }
        CanonicalResultKind::Value(value) => { bytes.push(2); encode_value(bytes, value); }
    }
}

fn encode_reference(bytes: &mut Vec<u8>, kind: &CanonicalReferenceKind) {
    match kind {
        CanonicalReferenceKind::String => bytes.push(0),
        CanonicalReferenceKind::Bytes => bytes.push(1),
        CanonicalReferenceKind::Aggregate(key) => { bytes.push(2); bytes.extend_from_slice(&key.0); }
        CanonicalReferenceKind::Capability(kind) => { bytes.push(3); bytes.push(*kind as u8); }
        CanonicalReferenceKind::RootedView { payload, carrier } => {
            bytes.push(4);
            bytes.extend_from_slice(&payload.0);
            bytes.push(*carrier as u8);
        }
    }
}

fn manifest_digest(entries: &[CanonicalCallableAbi]) -> [u8; 32] {
    let mut bytes = b"witchy.canonical-callable-abi-manifest.v1".to_vec();
    put_len(&mut bytes, entries.len());
    for entry in entries {
        bytes.extend_from_slice(&entry.key.0);
        let policy = match entry.target_policy { CanonicalTargetPolicy::ClosedLocal(target) => { bytes.push(0); target.function_ordinal } CanonicalTargetPolicy::OpenAbi => { bytes.push(1); 0 } };
        bytes.extend_from_slice(&policy.to_le_bytes());
    }
    *blake3::hash(&bytes).as_bytes()
}

fn put_len(bytes: &mut Vec<u8>, length: usize) {
    bytes.extend_from_slice(&(u32::try_from(length).expect("validated canonical ABI length")).to_le_bytes());
}

fn encode_ty(ty: &Ty, context: &Context, bytes: &mut Vec<u8>) -> Result<(), CanonicalAbiError> {
    match ty {
        Ty::Int => bytes.push(0), Ty::Float => bytes.push(1), Ty::Duration => bytes.push(2), Ty::String => bytes.push(3), Ty::Bytes => bytes.push(4), Ty::Msg => bytes.push(5), Ty::Bool => bytes.push(6), Ty::Unit => bytes.push(7),
        Ty::Console(rights) => { bytes.push(8); bytes.push(u8::from(rights.read)); bytes.push(u8::from(rights.write)); }
        Ty::Clock => bytes.push(9), Ty::Rand => bytes.push(10), Ty::Env => bytes.push(11),
        Ty::Secret(rights) => { bytes.push(12); bytes.push(u8::from(rights.reveal)); bytes.push(u8::from(rights.seal)); }
        Ty::Exec => bytes.push(13), Ty::Fetch => bytes.push(14),
        Ty::Dir(rights) => { bytes.push(15); bytes.push(u8::from(rights.read)); bytes.push(u8::from(rights.write)); }
        Ty::File(rights) => { bytes.push(16); bytes.push(u8::from(rights.read)); bytes.push(u8::from(rights.write)); }
        Ty::Net(rights) => { bytes.push(17); bytes.push(u8::from(rights.connect)); bytes.push(u8::from(rights.listen)); bytes.push(u8::from(rights.tcp)); bytes.push(u8::from(rights.udp)); bytes.push(u8::from(rights.uds)); }
        Ty::Socket => bytes.push(18), Ty::Listener => bytes.push(19), Ty::BuildOut => bytes.push(20), Ty::BuildRead => bytes.push(21), Ty::BuildEnv => bytes.push(22), Ty::BuildNet => bytes.push(23), Ty::BuildExec => bytes.push(24),
        Ty::List(element) => { bytes.push(25); encode_ty(element, context, bytes)?; }
        Ty::View(payload) => { bytes.push(32); encode_ty(payload, context, bytes)?; }
        Ty::Tuple(items) => { bytes.push(26); put_len(bytes, items.len()); for item in items.iter() { encode_ty(item, context, bytes)?; } }
        Ty::Named(name, args) if name.as_ref() == "SecretStore" && args.is_empty() => { bytes.push(30); }
        Ty::Named(name, args) if crate::typeck::anon_union_synthetic_variants(name).is_some() => {
            encode_anonymous_union_ty(name, args, context, bytes)?;
        }
        Ty::Named(name, args) => { bytes.push(27); bytes.extend_from_slice(&context.nominal(name)?.to_le_bytes()); put_len(bytes, args.len()); for arg in args.iter() { encode_ty(arg, context, bytes)?; } }
        Ty::Dyn(name, args) => { bytes.push(28); bytes.extend_from_slice(&context.nominal(name)?.to_le_bytes()); put_len(bytes, args.len()); for arg in args.iter() { encode_ty(arg, context, bytes)?; } }
        Ty::Fn(params, result, conventions, mutable, qualifiers) => { bytes.push(29); put_len(bytes, params.len()); for (index, param) in params.iter().enumerate() { bytes.push(convention(conventions.get(index).copied().unwrap_or_default()) as u8); bytes.push(u8::from(mutable.get(index).copied().unwrap_or(false))); encode_ty(param, context, bytes)?; } encode_ty(result, context, bytes)?; bytes.push(u8::from(qualifiers.pure)); bytes.push(u8::from(qualifiers.once)); }
        Ty::Var(id) => return Err(CanonicalAbiError::NonConcrete(format!("type variable {id}"))),
    }
    Ok(())
}

/// Anonymous unions are checked structural types, not declarations, so their
/// synthetic head must never be looked up in the nominal declaration table.
/// Encode the validated tag schema and its flattened payload lanes directly.
fn encode_anonymous_union_ty(
    name: &str,
    args: &[Ty],
    context: &Context,
    bytes: &mut Vec<u8>,
) -> Result<(), CanonicalAbiError> {
    let variants = crate::typeck::anon_union_synthetic_variants(name).ok_or_else(|| {
        CanonicalAbiError::InvalidCallable("invalid anonymous-union head".into())
    })?;
    let expected = variants.iter().map(|(_, arity)| *arity).sum::<usize>();
    if expected != args.len() {
        return Err(CanonicalAbiError::InvalidCallable(format!(
            "anonymous union head has {expected} payload lanes, got {}", args.len()
        )));
    }
    bytes.push(31);
    put_len(bytes, variants.len());
    for (tag, arity) in variants {
        put_len(bytes, tag.len());
        bytes.extend_from_slice(tag.as_bytes());
        put_len(bytes, arity);
    }
    put_len(bytes, args.len());
    for arg in args {
        encode_ty(arg, context, bytes)?;
    }
    Ok(())
}

fn encode_ast(ty: &Type, context: &Context, bytes: &mut Vec<u8>) -> Result<(), CanonicalAbiError> {
    if let Type::Qualified(ast::TypeQual::View, payload) = ty {
        bytes.push(6);
        encode_ast(payload, context, bytes)?;
        return Ok(());
    }
    match ty.unqualified() {
        Type::Named(name, args) => {
            if crate::typeck::anon_union_synthetic_variants(name).is_some() {
                return encode_anonymous_union_ast(name, args, context, bytes);
            }
            bytes.push(0);
            let tag = match (name.as_str(), args.is_empty()) {
                ("Int", true) => 0,
                ("Float", true) => 1,
                ("Duration", true) => 2,
                ("String", true) => 3,
                ("Bytes", true) => 4,
                ("Bool", true) => 5,
                ("Nil" | "Unit", true) => 6,
                _ if is_capability_name(name) => 7 + capability_name(name) as u32,
                _ => builtin_nominal_tag(name, args.len())?
                    .map_or_else(|| context.nominal(name).map(|ordinal| ordinal + 64), Ok)?,
            };
            bytes.extend_from_slice(&tag.to_le_bytes());
            if is_capability_name(name) {
                // A capability's type arguments are its rights (`Dir[Read]`),
                // names rather than types: encode each by name, in sorted
                // order so `Dir[Read, Write]` and `Dir[Write, Read]` are one
                // identity.
                let mut rights = Vec::with_capacity(args.len());
                for arg in args {
                    let Type::Named(right, inner) = arg.unqualified() else {
                        return Err(CanonicalAbiError::Unsupported(format!("capability right `{arg:?}` is not a name")));
                    };
                    if !inner.is_empty() {
                        return Err(CanonicalAbiError::Unsupported(format!("capability right `{arg:?}` carries arguments")));
                    }
                    rights.push(right.as_str());
                }
                rights.sort_unstable();
                rights.dedup();
                put_len(bytes, rights.len());
                for right in rights {
                    put_len(bytes, right.len());
                    bytes.extend_from_slice(right.as_bytes());
                }
                return Ok(());
            }
            put_len(bytes, args.len());
            for arg in args { encode_ast(arg, context, bytes)?; }
        }
        Type::Tuple(items) => { bytes.push(1); put_len(bytes, items.len()); for item in items { encode_ast(item, context, bytes)?; } }
        Type::Slice(item) => { bytes.push(2); encode_ast(item, context, bytes)?; }
        Type::RecordCompose { .. } => return Err(CanonicalAbiError::Unsupported("record composition survived type checking".into())),
        Type::Fn(params, result, conventions, qualifiers) => { bytes.push(3); put_len(bytes, params.len()); for (index, param) in params.iter().enumerate() { bytes.push(convention(conventions.get(index).copied().unwrap_or_default()) as u8); encode_ast(param, context, bytes)?; } encode_ast(result, context, bytes)?; bytes.push(u8::from(qualifiers.pure)); bytes.push(u8::from(qualifiers.once)); }
        Type::Dyn(name, args) => { bytes.push(4); bytes.extend_from_slice(&(context.nominal(name)?).to_le_bytes()); put_len(bytes, args.len()); for arg in args { encode_ast(arg, context, bytes)?; } }
        Type::Qualified(_, _) => unreachable!("Type::unqualified removes qualifiers"),
    }
    Ok(())
}

fn encode_anonymous_union_ast(
    name: &str,
    args: &[Type],
    context: &Context,
    bytes: &mut Vec<u8>,
) -> Result<(), CanonicalAbiError> {
    let variants = crate::typeck::anon_union_synthetic_variants(name).ok_or_else(|| {
        CanonicalAbiError::InvalidCallable("invalid anonymous-union head".into())
    })?;
    let expected = variants.iter().map(|(_, arity)| *arity).sum::<usize>();
    if expected != args.len() {
        return Err(CanonicalAbiError::InvalidCallable(format!(
            "anonymous union head has {expected} payload lanes, got {}", args.len()
        )));
    }
    bytes.push(5);
    put_len(bytes, variants.len());
    for (tag, arity) in variants {
        put_len(bytes, tag.len());
        bytes.extend_from_slice(tag.as_bytes());
        put_len(bytes, arity);
    }
    put_len(bytes, args.len());
    for arg in args {
        encode_ast(arg, context, bytes)?;
    }
    Ok(())
}

fn capability_kind(ty: &Ty) -> CanonicalCapabilityKind {
    match ty { Ty::Console(_) => CanonicalCapabilityKind::Console, Ty::Clock => CanonicalCapabilityKind::Clock, Ty::Rand => CanonicalCapabilityKind::Rand, Ty::Env => CanonicalCapabilityKind::Env, Ty::Secret(_) => CanonicalCapabilityKind::Secret, Ty::Exec => CanonicalCapabilityKind::Exec, Ty::Fetch => CanonicalCapabilityKind::Fetch, Ty::Dir(_) => CanonicalCapabilityKind::Dir, Ty::File(_) => CanonicalCapabilityKind::File, Ty::Net(_) => CanonicalCapabilityKind::Net, Ty::Socket => CanonicalCapabilityKind::Socket, Ty::Listener => CanonicalCapabilityKind::Listener, Ty::BuildOut => CanonicalCapabilityKind::BuildOut, Ty::BuildRead => CanonicalCapabilityKind::BuildRead, Ty::BuildEnv => CanonicalCapabilityKind::BuildEnv, Ty::BuildNet => CanonicalCapabilityKind::BuildNet, Ty::BuildExec => CanonicalCapabilityKind::BuildExec, _ => unreachable!("not a capability") }
}

fn builtin_nominal_tag(name: &str, arity: usize) -> Result<Option<u32>, CanonicalAbiError> {
    let expected_and_tag = match name {
        "str" => Some((0, 32)),
        "Option" => Some((1, 33)),
        "Result" => Some((2, 34)),
        "Dict" => Some((2, 35)),
        "Ordering" => Some((0, 36)),
        "Set" => Some((1, 37)),
        "Iter" => Some((1, 38)),
        "List" => Some((1, 39)),
        _ => None,
    };
    let Some((expected, tag)) = expected_and_tag else { return Ok(None) };
    if arity != expected {
        return Err(CanonicalAbiError::InvalidCallable(format!(
            "builtin `{name}` ABI type expects {expected} arguments, got {arity}"
        )));
    }
    Ok(Some(tag))
}

fn is_capability_name(name: &str) -> bool { matches!(name, "Console" | "Clock" | "Rand" | "Env" | "Secret" | "SecretStore" | "Exec" | "Fetch" | "Dir" | "File" | "Net" | "Socket" | "Listener" | "BuildOut" | "BuildRead" | "BuildEnv" | "BuildNet" | "BuildExec") }
fn capability_name(name: &str) -> CanonicalCapabilityKind { match name { "Console" => CanonicalCapabilityKind::Console, "Clock" => CanonicalCapabilityKind::Clock, "Rand" => CanonicalCapabilityKind::Rand, "Env" => CanonicalCapabilityKind::Env, "Secret" => CanonicalCapabilityKind::Secret, "SecretStore" => CanonicalCapabilityKind::SecretStore, "Exec" => CanonicalCapabilityKind::Exec, "Fetch" => CanonicalCapabilityKind::Fetch, "Dir" => CanonicalCapabilityKind::Dir, "File" => CanonicalCapabilityKind::File, "Net" => CanonicalCapabilityKind::Net, "Socket" => CanonicalCapabilityKind::Socket, "Listener" => CanonicalCapabilityKind::Listener, "BuildOut" => CanonicalCapabilityKind::BuildOut, "BuildRead" => CanonicalCapabilityKind::BuildRead, "BuildEnv" => CanonicalCapabilityKind::BuildEnv, "BuildNet" => CanonicalCapabilityKind::BuildNet, "BuildExec" => CanonicalCapabilityKind::BuildExec, _ => unreachable!("checked capability name") } }

#[cfg(test)]
mod tests {
    use super::*;
    use crate::typeck;

    fn typed(source: &str) -> TypedModule {
        let module = witchy_syntax::parser::parse_module(source).expect("parse");
        typeck::annotate_checked(module).expect("type check")
    }

    fn manifest(source: &str) -> CanonicalCallableAbiManifest {
        let typed = typed(source);
        let functions = typed
            .module()
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Function(function) => Some(function.name.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        derive_manifest(&typed, &functions).expect("manifest")
    }

    #[test]
    fn function_rename_does_not_change_manifest() {
        let a = manifest("fn apply(value: Int) -> Int:\n    value\n\nfn main() -> Int:\n    apply(1)\n");
        let b = manifest("fn renamed(value: Int) -> Int:\n    value\n\nfn main() -> Int:\n    renamed(1)\n");
        assert_eq!(a.digest(), b.digest());
    }

    #[test]
    fn registry_secret_store_callable_abi_is_closed_and_kind_bound() {
        let store = manifest("fn keep(store: SecretStore) -> SecretStore:\n    store\n");
        let secret = manifest("fn keep(secret: Secret) -> Secret:\n    secret\n");
        let clock = manifest("fn keep(clock: Clock) -> Clock:\n    clock\n");
        assert_ne!(store.digest(), secret.digest());
        assert_ne!(store.digest(), clock.digest());
        assert_ne!(secret.digest(), clock.digest());
    }

    #[test]
    fn convention_and_result_type_change_manifest() {
        let base = manifest("fn apply(value: Int) -> Int:\n    value\n\nfn main() -> Int:\n    apply(1)\n");
        let convention = manifest("fn apply(var value: Int) -> Int:\n    value\n\nfn main() -> Int:\n    var n = 1\n    apply(n)\n");
        let result = manifest("fn apply(value: Int) -> String:\n    \"x\"\n\nfn main() -> Int:\n    let ignored = apply(1)\n    0\n");
        assert_ne!(base.digest(), convention.digest());
        assert_ne!(base.digest(), result.digest());
    }

    #[test]
    fn rooted_view_callable_metadata_binds_payload_and_stays_closed_local() {
        let rooted = manifest(
            "fn relay(view: &Int) -> &Int:\n    view\n",
        );
        let plain = manifest(
            "fn relay(value: Int) -> Int:\n    value\n",
        );
        assert_ne!(rooted.digest(), plain.digest(), "a rooted carrier must not erase to Int");
        assert!(
            rooted.entries().iter().all(|entry| !matches!(entry.target_policy, CanonicalTargetPolicy::OpenAbi)),
            "a rooted callable has only its authenticated local declaration ABI",
        );
        let relay = rooted.entries().iter().find(|entry| {
            matches!(entry.target_policy, CanonicalTargetPolicy::ClosedLocal(target) if target.function_ordinal == 0)
        }).expect("closed relay ABI");
        let CanonicalValueKind::Reference(CanonicalReferenceKind::RootedView {
            payload: parameter_payload,
            carrier: CanonicalRootedViewCarrier::IntScalarRoot,
        }) = &relay.parameters[0].value else {
            panic!("relay parameter must retain the rooted Int carrier");
        };
        let CanonicalResultKind::Reference(CanonicalReferenceKind::RootedView {
            payload: result_payload,
            carrier: CanonicalRootedViewCarrier::IntScalarRoot,
        }) = &relay.result else {
            panic!("relay result must retain the rooted Int carrier");
        };
        assert_eq!(parameter_payload, result_payload, "the exact payload identity crosses the local callable");
    }

    #[test]
    fn rooted_view_payload_identity_is_checked_and_declaration_invariant() {
        let typed = typed("fn relay(view: &Int) -> &Int:\n    view\n");
        let context = Context::from_module(typed.module());
        let checked = rooted_view_from_ty(&Ty::Int, &context)
            .expect("admitted checked rooted Int payload");
        let signature = checked_declaration_signature(&typed, "relay")
            .expect("checked declaration signature");
        let Type::Fn(parameters, _, _, _) = signature else {
            panic!("callable signature");
        };
        let declared = rooted_view_from_ast(&parameters[0], &context)
            .expect("admitted declaration rooted Int payload");
        assert_eq!(checked, declared, "&Int has one payload key at every source boundary");
    }

    #[test]
    fn rooted_slice_payload_identity_is_checked_and_declaration_invariant() {
        let typed = typed("fn relay(view: &[Int]) -> &[Int]:\n    view\n");
        let context = Context::from_module(typed.module());
        let checked = rooted_view_from_ty(
            &Ty::Named("Slice".into(), vec![Ty::Int].into()),
            &context,
        ).expect("admitted checked rooted Int slice payload");
        let signature = checked_declaration_signature(&typed, "relay")
            .expect("checked declaration signature");
        let Type::Fn(parameters, _, _, _) = signature else {
            panic!("callable signature");
        };
        let Type::Qualified(ast::TypeQual::View, declared_payload) = &parameters[0] else {
            panic!("rooted slice parameter");
        };
        let declared = rooted_view_from_ast(declared_payload, &context)
            .expect("admitted declaration rooted Int slice payload");
        assert_eq!(checked, declared, "&[Int] has one payload key at every source boundary");

        // `&List(Int)` is a whole-list view (`ListRoot`), never the slice
        // carrier: it acquires no projection authority.
        let list = rooted_view_from_ty(&Ty::List(std::rc::Rc::new(Ty::Int)), &context)
            .expect("&List(Int) is a whole-list view");
        assert!(matches!(list, CanonicalReferenceKind::RootedView { carrier: CanonicalRootedViewCarrier::ListRoot, .. }),
            "&List(Int) uses the ListRoot carrier: {list:?}");
        assert_ne!(list, checked, "&List(Int) must not acquire slice projection authority");
    }

    #[test]
    fn rooted_view_tags_do_not_collide_with_anonymous_unions() {
        let rooted = typed("fn relay(view: &Int) -> &Int:\n    view\n");
        let union = typed(
            "fn select(choice: .[Left(Int) | Right(Int)]) -> Int:\n    match choice:\n        .Left(value) -> value\n        .Right(value) -> value\n",
        );

        let rooted_checked = canonical_checked_ty_bytes(
            &rooted,
            &Ty::View(std::rc::Rc::new(Ty::Int)),
        ).expect("rooted checked type encoding");
        let union_checked = union.table().concrete_types()
            .find(|ty| matches!(ty, Ty::Named(name, _) if crate::typeck::anon_union_synthetic_variants(name).is_some()))
            .and_then(|ty| canonical_checked_ty_bytes(&union, ty).ok())
            .expect("anonymous union checked type encoding");
        assert_eq!(rooted_checked.first(), Some(&32));
        assert_eq!(union_checked.first(), Some(&31));
        assert_ne!(rooted_checked, union_checked);

        let rooted_signature = checked_declaration_signature(&rooted, "relay")
            .expect("rooted declaration signature");
        let union_signature = checked_declaration_signature(&union, "select")
            .expect("anonymous union declaration signature");
        let Type::Fn(rooted_parameters, _, _, _) = rooted_signature else {
            panic!("rooted callable signature");
        };
        let Type::Fn(union_parameters, _, _, _) = union_signature else {
            panic!("anonymous union callable signature");
        };
        let rooted_ast = canonical_ast_type_bytes(&rooted, &rooted_parameters[0])
            .expect("rooted declaration type encoding");
        let union_ast = canonical_ast_type_bytes(&union, &union_parameters[0])
            .expect("anonymous union declaration type encoding");
        assert_eq!(rooted_ast.first(), Some(&6));
        assert_eq!(union_ast.first(), Some(&5));
        assert_ne!(rooted_ast, union_ast);
    }

    #[test]
    fn checked_declaration_signature_retains_exact_result_unique_ok_carrier() {
        let typed = typed(
            "fn split(var values: unique List(Int), succeed: Bool) -> Result(unique List(Int), String):\n    if succeed:\n        return Ok([7])\n    Err(\"no result\")\n",
        );
        let signature = checked_declaration_signature(&typed, "split")
            .expect("exact Result unique Ok declaration signature");
        let Type::Fn(_, result, _, _) = signature else { panic!("callable signature") };
        assert!(ast::is_direct_unique_result_ok_collection(&result));
    }
}
