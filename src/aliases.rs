//! Type-alias expansion.
//!
//! A top-level `type Id = Int` (`Item::TypeAlias`) is expanded to its target
//! everywhere a type is written — function signatures, record/variant fields,
//! actor fields and handler parameters, trait/impl method signatures, and lambda
//! parameter annotations — and then dropped, so the type checker and code
//! generator only ever see concrete types. Aliases may chain (`type B = A`,
//! resolved to a fixpoint first). Aliases are simple (non-parameterized): a name
//! standing for one fully-written type.

use crate::ast::{Block, Expr, Function, Item, MethodSig, Module, Stmt, Type};
use std::collections::HashMap;

/// The name of a type alias defined in terms of itself (directly or through a
/// chain), if any — so the linker can report it rather than letting the alias
/// expand to a dangling reference. Returns the first cyclic alias found.
pub fn find_cycle(module: &Module) -> Option<String> {
    let mut edges: HashMap<String, Vec<String>> = HashMap::new();
    for item in &module.items {
        if let Item::TypeAlias { name, ty } = item {
            let mut refs = Vec::new();
            collect_type_names(ty, &mut refs);
            edges.insert(name.clone(), refs);
        }
    }
    // Restrict edges to other aliases, then DFS for a back edge.
    let names: std::collections::HashSet<String> = edges.keys().cloned().collect();
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

fn collect_type_names(t: &Type, out: &mut Vec<String>) {
    match t {
        Type::Named(name, args) => {
            out.push(name.clone());
            for a in args {
                collect_type_names(a, out);
            }
        }
        Type::Tuple(ts) => {
            for t in ts {
                collect_type_names(t, out);
            }
        }
        Type::Fn(params, ret) => {
            for p in params {
                collect_type_names(p, out);
            }
            collect_type_names(ret, out);
        }
    }
}

/// DFS from `node`, returning a node that lies on a cycle if one is reachable.
fn dfs_cycle(
    node: &str,
    edges: &HashMap<String, Vec<String>>,
    state: &mut HashMap<String, u8>,
) -> Option<String> {
    match state.get(node) {
        Some(2) => return None,    // already fully explored
        Some(1) => return Some(node.to_string()), // back edge: cycle
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

/// Expand every type alias and drop the alias items. A no-op without aliases.
pub fn resolve(mut module: Module) -> Module {
    let mut map: HashMap<String, Type> = HashMap::new();
    for item in &module.items {
        if let Item::TypeAlias { name, ty } = item {
            map.insert(name.clone(), ty.clone());
        }
    }
    if map.is_empty() {
        return module;
    }

    // Resolve alias-to-alias references to a fixpoint, so each alias maps to an
    // alias-free type. The iteration cap makes a cyclic alias terminate.
    let rounds = map.len() + 1;
    for _ in 0..rounds {
        let snapshot = map.clone();
        let mut changed = false;
        for t in map.values_mut() {
            changed |= resolve_type(t, &snapshot);
        }
        if !changed {
            break;
        }
    }

    for item in &mut module.items {
        match item {
            Item::Function(f) => resolve_function(f, &map),
            Item::Type(t) => {
                for v in &mut t.variants {
                    for ft in &mut v.fields {
                        resolve_type(ft, &map);
                    }
                }
            }
            Item::Trait(t) => {
                for m in &mut t.methods {
                    resolve_methodsig(m, &map);
                }
            }
            Item::Impl(im) => {
                for m in &mut im.methods {
                    resolve_function(m, &map);
                }
                for h in &mut im.handlers {
                    for p in &mut h.params {
                        if let Some(t) = &mut p.ty {
                            resolve_type(t, &map);
                        }
                    }
                    resolve_in_block(&mut h.body, &map);
                }
            }
            Item::TypeAlias { .. } | Item::Const { .. } | Item::Comptime(_) => {}
        }
    }

    module.items.retain(|it| !matches!(it, Item::TypeAlias { .. }));
    module
}

/// Expand alias names appearing anywhere in a type. The `map` is already
/// fixpoint-resolved, so a single replacement yields an alias-free type. Returns
/// whether anything changed.
fn resolve_type(ty: &mut Type, map: &HashMap<String, Type>) -> bool {
    match ty {
        Type::Named(name, args) => {
            if args.is_empty() {
                if let Some(target) = map.get(name) {
                    *ty = target.clone();
                    return true;
                }
            }
            let mut changed = false;
            for a in args {
                changed |= resolve_type(a, map);
            }
            changed
        }
        Type::Tuple(ts) => {
            let mut changed = false;
            for t in ts {
                changed |= resolve_type(t, map);
            }
            changed
        }
        Type::Fn(params, ret) => {
            let mut changed = false;
            for p in params {
                changed |= resolve_type(p, map);
            }
            changed |= resolve_type(ret, map);
            changed
        }
    }
}

fn resolve_function(f: &mut Function, map: &HashMap<String, Type>) {
    for p in &mut f.params {
        if let Some(t) = &mut p.ty {
            resolve_type(t, map);
        }
    }
    if let Some(t) = &mut f.ret {
        resolve_type(t, map);
    }
    resolve_in_block(&mut f.body, map);
}

fn resolve_methodsig(m: &mut MethodSig, map: &HashMap<String, Type>) {
    for p in &mut m.params {
        if let Some(t) = &mut p.ty {
            resolve_type(t, map);
        }
    }
    if let Some(t) = &mut m.ret {
        resolve_type(t, map);
    }
    if let Some(b) = &mut m.default {
        resolve_in_block(b, map);
    }
}

/// Walk a block to reach lambda parameter annotations buried in expressions; the
/// only types written inside a body are on lambda parameters.
fn resolve_in_block(block: &mut Block, map: &HashMap<String, Type>) {
    for stmt in &mut block.stmts {
        match stmt {
            Stmt::Let { value, .. }
            | Stmt::LetTuple { value, .. }
            | Stmt::Assign { value, .. }
            | Stmt::Yield(value)
            | Stmt::Expr(value) => resolve_in_expr(value, map),
            Stmt::Return(Some(e)) => resolve_in_expr(e, map),
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
    }
}

fn resolve_in_expr(e: &mut Expr, map: &HashMap<String, Type>) {
    match e {
        Expr::Lambda { params, body } => {
            for p in params.iter_mut() {
                if let Some(t) = &mut p.ty {
                    resolve_type(t, map);
                }
            }
            resolve_in_block(body, map);
        }
        Expr::Int(_) | Expr::Float(_) | Expr::Duration(_) | Expr::Str(_) | Expr::Bool(_)
        | Expr::Var(_) => {}
        Expr::List(xs) | Expr::Tuple(xs) => {
            for x in xs {
                resolve_in_expr(x, map);
            }
        }
        Expr::Call { args, .. } | Expr::Ctor { args, .. } => {
            for a in args {
                resolve_in_expr(a, map);
            }
        }
        Expr::MethodCall { receiver, args, .. } => {
            resolve_in_expr(receiver, map);
            for a in args {
                resolve_in_expr(a, map);
            }
        }
        Expr::Apply { func, args } => {
            resolve_in_expr(func, map);
            for a in args {
                resolve_in_expr(a, map);
            }
        }
        Expr::Unary { expr, .. } | Expr::Try(expr) | Expr::As { expr, .. } | Expr::Field { base: expr, .. } => {
            resolve_in_expr(expr, map)
        }
        Expr::RecordUpdate { base, fields } => {
            resolve_in_expr(base, map);
            for (_, v) in fields {
                resolve_in_expr(v, map);
            }
        }
        Expr::Record { fields, spread, .. } => {
            for (_, v) in fields {
                resolve_in_expr(v, map);
            }
            if let Some(s) = spread {
                resolve_in_expr(s, map);
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            resolve_in_expr(lhs, map);
            resolve_in_expr(rhs, map);
        }
        Expr::Range { lo, hi, .. } => {
            resolve_in_expr(lo, map);
            resolve_in_expr(hi, map);
        }
        Expr::Index { base, index } => {
            resolve_in_expr(base, map);
            resolve_in_expr(index, map);
        }
        Expr::WhileLet { scrutinee, body, .. } => {
            resolve_in_expr(scrutinee, map);
            resolve_in_block(body, map);
        }
        Expr::If { cond, then_block, else_block } => {
            resolve_in_expr(cond, map);
            resolve_in_block(then_block, map);
            if let Some(b) = else_block {
                resolve_in_block(b, map);
            }
        }
        Expr::While { cond, body } => {
            resolve_in_expr(cond, map);
            resolve_in_block(body, map);
        }
        Expr::For { iter, body, .. } => {
            resolve_in_expr(iter, map);
            resolve_in_block(body, map);
        }
        Expr::Match { scrutinee, arms } => {
            resolve_in_expr(scrutinee, map);
            for arm in arms.iter_mut() {
                if let Some(g) = &mut arm.guard {
                    resolve_in_expr(g, map);
                }
                resolve_in_expr(&mut arm.body, map);
            }
        }
        Expr::Block(b) => resolve_in_block(b, map),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_alias_in_signature_and_chains() {
        // `type Meters = Int`, `type Distance = Meters` — both expand to Int, and
        // no alias item survives.
        let src = "type Meters = Int\ntype Distance = Meters\nfn far(d: Distance) -> Meters:\n    d\n";
        let m = resolve(crate::parser::parse_module(src).expect("parse"));
        assert!(!m.items.iter().any(|it| matches!(it, Item::TypeAlias { .. })));
        let f = m
            .items
            .iter()
            .find_map(|it| match it {
                Item::Function(f) => Some(f),
                _ => None,
            })
            .expect("function");
        assert_eq!(f.params[0].ty, Some(Type::Named("Int".into(), vec![])));
        assert_eq!(f.ret, Some(Type::Named("Int".into(), vec![])));
    }

    #[test]
    fn find_cycle_flags_cyclic_aliases() {
        let cyclic = crate::parser::parse_module("type A = B\ntype B = A\n").expect("parse");
        assert!(find_cycle(&cyclic).is_some());
        let ok = crate::parser::parse_module("type A = Int\ntype B = A\n").expect("parse");
        assert!(find_cycle(&ok).is_none());
    }

    #[test]
    fn expands_alias_inside_compound_types() {
        // `type Row = List(Int)` inside `List(Row)` becomes `List(List(Int))`.
        let src = "type Row = List(Int)\nfn grid(g: List(Row)) -> Int:\n    0\n";
        let m = resolve(crate::parser::parse_module(src).expect("parse"));
        let f = m
            .items
            .iter()
            .find_map(|it| match it {
                Item::Function(f) => Some(f),
                _ => None,
            })
            .expect("function");
        assert_eq!(
            f.params[0].ty,
            Some(Type::Named(
                "List".into(),
                vec![Type::Named("List".into(), vec![Type::Named("Int".into(), vec![])])]
            ))
        );
    }
}
