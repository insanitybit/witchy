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
    /// Every possible variant satisfies its respective stored-slot proof.
    Variants(BTreeMap<usize, Vec<Self>>),
    /// Every stored element satisfies this proof, including an empty sequence.
    Sequence(Box<Self>),
}

impl<Callable: Clone + PartialEq + Eq> OptValueProof<Callable> {
    pub fn callable(&self) -> Option<Callable> {
        match self { Self::Callable(contract) => Some(contract.clone()), _ => None }
    }

    pub fn slot(&self, index: usize) -> Self {
        match self {
            Self::Elements(elements) | Self::Variant { fields: elements, .. } =>
                elements.get(index).cloned().unwrap_or(Self::Unknown),
            Self::Variants(variants) if variants.len() == 1 =>
                variants.values().next().and_then(|fields| fields.get(index)).cloned().unwrap_or(Self::Unknown),
            _ => Self::Unknown,
        }
    }

    pub fn variant(&self, ordinal: usize) -> Self {
        match self {
            Self::Variants(variants) => variants.get(&ordinal).cloned()
                .map(Self::Elements).unwrap_or(Self::Unknown),
            Self::Variant { ordinal: actual, fields } if *actual == ordinal => Self::Elements(fields.clone()),
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
            Self::Sequence(element) => *element.clone(),
            _ => Self::Unknown,
        }
    }

    pub fn join(self, other: Self) -> Self {
        match (self, other) {
            (Self::Sequence(a), Self::Sequence(b)) => Self::Sequence(Box::new(a.join(*b))),
            (Self::Variant { ordinal, fields }, other @ Self::Variants(_)) =>
                Self::Variants(BTreeMap::from([(ordinal, fields)])).join(other),
            (other @ Self::Variants(_), Self::Variant { ordinal, fields }) =>
                other.join(Self::Variants(BTreeMap::from([(ordinal, fields)]))),
            (Self::Variants(mut a), Self::Variants(b)) => {
                for (ordinal, right) in b {
                    if let Some(left) = a.remove(&ordinal) {
                        if left.len() != right.len() { return Self::Unknown }
                        a.insert(ordinal, left.into_iter().zip(right).map(|(a, b)| a.join(b)).collect());
                    } else { a.insert(ordinal, right); }
                }
                Self::Variants(a)
            }
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
            (Self::Variant { ordinal: a, fields: left }, Self::Variant { ordinal: b, fields: right }) if a != b =>
                Self::Variants(BTreeMap::from([(a, left), (b, right)])),
            _ => Self::Unknown,
        }
    }
}
