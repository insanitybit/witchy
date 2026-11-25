//! Derive recipes declared in a library (docs/plan-derive-laws.md, slice 1):
//!
//! ```text
//! pub derive Key:
//!     impl Eq for Self:
//!         fn eq(self: &Self, other: &Self) -> Bool:
//!             static for f in fields:
//!                 if (*self).f != (*other).f:
//!                     return false
//!             return true
//! ```
//!
//! and at a type, `type Account derive(keys.Key(by: id)):`. The recipe is a template of ordinary
//! declarations: `Self` is the type the derive is applied to, and a `static for NAME in fields:`
//! block is written once per selected field (all of them, in order, unless the type names a
//! selection with `by:`), with `NAME` replaced by the field's name. A second binding carries its declared type. That is all the template
//! language there is: no compile-time evaluation, and the expansion is bounded by the number of
//! fields. What it writes is parsed and checked as if the type's author had written it, as the
//! built-in derives' impls are (derive.rs). `static for` exists only here.
use crate::{ast::Type, diagnostic::Diagnostic};
use std::collections::BTreeMap;

/// A recipe: the file declaring it, whether it is `pub`, its body (indented as written) and
/// where the declaration starts in the bundled source.
#[derive(Clone, Debug)]
pub(super) struct Recipe {
    pub public: bool,
    pub body: String,
    pub at: usize,
    /// Where the body starts and ends in the bundled source: an error in what the recipe
    /// writes is reported at the body line it was written from.
    pub body_at: usize,
    pub end: usize,
}

/// Take every top-level `derive NAME:` / `pub derive NAME:` with its indented body out of the
/// source, keyed by (file, name). The lines are blanked to spaces, so every offset after them
/// (spans, file starts) is unchanged.
pub(super) fn lift(source: &str, modules: &[usize]) -> Result<(String, BTreeMap<(u32, String), Recipe>), Diagnostic> {
    let file = |at: usize| modules.iter().filter(|first| **first <= at).count().saturating_sub(1) as u32;
    let mut recipes = BTreeMap::new();
    let mut out = source.as_bytes().to_vec();
    // Each line with its start offset.
    let mut lines = Vec::new();
    let mut start = 0;
    for line in source.split_inclusive('\n') { lines.push((start, line)); start += line.len(); }
    let mut index = 0;
    while index < lines.len() {
        let (at, line) = lines[index];
        let text = line.trim_end_matches(['\n', '\r']);
        let (public, rest) = match text.strip_prefix("pub derive ") {
            Some(rest) => (true, rest),
            None => match text.strip_prefix("derive ") { Some(rest) => (false, rest), None => { index += 1; continue } },
        };
        let Some(name) = rest.strip_suffix(':').map(str::trim) else {
            return Err(Diagnostic::new("W0100", at..at + text.len(), "a derive recipe is declared `pub derive NAME:` with its impls indented under it"));
        };
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') || !name.starts_with(|c: char| c.is_ascii_uppercase()) {
            return Err(Diagnostic::new("W0100", at..at + text.len(), "a derive recipe's name is one capitalized word, as a trait's is"));
        }
        // The built-in derives keep their meaning everywhere (review: a file's own `derive Eq:`
        // replaced `derive(Eq)` for its types).
        if matches!(name, "Eq" | "Hash" | "Before" | "Discard") {
            return Err(Diagnostic::new("W0100", at..at + text.len(), "`Eq`, `Hash`, `Before` and `Discard` are the built-in derives: give the recipe another name"));
        }
        // The body: every following line that is blank or indented, within this file (review: a
        // recipe ending a file took the next file's indented lines).
        let next_file = modules.iter().copied().find(|first| *first > at).unwrap_or(usize::MAX);
        let mut end = index + 1;
        while end < lines.len() && lines[end].0 < next_file {
            let body_line = lines[end].1;
            if body_line.trim().is_empty() || body_line.starts_with([' ', '\t']) { end += 1; } else { break; }
        }
        let body: String = lines[index + 1..end].iter().map(|(_, l)| *l).collect();
        if body.trim().is_empty() {
            return Err(Diagnostic::new("W0100", at..at + text.len(), "a derive recipe needs a body: the impls it writes, indented under it"));
        }
        // Lexed here, so a recipe no type uses is still well formed, and an error points into it.
        let body_start = lines[index + 1].0;
        // The expansion's escape for `@` and `#` (parser.rs `lex_written`): written by the
        // recipe itself it would become one of those characters (review: `"__witchy_hash__x"`
        // printed `#x`), so it is refused.
        if let Some(offset) = body.find("__witchy_") {
            return Err(Diagnostic::new("W0100", body_start + offset..body_start + offset + 9, "a derive recipe may not write `__witchy_`, the compiler's escape for generated names"));
        }
        match crate::lexer::lex(&body) {
            Err(mut error) => {
                error.span = error.span.start + body_start..error.span.end + body_start;
                return Err(error);
            }
            // What the recipe's own literals may spell in a `${...}` (interpolation.rs).
            Ok(tokens) => if let Err(mut error) = super::interpolation::written_names(&tokens) {
                error.span = error.span.start + body_start..error.span.end + body_start;
                return Err(error);
            },
        }
        let key = (file(at), name.to_string());
        if recipes.contains_key(&key) {
            return Err(Diagnostic::new("W0100", at..at + text.len(), format!("a second derive recipe named `{name}` in this file")));
        }
        let stop = lines.get(end).map_or(source.len(), |(s, _)| *s);
        recipes.insert(key, Recipe { public, body, at, body_at: body_start, end: stop });
        for byte in &mut out[at..stop] { if *byte != b'\n' { *byte = b' '; } }
        index = end;
    }
    Ok((String::from_utf8(out).expect("only ASCII bytes replaced by spaces"), recipes))
}

