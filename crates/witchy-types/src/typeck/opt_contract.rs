//! Checked behavior requirements for the values actually stored at an opt boundary.
use super::*;
use std::collections::BTreeMap;
use witchy_canonical_source::aggregates::scalar_aggregate_shape;
use witchy_canonical_source::storage::{stored_children, StoredChildren};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct OptCallableContract {
    pub(super) params: Vec<StoredOptProof>,
    pub(super) result: Box<StoredOptProof>,
}

impl OptCallableContract {
    pub(super) fn from_lambda(params: &[ast::Param], result: Option<&ast::Type>, definitions: &BTreeMap<String, ast::TypeDef>) -> Self {
        Self {
            params: params.iter().map(|p| p.ty.as_ref().map(|ty| requirement(ty, definitions)).unwrap_or(StoredOptProof::Unknown)).collect(),
            result: Box::new(result.map(|ty| requirement(ty, definitions)).unwrap_or(StoredOptProof::Unknown)),
        }
    }

    pub(super) fn from_function(function: &Function, definitions: &BTreeMap<String, ast::TypeDef>) -> Self {
        Self::from_lambda(&function.params, function.ret.as_ref(), definitions)
    }

    pub(super) fn has_higher_order_boundary(&self) -> bool {
        self.params.iter().any(has_callable) || has_higher_order(&self.result)
    }
}

/// Build a requirement, not a value fact. Only an enforced opt entry may use
/// the result as a proof. Unknown or recursive storage contributes no authority.
pub(super) fn requirement(ty: &ast::Type, definitions: &BTreeMap<String, ast::TypeDef>) -> StoredOptProof {
    fn walk(ty: &ast::Type, definitions: &BTreeMap<String, ast::TypeDef>, active: &mut Vec<ast::Type>, remaining: &mut usize) -> Result<StoredOptProof, ()> {
        if *remaining == 0 || active.len() >= 128 || active.contains(ty) { return Err(()) }
        *remaining -= 1;
        active.push(ty.clone());
        let result = (|| {
            if let ast::Type::Fn(params, result, ..) = ty.unqualified() {
                return Ok(StoredOptProof::Callable(OptCallableContract {
                    params: params.iter().map(|ty| walk(ty, definitions, active, remaining).unwrap_or(StoredOptProof::Unknown)).collect(),
                    result: Box::new(walk(result, definitions, active, remaining).unwrap_or(StoredOptProof::Unknown)),
                }));
            }
            // Sealing restricts construction, not field visibility. Until a
            // checked carrier boundary supplies a stored behavior contract,
            // do not infer one from private implementation closure fields.
            if let ast::Type::Named(name, _) = ty.unqualified()
                && definitions.get(name).is_some_and(|definition| definition.sealed)
            { return Ok(StoredOptProof::Unknown) }
            Ok(match stored_children(ty, definitions)? {
                Some(StoredChildren::Sequence(element)) => StoredOptProof::Sequence(Box::new(walk(&element, definitions, active, remaining)?)),
                Some(StoredChildren::Mapping(_, _)) => StoredOptProof::Unknown,
                Some(StoredChildren::Aggregate(fields)) => {
                    if matches!(ty.unqualified(), ast::Type::Tuple(_)) {
                        StoredOptProof::Elements(fields.iter().map(|field| walk(field, definitions, active, remaining)).collect::<Result<_, _>>()?)
                    } else if let ast::Type::Named(name, _) = ty.unqualified()
                        && let Some(definition) = definitions.get(name)
                    {
                        // `stored_children` already authenticated and instantiated
                        // these fields. Declaration arities only partition them;
                        // unresolved generic parameters need no physical layout.
                        let mut offset = 0;
                        let mut variants = Vec::new();
                        for variant in &definition.variants {
                            let end = offset + variant.fields.len();
                            let children = fields.get(offset..end).ok_or(())?;
                            variants.push(children.iter().map(|field| walk(field, definitions, active, remaining)).collect::<Result<_, _>>()?);
                            offset = end;
                        }
                        if offset != fields.len() { return Err(()) }
                        StoredOptProof::Variants(variants.into_iter().enumerate().collect())
                    } else if let Some(shape) = scalar_aggregate_shape(ty, definitions) {
                        StoredOptProof::Variants(shape.variants.iter().enumerate().map(|(ordinal, variant)| {
                            shape.fields[variant.first_field..variant.first_field + variant.arity].iter()
                                .map(|field| walk(field, definitions, active, remaining)).collect::<Result<Vec<_>, _>>()
                                .map(|fields| (ordinal, fields))
                        }).collect::<Result<_, _>>()?)
                    } else {
                        StoredOptProof::Elements(fields.iter().map(|field| walk(field, definitions, active, remaining)).collect::<Result<_, _>>()?)
                    }
                }
                None => StoredOptProof::Unknown,
            })
        })();
        active.pop();
        // This describes behavior obligations, not physical scalar layout.
        // Normalize callback-free children so inferred scalar/view results
        // and explicitly annotated results have the same empty requirement.
        result.map(|proof| if has_callable(&proof) { proof } else { StoredOptProof::Unknown })
    }
    walk(ty, definitions, &mut Vec::new(), &mut 16_384).unwrap_or(StoredOptProof::Unknown)
}

