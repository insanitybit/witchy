//! `"total ${t}, ready ${r}"` (book: tour-values.md) is a value of its own: a format
//! (docs/design-format.md). It builds no text. The literal becomes a sealed record
//! `Format#<at><P0, P1, ..>` holding the values in order, with
//! `impl<P0: Show, ..> Format for Format#<at><P0, ..>`, whose `write_to` and `push_onto` write
//! the text pieces and each value's `Show` (library/wasm-console.wy). Library functions
//! consume it: `console.print(message)` (no allocator) and
//! `text.push_format(&allocator, message)`. Nothing in the compiler knows their names.
//!
//! A record's type arguments are copied values, so a piece is a number, a Bool, a text
//! literal or a program's copied type implementing `Show`; an owned `String` is refused.
use super::{lex, Kind, Parser};
use crate::{ast::{Declaration, Expr, Expression}, diagnostic::Diagnostic};

/// Where a `${` value's closing `}` is: the first one outside a text literal of the value's own
/// source (`${label("}")}`; the lexer keeps those literals as written, escapes included).
pub(super) fn closing(after: &str) -> Option<usize> {
    let mut quoted = false;
    let mut chars = after.char_indices();
    while let Some((at, c)) = chars.next() {
        match c {
            '\\' if quoted => { chars.next(); }
            '"' => quoted = !quoted,
            '}' if !quoted => return Some(at),
            _ => {}
        }
    }
    None
}

/// A literal's text runs and the source of each `${...}` value, as `format_value` reads them.
/// None: a `${` is not closed.
pub(super) fn split(text: &str) -> Option<(Vec<String>, Vec<String>)> {
    let (mut runs, mut sources) = (Vec::new(), Vec::new());
    let mut rest = text;
    while let Some(at) = rest.find("${") {
        runs.push(rest[..at].to_string());
        let after = &rest[at + 2..];
        let end = closing(after)?;
        sources.push(after[..end].to_string());
        rest = &after[end + 1..];
    }
    runs.push(rest.to_string());
    Some((runs, sources))
}

/// A `${...}` value is source, read by `format_value` as the compiler's own text may be (a
/// derive's `Self` is `Name@file`), where `name@file`, `#name` and the `__witchy_` escape spell
/// names no source may. So a literal of a program's own text (a file, a recipe's body) holds
/// none of them in a value, at any depth: checked on the very pieces `format_value` will read
/// (`split`), so no reading of quotes or escapes can differ (review of the reach rule,
/// 2026-11-22: `${alpha_internal@8(1)}` called another package's inner function).
pub(super) fn written_names(tokens: &[super::Token]) -> Result<(), Diagnostic> {
    for token in tokens {
        let Kind::Text(text) = &token.kind else { continue };
        if !text.contains("${") { continue; }
        // Unclosed: refused where it is read, as before; its values hold nothing read yet.
        let Some((_, sources)) = split(text) else { continue };
        if sources.iter().any(|source| source.contains(['@', '#']) || source.contains("__witchy_")) {
            return Err(Diagnostic::new("W0100", token.span.clone(), "`@`, `#` and `__witchy_` are not written inside a `${...}` value: they spell the compiler's own names; bind a text that holds one to a name first"));
        }
    }
    Ok(())
}