fn indent(line: &str) -> usize { line.len() - line.trim_start_matches(' ').len() }

/// Replace the whole word `word` with `with` (a word is letters, digits and `_`) in code, not in
/// text or scalar literals, except inside a literal's `${...}`, which is code again, to any depth
/// (reviews: `"Self is it"` printed the type's name; a `'"'` scalar, or a `"}"` or `"{"` inside a
/// `${...}`, put the scanner in the wrong mode, so a field after it was left unsubstituted and the
/// impl read another field of the same name).
/// Whether `word` is a whole word in the code of `body` (not in its literals or comments).
pub(super) fn writes_word(body: &str, word: &str) -> bool { body.lines().any(|line| { let code = code_only(line); replace_word(&code, word, "") != code }) }

/// The line without its `//` comment, as the lexer reads it.
fn code_only(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    Scan { line, pairs: &[], comments: false, marks: Default::default() }.code(0, false, &mut out);
    out
}

fn replace_word(line: &str, word: &str, with: &str) -> String { replace_words(line, &[(word, with)]) }

/// Every word of `pairs` replaced in one pass, so what one replacement writes is never replaced
/// again (third review: the loop's field, then `Self`, then `For` in turn made a field named
/// `For` read the field named by `for:`).
fn replace_words(line: &str, pairs: &[(&str, &str)]) -> String { replace_words_marked(line, pairs).0 }

/// `replace_words`, and where in what it wrote each replacement lies (byte ranges).
fn replace_words_marked(line: &str, pairs: &[(&str, &str)]) -> (String, Vec<std::ops::Range<usize>>) {
    let mut out = String::with_capacity(line.len());
    let scan = Scan { line, pairs, comments: true, marks: Default::default() };
    scan.code(0, false, &mut out);
    (out, scan.marks.into_inner())
}

struct Scan<'a> { line: &'a str, pairs: &'a [(&'a str, &'a str)], comments: bool, marks: std::cell::RefCell<Vec<std::ops::Range<usize>>> }

