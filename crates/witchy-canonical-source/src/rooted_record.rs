//! Typed identity for a retained record-root view's source payload.
//!
//! The checked frontend supplies nominal ordinals. This module only describes
//! the finite structural relation; it cannot mint a checked-source witness.

use std::collections::BTreeMap;

use witchy_syntax::ast::{Type, TypeDef, TypeQual};

use crate::carrier::CarrierElement;
use crate::structural_identity::SourceTypeKey;

/// Identify the source heads that seek retained record-root storage. A bad
/// arity still returns `Some(true)` so the full classifier can refuse it;
/// an oversized qualifier chain returns `None` and must fail closed.
pub fn has_record_root_head(ty: &Type, definitions: &BTreeMap<String, TypeDef>) -> Option<bool> {
    let mut bare = ty;
    let mut qualifiers = 0usize;
    while let Type::Qualified(_, inner) = bare {
        qualifiers += 1;
        if qualifiers > 128 {
            return None;
        }
        bare = inner;
    }
    Some(matches!(bare, Type::Named(name, _) if definitions.contains_key(name)))
}

/// One checked record-root classification: canonical ABI identity together
/// with the exact source fields whose physical layout SafeWIR must match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordRootPlan {
    pub identity: RecordRootIdentity,
    pub shape: crate::aggregates::ScalarAggregateShape,
}

