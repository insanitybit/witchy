//! Closures (book: tour-functions.md, "Closures"), first version: desugared here, so the
//! checker, proofs and code generator see only records, traits and generics.
//!
//! - `fn(n: U64) -> U64: n + by` is a value of a sealed record `Closure#<at>` (a name no source can write; not `#...`, which marks a blanket impl) holding what
//!   it captures (`by`, copied), with `impl WitchyFn1<U64, U64>`: `call(self: &Self, n: U64)`
//!   copies each capture into a local and returns the body.
//! - A parameter `f: fn(U64) -> U64` makes the function generic: `f: #F0` with the bound
//!   `#F0: WitchyFn1<U64, U64>`, so any closure of that shape may be passed.
//! - `f(x)` where `f` is such a parameter, or a local bound to a closure, is `f.call(x)`.
//!
//! In this first version arguments, results and captures are numbers or Bools, as in
//! generators: a closure holds no owner, reference or capability, so nothing can escape a
//! loan through one. A captured variable is a parameter or a `let`/`var` with a type.
use super::{generator::scalar, lex, Kind, Parser};
use crate::{ast::{Declaration, Expr, Expression, Statement, Type}, diagnostic::Diagnostic};

/// The type `fn(A) -> R` as parsed, before a parameter of it is made generic.
pub(super) const FN_TYPE: &str = "#Fn";

/// Spellable, since bounds are read again as text: a program may implement one for its own
/// type, which makes that type callable where a function type is expected.
fn trait_name(arity: usize) -> String { format!("WitchyFn{arity}") }

/// In `call_arities`, the call-once trait of closures that take over owners: `WitchyOnce0<R>`,
/// with `call_once(self: Self) -> R`, run once and consumed (docs/wasm-closures.md).
pub(super) const ONCE: usize = usize::MAX;

fn refuse(span: &std::ops::Range<usize>, message: String) -> Diagnostic {
    Diagnostic::new("W0401", span.clone(), message)
}

impl Parser {
    /// `fn(A, B) -> R` in a type, after the `fn`.
    pub(super) fn function_type(&mut self) -> Result<Type, Diagnostic> {
        self.expect(Kind::Symbol("("))?;
        let mut parts = self.parameters(")", Self::ty)?;
        if !self.symbol("->") { return Err(self.error("a function type states its result: `fn(U64) -> U64`")); }
        parts.push(self.ty()?);
        Ok(Type::Named(FN_TYPE.into(), parts))
    }

