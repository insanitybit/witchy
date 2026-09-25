//! One typed source-storage classification shared across the checked frontend
//! and SafeWIR. A plan describes storage and ownership operations; it is not
//! authority without a checked-source witness and physical layout match.

use std::collections::{BTreeMap, BTreeSet};

use witchy_syntax::ast::{Type, TypeDef, TypeQual};

use crate::abi::CanonicalScalarKind;
use crate::aggregates::{ViewCarrierPlan, view_carrier_plan};
use crate::carrier::CarrierElement;
use crate::rooted_view::{OwnerCarriedViewPlan, OwnerCarriedViewStorage, RootedViewStorage};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageRefusal {
    ResourceLimit,
    UnprovedView,
    UnprovedQualifier,
    UnprovedOwnership,
}

/// A finite source-derived proof for the storage classes the canonical route
/// currently understands. An owned value retains, clones, and releases its
/// recursive `CarrierElement`; a view or view-bearing aggregate has its own
/// authenticated owner plan. Missing proof remains an explicit refusal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoragePlan {
    InlineScalar(CanonicalScalarKind),
    Owned(CarrierElement),
    /// A list of authenticated scalar-place owner tokens. Projected-view
    /// lists need a different owner-family proof and remain refused.
    RetainedOwnerList(OwnerCarriedViewPlan),
    RootedView(RootedViewStorage),
    AggregateView(ViewCarrierPlan),
    Refused(StorageRefusal),
}

impl StoragePlan {
    /// The recursive owned carrier shared by checking and lowering. Unit has
    /// no element slot; views and opaque owner plans need their own authority.
    pub fn into_carrier_element(self) -> Option<CarrierElement> {
        match self {
            Self::InlineScalar(kind) if kind != CanonicalScalarKind::Unit => {
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
        let mut remaining = 4_096;
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
        if matches!(ty, Type::Tuple(fields) if fields.is_empty()) {
            return Self::InlineScalar(CanonicalScalarKind::Unit);
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
        if let Some(plan) = view_carrier_plan(ty, definitions) {
            return Self::AggregateView(plan);
        }
        match CarrierElement::from_checked_type_with_definitions(ty, definitions) {
            Some(CarrierElement::Scalar(kind)) if kind != CanonicalScalarKind::Unit => {
                Self::InlineScalar(kind)
            }
            Some(plan) if plan.within_resource_bounds() => Self::Owned(plan),
            _ => Self::Refused(StorageRefusal::UnprovedOwnership),
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
