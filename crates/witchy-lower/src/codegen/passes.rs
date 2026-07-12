//! Module-level AST rewrite passes run before lowering: alpha-renaming of
//! shadowing bindings (the `Renamer`), string-concat flipping, and try-context
//! rewriting. Split out of `codegen/mod.rs` as the fourth slice of an
//! incremental break-up of that file. These are free functions over the AST
//! with no `Codegen` coupling.

use super::*;
use witchy_syntax::intrinsics;

/// Variables bound by a pattern (these become function locals).
/// Give shadowing bindings unique names before codegen. The compiled backend
/// declares one WASM local per distinct local *name* across a whole function,
/// so an inner binding that reuses an outer name would otherwise alias the
/// same local and clobber the outer value once the inner scope ends. This pass
/// walks the body with a scope stack and renames any binding (let, lettuple,
/// loop var, match-pattern var, lambda param) that shadows a name already in
/// scope, rewriting the references that resolve to it. Names that don't shadow
/// are left untouched, so output is unchanged for the common case.
struct Renamer {
    scopes: Vec<HashMap<String, String>>,
    counter: u32,
    // Every source name ever bound in this function. A WASM local has a single
    // type, so two *disjoint* scopes that reuse a name must still get distinct
    // locals — they can differ in kind (e.g. an i64 range loop var in one branch
    // and an i32 tuple destructure in another).
    seen: HashSet<String>,
}

impl Renamer {
    fn new() -> Self {
        Self { scopes: Vec::new(), counter: 0, seen: HashSet::new() }
    }

    fn resolve(&self, name: &str) -> String {
        for s in self.scopes.iter().rev() {
            if let Some(n) = s.get(name) {
                return n.clone();
            }
        }
        name.to_string()
    }

    /// Bind `name` in the current scope, renaming it if it's already in scope.
    fn declare(&mut self, name: &str) -> String {
        // First use of a name keeps it; any later binding of the same name (a
        // shadow, or a reuse in a sibling scope) gets a fresh unique local.
        let unique = if self.seen.insert(name.to_string()) {
            name.to_string()
        } else {
            self.counter += 1;
            format!("{name}__shadow{}", self.counter)
        };
        self.scopes
            .last_mut()
            .expect("scope")
            .insert(name.to_string(), unique.clone());
        unique
    }

    fn rename_block(&mut self, b: &mut Block) {
        self.scopes.push(HashMap::new());
        for stmt in &mut b.stmts {
            self.rename_stmt(stmt);
        }
        self.scopes.pop();
    }

