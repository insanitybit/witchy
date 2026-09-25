//! Recursive, checked-source vocabulary for rooted slice carriers.
//!
//! A carrier is admitted only when the checked frontend includes its element
//! in `CanonicalSourceView` and SafeWIR independently matches the recursive
//! physical layout. This description cannot grant authority on its own.

use std::collections::BTreeMap;
use witchy_syntax::ast::{Type, TypeDef, TypeQual};

use crate::abi::CanonicalScalarKind;

/// The value and ownership shape of one slice element. A scalar is copied;
/// managed values and nonempty tuples use owned storage and require retain,
/// clone and release at every containing edge.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CarrierElement {
    Scalar(CanonicalScalarKind),
    String,
    List(Box<Self>),
    Tuple(Vec<Self>),
    Dict(Box<Self>, Box<Self>),
    /// A checked, one-variant heap record. The declared identity and every
    /// instantiated field are part of the proof, including unused parameters.
    Record { name: String, arguments: Vec<Self>, fields: Vec<Self> },
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
    fn owned_record_fields(&self, layout: Self::Layout) -> Option<Vec<Self::Layout>>;
}

impl CarrierElement {
    /// A whole structural aggregate whose immutable view is the retained
    /// object token. This uses the same recursive element and ownership
    /// judgment as a slice element; qualifiers and unproved children cannot
    /// turn into root authority through a matching outer name.
    pub fn retained_structural_root(
        ty: &Type,
        definitions: &BTreeMap<String, TypeDef>,
    ) -> Option<Self> {
        if matches!(ty, Type::Qualified(_, _)) {
            return None;
        }
        let plan = Self::from_checked_type_with_definitions(ty, definitions)?;
        matches!(plan, Self::Tuple(_) | Self::Dict(_, _)).then_some(plan)
    }

    /// Slice carriers explicitly present in one finalized checked callable
    /// signature. An inbound closed-local parameter can name a carrier even
    /// when no borrow formation appears in this function body.
    pub fn in_checked_signature(signature: &Type) -> Vec<Self> {
        Self::in_checked_signature_with(signature, Self::from_view_type)
    }

    pub fn in_checked_signature_with_definitions(
        signature: &Type,
        definitions: &BTreeMap<String, TypeDef>,
    ) -> Vec<Self> {
        Self::in_checked_signature_with(signature, |ty| {
            Self::from_view_type_with_definitions(ty, definitions)
        })
    }

