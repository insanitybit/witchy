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
    Block, Convention, Expr, Function, Item, MatchArm, Module, Pattern, Stmt, Type,
    TypeDef, TypeQual, Variant, effective_nominal_type_def_params,
    effective_type_def_params,
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

/// One statically checked step from an owner root to borrowed storage.
///
/// This representation is shared by access signatures and loan facts so a
/// callable cannot preserve lifetime names while erasing the field, tuple, or
/// fixed-range relation those lifetimes govern.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum LoanProjectionStep {
    Field(String),
    Tuple(usize),
    Index(i64),
    Range { lo: i64, hi: i64, inclusive: bool },
}

/// A fixed projection relative to an owning root. Empty means the whole value.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct LoanProjection {
    pub steps: Vec<LoanProjectionStep>,
}

impl LoanProjection {
    pub(crate) fn extended(&self, suffix: &Self) -> Self {
        let mut steps = self.steps.clone();
        steps.extend(suffix.steps.iter().cloned());
        Self { steps }
    }

    pub(crate) fn prefixed(&self, step: LoanProjectionStep) -> Self {
        let mut steps = Vec::with_capacity(self.steps.len() + 1);
        steps.push(step);
        steps.extend(self.steps.iter().cloned());
        Self { steps }
    }
}

#[derive(Clone)]
pub(crate) struct BorrowSlot {
    pub(crate) lifetime: String,
    pub(crate) projection: LoanProjection,
    pub(crate) storage_type: Type,
}

/// Nominal declarations needed to derive exact borrowed slots from checked
/// types. The catalog is the single relation authority shared by access-flow
/// verification and the loan checker.
#[derive(Clone, Default)]
pub(crate) struct BorrowRelationCatalog {
    definitions: HashMap<String, TypeDef>,
    constructors: HashMap<String, String>,
}

impl BorrowRelationCatalog {
    pub(crate) fn from_module(module: &Module) -> Self {
        let mut catalog = Self::default();
        for item in &module.items {
            let Item::Type(definition) = item else { continue };
            catalog
                .definitions
                .insert(definition.name.clone(), definition.clone());
            for variant in &definition.variants {
                catalog
                    .constructors
                    .insert(variant.name.clone(), definition.name.clone());
            }
        }
        catalog
    }

    pub(crate) fn slots(&self, ty: &Type) -> Vec<BorrowSlot> {
        self.slots_with(ty, &HashMap::new(), &HashMap::new(), &mut Vec::new())
    }

    fn borrow_lifetimes(&self, ty: &Type) -> Vec<String> {
        slot_lifetimes(&self.slots(ty))
    }

    fn slots_with(
        &self,
        ty: &Type,
        lifetimes: &HashMap<String, String>,
        types: &HashMap<String, Type>,
        active_nominals: &mut Vec<(String, Type)>,
    ) -> Vec<BorrowSlot> {
        match ty {
            Type::Qualified(TypeQual::Borrow(lifetime), inner) => {
                let nested = self.slots_with(inner, lifetimes, types, active_nominals);
                if !nested.is_empty() {
                    nested
                } else {
                    vec![BorrowSlot {
                        lifetime: lifetimes
                            .get(lifetime)
                            .cloned()
                            .unwrap_or_else(|| lifetime.clone()),
                        projection: LoanProjection::default(),
                        storage_type: substitute_borrow_slot_type(inner, types),
                    }]
                }
            }
            Type::Qualified(_, inner) => {
                self.slots_with(inner, lifetimes, types, active_nominals)
            }
            Type::Tuple(items) => items
                .iter()
                .enumerate()
                .flat_map(|(index, item)| {
                    self.slots_with(item, lifetimes, types, active_nominals)
                        .into_iter()
                        .map(move |mut slot| {
                            slot.projection = slot
                                .projection
                                .prefixed(LoanProjectionStep::Tuple(index));
                            slot
                        })
                })
                .collect(),
            Type::Named(name, arguments) => {
                if arguments.is_empty()
                    && let Some(substituted) = types.get(name)
                {
                    let substituted = substitute_borrow_slot_type(substituted, types);
                    if &substituted == ty {
                        return coarse_borrow_slots(ty, lifetimes, types);
                    }
                    return self.slots_with(
                        &substituted,
                        lifetimes,
                        types,
                        active_nominals,
                    );
                }
                let Some(definition) = self.definitions.get(name) else {
                    return coarse_borrow_slots(ty, lifetimes, types);
                };
                let mut nested_lifetimes = lifetimes.clone();
                let mut nested_types = types.clone();
                for (parameter, argument) in
                    effective_nominal_type_def_params(definition).iter().zip(arguments)
                {
                    if parameter.starts_with('\'') {
                        if let Type::Named(argument, arguments) = argument
                            && arguments.is_empty()
                            && argument.starts_with('\'')
                        {
                            let parameter = parameter
                                .strip_prefix('\'')
                                .expect("guarded lifetime parameter");
                            let argument = argument
                                .strip_prefix('\'')
                                .expect("guarded lifetime argument");
                            nested_lifetimes.insert(
                                parameter.to_string(),
                                lifetimes
                                    .get(argument)
                                    .cloned()
                                    .unwrap_or_else(|| argument.to_string()),
                            );
                        }
                    } else {
                        nested_types.insert(
                            parameter.clone(),
                            substitute_borrow_slot_type(argument, types),
                        );
                    }
                }
                let instantiated = Type::Named(
                    name.clone(),
                    arguments
                        .iter()
                        .map(|argument| substitute_borrow_slot_type(argument, types))
                        .collect(),
                );
                if active_nominals.iter().any(|(_, active)| active == &instantiated) {
                    return Vec::new();
                }
                if active_nominals.iter().rev().any(|(active_name, active)| {
                    active_name == name
                        && type_node_count(&instantiated) >= type_node_count(active)
                }) {
                    // A recursive declaration which does not consume a finite
                    // type argument cannot reveal a new finite field path. Keep
                    // any lifetime relation visible in its instantiated
                    // arguments as a conservative root relation and terminate.
                    return coarse_borrow_slots(&instantiated, &nested_lifetimes, &nested_types);
                }
                active_nominals.push((name.clone(), instantiated));
                let [variant] = definition.variants.as_slice() else {
                    active_nominals.pop();
                    return Vec::new();
                };
                let slots = variant
                    .fields
                    .iter()
                    .enumerate()
                    .flat_map(|(index, field)| {
                        let step = variant
                            .field_names
                            .get(index)
                            .cloned()
                            .map(LoanProjectionStep::Field)
                            .unwrap_or(LoanProjectionStep::Tuple(index));
                        self.slots_with(
                            field,
                            &nested_lifetimes,
                            &nested_types,
                            active_nominals,
                        )
                        .into_iter()
                        .map(move |mut slot| {
                            slot.projection = slot.projection.prefixed(step.clone());
                            slot
                        })
                    })
                    .collect();
                active_nominals.pop();
                slots
            }
            Type::Dyn(_, _) => coarse_borrow_slots(ty, lifetimes, types),
            Type::RecordCompose { .. } | Type::Fn(_, _, _) => Vec::new(),
        }
    }

    pub(crate) fn borrowed_constructor(&self, constructor: &str) -> bool {
        self.constructors
            .get(constructor)
            .and_then(|name| self.definitions.get(name))
            .is_some_and(|definition| {
                definition.params.iter().any(|parameter| parameter.starts_with('\''))
            })
    }

    pub(crate) fn borrowed_record(&self, name: &str) -> bool {
        self.definitions
            .get(name)
            .is_some_and(|definition| {
                definition.params.iter().any(|parameter| parameter.starts_with('\''))
            })
    }

    pub(crate) fn constructor_step(
        &self,
        constructor: &str,
        index: usize,
    ) -> LoanProjectionStep {
        self.constructors
            .get(constructor)
            .and_then(|name| self.definitions.get(name))
            .and_then(|definition| {
                definition
                    .variants
                    .iter()
                    .find(|variant| variant.name == constructor)
            })
            .and_then(|variant| variant.field_names.get(index))
            .cloned()
            .map(LoanProjectionStep::Field)
            .unwrap_or(LoanProjectionStep::Tuple(index))
    }
}

fn slot_lifetimes(slots: &[BorrowSlot]) -> Vec<String> {
    let mut lifetimes = Vec::new();
    for slot in slots {
        if !lifetimes.contains(&slot.lifetime) {
            lifetimes.push(slot.lifetime.clone());
        }
    }
    lifetimes
}

/// Preserve the coarse lifetime relation available from a type alone when no
/// declaration catalog can authenticate nominal field paths. Every discovered
/// lifetime conservatively refers to the nominal root.
fn coarse_borrow_slots(
    ty: &Type,
    substitutions: &HashMap<String, String>,
    types: &HashMap<String, Type>,
) -> Vec<BorrowSlot> {
    fn collect(
        ty: &Type,
        substitutions: &HashMap<String, String>,
        found: &mut Vec<String>,
    ) {
        match ty {
            Type::Qualified(TypeQual::Borrow(lifetime), inner) => {
                let lifetime = substitutions
                    .get(lifetime)
                    .cloned()
                    .unwrap_or_else(|| lifetime.clone());
                if !found.contains(&lifetime) {
                    found.push(lifetime);
                }
                collect(inner, substitutions, found);
            }
            Type::Qualified(_, inner) => collect(inner, substitutions, found),
            Type::Named(name, arguments) if arguments.is_empty() && name.starts_with('\'') => {
                let lifetime = name
                    .strip_prefix('\'')
                    .expect("guarded lifetime marker");
                let lifetime = substitutions
                    .get(lifetime)
                    .cloned()
                    .unwrap_or_else(|| lifetime.to_string());
                if !found.contains(&lifetime) {
                    found.push(lifetime);
                }
            }
            Type::Named(_, arguments) | Type::Dyn(_, arguments) | Type::Tuple(arguments) => {
                for argument in arguments {
                    collect(argument, substitutions, found);
                }
            }
            Type::RecordCompose { base, fields } => {
                collect(base, substitutions, found);
                for (_, field) in fields {
                    collect(field, substitutions, found);
                }
            }
            Type::Fn(_, _, _) => {}
        }
    }

    let mut lifetimes = Vec::new();
    collect(ty, substitutions, &mut lifetimes);
    let storage_type = substitute_borrow_slot_type(ty, types);
    lifetimes
        .into_iter()
        .map(|lifetime| BorrowSlot {
            lifetime,
            projection: LoanProjection::default(),
            storage_type: storage_type.clone(),
        })
        .collect()
}

