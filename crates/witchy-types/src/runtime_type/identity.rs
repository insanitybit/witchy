use super::*;

use witchy_syntax::ast::Convention;

use crate::access::{
    AccessKind, AccessQualifier, AccessSignature, LoanProjection, LoanProjectionStep,
};

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

/// Compiler-authenticated identity for one toolchain capability. These names
/// are language ABI, not loader/display strings, and never receive readable
/// runtime descriptors.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RuntimeCapabilityIdentity {
    Console,
    Clock,
    Rand,
    Env,
    Secret,
    Exec,
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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RuntimeConvention {
    Let,
    Borrow,
    Var,
    Own,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RuntimeAccessKind {
    OwnedImmutable,
    SharedBorrow,
    ExclusiveWriteback,
    Consuming,
}

impl From<AccessKind> for RuntimeAccessKind {
    fn from(value: AccessKind) -> Self {
        match value {
            AccessKind::OwnedImmutable => Self::OwnedImmutable,
            AccessKind::SharedBorrow => Self::SharedBorrow,
            AccessKind::ExclusiveWriteback => Self::ExclusiveWriteback,
            AccessKind::Consuming => Self::Consuming,
        }
    }
}

/// One logical route to a qualifier inside a callable parameter or result.
/// These are source-type positions, never physical layout offsets.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RuntimeQualifierPathStep {
    TypeArgument(usize),
    TupleItem(usize),
    FunctionParameter(usize),
    FunctionResult,
    ExistentialArgument(usize),
    RecordBase,
    RecordField(String),
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RuntimeAccessQualifier {
    Frozen,
    Unique,
    LocalUnique,
    /// Canonical callable-local lifetime number. Source lifetime spelling is
    /// deliberately not part of runtime identity.
    Borrow(u32),
    /// Canonical callable-local lifetime number for an affine exclusive reference.
    BorrowMut(u32),
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RuntimeQualifierSite {
    pub path: Vec<RuntimeQualifierPathStep>,
    pub qualifiers: Vec<RuntimeAccessQualifier>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RuntimeLoanProjectionStep {
    Field(String),
    Tuple(usize),
    Index(i64),
    Range { lo: i64, hi: i64, inclusive: bool },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RuntimeLoanProjection {
    pub steps: Vec<RuntimeLoanProjectionStep>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RuntimeAccessParameterIdentity {
    pub kind: RuntimeAccessKind,
    pub qualifier_sites: Vec<RuntimeQualifierSite>,
    pub ownership_input: bool,
    pub writeback_output: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RuntimeAccessResultIdentity {
    pub qualifier_sites: Vec<RuntimeQualifierSite>,
    pub ownership_output: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RuntimeBorrowOwnerIdentity {
    pub parameter: usize,
    pub input_projection: RuntimeLoanProjection,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RuntimeBorrowRelationIdentity {
    pub lifetime: u32,
    pub output_projection: RuntimeLoanProjection,
    pub owners: Vec<RuntimeBorrowOwnerIdentity>,
    pub storage: Box<RuntimeTypeIdentity>,
    pub storage_qualifier_sites: Vec<RuntimeQualifierSite>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum RuntimeAccessAuthority {
    /// Derived from a function type without module-owned checked access facts.
    /// Nominal owner relations are conservative roots and cannot compare equal
    /// to checked projection facts.
    ConservativeType,
    /// Derived from the exact module-owned `CheckedAccessFacts` query.
    CheckedFacts,
}

/// Canonical logical access identity retained by runtime callable descriptors.
/// Its authority distinguishes exact checked relations from conservative
/// function-type relations. Physical offsets, flattened slots, and
/// ownership-token representations never enter this value.
/// Checked authority cannot be claimed with a struct literal outside
/// `witchy-types`; callers receive read-only accessors instead.
///
/// ```compile_fail
/// use witchy_types::runtime_type::RuntimeCallableAccessIdentity;
/// let _forged = RuntimeCallableAccessIdentity {
///     authority: unreachable!(),
///     callable_qualifiers: Vec::new(),
///     parameters: Vec::new(),
///     result: unreachable!(),
///     borrow_relations: Vec::new(),
/// };
/// ```
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RuntimeCallableAccessIdentity {
    authority: RuntimeAccessAuthority,
    callable_qualifiers: Vec<RuntimeAccessQualifier>,
    parameters: Vec<RuntimeAccessParameterIdentity>,
    result: RuntimeAccessResultIdentity,
    borrow_relations: Vec<RuntimeBorrowRelationIdentity>,
}

impl RuntimeCallableAccessIdentity {
    pub fn is_checked(&self) -> bool {
        self.authority == RuntimeAccessAuthority::CheckedFacts
    }

    pub fn is_conservative(&self) -> bool {
        self.authority == RuntimeAccessAuthority::ConservativeType
    }

    pub fn callable_qualifiers(&self) -> &[RuntimeAccessQualifier] {
        &self.callable_qualifiers
    }

    pub fn parameters(&self) -> &[RuntimeAccessParameterIdentity] {
        &self.parameters
    }

    pub fn result(&self) -> &RuntimeAccessResultIdentity {
        &self.result
    }

    pub fn borrow_relations(&self) -> &[RuntimeBorrowRelationIdentity] {
        &self.borrow_relations
    }
}

/// One callable parameter's canonical identity. Construction is private so a
/// catalog-free caller cannot relabel capability authority as an ordinary
/// runtime value.
///
/// ```compile_fail
/// use witchy_types::runtime_type::{PrimitiveType, RuntimeCallableParameterIdentity, RuntimeTypeIdentity};
/// let _forged = RuntimeCallableParameterIdentity::Value(
///     RuntimeTypeIdentity::Primitive(PrimitiveType::Int),
/// );
/// ```
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RuntimeCallableParameterIdentity {
    identity: RuntimeTypeIdentity,
    authority: bool,
}

impl RuntimeCallableParameterIdentity {
    pub fn identity(&self) -> &RuntimeTypeIdentity {
        &self.identity
    }

    pub fn is_authority(&self) -> bool {
        self.authority
    }

    pub(crate) fn value(identity: RuntimeTypeIdentity) -> Self {
        Self { identity, authority: false }
    }

    pub(crate) fn authority(identity: RuntimeTypeIdentity) -> Self {
        Self { identity, authority: true }
    }
}

#[derive(Default)]
struct RuntimeLifetimeNormalizer {
    by_name: BTreeMap<String, u32>,
}

impl RuntimeLifetimeNormalizer {
    fn id(&mut self, name: &str) -> Result<u32, RuntimeTypeError> {
        if let Some(id) = self.by_name.get(name) {
            return Ok(*id);
        }
        let id = u32::try_from(self.by_name.len())
            .map_err(|_| RuntimeTypeError::TooManyDescriptors)?;
        self.by_name.insert(name.to_string(), id);
        Ok(id)
    }

    fn qualifier(
        &mut self,
        qualifier: &AccessQualifier,
    ) -> Result<RuntimeAccessQualifier, RuntimeTypeError> {
        Ok(match qualifier {
            AccessQualifier::Frozen => RuntimeAccessQualifier::Frozen,
            AccessQualifier::Unique => RuntimeAccessQualifier::Unique,
            AccessQualifier::LocalUnique => RuntimeAccessQualifier::LocalUnique,
            AccessQualifier::Borrow(lifetime) => {
                RuntimeAccessQualifier::Borrow(self.id(lifetime)?)
            }
            AccessQualifier::BorrowMut(lifetime) => {
                RuntimeAccessQualifier::BorrowMut(self.id(lifetime)?)
            }
        })
    }
}

impl RuntimeCallableAccessIdentity {
    pub(crate) fn from_checked(
        signature: &AccessSignature,
        resolve: &impl Fn(&str, DeclarationKind) -> Option<DeclarationIdentity>,
    ) -> Result<Self, RuntimeTypeError> {
        Self::from_signature(signature, RuntimeAccessAuthority::CheckedFacts, resolve)
    }

    fn from_conservative_type(
        signature: &AccessSignature,
        resolve: &impl Fn(&str, DeclarationKind) -> Option<DeclarationIdentity>,
    ) -> Result<Self, RuntimeTypeError> {
        Self::from_signature(signature, RuntimeAccessAuthority::ConservativeType, resolve)
    }

    fn from_signature(
        signature: &AccessSignature,
        authority: RuntimeAccessAuthority,
        resolve: &impl Fn(&str, DeclarationKind) -> Option<DeclarationIdentity>,
    ) -> Result<Self, RuntimeTypeError> {
        let mut lifetimes = RuntimeLifetimeNormalizer::default();
        let callable_qualifiers = signature
            .callable_qualifiers()
            .iter()
            .map(|qualifier| lifetimes.qualifier(qualifier))
            .collect::<Result<_, _>>()?;
        let parameters = signature
            .params()
            .iter()
            .map(|parameter| {
                Ok(RuntimeAccessParameterIdentity {
                    kind: parameter.kind().into(),
                    qualifier_sites: qualifier_sites(parameter.ty(), &mut lifetimes)?,
                    ownership_input: parameter.ownership().input().is_some(),
                    writeback_output: parameter.ownership().writeback().is_some(),
                })
            })
            .collect::<Result<_, RuntimeTypeError>>()?;
        let result = RuntimeAccessResultIdentity {
            qualifier_sites: qualifier_sites(signature.result().ty(), &mut lifetimes)?,
            ownership_output: signature.result().ownership_output().is_some(),
        };
        let borrow_relations = signature
            .borrow_relations()
            .iter()
            .map(|relation| {
                Ok(RuntimeBorrowRelationIdentity {
                    lifetime: lifetimes.id(relation.lifetime())?,
                    output_projection: runtime_projection(relation.output_projection()),
                    owners: relation
                        .owners()
                        .iter()
                        .map(|owner| RuntimeBorrowOwnerIdentity {
                            parameter: owner.position(),
                            input_projection: runtime_projection(owner.input_projection()),
                        })
                        .collect(),
                    storage: Box::new(RuntimeTypeIdentity::from_resolved_type(
                        relation.storage_type(),
                        resolve,
                    )?),
                    storage_qualifier_sites: qualifier_sites(
                        relation.storage_type(),
                        &mut lifetimes,
                    )?,
                })
            })
            .collect::<Result<_, RuntimeTypeError>>()?;
        Ok(Self { authority, callable_qualifiers, parameters, result, borrow_relations })
    }
}

fn qualifier_sites(
    ty: &Type,
    lifetimes: &mut RuntimeLifetimeNormalizer,
) -> Result<Vec<RuntimeQualifierSite>, RuntimeTypeError> {
    fn visit(
        ty: &Type,
        path: &mut Vec<RuntimeQualifierPathStep>,
        lifetimes: &mut RuntimeLifetimeNormalizer,
        sites: &mut Vec<RuntimeQualifierSite>,
    ) -> Result<(), RuntimeTypeError> {
        let mut current = ty;
        let mut qualifiers = Vec::new();
        while let Type::Qualified(qualifier, inner) = current {
            qualifiers.push(lifetimes.qualifier(&AccessQualifier::from(qualifier))?);
            current = inner;
        }
        if !qualifiers.is_empty() {
            sites.push(RuntimeQualifierSite { path: path.clone(), qualifiers });
        }
        match current {
            Type::Named(_, arguments) => {
                for (index, argument) in arguments.iter().enumerate() {
                    path.push(RuntimeQualifierPathStep::TypeArgument(index));
                    visit(argument, path, lifetimes, sites)?;
                    path.pop();
                }
            }
            Type::Tuple(items) => {
                for (index, item) in items.iter().enumerate() {
                    path.push(RuntimeQualifierPathStep::TupleItem(index));
                    visit(item, path, lifetimes, sites)?;
                    path.pop();
                }
            }
            Type::Fn(parameters, result, _) => {
                for (index, parameter) in parameters.iter().enumerate() {
                    path.push(RuntimeQualifierPathStep::FunctionParameter(index));
                    visit(parameter, path, lifetimes, sites)?;
                    path.pop();
                }
                path.push(RuntimeQualifierPathStep::FunctionResult);
                visit(result, path, lifetimes, sites)?;
                path.pop();
            }
            Type::Dyn(_, arguments) => {
                for (index, argument) in arguments.iter().enumerate() {
                    path.push(RuntimeQualifierPathStep::ExistentialArgument(index));
                    visit(argument, path, lifetimes, sites)?;
                    path.pop();
                }
            }
            Type::RecordCompose { base, fields } => {
                path.push(RuntimeQualifierPathStep::RecordBase);
                visit(base, path, lifetimes, sites)?;
                path.pop();
                for (name, field) in fields {
                    path.push(RuntimeQualifierPathStep::RecordField(name.clone()));
                    visit(field, path, lifetimes, sites)?;
                    path.pop();
                }
            }
            Type::Qualified(_, _) => unreachable!("qualifiers peeled above"),
        }
        Ok(())
    }

    let mut sites = Vec::new();
    visit(ty, &mut Vec::new(), lifetimes, &mut sites)?;
    Ok(sites)
}

fn runtime_projection(projection: &LoanProjection) -> RuntimeLoanProjection {
    RuntimeLoanProjection {
        steps: projection
            .steps
            .iter()
            .map(|step| match step {
                LoanProjectionStep::Field(name) => {
                    RuntimeLoanProjectionStep::Field(name.clone())
                }
                LoanProjectionStep::Tuple(index) => RuntimeLoanProjectionStep::Tuple(*index),
                LoanProjectionStep::Index(index) => RuntimeLoanProjectionStep::Index(*index),
                LoanProjectionStep::Range { lo, hi, inclusive } => {
                    RuntimeLoanProjectionStep::Range {
                        lo: *lo,
                        hi: *hi,
                        inclusive: *inclusive,
                    }
                }
            })
            .collect(),
    }
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

/// Canonical semantic identity of one runtime-representable type. Compiler
/// authority variants are inspectable but cannot be constructed externally.
///
/// ```compile_fail
/// use witchy_types::runtime_type::{RuntimeCapabilityIdentity, RuntimeTypeIdentity};
/// let _forged = RuntimeTypeIdentity::Capability {
///     authority: RuntimeCapabilityIdentity::Console,
/// };
/// ```
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RuntimeTypeIdentity {
    Primitive(PrimitiveType),
    List(Box<Self>),
    Tuple(Vec<Self>),
    #[non_exhaustive]
    Function {
        params: Vec<RuntimeCallableParameterIdentity>,
        result: Box<Self>,
        conventions: Vec<RuntimeConvention>,
        access: Box<RuntimeCallableAccessIdentity>,
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
    /// Authenticated authority identity retained only beneath a callable's
    /// `Authority` parameter. Direct runtime descriptors for capabilities stay
    /// forbidden.
    #[non_exhaustive]
    Capability { authority: RuntimeCapabilityIdentity },
}

impl RuntimeTypeIdentity {
    pub(crate) fn from_checked_callable_with_authority(
        signature: &AccessSignature,
        resolve: &impl Fn(&str, DeclarationKind) -> Option<DeclarationIdentity>,
        authority_parameter: &impl Fn(&Type) -> Result<bool, RuntimeTypeError>,
    ) -> Result<Self, RuntimeTypeError> {
        let params = signature
            .params()
            .iter()
            .map(|param| {
                if authority_parameter(param.ty())? {
                    Self::from_resolved_type_inner(param.ty(), resolve, true)
                        .map(RuntimeCallableParameterIdentity::authority)
                } else {
                    Self::from_resolved_type(param.ty(), resolve)
                        .map(RuntimeCallableParameterIdentity::value)
                }
            })
            .collect::<Result<_, _>>()?;
        let result = Box::new(Self::from_resolved_type(signature.result().ty(), resolve)?);
        let conventions = signature
            .params()
            .iter()
            .map(|param| match param.kind() {
                AccessKind::OwnedImmutable => RuntimeConvention::Let,
                AccessKind::SharedBorrow => RuntimeConvention::Borrow,
                AccessKind::ExclusiveWriteback => RuntimeConvention::Var,
                AccessKind::Consuming => RuntimeConvention::Own,
            })
            .collect();
        Ok(Self::Function {
            params,
            result,
            conventions,
            access: Box::new(RuntimeCallableAccessIdentity::from_checked(
                signature,
                resolve,
            )?),
        })
    }

    /// Convert a fully resolved Witchy type into runtime identity.
    ///
    /// `resolve` is the sole nominal-identity authority. It receives the
    /// canonical compiler name plus the expected declaration kind; returning
    /// `None` is a loud error rather than a fallback to that name.
    pub fn from_resolved_type(
        ty: &Type,
        resolve: &impl Fn(&str, DeclarationKind) -> Option<DeclarationIdentity>,
    ) -> Result<Self, RuntimeTypeError> {
        Self::from_resolved_type_inner(ty, resolve, false)
    }

    fn from_resolved_type_inner(
        ty: &Type,
        resolve: &impl Fn(&str, DeclarationKind) -> Option<DeclarationIdentity>,
        allow_capability: bool,
    ) -> Result<Self, RuntimeTypeError> {
        match ty {
            Type::Qualified(_, inner) if !matches!(ty.unqualified(), Type::Fn(..)) => {
                Self::from_resolved_type_inner(inner, resolve, allow_capability)
            }
            Type::RecordCompose { .. } => Err(RuntimeTypeError::MalformedStructuralType(
                "compiler invariant violated: structural record composition reached runtime type identity before records::lower normalized it"
                    .to_string(),
            )),
            Type::Tuple(items) if items.is_empty() => Ok(Self::Primitive(PrimitiveType::Unit)),
            Type::Tuple(items) => Ok(Self::Tuple(
                items
                    .iter()
                    .map(|item| Self::from_resolved_type_inner(item, resolve, allow_capability))
                    .collect::<Result<_, _>>()?,
            )),
            Type::Fn(..) | Type::Qualified(_, _) => {
                let signature = AccessSignature::from_function_type(ty).map_err(|error| {
                    RuntimeTypeError::MalformedAccessSignature(error.to_string())
                })?;
                let Type::Fn(params, result, conventions) = ty.unqualified() else {
                    unreachable!("qualified callable guarded above")
                };
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
                        .map(|param| {
                            Self::from_resolved_type_inner(param, resolve, allow_capability)
                                .map(RuntimeCallableParameterIdentity::value)
                        })
                        .collect::<Result<_, _>>()?,
                    result: Box::new(Self::from_resolved_type_inner(
                        result,
                        resolve,
                        allow_capability,
                    )?),
                    conventions,
                    access: Box::new(RuntimeCallableAccessIdentity::from_conservative_type(
                        &signature,
                        resolve,
                    )?),
                })
            }
            Type::Dyn(name, args) => Ok(Self::Existential {
                declaration: resolve(name, DeclarationKind::Trait).ok_or_else(|| {
                    RuntimeTypeError::UnresolvedDeclaration {
                        kind: DeclarationKind::Trait,
                        name: name.clone(),
                    }
                })?,
                arguments: convert_arguments(args, resolve, allow_capability)?,
            }),
            Type::Named(name, args) => {
                if capability_type(name) {
                    if allow_capability {
                        return Ok(Self::Capability {
                            authority: capability_identity(name)
                                .expect("capability type has compiler identity"),
                        });
                    }
                    return Err(RuntimeTypeError::CapabilityType(name.clone()));
                }
                if let Some(primitive) = primitive(name, args.len()) {
                    return Ok(Self::Primitive(primitive));
                }
                if name == "List" && args.len() == 1 {
                    return Ok(Self::List(Box::new(Self::from_resolved_type_inner(
                        &args[0], resolve,
                        allow_capability,
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
                                Ok((
                                    field,
                                    Self::from_resolved_type_inner(
                                        ty,
                                        resolve,
                                        allow_capability,
                                    )?,
                                ))
                            })
                            .collect::<Result<Vec<_>, RuntimeTypeError>>()?;
                    fields.sort_by(|left, right| left.0.cmp(&right.0));
                    return Ok(Self::Record(fields));
                }
                if let Some(variants) = crate::typeck::anon_union_synthetic_variants(name) {
                    return convert_union(variants, args, resolve, allow_capability);
                }
                Ok(Self::Nominal {
                    declaration: resolve(name, DeclarationKind::Type).ok_or_else(|| {
                        RuntimeTypeError::UnresolvedDeclaration {
                            kind: DeclarationKind::Type,
                            name: name.clone(),
                        }
                    })?,
                    arguments: convert_runtime_arguments(args, resolve, allow_capability)?,
                })
            }
        }
    }
}

fn convert_arguments(
    args: &[Type],
    resolve: &impl Fn(&str, DeclarationKind) -> Option<DeclarationIdentity>,
    allow_capability: bool,
) -> Result<Vec<RuntimeTypeIdentity>, RuntimeTypeError> {
    args.iter()
        .map(|arg| {
            RuntimeTypeIdentity::from_resolved_type_inner(arg, resolve, allow_capability)
        })
        .collect()
}

fn convert_runtime_arguments(
    args: &[Type],
    resolve: &impl Fn(&str, DeclarationKind) -> Option<DeclarationIdentity>,
    allow_capability: bool,
) -> Result<Vec<RuntimeTypeIdentity>, RuntimeTypeError> {
    args.iter()
        .filter(|argument| {
            !matches!(argument, Type::Named(name, arguments) if arguments.is_empty() && name.starts_with('\''))
        })
        .map(|argument| {
            RuntimeTypeIdentity::from_resolved_type_inner(
                argument,
                resolve,
                allow_capability,
            )
        })
        .collect()
}

fn convert_union(
    variants: Vec<(String, usize)>,
    args: &[Type],
    resolve: &impl Fn(&str, DeclarationKind) -> Option<DeclarationIdentity>,
    allow_capability: bool,
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
            .map(|ty| {
                RuntimeTypeIdentity::from_resolved_type_inner(ty, resolve, allow_capability)
            })
            .collect::<Result<_, _>>()?;
        converted.push(UnionVariantIdentity { tag, payloads });
        at += arity;
    }
    converted.sort_by(|left, right| left.tag.cmp(&right.tag));
    Ok(RuntimeTypeIdentity::Union(converted))
}

pub(super) fn primitive(name: &str, arity: usize) -> Option<PrimitiveType> {
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

pub(super) fn capability_type(name: &str) -> bool {
    capability_identity(name).is_some()
}

fn capability_identity(name: &str) -> Option<RuntimeCapabilityIdentity> {
    Some(match name {
        "Console" => RuntimeCapabilityIdentity::Console,
        "Clock" => RuntimeCapabilityIdentity::Clock,
        "Rand" => RuntimeCapabilityIdentity::Rand,
        "Env" => RuntimeCapabilityIdentity::Env,
        "Secret" => RuntimeCapabilityIdentity::Secret,
        "Exec" => RuntimeCapabilityIdentity::Exec,
        "Dir" => RuntimeCapabilityIdentity::Dir,
        "File" => RuntimeCapabilityIdentity::File,
        "Net" => RuntimeCapabilityIdentity::Net,
        "Socket" => RuntimeCapabilityIdentity::Socket,
        "Listener" => RuntimeCapabilityIdentity::Listener,
        "BuildOut" => RuntimeCapabilityIdentity::BuildOut,
        "BuildRead" => RuntimeCapabilityIdentity::BuildRead,
        "BuildEnv" => RuntimeCapabilityIdentity::BuildEnv,
        "BuildNet" => RuntimeCapabilityIdentity::BuildNet,
        "BuildExec" => RuntimeCapabilityIdentity::BuildExec,
        _ => return None,
    })
}

pub(super) fn decode_anon_record(name: &str) -> Option<Vec<String>> {
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
