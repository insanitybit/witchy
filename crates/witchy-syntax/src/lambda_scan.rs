//! Free-variable / capture analysis over the AST.
//!
//! A lambda's *captures* are the variables it reads from an enclosing scope; its
//! *outer assignments* are the variables it writes that it does not bind
//! internally. Both are pure functions of the syntax tree, so they live here in
//! `witchy-syntax` and are shared by every consumer: the lowering pass uses
//! `scan_lambda`/`captures`/`assigns_outer` to emit closure environments, and the
//! type checker uses `lambda_outer_assigns` to reject by-value writes uniformly
//! (identically to what lowering would detect) rather than backend-specifically.

use crate::ast::*;
// foldhash: compiler-internal keys only — see witchy-types/src/typeck.rs.
use foldhash::HashSet;

/// The free reads and writes gathered while walking a lambda body.
#[derive(Default)]
pub struct LambdaScan {
    captures: HashSet<String>,
    assigns_outer: HashSet<String>,
}

impl LambdaScan {
    /// Variables read from the enclosing scope (the closure's captures), sorted
    /// for a deterministic capture-slot order.
    pub fn captures(&self) -> Vec<String> {
        let mut free: Vec<String> = self.captures.iter().cloned().collect();
        free.sort();
        free
    }

    /// Variables assigned that are not bound within the lambda — i.e. writes to
    /// an outer binding. By-value capture cannot propagate these back out.
    pub fn assigns_outer(&self) -> Vec<String> {
        let mut a: Vec<String> = self.assigns_outer.iter().cloned().collect();
        a.sort();
        a
    }
}

struct ScanState {
    scan: LambdaScan,
    scopes: Vec<HashSet<String>>,
}

impl ScanState {
    fn new(params: &[Param]) -> Self {
        let mut root = HashSet::default();
        root.extend(params.iter().map(|param| param.name.clone()));
        Self { scan: LambdaScan::default(), scopes: vec![root] }
    }

    fn is_bound(&self, name: &str) -> bool {
        self.scopes.iter().rev().any(|scope| scope.contains(name))
    }

    fn read(&mut self, name: &str) {
        if !self.is_bound(name) {
            self.scan.captures.insert(name.to_string());
        }
    }

    fn assign(&mut self, name: &str) {
        if !self.is_bound(name) {
            self.scan.assigns_outer.insert(name.to_string());
        }
    }

    fn bind(&mut self, name: String) {
        self.scopes
            .last_mut()
            .expect("lambda scan always has a lexical scope")
            .insert(name);
    }

    fn push_scope(&mut self, bindings: impl IntoIterator<Item = String>) {
        let mut scope = HashSet::default();
        scope.extend(bindings);
        self.scopes.push(scope);
    }

    fn pop_scope(&mut self) {
        self.scopes.pop().expect("nested lambda scan scope");
    }
}

/// Names a lambda assigns but does not bind internally — i.e. writes to a
/// captured/outer variable. By-value capture cannot propagate these out, so every
/// backend rejects them; the type checker calls this so the rejection is uniform
/// (and identical to what lowering would detect) rather than backend-specific.
pub fn lambda_outer_assigns(params: &[Param], body: &Block) -> Vec<String> {
    scan_lambda(params, body).assigns_outer()
}

/// Scan a lambda for captures and outer assignments. Bindings follow lexical
/// source order: a `let` becomes visible only after its initializer, and a
/// binder in one nested block, match arm, loop, or lambda never leaks into a
/// sibling scope. Nested lambdas are still walked because a free value they use
/// must be available when the outer closure constructs them.
pub fn scan_lambda(params: &[Param], body: &Block) -> LambdaScan {
    let mut state = ScanState::new(params);
    fv_block(body, &mut state);
    state.scan
}

fn fv_nested_block(
    block: &Block,
    bindings: impl IntoIterator<Item = String>,
    s: &mut ScanState,
) {
    s.push_scope(bindings);
    fv_block(block, s);
    s.pop_scope();
}