    /// Each parameter of a function type becomes a type parameter bounded by the call trait
    /// of its shape. Returns the names of those parameters, which calls rewrite.
    pub(super) fn function_parameters(&mut self, generics: &mut Vec<String>, any: &mut Vec<bool>, bounds: &mut Vec<Vec<String>>, parameters: &mut [(String, Type)]) -> Result<Vec<String>, Diagnostic> {
        let span = self.token().span.clone();
        let mut names = Vec::new();
        // A function type may also name the type parameters in scope (`fn(T) -> Bool` in a
        // method of `Vec<T>`): a closure literal's own types are still numbers and Bools, and
        // ownership through a call trait is checked like any generic call.
        let in_scope: Vec<String> = self.generic_scope.iter().cloned().chain(generics.iter().cloned()).collect();
        let named = |t: &Type| scalar(t) || matches!(t, Type::Named(n, a) if a.is_empty() && in_scope.contains(n));
        // An argument may also be a shared reference to anything: `fn(&String) -> Bool`.
        let allowed = |t: &Type| named(t) || matches!(t, Type::Borrow { mutable: false, .. });
        // A result may also be nothing: `fn(U64) -> ()`, a callback run for what it does.
        let shape = |parts: &[Type]| parts.split_last().is_some_and(|(result, arguments)| (named(result) || *result == Type::Unit) && arguments.iter().all(allowed));
        for (index, (name, ty)) in parameters.iter_mut().enumerate() {
            match ty {
                Type::Named(n, parts) if n == FN_TYPE => {
                    if !shape(parts) { return Err(refuse(&span, format!("`{name}`: a function type takes and gives numbers, Bools or type parameters in this first version"))); }
                    let generic = format!("#F{index}");
                    generics.push(generic.clone());
                    any.push(false);
                    bounds.push(vec![Type::Named(trait_name(parts.len() - 1), parts.clone()).render()]);
                    self.call_arities.insert(parts.len() - 1);
                    *ty = Type::Named(generic, Vec::new());
                    names.push(name.clone());
                }
                // `f: &fn(U64) -> U64`: a callback lent, not handed over. It may hold what is not
                // copied (a capability: docs/design-closure-authority.md), so its type
                // parameter is `Any`: the callee calls it and neither copies nor drops it.
                Type::Borrow { mutable: false, inner } if matches!(inner.as_ref(), Type::Named(n, _) if n == FN_TYPE) => {
                    let Type::Named(_, parts) = inner.as_ref() else { unreachable!() };
                    if !shape(parts) { return Err(refuse(&span, format!("`{name}`: a function type takes and gives numbers, Bools or type parameters in this first version"))); }
                    let generic = format!("#F{index}");
                    generics.push(generic.clone());
                    any.push(true);
                    bounds.push(vec![Type::Named(trait_name(parts.len() - 1), parts.clone()).render()]);
                    self.call_arities.insert(parts.len() - 1);
                    *ty = Type::Borrow { mutable: false, inner: Box::new(Type::Named(generic, Vec::new())) };
                    names.push(name.clone());
                }
                other if format!("{other:?}").contains(&format!("{FN_TYPE:?}")) => {
                    return Err(refuse(&span, format!("`{name}`: a function type is a parameter's own type in this first version, not inside `&` or another type")));
                }
                _ => {}
            }
        }
        Ok(names)
    }