impl Scan<'_> {
    fn wordy(b: u8) -> bool { b.is_ascii_alphanumeric() || b == b'_' }

    /// Code from `i`. Inside a `${...}` (`nested`) it reads as the lexer reads a value's source
    /// (parser/interpolation.rs `closing`): a `"` opens a literal and the first `}` outside one
    /// closes the value, unconsumed; a `'`, a `{` or a `//` there means nothing (third review: the
    /// scanner took `${0 // '}` for a scalar and `${0 // {}` for a brace, and the lexer did not).
    fn code(&self, mut i: usize, nested: bool, out: &mut String) -> usize {
        let bytes = self.line.as_bytes();
        while i < bytes.len() {
            let rest = &self.line[i..];
            let c = rest.chars().next().expect("in bounds");
            match c {
                '"' => { out.push('"'); i = self.text(i + 1, out); continue; }
                '}' if nested => return i,
                '/' if !nested && rest.starts_with("//") => { if self.comments { out.push_str(rest); } return bytes.len(); }
                '\'' if !nested => { i = self.scalar(i, out); continue; }
                _ => {}
            }
            let at_start = i == 0 || !Self::wordy(bytes[i - 1]);
            let found = self.pairs.iter().find(|(word, _)| at_start && rest.starts_with(word) && bytes.get(i + word.len()).is_none_or(|b| !Self::wordy(*b)));
            if let Some((word, with)) = found {
                self.marks.borrow_mut().push(out.len()..out.len() + with.len());
                out.push_str(with);
                i += word.len();
            } else {
                out.push(c);
                i += c.len_utf8();
            }
        }
        i
    }

    /// A text literal's content from `i` (after its `"`), through its closing `"`.
    fn text(&self, mut i: usize, out: &mut String) -> usize {
        let bytes = self.line.as_bytes();
        while i < bytes.len() {
            let rest = &self.line[i..];
            let c = rest.chars().next().expect("in bounds");
            if c == '\\' && let Some(next) = rest[1..].chars().next() {
                out.push(c); out.push(next); i += 1 + next.len_utf8(); continue;
            }
            if rest.starts_with("${") {
                out.push_str("${");
                i = self.code(i + 2, true, out);
                if i < bytes.len() { out.push('}'); i += 1; }
                continue;
            }
            out.push(c);
            i += c.len_utf8();
            if c == '"' { return i; }
        }
        i
    }

    /// A scalar literal from its opening `'`, through its closing `'`, copied as it is.
    fn scalar(&self, mut i: usize, out: &mut String) -> usize {
        let bytes = self.line.as_bytes();
        out.push('\''); i += 1;
        while i < bytes.len() {
            let rest = &self.line[i..];
            let c = rest.chars().next().expect("in bounds");
            if c == '\\' && let Some(next) = rest[1..].chars().next() {
                out.push(c); out.push(next); i += 1 + next.len_utf8(); continue;
            }
            out.push(c);
            i += c.len_utf8();
            if c == '\'' { return i; }
        }
        i
    }
}

/// The lines of a `static for` block for one field: a `static if NAME is TYPE:` block (and the
/// `static else:` after it) keeps the body of the branch the field's declared type selects,
/// one level less indented. Read-only: the type as declared is compared with the type written,
/// at expansion, and nothing else is computed (2026-11-19, the user's call).
fn select<'a>(block: &[&'a str], name: &str, field_type: &Type) -> Result<Vec<&'a str>, String> {
    let mut out = Vec::new();
    let mut index = 0;
    while index < block.len() {
        let line = block[index];
        let trimmed = line.trim();
        if trimmed == "static else:" { return Err("`static else:` follows a `static if` block".to_string()); }
        let Some(rest) = trimmed.strip_prefix("static if ") else { out.push(line); index += 1; continue };
        let Some(test) = rest.strip_suffix(':') else { return Err("a type test is written `static if NAME is TYPE:`".to_string()) };
        let Some((tested, written)) = test.split_once(" is ") else { return Err("a type test is written `static if NAME is TYPE:`".to_string()) };
        // A type named through a module would be compared as written, and never match the field's
        // declared type (review of sum recipes): refused, not quietly the other branch.
        if written.contains('.') { return Err(format!("`static if ... is {}:` names the type as the field declares it, without a module", written.trim())); }
        if tested.trim() != name { return Err(format!("`static if` tests the field of its `static for`, `{name}`")); }
        let outer = indent(line);
        let body = |from: usize| -> (Vec<&'a str>, usize) {
            let mut end = from;
            while end < block.len() && (block[end].trim().is_empty() || indent(block[end]) > outer) { end += 1; }
            (block[from..end].iter().copied().filter(|l| !l.trim().is_empty()).collect(), end)
        };
        let (yes, mut end) = body(index + 1);
        let mut no = Vec::new();
        if end < block.len() && indent(block[end]) == outer && block[end].trim() == "static else:" {
            let (other, after) = body(end + 1);
            no = other;
            end = after;
        }
        if yes.is_empty() { return Err("a `static if` block needs a body".to_string()); }
        if yes.iter().chain(&no).any(|l| l.trim().starts_with("static ")) {
            return Err("a `static if` block holds no other `static` block".to_string());
        }
        let written = written.trim();
        let wanted = super::parse_type(written).map_err(|e| format!("`static if ... is {written}:` names a type: {}", e.message))?;
        let chosen = if *field_type == wanted { yes } else { no };
        if let Some(inner) = chosen.first().map(|l| indent(l)) {
            for l in chosen { out.push(&l[(inner - outer).min(indent(l))..]); }
        }
        index = end;
    }
    Ok(out)
}

