use std::{borrow::Cow, ops::Range};

/// Public diagnostic identity and byte span, independent of wording and layout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub code: &'static str,
    pub span: Range<usize>,
    /// Static wording is borrowed. Source-dependent detail belongs to this diagnostic.
    pub message: Cow<'static, str>,
}

/// Whether rendered diagnostics also carry the byte range in the bundled program
/// (`W0401 167581..167586:`). An author reads the file, line and column; the range is kept
/// for the one command a tool reads by that shape (`package-test-consumer`, which Grimoire's
/// rejection tests match), and is off everywhere else (docs/decisions.md, 2026-11-22).
static OFFSETS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
pub fn show_offsets(shown: bool) { OFFSETS.store(shown, std::sync::atomic::Ordering::Relaxed); }
pub(crate) fn offsets_shown() -> bool { OFFSETS.load(std::sync::atomic::Ordering::Relaxed) }

/// A diagnostic that names no place in the source, as it is printed. Most are about the
/// program as a whole (a size limit, a missing `main`); a proof refusal with no place is about
/// the proof run. Either is said in words: `0..0` reads as a place and is none.
pub(crate) fn unplaced(code: &str, message: &str) -> String {
    if offsets_shown() { return format!("{code} 0..0: {message}"); }
    let about = if code == "W0402" { "proof check, no source line" } else { "whole program, no single line" };
    format!("{code} ({about}): {message}")
}

impl Diagnostic {
    pub(crate) fn new(code: &'static str, span: Range<usize>, message: impl Into<Cow<'static, str>>) -> Self {
        let message = message.into();
        let message = match source_spelling(&message) {
            Cow::Borrowed(_) => message,
            Cow::Owned(normalized) => Cow::Owned(normalized),
        };
        // A refusal of a contract's vocabulary says what to do next (`proof_advice::contract_next`).
        let message = if code == "W0401" { crate::compiler::proof_advice::advised(message) } else { message };
        Self {
            code,
            span,
            message,
        }
    }
}

/// A message with the compiler's own spellings of names taken back to the source's: a type
/// private to one of several files declaring the name is `Name@3` inside the compiler, and a
/// blanket impl's methods are `#Trait.method`; neither can be written, so a message naming them
/// read as noise (reviews of definition-site resolution: "`Unit@7.helper` is private").
pub(crate) fn source_spelling(message: &str) -> Cow<'_, str> {
    let bytes = message.as_bytes();
    let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let marked = (0..bytes.len()).any(|i| (bytes[i] == b'@' && i > 0 && word(bytes[i - 1]) && bytes.get(i + 1).is_some_and(u8::is_ascii_digit))
        || (bytes[i] == b'#' && i > 0 && bytes[i - 1] == b'`' && bytes.get(i + 1).is_some_and(u8::is_ascii_uppercase)));
    if !marked { return Cow::Borrowed(message); }
    let mut out = String::with_capacity(message.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'@' && i > 0 && word(bytes[i - 1]) && bytes.get(i + 1).is_some_and(u8::is_ascii_digit) {
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_digit() { i += 1; }
            continue;
        }
        if bytes[i] == b'#' && i > 0 && bytes[i - 1] == b'`' && bytes.get(i + 1).is_some_and(u8::is_ascii_uppercase) { i += 1; continue; }
        let c = message[i..].chars().next().expect("in bounds");
        out.push(c);
        i += c.len_utf8();
    }
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    #[test]
    fn compiler_spellings_of_names_are_shown_as_written() {
        assert_eq!(super::source_spelling("`Unit@7.helper` is private; `#Z.z` too; `Pt@12`"), "`Unit.helper` is private; `Z.z` too; `Pt`");
        assert_eq!(super::source_spelling("user@example 3 # x `#a` a@b"), "user@example 3 # x `#a` a@b");
    }
}
