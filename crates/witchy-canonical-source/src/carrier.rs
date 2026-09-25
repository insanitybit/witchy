//! Recursive, checked-source vocabulary for rooted slice carriers.
//!
//! A carrier is admitted only when the checked frontend includes its element
//! in `CanonicalSourceView` and SafeWIR independently matches the recursive
//! physical layout. This description cannot grant authority on its own.

use std::collections::{BTreeMap, BTreeSet};
use witchy_syntax::ast::{Type, TypeDef, TypeQual};

use crate::abi::CanonicalScalarKind;

/// The value and ownership shape of one slice element. A scalar is copied;
/// managed values and nonempty tuples use owned storage and require retain,
/// clone and release at every containing edge.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CarrierElement {
    Scalar(CanonicalScalarKind),
    String,
    Bytes,
    List(Box<Self>),
    Tuple(Vec<Self>),
    Dict(Box<Self>, Box<Self>),
    /// A checked, one-variant heap record. The declared identity and every
    /// instantiated field are part of the proof, including unused parameters.
    Record { name: String, constructor: String, field_names: Vec<String>, arguments: Vec<Self>, fields: Vec<Self> },
    /// A checked tagged heap aggregate. Variant payloads occupy flattened
    /// nullable slots; only the active variant's slots are populated.
    Sum { name: String, arguments: Vec<Self>, variants: Vec<CarrierVariant> },
}

/// A checked constructor's name and fields are both part of the carrier's
/// structural identity; equal storage with different tag meanings is distinct.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CarrierVariant {
    pub name: String,
    pub field_names: Vec<String>,
    pub fields: Vec<CarrierElement>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CarrierOwnership {
    Copy,
    RetainCloneRelease,
}

/// Nullable owned slots occur only in tagged aggregate payloads. A normal
/// owned pointer cannot silently stand in for one, or vice versa.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CarrierSlot {
    Owned,
    OwnedNullable,
}

/// SafeWIR supplies the physical facts without making this shared contract
/// depend on WIR's layout types. The managed queries return success only for
/// an owned pointer slot and a reference-counted pointee with valid release.
pub trait CarrierLayoutQuery {
    type Layout: Copy;

