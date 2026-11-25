//! Module constants: a top-level `let NAME = expression` (book: tour-modules.md). Each is
//! evaluated here, in order, and every later use of the name in an expression is the value
//! itself, so a constant is never state and costs nothing at run time.
//!
//! A constant is its file's, as types are (adversarial review round 16: a program's `let K`
//! broke every library naming a parameter `K`). A name means the constant in the file that
//! declares it, and in a file that imports that file when it is `pub let`; anywhere else the
//! name is untouched. A use that means a constant becomes `NAME@file`, which no source spells.
use super::{Parser, Kind, Token};
use crate::{ast::{Expr, Expression}, diagnostic::Diagnostic};
use std::collections::BTreeMap;

/// Take the top-level `let` lines out of `tokens` and evaluate them.
/// Types every program can name without declaring them.
pub(super) const BUILT_IN_TYPES: &[&str] = &["U8", "U16", "U32", "U64", "I64", "Bool", "Console", "Allocator", "Buffer", "PrefixBuffer", "Dir", "File",
    "Clock", "Rand", "Env", "Args", "Net", "Socket", "Listener", "Group", "Task", "Exec", "Run", "Secret", "SecretStore", "Result"];

pub(super) fn lift(tokens: Vec<Token>, modules: &[usize]) -> Result<(Vec<Token>, BTreeMap<String, Expression>), Diagnostic> {
    // Positions of the tokens that declare constants (`pub`, and each `let` line): left out of
    // what is kept, which is then moved, not copied (a copy per token was a profile's hot spot).
    let mut dropped = vec![false; tokens.len()];
    let mut constants: BTreeMap<String, Expression> = BTreeMap::new();
    // Each constant name: the files declaring one, and whether each is `pub let`.
    let mut homes: BTreeMap<String, Vec<(usize, bool)>> = BTreeMap::new();
    // The file a token is in: the last whose first byte is at or before it (files start in order).
    debug_assert!(modules.is_sorted(), "files start in bundle order");
    let file = |start: usize| modules.partition_point(|first| *first <= start).saturating_sub(1);
    // What each file imports (`import NAME FILE`, written by bundling).
    let mut imports: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    // And by the name each file qualifies with: `limits.LIMIT`.
    let mut named: BTreeMap<usize, BTreeMap<String, usize>> = BTreeMap::new();
    for at in 0..tokens.len() {
        if let (Kind::Name(word), Some(Kind::Name(module)), Some(Kind::Integer(target))) = (&tokens[at].kind, tokens.get(at + 1).map(|t| &t.kind), tokens.get(at + 2).map(|t| &t.kind))
            && word == "import" && (at == 0 || matches!(tokens[at - 1].kind, Kind::Newline | Kind::Dedent)) {
            let here = file(tokens[at].span.start);
            imports.entry(here).or_default().push(*target as usize);
            named.entry(here).or_default().insert(module.clone(), *target as usize);
        }
        // `import FILE`: a file that cannot be qualified, still imported (its `pub let` names).
        if let (Kind::Name(word), Some(Kind::Integer(target))) = (&tokens[at].kind, tokens.get(at + 1).map(|t| &t.kind))
            && word == "import" && (at == 0 || matches!(tokens[at - 1].kind, Kind::Newline | Kind::Dedent)) {
            imports.entry(file(tokens[at].span.start)).or_default().push(*target as usize);
        }
    }
    let mut functions = BTreeMap::new();
    let mut public = false;
    let (mut depth, mut at) = (0usize, 0usize);
    while at < tokens.len() {
        let start = at == 0 || matches!(tokens[at - 1].kind, Kind::Newline | Kind::Dedent)
            || (at >= 1 && matches!(&tokens[at - 1].kind, Kind::Name(p) if p == "pub") && (at == 1 || matches!(tokens[at - 2].kind, Kind::Newline | Kind::Dedent)));
        // `pub let`: a constant is a value, visible wherever it is imported; drop the `pub`.
        if depth == 0 && matches!(&tokens[at].kind, Kind::Name(p) if p == "pub") && matches!(tokens.get(at + 1).map(|t| &t.kind), Some(Kind::Name(l)) if l == "let" || (l == "const" && !word(tokens.get(at + 2), "fn")))
            && (at == 0 || matches!(tokens[at - 1].kind, Kind::Newline | Kind::Dedent)) { dropped[at] = true; at += 1; public = true; continue; }
        if depth == 0 && start && word(tokens.get(at), "const") && word(tokens.get(at + 1), "fn") {
            let here = file(tokens[at].span.start);
            let end = tokens[at + 2..].iter().position(|token| token.kind == Kind::Dedent)
                .map_or(tokens.len(), |offset| at + 3 + offset);
            let mut body_tokens = tokens[at + 2..end].to_vec();
            body_tokens.push(Token { kind: Kind::End, span: tokens[at].span.clone() });
            let mut parser = Parser::written(body_tokens, modules);
            let signature = parser.token().span.clone();
            let name = parser.name()?;
            parser.expect(Kind::Symbol("("))?;
            let parameters = parser.parameters(")", |p| {
                let param = p.name()?;
                p.expect(Kind::Symbol(":"))?;
                Ok((param, scalar(p)?))
            })?;
            parser.expect(Kind::Symbol("->"))?;
            let result = scalar(&mut parser)?;
            parser.open_block()?;
            if !parser.keyword("return") {
                return Err(parser.error("a const function body must be one return expression"));
            }
            let body = parser.expression(0)?;
            parser.expect(Kind::Newline)?;
            parser.expect(Kind::Dedent)?;
            validate(&body, &parameters, here, &functions)
                .map_err(|message| Diagnostic::new("W0401", body.span.clone(), message))?;
            if functions.insert((here, name), ConstFunction { parameters, result, body }).is_some() {
                return Err(Diagnostic::new("W0100", signature, "duplicate const function"));
            }
            dropped[at] = true;
            at += 1;
            continue;
        }
        match &tokens[at].kind {
            Kind::Indent => depth += 1,
            Kind::Dedent => depth = depth.saturating_sub(1),
            Kind::Name(word) if matches!(word.as_str(), "let" | "const") && depth == 0 && start => {
                let end = tokens[at..].iter().position(|t| matches!(t.kind, Kind::Newline | Kind::End)).map_or(tokens.len(), |n| at + n);
                let line = &tokens[at..end];
                let span = tokens[at].span.clone();
                let refuse = |message: &'static str| Diagnostic::new("W0100", span.clone(), message);
                let Some(Kind::Name(name)) = line.get(1).map(|t| &t.kind) else { return Err(refuse("a module constant is `const NAME = value` or `let NAME = value`")); };
                // A built-in type's name would be read as the constant where the type is written.
                if BUILT_IN_TYPES.contains(&name.as_str()) {
                    return Err(Diagnostic::new("W0100", line[1].span.clone(), "this name is a built-in type; give the constant another name"));
                }
                let (declared, value) = match line.get(2).map(|t| &t.kind) {
                    Some(Kind::Symbol(":")) => match (line.get(3).map(|t| &t.kind), line.get(4).map(|t| &t.kind)) {
                        (Some(Kind::Name(ty)), Some(Kind::Symbol("="))) => (Some(ty.clone()), 5),
                        _ => return Err(refuse("a typed module constant is `const NAME: Type = value` or `let NAME: Type = value`")),
                    },
                    Some(Kind::Symbol("=")) => (None, 3),
                    _ => return Err(refuse("a module constant is `const NAME = value` or `let NAME = value`")),
                };
                let here = file(span.start);
                if homes.get(name).is_some_and(|h| h.iter().any(|(f, _)| *f == here)) { return Err(refuse("this module constant is already declared")); }
                let mut expression: Vec<Token> = qualify(line[value..].to_vec(), &named, &homes, &file);
                for token in &mut expression {
                    match meant(token, here, &homes, &imports)? {
                        Some(key) => token.kind = Kind::Name(key),
                        // A constant's value names only constants, so a private one of an imported file is meant.
                        None => if let Kind::Name(n) = &token.kind && homes.get(n).is_some_and(|h| h.iter().any(|(f, public)| !public && imports.get(&here).is_some_and(|i| i.contains(f)))) {
                            return Err(Diagnostic::new("W0401", token.span.clone(), "this module constant is private to the file that declares it; that file offers it with `pub let`"));
                        },
                    }
                }
                let tail = tokens.get(end).map_or(span.clone(), |t| t.span.clone());
                expression.push(Token { kind: Kind::Newline, span: tail.clone() });
                expression.push(Token { kind: Kind::End, span: tail });
                let mut parser = Parser { tokens: expression, cursor: 0, depth: 0, expression_depth: 0, texts: Vec::new(), modules: modules.to_vec(), constants: constants.clone(), private: Default::default(), declared: Default::default(), uses: Vec::new(), generic_scope: Vec::new(), imports: Vec::new(), import_kinds: Vec::new(), variants: Default::default(), capabilities: Vec::new(), generator: false, extra: Vec::new(), shared_private: Default::default(), renamer: Default::default(), derived: Vec::new(), grantable: Vec::new(), generators: Vec::new(), inline_generators: Default::default(), fn_types: false, fn_locals: Vec::new(), scope_types: Vec::new(), call_arities: Default::default(), indent: 0, closure_shapes: Default::default(), view: Vec::new(), quiet: Vec::new(), record_fields: Default::default(), impl_parameters: false, results: Default::default(), recipes: Default::default(), formats: 0 };
                let parsed = parser.expression(0)?;
                if !parser.at(&Kind::Newline) { return Err(refuse("a module constant's value ends at the end of its line")); }
                let folded = evaluate(&parsed.kind, None, here, &functions, &mut 4096, 0).map_err(|message| Diagnostic::new("W0401", parsed.span.clone(), message))?;
                match (declared.as_deref(), &folded) {
                    (None | Some("U64"), Expression::Integer(_)) | (None | Some("Bool"), Expression::Bool(_)) => {}
                    _ => return Err(refuse("a module constant is a U64 or a Bool, and its value must have the type written")),
                }
                constants.insert(format!("{name}@{here}"), folded);
                homes.entry(name.clone()).or_default().push((here, std::mem::take(&mut public)));
                let next = end + usize::from(end < tokens.len() && matches!(tokens[end].kind, Kind::Newline));
                dropped[at..next].iter_mut().for_each(|d| *d = true);
                at = next;
                continue;
            }
            _ => {}
        }
        at += 1;
    }
    let kept: Vec<Token> = tokens.into_iter().zip(dropped).filter_map(|(token, dropped)| (!dropped).then_some(token)).collect();
    // A name means one thing: the constant, wherever an expression uses it. Declaring
    // something else with it would be read as the constant at every use. So a use is a
    // use of the constant, and one in another file needs it to be `pub let`.
    let mut kept = qualify(kept, &named, &homes, &file);
    // A name this file declares itself (a parameter, local or field): never another file's constant.
    // Only a constant's name is asked about, so only those are collected.
    let declared_here: std::collections::BTreeSet<(String, usize)> = kept.iter().enumerate().filter_map(|(index, token)| match &token.kind {
        Kind::Name(n) if homes.contains_key(n) && declaring(&kept, index) => Some((n.clone(), file(token.span.start))),
        _ => None,
    }).collect();
    let declared_in = |name: &str, here: usize| declared_here.contains(&(name.to_string(), here));
    for index in 0..kept.len() {
        if index > 0 && matches!(kept[index - 1].kind, Kind::Symbol(".")) { continue; }
        // Only a name some file declares as a constant can mean one (most programs have few).
        if !matches!(&kept[index].kind, Kind::Name(name) if homes.contains_key(name)) { continue; }
        let here = file(kept[index].span.start);
        let Some(key) = meant(&kept[index], here, &homes, &imports)? else {
            // An imported file's private constant, where this file has nothing of that name.
            if let Kind::Name(name) = &kept[index].kind && homes.get(name).is_some_and(|h| h.iter().any(|(f, public)| !public && imports.get(&here).is_some_and(|i| i.contains(f))))
                && !declared_in(name, here) {
                return Err(Diagnostic::new("W0401", kept[index].span.clone(), "this module constant is private to the file that declares it; that file offers it with `pub let`"));
            }
            continue;
        };
        let token = &kept[index];
        if declaring(&kept, index) {
            return Err(Diagnostic::new("W0100", token.span.clone(), "this name is a module constant; give this another name"));
        }
        kept[index].kind = Kind::Name(key);
    }
    Ok((kept, constants))
}

