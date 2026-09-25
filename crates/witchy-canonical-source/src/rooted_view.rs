//! Checked storage and identity plan for a rooted immutable view.
//!
//! This classifies source storage; authority still requires a checked-source
//! witness and SafeWIR's independent physical layout correspondence.

use std::collections::BTreeMap;

use witchy_syntax::ast::{Type, TypeDef};

use crate::abi::{
    CanonicalReferenceKind, CanonicalRootedViewCarrier, CanonicalScalarKind, CanonicalTypeKey,
};
use crate::carrier::CarrierElement;
use crate::rooted_record::RecordRootPlan;

/// Physical representation of a rooted source view. Its typed child proof
/// determines identity and the ownership operation at every containing edge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RootedViewStorage {
    ScalarCell(CanonicalScalarKind),
    InlineScalar(CanonicalScalarKind),
    RetainedProjection(CarrierElement),
    RetainedRoot(RetainedRootStorage),
}

/// The physical object behind a retained whole-root view. The child plan is
/// checked source data; SafeWIR still verifies its independent layout walk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetainedRootStorage {
    ByteBuffer,
    Sequence(CarrierElement),
    StructuralRecord(CarrierElement),
    CheckedAggregate(RecordRootPlan),
}

impl RootedViewStorage {
    /// Admit only checked storage shapes with a finite recursive proof. The
    /// caller supplies nominal ordinals from its checked declaration table.
    pub fn classify(
        payload: &Type,
        definitions: &BTreeMap<String, TypeDef>,
        nominal: impl Fn(&str) -> Option<u32>,
    ) -> Option<Self> {
        let mut bare = payload;
        for _ in 0..128 {
            let Type::Qualified(_, inner) = bare else {
                break;
            };
            bare = inner;
        }
        if matches!(bare, Type::Qualified(_, _)) {
            return None;
        }
        if matches!(bare, Type::Tuple(_)) || matches!(bare, Type::Named(name, _) if name == "Dict")
        {
            return Some(Self::RetainedRoot(RetainedRootStorage::StructuralRecord(
                CarrierElement::retained_structural_root(payload, definitions)?,
            )));
        }
        match bare {
            Type::Named(name, args) if args.is_empty() && name == "Int" => {
                Some(Self::ScalarCell(CanonicalScalarKind::Int))
            }
            Type::Named(name, args) if args.is_empty() && name == "String" => {
                Some(Self::RetainedRoot(RetainedRootStorage::ByteBuffer))
            }
            Type::Named(name, args)
                if args.is_empty() && matches!(name.as_str(), "Float" | "Bool" | "Duration") =>
            {
                let kind = match name.as_str() {
                    "Float" => CanonicalScalarKind::Float,
                    "Bool" => CanonicalScalarKind::Bool,
                    _ => CanonicalScalarKind::Duration,
                };
                Some(Self::InlineScalar(kind))
            }
            Type::Slice(element) => Some(Self::RetainedProjection(
                CarrierElement::from_checked_type_with_definitions(element, definitions)?,
            )),
            Type::Named(name, args) if name == "Slice" => {
                let [element] = args.as_slice() else {
                    return None;
                };
                Some(Self::RetainedProjection(
                    CarrierElement::from_checked_type_with_definitions(element, definitions)?,
                ))
            }
            Type::Named(name, _) if name == "List" => {
                let element = CarrierElement::from_list_root_payload_type_with_definitions(
                    payload,
                    definitions,
                )?;
                Some(Self::RetainedRoot(RetainedRootStorage::Sequence(element)))
            }
            Type::Named(..)
                if crate::rooted_record::has_record_root_head(payload, definitions)? =>
            {
                Some(Self::RetainedRoot(RetainedRootStorage::CheckedAggregate(RecordRootPlan::from_checked_type(
                    payload,
                    definitions,
                    nominal,
                )?)))
            }
            _ => None,
        }
    }

    /// One versioned structural encoding for every rooted-view source shape.
    /// The representation class selects the physical lanes; the key binds the
    /// checked recursive payload within that class.
    pub fn canonical_reference(&self) -> Option<CanonicalReferenceKind> {
        let mut bytes = b"witchy.canonical-rooted-view-payload.v4\0".to_vec();
        let carrier = match self {
            Self::ScalarCell(kind) => {
                if *kind != CanonicalScalarKind::Int {
                    return None;
                }
                bytes.push(0);
                bytes.push(*kind as u8);
                CanonicalRootedViewCarrier::ScalarPlace
            }
            Self::InlineScalar(kind) => {
                if !matches!(
                    kind,
                    CanonicalScalarKind::Float
                        | CanonicalScalarKind::Bool
                        | CanonicalScalarKind::Duration
                ) {
                    return None;
                }
                bytes.push(1);
                bytes.push(*kind as u8);
                CanonicalRootedViewCarrier::InlineScalar
            }
            Self::RetainedProjection(element) => {
                if !element.within_resource_bounds() {
                    return None;
                }
                bytes.push(2);
                bytes.extend_from_slice(&element.identity());
                CanonicalRootedViewCarrier::RetainedProjection
            }
            Self::RetainedRoot(root) => {
                match root {
                    RetainedRootStorage::ByteBuffer => bytes.extend_from_slice(&[3, 0]),
                    RetainedRootStorage::Sequence(element) => {
                        if !element.within_resource_bounds() { return None; }
                        bytes.extend_from_slice(&[3, 1]);
                        bytes.extend_from_slice(&element.identity());
                    }
                    RetainedRootStorage::StructuralRecord(element) => {
                        if !element.within_resource_bounds()
                            || !matches!(element, CarrierElement::Tuple(_) | CarrierElement::Dict(_, _)) {
                            return None;
                        }
                        bytes.extend_from_slice(&[3, 2]);
                        bytes.extend_from_slice(&element.identity());
                    }
                    RetainedRootStorage::CheckedAggregate(plan) => {
                        bytes.extend_from_slice(&[3, 3]);
                        if !plan.identity.append_identity_bytes(&mut bytes) { return None; }
                    }
                }
                CanonicalRootedViewCarrier::RetainedRoot
            }
        };
        Some(CanonicalReferenceKind::RootedView {
            payload: digest(&bytes),
            carrier,
        })
    }
}

fn digest(bytes: &[u8]) -> CanonicalTypeKey {
    CanonicalTypeKey::from_digest(*blake3::hash(bytes).as_bytes())
}
