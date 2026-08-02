//! RFC-0082 backend-neutral runtime type identities and descriptor plans.
//!
//! Runtime descriptors must not key on rendered type names. This module turns
//! resolved compiler types into structural identities rooted in an explicit
//! package/module/declaration identity, then assigns deterministic dense IDs for
//! the closed program. Backends consume the resulting plan; neither backend is
//! allowed to rediscover identity from a string at runtime.

use std::collections::{BTreeMap, BTreeSet};

use witchy_syntax::ast::{Item, Module, Type};
#[cfg(test)]
use witchy_syntax::ast::Convention;
#[cfg(test)]
use witchy_syntax::type_resolve::{ResolvedDeclarationKind, ResolvedDeclarations};

/// The immutable package coordinate that owns a declaration.
///
/// `name` is the manifest's full rune name, not an import alias. `version` is
/// the selected manifest/lock version. The source keeps a workspace package and
/// a registry release with the same name/version from collapsing accidentally.
mod catalog;
mod identity;

pub use catalog::{
    AuthenticatedModuleOwners, DeclarationIdentity, DeclarationKind, ModuleLoadIdentity,
    PackageCoordinate, PackageSource, RuntimeDeclarationCatalog,
};
pub use identity::{
    PrimitiveType, RuntimeAccessKind, RuntimeAccessParameterIdentity,
    RuntimeAccessQualifier, RuntimeAccessResultIdentity, RuntimeBorrowOwnerIdentity,
    RuntimeBorrowRelationIdentity, RuntimeCallableAccessIdentity, RuntimeCallableParameterIdentity,
    RuntimeCapabilityIdentity, RuntimeConvention, RuntimeLoanProjection,
    RuntimeLoanProjectionStep, RuntimeQualifierPathStep,
    RuntimeQualifierSite, RuntimeTypeIdentity, UnionVariantIdentity,
};

use catalog::instantiate_runtime_type;
use identity::decode_anon_record;
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RuntimeTypeId(u32);