    fn in_checked_signature_with(
        signature: &Type,
        parse: impl Fn(&Type) -> Option<Self>,
    ) -> Vec<Self> {
        let mut found = Vec::new();
        let mut pending = vec![signature];
        let mut visited = 0usize;
        while let Some(ty) = pending.pop() {
            visited += 1;
            if visited > 4_096 {
                return Vec::new();
            }
            if let Some(carrier) = parse(ty) {
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
        Self::from_checked_type_bounded(ty, 0, &mut 4_096)
    }

    /// Extend the structural grammar using checked source definitions. A
    /// nominal record is admitted only when its instantiated fields have the
    /// same recursive ownership proof; recursive and tagged declarations
    /// remain outside this carrier until their operations can be proved.
    pub fn from_checked_type_with_definitions(
        ty: &Type,
        definitions: &BTreeMap<String, TypeDef>,
    ) -> Option<Self> {
        let mut budget = 4_096;
        Self::from_checked_type_with_definitions_at(ty, definitions, 0, &mut budget)
    }

    fn from_checked_type_with_definitions_at(
        ty: &Type,
        definitions: &BTreeMap<String, TypeDef>,
        depth: usize,
        budget: &mut usize,
    ) -> Option<Self> {
        if depth > 128 || *budget == 0 {
            return None;
        }
        *budget -= 1;
        match ty {
            Type::Named(name, arguments) if definitions.contains_key(name) => {
                let definition = definitions.get(name)?;
                if definition.must_consume || definition.is_capability || name.len() > 1_024
                    || arguments.len() > 128 {
                    None
                } else {
                    let shape = crate::aggregates::scalar_aggregate_shape(ty, definitions)?;
                    if shape.tagged || shape.packed || shape.fields.is_empty() || shape.fields.len() > 128 {
                        None
                    } else {
                        let arguments = arguments.iter().map(|argument| {
                            Self::from_checked_type_with_definitions_at(argument, definitions, depth + 1, budget)
                        }).collect::<Option<Vec<_>>>()?;
                        let fields = shape.fields.iter().map(|field| {
                            Self::from_checked_type_with_definitions_at(field, definitions, depth + 1, budget)
                        }).collect::<Option<Vec<_>>>()?;
                        Some(Self::Record { name: name.clone(), arguments, fields })
                    }
                }
            }
            Type::Named(name, arguments) if name == "List" => {
                let [child] = arguments.as_slice() else { return None };
                Some(Self::List(Box::new(Self::from_checked_type_with_definitions_at(
                    child, definitions, depth + 1, budget,
                )?)))
            }
            Type::Named(name, arguments) if name == "Dict" => {
                let [key, value] = arguments.as_slice() else { return None };
                Some(Self::Dict(
                    Box::new(Self::from_checked_type_with_definitions_at(key, definitions, depth + 1, budget)?),
                    Box::new(Self::from_checked_type_with_definitions_at(value, definitions, depth + 1, budget)?),
                ))
            }
            Type::Tuple(fields) if !fields.is_empty() && fields.len() <= 128 => {
                Some(Self::Tuple(fields.iter().map(|field| {
                    Self::from_checked_type_with_definitions_at(field, definitions, depth + 1, budget)
                }).collect::<Option<Vec<_>>>()?))
            }
            _ => Self::from_checked_type_bounded(ty, depth, budget),
        }
    }

    fn from_checked_type_bounded(ty: &Type, depth: usize, budget: &mut usize) -> Option<Self> {
        // Bound both width and depth before identity encoding or physical
        // validation can visit a hostile source type.
        if depth > 128 || *budget == 0 {
            return None;
        }
        *budget -= 1;
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
                    budget,
                )?)))
            }
            Type::Tuple(fields) if !fields.is_empty() && fields.len() <= 128 => {
                Some(Self::Tuple(fields.iter().map(|field| {
                    Self::from_checked_type_bounded(field, depth + 1, budget)
                }).collect::<Option<Vec<_>>>()?))
            }
            Type::Named(name, arguments) if name == "Dict" => {
                let [key, value] = arguments.as_slice() else { return None };
                Some(Self::Dict(
                    Box::new(Self::from_checked_type_bounded(key, depth + 1, budget)?),
                    Box::new(Self::from_checked_type_bounded(value, depth + 1, budget)?),
                ))
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

    pub fn from_view_type_with_definitions(
        ty: &Type,
        definitions: &BTreeMap<String, TypeDef>,
    ) -> Option<Self> {
        let Type::Qualified(TypeQual::View, payload) = ty else { return None };
        Self::from_slice_payload_type_with_definitions(payload, definitions)
    }

    /// Raw candidate consistency only. This does not authenticate nominal
    /// fields: witnessed SafeWIR must reconstruct this proof from checked
    /// definitions and require membership in CanonicalSourceView.
    pub fn matches_view_source_syntax(&self, ty: &Type) -> bool {
        if !self.within_resource_bounds() { return false; }
        let Type::Qualified(TypeQual::View, payload) = ty else { return false };
        let element = match payload.as_ref() {
            Type::Slice(element) => element.as_ref(),
            Type::Named(name, arguments) if name == "Slice" => {
                let [element] = arguments.as_slice() else { return false };
                element
            }
            _ => return false,
        };
        *element == self.source_type()
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

    pub fn from_slice_payload_type_with_definitions(
        payload: &Type,
        definitions: &BTreeMap<String, TypeDef>,
    ) -> Option<Self> {
        let element = match payload {
            Type::Slice(element) => element.as_ref(),
            Type::Named(name, arguments) if name == "Slice" => {
                let [element] = arguments.as_slice() else { return None };
                element
            }
            _ => return None,
        };
        Self::from_checked_type_with_definitions(element, definitions)
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

    pub fn from_list_root_payload_type_with_definitions(
        mut payload: &Type,
        definitions: &BTreeMap<String, TypeDef>,
    ) -> Option<Self> {
        while let Type::Qualified(TypeQual::Frozen, inner) = payload {
            payload = inner;
        }
        let Type::Named(name, arguments) = payload else { return None };
        let [element] = arguments.as_slice() else { return None };
        (name == "List").then(|| Self::from_checked_type_with_definitions(element, definitions)).flatten()
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
            Self::Tuple(fields) => Type::Tuple(fields.iter().map(Self::source_type).collect()),
            Self::Dict(key, value) => Type::Named(
                "Dict".into(), vec![key.source_type(), value.source_type()],
            ),
            Self::Record { name, arguments, .. } => Type::Named(
                name.clone(), arguments.iter().map(Self::source_type).collect(),
            ),
        }
    }

    pub fn ownership(&self) -> CarrierOwnership {
        match self {
            Self::Scalar(_) => CarrierOwnership::Copy,
            Self::String | Self::List(_) | Self::Tuple(_) | Self::Dict(_, _) | Self::Record { .. } => CarrierOwnership::RetainCloneRelease,
        }
    }

    /// A raw candidate can carry a proof without passing through the checked
    /// parser. Enforce the same finite proof domain before recursive physical
    /// matching, using an iterative walk so hostile depth cannot overflow the
    /// checker's own stack.
    pub fn within_resource_bounds(&self) -> bool {
        self.bounded_resource_shape().is_some()
    }

    /// Node count and maximum relative depth for a checked carrier proof.
    /// Composite source identities use these totals to share one budget with
    /// nested carrier plans rather than restarting a budget for every List.
    pub fn bounded_resource_shape(&self) -> Option<(usize, usize)> {
        let mut pending = vec![(self, 0usize)];
        let mut remaining = 4_096usize;
        let mut maximum_depth = 0usize;
        while let Some((element, depth)) = pending.pop() {
            if depth > 128 || remaining == 0 {
                return None;
            }
            remaining -= 1;
            maximum_depth = maximum_depth.max(depth);
            match element {
                Self::Scalar(crate::abi::CanonicalScalarKind::Unit) => return None,
                Self::Scalar(_) | Self::String => {}
                Self::List(child) => pending.push((child, depth + 1)),
                Self::Tuple(fields) => {
                    if fields.is_empty() || fields.len() > 128 {
                        return None;
                    }
                    pending.extend(fields.iter().map(|field| (field, depth + 1)));
                }
                Self::Dict(key, value) => {
                    pending.push((key, depth + 1));
                    pending.push((value, depth + 1));
                }
                Self::Record { name, arguments, fields } => {
                    if name.len() > 1_024 || arguments.len() > 128 || fields.is_empty() || fields.len() > 128 {
                        return None;
                    }
                    pending.extend(arguments.iter().chain(fields).map(|field| (field, depth + 1)));
                }
            }
        }
        Some((4_096 - remaining, maximum_depth))
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
                Self::Tuple(fields) => {
                    if current.ownership() != CarrierOwnership::RetainCloneRelease {
                        return false;
                    }
                    let Some(actual) = query.owned_record_fields(layout) else {
                        return false;
                    };
                    return actual.len() == fields.len()
                        && fields.iter().zip(actual).all(|(field, slot)| {
                            field.matches_layout(query, slot)
                        });
                }
                Self::Dict(key, value) => {
                    let Some(actual) = query.owned_record_fields(layout) else {
                        return false;
                    };
                    let [keys, values] = actual.as_slice() else { return false };
                    let Some(key_layout) = query.owned_retained_list_element(*keys) else {
                        return false;
                    };
                    let Some(value_layout) = query.owned_retained_list_element(*values) else {
                        return false;
                    };
                    return key.matches_layout(query, key_layout)
                        && value.matches_layout(query, value_layout);
                }
                Self::Record { fields, .. } => {
                    let Some(actual) = query.owned_record_fields(layout) else { return false };
                    return actual.len() == fields.len()
                        && fields.iter().zip(actual).all(|(field, slot)| field.matches_layout(query, slot));
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
                CarrierElement::Tuple(fields) => {
                    bytes.push(4);
                    bytes.extend_from_slice(&(fields.len() as u32).to_le_bytes());
                    for field in fields { encode(field, bytes); }
                    bytes.push(5);
                }
                CarrierElement::Dict(key, value) => {
                    bytes.push(6);
                    encode(key, bytes);
                    encode(value, bytes);
                    bytes.push(7);
                }
                CarrierElement::Record { name, arguments, fields } => {
                    bytes.push(8);
                    bytes.extend_from_slice(&(name.len() as u32).to_le_bytes());
                    bytes.extend_from_slice(name.as_bytes());
                    bytes.extend_from_slice(&(arguments.len() as u32).to_le_bytes());
                    for argument in arguments { encode(argument, bytes); }
                    bytes.extend_from_slice(&(fields.len() as u32).to_le_bytes());
                    for field in fields { encode(field, bytes); }
                    bytes.push(9);
                }
            }
        }
        let mut bytes = b"witchy.rooted-slice-carrier.v1\0".to_vec();
        encode(self, &mut bytes);
        *blake3::hash(&bytes).as_bytes()
    }
}
