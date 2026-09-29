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
    snapshot_eligibility_for(ty, definitions, SnapshotUse::PersistentView, &BTreeSet::new()).map(|_| ())
}

/// A lazy recipe retains one copyable snapshot and clones invocation state on
/// every execution. Unlike a persistent view, its declared capability token
/// may be copied as delegated authority; affine storage still cannot replay.
pub fn replay_capture_eligibility(
    ty: &Type,
    definitions: &BTreeMap<String, TypeDef>,
) -> Result<(), SnapshotRefusal> {
    snapshot_eligibility_for(ty, definitions, SnapshotUse::ReplayableTask, &BTreeSet::new()).map(|_| ())
}

/// A generic recipe capture is conditional on each checked instantiation.
/// The returned flag records whether an unresolved formal was encountered;
/// callers must carry a replay obligation to every concrete call when true.
/// Known constructors, qualifiers, stored children, and resource limits are
/// still checked here, while exact clone/release admission occurs at the call.
pub fn replay_capture_template_eligibility(
    ty: &Type,
    definitions: &BTreeMap<String, TypeDef>,
    formals: &BTreeSet<String>,
) -> Result<bool, SnapshotRefusal> {
    snapshot_eligibility_for(ty, definitions, SnapshotUse::ReplayableTask, formals)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SnapshotUse { PersistentView, ReplayableTask }

/// Instantiated stored children, with collection construction distinguished
/// from declared aggregate fields. This describes source storage only; a
/// caller still needs its layout and ownership-operation proof.
pub enum StoredChildren {
    Sequence(Type),
    Mapping(Type, Type),
    Aggregate(Vec<Type>),
}

impl StoredChildren {
    /// Source fields that occupy storage, without physical tags or phantom
    /// generic arguments. A consumer still checks its own representation.
    pub fn into_types(self) -> Vec<Type> {
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

/// This spelling cannot occur in source (the lexer reserves `@`). The checker
/// uses it only while asking whether an unresolved generic could make a pure
/// closure capture unsafe; it is a refusal, never an authority grant.
pub const PURE_CAPTURE_UNKNOWN: &str = "@unresolved-pure-capture";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PureCaptureHazard {
    Capability(crate::capabilities::CapabilityKind),
    DeclaredCapability,
    OrdinaryCallable,
    Existential,
    UnresolvedGeneric,
    UnprovedStorage,
}

/// Classify a pure closure's actual stored capture through the same checked
/// child relation used for ownership. Callable signatures and phantom type
/// arguments cannot grant or revoke the captured value's effect contract.
pub fn pure_capture_hazard(
    ty: &Type,
    definitions: &BTreeMap<String, TypeDef>,
    catalog: &StoredObligationCatalog,
) -> Option<PureCaptureHazard> {
    fn walk(
        ty: &Type,
        definitions: &BTreeMap<String, TypeDef>,
        catalog: &StoredObligationCatalog,
        active: &mut BTreeSet<String>,
        remaining: &mut usize,
    ) -> Option<PureCaptureHazard> {
        if *remaining == 0 { return Some(PureCaptureHazard::UnprovedStorage); }
        *remaining -= 1;
        match ty {
            Type::Qualified(_, inner) | Type::Slice(inner) =>
                walk(inner, definitions, catalog, active, remaining),
            Type::Fn(_, _, _, qualifiers) => (!qualifiers.pure)
                .then_some(PureCaptureHazard::OrdinaryCallable),
            Type::Dyn(..) => Some(PureCaptureHazard::Existential),
            Type::Tuple(fields) => fields.iter().find_map(|field|
                walk(field, definitions, catalog, active, remaining)),
            Type::RecordCompose { .. } => Some(PureCaptureHazard::UnprovedStorage),
            Type::Named(name, arguments) => {
                if name == PURE_CAPTURE_UNKNOWN {
                    return Some(PureCaptureHazard::UnresolvedGeneric);
                }
                if definitions.get(name).is_some_and(|definition| definition.is_capability) {
                    return Some(PureCaptureHazard::DeclaredCapability);
                }
                if let Some(representation) = crate::capabilities::representation(ty, definitions) {
                    let kind = match representation {
                        crate::capabilities::CapabilityRepresentation::Direct(kind)
                        | crate::capabilities::CapabilityRepresentation::Nullable(kind) => kind,
                    };
                    return Some(PureCaptureHazard::Capability(kind));
                }
                if let Some(mask) = catalog_mask(name, &catalog.stored_parameters, definitions) {
                    if mask.len() != arguments.len() {
                        return Some(PureCaptureHazard::UnprovedStorage);
                    }
                } else if definitions.contains_key(name) {
                    return Some(PureCaptureHazard::UnprovedStorage);
                }
                let stored_shape = match stored_children(ty, definitions) {
                    Ok(shape) => shape,
                    Err(()) => return Some(PureCaptureHazard::UnprovedStorage),
                };
                for child in stored_arguments_for_catalog(ty, &catalog.stored_parameters, definitions) {
                    if let Some(hazard) = walk(&child, definitions, catalog, active, remaining) {
                        return Some(hazard);
                    }
                }
                // Recursive generic heads may transform their argument on
                // every trip. Inspect stored argument positions first, then
                // stop revisiting the declaration's field graph by name.
                if !active.insert(name.clone()) { return None; }
                let children = match stored_shape {
                    Some(children) => children.into_types(),
                    None if definitions.contains_key(name) => Vec::new(),
                    None if !matches!(StoragePlan::classify(ty, definitions, |_| Some(0)), StoragePlan::Refused(_)) => Vec::new(),
                    _ => return Some(PureCaptureHazard::UnprovedStorage),
                };
                for child in children {
                    if let Some(hazard) = walk(&child, definitions, catalog, active, remaining) {
                        return Some(hazard);
                    }
                }
                active.remove(name);
                None
            }
        }
    }
    walk(ty, definitions, catalog, &mut BTreeSet::new(), &mut 4_096)
}

impl StoredObligationCatalog {
    /// Whether this concrete checked type carries a must-consume value in
    /// stored fields. Callable signatures and phantom arguments are excluded
    /// by the same recursive relation used to build this catalog.
    pub fn carries_must_source(
        &self, ty: &Type, definitions: &BTreeMap<String, TypeDef>,
    ) -> bool {
        carries_must(ty, &self.must_types, &self.stored_parameters,
            &BTreeSet::new(), definitions)
    }

    /// The checked language's lazy task token has an execution obligation
    /// even when its result parameter is absent from stored fields. Both the
    /// source checker and final async frame consume this same catalog.
    pub fn from_module(module: &Module) -> Option<Self> {
        let execution_obligations = &[("Task", 1)];
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

/// The exact children held in source storage for a checked type. An error
/// means the constructor's arity or declared storage cannot be established.
/// Consumers must not infer stored children from generic arguments on error.
pub fn stored_children(
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
            None => Ok(scalar_aggregate_shape(ty, definitions).map(|shape| {
                // The shape's leading tag is physical metadata, not a source
                // child. Traverse only payload slots named by variants.
                let fields = shape.variants.iter().flat_map(|variant| {
                    shape.fields[variant.first_field..variant.first_field + variant.arity]
                        .iter().cloned()
                }).collect();
                StoredChildren::Aggregate(fields)
            })),
        },
        _ => Ok(None),
    }
}

fn snapshot_eligibility_for(
    ty: &Type,
    definitions: &BTreeMap<String, TypeDef>,
    use_kind: SnapshotUse,
    formals: &BTreeSet<String>,
) -> Result<bool, SnapshotRefusal> {
    fn contains_formal(ty: &Type, formals: &BTreeSet<String>) -> bool {
        match ty {
            Type::Named(name, arguments) => (arguments.is_empty() && formals.contains(name))
                || arguments.iter().any(|argument| contains_formal(argument, formals)),
            Type::Tuple(fields) | Type::Dyn(_, fields) =>
                fields.iter().any(|field| contains_formal(field, formals)),
            Type::Fn(parameters, result, _, _) => parameters.iter().any(|parameter| contains_formal(parameter, formals))
                || contains_formal(result, formals),
            Type::Slice(inner) | Type::Qualified(_, inner) => contains_formal(inner, formals),
            Type::RecordCompose { base, fields } => contains_formal(base, formals)
                || fields.iter().any(|(_, field)| contains_formal(field, formals)),
        }
    }
    fn walk(
        ty: &Type,
        definitions: &BTreeMap<String, TypeDef>,
        use_kind: SnapshotUse,
        formals: &BTreeSet<String>,
        active: &mut HashSet<Type>,
        depth: usize,
        remaining: &mut usize,
    ) -> Result<bool, SnapshotRefusal> {
        if depth > 128 || *remaining == 0 {
            return Err(SnapshotRefusal::ResourceLimit);
        }
        *remaining -= 1;
        match ty {
            Type::Qualified(TypeQual::Frozen, inner) => {
                walk(inner, definitions, use_kind, formals, active, depth + 1, remaining)
            }
            Type::Qualified(TypeQual::Unique, inner) => {
                if use_kind == SnapshotUse::ReplayableTask {
                    Err(SnapshotRefusal::UniqueOwner)
                } else {
                    walk(inner, definitions, use_kind, formals, active, depth + 1, remaining)
                }
            }
            Type::Qualified(TypeQual::View, inner) => {
                if use_kind == SnapshotUse::ReplayableTask
                    && RootedViewStorage::classify(inner, definitions, |_| Some(0)).is_none()
                    && !contains_formal(inner, formals)
                {
                    return Err(SnapshotRefusal::UnprovedStorage);
                }
                walk(inner, definitions, SnapshotUse::PersistentView, formals, active, depth + 1, remaining)
            }
            Type::Slice(inner) if use_kind == SnapshotUse::PersistentView => {
                walk(inner, definitions, use_kind, formals, active, depth + 1, remaining)
            }
            Type::Qualified(_, _) | Type::Dyn(_, _) | Type::RecordCompose { .. } => {
                Err(SnapshotRefusal::UnprovedStorage)
            }
            Type::Slice(_) => Err(SnapshotRefusal::UnprovedStorage),
            Type::Fn(_, _, _, qualifiers) => {
                if qualifiers.once {
                    Err(SnapshotRefusal::OnceCallable)
                } else if CallableStoragePlan::classify(ty, definitions).is_some() {
                    Ok(false)
                } else if contains_formal(ty, formals) {
                    Ok(true)
                } else {
                    Err(SnapshotRefusal::UnprovedStorage)
                }
            }
            Type::Tuple(fields) => {
                let mut deferred = false;
                for field in fields {
                    deferred |= walk(field, definitions, use_kind, formals, active, depth + 1, remaining)?;
                }
                Ok(deferred)
            }
            Type::Named(name, arguments) => {
                if arguments.is_empty() && formals.contains(name) {
                    return Ok(true);
                }
                if let Some(definition) = definitions.get(name) {
                    if definition.must_consume {
                        return Err(SnapshotRefusal::MustConsume);
                    }
                }
                if crate::capabilities::representation(ty, definitions).is_some() {
                    return if use_kind == SnapshotUse::ReplayableTask {
                        Ok(false)
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
                    return walk(&element.source_type(), definitions, use_kind, formals, active, depth + 1, remaining);
                }
                // A recursive nominal whose instantiated type grows on a
                // field edge cannot yield a finite snapshot proof. Detect
                // that edge before expanding thousands of increasingly deep
                // stored fields up to the general traversal budget.
                if active.iter().any(|previous| {
                    matches!(previous, Type::Named(previous_name, _) if previous_name == name)
                        && crate::access::type_node_count(ty)
                            > crate::access::type_node_count(previous)
                }) {
                    return Err(SnapshotRefusal::ResourceLimit);
                }
                if !active.insert(ty.clone()) {
                    return Err(SnapshotRefusal::UnprovedStorage);
                }
                // Both persistent views and replayable recipes require an
                // independent copy of the exact checked storage. The source
                // child relation excludes phantom parameters and physical
                // tags; the storage plan proves clone/release for its carrier.
                let children = stored_children(ty, definitions)
                    .map_err(|_| SnapshotRefusal::UnprovedStorage)?;
                let known_constructor = children.is_some() || definitions.contains_key(name);
                let fields = children.map(StoredChildren::into_types).unwrap_or_default();
                let mut deferred = contains_formal(ty, formals);
                for field in &fields {
                    deferred |= walk(field, definitions, use_kind, formals, active, depth + 1, remaining)?;
                }
                if (!deferred || !known_constructor)
                    && !StoragePlan::classify(ty, definitions, |_| Some(0)).admits_owned_copy()
                {
                    return Err(SnapshotRefusal::UnprovedStorage);
                }
                active.remove(ty);
                Ok(deferred)
            }
        }
    }
    SourceTypeKey { source: ty }.digest().ok_or(SnapshotRefusal::ResourceLimit)?;
    walk(ty, definitions, use_kind, formals, &mut HashSet::new(), 0, &mut 4_096)
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
    /// A checked unique owner of otherwise proved owned storage. Its physical
    /// pointee is the child's, but duplication is forbidden.
    MoveOnlyOwned(Box<StoragePlan>),
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
    AggregateView(ViewCarrierPlan, AggregateGraphPlan),
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

/// Preparation and ownership transfer for a retained view stored as one
/// sequence element. A byte-buffer view may denote static storage, so it
/// must first materialize a fresh owned buffer. Other retained roots keep
/// their authenticated token and transfer a shared reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetainedElementTransfer {
    MaterializeOwnedBuffer,
    RetainReference,
}

impl RetainedElementTransfer {
    pub fn for_element(ty: &Type, definitions: &BTreeMap<String, TypeDef>) -> Option<Self> {
        match StoragePlan::classify(ty, definitions, |_| Some(0)) {
            StoragePlan::RootedView(RootedViewStorage::RetainedRoot(RetainedRootStorage::ByteBuffer(_))) =>
                Some(Self::MaterializeOwnedBuffer),
            StoragePlan::RootedView(RootedViewStorage::RetainedRoot(_)) =>
                Some(Self::RetainReference),
            _ => None,
        }
    }

    pub fn for_sequence(source: &Type, definitions: &BTreeMap<String, TypeDef>) -> Option<Self> {
        let Ok(Some(StoredChildren::Sequence(element))) = stored_children(source, definitions) else {
            return None;
        };
        Self::for_element(&element, definitions)
    }
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
        let operations = graph_storage_operations(ty, definitions, &mut HashSet::new(), 0, &mut remaining)?.finish();
        Some(Self { source: ty.clone(), shape, source_key,
            cloneable: operations.cloneable, movable: operations.movable })
    }
}

#[derive(Clone, Copy)]
struct GraphOperations {
    cloneable: bool,
    movable: bool,
    direct_capability_field: bool,
}

impl GraphOperations {
    const COPY: Self = Self { cloneable: true, movable: true, direct_capability_field: false };
    const REFUSED: Self = Self { cloneable: false, movable: false, direct_capability_field: false };

    fn finish(mut self) -> Self {
        // Consuming deconstruction cannot transfer a capability OwnerField as
        // an output. A copyable shell can use its clone route; a shell with an
        // affine child has no such route and must refuse before lowering.
        if self.direct_capability_field && !self.cloneable { self.movable = false; }
        self
    }

    fn with(self, child: Self) -> Self {
        Self { cloneable: self.cloneable && child.cloneable,
            // A copyable field can be retained when transferring the owned
            // aggregate shell; it need not expose a unique move operation.
            movable: self.movable && (child.movable || child.cloneable),
            direct_capability_field: self.direct_capability_field || child.direct_capability_field }
    }
}

/// How a managed field crosses an aggregate-owner constructor. This is the
/// same recursive ownership judgment used by authenticated aggregate storage:
/// a noncopyable field moves when every child can move or be lawfully cloned,
/// while a field with neither operation cannot enter the owner at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagedAggregateFieldTransfer { Clone, Move }

pub fn aggregate_managed_field_transfer(
    ty: &Type,
    definitions: &BTreeMap<String, TypeDef>,
) -> Option<ManagedAggregateFieldTransfer> {
    let operations = graph_storage_operations(
        ty, definitions, &mut HashSet::new(), 0, &mut 4_096,
    )?.finish();
    if operations.cloneable {
        Some(ManagedAggregateFieldTransfer::Clone)
    } else if operations.movable {
        Some(ManagedAggregateFieldTransfer::Move)
    } else {
        None
    }
}

/// `unique` requires an owned pointer, proved by the checked physical storage
/// class. This intentionally does not recurse into the graph operation query:
/// a recursive nominal may refer to itself through this qualifier.
fn unique_owned_pointer_type(ty: &Type, definitions: &BTreeMap<String, TypeDef>) -> bool {
    if matches!(ty, Type::Qualified(..) | Type::Fn(..) | Type::Slice(..) | Type::Dyn(..)) {
        return false;
    }
    if scalar_aggregate_shape(ty, definitions).is_some_and(|shape| !shape.packed) {
        return true;
    }
    if matches!(stored_children(ty, definitions), Ok(Some(StoredChildren::Sequence(_) | StoredChildren::Mapping(_, _)))) {
        return true;
    }
    matches!(CarrierElement::from_checked_type_with_definitions(ty, definitions),
        Some(CarrierElement::String | CarrierElement::Bytes | CarrierElement::List(_)
            | CarrierElement::Dict(_, _) | CarrierElement::Tuple(_)
            | CarrierElement::Record { .. } | CarrierElement::Sum { .. }))
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
            if !unique_owned_pointer_type(inner, definitions) { return None; }
            child(inner, active, remaining).map(|operations| GraphOperations {
                cloneable: false,
                movable: operations.movable || operations.cloneable,
                direct_capability_field: operations.direct_capability_field,
            })
        }
        Type::Qualified(TypeQual::View, _) => match StoragePlan::classify(ty, definitions, |_| Some(0)) {
            StoragePlan::RootedView(storage) => match storage {
            RootedViewStorage::InlineScalar(_) => Some(GraphOperations { cloneable: true, movable: false, direct_capability_field: false }),
            RootedViewStorage::RetainedRoot(RetainedRootStorage::ByteBuffer(_)
                | RetainedRootStorage::Sequence(_)
                | RetainedRootStorage::StructuralRecord(_)
                | RetainedRootStorage::CheckedAggregate(_)) => Some(GraphOperations { cloneable: true, movable: false, direct_capability_field: false }),
            RootedViewStorage::RetainedProjection(element) if element.within_resource_bounds() =>
                Some(GraphOperations { cloneable: true, movable: false, direct_capability_field: false }),
            // A scalar-cell view carries a shared GC owner. Crossing an
            // aggregate-owner edge retains that owner; it does not move the
            // underlying scalar place.
            RootedViewStorage::ScalarCell(_) =>
                Some(GraphOperations { cloneable: true, movable: false, direct_capability_field: false }),
            RootedViewStorage::RetainedProjection(_) => None,
            },
            _ => None,
        },
        Type::Qualified(_, _) | Type::Slice(_) | Type::Dyn(_, _) | Type::RecordCompose { .. } => None,
        Type::Fn(_, _, _, qualifiers) => {
            CallableStoragePlan::classify(ty, definitions).map(|_| GraphOperations {
                cloneable: !qualifiers.once, movable: !qualifiers.once,
                direct_capability_field: false,
            })
        }
        Type::Named(_, _) | Type::Tuple(_) => {
            // The compiler-private erased message lane is a retained GC
            // envelope. Existing scheduler reads clone that envelope, while
            // the private source bridge and typed endpoint separately check
            // the payload's authority. Admit the envelope's existing clone
            // operation inside an aggregate; an affine sibling still makes
            // the aggregate move-only.
            if crate::is_erased_message_type(ty) {
                return Some(GraphOperations::COPY);
            }
            if let Some(children @ (StoredChildren::Sequence(_) | StoredChildren::Mapping(_, _))) =
                stored_children(ty, definitions).ok().flatten()
            {
                return children.into_types().iter().try_fold(GraphOperations::COPY, |all, field| {
                    Some(all.with(child(field, active, remaining)?))
                });
            }
            if let Some(element) = CarrierElement::from_checked_type(ty) {
                return Some(if matches!(element.ownership(), crate::carrier::CarrierOwnership::Copy
                    | crate::carrier::CarrierOwnership::RetainCloneRelease) {
                    GraphOperations::COPY
                } else { GraphOperations::REFUSED });
            }
            if let Some(representation) = crate::capabilities::representation(ty, definitions) {
                // A checked direct capability is an immutable host-minted
                // token. Copying it inside an owned aggregate delegates the
                // same authority; it does not mint a new capability. A mixed
                // affine owner shell cannot consume/deconstruct the token.
                // Nullable capability fields lack an aggregate capture layout.
                return Some(match representation {
                    crate::capabilities::CapabilityRepresentation::Direct(_) => GraphOperations {
                        cloneable: true, movable: false, direct_capability_field: true,
                    },
                    crate::capabilities::CapabilityRepresentation::Nullable(_) => GraphOperations::REFUSED,
                });
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

/// Facts about actual stored children of one source type. Phantom arguments and
/// callable signatures do not occupy storage; an unknown nominal still needs
/// its independent carrier proof before it can be admitted.
#[derive(Clone, Copy, Default)]
struct StoredContainment {
    opaque_view_owner: bool,
    capability_authority: bool,
    unique_storage: bool,
}

#[derive(Clone, Copy)]
struct ContainmentDemand {
    opaque_view_owner: bool,
    capability_authority: bool,
}

impl ContainmentDemand {
    const ALL: Self = Self { opaque_view_owner: true, capability_authority: true };
    const UNIQUE: Self = Self { opaque_view_owner: false, capability_authority: false };
}

/// Whether a source type owns unique storage in an instantiated stored child.
/// Views, slices, and borrowed handles do not own their referents. `None`
/// means the finite stored-child proof was unavailable and must not authorize
/// an ordinary copy.
pub fn contains_unique_storage(
    ty: &Type,
    definitions: &BTreeMap<String, TypeDef>,
) -> Option<bool> {
    stored_containment(ty, definitions, ContainmentDemand::UNIQUE).map(|facts| facts.unique_storage)
}

fn stored_containment(
    ty: &Type,
    definitions: &BTreeMap<String, TypeDef>,
    demand: ContainmentDemand,
) -> Option<StoredContainment> {
    fn walk(
        ty: &Type,
        definitions: &BTreeMap<String, TypeDef>,
        demand: ContainmentDemand,
        active: &mut HashSet<Type>,
        depth: usize,
        remaining: &mut usize,
    ) -> Option<StoredContainment> {
        if depth > 128 || *remaining == 0 { return None; }
        *remaining -= 1;
        let mut facts = StoredContainment {
            // Computing a view plan recursively inspects the stored graph.
            // Skip unused projections, not children: the outer walk must keep
            // its exact cycle, resource-limit, and malformed-storage refusals.
            opaque_view_owner: demand.opaque_view_owner && view_carrier_plan(ty, definitions)
                .is_some_and(|plan| plan.requires_opaque_owner()),
            capability_authority: demand.capability_authority
                && crate::capabilities::representation(ty, definitions).is_some(),
            unique_storage: matches!(ty, Type::Qualified(TypeQual::Unique, _)),
        };
        let owns_referent = !matches!(ty,
            Type::Qualified(TypeQual::View | TypeQual::Borrow(_) | TypeQual::BorrowMut(_)
                | TypeQual::LegacyBorrow(_), _) | Type::Slice(_));
        if !active.insert(ty.clone()) { return Some(facts); }
        // A qualifier is transparent to stored shape lookup, but Unique is
        // itself an ownership fact. Visit each wrapper before descending into
        // the base shape so frozen unique T cannot erase the unique proof.
        let children = match ty {
            Type::Qualified(_, inner) | Type::Slice(inner) => vec![*inner.clone()],
            _ => stored_children(ty, definitions).ok()?
                .map(StoredChildren::into_types).unwrap_or_default(),
        };
        for child in &children {
            let nested = walk(child, definitions, demand, active, depth + 1, remaining)?;
            facts.opaque_view_owner |= nested.opaque_view_owner;
            facts.capability_authority |= nested.capability_authority;
            facts.unique_storage |= owns_referent && nested.unique_storage;
        }
        active.remove(ty);
        Some(facts)
    }
    walk(ty, definitions, demand, &mut HashSet::new(), 0, &mut 4_096)
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
    opaque_callable_families: std::sync::Arc<HashSet<Type>>,
    owner_classes: OwnerClassMemo,
}

/// `owner_class` answers, shared by every attachment of one evidence. The
/// judgment is a pure function of the type, the definitions map and the
/// callable families, and all three are fixed once the evidence is built:
/// the map by the address binding, the families because the fixpoint in
/// `from_checked_source` (which asks only `requires_owner`) has finished.
/// Lowering asks it for the same few aggregate types on every constructor;
/// unmemoized it was 7% of an async program's compile (2026-09-26).
type OwnerClassMemo = std::sync::Arc<std::sync::Mutex<std::collections::HashMap<Type, Option<OpaqueOwnerClass>>>>;

/// [`OpaqueOwnerEvidence`] without its definitions borrow. Only
/// `OpaqueOwnerEvidence::detach` makes one, and `reattach` accepts it only
/// for the same definitions map address. The address is the whole binding,
/// so a detached value must not outlive the map it was judged over: another
/// map allocated at the same address afterwards would be accepted. Keep it
/// inside the borrow of that map (lowering holds it for one module).
#[derive(Debug, Clone)]
pub struct DetachedOpaqueOwnerEvidence {
    definitions: usize,
    opaque_callable_families: std::sync::Arc<HashSet<Type>>,
    owner_classes: OwnerClassMemo,
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
        let mut evidence = Self {
            definitions,
            opaque_callable_families: Default::default(),
            owner_classes: Default::default(),
        };
        loop {
            let mut changed = false;
            for (family, captures) in &families {
                if captures.iter().any(|capture| evidence.requires_owner(capture)) {
                    changed |= std::sync::Arc::make_mut(&mut evidence.opaque_callable_families).insert(family.clone());
                }
            }
            if !changed { break; }
        }
        Some(evidence)
    }

    /// Keep this evidence past the borrow of its definitions, e.g. in a
    /// lowering context that asks `owner_class` for many types. Rebuilding it
    /// per question re-ran the whole closure-family fixpoint each time.
    pub fn detach(&self) -> DetachedOpaqueOwnerEvidence {
        DetachedOpaqueOwnerEvidence {
            definitions: std::ptr::from_ref(self.definitions) as usize,
            opaque_callable_families: std::sync::Arc::clone(&self.opaque_callable_families),
            owner_classes: std::sync::Arc::clone(&self.owner_classes),
        }
    }

    /// Rebind detached evidence to the definitions it was derived from.
    /// `None` for any other map: the evidence is a judgment over one map and
    /// must be rebuilt for another.
    pub fn reattach(
        definitions: &'a BTreeMap<String, TypeDef>,
        detached: &DetachedOpaqueOwnerEvidence,
    ) -> Option<Self> {
        (detached.definitions == std::ptr::from_ref(definitions) as usize).then(|| Self {
            definitions,
            opaque_callable_families: std::sync::Arc::clone(&detached.opaque_callable_families),
            owner_classes: std::sync::Arc::clone(&detached.owner_classes),
        })
    }

    pub fn requires_owner(&self, ty: &Type) -> bool {
        self.requires_owner_at(ty, &mut HashSet::new(), 0, &mut 16_384) == Some(true)
    }

    /// The actual carrier at this edge. `requires_owner` answers whether a
    /// stored descendant needs an owner; this query answers whether the value
    /// itself occupies an opaque owner lane. Keeping the two judgments apart
    /// prevents a linear outer collection from inheriting a child's GC ABI.
    pub fn owner_class(&self, ty: &Type) -> Option<OpaqueOwnerClass> {
        if let Some(known) = self.owner_classes.lock().expect("owner-class memo").get(ty) {
            return *known;
        }
        let class = self.owner_class_uncached(ty);
        self.owner_classes.lock().expect("owner-class memo").insert(ty.clone(), class);
        class
    }

    fn owner_class_uncached(&self, ty: &Type) -> Option<OpaqueOwnerClass> {
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
    if arms.is_empty() || arms.iter().any(|arm| arm.guard.is_some()
        || consuming_aggregate_pattern_shape(ty, &arm.pattern, definitions).is_none()) {
        return None;
    }
    consuming_aggregate_pattern_shape(ty, &arms[0].pattern, definitions)
}

/// One consuming pattern transfers each affine field to exactly one binding.
/// Irrefutability remains a separate checker obligation for `let` patterns;
/// match arms additionally require coverage and an unguarded selected arm.
pub fn consuming_aggregate_pattern_shape(
    ty: &Type,
    pattern: &Pattern,
    definitions: &BTreeMap<String, TypeDef>,
) -> Option<ScalarAggregateShape> {
    affine_storage_shape(ty, definitions)?;
    let shape = scalar_aggregate_shape(ty, definitions)?;
    let (args, first_field, arity) = match pattern {
        Pattern::Ctor { name, args } | Pattern::AnonCtor { tag: name, args } => {
            let (_, variant) = shape.variant(name)?;
            (args, variant.first_field, variant.arity)
        }
        Pattern::Tuple(args) if matches!(ty.unqualified(), Type::Tuple(_))
            && !shape.tagged && shape.variants.len() == 1
            && shape.variants[0].name.is_empty() =>
            (args, 0, shape.fields.len()),
        _ => return None,
    };
    if args.len() != arity || args.iter().enumerate().any(|(offset, arg)| {
        let field = &shape.fields[first_field + offset];
        match arg {
            Pattern::Var(name) if name != "_" => false,
            Pattern::Var(_) | Pattern::Wildcard => affine_storage_shape(field, definitions).is_some(),
            _ => true,
        }
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

/// A tagged sum has inactive slots; an affine record needs empty slots after
/// consuming extraction. Both use the same nullable owned-pointer layout and
/// the same checked take operation. This is a source storage judgment, not a
/// property inferred from a candidate layout.
pub fn aggregate_uses_nullable_owned_fields(
    ty: &Type,
    definitions: &BTreeMap<String, TypeDef>,
) -> bool {
    scalar_aggregate_shape(ty, definitions).is_some_and(|shape| {
        shape.tagged || matches!(affine_storage_shape(ty, definitions), Some(AffineStorageShape::NullableFields(_)))
    })
}

pub fn affine_storage_shape(
    ty: &Type,
    definitions: &BTreeMap<String, TypeDef>,
) -> Option<AffineStorageShape> {
    if let Type::Qualified(TypeQual::Unique, inner) = ty {
        let mut remaining = 16_384;
        if !within_source_bounds(ty, definitions, &mut BTreeSet::new(), 0, &mut remaining) {
            return None;
        }
        if let Some(shape) = affine_storage_shape(inner, definitions) {
            return Some(shape);
        }
    }
    fn unique_leaf(ty: &Type, definitions: &BTreeMap<String, TypeDef>) -> Option<AffineStorageShape> {
        let mut storage = ty;
        for _ in 0..128 {
            let Type::Qualified(TypeQual::Frozen, inner) = storage else { break };
            storage = inner;
        }
        let Type::Qualified(TypeQual::Unique, inner) = storage else { return None; };
        if !unique_owned_pointer_type(inner, definitions) { return None; }
        graph_storage_operations(ty, definitions, &mut HashSet::new(), 0, &mut 4_096)
            .filter(|operations| operations.finish().movable)
            .map(|_| AffineStorageShape::Leaf)
    }
    fn walk(
        plan: &StoragePlan, definitions: &BTreeMap<String, TypeDef>,
        active: &mut HashSet<Type>, depth: usize, remaining: &mut usize,
    ) -> Option<AffineStorageShape> {
        if depth > 128 || *remaining == 0 || !plan.requires_affine_owner_token() { return None; }
        *remaining -= 1;
        if let StoragePlan::MoveOnlyOwned(child) = plan {
            return if child.requires_affine_owner_token() {
                walk(child, definitions, active, depth + 1, remaining)
            } else {
                Some(AffineStorageShape::Leaf)
            };
        }
        let source = match plan {
            StoragePlan::MoveOnlyList(sequence) => sequence.source(),
            StoragePlan::AuthenticatedPlace(graph) | StoragePlan::AggregateView(_, graph) => graph.source(),
            _ => return None,
        };
        if !active.insert(source.clone()) { return None; }
        let result = match plan {
            StoragePlan::MoveOnlyList(sequence) => Some(AffineStorageShape::Sequence(Box::new(
                walk(sequence.element(), definitions, active, depth + 1, remaining)?,
            ))),
            StoragePlan::AuthenticatedPlace(graph) | StoragePlan::AggregateView(_, graph)
                if graph.movable() && !graph.cloneable() => {
                let shape = graph.shape();
                if shape.packed { return None; }
                let mut children = Vec::new();
                for (index, field) in shape.fields.iter().enumerate() {
                    if *remaining == 0 { return None; }
                    *remaining -= 1;
                    if let Some(leaf) = unique_leaf(field, definitions) {
                        children.push((u32::try_from(index).ok()?, leaf));
                        continue;
                    }
                    let child = StoragePlan::classify(field, definitions, |_| Some(0));
                    if child.requires_affine_owner_token() {
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
    if let Some(leaf) = unique_leaf(ty, definitions) { return Some(leaf); }
    let plan = StoragePlan::classify(ty.unqualified(), definitions, |_| Some(0));
    walk(&plan, definitions, &mut HashSet::new(), 0, &mut 4_096)
}

/// The HM type corresponding to a proved affine aggregate. This erases
/// ownership qualifiers for checked type equality only; physical layout
/// identity retains the child qualifiers through `aggregate_layout_identity`.
pub fn affine_aggregate_erased_type(
    ty: &Type,
    definitions: &BTreeMap<String, TypeDef>,
) -> Option<Type> {
    if matches!(ty, Type::Qualified(..)) || affine_storage_shape(ty, definitions).is_none() {
        return None;
    }
    fn erase(ty: &Type, definitions: &BTreeMap<String, TypeDef>, depth: usize, remaining: &mut usize) -> Option<Type> {
        if depth > 128 || *remaining == 0 { return None; }
        *remaining -= 1;
        match ty {
            Type::Qualified(TypeQual::Frozen, inner) =>
                erase(inner, definitions, depth + 1, remaining),
            Type::Qualified(TypeQual::Unique, inner)
                if affine_storage_shape(ty, definitions).is_some() =>
                    erase(inner, definitions, depth + 1, remaining),
            Type::Qualified(..) | Type::Fn(..) | Type::Dyn(..) | Type::RecordCompose { .. } => None,
            Type::Named(name, arguments) => Some(Type::Named(name.clone(), arguments.iter()
                .map(|argument| erase(argument, definitions, depth + 1, remaining))
                .collect::<Option<Vec<_>>>()?)),
            Type::Tuple(fields) => Some(Type::Tuple(fields.iter()
                .map(|field| erase(field, definitions, depth + 1, remaining))
                .collect::<Option<Vec<_>>>()?)),
            Type::Slice(inner) => Some(Type::Slice(Box::new(erase(inner, definitions, depth + 1, remaining)?))),
        }
    }
    erase(ty, definitions, 0, &mut 4_096)
}

/// Checked HM erasure for either a root affine owner or an aggregate carrying
/// one. This is a type-equality witness, never permission to clone a token.
pub fn affine_storage_erased_type(
    ty: &Type,
    definitions: &BTreeMap<String, TypeDef>,
) -> Option<Type> {
    affine_storage_shape(ty, definitions)?;
    match ty {
        Type::Qualified(TypeQual::Unique, inner) => Some((**inner).clone()),
        _ => affine_aggregate_erased_type(ty, definitions),
    }
}

/// Source-bound layout identity for a recursively proved retained child.
/// Its qualifier changes ownership operations even when the physical slot
/// matches the unqualified referent.
pub fn retained_child_layout_identity(
    ty: &Type,
    definitions: &BTreeMap<String, TypeDef>,
) -> Option<Type> {
    if !matches!(ty, Type::Qualified(..))
        && CarrierElement::from_checked_type_with_definitions(ty, definitions)
            .is_some_and(|carrier| carrier.contains_retained_root_view())
    {
        // Retained children share their referent's physical slot, but their
        // clone and release obligations differ. Keep that checked relation
        // through every aggregate identity, including dictionaries and tuples.
        return Some(ty.clone());
    }
    if let Type::Qualified(TypeQual::Unique, inner) = ty
        && matches!(StoragePlan::classify(ty, definitions, |_| Some(0)), StoragePlan::MoveOnlyOwned(_))
        && CarrierElement::from_checked_type_with_definitions(inner, definitions)
            .is_some_and(|carrier| carrier.contains_retained_root_view())
    {
        return retained_child_layout_identity(inner, definitions);
    }
    None
}

/// Identity for linear-memory storage with affine or retained descendants.
/// The physical layout and SafeWIR correspondence both consume this identity.
pub fn aggregate_layout_identity(
    ty: &Type,
    definitions: &BTreeMap<String, TypeDef>,
) -> Option<Type> {
    if let Some(identity) = retained_child_layout_identity(ty, definitions) {
        return Some(identity);
    }
    if let Type::Qualified(TypeQual::Unique, inner) = ty
        && affine_storage_shape(ty, definitions).is_some()
        && affine_storage_shape(inner, definitions).is_some()
    {
        return aggregate_layout_identity(inner, definitions);
    }
    if !matches!(ty, Type::Qualified(..)) && affine_storage_shape(ty, definitions).is_some() {
        Some(ty.unqualified().clone())
    } else {
        crate::aggregates::canonical_owner_aggregate_identity(ty, definitions)
    }
}

impl StoragePlan {
    /// A checked sequence whose physical representation is a packed list.
    /// The element's retain, clone, move, and release judgment is already
    /// encoded by the selected plan; layout consumers must still validate the
    /// actual element slot and reference-counted header.
    pub fn has_packed_sequence_layout(&self) -> bool {
        matches!(self,
            Self::Owned(CarrierElement::List(_))
                | Self::RetainedOwnerList(_)
                | Self::RetainedGraphList(_)
                | Self::RetainedCallableList(_)
                | Self::RetainedOpaqueAggregateList(_)
                | Self::MoveOnlyList(_))
    }

    /// An independent owned copy can be made from this checked storage.
    /// A view-bearing aggregate requires its recursive graph clone proof;
    /// a bare view has no generic owned-copy permission.
    pub fn admits_owned_copy(&self) -> bool {
        match self {
            Self::InlineScalar(_) | Self::InlineAggregate(_) | Self::Owned(_)
            | Self::RetainedCallable(_) | Self::RetainedCallableList(_)
            | Self::RetainedOwnerList(_) => true,
            Self::AuthenticatedPlace(plan) | Self::AggregateView(_, plan) | Self::RetainedGraphList(plan)
            | Self::RetainedOpaqueAggregateList(plan) => plan.cloneable(),
            Self::MoveOnlyOwned(_) | Self::MoveOnlyList(_) | Self::RootedView(_)
            | Self::Refused(_) => false,
        }
    }

    /// A copy of this representation allocates an independent linear owner.
    /// Opaque GC references can gain a second retained token, but that token
    /// does not prove a fresh unique root for mutation.
    pub fn materializes_fresh_linear_owner(&self) -> bool {
        matches!(self, Self::Owned(_))
    }

    /// A copy also severs a call-scoped origin only when no callable capture
    /// or opaque aggregate field can hide another borrowed origin.
    pub fn admits_call_scoped_copy(&self) -> bool {
        matches!(self, Self::InlineScalar(_) | Self::InlineAggregate(_) | Self::Owned(_))
    }

    /// An affine aggregate has exactly one transferable owner token. The
    /// source graph proves moves and recursive release but supplies no clone.
    pub fn requires_affine_owner_token(&self) -> bool {
        matches!(self, Self::MoveOnlyOwned(_) | Self::MoveOnlyList(_))
            || matches!(self, Self::AuthenticatedPlace(plan) | Self::AggregateView(_, plan)
                if plan.movable() && !plan.cloneable())
    }

    /// A list can duplicate initialized elements. Require a copyable owned
    /// carrier, a non-once callable, or a cloneable authenticated graph.
    /// Other storage plans retain their own representation but cannot become
    /// an ordinary packed-list element through a second shape table.
    pub fn list_element_disposition(&self, ty: &Type) -> Option<ListElementDisposition> {
        match self {
            Self::InlineScalar(_) | Self::Owned(_) => Some(ListElementDisposition::Cloneable),
            Self::RootedView(RootedViewStorage::RetainedRoot(_)) =>
                Some(ListElementDisposition::Cloneable),
            Self::RetainedCallable(_) if matches!(ty, Type::Fn(_, _, _, qualifiers) if !qualifiers.once) =>
                Some(ListElementDisposition::Cloneable),
            Self::AuthenticatedPlace(plan) if plan.cloneable() => Some(ListElementDisposition::Cloneable),
            Self::AuthenticatedPlace(plan) if plan.movable() => Some(ListElementDisposition::MoveOnly),
            Self::RetainedGraphList(plan) if plan.cloneable() => Some(ListElementDisposition::Cloneable),
            Self::MoveOnlyList(_) => Some(ListElementDisposition::MoveOnly),
            Self::MoveOnlyOwned(_) => Some(ListElementDisposition::MoveOnly),
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
        // `nominal` is consulted only for a view reached through the
        // top-level `List(...)` element chain. Without one, the plan is a
        // pure function of the type and the map, so the open shape-memo scope
        // for that map may answer it.
        if !Self::consults_nominal(ty) {
            if let Some(plan) = crate::aggregates::shape_memo::storage_plan(definitions, ty) {
                return plan;
            }
            let plan = Self::classify_inner(ty, definitions, &nominal);
            crate::aggregates::shape_memo::remember_storage_plan(definitions, ty, &plan);
            return plan;
        }
        Self::classify_inner(ty, definitions, &nominal)
    }

    /// Whether `classify_inner` can reach `RootedViewStorage::classify`,
    /// the only reader of `nominal`: a view here or down the `List` element
    /// chain `classify_inner` recurses along.
    fn consults_nominal(ty: &Type) -> bool {
        match ty {
            Type::Qualified(TypeQual::View, _) => true,
            Type::Named(name, arguments) if name == "List" => {
                matches!(arguments.as_slice(), [element] if Self::consults_nominal(element))
            }
            _ => false,
        }
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
        if let Type::Qualified(TypeQual::Unique, inner) = ty {
            if !unique_owned_pointer_type(inner, definitions)
                || !graph_storage_operations(ty, definitions, &mut HashSet::new(), 0, &mut 4_096)
                    .is_some_and(|operations| operations.finish().movable)
            {
                return Self::Refused(StorageRefusal::UnprovedOwnership);
            }
            let child = Self::classify_inner(inner, definitions, nominal);
            return if matches!(child, Self::Owned(_) | Self::AuthenticatedPlace(_)
                | Self::MoveOnlyList(_) | Self::RetainedGraphList(_)) {
                Self::MoveOnlyOwned(Box::new(child))
            } else {
                Self::Refused(StorageRefusal::UnprovedOwnership)
            };
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
            return match stored_containment(element, definitions, ContainmentDemand::ALL) {
                Some(StoredContainment { opaque_view_owner: true, .. }) => Self::RetainedOpaqueAggregateList(plan),
                Some(_) => Self::RetainedGraphList(plan),
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
            return AggregateGraphPlan::classify(ty, definitions)
                .map(|graph| Self::AggregateView(plan, graph))
                .unwrap_or(Self::Refused(StorageRefusal::UnprovedOwnership));
        }
        match CarrierElement::from_checked_type_with_definitions(ty, definitions) {
            Some(CarrierElement::Scalar(kind)) if kind != CanonicalScalarKind::Unit => {
                Self::InlineScalar(kind)
            }
            Some(plan) if plan.within_resource_bounds() => Self::Owned(plan),
            _ => AggregateGraphPlan::classify(ty, definitions)
                .map(Self::AuthenticatedPlace)
                .unwrap_or_else(|| match stored_containment(ty, definitions, ContainmentDemand::ALL) {
                    Some(StoredContainment { capability_authority: true, .. }) => Self::Refused(StorageRefusal::UnprovedAuthority),
                    Some(_) => Self::Refused(StorageRefusal::UnprovedOwnership),
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

#[cfg(test)]
mod query_tests;

#[cfg(test)]
#[test]
fn erased_scheduler_lane_validates_task_signature_without_copy_authority() {
    let source = witchy_syntax::linker::bundled_source("task").unwrap();
    let module = witchy_syntax::parser::parse_module(source).unwrap();
    let mut definitions = crate::aggregates::type_definitions(&module);
    let option = witchy_syntax::parser::parse_module(witchy_syntax::linker::bundled_source("option").unwrap()).unwrap();
    definitions.extend(crate::aggregates::type_definitions(&option));
    let ty = Type::Named("Task".into(), vec![Type::Named("Int".into(), vec![])]);
    assert!(replay_capture_eligibility(&ty, &definitions).is_ok());
    let step = Type::Named("Step".into(), vec![Type::Named("Int".into(), vec![])]);
    assert!(replay_capture_eligibility(&step, &definitions).is_err());
    let affine_step = Type::Named("Step".into(), vec![Type::Named("Handle".into(), vec![])]);
    assert!(matches!(StoragePlan::classify(&affine_step, &definitions, |_| Some(0)),
        StoragePlan::AuthenticatedPlace(graph) if graph.movable() && !graph.cloneable()));
    assert!(matches!(affine_storage_shape(&affine_step, &definitions),
        Some(AffineStorageShape::NullableFields(_))));
    assert!(!crate::is_erased_message_type(&Type::Named("__Msg".into(), vec![
        Type::Named("Int".into(), vec![]),
    ])));
    assert!(!crate::is_erased_message_type(&Type::Qualified(
        TypeQual::View, Box::new(Type::Named("__Msg".into(), vec![])),
    )));
}