    fn rename_stmt(&mut self, s: &mut Stmt) {
        match s {
            // The value is evaluated in the scope *before* the binding exists.
            Stmt::Let { name, value, .. } => {
                self.rename_expr(value);
                *name = self.declare(name);
            }
            Stmt::Assign { name, value } => {
                self.rename_expr(value);
                *name = self.resolve(name);
            }
            Stmt::LetPattern { pattern, value } => {
                self.rename_expr(value);
                self.rename_pattern(pattern);
            }
            Stmt::Return(Some(e)) | Stmt::Expr(e) | Stmt::Yield(e) => self.rename_expr(e),
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
    }

    fn rename_expr(&mut self, e: &mut Expr) {
        match e {
            // A range survives only inside a `for` iterator; rename vars in its
            // bounds (e.g. a captured `n` in `0..n`). The other sugar nodes are
            // fully lowered before codegen.
            Expr::Range { lo, hi, .. } => {
                self.rename_expr(lo);
                self.rename_expr(hi);
            }
            Expr::Index { .. }
            | Expr::WhileLet { .. }
            | Expr::MethodCall { .. }
            | Expr::Record { .. }
            | Expr::LabeledCall { .. } => {
                unreachable!("range/index sugar is lowered before codegen (parser::lower_sugar_module)")
            }
            Expr::Var(n) => *n = self.resolve(n),
            Expr::Int(_)
        | Expr::Duration(_)
        | Expr::Float(_)
        | Expr::Str(_)
        | Expr::Bool(_)
        | Expr::TaggedLit { .. } => {}
            // Call / Ctor names are functions / constructors, not locals —
            // only the arguments are renamed.
            Expr::List(xs) | Expr::Tuple(xs) => {
                for x in xs {
                    self.rename_expr(x);
                }
            }
            Expr::Apply { func, args } => {
                self.rename_expr(func);
                for a in args {
                    self.rename_expr(a);
                }
            }
            // A `Call` name may be a LOCAL closure variable (`cont(x)` where
            // `cont` was bound by a `let`/parameter/match pattern), which
            // lexically shadows any global of the same name — exactly as the
            // type checker resolves it. Rename it like any other use: `resolve`
            // is a no-op for a true global (never bound in a scope), so this
            // only rewrites calls to a renamed local. Without this, a local
            // closure that gets alpha-renamed (e.g. a `cont` reused across
            // sibling match arms) loses its call sites. A `Ctor` name is always
            // a global constructor, never a local.
            Expr::Call { name, args } => {
                *name = self.resolve(name);
                for a in args {
                    self.rename_expr(a);
                }
            }
            Expr::Ctor { args, .. } | Expr::AnonCtor { args, .. } => {
                for a in args {
                    self.rename_expr(a);
                }
            }
            Expr::Unary { expr, .. } | Expr::Try(expr) | Expr::As { expr, .. } => self.rename_expr(expr),
            // The field name is not a local.
            Expr::Field { base, .. } => self.rename_expr(base),
            Expr::RecordUpdate { name: _, base, fields } => {
                self.rename_expr(base);
                for (_, v) in fields {
                    self.rename_expr(v);
                }
            }
            Expr::Binary { lhs, rhs, .. } => {
                self.rename_expr(lhs);
                self.rename_expr(rhs);
            }
            Expr::If { cond, then_block, else_block } => {
                self.rename_expr(cond);
                self.rename_block(then_block);
                if let Some(b) = else_block {
                    self.rename_block(b);
                }
            }
            Expr::Match { scrutinee, arms } => {
                self.rename_expr(scrutinee);
                for arm in arms {
                    self.scopes.push(HashMap::new());
                    self.rename_pattern(&mut arm.pattern);
                    if let Some(g) = &mut arm.guard {
                        self.rename_expr(g);
                    }
                    self.rename_expr(&mut arm.body);
                    self.scopes.pop();
                }
            }
            Expr::Block(b) => self.rename_block(b),
            Expr::While { cond, body } => {
                self.rename_expr(cond);
                self.rename_block(body);
            }
            // The loop variable is bound in the same scope as the body.
            Expr::For { var, iter, body } => {
                self.rename_expr(iter);
                self.scopes.push(HashMap::new());
                *var = self.declare(var);
                for stmt in &mut body.stmts {
                    self.rename_stmt(stmt);
                }
                self.scopes.pop();
            }
            Expr::Lambda { params, body, .. } => {
                self.scopes.push(HashMap::new());
                for p in params {
                    p.name = self.declare(&p.name);
                }
                for stmt in &mut body.stmts {
                    self.rename_stmt(stmt);
                }
                self.scopes.pop();
            }
        }
    }

    fn rename_pattern(&mut self, p: &mut Pattern) {
        match p {
            Pattern::Var(n) => *n = self.declare(n),
            Pattern::Ctor { args, .. } | Pattern::AnonCtor { args, .. } | Pattern::Tuple(args) => {
                for a in args {
                    self.rename_pattern(a);
                }
            }
            Pattern::List { elems, rest } => {
                for e in elems {
                    self.rename_pattern(e);
                }
                if let Some(Some(n)) = rest {
                    *n = self.declare(n);
                }
            }
            // (RFC-0052) Every or-pattern alternative binds the SAME names, and the
            // arm body must see ONE renamed name per source name. So declare the
            // bindings via the first alternative, then rewrite each remaining
            // alternative's variables to those already-declared names (`resolve`,
            // not `declare` — which would mint a fresh shadow on the repeat).
            Pattern::Or(alts) => {
                if let Some((first, rest)) = alts.split_first_mut() {
                    self.rename_pattern(first);
                    for alt in rest {
                        self.resolve_pattern_vars(alt);
                    }
                }
            }
            _ => {}
        }
    }

    /// Rewrite an or-pattern alternative's variables to their ALREADY-declared
    /// renamed names (the first alternative did the declaring). Used only for
    /// non-first or-pattern alternatives, which bind the identical name set.
    fn resolve_pattern_vars(&mut self, p: &mut Pattern) {
        match p {
            Pattern::Var(n) => *n = self.resolve(n),
            Pattern::Ctor { args, .. } | Pattern::AnonCtor { args, .. } | Pattern::Tuple(args) => {
                for a in args {
                    self.resolve_pattern_vars(a);
                }
            }
            Pattern::List { elems, rest } => {
                for e in elems {
                    self.resolve_pattern_vars(e);
                }
                if let Some(Some(n)) = rest {
                    *n = self.resolve(n);
                }
            }
            Pattern::Or(alts) => {
                for alt in alts {
                    self.resolve_pattern_vars(alt);
                }
            }
            _ => {}
        }
    }
}

/// Alpha-rename a function body so shadowing bindings get unique names.
/// `params` are bound in the outermost scope (never renamed themselves).
/// Alpha-rename every function body IN PLACE, once, at module
/// level — BEFORE `typeck::annotate` runs — so the annotated AST instance is
/// the very one codegen compiles (the type table and uniqueness facts are
/// keyed by node identity). `compile_function` compiles bodies as-given.
/// Flip string `+` to the internal `Concat` op, in place — AFTER annotation
/// (the table's node-identity keys survive a field mutation) and BEFORE the
/// ownership analysis (whose accumulator shapes match `Concat`). Detection is
/// the type table plus string literals; anything it misses still compiles
/// correctly through the val-type net in the `Add` arm, just unoptimized.
pub(crate) fn flip_string_add_module(m: &mut Module, table: &witchy_types::typeck::TypeTable) {
    fn stringy(e: &Expr, table: &witchy_types::typeck::TypeTable) -> bool {
        // A `Concat` is always a String — recognize it structurally so a nested
        // chain whose intermediate levels lack a literal operand (and whose other
        // operand the type table didn't resolve, e.g. a build-time `read_build`)
        // still flips the whole chain once the innermost level is anchored.
        matches!(e, Expr::Str(_))
            || matches!(e, Expr::Binary { op: BinOp::Concat, .. })
            || matches!(
                table.type_of(e).and_then(witchy_types::typeck::ty_to_ast),
                Some(Type::Named(n, _)) if n == "String"
            )
    }
    fn walk_expr(e: &mut Expr, table: &witchy_types::typeck::TypeTable) {
        match e {
            Expr::Binary { op, lhs, rhs } => {
                walk_expr(lhs, table);
                walk_expr(rhs, table);
                if *op == BinOp::Add && (stringy(lhs, table) || stringy(rhs, table)) {
                    *op = BinOp::Concat;
                }
            }
            Expr::List(xs) | Expr::Tuple(xs) | Expr::Ctor { args: xs, .. }
            | Expr::AnonCtor { args: xs, .. }
            | Expr::Call { args: xs, .. } => {
                for x in xs {
                    walk_expr(x, table);
                }
            }
            Expr::Apply { func, args } => {
                walk_expr(func, table);
                for a in args {
                    walk_expr(a, table);
                }
            }
            Expr::MethodCall { receiver, args, .. } => {
                walk_expr(receiver, table);
                for a in args {
                    walk_expr(a, table);
                }
            }
            Expr::Unary { expr, .. }
            | Expr::Try(expr)
            | Expr::As { expr, .. }
            | Expr::Field { base: expr, .. } => walk_expr(expr, table),
            Expr::Range { lo, hi, .. } => {
                walk_expr(lo, table);
                walk_expr(hi, table);
            }
            Expr::Index { base, index } => {
                walk_expr(base, table);
                walk_expr(index, table);
            }
            Expr::LabeledCall { .. } => {
                unreachable!("RFC-0056: labeled calls are lowered to positional Call before codegen")
            }
            Expr::Record { fields, spread, .. } => {
                for (_, v) in fields {
                    walk_expr(v, table);
                }
                if let Some(sp) = spread {
                    walk_expr(sp, table);
                }
            }
            Expr::RecordUpdate { name: _, base, fields } => {
                walk_expr(base, table);
                for (_, v) in fields {
                    walk_expr(v, table);
                }
            }
            Expr::If { cond, then_block, else_block } => {
                walk_expr(cond, table);
                walk_block(then_block, table);
                if let Some(b) = else_block {
                    walk_block(b, table);
                }
            }
            Expr::Match { scrutinee, arms } => {
                walk_expr(scrutinee, table);
                for a in arms {
                    if let Some(g) = &mut a.guard {
                        walk_expr(g, table);
                    }
                    walk_expr(&mut a.body, table);
                }
            }
            Expr::While { cond, body } => {
                walk_expr(cond, table);
                walk_block(body, table);
            }
            Expr::WhileLet { scrutinee, body, .. } => {
                walk_expr(scrutinee, table);
                walk_block(body, table);
            }
            Expr::For { iter, body, .. } => {
                walk_expr(iter, table);
                walk_block(body, table);
            }
            Expr::Lambda { body, .. } => walk_block(body, table),
            Expr::Block(b) => walk_block(b, table),
            Expr::Int(_) | Expr::Duration(_) | Expr::Float(_) | Expr::Str(_)
            | Expr::Bool(_) | Expr::Var(_) | Expr::TaggedLit { .. } => {}
        }
    }
    fn walk_block(b: &mut Block, table: &witchy_types::typeck::TypeTable) {
        for st in &mut b.stmts {
            match st {
                Stmt::Let { value, .. }
                | Stmt::Assign { value, .. }
                | Stmt::LetPattern { value, .. }
                | Stmt::Return(Some(value))
                | Stmt::Expr(value)
                | Stmt::Yield(value) => walk_expr(value, table),
                Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
            }
        }
    }
    for item in &mut m.items {
        match item {
            Item::Function(f) => walk_block(&mut f.body, table),
            Item::Impl(im) => {
                for f in &mut im.methods {
                    walk_block(&mut f.body, table);
                }
            }
            Item::Trait(t) => {
                for msig in &mut t.methods {
                    if let Some(b) = &mut msig.default {
                        walk_block(b, table);
                    }
                }
            }
            Item::Const { value, .. } => walk_expr(value, table),
            Item::Type(_) | Item::TypeAlias { .. } | Item::Comptime(_) => {}
        }
    }
}

pub(crate) fn alpha_rename_module(m: &mut Module) {
    for item in &mut m.items {
        if let Item::Function(f) = item {
            f.body = alpha_rename(&f.body, &f.params);
        }
    }
}

/// The `e ? "msg"` desugar (`__try_ctx(operand, msg)`) rewritten to a concrete
/// std call by the operand's type: `Option` -> `option.ok_or(operand, msg)`,
/// `Result` -> `result.map_err(operand, fn(__ctx_err): msg + ": " + __ctx_err)`.
/// The `+` stays `Add`; the later `flip_string_add_module` turns it into `Concat`.
/// Returns true if any node was rewritten (so the caller re-annotates, since
/// moved/new nodes change the address-keyed `TypeTable`).
pub(crate) fn rewrite_try_ctx_module(m: &mut Module, table: &witchy_types::typeck::TypeTable) -> bool {
    fn replacement(is_option: bool, operand: Expr, msg: Expr) -> Expr {
        if is_option {
            return Expr::Call { name: "option.ok_or".into(), args: vec![operand, msg] };
        }
        Expr::Call {
            name: "result.map_err".into(),
            args: vec![
                operand,
                Expr::Lambda {
                    params: vec![Param {
                        name: "__ctx_err".into(),
                        ty: None,
                        convention: Convention::default(),
                        default: None,
                    }],
                    body: Block {
                        stmts: vec![Stmt::Expr(Expr::Binary {
                            op: BinOp::Add,
                            lhs: Box::new(Expr::Binary {
                                op: BinOp::Add,
                                lhs: Box::new(msg),
                                rhs: Box::new(Expr::Str(": ".into())),
                            }),
                            rhs: Box::new(Expr::Var("__ctx_err".into())),
                        })],
                        lines: vec![0],
                        region: None,
                    },
                    ret: None,
                },
            ],
        }
    }
    fn walk_expr(e: &mut Expr, table: &witchy_types::typeck::TypeTable, changed: &mut bool) {
        match e {
            Expr::List(xs) | Expr::Tuple(xs) | Expr::Ctor { args: xs, .. }
            | Expr::AnonCtor { args: xs, .. }
            | Expr::Call { args: xs, .. } => {
                for x in xs {
                    walk_expr(x, table, changed);
                }
            }
            Expr::Apply { func, args } => {
                walk_expr(func, table, changed);
                for a in args {
                    walk_expr(a, table, changed);
                }
            }
            Expr::MethodCall { receiver, args, .. } => {
                walk_expr(receiver, table, changed);
                for a in args {
                    walk_expr(a, table, changed);
                }
            }
            Expr::Binary { lhs, rhs, .. } => {
                walk_expr(lhs, table, changed);
                walk_expr(rhs, table, changed);
            }
            Expr::Unary { expr, .. }
            | Expr::Try(expr)
            | Expr::As { expr, .. }
            | Expr::Field { base: expr, .. } => walk_expr(expr, table, changed),
            Expr::Range { lo, hi, .. } => {
                walk_expr(lo, table, changed);
                walk_expr(hi, table, changed);
            }
            Expr::Index { base, index } => {
                walk_expr(base, table, changed);
                walk_expr(index, table, changed);
            }
            Expr::LabeledCall { .. } => {
                unreachable!("RFC-0056: labeled calls are lowered to positional Call before codegen")
            }
            Expr::Record { fields, spread, .. } => {
                for (_, v) in fields {
                    walk_expr(v, table, changed);
                }
                if let Some(sp) = spread {
                    walk_expr(sp, table, changed);
                }
            }
            Expr::RecordUpdate { name: _, base, fields } => {
                walk_expr(base, table, changed);
                for (_, v) in fields {
                    walk_expr(v, table, changed);
                }
            }
            Expr::If { cond, then_block, else_block } => {
                walk_expr(cond, table, changed);
                walk_block(then_block, table, changed);
                if let Some(b) = else_block {
                    walk_block(b, table, changed);
                }
            }
            Expr::Match { scrutinee, arms } => {
                walk_expr(scrutinee, table, changed);
                for a in arms {
                    if let Some(g) = &mut a.guard {
                        walk_expr(g, table, changed);
                    }
                    walk_expr(&mut a.body, table, changed);
                }
            }
            Expr::While { cond, body } => {
                walk_expr(cond, table, changed);
                walk_block(body, table, changed);
            }
            Expr::WhileLet { scrutinee, body, .. } => {
                walk_expr(scrutinee, table, changed);
                walk_block(body, table, changed);
            }
            Expr::For { iter, body, .. } => {
                walk_expr(iter, table, changed);
                walk_block(body, table, changed);
            }
            Expr::Lambda { body, .. } => walk_block(body, table, changed),
            Expr::Block(b) => walk_block(b, table, changed),
            Expr::Int(_) | Expr::Duration(_) | Expr::Float(_) | Expr::Str(_)
            | Expr::Bool(_) | Expr::Var(_) | Expr::TaggedLit { .. } => {}
        }
        // After recursing into children, rewrite this node if it is `__try_ctx`.
        // Read the operand type BEFORE moving (the table is keyed by node address).
        let is_try = matches!(e, Expr::Call { name, args } if name == intrinsics::TRY_CONTEXT && args.len() == 2);
        if is_try {
            let is_option = if let Expr::Call { args, .. } = &*e {
                matches!(
                    table.type_of(&args[0]).and_then(witchy_types::typeck::ty_to_ast),
                    Some(Type::Named(n, _)) if n == "Option"
                )
            } else {
                false
            };
            if let Expr::Call { args, .. } = std::mem::replace(e, Expr::Bool(false)) {
                let mut it = args.into_iter();
                let operand = it.next().unwrap();
                let msg = it.next().unwrap();
                *e = replacement(is_option, operand, msg);
                *changed = true;
            }
        }
    }
    fn walk_block(b: &mut Block, table: &witchy_types::typeck::TypeTable, changed: &mut bool) {
        for st in &mut b.stmts {
            match st {
                Stmt::Let { value, .. }
                | Stmt::Assign { value, .. }
                | Stmt::LetPattern { value, .. }
                | Stmt::Return(Some(value))
                | Stmt::Expr(value)
                | Stmt::Yield(value) => walk_expr(value, table, changed),
                Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
            }
        }
    }
    let mut changed = false;
    for item in &mut m.items {
        match item {
            Item::Function(f) => walk_block(&mut f.body, table, &mut changed),
            Item::Impl(im) => {
                for f in &mut im.methods {
                    walk_block(&mut f.body, table, &mut changed);
                }
            }
            Item::Trait(t) => {
                for msig in &mut t.methods {
                    if let Some(b) = &mut msig.default {
                        walk_block(b, table, &mut changed);
                    }
                }
            }
            Item::Const { value, .. } => walk_expr(value, table, &mut changed),
            Item::Type(_) | Item::TypeAlias { .. } | Item::Comptime(_) => {}
        }
    }
    changed
}

fn alpha_rename(body: &Block, params: &[Param]) -> Block {
    let mut r = Renamer::new();
    r.scopes.push(HashMap::new());
    for p in params {
        r.declare(&p.name);
    }
    let mut b = body.clone();
    r.rename_block(&mut b);
    b
}
