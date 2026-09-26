//! One typed source-storage classification shared across the checked frontend
//! and SafeWIR. A plan describes storage and ownership operations; it is not
//! authority without a checked-source witness and physical layout match.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use witchy_syntax::ast::{Item, MatchArm, Module, Pattern, Type, TypeDef, TypeQual};

use crate::abi::CanonicalScalarKind;
use crate::aggregates::{ScalarAggregateShape, ViewCarrierPlan, instantiated_declared_fields, scalar_aggregate_shape, view_carrier_plan};
use crate::carrier::CarrierElement;
use crate::callable_storage::CallableStoragePlan;
use crate::closure::ClosureWitnessManifest;
use crate::rooted_view::{OwnerCarriedViewPlan, OwnerCarriedViewStorage, RetainedRootStorage, RootedViewStorage};
use crate::structural_identity::SourceTypeKey;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageRefusal {
    ResourceLimit,
    UnprovedView,
    UnprovedQualifier,
    UnprovedAuthority,
    UnprovedOwnership,
}

/// Why a retained snapshot of instantiated storage cannot be proven. Type
/// arguments participate in identity, but only stored fields participate in
/// this ownership judgment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotRefusal {
    ResourceLimit,
    Authority,
    UniqueOwner,
    MustConsume,
    OnceCallable,
    UnprovedStorage,
}

pub fn snapshot_eligibility(
    ty: &Type,
    definitions: &BTreeMap<String, TypeDef>,
) -> Result<(), SnapshotRefusal> {
    snapshot_eligibility_for(ty, definitions, SnapshotUse::PersistentView)
}

/// A lazy recipe retains one copyable snapshot and clones invocation state on
/// every execution. Unlike a persistent view, its declared capability token
/// may be copied as delegated authority; affine storage still cannot replay.
pub fn replay_capture_eligibility(
    ty: &Type,
    definitions: &BTreeMap<String, TypeDef>,
) -> Result<(), SnapshotRefusal> {
    snapshot_eligibility_for(ty, definitions, SnapshotUse::ReplayableTask)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SnapshotUse { PersistentView, ReplayableTask }

/// Instantiated stored children, with collection construction distinguished
/// from declared aggregate fields. This describes source storage only; a
/// caller still needs its layout and ownership-operation proof.
enum StoredChildren {
    Sequence(Type),
    Mapping(Type, Type),
    Aggregate(Vec<Type>),
}

impl StoredChildren {
    fn into_types(self) -> Vec<Type> {
        match self {
            Self::Sequence(element) => vec![element],
            Self::Mapping(key, value) => vec![key, value],
            Self::Aggregate(fields) => fields,
        }
    }
}

/// Which generic arguments occupy source storage, and which nominal values
/// transitively carry a must-consume obligation. The checker uses this same
/// stored-child relation for must, once-callable, and rooted-view containment;
/// callable signatures and phantom arguments are never treated as fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredObligationCatalog {
    pub stored_parameters: BTreeMap<String, Vec<bool>>,
    pub must_types: BTreeSet<String>,
}

impl StoredObligationCatalog {
    /// `execution_obligations` names semantic values, such as lazy tasks, that
    /// require explicit disposition even when their declaration stores no
    /// resource-bearing type argument. Their arity is checked against any
    /// declaration before the catalogue is used by the frontend.
    pub fn from_module(module: &Module, execution_obligations: &[(&str, usize)]) -> Option<Self> {
        let definitions = module.items.iter().filter_map(|item| match item {
            Item::Type(definition) => Some(definition),
            _ => None,
        }).collect::<Vec<_>>();
        let source_definitions = crate::aggregates::type_definitions(module);
        let mut stored_parameters = definitions.iter().map(|definition| (
            definition.name.clone(),
            vec![false; witchy_syntax::ast::effective_type_def_params(definition).len()],
        )).collect::<BTreeMap<_, _>>();
        for &(name, arity) in execution_obligations {
            match stored_parameters.get(name) {
                Some(mask) if mask.len() != arity => return None,
                Some(_) => {},
                None => { stored_parameters.insert(name.to_owned(), vec![false; arity]); },
            }
        }
        loop {
            let before = stored_parameters.clone();
            for definition in &definitions {
                let parameters = witchy_syntax::ast::effective_type_def_params(definition);
                let mask = stored_parameters.get_mut(&definition.name)?;
                for (index, parameter) in parameters.iter().enumerate() {
                    mask[index] |= definition.variants.iter().flat_map(|variant| &variant.fields)
                        .any(|field| stores_parameter(field, parameter, &before, &source_definitions));
                }
            }
            if stored_parameters == before { break; }
        }
        let mut must_types = definitions.iter().filter(|definition| definition.must_consume)
            .map(|definition| definition.name.clone()).collect::<BTreeSet<_>>();
        must_types.extend(execution_obligations.iter().map(|(name, _)| (*name).to_owned()));
        loop {
            let before = must_types.len();
            for definition in &definitions {
                let local_parameters = witchy_syntax::ast::effective_type_def_params(definition)
                    .into_iter().collect::<BTreeSet<_>>();
                if definition.variants.iter().flat_map(|variant| &variant.fields).any(|field| {
                    carries_must(field, &must_types, &stored_parameters, &local_parameters, &source_definitions)
                }) {
                    must_types.insert(definition.name.clone());
                }
            }
            if must_types.len() == before { break; }
        }
        // The linker may expose the same declaration under an entry-qualified
        // name. Publish both spellings so checker consumers never fall back to
        // treating its type arguments as unknown storage.
        for (alias, definition) in &source_definitions {
            if let Some(mask) = stored_parameters.get(&definition.name).cloned() {
                stored_parameters.insert(alias.clone(), mask);
            }
            if must_types.contains(&definition.name) {
                must_types.insert(alias.clone());
            }
        }
        Some(Self { stored_parameters, must_types })
    }
}