pub(super) fn has_callable(proof: &StoredOptProof) -> bool {
    match proof {
        StoredOptProof::Callable(_) => true,
        StoredOptProof::Elements(fields) | StoredOptProof::Variant { fields, .. } => fields.iter().any(has_callable),
        StoredOptProof::Variants(variants) => variants.values().flatten().any(has_callable),
        StoredOptProof::Fields(fields) => fields.values().any(has_callable),
        StoredOptProof::Sequence(element) => has_callable(element),
        StoredOptProof::Unknown => false,
    }
}

pub(super) fn has_higher_order(proof: &StoredOptProof) -> bool {
    match proof {
        StoredOptProof::Callable(contract) => contract.has_higher_order_boundary(),
        StoredOptProof::Elements(fields) | StoredOptProof::Variant { fields, .. } => fields.iter().any(has_higher_order),
        StoredOptProof::Variants(variants) => variants.values().flatten().any(has_higher_order),
        StoredOptProof::Fields(fields) => fields.values().any(has_higher_order),
        StoredOptProof::Sequence(element) => has_higher_order(element),
        StoredOptProof::Unknown => false,
    }
}

pub(super) fn satisfies(actual: &StoredOptProof, required: &StoredOptProof) -> bool {
    if !has_callable(required) { return true }
    match (actual, required) {
        (StoredOptProof::Callable(actual), StoredOptProof::Callable(required)) => actual == required,
        (StoredOptProof::Elements(actual), StoredOptProof::Elements(required)) => slots_satisfy(actual, required),
        (StoredOptProof::Variant { ordinal, fields }, StoredOptProof::Variants(variants)) => variants.get(ordinal).is_some_and(|required| slots_satisfy(fields, required)),
        (StoredOptProof::Variants(actual), StoredOptProof::Variants(required)) => !actual.is_empty() && actual.iter().all(|(ordinal, fields)| required.get(ordinal).is_some_and(|required| slots_satisfy(fields, required))),
        (StoredOptProof::Elements(actual), StoredOptProof::Sequence(required)) => actual.iter().all(|a| satisfies(a, required)),
        (StoredOptProof::Sequence(actual), StoredOptProof::Sequence(required)) => satisfies(actual, required),
        _ => false,
    }
}

fn slots_satisfy(actual: &[StoredOptProof], required: &[StoredOptProof]) -> bool {
    actual.len() == required.len() && actual.iter().zip(required).all(|(a, r)| satisfies(a, r))
}

impl Checker {
    pub(super) fn record_opt_value_proof(&self, name: Option<&str>, base: Option<&Expr>, updates: &[(String, Expr)]) -> StoredOptProof {
        let ty = name.and_then(|name| self.ctor_sigs.get(name).map(|(_, ty)| self.resolve(ty)))
            .or_else(|| base.and_then(|base| self.place_ty(base)));
        let Some(Ty::Named(owner, _)) = ty else { return StoredOptProof::Unknown };
        let Some((_, fields)) = self.record_fields.get(owner.as_ref()) else { return StoredOptProof::Unknown };
        let original = base.map(|base| self.expression_opt_value_proof(base)).unwrap_or(StoredOptProof::Unknown);
        StoredOptProof::Variant {
            ordinal: 0,
            fields: fields.iter().enumerate().map(|(index, (field, _))| {
                updates.iter().find(|(name, _)| name == field)
                    .map(|(_, value)| self.expression_opt_value_proof(value))
                    .unwrap_or_else(|| original.slot(index))
            }).collect(),
        }
    }

    pub(super) fn opt_value_requirement(&self, ty: &Ty) -> StoredOptProof {
        ty_to_ast_with_unresolved(&self.resolve(ty), UnresolvedAst::NeutralStorage)
            .map(|ty| requirement(&ty, &self.canonical_definitions)).unwrap_or(StoredOptProof::Unknown)
    }

    pub(super) fn require_opt_value_provenance(&self, expression: &Expr, required: &StoredOptProof, callee: &str, index: usize) -> Result<(), TypeError> {
        if !has_callable(required) {
            return self.reject_opt_callable_erasure(expression, &format!("argument {} to `{}`", index + 1, diagnostic_callable_name(callee)));
        }
        if satisfies(&self.expression_opt_value_proof(expression), required) { return Ok(()) }
        terr(format!("argument {} to `{}` requires an opt-compatible callable in its stored value; the actual value lacks authenticated opt provenance", index + 1, diagnostic_callable_name(callee)))
    }

    pub(super) fn bind_opt_value_proof(&mut self, name: &str, proof: StoredOptProof) {
        if let Some(contract) = proof.callable() { self.remember_opt_callable_provenance(name.to_owned(), contract); }
        self.remember_container_opt_provenance(name.to_owned(), proof);
    }
}