/// What a recipe repeats over: a record's selected fields (with the test for `static if`), or a
/// sum's variants, each with how many values it holds.
pub(super) enum Shape<'a> {
    Fields(&'a [(&'a str, &'a Type)]),
    Variants(&'a [(String, usize)]),
}

/// A `static for` line's names and what they range over: `static for a, b in X, Y:`.
fn loop_head(trimmed: &str) -> Option<(Vec<&str>, Vec<&str>)> {
    let rest = trimmed.strip_prefix("static for ")?.strip_suffix(':')?;
    let (names, over) = rest.split_once(" in ")?;
    Some((names.split(',').map(str::trim).collect(), over.split(',').map(str::trim).collect()))
}

/// The lines under `lines[index]` (more indented than it), and the index after them.
fn indented_block<'a>(lines: &[&'a str], index: usize) -> (Vec<&'a str>, usize) {
    let outer = indent(lines[index]);
    let mut end = index + 1;
    while end < lines.len() && (lines[end].trim().is_empty() || indent(lines[end]) > outer) { end += 1; }
    (lines[index + 1..end].iter().copied().filter(|l| !l.trim().is_empty()).collect(), end)
}

fn word_ok(name: &str) -> Result<(), String> {
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') || name.starts_with(|c: char| c.is_ascii_digit()) {
        return Err("`static for` names one word for each thing it ranges over".to_string());
    }
    if matches!(name, "Self" | "For" | "fields" | "variants" | "static" | "for" | "in") {
        return Err(format!("`{name}` has a meaning in a recipe: name it something else"));
    }
    Ok(())
}

/// Per byte of `line`, whether it is code: not inside a text literal's text (a `${...}` in one is
/// code again), a scalar literal or a `//` comment.
fn code_mask(line: &str) -> Vec<bool> {
    let bytes = line.as_bytes();
    let mut mask = vec![false; bytes.len()];
    // What the scanner is in: code (`true`) or a literal's text (`false`), innermost last.
    let mut stack = vec![true];
    let mut i = 0;
    while i < bytes.len() {
        let code = *stack.last().expect("never empty");
        if code {
            match bytes[i] {
                b'"' => { stack.push(false); i += 1; continue; }
                b'}' if stack.len() > 1 => { stack.pop(); i += 1; continue; }
                b'/' if stack.len() == 1 && bytes.get(i + 1) == Some(&b'/') => break,
                b'\'' if stack.len() == 1 => {
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'\'' { i += if bytes[i] == b'\\' { 2 } else { 1 }; }
                    i += 1;
                    continue;
                }
                _ => { mask[i] = true; i += 1; }
            }
        } else {
            match bytes[i] {
                b'\\' => i += 2,
                b'$' if bytes.get(i + 1) == Some(&b'{') => { stack.push(true); i += 2; }
                b'"' => { stack.pop(); i += 1; }
                _ => i += 1,
            }
        }
    }
    mask
}

