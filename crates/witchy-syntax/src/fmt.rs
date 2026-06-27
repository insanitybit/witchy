//! Shared value formatting used by both backends, so rendering stays
//! parity-identical. Kept out of the interpreter so the compiled/native/browser
//! paths can format without depending on the evaluator.

/// Render a `Float` to its canonical string. A finite, whole-valued float keeps a
/// trailing `.0` (so `3.0` renders as `3.0`, visibly distinct from the `Int` `3`);
/// other values use the shortest round-tripping form. Used by the interpreter's
/// `Display` and the WASM `float_to_str` host alike.
pub fn render_float(x: f64) -> String {
    if x.is_finite() && x.fract() == 0.0 {
        format!("{x:.1}")
    } else {
        format!("{x}")
    }
}
