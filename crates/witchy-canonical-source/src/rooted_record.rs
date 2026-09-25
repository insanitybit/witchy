//! Typed identity for a retained record-root view's source payload.
//!
//! The checked frontend supplies nominal ordinals. This module only describes
//! the finite structural relation; it cannot mint a checked-source witness.

use std::collections::BTreeMap;

use witchy_syntax::ast::{Type, TypeDef};

use crate::abi::CanonicalScalarKind;
use crate::carrier::CarrierElement;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordRootIdentity {
    Scalar(CanonicalScalarKind),
    String,
    List(CarrierElement),
    Option(Box<Self>),
    Result(Box<Self>, Box<Self>),
    Nominal { ordinal: u32, arguments: Vec<Self> },
}

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
        let mut qualified = false;
        let mut depth = depth;
        while let Type::Qualified(_, inner) = unqualified {
            if depth >= 128 || *budget == 0 {
                return None;
            }
            *budget -= 1;
            depth += 1;
            qualified = true;
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
                (
                    Self::Nominal {
                        ordinal,
                        arguments: children,
                    },
                    contains_list,
                )
            }
            _ => return None,
        };
        // Existing scalar-root identities normalize qualifiers. Never erase a
        // qualifier on or above a newly admitted recursive List child.
        (!qualified || !contains_list).then_some((identity, contains_list))
    }

    /// Deterministic structural bytes, preserving the currently admitted
    /// v3 record-root ABI payloads.
    pub fn append_identity_bytes(&self, bytes: &mut Vec<u8>) -> bool {
        const USER: u8 = 1;
        const OPTION: u8 = 2;
        const RESULT: u8 = 3;
        const LEAF: u8 = 4;
        const LIST: u8 = 5;
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
            Self::Nominal { ordinal, arguments } => {
                bytes.push(USER);
                bytes.extend_from_slice(&ordinal.to_le_bytes());
                bytes.extend_from_slice(&(arguments.len() as u32).to_le_bytes());
                for argument in arguments {
                    if !argument.append_identity_bytes(bytes) {
                        return false;
                    }
                }
            }
        }
        true
    }
}
