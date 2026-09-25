//! Typed identity for a retained record-root view's source payload.
//!
//! The checked frontend supplies nominal ordinals. This module only describes
//! the finite structural relation; it cannot mint a checked-source witness.

use std::collections::{BTreeMap, HashSet};

use witchy_syntax::ast::{Type, TypeDef, TypeQual};

use crate::abi::CanonicalScalarKind;
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
    Scalar(CanonicalScalarKind),
    String,
    List(CarrierElement),
    Qualified { qualifier: RecordRootQualifier, inner: Box<Self> },
    Option(Box<Self>),
    Result(Box<Self>, Box<Self>),
    Nominal { ordinal: u32, arguments: Vec<Self>, shape: [u8; 32] },
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
        let Type::Named(name, arguments) = unqualified else {
            return None;
        };
        let (identity, contains_list) = match (name.as_str(), arguments.as_slice()) {
            ("Int", []) => (Self::Scalar(CanonicalScalarKind::Int), false),
            ("Float", []) => (Self::Scalar(CanonicalScalarKind::Float), false),
            ("Duration", []) => (Self::Scalar(CanonicalScalarKind::Duration), false),
            ("Bool", []) => (Self::Scalar(CanonicalScalarKind::Bool), false),
            ("String", []) => (Self::String, false),
            ("List", [_]) => {
                let plan =
                    CarrierElement::from_checked_type_with_definitions(unqualified, definitions)?;
                let (nodes, relative_depth) = plan.bounded_resource_shape()?;
                if depth.checked_add(relative_depth)? > 128 {
                    return None;
                }
                *budget = (*budget).checked_sub(nodes.saturating_sub(1))?;
                (Self::List(plan), true)
            }
            ("Option", [child]) => {
                let (child, contains_list) =
                    Self::parse(child, definitions, nominal, depth + 1, budget)?;
                (Self::Option(Box::new(child)), contains_list)
            }
            ("Result", [ok, error]) => {
                let (ok, ok_list) = Self::parse(ok, definitions, nominal, depth + 1, budget)?;
                let (error, error_list) =
                    Self::parse(error, definitions, nominal, depth + 1, budget)?;
                (
                    Self::Result(Box::new(ok), Box::new(error)),
                    ok_list || error_list,
                )
            }
            _ if definitions.contains_key(name) && arguments.len() <= 128 => {
                let ordinal = nominal(name)?;
                let mut children = Vec::with_capacity(arguments.len());
                let mut contains_list = false;
                for argument in arguments {
                    let (child, child_list) =
                        Self::parse(argument, definitions, nominal, depth + 1, budget)?;
                    children.push(child);
                    contains_list |= child_list;
                }
                let shape = nominal_shape_identity(unqualified, definitions, depth + 1, budget)?;
                (
                    Self::Nominal {
                        ordinal,
                        arguments: children,
                        shape,
                    },
                    contains_list,
                )
            }
            _ => return None,
        };
        // Preserve the existing refusal for a qualified recursive List edge.
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
        const LIST: u8 = 5;
        const QUALIFIED: u8 = 6;
        const QUALIFIED_END: u8 = 7;
        match self {
            Self::Scalar(kind) => {
                let tag = match kind {
                    CanonicalScalarKind::Int => 1,
                    CanonicalScalarKind::Float => 2,
                    CanonicalScalarKind::Duration => 3,
                    CanonicalScalarKind::Bool => 4,
                    CanonicalScalarKind::Unit => return false,
                };
                bytes.extend_from_slice(&[LEAF, tag]);
            }
            Self::String => bytes.extend_from_slice(&[LEAF, 5]),
            Self::List(plan) => {
                bytes.push(LIST);
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
            Self::Nominal { ordinal, arguments, shape } => {
                bytes.push(USER_SHAPE);
                bytes.extend_from_slice(&ordinal.to_le_bytes());
                bytes.extend_from_slice(&(arguments.len() as u32).to_le_bytes());
                for argument in arguments {
                    if !argument.append_identity_bytes(bytes) {
                        return false;
                    }
                }
                bytes.extend_from_slice(shape);
            }
        }
        true
    }
}

/// Bind a nominal's instantiated declaration shape without treating a
/// matching physical record as source authority. Both the checked frontend
/// and SafeWIR obtain this from the same checked-source definitions.
fn nominal_shape_identity(
    ty: &Type,
    definitions: &BTreeMap<String, TypeDef>,
    depth: usize,
    budget: &mut usize,
) -> Option<[u8; 32]> {
    let mut active = HashSet::new();
    active.insert(ty.clone());
    let mut bytes = b"witchy.record-root-nominal-shape.v1\0".to_vec();
    append_nominal_shape(ty, definitions, &mut bytes, depth, budget, &mut active)?;
    Some(*blake3::hash(&bytes).as_bytes())
}

