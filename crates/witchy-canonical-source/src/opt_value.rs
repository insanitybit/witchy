//! Structural provenance of callback values stored in checked source data.
//!
//! This is a source-value judgment, never a permission inferred from a type.
//! A checker supplies the callable contract and authenticates constructor
//! ordinals against the declaration before building a variant proof.

use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OptValueProof<Callable> {
    Unknown,
    /// Bounded analysis lost the shape, but must not lose possible obligations.
    /// This never authenticates a callable, including after projection.
    MayRequireOpt,
    Callable(Callable),
    /// Possible values, not interchangeable proofs of a definite opt origin.
    /// Kept separately so a join with Unknown cannot erase entry obligations.
    Alternatives(Vec<Self>),
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
            Self::MayRequireOpt => Self::MayRequireOpt,
            Self::Alternatives(values) => Self::project_alternatives(values, |value| value.slot(index)),
            Self::Elements(elements) | Self::Variant { fields: elements, .. } =>
                elements.get(index).cloned().unwrap_or(Self::Unknown),
            Self::Variants(variants) if variants.len() == 1 =>
                variants.values().next().and_then(|fields| fields.get(index)).cloned().unwrap_or(Self::Unknown),
            _ => Self::Unknown,
        }
    }

    pub fn variant(&self, ordinal: usize) -> Self {
        match self {
            Self::MayRequireOpt => Self::MayRequireOpt,
            Self::Alternatives(values) => Self::project_alternatives(values, |value| value.variant(ordinal)),
            Self::Variants(variants) => variants.get(&ordinal).cloned()
                .map(Self::Elements).unwrap_or(Self::Unknown),
            Self::Variant { ordinal: actual, fields } if *actual == ordinal => Self::Elements(fields.clone()),
            _ => Self::Unknown,
        }
    }

    pub fn field(&self, name: &str) -> Self {
        match self {
            Self::MayRequireOpt => Self::MayRequireOpt,
            Self::Alternatives(values) => Self::project_alternatives(values, |value| value.field(name)),
            Self::Fields(fields) => fields.get(name).cloned().unwrap_or(Self::Unknown),
            _ => Self::Unknown,
        }
    }

    pub fn homogeneous_element(&self) -> Self {
        match self {
            Self::MayRequireOpt => Self::MayRequireOpt,
            Self::Alternatives(values) => Self::project_alternatives(values, Self::homogeneous_element),
            Self::Elements(elements) => elements.iter().cloned().reduce(Self::join).unwrap_or(Self::Unknown),
            Self::Sequence(element) => *element.clone(),
            _ => Self::Unknown,
        }
    }

    /// Eliminate a checked sum: project the selected variant's slot, otherwise
    /// use the fallback. Unknown success and possible fallback remain distinct.
    pub fn select_variant_slot(&self, selected: usize, index: usize, fallback: &Self) -> Self {
        let select = |ordinal: usize, fields: &[Self]| {
            if ordinal == selected { fields.get(index).cloned().unwrap_or(Self::Unknown) }
            else { fallback.clone() }
        };
        match self {
            Self::MayRequireOpt => Self::MayRequireOpt,
            Self::Alternatives(values) => Self::project_alternatives(values, |value| value.select_variant_slot(selected, index, fallback)),
            Self::Variant { ordinal, fields } => select(*ordinal, fields),
            Self::Variants(variants) => variants.iter().map(|(ordinal, fields)| select(*ordinal, fields))
                .reduce(Self::join).unwrap_or(Self::Unknown),
            _ => Self::Unknown.join(fallback.clone()),
        }
    }

    pub fn join(self, other: Self) -> Self {
        match (self, other) {
            (Self::MayRequireOpt, _) | (_, Self::MayRequireOpt) => Self::MayRequireOpt,
            (Self::Unknown, Self::Unknown) => Self::Unknown,
            (Self::Sequence(a), Self::Sequence(b)) => Self::Sequence(Box::new(a.join(*b))),
            (Self::Variant { ordinal, fields }, other @ Self::Variants(_)) =>
                Self::Variants(BTreeMap::from([(ordinal, fields)])).join(other),
            (other @ Self::Variants(_), Self::Variant { ordinal, fields }) =>
                other.join(Self::Variants(BTreeMap::from([(ordinal, fields)]))),
            (Self::Variants(mut a), Self::Variants(b)) => {
                for (ordinal, right) in b {
                    if let Some(left) = a.remove(&ordinal) {
                        if left.len() != right.len() { return Self::MayRequireOpt }
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
            (left, right) => Self::alternatives(left, right),
        }
    }

    fn project_alternatives(values: &[Self], project: impl Fn(&Self) -> Self) -> Self {
        values.iter().map(project).reduce(Self::join).unwrap_or(Self::Unknown)
    }

    fn alternatives(left: Self, right: Self) -> Self {
        // Bound both alternatives and nested structural proofs. Overflow is a
        // conservative refusal fact, never the absence of a precondition.
        fn bounded<C>(proof: &OptValueProof<C>, depth: usize, remaining: &mut usize) -> bool {
            if depth > 128 || *remaining == 0 { return false }
            *remaining -= 1;
            let mut child = |proof| bounded(proof, depth + 1, remaining);
            match proof {
                OptValueProof::Alternatives(values) | OptValueProof::Elements(values)
                    | OptValueProof::Variant { fields: values, .. } => values.iter().all(&mut child),
                OptValueProof::Variants(values) => values.values().flatten().all(&mut child),
                OptValueProof::Fields(values) => values.values().all(&mut child),
                OptValueProof::Sequence(value) => child(value),
                _ => true,
            }
        }
        let mut remaining = 16_384;
        if !bounded(&left, 0, &mut remaining) || !bounded(&right, 0, &mut remaining) {
            return Self::MayRequireOpt;
        }
        let mut alternatives = match left {
            Self::Alternatives(values) => values,
            other => vec![other],
        };
        let right = match right {
            Self::Alternatives(values) => values,
            other => vec![other],
        };
        for value in right {
            if !alternatives.contains(&value) { alternatives.push(value); }
            if alternatives.len() > 32 { return Self::MayRequireOpt }
        }
        if alternatives.len() == 1 { alternatives.pop().unwrap() }
        else { Self::Alternatives(alternatives) }
    }
}

#[cfg(test)]
mod tests {
    use super::OptValueProof as Proof;

    #[test]
    fn uncertain_origin_retains_projected_possibilities() {
        let proof = Proof::Elements(vec![Proof::Callable(1)])
            .join(Proof::Elements(vec![Proof::Unknown]));
        assert_eq!(proof.slot(0), Proof::Alternatives(vec![Proof::Callable(1), Proof::Unknown]));
        assert_eq!(proof.slot(0).callable(), None);
        assert_eq!(Proof::Callable(1).join(Proof::Callable(1)).callable(), Some(1));
        assert_eq!(Proof::Unknown.select_variant_slot(0, 0, &Proof::Callable(1)),
            Proof::Alternatives(vec![Proof::Unknown, Proof::Callable(1)]));
    }

    #[test]
    fn overflow_keeps_a_refusal_fact_through_projections() {
        let proof = (0..40).map(Proof::Callable).reduce(Proof::join).unwrap();
        assert_eq!(proof, Proof::MayRequireOpt);
        assert_eq!(proof.slot(0).variant(0).homogeneous_element(), Proof::MayRequireOpt);
        assert_eq!(proof.callable(), None);
        let deep = (0..130).fold(Proof::Callable(1), |proof, _| Proof::Sequence(Box::new(proof)));
        assert_eq!(deep.join(Proof::Unknown), Proof::MayRequireOpt);
    }
}
