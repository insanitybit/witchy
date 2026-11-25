//! `Ok(Some(x)):` is a match inside a match: one arm per outer variant, whose body matches the
//! variant's own value against the inner patterns. Each level is then checked as an ordinary match.
use crate::{ast as a, diagnostic::Diagnostic};

/// The arms with every nested pattern moved one level in, or `None` when no arm has one.
/// `through`: the match reads a sum behind a reference, so each variant's value is a reference too
/// and is matched where it is; otherwise the inner match takes the value.
/// `unique` makes the name that holds a variant's value between the two levels; no source name can spell it.
/// The outer match's `_:` arm, carried into an inner match that may or may not leave cases for it.
pub(super) const REST: &str = "_?";

pub(super) fn rewrite(arms: &[a::MatchArm], at: &a::Expr, through: bool, mutable: bool, unique: usize) -> Result<Option<Vec<a::MatchArm>>, Diagnostic> {
    // Through `&mut`, each name is a `&mut` to a different position of the variant's value,
    // taken through the arm's own reference: such loans are counted by field, so they stand
    // side by side, and the whole value still conflicts with them (`match/through-mut-two.wy`).
    let opened = open(arms, at, through, mutable, unique);
    Ok(levels(opened.as_deref().unwrap_or(arms), at, through, unique)?.or(opened))
}

/// `Variant(a, b):` holds its values as a record with a label for each position. The arm binds that
/// record and its body starts by taking it apart; through a reference, each name is a reference to its value.
fn open(arms: &[a::MatchArm], at: &a::Expr, through: bool, mutable: bool, unique: usize) -> Option<Vec<a::MatchArm>> {
    if arms.iter().all(|arm| arm.open.is_empty()) { return None; }
    let expr = |kind| a::Expr { kind, span: at.span.clone() };
    Some(arms.iter().enumerate().map(|(index, arm)| {
        if arm.open.is_empty() { return arm.clone(); }
        let held = format!("?fields{unique}.{index}");
        let mut body = if through {
            arm.open.iter().enumerate().filter(|(_, name)| *name != "_").map(|(position, name)| {
                let place = expr(a::Expression::Field(Box::new(expr(a::Expression::Unary("*", Box::new(expr(a::Expression::Name(held.clone())))))), position.to_string()));
                a::Statement::Bind { mutable: false, name: name.clone(), ty: None, value: expr(a::Expression::Unary(if mutable { "&mut" } else { "&" }, Box::new(place))), proof: None }
            }).collect::<Vec<_>>()
        } else {
            let fields = arm.open.iter().enumerate().map(|(position, name)| (position.to_string(), name.clone())).collect();
            vec![a::Statement::OpenFields { fields, value: expr(a::Expression::Unary("move", Box::new(expr(a::Expression::Name(held.clone()))))), mutable: false, ty: None }]
        };
        body.extend(arm.body.iter().cloned());
        a::MatchArm { variant: arm.variant.clone(), binding: Some(held), body, nested: arm.nested.clone(), open: vec![] }
    }).collect())
}

fn levels(arms: &[a::MatchArm], at: &a::Expr, through: bool, unique: usize) -> Result<Option<Vec<a::MatchArm>>, Diagnostic> {
    if arms.iter().all(|arm| arm.nested.is_none()) { return Ok(None); }
    let mut out: Vec<a::MatchArm> = Vec::new();
    for (index, arm) in arms.iter().enumerate() {
        if arms[..index].iter().any(|earlier| earlier.variant == arm.variant) {
            // Its group was written when the variant was first met; among plain arms a second one is the checker's duplicate.
            if arms.iter().filter(|other| other.variant == arm.variant).all(|other| other.nested.is_none()) { out.push(arm.clone()); }
            continue;
        }
        let group = arms.iter().filter(|other| other.variant == arm.variant).collect::<Vec<_>>();
        if group.iter().all(|other| other.nested.is_none()) { out.push(arm.clone()); continue; }
        // `Variant(_)` or a bare `Variant` after the nested arms takes the variant's remaining cases.
        let plain = group.iter().position(|other| other.nested.is_none());
        if let Some(at_plain) = plain {
            if at_plain + 1 != group.len() || !matches!(group[at_plain].binding.as_deref(), None | Some("_")) {
                return Err(Diagnostic::new("W0401", at.span.clone(), "a variant with a nested pattern has a pattern in each of its arms: name the remaining cases, or end them with `Variant(_)`"));
            }
        }
        let held = format!("?inner{unique}.{index}");
        let name = a::Expr { kind: a::Expression::Name(held.clone()), span: at.span.clone() };
        let value = if through { name } else { a::Expr { kind: a::Expression::Unary("move", Box::new(name)), span: at.span.clone() } };
        let mut inner = group.iter().filter_map(|other| {
            let pattern = other.nested.as_deref()?;
            Some(a::MatchArm { variant: pattern.variant.clone(), binding: pattern.binding.clone(), body: other.body.clone(), nested: pattern.nested.clone(), open: pattern.open.clone() })
        }).collect::<Vec<_>>();
        // What the inner patterns leave out goes where the outer match sends everything else.
        match (plain, arms.iter().find(|other| other.variant == "_" || other.variant == REST)) {
            (Some(at_plain), _) => inner.push(a::MatchArm { variant: "_".into(), binding: None, body: group[at_plain].body.clone(), nested: None, open: vec![] }),
            (None, Some(rest)) => inner.push(a::MatchArm { variant: REST.into(), binding: None, body: rest.body.clone(), nested: None, open: vec![] }),
            (None, None) => {}
        }
        out.push(a::MatchArm { variant: arm.variant.clone(), binding: Some(held), body: vec![a::Statement::Match { value, arms: inner }], nested: None, open: vec![] });
    }
    Ok(Some(out))
}