    /// A closure, after the `fn` that starts it.
    pub(super) fn closure(&mut self, at: std::ops::Range<usize>) -> Result<Expression, Diagnostic> {
        self.expect(Kind::Symbol("("))?;
        let parameters = self.parameters(")", Self::typed_name)?;
        // `-> T`, or left out: then read from the body below (`inferred`).
        let stated = if self.symbol("->") { Some(self.ty()?) } else { None };
        self.expect(Kind::Symbol(":"))?;
        // `fn(n: U64) -> U64: n * n`, or a block whose statements end in `return`.
        let block = if self.eat(&Kind::Newline) {
            self.expect(Kind::Indent)?;
            // A closure is not its maker's generator: a `yield` in it is refused as outside one
            // (found by an adversarial review: it crashed the checker).
            let generator = std::mem::replace(&mut self.generator, false);
            let mut statements = Vec::new();
            let mut parsed = Ok(());
            while !self.at(&Kind::Dedent) { match self.statement() { Ok(s) => statements.push(s), Err(e) => { parsed = Err(e); break; } } }
            self.generator = generator;
            parsed?;
            self.expect(Kind::Dedent)?;
            Some(statements)
        } else { None };
        let body = match block { Some(_) => Expr { kind: Expression::Unit, span: at.clone() }, None => self.expression(0)? };
        let result = match stated {
            Some(result) => result,
            None => match &block {
                // A block that returns no value gives `()`.
                Some(statements) if !returns_value(statements) => Type::Unit,
                Some(_) => return Err(refuse(&at, "a closure whose block returns a value states its result type: `fn(n: U64) -> U64:`".into())),
                None => self.inferred(&body, &parameters).ok_or_else(|| refuse(&at, "this closure's result type is not evident from its body (a parameter, a captured variable, a comparison or a decimal number): state it, `fn(n: U64) -> U64: ...`".into()))?,
            },
        };
        if !parameters.iter().all(|(_, t)| scalar(t) || matches!(t, Type::Borrow { mutable: false, .. })) || !(scalar(&result) || result == Type::Unit) {
            return Err(refuse(&at, "a closure takes and gives numbers or Bools in this first version".into()));
        }
        // What the body names of the enclosing function's variables, latest binding first.
        let text = match &block { Some(statements) => format!("{statements:?}"), None => format!("{body:?}") };
        let mut captures: Vec<(String, Type)> = Vec::new();
        // Capabilities the closure takes over (moved in; read through `self` in `call`).
        let mut held: Vec<(String, Type)> = Vec::new();
        // Owners the closure takes over (moved in); a closure holding one is called once.
        let mut owned: Vec<(String, Type)> = Vec::new();
        let mut seen: Vec<&str> = Vec::new();
        for (name, ty, _) in self.scope_types.iter().rev() {
            if seen.contains(&name.as_str()) { continue; }
            seen.push(name);
            if parameters.iter().any(|(p, _)| p == name) || !text.contains(&format!("Name({name:?})")) { continue; }
            match ty {
                Some(t) if scalar(t) => captures.push((name.clone(), t.clone())),
                // A shared reference or slice is copied in too: the closure is then a view, and
                // keeps what it borrows lent while it is in use (docs/design-references-to-views.md,
                // step 3).
                Some(t @ Type::Borrow { mutable: false, .. }) => captures.push((name.clone(), t.clone())),
                // What the body moves, it takes over: the closure is then called once.
                Some(t) if !scalar(t) && !matches!(t, Type::Borrow { .. }) && text.contains(&format!("Unary(\"move\", Expr {{ kind: Name({name:?})")) => owned.push((name.clone(), t.clone())),
                Some(t) if capability(t) => held.push((name.clone(), t.clone())),
                Some(_) => return Err(refuse(&at, format!("`{name}` is captured by a closure, which holds numbers, Bools, shared references and capabilities that need no finishing in this first version"))),
                None => return Err(refuse(&at, format!("`{name}` is captured by a closure: give it a type where it is declared (`let {name}: Type = ...`)"))),
            }
        }
        let record = format!("Closure#{}", at.start);
        if !owned.is_empty() { return self.once_closure(at, record, parameters, result, block, body, captures, held, owned); }
        self.closure_shapes.insert(record.clone(), parameters.iter().map(|(_, t)| t.clone()).chain([result.clone()]).collect());
        let arity = parameters.len();
        self.call_arities.insert(arity);
        let mut text = String::from("sealed type WitchyClosure:\n");
        if captures.is_empty() && held.is_empty() { text.push_str("    witchy_nothing: Bool\n"); }
        for (name, ty) in captures.iter().chain(&held) { text.push_str(&format!("    {name}: {}\n", ty.render())); }
        let shape: Vec<String> = parameters.iter().map(|(_, t)| t.render()).chain([result.render()]).collect();
        text.push_str(&format!("impl WitchyCall<{}> for WitchyClosure:\n", shape.join(", ")));
        let arguments: Vec<String> = parameters.iter().map(|(n, t)| format!("{n}: {}", t.render())).collect();
        text.push_str(&format!("    fn call(self: &WitchyClosure{}{}) -> {}:\n", if arguments.is_empty() { "" } else { ", " }, arguments.join(", "), result.render()));
        for (name, ty) in &captures { text.push_str(&format!("        let {name}: {} = (*self).{name}\n", ty.render())); }
        // A placeholder the body replaces (a block's statements take its place).
        text.push_str("        return witchy_body\n");
        let mut tokens = lex(&text)?;
        for token in &mut tokens {
            // An error in what is written here is reported at the closure, in its own file.
            token.span = at.clone();
            if let Kind::Name(name) = &mut token.kind {
                if name == "WitchyClosure" { *name = record.clone(); } else if name == "WitchyCall" { *name = trait_name(arity); }
            }
        }
        let module = self.module();
        let mut written = Parser::written(tokens, &self.modules);
        let mut declaration = written.declaration()?;
        if let Declaration::Record { module: m, .. } = &mut declaration { *m = module; }
        let mut implementation = written.declaration()?;
        if let Declaration::Impl { methods, .. } = &mut implementation {
            for (method, _) in methods.iter_mut() {
                method.module = module;
                let names: Vec<String> = held.iter().map(|(n, _)| n.clone()).collect();
                if let Some(statements) = &block {
                    let mut statements = statements.clone();
                    for statement in &mut statements { through_self_in(statement, &names); }
                    method.body.pop();
                    method.body.extend(statements);
                } else if let Some(Statement::Return(Some(value))) = method.body.last_mut() {
                    let mut body = body.clone();
                    through_self(&mut body, &names);
                    *value = body;
                }
            }
        }
        self.extra.push(declaration);
        self.extra.push(implementation);
        let name = |n: &String| Expr { kind: Expression::Name(n.clone()), span: at.clone() };
        let values = if captures.is_empty() && held.is_empty() { vec![Expr { kind: Expression::Bool(false), span: at.clone() }] }
            else { captures.iter().map(|(n, _)| name(n))
                .chain(held.iter().map(|(n, _)| Expr { kind: Expression::Unary("move", Box::new(name(n))), span: at.clone() })).collect() };
        Ok(Expression::Call(Box::new(Expr { kind: Expression::Name(record), span: at.clone() }), values))
    }
}