/// `module.NAME`, where the file imported as `module` offers `NAME` with `pub let`: that
/// constant, as one token holding its key (adversarial review round 19).
fn qualify(tokens: Vec<Token>, named: &BTreeMap<usize, BTreeMap<String, usize>>, homes: &BTreeMap<String, Vec<(usize, bool)>>, file: &dyn Fn(usize) -> usize) -> Vec<Token> {
    // Rewritten in place: `module.NAME` becomes one token, and the `.` and `NAME` after it go.
    let mut tokens = tokens;
    let mut gone: Vec<usize> = Vec::new();
    let mut index = 0;
    while index < tokens.len() {
        if let (Kind::Name(module), Some(Kind::Symbol(".")), Some(Kind::Name(name))) = (&tokens[index].kind, tokens.get(index + 1).map(|t| &t.kind), tokens.get(index + 2).map(|t| &t.kind))
            && !(index > 0 && matches!(tokens[index - 1].kind, Kind::Symbol(".")))
            && let Some(target) = named.get(&file(tokens[index].span.start)).and_then(|names| names.get(module.as_str()))
            && homes.get(name).is_some_and(|h| h.iter().any(|(f, public)| f == target && *public)) {
            let kind = Kind::Name(format!("{name}@{target}"));
            tokens[index].span = tokens[index].span.start..tokens[index + 2].span.end;
            tokens[index].kind = kind;
            gone.extend([index + 1, index + 2]);
            index += 3;
            continue;
        }
        index += 1;
    }
    if !gone.is_empty() {
        let mut at = 0;
        let mut next = gone.iter().peekable();
        tokens.retain(|_| { let keep = next.peek() != Some(&&at); if !keep { next.next(); } at += 1; keep });
    }
    tokens
}

