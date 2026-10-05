//! Checked storage and identity plan for a rooted immutable view.
//!
//! This classifies source storage; authority still requires a checked-source
//! witness and SafeWIR's independent physical layout correspondence.

use std::collections::BTreeMap;

use sha2::{Digest, Sha256};
use witchy_syntax::ast::{Type, TypeDef, TypeQual};

use crate::abi::{
    CanonicalReferenceKind, CanonicalRootedViewCarrier, CanonicalScalarKind, CanonicalTypeKey,
};
use crate::carrier::CarrierElement;
use crate::rooted_record::RecordRootPlan;
use crate::storage::AggregateGraphPlan;
use crate::structural_identity::SourceTypeKey;

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
    ByteBuffer(ByteBufferKind),
    Sequence(CarrierElement),
    StructuralRecord(CarrierElement),
    CheckedAggregate(RecordRootPlan),
    CheckedGraph(GraphRootPlan),
}

/// A heap aggregate root whose nested view fields use the checked aggregate
/// graph operation proof rather than the ordinary `CarrierElement` grammar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphRootPlan {
    pub graph: AggregateGraphPlan,
    ordinal: Option<u32>,
}

impl GraphRootPlan {
    fn classify(payload: &Type, definitions: &BTreeMap<String, TypeDef>,
        nominal: impl Fn(&str) -> Option<u32>) -> Option<Self> {
        let ordinal = match payload {
            Type::Named(name, _) if definitions.contains_key(name) => Some(nominal(name)?),
            Type::Tuple(_) => None,
            _ => return None,
        };
        if !matches!(crate::aggregates::contains_stored_view(payload, definitions), Some(true)) {
            return None;
        }
        let graph = AggregateGraphPlan::classify(payload, definitions)?;
        graph.cloneable().then_some(Self { graph, ordinal })
    }

    fn append_identity_bytes(&self, bytes: &mut Vec<u8>) -> Option<()> {
        match self.ordinal {
            Some(ordinal) => { bytes.push(1); bytes.extend_from_slice(&ordinal.to_le_bytes()); }
            None => bytes.push(0),
        }
        bytes.extend_from_slice(&self.graph.source_key());
        let shape = self.graph.shape();
        bytes.extend_from_slice(&u32::try_from(shape.fields.len()).ok()?.to_le_bytes());
        for field in shape.fields.iter() {
            bytes.extend_from_slice(&SourceTypeKey { source: field }.digest()?);
        }
        bytes.extend_from_slice(&u32::try_from(shape.variants.len()).ok()?.to_le_bytes());
        for variant in shape.variants.iter() {
            bytes.extend_from_slice(&u32::try_from(variant.name.len()).ok()?.to_le_bytes());
            bytes.extend_from_slice(variant.name.as_bytes());
            bytes.extend_from_slice(&u32::try_from(variant.first_field).ok()?.to_le_bytes());
            bytes.extend_from_slice(&u32::try_from(variant.arity).ok()?.to_le_bytes());
        }
        Some(())
    }
}

/// A shared physical byte buffer whose source identity remains distinct.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ByteBufferKind {
    String,
    Bytes,
}

/// The source-authenticated owner leaf carried by an aggregate field. The
/// payload qualifier remains visible so aggregate admission cannot silently
/// inherit a projection path established only for an unqualified slice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerCarriedViewPlan {
    storage: OwnerCarriedViewStorage,
    payload_frozen: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnerCarriedViewStorage {
    AuthenticatedPlace(CanonicalScalarKind),
    RetainedProjection(CarrierElement),
}

impl OwnerCarriedViewPlan {
    pub fn classify(ty: &Type, definitions: &BTreeMap<String, TypeDef>) -> Option<Self> {
        let Type::Qualified(TypeQual::View, payload) = ty else { return None };
        let (payload, payload_frozen) = match payload.as_ref() {
            Type::Qualified(TypeQual::Frozen, inner) => (inner.as_ref(), true),
            other => (other, false),
        };
        if matches!(payload, Type::Qualified(..)) { return None; }
        // This is a leaf query from aggregate shape checking. Restrict it to
        // the two owner-carried source heads before invoking the broader
        // rooted-view classifier; a record-root query here could re-enter the
        // same aggregate shape traversal through a recursive field.
        match payload {
            Type::Named(name, arguments) if name == "Int" && arguments.is_empty() => {}
            Type::Slice(_) => {}
            Type::Named(name, arguments) if name == "Slice" && arguments.len() == 1 => {}
            _ => return None,
        }
        let storage = match RootedViewStorage::classify(payload, definitions, |_| None)? {
            RootedViewStorage::ScalarCell(CanonicalScalarKind::Int) => {
                OwnerCarriedViewStorage::AuthenticatedPlace(CanonicalScalarKind::Int)
            }
            RootedViewStorage::RetainedProjection(child) => OwnerCarriedViewStorage::RetainedProjection(child),
            _ => return None,
        };
        Some(Self { storage, payload_frozen })
    }