impl Parser {
    /// A closure that takes over an owner (`fn() -> (): serve(move socket)`): its record holds
    /// what it captured, owners included, and `call_once(self)` takes the record apart into
    /// locals of the same names and runs the body, once. Nothing is reached through `self`.
    #[allow(clippy::too_many_arguments)]
    fn once_closure(&mut self, at: std::ops::Range<usize>, record: String, parameters: Vec<(String, Type)>, result: Type, block: Option<Vec<Statement>>, body: Expr,
        captures: Vec<(String, Type)>, held: Vec<(String, Type)>, owned: Vec<(String, Type)>) -> Result<Expression, Diagnostic> {
        let arity = parameters.len();
        if arity > 8 { return Err(refuse(&at, "a closure takes at most 8 arguments".into())); }
        if captures.iter().any(|(_, t)| matches!(t, Type::Borrow { .. })) {
            return Err(refuse(&at, "a closure that takes over an owner holds no reference in this first version".into()));
        }
        self.call_arities.insert(ONCE - arity);
        let fields: Vec<&(String, Type)> = captures.iter().chain(&held).chain(&owned).collect();
        let mut text = String::from("sealed type WitchyClosure:\n");
        for (name, ty) in &fields { text.push_str(&format!("    {name}: {}\n", ty.render())); }
        let shape: Vec<String> = parameters.iter().map(|(_, t)| t.render()).chain([result.render()]).collect();
        text.push_str(&format!("impl WitchyOnce{arity}<{}> for WitchyClosure:\n", shape.join(", ")));
        let arguments: String = parameters.iter().map(|(n, t)| format!(", {n}: {}", t.render())).collect();
        text.push_str(&format!("    fn call_once(self: WitchyClosure{arguments}) -> {}:\n", result.render()));

        text.push_str(&format!("        let WitchyClosure({}) = move self\n", fields.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>().join(", ")));
        text.push_str("        return witchy_body\n");
        // As a task's function (`Task.spawn`): told to stop before it began (a race given up, a
        // shutdown), it runs nothing and gives up what it took over. Only here, at the task's
        // start: called anywhere else, `call_once` runs (an automated review: a cleanup closure
        // called in a task already told to stop was skipped).
        if arity > 0 {
        } else if result == Type::Unit {
            text.push_str("    fn start_task(self: WitchyClosure):\n        if memory.first_word() == 9223372036854775808:\n            self.discard()\n            return\n        self.call_once()\n");
        } else {
            // Work that gives something back is not a task's (`Task.spawn` refuses it).
            text.push_str("    fn start_task(self: WitchyClosure):\n        self.discard()\n        unreachable\n");
        }
        // Given up unstarted (a task that could not be made): each owner it took over is
        // discarded, with its own `discard` (`impl Discard`, or a method of that name).
        text.push_str("    fn discard(self: WitchyClosure):\n");
        text.push_str(&format!("        let WitchyClosure({}) = move self\n", fields.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>().join(", ")));
        // A capability needs no finishing; any other owner is discarded.
        for (name, ty) in &owned { if !capability(ty) { text.push_str(&format!("        {name}.discard()\n")); } }
        let mut tokens = lex(&text)?;
        for token in &mut tokens {
            token.span = at.clone();
            if let Kind::Name(name) = &mut token.kind && name == "WitchyClosure" { *name = record.clone(); }
        }
        let module = self.module();
        let mut written = Parser::written(tokens, &self.modules);
        let mut declaration = written.declaration()?;
        if let Declaration::Record { module: m, .. } = &mut declaration { *m = module; }
        let mut implementation = written.declaration()?;
        if let Declaration::Impl { methods, .. } = &mut implementation {
            for (method, _) in methods.iter_mut() {
                method.module = module;
                if method.name != "call_once" { continue; }
                match &block {
                    Some(statements) => { method.body.pop(); method.body.extend(statements.iter().cloned()); }
                    None => if let Some(Statement::Return(Some(value))) = method.body.last_mut() { *value = body.clone(); },
                }
            }
        }
        self.extra.push(declaration);
        self.extra.push(implementation);
        let name = |n: &String| Expr { kind: Expression::Name(n.clone()), span: at.clone() };
        let values = captures.iter().map(|(n, _)| name(n))
            .chain(held.iter().chain(&owned).map(|(n, _)| Expr { kind: Expression::Unary("move", Box::new(name(n))), span: at.clone() })).collect();
        Ok(Expression::Call(Box::new(Expr { kind: Expression::Name(record), span: at.clone() }), values))
    }
}

