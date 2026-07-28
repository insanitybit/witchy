//! Pattern-match exhaustiveness/coverage helpers (RFC-0052, BUG-293).
//!
//! A self-contained cluster of pure functions extracted verbatim from the main
//! checker: or-pattern flattening, the exhaustiveness constructor matrix
//! (`ColCtor` + specialization), and diagnostic pattern rendering. None of these
//! touch checker state — the `Checker` methods in the parent module drive them —
//! so they live here to keep the checker file focused on inference.

use witchy_syntax::ast::{Expr, MatchArm, Pattern};

use super::Ty;

/// A short, human-readable rendering of a pattern for diagnostics.
/// Expand every top-level `Pattern::Or` arm into one arm per alternative
/// (sharing the guard), so the coverage checks — which reason per alternative —
/// see the same shape the old parse-time or-expansion produced. Nested
/// or-patterns are left intact (the checks are permissive on shapes they can't
/// analyze). The body is irrelevant to coverage, so a `Wildcard` placeholder is
/// reused. (RFC-0052)
pub(super) fn flatten_or_arms(arms: &[MatchArm]) -> Vec<MatchArm> {
    let mut out = Vec::with_capacity(arms.len());
    for arm in arms {
        match &arm.pattern {
            Pattern::Or(alts) => {
                for alt in alts {
                    out.push(MatchArm {
                        line: arm.line,
                        pattern: alt.clone(),
                        guard: arm.guard.clone(),
                        body: Expr::Bool(false),
                    });
                }
            }
            _ => out.push(arm.clone()),
        }
    }
    out
}

/// (BUG-293) One entry of a type's constructor signature for the exhaustiveness
/// matrix: a constructor key plus the field types it introduces as sub-columns.
pub(super) struct ColCtor {
    pub(super) key: String,
    pub(super) args: Vec<Ty>,
}

/// Synthetic constructor keys for the structural (non-ADT) constructors.
pub(super) const TUPLE_CTOR: &str = "(tuple)";
pub(super) const LIST_NIL: &str = "(nil)";
pub(super) const LIST_CONS: &str = "(cons)";

/// The constructor key a pattern tests at its column, or `None` if it matches ANY
/// value of that column (a wildcard, a variable, or an open list tail `[..r]`).
/// Literal patterns on open types (Int/String/Duration/range) return a key so they
/// are NOT mistaken for wildcards — they never complete a signature (their types
/// are open), and excluding them from the default matrix keeps a literal-only
/// match non-exhaustive, as it should be.
pub(super) fn pattern_ctor_key(p: &Pattern) -> Option<String> {
    match p {
        Pattern::Wildcard | Pattern::Var(_) => None,
        Pattern::Bool(true) => Some("true".to_string()),
        Pattern::Bool(false) => Some("false".to_string()),
        Pattern::Ctor { name, .. } => Some(name.clone()),
        Pattern::AnonCtor { tag, .. } => Some(format!(".{tag}")),
        Pattern::Tuple(_) => Some(TUPLE_CTOR.to_string()),
        Pattern::List { elems, rest } => {
            if !elems.is_empty() {
                Some(LIST_CONS.to_string())
            } else if rest.is_none() {
                Some(LIST_NIL.to_string())
            } else {
                None // `[..r]` matches any list — wildcard-like for this column
            }
        }
        Pattern::Int(_) | Pattern::Str(_) | Pattern::Duration(_) | Pattern::IntRange { .. } => {
            Some("(literal)".to_string())
        }
        // Expanded away at each column head by `expand_head_or` before this runs.
        Pattern::Or(_) => None,
    }
}

/// Specialize one matrix row by constructor `c`: if the row's head pattern can
/// match `c`, return the row with the head replaced by `c`'s field sub-patterns
/// (so `c`'s arguments become new leading columns); otherwise `None` (the row is
/// dropped, as it can never match `c`).
pub(super) fn specialize_row(row: &[Pattern], c: &ColCtor) -> Option<Vec<Pattern>> {
    let (head, tail) = row.split_first()?;
    let mut new_row = specialize_head(head, c)?;
    new_row.extend_from_slice(tail);
    Some(new_row)
}