    pub fn admitted_aggregate_field(&self) -> bool {
        self.admitted_storage().is_some()
    }

    /// Storage that the current aggregate and callable owner-family paths
    /// can authenticate. Frozen slices keep an identity but lack that path.
    pub fn admitted_storage(&self) -> Option<&OwnerCarriedViewStorage> {
        match (&self.storage, self.payload_frozen) {
            (OwnerCarriedViewStorage::AuthenticatedPlace(CanonicalScalarKind::Int), _) => Some(&self.storage),
            (OwnerCarriedViewStorage::RetainedProjection(_), false) => Some(&self.storage),
            _ => None,
        }
    }

    pub fn source_identity(&self) -> Type {
        let payload = match &self.storage {
            OwnerCarriedViewStorage::AuthenticatedPlace(CanonicalScalarKind::Int) => {
                Type::Named("Int".into(), Vec::new())
            }
            OwnerCarriedViewStorage::RetainedProjection(child) => Type::Slice(Box::new(child.source_type())),
            _ => unreachable!("only Int has an authenticated scalar place"),
        };
        Type::Qualified(TypeQual::View, Box::new(payload))
    }
}

impl RootedViewStorage {
    /// Structural owner-family ID for the two GC-backed rooted projections.
    /// The bounded source plan supplies identity; checked-source membership
    /// and the physical family still need independent SafeWIR validation.
    pub fn owner_family_id(&self) -> Option<u32> {
        let mut digest = Sha256::new();
        digest.update(b"witchy/rooted-view-owner-family-v2\0");
        match self {
            Self::ScalarCell(CanonicalScalarKind::Int) => digest.update([0, CanonicalScalarKind::Int as u8]),
            Self::RetainedProjection(element) if element.within_resource_bounds() => {
                digest.update([1]);
                digest.update(element.identity());
            }
            _ => return None,
        }
        let digest: [u8; 32] = digest.finalize().into();
        let lower = u32::from_le_bytes([digest[0], digest[1], digest[2], digest[3]]);
        Some(0xc300_0000 | ((lower & 0x007f_ffff) << 1))
    }

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
        if let Type::Slice(element) = bare {
            return Some(Self::RetainedProjection(
                CarrierElement::from_checked_type_with_definitions(element, definitions)?,
            ));
        }
        if let Type::Named(name, arguments) = bare {
            if name == "str" && arguments.is_empty() {
                return Some(Self::RetainedRoot(RetainedRootStorage::ByteBuffer(ByteBufferKind::String)));
            }
            if name == "Slice" {
                let [element] = arguments.as_slice() else { return None };
                return Some(Self::RetainedProjection(
                    CarrierElement::from_checked_type_with_definitions(element, definitions)?,
                ));
            }
        }
        if crate::rooted_record::has_record_root_head(payload, definitions)? {
            if let Some(plan) = RecordRootPlan::from_checked_type(payload, definitions, &nominal) {
                return Some(Self::RetainedRoot(RetainedRootStorage::CheckedAggregate(plan)));
            }
            return Some(Self::RetainedRoot(RetainedRootStorage::CheckedGraph(
                GraphRootPlan::classify(payload, definitions, nominal)?,
            )));
        }
        if matches!(payload, Type::Tuple(_)) {
            if let Some(plan) = GraphRootPlan::classify(payload, definitions, nominal) {
                return Some(Self::RetainedRoot(RetainedRootStorage::CheckedGraph(plan)));
            }
        }
        if let Some(element) = CarrierElement::from_list_root_payload_type_with_definitions(
            payload, definitions,
        ) {
            return Some(Self::RetainedRoot(RetainedRootStorage::Sequence(element)));
        }
        // The recursive checked carrier proof determines the storage class
        // for ordinary values. The prior gates above keep their exact child
        // depth budget and avoid reparsing a checked nominal's shape.
        if let Some(plan) = CarrierElement::from_checked_type_with_definitions(bare, definitions) {
            match plan {
                CarrierElement::Scalar(CanonicalScalarKind::Int) => {
                    return Some(Self::ScalarCell(CanonicalScalarKind::Int));
                }
                CarrierElement::Scalar(CanonicalScalarKind::Unit) => return None,
                CarrierElement::Scalar(kind) => return Some(Self::InlineScalar(kind)),
                CarrierElement::String => {
                    return Some(Self::RetainedRoot(RetainedRootStorage::ByteBuffer(ByteBufferKind::String)));
                }
                CarrierElement::Bytes => {
                    return Some(Self::RetainedRoot(RetainedRootStorage::ByteBuffer(ByteBufferKind::Bytes)));
                }
                CarrierElement::List(_) | CarrierElement::RetainedRootView(_) => return None,
                plan @ (CarrierElement::Tuple(_) | CarrierElement::Dict(_, _)) => {
                    if matches!(payload, Type::Qualified(_, _)) { return None; }
                    return Some(Self::RetainedRoot(RetainedRootStorage::StructuralRecord(plan)));
                }
                CarrierElement::Record { .. } | CarrierElement::Sum { .. } => {}
            }
        }
        None
    }

    /// One versioned structural encoding for every rooted-view source shape.
    /// The representation class selects the physical lanes; the key binds the
    /// checked recursive payload within that class.
    pub fn canonical_reference(&self, source_payload: &Type) -> Option<CanonicalReferenceKind> {
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
                    RetainedRootStorage::ByteBuffer(kind) => bytes.extend_from_slice(&[3, 0, match kind {
                        ByteBufferKind::String => 0,
                        ByteBufferKind::Bytes => 1,
                    }]),
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
                    RetainedRootStorage::CheckedGraph(plan) => {
                        bytes.extend_from_slice(&[3, 4]);
                        plan.append_identity_bytes(&mut bytes)?;
                    }
                }
                CanonicalRootedViewCarrier::RetainedRoot
            }
        };
        // The storage class selects the physical lanes, while the checked
        // source qualifier chain remains part of the ABI identity. The
        // classifier may erase these shells to find storage; doing so here
        // would let `&T` and `&unique T` share an authority key.
        let mut qualifiers = Vec::new();
        let mut bare = source_payload;
        for _ in 0..128 {
            match bare {
                Type::Qualified(TypeQual::Frozen, inner) => {
                    qualifiers.push(0_u8);
                    bare = inner;
                }
                Type::Qualified(TypeQual::Unique, inner) => {
                    qualifiers.push(1_u8);
                    bare = inner;
                }
                Type::Qualified(..) => return None,
                _ => break,
            }
        }
        if matches!(bare, Type::Qualified(..)) { return None; }
        bytes.push(qualifiers.len() as u8);
        bytes.extend_from_slice(&qualifiers);
        Some(CanonicalReferenceKind::RootedView {
            payload: digest(&bytes),
            carrier,
        })
    }
}

