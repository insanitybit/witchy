//! The one encoding of compiler-synthesized anonymous union type names.
//!
//! The parser mints `__union…` names for anonymous tag unions, and every later
//! stage (formatter, checker, lowering, SafeWIR) decodes them. The name is a
//! fixed-width decimal record: the variant count, then per variant the tag's
//! byte length, each byte as three digits, and the arity. Source identifiers
//! may not contain `__`, so no user name collides with the encoding.

/// Encode an anonymous union's `(tag, arity)` variants as its synthetic name.
pub fn anon_union_type_name(variants: &[(String, usize)]) -> String {
    let mut suffix = format!("{:010}", variants.len());
    for (tag, arity) in variants {
        suffix.push_str(&format!("{:010}", tag.len()));
        for byte in tag.as_bytes() {
            suffix.push_str(&format!("{byte:03}"));
        }
        suffix.push_str(&format!("{arity:010}"));
    }
    format!("__union{suffix}")
}

/// Decode a synthetic anonymous union name back into its `(tag, arity)`
/// variants; `None` for any other name.
pub fn anon_union_synthetic_variants(name: &str) -> Option<Vec<(String, usize)>> {
    let mut pos = "__union".len();
    let rest = name.strip_prefix("__union")?;
    if rest.len() < 10 {
        return None;
    }
    let count = parse_fixed_width_usize(name, &mut pos, 10)?;
    let mut variants = Vec::with_capacity(count);
    for _ in 0..count {
        let len = parse_fixed_width_usize(name, &mut pos, 10)?;
        let mut bytes = Vec::with_capacity(len);
        for _ in 0..len {
            let byte = parse_fixed_width_usize(name, &mut pos, 3)?;
            if byte > u8::MAX as usize {
                return None;
            }
            bytes.push(byte as u8);
        }
        let tag = String::from_utf8(bytes).ok()?;
        let arity = parse_fixed_width_usize(name, &mut pos, 10)?;
        variants.push((tag, arity));
    }
    (pos == name.len()).then_some(variants)
}

fn parse_fixed_width_usize(s: &str, pos: &mut usize, width: usize) -> Option<usize> {
    let end = pos.checked_add(width)?;
    let part = s.get(*pos..end)?;
    if !part.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    *pos = end;
    part.parse().ok()
}
