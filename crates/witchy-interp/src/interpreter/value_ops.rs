//! Value-level operations the evaluator delegates to: pattern matching,
//! binary-operator evaluation, ordering comparison, and conversion to and from
//! the native runtime value representation.

use std::rc::Rc;

use witchy_syntax::ast::{BinOp, Pattern};
use witchy_syntax::diag::DiagTemplate;

use super::{err, Env, RuntimeError, Value};

pub(super) fn match_pattern(pat: &Pattern, value: &Value, env: &mut Env) -> bool {
    match (pat, value) {
        (Pattern::Wildcard, _) => true,
        (Pattern::Var(name), v) => {
            env.define(Rc::from(name.as_str()), v.clone(), false);
            true
        }
        (Pattern::Ctor { name, args }, Value::Unit) if name == "Nil" && args.is_empty() => true,
        (Pattern::Tuple(pats), Value::Unit) if pats.is_empty() => true,
        (Pattern::Int(a), Value::Int(b)) => a == b,
        (Pattern::Str(a), Value::Str(b)) => *a == **b,
        (Pattern::Bool(a), Value::Bool(b)) => a == b,
        // A Duration literal pattern is carried as whole milliseconds, and a
        // Duration value is an `Int` of milliseconds (Expr::Duration -> Value::Int),
        // so it is exact i64 equality — no float hazard.
        (Pattern::Duration(a), Value::Int(b)) => a == b,
        // `lo..hi` (half-open) / `lo..=hi` (inclusive) against an Int.
        (Pattern::IntRange { lo, hi, inclusive }, Value::Int(b)) => {
            *b >= *lo && (if *inclusive { *b <= *hi } else { *b < *hi })
        }
        // Every alternative binds the same names (checker-enforced), so binding
        // through the first that matches is well-defined.
        (Pattern::Or(alts), v) => alts.iter().any(|p| match_pattern(p, v, env)),
        (Pattern::Ctor { name, args }, Value::Ctor { name: vname, fields }) => {
            name.as_str() == &**vname
                && args.len() == fields.len()
                && args
                    .iter()
                    .zip(fields.iter())
                    .all(|(p, v)| match_pattern(p, v, env))
        }
        (Pattern::AnonCtor { tag, args }, Value::Ctor { name: vname, fields }) => {
            &**vname == format!(".{tag}").as_str()
                && args.len() == fields.len()
                && args
                    .iter()
                    .zip(fields.iter())
                    .all(|(p, v)| match_pattern(p, v, env))
        }
        (Pattern::Tuple(pats), Value::Tuple(items)) => {
            pats.len() == items.len()
                && pats
                    .iter()
                    .zip(items.iter())
                    .all(|(p, v)| match_pattern(p, v, env))
        }
        (Pattern::List { elems, rest }, Value::List(items)) => {
            let len_ok = match rest {
                None => items.len() == elems.len(),
                Some(_) => items.len() >= elems.len(),
            };
            if !len_ok {
                return false;
            }
            if !elems
                .iter()
                .zip(items.iter())
                .all(|(p, v)| match_pattern(p, v, env))
            {
                return false;
            }
            if let Some(Some(name)) = rest {
                let tail = items[elems.len()..].to_vec();
                env.define(Rc::from(name.as_str()), Value::list(tail), false);
            }
            true
        }
        _ => false,
    }
}

