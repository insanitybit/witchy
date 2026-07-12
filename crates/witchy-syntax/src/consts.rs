//! Module-level constant inlining.
//!
//! A top-level `let NAME = EXPR` (`Item::Const`) is substituted at each of its
//! use sites and then dropped, so the type checker, interpreter, and code
//! generator never see constants — only the expressions they expand to. Constant
//! values may reference other constants (resolved to a fixpoint first), and a
//! local binding of the same name shadows the constant (substitution is
//! scope-aware), so a constant behaves exactly like an ordinary immutable binding
//! that is in scope everywhere it isn't shadowed.

use crate::ast::{Block, Expr, Item, Module, Pattern, Stmt};
// foldhash: compiler-internal keys only — see witchy-types/src/typeck.rs.
use foldhash::{HashMap, HashMapExt as _, HashSet, HashSetExt as _};

/// The name of a constant defined in terms of itself (directly or through a
/// chain), if any — so the linker can report it rather than letting it expand to
/// a dangling self-reference. Returns the first cyclic constant found.
pub fn find_cycle(module: &Module) -> Option<String> {
    let mut edges: HashMap<String, Vec<String>> = HashMap::new();
    for item in &module.items {
        if let Item::Const { name, value } = item {
            let mut refs = Vec::new();
            collect_names(value, &mut refs);
            edges.insert(name.clone(), refs);
        }
    }
    let names: HashSet<String> = edges.keys().cloned().collect();
    for refs in edges.values_mut() {
        refs.retain(|r| names.contains(r));
    }
    let mut state: HashMap<String, u8> = HashMap::new(); // 0=unseen,1=on-stack,2=done
    for start in edges.keys() {
        if let Some(c) = dfs_cycle(start, &edges, &mut state) {
            return Some(c);
        }
    }
    None
}

/// Collect every variable/constructor name an expression references (an
/// uppercase constant reference parses as a nullary constructor, so both forms
/// matter).
fn collect_names(e: &Expr, out: &mut Vec<String>) {
    match e {
        Expr::Var(n) => out.push(n.clone()),
        Expr::Ctor { name, args } => {
            out.push(name.clone());
            for a in args {
                collect_names(a, out);
            }
        }
        Expr::Int(_) | Expr::Float(_) | Expr::Duration(_) | Expr::Str(_) | Expr::Bool(_)
        | Expr::TaggedLit { .. } => {}
        Expr::List(xs) | Expr::Tuple(xs) | Expr::Call { args: xs, .. }
        | Expr::AnonCtor { args: xs, .. } => {
            for x in xs {
                collect_names(x, out);
            }
        }
        Expr::LabeledCall { args, .. } => {
            for (_, a) in args {
                collect_names(a, out);
            }
        }
        Expr::MethodCall { receiver, args, .. } => {
            collect_names(receiver, out);
            for a in args {
                collect_names(a, out);
            }
        }
        Expr::Apply { func, args } => {
            collect_names(func, out);
            for a in args {
                collect_names(a, out);
            }
        }
        Expr::Unary { expr, .. } | Expr::Try(expr) | Expr::As { expr, .. } | Expr::Field { base: expr, .. } => {
            collect_names(expr, out)
        }
        Expr::RecordUpdate { name: _, base, fields } => {
            collect_names(base, out);
            for (_, v) in fields {
                collect_names(v, out);
            }
        }
        Expr::Record { fields, spread, .. } => {
            for (_, v) in fields {
                collect_names(v, out);
            }
            if let Some(s) = spread {
                collect_names(s, out);
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            collect_names(lhs, out);
            collect_names(rhs, out);
        }
        Expr::Range { lo, hi, .. } => {
            collect_names(lo, out);
            collect_names(hi, out);
        }
        Expr::Index { base, index } => {
            collect_names(base, out);
            collect_names(index, out);
        }
        Expr::WhileLet { scrutinee, body, .. } => {
            collect_names(scrutinee, out);
            collect_names_block(body, out);
        }
        Expr::If { cond, then_block, else_block } => {
            collect_names(cond, out);
            collect_names_block(then_block, out);
            if let Some(b) = else_block {
                collect_names_block(b, out);
            }
        }
        Expr::While { cond, body } => {
            collect_names(cond, out);
            collect_names_block(body, out);
        }
        Expr::For { iter, body, .. } => {
            collect_names(iter, out);
            collect_names_block(body, out);
        }
        Expr::Match { scrutinee, arms } => {
            collect_names(scrutinee, out);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    collect_names(g, out);
                }
                collect_names(&arm.body, out);
            }
        }
        Expr::Lambda { body, .. } => collect_names_block(body, out),
        Expr::Block(b) => collect_names_block(b, out),
    }
}