/// Whether the name at `index` declares something: after `let`, `var`, `for`, `fn`, `type`,
/// `trait` or `using`; a parameter or field (`NAME:` first in a list or on a line; `if x <
/// NAME:` is a use); or a variant's binding (`Ok(NAME)`).
fn declaring(kept: &[Token], index: usize) -> bool {
    let before = index.checked_sub(1).map(|i| &kept[i].kind);
    matches!(before, Some(Kind::Name(word)) if matches!(word.as_str(), "let" | "var" | "for" | "fn" | "type" | "trait" | "using"))
        || (matches!(kept.get(index + 1).map(|t| &t.kind), Some(Kind::Symbol(":")))
            && matches!(before, None | Some(Kind::Symbol("(" | ",") | Kind::Newline | Kind::Indent)))
        || in_pattern(kept, index)
}

/// Whether the name at `index` is bound by a pattern: an element of `Variant(a, b)` (or of a
/// pattern nested in one) whose outermost group begins a line or follows `let`, `var` or
/// `else`, and is followed by `:` (an arm, an annotated `let`) or `=`. `if x == Ok(NAME):`,
/// `return Err(NAME)` and `f(Ok(NAME), x)` are expressions.
fn in_pattern(kept: &[Token], index: usize) -> bool {
    let line_end = |kind: &Kind| matches!(kind, Kind::Newline | Kind::Indent | Kind::Dedent);
    let mut at = index;
    loop {
        // The `(` of the group `at` is an element of.
        let mut depth = 0usize;
        let mut open = None;
        for i in (0..at).rev() {
            match &kept[i].kind {
                Kind::Symbol(")") => depth += 1,
                Kind::Symbol("(") if depth == 0 => { open = Some(i); break; }
                Kind::Symbol("(") => depth -= 1,
                kind if line_end(kind) => return false,
                _ => {}
            }
        }
        let Some(open) = open else { return false };
        let Some(Kind::Name(head)) = open.checked_sub(1).map(|i| &kept[i].kind) else { return false };
        if !head.starts_with(|c: char| c.is_ascii_uppercase()) { return false; }
        // Its `)`, every element at this depth a name, a number or a nested group.
        let mut depth = 0usize;
        let mut close = None;
        for (i, token) in kept.iter().enumerate().skip(open + 1) {
            match &token.kind {
                Kind::Symbol("(") => depth += 1,
                Kind::Symbol(")") if depth == 0 => { close = Some(i); break; }
                Kind::Symbol(")") => depth -= 1,
                kind if line_end(kind) => return false,
                Kind::Name(_) | Kind::Symbol(",") => {}
                _ if depth > 0 => {}
                Kind::Integer(_) => {}
                _ => return false,
            }
        }
        let Some(close) = close else { return false };
        match kept.get(close + 1).map(|t| &t.kind) {
            Some(Kind::Symbol(":" | "=")) => {
                return match open.checked_sub(2).map(|i| &kept[i].kind) {
                    None => true,
                    Some(Kind::Name(word)) => matches!(word.as_str(), "let" | "var" | "else"),
                    Some(kind) => line_end(kind),
                };
            }
            // Nested: the group it is an element of must be a pattern in turn.
            Some(Kind::Symbol("," | ")")) => at = open - 1,
            _ => return false,
        }
    }
}

