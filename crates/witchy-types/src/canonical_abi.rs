//! Canonical source-level callable ABI identities.
//!
//! This module deliberately knows nothing about WIR.  It projects the final
//! checked type representation into a deterministic, tagged description that
//! can be bound to a later candidate by a consumer.  Function names and
//! backend-assigned IDs are never part of the projection.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use witchy_syntax::ast::{self, Convention, Item, Type};

use crate::typeck::{Ty, TypedModule};
use witchy_canonical_source::abi::*;


/// Exposed for the ABI-security pins in `tests/typeck/capabilities.rs`.
#[doc(hidden)]
pub struct Context {
    nominal_ordinals: BTreeMap<String, u32>,
    type_declarations: std::collections::BTreeSet<String>,
    /// The module's type definitions, so a record-root view is admitted by the
    /// same rule lowering and authority apply (`is_record_root_payload`).
    definitions: BTreeMap<String, ast::TypeDef>,
}

impl Context {
    #[doc(hidden)]
    pub fn from_module(module: &ast::Module) -> Self {
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
    if !within_canonical_abi_type_budget(ty) {
        return Err(CanonicalAbiError::Unsupported("checked type exceeds canonical ABI bounds".into()));
    }
    let context = Context::from_module(typed.module());
    let mut bytes = Vec::new();
    encode_ty(ty, &context, &mut bytes)?;
    Ok(bytes)
}

pub(crate) fn canonical_ast_type_bytes(
    typed: &TypedModule,
    ty: &Type,
) -> Result<Vec<u8>, CanonicalAbiError> {
    if !within_canonical_abi_ast_budget(ty) {
        return Err(CanonicalAbiError::Unsupported("source type exceeds canonical ABI bounds".into()));
    }
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
/// Exposed for the ABI-security pins in `tests/typeck/capabilities.rs`.
#[doc(hidden)]
pub fn checked_declaration_signature(typed: &TypedModule, name: &str) -> Result<Type, CanonicalAbiError> {
    DeclarationSignatures::new(typed).signature(name)
}

/// One module's ABI context and declaration lookup, built once for every
/// signature asked of it. Rebuilding both per declaration made manifest
/// derivation quadratic in the number of functions.
#[doc(hidden)]
pub struct DeclarationSignatures<'a> {
    typed: &'a TypedModule,
    context: Context,
    declarations: HashMap<&'a str, &'a ast::Function>,
}

impl<'a> DeclarationSignatures<'a> {
    pub fn new(typed: &'a TypedModule) -> Self {
        let mut declarations = HashMap::new();
        for item in &typed.module().items {
            if let Item::Function(function) = item {
                // The first declaration of a name wins, as a linear search would.
                declarations.entry(function.name.as_str()).or_insert(function);
            }
        }
        Self { typed, context: Context::from_module(typed.module()), declarations }
    }