fn catalog_mask<'a>(
    name: &str,
    positions: &'a BTreeMap<String, Vec<bool>>,
    definitions: &BTreeMap<String, TypeDef>,
) -> Option<&'a Vec<bool>> {
    positions.get(name).or_else(|| definitions.get(name)
        .and_then(|definition| positions.get(&definition.name)))
}

fn stored_arguments_for_catalog(
    ty: &Type,
    positions: &BTreeMap<String, Vec<bool>>,
    definitions: &BTreeMap<String, TypeDef>,
) -> Vec<Type> {
    match ty {
        Type::Qualified(_, inner) | Type::Slice(inner) => vec![inner.as_ref().clone()],
        Type::Named(name, arguments) => {
            if let Some(mask) = catalog_mask(name, positions, definitions) {
                return mask.iter().zip(arguments).filter_map(|(stored, argument)|
                    stored.then_some(argument.clone())).collect();
            }
            // Built-in sequences and mappings take their stored positions from
            // the same relation used by snapshot and layout classification.
            stored_children(ty, definitions).ok().flatten().map(StoredChildren::into_types)
                .unwrap_or_else(|| arguments.clone())
        }
        Type::Tuple(fields) => fields.clone(),
        Type::RecordCompose { base, fields } => std::iter::once(base.as_ref().clone())
            .chain(fields.iter().map(|(_, field)| field.clone())).collect(),
        Type::Dyn(..) | Type::Fn(..) => Vec::new(),
    }
}

enum StoredTypeStep { Found, Stop, Continue }

fn any_stored_type(
    ty: &Type,
    positions: &BTreeMap<String, Vec<bool>>,
    definitions: &BTreeMap<String, TypeDef>,
    test: &impl Fn(&Type) -> StoredTypeStep,
    depth: usize,
    remaining: &mut usize,
) -> bool {
    // The catalog grants copy/escape permission when it says an obligation is
    // absent. Exhausting its proof budget must therefore answer conservatively.
    if depth > 128 || *remaining == 0 { return true; }
    *remaining -= 1;
    match test(ty) {
        StoredTypeStep::Found => return true,
        StoredTypeStep::Stop => return false,
        StoredTypeStep::Continue => {},
    }
    if matches!(ty, Type::Named(name, _) if definitions.contains_key(name)
        && catalog_mask(name, positions, definitions).is_none()) {
        return true;
    }
    stored_arguments_for_catalog(ty, positions, definitions).iter().any(|child|
        any_stored_type(child, positions, definitions, test, depth + 1, remaining))
}

fn stores_parameter(
    ty: &Type,
    parameter: &str,
    positions: &BTreeMap<String, Vec<bool>>,
    definitions: &BTreeMap<String, TypeDef>,
) -> bool {
    let mut remaining = 16_384;
    any_stored_type(ty, positions, definitions, &|candidate| {
        if matches!(candidate, Type::Named(name, arguments) if name == parameter && arguments.is_empty()) {
            StoredTypeStep::Found
        } else {
            StoredTypeStep::Continue
        }
    }, 0, &mut remaining)
}

fn carries_must(
    ty: &Type,
    must_types: &BTreeSet<String>,
    positions: &BTreeMap<String, Vec<bool>>,
    local_parameters: &BTreeSet<String>,
    definitions: &BTreeMap<String, TypeDef>,
) -> bool {
    let mut remaining = 16_384;
    any_stored_type(ty, positions, definitions, &|candidate| {
        if let Type::Named(name, arguments) = candidate {
            if arguments.is_empty() && local_parameters.contains(name) { return StoredTypeStep::Stop; }
            if must_types.contains(name) || definitions.get(name)
                .is_some_and(|definition| must_types.contains(&definition.name)) {
                return StoredTypeStep::Found;
            }
        }
        StoredTypeStep::Continue
    }, 0, &mut remaining)
}

fn stored_children(
    ty: &Type,
    definitions: &BTreeMap<String, TypeDef>,
) -> Result<Option<StoredChildren>, ()> {
    match ty.unqualified() {
        Type::Tuple(fields) => Ok(Some(StoredChildren::Aggregate(fields.clone()))),
        Type::Named(name, arguments) if name == "List" => {
            let [element] = arguments.as_slice() else { return Err(()) };
            Ok(Some(StoredChildren::Sequence(element.clone())))
        }
        Type::Named(name, arguments) if name == "Dict" => {
            let [key, value] = arguments.as_slice() else { return Err(()) };
            Ok(Some(StoredChildren::Mapping(key.clone(), value.clone())))
        }
        Type::Named(..) => match instantiated_declared_fields(ty, definitions) {
            Some(fields) => fields.map(|fields| Some(StoredChildren::Aggregate(fields))),
            None => Ok(scalar_aggregate_shape(ty, definitions)
                .map(|shape| StoredChildren::Aggregate(shape.fields))),
        },
        _ => Ok(None),
    }
}

