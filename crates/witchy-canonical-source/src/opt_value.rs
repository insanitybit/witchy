//! Structural provenance of callback values stored in checked source data.
//!
//! This is a source-value judgment, never a permission inferred from a type.
//! A checker supplies the callable contract and authenticates constructor
//! ordinals against the declaration before building a variant proof.

use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OptValueProof<Callable> {
    Unknown,
    Callable(Callable),
    Elements(Vec<Self>),
    Fields(BTreeMap<String, Self>),
    Variant { ordinal: usize, fields: Vec<Self> },
}

impl<Callable: Clone + PartialEq + Eq> OptValueProof<Callable> {
    pub fn callable(&self) -> Option<Callable> {
        match self { Self::Callable(contract) => Some(contract.clone()), _ => None }
    }

    pub fn slot(&self, index: usize) -> Self {
        match self {
            Self::Elements(elements) | Self::Variant { fields: elements, .. } =>
                elements.get(index).cloned().unwrap_or(Self::Unknown),
            _ => Self::Unknown,
        }
    }

    pub fn field(&self, name: &str) -> Self {
        match self {
            Self::Fields(fields) => fields.get(name).cloned().unwrap_or(Self::Unknown),
            _ => Self::Unknown,
        }
    }

    pub fn homogeneous_element(&self) -> Self {
        match self {
            Self::Elements(elements) => elements.iter().cloned().reduce(Self::join).unwrap_or(Self::Unknown),
            _ => Self::Unknown,
        }
    }

    pub fn join(self, other: Self) -> Self {
        match (self, other) {
            (Self::Callable(a), Self::Callable(b)) if a == b => Self::Callable(a),
            (Self::Elements(a), Self::Elements(b)) if a.len() == b.len() =>
                Self::Elements(a.into_iter().zip(b).map(|(a, b)| a.join(b)).collect()),
            (Self::Fields(a), Self::Fields(mut b)) if a.keys().eq(b.keys()) =>
                Self::Fields(a.into_iter().map(|(name, child)| {
                    let other = b.remove(&name).expect("matching field keys");
                    (name, child.join(other))
                }).collect()),
            (Self::Variant { ordinal: a, fields: left }, Self::Variant { ordinal: b, fields: right })
                if a == b && left.len() == right.len() => Self::Variant {
                    ordinal: a,
                    fields: left.into_iter().zip(right).map(|(a, b)| a.join(b)).collect(),
                },
            _ => Self::Unknown,
        }
    }
}
