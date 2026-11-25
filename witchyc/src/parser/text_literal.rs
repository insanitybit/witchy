//! A text literal is borrowed UTF-8, `&str`, wherever nothing asks for more. Where a value
//! must be stored (a record's field, an array's element, a map key) it is a `Text`: the
//! literal's number in the program's table, which `as_str` turns back into the literal's
//! static bytes, and which is given as a `&str` wherever one is expected.
use std::fmt::Write;

/// More literals than this in one program would outgrow one dispatch function, and the test
/// runner packs a literal's number in twelve bits (library/wasm-test-runner.wy).
pub(super) const COUNT: usize = 4096;
pub(super) const TYPE: &str = "Text";

/// `subject` is one of `low..high`: return `leaf` of the one it is, by halving.
fn dispatch(out: &mut String, indent: usize, subject: &str, low: usize, high: usize, leaf: &dyn Fn(usize) -> String) {
    let pad = " ".repeat(indent);
    if high - low == 1 { let _ = writeln!(out, "{pad}return {}", leaf(low)); return; }
    let middle = low + (high - low) / 2;
    let _ = writeln!(out, "{pad}if {subject} < {middle}:");
    dispatch(out, indent + 4, subject, low, middle, leaf);
    dispatch(out, indent, subject, middle, high, leaf);
}

/// The declarations for a program's literals, in first-use order. A `Text` whose number names
/// no literal (the test runner makes them from numbers) reads as empty.
pub(super) fn definitions(texts: &[String]) -> String {
    let mut out = String::from("type Text:\n    id: U64\n\nimpl Text:\n    fn as_str(self: Text) -> &str:\n");
    if texts.is_empty() {
        out.push_str("        return static_utf8_literal(0)\n");
    } else {
        let _ = writeln!(out, "        if self.id >= {}:\n            return static_utf8_literal({})", texts.len(), texts.len());
        dispatch(&mut out, 8, "self.id", 0, texts.len(), &|index| format!("static_utf8_literal({index})"));
    }
    out.push_str("\n    fn byte_len(self: &Text) -> U64:\n        return (*self).as_str().len()\n");
    // Every module's borrowed UTF-8 reads its length and bytes by the names a String's have.
    out.push_str(concat!(
        "\nimpl str:\n",
        "    fn byte_len(self: &str) -> U64:\n        return self.len()\n\n",
        "    fn as_str(self: &str) -> &str:\n        return self\n\n",
        "    fn byte_at(self: &str, index: U64) -> Result<U8, U64>:\n",
        "        let bytes = self.as_bytes()\n",
        "        if index >= bytes.len():\n            return Err(1)\n",
        "        return Ok(bytes[index])\n"));
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn each_literal_is_found_by_halving() {
        let source = super::definitions(&["hé".into(), "123456789".into(), String::new()]);
        assert!(source.starts_with("type Text:\n    id: U64\n"));
        assert!(source.contains("        if self.id >= 3:\n            return static_utf8_literal(3)\n"), "{source}");
        assert!(source.contains("            return static_utf8_literal(0)\n"), "{source}");
        // No literal at all: the type still exists and every read is empty.
        assert!(super::definitions(&[]).contains("        return static_utf8_literal(0)\n"));
    }
}
