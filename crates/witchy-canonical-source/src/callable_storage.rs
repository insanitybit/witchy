//! Checked callable storage identity shared by layout projection and source
//! classification. The exact signature remains separate from the normalized
//! closure family: the latter names physical storage, not source authority.

use std::collections::BTreeMap;

use witchy_syntax::ast::{CallableQualifiers, Convention, Type, TypeDef};

use crate::aggregates::{concrete_aggregate_identity, memoize_derived_type, owner_carried_view_identity};
use crate::structural_identity::SourceTypeKey;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallableStoragePlan {
    exact_signature: Type,
    family: Type,
    exact_key: [u8; 32],
    family_key: [u8; 32],
}

impl CallableStoragePlan {
    /// A bounded structural description only. Checked-source membership and
    /// the callable ABI/layout relation must be established by consumers.
    pub fn classify(ty: &Type, definitions: &BTreeMap<String, TypeDef>) -> Option<Self> {
        let Type::Fn(parameters, _, conventions, _) = ty else { return None };
        if parameters.len() > 128 || (!conventions.is_empty() && conventions.len() != parameters.len()) {
            return None;
        }
        let exact_key = SourceTypeKey { source: ty }.digest()?;
        let family = canonical_closure_family_type(ty, definitions);
        let family_key = SourceTypeKey { source: &family }.digest()?;
        Some(Self { exact_signature: ty.clone(), family, exact_key, family_key })
    }

    pub fn exact_signature(&self) -> &Type { &self.exact_signature }
    pub fn family(&self) -> &Type { &self.family }
    pub fn exact_key(&self) -> [u8; 32] { self.exact_key }
    pub fn family_key(&self) -> [u8; 32] { self.family_key }
}

/// The physical closure family. This preserves the existing canonical layout
/// normalization exactly; it may erase source qualifiers, so admission must
/// also compare the exact checked signature carried by `CallableStoragePlan`.
pub fn canonical_closure_family_type(ty: &Type, definitions: &BTreeMap<String, TypeDef>) -> Type {
    memoize_derived_type("canonical_closure_key", ty, definitions, || {
        let identity = concrete_aggregate_identity(ty, definitions)
            .unwrap_or_else(|| ty.unqualified().clone());
        closure_family_at(&identity, definitions)
    })
}

fn closure_family_at(ty: &Type, definitions: &BTreeMap<String, TypeDef>) -> Type {
    if let Some(view) = owner_carried_view_identity(ty, definitions) {
        return view;
    }
    match ty.unqualified() {
        Type::Fn(parameters, result, conventions, _) => {
            let conventions = if conventions.is_empty() {
                vec![Convention::Let; parameters.len()]
            } else {
                conventions.clone()
            };
            Type::Fn(
                parameters.iter().map(|parameter| closure_family_at(parameter, definitions)).collect(),
                Box::new(closure_family_at(result, definitions)),
                conventions,
                CallableQualifiers::default(),
            )
        }
        Type::Named(name, arguments) => Type::Named(
            name.clone(),
            arguments.iter().map(|argument| closure_family_at(argument, definitions)).collect(),
        ),
        Type::Tuple(items) => Type::Tuple(
            items.iter().map(|item| closure_family_at(item, definitions)).collect(),
        ),
        other => other.clone(),
    }
}