/// The constant a name in file `here` means, as its key `NAME@file`: this file's own, else the
/// one `pub let` of the files it imports; none, and the name is something else of this file's.
fn meant(token: &Token, here: usize, homes: &BTreeMap<String, Vec<(usize, bool)>>, imports: &BTreeMap<usize, Vec<usize>>) -> Result<Option<String>, Diagnostic> {
    let Kind::Name(name) = &token.kind else { return Ok(None) };
    let Some(declared) = homes.get(name) else { return Ok(None) };
    if declared.iter().any(|(f, _)| *f == here) { return Ok(Some(format!("{name}@{here}"))); }
    let imported = imports.get(&here).map(Vec::as_slice).unwrap_or(&[]);
    let offered: Vec<usize> = declared.iter().filter(|(f, public)| *public && imported.contains(f)).map(|(f, _)| *f).collect();
    match offered.as_slice() {
        [] => Ok(None),
        [only] => Ok(Some(format!("{name}@{only}"))),
        _ => Err(Diagnostic::new("W0401", token.span.clone(), "two imported modules offer a constant of this name: give one another name")),
    }
}

fn word(token: Option<&Token>, expected: &str) -> bool {
    matches!(token.map(|t| &t.kind), Some(Kind::Name(name)) if name == expected)
}