impl RuntimeTypeId {
    pub fn index(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeTypeDescriptor {
    pub id: RuntimeTypeId,
    pub identity: RuntimeTypeIdentity,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeFieldDescriptor {
    pub name: String,
    pub descriptor: RuntimeTypeId,
    pub display: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RuntimeMethodArgumentDescriptor {
    pub descriptor: RuntimeTypeId,
    pub ty: Type,
    pub display: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RuntimeMethodCapabilityDescriptor {
    pub ty: Type,
    pub display: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum RuntimeMethodParameterDescriptor {
    Value(RuntimeMethodArgumentDescriptor),
    Capability(RuntimeMethodCapabilityDescriptor),
}

#[derive(Clone, Debug, PartialEq)]
pub struct RuntimeMethodDescriptor {
    pub receiver: RuntimeTypeId,
    pub receiver_type: Type,
    pub name: String,
    pub function: String,
    pub parameters: Vec<RuntimeMethodParameterDescriptor>,
    pub result: RuntimeTypeId,
    pub result_type: Type,
    pub result_display: String,
    pub callable_identity: RuntimeTypeIdentity,
    pub access: RuntimeCallableAccessIdentity,
    pub borrow_relation_storage_displays: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum RuntimeTypeShape {
    #[default]
    Opaque,
    Sealed,
    Record(Vec<RuntimeFieldDescriptor>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum RuntimeTypeShapeDraft {
    Opaque,
    Sealed,
    Record(Vec<(String, RuntimeTypeIdentity, String)>),
}

/// Deterministic descriptor constants for one closed program.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RuntimeTypePlan {
    descriptors: Vec<RuntimeTypeDescriptor>,
    by_identity: BTreeMap<RuntimeTypeIdentity, RuntimeTypeId>,
    shapes: Vec<RuntimeTypeShape>,
    methods: Vec<RuntimeMethodDescriptor>,
    trait_relations: Vec<(RuntimeTypeId, RuntimeTypeId)>,
}

impl RuntimeTypePlan {
    pub fn build(
        identities: impl IntoIterator<Item = RuntimeTypeIdentity>,
    ) -> Result<Self, RuntimeTypeError> {
        let mut identities = identities.into_iter().collect::<BTreeSet<_>>();
        let roots = identities.iter().cloned().collect::<Vec<_>>();
        for root in &roots {
            validate_descriptor_identity(root)?;
            collect_nested_identities(root, &mut identities);
        }
        let mut descriptors = Vec::with_capacity(identities.len());
        let mut by_identity = BTreeMap::new();
        for (index, identity) in identities.into_iter().enumerate() {
            let index = u32::try_from(index).map_err(|_| RuntimeTypeError::TooManyDescriptors)?;
            let id = RuntimeTypeId(index);
            by_identity.insert(identity.clone(), id);
            descriptors.push(RuntimeTypeDescriptor { id, identity });
        }
        let shapes = vec![RuntimeTypeShape::Opaque; descriptors.len()];
        Ok(Self {
            descriptors,
            by_identity,
            shapes,
            methods: Vec::new(),
            trait_relations: Vec::new(),
        })
    }

    pub fn build_with_runtime_shapes<'a>(
        types: impl IntoIterator<Item = &'a Type>,
        catalog: &RuntimeDeclarationCatalog,
        module: &Module,
    ) -> Result<Self, RuntimeTypeError> {
        Self::build_with_runtime_shapes_and_checked_callables(
            types,
            std::iter::empty(),
            catalog,
            module,
        )
    }

    /// Build an authenticated module plan while retaining exact checked
    /// callable identities. A caller without `CheckedAccessFacts` deliberately
    /// receives only conservative function identities from
    /// `build_with_runtime_shapes`.
    pub(crate) fn build_with_runtime_shapes_and_checked_callables<'a>(
        types: impl IntoIterator<Item = &'a Type>,
        callables: impl IntoIterator<Item = &'a crate::access::AccessSignature>,
        catalog: &RuntimeDeclarationCatalog,
        module: &Module,
    ) -> Result<Self, RuntimeTypeError> {
        let mut identities = BTreeSet::new();
        let mut drafts = BTreeMap::new();
        let mut visiting = BTreeSet::new();
        for ty in types {
            collect_runtime_type_shape(
                ty,
                catalog,
                module,
                &mut identities,
                &mut drafts,
                &mut visiting,
            )?;
        }
        for callable in callables {
            identities.insert(catalog.checked_callable_identity_with_authority(callable, module)?);
            for parameter in callable.params() {
                match catalog.capability_free_type_identity(parameter.ty(), module) {
                    Ok(_) => collect_runtime_type_shape(
                        parameter.ty(),
                        catalog,
                        module,
                        &mut identities,
                        &mut drafts,
                        &mut visiting,
                    )?,
                    Err(RuntimeTypeError::CapabilityType(_)
                    | RuntimeTypeError::CapabilityRetained { .. }) => {}
                    Err(error) => return Err(error),
                }
            }
            for ty in std::iter::once(callable.result().ty()).chain(
                callable
                    .borrow_relations()
                    .iter()
                    .map(|relation| relation.storage_type()),
            ) {
                collect_runtime_type_shape(
                    ty,
                    catalog,
                    module,
                    &mut identities,
                    &mut drafts,
                    &mut visiting,
                )?;
            }
        }
        let mut plan = Self::build(identities)?;
        for (identity, draft) in drafts {
            let Some(id) = plan.id(&identity) else { continue };
            let shape = match draft {
                RuntimeTypeShapeDraft::Opaque => RuntimeTypeShape::Opaque,
                RuntimeTypeShapeDraft::Sealed => RuntimeTypeShape::Sealed,
                RuntimeTypeShapeDraft::Record(fields) => RuntimeTypeShape::Record(
                    fields
                        .into_iter()
                        .map(|(name, identity, display)| RuntimeFieldDescriptor {
                            name,
                            descriptor: plan
                                .id(&identity)
                                .expect("runtime shape collector retains every field descriptor"),
                            display,
                        })
                        .collect(),
                ),
            };
            plan.shapes[usize::try_from(id.0).expect("descriptor index fits usize")] = shape;
        }
        Ok(plan)
    }

    /// Build the backend-neutral descriptor constants for resolved compiler
    /// types. Every nominal and existential head must already be authenticated
    /// by `catalog`; unresolved names and capabilities fail before a backend can
    /// observe a partial plan.
    #[cfg(test)]
    fn from_resolved_types<'a>(
        types: impl IntoIterator<Item = &'a Type>,
        catalog: &RuntimeDeclarationCatalog,
    ) -> Result<Self, RuntimeTypeError> {
        let identities = types
            .into_iter()
            .map(|ty| catalog.type_identity(ty))
            .collect::<Result<Vec<_>, _>>()?;
        Self::build(identities)
    }

    pub fn descriptors(&self) -> &[RuntimeTypeDescriptor] {
        &self.descriptors
    }

    pub fn id(&self, identity: &RuntimeTypeIdentity) -> Option<RuntimeTypeId> {
        self.by_identity.get(identity).copied()
    }

    pub fn get(&self, id: RuntimeTypeId) -> Option<&RuntimeTypeDescriptor> {
        self.descriptors
            .get(usize::try_from(id.0).ok()?)
            .filter(|descriptor| descriptor.id == id)
    }

    pub fn shape(&self, id: RuntimeTypeId) -> Option<&RuntimeTypeShape> {
        self.shapes.get(usize::try_from(id.0).ok()?)
    }

    pub(crate) fn set_methods(&mut self, mut methods: Vec<RuntimeMethodDescriptor>) {
        methods.sort_by(|left, right| {
            (left.receiver.index(), left.name.as_str(), left.function.as_str()).cmp(&(
                right.receiver.index(),
                right.name.as_str(),
                right.function.as_str(),
            ))
        });
        self.methods = methods;
    }

    pub fn methods(&self) -> &[RuntimeMethodDescriptor] {
        &self.methods
    }

    pub(crate) fn set_trait_relations(
        &mut self,
        mut relations: Vec<(RuntimeTypeId, RuntimeTypeId)>,
    ) {
        relations.sort_by_key(|(concrete, trait_id)| (concrete.index(), trait_id.index()));
        relations.dedup();
        self.trait_relations = relations;
    }

    pub fn trait_relations(&self) -> &[(RuntimeTypeId, RuntimeTypeId)] {
        &self.trait_relations
    }
}

fn collect_runtime_type_shape(
    ty: &Type,
    catalog: &RuntimeDeclarationCatalog,
    module: &Module,
    identities: &mut BTreeSet<RuntimeTypeIdentity>,
    drafts: &mut BTreeMap<RuntimeTypeIdentity, RuntimeTypeShapeDraft>,
    visiting: &mut BTreeSet<DeclarationIdentity>,
) -> Result<(), RuntimeTypeError> {
    if let Type::Qualified(_, inner) = ty
        && !matches!(ty.unqualified(), Type::Fn(..))
    {
        return collect_runtime_type_shape(inner, catalog, module, identities, drafts, visiting);
    }
    let identity = catalog.type_identity(ty)?;
    if identities.contains(&identity) {
        return Ok(());
    }
    let entered = match &identity {
        RuntimeTypeIdentity::Nominal { declaration, .. } => {
            if visiting.contains(declaration) {
                return Err(RuntimeTypeError::ExpandingRuntimeShapeRecursion {
                    declaration: Box::new(declaration.clone()),
                    identity: Box::new(identity),
                });
            }
            visiting.insert(declaration.clone());
            Some(declaration.clone())
        }
        _ => None,
    };
    identities.insert(identity.clone());
    drafts.insert(identity.clone(), RuntimeTypeShapeDraft::Opaque);
    let result = collect_runtime_type_shape_inner(
        ty,
        &identity,
        catalog,
        module,
        identities,
        drafts,
        visiting,
    );
    if let Some(declaration) = entered {
        visiting.remove(&declaration);
    }
    result
}

fn collect_runtime_type_shape_inner(
    ty: &Type,
    identity: &RuntimeTypeIdentity,
    catalog: &RuntimeDeclarationCatalog,
    module: &Module,
    identities: &mut BTreeSet<RuntimeTypeIdentity>,
    drafts: &mut BTreeMap<RuntimeTypeIdentity, RuntimeTypeShapeDraft>,
    visiting: &mut BTreeSet<DeclarationIdentity>,
) -> Result<(), RuntimeTypeError> {
    match ty {
        Type::Named(name, arguments) => {
            if let RuntimeTypeIdentity::Record(_) = &identity
                && let Some(field_names) = decode_anon_record(name)
            {
                let mut fields = Vec::new();
                for (field_name, field_type) in field_names.into_iter().zip(arguments) {
                    let field_identity = catalog.type_identity(field_type)?;
                    collect_runtime_type_shape(
                        field_type,
                        catalog,
                        module,
                        identities,
                        drafts,
                        visiting,
                    )?;
                    fields.push((
                        field_name,
                        field_identity,
                        witchy_syntax::format::type_str(field_type),
                    ));
                }
                drafts.insert(identity.clone(), RuntimeTypeShapeDraft::Record(fields));
                return Ok(());
            }
            let RuntimeTypeIdentity::Nominal { declaration, .. } = &identity else {
                for argument in arguments
                    .iter()
                    .filter(|argument| !is_runtime_lifetime_argument(argument))
                {
                    collect_runtime_type_shape(
                        argument,
                        catalog,
                        module,
                        identities,
                        drafts,
                        visiting,
                    )?;
                }
                return Ok(());
            };
            let definition = module.items.iter().find_map(|item| {
                let Item::Type(definition) = item else { return None };
                let matches = definition.name == *name
                    || catalog.resolve(&definition.name, DeclarationKind::Type)
                        == Some(declaration);
                matches.then_some(definition)
            });
            let Some(definition) = definition else {
                return Err(RuntimeTypeError::MissingRuntimeShape {
                    declaration: Box::new(declaration.clone()),
                });
            };
            let nominal_parameters = witchy_syntax::ast::effective_nominal_type_def_params(definition);
            let runtime_arguments = if nominal_parameters.len() == arguments.len() {
                nominal_parameters
                    .iter()
                    .zip(arguments)
                    .filter_map(|(parameter, argument)| {
                        (!witchy_syntax::ast::is_lifetime_param(parameter)).then_some(argument)
                    })
                    .collect::<Vec<_>>()
            } else {
                arguments
                    .iter()
                    .filter(|argument| !is_runtime_lifetime_argument(argument))
                    .collect::<Vec<_>>()
            };
            for argument in &runtime_arguments {
                collect_runtime_type_shape(
                    argument,
                    catalog,
                    module,
                    identities,
                    drafts,
                    visiting,
                )?;
            }
            if definition.sealed {
                drafts.insert(identity.clone(), RuntimeTypeShapeDraft::Sealed);
                return Ok(());
            }
            let Some(variant) = definition.variants.first().filter(|_| {
                definition.variants.len() == 1
                    && !definition.variants[0].field_names.is_empty()
                    && definition.variants[0].field_names.len()
                        == definition.variants[0].fields.len()
            }) else {
                return Ok(());
            };
            let parameters = crate::typeck::type_def_params(definition);
            if parameters.len() != runtime_arguments.len() {
                return Err(RuntimeTypeError::RuntimeShapeArity {
                    name: definition.name.clone(),
                    expected: parameters.len(),
                    actual: runtime_arguments.len(),
                });
            }
            let bindings = parameters
                .into_iter()
                .zip(runtime_arguments.into_iter().cloned())
                .collect::<BTreeMap<_, _>>();
            let mut fields = Vec::new();
            for (field_name, field_type) in variant.field_names.iter().zip(&variant.fields) {
                let field_type = instantiate_runtime_type(field_type, &bindings);
                let field_identity = catalog.type_identity(&field_type)?;
                collect_runtime_type_shape(
                    &field_type,
                    catalog,
                    module,
                    identities,
                    drafts,
                    visiting,
                )?;
                fields.push((
                    field_name.clone(),
                    field_identity,
                    witchy_syntax::format::type_str(&field_type),
                ));
            }
            drafts.insert(identity.clone(), RuntimeTypeShapeDraft::Record(fields));
        }
        Type::Dyn(_, arguments) | Type::Tuple(arguments) => {
            for argument in arguments {
                collect_runtime_type_shape(
                    argument,
                    catalog,
                    module,
                    identities,
                    drafts,
                    visiting,
                )?;
            }
        }
        Type::Fn(parameters, result, _) => {
            for parameter in parameters {
                collect_runtime_type_shape(
                    parameter,
                    catalog,
                    module,
                    identities,
                    drafts,
                    visiting,
                )?;
            }
            collect_runtime_type_shape(
                result,
                catalog,
                module,
                identities,
                drafts,
                visiting,
            )?;
        }
        Type::RecordCompose { base, fields } => {
            collect_runtime_type_shape(base, catalog, module, identities, drafts, visiting)?;
            for (_, field) in fields {
                collect_runtime_type_shape(
                    field,
                    catalog,
                    module,
                    identities,
                    drafts,
                    visiting,
                )?;
            }
        }
        Type::Qualified(_, inner) => collect_runtime_type_shape_inner(
            inner,
            identity,
            catalog,
            module,
            identities,
            drafts,
            visiting,
        )?,
    }
    Ok(())
}

fn is_runtime_lifetime_argument(ty: &Type) -> bool {
    matches!(ty, Type::Named(name, arguments) if arguments.is_empty() && name.starts_with('\''))
}

fn collect_nested_identities(
    identity: &RuntimeTypeIdentity,
    identities: &mut BTreeSet<RuntimeTypeIdentity>,
) {
    let children: Vec<&RuntimeTypeIdentity> = match identity {
        RuntimeTypeIdentity::Primitive(_) => Vec::new(),
        RuntimeTypeIdentity::List(item) => vec![item],
        RuntimeTypeIdentity::Tuple(items) => items.iter().collect(),
        RuntimeTypeIdentity::Function { params, result, access, .. } => params
            .iter()
            .filter(|parameter| !parameter.is_authority())
            .map(RuntimeCallableParameterIdentity::identity)
            .chain(std::iter::once(result.as_ref()))
            .chain(
                access
                    .borrow_relations()
                    .iter()
                    .map(|relation| relation.storage.as_ref()),
            )
            .collect(),
        RuntimeTypeIdentity::Nominal { arguments, .. }
        | RuntimeTypeIdentity::Existential { arguments, .. } => arguments.iter().collect(),
        RuntimeTypeIdentity::Record(fields) => fields.iter().map(|(_, ty)| ty).collect(),
        RuntimeTypeIdentity::Union(variants) => variants
            .iter()
            .flat_map(|variant| variant.payloads.iter())
            .collect(),
        RuntimeTypeIdentity::Capability { .. } => Vec::new(),
    };
    for child in children {
        if identities.insert(child.clone()) {
            collect_nested_identities(child, identities);
        }
    }
}

fn identity_contains_capability(identity: &RuntimeTypeIdentity) -> bool {
    match identity {
        RuntimeTypeIdentity::Capability { .. } => true,
        RuntimeTypeIdentity::Primitive(_) => false,
        RuntimeTypeIdentity::List(item) => identity_contains_capability(item),
        RuntimeTypeIdentity::Tuple(items) => items.iter().any(identity_contains_capability),
        RuntimeTypeIdentity::Function { params, result, access, .. } => {
            params
                .iter()
                .any(|parameter| identity_contains_capability(parameter.identity()))
                || identity_contains_capability(result)
                || access
                    .borrow_relations()
                    .iter()
                    .any(|relation| identity_contains_capability(&relation.storage))
        }
        RuntimeTypeIdentity::Nominal { arguments, .. }
        | RuntimeTypeIdentity::Existential { arguments, .. } => {
            arguments.iter().any(identity_contains_capability)
        }
        RuntimeTypeIdentity::Record(fields) => {
            fields.iter().any(|(_, field)| identity_contains_capability(field))
        }
        RuntimeTypeIdentity::Union(variants) => variants
            .iter()
            .flat_map(|variant| &variant.payloads)
            .any(identity_contains_capability),
    }
}

fn validate_descriptor_identity(identity: &RuntimeTypeIdentity) -> Result<(), RuntimeTypeError> {
    match identity {
        RuntimeTypeIdentity::Capability { .. } => {
            Err(RuntimeTypeError::CapabilityDescriptorIdentity)
        }
        RuntimeTypeIdentity::Primitive(_) => Ok(()),
        RuntimeTypeIdentity::List(item) => validate_descriptor_identity(item),
        RuntimeTypeIdentity::Tuple(items) => {
            items.iter().try_for_each(validate_descriptor_identity)
        }
        RuntimeTypeIdentity::Function { params, result, access, .. } => {
            for parameter in params {
                if parameter.is_authority() {
                    continue;
                }
                if identity_contains_capability(parameter.identity()) {
                    return Err(RuntimeTypeError::CapabilityInValueCallableParameter);
                }
                validate_descriptor_identity(parameter.identity())?;
            }
            validate_descriptor_identity(result)?;
            access
                .borrow_relations()
                .iter()
                .try_for_each(|relation| validate_descriptor_identity(&relation.storage))
        }
        RuntimeTypeIdentity::Nominal { arguments, .. }
        | RuntimeTypeIdentity::Existential { arguments, .. } => {
            arguments.iter().try_for_each(validate_descriptor_identity)
        }
        RuntimeTypeIdentity::Record(fields) => fields
            .iter()
            .try_for_each(|(_, field)| validate_descriptor_identity(field)),
        RuntimeTypeIdentity::Union(variants) => variants
            .iter()
            .flat_map(|variant| &variant.payloads)
            .try_for_each(validate_descriptor_identity),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuntimeTypeError {
    InvalidPackageCoordinate(String),
    InvalidDeclarationIdentity(String),
    InvalidModuleOwner(String),
    CapabilityType(String),
    CapabilityDescriptorIdentity,
    CapabilityInValueCallableParameter,
    CapabilityRetained { capability: String, path: Vec<String> },
    UninspectableDynamicPayload { kind: String, path: Vec<String> },
    MissingRuntimeShape {
        declaration: Box<DeclarationIdentity>,
    },
    RuntimeShapeArity { name: String, expected: usize, actual: usize },
    ExpandingRuntimeShapeRecursion {
        declaration: Box<DeclarationIdentity>,
        identity: Box<RuntimeTypeIdentity>,
    },
    MissingAuthenticatedModuleOwners,
    MissingModuleOwner { module: String },
    ConflictingModuleOwner { module: String },
    ConflictingDeclaration { kind: DeclarationKind, name: String },
    UnresolvedDeclaration { kind: DeclarationKind, name: String },
    ConventionArity { params: usize, conventions: usize },
    MalformedAccessSignature(String),
    MalformedStructuralType(String),
    TooManyDescriptors,
}

impl std::fmt::Display for RuntimeTypeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidPackageCoordinate(message)
            | Self::InvalidDeclarationIdentity(message)
            | Self::InvalidModuleOwner(message)
            | Self::MalformedAccessSignature(message)
            | Self::MalformedStructuralType(message) => f.write_str(message),
            Self::CapabilityType(name) => {
                write!(f, "capability type `{name}` cannot have a runtime descriptor")
            }
            Self::CapabilityDescriptorIdentity => {
                f.write_str("capability authority cannot be a readable runtime descriptor")
            }
            Self::CapabilityInValueCallableParameter => f.write_str(
                "capability authority cannot occupy a callable value-descriptor parameter",
            ),
            Self::CapabilityRetained { capability, path } => {
                write!(f, "capability type `{capability}` cannot convert to Dynamic")?;
                if !path.is_empty() {
                    write!(f, "; retained by `{}`", path.join(" -> "))?;
                }
                Ok(())
            }
            Self::UninspectableDynamicPayload { kind, path } => {
                write!(f, "{kind} payload cannot convert to Dynamic")?;
                if !path.is_empty() {
                    write!(f, "; retained by `{}`", path.join(" -> "))?;
                }
                Ok(())
            }
            Self::MissingRuntimeShape { declaration } => write!(
                f,
                "runtime shape for authenticated declaration `{}::{}` is missing",
                declaration.module().join("."),
                declaration.name()
            ),
            Self::RuntimeShapeArity { name, expected, actual } => write!(
                f,
                "runtime shape `{name}` expects {expected} type argument(s), got {actual}"
            ),
            Self::ExpandingRuntimeShapeRecursion { declaration, identity } => write!(
                f,
                "runtime shape `{}::{}` recursively changes its type arguments to `{identity:?}` and has no finite descriptor closure",
                declaration.module().join("."),
                declaration.name()
            ),
            Self::MissingAuthenticatedModuleOwners => f.write_str(
                "checked module lacks authenticated loader ownership; use an authenticated checked-link API",
            ),
            Self::MissingModuleOwner { module } => write!(
                f,
                "runtime descriptor declarations from module `{module}` lack loader ownership"
            ),
            Self::ConflictingModuleOwner { module } => write!(
                f,
                "loader supplied conflicting package identities for module `{module}`"
            ),
            Self::ConflictingDeclaration { kind, name } => write!(
                f,
                "resolved {kind:?} declaration `{name}` maps to conflicting package identities"
            ),
            Self::UnresolvedDeclaration { kind, name } => {
                write!(f, "runtime type references unresolved {kind:?} declaration `{name}`")
            }
            Self::ConventionArity { params, conventions } => write!(
                f,
                "function runtime type has {params} parameter(s) but {conventions} convention(s)"
            ),
            Self::TooManyDescriptors => {
                f.write_str("runtime descriptor plan exceeds u32 identities")
            }
        }
    }
}

impl std::error::Error for RuntimeTypeError {}

#[cfg(test)]
mod tests {
    use super::*;
    use witchy_syntax::type_resolve::ResolvedDeclaration;

    fn package(source: PackageSource, name: &str, version: &str) -> PackageCoordinate {
        PackageCoordinate::new(source, name, version).expect("valid test package")
    }

    fn declaration(package: PackageCoordinate, module: &str, name: &str) -> DeclarationIdentity {
        DeclarationIdentity::new(package, [module], DeclarationKind::Type, name)
            .expect("valid test declaration")
    }

    fn nominal(declaration: DeclarationIdentity) -> RuntimeTypeIdentity {
        RuntimeTypeIdentity::Nominal { declaration, arguments: Vec::new() }
    }

    #[test]
    fn package_distinct_same_spelled_declarations_do_not_collapse() {
        let left = nominal(declaration(
            package(PackageSource::Registry("coven-a".into()), "acme/model", "1.0.0"),
            "model",
            "User",
        ));
        let right = nominal(declaration(
            package(PackageSource::Registry("coven-b".into()), "acme/model", "1.0.0"),
            "model",
            "User",
        ));
        let plan = RuntimeTypePlan::build([left.clone(), right.clone()]).expect("descriptor plan");
        assert_ne!(plan.id(&left), plan.id(&right));
        assert_eq!(plan.descriptors().len(), 2);
    }

    #[test]
    fn import_aliases_do_not_change_resolved_declaration_identity() {
        let identity = nominal(declaration(
            package(PackageSource::Registry("coven".into()), "acme/model", "1.0.0"),
            "model",
            "User",
        ));
        let plan = RuntimeTypePlan::build([identity.clone(), identity.clone()]).expect("plan");
        assert_eq!(plan.descriptors().len(), 1);
        assert_eq!(plan.id(&identity), Some(RuntimeTypeId(0)));
    }

    #[test]
    fn descriptor_ids_are_deterministic_across_discovery_order() {
        let int = RuntimeTypeIdentity::Primitive(PrimitiveType::Int);
        let string = RuntimeTypeIdentity::Primitive(PrimitiveType::String);
        let forward = RuntimeTypePlan::build([int.clone(), string.clone()]).expect("forward");
        let reverse = RuntimeTypePlan::build([string.clone(), int.clone()]).expect("reverse");
        assert_eq!(forward, reverse);
        assert_eq!(forward.id(&int), reverse.id(&int));
    }

    #[test]
    fn conversion_never_falls_back_to_a_display_name() {
        let ty = Type::Named("some_alias.User".to_string(), Vec::new());
        let error = RuntimeTypeIdentity::from_resolved_type(&ty, &|_, _| None)
            .expect_err("unresolved identity must be loud");
        assert!(matches!(
            error,
            RuntimeTypeError::UnresolvedDeclaration { name, .. } if name == "some_alias.User"
        ));
    }

    #[test]
    fn type_derived_access_signature_cannot_claim_checked_runtime_authority() {
        let string = Type::Named("String".to_string(), Vec::new());
        let ty = Type::Fn(
            vec![string.clone()],
            Box::new(string),
            vec![Convention::Borrow],
        );
        let type_only_signature = crate::access::AccessSignature::from_function_type(&ty)
            .expect("type-only callable signature");
        assert_eq!(
            type_only_signature.params()[0].kind(),
            crate::access::AccessKind::SharedBorrow,
        );

        let public_identity = RuntimeTypeIdentity::from_resolved_type(&ty, &|_, _| None)
            .expect("public type-only runtime identity");
        let RuntimeTypeIdentity::Function { access, .. } = public_identity else {
            panic!("function type must produce a callable identity")
        };
        assert!(
            access.is_conservative(),
            "type-only access facts must remain conservative",
        );
        assert!(!access.is_checked());
    }

    #[test]
    fn canonical_anonymous_shapes_ignore_nominal_resolution() {
        let module = witchy_syntax::parser::parse_module(
            "fn f(record: .{a: Int, b: String}, event: .[Text(String) | Quit]) -> Int:\n    0\n",
        )
        .expect("parse structural types");
        let function = module
            .items
            .iter()
            .find_map(|item| match item {
                witchy_syntax::ast::Item::Function(function) => Some(function),
                _ => None,
            })
            .expect("expected parsed function");
        let record = RuntimeTypeIdentity::from_resolved_type(
            function.params[0].ty.as_ref().expect("record type"),
            &|_, _| None,
        )
        .expect("record identity");
        let union = RuntimeTypeIdentity::from_resolved_type(
            function.params[1].ty.as_ref().expect("union type"),
            &|_, _| None,
        )
        .expect("union identity");
        assert!(matches!(record, RuntimeTypeIdentity::Record(fields) if fields.len() == 2));
        assert!(matches!(union, RuntimeTypeIdentity::Union(variants) if variants.len() == 2));
    }

    #[test]
    fn function_conventions_are_part_of_runtime_identity() {
        let int = Type::Named("Int".to_string(), Vec::new());
        let borrowed = Type::Fn(
            vec![int.clone()],
            Box::new(int.clone()),
            vec![Convention::Borrow],
        );
        let owned = Type::Fn(vec![int.clone()], Box::new(int), vec![Convention::Own]);
        let borrowed = RuntimeTypeIdentity::from_resolved_type(&borrowed, &|_, _| None)
            .expect("borrowed function identity");
        let owned = RuntimeTypeIdentity::from_resolved_type(&owned, &|_, _| None)
            .expect("owned function identity");
        assert_ne!(borrowed, owned);
    }

    #[test]
    fn qualified_callable_and_nested_qualifier_contracts_do_not_collide() {
        let string = Type::Named("String".to_string(), Vec::new());
        let list = |item| Type::Named("List".to_string(), vec![item]);
        let callable = |parameter| {
            Type::Fn(
                vec![parameter],
                Box::new(Type::Tuple(Vec::new())),
                vec![Convention::Let],
            )
        };

        let plain = callable(list(string.clone()));
        let qualified_callable = Type::Qualified(
            witchy_syntax::ast::TypeQual::Unique,
            Box::new(plain.clone()),
        );
        let qualified_argument = callable(list(Type::Qualified(
            witchy_syntax::ast::TypeQual::Frozen,
            Box::new(string),
        )));

        let identity = |ty: &Type| {
            RuntimeTypeIdentity::from_resolved_type(ty, &|_, _| None)
                .expect("closed callable runtime identity")
        };
        assert_ne!(identity(&plain), identity(&qualified_callable));
        assert_ne!(identity(&plain), identity(&qualified_argument));

        let empty = witchy_syntax::parser::parse_module("").expect("empty module");
        let plan = RuntimeTypePlan::build_with_runtime_shapes(
            [&plain, &qualified_callable],
            &RuntimeDeclarationCatalog::default(),
            &empty,
        )
        .expect("qualified callable descriptor plan");
        assert_ne!(
            plan.id(&identity(&plain)),
            plan.id(&identity(&qualified_callable)),
            "descriptor planning must not strip the callable qualifier",
        );

        let RuntimeTypeIdentity::Function { access, .. } = identity(&qualified_argument) else {
            panic!("expected function identity")
        };
        assert_eq!(
            access.parameters()[0].qualifier_sites,
            vec![RuntimeQualifierSite {
                path: vec![RuntimeQualifierPathStep::TypeArgument(0)],
                qualifiers: vec![RuntimeAccessQualifier::Frozen],
            }]
        );
    }

    #[test]
    fn callable_lifetime_names_normalize_but_owner_relations_do_not_collide() {
        let string = Type::Named("String".to_string(), Vec::new());
        let view = |lifetime: &str| {
            Type::Qualified(
                witchy_syntax::ast::TypeQual::Borrow(lifetime.to_string()),
                Box::new(string.clone()),
            )
        };
        let signature = |left: &str, right: &str, result: &str| {
            Type::Fn(
                vec![view(left), view(right)],
                Box::new(view(result)),
                vec![Convention::Borrow, Convention::Borrow],
            )
        };
        let identity = |ty: &Type| {
            RuntimeTypeIdentity::from_resolved_type(ty, &|_, _| None)
                .expect("borrowed callable runtime identity")
        };

        let original = identity(&signature("left", "right", "left"));
        let renamed = identity(&signature("a", "b", "a"));
        let swapped = identity(&signature("left", "right", "right"));
        assert_eq!(original, renamed, "alpha-renaming is not runtime identity");
        assert_ne!(original, swapped, "the result owner position is runtime identity");

        let RuntimeTypeIdentity::Function { access, .. } = original else {
            panic!("expected function identity")
        };
        assert_eq!(access.borrow_relations().len(), 1);
        assert_eq!(access.borrow_relations()[0].lifetime, 0);
        assert_eq!(access.borrow_relations()[0].owners[0].parameter, 0);
    }

    #[test]
    fn callable_access_identity_retains_writeback_and_ownership_outputs() {
        let string = Type::Named("String".to_string(), Vec::new());
        let parameter = Type::Qualified(
            witchy_syntax::ast::TypeQual::Unique,
            Box::new(string.clone()),
        );
        let result = Type::Qualified(
            witchy_syntax::ast::TypeQual::Unique,
            Box::new(string),
        );
        let ty = Type::Fn(
            vec![parameter],
            Box::new(result),
            vec![Convention::Var],
        );
        let RuntimeTypeIdentity::Function { access, .. } =
            RuntimeTypeIdentity::from_resolved_type(&ty, &|_, _| None)
                .expect("writeback callable identity")
        else {
            panic!("expected function identity")
        };
        assert_eq!(access.parameters()[0].kind, RuntimeAccessKind::ExclusiveWriteback);
        assert!(access.parameters()[0].ownership_input);
        assert!(access.parameters()[0].writeback_output);
        assert!(access.result().ownership_output);
    }

    #[test]
    fn checked_callable_reflection_retains_nominal_projection_relations() {
        let module = witchy_syntax::parser::parse_module(
            "mode opt\n\n\
             type PairView('left, 'right):\n    first: View(String, 'left)\n    second: View(String, 'right)\n\n\
             fn pair(left: let('left) String, right: let('right) String, \
                 pair: PairView('left, 'right)) -> PairView('left, 'right):\n    pair\n",
        )
        .expect("parse projected relation fixture");
        let typed = crate::typeck::annotate_checked(module)
            .expect("type-check projected relation fixture");
        let facts = crate::access::checked_facts(typed.module(), typed.table())
            .expect("checked access facts");
        let signature = facts.declaration("pair").expect("pair access signature");
        let access = RuntimeCallableAccessIdentity::from_checked(signature, &|_, _| None)
            .expect("logical callable reflection");

        assert_eq!(access.borrow_relations().len(), 2);
        assert_eq!(
            access.borrow_relations()[0].output_projection,
            RuntimeLoanProjection {
                steps: vec![RuntimeLoanProjectionStep::Field("first".into())],
            }
        );
        assert_eq!(access.borrow_relations()[0].owners[0].parameter, 0);
        assert_eq!(
            access.borrow_relations()[1].output_projection,
            RuntimeLoanProjection {
                steps: vec![RuntimeLoanProjectionStep::Field("second".into())],
            }
        );
        assert_eq!(access.borrow_relations()[1].owners[0].parameter, 1);
    }

    #[test]
    fn checked_function_identity_retains_nominal_projections_and_owner_mismatch() {
        let module = witchy_syntax::parser::parse_module(
            "mode opt\n\n\
             type PairView('left, 'right):\n    first: View(String, 'left)\n    second: View(String, 'right)\n\n\
             fn pair(left: let('left) String, right: let('right) String, \
                 pair: PairView('left, 'right)) -> PairView('left, 'right):\n    pair\n\n\
             fn reversed(right: let('right) String, left: let('left) String, \
                 pair: PairView('left, 'right)) -> PairView('left, 'right):\n    pair\n",
        )
        .expect("parse checked callable identity fixture");
        let typed = crate::typeck::annotate_checked(module)
            .expect("type-check checked callable identity fixture");
        let facts = crate::access::checked_facts(typed.module(), typed.table())
            .expect("checked callable identity facts");

        let owner = ModuleLoadIdentity::new(
            package(PackageSource::Workspace, "reflection-test", "0.0.0"),
            ["main"],
        )
        .expect("test module owner");
        let mut catalog = RuntimeDeclarationCatalog::default();
        catalog
            .insert_resolved("PairView", &owner, "PairView", DeclarationKind::Type)
            .expect("authenticate PairView");
        let pair = facts.declaration("pair").expect("pair checked signature");
        let reversed = facts
            .declaration("reversed")
            .expect("reversed checked signature");
        let exact = catalog
            .checked_callable_identity_with_authority(pair, typed.module())
            .expect("exact checked callable identity");
        let mismatch = catalog
            .checked_callable_identity_with_authority(reversed, typed.module())
            .expect("mismatched checked callable identity");
        assert_ne!(exact, mismatch, "owner-position rewiring must remain distinct");

        let coarse_type = Type::Fn(
            pair.params().iter().map(|parameter| parameter.ty().clone()).collect(),
            Box::new(pair.result().ty().clone()),
            pair.params()
                .iter()
                .map(|parameter| match parameter.kind() {
                    crate::access::AccessKind::OwnedImmutable => Convention::Let,
                    crate::access::AccessKind::SharedBorrow => Convention::Borrow,
                    crate::access::AccessKind::ExclusiveWriteback => Convention::Var,
                    crate::access::AccessKind::Consuming => Convention::Own,
                })
                .collect(),
        );
        let coarse = catalog
            .type_identity(&coarse_type)
            .expect("conservative catalog-free callable identity");
        assert_ne!(exact, coarse, "coarse roots cannot authenticate exact relations");
        let plan = RuntimeTypePlan::build_with_runtime_shapes_and_checked_callables(
            std::iter::empty::<&Type>(),
            [pair],
            &catalog,
            typed.module(),
        )
        .expect("authenticated plan with checked callable identity");
        assert!(plan.id(&exact).is_some());
        assert!(
            plan.id(&coarse).is_none(),
            "authenticated callable collection must not silently add a coarse identity",
        );

        let RuntimeTypeIdentity::Function { access, .. } = exact else {
            panic!("expected checked function identity")
        };
        assert!(access.is_checked());
        assert_eq!(
            access.borrow_relations()[0].output_projection.steps,
            vec![RuntimeLoanProjectionStep::Field("first".into())],
        );
        assert_eq!(access.borrow_relations()[0].owners[0].parameter, 0);
        let RuntimeTypeIdentity::Function { access, .. } = coarse else {
            panic!("expected conservative function identity")
        };
        assert!(access.is_conservative());
        assert_eq!(
            access.borrow_relations()[0].output_projection,
            RuntimeLoanProjection::default(),
        );
    }

    #[test]
    fn checked_callable_authority_is_identity_but_not_a_value_descriptor() {
        let module = witchy_syntax::parser::parse_module(
            "fn announce(console: Console, text: String) -> String:\n    text\n",
        )
        .expect("parse authority-bearing callable fixture");
        let typed = crate::typeck::annotate_checked(module)
            .expect("type-check authority-bearing callable fixture");
        let facts = crate::access::checked_facts(typed.module(), typed.table())
            .expect("checked authority-bearing callable facts");

        let catalog = RuntimeDeclarationCatalog::default();
        let signature = facts.declaration("announce").expect("announce signature");
        let identity = catalog
            .checked_callable_identity_with_authority(signature, typed.module())
            .expect("authority-bearing checked callable identity");
        let RuntimeTypeIdentity::Function { params, .. } = &identity else {
            panic!("expected function identity")
        };
        assert!(params[0].is_authority(), "Console must remain an authority parameter");
        let capability = params[0].identity();
        assert!(matches!(
            capability,
            RuntimeTypeIdentity::Capability {
                authority: RuntimeCapabilityIdentity::Console
            }
        ));
        assert!(matches!(
            params[1].identity(),
            RuntimeTypeIdentity::Primitive(PrimitiveType::String)
        ));
        assert!(!params[1].is_authority());

        let plan = RuntimeTypePlan::build_with_runtime_shapes_and_checked_callables(
            std::iter::empty::<&Type>(),
            [signature],
            &catalog,
            typed.module(),
        )
        .expect("authority-bearing callable descriptor plan");
        assert!(plan.id(&identity).is_some());
        assert_eq!(plan.id(capability), None, "authority is not a readable value descriptor");

        let capability = capability.clone();
        let root_error = RuntimeTypePlan::build([capability.clone()])
            .expect_err("capability identity cannot become a descriptor root");
        assert_eq!(root_error, RuntimeTypeError::CapabilityDescriptorIdentity);

        let RuntimeTypeIdentity::Function { result, conventions, access, .. } = identity else {
            unreachable!("checked callable identity remains a function")
        };
        let forged_value = RuntimeTypeIdentity::Function {
            params: vec![RuntimeCallableParameterIdentity::value(capability)],
            result,
            conventions,
            access,
        };
        let value_error = RuntimeTypePlan::build([forged_value])
            .expect_err("capability identity cannot occupy a value parameter");
        assert_eq!(
            value_error,
            RuntimeTypeError::CapabilityInValueCallableParameter,
        );
    }

    #[test]
    fn direct_and_function_value_reflection_share_checked_access_identity() {
        let module = witchy_syntax::parser::parse_module(
            "mode opt\n\n\
             fn keep(text: let('source) String) -> View(String, 'source):\n    text\n\n\
             fn main() -> Int:\n    let callback = keep\n    0\n",
        )
        .expect("parse direct/function-value fixture");
        let typed = crate::typeck::annotate_checked(module)
            .expect("type-check direct/function-value fixture");
        let facts = crate::access::checked_facts(typed.module(), typed.table())
            .expect("checked access facts");
        let direct = facts.declaration("keep").expect("direct signature");
        let function_value = typed
            .module()
            .items
            .iter()
            .find_map(|item| match item {
                Item::Function(function) if function.name == "main" => {
                    function.body.stmts.iter().find_map(|statement| match statement {
                        witchy_syntax::ast::Stmt::Let { name, value, .. }
                            if name == "callback" => facts.callable_at(typed.module(), value),
                        _ => None,
                    })
                }
                _ => None,
            })
            .expect("function-value signature");
        let direct = RuntimeCallableAccessIdentity::from_checked(direct, &|_, _| None)
            .expect("direct reflection identity");
        let function_value = RuntimeCallableAccessIdentity::from_checked(
            function_value,
            &|_, _| None,
        )
        .expect("function-value reflection identity");
        assert_eq!(direct, function_value);
        assert_eq!(direct.borrow_relations()[0].owners[0].parameter, 0);
    }

    #[test]
    fn unit_has_one_identity_and_capabilities_are_rejected() {
        let unit = RuntimeTypeIdentity::from_resolved_type(&Type::Tuple(Vec::new()), &|_, _| None)
            .expect("unit identity");
        assert_eq!(unit, RuntimeTypeIdentity::Primitive(PrimitiveType::Unit));

        let error = RuntimeTypeIdentity::from_resolved_type(
            &Type::Named("Console".to_string(), Vec::new()),
            &|_, _| None,
        )
        .expect_err("capability descriptors are forbidden");
        assert_eq!(error, RuntimeTypeError::CapabilityType("Console".to_string()));
    }

    #[test]
    fn structural_identity_canonicalizes_encoded_member_order() {
        fn encoded(prefix: &str, members: &[(&str, usize)]) -> String {
            let mut name = format!("{prefix}{:010}", members.len());
            for (member, arity) in members {
                name.push_str(&format!("{:010}", member.len()));
                for byte in member.bytes() {
                    name.push_str(&format!("{byte:03}"));
                }
                if prefix == "__union" {
                    name.push_str(&format!("{arity:010}"));
                }
            }
            name
        }

        let string = Type::Named("String".to_string(), Vec::new());
        let int = Type::Named("Int".to_string(), Vec::new());
        let record_left = Type::Named(
            encoded("__anon", &[("a", 0), ("b", 0)]),
            vec![int.clone(), string.clone()],
        );
        let record_right = Type::Named(
            encoded("__anon", &[("b", 0), ("a", 0)]),
            vec![string.clone(), int.clone()],
        );
        let union_left = Type::Named(
            encoded("__union", &[("A", 1), ("B", 1)]),
            vec![int.clone(), string.clone()],
        );
        let union_right = Type::Named(
            encoded("__union", &[("B", 1), ("A", 1)]),
            vec![string, int],
        );
        for (left, right) in [(record_left, record_right), (union_left, union_right)] {
            let left = RuntimeTypeIdentity::from_resolved_type(&left, &|_, _| None)
                .expect("left structural identity");
            let right = RuntimeTypeIdentity::from_resolved_type(&right, &|_, _| None)
                .expect("right structural identity");
            assert_eq!(left, right);
        }
    }

    #[test]
    fn declaration_catalog_authenticates_aliases_and_rejects_rebinding() {
        let owner = ModuleLoadIdentity::new(
            package(
                PackageSource::Registry("coven".into()),
                "acme/model",
                "1.0.0",
            ),
            ["model"],
        )
        .expect("module owner");
        let mut catalog = RuntimeDeclarationCatalog::default();
        catalog
            .insert_resolved("model.User", &owner, "User", DeclarationKind::Type)
            .expect("canonical declaration");
        catalog
            .insert_resolved(
                "dependency_alias.User",
                &owner,
                "User",
                DeclarationKind::Type,
            )
            .expect("import alias for same declaration");

        let canonical = catalog
            .type_identity(&Type::Named("model.User".into(), Vec::new()))
            .expect("canonical identity");
        let aliased = catalog
            .type_identity(&Type::Named("dependency_alias.User".into(), Vec::new()))
            .expect("aliased identity");
        assert_eq!(canonical, aliased);

        let impostor = declaration(
            package(
                PackageSource::Registry("other-coven".into()),
                "acme/model",
                "1.0.0",
            ),
            "model",
            "User",
        );
        let error = catalog
            .insert("model.User", impostor)
            .expect_err("one resolved key cannot be rebound");
        assert!(matches!(
            error,
            RuntimeTypeError::ConflictingDeclaration {
                kind: DeclarationKind::Type,
                name,
            } if name == "model.User"
        ));
    }

    #[test]
    fn declaration_catalog_joins_linker_provenance_with_loader_ownership() {
        let owner = ModuleLoadIdentity::new(
            package(
                PackageSource::Registry("coven".into()),
                "acme/model",
                "1.0.0",
            ),
            ["src", "model"],
        )
        .expect("module owner");
        let declarations = ResolvedDeclarations {
            declarations: vec![
                ResolvedDeclaration {
                    compiler_name: "dependency_alias.User".into(),
                    source_module: "dependency_alias".into(),
                    local_name: "User".into(),
                    kind: ResolvedDeclarationKind::Type,
                },
                ResolvedDeclaration {
                    compiler_name: "dependency_alias.Render".into(),
                    source_module: "dependency_alias".into(),
                    local_name: "Render".into(),
                    kind: ResolvedDeclarationKind::Trait,
                },
            ],
        };
        let owners = AuthenticatedModuleOwners::from_loader_assignments(
            [("dependency_alias".to_string(), owner.clone())],
        )
        .expect("authenticated module ownership");

        let catalog = RuntimeDeclarationCatalog::from_resolved_declarations(
            &declarations,
            &owners,
        )
        .expect("authenticated catalog");
        let expected_type = owner
            .declaration(DeclarationKind::Type, "User")
            .expect("type identity");
        let expected_trait = owner
            .declaration(DeclarationKind::Trait, "Render")
            .expect("trait identity");

        assert_eq!(
            catalog.resolve("dependency_alias.User", DeclarationKind::Type),
            Some(&expected_type)
        );
        assert_eq!(
            catalog.resolve("dependency_alias.Render", DeclarationKind::Trait),
            Some(&expected_trait)
        );
    }

    #[test]
    fn declaration_catalog_rejects_missing_loader_ownership() {
        let declarations = ResolvedDeclarations {
            declarations: vec![ResolvedDeclaration {
                compiler_name: "unowned.User".into(),
                source_module: "unowned".into(),
                local_name: "User".into(),
                kind: ResolvedDeclarationKind::Type,
            }],
        };

        let owners = AuthenticatedModuleOwners::from_loader_assignments(
            [(
                "owned".to_string(),
                ModuleLoadIdentity::new(
                    package(PackageSource::Workspace, "app", "0.1.0"),
                    ["owned"],
                )
                .expect("owner"),
            )],
        )
        .expect("authenticated ownership");
        let error = RuntimeDeclarationCatalog::from_resolved_declarations(&declarations, &owners)
        .expect_err("unowned declarations must fail closed");
        assert_eq!(
            error,
            RuntimeTypeError::MissingModuleOwner {
                module: "unowned".into(),
            }
        );
    }

    #[test]
    fn authenticated_module_owners_reject_missing_and_conflicting_assignments() {
        let workspace = ModuleLoadIdentity::new(
            package(PackageSource::Workspace, "app", "0.1.0"),
            ["model"],
        )
        .expect("workspace owner");
        let registry = ModuleLoadIdentity::new(
            package(
                PackageSource::Registry("https://packages.example".into()),
                "acme/model",
                "1.0.0",
            ),
            ["model"],
        )
        .expect("registry owner");

        let incomplete = AuthenticatedModuleOwners::from_loader_assignments(
            [("main".to_string(), workspace.clone())],
        )
        .expect("structurally valid owner map");
        let missing = incomplete
            .validate_module_names(["main", "model"])
            .expect_err("every linked module requires an owner");
        assert_eq!(
            missing,
            RuntimeTypeError::MissingModuleOwner { module: "model".into() }
        );

        let conflict = AuthenticatedModuleOwners::from_loader_assignments(
            [
                ("model".to_string(), workspace),
                ("model".to_string(), registry),
            ],
        )
        .expect_err("one linker module key cannot have two owners");
        assert_eq!(
            conflict,
            RuntimeTypeError::ConflictingModuleOwner { module: "model".into() }
        );
    }

    #[test]
    fn resolved_type_plan_fails_atomically_on_unknown_or_capability_types() {
        let coordinate = package(PackageSource::Workspace, "app", "0.1.0");
        let user = declaration(coordinate, "main", "User");
        let mut catalog = RuntimeDeclarationCatalog::default();
        catalog.insert("main.User", user).expect("user declaration");
        let good = Type::Named("main.User".into(), Vec::new());
        let unknown = Type::Named("other.User".into(), Vec::new());
        let capability = Type::Named("Console".into(), Vec::new());

        let unknown_error = RuntimeTypePlan::from_resolved_types([&good, &unknown], &catalog)
            .expect_err("unknown declaration fails the complete plan");
        assert!(matches!(
            unknown_error,
            RuntimeTypeError::UnresolvedDeclaration { name, .. } if name == "other.User"
        ));
        let capability_error =
            RuntimeTypePlan::from_resolved_types([&good, &capability], &catalog)
                .expect_err("capability fails the complete plan");
        assert_eq!(
            capability_error,
            RuntimeTypeError::CapabilityType("Console".to_string())
        );
    }

    #[test]
    fn descriptor_plan_is_closed_over_nested_type_identities() {
        let int = RuntimeTypeIdentity::Primitive(PrimitiveType::Int);
        let tuple = RuntimeTypeIdentity::Tuple(vec![int.clone()]);
        let list = RuntimeTypeIdentity::List(Box::new(tuple.clone()));
        let plan = RuntimeTypePlan::build([list.clone()]).expect("closed plan");
        assert_eq!(plan.descriptors().len(), 3);
        assert!(plan.id(&int).is_some());
        assert!(plan.id(&tuple).is_some());
        assert!(plan.id(&list).is_some());
    }

    #[test]
    fn capability_free_identity_reports_the_nominal_retaining_path() {
        let module = witchy_syntax::parser::parse_module(
            "type Inner:\n    Inner(Console)\n\ntype Outer:\n    Outer(Inner)\n",
        )
        .expect("parse retaining types");
        let owner = ModuleLoadIdentity::new(
            package(PackageSource::Workspace, "app", "0.1.0"),
            ["main"],
        )
        .expect("module owner");
        let mut catalog = RuntimeDeclarationCatalog::default();
        catalog
            .insert_resolved("Inner", &owner, "Inner", DeclarationKind::Type)
            .expect("inner declaration");
        catalog
            .insert_resolved("Outer", &owner, "Outer", DeclarationKind::Type)
            .expect("outer declaration");

        let error = catalog
            .capability_free_type_identity(
                &Type::Named("Outer".into(), Vec::new()),
                &module,
            )
            .expect_err("transitive capability must be rejected");
        assert_eq!(
            error,
            RuntimeTypeError::CapabilityRetained {
                capability: "Console".into(),
                path: vec!["Outer[0]".into(), "Inner[0]".into()],
            }
        );
        assert!(error.to_string().contains("Outer[0] -> Inner[0]"));
    }

    #[test]
    fn capability_free_identity_substitutes_generics_and_terminates_recursion() {
        let module = witchy_syntax::parser::parse_module(
            "type Boxed:\n    Boxed(a)\n\ntype Grow:\n    More(Grow(List(a)))\n\ntype UnsafeGrow(a):\n    More(UnsafeGrow(Console))\n\ntype BadGrow(a):\n    More(BadGrow(a, String))\n\ntype Recursive:\n    Empty\n    Next(List(Recursive))\n",
        )
        .expect("parse generic and recursive types");
        let owner = ModuleLoadIdentity::new(
            package(PackageSource::Workspace, "app", "0.1.0"),
            ["main"],
        )
        .expect("module owner");
        let mut catalog = RuntimeDeclarationCatalog::default();
        for name in ["Boxed", "Grow", "UnsafeGrow", "BadGrow", "Recursive"] {
            catalog
                .insert_resolved(name, &owner, name, DeclarationKind::Type)
                .expect("declaration");
        }

        let error = catalog
            .capability_free_type_identity(
                &Type::Named(
                    "Boxed".into(),
                    vec![Type::Named("Console".into(), Vec::new())],
                ),
                &module,
            )
            .expect_err("generic capability payload must be rejected");
        assert_eq!(
            error,
            RuntimeTypeError::CapabilityRetained {
                capability: "Console".into(),
                path: vec!["Boxed argument[0]".into()],
            }
        );

        catalog
            .capability_free_type_identity(
                &Type::Named(
                    "Grow".into(),
                    vec![Type::Named("Int".into(), Vec::new())],
                ),
                &module,
            )
            .expect("argument-growing recursion terminates by declaration");

        let error = catalog
            .capability_free_type_identity(
                &Type::Named(
                    "UnsafeGrow".into(),
                    vec![Type::Named("Int".into(), Vec::new())],
                ),
                &module,
            )
            .expect_err("recursive transformed capability argument must be rejected");
        assert!(matches!(
            error,
            RuntimeTypeError::CapabilityRetained { ref capability, .. }
                if capability == "Console"
        ));

        let error = catalog
            .capability_free_type_identity(
                &Type::Named(
                    "BadGrow".into(),
                    vec![Type::Named("Int".into(), Vec::new())],
                ),
                &module,
            )
            .expect_err("recursive transformed arity must be checked");
        assert_eq!(
            error,
            RuntimeTypeError::RuntimeShapeArity {
                name: "BadGrow".into(),
                expected: 1,
                actual: 2,
            }
        );

        let recursive = catalog
            .capability_free_type_identity(
                &Type::Named("Recursive".into(), Vec::new()),
                &module,
            )
            .expect("safe recursion terminates");
        assert!(matches!(recursive, RuntimeTypeIdentity::Nominal { .. }));
    }

    #[test]
    fn runtime_record_shapes_substitute_inferred_type_parameters() {
        let module = witchy_syntax::parser::parse_module(
            "type Boxed:\n    value: a\n\ntype Grow(a):\n    next: Grow(List(a))\n\ntype Node(a):\n    value: a\n    next: Node(a)\n\ntype Left(a):\n    right: Right(a)\n\ntype Right(a):\n    left: Left(a)\n",
        )
        .expect("parse inferred generic record");
        let owner = ModuleLoadIdentity::new(
            package(PackageSource::Workspace, "app", "0.1.0"),
            ["main"],
        )
        .expect("module owner");
        let mut catalog = RuntimeDeclarationCatalog::default();
        for name in ["Boxed", "Grow", "Node", "Left", "Right"] {
            catalog
                .insert_resolved(name, &owner, name, DeclarationKind::Type)
                .expect("declaration");
        }
        let boxed = Type::Named(
            "Boxed".into(),
            vec![Type::Named("String".into(), Vec::new())],
        );
        let plan = RuntimeTypePlan::build_with_runtime_shapes([&boxed], &catalog, &module)
            .expect("inferred parameter participates in runtime shape substitution");
        let identity = catalog.type_identity(&boxed).expect("boxed identity");
        let id = plan.id(&identity).expect("boxed descriptor");
        assert!(matches!(
            plan.shape(id),
            Some(RuntimeTypeShape::Record(fields))
                if fields.len() == 1 && fields[0].name == "value" && fields[0].display == "String"
        ));
        let grow = Type::Named(
            "Grow".into(),
            vec![Type::Named("Int".into(), Vec::new())],
        );
        let error = RuntimeTypePlan::build_with_runtime_shapes([&grow], &catalog, &module)
            .expect_err("argument-growing record shape has no finite closure");
        assert!(matches!(
            error,
            RuntimeTypeError::ExpandingRuntimeShapeRecursion { .. }
        ));

        let node = Type::Named(
            "Node".into(),
            vec![Type::Named("Int".into(), Vec::new())],
        );
        let plan = RuntimeTypePlan::build_with_runtime_shapes([&node], &catalog, &module)
            .expect("exact recursive record has a finite closure");
        let node_identity = catalog.type_identity(&node).expect("node identity");
        let node_id = plan.id(&node_identity).expect("node descriptor");
        let Some(RuntimeTypeShape::Record(node_fields)) = plan.shape(node_id) else {
            panic!("Node must retain its record shape");
        };
        let next = node_fields
            .iter()
            .find(|field| field.name == "next")
            .expect("next field");
        assert_eq!(next.descriptor, node_id);

        let left = Type::Named(
            "Left".into(),
            vec![Type::Named("String".into(), Vec::new())],
        );
        let right = Type::Named(
            "Right".into(),
            vec![Type::Named("String".into(), Vec::new())],
        );
        let plan = RuntimeTypePlan::build_with_runtime_shapes([&left], &catalog, &module)
            .expect("mutually recursive records have a finite closure");
        let left_id = plan
            .id(&catalog.type_identity(&left).expect("left identity"))
            .expect("left descriptor");
        let right_id = plan
            .id(&catalog.type_identity(&right).expect("right identity"))
            .expect("right descriptor");
        let Some(RuntimeTypeShape::Record(left_fields)) = plan.shape(left_id) else {
            panic!("Left must retain its record shape");
        };
        let Some(RuntimeTypeShape::Record(right_fields)) = plan.shape(right_id) else {
            panic!("Right must retain its record shape");
        };
        assert_eq!(left_fields[0].descriptor, right_id);
        assert_eq!(right_fields[0].descriptor, left_id);
    }
}
