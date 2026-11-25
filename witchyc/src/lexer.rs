use crate::diagnostic::Diagnostic;
use crate::token::{Kind, Token};

pub fn lex(source: &str) -> Result<Vec<Token>, Diagnostic> {
    if source.len() > 1_048_576 {
        return Err(Diagnostic::new(
            "W0007",
            0..source.len(),
            "source exceeds bootstrap byte limit",
        ));
    }
    // The byte budget bounds token storage and owned text as well: scanning
    // tokens consume source bytes, and indentation/newline tokens grow only
    // with the number of source lines. Token positions and spans use usize.
    // About one token per five bytes of source: reserved once rather than grown and copied.
    let mut tokens = Vec::with_capacity(source.len() / 5);
    let mut indents = vec![0];
    let mut offset = 0;
    // Brackets open at the end of a line: the line goes on (`[` and a value on each line).
    let mut open = 0usize;
    for raw in source.split_inclusive('\n') {
        let line = raw.trim_end_matches('\n').trim_end_matches('\r');
        let spaces = line.bytes().take_while(|b| *b == b' ').count();
        let body = &line[spaces..];
        if body.starts_with('\t') {
            return Err(Diagnostic::new(
                "W0002",
                offset + spaces..offset + spaces + 1,
                "tabs are not permitted",
            ));
        }
        if !body.is_empty() && !body.starts_with("//") {
            if open == 0 { indentation(&mut tokens, &mut indents, spaces, offset)?; }
            let first = tokens.len();
            scan_line(line, offset, spaces, &mut tokens)?;
            for token in &tokens[first..] {
                match token.kind {
                    Kind::Symbol("(" | "[" | "{") => open += 1,
                    Kind::Symbol(")" | "]" | "}") => open = open.saturating_sub(1),
                    _ => {}
                }
            }
            if open == 0 {
                tokens.push(Token {
                    kind: Kind::Newline,
                    span: offset + line.len()..offset + raw.len(),
                });
            }
        }
        offset += raw.len();
    }
    while indents.len() > 1 {
        indents.pop();
        tokens.push(Token {
            kind: Kind::Dedent,
            span: offset..offset,
        });
    }
    tokens.push(Token {
        kind: Kind::End,
        span: offset..offset,
    });
    Ok(tokens)
}

fn indentation(
    tokens: &mut Vec<Token>,
    stack: &mut Vec<usize>,
    spaces: usize,
    start: usize,
) -> Result<(), Diagnostic> {
    let current = *stack.last().expect("indent stack always has a root");
    if spaces % 4 != 0 || spaces > current + 4 {
        // What was found and what fits: the most common refusal of a mutated program.
        let message = if spaces % 4 != 0 {
            format!("indentation must advance by four spaces: this line has {spaces}, which is not a multiple of four; the line it is under has {current}")
        } else {
            format!("indentation must advance by four spaces: this line has {spaces}, more than one level past the {current} of the line it is under; write {}, or end the line above with `:` if it opens a block", current + 4)
        };
        return Err(Diagnostic::new("W0001", start..start + spaces, message));
    }
    if spaces > current {
        stack.push(spaces);
        tokens.push(Token {
            kind: Kind::Indent,
            span: start..start + spaces,
        });
    } else {
        while spaces < *stack.last().expect("root is retained") {
            stack.pop();
            tokens.push(Token {
                kind: Kind::Dedent,
                span: start..start + spaces,
            });
        }
    }
    Ok(())
}

