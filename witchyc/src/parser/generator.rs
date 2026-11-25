//! `gen fn` (docs/design-generators.md): before anything is checked, a generator becomes a
//! sealed record holding its arguments, its locals and where it stopped; a function that
//! builds one; and the record's `next_mut`, which runs the body to the next `yield`. The
//! checker sees only ordinary code, so nothing here is trusted.
use crate::ast::*;
use crate::diagnostic::Diagnostic;
use std::ops::Range;

/// The name a generator's `yield` carries in `Statement::Yield`, which no source can spell.
pub(super) const YIELD: &str = "gen#";
/// Where a finished generator stays.
const DONE: u64 = u64::MAX;
const STATE: &str = "gen#state";
const VALUE: &str = "gen#value";

/// `count_up` makes type `CountUp`.
pub(super) fn type_name(function: &str) -> String {
    function.split('_').filter(|part| !part.is_empty()).map(|part| {
        let mut letters = part.chars();
        letters.next().map(|first| first.to_ascii_uppercase().to_string() + letters.as_str()).unwrap_or_default()
    }).collect()
}

fn refuse(at: &Range<usize>, message: &str) -> Diagnostic { Diagnostic::new("W0100", at.clone(), message.to_owned()) }

/// Type names the compiler gives meaning to: a generator named for one (`gen fn u64` makes
/// `U64`) would be taken for it (review of nested generators: `#generator U64` made every `U64`
/// local a generator, and the compiler recursed without end).
pub(super) fn built_in(name: &str) -> bool {
    matches!(name, "U8" | "U16" | "U32" | "U64" | "I64" | "F64" | "Bool" | "Text" | "Result" | "Option" | "Buffer" | "PrefixBuffer" | "Allocator"
        | "Clock" | "Console" | "Read" | "Write" | "Env" | "Args" | "Exec" | "File" | "Dir" | "Group" | "Listener" | "Socket" | "Net" | "Rand"
        | "Run" | "Secret" | "SecretStore" | "Task" | "Self" | "Any" | "Brand" | "Fresh")
}
/// What a local of a generator may be: a value the record can hold and copy.
pub(super) fn scalar(ty: &Type) -> bool {
    matches!(ty, Type::Named(name, args) if args.is_empty() && matches!(name.as_str(), "U8" | "U16" | "U32" | "U64" | "I64" | "F64" | "Bool"))
}
fn default(ty: &Type, span: &Range<usize>, results: &std::collections::BTreeMap<String, Option<Type>>) -> Expr {
    // A generator of numbers kept as a local: one made by its function from default arguments,
    // replaced before it is used.
    if !scalar(ty) && let Type::Named(name, args) = ty && args.is_empty() && let Some(Some(Type::Named(function, parameters))) = results.get(&format!("#generator {name}")) {
        let callee = Expr { kind: Expression::Name(function.clone()), span: span.clone() };
        return Expr { kind: Expression::Call(Box::new(callee), parameters.iter().map(|p| default(p, span, results)).collect()), span: span.clone() };
    }
    let kind = match ty {
        Type::Named(name, _) if name == "Bool" => Expression::Bool(false),
        Type::Named(name, _) if name == "F64" => Expression::Float(0f64.to_bits()),
        _ => Expression::Integer(0),
    };
    Expr { kind, span: span.clone() }
}

fn contains_yield(statements: &[Statement]) -> bool { statements.iter().any(yields) }
fn yields(statement: &Statement) -> bool {
    match statement {
        Statement::Yield { name, .. } => name == YIELD,
        Statement::If { yes, no, .. } => contains_yield(yes) || contains_yield(no),
        Statement::While { body, .. } | Statement::For { body, .. } | Statement::Using { body, .. } | Statement::Block(body) => contains_yield(body),
        Statement::Match { arms, .. } => arms.iter().any(|arm| contains_yield(&arm.body)),
        Statement::Guard { failure, .. } => contains_yield(&failure.body),
        Statement::Choose { inner, .. } => yields(inner),
        _ => false,
    }
}

struct Flattener {
    span: Range<usize>,
    blocks: Vec<Vec<Statement>>,
    /// The record's locals: each binding at a level the body is cut at, in order.
    locals: Vec<(String, Type)>,
    parameters: Vec<String>,
    /// Each parameter's type, for a local whose type its value shows (`var x = start`).
    parameter_types: Vec<(String, Type)>,
    /// Plain functions' result types (`Parser::results`).
    results: std::collections::BTreeMap<String, Option<Type>>,
    /// What each step yields: the yielded value is checked as one (a generic record's
    /// constructor takes its type from it).
    element: Type,
}