fn fv_block(block: &Block, s: &mut ScanState) {
    for stmt in &block.stmts {
        match stmt {
            Stmt::Let { name, value, .. } => {
                fv_expr(value, s);
                s.bind(name.clone());
            }
            Stmt::Assign { name, value } => {
                s.assign(name);
                fv_expr(value, s);
            }
            Stmt::LetPattern { pattern, value } => {
                fv_expr(value, s);
                let mut names = Vec::new();
                crate::ast::pattern_binds(pattern, &mut names);
                for n in names {
                    s.bind(n);
                }
            }
            Stmt::Return(Some(e)) | Stmt::Expr(e) | Stmt::Yield(e) => fv_expr(e, s),
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
    }
}

fn fv_expr(e: &Expr, s: &mut ScanState) {
    match e {
        // The surface-sugar nodes (`Range`, `Index`, `MethodCall`, `Record`,
        // `LabeledCall`, `WhileLet`) are only lowered by `lower_sugar_module`,
        // which the COMPILED backend runs up front — but the TYPE CHECKER calls
        // this scan (via `lambda_outer_assigns`) on the still-sugared tree (it
        // desugars subscripts/ranges transiently for inference, never mutating the
        // AST). Free-variable analysis is purely structural, so scan the operands
        // of each rather than assume they are gone (a lambda body may legitimately
        // subscript a captured list, `fn(i): xs[i]`, or method-call it). A
        // place-assignment target (`xs[i] = v`) never appears here — the parser
        // desugars it to `xs.set_at(i, v)` (a `Stmt::Assign`) at parse time —
        // so an `Index`/`Field` here is always a READ.
        Expr::Range { lo, hi, .. } => {
            fv_expr(lo, s);
            fv_expr(hi, s);
        }
        Expr::Index { base, index } => {
            fv_expr(base, s);
            fv_expr(index, s);
        }
        Expr::MethodCall { receiver, args, .. } => {
            fv_expr(receiver, s);
            for a in args {
                fv_expr(a, s);
            }
        }
        Expr::LabeledMethodCall { receiver, args, .. } => {
            fv_expr(receiver, s);
            for (_, a) in args {
                fv_expr(a, s);
            }
        }
        Expr::Record { fields, spread, .. } => {
            for (_, v) in fields {
                fv_expr(v, s);
            }
            if let Some(sp) = spread {
                fv_expr(sp, s);
            }
        }
        // A labeled call's name may be a captured function-valued local (mirrors
        // the `Call` arm); recurse over its argument values.
        Expr::LabeledCall { name, args } => {
            s.read(name);
            for (_, a) in args {
                fv_expr(a, s);
            }
        }
        Expr::WhileLet { pattern, scrutinee, body } => {
            fv_expr(scrutinee, s);
            let mut pv = Vec::new();
            collect_pattern_vars(pattern, &mut pv);
            fv_nested_block(body, pv, s);
        }
        Expr::Var(n) => {
            s.read(n);
        }
        Expr::Int(_)
        | Expr::Duration(_)
        | Expr::Float(_)
        | Expr::Str(_)
        | Expr::Bool(_)
        | Expr::TaggedLit { .. } => {}
        // A `Call` name is a function/builtin (or a closure local, caught at WASM
        // validation), never an outer value capture — only its args matter here.
        Expr::List(xs) | Expr::Tuple(xs) => {
            for x in xs {
                fv_expr(x, s);
            }
        }
        // The callee name matters: it may be a captured function-valued local
        // (which must be pulled into the closure), not only a top-level
        // function. Non-local names are filtered out where captures are built.
        Expr::Call { name, args } => {
            s.read(name);
            for a in args {
                fv_expr(a, s);
            }
        }
        Expr::Ctor { args, .. } | Expr::AnonCtor { args, .. } => {
            for a in args {
                fv_expr(a, s);
            }
        }
        Expr::Apply { func, args } => {
            fv_expr(func, s);
            for a in args {
                fv_expr(a, s);
            }
        }
        Expr::Unary { expr, .. }
        | Expr::Try(expr)
        | Expr::As { expr, .. }
        | Expr::ExistentialPack { expr, .. }
        | Expr::ExistentialUpcast { expr, .. } => fv_expr(expr, s),
        Expr::ExistentialCall { receiver, args, .. } => {
            fv_expr(receiver, s);
            for arg in args { fv_expr(arg, s); }
        }
        Expr::Field { base, .. } => fv_expr(base, s),
        Expr::RecordUpdate { name: _, base, fields } => {
            fv_expr(base, s);
            for (_, v) in fields {
                fv_expr(v, s);
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            fv_expr(lhs, s);
            fv_expr(rhs, s);
        }
        Expr::If {
            cond,
            then_block,
            else_block,
        } => {
            fv_expr(cond, s);
            fv_nested_block(then_block, std::iter::empty(), s);
            if let Some(b) = else_block {
                fv_nested_block(b, std::iter::empty(), s);
            }
        }
        Expr::Match { scrutinee, arms } => {
            fv_expr(scrutinee, s);
            for arm in arms {
                let mut pv = Vec::new();
                collect_pattern_vars(&arm.pattern, &mut pv);
                s.push_scope(pv);
                if let Some(g) = &arm.guard {
                    fv_expr(g, s);
                }
                fv_expr(&arm.body, s);
                s.pop_scope();
            }
        }
        Expr::Block(b) => fv_nested_block(b, std::iter::empty(), s),
        Expr::While { cond, body } => {
            fv_expr(cond, s);
            fv_nested_block(body, std::iter::empty(), s);
        }
        Expr::For { var, iter, body } => {
            fv_expr(iter, s);
            fv_nested_block(body, std::iter::once(var.clone()), s);
        }
        Expr::Lambda { params, body, .. } => {
            fv_nested_block(body, params.iter().map(|param| param.name.clone()), s);
        }
    }
}

/// Collect the variable names a pattern binds (recursively through ctor/tuple/
/// list sub-patterns and a list-rest binding) into any `Extend<String>` sink —
/// a `Vec` to keep binding order, a `HashSet` for set membership.
pub fn collect_pattern_vars<S: Extend<String>>(pat: &Pattern, out: &mut S) {
    match pat {
        Pattern::Var(name) => out.extend([name.clone()]),
        Pattern::Ctor { args, .. } | Pattern::AnonCtor { args, .. } | Pattern::Tuple(args) => {
            for sub in args {
                collect_pattern_vars(sub, out);
            }
        }
        Pattern::List { elems, rest } => {
            for sub in elems {
                collect_pattern_vars(sub, out);
            }
            if let Some(Some(name)) = rest {
                out.extend([name.clone()]);
            }
        }
        // (RFC-0052) Every or-pattern alternative binds the SAME names, so the
        // first alternative's bindings are the complete set (the checker enforces
        // consistency). Walking just the first avoids double-counting.
        Pattern::Or(alts) => {
            if let Some(first) = alts.first() {
                collect_pattern_vars(first, out);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outer_lambda(source: &str) -> (Vec<Param>, Block) {
        let module = crate::parser::parse_module(source).expect("lambda scan fixture parses");
        let function = match module.items.into_iter().next().expect("one function") {
            Item::Function(function) => function,
            other => panic!("expected function, found {other:?}"),
        };
        let expression = match function.body.stmts.into_iter().last().expect("function tail") {
            Stmt::Expr(expression) => expression,
            other => panic!("expected expression tail, found {other:?}"),
        };
        let Expr::Lambda { params, body, .. } = expression else {
            panic!("expected lambda tail, found {expression:?}");
        };
        (params, body)
    }

    fn scan(source: &str) -> LambdaScan {
        let (params, body) = outer_lambda(source);
        scan_lambda(&params, &body)
    }

    #[test]
    fn read_before_later_shadow_remains_an_authority_capture() {
        let scanned = scan(
            "fn factory(console: Console) -> fn() -> Nil:\n    fn():\n        console.print(\"effect\")\n        let console = 0\n",
        );
        assert_eq!(scanned.captures(), ["console"]);
    }

    #[test]
    fn ordinary_reads_and_assignments_respect_binding_order() {
        let scanned = scan(
            "fn factory(value: Int) -> fn() -> Int:\n    fn():\n        value = 1\n        let before = value\n        let value = 2\n        before\n",
        );
        assert_eq!(scanned.captures(), ["value"]);
        assert_eq!(scanned.assigns_outer(), ["value"]);

        let local = scan(
            "fn factory() -> fn() -> Int:\n    fn():\n        var value = 1\n        value = 2\n        value\n",
        );
        assert!(local.captures().is_empty(), "local read is not a capture");
        assert!(
            local.assigns_outer().is_empty(),
            "assignment after the local binding is internal"
        );
    }

    #[test]
    fn sibling_and_nested_lambda_binders_do_not_leak() {
        let inner_shadow = scan(
            "fn factory(value: Int) -> fn() -> Int:\n    fn():\n        if true:\n            let value = 1\n            value\n        0\n",
        );
        assert!(
            inner_shadow.captures().is_empty(),
            "reads of a true inner-scope shadow are local"
        );

        let sibling = scan(
            "fn factory(value: Int) -> fn() -> Int:\n    fn():\n        if true:\n            let value = 1\n            value\n        value\n",
        );
        assert_eq!(sibling.captures(), ["value"]);

        let nested = scan(
            "fn factory(console: Console) -> fn() -> Console:\n    fn():\n        let inner = fn(console: Console): console\n        console\n",
        );
        assert_eq!(
            nested.captures(),
            ["console"],
            "a nested parameter shadows only inside its own lambda"
        );
    }

    #[test]
    fn nested_lambda_free_reads_propagate_to_the_outer_environment() {
        let scanned = scan(
            "fn factory(console: Console) -> fn() -> fn() -> Console:\n    fn():\n        fn(): console\n",
        );
        assert_eq!(scanned.captures(), ["console"]);
    }
}