fn scan_line(
    line: &str,
    base: usize,
    mut i: usize,
    tokens: &mut Vec<Token>,
) -> Result<(), Diagnostic> {
    let bytes = line.as_bytes();
    while i < bytes.len() {
        let start = i;
        let kind = match bytes[i] {
            b' ' => {
                i += 1;
                continue;
            }
            b'\t' => {
                return Err(Diagnostic::new(
                    "W0002",
                    base + i..base + i + 1,
                    "tabs are not permitted",
                ));
            }
            b'/' if line[i..].starts_with("//") => break,
            b'a'..=b'z' | b'A'..=b'Z' | b'_' => {
                i += 1;
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
                match &line[start..i] {
                    "brand" => Kind::Brand,
                    "with" => Kind::Reserved("with"),
                    "async" => Kind::Reserved("async"),
                    "await" => Kind::Reserved("await"),
                    name => Kind::Name(name.to_owned()),
                }
            }
            b'0'..=b'9' => {
                // Decimal, or hexadecimal after `0x`. `_` may stand between digits: 1_000_000, 0xFFFF_FFFF.
                let hex = bytes[i] == b'0' && bytes.get(i + 1) == Some(&b'x');
                if hex { i += 2; }
                let digits = i;
                while i < bytes.len() && (bytes[i] == b'_' || if hex { bytes[i].is_ascii_hexdigit() } else { bytes[i].is_ascii_digit() }) {
                    i += 1;
                }
                let written = &line[digits..i];
                // `250ms`, `30s`, `5m`, `2h`, `1d`, `1w`, `1m30s`: a duration, as U64 milliseconds
                // (what the clock's methods take and give).
                if !hex && !written.is_empty() && i < bytes.len() && bytes[i].is_ascii_alphabetic() {
                    let mut total: u64 = 0;
                    let mut at = digits;
                    // Units from largest to smallest, each once (`1m30s`, not `30s1m`).
                    let mut last_factor = u64::MAX;
                    loop {
                        let number_end = { let mut j = at; while j < bytes.len() && (bytes[j].is_ascii_digit() || bytes[j] == b'_') { j += 1; } j };
                        let unit_end = { let mut j = number_end; while j < bytes.len() && bytes[j].is_ascii_alphabetic() { j += 1; } j };
                        let digits_text: String = line[at..number_end].chars().filter(|c| *c != '_').collect();
                        let factor: u64 = match &line[number_end..unit_end] {
                            "ms" => 1, "s" => 1000, "m" => 60_000, "h" | "hr" => 3_600_000, "d" => 86_400_000, "w" => 604_800_000,
                            "us" | "ns" => return Err(Diagnostic::new("W0003", base + start..base + unit_end, "a duration is whole milliseconds: `us` and `ns` are finer than it holds")),
                            // `1e300`: an exponent on a number with no point.
                            "e" | "E" if bytes.get(unit_end).is_some_and(|b| b.is_ascii_digit() || *b == b'+' || *b == b'-') => {
                                let mantissa = &line[at..number_end];
                                return Err(Diagnostic::new("W0003", base + start..base + unit_end, format!("an F64 is written with a point before its exponent: `{mantissa}.0e...`")));
                            }
                            _ => return Err(Diagnostic::new("W0003", base + start..base + unit_end, "a number is digits, or `0x` and hexadecimal digits, with `_` only between digits; a duration ends in `ms`, `s`, `m`, `h`, `d` or `w`")),
                        };
                        if factor >= last_factor || line[at..number_end].ends_with('_') || line[at..number_end].starts_with('_') {
                            return Err(Diagnostic::new("W0003", base + start..base + unit_end, "a duration is numbers each with a unit, largest unit first and each once: `1h30m`"));
                        }
                        last_factor = factor;
                        let part = digits_text.parse::<u64>().ok().and_then(|n| n.checked_mul(factor)).and_then(|n| total.checked_add(n))
                            .ok_or_else(|| Diagnostic::new("W0003", base + start..base + unit_end, "a duration beyond U64 milliseconds"))?;
                        total = part;
                        at = unit_end;
                        // `1m30s`: another number and unit straight after.
                        if at < bytes.len() && bytes[at].is_ascii_digit() { continue; }
                        break;
                    }
                    // `250ms.ms()` calls a method; `30s.5` is no number.
                    if at < bytes.len() && (bytes[at].is_ascii_alphanumeric() || bytes[at] == b'_' || (bytes[at] == b'.' && bytes.get(at + 1).is_some_and(u8::is_ascii_digit))) {
                        return Err(Diagnostic::new("W0003", base + start..base + at + 1, "a duration is numbers each with a unit: `1m30s`"));
                    }
                    tokens.push(Token { kind: Kind::Duration(total), span: base + start..base + at });
                    i = at;
                    continue;
                }
                // A letter straight after would be read as a name: `12abc` and `0xZ` are mistakes.
                let runs_on = i < bytes.len() && bytes[i].is_ascii_alphanumeric();
                if written.is_empty() || written.starts_with('_') || written.ends_with('_') || written.contains("__") || runs_on {
                    return Err(Diagnostic::new("W0003", base + start..base + i + usize::from(runs_on), "a number is digits, or `0x` and hexadecimal digits, with `_` only between digits"));
                }
                let plain: String = written.chars().filter(|c| *c != '_').collect();
                // `1.5`, `2.0e-3`: digits, a point and digits (so `0..n` stays a range), then an
                // exponent if any. The nearest F64, as Rust parses it.
                if !hex && bytes.get(i) == Some(&b'.') && bytes.get(i + 1).is_some_and(u8::is_ascii_digit) {
                    i += 1;
                    while i < bytes.len() && bytes[i].is_ascii_digit() { i += 1; }
                    if i < bytes.len() && (bytes[i] == b'e' || bytes[i] == b'E') {
                        let mark = i;
                        i += 1;
                        if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') { i += 1; }
                        if !(i < bytes.len() && bytes[i].is_ascii_digit()) { i = mark; } else { while i < bytes.len() && bytes[i].is_ascii_digit() { i += 1; } }
                    }
                    let text: String = line[digits..i].chars().filter(|c| *c != '_').collect();
                    // `2.5.min(x)`: a method on a decimal literal.
                    if bytes.get(i) == Some(&b'.') && bytes.get(i + 1).is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_') {
                        let number = &line[start..i];
                        return Err(Diagnostic::new("W0003", base + start..base + i + 1, format!("a method on a decimal number needs it in parentheses or named: `({number}).method(...)`")));
                    }
                    if i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'.') {
                        return Err(Diagnostic::new("W0003", base + start..base + i + 1, "a decimal number is digits, a point and digits, and an exponent such as `e-3`"));
                    }
                    let value: f64 = text.parse().map_err(|_| Diagnostic::new("W0003", base + start..base + i, "not a decimal number"))?;
                    if !value.is_finite() { return Err(Diagnostic::new("W0003", base + start..base + i, "a decimal number beyond the range of F64")); }
                    tokens.push(Token { kind: Kind::Float(value.to_bits()), span: base + start..base + i });
                    continue;
                }
                let n = u64::from_str_radix(&plain, if hex { 16 } else { 10 }).map_err(|_| {
                    Diagnostic::new("W0003", base + start..base + i, "integer exceeds U64")
                })?;
                Kind::Integer(n)
            }
            // A scalar in single quotes is its number: 'a' is 97.
            b'\'' => {
                // `<'a>`, `&'a T`: a lifetime, as Rust writes one; Witchy names none.
                let after_open = matches!(tokens.last().map(|t: &Token| &t.kind), Some(Kind::Symbol("<" | "&" | ",")));
                let named = line[i + 1..].chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').count();
                if after_open && named > 0 && line[i + 1 + named..].chars().next() != Some('\'') {
                    return Err(Diagnostic::new("W0008", base + start..base + i + 1 + named, "Witchy has no lifetime parameters (`'a`): a reference's lifetime is checked where it is used, without a name; leave it out"));
                }
                let bad = |end: usize| Diagnostic::new("W0008", base + start..base + end, "a quoted scalar is one character or one of \\n \\r \\t \\0 \\\\ \\' between single quotes");
                let mut rest = line[i + 1..].chars();
                let first = rest.next().ok_or_else(|| bad(i + 1))?;
                let (scalar, width) = match first {
                    '\'' => return Err(bad(i + 2)),
                    '\\' => {
                        let escape = rest.next().ok_or_else(|| bad(i + 2))?;
                        (match escape { 'n' => '\n', 'r' => '\r', 't' => '\t', '0' => '\0', '\\' => '\\', '\'' => '\'', _ => return Err(bad(i + 2 + escape.len_utf8())) }, 2)
                    }
                    other => (other, other.len_utf8()),
                };
                if rest.next() != Some('\'') { return Err(bad(i + 1 + width)); }
                i += width + 2;
                Kind::Integer(scalar as u64)
            }
            b'"' => {
                let (text, end) = string(line, i, base)?;
                i = end;
                Kind::Text(text)
            }
            _ => {
                // Two characters before one (`->` before `-`), by byte rather than by comparing
                // each symbol in turn: a symbol is most tokens, and the scan was a fifth of lexing.
                let two: Option<&'static str> = match (bytes[i], bytes.get(i + 1)) {
                    (b'-', Some(b'>')) => Some("->"), (b'=', Some(b'=')) => Some("=="), (b'!', Some(b'=')) => Some("!="),
                    (b'<', Some(b'=')) => Some("<="), (b'>', Some(b'=')) => Some(">="), (b'&', Some(b'&')) => Some("&&"),
                    (b'|', Some(b'|')) => Some("||"), (b'+', Some(b'=')) => Some("+="), (b'-', Some(b'=')) => Some("-="),
                    (b'*', Some(b'=')) => Some("*="), (b'/', Some(b'=')) => Some("/="), (b'%', Some(b'=')) => Some("%="),
                    (b'.', Some(b'.')) => Some(".."),
                    _ => None,
                };
                let one: Option<&'static str> = match bytes[i] {
                    b':' => Some(":"), b',' => Some(","), b'.' => Some("."), b';' => Some(";"), b'(' => Some("("), b')' => Some(")"),
                    b'[' => Some("["), b']' => Some("]"), b'{' => Some("{"), b'}' => Some("}"), b'<' => Some("<"), b'>' => Some(">"),
                    b'=' => Some("="), b'+' => Some("+"), b'-' => Some("-"), b'*' => Some("*"), b'/' => Some("/"), b'%' => Some("%"),
                    b'&' => Some("&"), b'!' => Some("!"), b'?' => Some("?"), b'|' => Some("|"), b'^' => Some("^"), b'~' => Some("~"),
                    _ => None,
                };
                let Some(symbol) = two.or(one) else {
                    let width = line[i..].chars().next().expect("not at end").len_utf8();
                    return Err(Diagnostic::new(
                        "W0004",
                        base + i..base + i + width,
                        "unexpected character",
                    ));
                };
                i += symbol.len();
                Kind::Symbol(symbol)
            }
        };
        tokens.push(Token {
            kind,
            span: base + start..base + i,
        });
    }
    Ok(())
}