/// The call traits of the arities used: `WitchyFn1<A0, R>` with `call(self: &Self, a0: A0) -> R`.
pub(super) fn call_traits(arities: &std::collections::BTreeSet<usize>, modules: &[usize]) -> Result<Vec<Declaration>, Diagnostic> {
    let mut out = Vec::new();
    for &arity in arities {
        if arity > ONCE - 16 {
            // `discard`: what an unstarted closure holds is given up, owners each discarded.
            // `start_task` (no arguments only): the same as `call_once`, as a task's function
            // (closures give nothing back there), unless the task was told to stop before it began.
            let count = ONCE - arity;
            // Type parameter names a program does not choose: a program's own type `R` or `A0`
            // conflicted with these (integration review).
            let names: Vec<String> = (0..count).map(|i| format!("WitchyArgument{i}")).collect();
            let generics = names.iter().cloned().chain(["WitchyResult".to_string()]).collect::<Vec<_>>().join(", ");
            let arguments: String = names.iter().enumerate().map(|(i, t)| format!(", a{i}: {t}")).collect();
            let start = if count == 0 { "    fn start_task(self: Self) suspends\n" } else { "" };
            let text = format!("pub trait WitchyOnce{count}<{generics}>:\n    fn call_once(self: Self{arguments}) -> WitchyResult suspends\n    fn discard(self: Self)\n{start}");
            out.push(Parser::written(lex(&text)?, modules).declaration()?);
            continue;
        }
        let names: Vec<String> = (0..arity).map(|i| format!("WitchyArgument{i}")).collect();
        let generics = names.iter().cloned().chain(["WitchyResult".to_string()]).collect::<Vec<_>>().join(", ");
        let arguments: Vec<String> = names.iter().enumerate().map(|(i, t)| format!(", a{i}: {t}")).collect();
        // `suspends`: a closure may pause its task; a caller's effect follows the closure it is
        // given (docs/design-async.md).
        let text = format!("pub trait WitchyCall<{generics}>:\n    fn call(self: &Self{}) -> WitchyResult suspends\n", arguments.concat());
        let mut tokens = lex(&text)?;
        for token in &mut tokens { if let Kind::Name(name) = &mut token.kind && name == "WitchyCall" { *name = trait_name(arity); } }
        out.push(Parser::written(tokens, modules).declaration()?);
    }
    Ok(out)
}