fn snapshot_eligibility_for(
    ty: &Type,
    definitions: &BTreeMap<String, TypeDef>,
    use_kind: SnapshotUse,
) -> Result<(), SnapshotRefusal> {
    fn walk(
        ty: &Type,
        definitions: &BTreeMap<String, TypeDef>,
        use_kind: SnapshotUse,
        active: &mut HashSet<Type>,
        depth: usize,
        remaining: &mut usize,
    ) -> Result<(), SnapshotRefusal> {
        if depth > 128 || *remaining == 0 {
            return Err(SnapshotRefusal::ResourceLimit);
        }
        *remaining -= 1;
        match ty {
            Type::Qualified(TypeQual::Frozen, inner) => {
                walk(inner, definitions, use_kind, active, depth + 1, remaining)
            }
            Type::Qualified(TypeQual::Unique, inner) => {
                if use_kind == SnapshotUse::ReplayableTask {
                    Err(SnapshotRefusal::UniqueOwner)
                } else {
                    walk(inner, definitions, use_kind, active, depth + 1, remaining)
                }
            }
            Type::Qualified(TypeQual::View, inner) => {
                if use_kind == SnapshotUse::ReplayableTask
                    && RootedViewStorage::classify(inner, definitions, |_| Some(0)).is_none()
                {
                    return Err(SnapshotRefusal::UnprovedStorage);
                }
                walk(inner, definitions, SnapshotUse::PersistentView, active, depth + 1, remaining)
            }
            Type::Slice(inner) if use_kind == SnapshotUse::PersistentView => {
                walk(inner, definitions, use_kind, active, depth + 1, remaining)
            }
            Type::Qualified(_, _) | Type::Dyn(_, _) | Type::RecordCompose { .. } => {
                Err(SnapshotRefusal::UnprovedStorage)
            }
            Type::Slice(_) => Err(SnapshotRefusal::UnprovedStorage),
            Type::Fn(_, _, _, qualifiers) => {
                if qualifiers.once {
                    Err(SnapshotRefusal::OnceCallable)
                } else if CallableStoragePlan::classify(ty, definitions).is_some() {
                    Ok(())
                } else {
                    Err(SnapshotRefusal::UnprovedStorage)
                }
            }
            Type::Tuple(fields) => {
                for field in fields {
                    walk(field, definitions, use_kind, active, depth + 1, remaining)?;
                }
                Ok(())
            }
            Type::Named(name, _) => {
                if let Some(definition) = definitions.get(name) {
                    if definition.must_consume {
                        return Err(SnapshotRefusal::MustConsume);
                    }
                }
                if crate::capabilities::representation(ty, definitions).is_some() {
                    return if use_kind == SnapshotUse::ReplayableTask {
                        Ok(())
                    } else {
                        Err(SnapshotRefusal::Authority)
                    };
                }
                if definitions.get(name).is_some_and(|definition| definition.is_capability) {
                    return Err(SnapshotRefusal::Authority);
                }
                if let Some(RootedViewStorage::RetainedProjection(element)) =
                    RootedViewStorage::classify(ty, definitions, |_| Some(0))
                {
                    return walk(&element.source_type(), definitions, use_kind, active, depth + 1, remaining);
                }
                if !active.insert(ty.clone()) {
                    return Err(SnapshotRefusal::UnprovedStorage);
                }
                // The aggregate layout judgment supplies instantiated stored
                // fields. Phantom arguments bind identity but carry no value.
                let fields = if use_kind == SnapshotUse::ReplayableTask
                    && let Some(children) = stored_children(ty, definitions)
                        .map_err(|_| SnapshotRefusal::UnprovedStorage)?
                {
                    if matches!(StoragePlan::classify(ty, definitions, |_| Some(0)),
                        StoragePlan::Refused(_)) {
                        return Err(SnapshotRefusal::UnprovedStorage);
                    }
                    children.into_types()
                } else if let Some(shape) = scalar_aggregate_shape(ty, definitions) {
                    shape.fields
                } else {
                    match StoragePlan::classify(ty, definitions, |_| Some(0)) {
                        StoragePlan::Owned(CarrierElement::List(element)) => vec![element.source_type()],
                        StoragePlan::Owned(CarrierElement::Scalar(_) | CarrierElement::String | CarrierElement::Bytes)
                        | StoragePlan::InlineScalar(_) => Vec::new(),
                        StoragePlan::AuthenticatedPlace(plan) if plan.cloneable() => Vec::new(),
                        StoragePlan::RetainedGraphList(plan) | StoragePlan::RetainedOpaqueAggregateList(plan)
                            if plan.cloneable() => vec![plan.source().clone()],
                        StoragePlan::RetainedCallableList(plan) => vec![plan.exact_signature().clone()],
                        StoragePlan::RetainedOwnerList(plan) => vec![plan.source_identity()],
                        _ => return Err(SnapshotRefusal::UnprovedStorage),
                    }
                };
                for field in &fields {
                    walk(field, definitions, use_kind, active, depth + 1, remaining)?;
                }
                active.remove(ty);
                Ok(())
            }
        }
    }
    SourceTypeKey { source: ty }.digest().ok_or(SnapshotRefusal::ResourceLimit)?;
    walk(ty, definitions, use_kind, &mut HashSet::new(), 0, &mut 4_096)
}

/// A finite source-derived proof for the storage classes the canonical route
/// currently understands. An owned value retains, clones, and releases its
/// recursive `CarrierElement`; a view or view-bearing aggregate has its own
/// authenticated owner plan. Missing proof remains an explicit refusal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoragePlan {
    InlineScalar(CanonicalScalarKind),
    /// A checked packed record whose fields are all inline scalar lanes.
    InlineAggregate(ScalarAggregateShape),
    Owned(CarrierElement),
    /// A checked finite aggregate graph whose recursive edges are retained
    /// pointer or callable edges. Its layout and each owner operation still
    /// require physical validation; it is not a slice carrier proof.
    AuthenticatedPlace(AggregateGraphPlan),
    /// An owned closure record, whose exact source contract and normalized
    /// physical family must both match before any owner authority is granted.
    RetainedCallable(CallableStoragePlan),
    /// A list of authenticated scalar-place owner tokens. Projected-view
    /// lists need a different owner-family proof and remain refused.
    RetainedOwnerList(OwnerCarriedViewPlan),
    /// A retained list whose element is a checked recursive aggregate graph.
    RetainedGraphList(AggregateGraphPlan),
    /// A retained list whose element has an authenticated closure family.
    RetainedCallableList(CallableStoragePlan),
    /// A typed GC list of cloneable opaque aggregate owners.
    RetainedOpaqueAggregateList(AggregateGraphPlan),
    /// A packed sequence whose checked element graph can be transferred and
    /// released, but cannot be cloned. Each initialized leaf carries its own
    /// affine disposition; no ordinary list read may duplicate it.
    MoveOnlyList(MoveOnlySequencePlan),
    RootedView(RootedViewStorage),
    AggregateView(ViewCarrierPlan),
    Refused(StorageRefusal),
}