fn specialize_head(p: &Pattern, c: &ColCtor) -> Option<Vec<Pattern>> {
    let arity = c.args.len();
    match p {
        Pattern::Wildcard | Pattern::Var(_) => Some(vec![Pattern::Wildcard; arity]),
        Pattern::Bool(b) => (c.key == if *b { "true" } else { "false" }).then(Vec::new),
        Pattern::Ctor { name, args } => (c.key == *name).then(|| args.clone()),
        Pattern::AnonCtor { tag, args } => (c.key == format!(".{tag}")).then(|| args.clone()),
        Pattern::Tuple(ps) => (c.key == TUPLE_CTOR).then(|| ps.clone()),
        Pattern::List { elems, rest } => specialize_list(elems, rest, c),
        // A literal on an open type never reaches a synthetic ctor; drop the row.
        Pattern::Int(_) | Pattern::Str(_) | Pattern::Duration(_) | Pattern::IntRange { .. } => None,
        Pattern::Or(_) => None, // expanded away before specialization
    }
}

/// Specialize a list pattern under the `nil`/`cons` constructors of `List(elem)`.
fn specialize_list(
    elems: &[Pattern],
    rest: &Option<Option<String>>,
    c: &ColCtor,
) -> Option<Vec<Pattern>> {
    match c.key.as_str() {
        // `[]` / `[..r]` match the empty list; a fixed-length prefix does not.
        LIST_NIL => elems.is_empty().then(Vec::new),
        LIST_CONS => {
            if let Some((first, more)) = elems.split_first() {
                // `[e0, e1.., ..r]` = cons(e0, `[e1.., ..r]`)
                Some(vec![first.clone(), Pattern::List { elems: more.to_vec(), rest: rest.clone() }])
            } else if rest.is_some() {
                // `[..r]` matches a non-empty list too: head `_`, tail `_`.
                Some(vec![Pattern::Wildcard, Pattern::Wildcard])
            } else {
                None // `[]` is not a cons
            }
        }
        _ => None,
    }
}

/// Expand a head-position or-pattern into one row per alternative (recursively, so
/// a nested `1 | 2 | 3` fully flattens). Non-or heads pass through unchanged.
pub(super) fn expand_head_or(row: &[Pattern]) -> Vec<Vec<Pattern>> {
    match row.first() {
        Some(Pattern::Or(alts)) => alts
            .iter()
            .flat_map(|alt| {
                let mut r = vec![alt.clone()];
                r.extend_from_slice(&row[1..]);
                expand_head_or(&r)
            })
            .collect(),
        _ => vec![row.to_vec()],
    }
}

pub(super) fn describe_pattern(p: &Pattern) -> String {
    match p {
        Pattern::Wildcard => "_".to_string(),
        Pattern::Var(n) => n.clone(),
        Pattern::Int(n) => n.to_string(),
        Pattern::Str(s) => format!("\"{s}\""),
        Pattern::Bool(b) => b.to_string(),
        Pattern::Ctor { name, args } if args.is_empty() => name.clone(),
        Pattern::Ctor { name, args } => {
            format!("{name}({})", args.iter().map(describe_pattern).collect::<Vec<_>>().join(", "))
        }
        Pattern::AnonCtor { tag, args } if args.is_empty() => format!(".{tag}"),
        Pattern::AnonCtor { tag, args } => {
            format!(
                ".{tag}({})",
                args.iter().map(describe_pattern).collect::<Vec<_>>().join(", ")
            )
        }
        Pattern::Tuple(ps) => {
            format!("({})", ps.iter().map(describe_pattern).collect::<Vec<_>>().join(", "))
        }
        Pattern::List { .. } => "list pattern".to_string(),
        Pattern::Duration(ms) => describe_duration_pattern(*ms),
        Pattern::IntRange { lo, hi, inclusive } => {
            format!("{lo}{}{hi}", if *inclusive { "..=" } else { ".." })
        }
        Pattern::Or(alts) => alts.iter().map(describe_pattern).collect::<Vec<_>>().join(" | "),
    }
}

fn describe_duration_pattern(ms: i64) -> String {
    if ms < 0 {
        return format!("-{}", describe_duration_pattern(ms.saturating_abs()));
    }

    let hours = ms / 3_600_000;
    let minutes = (ms / 60_000) % 60;
    let seconds = (ms / 1_000) % 60;
    let millis = ms % 1_000;

    if hours > 0 {
        format!("{hours}h{minutes}m{seconds}s")
    } else if minutes > 0 {
        format!("{minutes}m{seconds}s")
    } else if seconds > 0 {
        format!("{seconds}s")
    } else {
        format!("{millis}ms")
    }
}