#[derive(Clone, Copy)]
enum Scalar { U64, Bool }

impl Scalar {
    fn accepts(self, value: &Expression) -> bool {
        matches!((self, value), (Self::U64, Expression::Integer(_)) | (Self::Bool, Expression::Bool(_)))
    }
}

fn scalar(parser: &mut Parser) -> Result<Scalar, Diagnostic> {
    match &parser.token().kind {
        Kind::Name(name) if name == "U64" => { parser.take(); Ok(Scalar::U64) }
        Kind::Name(name) if name == "Bool" => { parser.take(); Ok(Scalar::Bool) }
        _ => Err(parser.error("const functions accept and return U64 or Bool")),
    }
}

struct ConstFunction {
    parameters: Vec<(String, Scalar)>,
    result: Scalar,
    body: Expr,
}

type ConstFunctions = BTreeMap<(usize, String), ConstFunction>;

fn validate(expr: &Expr, params: &[(String, Scalar)], file: usize, functions: &ConstFunctions) -> Result<(), &'static str> {
    match &expr.kind {
        Expression::Integer(_) | Expression::Bool(_) => Ok(()),
        Expression::Name(name) if params.iter().any(|(param, _)| param == name) => Ok(()),
        Expression::Unary("!", value) => validate(value, params, file, functions),
        Expression::Binary("+" | "-" | "*" | "/" | "%" | "<" | "<=" | ">" | ">=" | "==" | "!=" | "&&" | "||", a, b) => {
            validate(a, params, file, functions)?;
            validate(b, params, file, functions)
        }
        Expression::Call(callee, args) if matches!(&callee.kind, Expression::Name(name) if functions.contains_key(&(file, name.clone()))) =>
            args.iter().try_for_each(|arg| validate(arg, params, file, functions)),
        _ => Err("const function bodies use scalar expressions, parameters, and earlier const functions from the same file"),
    }
}