impl Flattener {
    fn expr(&self, kind: Expression) -> Expr { Expr { kind, span: self.span.clone() } }
    fn name(&self, name: &str) -> Expr { self.expr(Expression::Name(name.to_owned())) }
    /// `(*self).field`
    fn field(&self, field: &str) -> Expr {
        let deref = self.expr(Expression::Unary("*", Box::new(self.name("self"))));
        self.expr(Expression::Field(Box::new(deref), field.to_owned()))
    }
    fn set(&self, target: Expr, value: Expr) -> Statement { Statement::Assign { target, operation: "=", value } }
    /// Go to `state` on the dispatcher's next turn.
    fn jump(&self, state: u64) -> Vec<Statement> {
        vec![self.set(self.name(STATE), self.expr(Expression::Integer(state))), Statement::Continue]
    }
    /// The type a local's value shows, as an ordinary `let` would take it: a whole number is a
    /// U64, a decimal an F64, a comparison a Bool, a name its own type, and arithmetic the type
    /// of an operand that shows one. Anything else is written out.
    fn evident(&self, value: &Expr) -> Option<Type> {
        let named = |n: &str| Some(Type::Named(n.into(), Vec::new()));
        match &value.kind {
            Expression::Integer(_) => named("U64"),
            Expression::Float(_) => named("F64"),
            Expression::Bool(_) | Expression::Unary("!", _) | Expression::Binary("==" | "!=" | "<" | "<=" | ">" | ">=" | "&&" | "||", _, _) => named("Bool"),
            Expression::Name(name) => self.locals.iter().rev().chain(self.parameter_types.iter()).find(|(n, _)| n == name).map(|(_, t)| t.clone()).filter(scalar),
            Expression::Binary("<<" | ">>", left, _) => self.evident(left),
            Expression::Binary(_, left, right) => {
                // A literal beside a name takes the name's type.
                let known = |e: &Expr| if matches!(e.kind, Expression::Integer(_)) { None } else { self.evident(e) };
                known(left).or_else(|| known(right)).or_else(|| self.evident(left))
            }
            Expression::Unary("-" | "~", inner) => self.evident(inner),
            Expression::Call(callee, _) => match &callee.kind {
                Expression::Name(name) if !self.locals.iter().any(|(n, _)| n == name) && !self.parameters.contains(name) => self.results.get(name).cloned().flatten(),
                // A number type's conversions: `U8.to_u64(b)` is a U64, `U8.truncate(n)` a U8
                // (a gen fn local held them only with the type written out).
                Expression::Field(receiver, method) => match &receiver.kind {
                    Expression::Name(ty) if matches!(ty.as_str(), "U8" | "U16" | "U32" | "U64" | "I64" | "F64")
                        && !self.locals.iter().any(|(n, _)| n == ty) && !self.parameters.contains(ty) => match method.as_str() {
                        "to_u64" => named("U64"),
                        "to_i64" => named("I64"),
                        "to_f64" => named("F64"),
                        "truncate" => named(ty),
                        _ => None,
                    },
                    _ => None,
                },
                _ => None,
            },
            _ => None,
        }
    }
    fn finish(&self) -> Vec<Statement> {
        let error = self.expr(Expression::Call(Box::new(self.name("Err")), vec![self.expr(Expression::Unit)]));
        vec![self.set(self.field("state"), self.expr(Expression::Integer(DONE))), Statement::Return(Some(error))]
    }
    fn save(&self) -> Vec<Statement> {
        self.locals.iter().map(|(name, _)| self.set(self.field(name), self.name(name))).collect()
    }
    fn block(&mut self, statements: Vec<Statement>) -> u64 {
        self.blocks.push(statements);
        self.blocks.len() as u64 - 1
    }

