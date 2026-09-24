//! Canonical source-level callable ABI identities.
//!
//! This module deliberately knows nothing about WIR.  It projects the final
//! checked type representation into a deterministic, tagged description that
//! can be bound to a later candidate by a consumer.  Function names and
//! backend-assigned IDs are never part of the projection.

use std::collections::{BTreeMap, BTreeSet};

use witchy_syntax::ast::{self, Convention, Item, Type};

use crate::typeck::{Ty, TypedModule};
use witchy_canonical_source::abi::*;


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
        let definitions = witchy_canonical_source::aggregates::type_definitions(module);
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
    Ok(CanonicalCallableAbiManifest::from_entries(entries.into_iter().collect()))
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
        key: CanonicalCallableAbiKey::from_digest(*blake3::hash(&bytes).as_bytes()),
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
        key: CanonicalCallableAbiKey::from_digest(*blake3::hash(&bytes).as_bytes()),
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
    let key = CanonicalTypeKey::from_digest(*blake3::hash(&bytes).as_bytes());
    Ok(match ty {
        Ty::Int => CanonicalValueKind::Scalar(CanonicalScalarKind::Int),
        Ty::Float => CanonicalValueKind::Scalar(CanonicalScalarKind::Float),
        Ty::Duration => CanonicalValueKind::Scalar(CanonicalScalarKind::Duration),
        Ty::Bool => CanonicalValueKind::Scalar(CanonicalScalarKind::Bool),
        Ty::Unit => CanonicalValueKind::Scalar(CanonicalScalarKind::Unit),
        Ty::String => CanonicalValueKind::Reference(CanonicalReferenceKind::String),
        Ty::Bytes => CanonicalValueKind::Reference(CanonicalReferenceKind::Bytes),
        Ty::Fn(..) => CanonicalValueKind::Callable(CanonicalCallableAbiKey::from_digest(*blake3::hash(&bytes).as_bytes())),
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
    let key = CanonicalTypeKey::from_digest(*blake3::hash(&bytes).as_bytes());
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
        Type::Fn(..) => CanonicalValueKind::Callable(CanonicalCallableAbiKey::from_digest(*blake3::hash(&bytes).as_bytes())),
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
        Ty::Tuple(items) if matches!(items.as_ref(), [Ty::Int, Ty::String]) => {
            Ok(CanonicalReferenceKind::RootedView {
                payload: rooted_tuple_int_string_payload_key(),
                carrier: CanonicalRootedViewCarrier::TupleRoot,
            })
        }
        Ty::Named(name, arguments)
            if name.as_ref() == "Dict"
                && matches!(arguments.as_ref(), [Ty::String, Ty::Int]) =>
        {
            Ok(CanonicalReferenceKind::RootedView {
                payload: rooted_dict_string_int_payload_key(),
                carrier: CanonicalRootedViewCarrier::DictRoot,
            })
        }
        Ty::Int => Ok(CanonicalReferenceKind::RootedView {
            payload: rooted_scalar_payload_key(CanonicalScalarKind::Int),
            carrier: CanonicalRootedViewCarrier::IntScalarRoot,
        }),
        Ty::Named(name, arguments)
            if name.as_ref() == "Slice"
                && arguments.len() == 1
                && arguments.first().and_then(slice_element_name_ty).is_some() =>
        {
            let element = arguments.first().and_then(slice_element_name_ty).expect("matched above");
            Ok(CanonicalReferenceKind::RootedView {
                payload: rooted_slice_payload_key(element),
                carrier: CanonicalRootedViewCarrier::ListSliceRoot,
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
                witchy_canonical_source::aggregates::is_record_root_payload(&payload, &context.definitions)
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
        exact if witchy_canonical_source::aggregates::is_tuple_int_string_root_payload(exact)
            && payload == exact =>
        {
            Ok(CanonicalReferenceKind::RootedView {
                payload: rooted_tuple_int_string_payload_key(),
                carrier: CanonicalRootedViewCarrier::TupleRoot,
            })
        }
        exact if witchy_canonical_source::aggregates::is_dict_string_int_root_payload(exact)
            && payload == exact =>
        {
            Ok(CanonicalReferenceKind::RootedView {
                payload: rooted_dict_string_int_payload_key(),
                carrier: CanonicalRootedViewCarrier::DictRoot,
            })
        }
        Type::Named(name, arguments) if name == "Int" && arguments.is_empty() => Ok(CanonicalReferenceKind::RootedView {
            payload: rooted_scalar_payload_key(CanonicalScalarKind::Int),
            carrier: CanonicalRootedViewCarrier::IntScalarRoot,
        }),
        Type::Slice(element) if slice_element_name_ast(element).is_some() => {
            Ok(CanonicalReferenceKind::RootedView {
                payload: rooted_slice_payload_key(slice_element_name_ast(element).expect("matched above")),
                carrier: CanonicalRootedViewCarrier::ListSliceRoot,
            })
        }
        Type::Named(name, arguments)
            if name == "Slice"
                && matches!(arguments.as_slice(), [element] if slice_element_name_ast(element).is_some()) =>
        {
            Ok(CanonicalReferenceKind::RootedView {
                payload: rooted_slice_payload_key(slice_element_name_ast(&arguments[0]).expect("matched above")),
                carrier: CanonicalRootedViewCarrier::ListSliceRoot,
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
            if !witchy_canonical_source::aggregates::is_record_root_payload(payload.unqualified(), &context.definitions) {
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
    CanonicalTypeKey::from_digest(*blake3::hash(&bytes).as_bytes())
}

/// Builtin heads and capabilities are never record roots; only a declared
/// user type (resolved through the nominal ordinal table) is.
fn is_user_record_name(name: &str) -> bool {
    !matches!(name, "Int" | "Float" | "Duration" | "Bool" | "Nil" | "Unit" | "String" | "Bytes"
        | "List" | "Dict" | "Option" | "Result" | "Slice" | "SecretStore")
        && !is_capability_name(name)
        && witchy_syntax::synthetic_names::anon_union_synthetic_variants(name).is_none()
}

fn rooted_record_payload_key(identity: &[u8]) -> CanonicalTypeKey {
    let mut bytes = b"witchy.canonical-rooted-view-payload.v2/record".to_vec();
    bytes.extend_from_slice(identity);
    CanonicalTypeKey::from_digest(*blake3::hash(&bytes).as_bytes())
}

fn rooted_dict_string_int_payload_key() -> CanonicalTypeKey {
    CanonicalTypeKey::from_digest(
        *blake3::hash(b"witchy.canonical-rooted-view-payload.v1/dict-string-int").as_bytes(),
    )
}

fn rooted_tuple_int_string_payload_key() -> CanonicalTypeKey {
    CanonicalTypeKey::from_digest(
        *blake3::hash(b"witchy.canonical-rooted-view-payload.v1/tuple-int-string").as_bytes(),
    )
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
        payload: CanonicalTypeKey::from_digest(*blake3::hash(&bytes).as_bytes()),
        carrier: CanonicalRootedViewCarrier::ListRoot,
    })
}

fn rooted_scalar_value_payload_key(kind: CanonicalScalarKind) -> CanonicalTypeKey {
    let mut bytes = b"witchy.canonical-rooted-view-payload.v1/scalar-value".to_vec();
    bytes.push(kind as u8);
    CanonicalTypeKey::from_digest(*blake3::hash(&bytes).as_bytes())
}

fn rooted_string_payload_key() -> CanonicalTypeKey {
    CanonicalTypeKey::from_digest(*blake3::hash(b"witchy.canonical-rooted-view-payload.v1/string").as_bytes())
}

/// The element of a `ListSliceRoot` carrier: one scalar kind carried inline,
/// or an immutable String. An element that is itself an owner has none.
fn slice_element_name_ty(element: &Ty) -> Option<&'static str> {
    match element {
        Ty::Int => Some("Int"),
        Ty::Float => Some("Float"),
        Ty::Bool => Some("Bool"),
        Ty::Duration => Some("Duration"),
        Ty::String => Some("String"),
        _ => None,
    }
}

fn slice_element_name_ast(element: &Type) -> Option<&'static str> {
    let Type::Named(name, arguments) = element.unqualified() else { return None };
    if !arguments.is_empty() {
        return None;
    }
    ["Int", "Float", "Bool", "Duration", "String"].into_iter().find(|admitted| admitted == name)
}

/// One payload identity per slice element. `Int` keeps its original key.
fn rooted_slice_payload_key(element: &str) -> CanonicalTypeKey {
    let mut bytes = b"witchy.canonical-rooted-view-payload.v1/slice-".to_vec();
    bytes.extend(element.to_ascii_lowercase().bytes());
    CanonicalTypeKey::from_digest(*blake3::hash(&bytes).as_bytes())
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
        // The checker's private `Slice(T)` payload of a `&[T]` view is a
        // builtin structural type, never a declaration.
        Ty::Named(name, args) if name.as_ref() == "Slice" && args.len() == 1 => {
            bytes.push(33);
            encode_ty(&args[0], context, bytes)?;
        }
        Ty::Named(name, args) if witchy_syntax::synthetic_names::anon_union_synthetic_variants(name).is_some() => {
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
    let variants = witchy_syntax::synthetic_names::anon_union_synthetic_variants(name).ok_or_else(|| {
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
        // The checker's private `Slice(T)` nominal is the surface `[T]`.
        Type::Named(name, args) if name == "Slice" && args.len() == 1 => {
            bytes.push(2);
            encode_ast(&args[0], context, bytes)?;
        }
        Type::Named(name, args) => {
            if witchy_syntax::synthetic_names::anon_union_synthetic_variants(name).is_some() {
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
    let variants = witchy_syntax::synthetic_names::anon_union_synthetic_variants(name).ok_or_else(|| {
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
    fn registry_secret_store_callable_abi_is_closed_and_kind_bound() {
        let store = manifest("fn keep(store: SecretStore) -> SecretStore:\n    store\n");
        let secret = manifest("fn keep(secret: Secret) -> Secret:\n    secret\n");
        let clock = manifest("fn keep(clock: Clock) -> Clock:\n    clock\n");
        assert_ne!(store.digest(), secret.digest());
        assert_ne!(store.digest(), clock.digest());
        assert_ne!(secret.digest(), clock.digest());
    }
}