fn digest(bytes: &[u8]) -> CanonicalTypeKey {
    CanonicalTypeKey::from_digest(*blake3::hash(bytes).as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_byte_layout_keeps_distinct_view_keys() {
        let definitions = BTreeMap::new();
        let source = |name: &str| Type::Named(name.into(), Vec::new());
        let string = RootedViewStorage::classify(&source("String"), &definitions, |_| None)
            .expect("String has a retained root");
        let bytes = RootedViewStorage::classify(&source("Bytes"), &definitions, |_| None)
            .expect("Bytes has a retained root");
        assert_eq!(string, RootedViewStorage::RetainedRoot(RetainedRootStorage::ByteBuffer(ByteBufferKind::String)));
        assert_eq!(bytes, RootedViewStorage::RetainedRoot(RetainedRootStorage::ByteBuffer(ByteBufferKind::Bytes)));
        assert_ne!(string.canonical_reference(&source("String")), bytes.canonical_reference(&source("Bytes")),
            "one physical buffer layout must not erase checked source identity");

        let plain_slice = Type::Slice(Box::new(source("String")));
        let frozen_slice = Type::Slice(Box::new(Type::Qualified(
            TypeQual::Frozen, Box::new(source("String")),
        )));
        assert!(RootedViewStorage::classify(&plain_slice, &definitions, |_| None).is_some());
        assert!(RootedViewStorage::classify(&frozen_slice, &definitions, |_| None).is_none(),
            "a qualified slice child cannot inherit the unqualified projection key");
    }
}
