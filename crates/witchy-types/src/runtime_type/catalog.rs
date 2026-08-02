use super::*;

use witchy_syntax::ast::TypeDef;
use witchy_syntax::type_resolve::{ResolvedDeclarationKind, ResolvedDeclarations};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PackageCoordinate {
    source: PackageSource,
    name: String,
    version: String,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PackageSource {
    Toolchain,
    Workspace,
    Registry(String),
}

impl PackageCoordinate {
    pub fn new(
        source: PackageSource,
        name: impl Into<String>,
        version: impl Into<String>,
    ) -> Result<Self, RuntimeTypeError> {
        let name = name.into();
        let version = version.into();
        if name.is_empty() {
            return Err(RuntimeTypeError::InvalidPackageCoordinate(
                "package name is empty".to_string(),
            ));
        }
        if version.is_empty() {
            return Err(RuntimeTypeError::InvalidPackageCoordinate(format!(
                "package `{name}` has an empty version"
            )));
        }
        if matches!(&source, PackageSource::Registry(registry) if registry.is_empty()) {
            return Err(RuntimeTypeError::InvalidPackageCoordinate(format!(
                "package `{name}@{version}` has an empty registry identity"
            )));
        }
        Ok(Self { source, name, version })
    }

    pub fn source(&self) -> &PackageSource {
        &self.source
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn version(&self) -> &str {
        &self.version
    }
}

/// Immutable ownership assigned by the loader before a module enters linking.
///
/// `module_path` is the module's logical path inside its package, not the local
/// import alias chosen by a dependent package.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ModuleLoadIdentity {
    package: PackageCoordinate,
    module_path: Vec<String>,
}

impl ModuleLoadIdentity {
    pub fn new(
        package: PackageCoordinate,
        module_path: impl IntoIterator<Item = impl Into<String>>,
    ) -> Result<Self, RuntimeTypeError> {
        let module_path: Vec<String> = module_path.into_iter().map(Into::into).collect();
        if module_path.is_empty() || module_path.iter().any(String::is_empty) {
            return Err(RuntimeTypeError::InvalidDeclarationIdentity(
                "loaded module has an empty logical module path".to_string(),
            ));
        }
        Ok(Self { package, module_path })
    }

    pub fn package(&self) -> &PackageCoordinate {
        &self.package
    }

    pub fn module_path(&self) -> &[String] {
        &self.module_path
    }

    pub fn declaration(
        &self,
        kind: DeclarationKind,
        local_name: impl Into<String>,
    ) -> Result<DeclarationIdentity, RuntimeTypeError> {
        DeclarationIdentity::new(
            self.package.clone(),
            self.module_path.clone(),
            kind,
            local_name,
        )
    }
}

/// Opaque module ownership retained from an authenticated loader decision.
///
/// Module keys are the input names passed to the linker plus any canonical
/// toolchain modules the linker adds. Package and logical module identities
/// come from the resolved package graph, never from those keys. Construction
/// rejects malformed or conflicting loader assignments, and checked-link APIs
/// require coverage of the complete post-link set before retaining this map.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthenticatedModuleOwners {
    owners: BTreeMap<String, ModuleLoadIdentity>,
}

impl AuthenticatedModuleOwners {
    pub fn from_loader_assignments<A>(
        assignments: A,
    ) -> Result<Self, RuntimeTypeError>
    where
        A: IntoIterator<Item = (String, ModuleLoadIdentity)>,
    {
        let mut owners = BTreeMap::new();
        for (module, owner) in assignments {
            if module.is_empty() {
                return Err(RuntimeTypeError::InvalidModuleOwner(
                    "loader ownership has an empty linker module key".to_string(),
                ));
            }
            if let Some(existing) = owners.get(&module) {
                if existing != &owner {
                    return Err(RuntimeTypeError::ConflictingModuleOwner { module });
                }
                continue;
            }
            owners.insert(module, owner);
        }
        Ok(Self { owners })
    }

    pub(crate) fn validate_module_names<M, N>(
        &self,
        module_names: M,
    ) -> Result<(), RuntimeTypeError>
    where
        M: IntoIterator<Item = N>,
        N: Into<String>,
    {
        let mut loaded = BTreeSet::new();
        for module in module_names {
            let module = module.into();
            if module.is_empty() {
                return Err(RuntimeTypeError::InvalidModuleOwner(
                    "linked module has an empty loader key".to_string(),
                ));
            }
            loaded.insert(module);
        }
        if let Some(module) = loaded.iter().find(|module| !self.owners.contains_key(*module)) {
            return Err(RuntimeTypeError::MissingModuleOwner { module: module.to_string() });
        }
        Ok(())
    }
}

