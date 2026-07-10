//! Shared runtime-abort message templates (RFC-0045).
//!
//! The interpreter (the reference) and the compiled backend must produce
//! *byte-for-byte identical* abort messages. Rust wording lives here: a
//! [`DiagTemplate`] per abort class, one [`DiagTemplate::render`] that turns a
//! template plus its arguments into the interpreter's exact wording, and one
//! [`runtime_error`] that adds the `` `func`, line N: `` location prefix the
//! interpreter's `Display` + `rt_at_line` produce. The browser host mirrors the
//! small rendering table and its compiled-abort matrix pins every pure template.
//!
//! - The **interpreter** constructs these errors through `render`, so its
//!   messages are these strings by construction.
//! - The **compiled backend** emits a call to the always-linked `__witchy_abort`
//!   host import carrying a template id (see [`DiagTemplate::id`]) plus its
//!   arguments; the host renders the *same* template through `render` and traps
//!   with the `runtime_error`-prefixed string.
//!
//! The template ids are part of the compiled ABI: codegen bakes them into the
//! `__witchy_abort` call arguments. The wasmtime host decodes them here; the
//! browser host mirrors the rendering table and its compiled-abort matrix pins
//! the same output. Do not renumber an existing id.

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
    /// `math.to_int: NaN cannot be converted to Int`.
    NanToInt,
    /// `dict.at: missing key` — strict `d[k]` dictionary reads.
    DictMissing,
    /// `{s}` — a user `fail(msg)`; the message is passed through verbatim.
    Fail,
    /// `required secret `{s}` was not granted` — `SecretStore.require(name)` on a
    /// name the host did not grant. Matches the interpreter's eager error so the
    /// compiled backend fails at the require site, not lazily (BUG-394).
    SecretRequired,
    /// `division by zero` — integer `/` with a zero divisor.
    DivisionByZero,
    /// `integer overflow in `/`` — `Int::MIN / -1`.
    DivisionOverflow,
    /// `modulo by zero` — integer `%` with a zero divisor.
    ModuloByZero,
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
            DiagTemplate::SecretRequired => 6,
            DiagTemplate::NanToInt => 7,
            DiagTemplate::DictMissing => 8,
            DiagTemplate::DivisionByZero => 9,
            DiagTemplate::DivisionOverflow => 10,
            DiagTemplate::ModuloByZero => 11,
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
            6 => Some(DiagTemplate::SecretRequired),
            7 => Some(DiagTemplate::NanToInt),
            8 => Some(DiagTemplate::DictMissing),
            9 => Some(DiagTemplate::DivisionByZero),
            10 => Some(DiagTemplate::DivisionOverflow),
            11 => Some(DiagTemplate::ModuloByZero),
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
            DiagTemplate::NanToInt => "math.to_int: NaN cannot be converted to Int".to_string(),
            DiagTemplate::DictMissing => "dict.at: missing key".to_string(),
            DiagTemplate::Fail => s.to_string(),
            DiagTemplate::SecretRequired => format!("required secret `{s}` was not granted"),
            DiagTemplate::DivisionByZero => "division by zero".to_string(),
            DiagTemplate::DivisionOverflow => "integer overflow in `/`".to_string(),
            DiagTemplate::ModuloByZero => "modulo by zero".to_string(),
        }
    }
}

/// Pack a static Witchy-string pointer and source line into the single mutable
/// site global exported by an abort-capable module. This is compiled ABI: the
/// function pointer occupies the high 32 bits and the line the low 32 bits.
pub const fn pack_site(func_ptr: u32, line: u32) -> i64 {
    (((func_ptr as u64) << 32) | line as u64) as i64
}

/// Decode the packed abort-site ABI into `(function_string_pointer, line)`.
pub const fn unpack_site(site: i64) -> (u32, u32) {
    let bits = site as u64;
    ((bits >> 32) as u32, bits as u32)
}

/// Build the full runtime-error string a *routed* abort surfaces, reproducing
/// the interpreter's `RuntimeError` `Display` (`runtime error: …`) composed with
/// `rt_at_line` (the `` `func`, line N: `` location prefix). `line == 0` means
/// "no line available" (the prefix is omitted); an empty `func` omits the name
/// but keeps the line. The interpreter and native host call this; the browser
/// host's compiled-abort matrix pins the same formatting.
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
            DiagTemplate::NanToInt,
            DiagTemplate::DictMissing,
            DiagTemplate::Fail,
            DiagTemplate::SecretRequired,
            DiagTemplate::DivisionByZero,
            DiagTemplate::DivisionOverflow,
            DiagTemplate::ModuloByZero,
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
        assert_eq!(
            DiagTemplate::NanToInt.render(0, 0, ""),
            "math.to_int: NaN cannot be converted to Int"
        );
        assert_eq!(DiagTemplate::DictMissing.render(0, 0, ""), "dict.at: missing key");
        assert_eq!(DiagTemplate::Fail.render(0, 0, "the reason"), "the reason");
        assert_eq!(
            DiagTemplate::SecretRequired.render(0, 0, "signing"),
            "required secret `signing` was not granted"
        );
        assert_eq!(DiagTemplate::DivisionByZero.render(0, 0, ""), "division by zero");
        assert_eq!(
            DiagTemplate::DivisionOverflow.render(0, 0, ""),
            "integer overflow in `/`"
        );
        assert_eq!(DiagTemplate::ModuloByZero.render(0, 0, ""), "modulo by zero");
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

    #[test]
    fn packed_site_round_trips_both_u32_halves() {
        let site = pack_site(0xfedc_ba98, 0x7654_3210);
        assert_eq!(unpack_site(site), (0xfedc_ba98, 0x7654_3210));
    }
}
