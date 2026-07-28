//! Shared value formatting used by both backends, so rendering stays
//! parity-identical. Kept out of the interpreter so the compiled/native/browser
//! paths can format without depending on the evaluator.

/// Render a `Float` to its canonical string. A finite, whole-valued float keeps a
/// trailing `.0` (so `3.0` renders as `3.0`, visibly distinct from the `Int` `3`);
/// other values use the shortest round-tripping form. Used by the interpreter's
/// `Display` and the WASM `float_to_str` host alike.
pub fn render_float(x: f64) -> String {
    if x.is_finite() && x.fract() == 0.0 {
        let mut buffer = ryu::Buffer::new();
        let s = buffer.format_finite(x);
        if s.contains('.') || s.contains('e') || s.contains('E') {
            s.to_string()
        } else {
            format!("{s}.0")
        }
    } else {
        let mut buffer = ryu::Buffer::new();
        buffer.format(x).to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::render_float;

    #[test]
    fn whole_floats_keep_shortest_round_trip_digits_with_float_suffix() {
        assert_eq!(render_float(3.0), "3.0");
        assert_eq!(render_float(-0.0), "-0.0");
        assert_eq!(render_float(1234567890123456789.0), "1.2345678901234568e18");
        assert_eq!(render_float(1e308), "1e308");
        assert_eq!(render_float(0.1 + 0.2), "0.30000000000000004");
        assert_eq!(render_float(f64::INFINITY), "inf");
        assert_eq!(render_float(f64::NEG_INFINITY), "-inf");
        assert_eq!(render_float(f64::NAN), "NaN");
    }
}