impl RecordRootPlan {
    pub fn from_checked_type(
        ty: &Type,
        definitions: &BTreeMap<String, TypeDef>,
        nominal: impl Fn(&str) -> Option<u32>,
    ) -> Option<Self> {
        if !crate::aggregates::is_record_root_payload(ty, definitions) {
            return None;
        }
        let identity = RecordRootIdentity::from_checked_type(ty, definitions, nominal)?;
        let shape = crate::aggregates::scalar_aggregate_shape(ty, definitions)?;
        Some(Self { identity, shape })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordRootIdentity {
    /// A recursively proved storage leaf, with one structural encoding for
    /// scalar, byte-buffer, list, tuple, and dictionary payloads.
    Leaf(CarrierElement),
    Qualified { qualifier: RecordRootQualifier, inner: Box<Self> },
    Nominal { ordinal: u32, storage: Box<CarrierElement>, field_types: Vec<[u8; 32]> },
}

/// Only storage qualifiers accepted by the checked record-root grammar.
/// Views and retired borrow syntax have separate authenticated carriers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordRootQualifier { Frozen, Unique }

impl RecordRootIdentity {
    pub fn from_checked_type(
        ty: &Type,
        definitions: &BTreeMap<String, TypeDef>,
        nominal: impl Fn(&str) -> Option<u32>,
    ) -> Option<Self> {
        let mut budget = 4_096;
        Self::parse(ty, definitions, &nominal, 0, &mut budget).map(|(identity, _)| identity)
    }

    fn parse(
        ty: &Type,
        definitions: &BTreeMap<String, TypeDef>,
        nominal: &impl Fn(&str) -> Option<u32>,
        depth: usize,
        budget: &mut usize,
    ) -> Option<(Self, bool)> {
        if depth > 128 || *budget == 0 {
            return None;
        }
        *budget -= 1;
        let mut unqualified = ty;
        let mut qualifiers = Vec::new();
        let mut depth = depth;
        while let Type::Qualified(qualifier, inner) = unqualified {
            if depth >= 128 || *budget == 0 {
                return None;
            }
            qualifiers.push(match qualifier {
                TypeQual::Frozen => RecordRootQualifier::Frozen,
                TypeQual::Unique => RecordRootQualifier::Unique,
                _ => return None,
            });
            *budget -= 1;
            depth += 1;
            unqualified = inner;
        }
        let (identity, contains_list) = match unqualified {
            Type::Named(name, arguments) if definitions.contains_key(name) && arguments.len() <= 128 => {
                let ordinal = nominal(name)?;
                let storage = CarrierElement::from_checked_type_with_definitions(unqualified, definitions)?;
                if !matches!(storage, CarrierElement::Record { .. } | CarrierElement::Sum { .. }) {
                    return None;
                }
                let shape = crate::aggregates::scalar_aggregate_shape(unqualified, definitions)?;
                let field_types = shape.fields.iter().map(|field| SourceTypeKey { source: field }.digest())
                    .collect::<Option<Vec<_>>>()?;
                let (nodes, relative_depth) = storage.bounded_resource_shape()?;
                if depth.checked_add(relative_depth)? > 128 {
                    return None;
                }
                *budget = (*budget).checked_sub(nodes.saturating_sub(1))?;
                let contains_list = storage.contains_retained_sequence();
                (
                    Self::Nominal {
                        ordinal,
                        storage: Box::new(storage),
                        field_types,
                    },
                    contains_list,
                )
            }
            _ => {
                let plan = CarrierElement::from_checked_type_with_definitions(unqualified, definitions)?;
                if matches!(plan, CarrierElement::Record { .. } | CarrierElement::Sum { .. }) {
                    return None;
                }
                let (nodes, relative_depth) = plan.bounded_resource_shape()?;
                if depth.checked_add(relative_depth)? > 128 {
                    return None;
                }
                *budget = (*budget).checked_sub(nodes.saturating_sub(1))?;
                let contains_list = plan.contains_retained_sequence();
                (Self::Leaf(plan), contains_list)
            }
        };
        if !qualifiers.is_empty() && contains_list { return None; }
        let identity = qualifiers.into_iter().rev().fold(identity, |inner, qualifier| {
            Self::Qualified { qualifier, inner: Box::new(inner) }
        });
        Some((identity, contains_list))
    }

    /// Deterministic structural bytes. Checked nominal roots bind their
    /// instantiated fields and constructor names as well as their ordinal.
    pub fn append_identity_bytes(&self, bytes: &mut Vec<u8>) -> bool {
        const USER_SHAPE: u8 = 8;
        const LEAF: u8 = 4;
        const QUALIFIED: u8 = 6;
        const QUALIFIED_END: u8 = 7;
        match self {
            Self::Leaf(plan) => {
                bytes.push(LEAF);
                bytes.extend_from_slice(&plan.identity());
            }
            Self::Qualified { qualifier, inner } => {
                bytes.extend_from_slice(&[QUALIFIED, match qualifier {
                    RecordRootQualifier::Frozen => 0,
                    RecordRootQualifier::Unique => 1,
                }]);
                if !inner.append_identity_bytes(bytes) { return false; }
                bytes.push(QUALIFIED_END);
            }
            Self::Nominal { ordinal, storage, field_types } => {
                bytes.push(USER_SHAPE);
                bytes.extend_from_slice(&ordinal.to_le_bytes());
                if !storage.within_resource_bounds() { return false; }
                bytes.extend_from_slice(&storage.identity());
                let Ok(count) = u32::try_from(field_types.len()) else { return false; };
                bytes.extend_from_slice(&count.to_le_bytes());
                for field in field_types { bytes.extend_from_slice(field); }
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aggregates::ScalarAggregateShape;

    #[test]
    fn qualified_record_root_refuses_hidden_lists() {
        let named = |name: &str| Type::Named(name.into(), Vec::new());
        let list = Type::Named("List".into(), vec![named("Int")]);
        let mut definition = ScalarAggregateShape {
            name: "Bag".into(), fields: vec![list].into(), variants: Vec::new().into(), tagged: false, packed: false,
        }.packed_view();
        definition.packed = false;
        let definitions = BTreeMap::from([("Bag".into(), definition)]);
        let bag = named("Bag");
        assert!(RecordRootIdentity::from_checked_type(&bag, &definitions, |_| Some(0)).is_some());
        let frozen_bag = Type::Qualified(TypeQual::Frozen, Box::new(bag));
        assert!(RecordRootIdentity::from_checked_type(&frozen_bag, &definitions, |_| Some(0)).is_none());

        let dict = Type::Named("Dict".into(), vec![named("Int"), named("Int")]);
        assert!(RecordRootIdentity::from_checked_type(&dict, &definitions, |_| Some(0)).is_some());
        let frozen_dict = Type::Qualified(TypeQual::Frozen, Box::new(dict));
        assert!(RecordRootIdentity::from_checked_type(&frozen_dict, &definitions, |_| Some(0)).is_none());

        // A nominal argument binds source identity, but an unused argument
        // does not place its resource in the record's retained storage.
        let mut phantom = ScalarAggregateShape {
            name: "Phantom".into(), fields: vec![named("Int")].into(), variants: Vec::new().into(), tagged: false, packed: false,
        }.packed_view();
        phantom.packed = false;
        phantom.params = vec!["a".into()];
        let mut held = phantom.clone();
        held.name = "Held".into();
        held.variants[0].name = "Held".into();
        held.variants[0].fields = vec![named("a")];
        let definitions = BTreeMap::from([("Phantom".into(), phantom), ("Held".into(), held)]);
        let phantom_clock = Type::Named("Phantom".into(), vec![named("Clock")]);
        let phantom_int = Type::Named("Phantom".into(), vec![named("Int")]);
        let clock_root = RecordRootIdentity::from_checked_type(&phantom_clock, &definitions, |_| Some(0)).expect("phantom argument does not require a carrier");
        let int_root = RecordRootIdentity::from_checked_type(&phantom_int, &definitions, |_| Some(0)).expect("ordinary argument remains admitted");
        assert_ne!(clock_root, int_root);
        assert_eq!(crate::storage::snapshot_eligibility(&phantom_clock, &definitions), Ok(()));
        let held_clock = Type::Named("Held".into(), vec![named("Clock")]);
        assert_ne!(crate::storage::snapshot_eligibility(&held_clock, &definitions), Ok(()));
    }
}
