//! Recursive, checked-source vocabulary for rooted slice carriers.
//!
//! A carrier is admitted only when the checked frontend includes its element
//! in `CanonicalSourceView` and SafeWIR independently matches the recursive
//! physical layout. This description cannot grant authority on its own.

use witchy_syntax::ast::{Type, TypeQual};

use crate::abi::CanonicalScalarKind;

/// The value and ownership shape of one slice element. A scalar is copied;
/// String and List values use owned, reference-counted storage and therefore
/// require retain, clone and release at every containing List level.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CarrierElement {
    Scalar(CanonicalScalarKind),
    String,
    List(Box<Self>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CarrierOwnership {
    Copy,
    RetainCloneRelease,
}

/// SafeWIR supplies the physical facts without making this shared contract
/// depend on WIR's layout types. The managed queries return success only for
/// an owned pointer slot and a reference-counted pointee with valid release.
pub trait CarrierLayoutQuery {
    type Layout: Copy;

    fn scalar(&self, layout: Self::Layout) -> Option<CanonicalScalarKind>;
    fn owned_string(&self, layout: Self::Layout) -> bool;
    fn owned_retained_list_element(&self, layout: Self::Layout) -> Option<Self::Layout>;
}

impl CarrierElement {
    /// Slice carriers explicitly present in one finalized checked callable
    /// signature. An inbound closed-local parameter can name a carrier even
    /// when no borrow formation appears in this function body.
    pub fn in_checked_signature(signature: &Type) -> Vec<Self> {
        let mut found = Vec::new();
        let mut pending = vec![signature];
        let mut visited = 0usize;
        while let Some(ty) = pending.pop() {
            visited += 1;
            if visited > 4_096 {
                return Vec::new();
            }
            if let Some(carrier) = Self::from_view_type(ty) {
                found.push(carrier);
            }
            match ty {
                Type::Named(_, arguments) | Type::Tuple(arguments) | Type::Dyn(_, arguments) => {
                    pending.extend(arguments);
                }
                Type::Slice(inner) | Type::Qualified(_, inner) => pending.push(inner),
                Type::Fn(parameters, result, _, _) => {
                    pending.extend(parameters);
                    pending.push(result);
                }
                Type::RecordCompose { base, fields } => {
                    pending.push(base);
                    pending.extend(fields.iter().map(|(_, field)| field));
                }
            }
        }
        found
    }

    /// Parse the exact structural checked element grammar. Qualifiers,
    /// aliases, capabilities and arbitrary nominal types do not acquire a
    /// carrier merely because their storage happens to resemble one.
    pub fn from_checked_type(ty: &Type) -> Option<Self> {
        Self::from_checked_type_bounded(ty, 0)
    }

    fn from_checked_type_bounded(ty: &Type, depth: usize) -> Option<Self> {
        // Bound compiler work on hostile source. The same limit applies to
        // identity encoding and physical validation because only admitted
        // elements can reach either path.
        if depth > 128 {
            return None;
        }
        match ty {
            Type::Named(name, arguments) if arguments.is_empty() => match name.as_str() {
                "Int" => Some(Self::Scalar(CanonicalScalarKind::Int)),
                "Float" => Some(Self::Scalar(CanonicalScalarKind::Float)),
                "Bool" => Some(Self::Scalar(CanonicalScalarKind::Bool)),
                "Duration" => Some(Self::Scalar(CanonicalScalarKind::Duration)),
                "String" => Some(Self::String),
                _ => None,
            },
            Type::Named(name, arguments) if name == "List" => {
                let [child] = arguments.as_slice() else {
                    return None;
                };
                Some(Self::List(Box::new(Self::from_checked_type_bounded(
                    child,
                    depth + 1,
                )?)))
            }
            _ => None,
        }
    }

    pub fn from_view_type(ty: &Type) -> Option<Self> {
        let Type::Qualified(TypeQual::View, payload) = ty else {
            return None;
        };
        Self::from_slice_payload_type(payload)
    }

    /// The checker's private `Slice(T)` payload of a retained formation fact.
    pub fn from_slice_payload_type(payload: &Type) -> Option<Self> {
        let element = match payload {
            Type::Slice(element) => element.as_ref(),
            Type::Named(name, arguments) if name == "Slice" => {
                let [element] = arguments.as_slice() else {
                    return None;
                };
                element
            }
            _ => return None,
        };
        Self::from_checked_type(element)
    }

    /// Element of a whole rooted `&List(T)` view. Only an outer `frozen`
    /// qualifier is representation-neutral; nested qualifiers need their own
    /// ownership carrier and therefore fail this relation.
    pub fn from_list_root_payload_type(mut payload: &Type) -> Option<Self> {
        while let Type::Qualified(TypeQual::Frozen, inner) = payload {
            payload = inner;
        }
        let Type::Named(name, arguments) = payload else {
            return None;
        };
        let [element] = arguments.as_slice() else {
            return None;
        };
        (name == "List").then(|| Self::from_checked_type(element)).flatten()
    }

    pub fn source_type(&self) -> Type {
        match self {
            Self::Scalar(kind) => Type::Named(
                match kind {
                    CanonicalScalarKind::Int => "Int",
                    CanonicalScalarKind::Float => "Float",
                    CanonicalScalarKind::Bool => "Bool",
                    CanonicalScalarKind::Duration => "Duration",
                    CanonicalScalarKind::Unit => "Nil",
                }
                .into(),
                Vec::new(),
            ),
            Self::String => Type::Named("String".into(), Vec::new()),
            Self::List(child) => Type::Named("List".into(), vec![child.source_type()]),
        }
    }

    pub fn ownership(&self) -> CarrierOwnership {
        match self {
            Self::Scalar(_) => CarrierOwnership::Copy,
            Self::String | Self::List(_) => CarrierOwnership::RetainCloneRelease,
        }
    }

    /// The single recursive relation between checked element, physical
    /// storage, and its copy or retain/clone/release operation. Every List
    /// edge is checked independently; any missing fact fails closed.
    pub fn matches_layout<Q: CarrierLayoutQuery>(&self, query: &Q, mut layout: Q::Layout) -> bool {
        let mut current = self;
        loop {
            match current {
                Self::Scalar(kind) => {
                    return current.ownership() == CarrierOwnership::Copy
                        && query.scalar(layout) == Some(*kind);
                }
                Self::String => {
                    return current.ownership() == CarrierOwnership::RetainCloneRelease
                        && query.owned_string(layout);
                }
                Self::List(child) => {
                    if current.ownership() != CarrierOwnership::RetainCloneRelease {
                        return false;
                    }
                    let Some(next) = query.owned_retained_list_element(layout) else {
                        return false;
                    };
                    layout = next;
                    current = child;
                }
            }
        }
    }

    /// Collision-resistant identity of the canonical recursive element. The
    /// layout allocator assigns compact, collision-free local IDs by sorting
    /// these full keys; their truncated digest is never used as an ID.
    pub fn identity(&self) -> [u8; 32] {
        fn encode(element: &CarrierElement, bytes: &mut Vec<u8>) {
            match element {
                CarrierElement::Scalar(kind) => {
                    bytes.push(0);
                    bytes.push(*kind as u8);
                }
                CarrierElement::String => bytes.push(1),
                CarrierElement::List(child) => {
                    bytes.push(2);
                    encode(child, bytes);
                    bytes.push(3);
                }
            }
        }
        let mut bytes = b"witchy.rooted-slice-carrier.v1\0".to_vec();
        encode(self, &mut bytes);
        *blake3::hash(&bytes).as_bytes()
    }
}