impl Parser {
    pub(super) fn format_value(&mut self, text: String, span: std::ops::Range<usize>) -> Result<Expression, Diagnostic> {
        let refuse = |message: &'static str| Diagnostic::new("W0100", span.clone(), message);
        // The pieces: text runs, and the source of each `${...}`.
        let (mut runs, sources) = split(&text).ok_or_else(|| refuse("a `${` in a text literal is closed with `}`"))?;
        for run in &mut runs { *run = run.replace(crate::lexer::ESCAPED_DOLLAR, "$"); }
        if sources.len() > 32 { return Err(refuse("a text literal holds at most 32 `${...}` values")); }
        // Each value, read as source; an error in it is reported at the literal.
        let mut values = Vec::new();
        for source in &sources {
            // An error in it is placed at the literal too: its own span counted from the value's
            // first byte, and was reported in the first file read (the standard library's).
            // Read as the compiler's own text may be: a derive's `Self` is `Name@file` when two
            // files declare the name (review of definition-site resolution: it failed to lex).
            let mut tokens = super::lex_written(source).map_err(|mut error| { error.span = span.clone(); error })?;
            super::respan(&mut tokens, |_| span.clone());
            let mut written = Parser::written(tokens, &self.modules);
            written.constants = self.constants.clone();
            written.fn_locals = self.fn_locals.clone();
            // A literal inside (`${s == "pen"}`) joins the program's literals, as the text pieces
            // do below: in a table of its own it was numbered from 0 and read another literal.
            written.texts = std::mem::take(&mut self.texts);
            // And an interpolation inside (`${f("b ${n}")}`) is counted with the program's and its
            // record kept (review: counted from 0, it took the outer literal's name, and its own
            // record was dropped, so the inner literal printed with the outer one's text).
            written.formats = self.formats;
            let value = written.expression(0);
            self.texts = std::mem::take(&mut written.texts);
            self.formats = written.formats;
            self.extra.append(&mut written.extra);
            let value = value?;
            while matches!(written.token().kind, Kind::Newline) { written.take(); }
            if !matches!(written.token().kind, Kind::End) { return Err(refuse("a `${...}` in a text literal holds one expression")); }
            values.push(value);
        }
        // A number known here (`${7}`, a module constant, or arithmetic of them, `${LIMIT * 2}`;
        // parser/constant.rs folds it) is written into the text around it: it settles no type
        // parameter of the record, and its digits are known. With nothing left to show, the
        // whole is a text literal (a Text is a Format). A value's source is lexed here, so a
        // constant's name is bare: it is this file's constant, `NAME@file` in `constants`.
        let module = self.module();
        fn known(e: &Expression, constants: &std::collections::BTreeMap<String, Expression>, module: usize) -> Expression {
            match e {
                Expression::Name(n) => constants.get(&format!("{n}@{module}")).cloned().unwrap_or_else(|| e.clone()),
                Expression::Unary(op, inner) => Expression::Unary(op, Box::new(Expr { kind: known(&inner.kind, constants, module), span: inner.span.clone() })),
                Expression::Binary(op, a, b) => Expression::Binary(op,
                    Box::new(Expr { kind: known(&a.kind, constants, module), span: a.span.clone() }),
                    Box::new(Expr { kind: known(&b.kind, constants, module), span: b.span.clone() })),
                other => other.clone(),
            }
        }
        let mut index = 0;
        while index < values.len() {
            if let Ok(Expression::Integer(number)) = super::constant::fold(&known(&values[index].kind, &self.constants, module as usize)) {
                let after = runs.remove(index + 1);
                runs[index] = format!("{}{number}{after}", runs[index]);
                values.remove(index);
            } else {
                index += 1;
            }
        }
        if values.is_empty() { return self.text_literal(runs.remove(0), span); }
        // The record and its `Format`. Text pieces are written as literals of the program.
        // The second and later at one span (a recipe's expansion) get a count, so names differ.
        let record = if self.formats == 0 { format!("Format#{}", span.start) } else { format!("Format#{}_{}", span.start, self.formats) };
        self.formats += 1;
        let parameters: Vec<String> = (0..values.len()).map(|i| format!("P{i}")).collect();
        let quoted = |run: &str| format!("\"{}\"", run.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n").replace('\r', "\\r").replace('\t', "\\t"));
        let mut source = format!("sealed type WitchyFormat<{}>:\n", parameters.join(", "));
        for (i, p) in parameters.iter().enumerate() { source.push_str(&format!("    v{i}: {p}\n")); }
        let bounded: Vec<String> = parameters.iter().map(|p| format!("{p}: Show")).collect();
        let this = format!("WitchyFormat<{}>", parameters.join(", "));
        source.push_str(&format!("impl<{}> Format for {this}:\n", bounded.join(", ")));
        source.push_str(&format!("    fn write_to(self: {this}, console: &Console<Write>):\n"));
        for (i, run) in runs.iter().enumerate() {
            if !run.is_empty() { source.push_str(&format!("        console.write({})\n", quoted(run))); }
            if i < values.len() { source.push_str(&format!("        let v{i}: P{i} = self.v{i}\n        v{i}.show_on(console)\n")); }
        }
        source.push_str(&format!("    fn push_onto(self: {this}, text: &mut String, allocator: &Allocator) -> Result<(), U64>:\n"));
        for (i, run) in runs.iter().enumerate() {
            if !run.is_empty() { source.push_str(&format!("        text.push_text(allocator, {})?\n", quoted(run))); }
            if i < values.len() { source.push_str(&format!("        let v{i}: P{i} = self.v{i}\n        v{i}.push_on(text, allocator)?\n")); }
        }
        source.push_str("        return Ok(())\n");
        let module = self.module();
        let mut tokens = lex(&source)?;
        for token in &mut tokens {
            token.span = span.clone();
            if let Kind::Name(name) = &mut token.kind && name == "WitchyFormat" { *name = record.clone(); }
            // The library's traits and types, as the library's own uses name them: when the program
            // declares one of the same name (a `trait Show` of its own), each becomes `Name@file`,
            // and a bare name here would mean neither (Astra audit: interpolation knows library names).
            if let Kind::Name(name) = &mut token.kind && let Some(files) = self.shared_private.get(name.as_str()) {
                let others: Vec<usize> = files.iter().copied().filter(|file| *file != module as usize).collect();
                if let [library] = others.as_slice() { *name = format!("{name}@{library}"); }
            }
        }
        let mut written = Parser::written(tokens, &self.modules);
        // The text pieces join the program's own literals.
        written.texts = std::mem::take(&mut self.texts);
        let parsed = written.declaration().and_then(|record| Ok((record, written.declaration()?)));
        self.texts = std::mem::take(&mut written.texts);
        let (mut declaration, mut implementation) = parsed?;
        if let Declaration::Record { module: m, .. } = &mut declaration { *m = module; }
        if let Declaration::Impl { methods, .. } = &mut implementation { for (method, _) in methods.iter_mut() { method.module = module; } }
        self.extra.push(declaration);
        self.extra.push(implementation);
        Ok(Expression::Call(Box::new(Expr { kind: Expression::Name(record), span: span.clone() }), values))
    }
}

#[cfg(test)]
mod tests {
    // An error in reading a `${...}` value is placed at the literal: it was placed by its offset
    // in the value alone, so a report named the first file read, line 1.
    #[test]
    fn a_lexing_error_inside_a_value_is_placed_at_the_literal() {
        let source = "fn main():\n    let s = \"x ${12abc} y\"\n";
        let error = super::super::parse(source).expect_err("12abc is no number");
        let literal = source.find('"').unwrap();
        assert_eq!(error.code, "W0003");
        assert!(error.span.start >= literal && error.span.end <= source.len(), "{:?}", error.span);
    }
}
