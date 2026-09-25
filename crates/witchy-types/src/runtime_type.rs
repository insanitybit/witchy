//! RFC-0082 backend-neutral runtime type identities and descriptor plans.
//!
//! Runtime descriptors must not key on rendered type names. This module turns
//! resolved compiler types into structural identities rooted in an explicit
//! package/module/declaration identity, then assigns deterministic dense IDs for
//! the closed program. Backends consume the resulting plan; neither backend is
//! allowed to rediscover identity from a string at runtime.

use std::collections::{BTreeMap, BTreeSet};

use witchy_syntax::ast::{Item, Module, Type};

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
        callables: impl IntoIterator<Item = &'a witchy_canonical_source::access::AccessSignature>,
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
        Type::Slice(elem) => {
            collect_runtime_type_shape(
                elem,
                catalog,
                module,
                identities,
                drafts,
                visiting,
            )?;
        }
        Type::Fn(parameters, result, _, _) => {
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
    CapabilityDescriptorRetained { capability: String, path: Vec<String> },
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
            Self::CapabilityDescriptorRetained { capability, path } => {
                write!(f, "capability type `{capability}` cannot have a runtime descriptor")?;
                if !path.is_empty() {
                    write!(f, "; retained by `{}`", path.join(" -> "))?;
                }
                Ok(())
            }
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
