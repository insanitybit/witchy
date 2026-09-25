//! Typed identity for a retained record-root view's source payload.
//!
//! The checked frontend supplies nominal ordinals. This module only describes
//! the finite structural relation; it cannot mint a checked-source witness.

use std::collections::BTreeMap;

use witchy_syntax::ast::{Type, TypeDef, TypeQual};

use crate::carrier::CarrierElement;

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
    Some(matches!(bare, Type::Named(name, _)
        if matches!(name.as_str(), "Option" | "Result") || definitions.contains_key(name)))
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
    Option(Box<Self>),
    Result(Box<Self>, Box<Self>),
    Nominal { ordinal: u32, arguments: Vec<Self>, storage: Box<CarrierElement> },
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
            Type::Named(name, arguments) if name == "Option" && arguments.len() == 1 => {
                let child = &arguments[0];
                let (child, contains_list) =
                    Self::parse(child, definitions, nominal, depth + 1, budget)?;
                (Self::Option(Box::new(child)), contains_list)
            }
            Type::Named(name, arguments) if name == "Result" && arguments.len() == 2 => {
                let (ok, error) = (&arguments[0], &arguments[1]);
                let (ok, ok_list) = Self::parse(ok, definitions, nominal, depth + 1, budget)?;
                let (error, error_list) =
                    Self::parse(error, definitions, nominal, depth + 1, budget)?;
                (
                    Self::Result(Box::new(ok), Box::new(error)),
                    ok_list || error_list,
                )
            }
            Type::Named(name, arguments) if definitions.contains_key(name) && arguments.len() <= 128 => {
                let ordinal = nominal(name)?;
                let mut children = Vec::with_capacity(arguments.len());
                let mut contains_list = false;
                for argument in arguments {
                    let (child, child_list) =
                        Self::parse(argument, definitions, nominal, depth + 1, budget)?;
                    children.push(child);
                    contains_list |= child_list;
                }
                let storage = CarrierElement::from_checked_type_with_definitions(unqualified, definitions)?;
                if !matches!(storage, CarrierElement::Record { .. } | CarrierElement::Sum { .. }) {
                    return None;
                }
                let (nodes, relative_depth) = storage.bounded_resource_shape()?;
                if depth.checked_add(relative_depth)? > 128 {
                    return None;
                }
                *budget = (*budget).checked_sub(nodes.saturating_sub(1))?;
                contains_list |= carrier_contains_list(&storage);
                (
                    Self::Nominal {
                        ordinal,
                        arguments: children,
                        storage: Box::new(storage),
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
                let contains_list = carrier_contains_list(&plan);
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
        const OPTION: u8 = 2;
        const RESULT: u8 = 3;
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
            Self::Option(child) => {
                bytes.push(OPTION);
                bytes.extend_from_slice(&1u32.to_le_bytes());
                if !child.append_identity_bytes(bytes) {
                    return false;
                }
            }
            Self::Result(ok, error) => {
                bytes.push(RESULT);
                bytes.extend_from_slice(&2u32.to_le_bytes());
                if !ok.append_identity_bytes(bytes) || !error.append_identity_bytes(bytes) {
                    return false;
                }
            }
            Self::Nominal { ordinal, arguments, storage } => {
                bytes.push(USER_SHAPE);
                bytes.extend_from_slice(&ordinal.to_le_bytes());
                bytes.extend_from_slice(&(arguments.len() as u32).to_le_bytes());
                for argument in arguments {
                    if !argument.append_identity_bytes(bytes) {
                        return false;
                    }
                }
                if !storage.within_resource_bounds() { return false; }
                bytes.extend_from_slice(&storage.identity());
            }
        }
        true
    }
}

fn carrier_contains_list(plan: &CarrierElement) -> bool {
    match plan {
        CarrierElement::List(_) => true,
        CarrierElement::Tuple(fields) => fields.iter().any(carrier_contains_list),
        // A dictionary is physically a pair of retained key/value Lists.
        CarrierElement::Dict(_, _) => true,
        CarrierElement::Record { arguments, fields, .. } => arguments.iter().chain(fields).any(carrier_contains_list),
        CarrierElement::Sum { arguments, variants, .. } => {
            arguments.iter().any(carrier_contains_list)
                || variants.iter().any(|variant| variant.fields.iter().any(carrier_contains_list))
        }
        CarrierElement::Scalar(_) | CarrierElement::String | CarrierElement::Bytes => false,
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
            name: "Bag".into(), fields: vec![list], variants: Vec::new(), tagged: false, packed: false,
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
    }
}