    /// The record's locals: every binding in a list of statements that is cut into states,
    /// found before any state is made, since states are made from the end and a `yield`
    /// saves every local.
    fn collect(&mut self, statements: &[Statement]) -> Result<(), Diagnostic> {
        for statement in statements {
            match statement {
                Statement::Bind { name, ty, value, .. } => {
                    let shown = if ty.is_none() { self.evident(value) } else { None };
                    let Some(ty) = ty.as_ref().or(shown.as_ref()) else { return Err(refuse(&value.span, "a local of a `gen fn` names its type unless its value shows it: `var n: U64 = start * 2`")) };
                    // A generator of numbers is a record of numbers: kept between steps as they are.
                    let generator = matches!(ty, Type::Named(n, args) if args.is_empty() && self.results.contains_key(&format!("#generator {n}")));
                    // The local starts as a generator made by its function (`default`), so that name must
                    // still mean the function here (review: a parameter `count` hid `gen fn count`).
                    if generator && let Type::Named(n, _) = ty && let Some(Some(Type::Named(function, _))) = self.results.get(&format!("#generator {n}"))
                        && (self.parameters.contains(function) || self.locals.iter().any(|(l, _)| l == function)) {
                        return Err(refuse(&value.span, &format!("`{function}` here names a parameter or local, not the generator function that makes this `{n}`: rename it")));
                    }
                    if !scalar(ty) && !generator { return Err(refuse(&value.span, "a local of a `gen fn` is a number or a Bool: the generator keeps it between steps. An owner it needs across steps is a parameter, handed back by `finish`")); }
                    if self.locals.iter().any(|(n, _)| n == name) || self.parameters.contains(name) { return Err(refuse(&value.span, "a `gen fn` names each of its locals once")); }
                    self.locals.push((name.clone(), ty.clone()));
                }
                Statement::If { yes, no, .. } if yields(statement) => { self.collect(yes)?; self.collect(no)?; }
                Statement::While { body, .. } if yields(statement) => self.collect(body)?,
                Statement::Match { arms, .. } if yields(statement) => for arm in arms { self.collect(&arm.body)?; },
                // `for i in a..b:` keeps its counter and its end between steps.
                Statement::For { end: Some(Expr { kind: Expression::Unary("..=", _), .. }), start, .. } if yields(statement) => {
                    return Err(refuse(&start.span, "a `gen fn` counts with `a..b`, not `a..=b`: write `a..b + 1` when `b` is below the largest U64"));
                }
                Statement::For { index: None, binding, end: Some(_), body, start, .. } if yields(statement) => {
                    if self.locals.iter().any(|(n, _)| n == binding) || self.parameters.contains(binding) { return Err(refuse(&start.span, "a `gen fn` names each of its locals once")); }
                    let u64 = Type::Named("U64".into(), Vec::new());
                    self.locals.push((binding.clone(), u64.clone()));
                    self.locals.push((format!("gen#end{}", start.span.start), u64));
                    self.collect(body)?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// The statements as states; returns the state that starts them. `next` is where they
    /// go after, and `exits` where `break` and `continue` of the loop around them go.
    fn sequence(&mut self, statements: Vec<Statement>, next: u64, exits: Option<(u64, u64)>) -> Result<u64, Diagnostic> {
        // Runs of statements with no `yield` stay as they are, one state each; a statement
        // with a `yield` is cut. Built from the end, so each part knows where it goes next.
        let mut parts: Vec<(bool, Vec<Statement>)> = Vec::new();
        for statement in statements {
            let cut = yields(&statement);
            match parts.last_mut() { Some((false, run)) if !cut => run.push(statement), _ => parts.push((cut, vec![statement])) }
        }
        let mut at = next;
        for (cut, run) in parts.into_iter().rev() {
            at = if cut { self.cut(run.into_iter().next().expect("one statement"), at, exits)? } else {
                let mut body = Vec::new();
                for statement in run { body.extend(self.plain(statement, exits, false)?); }
                body.extend(self.jump(at));
                self.block(body)
            };
        }
        Ok(at)
    }

    /// Hand out `value`: save the locals, resume at `next`.
    fn handing(&self, value: Expr, next: u64) -> Vec<Statement> {
        let mut body = vec![Statement::Bind { mutable: false, name: VALUE.into(), ty: Some(self.element.clone()), value, proof: None }];
        body.extend(self.save());
        body.push(self.set(self.field("state"), self.expr(Expression::Integer(next))));
        body.push(Statement::Return(Some(self.expr(Expression::Call(Box::new(self.name("Ok")), vec![self.name(VALUE)])))));
        body
    }
    /// A `match` arm whose body yields: its statements before the first cut, and a leading
    /// `yield`, run in the arm; the rest are states.
    fn arm(&mut self, mut body: Vec<Statement>, bound: &[String], next: u64, exits: Option<(u64, u64)>) -> Result<Vec<Statement>, Diagnostic> {
        let mut out = Vec::new();
        while !body.is_empty() && !yields(&body[0]) {
            let statement = body.remove(0);
            out.extend(self.plain(statement, exits, false)?);
        }
        if body.is_empty() { out.extend(self.jump(next)); return Ok(out); }
        let first = body.remove(0);
        // `if *item % 2 == 0: yield *item`: an `if` whose branches each start with their
        // `yield` (or yield nothing) is decided, and its leading `yield` handed out, here in the
        // arm, where the arm's binding is; what follows each `yield` is a state of its own,
        // as is the rest of the arm, and does not name the binding.
        if let Statement::If { condition, yes, no } = &first
            && [yes, no].iter().all(|branch| branch.first().is_none_or(|s| matches!(s, Statement::Yield { .. })) || !branch.iter().any(yields)) {
            let (condition, yes, no) = (condition.clone(), yes.clone(), no.clone());
            let mut tails = format!("{body:?}");
            for branch in [&yes, &no] {
                let skip = usize::from(matches!(branch.first(), Some(Statement::Yield { .. })));
                tails.push_str(&format!("{:?}", &branch[skip..]));
            }
            if let Some(name) = bound.iter().find(|n| tails.contains(&format!("Name({n:?})"))) {
                return Err(refuse(&self.span, &format!("`{name}` is bound by a `match` arm and is not kept past the arm's first `yield`: copy it into a local with its type first")));
            }
            let after = self.sequence(body, next, exits)?;
            let branch = |this: &mut Self, mut branch: Vec<Statement>| -> Result<Vec<Statement>, Diagnostic> {
                match branch.first() {
                    Some(Statement::Yield { .. }) => {
                        let Statement::Yield { value, .. } = branch.remove(0) else { unreachable!() };
                        let rest = this.sequence(branch, after, exits)?;
                        Ok(this.handing(value, rest))
                    }
                    _ => { let at = this.sequence(branch, after, exits)?; Ok(this.jump(at)) }
                }
            };
            let yes = branch(self, yes)?;
            let no = branch(self, no)?;
            out.push(Statement::If { condition, yes, no });
            return Ok(out);
        }
        // An `if` that yields is a state of its own, but its condition is evaluated here in the
        // arm: one that names what the arm bound (`if *item == wanted: yield i`) is kept in a
        // Bool local first.
        let first = match first {
            Statement::If { condition, yes, no } if bound.iter().any(|n| format!("{condition:?}").contains(&format!("Name({n:?})"))) => {
                let name = format!("gen#cond{}", self.locals.len());
                let ty = Type::Named("Bool".into(), Vec::new());
                self.locals.push((name.clone(), ty.clone()));
                let span = condition.span.clone();
                out.extend(self.plain(Statement::Bind { mutable: true, name: name.clone(), ty: Some(ty), value: condition, proof: None }, exits, false)?);
                Statement::If { condition: Expr { kind: Expression::Name(name), span }, yes, no }
            }
            other => other,
        };
        // What the arm bound lives in the arm only: a use past its first step is refused here,
        // by name (a use the parser cannot tell from a shadowing binding is refused too).
        let rest = format!("{body:?}");
        let later = if matches!(first, Statement::Yield { .. }) { rest } else { format!("{first:?}{rest}") };
        if let Some(name) = bound.iter().find(|n| later.contains(&format!("Name({n:?})"))) {
            return Err(refuse(&self.span, &format!("`{name}` is bound by a `match` arm and is not kept past the arm's first `yield`: copy it into a local with its type first")));
        }
        let after = self.sequence(body, next, exits)?;
        match first {
            Statement::Yield { value, .. } => out.extend(self.handing(value, after)),
            other => { let at = self.cut(other, after, exits)?; out.extend(self.jump(at)); }
        }
        Ok(out)
    }

    fn cut(&mut self, statement: Statement, next: u64, exits: Option<(u64, u64)>) -> Result<u64, Diagnostic> {
        Ok(match statement {
            Statement::Yield { value, .. } => { let body = self.handing(value, next); self.block(body) }
            // Each arm runs where its binding is in scope: its first steps, up to and with a
            // leading `yield`, sit in the arm itself; a binding is not kept past that.
            Statement::Match { value, arms } => {
                let mut kept = Vec::new();
                for mut arm in arms {
                    let names: Vec<String> = arm.binding.iter().chain(&arm.open).cloned().collect();
                    arm.body = self.arm(std::mem::take(&mut arm.body), &names, next, exits)?;
                    kept.push(arm);
                }
                self.block(vec![Statement::Match { value, arms: kept }])
            }
            Statement::If { condition, yes, no } => {
                let yes = self.sequence(yes, next, exits)?;
                let no = self.sequence(no, next, exits)?;
                let test = Statement::If { condition, yes: self.jump(yes), no: self.jump(no) };
                self.block(vec![test])
            }
            Statement::While { condition, invariants, body } => {
                if !invariants.is_empty() { return Err(refuse(&self.span, "a loop of a `gen fn` that yields states no invariants yet")); }
                // The head is made first so the body can go back to it.
                let head = self.block(Vec::new());
                let start = self.sequence(body, head, Some((next, head)))?;
                self.blocks[head as usize] = vec![Statement::If { condition, yes: self.jump(start), no: self.jump(next) }];
                head
            }
            Statement::For { index: None, binding, start, end: Some(end), invariants, body } => {
                if !invariants.is_empty() { return Err(refuse(&self.span, "a loop of a `gen fn` that yields states no invariants yet")); }
                // Counter and end (evaluated once, as `for` does) are locals; `continue` goes to
                // the step that moves the counter on, `break` past the loop.
                let end_name = format!("gen#end{}", start.span.start);
                let head = self.block(Vec::new());
                let step = self.block(Vec::new());
                let first = self.sequence(body, step, Some((next, step)))?;
                let below = self.expr(Expression::Binary("<", Box::new(self.name(&binding)), Box::new(self.name(&end_name))));
                self.blocks[head as usize] = vec![Statement::If { condition: below, yes: self.jump(first), no: self.jump(next) }];
                let mut moved = vec![Statement::Assign { target: self.name(&binding), operation: "+=", value: self.expr(Expression::Integer(1)) }];
                moved.extend(self.jump(head));
                self.blocks[step as usize] = moved;
                let mut begin = vec![self.set(self.name(&binding), start), self.set(self.name(&end_name), end)];
                begin.extend(self.jump(head));
                self.block(begin)
            }
            Statement::For { .. } => return Err(refuse(&self.span, "a `yield` inside a `for` over a container is not supported yet: count with `for i in a..b` or `while`")),
            Statement::Guard { .. } | Statement::Choose { .. } => return Err(refuse(&self.span, "a `yield` inside a `let ... else` or a value `match` is not supported yet: use a `match` statement")),
            _ => return Err(refuse(&self.span, "a `yield` is allowed only in the body of a `gen fn`, in `while` and `if`")),
        })
    }

    /// A statement with no `yield`: as written, except that its bindings at the level being
    /// cut are the record's locals (`scoped`: inside a branch, whose bindings end with it),
    /// and its ways out of the generator or of a loop being cut are jumps. A loop kept as it
    /// is keeps its own `break` and `continue` (`inner`).
    fn plain(&mut self, statement: Statement, exits: Option<(u64, u64)>, scoped: bool) -> Result<Vec<Statement>, Diagnostic> {
        Ok(match statement {
            // One of the record's locals (`collect`): its binding is an assignment.
            Statement::Bind { name, value, .. } if !scoped => vec![self.set(self.name(&name), value)],
            Statement::Return(None) => self.finish(),
            Statement::Return(Some(value)) => return Err(refuse(&value.span, "a `gen fn` ends with `return` and no value; it hands out values with `yield`")),
            // Not inside a loop of its own: these leave a loop that was cut into states.
            Statement::Break => match exits { Some((after, _)) => self.jump(after), None => return Err(refuse(&self.span, "`break` outside a loop")) },
            Statement::Continue => match exits { Some((_, head)) => self.jump(head), None => return Err(refuse(&self.span, "`continue` outside a loop")) },
            Statement::If { condition, yes, no } => {
                let mut kept = Vec::new();
                for s in yes { kept.extend(self.plain(s, exits, true)?); }
                let yes = kept;
                let mut kept = Vec::new();
                for s in no { kept.extend(self.plain(s, exits, true)?); }
                vec![Statement::If { condition, yes, no: kept }]
            }
            Statement::While { condition, invariants, body } => {
                let mut kept = Vec::new();
                for s in body { kept.extend(self.inner(s)?); }
                vec![Statement::While { condition, invariants, body: kept }]
            }
            Statement::Match { value, arms } => {
                let mut changed = Vec::new();
                for mut arm in arms {
                    let mut kept = Vec::new();
                    for s in std::mem::take(&mut arm.body) { kept.extend(self.plain(s, exits, true)?); }
                    arm.body = kept;
                    changed.push(arm);
                }
                vec![Statement::Match { value, arms: changed }]
            }
            // Statements with bodies of their own scope: their `break`, `continue` and `return`
            // are translated like an `if` branch's (an adversarial review found them copied
            // through, aimed at the generator's own loop).
            Statement::Guard { value, variant, binding, ty, mut failure } => {
                let mut kept = Vec::new();
                for s in std::mem::take(&mut failure.body) { kept.extend(self.plain(s, exits, true)?); }
                failure.body = kept;
                vec![Statement::Guard { value, variant, binding, ty, failure }]
            }
            Statement::Using { mutable, name, ty, value, body } => {
                let mut kept = Vec::new();
                for s in body { kept.extend(self.plain(s, exits, true)?); }
                vec![Statement::Using { mutable, name, ty, value, body: kept }]
            }
            Statement::Block(body) => {
                let mut kept = Vec::new();
                for s in body { kept.extend(self.plain(s, exits, true)?); }
                vec![Statement::Block(kept)]
            }
            Statement::Choose { mutable, name, ty, inner } => {
                let mut translated = self.plain(*inner, exits, true)?;
                let [inner] = <[Statement; 1]>::try_from(std::mem::take(&mut translated)).map_err(|_| refuse(&self.span, "a value `if` or `match` of a `gen fn` is one statement"))?;
                // A binding the value names is the record's local when it is at the cut level.
                vec![Statement::Choose { mutable, name, ty, inner: Box::new(inner) }]
            }
            // A `for` is a loop of its own: its `break` and `continue` stay.
            Statement::For { .. } => self.inner(statement)?,
            other => vec![other],
        })
    }

    /// A statement inside a loop that is kept as it is: its own `break` and `continue` stay;
    /// a `return` still ends the generator.
    fn inner(&mut self, statement: Statement) -> Result<Vec<Statement>, Diagnostic> {
        Ok(match statement {
            Statement::Return(None) => self.finish(),
            Statement::Return(Some(value)) => return Err(refuse(&value.span, "a `gen fn` ends with `return` and no value; it hands out values with `yield`")),
            Statement::If { condition, yes, no } => {
                let mut y = Vec::new(); for s in yes { y.extend(self.inner(s)?); }
                let mut n = Vec::new(); for s in no { n.extend(self.inner(s)?); }
                vec![Statement::If { condition, yes: y, no: n }]
            }
            Statement::While { condition, invariants, body } => {
                let mut kept = Vec::new(); for s in body { kept.extend(self.inner(s)?); }
                vec![Statement::While { condition, invariants, body: kept }]
            }
            // The generator's `return` is a real `return` (`finish`), not a jump, so it is the
            // same at any depth: every body is translated alike, and the statement's own
            // `break` and `continue` are untouched.
            Statement::Match { value, arms } => {
                let mut changed = Vec::new();
                for mut arm in arms {
                    let mut kept = Vec::new(); for s in std::mem::take(&mut arm.body) { kept.extend(self.inner(s)?); }
                    arm.body = kept;
                    changed.push(arm);
                }
                vec![Statement::Match { value, arms: changed }]
            }
            Statement::For { index, binding, start, end, invariants, body } => {
                let mut kept = Vec::new(); for s in body { kept.extend(self.inner(s)?); }
                vec![Statement::For { index, binding, start, end, invariants, body: kept }]
            }
            Statement::Guard { value, variant, binding, ty, mut failure } => {
                let mut kept = Vec::new(); for s in std::mem::take(&mut failure.body) { kept.extend(self.inner(s)?); }
                failure.body = kept;
                vec![Statement::Guard { value, variant, binding, ty, failure }]
            }
            Statement::Using { mutable, name, ty, value, body } => {
                let mut kept = Vec::new(); for s in body { kept.extend(self.inner(s)?); }
                vec![Statement::Using { mutable, name, ty, value, body: kept }]
            }
            Statement::Block(body) => {
                let mut kept = Vec::new(); for s in body { kept.extend(self.inner(s)?); }
                vec![Statement::Block(kept)]
            }
            Statement::Choose { mutable, name, ty, inner } => {
                let mut translated = self.inner(*inner)?;
                let [inner] = <[Statement; 1]>::try_from(std::mem::take(&mut translated)).map_err(|_| refuse(&self.span, "a value `if` or `match` of a `gen fn` is one statement"))?;
                vec![Statement::Choose { mutable, name, ty, inner: Box::new(inner) }]
            }
            other => { if returns(&other) { return Err(refuse(&self.span, "a `return` inside this statement of a `gen fn` is not supported yet")); } vec![other] }
        })
    }
}

fn returns(statement: &Statement) -> bool {
    let any = |body: &[Statement]| body.iter().any(returns);
    match statement {
        Statement::Return(_) => true,
        Statement::If { yes, no, .. } => any(yes) || any(no),
        Statement::While { body, .. } | Statement::For { body, .. } | Statement::Using { body, .. } | Statement::Block(body) => any(body),
        Statement::Match { arms, .. } => arms.iter().any(|arm| any(&arm.body)),
        Statement::Guard { failure, .. } => any(&failure.body),
        Statement::Choose { inner, .. } => returns(inner),
        _ => false,
    }
}

/// The record's field names in order: parameters, locals, then the state.
fn fields_names(parameters: &[(String, Type)], locals: &[(String, Type)]) -> Vec<String> {
    parameters.iter().chain(locals).map(|(n, _)| n.clone()).chain(["state".to_string()]).collect()
}

/// `for item in container:` with a `yield` in it, as a loop the generator already keeps
/// between steps: an index local, `match container.at(index)`, `item` as the arm's binding
/// (so copied into a typed local before a `yield` that it must outlive), and `break` on
/// `Err`. The index moves on before the body, so `continue` takes the next element. The
/// container is a name, read again at each step.
/// `generators`: the locals that hold a generator of numbers (`var numbers = fibonacci(n)`),
/// stepped by their `next_mut` rather than walked by position.
fn containers(statements: Vec<Statement>, span: &Range<usize>, generators: &std::collections::BTreeSet<String>) -> Result<Vec<Statement>, Diagnostic> {
    let mut out = Vec::with_capacity(statements.len());
    for statement in statements {
        let statement = match statement {
            Statement::For { index, binding, start, end: None, invariants, body } if contains_yield(&body) => {
                if !invariants.is_empty() { return Err(refuse(span, "a loop of a `gen fn` that yields states no invariants yet")); }
                // Read again at each step: a name, a field of one (`self.items`), or `*name`.
                fn place(e: &Expr) -> bool { match &e.kind { Expression::Name(_) => true, Expression::Field(inner, _) => place(inner), Expression::Unary("*", inner) => matches!(inner.kind, Expression::Name(_)), _ => false } }
                if !place(&start) { return Err(refuse(&start.span, "a `for` with a `yield` in a `gen fn` walks a container by its name or a field of one: bind it first")); }
                let at = format!("gen#at{}", start.span.start);
                let e = |kind: Expression| Expr { kind, span: start.span.clone() };
                let u64 = Type::Named("U64".into(), Vec::new());
                // A generator held in a local is stepped: `name.next_mut(&mut at)`, `at` its cursor.
                let stepped = matches!(&start.kind, Expression::Name(n) if generators.contains(n));
                if stepped && index.is_some() { return Err(refuse(&start.span, "`for index, item in` walks a container; a generator gives items only")); }
                let call = if stepped {
                    e(Expression::Call(Box::new(e(Expression::Field(Box::new(start.clone()), "next_mut".into()))), vec![e(Expression::Unary("&mut", Box::new(e(Expression::Name(at.clone())))))]))
                } else { e(Expression::Call(Box::new(e(Expression::Field(Box::new(start.clone()), "at".into()))), vec![e(Expression::Name(at.clone()))])) };
                // `for index, item in`: the index is a local the generator keeps, the position
                // before it moves on.
                let mut found = index.map(|index| Statement::Bind { mutable: false, name: index, ty: Some(u64.clone()), value: e(Expression::Name(at.clone())), proof: None }).into_iter().collect::<Vec<_>>();
                if !stepped { found.push(Statement::Assign { target: e(Expression::Name(at.clone())), operation: "+=", value: e(Expression::Integer(1)) }); }
                found.extend(containers(body, span, generators)?);
                let arms = vec![
                    MatchArm { variant: "Ok".into(), binding: Some(binding), body: found, nested: None, open: Vec::new() },
                    MatchArm { variant: "Err".into(), binding: Some(format!("gen#code{}", start.span.start)), body: vec![Statement::Break], nested: None, open: Vec::new() },
                ];
                out.push(Statement::Bind { mutable: true, name: at, ty: Some(u64), value: e(Expression::Integer(0)), proof: None });
                Statement::While { condition: e(Expression::Bool(true)), invariants: Vec::new(), body: vec![Statement::Match { value: call, arms }] }
            }
            Statement::If { condition, yes, no } => Statement::If { condition, yes: containers(yes, span, generators)?, no: containers(no, span, generators)? },
            Statement::While { condition, invariants, body } => Statement::While { condition, invariants, body: containers(body, span, generators)? },
            Statement::For { index, binding, start, end: end @ Some(_), invariants, body } => Statement::For { index, binding, start, end, invariants, body: containers(body, span, generators)? },
            Statement::Match { value, arms } => Statement::Match { value, arms: arms.into_iter().map(|mut arm| { arm.body = containers(std::mem::take(&mut arm.body), span, generators)?; Ok(arm) }).collect::<Result<_, Diagnostic>>()? },
            other => other,
        };
        out.push(statement);
    }
    Ok(out)
}

/// The declarations a `gen fn` becomes: its record, the function that builds one, and the
/// record's `next_mut` (as an `impl` block, so it is a method like any other).
pub(super) fn desugar(mut f: Function, span: Range<usize>, record: String, results: &std::collections::BTreeMap<String, Option<Type>>) -> Result<(Declaration, Function, Declaration), Diagnostic> {
    if !f.requires.is_empty() || !f.ensures.is_empty() { return Err(refuse(&span, "a `gen fn` states no contract yet")); }
    // A generator is stepped many times; the brand of an opening is given once, to one call.
    if f.bounds.iter().flatten().any(|b| b == "Fresh") { return Err(refuse(&span, "a `gen fn` makes no new numbering: a `Fresh` type parameter belongs to a function opened once by `with`")); }
    // The record keeps the arguments between steps: numbers, Bools, and shared references or
    // slices. With one, the record is a view that keeps the loan while the generator is in use;
    // `next_mut` reads its parameters and never writes them, as a `&mut` to a view requires
    // (docs/design-references-to-views.md, step 2). No capability or owner.
    if f.parameters.iter().any(|(_, ty)| matches!(ty, Type::Borrow { mutable: true, .. })) { return Err(refuse(&span, "a parameter of a `gen fn` is a number, a Bool, a shared reference (`&T`, `&[T]`) or an owner")); }
    // Owners may be handed in (`gen fn serve(socket: Socket, buffer: Vec<U8>)`): the generator
    // holds them, its steps borrow them, and `finish` hands them back, one as itself and several
    // as an anonymous record of them (docs/wasm-generators.md).
    let owned: Vec<(String, Type)> = f.parameters.iter().filter(|(_, ty)| !scalar(ty) && !matches!(ty, Type::Borrow { .. })).cloned().collect();
    // `-> Iter<U64>` (the book's spelling) is `-> U64`: what each step yields.
    if let Type::Named(name, arguments) = &f.result && name == "Iter" && arguments.len() == 1 { f.result = arguments[0].clone(); }
    // What it yields: a number, a Bool, or a record or other value without a reference; the
    // generated `next_mut` is checked like any code, so a record holding an owner is moved out.
    // A reference would point into the generator's own record, which the next step changes.
    if matches!(f.result, Type::Borrow { .. } | Type::Slice(_)) { return Err(refuse(&span, "a `gen fn` yields values, not references: a reference would point into the generator, which the next step changes")); }
    let element = f.result.clone();
    let mut f = f;
    // Locals that hold a generator of numbers: bound from a call of its function, or typed so.
    let generator_of = |ty: &Type| matches!(ty, Type::Named(n, a) if a.is_empty() && results.contains_key(&format!("#generator {n}")));
    let mut generators = std::collections::BTreeSet::new();
    fn bound(statements: &[Statement], out: &mut Vec<(String, Option<Type>, Expr)>) {
        for statement in statements {
            match statement {
                Statement::Bind { name, ty, value, .. } => out.push((name.clone(), ty.clone(), value.clone())),
                Statement::If { yes, no, .. } => { bound(yes, out); bound(no, out); }
                Statement::While { body, .. } | Statement::For { body, .. } | Statement::Using { body, .. } | Statement::Block(body) => bound(body, out),
                Statement::Match { arms, .. } => for arm in arms { bound(&arm.body, out); },
                _ => {}
            }
        }
    }
    let mut bindings = Vec::new();
    bound(&f.body, &mut bindings);
    for (name, ty, value) in bindings {
        let made = match (&ty, &value.kind) {
            (Some(ty), _) => generator_of(ty),
            (None, Expression::Call(callee, _)) => matches!(&callee.kind, Expression::Name(fun) if results.get(fun).cloned().flatten().is_some_and(|t| generator_of(&t))),
            _ => false,
        };
        if made { generators.insert(name); }
    }
    scoped(&f.body, &generators)?;
    f.body = containers(std::mem::take(&mut f.body), &span, &generators)?;
    let mut flat = Flattener { span: span.clone(), blocks: Vec::new(), locals: Vec::new(), parameters: f.parameters.iter().map(|(n, _)| n.clone()).collect(), parameter_types: f.parameters.clone(), results: results.clone(), element: f.result.clone() };
    // State 0 is the finished one's stand-in: every path that ends goes there.
    let finished = flat.finish();
    let done = flat.block(finished);
    flat.collect(&f.body)?;
    let start = flat.sequence(f.body, done, None)?;
    // next_mut: load, then go round the states.
    let mut body = vec![Statement::Bind { mutable: true, name: STATE.into(), ty: Some(Type::Named("U64".into(), Vec::new())), value: flat.field("state"), proof: None }];
    for (name, ty) in &f.parameters {
        // An owner stays in the record; a step borrows it.
        let (ty, value) = if owned.iter().any(|(n, _)| n == name) {
            (Type::Borrow { mutable: false, inner: Box::new(ty.clone()) }, flat.expr(Expression::Unary("&", Box::new(flat.field(name)))))
        } else { (ty.clone(), flat.field(name)) };
        body.push(Statement::Bind { mutable: false, name: name.clone(), ty: Some(ty), value, proof: None });
    }
    for (name, ty) in &flat.locals {
        body.push(Statement::Bind { mutable: true, name: name.clone(), ty: Some(ty.clone()), value: flat.field(name), proof: None });
    }
    let mut turn = Vec::new();
    for (state, block) in flat.blocks.iter().enumerate() {
        let test = flat.expr(Expression::Binary("==", Box::new(flat.name(STATE)), Box::new(flat.expr(Expression::Integer(state as u64)))));
        turn.push(Statement::If { condition: test, yes: block.clone(), no: Vec::new() });
    }
    turn.extend(flat.finish());
    body.push(Statement::While { condition: flat.expr(Expression::Bool(true)), invariants: Vec::new(), body: turn });
    body.extend(flat.finish());
    let named = |n: &str| Type::Named(n.to_owned(), Vec::new());
    // A generic `gen fn` makes a generic record: `Neighbors<G>` for `gen fn neighbors<G: Brand>`.
    let this = Type::Named(record.clone(), f.generics.iter().map(|g| named(g)).collect());
    let next = Function {
        name: "next_mut".into(), generics: Vec::new(), any: Vec::new(), bounds: Vec::new(), required: false, module: f.module, public: f.public, pure: false, nopanic: None, span: f.span.clone(),
        suspends: false, nosuspend: Vec::new(), quiet: Vec::new(), defaults: vec![None, None],
        parameters: vec![("self".into(), Type::Borrow { mutable: true, inner: Box::new(this.clone()) }), ("cursor".into(), Type::Borrow { mutable: true, inner: Box::new(named("U64")) })],
        result: Type::Named("Result".into(), vec![element, Type::Unit]), body, requires: Vec::new(), ensures: Vec::new(), borrows: Vec::new(),
    };
    // The record: arguments, locals, and where it stopped.
    let mut fields: Vec<(String, Type)> = f.parameters.clone();
    fields.extend(flat.locals.iter().cloned());
    fields.push(("state".into(), named("U64")));
    let mut values: Vec<Expr> = f.parameters.iter().map(|(n, _)| if owned.iter().any(|(o, _)| o == n) { flat.expr(Expression::Unary("move", Box::new(flat.name(n)))) } else { flat.name(n) }).collect();
    values.extend(flat.locals.iter().map(|(_, ty)| default(ty, &span, results)));
    values.push(flat.expr(Expression::Integer(start)));
    let build = flat.expr(Expression::Call(Box::new(flat.name(&record)), values));
    let field_names = fields_names(&f.parameters, &flat.locals);
    let (generics, any, bounds) = (f.generics.clone(), f.any.clone(), f.bounds.clone());
    let constructor = Function { body: vec![Statement::Return(Some(build))], result: this.clone(), ..f };
    let declaration = Declaration::Record { name: record.clone(), generics: generics.clone(), fields, invariants: Vec::new(), conditions: Vec::new(), sealed: true, module: constructor.module, span: 0..0 };
    // `finish(self)`: the generator, done with or not, hands back the owner it was given.
    let mut methods = vec![(next.clone(), span.clone())];
    if !owned.is_empty() {
        let names: Vec<String> = field_names.into_iter().map(|n| if owned.iter().any(|(o, _)| *o == n) { n } else { format!("gen#{n}") }).collect();
        let moved = |name: &str| flat.expr(Expression::Unary("move", Box::new(flat.name(name))));
        let (result, value) = match owned.as_slice() {
            [(owner, ty)] => (ty.clone(), moved(owner)),
            several => (Type::Record(several.to_vec()), flat.expr(Expression::Record(several.iter().map(|(n, _)| (n.clone(), moved(n))).collect()))),
        };
        let finish = Function {
            name: "finish".into(), parameters: vec![("self".into(), this.clone())], result,
            body: vec![
                Statement::Open { record: record.clone(), names, value: flat.expr(Expression::Unary("move", Box::new(flat.name("self")))) },
                Statement::Return(Some(value)),
            ],
            ..next
        };
        methods.push((finish, span.clone()));
    }
    let methods = Declaration::Impl { generics, any, bounds, implements: None, trait_arguments: Vec::new(), target: this, methods, span };
    Ok((declaration, constructor, methods))
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_generator_is_named_after_its_function() {
        assert_eq!(super::type_name("squares"), "Squares");
        assert_eq!(super::type_name("count_up_to"), "CountUpTo");
    }
}

/// The body's `let` and `var` locals as plain code has them, checked before they become the
/// record's fields (live from the first step, so the checker no longer sees their scope; review
/// of nested generators: a local read after its block ended read the record's start value, and a
/// generator so read was a placeholder never asked for; a `let` could be assigned). A `gen fn`
/// names each local once, so a use is in scope when it follows the binding within its block.
/// A `let` is not assigned, and a generator stepped by `for` is a `var`, as in plain code.
fn scoped(body: &[Statement], generators: &std::collections::BTreeSet<String>) -> Result<(), Diagnostic> {
    use std::collections::{BTreeMap, BTreeSet};
    fn binds(statements: &[Statement], out: &mut BTreeMap<String, (usize, bool)>) {
        for statement in statements {
            if let Statement::Bind { name, mutable, .. } = statement { let entry = out.entry(name.clone()).or_insert((0, *mutable)); entry.0 += 1; }
            match statement {
                Statement::If { yes, no, .. } => { binds(yes, out); binds(no, out); }
                Statement::While { body, .. } | Statement::For { body, .. } | Statement::Using { body, .. } | Statement::Block(body) => binds(body, out),
                Statement::Match { arms, .. } => for arm in arms { binds(&arm.body, out); },
                Statement::Guard { failure, .. } => binds(&failure.body, out),
                Statement::Choose { inner, .. } => binds(std::slice::from_ref(&**inner), out),
                _ => {}
            }
        }
    }
    let mut all = BTreeMap::new();
    binds(body, &mut all);
    // Names bound once: the ones whose scope is a single block.
    let tracked: BTreeMap<String, bool> = all.into_iter().filter(|(_, (count, _))| *count == 1).map(|(name, (_, mutable))| (name, mutable)).collect();
    struct Walk<'a> { tracked: &'a BTreeMap<String, bool>, generators: &'a BTreeSet<String> }
    impl Walk<'_> {
        fn uses(&self, e: &Expr, scope: &BTreeSet<String>) -> Result<(), Diagnostic> {
            let mut outside = None;
            e.clone().visit_mut(&mut |e| {
                if let Expression::Name(name) = &e.kind && self.tracked.contains_key(name) && !scope.contains(name) && outside.is_none() { outside = Some((name.clone(), e.span.clone())); }
                false
            });
            match outside {
                Some((name, span)) => Err(refuse(&span, &format!("`{name}` is used outside the block that binds it, or before it"))),
                None => Ok(()),
            }
        }
        fn statements(&self, statements: &[Statement], outer: &BTreeSet<String>) -> Result<(), Diagnostic> {
            let mut scope = outer.clone();
            for statement in statements {
                match statement {
                    Statement::If { condition, yes, no } => { self.uses(condition, &scope)?; self.statements(yes, &scope)?; self.statements(no, &scope)?; }
                    Statement::While { condition, body, .. } => { self.uses(condition, &scope)?; self.statements(body, &scope)?; }
                    Statement::For { start, end, body, .. } => {
                        self.uses(start, &scope)?;
                        if let Some(end) = end { self.uses(end, &scope)?; }
                        if let Expression::Name(name) = &start.kind && self.generators.contains(name) && self.tracked.get(name) == Some(&false) {
                            return Err(refuse(&start.span, &format!("`for` steps the generator `{name}`, which changes it: bind it with `var {name}`")));
                        }
                        self.statements(body, &scope)?;
                    }
                    Statement::Match { value, arms } => { self.uses(value, &scope)?; for arm in arms { self.statements(&arm.body, &scope)?; } }
                    Statement::Using { value, body, .. } => { self.uses(value, &scope)?; self.statements(body, &scope)?; }
                    Statement::Block(body) => self.statements(body, &scope)?,
                    Statement::Guard { value, failure, .. } => { self.uses(value, &scope)?; self.statements(&failure.body, &scope)?; }
                    Statement::Choose { inner, .. } => self.statements(std::slice::from_ref(&**inner), &scope)?,
                    other => {
                        if let Statement::Assign { target, .. } = other {
                            let mut root = target;
                            while let Expression::Field(inner, _) | Expression::Index(inner, _) = &root.kind { root = inner; }
                            if let Expression::Name(name) = &root.kind && self.tracked.get(name) == Some(&false) {
                                return Err(refuse(&target.span, &format!("`{name}` is bound by `let` and is not assigned: write `var {name}`")));
                            }
                        }
                        let mut failure = Ok(());
                        other.clone().visit_exprs_mut(&mut |e| {
                            if failure.is_ok() { failure = self.uses(e, &scope); }
                            true
                        });
                        failure?;
                    }
                }
                if let Statement::Bind { name, .. } = statement { scope.insert(name.clone()); }
            }
            Ok(())
        }
    }
    Walk { tracked: &tracked, generators }.statements(body, &BTreeSet::new())
}

/// Whether a `for` over a call of this `gen fn` may run its body in place, each `yield` the
/// loop's body (check.rs `inlined_generator`): not generic, no contract, and parameters that are
/// numbers, Bools or shared references, so nothing is held that `finish` would hand back.
pub(super) fn inlinable(f: &Function) -> bool {
    f.generics.is_empty() && f.requires.is_empty() && f.ensures.is_empty()
        && f.parameters.iter().all(|(_, ty)| scalar(ty) || matches!(ty, Type::Borrow { mutable: false, .. }) || matches!(ty, Type::Named(n, a) if n == "Text" && a.is_empty()))
}