/// A function whose result is a function type returns one closure: its result is that
/// closure's record (the caller calls it through `call`; nothing else can be written of it).
/// Every `return` gives the same closure literal, so the type is that one record.
pub(super) fn returned(result: Type, body: &[Statement], at: &std::ops::Range<usize>, shapes: &std::collections::BTreeMap<String, Vec<Type>>) -> Result<Type, Diagnostic> {
    let own = matches!(&result, Type::Named(name, _) if name == FN_TYPE);
    if !own {
        // `-> &fn(...)` would hand back a callback lent to the function, which outlives it.
        if format!("{result:?}").contains(&format!("{FN_TYPE:?}")) { return Err(refuse(at, "a function returns a closure as `fn(...)`, not inside `&` or another type, in this first version".into())); }
        return Ok(result);
    }
    let Type::Named(_, parts) = &result else { unreachable!() };
    if !parts.split_last().is_some_and(|(r, a)| (scalar(r) || *r == Type::Unit) && a.iter().all(scalar)) { return Err(refuse(at, "a function type takes and gives numbers or Bools in this first version".into())); }
    let mut records = Vec::new();
    let mut other = false;
    fn walk(statements: &[Statement], records: &mut Vec<String>, other: &mut bool) {
        for statement in statements {
            match statement {
                Statement::Return(Some(value)) => match &value.kind {
                    Expression::Call(callee, _) if matches!(&callee.kind, Expression::Name(n) if n.starts_with("Closure#")) => {
                        let Expression::Name(n) = &callee.kind else { unreachable!() };
                        if !records.contains(n) { records.push(n.clone()); }
                    }
                    _ => *other = true,
                },
                Statement::If { yes, no, .. } => { walk(yes, records, other); walk(no, records, other); }
                Statement::While { body, .. } | Statement::For { body, .. } | Statement::Block(body) | Statement::Using { body, .. } => walk(body, records, other),
                Statement::Match { arms, .. } => for arm in arms { walk(&arm.body, records, other); },
                Statement::Guard { failure, .. } => walk(&failure.body, records, other),
                _ => {}
            }
        }
    }
    walk(body, &mut records, &mut other);
    match records.as_slice() {
        // The closure's own shape is the declared one (found by an adversarial review: a
        // `-> fn(U64) -> U64` returned a closure of Bools, and the signature was false).
        [record] if !other && shapes.get(record) == Some(parts) => Ok(Type::Named(record.clone(), Vec::new())),
        [record] if !other => Err(refuse(at, format!("this function says it returns a `fn({}`, and the closure it returns is a `fn({}`",
            describe(parts), shapes.get(record).map(|s| describe(s)).unwrap_or_default()))),
        _ => Err(refuse(at, "a function returning a function type returns one closure, written once, in this first version".into())),
    }
}

/// `U64) -> U64` for the parts of a function type, as written after `fn(`.
fn describe(parts: &[Type]) -> String {
    let (result, arguments) = parts.split_last().expect("a function type has a result");
    format!("{}) -> {}", arguments.iter().map(Type::render).collect::<Vec<_>>().join(", "), result.render())
}