/// `\$` in a text literal: a `$` that does not start a `${...}`. Kept as this noncharacter
/// while the literal still has a `${` to read (parser/interpolation.rs puts the `$` back);
/// written in a literal itself, it is refused.
pub const ESCAPED_DOLLAR: char = '\u{FDD0}';

fn string(line: &str, start: usize, base: usize) -> Result<(String, usize), Diagnostic> {
    // The mark stays only where it stops a `${`: everything else reads a plain `$`.
    let (out, end) = string_escaped(line, start, base)?;
    let mut kept = String::with_capacity(out.len());
    let mut chars = out.chars().peekable();
    while let Some(c) = chars.next() {
        if c == ESCAPED_DOLLAR && chars.peek() != Some(&'{') { kept.push('$'); } else { kept.push(c); }
    }
    Ok((kept, end))
}

/// A literal starting just before `rest` ends on this line: an unescaped `"` follows.
fn inner_closes(rest: &str) -> bool {
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match c { '\\' => { chars.next(); } '"' => return true, _ => {} }
    }
    false
}

fn string_escaped(line: &str, start: usize, base: usize) -> Result<(String, usize), Diagnostic> {
    let mut out = String::new();
    let mut i = start + 1;
    // Inside an open `${`: a `"` there starts a literal of the value's source, copied as written
    // (its escapes too) for parser/interpolation.rs to lex again, as `${has(&nodes, "tea")}`.
    let mut open = false;
    while i < line.len() {
        let ch = line[i..].chars().next().expect("not at end");
        i += ch.len_utf8();
        match ch {
            '$' if !open && line[i..].starts_with('{') => { open = true; out.push('$'); }
            '}' if open => { open = false; out.push('}'); }
            // Only when the inner literal closes on this line: otherwise this quote ends the
            // outer one, and its unclosed `${` is reported as that (`console/print-unclosed`).
            '"' if open && inner_closes(&line[i..]) => {
                out.push('"');
                loop {
                    let Some(inner) = line[i..].chars().next() else { break };
                    i += inner.len_utf8();
                    out.push(inner);
                    if inner == '\\' {
                        if let Some(escaped) = line[i..].chars().next() { i += escaped.len_utf8(); out.push(escaped); }
                    } else if inner == '"' { break; }
                }
            }
            '"' => return Ok((out, i)),
            '\\' => {
                let Some(escape) = line[i..].chars().next() else {
                    break;
                };
                let decoded = match escape {
                    'n' => '\n',
                    'r' => '\r',
                    't' => '\t',
                    '0' => '\0',
                    '$' => ESCAPED_DOLLAR,
                    '\\' => '\\',
                    '"' => '"',
                    _ => {
                        return Err(Diagnostic::new(
                            "W0006",
                            base + i - 1..base + i + escape.len_utf8(),
                            "unknown string escape",
                        ));
                    }
                };
                i += escape.len_utf8();
                out.push(decoded);
            }
            ESCAPED_DOLLAR => return Err(Diagnostic::new("W0006", base + i - ch.len_utf8()..base + i, "U+FDD0 is not written in a text literal")),
            _ => out.push(ch),
        }
    }
    Err(Diagnostic::new(
        "W0005",
        base + start..base + line.len(),
        "unterminated string",
    ))
}