pub(super) fn eval_binary(op: BinOp, l: Value, r: Value) -> Result<Value, RuntimeError> {
    use BinOp::*;
    use Value::{Float, Int, Str};
    match op {
        // Int arithmetic WRAPS on overflow — well-defined two's-complement i64,
        // identical to the WASM backend's `i64.add/sub/mul` (so the two backends
        // agree exactly). It never panics the host. Division still errors on the
        // two cases WASM's `i64.div_s` traps on: divide-by-zero and INT_MIN / -1.
        Add | Sub | Mul | Div => match (op, l, r) {
            // `+` on strings concatenates (typeck guarantees both sides are
            // strings; this arm makes the reference semantics value-exact).
            (Add, Str(a), Str(b)) => Ok(Value::str(format!("{a}{b}"))),
            (Add, Int(a), Int(b)) => Ok(Int(a.wrapping_add(b))),
            (Sub, Int(a), Int(b)) => Ok(Int(a.wrapping_sub(b))),
            (Mul, Int(a), Int(b)) => Ok(Int(a.wrapping_mul(b))),
            (Div, Int(_), Int(0)) => err(DiagTemplate::DivisionByZero.render(0, 0, "")),
            (Div, Int(a), Int(b)) => a.checked_div(b).map(Int).ok_or_else(|| RuntimeError {
                message: DiagTemplate::DivisionOverflow.render(0, 0, ""),
            }),
            (Add, Float(a), Float(b)) => Ok(Float(a + b)),
            (Sub, Float(a), Float(b)) => Ok(Float(a - b)),
            (Mul, Float(a), Float(b)) => Ok(Float(a * b)),
            (Div, Float(a), Float(b)) => Ok(Float(a / b)),
            (_, a, b) => err(format!("cannot apply arithmetic to `{a}` and `{b}`")),
        },
        Mod => match (l, r) {
            (Int(_), Int(0)) => err(DiagTemplate::ModuloByZero.render(0, 0, "")),
            (Int(a), Int(b)) => Ok(Int(a.wrapping_rem(b))),
            (a, b) => err(format!("`%` expects two Ints, got `{a}` and `{b}`")),
        },
        BitAnd => match (l, r) {
            (Int(a), Int(b)) => Ok(Int(a & b)),
            (a, b) => err(format!("`&` expects two Ints, got `{a}` and `{b}`")),
        },
        BitOr => match (l, r) {
            (Int(a), Int(b)) => Ok(Int(a | b)),
            (a, b) => err(format!("`|` expects two Ints, got `{a}` and `{b}`")),
        },
        BitXor => match (l, r) {
            (Int(a), Int(b)) => Ok(Int(a ^ b)),
            (a, b) => err(format!("`^` expects two Ints, got `{a}` and `{b}`")),
        },
        Shl => match (l, r) {
            (Int(a), Int(b)) => Ok(Int(a.wrapping_shl(b as u32))),
            (a, b) => err(format!("`<<` expects two Ints, got `{a}` and `{b}`")),
        },
        Shr => match (l, r) {
            (Int(a), Int(b)) => Ok(Int(a.wrapping_shr(b as u32))),
            (a, b) => err(format!("`>>` expects two Ints, got `{a}` and `{b}`")),
        },
        Concat => match (l, r) {
            (Str(a), Str(b)) => {
                // Reuse `a`'s buffer when this value is unshared (the string
                // accumulation fast path); copy-on-write otherwise.
                let mut out = Rc::try_unwrap(a).unwrap_or_else(|rc| (*rc).clone());
                out.push_str(&b);
                Ok(Str(Rc::new(out)))
            }
            (a, b) => err(format!("`<>` expects two Strings, got `{a}` and `{b}`")),
        },
        Eq => Ok(Value::Bool(l == r)),
        NotEq => Ok(Value::Bool(l != r)),
        Lt | LtEq | Gt | GtEq => {
            let ord = compare(&l, &r)?;
            let result = match op {
                Lt => ord == std::cmp::Ordering::Less,
                LtEq => ord != std::cmp::Ordering::Greater,
                Gt => ord == std::cmp::Ordering::Greater,
                GtEq => ord != std::cmp::Ordering::Less,
                _ => unreachable!(),
            };
            Ok(Value::Bool(result))
        }
        And | Or | Coalesce => unreachable!("&&/||/?? are short-circuited in eval"),
    }
}

pub(super) fn compare(l: &Value, r: &Value) -> Result<std::cmp::Ordering, RuntimeError> {
    use Value::*;
    match (l, r) {
        (Int(a), Int(b)) => Ok(a.cmp(b)),
        (Float(a), Float(b)) => a
            .partial_cmp(b)
            .ok_or_else(|| RuntimeError { message: DiagTemplate::NanOrder.render(0, 0, "") }),
        (Str(a), Str(b)) => Ok(a.cmp(b)),
        _ => err(format!("cannot order `{l}` and `{r}`")),
    }
}

// Bridge between the interpreter's `Value` and the registry's `NativeValue` at
// the single native-dispatch site. Native functions are typed (their `.witchy`
// stubs), so they only ever receive the simple shapes `NativeValue` carries; any
// other `Value` is a caller bug surfaced as a runtime error.
pub(super) fn value_to_native(v: &Value) -> Result<witchy_runtime::value::NativeValue, RuntimeError> {
    use witchy_runtime::value::NativeValue as N;
    Ok(match v {
        Value::Int(i) => N::Int(*i),
        Value::Str(s) => N::Str((**s).clone()),
        Value::Bytes(b) => N::Bytes(b.clone()),
        Value::Bool(b) => N::Bool(*b),
        Value::List(xs) => N::List(
            xs.iter().map(value_to_native).collect::<Result<Vec<_>, RuntimeError>>()?,
        ),
        // The native crypto op already passed the reveal gate (use-only is checked
        // before dispatch), so the raw bytes cross without the flag.
        Value::Secret(s, _) => N::Secret(s.clone()),
        other => {
            return Err(RuntimeError {
                message: format!("native function received an unsupported argument: {other}"),
            });
        }
    })
}

pub(super) fn native_to_value(v: witchy_runtime::value::NativeValue) -> Value {
    use witchy_runtime::value::NativeValue as N;
    match v {
        N::Int(i) => Value::Int(i),
        N::Str(s) => Value::str(s),
        N::Bytes(b) => Value::Bytes(b),
        N::Bool(b) => Value::Bool(b),
        N::List(xs) => Value::list(xs.into_iter().map(native_to_value).collect()),
        // A secret produced by a native op (none do today) is revealable by default.
        N::Secret(s) => Value::Secret(s, false),
    }
}