/// A finite checked aggregate graph, including callable and recursive
/// nominal fields. `cloneable` is false when an affine or authority field
/// prevents a generic collection from duplicating the graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AggregateGraphPlan {
    source: Type,
    shape: ScalarAggregateShape,
    source_key: [u8; 32],
    cloneable: bool,
    movable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveOnlySequencePlan {
    source: Type,
    element: Box<StoragePlan>,
    source_key: [u8; 32],
}

impl MoveOnlySequencePlan {
    pub fn source(&self) -> &Type { &self.source }
    pub fn element(&self) -> &StoragePlan { &self.element }
    pub fn source_key(&self) -> [u8; 32] { self.source_key }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListElementDisposition {
    Cloneable,
    MoveOnly,
}

impl AggregateGraphPlan {
    pub fn source(&self) -> &Type { &self.source }
    pub fn shape(&self) -> &ScalarAggregateShape { &self.shape }
    pub fn source_key(&self) -> [u8; 32] { self.source_key }
    pub fn cloneable(&self) -> bool { self.cloneable }
    pub fn movable(&self) -> bool { self.movable }

    fn classify(ty: &Type, definitions: &BTreeMap<String, TypeDef>) -> Option<Self> {
        if crate::capabilities::representation(ty, definitions).is_some() { return None; }
        let shape = scalar_aggregate_shape(ty, definitions)?;
        if shape.packed { return None; }
        let source_key = SourceTypeKey { source: ty }.digest()?;
        let mut remaining = 4_096;
        let operations = graph_storage_operations(ty, definitions, &mut HashSet::new(), 0, &mut remaining)?;
        Some(Self { source: ty.clone(), shape, source_key,
            cloneable: operations.cloneable, movable: operations.movable })
    }
}

#[derive(Clone, Copy)]
struct GraphOperations {
    cloneable: bool,
    movable: bool,
}

impl GraphOperations {
    const COPY: Self = Self { cloneable: true, movable: true };
    const REFUSED: Self = Self { cloneable: false, movable: false };