/// `V(..mine)` in a variant's lines: the arm's pattern, with one binding per value the variant
/// holds, `V(mine0, mine1)`, `V(mine0)` or `V` alone (`variant` is the loop's word for V).
fn patterns(line: &str, variant: &str, count: usize, bound: &mut Vec<String>) -> Result<String, String> {
    let open = format!("{variant}(..");
    // Only in code: a literal's text or a comment holding the same letters is left as written, and
    // binds nothing (review of sum recipes: a comment's `v(..cc)` let `static for a in cc:` pass).
    let mask = code_mask(line);
    let mut out = String::new();
    let mut rest = line;
    while let Some(at) = rest.find(&open) {
        let offset = line.len() - rest.len() + at;
        let before_ok = rest[..at].chars().next_back().is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '_' || c == '.'));
        if !before_ok || !mask.get(offset).copied().unwrap_or(false) { out.push_str(&rest[..at + open.len()]); rest = &rest[at + open.len()..]; continue; }
        let after = &rest[at + open.len()..];
        let Some(close) = after.find(')') else { return Err(format!("`{variant}(..NAME)` names its bindings with one word")) };
        let name = after[..close].trim();
        word_ok(name)?;
        if !bound.iter().any(|b| b == name) { bound.push(name.to_string()); }
        out.push_str(&rest[..at]);
        out.push_str(variant);
        if count > 0 { out.push_str(&format!("({})", (0..count).map(|k| format!("{name}{k}")).collect::<Vec<_>>().join(", "))); }
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// One variant's lines: its arm patterns written out, and each `static for a, b in X, Y:` over
/// the values bound by `V(..X)` and `V(..Y)` written once per value, `a` as `X0`, `X1`...
fn variant_lines(block: &[&str], variant: &str, count: usize, out: &mut Vec<Unrolled>, outer: &[(String, String)]) -> Result<(), String> {
    let mut bound = Vec::new();
    let mut index = 0;
    while index < block.len() {
        let line = block[index];
        let trimmed = line.trim();
        if trimmed.starts_with("static if ") || trimmed == "static else:" {
            return Err("a type test, `static if NAME is TYPE:`, is written over a record's fields".to_string());
        }
        if trimmed.starts_with("static for ") {
            let Some((names, over)) = loop_head(trimmed) else { return Err("a recipe repeats over a variant's values as `static for a, b in X, Y:`".to_string()) };
            if names.len() != over.len() { return Err("`static for a, b in X, Y:` names one word for each binding list".to_string()); }
            for name in &names { word_ok(name)?; }
            // Each name once, and none the outer loop's (review of sum recipes: `static for a, a`
            // read both as the first list, and an inner `t` named like the outer tag became it).
            if let Some(name) = names.iter().enumerate().find(|(i, n)| names[..*i].contains(n) || outer.iter().any(|(w, _)| w == *n)).map(|(_, n)| n) {
                return Err(format!("`{name}` is named twice in this recipe's `static for` loops: give each its own name"));
            }
            for list in &over {
                if !bound.iter().any(|b| b == list) { return Err(format!("`{list}` is not bound by a pattern `{variant}(..{list})` before this `static for`")); }
            }
            let (inner, end) = indented_block(block, index);
            let Some(depth) = inner.first().map(|l| indent(l)) else { return Err("a `static for` block needs a body".to_string()) };
            if inner.iter().any(|l| l.trim().starts_with("static ")) { return Err("a `static for` over a variant's values holds no other `static` block".to_string()); }
            let outer_indent = indent(line);
            for k in 0..count {
                let mut pairs = outer.to_vec();
                for (name, list) in names.iter().zip(&over) { pairs.push((name.to_string(), format!("{list}{k}"))); }
                for l in &inner { out.push((l[(depth - outer_indent).min(indent(l))..].to_string(), pairs.clone(), l.as_ptr())); }
            }
            index = end;
            continue;
        }
        out.push((patterns(line, variant, count, &mut bound)?, outer.to_vec(), line.as_ptr()));
        index += 1;
    }
    Ok(())
}

/// A line the recipe writes: its text, the loop words replaced on it, and the body line (an
/// address in the recipe's body) it was written from.
type Unrolled = (String, Vec<(String, String)>, *const u8);

