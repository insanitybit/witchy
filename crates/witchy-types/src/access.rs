//! Canonical ownership/access contracts for checked callable types.
//!
//! This module is the representation boundary introduced by RFC-0110 stage 1.
//! It deliberately derives contracts from checked [`Type`] and [`Convention`]
//! values. Later call-shape and lowering work consumes this representation
//! instead of rediscovering ownership from syntax, rendered names, or individual
//! container operations.

use std::fmt;

use witchy_cap_model::CapabilityKind;
use witchy_syntax::ast::{Convention, Function, Type, TypeQual};

use crate::storage::externref_cap_name;

/// The source-level access granted to one callable parameter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccessKind {
    /// A bare parameter: an owned value which is immutable in the callee.
    OwnedImmutable,
    /// An explicit `let` parameter: call-scoped shared access.
    SharedBorrow,
    /// A `var` parameter: exclusive move-in/write-back access.
    ExclusiveWriteback,
    /// An `own` parameter: consuming ownership transfer.
    Consuming,
}

impl From<Convention> for AccessKind {
    fn from(convention: Convention) -> Self {
        match convention {
            Convention::Let => Self::OwnedImmutable,
            Convention::Borrow => Self::SharedBorrow,
            Convention::Var => Self::ExclusiveWriteback,
            Convention::Own => Self::Consuming,
        }
    }
}

/// A checked qualifier in its declared outer-to-inner order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AccessQualifier {
    Frozen,
    Unique,
    LocalUnique,
    Borrow(String),
}

impl From<&TypeQual> for AccessQualifier {
    fn from(qualifier: &TypeQual) -> Self {
        match qualifier {
            TypeQual::Frozen => Self::Frozen,
            TypeQual::Unique => Self::Unique,
            TypeQual::LocalUnique => Self::LocalUnique,
            TypeQual::Borrow(lifetime) => Self::Borrow(lifetime.clone()),
        }
    }
}

/// The logical ownership state associated with a physical representation.
///
/// `LayoutDependent` is intentional: a checked nominal type alone does not say
/// whether RFC-0111 selected a boxed, packed, or destination-passed layout. The
/// layout planner must refine that case rather than this layer guessing from a
/// container name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OwnershipStateClass {
    /// RC/capacity state belonging to an owning linear-memory object.
    LinearMemoryObject,
    /// Static access/uniqueness state for a Wasm reference value.
    GcReference,
    /// An owner-root relation, never an owning-object token for the viewed data.
    BorrowedOwnerRoot { lifetime: String },
    /// State for a product, preserving the position of state-free fields.
    Aggregate(Vec<Option<OwnershipStateClass>>),
    /// A nominal or generic representation that must be refined by layout facts.
    LayoutDependent,
}

/// Ownership components entering and leaving one parameter access.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OwnershipStateFlow {
    input: Option<OwnershipStateClass>,
    writeback: Option<OwnershipStateClass>,
}

impl OwnershipStateFlow {
    pub fn input(&self) -> Option<&OwnershipStateClass> {
        self.input.as_ref()
    }

    pub fn writeback(&self) -> Option<&OwnershipStateClass> {
        self.writeback.as_ref()
    }
}

/// One parameter in a canonical access signature.
#[derive(Clone, Debug, PartialEq)]
pub struct AccessParam {
    ty: Type,
    kind: AccessKind,
    qualifiers: Vec<AccessQualifier>,
    borrow_lifetimes: Vec<String>,
    ownership: OwnershipStateFlow,
}

impl AccessParam {
    pub fn ty(&self) -> &Type {
        &self.ty
    }

    pub fn kind(&self) -> AccessKind {
        self.kind
    }

    pub fn qualifiers(&self) -> &[AccessQualifier] {
        &self.qualifiers
    }

    pub fn borrow_lifetimes(&self) -> &[String] {
        &self.borrow_lifetimes
    }

    pub fn ownership(&self) -> &OwnershipStateFlow {
        &self.ownership
    }
}

/// A borrowed result lifetime and the parameter positions which can own it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BorrowRelation {
    lifetime: String,
    owner_positions: Vec<usize>,
}