    fn with(self, child: Self) -> Self {
        Self { cloneable: self.cloneable && child.cloneable,
            movable: self.movable && child.movable }
    }
}

/// Verify representation and ownership at every source edge. An aggregate
/// back edge is guarded by its retained heap pointer; callable signatures
/// carry an exact bounded key and receive separate SafeWIR ABI validation.
fn graph_storage_operations(
    ty: &Type,
    definitions: &BTreeMap<String, TypeDef>,
    active: &mut HashSet<Type>,
    depth: usize,
    remaining: &mut usize,
) -> Option<GraphOperations> {
    if depth > 128 || *remaining == 0 { return None; }
    *remaining -= 1;
    let child = |ty: &Type, active: &mut HashSet<Type>, remaining: &mut usize| {
        graph_storage_operations(ty, definitions, active, depth + 1, remaining)
    };
    match ty {
        Type::Qualified(TypeQual::Frozen, inner) => child(inner, active, remaining),
        Type::Qualified(TypeQual::Unique, inner) => {
            child(inner, active, remaining).map(|_| GraphOperations::REFUSED)
        }
        Type::Qualified(TypeQual::View, _) => match StoragePlan::classify(ty, definitions, |_| Some(0)) {
            StoragePlan::RootedView(storage) => match storage {
            RootedViewStorage::InlineScalar(_) => Some(GraphOperations { cloneable: true, movable: false }),
            RootedViewStorage::RetainedRoot(RetainedRootStorage::ByteBuffer(_)
                | RetainedRootStorage::Sequence(_)
                | RetainedRootStorage::StructuralRecord(_)
                | RetainedRootStorage::CheckedAggregate(_)) => Some(GraphOperations { cloneable: true, movable: false }),
            RootedViewStorage::ScalarCell(_) | RootedViewStorage::RetainedProjection(_) => None,
            },
            _ => None,
        },
        Type::Qualified(_, _) | Type::Slice(_) | Type::Dyn(_, _) | Type::RecordCompose { .. } => None,
        Type::Fn(_, _, _, qualifiers) => {
            CallableStoragePlan::classify(ty, definitions).map(|_| GraphOperations {
                cloneable: !qualifiers.once, movable: false,
            })
        }
        Type::Named(_, _) | Type::Tuple(_) => {
            if let Some(children @ (StoredChildren::Sequence(_) | StoredChildren::Mapping(_, _))) =
                stored_children(ty, definitions).ok().flatten()
            {
                return children.into_types().iter().try_fold(GraphOperations::COPY, |all, field| {
                    Some(all.with(child(field, active, remaining)?))
                });
            }
            if view_carrier_plan(ty, definitions).is_some_and(|plan| plan.requires_opaque_owner()) {
                return Some(GraphOperations { cloneable: opaque_aggregate_cloneable(ty, definitions), movable: false });
            }
            if let Some(element) = CarrierElement::from_checked_type(ty) {
                return Some(if matches!(element.ownership(), crate::carrier::CarrierOwnership::Copy
                    | crate::carrier::CarrierOwnership::RetainCloneRelease) {
                    GraphOperations::COPY
                } else { GraphOperations::REFUSED });
            }
            if crate::capabilities::representation(ty, definitions).is_some() {
                return Some(GraphOperations::REFUSED);
            }
            let shape = scalar_aggregate_shape(ty, definitions)?;
            if shape.packed { return None; }
            if !active.insert(ty.clone()) { return Some(GraphOperations::COPY); }
            // Instantiated fields, not phantom generic arguments, determine
            // storage operations. The source key still binds every argument.
            let operations = shape.fields.iter().try_fold(GraphOperations::COPY, |all, field| {
                    Some(all.with(child(field, active, remaining)?))
            });
            active.remove(ty);
            let must_consume = match ty {
                Type::Named(name, _) => definitions.get(name).is_some_and(|definition| definition.must_consume),
                _ => false,
            };
            operations.map(|mut operations| {
                operations.cloneable &= !must_consume;
                operations
            })
        }
    }
}

fn opaque_aggregate_cloneable(ty: &Type, definitions: &BTreeMap<String, TypeDef>) -> bool {
    let Some(shape) = scalar_aggregate_shape(ty, definitions) else { return false };
    if shape.packed || matches!(ty, Type::Named(name, _) if definitions.get(name).is_some_and(|definition| definition.must_consume)) {
        return false;
    }
    shape.fields.iter().all(|field| {
        if let Type::Qualified(TypeQual::View, payload) = field {
            let view = Type::Qualified(TypeQual::View, payload.clone());
            return matches!(StoragePlan::classify(&view, definitions, |_| Some(0)), StoragePlan::RootedView(_));
        }
        crate::is_unit_type(field)
            || CarrierElement::from_checked_type_with_definitions(field, definitions)
                .is_some_and(|element| matches!(element.ownership(),
                    crate::carrier::CarrierOwnership::Copy
                        | crate::carrier::CarrierOwnership::RetainCloneRelease))
            || matches!(field, Type::Fn(_, _, _, qualifiers) if !qualifiers.once)
                && CallableStoragePlan::classify(field, definitions).is_some()
    })
}

fn contains_gc_view_owner(ty: &Type, definitions: &BTreeMap<String, TypeDef>) -> Option<bool> {
    fn walk(
        ty: &Type,
        definitions: &BTreeMap<String, TypeDef>,
        active: &mut HashSet<Type>,
        depth: usize,
        remaining: &mut usize,
    ) -> Option<bool> {
        if depth > 128 || *remaining == 0 { return None; }
        *remaining -= 1;
        if !active.insert(ty.clone()) { return Some(false); }
        let result = if view_carrier_plan(ty, definitions).is_some_and(|plan| plan.requires_opaque_owner()) {
            Some(true)
        } else {
            let fields = if let Some(children) = stored_children(ty, definitions).ok()? {
                children.into_types()
            } else {
                match ty {
                    Type::Qualified(_, inner) | Type::Slice(inner) => vec![*inner.clone()],
                    _ => Vec::new(),
                }
            };
            fields.iter().try_fold(false, |found, field| {
                let child = walk(field, definitions, active, depth + 1, remaining)?;
                Some(found || child)
            })
        };
        active.remove(ty);
        result
    }
    walk(ty, definitions, &mut HashSet::new(), 0, &mut 4_096)
}

/// A refusal caused by source capability authority must remain distinguishable
/// from an ordinary missing ownership proof. Walk instantiated aggregate
/// fields, rather than relying on a list of concrete container spellings.
fn contains_capability_authority(ty: &Type, definitions: &BTreeMap<String, TypeDef>) -> Option<bool> {
    fn walk(
        ty: &Type,
        definitions: &BTreeMap<String, TypeDef>,
        active: &mut HashSet<Type>,
        depth: usize,
        remaining: &mut usize,
    ) -> Option<bool> {
        if depth > 128 || *remaining == 0 { return None; }
        *remaining -= 1;
        if crate::capabilities::representation(ty, definitions).is_some() { return Some(true); }
        if !active.insert(ty.clone()) { return Some(false); }
        let children = if let Some(children) = stored_children(ty, definitions).ok().flatten() {
            children.into_types()
        } else {
            match ty {
                Type::Named(_, arguments) | Type::Dyn(_, arguments) | Type::Tuple(arguments) => arguments.clone(),
                Type::Qualified(_, inner) | Type::Slice(inner) => vec![*inner.clone()],
                Type::RecordCompose { base, fields } => {
                    std::iter::once(base.as_ref().clone())
                        .chain(fields.iter().map(|(_, field)| field.clone())).collect()
                }
                Type::Fn(parameters, result, _, _) => parameters.iter().cloned()
                    .chain(std::iter::once(result.as_ref().clone())).collect(),
            }
        };
        let result = children.iter().try_fold(false, |found, child| {
            Some(found || walk(child, definitions, active, depth + 1, remaining)?)
        });
        active.remove(ty);
        result
    }
    walk(ty, definitions, &mut HashSet::new(), 0, &mut 4_096)
}

/// The packed one-variant record layout class is selected from field storage,
/// without duplicating a source-type name table in each compiler stage.
pub fn is_inline_scalar_record(definition: &TypeDef) -> bool {
    definition.params.is_empty()
        && definition.variants.len() == 1
        && definition.variants[0].fields.iter().all(|field| {
            matches!(
                StoragePlan::classify(field.unqualified(), &BTreeMap::new(), |_| None),
                StoragePlan::InlineScalar(_)
            )
        })
}

/// Opaque aggregate membership derived from checked closure captures. A
/// candidate owner family cannot create this evidence by naming itself.
pub struct OpaqueOwnerEvidence<'a> {
    definitions: &'a BTreeMap<String, TypeDef>,
    opaque_callable_families: HashSet<Type>,
}

/// Physical owner-bearing lane selected from checked storage, not from a
/// candidate GC type ID. A collection carries its own owner only when its
/// recursively classified element has a matching retained owner operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpaqueOwnerClass {
    Capability,
    RootedView,
    Callable,
    Aggregate,
    Collection,
}