fn append_nominal_shape(
    ty: &Type,
    definitions: &BTreeMap<String, TypeDef>,
    bytes: &mut Vec<u8>,
    depth: usize,
    budget: &mut usize,
    active: &mut HashSet<Type>,
) -> Option<()> {
    if depth > 128 || *budget == 0 { return None; }
    *budget -= 1;
    let shape = crate::aggregates::scalar_aggregate_shape(ty, definitions)?;
    if shape.variants.is_empty() || shape.variants.len() > 128 || shape.name.len() > 1_024 {
        return None;
    }
    append_text(bytes, &shape.name)?;
    bytes.extend_from_slice(&[u8::from(shape.tagged), u8::from(shape.packed)]);
    bytes.extend_from_slice(&(shape.variants.len() as u32).to_le_bytes());
    let mut next_field = usize::from(shape.tagged);
    if shape.tagged && !matches!(shape.fields.first(), Some(Type::Named(name, arguments))
        if name == "Int" && arguments.is_empty()) {
        return None;
    }
    for variant in &shape.variants {
        if variant.first_field != next_field || variant.name.len() > 1_024 {
            return None;
        }
        let end = next_field.checked_add(variant.arity)?;
        let fields = shape.fields.get(next_field..end)?;
        append_text(bytes, &variant.name)?;
        bytes.extend_from_slice(&(fields.len() as u32).to_le_bytes());
        for field in fields {
            append_field_type(field, definitions, bytes, depth + 1, budget, active)?;
        }
        next_field = end;
    }
    if next_field != shape.fields.len() { return None; }
    Some(())
}

fn append_text(bytes: &mut Vec<u8>, value: &str) -> Option<()> {
    if value.len() > 1_024 { return None; }
    bytes.extend_from_slice(&u32::try_from(value.len()).ok()?.to_le_bytes());
    bytes.extend_from_slice(value.as_bytes());
    Some(())
}

/// A bounded, tagged source-type encoding for instantiated field identities.
/// The caller has already proved the aggregate shape; this binds its source
/// meaning and grants no ownership or layout operation by itself.
fn append_field_type(
    ty: &Type,
    definitions: &BTreeMap<String, TypeDef>,
    bytes: &mut Vec<u8>,
    depth: usize,
    budget: &mut usize,
    active: &mut HashSet<Type>,
) -> Option<()> {
    if depth > 128 || *budget == 0 { return None; }
    *budget -= 1;
    match ty {
        Type::Named(name, arguments) => {
            bytes.push(0);
            append_text(bytes, name)?;
            bytes.extend_from_slice(&u32::try_from(arguments.len()).ok()?.to_le_bytes());
            for argument in arguments {
                append_field_type(argument, definitions, bytes, depth + 1, budget, active)?;
            }
            if definitions.contains_key(name) {
                if active.insert(ty.clone()) {
                    bytes.push(1);
                    let result = append_nominal_shape(ty, definitions, bytes, depth + 1, budget, active);
                    active.remove(ty);
                    result?;
                } else {
                    // The exact named type above is the bounded back edge.
                    bytes.push(2);
                }
            } else {
                bytes.push(0);
            }
        }
        Type::Tuple(fields) => {
            bytes.push(1);
            bytes.extend_from_slice(&u32::try_from(fields.len()).ok()?.to_le_bytes());
            for field in fields {
                append_field_type(field, definitions, bytes, depth + 1, budget, active)?;
            }
        }
        Type::Slice(element) => {
            bytes.push(2);
            append_field_type(element, definitions, bytes, depth + 1, budget, active)?;
        }
        Type::Qualified(qualifier, inner) => {
            bytes.push(3);
            bytes.push(match qualifier {
                TypeQual::Frozen => 0,
                TypeQual::Unique => 1,
                _ => return None,
            });
            append_field_type(inner, definitions, bytes, depth + 1, budget, active)?;
        }
        Type::Fn(parameters, result, conventions, qualifiers) => {
            if !conventions.is_empty() && conventions.len() != parameters.len() { return None; }
            bytes.push(4);
            bytes.extend_from_slice(&u32::try_from(parameters.len()).ok()?.to_le_bytes());
            for (index, parameter) in parameters.iter().enumerate() {
                bytes.push(match conventions.get(index).copied().unwrap_or_default() {
                    witchy_syntax::ast::Convention::Let => 0,
                    witchy_syntax::ast::Convention::Borrow => 1,
                    witchy_syntax::ast::Convention::Var => 2,
                    witchy_syntax::ast::Convention::Own => 3,
                });
                append_field_type(parameter, definitions, bytes, depth + 1, budget, active)?;
            }
            append_field_type(result, definitions, bytes, depth + 1, budget, active)?;
            bytes.extend_from_slice(&[u8::from(qualifiers.pure), u8::from(qualifiers.once)]);
        }
        Type::Dyn(name, arguments) => {
            bytes.push(5);
            append_text(bytes, name)?;
            bytes.extend_from_slice(&u32::try_from(arguments.len()).ok()?.to_le_bytes());
            for argument in arguments {
                append_field_type(argument, definitions, bytes, depth + 1, budget, active)?;
            }
        }
        Type::RecordCompose { .. } => return None,
    }
    Some(())
}