impl BorrowRelation {
    pub fn lifetime(&self) -> &str {
        &self.lifetime
    }

    pub fn owner_positions(&self) -> &[usize] {
        &self.owner_positions
    }
}

/// The result component of a canonical access signature.
#[derive(Clone, Debug, PartialEq)]
pub struct AccessResult {
    ty: Type,
    qualifiers: Vec<AccessQualifier>,
    borrow_lifetimes: Vec<String>,
    ownership_output: Option<OwnershipStateClass>,
}

impl AccessResult {
    pub fn ty(&self) -> &Type {
        &self.ty
    }

    pub fn qualifiers(&self) -> &[AccessQualifier] {
        &self.qualifiers
    }

    pub fn borrow_lifetimes(&self) -> &[String] {
        &self.borrow_lifetimes
    }

    pub fn ownership_output(&self) -> Option<&OwnershipStateClass> {
        self.ownership_output.as_ref()
    }
}

/// The exact checked ownership/access identity of one callable.
#[derive(Clone, Debug, PartialEq)]
pub struct AccessSignature {
    params: Vec<AccessParam>,
    result: AccessResult,
    borrow_relations: Vec<BorrowRelation>,
}

impl AccessSignature {
    /// Derive the signature of a checked declaration.
    ///
    /// Checked declarations with inferred parameter types still retain `None`
    /// in the AST. Callers must first obtain their finalized checked [`Type`]
    /// and use [`Self::from_function_type`] instead of inventing a type here.
    pub fn from_function(function: &Function) -> Result<Self, AccessSignatureError> {
        let params = function
            .params
            .iter()
            .enumerate()
            .map(|(position, param)| {
                param
                    .ty
                    .clone()
                    .ok_or(AccessSignatureError::MissingParameterType { position })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let result = function
            .ret
            .clone()
            .unwrap_or_else(|| Type::Named("Nil".to_string(), Vec::new()));
        let conventions = function.params.iter().map(|param| param.convention).collect();
        Self::from_parts(params, result, conventions)
    }

    /// Derive the signature carried by a checked first-class function type.
    pub fn from_function_type(ty: &Type) -> Result<Self, AccessSignatureError> {
        let Type::Fn(params, result, conventions) = ty else {
            return Err(AccessSignatureError::NotFunctionType);
        };
        let conventions = normalized_conventions(params.len(), conventions)?;
        Self::from_parts(params.clone(), result.as_ref().clone(), conventions)
    }

    /// Derive a signature from finalized checked parameter and result types.
    pub fn from_parts(
        params: Vec<Type>,
        result: Type,
        conventions: Vec<Convention>,
    ) -> Result<Self, AccessSignatureError> {
        if params.len() != conventions.len() {
            return Err(AccessSignatureError::ConventionArity {
                params: params.len(),
                conventions: conventions.len(),
            });
        }

        let mut access_params = Vec::with_capacity(params.len());
        for (position, (ty, convention)) in params.into_iter().zip(conventions).enumerate() {
            let qualifiers = leading_qualifiers(&ty);
            let kind = AccessKind::from(convention);
            if matches!(kind, AccessKind::ExclusiveWriteback | AccessKind::Consuming)
                && qualifiers.contains(&AccessQualifier::Frozen)
            {
                return Err(AccessSignatureError::FrozenMutableParameter { position });
            }
            if matches!(kind, AccessKind::ExclusiveWriteback | AccessKind::Consuming)
                && qualifiers
                    .iter()
                    .any(|qualifier| matches!(qualifier, AccessQualifier::Borrow(_)))
            {
                return Err(AccessSignatureError::MutableBorrowedView { position });
            }

            let borrow_lifetimes = borrow_lifetimes(&ty);
            let state = ownership_state_class(&ty)?;
            let requires_state = matches!(kind, AccessKind::ExclusiveWriteback | AccessKind::Consuming)
                || qualifiers.iter().any(|qualifier| {
                    matches!(
                        qualifier,
                        AccessQualifier::Unique
                            | AccessQualifier::LocalUnique
                            | AccessQualifier::Borrow(_)
                    )
                });
            let input = requires_state.then_some(state.clone()).flatten();
            let writeback = matches!(kind, AccessKind::ExclusiveWriteback)
                .then_some(state)
                .flatten();
            access_params.push(AccessParam {
                ty,
                kind,
                qualifiers,
                borrow_lifetimes,
                ownership: OwnershipStateFlow { input, writeback },
            });
        }

        let result_qualifiers = leading_qualifiers(&result);
        if result_qualifiers.contains(&AccessQualifier::LocalUnique) {
            return Err(AccessSignatureError::LocalUniqueResult);
        }
        let result_lifetimes = borrow_lifetimes(&result);
        let result_state = ownership_state_class(&result)?;
        let returns_state = result_qualifiers.contains(&AccessQualifier::Unique)
            || !result_lifetimes.is_empty();
        let ownership_output = returns_state.then_some(result_state).flatten();

        let mut borrow_relations = Vec::with_capacity(result_lifetimes.len());
        for lifetime in &result_lifetimes {
            let owner_positions = access_params
                .iter()
                .enumerate()
                .filter_map(|(position, param)| {
                    param.borrow_lifetimes.contains(lifetime).then_some(position)
                })
                .collect::<Vec<_>>();
            if owner_positions.is_empty() {
                return Err(AccessSignatureError::UnboundResultLifetime {
                    lifetime: lifetime.clone(),
                });
            }
            borrow_relations.push(BorrowRelation {
                lifetime: lifetime.clone(),
                owner_positions,
            });
        }

        Ok(Self {
            params: access_params,
            result: AccessResult {
                ty: result,
                qualifiers: result_qualifiers,
                borrow_lifetimes: result_lifetimes,
                ownership_output,
            },
            borrow_relations,
        })
    }

    pub fn params(&self) -> &[AccessParam] {
        &self.params
    }

    pub fn result(&self) -> &AccessResult {
        &self.result
    }

    pub fn borrow_relations(&self) -> &[BorrowRelation] {
        &self.borrow_relations
    }

    /// Verify that `candidate` preserves this signature without erasing or
    /// changing any access component. Lifetime names are alpha-renamable; their
    /// occurrence structure and result-to-owner positions are not.
    pub fn verify_exact(&self, candidate: &Self) -> Result<(), AccessMismatch> {
        if self.params.len() != candidate.params.len() {
            return Err(AccessMismatch::new(None, AccessMismatchKind::ParameterCount));
        }
        for (position, (required, found)) in
            self.params.iter().zip(&candidate.params).enumerate()
        {
            if required.kind != found.kind {
                return Err(AccessMismatch::new(
                    Some(SignaturePosition::Parameter(position)),
                    AccessMismatchKind::AccessKind,
                ));
            }
        }

        let mut lifetimes = LifetimeBijection::default();
        for (position, (required, found)) in
            self.params.iter().zip(&candidate.params).enumerate()
        {
            compare_type(&required.ty, &found.ty, &mut lifetimes).map_err(|kind| {
                AccessMismatch::new(Some(SignaturePosition::Parameter(position)), kind)
            })?;
        }
        compare_type(&self.result.ty, &candidate.result.ty, &mut lifetimes)
            .map_err(|kind| AccessMismatch::new(Some(SignaturePosition::Result), kind))?;

        for (position, (required, found)) in
            self.params.iter().zip(&candidate.params).enumerate()
        {
            if !state_flow_compatible(&required.ownership, &found.ownership, &lifetimes) {
                return Err(AccessMismatch::new(
                    Some(SignaturePosition::Parameter(position)),
                    AccessMismatchKind::OwnershipState,
                ));
            }
        }
        if !optional_state_compatible(
            self.result.ownership_output.as_ref(),
            candidate.result.ownership_output.as_ref(),
            &lifetimes,
        ) {
            return Err(AccessMismatch::new(
                Some(SignaturePosition::Result),
                AccessMismatchKind::OwnershipState,
            ));
        }

        let required_relations = self
            .borrow_relations
            .iter()
            .map(|relation| relation.owner_positions.as_slice())
            .collect::<Vec<_>>();
        let candidate_relations = candidate
            .borrow_relations
            .iter()
            .map(|relation| relation.owner_positions.as_slice())
            .collect::<Vec<_>>();
        if required_relations != candidate_relations {
            return Err(AccessMismatch::new(
                Some(SignaturePosition::Result),
                AccessMismatchKind::BorrowRelation,
            ));
        }
        Ok(())
    }
}

/// Classify the ownership state which a type's current representation can need.
///
/// This query is structural. It intentionally leaves nominal layouts opaque and
/// contains no `List`/`Dict`/operation-specific rules.
pub fn ownership_state_class(
    ty: &Type,
) -> Result<Option<OwnershipStateClass>, AccessSignatureError> {
    match ty {
        Type::Qualified(TypeQual::Borrow(lifetime), _) => {
            Ok(Some(OwnershipStateClass::BorrowedOwnerRoot {
                lifetime: lifetime.clone(),
            }))
        }
        Type::Qualified(_, inner) => ownership_state_class(inner),
        Type::RecordCompose { .. } => Err(AccessSignatureError::UnnormalizedRecordCompose),
        Type::Tuple(fields) => {
            let states = fields
                .iter()
                .map(ownership_state_class)
                .collect::<Result<Vec<_>, _>>()?;
            if states.iter().all(Option::is_none) {
                Ok(None)
            } else {
                Ok(Some(OwnershipStateClass::Aggregate(states)))
            }
        }
        Type::Fn(_, _, _) | Type::Dyn(_, _) => Ok(Some(OwnershipStateClass::GcReference)),
        Type::Named(name, arguments) if arguments.is_empty() => match name.as_str() {
            "Int" | "Float" | "Duration" | "Bool" | "Nil" | "()" => Ok(None),
            "String" | "Bytes" | "__Msg" => {
                Ok(Some(OwnershipStateClass::LinearMemoryObject))
            }
            _ if CapabilityKind::from_name(name).is_some() => {
                if externref_cap_name(name).is_some() {
                    Ok(Some(OwnershipStateClass::GcReference))
                } else {
                    Ok(None)
                }
            }
            _ => Ok(Some(OwnershipStateClass::LayoutDependent)),
        },
        Type::Named(_, _) => Ok(Some(OwnershipStateClass::LayoutDependent)),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AccessSignatureError {
    NotFunctionType,
    MissingParameterType { position: usize },
    ConventionArity { params: usize, conventions: usize },
    FrozenMutableParameter { position: usize },
    MutableBorrowedView { position: usize },
    LocalUniqueResult,
    UnboundResultLifetime { lifetime: String },
    UnnormalizedRecordCompose,
}

impl fmt::Display for AccessSignatureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFunctionType => write!(formatter, "expected a checked function type"),
            Self::MissingParameterType { position } => write!(
                formatter,
                "parameter {position} has no finalized checked type"
            ),
            Self::ConventionArity { params, conventions } => write!(
                formatter,
                "function type has {params} parameter(s) but {conventions} convention(s)"
            ),
            Self::FrozenMutableParameter { position } => write!(
                formatter,
                "parameter {position} is frozen but grants mutable access"
            ),
            Self::MutableBorrowedView { position } => write!(
                formatter,
                "parameter {position} is a borrowed view but grants mutable access"
            ),
            Self::LocalUniqueResult => {
                write!(formatter, "a local unique result would escape its activation")
            }
            Self::UnboundResultLifetime { lifetime } => write!(
                formatter,
                "borrowed result lifetime '{lifetime} has no owner parameter"
            ),
            Self::UnnormalizedRecordCompose => write!(
                formatter,
                "structural record composition reached access-signature derivation before normalization"
            ),
        }
    }
}

impl std::error::Error for AccessSignatureError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignaturePosition {
    Parameter(usize),
    Result,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccessMismatchKind {
    ParameterCount,
    AccessKind,
    TypeShape,
    Qualifier,
    BorrowRelation,
    OwnershipState,
    NestedConvention,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccessMismatch {
    position: Option<SignaturePosition>,
    kind: AccessMismatchKind,
}

impl AccessMismatch {
    fn new(position: Option<SignaturePosition>, kind: AccessMismatchKind) -> Self {
        Self { position, kind }
    }

    pub fn position(&self) -> Option<SignaturePosition> {
        self.position
    }

    pub fn kind(&self) -> AccessMismatchKind {
        self.kind
    }
}

impl fmt::Display for AccessMismatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let position = match self.position {
            Some(SignaturePosition::Parameter(index)) => format!("parameter {index}"),
            Some(SignaturePosition::Result) => "result".to_string(),
            None => "signature".to_string(),
        };
        write!(formatter, "{position} does not preserve {:?}", self.kind)
    }
}

impl std::error::Error for AccessMismatch {}

fn normalized_conventions(
    params: usize,
    conventions: &[Convention],
) -> Result<Vec<Convention>, AccessSignatureError> {
    if conventions.is_empty() {
        return Ok(vec![Convention::Let; params]);
    }
    if params != conventions.len() {
        return Err(AccessSignatureError::ConventionArity {
            params,
            conventions: conventions.len(),
        });
    }
    Ok(conventions.to_vec())
}

fn leading_qualifiers(ty: &Type) -> Vec<AccessQualifier> {
    let mut qualifiers = Vec::new();
    let mut current = ty;
    while let Type::Qualified(qualifier, inner) = current {
        qualifiers.push(AccessQualifier::from(qualifier));
        current = inner;
    }
    qualifiers
}

fn borrow_lifetimes(ty: &Type) -> Vec<String> {
    fn collect(ty: &Type, found: &mut Vec<String>) {
        match ty {
            Type::Qualified(TypeQual::Borrow(lifetime), inner) => {
                if !found.contains(lifetime) {
                    found.push(lifetime.clone());
                }
                collect(inner, found);
            }
            Type::Qualified(_, inner) => collect(inner, found),
            Type::Named(_, arguments) | Type::Dyn(_, arguments) | Type::Tuple(arguments) => {
                for argument in arguments {
                    collect(argument, found);
                }
            }
            Type::RecordCompose { base, fields } => {
                collect(base, found);
                for (_, field) in fields {
                    collect(field, found);
                }
            }
            // A nested function type introduces its own lifetime-relation scope.
            // Those relations are checked recursively by `compare_type`; they do
            // not name owners in the enclosing callable signature.
            Type::Fn(_, _, _) => {}
        }
    }

    let mut found = Vec::new();
    collect(ty, &mut found);
    found
}

#[derive(Default)]
struct LifetimeBijection {
    forward: Vec<(String, String)>,
}

impl LifetimeBijection {
    fn relate(&mut self, required: &str, candidate: &str) -> bool {
        if let Some((_, mapped)) = self.forward.iter().find(|(from, _)| from == required) {
            return mapped == candidate;
        }
        if self.forward.iter().any(|(_, to)| to == candidate) {
            return false;
        }
        self.forward.push((required.to_string(), candidate.to_string()));
        true
    }