    fn scalar(&self, layout: Self::Layout) -> Option<CanonicalScalarKind>;
    fn owned_byte_buffer(&self, layout: Self::Layout, slot: CarrierSlot) -> bool;
    fn owned_retained_list_element(&self, layout: Self::Layout, slot: CarrierSlot) -> Option<Self::Layout>;
    fn owned_record_fields(&self, layout: Self::Layout, slot: CarrierSlot) -> Option<Vec<Self::Layout>>;
    /// Require an Int tag followed by the flattened variant field slots.
    fn owned_tagged_fields(&self, layout: Self::Layout, slot: CarrierSlot) -> Option<Vec<Self::Layout>>;
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
        Self::from_checked_type_with_definitions(ty, &BTreeMap::new())
    }

    /// Extend the structural grammar using checked source definitions. A
    /// nominal aggregate is admitted only when each instantiated variant
    /// field has the same recursive ownership proof. Cycles and unproved
    /// fields remain outside the carrier.
    pub fn from_checked_type_with_definitions(
        ty: &Type,
        definitions: &BTreeMap<String, TypeDef>,
    ) -> Option<Self> {
        // Shape validation can ask for a slice element's carrier while it is
        // still validating an enclosing nominal. Reject nominal cycles before
        // entering that cross-module traversal: each shape query has its own
        // budget, so a cycle across the two queries would otherwise reset it.
        if !nominal_graph_acyclic(ty, definitions) { return None; }
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
            Type::Named(name, arguments)
                if definitions.contains_key(name) || matches!(name.as_str(), "Option" | "Result") => {
                if definitions.get(name).is_some_and(|definition| definition.must_consume || definition.is_capability)
                    || name.len() > 1_024 || arguments.len() > 128 {
                    None
                } else {
                    let shape = crate::aggregates::scalar_aggregate_shape(ty, definitions)?;
                    if shape.packed || shape.fields.len() > 128 || shape.variants.len() > 128 {
                        None
                    } else {
                        let arguments = arguments.iter().map(|argument| {
                            Self::from_checked_type_with_definitions_at(argument, definitions, depth + 1, budget)
                        }).collect::<Option<Vec<_>>>()?;
                        if shape.tagged {
                            if shape.variants.len() < 2 { return None; }
                            let variants = shape.variants.iter().enumerate().map(|(index, variant)| {
                                let end = variant.first_field.checked_add(variant.arity)?;
                                let fields = shape.fields.get(variant.first_field..end)?.iter().map(|field| {
                                    Self::from_checked_type_with_definitions_at(field, definitions, depth + 1, budget)
                                }).collect::<Option<Vec<_>>>()?;
                                let field_names = checked_variant_field_names(name, index, variant, definitions)?;
                                Some(CarrierVariant { name: variant.name.clone(), field_names, fields })
                            }).collect::<Option<Vec<_>>>()?;
                            Some(Self::Sum { name: name.clone(), arguments, variants })
                        } else {
                            if shape.fields.is_empty() { return None; }
                            let [variant] = shape.variants.as_slice() else { return None };
                            let field_names = checked_variant_field_names(name, 0, variant, definitions)?;
                            let fields = shape.fields.iter().map(|field| {
                                Self::from_checked_type_with_definitions_at(field, definitions, depth + 1, budget)
                            }).collect::<Option<Vec<_>>>()?;
                            Some(Self::Record { name: name.clone(), constructor: variant.name.clone(), field_names, arguments, fields })
                        }
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
            Type::Tuple(fields) if fields.is_empty() => Some(Self::Scalar(CanonicalScalarKind::Unit)),
            Type::Named(name, arguments) if arguments.is_empty() => match name.as_str() {
                "Nil" | "Unit" => Some(Self::Scalar(CanonicalScalarKind::Unit)),
                "Int" => Some(Self::Scalar(CanonicalScalarKind::Int)),
                "Float" => Some(Self::Scalar(CanonicalScalarKind::Float)),
                "Bool" => Some(Self::Scalar(CanonicalScalarKind::Bool)),
                "Duration" => Some(Self::Scalar(CanonicalScalarKind::Duration)),
                "String" => Some(Self::String),
                "Bytes" => Some(Self::Bytes),
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
            Self::Scalar(CanonicalScalarKind::Unit) => Type::Tuple(Vec::new()),
            Self::Scalar(kind) => Type::Named(
                match kind {
                    CanonicalScalarKind::Int => "Int",
                    CanonicalScalarKind::Float => "Float",
                    CanonicalScalarKind::Bool => "Bool",
                    CanonicalScalarKind::Duration => "Duration",
                    CanonicalScalarKind::Unit => unreachable!("Unit has structural tuple spelling"),
                }
                .into(),
                Vec::new(),
            ),
            Self::String => Type::Named("String".into(), Vec::new()),
            Self::Bytes => Type::Named("Bytes".into(), Vec::new()),
            Self::List(child) => Type::Named("List".into(), vec![child.source_type()]),
            Self::Tuple(fields) => Type::Tuple(fields.iter().map(Self::source_type).collect()),
            Self::Dict(key, value) => Type::Named(
                "Dict".into(), vec![key.source_type(), value.source_type()],
            ),
            Self::Record { name, arguments, .. } => Type::Named(
                name.clone(), arguments.iter().map(Self::source_type).collect(),
            ),
            Self::Sum { name, arguments, .. } => Type::Named(
                name.clone(), arguments.iter().map(Self::source_type).collect(),
            ),
        }
    }

    pub fn ownership(&self) -> CarrierOwnership {
        match self {
            Self::Scalar(_) => CarrierOwnership::Copy,
            Self::String | Self::Bytes | Self::List(_) | Self::Tuple(_) | Self::Dict(_, _) | Self::Record { .. } | Self::Sum { .. } => CarrierOwnership::RetainCloneRelease,
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
                Self::Scalar(_) | Self::String | Self::Bytes => {}
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
                Self::Record { name, constructor, field_names, arguments, fields } => {
                    if name.len() > 1_024 || constructor.len() > 1_024 || arguments.len() > 128 || fields.is_empty() || fields.len() > 128
                        || (!field_names.is_empty() && field_names.len() != fields.len())
                        || field_names.iter().any(|field| field.len() > 1_024) {
                        return None;
                    }
                    pending.extend(arguments.iter().chain(fields).map(|field| (field, depth + 1)));
                }
                Self::Sum { name, arguments, variants } => {
                    if name.len() > 1_024 || arguments.len() > 128 || variants.len() < 2 || variants.len() > 128
                        || variants.iter().any(|variant| variant.name.len() > 1_024
                            || (!variant.field_names.is_empty() && variant.field_names.len() != variant.fields.len())
                            || variant.field_names.iter().any(|field| field.len() > 1_024))
                        || !variants.iter().try_fold(0usize, |count, variant| count.checked_add(variant.fields.len())).is_some_and(|count| count <= 127) {
                        return None;
                    }
                    pending.extend(arguments.iter().chain(variants.iter().flat_map(|variant| &variant.fields)).map(|field| (field, depth + 1)));
                }
            }
        }
        Some((4_096 - remaining, maximum_depth))
    }

    /// The single recursive relation between checked element, physical
    /// storage, and its copy or retain/clone/release operation. Every List
    /// edge is checked independently; any missing fact fails closed.
    pub fn matches_layout<Q: CarrierLayoutQuery>(&self, query: &Q, layout: Q::Layout) -> bool {
        self.within_resource_bounds() && self.matches_layout_at(query, layout, CarrierSlot::Owned)
    }

    fn matches_layout_at<Q: CarrierLayoutQuery>(&self, query: &Q, mut layout: Q::Layout, mut slot: CarrierSlot) -> bool {
        let mut current = self;
        loop {
            match current {
                Self::Scalar(kind) => {
                    return current.ownership() == CarrierOwnership::Copy
                        && query.scalar(layout) == Some(*kind);
                }
                Self::String | Self::Bytes => {
                    return current.ownership() == CarrierOwnership::RetainCloneRelease
                        && query.owned_byte_buffer(layout, slot);
                }
                Self::List(child) => {
                    if current.ownership() != CarrierOwnership::RetainCloneRelease {
                        return false;
                    }
                    let Some(next) = query.owned_retained_list_element(layout, slot) else {
                        return false;
                    };
                    layout = next;
                    current = child;
                    slot = CarrierSlot::Owned;
                }
                Self::Tuple(fields) => {
                    if current.ownership() != CarrierOwnership::RetainCloneRelease {
                        return false;
                    }
                    let Some(actual) = query.owned_record_fields(layout, slot) else {
                        return false;
                    };
                    return actual.len() == fields.len()
                        && fields.iter().zip(actual).all(|(field, slot)| {
                            field.matches_layout_at(query, slot, CarrierSlot::Owned)
                        });
                }
                Self::Dict(key, value) => {
                    let Some(actual) = query.owned_record_fields(layout, slot) else {
                        return false;
                    };
                    let [keys, values] = actual.as_slice() else { return false };
                    let Some(key_layout) = query.owned_retained_list_element(*keys, CarrierSlot::Owned) else {
                        return false;
                    };
                    let Some(value_layout) = query.owned_retained_list_element(*values, CarrierSlot::Owned) else {
                        return false;
                    };
                    return key.matches_layout_at(query, key_layout, CarrierSlot::Owned)
                        && value.matches_layout_at(query, value_layout, CarrierSlot::Owned);
                }
                Self::Record { fields, .. } => {
                    let Some(actual) = query.owned_record_fields(layout, slot) else { return false };
                    return actual.len() == fields.len()
                        && fields.iter().zip(actual).all(|(field, child)| field.matches_layout_at(query, child, CarrierSlot::Owned));
                }
                Self::Sum { variants, .. } => {
                    let Some(actual) = query.owned_tagged_fields(layout, slot) else { return false };
                    let expected = variants.iter().flat_map(|variant| &variant.fields).collect::<Vec<_>>();
                    return actual.len() == expected.len()
                        && expected.iter().zip(actual).all(|(field, child)| {
                            field.matches_layout_at(query, child, CarrierSlot::OwnedNullable)
                        });
                }
            }
        }
    }

    /// Collision-resistant identity of the canonical recursive element. The
    /// layout allocator assigns compact, collision-free local IDs by sorting
    /// these full keys; their truncated digest is never used as an ID.
    pub fn identity(&self) -> [u8; 32] {
        fn encode_field_names(names: &[String], bytes: &mut Vec<u8>) {
            bytes.extend_from_slice(&(names.len() as u32).to_le_bytes());
            for name in names {
                bytes.extend_from_slice(&(name.len() as u32).to_le_bytes());
                bytes.extend_from_slice(name.as_bytes());
            }
        }
        fn encode(element: &CarrierElement, bytes: &mut Vec<u8>) {
            match element {
                CarrierElement::Scalar(kind) => {
                    bytes.push(0);
                    bytes.push(*kind as u8);
                }
                CarrierElement::String => bytes.push(1),
                CarrierElement::Bytes => bytes.push(12),
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
                CarrierElement::Record { name, constructor, field_names, arguments, fields } => {
                    bytes.push(8);
                    bytes.extend_from_slice(&(name.len() as u32).to_le_bytes());
                    bytes.extend_from_slice(name.as_bytes());
                    bytes.extend_from_slice(&(constructor.len() as u32).to_le_bytes());
                    bytes.extend_from_slice(constructor.as_bytes());
                    bytes.extend_from_slice(&(arguments.len() as u32).to_le_bytes());
                    for argument in arguments { encode(argument, bytes); }
                    encode_field_names(field_names, bytes);
                    bytes.extend_from_slice(&(fields.len() as u32).to_le_bytes());
                    for field in fields { encode(field, bytes); }
                    bytes.push(9);
                }
                CarrierElement::Sum { name, arguments, variants } => {
                    bytes.push(10);
                    bytes.extend_from_slice(&(name.len() as u32).to_le_bytes());
                    bytes.extend_from_slice(name.as_bytes());
                    bytes.extend_from_slice(&(arguments.len() as u32).to_le_bytes());
                    for argument in arguments { encode(argument, bytes); }
                    bytes.extend_from_slice(&(variants.len() as u32).to_le_bytes());
                    for variant in variants {
                        bytes.extend_from_slice(&(variant.name.len() as u32).to_le_bytes());
                        bytes.extend_from_slice(variant.name.as_bytes());
                        encode_field_names(&variant.field_names, bytes);
                        bytes.extend_from_slice(&(variant.fields.len() as u32).to_le_bytes());
                        for field in &variant.fields { encode(field, bytes); }
                    }
                    bytes.push(11);
                }
            }
        }
        let mut bytes = b"witchy.rooted-slice-carrier.v1\0".to_vec();
        encode(self, &mut bytes);
        *blake3::hash(&bytes).as_bytes()
    }
}

fn checked_variant_field_names(
    name: &str,
    index: usize,
    variant: &crate::aggregates::ScalarVariant,
    definitions: &BTreeMap<String, TypeDef>,
) -> Option<Vec<String>> {
    let Some(definition) = definitions.get(name) else { return Some(Vec::new()) };
    let declared = definition.variants.get(index)?;
    if declared.name != variant.name || declared.fields.len() != variant.arity
        || (!declared.field_names.is_empty() && declared.field_names.len() != variant.arity)
        || declared.field_names.iter().any(|field| field.len() > 1_024)
    {
        return None;
    }
    Some(declared.field_names.clone())
}

fn nominal_graph_acyclic(ty: &Type, definitions: &BTreeMap<String, TypeDef>) -> bool {
    fn visit(
        ty: &Type,
        definitions: &BTreeMap<String, TypeDef>,
        active: &mut BTreeSet<String>,
        budget: &mut usize,
        depth: usize,
    ) -> bool {
        if depth > 128 || *budget == 0 { return false; }
        *budget -= 1;
        match ty {
            Type::Named(name, arguments) => {
                if arguments.iter().any(|argument| !visit(argument, definitions, active, budget, depth + 1)) {
                    return false;
                }
                let Some(definition) = definitions.get(name) else { return true };
                if definition.name.len() > 1_024 { return false; }
                if active.contains(&definition.name) { return false; }
                active.insert(definition.name.clone());
                let valid = definition.variants.iter().all(|variant| {
                    variant.fields.iter().all(|field| visit(field, definitions, active, budget, depth + 1))
                });
                active.remove(&definition.name);
                valid
            }
            Type::Tuple(fields) | Type::Dyn(_, fields) => fields.iter()
                .all(|field| visit(field, definitions, active, budget, depth + 1)),
            Type::Slice(inner) | Type::Qualified(_, inner) => {
                visit(inner, definitions, active, budget, depth + 1)
            }
            Type::Fn(parameters, result, _, _) => parameters.iter()
                .all(|parameter| visit(parameter, definitions, active, budget, depth + 1))
                && visit(result, definitions, active, budget, depth + 1),
            Type::RecordCompose { base, fields } => visit(base, definitions, active, budget, depth + 1)
                && fields.iter().all(|(_, field)| visit(field, definitions, active, budget, depth + 1)),
        }
    }

    visit(ty, definitions, &mut BTreeSet::new(), &mut 4_096, 0)
}
