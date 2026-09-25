//! One typed source-storage classification shared across the checked frontend
//! and SafeWIR. A plan describes storage and ownership operations; it is not
//! authority without a checked-source witness and physical layout match.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use witchy_syntax::ast::{Type, TypeDef, TypeQual};

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
}

impl AggregateGraphPlan {
    pub fn source(&self) -> &Type { &self.source }
    pub fn shape(&self) -> &ScalarAggregateShape { &self.shape }
    pub fn source_key(&self) -> [u8; 32] { self.source_key }
    pub fn cloneable(&self) -> bool { self.cloneable }

    fn classify(ty: &Type, definitions: &BTreeMap<String, TypeDef>) -> Option<Self> {
        if crate::capabilities::representation(ty, definitions).is_some() { return None; }
        let shape = scalar_aggregate_shape(ty, definitions)?;
        if shape.packed { return None; }
        let source_key = SourceTypeKey { source: ty }.digest()?;
        let mut remaining = 4_096;
        let cloneable = graph_storage_operations(ty, definitions, &mut HashSet::new(), 0, &mut remaining)?;
        Some(Self { source: ty.clone(), shape, source_key, cloneable })
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
) -> Option<bool> {
    if depth > 128 || *remaining == 0 { return None; }
    *remaining -= 1;
    let child = |ty: &Type, active: &mut HashSet<Type>, remaining: &mut usize| {
        graph_storage_operations(ty, definitions, active, depth + 1, remaining)
    };
    match ty {
        Type::Qualified(TypeQual::Frozen, inner) => child(inner, active, remaining),
        Type::Qualified(TypeQual::Unique, inner) => {
            child(inner, active, remaining).map(|_| false)
        }
        Type::Qualified(TypeQual::View, payload) => match RootedViewStorage::classify(payload, definitions, |_| Some(0))? {
            RootedViewStorage::InlineScalar(_) => Some(true),
            RootedViewStorage::RetainedRoot(RetainedRootStorage::ByteBuffer(_)
                | RetainedRootStorage::Sequence(_)
                | RetainedRootStorage::StructuralRecord(_)
                | RetainedRootStorage::CheckedAggregate(_)) => Some(true),
            RootedViewStorage::ScalarCell(_) | RootedViewStorage::RetainedProjection(_) => None,
        },
        Type::Qualified(_, _) | Type::Slice(_) | Type::Dyn(_, _) | Type::RecordCompose { .. } => None,
        Type::Fn(_, _, _, qualifiers) => {
            CallableStoragePlan::classify(ty, definitions).map(|_| !qualifiers.once)
        }
        Type::Named(name, arguments) if name == "List" => {
            let [element] = arguments.as_slice() else { return None };
            child(element, active, remaining).filter(|cloneable| *cloneable)
        }
        Type::Named(name, arguments) if name == "Dict" => {
            let [key, value] = arguments.as_slice() else { return None };
            (child(key, active, remaining)? && child(value, active, remaining)?)
                .then_some(true)
        }
        Type::Named(_, _) | Type::Tuple(_) => {
            if view_carrier_plan(ty, definitions).is_some_and(|plan| plan.requires_opaque_owner()) {
                return Some(opaque_aggregate_cloneable(ty, definitions));
            }
            if let Some(element) = CarrierElement::from_checked_type(ty) {
                return Some(matches!(element.ownership(), crate::carrier::CarrierOwnership::Copy
                    | crate::carrier::CarrierOwnership::RetainCloneRelease));
            }
            if crate::capabilities::representation(ty, definitions).is_some() {
                return Some(false);
            }
            let shape = scalar_aggregate_shape(ty, definitions)?;
            if shape.packed { return None; }
            if !active.insert(ty.clone()) { return Some(true); }
            // Instantiated fields, not phantom generic arguments, determine
            // storage operations. The source key still binds every argument.
            let fields_cloneable = shape.fields.iter().try_fold(true, |all, field| {
                    let proved = child(field, active, remaining)?;
                    Some(all && proved)
            });
            active.remove(ty);
            let must_consume = match ty {
                Type::Named(name, _) => definitions.get(name).is_some_and(|definition| definition.must_consume),
                _ => false,
            };
            fields_cloneable.map(|cloneable| cloneable && !must_consume)
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
            return RootedViewStorage::classify(payload, definitions, |_| Some(0)).is_some();
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
            let fields = if let Some(shape) = scalar_aggregate_shape(ty, definitions) {
                shape.fields
            } else {
                match ty {
                    Type::Named(_, arguments) | Type::Tuple(arguments) => arguments.clone(),
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
        let children = if let Some(shape) = scalar_aggregate_shape(ty, definitions) {
            shape.fields
        } else if let Some(fields) = instantiated_declared_fields(ty, definitions) {
            fields.ok()?
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
            let children = if let Some(shape) = scalar_aggregate_shape(ty, self.definitions) {
                shape.fields
            } else {
                match ty.unqualified() {
                    Type::Named(name, arguments) if name == "List" && arguments.len() == 1 => arguments.clone(),
                    Type::Named(name, arguments) if name == "Dict" && arguments.len() == 2 => arguments.clone(),
                    Type::Tuple(items) => items.clone(),
                    _ => Vec::new(),
                }
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

impl StoragePlan {
    /// A list can duplicate initialized elements. Require a copyable owned
    /// carrier, a non-once callable, or a cloneable authenticated graph.
    /// Other storage plans retain their own representation but cannot become
    /// an ordinary packed-list element through a second shape table.
    pub fn admits_list_element(&self, ty: &Type) -> bool {
        match self {
            Self::InlineScalar(_) | Self::Owned(_) => true,
            Self::RetainedCallable(_) => matches!(ty, Type::Fn(_, _, _, qualifiers) if !qualifiers.once),
            Self::AuthenticatedPlace(plan) => plan.cloneable(),
            _ => false,
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
        // `view_carrier_plan` traverses checked nominal definitions before the
        // owned-carrier parser applies its own limits. Bound that traversal as
        // well, including repeated visits through distinct source branches.
        let mut remaining = 16_384;
        if !within_source_bounds(ty, definitions, &mut BTreeSet::new(), 0, &mut remaining) {
            return Self::Refused(StorageRefusal::ResourceLimit);
        }
        if let Type::Qualified(TypeQual::View, payload) = ty {
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