/// A capability a closure may take over: one that is used, not finished or closed (a
/// `File<Write>` must be finished, so it stays with its owner).
impl Parser {
    /// A binding's type when its value says it plainly, for what closures capture: another
    /// handle from `share()` on an allocator, clock or random source (or a reference to one),
    /// and a variable moved or copied whole. Anything else needs `let name: Type = ...`.
    /// A closure body's type when it shows: a comparison or Bool is a Bool; arithmetic and bit
    /// operations have the type of an operand that shows one (a parameter, a captured variable,
    /// a decimal number). A bare integer literal or a call does not show its type.
    fn inferred(&self, body: &Expr, parameters: &[(String, Type)]) -> Option<Type> {
        match &body.kind {
            Expression::Bool(_) => Some(Type::Named("Bool".into(), Vec::new())),
            Expression::Binary("==" | "!=" | "<" | "<=" | ">" | ">=" | "&&" | "||", _, _) | Expression::Unary("!", _) => Some(Type::Named("Bool".into(), Vec::new())),
            Expression::Binary("+" | "-" | "*" | "/" | "%" | "&" | "|" | "^" | "<<" | ">>", left, right) => {
                let shift = matches!(&body.kind, Expression::Binary("<<" | ">>", _, _));
                self.inferred(left, parameters).or_else(|| if shift { None } else { self.inferred(right, parameters) })
            }
            Expression::Unary("-" | "~", inner) => self.inferred(inner, parameters),
            // `if c: a else: b`: what either value shows.
            Expression::Conditional(_, yes, no) => self.inferred(yes, parameters).or_else(|| self.inferred(no, parameters)),
            Expression::Name(name) => parameters.iter().rev().find(|(n, _)| n == name).map(|(_, t)| t.clone())
                .filter(|t| scalar(t)).or_else(|| self.evident(body).filter(scalar)),
            Expression::Float(_) => self.evident(body),
            _ => None,
        }
    }
    pub(super) fn evident(&self, value: &Expr) -> Option<Type> {
        let known = |name: &str| self.scope_types.iter().rev().find(|(n, _, _)| n == name).and_then(|(_, t, _)| t.clone());
        match &value.kind {
            Expression::Name(name) => known(name),
            // A float literal is an F64, whatever it is used with (an integer literal is not
            // evident: it may be any integer type).
            Expression::Float(_) => Some(Type::Named("F64".into(), Vec::new())),
            Expression::Unary("-", inner) if matches!(inner.kind, Expression::Float(_)) => Some(Type::Named("F64".into(), Vec::new())),
            Expression::Unary("move", inner) => match &inner.kind { Expression::Name(name) => known(name), _ => None },
            // As the checker types an unwritten `let`: a whole number is a U64, a comparison a
            // Bool, arithmetic the type of an operand that shows one (a literal beside a name
            // takes the name's).
            Expression::Integer(_) => Some(Type::Named("U64".into(), Vec::new())),
            Expression::Bool(_) | Expression::Unary("!", _) | Expression::Binary("==" | "!=" | "<" | "<=" | ">" | ">=" | "&&" | "||", _, _) => Some(Type::Named("Bool".into(), Vec::new())),
            Expression::Binary("<<" | ">>", left, _) => self.evident(left),
            Expression::Binary(_, left, right) => {
                let shown = |e: &Expr| if matches!(e.kind, Expression::Integer(_)) { None } else { self.evident(e) };
                shown(left).or_else(|| shown(right)).or_else(|| self.evident(left))
            }
            Expression::Unary("-" | "~", inner) => self.evident(inner),
            // A call of a plain function whose result type its signature names (`results`).
            Expression::Call(callee, _) if matches!(callee.kind, Expression::Name(_)) => match &callee.kind {
                Expression::Name(name) if known(name).is_none() && !self.scope_types.iter().any(|(n, _, _)| n == name) => self.results.get(name).cloned().flatten()
                    // A non-generic record built by its name (`Box(3, 4)`) is a `Box`.
                    .or_else(|| self.record_fields.contains_key(name).then(|| Type::Named(name.clone(), Vec::new()))),
                _ => None,
            },
            // `x.method(...)` where `x`'s type is a plain named one and the method's result a
            // number or Bool (`results`, as `Owner.method`).
            Expression::Call(callee, _) if matches!(&callee.kind, Expression::Field(receiver, method)
                if matches!(&receiver.kind, Expression::Name(name) if known(name).is_some()) && method != "share") => {
                let Expression::Field(receiver, method) = &callee.kind else { return None };
                let Expression::Name(name) = &receiver.kind else { return None };
                let owner = match known(name)? { Type::Borrow { inner, .. } => *inner, other => other };
                let Type::Named(owner, arguments) = owner else { return None };
                if !arguments.is_empty() { return None; }
                self.results.get(&format!("{owner}.{method}")).cloned().flatten()
            }
            Expression::Call(callee, arguments) if arguments.is_empty() => {
                let Expression::Field(receiver, method) = &callee.kind else { return None };
                let Expression::Name(name) = &receiver.kind else { return None };
                if method != "share" { return None; }
                let ty = known(name)?;
                let owner = match ty { Type::Borrow { inner, .. } => *inner, other => other };
                matches!(&owner, Type::Named(n, arguments) if arguments.is_empty() && matches!(n.as_str(), "Allocator" | "Clock" | "Rand")).then_some(owner)
            }
            _ => None,
        }
    }
}