fn type_node_count(ty: &Type) -> usize {
    let children = match ty {
        Type::Qualified(_, inner) => type_node_count(inner),
        Type::Named(_, arguments) | Type::Dyn(_, arguments) | Type::Tuple(arguments) => arguments
            .iter()
            .fold(0usize, |count, argument| count.saturating_add(type_node_count(argument))),
        Type::Fn(params, result, _) => params
            .iter()
            .fold(type_node_count(result), |count, parameter| {
                count.saturating_add(type_node_count(parameter))
            }),
        Type::RecordCompose { base, fields } => fields
            .iter()
            .fold(type_node_count(base), |count, (_, field)| {
                count.saturating_add(type_node_count(field))
            }),
    };
    1usize.saturating_add(children)
}

fn substitute_borrow_slot_type(ty: &Type, substitutions: &HashMap<String, Type>) -> Type {
    substitute_borrow_slot_type_with(ty, substitutions, &mut Vec::new())
}

fn substitute_borrow_slot_type_with(
    ty: &Type,
    substitutions: &HashMap<String, Type>,
    active: &mut Vec<String>,
) -> Type {
    match ty {
        Type::Named(name, arguments) if arguments.is_empty() => {
            let Some(substitution) = substitutions.get(name) else { return ty.clone() };
            if active.contains(name) {
                return ty.clone();
            }
            active.push(name.clone());
            let substituted = substitute_borrow_slot_type_with(substitution, substitutions, active);
            active.pop();
            substituted
        }
        Type::Named(name, arguments) => Type::Named(
            name.clone(),
            arguments
                .iter()
                .map(|argument| substitute_borrow_slot_type_with(argument, substitutions, active))
                .collect(),
        ),
        Type::Qualified(qualifier, inner) => Type::Qualified(
            qualifier.clone(),
            Box::new(substitute_borrow_slot_type_with(inner, substitutions, active)),
        ),
        Type::Tuple(items) => Type::Tuple(
            items
                .iter()
                .map(|item| substitute_borrow_slot_type_with(item, substitutions, active))
                .collect(),
        ),
        Type::Fn(params, result, conventions) => Type::Fn(
            params
                .iter()
                .map(|param| substitute_borrow_slot_type_with(param, substitutions, active))
                .collect(),
            Box::new(substitute_borrow_slot_type_with(result, substitutions, active)),
            conventions.clone(),
        ),
        Type::Dyn(name, arguments) => Type::Dyn(
            name.clone(),
            arguments
                .iter()
                .map(|argument| substitute_borrow_slot_type_with(argument, substitutions, active))
                .collect(),
        ),
        Type::RecordCompose { base, fields } => Type::RecordCompose {
            base: Box::new(substitute_borrow_slot_type_with(base, substitutions, active)),
            fields: fields
                .iter()
                .map(|(name, field)| {
                    (
                        name.clone(),
                        substitute_borrow_slot_type_with(field, substitutions, active),
                    )
                })
                .collect(),
        },
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

/// One input slot which may own a borrowed result slot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BorrowOwnerRelation {
    position: usize,
    input_projection: LoanProjection,
}

impl BorrowOwnerRelation {
    pub fn position(&self) -> usize {
        self.position
    }

    pub fn input_projection(&self) -> &LoanProjection {
        &self.input_projection
    }
}

/// An exact borrowed result slot and the input slots which can own it.
#[derive(Clone, Debug, PartialEq)]
pub struct BorrowRelation {
    lifetime: String,
    output_projection: LoanProjection,
    owners: Vec<BorrowOwnerRelation>,
    storage_type: Type,
}

impl BorrowRelation {
    pub fn lifetime(&self) -> &str {
        &self.lifetime
    }

    pub fn output_projection(&self) -> &LoanProjection {
        &self.output_projection
    }

    pub fn owners(&self) -> &[BorrowOwnerRelation] {
        &self.owners
    }

    pub fn storage_type(&self) -> &Type {
        &self.storage_type
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
    fn as_type(&self) -> Type {
        let params = self.params.iter().map(|param| param.ty.clone()).collect();
        let conventions = self
            .params
            .iter()
            .map(|param| match param.kind {
                AccessKind::OwnedImmutable => Convention::Let,
                AccessKind::SharedBorrow => Convention::Borrow,
                AccessKind::ExclusiveWriteback => Convention::Var,
                AccessKind::Consuming => Convention::Own,
            })
            .collect();
        let mut ty = Type::Fn(params, Box::new(self.result.ty.clone()), conventions);
        for qualifier in self.callable_qualifiers.iter().rev() {
            let qualifier = match qualifier {
                AccessQualifier::Frozen => TypeQual::Frozen,
                AccessQualifier::Unique => TypeQual::Unique,
                AccessQualifier::LocalUnique => TypeQual::LocalUnique,
                AccessQualifier::Borrow(lifetime) => TypeQual::Borrow(lifetime.clone()),
            };
            ty = Type::Qualified(qualifier, Box::new(ty));
        }
        ty
    }

    /// Derive the signature of a checked declaration.
    ///
    /// Checked declarations with inferred parameter types still retain `None`
    /// in the AST. Callers must first obtain their finalized checked [`Type`]
    /// and use [`Self::from_function_type`] instead of inventing a type here.
    pub fn from_function(function: &Function) -> Result<Self, AccessSignatureError> {
        Self::from_function_with_catalog(function, &BorrowRelationCatalog::default())
    }

    pub(crate) fn from_function_with_catalog(
        function: &Function,
        catalog: &BorrowRelationCatalog,
    ) -> Result<Self, AccessSignatureError> {
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
        Self::from_parts_with_catalog(params, result, conventions, catalog)
    }

    /// Derive the signature carried by a checked first-class function type.
    pub fn from_function_type(ty: &Type) -> Result<Self, AccessSignatureError> {
        Self::from_function_type_with_catalog(ty, &BorrowRelationCatalog::default())
    }

    pub(crate) fn from_function_type_with_catalog(
        ty: &Type,
        catalog: &BorrowRelationCatalog,
    ) -> Result<Self, AccessSignatureError> {
        let callable_qualifiers = leading_qualifiers(ty);
        let Type::Fn(params, result, conventions) = ty.unqualified() else {
            return Err(AccessSignatureError::NotFunctionType);
        };
        let conventions = normalized_conventions(params.len(), conventions)?;
        let mut signature = Self::from_parts_with_catalog(
            params.clone(),
            result.as_ref().clone(),
            conventions,
            catalog,
        )?;
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
        Self::from_resolved_function_with_catalog(
            function,
            resolved,
            &BorrowRelationCatalog::default(),
        )
    }

    fn from_resolved_function_with_catalog(
        function: &Function,
        resolved: &Type,
        catalog: &BorrowRelationCatalog,
    ) -> Result<Self, AccessSignatureError> {
        let params = function.params.iter().map(|param| param.ty.as_ref()).collect();
        let conventions = function.params.iter().map(|param| param.convention).collect();
        Self::from_resolved_parts_with_catalog(
            params,
            function.ret.as_ref(),
            conventions,
            resolved,
            catalog,
        )
    }

    /// Derive a lambda or generated callable contract from finalized checked
    /// parameter/result types without fabricating placeholder type names.
    pub fn from_resolved_parts(
        declared_params: Vec<Option<&Type>>,
        declared_result: Option<&Type>,
        conventions: Vec<Convention>,
        resolved: &Type,
    ) -> Result<Self, AccessSignatureError> {
        Self::from_resolved_parts_with_catalog(
            declared_params,
            declared_result,
            conventions,
            resolved,
            &BorrowRelationCatalog::default(),
        )
    }

    fn from_resolved_parts_with_catalog(
        declared_params: Vec<Option<&Type>>,
        declared_result: Option<&Type>,
        conventions: Vec<Convention>,
        resolved: &Type,
        catalog: &BorrowRelationCatalog,
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
        Self::from_parts_with_catalog(params, result, conventions, catalog)
    }

    /// Derive a signature from finalized checked parameter and result types.
    pub fn from_parts(
        params: Vec<Type>,
        result: Type,
        conventions: Vec<Convention>,
    ) -> Result<Self, AccessSignatureError> {
        Self::from_parts_with_catalog(
            params,
            result,
            conventions,
            &BorrowRelationCatalog::default(),
        )
    }

    pub(crate) fn from_parts_with_catalog(
        params: Vec<Type>,
        result: Type,
        conventions: Vec<Convention>,
        catalog: &BorrowRelationCatalog,
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

            let borrow_lifetimes = catalog.borrow_lifetimes(&ty);
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
        let result_slots = catalog.slots(&result);
        let result_lifetimes = slot_lifetimes(&result_slots);
        let result_state = ownership_state_class(&result)?;
        let returns_state = type_has_ownership_qualifier(&result);
        let ownership_output = returns_state.then_some(result_state).flatten();

        let mut borrow_relations = Vec::with_capacity(result_slots.len());
        for slot in result_slots {
            let mut owners = Vec::new();
            for (position, param) in access_params.iter().enumerate() {
                for input in catalog.slots(&param.ty) {
                    if input.lifetime == slot.lifetime {
                        owners.push(BorrowOwnerRelation {
                            position,
                            input_projection: input.projection,
                        });
                    }
                }
            }
            if owners.is_empty() {
                return Err(AccessSignatureError::UnboundResultLifetime {
                    lifetime: slot.lifetime,
                });
            }
            borrow_relations.push(BorrowRelation {
                lifetime: slot.lifetime,
                output_projection: slot.projection,
                owners,
                storage_type: slot.storage_type,
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

        if self.borrow_relations.len() != candidate.borrow_relations.len()
            || self
                .borrow_relations
                .iter()
                .zip(&candidate.borrow_relations)
                .any(|(required, candidate)| {
                    !borrow_relation_compatible(required, candidate, &lifetimes)
                })
        {
            return Err(AccessMismatch::new(
                Some(SignaturePosition::Result),
                AccessMismatchKind::BorrowRelation,
            ));
        }
        Ok(())
    }

    /// Compare the canonical output-slot to input-slot graph independently of
    /// surface type specialization. The type checker already owns surface type
    /// compatibility at callable-value sites; this preserves the relation
    /// identity while allowing a generic callable and its specialization.
    pub(crate) fn has_same_projected_borrow_relations(&self, candidate: &Self) -> bool {
        self.borrow_relations.len() == candidate.borrow_relations.len()
            && self
                .borrow_relations
                .iter()
                .zip(&candidate.borrow_relations)
                .all(|(left, right)| {
                    left.output_projection == right.output_projection
                        && left.owners.len() == right.owners.len()
                        && left.owners.iter().zip(&right.owners).all(
                            |(left_owner, right_owner)| {
                                left_owner.position == right_owner.position
                                    && left_owner.input_projection
                                        == right_owner.input_projection
                            },
                        )
                })
    }
}

fn borrow_relation_compatible(
    required: &BorrowRelation,
    candidate: &BorrowRelation,
    lifetimes: &LifetimeBijection,
) -> bool {
    lifetimes.matches(&required.lifetime, &candidate.lifetime)
        && required.output_projection == candidate.output_projection
        && required.owners.len() == candidate.owners.len()
        && required
            .owners
            .iter()
            .zip(&candidate.owners)
            .all(|(required, candidate)| {
                required.position == candidate.position
                    && required.input_projection == candidate.input_projection
            })
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
        (Type::Named(left, left_args), Type::Named(right, right_args))
            if left_args.is_empty()
                && right_args.is_empty()
                && left.starts_with('\'')
                && right.starts_with('\'') =>
        {
            let left = left.strip_prefix('\'').expect("guarded lifetime marker");
            let right = right.strip_prefix('\'').expect("guarded lifetime marker");
            if lifetimes.relate(left, right) {
                Ok(())
            } else {
                Err(AccessMismatchKind::BorrowRelation)
            }
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
    /// An access component which the active value does not instantiate (for
    /// example the element of `None` or an empty list). It refines to the other
    /// side at an ascription or control-flow join; it is never evidence that a
    /// present callable may erase its contract.
    Unknown,
    Callable(AccessSignature),
    Product(Vec<AccessFlow>),
    Sequence(Box<AccessFlow>),
    Named { name: String, arguments: Vec<AccessFlow>, dynamic: bool },
}

impl AccessFlow {
    fn has_callable_contract(&self) -> bool {
        match self {
            Self::Callable(_) => true,
            Self::Product(children) => children.iter().any(Self::has_callable_contract),
            Self::Sequence(element) => element.has_callable_contract(),
            Self::Named { arguments, .. } => {
                arguments.iter().any(Self::has_callable_contract)
            }
            Self::None | Self::Unknown => false,
        }
    }

    fn product(children: Vec<Self>) -> Self {
        if children.iter().all(|child| matches!(child, Self::None)) {
            Self::None
        } else {
            Self::Product(children)
        }
    }

    fn from_type_with_catalog(
        ty: &Type,
        catalog: &BorrowRelationCatalog,
    ) -> Result<Self, AccessSignatureError> {
        if matches!(ty.unqualified(), Type::Fn(_, _, _)) {
            return AccessSignature::from_function_type_with_catalog(ty, catalog)
                .map(Self::Callable);
        }
        match ty.unqualified() {
            Type::Tuple(children) => {
                let children = children
                    .iter()
                    .map(|child| Self::from_type_with_catalog(child, catalog))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(Self::product(children))
            }
            Type::Named(name, arguments) => {
                let arguments = arguments
                    .iter()
                    .map(|argument| Self::from_type_with_catalog(argument, catalog))
                    .collect::<Result<Vec<_>, _>>()?;
                if arguments.iter().all(|argument| matches!(argument, Self::None)) {
                    Ok(Self::None)
                } else {
                    Ok(Self::Named { name: name.clone(), arguments, dynamic: false })
                }
            }
            Type::Dyn(name, arguments) => {
                let arguments = arguments
                    .iter()
                    .map(|argument| Self::from_type_with_catalog(argument, catalog))
                    .collect::<Result<Vec<_>, _>>()?;
                if arguments.iter().all(|argument| matches!(argument, Self::None)) {
                    Ok(Self::None)
                } else {
                    Ok(Self::Named { name: name.clone(), arguments, dynamic: true })
                }
            }
            Type::Qualified(_, _) => unreachable!("unqualified removes every qualifier"),
            Type::RecordCompose { .. } | Type::Fn(_, _, _) => Ok(Self::None),
        }
    }

    /// Reapply the access identity carried by this flow to a finalized type
    /// shape. Finalized checker types deliberately erase ownership qualifiers;
    /// inferred aggregate and closure results recover them from the checked
    /// value flow instead of silently publishing the erased shape.
    fn materialize_type(&self, ty: &Type) -> Type {
        match (self, ty.unqualified()) {
            (Self::Callable(signature), Type::Fn(_, _, _)) => signature.as_type(),
            (Self::Product(actual), Type::Tuple(expected)) if actual.len() == expected.len() => {
                Type::Tuple(
                    actual
                        .iter()
                        .zip(expected)
                        .map(|(actual, expected)| actual.materialize_type(expected))
                        .collect(),
                )
            }
            (Self::Sequence(actual), Type::Named(name, expected)) if expected.len() == 1 => {
                Type::Named(
                    name.clone(),
                    vec![actual.materialize_type(&expected[0])],
                )
            }
            (
                Self::Named { name: actual_name, arguments: actual, dynamic: false },
                Type::Named(expected_name, expected),
            ) if actual_name == expected_name && actual.len() == expected.len() => Type::Named(
                expected_name.clone(),
                actual
                    .iter()
                    .zip(expected)
                    .map(|(actual, expected)| actual.materialize_type(expected))
                    .collect(),
            ),
            (
                Self::Named { name: actual_name, arguments: actual, dynamic: true },
                Type::Dyn(expected_name, expected),
            ) if actual_name == expected_name && actual.len() == expected.len() => Type::Dyn(
                expected_name.clone(),
                actual
                    .iter()
                    .zip(expected)
                    .map(|(actual, expected)| actual.materialize_type(expected))
                    .collect(),
            ),
            _ => ty.clone(),
        }
    }

    fn verify_exact(&self, expected: &Self, context: &str) -> Result<(), AccessFlowError> {
        if !expected.has_callable_contract() {
            return Ok(());
        }
        match (self, expected) {
            (Self::Unknown, _) => Ok(()),
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
            (Self::Sequence(actual), Self::Sequence(expected)) => {
                actual.verify_exact(expected, context)
            }
            (
                Self::Sequence(actual),
                Self::Named { arguments, dynamic: false, .. },
            ) if arguments.len() == 1 => {
                actual.verify_exact(&arguments[0], context)
            }
            (
                Self::Named { arguments, dynamic: false, .. },
                Self::Sequence(expected),
            ) if arguments.len() == 1 => arguments[0].verify_exact(expected, context),
            (
                Self::Named { name: actual_name, arguments: actual, dynamic: actual_dynamic },
                Self::Named { name: expected_name, arguments: expected, dynamic: expected_dynamic },
            ) if actual_name == expected_name
                && actual_dynamic == expected_dynamic
                && actual.len() == expected.len() =>
            {
                for (actual, expected) in actual.iter().zip(expected) {
                    actual.verify_exact(expected, context)?;
                }
                Ok(())
            }
            _ => Err(AccessFlowError::missing(context)),
        }
    }

    fn verify_directed(&self, expected: &Self, context: &str) -> Result<(), AccessFlowError> {
        if !expected.has_callable_contract() {
            return Ok(());
        }
        match (self, expected) {
            (Self::Unknown, _) => Ok(()),
            (
                Self::Named { name: actual_name, arguments: actual, dynamic: true },
                Self::Named { name: expected_name, arguments: expected, dynamic: true },
            ) if actual_name == expected_name && actual.len() == expected.len() => {
                for (actual, expected) in actual.iter().zip(expected) {
                    actual.verify_directed(expected, context)?;
                }
                Ok(())
            }
            // Concrete-to-existential conversion and existential upcasting change
            // nominal identity. Type checking has authenticated the witness or
            // upcast; the target publishes its own access identity.
            (_, Self::Named { dynamic: true, .. }) => Ok(()),
            (Self::Product(actual), Self::Product(expected))
                if actual.len() == expected.len() =>
            {
                for (actual, expected) in actual.iter().zip(expected) {
                    actual.verify_directed(expected, context)?;
                }
                Ok(())
            }
            (Self::Sequence(actual), Self::Sequence(expected)) => {
                actual.verify_directed(expected, context)
            }
            (
                Self::Sequence(actual),
                Self::Named { arguments, dynamic: false, .. },
            ) if arguments.len() == 1 => actual.verify_directed(&arguments[0], context),
            (
                Self::Named { arguments, dynamic: false, .. },
                Self::Sequence(expected),
            ) if arguments.len() == 1 => arguments[0].verify_directed(expected, context),
            (
                Self::Named { name: actual_name, arguments: actual, dynamic: actual_dynamic },
                Self::Named { name: expected_name, arguments: expected, dynamic: expected_dynamic },
            ) if actual_name == expected_name
                && actual_dynamic == expected_dynamic
                && actual.len() == expected.len() =>
            {
                for (actual, expected) in actual.iter().zip(expected) {
                    actual.verify_directed(expected, context)?;
                }
                Ok(())
            }
            _ => self.verify_exact(expected, context),
        }
    }

    fn join(&self, other: &Self, context: &str) -> Result<Self, AccessFlowError> {
        if matches!(self, Self::Unknown) {
            return Ok(other.clone());
        }
        if matches!(other, Self::Unknown) {
            return Ok(self.clone());
        }
        if let (
            Self::Named { name: left_name, arguments: left, dynamic: left_dynamic },
            Self::Named { name: right_name, arguments: right, dynamic: right_dynamic },
        ) = (self, other)
            && left_name == right_name
            && left_dynamic == right_dynamic
            && left.len() == right.len()
        {
            return Ok(Self::Named {
                name: left_name.clone(),
                arguments: left
                    .iter()
                    .zip(right)
                    .map(|(left, right)| left.join(right, context))
                    .collect::<Result<Vec<_>, _>>()?,
                dynamic: *left_dynamic,
            });
        }
        self.verify_exact(other, context)?;
        other.verify_exact(self, context)?;
        Ok(self.clone())
    }

}

/// Finalized access-signature facts keyed to the exact typed AST used to build
/// them. The owning [`crate::typeck::TypedModule`] must remain alive while these
/// address-keyed queries are used.
pub struct CheckedAccessFacts<'module> {
    owner: &'module Module,
    values: HashMap<usize, AccessFlow>,
    declarations: HashMap<String, AccessSignature>,
    calls: HashMap<usize, AccessSignature>,
}

impl CheckedAccessFacts<'_> {
    fn owns(&self, module: &Module) -> bool {
        std::ptr::eq(self.owner, module)
    }

    pub fn callable_at(&self, module: &Module, expression: &Expr) -> Option<&AccessSignature> {
        if !self.owns(module) {
            return None;
        }
        match self.values.get(&(expression as *const Expr as usize)) {
            Some(AccessFlow::Callable(signature)) => Some(signature),
            _ => None,
        }
    }

    /// The finalized access identity of a checked direct declaration.
    pub fn declaration(&self, name: &str) -> Option<&AccessSignature> {
        self.declarations.get(name)
    }

    /// The finalized direct, indirect, or existential callable selected at one
    /// exact checked call expression.
    pub fn call_at(&self, module: &Module, expression: &Expr) -> Option<&AccessSignature> {
        if !self.owns(module) {
            return None;
        }
        self.calls.get(&(expression as *const Expr as usize))
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

#[derive(Clone, Copy)]
struct CallTypeContext<'a> {
    arguments: &'a [Option<Type>],
    result: Option<&'a Type>,
}

struct AccessVerifier<'a> {
    module: &'a Module,
    table: &'a TypeTable,
    functions: HashMap<String, Function>,
    types: HashMap<String, TypeDef>,
    variants: HashMap<String, Vec<(String, Variant)>>,
    borrow_catalog: BorrowRelationCatalog,
    facts: CheckedAccessFacts<'a>,
    return_frames: Vec<Vec<AccessFlow>>,
    expression_type_hints: HashMap<usize, Type>,
}

impl<'a> AccessVerifier<'a> {
    fn new(module: &'a Module, table: &'a TypeTable) -> Self {
        let borrow_catalog = BorrowRelationCatalog::from_module(module);
        let functions = module
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Function(function) => Some((function.name.clone(), function.clone())),
                _ => None,
            })
            .collect();
        let types = module
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Type(definition) => Some((definition.name.clone(), definition.clone())),
                _ => None,
            })
            .collect::<HashMap<_, _>>();
        let mut variants = HashMap::<String, Vec<(String, Variant)>>::new();
        for definition in types.values() {
            for variant in &definition.variants {
                variants
                    .entry(variant.name.clone())
                    .or_default()
                    .push((definition.name.clone(), variant.clone()));
            }
        }
        Self {
            module,
            table,
            functions,
            types,
            variants,
            borrow_catalog,
            facts: CheckedAccessFacts {
                owner: module,
                values: HashMap::new(),
                declarations: HashMap::new(),
                calls: HashMap::new(),
            },
            return_frames: Vec::new(),
            expression_type_hints: HashMap::new(),
        }
    }

    fn verify(mut self) -> Result<CheckedAccessFacts<'a>, AccessFlowError> {
        for item in &self.module.items {
            let Item::Function(function) = item else { continue };
            self.verify_function(function)?;
        }
        Ok(self.facts)
    }

    fn flow_from_type(&self, ty: &Type) -> Result<AccessFlow, AccessSignatureError> {
        AccessFlow::from_type_with_catalog(ty, &self.borrow_catalog)
    }

    fn resolved_expression_type(&self, expression: &Expr) -> Option<Type> {
        self.table.type_of(expression).and_then(ty_to_ast)
    }

    fn contextual_expression_type(&self, expression: &Expr) -> Option<Type> {
        self.expression_type_hints
            .get(&(expression as *const Expr as usize))
            .cloned()
            .or_else(|| self.resolved_expression_type(expression))
    }

    fn declaration_signature(
        &self,
        function: &Function,
    ) -> Result<Option<AccessSignature>, AccessFlowError> {
        let resolved = self.table.function_type(&function.name);
        let signature = match resolved {
            Some(resolved) => AccessSignature::from_resolved_function_with_catalog(
                function,
                &resolved,
                &self.borrow_catalog,
            ),
            None => AccessSignature::from_function_with_catalog(
                function,
                &self.borrow_catalog,
            ),
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
        self.facts
            .declarations
            .insert(function.name.clone(), signature.clone());
        let mut environment = HashMap::new();
        for (parameter, access) in function.params.iter().zip(signature.params()) {
            environment.insert(
                parameter.name.clone(),
                self.flow_from_type(access.ty()).map_err(Self::signature_error)?,
            );
        }
        let expected = self.flow_from_type(signature.result().ty())
            .map_err(Self::signature_error)?;
        let tail = self.eval_block(&function.body, &mut environment, &expected)?;
        if matches!(function.body.stmts.last(), Some(Stmt::Expr(_))) {
            tail.verify_directed(
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
        match AccessSignature::from_function_with_catalog(function, &self.borrow_catalog) {
            Ok(signature) => return Ok(Some(signature)),
            Err(
                AccessSignatureError::MissingParameterType { .. }
                | AccessSignatureError::MissingResultType,
            ) => {}
            Err(error) => return Err(Self::signature_error(error)),
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
        AccessSignature::from_resolved_function_with_catalog(
            function,
            &resolved,
            &self.borrow_catalog,
        )
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
        AccessSignature::from_resolved_function_with_catalog(
            function,
            &resolved,
            &self.borrow_catalog,
        )
            .map(Some)
            .map_err(Self::signature_error)
    }

    fn nominal_arguments<'b>(flow: &'b AccessFlow, name: &str) -> Option<&'b [AccessFlow]> {
        match flow {
            AccessFlow::Named { name: found, arguments, dynamic: false } if found == name => {
                Some(arguments)
            }
            _ => None,
        }
    }

    fn substitute_type(ty: &Type, substitutions: &HashMap<String, Type>) -> Type {
        match ty {
            Type::Named(name, arguments) if arguments.is_empty() => substitutions
                .get(name)
                .cloned()
                .unwrap_or_else(|| ty.clone()),
            Type::Named(name, arguments) => Type::Named(
                name.clone(),
                arguments
                    .iter()
                    .map(|argument| Self::substitute_type(argument, substitutions))
                    .collect(),
            ),
            Type::Dyn(name, arguments) => Type::Dyn(
                name.clone(),
                arguments
                    .iter()
                    .map(|argument| Self::substitute_type(argument, substitutions))
                    .collect(),
            ),
            Type::Tuple(fields) => Type::Tuple(
                fields
                    .iter()
                    .map(|field| Self::substitute_type(field, substitutions))
                    .collect(),
            ),
            Type::Fn(params, result, conventions) => Type::Fn(
                params
                    .iter()
                    .map(|param| Self::substitute_type(param, substitutions))
                    .collect(),
                Box::new(Self::substitute_type(result, substitutions)),
                conventions.clone(),
            ),
            Type::Qualified(qualifier, inner) => Type::Qualified(
                qualifier.clone(),
                Box::new(Self::substitute_type(inner, substitutions)),
            ),
            Type::RecordCompose { base, fields } => Type::RecordCompose {
                base: Box::new(Self::substitute_type(base, substitutions)),
                fields: fields
                    .iter()
                    .map(|(name, field)| {
                        (name.clone(), Self::substitute_type(field, substitutions))
                    })
                    .collect(),
            },
        }
    }

    fn nominal_substitutions(&self, name: &str, ty: &Type) -> HashMap<String, Type> {
        let Some(definition) = self.types.get(name) else { return HashMap::new() };
        let arguments = match ty.unqualified() {
            Type::Named(found, arguments) if found == name => arguments,
            _ => return HashMap::new(),
        };
        effective_type_def_params(definition)
            .into_iter()
            .zip(arguments.iter().cloned())
            .collect()
    }

    fn flow_for_declared_type(
        &self,
        declared: &Type,
        substitutions: &HashMap<String, Type>,
        access_arguments: &HashMap<String, AccessFlow>,
    ) -> Result<AccessFlow, AccessFlowError> {
        if let Type::Named(name, arguments) = declared
            && arguments.is_empty()
            && let Some(flow) = access_arguments.get(name)
        {
            return Ok(flow.clone());
        }
        let resolved = Self::substitute_type(declared, substitutions);
        self.flow_from_type(&resolved).map_err(Self::signature_error)
    }

    fn nominal_access_arguments(
        &self,
        name: &str,
        flow: &AccessFlow,
    ) -> HashMap<String, AccessFlow> {
        let Some(definition) = self.types.get(name) else { return HashMap::new() };
        let Some(arguments) = Self::nominal_arguments(flow, name) else {
            return HashMap::new();
        };
        effective_type_def_params(definition)
            .into_iter()
            .zip(arguments.iter().cloned())
            .collect()
    }

    fn constrain_declared_flow(
        &self,
        declared: &Type,
        actual: &AccessFlow,
        substitutions: &HashMap<String, Type>,
        access_arguments: &mut HashMap<String, AccessFlow>,
        context: &str,
    ) -> Result<(), AccessFlowError> {
        if let Type::Named(name, arguments) = declared
            && arguments.is_empty()
            && substitutions.contains_key(name)
        {
            let entry = access_arguments.entry(name.clone()).or_insert(AccessFlow::Unknown);
            *entry = entry.join(actual, context)?;
            return Ok(());
        }
        if let Type::Named(name, declared_arguments) = declared {
            let actual_arguments = match actual {
                AccessFlow::Named {
                    name: actual_name,
                    arguments,
                    dynamic: false,
                } if actual_name == name => Some(arguments.as_slice()),
                AccessFlow::Sequence(element) if declared_arguments.len() == 1 => {
                    Some(std::slice::from_ref(element.as_ref()))
                }
                _ => None,
            };
            if let Some(actual_arguments) = actual_arguments
                && actual_arguments.len() == declared_arguments.len()
            {
                for (declared, actual) in declared_arguments.iter().zip(actual_arguments) {
                    self.constrain_declared_flow(
                        declared,
                        actual,
                        substitutions,
                        access_arguments,
                        context,
                    )?;
                }
                return Ok(());
            }
        }
        if let Type::Tuple(declared_fields) = declared
            && let AccessFlow::Product(actual_fields) = actual
            && declared_fields.len() == actual_fields.len()
        {
            for (declared, actual) in declared_fields.iter().zip(actual_fields) {
                self.constrain_declared_flow(
                    declared,
                    actual,
                    substitutions,
                    access_arguments,
                    context,
                )?;
            }
            return Ok(());
        }
        let expected = self.flow_for_declared_type(declared, substitutions, access_arguments)?;
        actual.verify_directed(&expected, context)
    }

    fn variant_for(&self, constructor: &str, value_type: &Type) -> Option<&(String, Variant)> {
        let candidates = self.variants.get(constructor)?;
        let owner = match value_type.unqualified() {
            Type::Named(name, _) => Some(name),
            _ => None,
        };
        owner
            .and_then(|owner| candidates.iter().find(|(name, _)| name == owner))
            .or_else(|| (candidates.len() == 1).then(|| &candidates[0]))
    }

    fn builtin_constructor_flow(
        constructor: &str,
        expression_type: &Type,
        arguments: &[AccessFlow],
    ) -> Option<AccessFlow> {
        let Type::Named(type_name, type_arguments) = expression_type.unqualified() else {
            return None;
        };
        let constructor = constructor.rsplit('.').next().unwrap_or(constructor);
        let type_leaf = type_name.rsplit('.').next().unwrap_or(type_name);
        let access_arguments = match (type_leaf, constructor, type_arguments.len(), arguments) {
            ("Option", "Some", 1, [value]) => vec![value.clone()],
            ("Option", "None", 1, []) => vec![AccessFlow::Unknown],
            ("Result", "Ok", 2, [value]) => vec![value.clone(), AccessFlow::Unknown],
            ("Result", "Err", 2, [error]) => vec![AccessFlow::Unknown, error.clone()],
            _ => return None,
        };
        Some(AccessFlow::Named {
            name: type_name.clone(),
            arguments: access_arguments,
            dynamic: false,
        })
    }

    fn builtin_pattern_fields(
        constructor: &str,
        value: &AccessFlow,
        value_type: Option<&Type>,
    ) -> Option<Vec<(AccessFlow, Option<Type>)>> {
        let AccessFlow::Named {
            name: type_name,
            arguments: access_arguments,
            dynamic: false,
        } = value
        else {
            return None;
        };
        let constructor = constructor.rsplit('.').next().unwrap_or(constructor);
        let type_leaf = type_name.rsplit('.').next().unwrap_or(type_name);
        let type_arguments = value_type.and_then(|value_type| match value_type.unqualified() {
            Type::Named(name, arguments) if name == type_name => Some(arguments.as_slice()),
            _ => None,
        });
        let field_indexes: &[usize] = match (type_leaf, constructor, access_arguments.len()) {
            ("Option", "Some", 1) => &[0],
            ("Option", "None", 1) => &[],
            ("Result", "Ok", 2) => &[0],
            ("Result", "Err", 2) => &[1],
            _ => return None,
        };
        Some(
            field_indexes
                .iter()
                .map(|&index| {
                    let ty = type_arguments
                        .and_then(|arguments| arguments.get(index))
                        .cloned();
                    let flow = access_arguments
                        .get(index)
                        .cloned()
                        .unwrap_or(AccessFlow::Unknown);
                    (flow, ty)
                })
                .collect(),
        )
    }

    fn constructor_flow(
        &self,
        constructor: &str,
        expression_type: &Type,
        arguments: &[AccessFlow],
    ) -> Result<AccessFlow, AccessFlowError> {
        let Some((type_name, variant)) = self.variant_for(constructor, expression_type) else {
            if let Some(flow) =
                Self::builtin_constructor_flow(constructor, expression_type, arguments)
            {
                return Ok(flow);
            }
            return self.flow_from_type(expression_type).map_err(Self::signature_error);
        };
        let substitutions = self.nominal_substitutions(type_name, expression_type);
        let mut access_arguments = effective_type_def_params(&self.types[type_name])
            .into_iter()
            .map(|name| (name, AccessFlow::Unknown))
            .collect::<HashMap<_, _>>();
        for (index, (declared, actual)) in variant.fields.iter().zip(arguments).enumerate() {
            self.constrain_declared_flow(
                declared,
                actual,
                &substitutions,
                &mut access_arguments,
                &format!("field {} of constructor `{constructor}`", index + 1),
            )?;
        }
        let arguments = effective_type_def_params(&self.types[type_name])
            .into_iter()
            .map(|name| access_arguments.remove(&name).unwrap_or(AccessFlow::Unknown))
            .collect();
        Ok(AccessFlow::Named {
            name: type_name.clone(),
            arguments,
            dynamic: false,
        })
    }

    fn field_flow(
        &self,
        base_type: &Type,
        base_flow: &AccessFlow,
        field: &str,
    ) -> Result<AccessFlow, AccessFlowError> {
        let Type::Named(name, _) = base_type.unqualified() else { return Ok(AccessFlow::None) };
        let Some(definition) = self.types.get(name) else { return Ok(AccessFlow::None) };
        let Some((variant, index)) = definition
            .variants
            .iter()
            .find_map(|variant| {
                variant
                    .field_names
                    .iter()
                    .position(|found| found == field)
                    .map(|index| (variant, index))
            })
        else {
            return Ok(AccessFlow::None);
        };
        let substitutions = self.nominal_substitutions(name, base_type);
        let access_arguments = self.nominal_access_arguments(name, base_flow);
        self.flow_for_declared_type(&variant.fields[index], &substitutions, &access_arguments)
    }

    fn record_flow(
        &mut self,
        name: &str,
        fields: &[(String, Expr)],
        spread: Option<&Expr>,
        expression_type: &Type,
        environment: &mut FlowEnvironment,
        return_expected: &AccessFlow,
    ) -> Result<AccessFlow, AccessFlowError> {
        let spread_flow = spread
            .map(|spread| self.eval_expr(spread, environment, return_expected))
            .transpose()?;
        let Type::Named(type_name, _) = expression_type.unqualified() else {
            for (_, value) in fields {
                self.eval_expr(value, environment, return_expected)?;
            }
            return self.flow_from_type(expression_type).map_err(Self::signature_error);
        };
        let Some(definition) = self.types.get(type_name).cloned() else {
            for (_, value) in fields {
                self.eval_expr(value, environment, return_expected)?;
            }
            return self.flow_from_type(expression_type).map_err(Self::signature_error);
        };
        let substitutions = self.nominal_substitutions(type_name, expression_type);
        let mut access_arguments = spread_flow
            .as_ref()
            .map(|flow| self.nominal_access_arguments(type_name, flow))
            .unwrap_or_default();
        for parameter in effective_type_def_params(&definition) {
            access_arguments.entry(parameter).or_insert(AccessFlow::Unknown);
        }
        for (field, value) in fields {
            let actual = self.eval_expr(value, environment, return_expected)?;
            let Some(declared) = definition.variants.iter().find_map(|variant| {
                variant
                    .field_names
                    .iter()
                    .position(|found| found == field)
                    .and_then(|index| variant.fields.get(index))
            }) else {
                continue;
            };
            self.constrain_declared_flow(
                declared,
                &actual,
                &substitutions,
                &mut access_arguments,
                &format!("record field `{field}` of `{name}`"),
            )?;
        }
        let arguments = effective_type_def_params(&definition)
            .into_iter()
            .map(|parameter| {
                access_arguments.remove(&parameter).unwrap_or(AccessFlow::Unknown)
            })
            .collect();
        Ok(AccessFlow::Named {
            name: type_name.clone(),
            arguments,
            dynamic: false,
        })
    }

    fn existential_signature(
        &self,
        receiver: &Type,
        params: &[Type],
        checked_result: &Type,
        conventions: &[Convention],
    ) -> Result<AccessSignature, AccessFlowError> {
        let mut params = params.to_vec();
        params.insert(0, receiver.clone());
        AccessSignature::from_parts_with_catalog(
            params,
            checked_result.clone(),
            conventions.to_vec(),
            &self.borrow_catalog,
        )
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
            let expected = self.flow_from_type(parameter.ty()).map_err(Self::signature_error)?;
            argument.verify_directed(
                &expected,
                &format!("argument {} passed to `{name}`", index + 1),
            )?;
        }
        Ok(())
    }

    fn collect_type_substitutions(
        declared: &Type,
        actual: &Type,
        substitutions: &mut HashMap<String, Type>,
    ) {
        if let Type::Qualified(_, inner) = declared {
            Self::collect_type_substitutions(inner, actual.unqualified(), substitutions);
            return;
        }
        if let Type::Named(name, arguments) = declared
            && arguments.is_empty()
            && !name.contains('.')
            && name.chars().next().is_some_and(char::is_lowercase)
        {
            substitutions.entry(name.clone()).or_insert_with(|| actual.clone());
            return;
        }
        match (declared.unqualified(), actual.unqualified()) {
            (Type::Named(declared_name, declared), Type::Named(actual_name, actual))
            | (Type::Dyn(declared_name, declared), Type::Dyn(actual_name, actual))
                if declared_name == actual_name && declared.len() == actual.len() =>
            {
                for (declared, actual) in declared.iter().zip(actual) {
                    Self::collect_type_substitutions(declared, actual, substitutions);
                }
            }
            (Type::Tuple(declared), Type::Tuple(actual)) if declared.len() == actual.len() => {
                for (declared, actual) in declared.iter().zip(actual) {
                    Self::collect_type_substitutions(declared, actual, substitutions);
                }
            }
            (Type::Fn(declared_params, declared_result, _), Type::Fn(actual_params, actual_result, _))
                if declared_params.len() == actual_params.len() =>
            {
                for (declared, actual) in declared_params.iter().zip(actual_params) {
                    Self::collect_type_substitutions(declared, actual, substitutions);
                }
                Self::collect_type_substitutions(declared_result, actual_result, substitutions);
            }
            _ => {}
        }
    }

    fn collect_flow_substitutions(
        declared: &Type,
        actual: &AccessFlow,
        substitutions: &mut HashMap<String, Type>,
    ) {
        if let Type::Named(name, arguments) = declared
            && arguments.is_empty()
            && !name.contains('.')
            && name.chars().next().is_some_and(char::is_lowercase)
            && let AccessFlow::Callable(signature) = actual
        {
            substitutions.entry(name.clone()).or_insert_with(|| signature.as_type());
            return;
        }
        match (declared.unqualified(), actual) {
            (Type::Fn(_, _, _), AccessFlow::Callable(signature)) => {
                Self::collect_type_substitutions(declared, &signature.as_type(), substitutions);
            }
            (Type::Tuple(declared), AccessFlow::Product(actual))
                if declared.len() == actual.len() =>
            {
                for (declared, actual) in declared.iter().zip(actual) {
                    Self::collect_flow_substitutions(declared, actual, substitutions);
                }
            }
            (Type::Named(_, declared), AccessFlow::Sequence(actual)) if declared.len() == 1 => {
                Self::collect_flow_substitutions(&declared[0], actual, substitutions);
            }
            (
                Type::Named(declared_name, declared),
                AccessFlow::Named {
                    name: actual_name,
                    arguments: actual,
                    dynamic: false,
                },
            ) if declared_name == actual_name && declared.len() == actual.len() => {
                for (declared, actual) in declared.iter().zip(actual) {
                    Self::collect_flow_substitutions(declared, actual, substitutions);
                }
            }
            (
                Type::Dyn(declared_name, declared),
                AccessFlow::Named {
                    name: actual_name,
                    arguments: actual,
                    dynamic: true,
                },
            ) if declared_name == actual_name && declared.len() == actual.len() => {
                for (declared, actual) in declared.iter().zip(actual) {
                    Self::collect_flow_substitutions(declared, actual, substitutions);
                }
            }
            _ => {}
        }
    }

    fn specialize_signature_from_flows(
        &self,
        signature: AccessSignature,
        arguments: &[AccessFlow],
        argument_types: &[Option<Type>],
        result_type: Option<&Type>,
    ) -> Result<(AccessSignature, HashMap<String, Type>), AccessFlowError> {
        let mut substitutions = HashMap::new();
        for (index, parameter) in signature.params().iter().enumerate() {
            if let Some(argument) = arguments.get(index) {
                // A checked value flow retains callable qualifiers that the
                // finalized expression type intentionally erases. Let that
                // access identity bind the callee variable first, then use the
                // checked type only to fill variables the flow cannot represent.
                Self::collect_flow_substitutions(
                    parameter.ty(),
                    argument,
                    &mut substitutions,
                );
                if let Some(actual_type) = argument_types.get(index).and_then(Option::as_ref) {
                    let actual_type = argument.materialize_type(actual_type);
                    Self::collect_type_substitutions(
                        parameter.ty(),
                        &actual_type,
                        &mut substitutions,
                    );
                }
            } else if let Some(actual_type) = argument_types.get(index).and_then(Option::as_ref) {
                Self::collect_type_substitutions(
                    parameter.ty(),
                    actual_type,
                    &mut substitutions,
                );
            }
        }
        if let Some(result_type) = result_type {
            Self::collect_type_substitutions(
                signature.result().ty(),
                result_type,
                &mut substitutions,
            );
        }
        if substitutions.is_empty() {
            return Ok((signature, substitutions));
        }
        let specialized = Self::substitute_type(&signature.as_type(), &substitutions);
        let signature = AccessSignature::from_function_type_with_catalog(
            &specialized,
            &self.borrow_catalog,
        )
        .map_err(Self::signature_error)?;
        Ok((signature, substitutions))
    }

    fn eval_arguments_with_dependent_hints(
        &mut self,
        expressions: &[&Expr],
        signature: Option<&AccessSignature>,
        prefix: &[AccessFlow],
        types: CallTypeContext<'_>,
        environment: &mut FlowEnvironment,
        return_expected: &AccessFlow,
    ) -> Result<Vec<AccessFlow>, AccessFlowError> {
        let mut evaluated = prefix.to_vec();
        let mut arguments = Vec::with_capacity(expressions.len());
        for expression in expressions {
            let parameter_index = evaluated.len();
            let hint = signature
                .cloned()
                .map(|signature| {
                    self.specialize_signature_from_flows(
                        signature,
                        &evaluated,
                        types.arguments,
                        types.result,
                    )
                    .map(|(signature, _)| signature)
                })
                .transpose()?;
            let argument = self.eval_expr_with_hint(
                expression,
                hint.as_ref()
                    .and_then(|signature| signature.params().get(parameter_index))
                    .map(AccessParam::ty),
                environment,
                return_expected,
            )?;
            evaluated.push(argument.clone());
            arguments.push(argument);
        }
        Ok(arguments)
    }

    fn eval_block(
        &mut self,
        block: &Block,
        environment: &mut FlowEnvironment,
        return_expected: &AccessFlow,
    ) -> Result<AccessFlow, AccessFlowError> {
        let mut tail = AccessFlow::None;
        let return_hint = match return_expected {
            AccessFlow::Callable(signature) => Some(signature.as_type()),
            _ => None,
        };
        for (index, statement) in block.stmts.iter().enumerate() {
            tail = match statement {
                Stmt::Let { name, ty, value, .. } => {
                    let actual =
                        self.eval_expr_with_hint(value, ty.as_ref(), environment, return_expected)?;
                    let value = if let Some(ty) = ty {
                        let expected = self.flow_from_type(ty).map_err(Self::signature_error)?;
                        actual.verify_directed(&expected, &format!("function value `{name}`"))?;
                        expected
                    } else {
                        actual
                    };
                    environment.insert(name.clone(), value);
                    AccessFlow::None
                }
                Stmt::Assign { name, value } => {
                    let hint = match environment.get(name) {
                        Some(AccessFlow::Callable(signature)) => Some(signature.as_type()),
                        _ => None,
                    };
                    let actual = self.eval_expr_with_hint(
                        value,
                        hint.as_ref(),
                        environment,
                        return_expected,
                    )?;
                    if let Some(expected) = environment.get(name) {
                        actual.verify_directed(expected, &format!("assignment to `{name}`"))?;
                    }
                    environment.insert(name.clone(), actual);
                    AccessFlow::None
                }
                Stmt::LetPattern { pattern, value } => {
                    let value_type = self.resolved_expression_type(value);
                    let value = self.eval_expr(value, environment, return_expected)?;
                    self.bind_pattern(pattern, &value, value_type.as_ref(), environment)?;
                    AccessFlow::None
                }
                Stmt::Return(value) => {
                    let actual = match value {
                        Some(value) => self.eval_expr_with_hint(
                            value,
                            return_hint.as_ref(),
                            environment,
                            return_expected,
                        )?,
                        None => AccessFlow::None,
                    };
                    if let Some(frame) = self.return_frames.last_mut() {
                        frame.push(actual.clone());
                    }
                    actual.verify_directed(return_expected, "returned function value")?;
                    AccessFlow::None
                }
                Stmt::Yield(value) => self.eval_expr(value, environment, return_expected)?,
                Stmt::Expr(value) => self.eval_expr_with_hint(
                    value,
                    (index + 1 == block.stmts.len())
                        .then_some(return_hint.as_ref())
                        .flatten(),
                    environment,
                    return_expected,
                )?,
                Stmt::Break | Stmt::Continue => AccessFlow::None,
            };
            if index + 1 != block.stmts.len() {
                tail = AccessFlow::None;
            }
        }
        Ok(tail)
    }

    fn bind_pattern(
        &self,
        pattern: &Pattern,
        value: &AccessFlow,
        value_type: Option<&Type>,
        environment: &mut FlowEnvironment,
    ) -> Result<(), AccessFlowError> {
        match pattern {
            Pattern::Var(name) if name != "_" => {
                environment.insert(name.clone(), value.clone());
            }
            Pattern::Tuple(args) => {
                let field_types = value_type.and_then(|ty| match ty.unqualified() {
                    Type::Tuple(fields) => Some(fields.as_slice()),
                    _ => None,
                });
                for (index, pattern) in args.iter().enumerate() {
                    let flow = match value {
                        AccessFlow::Product(children) => {
                            children.get(index).cloned().unwrap_or(AccessFlow::None)
                        }
                        _ => field_types
                            .and_then(|fields| fields.get(index))
                            .map(|field| self.flow_from_type(field))
                            .transpose()
                            .map_err(Self::signature_error)?
                            .unwrap_or(AccessFlow::None),
                    };
                    self.bind_pattern(
                        pattern,
                        &flow,
                        field_types.and_then(|fields| fields.get(index)),
                        environment,
                    )?;
                }
            }
            Pattern::Ctor { name: constructor, args }
            | Pattern::AnonCtor { tag: constructor, args } => {
                if let Some(value_type) = value_type
                    && let Some((type_name, variant)) = self.variant_for(constructor, value_type)
                {
                    let substitutions = self.nominal_substitutions(type_name, value_type);
                    let access_arguments = self.nominal_access_arguments(type_name, value);
                    for (index, pattern) in args.iter().enumerate() {
                        let Some(declared) = variant.fields.get(index) else { continue };
                        let flow = self.flow_for_declared_type(
                            declared,
                            &substitutions,
                            &access_arguments,
                        )?;
                        let field_type = Self::substitute_type(declared, &substitutions);
                        self.bind_pattern(pattern, &flow, Some(&field_type), environment)?;
                    }
                } else if let Some(fields) =
                    Self::builtin_pattern_fields(constructor, value, value_type)
                {
                    for (pattern, (flow, field_type)) in args.iter().zip(fields) {
                        self.bind_pattern(pattern, &flow, field_type.as_ref(), environment)?;
                    }
                }
            }
            Pattern::List { elems, rest } => {
                let element = match value {
                    AccessFlow::Sequence(element) => element.as_ref().clone(),
                    AccessFlow::Named { arguments, dynamic: false, .. }
                        if arguments.len() == 1 => arguments[0].clone(),
                    _ => AccessFlow::Unknown,
                };
                let element_type = value_type.and_then(|ty| match ty.unqualified() {
                    Type::Named(_, arguments) if arguments.len() == 1 => arguments.first(),
                    _ => None,
                });
                for pattern in elems {
                    self.bind_pattern(pattern, &element, element_type, environment)?;
                }
                if let Some(Some(name)) = rest {
                    environment.insert(name.clone(), value.clone());
                }
            }
            Pattern::Or(alternatives) => {
                if let Some(first) = alternatives.first() {
                    self.bind_pattern(first, value, value_type, environment)?;
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
        Ok(())
    }

    fn eval_arm(
        &mut self,
        arm: &MatchArm,
        scrutinee: &AccessFlow,
        scrutinee_type: Option<&Type>,
        environment: &FlowEnvironment,
        return_expected: &AccessFlow,
    ) -> Result<(AccessFlow, FlowEnvironment), AccessFlowError> {
        let mut environment = environment.clone();
        self.bind_pattern(&arm.pattern, scrutinee, scrutinee_type, &mut environment)?;
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

    fn record_call(&mut self, expression: &Expr, signature: &AccessSignature) {
        self.facts
            .calls
            .insert(expression as *const Expr as usize, signature.clone());
    }

    fn eval_expr_with_hint(
        &mut self,
        expression: &Expr,
        hint: Option<&Type>,
        environment: &mut FlowEnvironment,
        return_expected: &AccessFlow,
    ) -> Result<AccessFlow, AccessFlowError> {
        let key = expression as *const Expr as usize;
        let previous = hint.and_then(|hint| self.expression_type_hints.insert(key, hint.clone()));
        let result = self.eval_expr(expression, environment, return_expected);
        if let Some(previous) = previous {
            self.expression_type_hints.insert(key, previous);
        } else if hint.is_some() {
            self.expression_type_hints.remove(&key);
        }
        result
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
            Expr::List(values) => {
                let element_hint = self
                    .expression_type_hints
                    .get(&(expression as *const Expr as usize))
                    .and_then(|hint| match hint.unqualified() {
                        Type::Named(_, arguments) if arguments.len() == 1 => {
                            arguments.first().cloned()
                        }
                        _ => None,
                    });
                let elements = values
                    .iter()
                    .map(|value| {
                        self.eval_expr_with_hint(
                            value,
                            element_hint.as_ref(),
                            environment,
                            return_expected,
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let element = elements.into_iter().try_fold(
                    AccessFlow::Unknown,
                    |left, right| left.join(&right, "list element access contracts"),
                )?;
                if matches!(element, AccessFlow::None) {
                    AccessFlow::None
                } else {
                    AccessFlow::Sequence(Box::new(element))
                }
            }
            Expr::Tuple(values) => {
                let field_hints = self
                    .expression_type_hints
                    .get(&(expression as *const Expr as usize))
                    .and_then(|hint| match hint.unqualified() {
                        Type::Tuple(fields) if fields.len() == values.len() => Some(fields.clone()),
                        _ => None,
                    })
                    .unwrap_or_default();
                AccessFlow::product(
                    values
                        .iter()
                        .enumerate()
                        .map(|(index, value)| {
                            self.eval_expr_with_hint(
                                value,
                                field_hints.get(index),
                                environment,
                                return_expected,
                            )
                        })
                        .collect::<Result<Vec<_>, _>>()?,
                )
            }
            Expr::Call { name, args } => {
                let positional = args.iter().collect::<Vec<_>>();
                let signature = match environment.get(name) {
                    Some(AccessFlow::Callable(signature)) => Some(signature.clone()),
                    _ => self.signature_for_named_call(name, expression, &positional)?,
                };
                let argument_types = args
                    .iter()
                    .map(|argument| self.resolved_expression_type(argument))
                    .collect::<Vec<_>>();
                let result_type = self.contextual_expression_type(expression);
                let arguments = self.eval_arguments_with_dependent_hints(
                    &positional,
                    signature.as_ref(),
                    &[],
                    CallTypeContext {
                        arguments: &argument_types,
                        result: result_type.as_ref(),
                    },
                    environment,
                    return_expected,
                )?;
                match signature {
                    Some(signature) => {
                        let (signature, _) = self.specialize_signature_from_flows(
                            signature,
                            &arguments,
                            &argument_types,
                            result_type.as_ref(),
                        )?;
                        self.verify_arguments(name, &signature, &arguments)?;
                        self.record_call(expression, &signature);
                        self.flow_from_type(signature.result().ty())
                            .map_err(Self::signature_error)?
                    }
                    None => AccessFlow::None,
                }
            }
            Expr::LabeledCall { name, args } => {
                let positional = args.iter().map(|(_, argument)| argument).collect::<Vec<_>>();
                let signature = self.signature_for_named_call(name, expression, &positional)?;
                let argument_types = positional
                    .iter()
                    .map(|argument| self.resolved_expression_type(argument))
                    .collect::<Vec<_>>();
                let result_type = self.contextual_expression_type(expression);
                let arguments = self.eval_arguments_with_dependent_hints(
                    &positional,
                    signature.as_ref(),
                    &[],
                    CallTypeContext {
                        arguments: &argument_types,
                        result: result_type.as_ref(),
                    },
                    environment,
                    return_expected,
                )?;
                match signature {
                    Some(signature) => {
                        let (signature, _) = self.specialize_signature_from_flows(
                            signature,
                            &arguments,
                            &argument_types,
                            result_type.as_ref(),
                        )?;
                        self.verify_arguments(name, &signature, &arguments)?;
                        self.record_call(expression, &signature);
                        self.flow_from_type(signature.result().ty())
                            .map_err(Self::signature_error)?
                    }
                    None => AccessFlow::None,
                }
            }
            Expr::Apply { func, args } => {
                let function = self.eval_expr(func, environment, return_expected)?;
                let signature = match function {
                    AccessFlow::Callable(signature) => Some(signature),
                    _ => None,
                };
                let argument_types = args
                    .iter()
                    .map(|argument| self.resolved_expression_type(argument))
                    .collect::<Vec<_>>();
                let result_type = self.contextual_expression_type(expression);
                let positional = args.iter().collect::<Vec<_>>();
                let arguments = self.eval_arguments_with_dependent_hints(
                    &positional,
                    signature.as_ref(),
                    &[],
                    CallTypeContext {
                        arguments: &argument_types,
                        result: result_type.as_ref(),
                    },
                    environment,
                    return_expected,
                )?;
                let Some(signature) = signature else {
                    return Ok(self.record(expression, AccessFlow::None));
                };
                let (signature, _) = self.specialize_signature_from_flows(
                    signature,
                    &arguments,
                    &argument_types,
                    result_type.as_ref(),
                )?;
                self.verify_arguments("indirect function", &signature, &arguments)?;
                self.record_call(expression, &signature);
                self.flow_from_type(signature.result().ty()).map_err(Self::signature_error)?
            }
            Expr::Ctor { name, args } | Expr::AnonCtor { tag: name, args } => {
                let expression_type = self.resolved_expression_type(expression).unwrap_or_else(|| {
                    self.variants
                        .get(name)
                        .and_then(|candidates| candidates.first())
                        .map(|(type_name, _)| Type::Named(type_name.clone(), Vec::new()))
                        .unwrap_or_else(|| Type::Named(name.clone(), Vec::new()))
                });
                let field_hints = self
                    .variant_for(name, &expression_type)
                    .map(|(type_name, variant)| {
                        let substitutions = self.nominal_substitutions(type_name, &expression_type);
                        variant
                            .fields
                            .iter()
                            .map(|field| Self::substitute_type(field, &substitutions))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                let arguments = args
                    .iter()
                    .enumerate()
                    .map(|(index, argument)| {
                        self.eval_expr_with_hint(
                            argument,
                            field_hints.get(index),
                            environment,
                            return_expected,
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                self.constructor_flow(name, &expression_type, &arguments)?
            }
            Expr::Unary { expr, .. } => {
                self.eval_expr(expr, environment, return_expected)?
            }
            Expr::Try(expr) => {
                let operand = self.eval_expr(expr, environment, return_expected)?;
                match operand {
                    AccessFlow::Named {
                        name,
                        arguments,
                        dynamic: false,
                    } if matches!(name.rsplit('.').next(), Some("Option" | "Result")) => {
                        arguments.into_iter().next().unwrap_or(AccessFlow::None)
                    }
                    other => other,
                }
            }
            Expr::ExistentialPack { expr, ty, .. }
            | Expr::ExistentialUpcast { expr, ty } => {
                self.eval_expr(expr, environment, return_expected)?;
                self.flow_from_type(ty).map_err(Self::signature_error)?
            }
            Expr::Field { base, field } => {
                let base_flow = self.eval_expr(base, environment, return_expected)?;
                match self.resolved_expression_type(base) {
                    Some(base_type) => self.field_flow(&base_type, &base_flow, field)?,
                    None => AccessFlow::None,
                }
            }
            Expr::Lambda { params, body, ret } => {
                let resolved = self
                    .expression_type_hints
                    .get(&(expression as *const Expr as usize))
                    .cloned()
                    .or_else(|| self.resolved_expression_type(expression))
                    .ok_or_else(|| AccessFlowError {
                        message: "lambda has no finalized checked type or checked expression context"
                            .into(),
                    })?;
                let conventions: Vec<Convention> =
                    params.iter().map(|parameter| parameter.convention).collect();
                let mut signature = AccessSignature::from_resolved_parts_with_catalog(
                    params.iter().map(|parameter| parameter.ty.as_ref()).collect(),
                    ret.as_ref(),
                    conventions.clone(),
                    &resolved,
                    &self.borrow_catalog,
                )
                .map_err(Self::signature_error)?;
                let mut lambda_environment = environment.clone();
                for (parameter, access) in params.iter().zip(signature.params()) {
                    lambda_environment.insert(
                        parameter.name.clone(),
                        self.flow_from_type(access.ty()).map_err(Self::signature_error)?,
                    );
                }
                let preliminary_expected = if ret.is_none() {
                    AccessFlow::Unknown
                } else {
                    self.flow_from_type(signature.result().ty())
                        .map_err(Self::signature_error)?
                };
                self.return_frames.push(Vec::new());
                let tail =
                    self.eval_block(body, &mut lambda_environment, &preliminary_expected)?;
                let mut returned = self.return_frames.pop().unwrap_or_default();
                if ret.is_none() {
                    if matches!(body.stmts.last(), Some(Stmt::Expr(_))) {
                        returned.push(tail.clone());
                    }
                    let inferred = Self::join_flows(
                        returned.iter().cloned(),
                        "inferred lambda result paths",
                    )?;
                    let inferred_result = inferred.materialize_type(signature.result().ty());
                    let callable_qualifiers = signature.callable_qualifiers.clone();
                    signature = AccessSignature::from_parts_with_catalog(
                        signature
                            .params()
                            .iter()
                            .map(|parameter| parameter.ty().clone())
                            .collect(),
                        inferred_result,
                        conventions,
                        &self.borrow_catalog,
                    )
                    .map_err(Self::signature_error)?;
                    signature.callable_qualifiers = callable_qualifiers;
                }
                let expected = self.flow_from_type(signature.result().ty())
                    .map_err(Self::signature_error)?;
                for returned in &returned {
                    returned.verify_directed(&expected, "lambda return")?;
                }
                if matches!(body.stmts.last(), Some(Stmt::Expr(_))) {
                    tail.verify_directed(&expected, "lambda result")?;
                }
                AccessFlow::Callable(signature)
            }
            Expr::As { expr, ty } => {
                let actual = self.eval_expr(expr, environment, return_expected)?;
                let expected = self.flow_from_type(ty).map_err(Self::signature_error)?;
                actual.verify_directed(&expected, "function cast")?;
                expected
            }
            Expr::RecordUpdate { name, base, fields } => {
                let base_type = self.resolved_expression_type(base);
                let Some(base_type) = base_type else {
                    self.eval_expr(base, environment, return_expected)?;
                    for (_, value) in fields {
                        self.eval_expr(value, environment, return_expected)?;
                    }
                    return Ok(self.record(expression, AccessFlow::None));
                };
                let record_name = name
                    .as_deref()
                    .or_else(|| match base_type.unqualified() {
                        Type::Named(name, _) => Some(name.as_str()),
                        _ => None,
                    })
                    .unwrap_or("record");
                self.record_flow(
                    record_name,
                    fields,
                    Some(base),
                    &base_type,
                    environment,
                    return_expected,
                )?
            }
            Expr::Record { name, fields, spread } => {
                let expression_type = self
                    .resolved_expression_type(expression)
                    .unwrap_or_else(|| Type::Named(name.clone(), Vec::new()));
                self.record_flow(
                    name,
                    fields,
                    spread.as_deref(),
                    &expression_type,
                    environment,
                    return_expected,
                )?
            }
            Expr::ExistentialCall {
                receiver,
                args,
                ty,
                method,
                params,
                result,
                conventions,
                ..
            } => {
                let receiver_flow = self.eval_expr(receiver, environment, return_expected)?;
                let signature = self.existential_signature(ty, params, result, conventions)?;
                let mut argument_types = Vec::with_capacity(args.len() + 1);
                argument_types.push(self.resolved_expression_type(receiver));
                argument_types.extend(
                    args.iter()
                        .map(|argument| self.resolved_expression_type(argument)),
                );
                let result_type = self.contextual_expression_type(expression);
                let positional = args.iter().collect::<Vec<_>>();
                let arguments = self.eval_arguments_with_dependent_hints(
                    &positional,
                    Some(&signature),
                    std::slice::from_ref(&receiver_flow),
                    CallTypeContext {
                        arguments: &argument_types,
                        result: result_type.as_ref(),
                    },
                    environment,
                    return_expected,
                )?;
                let mut all_arguments = Vec::with_capacity(arguments.len() + 1);
                all_arguments.push(receiver_flow.clone());
                all_arguments.extend(arguments.iter().cloned());
                let (signature, _) = self.specialize_signature_from_flows(
                    signature,
                    &all_arguments,
                    &argument_types,
                    self.contextual_expression_type(expression).as_ref(),
                )?;
                if let Some(receiver_param) = signature.params().first() {
                    let expected = self.flow_from_type(receiver_param.ty())
                        .map_err(Self::signature_error)?;
                    receiver_flow.verify_directed(
                        &expected,
                        &format!("receiver passed to dynamic method `{method}`"),
                    )?;
                }
                for (index, (actual, expected)) in arguments
                    .iter()
                    .zip(signature.params().iter().skip(1))
                    .enumerate()
                {
                    let expected = self.flow_from_type(expected.ty())
                        .map_err(Self::signature_error)?;
                    actual.verify_directed(
                        &expected,
                        &format!("argument {} passed to dynamic method `{method}`", index + 1),
                    )?;
                }
                self.record_call(expression, &signature);
                self.flow_from_type(signature.result().ty()).map_err(Self::signature_error)?
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
                let scrutinee_type = self.resolved_expression_type(scrutinee);
                let scrutinee = self.eval_expr(scrutinee, environment, return_expected)?;
                let branches = arms
                    .iter()
                    .map(|arm| {
                        self.eval_arm(
                            arm,
                            &scrutinee,
                            scrutinee_type.as_ref(),
                            environment,
                            return_expected,
                        )
                    })
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
                let iter_type = self.resolved_expression_type(iter);
                let iter_flow = self.eval_expr(iter, environment, return_expected)?;
                let element = match &iter_flow {
                    AccessFlow::Sequence(element) => element.as_ref().clone(),
                    AccessFlow::Named { arguments, dynamic: false, .. }
                        if arguments.len() == 1 => arguments[0].clone(),
                    _ => iter_type
                        .as_ref()
                        .and_then(|ty| match ty.unqualified() {
                            Type::Named(_, arguments) if arguments.len() == 1 => arguments.first(),
                            _ => None,
                        })
                        .map(|element| self.flow_from_type(element))
                        .transpose()
                        .map_err(Self::signature_error)?
                        .unwrap_or(AccessFlow::None),
                };
                let mut body_environment = environment.clone();
                body_environment.insert(var.clone(), element);
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
                match &base {
                    AccessFlow::Sequence(element) => element.as_ref().clone(),
                    AccessFlow::Named { arguments, dynamic: false, .. }
                        if arguments.len() == 1 => arguments[0].clone(),
                    _ => self
                        .resolved_expression_type(expression)
                        .map(|ty| self.flow_from_type(&ty))
                        .transpose()
                        .map_err(Self::signature_error)?
                        .unwrap_or(AccessFlow::None),
                }
            }
            Expr::WhileLet { pattern, scrutinee, body } => {
                let scrutinee_type = self.resolved_expression_type(scrutinee);
                let scrutinee = self.eval_expr(scrutinee, environment, return_expected)?;
                let mut body_environment = environment.clone();
                self.bind_pattern(
                    pattern,
                    &scrutinee,
                    scrutinee_type.as_ref(),
                    &mut body_environment,
                )?;
                self.eval_block(body, &mut body_environment, return_expected)?;
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
pub fn checked_facts<'module>(
    module: &'module Module,
    table: &'module TypeTable,
) -> Result<CheckedAccessFacts<'module>, AccessFlowError> {
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
