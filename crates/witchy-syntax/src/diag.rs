//! Shared runtime-abort message templates (RFC-0045).
//!
//! The interpreter (the reference) and the compiled backend must produce
//! *byte-for-byte identical* abort messages. That is only guaranteed if the
//! message text lives in exactly one place. This module is that place: a
//! [`DiagTemplate`] per abort class, one [`DiagTemplate::render`] that turns a
//! template plus its arguments into the interpreter's exact wording, and one
//! [`runtime_error`] that adds the `` `func`, line N: `` location prefix the
//! interpreter's `Display` + `rt_at_line` produce.
//!
//! - The **interpreter** constructs these errors through `render`, so its
//!   messages are these strings by construction.
//! - The **compiled backend** emits a call to the always-linked `__witchy_abort`
//!   host import carrying a template id (see [`DiagTemplate::id`]) plus its
//!   arguments; the host renders the *same* template through `render` and traps
//!   with the `runtime_error`-prefixed string.
//!
//! The template ids are part of the compiled ABI: codegen bakes them into the
//! `__witchy_abort` call arguments, and both hosts (the wasmtime runtime and the
//! browser shim) decode them here. Do not renumber an existing id.

/// One runtime-abort class. Each maps to exactly one interpreter message that,
/// on the compiled backend, was previously a bare `unreachable` trap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagTemplate {
    /// `list index {a} out of bounds (length {b})` — `list.at` / `xs[i]`.
    ListIndexOob,
    /// `bytes index {a} out of bounds (length {b})` — `bytes.at`.
    BytesIndexOob,
    /// `cannot parse `{s}` as an Int` — `string.to_int` on junk.
    ParseInt,
    /// `cannot compare NaN` — ordering (`<`/`<=`/`>`/`>=`) a NaN float.
    NanOrder,
    /// `{s}` — a user `fail(msg)`; the message is passed through verbatim.
    Fail,
}

impl DiagTemplate {
    /// The stable compiled-ABI id for this template (0 is reserved for
    /// "unknown / not a diag abort"). Do not renumber.
    pub const fn id(self) -> i32 {
        match self {
            DiagTemplate::ListIndexOob => 1,
            DiagTemplate::BytesIndexOob => 2,
            DiagTemplate::ParseInt => 3,
            DiagTemplate::NanOrder => 4,
            DiagTemplate::Fail => 5,
        }
    }

    /// Decode a template id back to its variant (the inverse of [`id`]).
    ///
    /// [`id`]: DiagTemplate::id
    pub const fn from_id(id: i32) -> Option<DiagTemplate> {
        match id {
            1 => Some(DiagTemplate::ListIndexOob),
            2 => Some(DiagTemplate::BytesIndexOob),
            3 => Some(DiagTemplate::ParseInt),
            4 => Some(DiagTemplate::NanOrder),
            5 => Some(DiagTemplate::Fail),
            _ => None,
        }
    }

    /// Render the template's *core* message (no location prefix) from its
    /// arguments. `a`/`b` are the integer holes (index, length); `s` is the
    /// string hole (the junk input, or the `fail` message). Unused holes are
    /// ignored. This is the single source of truth for the wording — the
    /// interpreter calls it directly, the compiled host calls it from the
    /// `__witchy_abort` handler.
    pub fn render(self, a: i64, b: i64, s: &str) -> String {
        match self {
            DiagTemplate::ListIndexOob => format!("list index {a} out of bounds (length {b})"),
            DiagTemplate::BytesIndexOob => format!("bytes index {a} out of bounds (length {b})"),
            DiagTemplate::ParseInt => format!("cannot parse `{s}` as an Int"),
            DiagTemplate::NanOrder => "cannot compare NaN".to_string(),
            DiagTemplate::Fail => s.to_string(),
        }
    }
}

/// Build the full runtime-error string a *routed* abort surfaces, reproducing
/// the interpreter's `RuntimeError` `Display` (`runtime error: …`) composed with
/// `rt_at_line` (the `` `func`, line N: `` location prefix). `line == 0` means
/// "no line available" (the prefix is omitted); an empty `func` omits the name
/// but keeps the line. Both backends call this so the surfaced text matches.
pub fn runtime_error(func: &str, line: u32, core: &str) -> String {
    if line == 0 {
        format!("runtime error: {core}")
    } else if func.is_empty() {
        format!("runtime error: line {line}: {core}")
    } else {
        format!("runtime error: `{func}`, line {line}: {core}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip() {
        for t in [
            DiagTemplate::ListIndexOob,
            DiagTemplate::BytesIndexOob,
            DiagTemplate::ParseInt,
            DiagTemplate::NanOrder,
            DiagTemplate::Fail,
        ] {
            assert_eq!(DiagTemplate::from_id(t.id()), Some(t));
        }
        assert_eq!(DiagTemplate::from_id(0), None);
    }

    #[test]
    fn render_matches_interpreter_wording() {
        assert_eq!(
            DiagTemplate::ListIndexOob.render(5, 2, ""),
            "list index 5 out of bounds (length 2)"
        );
        assert_eq!(
            DiagTemplate::BytesIndexOob.render(5, 2, ""),
            "bytes index 5 out of bounds (length 2)"
        );
        assert_eq!(
            DiagTemplate::ParseInt.render(0, 0, "junk"),
            "cannot parse `junk` as an Int"
        );
        assert_eq!(DiagTemplate::NanOrder.render(0, 0, ""), "cannot compare NaN");
        assert_eq!(DiagTemplate::Fail.render(0, 0, "the reason"), "the reason");
    }

    #[test]
    fn runtime_error_prefix() {
        assert_eq!(
            runtime_error("p20.test_fail", 4, "the reason"),
            "runtime error: `p20.test_fail`, line 4: the reason"
        );
        assert_eq!(runtime_error("", 4, "boom"), "runtime error: line 4: boom");
        assert_eq!(runtime_error("f", 0, "boom"), "runtime error: boom");
    }
}