/// Resolved identity of one nominal type or trait declaration.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DeclarationIdentity {
    package: PackageCoordinate,
    module: Vec<String>,
    kind: DeclarationKind,
    name: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DeclarationKind {
    Type,
    Trait,
}

impl DeclarationIdentity {
    pub fn new(
        package: PackageCoordinate,
        module: impl IntoIterator<Item = impl Into<String>>,
        kind: DeclarationKind,
        name: impl Into<String>,
    ) -> Result<Self, RuntimeTypeError> {
        let module: Vec<String> = module.into_iter().map(Into::into).collect();
        let name = name.into();
        if module.is_empty() || module.iter().any(String::is_empty) {
            return Err(RuntimeTypeError::InvalidDeclarationIdentity(format!(
                "declaration `{name}` has an empty module path"
            )));
        }
        if name.is_empty() {
            return Err(RuntimeTypeError::InvalidDeclarationIdentity(
                "declaration name is empty".to_string(),
            ));
        }
        Ok(Self { package, module, kind, name })
    }

    pub fn package(&self) -> &PackageCoordinate {
        &self.package
    }

    pub fn module(&self) -> &[String] {
        &self.module
    }

    pub fn kind(&self) -> DeclarationKind {
        self.kind
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

/// Authenticated mapping from the compiler's resolved declaration keys to
/// package-stable identities.
///
/// The keys are compile-time names only. They never enter a runtime descriptor;
/// import aliases may map to the same declaration identity, while one resolved
/// key may not be rebound to a different declaration.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RuntimeDeclarationCatalog {
    declarations: BTreeMap<DeclarationKind, BTreeMap<String, DeclarationIdentity>>,
}

impl RuntimeDeclarationCatalog {
    /// Join linker-retained declaration provenance with loader-authenticated
    /// module ownership. Every source module must have an owner; compiler names
    /// are lookup keys and are never parsed to recover package identity.
    pub(crate) fn from_resolved_declarations(
        declarations: &ResolvedDeclarations,
        module_owners: &AuthenticatedModuleOwners,
    ) -> Result<Self, RuntimeTypeError> {
        let mut catalog = Self::default();
        for declaration in &declarations.declarations {
            let owner = module_owners.owners.get(&declaration.source_module).ok_or_else(|| {
                RuntimeTypeError::MissingModuleOwner {
                    module: declaration.source_module.clone(),
                }
            })?;
            let kind = match declaration.kind {
                ResolvedDeclarationKind::Type => DeclarationKind::Type,
                ResolvedDeclarationKind::Trait => DeclarationKind::Trait,
            };
            catalog.insert_resolved(
                &declaration.compiler_name,
                owner,
                &declaration.local_name,
                kind,
            )?;
        }
        Ok(catalog)
    }

    /// Authenticate one linker-resolved compiler name from loader provenance.
    /// The compiler name is deliberately not parsed to recover package, module,
    /// or local declaration identity.
    pub(crate) fn insert_resolved(
        &mut self,
        resolved_name: impl Into<String>,
        owner: &ModuleLoadIdentity,
        local_name: impl Into<String>,
        kind: DeclarationKind,
    ) -> Result<(), RuntimeTypeError> {
        self.insert(resolved_name, owner.declaration(kind, local_name)?)
    }

    pub(crate) fn insert(
        &mut self,
        resolved_name: impl Into<String>,
        identity: DeclarationIdentity,
    ) -> Result<(), RuntimeTypeError> {
        let resolved_name = resolved_name.into();
        if resolved_name.is_empty() {
            return Err(RuntimeTypeError::InvalidDeclarationIdentity(
                "resolved declaration name is empty".to_string(),
            ));
        }
        let declarations = self.declarations.entry(identity.kind).or_default();
        if let Some(existing) = declarations.get(&resolved_name) {
            if existing == &identity {
                return Ok(());
            }
            return Err(RuntimeTypeError::ConflictingDeclaration {
                kind: identity.kind,
                name: resolved_name,
            });
        }
        declarations.insert(resolved_name, identity);
        Ok(())
    }

    pub fn resolve(
        &self,
        resolved_name: &str,
        kind: DeclarationKind,
    ) -> Option<&DeclarationIdentity> {
        self.declarations.get(&kind)?.get(resolved_name)
    }