fn evaluate(e: &Expression, locals: Option<&[(String, Expression)]>, file: usize, functions: &ConstFunctions, fuel: &mut usize, depth: usize) -> Result<Expression, &'static str> {
    if depth >= 64 || *fuel == 0 { return Err("constant evaluation exceeds its work limit"); }
    *fuel -= 1;
    let mut eval = |expr: &Expression| evaluate(expr, locals, file, functions, fuel, depth + 1);
    use Expression::{Bool, Integer};
    match e {
        Expression::Name(name) => locals.unwrap_or_default().iter().find(|(n, _)| n == name).map(|(_, value)| value.clone())
            .ok_or("a module constant may name only constants declared above it"),
        Expression::Call(callee, args) => {
            let Expression::Name(name) = &callee.kind else { return Err("a constant call needs a const function name"); };
            let function = functions.get(&(file, name.clone())).ok_or("a constant call needs an earlier const function in the same file")?;
            if args.len() != function.parameters.len() { return Err("wrong number of const function arguments"); }
            let mut bindings = Vec::with_capacity(args.len());
            for ((param, ty), arg) in function.parameters.iter().zip(args) {
                let value = eval(&arg.kind)?;
                if !ty.accepts(&value) { return Err("const function argument has the wrong type"); }
                bindings.push((param.clone(), value));
            }
            let value = evaluate(&function.body.kind, Some(&bindings), file, functions, fuel, depth + 1)?;
            if !function.result.accepts(&value) { return Err("const function result has the wrong type"); }
            Ok(value)
        }
        Expression::Unary("!", arg) => match eval(&arg.kind)? {
            Bool(value) => Ok(Bool(!value)),
            _ => Err("`!` needs a Bool"),
        },
        Expression::Binary(op, a, b) => {
            let left = eval(&a.kind)?;
            if locals.is_some() && matches!((op, &left), (&"&&", Bool(false)) | (&"||", Bool(true))) {
                return Ok(left);
            }
            let right = eval(&b.kind)?;
            fold_binary(op, left, right)
        }
        Integer(_) | Bool(_) => Ok(e.clone()),
        _ => Err("a module constant is made of numbers, Bools, operators and earlier constants"),
    }
}

