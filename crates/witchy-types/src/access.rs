//! Canonical ownership/access contracts for checked callable types.
//!
//! This module is the representation boundary introduced by RFC-0110 stage 1.
//! It deliberately derives contracts from checked [`Type`] and [`Convention`]
//! values. Later call-shape and lowering work consumes this representation
//! instead of rediscovering ownership from syntax, rendered names, or individual
//! container operations.

use std::collections::HashMap;
use std::fmt;

use witchy_cap_model::CapabilityKind;
use witchy_syntax::ast::{
    Block, Convention, Expr, Function, Item, MatchArm, Module, Pattern, Stmt, Type, TypeQual,
};

use crate::storage::externref_cap_name;
use crate::typeck::{TypeTable, ty_to_ast};

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
    /// A tuple, nominal, or generic representation that must be refined by
    /// layout facts. Child requirements remain positional even though this
    /// layer does not guess the outer physical layout.
    LayoutDependent { children: Vec<Option<OwnershipStateClass>> },
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
    callable_qualifiers: Vec<AccessQualifier>,
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
            .ok_or(AccessSignatureError::MissingResultType)?;
        let conventions = function.params.iter().map(|param| param.convention).collect();
        Self::from_parts(params, result, conventions)
    }

    /// Derive the signature carried by a checked first-class function type.
    pub fn from_function_type(ty: &Type) -> Result<Self, AccessSignatureError> {
        let callable_qualifiers = leading_qualifiers(ty);
        let Type::Fn(params, result, conventions) = ty.unqualified() else {
            return Err(AccessSignatureError::NotFunctionType);
        };
        let conventions = normalized_conventions(params.len(), conventions)?;
        let mut signature =
            Self::from_parts(params.clone(), result.as_ref().clone(), conventions)?;
        signature.callable_qualifiers = callable_qualifiers;
        Ok(signature)
    }

    /// Derive a declaration's access contract using its finalized checked
    /// function type. Source qualifiers and lifetime names remain authoritative;
    /// inferred holes and generic leaves take their concrete checked shape from
    /// `resolved`.
    pub fn from_resolved_function(
        function: &Function,
        resolved: &Type,
    ) -> Result<Self, AccessSignatureError> {
        let params = function.params.iter().map(|param| param.ty.as_ref()).collect();
        let conventions = function.params.iter().map(|param| param.convention).collect();
        Self::from_resolved_parts(params, function.ret.as_ref(), conventions, resolved)
    }

    /// Derive a lambda or generated callable contract from finalized checked
    /// parameter/result types without fabricating placeholder type names.
    pub fn from_resolved_parts(
        declared_params: Vec<Option<&Type>>,
        declared_result: Option<&Type>,
        conventions: Vec<Convention>,
        resolved: &Type,
    ) -> Result<Self, AccessSignatureError> {
        let Type::Fn(resolved_params, resolved_result, resolved_conventions) =
            resolved.unqualified()
        else {
            return Err(AccessSignatureError::NotFunctionType);
        };
        if declared_params.len() != resolved_params.len() {
            return Err(AccessSignatureError::ConventionArity {
                params: resolved_params.len(),
                conventions: declared_params.len(),
            });
        }
        let conventions = if conventions.is_empty() {
            normalized_conventions(resolved_params.len(), resolved_conventions)?
        } else {
            normalized_conventions(resolved_params.len(), &conventions)?
        };
        let params = declared_params
            .into_iter()
            .zip(resolved_params)
            .map(|(declared, resolved)| {
                declared.map_or_else(|| resolved.clone(), |declared| {
                    apply_declared_contract(declared, resolved)
                })
            })
            .collect();
        let result = declared_result.map_or_else(
            || resolved_result.as_ref().clone(),
            |declared| apply_declared_contract(declared, resolved_result),
        );
        Self::from_parts(params, result, conventions)
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
                || type_has_ownership_qualifier(&ty);
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
        if type_has_qualifier(&result, |qualifier| {
            matches!(qualifier, TypeQual::LocalUnique)
        }) {
            return Err(AccessSignatureError::LocalUniqueResult);
        }
        let result_lifetimes = borrow_lifetimes(&result);
        let result_state = ownership_state_class(&result)?;
        let returns_state = type_has_ownership_qualifier(&result);
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
            callable_qualifiers: Vec::new(),
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

    pub fn callable_qualifiers(&self) -> &[AccessQualifier] {
        &self.callable_qualifiers
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
        compare_qualifiers(
            &self.callable_qualifiers,
            &candidate.callable_qualifiers,
            &mut lifetimes,
        )
        .map_err(|kind| AccessMismatch::new(None, kind))?;
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
        Type::Tuple(fields) if fields.is_empty() => Ok(None),
        Type::Tuple(fields) => {
            let states = fields
                .iter()
                .map(ownership_state_class)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Some(OwnershipStateClass::LayoutDependent { children: states }))
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
            _ => Ok(Some(OwnershipStateClass::LayoutDependent {
                children: Vec::new(),
            })),
        },
        Type::Named(_, arguments) => Ok(Some(OwnershipStateClass::LayoutDependent {
            children: arguments
                .iter()
                .map(ownership_state_class)
                .collect::<Result<Vec<_>, _>>()?,
        })),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AccessSignatureError {
    NotFunctionType,
    MissingParameterType { position: usize },
    MissingResultType,
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
            Self::MissingResultType => {
                write!(formatter, "function has no finalized checked result type")
            }
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

fn apply_declared_contract(declared: &Type, resolved: &Type) -> Type {
    match declared {
        Type::Qualified(qualifier, inner) => Type::Qualified(
            qualifier.clone(),
            Box::new(apply_declared_contract(inner, resolved)),
        ),
        Type::Named(name, arguments)
            if arguments.is_empty()
                && !name.contains('.')
                && name.chars().next().is_some_and(char::is_lowercase) =>
        {
            resolved.clone()
        }
        Type::Named(_, declared_arguments) => {
            let Type::Named(resolved_name, resolved_arguments) = resolved.unqualified() else {
                return resolved.clone();
            };
            if declared_arguments.len() != resolved_arguments.len() {
                return resolved.clone();
            }
            Type::Named(
                resolved_name.clone(),
                declared_arguments
                    .iter()
                    .zip(resolved_arguments)
                    .map(|(declared, resolved)| apply_declared_contract(declared, resolved))
                    .collect(),
            )
        }
        Type::Dyn(_, declared_arguments) => {
            let Type::Dyn(resolved_name, resolved_arguments) = resolved.unqualified() else {
                return resolved.clone();
            };
            if declared_arguments.len() != resolved_arguments.len() {
                return resolved.clone();
            }
            Type::Dyn(
                resolved_name.clone(),
                declared_arguments
                    .iter()
                    .zip(resolved_arguments)
                    .map(|(declared, resolved)| apply_declared_contract(declared, resolved))
                    .collect(),
            )
        }
        Type::Tuple(declared_fields) => {
            let Type::Tuple(resolved_fields) = resolved.unqualified() else {
                return resolved.clone();
            };
            if declared_fields.len() != resolved_fields.len() {
                return resolved.clone();
            }
            Type::Tuple(
                declared_fields
                    .iter()
                    .zip(resolved_fields)
                    .map(|(declared, resolved)| apply_declared_contract(declared, resolved))
                    .collect(),
            )
        }
        Type::Fn(declared_params, declared_result, declared_conventions) => {
            let Type::Fn(resolved_params, resolved_result, resolved_conventions) =
                resolved.unqualified()
            else {
                return resolved.clone();
            };
            if declared_params.len() != resolved_params.len() {
                return resolved.clone();
            }
            let conventions = if declared_conventions.is_empty() {
                resolved_conventions.clone()
            } else {
                declared_conventions.clone()
            };
            Type::Fn(
                declared_params
                    .iter()
                    .zip(resolved_params)
                    .map(|(declared, resolved)| apply_declared_contract(declared, resolved))
                    .collect(),
                Box::new(apply_declared_contract(declared_result, resolved_result)),
                conventions,
            )
        }
        Type::RecordCompose { .. } => resolved.clone(),
    }
}

fn type_has_qualifier(ty: &Type, predicate: impl Copy + Fn(&TypeQual) -> bool) -> bool {
    match ty {
        Type::Qualified(qualifier, inner) => {
            predicate(qualifier) || type_has_qualifier(inner, predicate)
        }
        Type::Named(_, arguments) | Type::Dyn(_, arguments) | Type::Tuple(arguments) => {
            arguments
                .iter()
                .any(|argument| type_has_qualifier(argument, predicate))
        }
        Type::RecordCompose { base, fields } => {
            type_has_qualifier(base, predicate)
                || fields
                    .iter()
                    .any(|(_, field)| type_has_qualifier(field, predicate))
        }
        // A nested callable owns its activation-local result contract. Returning
        // the callable does not return a value produced by invoking it.
        Type::Fn(_, _, _) => false,
    }
}

fn type_has_ownership_qualifier(ty: &Type) -> bool {
    match ty {
        Type::Qualified(
            TypeQual::Unique | TypeQual::LocalUnique | TypeQual::Borrow(_),
            _,
        ) => true,
        Type::Qualified(TypeQual::Frozen, inner) => type_has_ownership_qualifier(inner),
        Type::Named(_, arguments) | Type::Dyn(_, arguments) | Type::Tuple(arguments) => {
            arguments.iter().any(type_has_ownership_qualifier)
        }
        Type::RecordCompose { base, fields } => {
            type_has_ownership_qualifier(base)
                || fields
                    .iter()
                    .any(|(_, field)| type_has_ownership_qualifier(field))
        }
        // A nested callable's parameter/result ownership belongs to that
        // callable's own access signature, not to the function value which
        // contains it.
        Type::Fn(_, _, _) => false,
    }
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

fn compare_qualifiers(
    required: &[AccessQualifier],
    candidate: &[AccessQualifier],
    lifetimes: &mut LifetimeBijection,
) -> Result<(), AccessMismatchKind> {
    if required.len() != candidate.len() {
        return Err(AccessMismatchKind::Qualifier);
    }
    for (required, candidate) in required.iter().zip(candidate) {
        match (required, candidate) {
            (AccessQualifier::Borrow(required), AccessQualifier::Borrow(candidate)) => {
                if !lifetimes.relate(required, candidate) {
                    return Err(AccessMismatchKind::BorrowRelation);
                }
            }
            _ if required == candidate => {}
            _ => return Err(AccessMismatchKind::Qualifier),
        }
    }
    Ok(())
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
        (
            OwnershipStateClass::LayoutDependent { children: required },
            OwnershipStateClass::LayoutDependent { children: candidate },
        ) => {
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

#[derive(Clone, Debug)]
enum AccessFlow {
    None,
    Callable(AccessSignature),
    Product(Vec<AccessFlow>),
}

impl AccessFlow {
    fn product(children: Vec<Self>) -> Self {
        if children.iter().all(|child| matches!(child, Self::None)) {
            Self::None
        } else {
            Self::Product(children)
        }
    }

    fn from_type(ty: &Type) -> Result<Self, AccessSignatureError> {
        if matches!(ty.unqualified(), Type::Fn(_, _, _)) {
            return AccessSignature::from_function_type(ty).map(Self::Callable);
        }
        let children = match ty.unqualified() {
            Type::Tuple(children) | Type::Named(_, children) | Type::Dyn(_, children) => children,
            Type::Qualified(_, _) => unreachable!("unqualified removes every qualifier"),
            Type::RecordCompose { .. } | Type::Fn(_, _, _) => return Ok(Self::None),
        };
        let children = children
            .iter()
            .map(Self::from_type)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self::product(children))
    }

    fn verify_exact(&self, expected: &Self, context: &str) -> Result<(), AccessFlowError> {
        match (self, expected) {
            (_, Self::None) => Ok(()),
            (Self::Callable(actual), Self::Callable(expected)) => actual
                .verify_exact(expected)
                .map_err(|mismatch| AccessFlowError::mismatch(context, mismatch)),
            (Self::Product(actual), Self::Product(expected))
                if actual.len() == expected.len() =>
            {
                for (actual, expected) in actual.iter().zip(expected) {
                    actual.verify_exact(expected, context)?;
                }
                Ok(())
            }
            _ => Err(AccessFlowError::missing(context)),
        }
    }

    fn join(&self, other: &Self, context: &str) -> Result<Self, AccessFlowError> {
        self.verify_exact(other, context)?;
        other.verify_exact(self, context)?;
        Ok(self.clone())
    }

    fn project(&self, index: usize) -> Self {
        match self {
            Self::Product(children) => children.get(index).cloned().unwrap_or(Self::None),
            _ => Self::None,
        }
    }
}

/// Finalized access-signature facts keyed to the exact typed AST used to build
/// them. The owning [`crate::typeck::TypedModule`] must remain alive while these
/// address-keyed queries are used.
#[derive(Default)]
pub struct CheckedAccessFacts {
    values: HashMap<usize, AccessFlow>,
}

impl CheckedAccessFacts {
    pub fn callable_at(&self, expression: &Expr) -> Option<&AccessSignature> {
        match self.values.get(&(expression as *const Expr as usize)) {
            Some(AccessFlow::Callable(signature)) => Some(signature),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct AccessFlowError {
    message: String,
}

impl AccessFlowError {
    fn mismatch(context: &str, mismatch: AccessMismatch) -> Self {
        Self {
            message: format!(
                "{context} erases or changes its ownership/access contract ({mismatch})"
            ),
        }
    }

    fn missing(context: &str) -> Self {
        Self {
            message: format!(
                "{context} loses a checked callable ownership/access contract"
            ),
        }
    }
}

impl fmt::Display for AccessFlowError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for AccessFlowError {}

type FlowEnvironment = HashMap<String, AccessFlow>;

struct AccessVerifier<'a> {
    module: &'a Module,
    table: &'a TypeTable,
    functions: HashMap<String, Function>,
    record_fields: HashMap<(String, String), usize>,
    facts: CheckedAccessFacts,
}

impl<'a> AccessVerifier<'a> {
    fn new(module: &'a Module, table: &'a TypeTable) -> Self {
        let functions = module
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Function(function) => Some((function.name.clone(), function.clone())),
                _ => None,
            })
            .collect();
        let mut record_fields = HashMap::new();
        for item in &module.items {
            let Item::Type(definition) = item else { continue };
            for variant in &definition.variants {
                for (index, field) in variant.field_names.iter().enumerate() {
                    record_fields.insert((definition.name.clone(), field.clone()), index);
                }
            }
        }
        Self { module, table, functions, record_fields, facts: CheckedAccessFacts::default() }
    }

    fn verify(mut self) -> Result<CheckedAccessFacts, AccessFlowError> {
        for item in &self.module.items {
            let Item::Function(function) = item else { continue };
            self.verify_function(function)?;
        }
        Ok(self.facts)
    }

    fn resolved_expression_type(&self, expression: &Expr) -> Option<Type> {
        self.table.type_of(expression).and_then(ty_to_ast)
    }

    fn declaration_signature(
        &self,
        function: &Function,
    ) -> Result<Option<AccessSignature>, AccessFlowError> {
        let resolved = self.table.function_type(&function.name);
        let signature = match resolved {
            Some(resolved) => AccessSignature::from_resolved_function(function, &resolved),
            None => AccessSignature::from_function(function),
        };
        match signature {
            Ok(signature) => Ok(Some(signature)),
            Err(
                AccessSignatureError::MissingParameterType { .. }
                | AccessSignatureError::MissingResultType,
            ) => Ok(None),
            Err(error) => Err(AccessFlowError {
                message: format!("access signature for `{}` is invalid: {error}", function.name),
            }),
        }
    }

    fn verify_function(&mut self, function: &Function) -> Result<(), AccessFlowError> {
        let Some(signature) = self.declaration_signature(function)? else {
            return Ok(());
        };
        let mut environment = HashMap::new();
        for (parameter, access) in function.params.iter().zip(signature.params()) {
            environment.insert(
                parameter.name.clone(),
                AccessFlow::from_type(access.ty()).map_err(Self::signature_error)?,
            );
        }
        let expected = AccessFlow::from_type(signature.result().ty())
            .map_err(Self::signature_error)?;
        let tail = self.eval_block(&function.body, &mut environment, &expected)?;
        if matches!(function.body.stmts.last(), Some(Stmt::Expr(_))) {
            tail.verify_exact(
                &expected,
                &format!("returned value from `{}`", function.name),
            )?;
        }
        Ok(())
    }

    fn signature_error(error: AccessSignatureError) -> AccessFlowError {
        AccessFlowError { message: error.to_string() }
    }

    fn signature_for_named_call(
        &self,
        name: &str,
        expression: &Expr,
        args: &[&Expr],
    ) -> Result<Option<AccessSignature>, AccessFlowError> {
        let Some(function) = self.functions.get(name) else { return Ok(None) };
        if function.params.len() != args.len() {
            return Ok(None);
        }
        let Some(result) = self.resolved_expression_type(expression) else {
            return self.declaration_signature(function);
        };
        let params = args
            .iter()
            .map(|argument| self.resolved_expression_type(argument))
            .collect::<Option<Vec<_>>>();
        let Some(params) = params else {
            return self.declaration_signature(function);
        };
        let conventions = function.params.iter().map(|parameter| parameter.convention).collect();
        let resolved = Type::Fn(params, Box::new(result), conventions);
        AccessSignature::from_resolved_function(function, &resolved)
            .map(Some)
            .map_err(Self::signature_error)
    }

    fn signature_for_function_value(
        &self,
        name: &str,
        expression: &Expr,
    ) -> Result<Option<AccessSignature>, AccessFlowError> {
        let Some(function) = self.functions.get(name) else { return Ok(None) };
        let Some(resolved) = self.resolved_expression_type(expression) else {
            return self.declaration_signature(function);
        };
        let Type::Fn(params, _, _) = resolved.unqualified() else { return Ok(None) };
        if params.len() != function.params.len() {
            return Ok(None);
        }
        AccessSignature::from_resolved_function(function, &resolved)
            .map(Some)
            .map_err(Self::signature_error)
    }

    fn verify_arguments(
        &self,
        name: &str,
        signature: &AccessSignature,
        arguments: &[AccessFlow],
    ) -> Result<(), AccessFlowError> {
        for (index, (argument, parameter)) in
            arguments.iter().zip(signature.params()).enumerate()
        {
            let expected = AccessFlow::from_type(parameter.ty()).map_err(Self::signature_error)?;
            argument.verify_exact(
                &expected,
                &format!("argument {} passed to `{name}`", index + 1),
            )?;
        }
        Ok(())
    }

    fn eval_block(
        &mut self,
        block: &Block,
        environment: &mut FlowEnvironment,
        return_expected: &AccessFlow,
    ) -> Result<AccessFlow, AccessFlowError> {
        let mut tail = AccessFlow::None;
        for (index, statement) in block.stmts.iter().enumerate() {
            tail = match statement {
                Stmt::Let { name, ty, value, .. } => {
                    let actual = self.eval_expr(value, environment, return_expected)?;
                    let value = if let Some(ty) = ty {
                        let expected = AccessFlow::from_type(ty).map_err(Self::signature_error)?;
                        actual.verify_exact(&expected, &format!("function value `{name}`"))?;
                        expected
                    } else {
                        actual
                    };
                    environment.insert(name.clone(), value);
                    AccessFlow::None
                }
                Stmt::Assign { name, value } => {
                    let actual = self.eval_expr(value, environment, return_expected)?;
                    if let Some(expected) = environment.get(name) {
                        actual.verify_exact(expected, &format!("assignment to `{name}`"))?;
                    }
                    environment.insert(name.clone(), actual);
                    AccessFlow::None
                }
                Stmt::LetPattern { pattern, value } => {
                    let value = self.eval_expr(value, environment, return_expected)?;
                    Self::bind_pattern(pattern, &value, environment);
                    AccessFlow::None
                }
                Stmt::Return(value) => {
                    let actual = match value {
                        Some(value) => self.eval_expr(value, environment, return_expected)?,
                        None => AccessFlow::None,
                    };
                    actual.verify_exact(return_expected, "returned function value")?;
                    AccessFlow::None
                }
                Stmt::Yield(value) | Stmt::Expr(value) => {
                    self.eval_expr(value, environment, return_expected)?
                }
                Stmt::Break | Stmt::Continue => AccessFlow::None,
            };
            if index + 1 != block.stmts.len() {
                tail = AccessFlow::None;
            }
        }
        Ok(tail)
    }

    fn bind_pattern(pattern: &Pattern, value: &AccessFlow, environment: &mut FlowEnvironment) {
        match pattern {
            Pattern::Var(name) if name != "_" => {
                environment.insert(name.clone(), value.clone());
            }
            Pattern::Ctor { args, .. }
            | Pattern::AnonCtor { args, .. }
            | Pattern::Tuple(args) => {
                for (index, pattern) in args.iter().enumerate() {
                    Self::bind_pattern(pattern, &value.project(index), environment);
                }
            }
            Pattern::List { elems, rest } => {
                for (index, pattern) in elems.iter().enumerate() {
                    Self::bind_pattern(pattern, &value.project(index), environment);
                }
                if let Some(Some(name)) = rest {
                    environment.insert(name.clone(), AccessFlow::None);
                }
            }
            Pattern::Or(alternatives) => {
                if let Some(first) = alternatives.first() {
                    Self::bind_pattern(first, value, environment);
                }
            }
            Pattern::Wildcard
            | Pattern::Var(_)
            | Pattern::Int(_)
            | Pattern::Str(_)
            | Pattern::Bool(_)
            | Pattern::Duration(_)
            | Pattern::IntRange { .. } => {}
        }
    }

    fn eval_arm(
        &mut self,
        arm: &MatchArm,
        scrutinee: &AccessFlow,
        environment: &FlowEnvironment,
        return_expected: &AccessFlow,
    ) -> Result<(AccessFlow, FlowEnvironment), AccessFlowError> {
        let mut environment = environment.clone();
        Self::bind_pattern(&arm.pattern, scrutinee, &mut environment);
        if let Some(guard) = &arm.guard {
            self.eval_expr(guard, &mut environment, return_expected)?;
        }
        let flow = self.eval_expr(&arm.body, &mut environment, return_expected)?;
        Ok((flow, environment))
    }

    fn join_flows(
        flows: impl IntoIterator<Item = AccessFlow>,
        context: &str,
    ) -> Result<AccessFlow, AccessFlowError> {
        let mut flows = flows.into_iter();
        let Some(first) = flows.next() else { return Ok(AccessFlow::None) };
        flows.try_fold(first, |left, right| left.join(&right, context))
    }

    fn merge_environments(
        target: &mut FlowEnvironment,
        branches: &[FlowEnvironment],
        context: &str,
    ) -> Result<(), AccessFlowError> {
        let names = target.keys().cloned().collect::<Vec<_>>();
        for name in names {
            let mut values = branches
                .iter()
                .filter_map(|environment| environment.get(&name).cloned());
            let Some(first) = values.next() else { continue };
            let value = values.try_fold(first, |left, right| {
                left.join(&right, &format!("{context} binding `{name}`"))
            })?;
            target.insert(name, value);
        }
        Ok(())
    }

    fn record(&mut self, expression: &Expr, flow: AccessFlow) -> AccessFlow {
        self.facts.values.insert(expression as *const Expr as usize, flow.clone());
        flow
    }

    fn eval_expr(
        &mut self,
        expression: &Expr,
        environment: &mut FlowEnvironment,
        return_expected: &AccessFlow,
    ) -> Result<AccessFlow, AccessFlowError> {
        let flow = match expression {
            Expr::Int(_) | Expr::Float(_) | Expr::Duration(_) | Expr::Str(_)
            | Expr::Bool(_) | Expr::TaggedLit { .. } => AccessFlow::None,
            Expr::Var(name) => {
                if let Some(flow) = environment.get(name) {
                    flow.clone()
                } else {
                    self.signature_for_function_value(name, expression)?
                        .map(AccessFlow::Callable)
                        .unwrap_or(AccessFlow::None)
                }
            }
            Expr::List(values) | Expr::Tuple(values) => AccessFlow::product(
                values
                    .iter()
                    .map(|value| self.eval_expr(value, environment, return_expected))
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            Expr::Call { name, args } => {
                let arguments = args
                    .iter()
                    .map(|argument| self.eval_expr(argument, environment, return_expected))
                    .collect::<Result<Vec<_>, _>>()?;
                let signature = match environment.get(name) {
                    Some(AccessFlow::Callable(signature)) => Some(signature.clone()),
                    _ => {
                        let positional = args.iter().collect::<Vec<_>>();
                        self.signature_for_named_call(name, expression, &positional)?
                    }
                };
                match signature {
                    Some(signature) => {
                        self.verify_arguments(name, &signature, &arguments)?;
                        AccessFlow::from_type(signature.result().ty())
                            .map_err(Self::signature_error)?
                    }
                    None => AccessFlow::None,
                }
            }
            Expr::LabeledCall { name, args } => {
                let positional = args.iter().map(|(_, argument)| argument).collect::<Vec<_>>();
                let arguments = positional
                    .iter()
                    .map(|argument| self.eval_expr(argument, environment, return_expected))
                    .collect::<Result<Vec<_>, _>>()?;
                match self.signature_for_named_call(name, expression, &positional)? {
                    Some(signature) => {
                        self.verify_arguments(name, &signature, &arguments)?;
                        AccessFlow::from_type(signature.result().ty())
                            .map_err(Self::signature_error)?
                    }
                    None => AccessFlow::None,
                }
            }
            Expr::Apply { func, args } => {
                let function = self.eval_expr(func, environment, return_expected)?;
                let arguments = args
                    .iter()
                    .map(|argument| self.eval_expr(argument, environment, return_expected))
                    .collect::<Result<Vec<_>, _>>()?;
                let AccessFlow::Callable(signature) = function else {
                    return Ok(self.record(expression, AccessFlow::None));
                };
                self.verify_arguments("indirect function", &signature, &arguments)?;
                AccessFlow::from_type(signature.result().ty()).map_err(Self::signature_error)?
            }
            Expr::Ctor { args, .. } | Expr::AnonCtor { args, .. } => AccessFlow::product(
                args.iter()
                    .map(|argument| self.eval_expr(argument, environment, return_expected))
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            Expr::Unary { expr, .. }
            | Expr::Try(expr)
            | Expr::ExistentialPack { expr, .. }
            | Expr::ExistentialUpcast { expr, .. } => {
                self.eval_expr(expr, environment, return_expected)?
            }
            Expr::Field { base, field } => {
                let base_flow = self.eval_expr(base, environment, return_expected)?;
                let index = self
                    .resolved_expression_type(base)
                    .and_then(|ty| match ty.unqualified() {
                        Type::Named(name, _) => self
                            .record_fields
                            .get(&(name.clone(), field.clone()))
                            .copied(),
                        _ => None,
                    });
                index.map_or(AccessFlow::None, |index| base_flow.project(index))
            }
            Expr::Lambda { params, body, ret } => {
                let resolved = self.resolved_expression_type(expression).ok_or_else(|| {
                    AccessFlowError { message: "lambda has no finalized checked type".into() }
                })?;
                let conventions = params.iter().map(|parameter| parameter.convention).collect();
                let signature = AccessSignature::from_resolved_parts(
                    params.iter().map(|parameter| parameter.ty.as_ref()).collect(),
                    ret.as_ref(),
                    conventions,
                    &resolved,
                )
                .map_err(Self::signature_error)?;
                let mut lambda_environment = environment.clone();
                for (parameter, access) in params.iter().zip(signature.params()) {
                    lambda_environment.insert(
                        parameter.name.clone(),
                        AccessFlow::from_type(access.ty()).map_err(Self::signature_error)?,
                    );
                }
                let expected = AccessFlow::from_type(signature.result().ty())
                    .map_err(Self::signature_error)?;
                let tail = self.eval_block(body, &mut lambda_environment, &expected)?;
                if matches!(body.stmts.last(), Some(Stmt::Expr(_))) {
                    tail.verify_exact(&expected, "lambda result")?;
                }
                AccessFlow::Callable(signature)
            }
            Expr::As { expr, ty } => {
                let actual = self.eval_expr(expr, environment, return_expected)?;
                let expected = AccessFlow::from_type(ty).map_err(Self::signature_error)?;
                actual.verify_exact(&expected, "function cast")?;
                expected
            }
            Expr::RecordUpdate { base, fields, .. } => {
                let mut flow = self.eval_expr(base, environment, return_expected)?;
                let base_type = self.resolved_expression_type(base);
                if let AccessFlow::Product(children) = &mut flow {
                    for (field, value) in fields {
                        let value = self.eval_expr(value, environment, return_expected)?;
                        if let Some(index) = base_type.as_ref().and_then(|ty| match ty.unqualified() {
                            Type::Named(name, _) => self
                                .record_fields
                                .get(&(name.clone(), field.clone()))
                                .copied(),
                            _ => None,
                        }) && let Some(slot) = children.get_mut(index)
                        {
                            *slot = value;
                        }
                    }
                }
                flow
            }
            Expr::Record { fields, spread, .. } => {
                if let Some(spread) = spread {
                    self.eval_expr(spread, environment, return_expected)?;
                }
                AccessFlow::product(
                    fields
                        .iter()
                        .map(|(_, value)| self.eval_expr(value, environment, return_expected))
                        .collect::<Result<Vec<_>, _>>()?,
                )
            }
            Expr::ExistentialCall { receiver, args, .. } => {
                self.eval_expr(receiver, environment, return_expected)?;
                for argument in args {
                    self.eval_expr(argument, environment, return_expected)?;
                }
                AccessFlow::None
            }
            Expr::Binary { lhs, rhs, .. } => {
                self.eval_expr(lhs, environment, return_expected)?;
                self.eval_expr(rhs, environment, return_expected)?;
                AccessFlow::None
            }
            Expr::If { cond, then_block, else_block } => {
                self.eval_expr(cond, environment, return_expected)?;
                let mut then_environment = environment.clone();
                let then_flow =
                    self.eval_block(then_block, &mut then_environment, return_expected)?;
                let (else_flow, else_environment) = if let Some(else_block) = else_block {
                    let mut else_environment = environment.clone();
                    let flow =
                        self.eval_block(else_block, &mut else_environment, return_expected)?;
                    (flow, else_environment)
                } else {
                    (AccessFlow::None, environment.clone())
                };
                Self::merge_environments(
                    environment,
                    &[then_environment, else_environment],
                    "if branches",
                )?;
                then_flow.join(&else_flow, "if-expression branches")?
            }
            Expr::Match { scrutinee, arms } => {
                let scrutinee = self.eval_expr(scrutinee, environment, return_expected)?;
                let branches = arms
                    .iter()
                    .map(|arm| self.eval_arm(arm, &scrutinee, environment, return_expected))
                    .collect::<Result<Vec<_>, _>>()?;
                let flows = branches.iter().map(|(flow, _)| flow.clone());
                let branch_environments = branches
                    .iter()
                    .map(|(_, environment)| environment.clone())
                    .collect::<Vec<_>>();
                Self::merge_environments(environment, &branch_environments, "match arms")?;
                Self::join_flows(flows, "match-expression arms")?
            }
            Expr::Block(block) => self.eval_block(block, environment, return_expected)?,
            Expr::While { cond, body } => {
                self.eval_expr(cond, environment, return_expected)?;
                self.eval_block(body, &mut environment.clone(), return_expected)?;
                AccessFlow::None
            }
            Expr::For { var, iter, body } => {
                self.eval_expr(iter, environment, return_expected)?;
                let mut body_environment = environment.clone();
                body_environment.insert(var.clone(), AccessFlow::None);
                self.eval_block(body, &mut body_environment, return_expected)?;
                AccessFlow::None
            }
            Expr::Range { lo, hi, .. } => {
                self.eval_expr(lo, environment, return_expected)?;
                self.eval_expr(hi, environment, return_expected)?;
                AccessFlow::None
            }
            Expr::Index { base, index } => {
                let base = self.eval_expr(base, environment, return_expected)?;
                self.eval_expr(index, environment, return_expected)?;
                match index.as_ref() {
                    Expr::Int(index) if *index >= 0 => base.project(*index as usize),
                    _ => AccessFlow::None,
                }
            }
            Expr::WhileLet { scrutinee, body, .. } => {
                self.eval_expr(scrutinee, environment, return_expected)?;
                self.eval_block(body, &mut environment.clone(), return_expected)?;
                AccessFlow::None
            }
            Expr::MethodCall { receiver, args, .. } => {
                self.eval_expr(receiver, environment, return_expected)?;
                for argument in args {
                    self.eval_expr(argument, environment, return_expected)?;
                }
                AccessFlow::None
            }
        };
        Ok(self.record(expression, flow))
    }
}

/// Build the checked access-signature query for an already type-checked module.
/// The supplied table is authoritative for every inferred and use-site
/// specialized type; this pass propagates only ownership/access contracts.
pub fn checked_facts(
    module: &Module,
    table: &TypeTable,
) -> Result<CheckedAccessFacts, AccessFlowError> {
    AccessVerifier::new(module, table).verify()
}

/// Verify one lowered module through the same checked query used by access-fact
/// consumers. This convenience wrapper owns the cloned AST together with its
/// address-keyed type table for the duration of the query.
pub fn verify_module(module: &Module) -> Result<(), AccessFlowError> {
    let typed = crate::typeck::annotate_checked(module.clone()).map_err(|error| {
        AccessFlowError { message: error.message }
    })?;
    checked_facts(typed.module(), typed.table()).map(|_| ())
}