fn capability(ty: &Type) -> bool {
    match ty {
        Type::Named(name, arguments) => match name.as_str() {
            "Console" | "Dir" | "Clock" | "Rand" | "Env" | "Args" | "Net" | "Listener" | "Exec" | "SecretStore" | "Allocator" => true,
            "File" => matches!(arguments.as_slice(), [Type::Named(right, _)] if right == "Read"),
            _ => false,
        },
        _ => false,
    }
}

/// In `call`'s body, a captured capability is `(*self).name`: it is reached where it is
/// held, never copied out.
fn through_self(e: &mut Expr, names: &[String]) {
    if let Expression::Name(n) = &e.kind && names.contains(n) {
        let span = e.span.clone();
        let this = Expr { kind: Expression::Name("self".into()), span: span.clone() };
        let held = Expr { kind: Expression::Unary("*", Box::new(this)), span: span.clone() };
        e.kind = Expression::Field(Box::new(held), n.clone());
        return;
    }
    match &mut e.kind {
        Expression::Forall { lower, upper, body, .. } => { through_self(lower, names); through_self(upper, names); through_self(body, names); }
        Expression::Array(items) => for item in items { through_self(item, names); },
        Expression::Repeat(inner, _) | Expression::Unary(_, inner) | Expression::Field(inner, _) | Expression::Try(inner) => through_self(inner, names),
        Expression::Binary(_, a, b) | Expression::Index(a, b) => { through_self(a, names); through_self(b, names); }
        Expression::Call(callee, arguments) => { through_self(callee, names); for a in arguments { through_self(a, names); } }
        Expression::Conditional(c, a, b) => { through_self(c, names); through_self(a, names); through_self(b, names); }
        Expression::Record(fields) => for (_, value) in fields { through_self(value, names); },
        Expression::Inject(_, Some(inner)) => through_self(inner, names),
        _ => {}
    }
}

/// `through_self` in each expression of a statement, at any depth.
fn through_self_in(statement: &mut Statement, names: &[String]) {
    let all = |statements: &mut Vec<Statement>| for s in statements { through_self_in(s, names); };
    match statement {
        Statement::Expr(e) | Statement::Assert(e) | Statement::Return(Some(e)) => through_self(e, names),
        Statement::Bind { value, .. } | Statement::Open { value, .. } | Statement::OpenFields { value, .. } => through_self(value, names),
        Statement::Assign { target, value, .. } => { through_self(target, names); through_self(value, names); }
        Statement::Guard { value, failure, .. } => { through_self(value, names); all(&mut failure.body); }
        Statement::If { condition, yes, no } => { through_self(condition, names); all(yes); all(no); }
        Statement::While { condition, body, .. } => { through_self(condition, names); all(body); }
        Statement::For { start, end, body, .. } => { through_self(start, names); if let Some(end) = end { through_self(end, names); } all(body); }
        Statement::Match { value, arms } => { through_self(value, names); for arm in arms { all(&mut arm.body); } }
        Statement::Using { value, body, .. } => { through_self(value, names); all(body); }
        Statement::Block(body) => all(body),
        Statement::Choose { inner, .. } => through_self_in(inner, names),
        Statement::Yield { value, .. } => through_self(value, names),
        _ => {}
    }
}

/// Whether a trait name is a call trait's, `WitchyFn<n>`, in any file.
pub(super) fn reserved(name: &str) -> bool {
    let name = name.split('@').next().unwrap_or(name);
    ["WitchyFn", "WitchyOnce"].iter().any(|prefix| name.strip_prefix(prefix).is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())))
}

/// Whether a closure's block returns a value anywhere (a `return` inside a nested closure is that
/// closure's, but closures are already lifted out of the block by now).
fn returns_value(statements: &[Statement]) -> bool {
    // A block ending in a value other than a call's (`n + 1`) means to give it back.
    let ends_in_value = matches!(statements.last(), Some(Statement::Expr(e)) if !matches!(e.kind, Expression::Call(..) | Expression::Unit));
    ends_in_value || format!("{statements:?}").contains("Return(Some(")
}