impl<'a> OpaqueOwnerEvidence<'a> {
    pub fn from_checked_source(
        definitions: &'a BTreeMap<String, TypeDef>,
        witnesses: &ClosureWitnessManifest,
    ) -> Option<Self> {
        let families = witnesses.entries().iter().filter_map(|witness| {
            witness.signature().map(|signature| {
                let family = crate::callable_storage::bounded_closure_family_type(signature, definitions)?;
                Some((family, witness.captures().iter().map(|capture| capture.checked_type()).collect::<Vec<_>>()))
            })
        }).collect::<Option<Vec<_>>>()?;
        if families.len() > 4_096 { return None; }
        let mut evidence = Self { definitions, opaque_callable_families: HashSet::new() };
        loop {
            let mut changed = false;
            for (family, captures) in &families {
                if captures.iter().any(|capture| evidence.requires_owner(capture)) {
                    changed |= evidence.opaque_callable_families.insert(family.clone());
                }
            }
            if !changed { break; }
        }
        Some(evidence)
    }

    pub fn requires_owner(&self, ty: &Type) -> bool {
        self.requires_owner_at(ty, &mut HashSet::new(), 0, &mut 16_384) == Some(true)
    }

    /// The actual carrier at this edge. `requires_owner` answers whether a
    /// stored descendant needs an owner; this query answers whether the value
    /// itself occupies an opaque owner lane. Keeping the two judgments apart
    /// prevents a linear outer collection from inheriting a child's GC ABI.
    pub fn owner_class(&self, ty: &Type) -> Option<OpaqueOwnerClass> {
        if crate::capabilities::representation(ty, self.definitions).is_some() {
            return Some(OpaqueOwnerClass::Capability);
        }
        match StoragePlan::classify(ty, self.definitions, |_| Some(0)) {
            StoragePlan::RootedView(RootedViewStorage::ScalarCell(_)
                | RootedViewStorage::RetainedProjection(_)) => return Some(OpaqueOwnerClass::RootedView),
            StoragePlan::RetainedOwnerList(_) => {
                let Some(StoredChildren::Sequence(element)) = stored_children(ty, self.definitions).ok().flatten() else { return None };
                return (self.owner_class(&element) == Some(OpaqueOwnerClass::RootedView))
                    .then_some(OpaqueOwnerClass::Collection);
            }
            StoragePlan::RetainedCallableList(_) => {
                let Some(StoredChildren::Sequence(element)) = stored_children(ty, self.definitions).ok().flatten() else { return None };
                return (self.owner_class(&element) == Some(OpaqueOwnerClass::Callable))
                    .then_some(OpaqueOwnerClass::Collection);
            }
            StoragePlan::RetainedOpaqueAggregateList(_) | StoragePlan::RetainedGraphList(_) => {
                let Some(StoredChildren::Sequence(element)) = stored_children(ty, self.definitions).ok().flatten() else { return None };
                return (self.owner_class(&element) == Some(OpaqueOwnerClass::Aggregate))
                    .then_some(OpaqueOwnerClass::Collection);
            }
            StoragePlan::Refused(_) => return None,
            _ => {}
        }
        if matches!(ty.unqualified(), Type::Fn(..)) {
            let family = crate::callable_storage::bounded_closure_family_type(ty, self.definitions)?;
            return self.opaque_callable_families.contains(&family)
                .then_some(OpaqueOwnerClass::Callable);
        }
        let shape = scalar_aggregate_shape(ty, self.definitions)?;
        (!shape.packed && self.requires_owner(ty))
            .then_some(OpaqueOwnerClass::Aggregate)
    }

    fn requires_owner_at(
        &self,
        ty: &Type,
        active: &mut HashSet<Type>,
        depth: usize,
        remaining: &mut usize,
    ) -> Option<bool> {
        if depth > 128 || *remaining == 0 { return None; }
        *remaining -= 1;
        if !active.insert(ty.clone()) { return Some(false); }
        let result = if view_carrier_plan(ty, self.definitions)
            .is_some_and(|plan| plan.requires_opaque_owner())
            || crate::capabilities::representation(ty, self.definitions).is_some()
        {
            Some(true)
        } else if matches!(ty.unqualified(), Type::Fn(..)) {
            let family = crate::callable_storage::bounded_closure_family_type(ty, self.definitions)?;
            Some(self.opaque_callable_families.contains(&family))
        } else {
            let children = match stored_children(ty, self.definitions) {
                Ok(children) => children.map(StoredChildren::into_types).unwrap_or_default(),
                Err(()) => return Some(true),
            };
            children.iter().try_fold(false, |found, child| {
                let required = self.requires_owner_at(child, active, depth + 1, remaining)?;
                Some(found || required)
            })
        };
        active.remove(ty);
        result
    }
}

/// An aggregate can transfer its affine fields through a match only
/// when each successful arm binds every such field exactly once. The frontend
/// and lowerer use this same shape and pattern judgment; physical nullable
/// slots and owner-token moves still require SafeWIR validation.
pub fn consuming_aggregate_match_shape(
    ty: &Type,
    arms: &[MatchArm],
    definitions: &BTreeMap<String, TypeDef>,
) -> Option<ScalarAggregateShape> {
    if !StoragePlan::classify(ty, definitions, |_| Some(0)).requires_affine_owner_token() {
        return None;
    }
    let shape = scalar_aggregate_shape(ty, definitions)?;
    if arms.is_empty() || arms.iter().any(|arm| {
        if arm.guard.is_some() {
            return true;
        }
        let (name, args) = match &arm.pattern {
            Pattern::Ctor { name, args } => (name, args),
            Pattern::AnonCtor { tag, args } => (tag, args),
            _ => return true,
        };
        let Some((_, variant)) = shape.variant(name) else {
            return true;
        };
        args.len() != variant.arity || args.iter().enumerate().any(|(offset, arg)| {
            let field = &shape.fields[variant.first_field + offset];
            match arg {
                Pattern::Var(name) if name != "_" => false,
                Pattern::Var(_) | Pattern::Wildcard => StoragePlan::classify(
                    field, definitions, |_| Some(0),
                ).requires_affine_owner_token(),
                _ => true,
            }
        })
    }) {
        return None;
    }
    Some(shape)
}

