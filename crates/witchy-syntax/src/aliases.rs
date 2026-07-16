//! Type-alias expansion.
//!
//! A top-level `type Id = Int` (`Item::TypeAlias`) is expanded to its target
//! everywhere a type is written — function signatures, record/variant fields,
//! trait/impl method signatures, `where`-clause trait arguments, impl heads
//! (`impl T for Id`), and every type written inside a body: `let`/`var`
//! ascriptions (`let x: Id = …`), `as` casts (`… as Id`), and lambda
//! parameter/return annotations — and then dropped, so the type checker and code
//! generator only ever see concrete types. Generic aliases substitute their
//! arguments before expansion (`type Pair(a) = (a, a)`; `Pair(Int)` →
//! `(Int, Int)`). Aliases may chain (`type B = A`, resolved to a fixpoint
//! first).

use crate::ast::{collect_type_names, Block, Expr, Function, Item, MethodSig, Module, Stmt, Type};
use crate::intrinsics;
// foldhash: compiler-internal keys only — see witchy-types/src/typeck.rs.
use foldhash::{HashMap, HashMapExt as _, HashSet};

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Alias {
    pub(crate) params: Vec<String>,
    pub(crate) ty: Type,
}

/// The name of a type alias defined in terms of itself (directly or through a
/// chain), if any — so the linker can report it rather than letting the alias
/// expand to a dangling reference. Returns the first cyclic alias found.
pub fn find_cycle(module: &Module) -> Option<String> {
    let mut edges: HashMap<String, Vec<String>> = HashMap::new();
    for item in &module.items {
        if let Item::TypeAlias { name, ty, .. } = item {
            let mut refs = Vec::new();
            collect_type_names(ty, &mut refs);
            edges.insert(name.clone(), refs);
        }
    }
    // Restrict edges to other aliases, then DFS for a back edge.
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
    let map = resolved_map(&module);
    if map.is_empty() {
        return module;
    }

    for item in &mut module.items {
        resolve_item(item, &map);
    }

    module.items.retain(|it| !matches!(it, Item::TypeAlias { .. }));
    module
}

/// Build the same fixpoint-resolved alias map [`resolve`] uses, without mutating
/// the module. Consumers that only need normalized type facts, such as
/// `derive(...)` TypeInfo construction, can apply this to a clone while leaving
/// the linker's later alias-cycle diagnostics and alias-erasure pass intact.
pub(crate) fn resolved_map(module: &Module) -> HashMap<String, Alias> {
    let mut map: HashMap<String, Alias> = HashMap::new();
    for item in &module.items {
        if let Item::TypeAlias { name, params, ty } = item {
            map.insert(name.clone(), Alias { params: params.clone(), ty: ty.clone() });
        }
    }
    if map.is_empty() {
        return map;
    }

    // Resolve alias-to-alias references to a fixpoint, so each alias maps to an
    // alias-free type. The iteration cap makes a cyclic alias terminate.
    let rounds = map.len() + 1;
    for _ in 0..rounds {
        let snapshot = map.clone();
        let mut changed = false;
        for alias in map.values_mut() {
            changed |= resolve_type(&mut alias.ty, &snapshot);
        }
        if !changed {
            break;
        }
    }

    map
}

fn resolve_item(item: &mut Item, map: &HashMap<String, Alias>) {
    match item {
        Item::Function(f) => resolve_function(f, map),
        Item::Type(t) => {
            for v in &mut t.variants {
                for ft in &mut v.fields {
                    resolve_type(ft, map);
                }
            }
        }
        Item::Trait(t) => {
            for m in &mut t.methods {
                resolve_methodsig(m, map);
            }
        }
        Item::Impl(im) => {
            // The impl head is itself a written-type position: `impl Show
            // for Id` targets an alias, and `impl FromIterator(Id) for
            // Set(Id) where a: Bound(Id)` writes aliases in its trait/target
            // arguments and `where` clause.
            resolve_impl_target(&mut im.type_name, &mut im.target_args, map);
            for t in &mut im.trait_args {
                resolve_type(t, map);
            }
            resolve_bounds(&mut im.bounds, map);
            for m in &mut im.methods {
                resolve_function(m, map);
            }
        }
        Item::TypeAlias { .. } | Item::Const { .. } | Item::Comptime(_) => {}
    }
}

pub(crate) fn resolve_type_aliases(ty: &mut Type, map: &HashMap<String, Alias>) -> bool {
    resolve_type(ty, map)
}

