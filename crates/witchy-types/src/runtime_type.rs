//! RFC-0082 backend-neutral runtime type identities and descriptor plans.
//!
//! Runtime descriptors must not key on rendered type names. This module turns
//! resolved compiler types into structural identities rooted in an explicit
//! package/module/declaration identity, then assigns deterministic dense IDs for
//! the closed program. Backends consume the resulting plan; neither backend is
//! allowed to rediscover identity from a string at runtime.

use std::collections::{BTreeMap, BTreeSet};

use witchy_syntax::ast::{Convention, Type};
use witchy_syntax::type_resolve::{ResolvedDeclarationKind, ResolvedDeclarations};

/// The immutable package coordinate that owns a declaration.
///
/// `name` is the manifest's full rune name, not an import alias. `version` is
/// the selected manifest/lock version. The source keeps a workspace package and
/// a registry release with the same name/version from collapsing accidentally.
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
    pub fn from_resolved_declarations(
        declarations: &ResolvedDeclarations,
        module_owners: &BTreeMap<String, ModuleLoadIdentity>,
    ) -> Result<Self, RuntimeTypeError> {
        let mut catalog = Self::default();
        for declaration in &declarations.declarations {
            let owner = module_owners.get(&declaration.source_module).ok_or_else(|| {
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
    pub fn insert_resolved(
        &mut self,
        resolved_name: impl Into<String>,
        owner: &ModuleLoadIdentity,
        local_name: impl Into<String>,
        kind: DeclarationKind,
    ) -> Result<(), RuntimeTypeError> {
        self.insert(resolved_name, owner.declaration(kind, local_name)?)
    }

    pub fn insert(
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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PrimitiveType {
    Int,
    Float,
    Duration,
    String,
    Bytes,
    Bool,
    Unit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RuntimeConvention {
    Let,
    Borrow,
    Var,
    Own,
}

impl From<Convention> for RuntimeConvention {
    fn from(value: Convention) -> Self {
        match value {
            Convention::Let => Self::Let,
            Convention::Borrow => Self::Borrow,
            Convention::Var => Self::Var,
            Convention::Own => Self::Own,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UnionVariantIdentity {
    pub tag: String,
    pub payloads: Vec<RuntimeTypeIdentity>,
}

/// Canonical semantic identity of one runtime-representable type.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RuntimeTypeIdentity {
    Primitive(PrimitiveType),
    List(Box<Self>),
    Tuple(Vec<Self>),
    Function {
        params: Vec<Self>,
        result: Box<Self>,
        conventions: Vec<RuntimeConvention>,
    },
    Nominal {
        declaration: DeclarationIdentity,
        arguments: Vec<Self>,
    },
    Existential {
        declaration: DeclarationIdentity,
        arguments: Vec<Self>,
    },
    Record(Vec<(String, Self)>),
    Union(Vec<UnionVariantIdentity>),
}

impl RuntimeTypeIdentity {
    /// Convert a fully resolved Witchy type into runtime identity.
    ///
    /// `resolve` is the sole nominal-identity authority. It receives the
    /// canonical compiler name plus the expected declaration kind; returning
    /// `None` is a loud error rather than a fallback to that name.
    pub fn from_resolved_type(
        ty: &Type,
        resolve: &impl Fn(&str, DeclarationKind) -> Option<DeclarationIdentity>,
    ) -> Result<Self, RuntimeTypeError> {
        match ty {
            Type::Qualified(_, inner) => Self::from_resolved_type(inner, resolve),
            Type::RecordCompose { .. } => Err(RuntimeTypeError::MalformedStructuralType(
                "compiler invariant violated: structural record composition reached runtime type identity before records::lower normalized it"
                    .to_string(),
            )),
            Type::Tuple(items) if items.is_empty() => Ok(Self::Primitive(PrimitiveType::Unit)),
            Type::Tuple(items) => Ok(Self::Tuple(
                items
                    .iter()
                    .map(|item| Self::from_resolved_type(item, resolve))
                    .collect::<Result<_, _>>()?,
            )),
            Type::Fn(params, result, conventions) => {
                if !conventions.is_empty() && conventions.len() != params.len() {
                    return Err(RuntimeTypeError::ConventionArity {
                        params: params.len(),
                        conventions: conventions.len(),
                    });
                }
                let conventions = if conventions.is_empty() {
                    vec![RuntimeConvention::Let; params.len()]
                } else {
                    conventions.iter().copied().map(Into::into).collect()
                };
                Ok(Self::Function {
                    params: params
                        .iter()
                        .map(|param| Self::from_resolved_type(param, resolve))
                        .collect::<Result<_, _>>()?,
                    result: Box::new(Self::from_resolved_type(result, resolve)?),
                    conventions,
                })
            }
            Type::Dyn(name, args) => Ok(Self::Existential {
                declaration: resolve(name, DeclarationKind::Trait).ok_or_else(|| {
                    RuntimeTypeError::UnresolvedDeclaration {
                        kind: DeclarationKind::Trait,
                        name: name.clone(),
                    }
                })?,
                arguments: convert_arguments(args, resolve)?,
            }),
            Type::Named(name, args) => {
                if capability_type(name) {
                    return Err(RuntimeTypeError::CapabilityType(name.clone()));
                }
                if let Some(primitive) = primitive(name, args.len()) {
                    return Ok(Self::Primitive(primitive));
                }
                if name == "List" && args.len() == 1 {
                    return Ok(Self::List(Box::new(Self::from_resolved_type(
                        &args[0], resolve,
                    )?)));
                }
                if let Some(fields) = decode_anon_record(name) {
                    if fields.len() != args.len() {
                        return Err(RuntimeTypeError::MalformedStructuralType(format!(
                            "anonymous record head has {} field(s) but {} type argument(s)",
                            fields.len(),
                            args.len()
                        )));
                    }
                    let mut fields = fields
                            .into_iter()
                            .zip(args)
                            .map(|(field, ty)| {
                                Ok((field, Self::from_resolved_type(ty, resolve)?))
                            })
                            .collect::<Result<Vec<_>, RuntimeTypeError>>()?;
                    fields.sort_by(|left, right| left.0.cmp(&right.0));
                    return Ok(Self::Record(fields));
                }
                if let Some(variants) = crate::typeck::anon_union_synthetic_variants(name) {
                    return convert_union(variants, args, resolve);
                }
                Ok(Self::Nominal {
                    declaration: resolve(name, DeclarationKind::Type).ok_or_else(|| {
                        RuntimeTypeError::UnresolvedDeclaration {
                            kind: DeclarationKind::Type,
                            name: name.clone(),
                        }
                    })?,
                    arguments: convert_arguments(args, resolve)?,
                })
            }
        }
    }
}

fn convert_arguments(
    args: &[Type],
    resolve: &impl Fn(&str, DeclarationKind) -> Option<DeclarationIdentity>,
) -> Result<Vec<RuntimeTypeIdentity>, RuntimeTypeError> {
    args.iter()
        .map(|arg| RuntimeTypeIdentity::from_resolved_type(arg, resolve))
        .collect()
}

fn convert_union(
    variants: Vec<(String, usize)>,
    args: &[Type],
    resolve: &impl Fn(&str, DeclarationKind) -> Option<DeclarationIdentity>,
) -> Result<RuntimeTypeIdentity, RuntimeTypeError> {
    let expected: usize = variants.iter().map(|(_, arity)| arity).sum();
    if expected != args.len() {
        return Err(RuntimeTypeError::MalformedStructuralType(format!(
            "anonymous union head requires {expected} payload type(s), got {}",
            args.len()
        )));
    }
    let mut at = 0;
    let mut converted = Vec::with_capacity(variants.len());
    for (tag, arity) in variants {
        let payloads = args[at..at + arity]
            .iter()
            .map(|ty| RuntimeTypeIdentity::from_resolved_type(ty, resolve))
            .collect::<Result<_, _>>()?;
        converted.push(UnionVariantIdentity { tag, payloads });
        at += arity;
    }
    converted.sort_by(|left, right| left.tag.cmp(&right.tag));
    Ok(RuntimeTypeIdentity::Union(converted))
}

fn primitive(name: &str, arity: usize) -> Option<PrimitiveType> {
    if arity != 0 {
        return None;
    }
    match name {
        "Int" => Some(PrimitiveType::Int),
        "Float" => Some(PrimitiveType::Float),
        "Duration" => Some(PrimitiveType::Duration),
        "String" => Some(PrimitiveType::String),
        "Bytes" => Some(PrimitiveType::Bytes),
        "Bool" => Some(PrimitiveType::Bool),
        "Nil" | "()" => Some(PrimitiveType::Unit),
        _ => None,
    }
}

fn capability_type(name: &str) -> bool {
    matches!(
        name,
        "Console"
            | "Clock"
            | "Rand"
            | "Env"
            | "Secret"
            | "Exec"
            | "Dir"
            | "File"
            | "Net"
            | "Socket"
            | "Listener"
            | "BuildOut"
            | "BuildRead"
            | "BuildEnv"
            | "BuildNet"
            | "BuildExec"
    )
}

fn decode_anon_record(name: &str) -> Option<Vec<String>> {
    let mut at = "__anon".len();
    name.strip_prefix("__anon")?;
    let count = fixed_width(name, &mut at, 10)?;
    let mut fields = Vec::with_capacity(count);
    for _ in 0..count {
        let bytes = fixed_width(name, &mut at, 10)?;
        let mut field = Vec::with_capacity(bytes);
        for _ in 0..bytes {
            let byte = fixed_width(name, &mut at, 3)?;
            field.push(u8::try_from(byte).ok()?);
        }
        fields.push(String::from_utf8(field).ok()?);
    }
    (at == name.len()).then_some(fields)
}

fn fixed_width(text: &str, at: &mut usize, width: usize) -> Option<usize> {
    let end = at.checked_add(width)?;
    let part = text.get(*at..end)?;
    if !part.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    *at = end;
    part.parse().ok()
}

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

/// Deterministic descriptor constants for one closed program.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RuntimeTypePlan {
    descriptors: Vec<RuntimeTypeDescriptor>,
    by_identity: BTreeMap<RuntimeTypeIdentity, RuntimeTypeId>,
}

impl RuntimeTypePlan {
    pub fn build(
        identities: impl IntoIterator<Item = RuntimeTypeIdentity>,
    ) -> Result<Self, RuntimeTypeError> {
        let mut identities = identities.into_iter().collect::<BTreeSet<_>>();
        let roots = identities.iter().cloned().collect::<Vec<_>>();
        for root in &roots {
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
        Ok(Self { descriptors, by_identity })
    }

    /// Build the backend-neutral descriptor constants for resolved compiler
    /// types. Every nominal and existential head must already be authenticated
    /// by `catalog`; unresolved names and capabilities fail before a backend can
    /// observe a partial plan.
    pub fn from_resolved_types<'a>(
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
}

fn collect_nested_identities(
    identity: &RuntimeTypeIdentity,
    identities: &mut BTreeSet<RuntimeTypeIdentity>,
) {
    let children: Vec<&RuntimeTypeIdentity> = match identity {
        RuntimeTypeIdentity::Primitive(_) => Vec::new(),
        RuntimeTypeIdentity::List(item) => vec![item],
        RuntimeTypeIdentity::Tuple(items) => items.iter().collect(),
        RuntimeTypeIdentity::Function { params, result, .. } => {
            params.iter().chain(std::iter::once(result.as_ref())).collect()
        }
        RuntimeTypeIdentity::Nominal { arguments, .. }
        | RuntimeTypeIdentity::Existential { arguments, .. } => arguments.iter().collect(),
        RuntimeTypeIdentity::Record(fields) => fields.iter().map(|(_, ty)| ty).collect(),
        RuntimeTypeIdentity::Union(variants) => variants
            .iter()
            .flat_map(|variant| variant.payloads.iter())
            .collect(),
    };
    for child in children {
        if identities.insert(child.clone()) {
            collect_nested_identities(child, identities);
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuntimeTypeError {
    InvalidPackageCoordinate(String),
    InvalidDeclarationIdentity(String),
    CapabilityType(String),
    MissingModuleOwner { module: String },
    ConflictingDeclaration { kind: DeclarationKind, name: String },
    UnresolvedDeclaration { kind: DeclarationKind, name: String },
    ConventionArity { params: usize, conventions: usize },
    MalformedStructuralType(String),
    TooManyDescriptors,
}

impl std::fmt::Display for RuntimeTypeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidPackageCoordinate(message)
            | Self::InvalidDeclarationIdentity(message)
            | Self::MalformedStructuralType(message) => f.write_str(message),
            Self::CapabilityType(name) => {
                write!(f, "capability type `{name}` cannot have a runtime descriptor")
            }
            Self::MissingModuleOwner { module } => write!(
                f,
                "runtime descriptor declarations from module `{module}` lack loader ownership"
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
        let owners = BTreeMap::from([("dependency_alias".to_string(), owner.clone())]);

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

        let error = RuntimeDeclarationCatalog::from_resolved_declarations(
            &declarations,
            &BTreeMap::new(),
        )
        .expect_err("unowned declarations must fail closed");
        assert_eq!(
            error,
            RuntimeTypeError::MissingModuleOwner {
                module: "unowned".into(),
            }
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
}