fn unroll(lines: &[&str], shape: &Shape, out: &mut Vec<Unrolled>) -> Result<(), String> {
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];
        let trimmed = line.trim();
        if trimmed.starts_with("static for ") {
            let Some((names, over)) = loop_head(trimmed) else {
                return Err("a recipe repeats as `static for NAME in fields:` (a record) or `static for NAME in variants:` (a sum)".to_string());
            };
            for name in &names { word_ok(name)?; }
            let (block, end) = indented_block(lines, index);
            let Some(inner) = block.first().map(|l| indent(l)) else { return Err("a `static for` block needs a body".to_string()) };
            let outer = indent(line);
            let kept: Vec<&str> = block.iter().map(|l| &l[(inner - outer).min(indent(l))..]).collect();
            match (over.as_slice(), shape) {
                (["fields"], Shape::Fields(fields)) => {
                    if names.is_empty() || names.len()>2 || (names.len()==2 && names[0]==names[1]) {
                        return Err("a field loop names its field and optionally its declared type".into());
                    }
                    let name = &names[0];
                    // One level, so what a recipe writes grows with the number of fields, not a power of it.
                    if block.iter().any(|l| l.trim().starts_with("static for ")) {
                        return Err("a `static for` block holds no other `static for`".to_string());
                    }
                    for &(field, ty) in *fields {
                        let mut bindings = vec![(name.to_string(), field.to_owned())];
                        if let Some(t) = names.get(1) { bindings.push((t.to_string(), ty.render())); }
                        for l in select(&kept, name, ty)? { out.push((l.to_string(), bindings.clone(), l.as_ptr())); }
                    }
                }
                (["variants"], Shape::Variants(variants)) => {
                    // `static for v in variants:` or `static for v, tag in variants:`: the variant's
                    // name, and its tag, the variant's place in the declaration (explicit, so a hash
                    // tells variants apart by what the recipe feeds).
                    if names.is_empty() || names.len() > 2 { return Err("`static for v in variants:` or `static for v, tag in variants:`".to_string()); }
                    if names.len() == 2 && names[0] == names[1] { return Err(format!("`{}` is named twice in this recipe's `static for` loops: give each its own name", names[0])); }
                    for (tag, (variant, count)) in variants.iter().enumerate() {
                        let mut pairs = vec![(names[0].to_string(), variant.clone())];
                        if let Some(word) = names.get(1) { pairs.push((word.to_string(), tag.to_string())); }
                        variant_lines(&kept, names[0], *count, out, &pairs)?;
                    }
                }
                (["fields"], Shape::Variants(_)) => return Err("this recipe repeats over fields, and a sum type has variants: `static for v in variants:`".to_string()),
                (["variants"], Shape::Fields(..)) => return Err("this recipe repeats over variants, and a record has fields: `static for f in fields:`".to_string()),
                _ => return Err("a recipe repeats as `static for NAME in fields:` (a record) or `static for NAME in variants:` (a sum)".to_string()),
            }
            index = end;
        } else if trimmed.starts_with("static if ") || trimmed == "static else:" {
            return Err("a type test, `static if NAME is TYPE:`, is written inside a `static for` block".to_string());
        } else {
            out.push((line.to_string(), Vec::new(), line.as_ptr()));
            index += 1;
        }
    }
    Ok(())
}

/// What a recipe writes for one type: the text, where the derive's words landed in it, and for
/// each line written, where it starts in the text and the span of the body line it came from.
pub(super) struct Expanded {
    pub text: String,
    pub replaced: Vec<std::ops::Range<usize>>,
    pub lines: Vec<(usize, std::ops::Range<usize>)>,
}