/// The value of a constant expression: numbers in U64, checked like any arithmetic, and Bools.
pub(super) fn fold(e: &Expression) -> Result<Expression, &'static str> {
    use Expression::{Bool, Integer};
    Ok(match e {
        Integer(n) => Integer(*n),
        Bool(b) => Bool(*b),
        Expression::Unary("!", inner) => match fold(&inner.kind)? { Bool(b) => Bool(!b), _ => return Err("`!` needs a Bool") },
        Expression::Binary(op, a, b) => return fold_binary(op, fold(&a.kind)?, fold(&b.kind)?),
        Expression::Name(_) => return Err("a module constant may name only constants declared above it"),
        _ => return Err("a module constant is made of numbers, Bools, operators and earlier constants"),
    })
}

fn fold_binary(op: &str, a: Expression, b: Expression) -> Result<Expression, &'static str> {
    use Expression::{Bool, Integer};
    Ok(match (a, b) {
            (Integer(x), Integer(y)) => match op {
                "+" => Integer(x.checked_add(y).ok_or("a module constant overflows U64")?),
                "-" => Integer(x.checked_sub(y).ok_or("a module constant goes below zero")?),
                "*" => Integer(x.checked_mul(y).ok_or("a module constant overflows U64")?),
                "/" => Integer(x.checked_div(y).ok_or("a module constant divides by zero")?),
                "%" => Integer(x.checked_rem(y).ok_or("a module constant divides by zero")?),
                "<" => Bool(x < y), "<=" => Bool(x <= y), ">" => Bool(x > y), ">=" => Bool(x >= y),
                "==" => Bool(x == y), "!=" => Bool(x != y),
                _ => return Err("this operator is not available in a module constant"),
            },
            (Bool(x), Bool(y)) => match op {
                "&&" => Bool(x && y), "||" => Bool(x || y), "==" => Bool(x == y), "!=" => Bool(x != y),
                _ => return Err("this operator is not available on Bools"),
            },
            _ => return Err("both sides of an operator in a module constant have one type"),
    })
}

#[cfg(test)]
mod tests {
    use crate::{ast::{Declaration, Expression, Statement}, parser::parse};
    fn first_return(source: &str) -> Expression {
        let program = parse(source).expect("parses");
        let Some(Declaration::Function(f)) = program.declarations.first() else { panic!("a function") };
        let Some(Statement::Return(Some(e))) = f.body.first() else { panic!("a return") };
        e.kind.clone()
    }
    #[test]
    fn a_constant_is_its_value_wherever_it_is_used_and_may_use_earlier_ones() {
        let source = "let WIDTH = 16\nlet CELLS: U64 = WIDTH * WIDTH + 1\nlet BIG = CELLS > 200\nfn f() -> U64:\n    return CELLS\n";
        assert_eq!(first_return(source), Expression::Integer(257));
        assert_eq!(first_return(&source.replace("return CELLS", "return BIG")), Expression::Bool(true));
    }
    #[test]
    fn constants_are_checked_and_refused_when_they_could_mislead() {
        let refused = |source: &str, part: &str| {
            let error = parse(source).err().unwrap_or_else(|| panic!("refused: {source}"));
            assert!(error.message.contains(part), "{source}: {}", error.message);
        };
        refused("let A = 18446744073709551615 + 1\nfn f():\n    return\n", "overflows");
        refused("let A = 1 - 2\nfn f():\n    return\n", "below zero");
        refused("let A = B\nlet B = 1\nfn f():\n    return\n", "declared above");
        refused("let A: Bool = 1\nfn f():\n    return\n", "type written");
        refused("let A = 1\nlet A = 2\nfn f():\n    return\n", "already declared");
        refused("let A = 1\nfn f():\n    let A = 2\n    return\n", "module constant");
        refused("let A = 1\nfn f(A: U64):\n    return\n", "module constant");
        // A use followed by a block's `:` is a use, not a declaration.
        assert!(parse("let A = 1\nfn f(x: U64):\n    if x < A:\n        return\n    return\n").is_ok());
    }
}