/// The finite ownership structure of a checked affine value. Each recursive
/// edge must independently match its physical owned slot before SafeWIR may
/// use it; this source judgment alone does not grant an owner token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AffineStorageShape {
    Leaf,
    NullableFields(Vec<(u32, AffineStorageShape)>),
    Sequence(Box<AffineStorageShape>),
}

pub fn affine_storage_shape(
    ty: &Type,
    definitions: &BTreeMap<String, TypeDef>,
) -> Option<AffineStorageShape> {
    fn walk(
        plan: &StoragePlan, definitions: &BTreeMap<String, TypeDef>,
        active: &mut HashSet<Type>, depth: usize, remaining: &mut usize,
    ) -> Option<AffineStorageShape> {
        if depth > 128 || *remaining == 0 || !plan.requires_affine_owner_token() { return None; }
        *remaining -= 1;
        let source = match plan {
            StoragePlan::MoveOnlyList(sequence) => sequence.source(),
            StoragePlan::AuthenticatedPlace(graph) => graph.source(),
            _ => return None,
        };
        if !active.insert(source.clone()) { return None; }
        let result = match plan {
            StoragePlan::MoveOnlyList(sequence) => Some(AffineStorageShape::Sequence(Box::new(
                walk(sequence.element(), definitions, active, depth + 1, remaining)?,
            ))),
            StoragePlan::AuthenticatedPlace(graph) if graph.movable() && !graph.cloneable() => {
                let shape = graph.shape();
                if shape.packed { return None; }
                let mut children = Vec::new();
                for (index, field) in shape.fields.iter().enumerate() {
                    if *remaining == 0 { return None; }
                    *remaining -= 1;
                    let child = StoragePlan::classify(field, definitions, |_| Some(0));
                    if child.requires_affine_owner_token() {
                        if !shape.tagged { return None; }
                        children.push((u32::try_from(index).ok()?, walk(&child, definitions, active, depth + 1, remaining)?));
                    }
                }
                Some(if children.is_empty() { AffineStorageShape::Leaf }
                    else { AffineStorageShape::NullableFields(children) })
            }
            _ => None,
        };
        active.remove(source);
        result
    }
    let plan = StoragePlan::classify(ty.unqualified(), definitions, |_| Some(0));
    walk(&plan, definitions, &mut HashSet::new(), 0, &mut 4_096)
}

impl StoragePlan {
    /// An independent owned copy can be made from this checked storage.
    /// Borrowed and view-bearing plans never acquire this proof by omission.
    pub fn admits_owned_copy(&self) -> bool {
        match self {
            Self::InlineScalar(_) | Self::InlineAggregate(_) | Self::Owned(_)
            | Self::RetainedCallable(_) | Self::RetainedCallableList(_) => true,
            Self::AuthenticatedPlace(plan) | Self::RetainedGraphList(plan)
            | Self::RetainedOpaqueAggregateList(plan) => plan.cloneable(),
            Self::MoveOnlyList(_) | Self::RetainedOwnerList(_) | Self::RootedView(_) | Self::AggregateView(_)
            | Self::Refused(_) => false,
        }
    }

    /// A copy also severs a call-scoped origin only when no callable capture
    /// or opaque aggregate field can hide another borrowed origin.
    pub fn admits_call_scoped_copy(&self) -> bool {
        matches!(self, Self::InlineScalar(_) | Self::InlineAggregate(_) | Self::Owned(_))
    }

    /// An affine aggregate has exactly one transferable owner token. The
    /// source graph proves moves and recursive release but supplies no clone.
    pub fn requires_affine_owner_token(&self) -> bool {
        matches!(self, Self::MoveOnlyList(_))
            || matches!(self, Self::AuthenticatedPlace(plan) if plan.movable() && !plan.cloneable())
    }

    /// A list can duplicate initialized elements. Require a copyable owned
    /// carrier, a non-once callable, or a cloneable authenticated graph.
    /// Other storage plans retain their own representation but cannot become
    /// an ordinary packed-list element through a second shape table.
    pub fn list_element_disposition(&self, ty: &Type) -> Option<ListElementDisposition> {
        match self {
            Self::InlineScalar(_) | Self::Owned(_) => Some(ListElementDisposition::Cloneable),
            Self::RetainedCallable(_) if matches!(ty, Type::Fn(_, _, _, qualifiers) if !qualifiers.once) =>
                Some(ListElementDisposition::Cloneable),
            Self::AuthenticatedPlace(plan) if plan.cloneable() => Some(ListElementDisposition::Cloneable),
            Self::AuthenticatedPlace(plan) if plan.movable() => Some(ListElementDisposition::MoveOnly),
            Self::MoveOnlyList(_) => Some(ListElementDisposition::MoveOnly),
            _ => None,
        }
    }

    /// The recursive owned carrier shared by checking and lowering. Views and
    /// opaque owner plans need their own authority.
    pub fn into_carrier_element(self) -> Option<CarrierElement> {
        match self {
            Self::InlineScalar(kind) => {
                Some(CarrierElement::Scalar(kind))
            }
            Self::Owned(plan) => Some(plan),
            _ => None,
        }
    }

    pub fn classify(
        ty: &Type,
        definitions: &BTreeMap<String, TypeDef>,
        nominal: impl Fn(&str) -> Option<u32>,
    ) -> Self {
        Self::classify_inner(ty, definitions, &nominal)
    }