    pub fn type_identity(&self, ty: &Type) -> Result<RuntimeTypeIdentity, RuntimeTypeError> {
        RuntimeTypeIdentity::from_resolved_type(ty, &|name, kind| {
            self.resolve(name, kind).cloned()
        })
    }

    /// Resolve one callable through the authenticated declaration catalog while
    /// retaining the exact module-owned access relation supplied by
    /// `CheckedAccessFacts`.
    pub fn checked_callable_identity(
        &self,
        signature: &crate::access::AccessSignature,
    ) -> Result<RuntimeTypeIdentity, RuntimeTypeError> {
        RuntimeTypeIdentity::from_checked_callable(signature, &|name, kind| {
            self.resolve(name, kind).cloned()
        })
    }

    /// Resolve a runtime identity only after proving that its complete nominal
    /// payload graph is capability-free. The diagnostic path is expressed in
    /// declaration/field/constructor terms so a source conversion can identify
    /// the retaining edge rather than merely naming the leaf capability.
    pub fn capability_free_type_identity(
        &self,
        ty: &Type,
        module: &Module,
    ) -> Result<RuntimeTypeIdentity, RuntimeTypeError> {
        let mut definitions = BTreeMap::new();
        for item in &module.items {
            let Item::Type(definition) = item else { continue };
            let Some(identity) = self.resolve(&definition.name, DeclarationKind::Type) else {
                continue;
            };
            definitions.insert(identity.clone(), definition);
        }
        let mut visiting = Vec::new();
        validate_capability_free_type(
            ty,
            self,
            &definitions,
            &BTreeMap::new(),
            &mut visiting,
            &[],
        )?;
        self.type_identity(ty)
    }
}

fn validate_capability_free_type(
    ty: &Type,
    catalog: &RuntimeDeclarationCatalog,
    definitions: &BTreeMap<DeclarationIdentity, &TypeDef>,
    bindings: &BTreeMap<String, Type>,
    visiting: &mut Vec<DeclarationIdentity>,
    path: &[String],
) -> Result<(), RuntimeTypeError> {
    match ty {
        Type::Qualified(_, inner) => validate_capability_free_type(
            inner,
            catalog,
            definitions,
            bindings,
            visiting,
            path,
        ),
        Type::Tuple(items) => {
            for (index, item) in items.iter().enumerate() {
                let mut child_path = path.to_vec();
                child_path.push(format!("tuple[{index}]"));
                validate_capability_free_type(
                    item,
                    catalog,
                    definitions,
                    bindings,
                    visiting,
                    &child_path,
                )?;
            }
            Ok(())
        }
        Type::Fn(_, _, _) => Err(RuntimeTypeError::UninspectableDynamicPayload {
            kind: "function".into(),
            path: path.to_vec(),
        }),
        Type::Dyn(name, _) => Err(RuntimeTypeError::UninspectableDynamicPayload {
            kind: format!("existential `{name}`"),
            path: path.to_vec(),
        }),
        Type::RecordCompose { base, fields } => {
            validate_capability_free_type(
                base,
                catalog,
                definitions,
                bindings,
                visiting,
                path,
            )?;
            for (name, field) in fields {
                let mut child_path = path.to_vec();
                child_path.push(name.clone());
                validate_capability_free_type(
                    field,
                    catalog,
                    definitions,
                    bindings,
                    visiting,
                    &child_path,
                )?;
            }
            Ok(())
        }
        Type::Named(name, args) => {
            if args.is_empty() {
                if let Some(bound) = bindings.get(name) {
                    return validate_capability_free_type(
                        bound,
                        catalog,
                        definitions,
                        bindings,
                        visiting,
                        path,
                    );
                }
            }
            if super::identity::capability_type(name) {
                return Err(RuntimeTypeError::CapabilityRetained {
                    capability: name.clone(),
                    path: path.to_vec(),
                });
            }
            if super::identity::primitive(name, args.len()).is_some() {
                return Ok(());
            }
            if name == "List" && args.len() == 1 {
                let mut child_path = path.to_vec();
                child_path.push("list item".into());
                return validate_capability_free_type(
                    &args[0],
                    catalog,
                    definitions,
                    bindings,
                    visiting,
                    &child_path,
                );
            }
            if let Some(fields) = decode_anon_record(name) {
                for (field, ty) in fields.into_iter().zip(args) {
                    let mut child_path = path.to_vec();
                    child_path.push(field);
                    validate_capability_free_type(
                        ty,
                        catalog,
                        definitions,
                        bindings,
                        visiting,
                        &child_path,
                    )?;
                }
                return Ok(());
            }
            if let Some(variants) = crate::typeck::anon_union_synthetic_variants(name) {
                let mut at = 0;
                for (variant, arity) in variants {
                    for index in 0..arity {
                        let mut child_path = path.to_vec();
                        child_path.push(format!("{variant}[{index}]"));
                        if let Some(payload) = args.get(at) {
                            validate_capability_free_type(
                                payload,
                                catalog,
                                definitions,
                                bindings,
                                visiting,
                                &child_path,
                            )?;
                        }
                        at += 1;
                    }
                }
                return Ok(());
            }

            let instantiated = instantiate_runtime_type(ty, bindings);
            let Some(declaration) = catalog.resolve(name, DeclarationKind::Type).cloned() else {
                return catalog.type_identity(&instantiated).map(|_| ());
            };
            let instantiated_args = args
                .iter()
                .map(|argument| instantiate_runtime_type(argument, bindings))
                .collect::<Vec<_>>();
            let definition = definitions.get(&declaration).ok_or_else(|| {
                RuntimeTypeError::MissingRuntimeShape {
                    declaration: Box::new(declaration.clone()),
                }
            })?;
            if definition.is_capability {
                return Err(RuntimeTypeError::CapabilityRetained {
                    capability: definition.name.clone(),
                    path: path.to_vec(),
                });
            }
            let parameters = crate::typeck::type_def_params(definition);
            if parameters.len() != instantiated_args.len() {
                return Err(RuntimeTypeError::RuntimeShapeArity {
                    name: definition.name.clone(),
                    expected: parameters.len(),
                    actual: instantiated_args.len(),
                });
            }
            for (index, argument) in instantiated_args.iter().enumerate() {
                let mut child_path = path.to_vec();
                child_path.push(format!("{} argument[{index}]", definition.name));
                validate_capability_free_type(
                    argument,
                    catalog,
                    definitions,
                    &BTreeMap::new(),
                    visiting,
                    &child_path,
                )?;
            }
            if visiting.contains(&declaration) {
                return Ok(());
            }
            visiting.push(declaration);
            let mut nested_bindings = bindings.clone();
            for (parameter, argument) in parameters.into_iter().zip(instantiated_args) {
                nested_bindings.insert(parameter, argument);
            }
            for variant in &definition.variants {
                for (index, field) in variant.fields.iter().enumerate() {
                    let label = variant.field_names.get(index).map_or_else(
                        || format!("{}[{}]", variant.name, index),
                        |field| format!("{}.{}", definition.name, field),
                    );
                    let mut child_path = path.to_vec();
                    child_path.push(label);
                    validate_capability_free_type(
                        field,
                        catalog,
                        definitions,
                        &nested_bindings,
                        visiting,
                        &child_path,
                    )?;
                }
            }
            visiting.pop();
            Ok(())
        }
    }
}

pub(super) fn instantiate_runtime_type(ty: &Type, bindings: &BTreeMap<String, Type>) -> Type {
    match ty {
        Type::Named(name, args) if args.is_empty() && bindings.contains_key(name) => {
            bindings[name].clone()
        }
        Type::Named(name, args) => Type::Named(
            name.clone(),
            args.iter()
                .map(|arg| instantiate_runtime_type(arg, bindings))
                .collect(),
        ),
        Type::Dyn(name, args) => Type::Dyn(
            name.clone(),
            args.iter()
                .map(|arg| instantiate_runtime_type(arg, bindings))
                .collect(),
        ),
        Type::Tuple(items) => Type::Tuple(
            items
                .iter()
                .map(|item| instantiate_runtime_type(item, bindings))
                .collect(),
        ),
        Type::Fn(params, result, conventions) => Type::Fn(
            params
                .iter()
                .map(|param| instantiate_runtime_type(param, bindings))
                .collect(),
            Box::new(instantiate_runtime_type(result, bindings)),
            conventions.clone(),
        ),
        Type::Qualified(qualifier, inner) => Type::Qualified(
            qualifier.clone(),
            Box::new(instantiate_runtime_type(inner, bindings)),
        ),
        Type::RecordCompose { base, fields } => Type::RecordCompose {
            base: Box::new(instantiate_runtime_type(base, bindings)),
            fields: fields
                .iter()
                .map(|(name, field)| {
                    (name.clone(), instantiate_runtime_type(field, bindings))
                })
                .collect(),
        },
    }
}