/// Expand alias names appearing anywhere in a type. The `map` is already
/// fixpoint-resolved, so a single replacement yields an alias-free type. Returns
/// whether anything changed.
fn resolve_type(ty: &mut Type, map: &HashMap<String, Alias>) -> bool {
    match ty {
        Type::Qualified(_, inner) => resolve_type(inner, map),
        Type::Named(name, args) => {
            let mut changed = false;
            for a in args.iter_mut() {
                changed |= resolve_type(a, map);
            }
            if let Some(alias) = map.get(name) {
                if alias.params.len() == args.len() {
                    let subst: HashMap<String, Type> = alias
                        .params
                        .iter()
                        .cloned()
                        .zip(args.iter().cloned())
                        .collect();
                    let mut target = alias.ty.clone();
                    substitute_alias_params(&mut target, &subst);
                    *ty = target;
                    return true;
                }
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
        Type::Fn(params, ret, _) => {
            let mut changed = false;
            for p in params {
                changed |= resolve_type(p, map);
            }
            changed |= resolve_type(ret, map);
            changed
        }
        // (RFC-0081) The head is a trait name — aliases bind TYPE names, so only
        // the trait arguments expand. (`type R = dyn Render` itself resolves via
        // the ordinary `Named` alias lookup at R's use sites.)
        Type::Dyn(_, args) => {
            let mut changed = false;
            for a in args {
                changed |= resolve_type(a, map);
            }
            changed
        }
    }
}

fn substitute_alias_params(ty: &mut Type, subst: &HashMap<String, Type>) -> bool {
    match ty {
        Type::Qualified(_, inner) => substitute_alias_params(inner, subst),
        Type::Named(name, args) => {
            if args.is_empty() {
                if let Some(target) = subst.get(name) {
                    *ty = target.clone();
                    return true;
                }
            }
            let mut changed = false;
            for a in args {
                changed |= substitute_alias_params(a, subst);
            }
            changed
        }
        Type::Tuple(ts) => {
            let mut changed = false;
            for t in ts {
                changed |= substitute_alias_params(t, subst);
            }
            changed
        }
        Type::Fn(params, ret, _) => {
            let mut changed = false;
            for p in params {
                changed |= substitute_alias_params(p, subst);
            }
            changed |= substitute_alias_params(ret, subst);
            changed
        }
        Type::Dyn(_, args) => {
            let mut changed = false;
            for a in args {
                changed |= substitute_alias_params(a, subst);
            }
            changed
        }
    }
}

fn resolve_function(f: &mut Function, map: &HashMap<String, Alias>) {
    for p in &mut f.params {
        if let Some(t) = &mut p.ty {
            resolve_type(t, map);
        }
    }
    if let Some(t) = &mut f.ret {
        resolve_type(t, map);
    }
    resolve_bounds(&mut f.bounds, map);
    resolve_in_block(&mut f.body, map);
}

/// Resolve aliases in a `where`-clause's trait type-arguments (`where c:
/// FromIterator(Id)` → `… FromIterator(Int)`). The bound's variable and trait
/// names are never type aliases, so only the trait arguments are rewritten.
fn resolve_bounds(bounds: &mut [(String, String, Vec<Type>)], map: &HashMap<String, Alias>) {
    for (_, _, trait_args) in bounds.iter_mut() {
        for t in trait_args {
            resolve_type(t, map);
        }
    }
}

/// Resolve an alias used as an impl-head target (`impl Show for Id`). If the
/// target denotes a named type after alias substitution it is rewritten to that
/// type's head and arguments. An alias to a non-named target (tuple/function
/// type) is not a valid impl target, so it is left untouched for the checker to
/// report — this stays fail-closed.
fn resolve_impl_target(name: &mut String, args: &mut Vec<Type>, map: &HashMap<String, Alias>) {
    for a in args.iter_mut() {
        resolve_type(a, map);
    }
    let Some(alias) = map.get(name) else {
        return;
    };
    if alias.params.len() != args.len() {
        return;
    }
    let subst: HashMap<String, Type> = alias
        .params
        .iter()
        .cloned()
        .zip(args.iter().cloned())
        .collect();
    let mut target = alias.ty.clone();
    substitute_alias_params(&mut target, &subst);
    if let Type::Named(target_name, target_args) = target {
        *name = target_name;
        *args = target_args;
    }
}

fn resolve_methodsig(m: &mut MethodSig, map: &HashMap<String, Alias>) {
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

/// Walk a block, resolving aliases in every type written inside a body: `let`/`var`
/// ascriptions, `as`-cast targets, and lambda parameter/return annotations (the
/// last two reached through `resolve_in_expr`).
fn resolve_in_block(block: &mut Block, map: &HashMap<String, Alias>) {
    for stmt in &mut block.stmts {
        match stmt {
            Stmt::Let { ty, value, .. } => {
                if let Some(t) = ty {
                    resolve_type(t, map);
                }
                resolve_in_expr(value, map);
            }
            Stmt::LetPattern { value, .. }
            | Stmt::Assign { value, .. }
            | Stmt::Yield(value)
            | Stmt::Expr(value) => resolve_in_expr(value, map),
            Stmt::Return(Some(e)) => resolve_in_expr(e, map),
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
    }
}

fn resolve_in_expr(e: &mut Expr, map: &HashMap<String, Alias>) {
    match e {
        Expr::Lambda { params, body, ret } => {
            for p in params.iter_mut() {
                if let Some(t) = &mut p.ty {
                    resolve_type(t, map);
                }
            }
            if let Some(t) = ret {
                resolve_type(t, map);
            }
            resolve_in_block(body, map);
        }
        Expr::Int(_) | Expr::Float(_) | Expr::Duration(_) | Expr::Str(_) | Expr::Bool(_)
        | Expr::Var(_) | Expr::TaggedLit { .. } => {}
        Expr::List(xs) | Expr::Tuple(xs) => {
            for x in xs {
                resolve_in_expr(x, map);
            }
        }
        Expr::Call { args, .. } | Expr::Ctor { args, .. }
        | Expr::AnonCtor { args, .. } => {
            for a in args {
                resolve_in_expr(a, map);
            }
        }
        Expr::LabeledCall { args, .. } => {
            for (_, a) in args {
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
        Expr::As { expr, ty } => {
            resolve_in_expr(expr, map);
            resolve_type(ty, map);
        }
        Expr::Unary { expr, .. } | Expr::Try(expr) | Expr::Field { base: expr, .. } => {
            resolve_in_expr(expr, map)
        }
        Expr::RecordUpdate { name: _, base, fields } => {
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

/// Map a bare builtin name that has since moved to a module-qualified stdlib
/// path (e.g. `push` -> `list.push`). Returns `None` for names that were never
/// moved. The type checker uses this to suggest the new spelling; the formatter
/// uses it to rewrite legacy calls to their canonical form.
pub fn moved_builtin(bare: &str) -> Option<&'static str> {
    Some(match bare {
        "push" => "list.push",
        "at" => intrinsics::LIST_AT,
        "length" => intrinsics::LIST_LENGTH,
        "concat" => intrinsics::LIST_CONCAT,
        "dict_new" => intrinsics::DICT_NEW,
        "insert" => "dict.insert",
        "get_or" => intrinsics::DICT_GET_OR,
        "has" => intrinsics::DICT_CONTAINS_KEY,
        "remove" => "dict.remove",
        "update" => "dict.update",
        "keys" => intrinsics::DICT_KEYS,
        "values" => intrinsics::DICT_VALUES,
        "pairs" => intrinsics::DICT_PAIRS,
        "size" => intrinsics::DICT_LENGTH,
        "split" => intrinsics::STRING_SPLIT,
        "trim" => intrinsics::STRING_TRIM,
        "contains" => intrinsics::STRING_CONTAINS,
        "starts_with" => intrinsics::STRING_STARTS_WITH,
        "ends_with" => intrinsics::STRING_ENDS_WITH,
        "replace" => intrinsics::STRING_REPLACE,
        "index_of" => "string.index_of",
        "substring" => intrinsics::STRING_SUBSTRING,
        "string_length" => intrinsics::STRING_LENGTH,
        "char_count" => intrinsics::STRING_CHAR_COUNT,
        "string_chars" => intrinsics::STRING_CHARS,
        "to_chars" => intrinsics::STRING_CHARS,
        "to_upper" => intrinsics::STRING_TO_UPPER,
        "to_lower" => intrinsics::STRING_TO_LOWER,
        "string_to_int" => intrinsics::STRING_TO_INT,
        "int_to_float" => intrinsics::MATH_TO_FLOAT,
        "float_to_int" => intrinsics::MATH_TO_INT,
        "sqrt" => intrinsics::MATH_SQRT,
        _ => return None,
    })
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
    fn expands_alias_in_body_written_types() {
        // `type Id = Int` written inside a body — a `let` ascription, a lambda
        // parameter and return type — must all expand to `Int`, not stay `Id`
        // (which the checker would reject as an unknown type).
        let src = "type Id = Int\nfn main():\n    let x: Id = 5\n    let f = fn(n: Id) -> Id: n\n    x\n";
        let m = resolve(crate::parser::parse_module(src).expect("parse"));
        let f = m
            .items
            .iter()
            .find_map(|it| match it {
                Item::Function(f) => Some(f),
                _ => None,
            })
            .expect("function");
        let int = Type::Named("Int".into(), vec![]);
        // `let x: Id = 5` — the ascription expands.
        match &f.body.stmts[0] {
            Stmt::Let { ty, .. } => assert_eq!(ty.as_ref(), Some(&int)),
            s => panic!("expected `let x: Id`, got {s:?}"),
        }
        // `let f = fn(n: Id) -> Id: n` — lambda parameter and return expand.
        match &f.body.stmts[1] {
            Stmt::Let { value: Expr::Lambda { params, ret, .. }, .. } => {
                assert_eq!(params[0].ty.as_ref(), Some(&int));
                assert_eq!(ret.as_ref(), Some(&int));
            }
            s => panic!("expected `let f = fn ...`, got {s:?}"),
        }
    }

    #[test]
    fn expands_alias_in_as_cast_and_where_bound() {
        // `x as Id` (`Expr::As`) and a `where` trait argument (`FromIterator(Id)`)
        // are both written-type positions that must expand to the target.
        let src = "type Id = Int\nfn each(c: Id) -> Id where c: FromIterator(Id):\n    c as Id\n";
        let m = resolve(crate::parser::parse_module(src).expect("parse"));
        let f = m
            .items
            .iter()
            .find_map(|it| match it {
                Item::Function(f) => Some(f),
                _ => None,
            })
            .expect("function");
        let int = Type::Named("Int".into(), vec![]);
        // The `where c: FromIterator(Id)` bound's trait argument expands.
        assert_eq!(f.bounds[0].2, vec![int.clone()]);
        // `c as Id` — the cast target expands (last statement is the tail expr).
        match f.body.stmts.last().expect("stmt") {
            Stmt::Expr(Expr::As { ty, .. }) => assert_eq!(ty, &int),
            s => panic!("expected `c as Id`, got {s:?}"),
        }
    }

    #[test]
    fn expands_alias_in_impl_head() {
        // `impl Describe for Id` targets an alias; the head's `type_name` must
        // expand so the impl attaches to the concrete `Int`.
        let src = "type Id = Int\ntrait Describe:\n    fn describe(self) -> String\nimpl Describe for Id:\n    fn describe(self) -> String:\n        \"id\"\n";
        let m = resolve(crate::parser::parse_module(src).expect("parse"));
        let im = m
            .items
            .iter()
            .find_map(|it| match it {
                Item::Impl(im) => Some(im),
                _ => None,
            })
            .expect("impl");
        assert_eq!(im.type_name, "Int");
        assert!(im.target_args.is_empty());
    }

    #[test]
    fn expands_generic_alias_in_impl_head() {
        // Impl heads are written type positions too, so generic aliases must
        // substitute just like function parameters and return types do.
        let src = "type Row(a) = List(a)\ntrait Describe:\n    fn describe(self) -> String\nimpl Describe for Row(Int):\n    fn describe(self) -> String:\n        \"row\"\n";
        let m = resolve(crate::parser::parse_module(src).expect("parse"));
        let im = m
            .items
            .iter()
            .find_map(|it| match it {
                Item::Impl(im) => Some(im),
                _ => None,
            })
            .expect("impl");
        assert_eq!(im.type_name, "List");
        assert_eq!(im.target_args, vec![Type::Named("Int".into(), vec![])]);
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

    #[test]
    fn expands_generic_aliases_by_substitution() {
        // BUG-563: the parser accepted `type Pair(a) = ...` but uses such as
        // `Pair(Int)` used to survive alias resolution and then fail as an
        // unknown type. A generic alias is just transparent substitution.
        let src = "type Pair(a) = (a, a)\ntype Rows(a) = List(Pair(a))\nfn first(p: Pair(Int), rows: Rows(String)) -> Int:\n    p.0\n";
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
        assert_eq!(
            f.params[0].ty,
            Some(Type::Tuple(vec![Type::Named("Int".into(), vec![]), Type::Named("Int".into(), vec![])]))
        );
        assert_eq!(
            f.params[1].ty,
            Some(Type::Named(
                "List".into(),
                vec![Type::Tuple(vec![
                    Type::Named("String".into(), vec![]),
                    Type::Named("String".into(), vec![]),
                ])],
            ))
        );
    }
}
