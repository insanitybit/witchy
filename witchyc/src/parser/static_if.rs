use crate::diagnostic::Diagnostic;
use crate::token::{Kind, Token};

thread_local! {
    static DEBUG_BUILD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

pub(crate) fn debug_build() -> bool {
    DEBUG_BUILD.get()
}

pub(crate) fn set_debug_build(enabled: bool) {
    DEBUG_BUILD.set(enabled);
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Target {
    Wasm,
    MacosArm64,
    LinuxArm64,
}

pub(super) fn select(tokens: Vec<Token>, target: Target) -> Result<Vec<Token>, Diagnostic> {
    if !tokens.windows(2).any(|pair| name(pair.first(), "static") && name(pair.get(1), "if")) {
        return Ok(tokens);
    }
    let mut output = Vec::with_capacity(tokens.len());
    expand(&tokens, target, 0, &mut output)?;
    Ok(output)
}

fn name(token: Option<&Token>, word: &str) -> bool {
    matches!(token.map(|t| &t.kind), Some(Kind::Name(n)) if n == word)
}

fn symbol(token: Option<&Token>, word: &str) -> bool {
    matches!(token.map(|t| &t.kind), Some(Kind::Symbol(s)) if *s == word)
}

fn error(at: &Token, message: &'static str) -> Diagnostic {
    Diagnostic::new("W0100", at.span.clone(), message)
}

fn condition(tokens: &[Token], target: Target, at: &Token) -> Result<bool, Diagnostic> {
    let malformed = || error(at, "expected `static if target.backend == .Wasm:` or `static if target.os == .MacOS:`");
    if tokens.len() != 6 || !name(tokens.first(), "target") || !symbol(tokens.get(1), ".")
        || !symbol(tokens.get(3), "==") || !symbol(tokens.get(4), ".") {
        return Err(malformed());
    }
    if name(tokens.get(2), "profile") {
        if name(tokens.get(5), "Debug") {
            return Ok(debug_build());
        }
        if name(tokens.get(5), "Release") {
            return Ok(!debug_build());
        }
    } else if name(tokens.get(2), "backend") {
        if name(tokens.get(5), "Wasm") {
            return Ok(target == Target::Wasm);
        }
        if name(tokens.get(5), "Native") {
            return Ok(target != Target::Wasm);
        }
    } else if name(tokens.get(2), "os") {
        if name(tokens.get(5), "MacOS") {
            return Ok(target == Target::MacosArm64);
        }
        if name(tokens.get(5), "Linux") {
            return Ok(target == Target::LinuxArm64);
        }
    }
    Err(error(at, "unknown static target fact or value"))
}

fn block(tokens: &[Token], start: usize, at: &Token) -> Result<usize, Diagnostic> {
    if !matches!(tokens.get(start).map(|t| &t.kind), Some(Kind::Indent)) {
        return Err(error(at, "expected an indented static branch"));
    }
    let mut depth = 1;
    for (i, token) in tokens.iter().enumerate().skip(start + 1) {
        match token.kind {
            Kind::Indent => depth += 1,
            Kind::Dedent => {
                depth -= 1;
                if depth == 0 {
                    return Ok(i);
                }
            }
            _ => {}
        }
    }
    Err(error(at, "unterminated static branch"))
}

fn expand(tokens: &[Token], target: Target, depth: usize, output: &mut Vec<Token>) -> Result<(), Diagnostic> {
    let mut i = 0;
    while i < tokens.len() {
        let at = &tokens[i];
        let line_start = i == 0 || matches!(tokens[i - 1].kind, Kind::Newline | Kind::Indent | Kind::Dedent);
        if !line_start || !name(Some(at), "static") || !name(tokens.get(i + 1), "if") {
            output.push(at.clone());
            i += 1;
            continue;
        }
        if depth == 60 {
            return Err(error(at, "static branch nesting exceeds 60 levels"));
        }
        let header = i + 8;
        if !symbol(tokens.get(header), ":") || !matches!(tokens.get(header + 1).map(|t| &t.kind), Some(Kind::Newline)) {
            return Err(error(at, "expected a static target comparison followed by `:` and a block"));
        }
        let take_first = condition(&tokens[i + 2..header], target, at)?;
        let start = header + 2;
        let end = block(tokens, start, at)?;
        i = end + 1;
        if take_first {
            expand(&tokens[start + 1..end], target, depth + 1, output)?;
        }
        let otherwise = if name(tokens.get(i), "static") && name(tokens.get(i + 1), "else") {
            Some(i + 2)
        } else if name(tokens.get(i), "else") {
            Some(i + 1)
        } else {
            None
        };
        if let Some(colon) = otherwise {
            if !symbol(tokens.get(colon), ":") || !matches!(tokens.get(colon + 1).map(|t| &t.kind), Some(Kind::Newline)) {
                return Err(error(&tokens[i], "expected `else:` followed by an indented static branch"));
            }
            let start = colon + 2;
            let end = block(tokens, start, &tokens[i])?;
            if !take_first {
                expand(&tokens[start + 1..end], target, depth + 1, output)?;
            }
            i = end + 1;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_selection_keeps_spans_and_nested_indentation() {
        let source = "fn main():\n    static if target.backend == .Wasm:\n        wasm()\n    else:\n        static if target.os == .MacOS:\n            mac()\n        else:\n            linux()\n";
        for (target, call) in [
            (Target::Wasm, "wasm"),
            (Target::MacosArm64, "mac"),
            (Target::LinuxArm64, "linux"),
        ] {
            let selected = select(crate::lexer::lex(source).unwrap(), target).unwrap();
            let selected_call = selected.iter().find(|t| name(Some(t), call)).unwrap();
            assert_eq!(&source[selected_call.span.clone()], call);
            let mut kinds = selected.into_iter().map(|token| token.kind);
            let expected = crate::lexer::lex(&format!("fn main():\n    {call}()\n")).unwrap();
            assert!(kinds.by_ref().eq(expected.into_iter().map(|token| token.kind)));
        }
    }
}
