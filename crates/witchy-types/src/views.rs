//! RFC-0167 authenticated immutable rooted-view formation facts.
//!
//! These facts are deliberately separate from `LoanFacts`: a canonical view is
//! not a lexical loan and must never acquire last-use, escape, or exclusive
//! write-back behaviour.  The facts bind a specific checked `&place` AST node
//! to its root and exact checked projection.  A lowering consumer must retain
//! the root and authenticate the projection from this product; it may not
//! reconstruct either by walking raw source syntax.

use witchy_syntax::ast::{Expr, Function, Item, Module, Type, TypeQual};

use witchy_canonical_source::access::{checked_view_place, CheckedPlace};
use crate::typeck::{Ty, TypeError, TypeTable};

fn terr(message: impl Into<String>) -> TypeError {
    TypeError { message: message.into() }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CheckedViewFact {
    expression: *const Expr,
    place: CheckedPlace,
    through_view: bool,
    payload: Ty,
    dynamic_element: Option<CheckedDynamicElement>,
    dynamic_range: Option<CheckedDynamicRange>,
}

/// The exact checked source index of a dynamic element projection.  As with a
/// dynamic range, this identity is an authentication token, never a raw-AST
/// authority fallback for lowering.
#[derive(Debug, Clone, PartialEq)]
pub struct CheckedDynamicElement {
    index: *const Expr,
}

impl CheckedDynamicElement {
    pub fn matches(&self, index: &Expr) -> bool {
        std::ptr::eq(self.index, index)
    }
}

/// The exact checked source endpoints of a dynamic slice projection.  The
/// pointers are identity tokens only; consumers receive the retained AST and
/// must prove the same nodes before lowering.  They are never dereferenced.
#[derive(Debug, Clone, PartialEq)]
pub struct CheckedDynamicRange {
    lo: *const Expr,
    hi: *const Expr,
    inclusive: bool,
}

impl CheckedDynamicRange {
    pub fn matches(&self, lo: &Expr, hi: &Expr, inclusive: bool) -> bool {
        self.inclusive == inclusive && std::ptr::eq(self.lo, lo) && std::ptr::eq(self.hi, hi)
    }
}

impl CheckedViewFact {
    pub fn place(&self) -> &CheckedPlace {
        &self.place
    }

    /// Whether the projection is formed through a dereferenced rooted view
    /// (`&(*b).f`, `&(*v)[i]`).  The place root then names that view
    /// binding: its carrier retains the original root, and the steps compose
    /// onto the projection it already carries.
    pub fn formed_through_view(&self) -> bool {
        self.through_view
    }

    pub fn payload(&self) -> &Ty {
        &self.payload
    }

    pub fn dynamic_range(&self) -> Option<&CheckedDynamicRange> {
        self.dynamic_range.as_ref()
    }

    pub fn dynamic_element(&self) -> Option<&CheckedDynamicElement> {
        self.dynamic_element.as_ref()
    }
}

/// Facts are valid only for the exact retained AST allocation that was checked.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CheckedViewFacts {
    facts: Vec<CheckedViewFact>,
}

impl CheckedViewFacts {
    pub fn view_for(&self, expression: &Expr) -> Option<&CheckedViewFact> {
        self.facts
            .iter()
            .find(|fact| std::ptr::eq(fact.expression, expression))
    }

    pub fn iter(&self) -> impl Iterator<Item = &CheckedViewFact> {
        self.facts.iter()
    }
}

pub(crate) fn module_contains_canonical_view(module: &Module) -> bool {
    fn contains_type(checked: &Type) -> bool {
        match checked {
            Type::Qualified(TypeQual::View, _) => true,
            Type::Qualified(_, inner) | Type::Slice(inner) => contains_type(inner),
            Type::Named(_, args) | Type::Tuple(args) | Type::Dyn(_, args) => args.iter().any(contains_type),
            Type::Fn(params, result, _, _) => params.iter().any(contains_type) || contains_type(result),
            Type::RecordCompose { base, fields } => contains_type(base) || fields.iter().any(|(_, field)| contains_type(field)),
        }
    }
    fn contains_body_formation(module: &Module) -> bool {
        let mut found = false;
        let _ = witchy_syntax::ast::visit::visit_module_exprs::<()>(module, &mut |expression| {
            found |= matches!(expression, Expr::Unary { op: witchy_syntax::ast::UnOp::Borrow, .. });
            Ok(())
        });
        found
    }
    fn contains_function(function: &Function) -> bool {
        function.params.iter().any(|param| param.ty.as_ref().is_some_and(contains_type))
            || function.ret.as_ref().is_some_and(contains_type)
    }
    let declared = module.items.iter().any(|item| match item {
        Item::Function(function) => contains_function(function),
        Item::Type(definition) => definition.variants.iter().flat_map(|variant| &variant.fields).any(contains_type),
        Item::TypeAlias { ty: alias, .. } => contains_type(alias),
        Item::Trait(definition) => definition.methods.iter().any(|method| {
            method.params.iter().any(|param| param.ty.as_ref().is_some_and(contains_type))
                || method.ret.as_ref().is_some_and(contains_type)
        }),
        Item::Impl(definition) => definition.methods.iter().any(contains_function),
        Item::Comptime(_) | Item::Const { .. } => false,
    });
    declared || contains_body_formation(module)
}