fn collect_names_block(b: &Block, out: &mut Vec<String>) {
    for stmt in &b.stmts {
        match stmt {
            Stmt::Let { value, .. }
            | Stmt::LetPattern { value, .. }
            | Stmt::Assign { value, .. }
            | Stmt::Expr(value)
            | Stmt::Yield(value)
            | Stmt::Return(Some(value)) => collect_names(value, out),
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
    }
}

fn dfs_cycle(
    node: &str,
    edges: &HashMap<String, Vec<String>>,
    state: &mut HashMap<String, u8>,
) -> Option<String> {
    match state.get(node) {
        Some(2) => return None,
        Some(1) => return Some(node.to_string()),
        _ => {}
    }
    state.insert(node.to_string(), 1);
    if let Some(next) = edges.get(node) {
        for n in next {
            if let Some(c) = dfs_cycle(n, edges, state) {
                return Some(c);
            }
        }
    }
    state.insert(node.to_string(), 2);
    None
}

/// Inline every module-level constant at its use sites and drop the constant
/// items. A no-op for modules without constants.
pub fn inline(mut module: Module) -> Module {
    let mut consts: HashMap<String, Expr> = HashMap::new();
    for item in &module.items {
        if let Item::Const { name, value } = item {
            consts.insert(name.clone(), value.clone());
        }
    }
    if consts.is_empty() {
        return module;
    }
    let cnames: HashSet<String> = consts.keys().cloned().collect();

    // Resolve constant-to-constant references to a fixpoint, so `let B = A + 1`
    // expands `A` too. The iteration cap makes a cyclic definition terminate.
    for _ in 0..=cnames.len() {
        let snapshot = consts.clone();
        let mut changed = false;
        for value in consts.values_mut() {
            let mut scope = HashSet::new();
            changed |= subst_expr(value, &snapshot, &cnames, &mut scope);
        }
        if !changed {
            break;
        }
    }

    for item in &mut module.items {
        match item {
            Item::Function(f) => {
                let mut scope: HashSet<String> =
                    f.params.iter().map(|p| p.name.clone()).collect();
                subst_block(&mut f.body, &consts, &cnames, &mut scope);
            }
            Item::Impl(im) => {
                for m in &mut im.methods {
                    let mut scope: HashSet<String> =
                        m.params.iter().map(|p| p.name.clone()).collect();
                    subst_block(&mut m.body, &consts, &cnames, &mut scope);
                }
            }
            Item::Const { .. } | Item::Type(_) | Item::Trait(_) | Item::TypeAlias { .. } | Item::Comptime(_) => {}
        }
    }

    module.items.retain(|it| !matches!(it, Item::Const { .. }));
    module
}

/// `scope` holds the names bound locally so far; a constant is substituted only
/// where its name isn't shadowed. Statements thread `scope` forward (a `let`
/// binds for the rest of the block); nested blocks get a clone so their bindings
/// don't leak. Returns whether anything was replaced.
fn subst_block(
    block: &mut Block,
    consts: &HashMap<String, Expr>,
    cnames: &HashSet<String>,
    scope: &mut HashSet<String>,
) -> bool {
    let mut changed = false;
    for stmt in &mut block.stmts {
        changed |= subst_stmt(stmt, consts, cnames, scope);
    }
    changed
}

fn subst_stmt(
    stmt: &mut Stmt,
    consts: &HashMap<String, Expr>,
    cnames: &HashSet<String>,
    scope: &mut HashSet<String>,
) -> bool {
    match stmt {
        Stmt::Let { name, value, .. } => {
            let changed = subst_expr(value, consts, cnames, scope);
            scope.insert(name.clone());
            changed
        }
        Stmt::LetPattern { pattern, value } => {
            let changed = subst_expr(value, consts, cnames, scope);
            pattern_binds(pattern, scope);
            changed
        }
        Stmt::Assign { value, .. } => subst_expr(value, consts, cnames, scope),
        Stmt::Return(Some(e)) => subst_expr(e, consts, cnames, scope),
        Stmt::Expr(e) | Stmt::Yield(e) => subst_expr(e, consts, cnames, scope),
        Stmt::Return(None) | Stmt::Break | Stmt::Continue => false,
    }
}

fn subst_expr(
    e: &mut Expr,
    consts: &HashMap<String, Expr>,
    cnames: &HashSet<String>,
    scope: &mut HashSet<String>,
) -> bool {
    match e {
        Expr::Var(name) => {
            if cnames.contains(name) && !scope.contains(name) {
                *e = consts[name].clone();
                true
            } else {
                false
            }
        }
        Expr::Int(_) | Expr::Float(_) | Expr::Duration(_) | Expr::Str(_) | Expr::Bool(_)
        | Expr::TaggedLit { .. } => false,
        Expr::List(xs) | Expr::Tuple(xs) | Expr::AnonCtor { args: xs, .. } => {
            let mut changed = false;
            for x in xs {
                changed |= subst_expr(x, consts, cnames, scope);
            }
            changed
        }
        Expr::Ctor { name, args } => {
            // A reference to an uppercase constant parses as a nullary
            // constructor, so substitute those too (a real constructor of the
            // same name is impossible: a constant occupies that name).
            if args.is_empty() && cnames.contains(name) && !scope.contains(name) {
                *e = consts[name].clone();
                return true;
            }
            let mut changed = false;
            for a in args {
                changed |= subst_expr(a, consts, cnames, scope);
            }
            changed
        }
        Expr::Call { args, .. } => {
            let mut changed = false;
            for a in args {
                changed |= subst_expr(a, consts, cnames, scope);
            }
            changed
        }
        Expr::LabeledCall { args, .. } => {
            let mut changed = false;
            for (_, a) in args {
                changed |= subst_expr(a, consts, cnames, scope);
            }
            changed
        }
        Expr::MethodCall { receiver, args, .. } => {
            let mut changed = subst_expr(receiver, consts, cnames, scope);
            for a in args {
                changed |= subst_expr(a, consts, cnames, scope);
            }
            changed
        }
        Expr::Apply { func, args } => {
            let mut changed = subst_expr(func, consts, cnames, scope);
            for a in args {
                changed |= subst_expr(a, consts, cnames, scope);
            }
            changed
        }
        Expr::Unary { expr, .. } | Expr::Try(expr) | Expr::As { expr, .. } | Expr::Field { base: expr, .. } => {
            subst_expr(expr, consts, cnames, scope)
        }
        Expr::RecordUpdate { name: _, base, fields } => {
            let mut changed = subst_expr(base, consts, cnames, scope);
            for (_, v) in fields {
                changed |= subst_expr(v, consts, cnames, scope);
            }
            changed
        }
        Expr::Record { fields, spread, .. } => {
            let mut changed = false;
            for (_, v) in fields {
                changed |= subst_expr(v, consts, cnames, scope);
            }
            if let Some(s) = spread {
                changed |= subst_expr(s, consts, cnames, scope);
            }
            changed
        }
        Expr::Binary { lhs, rhs, .. } => {
            let a = subst_expr(lhs, consts, cnames, scope);
            let b = subst_expr(rhs, consts, cnames, scope);
            a || b
        }
        Expr::Range { lo, hi, .. } => {
            let a = subst_expr(lo, consts, cnames, scope);
            let b = subst_expr(hi, consts, cnames, scope);
            a || b
        }
        Expr::Index { base, index } => {
            let a = subst_expr(base, consts, cnames, scope);
            let b = subst_expr(index, consts, cnames, scope);
            a || b
        }
        Expr::WhileLet { pattern, scrutinee, body } => {
            let mut changed = subst_expr(scrutinee, consts, cnames, scope);
            let mut s = scope.clone();
            pattern_binds(pattern, &mut s);
            changed |= subst_block(body, consts, cnames, &mut s);
            changed
        }
        Expr::If { cond, then_block, else_block } => {
            let mut changed = subst_expr(cond, consts, cnames, scope);
            changed |= subst_block(then_block, consts, cnames, &mut scope.clone());
            if let Some(b) = else_block {
                changed |= subst_block(b, consts, cnames, &mut scope.clone());
            }
            changed
        }
        Expr::While { cond, body } => {
            let mut changed = subst_expr(cond, consts, cnames, scope);
            changed |= subst_block(body, consts, cnames, &mut scope.clone());
            changed
        }
        Expr::For { var, iter, body } => {
            let mut changed = subst_expr(iter, consts, cnames, scope);
            let mut s = scope.clone();
            s.insert(var.clone());
            changed |= subst_block(body, consts, cnames, &mut s);
            changed
        }
        Expr::Match { scrutinee, arms } => {
            let mut changed = subst_expr(scrutinee, consts, cnames, scope);
            for arm in arms.iter_mut() {
                let mut s = scope.clone();
                pattern_binds(&arm.pattern, &mut s);
                if let Some(g) = &mut arm.guard {
                    changed |= subst_expr(g, consts, cnames, &mut s);
                }
                changed |= subst_expr(&mut arm.body, consts, cnames, &mut s);
            }
            changed
        }
        Expr::Lambda { params, body, .. } => {
            let mut s = scope.clone();
            for p in params.iter() {
                s.insert(p.name.clone());
            }
            subst_block(body, consts, cnames, &mut s)
        }
        Expr::Block(b) => subst_block(b, consts, cnames, &mut scope.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn func_of(src: &str, name: &str) -> crate::ast::Function {
        let m = inline(crate::parser::parse_module(src).expect("parse"));
        m.items
            .into_iter()
            .find_map(|it| match it {
                Item::Function(f) if f.name == name => Some(f),
                _ => None,
            })
            .expect("function")
    }

    #[test]
    fn inlines_constant_and_resolves_const_to_const() {
        // `BASE` is inlined into `DOUBLE`, and both into `f`; no const items
        // remain and no constant name survives in the body.
        let src = "let BASE = 10\nlet DOUBLE = BASE * 2\nfn f() -> Int:\n    DOUBLE + BASE\n";
        let m = inline(crate::parser::parse_module(src).expect("parse"));
        assert!(!m.items.iter().any(|it| matches!(it, Item::Const { .. })));
        let f = func_of(src, "f");
        let printed = format!("{:?}", f.body);
        assert!(!printed.contains("BASE") && !printed.contains("DOUBLE"), "{printed}");
    }

    #[test]
    fn find_cycle_flags_cyclic_constants() {
        let cyclic = crate::parser::parse_module("let A = B\nlet B = A\n").expect("parse");
        assert!(find_cycle(&cyclic).is_some());
        let self_ref = crate::parser::parse_module("let A = A + 1\n").expect("parse");
        assert!(find_cycle(&self_ref).is_some());
        let ok = crate::parser::parse_module("let A = 10\nlet B = A + 1\n").expect("parse");
        assert!(find_cycle(&ok).is_none());
    }

    #[test]
    fn local_binding_shadows_constant() {
        // A parameter named like the constant wins: the reference is left alone.
        let f = func_of("let limit = 100\nfn g(limit: Int) -> Int:\n    limit\n", "g");
        assert_eq!(f.body.stmts.len(), 1);
        match &f.body.stmts[0] {
            Stmt::Expr(Expr::Var(n)) => assert_eq!(n, "limit"),
            other => panic!("expected the parameter reference, got {other:?}"),
        }
    }
}

/// Add the names a pattern binds to `scope`.
fn pattern_binds(p: &Pattern, scope: &mut HashSet<String>) {
    match p {
        Pattern::Var(name) => {
            scope.insert(name.clone());
        }
        Pattern::Ctor { args, .. } => {
            for a in args {
                pattern_binds(a, scope);
            }
        }
        Pattern::Tuple(ps) => {
            for a in ps {
                pattern_binds(a, scope);
            }
        }
        Pattern::List { elems, rest } => {
            for a in elems {
                pattern_binds(a, scope);
            }
            if let Some(Some(n)) = rest {
                scope.insert(n.clone());
            }
        }
        // All alternatives of an or-pattern bind the same names, so the first
        // one's bindings are the set (RFC-0052).
        Pattern::Or(alts) => {
            if let Some(first) = alts.first() {
                pattern_binds(first, scope);
            }
        }
        Pattern::Wildcard | Pattern::Int(_) | Pattern::Str(_) | Pattern::Bool(_)
        | Pattern::Duration(_) | Pattern::IntRange { .. } => {}
    }
}