    pub fn signature(&self, name: &str) -> Result<Type, CanonicalAbiError> {
        declaration_signature(self.typed, &self.context, self.declarations.get(name).copied(), name)
    }
}

fn declaration_signature(
    typed: &TypedModule,
    context: &Context,
    declaration: Option<&ast::Function>,
    name: &str,
) -> Result<Type, CanonicalAbiError> {
    let mut signature = typed.table().function_type(name).ok_or_else(||
        CanonicalAbiError::InvalidCallable(format!("checked declaration `{name}` has no finalized signature")))?;
    let declaration = declaration
        .ok_or_else(|| CanonicalAbiError::InvalidCallable(format!("missing checked declaration `{name}`")))?;
    let Type::Fn(parameters, result, conventions, _) = &mut signature else {
        return Err(CanonicalAbiError::InvalidCallable("checked declaration is not callable".into()));
    };
    if parameters.len() != declaration.params.len() {
        return Err(CanonicalAbiError::InvalidCallable("checked declaration parameter arity".into()));
    }
    for (index, (parameter, declared)) in parameters.iter_mut().zip(&declaration.params).enumerate() {
        if let Some(source) = declared.ty.as_ref()
            && witchy_canonical_source::storage::affine_aggregate_erased_type(
                source, &context.definitions,
            ).as_ref() == Some(parameter)
        {
            *parameter = source.clone();
        }
        if let Some(source) = declared.ty.as_ref()
            && qualified_rooted_view(source)
        {
            // A `var unique` receiver carries its uniqueness in the calling
            // convention. Restore the recursively checked payload before
            // wrapping it in that convention's affine root below. The
            // storage judgment must also prove the complete unique owner.
            if declared.convention == Convention::Var
                && let Type::Qualified(ast::TypeQual::Unique, payload) = source
            {
                if !matches!(witchy_canonical_source::storage::StoragePlan::classify(
                    source, &context.definitions, |name| context.nominal(name).ok(),
                ), witchy_canonical_source::storage::StoragePlan::MoveOnlyOwned(_)) {
                    return Err(CanonicalAbiError::InvalidCallable(
                        "unique receiver lacks checked storage operations".into(),
                    ));
                }
                restore_qualified_rooted_view(parameter, payload, &context)?;
            } else {
                restore_qualified_rooted_view(parameter, source, &context)?;
            }
        }
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
        && let Some(physical) = witchy_canonical_source::storage::affine_aggregate_erased_type(
            declared, &context.definitions,
        )
        && result.as_ref() == &physical
    {
        // The finalized value type establishes the physical Result shape;
        // restore only the exact checked source carrier relation that its
        // canonical ABI needs to authenticate the returned owned wrapper.
        *result = Box::new(declared.clone());
    }
    if let Some(source) = declaration.ret.as_ref()
        && qualified_rooted_view(source)
    {
        restore_qualified_rooted_view(result, source, &context)?;
    }
    Ok(signature)
}

/// A finalized HM type erases storage qualifiers. Restore the checked source
/// spelling only after proving the same erased type and a storage plan for
/// every rooted view it contains. This also covers views nested in a callable
/// signature without granting an erased qualifier its own carrier.
fn restore_qualified_rooted_view(finalized: &mut Type, source: &Type, context: &Context) -> Result<(), CanonicalAbiError> {
    if !within_canonical_abi_ast_budget(source)
        || erase_storage_qualifiers(source, 0, &mut 4_096).as_ref() != Some(finalized)
    {
        return Err(CanonicalAbiError::InvalidCallable(
            "qualified rooted view differs from finalized checked type".into(),
        ));
    }
    if !matches!(source, Type::Qualified(ast::TypeQual::View, _))
        && !nested_qualified_views_have_carriers(source, context)
    {
        return Err(CanonicalAbiError::InvalidCallable(
            "nested qualified rooted view has no installed callable carrier".into(),
        ));
    }
    // `encode_ast` validates each rooted view against the same recursive
    // checked storage judgment used to make its structural ABI key.
    encode_ast(source, context, &mut Vec::new())?;
    *finalized = source.clone();
    Ok(())
}

fn nested_qualified_views_have_carriers(ty: &Type, context: &Context) -> bool {
    match ty {
        Type::Qualified(ast::TypeQual::View, payload) => {
            if has_storage_qualifier(payload) {
                let view = Type::Qualified(ast::TypeQual::View, payload.clone());
                return matches!(witchy_canonical_source::storage::StoragePlan::classify(
                    &view, &context.definitions, |name| context.nominal(name).ok(),
                ), witchy_canonical_source::storage::StoragePlan::RootedView(
                    witchy_canonical_source::rooted_view::RootedViewStorage::ScalarCell(_)));
            }
            true
        }
        // A qualifier outside the view would be erased by the present
        // aggregate/callable encoder. It has no distinct installed carrier.
        Type::Qualified(_, _) => false,
        Type::Slice(inner) => nested_qualified_views_have_carriers(inner, context),
        Type::Named(_, children) | Type::Tuple(children) | Type::Dyn(_, children) =>
            children.iter().all(|child| nested_qualified_views_have_carriers(child, context)),
        Type::Fn(parameters, result, _, _) =>
            parameters.iter().all(|parameter| nested_qualified_views_have_carriers(parameter, context))
                && nested_qualified_views_have_carriers(result, context),
        Type::RecordCompose { base, fields } =>
            nested_qualified_views_have_carriers(base, context)
                && fields.iter().all(|(_, field)| nested_qualified_views_have_carriers(field, context)),
    }
}

fn qualified_rooted_view(ty: &Type) -> bool {
    if !within_canonical_abi_ast_budget(ty) { return true; }
    fn visit(ty: &Type, storage_qualifier_above: bool) -> bool {
        match ty {
            Type::Qualified(ast::TypeQual::Frozen | ast::TypeQual::Unique, inner) =>
                visit(inner, true),
            Type::Qualified(ast::TypeQual::View, payload) =>
                storage_qualifier_above || has_storage_qualifier(payload) || visit(payload, false),
            Type::Qualified(_, inner) | Type::Slice(inner) => visit(inner, storage_qualifier_above),
            Type::Named(_, arguments) | Type::Tuple(arguments) | Type::Dyn(_, arguments) =>
                arguments.iter().any(|item| visit(item, storage_qualifier_above)),
            Type::Fn(parameters, result, _, _) =>
                parameters.iter().any(|item| visit(item, storage_qualifier_above)) || visit(result, storage_qualifier_above),
            Type::RecordCompose { base, fields } =>
                visit(base, storage_qualifier_above) || fields.iter().any(|(_, field)| visit(field, storage_qualifier_above)),
        }
    }
    visit(ty, false)
}

pub(crate) fn has_storage_qualifier(ty: &Type) -> bool {
    fn walk(ty: &Type, depth: usize, remaining: &mut usize) -> bool {
        if depth >= 128 || *remaining == 0 { return true; }
        *remaining -= 1;
        let child = |ty: &Type, remaining: &mut usize| walk(ty, depth + 1, remaining);
        match ty {
            Type::Qualified(ast::TypeQual::Frozen | ast::TypeQual::Unique, _) => true,
            Type::Qualified(_, inner) | Type::Slice(inner) => child(inner, remaining),
            Type::Named(_, arguments) | Type::Tuple(arguments) | Type::Dyn(_, arguments) =>
                arguments.iter().any(|argument| child(argument, remaining)),
            Type::Fn(parameters, result, _, _) =>
                parameters.iter().any(|parameter| child(parameter, remaining)) || child(result, remaining),
            Type::RecordCompose { base, fields } =>
                child(base, remaining) || fields.iter().any(|(_, field)| child(field, remaining)),
        }
    }
    walk(ty, 0, &mut 4_096)
}

pub(crate) fn erase_storage_qualifiers(ty: &Type, depth: usize, budget: &mut usize) -> Option<Type> {
    if depth > 128 || *budget == 0 { return None; }
    *budget -= 1;
    let child = |inner: &Type, budget: &mut usize| erase_storage_qualifiers(inner, depth + 1, budget);
    match ty {
        Type::Qualified(ast::TypeQual::Frozen | ast::TypeQual::Unique, inner) => child(inner, budget),
        Type::Qualified(qualifier, inner) =>
            Some(Type::Qualified(qualifier.clone(), Box::new(child(inner, budget)?))),
        Type::Slice(inner) => Some(Type::Slice(Box::new(child(inner, budget)?))),
        Type::Named(name, arguments) => Some(Type::Named(name.clone(), arguments.iter().map(|item| child(item, budget)).collect::<Option<Vec<_>>>()?)),
        Type::Tuple(items) => Some(Type::Tuple(items.iter().map(|item| child(item, budget)).collect::<Option<Vec<_>>>()?)),
        Type::Dyn(name, arguments) => Some(Type::Dyn(name.clone(), arguments.iter().map(|item| child(item, budget)).collect::<Option<Vec<_>>>()?)),
        Type::Fn(parameters, result, conventions, qualifiers) => Some(Type::Fn(
            parameters.iter().map(|item| child(item, budget)).collect::<Option<Vec<_>>>()?,
            Box::new(child(result, budget)?), conventions.clone(), qualifiers.clone(),
        )),
        Type::RecordCompose { base, fields } => Some(Type::RecordCompose {
            base: Box::new(child(base, budget)?),
            fields: fields.iter().map(|(name, field)| Some((name.clone(), child(field, budget)?))).collect::<Option<Vec<_>>>()?,
        }),
    }
}

pub fn derive_manifest(
    typed: &TypedModule,
    closed_functions: &[String],
) -> Result<CanonicalCallableAbiManifest, CanonicalAbiError> {
    let declarations = DeclarationSignatures::new(typed);
    let context = &declarations.context;
    let mut entries = BTreeSet::new();
    for (ordinal, name) in closed_functions.iter().enumerate() {
        let function_ordinal = u32::try_from(ordinal).map_err(|_| {
            CanonicalAbiError::InvalidCallable("function ordinal overflow".into())
        })?;
        let signature = declarations.signature(name)?;
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
    if !within_canonical_abi_type_budget(ty) {
        return Err(CanonicalAbiError::Unsupported("checked type exceeds canonical ABI bounds".into()));
    }
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
    if !within_canonical_abi_ast_budget(ty) {
        return Err(CanonicalAbiError::Unsupported("source type exceeds canonical ABI bounds".into()));
    }
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

/// Exposed for the ABI-security pins in `tests/typeck/capabilities.rs`.
#[doc(hidden)]
pub fn rooted_view_from_ty(
    payload: &Ty,
    context: &Context,
) -> Result<CanonicalReferenceKind, CanonicalAbiError> {
    if !within_canonical_abi_type_budget(payload) {
        return Err(CanonicalAbiError::Unsupported("rooted view payload exceeds canonical ABI bounds".into()));
    }
    let source = crate::typeck::ty_to_ast(payload).ok_or_else(|| CanonicalAbiError::Unsupported(
        "rooted view payload has no concrete checked type".into(),
    ))?;
    rooted_view_from_ast(&source, context)
}

fn within_canonical_abi_type_budget(root: &Ty) -> bool {
    let mut pending = vec![(root, 0usize)];
    let mut remaining = 4_096usize;
    while let Some((ty, depth)) = pending.pop() {
        if depth > 128 || remaining == 0 { return false; }
        remaining -= 1;
        match ty {
            Ty::List(child) | Ty::View(child) => pending.push((child, depth + 1)),
            Ty::Tuple(children) | Ty::Named(_, children) | Ty::Dyn(_, children) => {
                pending.extend(children.iter().map(|child| (child, depth + 1)));
            }
            Ty::Fn(parameters, result, _, _, _) => {
                pending.extend(parameters.iter().map(|parameter| (parameter, depth + 1)));
                pending.push((result, depth + 1));
            }
            _ => {}
        }
    }
    true
}

fn within_canonical_abi_ast_budget(root: &Type) -> bool {
    let mut pending = vec![(root, 0usize)];
    let mut remaining = 4_096usize;
    while let Some((ty, depth)) = pending.pop() {
        if depth > 128 || remaining == 0 { return false; }
        remaining -= 1;
        match ty {
            Type::Qualified(_, inner) | Type::Slice(inner) => pending.push((inner, depth + 1)),
            Type::Named(_, children) | Type::Dyn(_, children) | Type::Tuple(children) => {
                pending.extend(children.iter().map(|child| (child, depth + 1)));
            }
            Type::Fn(parameters, result, _, _) => {
                pending.extend(parameters.iter().map(|parameter| (parameter, depth + 1)));
                pending.push((result, depth + 1));
            }
            Type::RecordCompose { base, fields } => {
                pending.push((base, depth + 1));
                pending.extend(fields.iter().map(|(_, field)| (field, depth + 1)));
            }
        }
    }
    true
}

/// Exposed for the ABI-security pins in `tests/typeck/capabilities.rs`.
#[doc(hidden)]
pub fn rooted_view_from_ast(
    payload: &Type,
    context: &Context,
) -> Result<CanonicalReferenceKind, CanonicalAbiError> {
    if !within_canonical_abi_ast_budget(payload) {
        return Err(CanonicalAbiError::Unsupported("rooted view payload exceeds canonical ABI bounds".into()));
    }
    let view = Type::Qualified(ast::TypeQual::View, Box::new(payload.clone()));
    match witchy_canonical_source::storage::StoragePlan::classify(
        &view,
        &context.definitions,
        |name| context.declares_type(name).then(|| context.nominal(name).ok()).flatten(),
    ) {
        witchy_canonical_source::storage::StoragePlan::RootedView(plan) => plan.canonical_reference(payload),
        _ => None,
    }
    .ok_or_else(|| CanonicalAbiError::Unsupported(
        "rooted view payload has no admitted authenticated carrier".into(),
    ))
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
        let CanonicalReferenceKind::RootedView { payload, carrier } = rooted_view_from_ast(payload, context)? else {
            return Err(CanonicalAbiError::Unsupported("rooted view has no structural carrier".into()));
        };
        bytes.extend_from_slice(&payload.as_bytes());
        bytes.push(carrier as u8);
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