/// Migration-only references still require `LoanFacts`.  A module which mixes
/// them with canonical carriers cannot safely use either fact product as a
/// substitute for the other, so the checked path rejects the combination.
pub(crate) fn module_contains_legacy_loan(module: &Module) -> bool {
    fn contains_type(checked: &Type) -> bool {
        match checked {
            Type::Qualified(
                TypeQual::Borrow(_) | TypeQual::LegacyBorrow(_) | TypeQual::BorrowMut(_),
                _,
            ) => true,
            Type::Qualified(_, inner) | Type::Slice(inner) => contains_type(inner),
            Type::Named(_, args) | Type::Tuple(args) | Type::Dyn(_, args) => args.iter().any(contains_type),
            Type::Fn(params, result, _, _) => params.iter().any(contains_type) || contains_type(result),
            Type::RecordCompose { base, fields } => contains_type(base) || fields.iter().any(|(_, field)| contains_type(field)),
        }
    }
    let declared = module.items.iter().any(|item| match item {
        Item::Function(function) => function.params.iter().any(|param| param.ty.as_ref().is_some_and(contains_type))
            || function.ret.as_ref().is_some_and(contains_type),
        Item::Type(definition) => definition.variants.iter().flat_map(|variant| &variant.fields).any(contains_type),
        Item::TypeAlias { ty, .. } => contains_type(ty),
        Item::Trait(definition) => definition.methods.iter().any(|method| {
            method.params.iter().any(|param| param.ty.as_ref().is_some_and(contains_type))
                || method.ret.as_ref().is_some_and(contains_type)
        }),
        Item::Impl(definition) => definition.methods.iter().any(|function| {
            function.params.iter().any(|param| param.ty.as_ref().is_some_and(contains_type))
                || function.ret.as_ref().is_some_and(contains_type)
        }),
        Item::Comptime(_) | Item::Const { .. } => false,
    });
    if declared {
        return true;
    }
    let mut found = false;
    let _ = witchy_syntax::ast::visit::visit_module_exprs::<()>(module, &mut |expression| {
        found |= matches!(expression, Expr::Unary { op: witchy_syntax::ast::UnOp::BorrowMut, .. });
        Ok(())
    });
    found
}

/// Constants are inlined before the normal runtime checker retains a module.
/// A rooted carrier therefore cannot have a stable owner root at that boundary;
/// reject it before const lowering instead of allowing it to disappear before
/// `CheckedViewFacts` can bind it to an AST allocation.
pub(crate) fn reject_const_view_formation(module: &Module) -> Result<(), TypeError> {
    for item in &module.items {
        let Item::Const { value, .. } = item else { continue };
        let mut rejected = false;
        let _ = witchy_syntax::ast::visit::visit_expr::<()>(value, &mut |expression| {
            rejected |= matches!(expression, Expr::Unary { op: witchy_syntax::ast::UnOp::Borrow, .. });
            Ok(())
        });
        if rejected {
            return Err(terr(
                "immutable view formation is not supported in a const; constants are inlined before rooted-view facts can retain an owner root",
            ));
        }
    }
    Ok(())
}

pub(crate) fn checked_facts(module: &Module, table: &TypeTable) -> Result<CheckedViewFacts, TypeError> {
    let mut facts = CheckedViewFacts::default();
    let mut visit = |expression: &Expr| -> Result<(), TypeError> {
        let Expr::Unary { op: witchy_syntax::ast::UnOp::Borrow, expr } = expression else {
            return Ok(());
        };
        let (place, through_view) = checked_view_place(expr).ok_or_else(|| {
            terr("immutable view formation requires a checked local, field, element, or literal slice projection, optionally through one dereferenced view")
        })?;
        if through_view {
            // The dereferenced base must itself be a checked rooted view; a
            // view is never formed through any other dereference.
            let mut base = expr.as_ref();
            while let Expr::Field { base: next, .. } | Expr::Index { base: next, .. } = base {
                base = next;
            }
            let Expr::Unary { expr: viewed, .. } = base else {
                return Err(terr("immutable view formed through a view has no dereferenced view base"));
            };
            if !matches!(table.type_of(viewed), Some(Ty::View(_))) {
                return Err(terr("immutable view formation dereferences a value that is not a checked rooted view"));
            }
        }
        let (dynamic_element, dynamic_range) = match expr.as_ref() {
            Expr::Index { index, .. } => match index.as_ref() {
                Expr::Range { lo, hi, inclusive }
                    if !matches!(lo.as_ref(), Expr::Int(_)) || !matches!(hi.as_ref(), Expr::Int(_)) =>
                {
                    (None, Some(CheckedDynamicRange {
                        lo: std::ptr::from_ref(lo.as_ref()),
                        hi: std::ptr::from_ref(hi.as_ref()),
                        inclusive: *inclusive,
                    }))
                }
                Expr::Range { .. } | Expr::Int(_) => (None, None),
                index => (Some(CheckedDynamicElement { index: std::ptr::from_ref(index) }), None),
            },
            _ => (None, None),
        };
        if place.has_dynamic_index() && dynamic_element.is_none() && dynamic_range.is_none() {
            return Err(terr(
                "immutable view formation needs an authenticated checked dynamic projection",
            ));
        }
        let Some(Ty::View(payload)) = table
            .type_of(expression)
            .or_else(|| table.view_type_of(expression))
        else {
            return Err(terr("immutable view formation has no checked rooted-view carrier type"));
        };
        facts.facts.push(CheckedViewFact {
            expression: std::ptr::from_ref(expression),
            place,
            through_view,
            payload: payload.as_ref().clone(),
            dynamic_element,
            dynamic_range,
        });
        Ok(())
    };
    witchy_syntax::ast::visit::visit_module_exprs(module, &mut visit)?;
    Ok(facts)
}