/// The recipe's declarations for the type `name<generics>` with the selected `fields`: `impl
/// Trait for Self:` binds each type parameter by `Trait` (as the built-in derives do, so
/// `Pair<T>` has the impl when `T` has the trait), and every other `Self` is the type.
pub(super) fn expand(recipe: &Recipe, name: &str, generics: &[String], for_type: Option<&str>, shape: Shape) -> Result<Expanded, String> {
    let lines: Vec<&str> = recipe.body.lines().collect();
    let base = lines.iter().filter(|l| !l.trim().is_empty()).map(|l| indent(l)).min().unwrap_or(0);
    let dedented: Vec<&str> = lines.iter().map(|l| if l.trim().is_empty() { "" } else { &l[base..] }).collect();
    // Each top-level line opens an impl for the type itself (review: `impl Hash for Mine` in a
    // recipe implemented a trait for another type, a private one included).
    for line in dedented.iter().filter(|l| !l.trim().is_empty() && indent(l) == 0) {
        // Read as the lexer reads it, without its comment (fourth review: `impl Name for Mine: //
        // for Self:` passed a check of the text and implemented the trait for `Mine`).
        let line = &code_only(line);
        if !(line.starts_with("impl ") && line.trim_end().ends_with(" for Self:")) {
            return Err(format!("a derive recipe writes impls for the type it is applied to (`impl Trait for Self:`), not `{}`", line.trim_end()));
        }
    }
    let mut unrolled = Vec::new();
    unroll(&dedented, &shape, &mut unrolled)?;
    let head = if generics.is_empty() { name.to_string() } else { format!("{name}<{}>", generics.join(", ")) };
    let mut out = String::new();
    // Where the derive's words (the type, `For`, the fields) were written into the recipe's text.
    let mut replaced: Vec<std::ops::Range<usize>> = Vec::new();
    // The body line a written line came from: its text in the bundled source, without its
    // indentation (every written line is a slice of the body; the header's span otherwise).
    let body = recipe.body.as_bytes().as_ptr_range();
    let from = |at: *const u8| -> std::ops::Range<usize> {
        if !body.contains(&at) { return recipe.at..recipe.at + 1; }
        let offset = at as usize - body.start as usize;
        let line = recipe.body[offset..].lines().next().unwrap_or("");
        let start = recipe.body_at + offset + (line.len() - line.trim_start().len());
        start..recipe.body_at + offset + line.trim_end().len()
    };
    let mut lines = Vec::new();
    for (line, field, at) in unrolled {
        // `For` written by the recipe itself, before anything is replaced (a field named `For` is
        // not the recipe writing it).
        if for_type.is_none() && replace_word(&line, "For", "") != line {
            return Err("this recipe writes `For`: name the type it is for at the derive, `(for: Type)`".to_string());
        }
        // The type's parameters in the header are the derive's words, read in the type's file
        // (review of definition-site resolution: a parameter named like a private type of the
        // recipe's file was read as that type).
        let mut header_words: Vec<std::ops::Range<usize>> = Vec::new();
        let line = match line.strip_prefix("impl ").and_then(|rest| rest.strip_suffix(" for Self:")) {
            Some(trait_name) if !generics.is_empty() => {
                let mut written = String::from("impl<");
                for (i, g) in generics.iter().enumerate() {
                    if i > 0 { written.push_str(", "); }
                    header_words.push(written.len()..written.len() + g.len());
                    written.push_str(&format!("{g}: Any + {trait_name}"));
                }
                written.push_str(&format!("> {trait_name} for Self:"));
                written
            }
            _ => line,
        };
        // `Self`, `For` (the type named by `for:` at the derive, `Query(for: NameKey)`) and the
        // loop's name, in one pass.
        let mut pairs: Vec<(&str, &str)> = vec![("Self", head.as_str())];
        if let Some(given) = for_type { pairs.push(("For", given)); }
        for (word, with) in &field { pairs.push((word.as_str(), with.as_str())); }
        let (line, marks) = replace_words_marked(&line, &pairs);
        let base = out.len();
        // The header's parameters lie before any replacement on the line, so their offsets hold.
        replaced.extend(header_words.into_iter().map(|m| m.start + base..m.end + base));
        replaced.extend(marks.into_iter().map(|m| m.start + base..m.end + base));
        replaced.sort_by_key(|r| r.start);
        lines.push((out.len(), from(at)));
        out.push_str(&line);
        out.push('\n');
    }
    Ok(Expanded { text: format!("{}\n", out.trim_end()), replaced, lines })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_a_recipe_writes_carries_the_span_of_the_body_line_it_came_from() {
        let source = "trait Total:\n    fn total(self: &Self) -> U64\n\nderive Sum:\n    impl Total for Self:\n        fn total(self: &Self) -> U64:\n            return 0\n\ntype Two derive(Sum):\n    a: U64\n";
        let program = crate::parser::parse_modules(source, &[0]).unwrap();
        let method = program.declarations.iter().find_map(|d| match d { crate::ast::Declaration::Impl { methods, .. } => methods.iter().map(|(f, _)| f).find(|f| f.name.ends_with("total")), _ => None }).expect("the derived method");
        let line = source.find("        fn total").unwrap() + 8;
        assert_eq!(method.span.start, line, "the method starts at its body line, not the recipe's header");
        // And the derive still knows the method as its own.
        assert!(program.derived.iter().any(|d| d.recipe.contains(&method.span.start) && d.target == "Two"));
    }

    #[test]
    fn a_recipe_is_taken_out_and_its_field_loop_written_per_field() {
        let source = "pub derive Key:\n    impl Eq for Self:\n        fn eq(self: &Self, other: &Self) -> Bool:\n            static for f in fields:\n                if (*self).f != (*other).f:\n                    return false\n            return true\n\nfn main():\n    return\n";
        let (rest, recipes) = lift(source, &[0]).unwrap();
        assert_eq!(rest.len(), source.len());
        assert!(rest.trim_start().starts_with("fn main():"));
        let recipe = &recipes[&(0, "Key".to_string())];
        assert!(recipe.public);
        let ty = Type::Named("T".into(), vec![]);
        let Expanded { text: written, replaced, .. } = expand(recipe, "Pair", &["T".to_string()], None, Shape::Fields(&[("left", &ty), ("right", &ty)])).unwrap();
        // Each replacement is recorded where it landed: the type with its parameter, and the fields.
        let words: Vec<&str> = replaced.iter().map(|r| &written[r.clone()]).collect();
        assert_eq!(words, ["T", "Pair<T>", "Pair<T>", "Pair<T>", "left", "left", "right", "right"]);
        assert_eq!(written, "impl<T: Any + Eq> Eq for Pair<T>:\n    fn eq(self: &Pair<T>, other: &Pair<T>) -> Bool:\n        if (*self).left != (*other).left:\n            return false\n        if (*self).right != (*other).right:\n            return false\n        return true\n");
    }

    #[test]
    fn only_whole_words_are_replaced() {
        assert_eq!(replace_word("f + fx + (*self).f + f_2", "f", "id"), "id + fx + (*self).id + f_2");
    }

    #[test]
    fn scalars_and_literals_inside_interpolations_keep_the_scanner_in_step() {
        assert_eq!(replace_word(r#"'"' + f"#, "f", "id"), r#"'"' + id"#);
        assert_eq!(replace_word(r#"'\'' + f + '\\' + f"#, "f", "id"), r#"'\'' + id + '\\' + id"#);
        assert_eq!(replace_word(r#"g("${h("}")}", f)"#, "f", "id"), r#"g("${h("}")}", id)"#);
        assert_eq!(replace_word(r#""${h("{")} f=${(*s).f} f""#, "f", "id"), r#""${h("{")} f=${(*s).id} f""#);
        assert_eq!(replace_word(r#""${g("a ${f} f")} f" + f"#, "f", "id"), r#""${g("a ${id} f")} f" + id"#);
        assert_eq!(replace_word(r#""${ {f} }" f"#, "f", "id"), r#""${ {id} }" id"#);
    }

    #[test]
    fn an_interpolation_ends_where_the_lexer_ends_it_and_words_are_replaced_once() {
        // The first `}` outside a quote closes the value, whatever `'`, `{` or `//` came before.
        assert_eq!(replace_word(r#"g(&"${0 // '}", f)"#, "f", "id"), r#"g(&"${0 // '}", id)"#);
        assert_eq!(replace_word(r#"g(&"${0 // {}", f)"#, "f", "id"), r#"g(&"${0 // {}", id)"#);
        // What one replacement writes is not replaced again.
        assert_eq!(replace_words("(*self).f + Self.x + For", &[("Self", "For"), ("For", "Other"), ("f", "For")]), "(*self).For + For.x + Other");
    }

    #[test]
    fn literals_are_kept_but_interpolations_are_code() {
        assert_eq!(replace_word(r#"f + "f is \"f\" ${(*self).f} {f}" + f // f"#, "f", "id"),
                   r#"id + "f is \"f\" ${(*self).id} {f}" + id // f"#);
    }
}