    fn classify_inner(
        ty: &Type,
        definitions: &BTreeMap<String, TypeDef>,
        nominal: &dyn Fn(&str) -> Option<u32>,
    ) -> Self {
        // `view_carrier_plan` traverses checked nominal definitions before the
        // owned-carrier parser applies its own limits. Bound that traversal as
        // well, including repeated visits through distinct source branches.
        let mut remaining = 16_384;
        if !within_source_bounds(ty, definitions, &mut BTreeSet::new(), 0, &mut remaining) {
            return Self::Refused(StorageRefusal::ResourceLimit);
        }
        if let Type::Qualified(TypeQual::View, payload) = ty {
            if let Err(reason) = snapshot_eligibility(payload, definitions) {
                return Self::Refused(match reason {
                    SnapshotRefusal::ResourceLimit => StorageRefusal::ResourceLimit,
                    SnapshotRefusal::Authority => StorageRefusal::UnprovedAuthority,
                    SnapshotRefusal::UniqueOwner | SnapshotRefusal::MustConsume | SnapshotRefusal::OnceCallable
                    | SnapshotRefusal::UnprovedStorage => StorageRefusal::UnprovedOwnership,
                });
            }
            return RootedViewStorage::classify(payload, definitions, nominal)
                .map(Self::RootedView)
                .unwrap_or(Self::Refused(StorageRefusal::UnprovedView));
        }
        if matches!(ty, Type::Qualified(..)) {
            return Self::Refused(StorageRefusal::UnprovedQualifier);
        }
        if crate::is_unit_type(ty) {
            return Self::InlineScalar(CanonicalScalarKind::Unit);
        }
        if let Some(shape) = scalar_aggregate_shape(ty, definitions)
            && shape.packed
            && shape.fields.iter().all(|field| crate::is_unit_type(field)
                || matches!(CarrierElement::from_checked_type(field),
                    Some(CarrierElement::Scalar(_))))
        {
            return Self::InlineAggregate(shape);
        }
        if matches!(ty, Type::Fn(..)) {
            return CallableStoragePlan::classify(ty, definitions)
                .map(Self::RetainedCallable)
                .unwrap_or(Self::Refused(StorageRefusal::UnprovedOwnership));
        }
        if let Type::Named(name, arguments) = ty
            && name == "List"
            && let [element] = arguments.as_slice()
            && let Some(plan) = OwnerCarriedViewPlan::classify(element, definitions)
            && matches!(
                plan.admitted_storage(),
                Some(OwnerCarriedViewStorage::AuthenticatedPlace(_))
            )
        {
            return Self::RetainedOwnerList(plan);
        }
        if let Type::Named(name, arguments) = ty
            && name == "List"
            && let [element] = arguments.as_slice()
            && let Some(plan) = AggregateGraphPlan::classify(element, definitions)
            && plan.cloneable()
        {
            return match contains_gc_view_owner(element, definitions) {
                Some(true) => Self::RetainedOpaqueAggregateList(plan),
                Some(false) => Self::RetainedGraphList(plan),
                None => Self::Refused(StorageRefusal::ResourceLimit),
            };
        }
        if let Type::Named(name, arguments) = ty
            && name == "List"
            && let [element] = arguments.as_slice()
        {
            let child = Self::classify_inner(element, definitions, nominal);
            if child.list_element_disposition(element) == Some(ListElementDisposition::MoveOnly) {
                let Some(source_key) = (SourceTypeKey { source: ty }).digest() else {
                    return Self::Refused(StorageRefusal::ResourceLimit);
                };
                return Self::MoveOnlyList(MoveOnlySequencePlan {
                    source: ty.clone(), element: Box::new(child), source_key,
                });
            }
        }
        if let Type::Named(name, arguments) = ty
            && name == "List"
            && let [element] = arguments.as_slice()
            && matches!(element, Type::Fn(_, _, _, qualifiers) if !qualifiers.once)
            && let Some(plan) = CallableStoragePlan::classify(element, definitions)
        {
            return Self::RetainedCallableList(plan);
        }
        if let Some(plan) = view_carrier_plan(ty, definitions).filter(ViewCarrierPlan::requires_opaque_owner) {
            return Self::AggregateView(plan);
        }
        match CarrierElement::from_checked_type_with_definitions(ty, definitions) {
            Some(CarrierElement::Scalar(kind)) if kind != CanonicalScalarKind::Unit => {
                Self::InlineScalar(kind)
            }
            Some(plan) if plan.within_resource_bounds() => Self::Owned(plan),
            _ => AggregateGraphPlan::classify(ty, definitions)
                .map(Self::AuthenticatedPlace)
                .unwrap_or_else(|| match contains_capability_authority(ty, definitions) {
                    Some(true) => Self::Refused(StorageRefusal::UnprovedAuthority),
                    Some(false) => Self::Refused(StorageRefusal::UnprovedOwnership),
                    None => Self::Refused(StorageRefusal::ResourceLimit),
                }),
        }
    }
}

fn within_source_bounds(
    ty: &Type,
    definitions: &BTreeMap<String, TypeDef>,
    active: &mut BTreeSet<String>,
    depth: usize,
    remaining: &mut usize,
) -> bool {
    if depth > 128 || *remaining == 0 {
        return false;
    }
    *remaining -= 1;
    let child = |ty: &Type, active: &mut BTreeSet<String>, remaining: &mut usize| {
        within_source_bounds(ty, definitions, active, depth + 1, remaining)
    };
    match ty {
        Type::Qualified(_, inner) | Type::Slice(inner) => child(inner, active, remaining),
        Type::Named(name, arguments) | Type::Dyn(name, arguments) => {
            if !arguments
                .iter()
                .all(|argument| child(argument, active, remaining))
            {
                return false;
            }
            let Some(definition) = definitions.get(name) else {
                return true;
            };
            if !active.insert(name.clone()) {
                return true;
            }
            let within = definition.variants.iter().all(|variant| {
                variant
                    .fields
                    .iter()
                    .all(|field| child(field, active, remaining))
            });
            active.remove(name);
            within
        }
        Type::Tuple(items) => items.iter().all(|item| child(item, active, remaining)),
        Type::RecordCompose { base, fields } => {
            child(base, active, remaining)
                && fields
                    .iter()
                    .all(|(_, field)| child(field, active, remaining))
        }
        Type::Fn(parameters, result, _, _) => {
            parameters
                .iter()
                .all(|parameter| child(parameter, active, remaining))
                && child(result, active, remaining)
        }
    }
}