    fn matches(&self, required: &str, candidate: &str) -> bool {
        self.forward
            .iter()
            .find(|(from, _)| from == required)
            .is_some_and(|(_, mapped)| mapped == candidate)
    }
}

fn compare_type(
    required: &Type,
    candidate: &Type,
    lifetimes: &mut LifetimeBijection,
) -> Result<(), AccessMismatchKind> {
    match (required, candidate) {
        (
            Type::Qualified(TypeQual::Borrow(left), left_inner),
            Type::Qualified(TypeQual::Borrow(right), right_inner),
        ) => {
            if !lifetimes.relate(left, right) {
                return Err(AccessMismatchKind::BorrowRelation);
            }
            compare_type(left_inner, right_inner, lifetimes)
        }
        (Type::Qualified(left, left_inner), Type::Qualified(right, right_inner)) => {
            if left != right {
                return Err(AccessMismatchKind::Qualifier);
            }
            compare_type(left_inner, right_inner, lifetimes)
        }
        (Type::Qualified(_, _), _) | (_, Type::Qualified(_, _)) => {
            Err(AccessMismatchKind::Qualifier)
        }
        (Type::Named(left_name, left_args), Type::Named(right_name, right_args))
        | (Type::Dyn(left_name, left_args), Type::Dyn(right_name, right_args)) => {
            if left_name != right_name || left_args.len() != right_args.len() {
                return Err(AccessMismatchKind::TypeShape);
            }
            compare_type_lists(left_args, right_args, lifetimes)
        }
        (Type::Tuple(left), Type::Tuple(right)) => compare_type_lists(left, right, lifetimes),
        (
            Type::RecordCompose { base: left_base, fields: left_fields },
            Type::RecordCompose { base: right_base, fields: right_fields },
        ) => {
            if left_fields.len() != right_fields.len()
                || left_fields
                    .iter()
                    .zip(right_fields)
                    .any(|((left, _), (right, _))| left != right)
            {
                return Err(AccessMismatchKind::TypeShape);
            }
            compare_type(left_base, right_base, lifetimes)?;
            for ((_, left), (_, right)) in left_fields.iter().zip(right_fields) {
                compare_type(left, right, lifetimes)?;
            }
            Ok(())
        }
        (
            Type::Fn(left_params, left_result, left_conventions),
            Type::Fn(right_params, right_result, right_conventions),
        ) => {
            if left_params.len() != right_params.len() {
                return Err(AccessMismatchKind::TypeShape);
            }
            let left_conventions = normalized_conventions(left_params.len(), left_conventions)
                .map_err(|_| AccessMismatchKind::NestedConvention)?;
            let right_conventions = normalized_conventions(right_params.len(), right_conventions)
                .map_err(|_| AccessMismatchKind::NestedConvention)?;
            if left_conventions != right_conventions {
                return Err(AccessMismatchKind::NestedConvention);
            }
            let mut nested_lifetimes = LifetimeBijection::default();
            compare_type_lists(left_params, right_params, &mut nested_lifetimes)?;
            compare_type(left_result, right_result, &mut nested_lifetimes)
        }
        _ => Err(AccessMismatchKind::TypeShape),
    }
}

fn compare_type_lists(
    required: &[Type],
    candidate: &[Type],
    lifetimes: &mut LifetimeBijection,
) -> Result<(), AccessMismatchKind> {
    if required.len() != candidate.len() {
        return Err(AccessMismatchKind::TypeShape);
    }
    for (required, candidate) in required.iter().zip(candidate) {
        compare_type(required, candidate, lifetimes)?;
    }
    Ok(())
}

fn state_flow_compatible(
    required: &OwnershipStateFlow,
    candidate: &OwnershipStateFlow,
    lifetimes: &LifetimeBijection,
) -> bool {
    optional_state_compatible(required.input.as_ref(), candidate.input.as_ref(), lifetimes)
        && optional_state_compatible(
            required.writeback.as_ref(),
            candidate.writeback.as_ref(),
            lifetimes,
        )
}

fn optional_state_compatible(
    required: Option<&OwnershipStateClass>,
    candidate: Option<&OwnershipStateClass>,
    lifetimes: &LifetimeBijection,
) -> bool {
    match (required, candidate) {
        (None, None) => true,
        (Some(required), Some(candidate)) => {
            state_compatible(required, candidate, lifetimes)
        }
        _ => false,
    }
}

fn state_compatible(
    required: &OwnershipStateClass,
    candidate: &OwnershipStateClass,
    lifetimes: &LifetimeBijection,
) -> bool {
    match (required, candidate) {
        (
            OwnershipStateClass::BorrowedOwnerRoot { lifetime: required },
            OwnershipStateClass::BorrowedOwnerRoot { lifetime: candidate },
        ) => lifetimes.matches(required, candidate),
        (OwnershipStateClass::Aggregate(required), OwnershipStateClass::Aggregate(candidate)) => {
            required.len() == candidate.len()
                && required.iter().zip(candidate).all(|(required, candidate)| {
                    optional_state_compatible(
                        required.as_ref(),
                        candidate.as_ref(),
                        lifetimes,
                    )
                })
        }
        _ => required == candidate,
    }
}
