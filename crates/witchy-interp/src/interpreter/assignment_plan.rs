//! Assignment-place planning: decompose an assignment target into a base
//! binding + projection path, detect whether the RHS reads the place being
//! written (aliasing), and build the desugared in-place assignment plan the
//! evaluator executes. Pure syntactic planning, no evaluator state.

use witchy_syntax::ast::Expr;

use witchy_syntax::intrinsics;

use super::Value;

#[derive(Clone, Debug, PartialEq)]
pub enum PlaceProjection {
    Field(String),
    Index(Value),
}

#[derive(Clone)]
pub(super) struct CapturedPlace {
    pub(super) root: String,
    pub(super) projections: Vec<PlaceProjection>,
}

pub(super) enum AssignmentProjection<'a> {
    Field(&'a str),
    Index { access: &'static str, expression: &'a Expr },
}

pub(super) struct AssignmentPlan<'a> {
    pub(super) projections: Vec<AssignmentProjection<'a>>,
    pub(super) replacement: &'a Expr,
}

// Surface place assignments are desugared before either backend sees them:
// `root[i].field = value` becomes a root assignment built from private
// set-at/record-update expressions. Recover only that structural spine so the
// interpreter can mirror compiled lowering: capture coordinates, evaluate the
// replacement, then apply it to the current root.
pub(super) fn expression_reads_assignment_place(
    expression: &Expr,
    root: &str,
    projections: &[AssignmentProjection<'_>],
) -> bool {
    let Some((projection, prefix)) = projections.split_last() else {
        return matches!(expression, Expr::Var(name) if name == root);
    };
    match (projection, expression) {
        (AssignmentProjection::Field(expected), Expr::Field { base, field }) => {
            field == expected
                && expression_reads_assignment_place(base, root, prefix)
        }
        (
            AssignmentProjection::Index { access, expression: expected },
            Expr::Call { name, args },
        ) => {
            name == access
                && args.len() == 2
                && args[1] == **expected
                && expression_reads_assignment_place(&args[0], root, prefix)
        }
        _ => false,
    }
}

pub(super) fn desugared_assignment_plan<'a>(
    root: &str,
    expression: &'a Expr,
    depth: u32,
) -> Option<AssignmentPlan<'a>> {
    fn decode<'a>(
        root: &str,
        expression: &'a Expr,
        projections: &mut Vec<AssignmentProjection<'a>>,
        remaining: u32,
    ) -> Option<&'a Expr> {
        // Stop at the source RHS even when it has exactly the same shape as
        // another generated rebuild. Its coordinates are separate evaluations.
        if remaining == 0 { return Some(expression); }
        match expression {
            Expr::Call { name, args }
                if args.len() == 3
                    && matches!(
                        name.as_str(),
                        intrinsics::LIST_SET_AT | intrinsics::DICT_INSERT
                    )
                    && expression_reads_assignment_place(
                        &args[0],
                        root,
                        projections,
                    ) =>
            {
                let access = if name == intrinsics::LIST_SET_AT {
                    intrinsics::LIST_AT
                } else {
                    intrinsics::DICT_AT
                };
                projections.push(AssignmentProjection::Index {
                    access,
                    expression: &args[1],
                });
                decode(root, &args[2], projections, remaining - 1)
            }
            Expr::RecordUpdate { name: None, base, fields }
                if fields.len() == 1
                    && expression_reads_assignment_place(
                        base,
                        root,
                        projections,
                    ) =>
            {
                let (field, value) = &fields[0];
                projections.push(AssignmentProjection::Field(field));
                decode(root, value, projections, remaining - 1)
            }
            _ => None,
        }
    }

    let mut projections = Vec::new();
    let replacement = decode(root, expression, &mut projections, depth)?;
    Some(AssignmentPlan { projections, replacement })
}
