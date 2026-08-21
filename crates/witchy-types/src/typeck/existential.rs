//! RFC-0081 existential (`dyn Trait`) frontend validation.
//!
//! A self-contained pass extracted verbatim from the main checker. It validates
//! every `dyn Trait(args…)` occurrence in the module — signatures, fields,
//! type-alias right-hand sides, where-clause trait arguments, and body type
//! positions — against the frontend contract: the trait must exist, its arity
//! must match, every trait type parameter must be fixed by a concrete type, the
//! trait must be existential-safe, and borrowed existentials are excluded in v1.
//! None of it touches checker state — the parent module drives
//! `check_existential_types` — so it lives here to keep the checker file focused
//! on inference. `existential_bare` is the shared bare-name helper the checker
//! also reuses when rendering `dyn` diagnostics.

use foldhash::{HashMap, HashMapExt as _, HashSet, HashSetExt as _};

use witchy_syntax::ast::{self, Block, Expr, Item, Stmt};

use super::{terr, TypeError, AMBIENT_TRAIT_NAMES};

/// Validate every `dyn Trait(args…)` occurrence in the
/// module — in function/method signatures, record/sum fields, type-alias
/// right-hand sides, where-clause trait arguments, and body type positions
/// (`let` annotations, `as` targets, lambda parameter/return types) — against
/// the frontend contract: the trait must exist, its arity must match, every
/// trait type parameter must be fixed by a concrete type, the trait must be
/// existential-safe, and borrowed existentials are excluded in v1.
///
/// Runs right after `check_trait_names`, before trait lowering, so `Item::Trait`
/// declarations are still present.
pub(super) fn check_existential_types(items: &[Item]) -> Result<(), TypeError> {
    let mut traits: HashMap<&str, &ast::TraitDef> = HashMap::new();
    for item in items {
        if let Item::Trait(tr) = item {
            traits.insert(tr.name.as_str(), tr);
        }
    }
    let mut check = ExistentialCheck { traits };
    for item in items {
        match item {
            Item::Function(f) => check.visit_function(f)?,
            Item::Type(t) => {
                for variant in &t.variants {
                    for field in &variant.fields {
                        check.visit_type(field)?;
                    }
                }
            }
            Item::Trait(tr) => {
                for method in &tr.methods {
                    for param in &method.params {
                        if let Some(ty) = &param.ty {
                            check.visit_type(ty)?;
                        }
                    }
                    if let Some(ret) = &method.ret {
                        check.visit_type(ret)?;
                    }
                    if let Some(default) = &method.default {
                        check.visit_block(default)?;
                    }
                }
            }
            Item::Impl(im) => {
                for arg in im.trait_args.iter().chain(&im.target_args) {
                    check.visit_type(arg)?;
                }
                for (_, _, trait_args) in &im.bounds {
                    for arg in trait_args {
                        check.visit_type(arg)?;
                    }
                }
                for method in &im.methods {
                    check.visit_function(method)?;
                }
            }
            Item::TypeAlias { ty, .. } => check.visit_type(ty)?,
            Item::Const { value, .. } => check.visit_expr(value)?,
            Item::Comptime(block) => check.visit_block(block)?,
        }
    }
    Ok(())
}

pub(super) fn existential_bare(name: &str) -> &str {
    name.rsplit('.').next().unwrap_or(name)
}

/// Whether `Self` appears anywhere in `t` (any nesting depth).
fn type_mentions_self(t: &ast::Type) -> bool {
    match t {
        ast::Type::Named(n, args) => n == "Self" || args.iter().any(type_mentions_self),
        ast::Type::Dyn(_, args) => args.iter().any(type_mentions_self),
        ast::Type::Tuple(items) => items.iter().any(type_mentions_self),
        ast::Type::Fn(params, ret, _, _) => {
            params.iter().any(type_mentions_self) || type_mentions_self(ret)
        }
        ast::Type::RecordCompose { base, fields } => {
            type_mentions_self(base) || fields.iter().any(|(_, ty)| type_mentions_self(ty))
        }
        ast::Type::Qualified(_, inner) => type_mentions_self(inner),
        ast::Type::Slice(inner) => type_mentions_self(inner),
    }
}

struct ExistentialCheck<'a> {
    /// Declared traits, keyed by resolved declaration identity.
    traits: HashMap<&'a str, &'a ast::TraitDef>,
}

impl<'a> ExistentialCheck<'a> {
    fn visit_function(&mut self, f: &ast::Function) -> Result<(), TypeError> {
        for p in &f.params {
            if let Some(ty) = &p.ty {
                self.visit_type(ty)?;
            }
        }
        if let Some(ret) = &f.ret {
            self.visit_type(ret)?;
        }
        for (_, _, trait_args) in &f.bounds {
            for arg in trait_args {
                self.visit_type(arg)?;
            }
        }
        self.visit_block(&f.body)
    }

    fn visit_block(&mut self, block: &Block) -> Result<(), TypeError> {
        if let Some(region) = &block.region {
            if let Some(ty) = &region.ty {
                self.visit_type(ty)?;
            }
        }
        block
            .stmts
            .iter()
            .try_for_each(|stmt| self.visit_stmt(stmt))
    }

    fn visit_stmt(&mut self, stmt: &Stmt) -> Result<(), TypeError> {
        match stmt {
            Stmt::Let { ty, value, .. } => {
                if let Some(ty) = ty {
                    self.visit_type(ty)?;
                }
                self.visit_expr(value)
            }
            Stmt::Assign { value, .. }
            | Stmt::LetPattern { value, .. }
            | Stmt::Return(Some(value))
            | Stmt::Yield(value)
            | Stmt::Expr(value) => self.visit_expr(value),
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => Ok(()),
        }
    }

    fn visit_expr(&mut self, expr: &Expr) -> Result<(), TypeError> {
        match expr {
            Expr::Int(_)
            | Expr::Float(_)
            | Expr::Duration(_)
            | Expr::Str(_)
            | Expr::Bool(_)
            | Expr::Var(_)
            | Expr::TaggedLit { .. } => Ok(()),
            Expr::List(values) | Expr::Tuple(values) => {
                values.iter().try_for_each(|value| self.visit_expr(value))
            }
            Expr::Call { args, .. } | Expr::Ctor { args, .. } | Expr::AnonCtor { args, .. } => {
                args.iter().try_for_each(|arg| self.visit_expr(arg))
            }
            Expr::LabeledCall { args, .. } => {
                args.iter().try_for_each(|(_, arg)| self.visit_expr(arg))
            }
            Expr::LabeledMethodCall { receiver, args, .. } => {
                self.visit_expr(receiver)?;
                args.iter().try_for_each(|(_, arg)| self.visit_expr(arg))
            }
            Expr::MethodCall { receiver, args, .. } => {
                self.visit_expr(receiver)?;
                args.iter().try_for_each(|arg| self.visit_expr(arg))
            }
            Expr::ExistentialCall {
                receiver,
                args,
                ty,
                result,
                ..
            } => {
                self.visit_expr(receiver)?;
                args.iter().try_for_each(|arg| self.visit_expr(arg))?;
                self.visit_type(ty)?;
                self.visit_type(result)
            }
            Expr::Apply { func, args } => {
                self.visit_expr(func)?;
                args.iter().try_for_each(|arg| self.visit_expr(arg))
            }
            Expr::Unary { expr, .. } | Expr::Field { base: expr, .. } | Expr::Try(expr) => {
                self.visit_expr(expr)
            }
            Expr::As { expr, ty } => {
                self.visit_expr(expr)?;
                self.visit_type(ty)
            }
            Expr::ExistentialPack { expr, ty, .. } => {
                self.visit_expr(expr)?;
                self.visit_type(ty)
            }
            Expr::ExistentialUpcast { expr, ty } => {
                self.visit_expr(expr)?;
                self.visit_type(ty)
            }
            Expr::Lambda { params, body, ret, .. } => {
                for param in params {
                    if let Some(ty) = &param.ty {
                        self.visit_type(ty)?;
                    }
                }
                if let Some(ret) = ret {
                    self.visit_type(ret)?;
                }
                self.visit_block(body)
            }
            Expr::RecordUpdate { base, fields, .. } => {
                self.visit_expr(base)?;
                fields
                    .iter()
                    .try_for_each(|(_, value)| self.visit_expr(value))
            }
            Expr::Record { fields, spread, .. } => {
                fields
                    .iter()
                    .try_for_each(|(_, value)| self.visit_expr(value))?;
                if let Some(base) = spread {
                    self.visit_expr(base)?;
                }
                Ok(())
            }
            Expr::Binary { lhs, rhs, .. }
            | Expr::Range {
                lo: lhs, hi: rhs, ..
            } => {
                self.visit_expr(lhs)?;
                self.visit_expr(rhs)
            }
            Expr::If {
                cond,
                then_block,
                else_block,
            } => {
                self.visit_expr(cond)?;
                self.visit_block(then_block)?;
                if let Some(block) = else_block {
                    self.visit_block(block)?;
                }
                Ok(())
            }
            Expr::Match { scrutinee, arms } => {
                self.visit_expr(scrutinee)?;
                for arm in arms {
                    if let Some(guard) = &arm.guard {
                        self.visit_expr(guard)?;
                    }
                    self.visit_expr(&arm.body)?;
                }
                Ok(())
            }
            Expr::Block(block) => self.visit_block(block),
            Expr::While { cond, body } => {
                self.visit_expr(cond)?;
                self.visit_block(body)
            }
            Expr::For { iter, body, .. } => {
                self.visit_expr(iter)?;
                self.visit_block(body)
            }
            Expr::Index { base, index } => {
                self.visit_expr(base)?;
                self.visit_expr(index)
            }
            Expr::WhileLet {
                scrutinee, body, ..
            } => {
                self.visit_expr(scrutinee)?;
                self.visit_block(body)
            }
        }
    }

    /// The one chokepoint every `ast::Type` in the module flows through.
    fn visit_type(&mut self, t: &ast::Type) -> Result<(), TypeError> {
        match t {
            // (v1 exclusion) A dyn directly wrapped in a borrow. `frozen` /
            // `unique` wrapping stays legal (the Qualified arm below).
            ast::Type::Qualified(
                ast::TypeQual::Borrow(_)
                | ast::TypeQual::LegacyBorrow(_)
                | ast::TypeQual::BorrowMut(_),
                inner,
            ) if matches!(inner.unqualified(), ast::Type::Dyn(..)) => {
                let ast::Type::Dyn(name, _) = inner.unqualified() else {
                    unreachable!("guard matched Dyn");
                };
                terr(format!(
                    "borrowed existential values (`View(dyn {name}, 'a)` / `let('a) dyn {name}`) \
                     are excluded from RFC-0081 v1"
                ))
            }
            ast::Type::Qualified(_, inner) => self.visit_type(inner),
            ast::Type::Slice(inner) => self.visit_type(inner),
            ast::Type::Tuple(items) => items.iter().try_for_each(|i| self.visit_type(i)),
            ast::Type::Fn(params, ret, _, _) => {
                params.iter().try_for_each(|p| self.visit_type(p))?;
                self.visit_type(ret)
            }
            ast::Type::RecordCompose { base, fields } => {
                self.visit_type(base)?;
                fields.iter().try_for_each(|(_, ty)| self.visit_type(ty))
            }
            ast::Type::Named(_, args) => args.iter().try_for_each(|a| self.visit_type(a)),
            ast::Type::Dyn(name, args) => {
                self.check_dyn(t, name, args)?;
                args.iter().try_for_each(|a| self.visit_type(a))
            }
        }
    }

    fn check_dyn(
        &mut self,
        _whole: &ast::Type,
        name: &str,
        args: &[ast::Type],
    ) -> Result<(), TypeError> {
        let display_name = existential_bare(name);
        let args_rendered = if args.is_empty() {
            String::new()
        } else {
            format!(
                "({})",
                args.iter()
                    .map(witchy_syntax::format::type_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        let rendered = format!("dyn {display_name}{args_rendered}");
        let Some(tr) = self.traits.get(name).copied() else {
            // The ambient built-in comparison traits exist without a local
            // declaration; every one of them is Self-binary, which the RFC
            // calls out explicitly as existential-unsafe.
            if AMBIENT_TRAIT_NAMES.contains(&display_name) {
                return terr(ambient_dyn_unsafe_msg(display_name));
            }
            return terr(format!(
                "unknown trait `{display_name}` in `dyn {display_name}`"
            ));
        };
        if tr.typarams.len() != args.len() {
            return terr(format!(
                "trait `{display_name}` expects {} type argument(s) but got {} in `{rendered}`",
                tr.typarams.len(),
                args.len()
            ));
        }
        for arg in args {
            let mut vars = Vec::new();
            ast::collect_type_vars(arg, &mut vars);
            if let Some(var) = vars.first() {
                return terr(format!(
                    "`{rendered}`: every trait type parameter must be fixed by a concrete \
                     type — `{var}` is an unresolved type parameter"
                ));
            }
        }
        let mut violations: Vec<String> = Vec::new();
        let mut seen: HashSet<&str> = HashSet::new();
        self.collect_safety_violations(tr, &mut seen, &mut violations);
        if !violations.is_empty() {
            return terr(format!(
                "trait `{display_name}` is not existential-safe as `dyn {display_name}`: {}",
                violations.join("; ")
            ));
        }
        Ok(())
    }

    /// Collect every existential-safety violation over the trait's own methods
    /// AND its transitive supertraits' methods (the whole callable surface of
    /// `dyn Trait`), so the one diagnostic names every blocking method.
    fn collect_safety_violations(
        &self,
        tr: &'a ast::TraitDef,
        seen: &mut HashSet<&'a str>,
        out: &mut Vec<String>,
    ) {
        if !seen.insert(tr.name.as_str()) {
            return;
        }
        for method in &tr.methods {
            method_safety_violations(&tr.typarams, method, out);
        }
        for supertrait in &tr.supertraits {
            match self.traits.get(supertrait.as_str()) {
                Some(sup) => self.collect_safety_violations(sup, seen, out),
                None => {
                    // An ambient built-in supertrait contributes Self-binary
                    // comparison methods to the callable surface. Any other
                    // unresolved name was already rejected by
                    // `check_trait_names`.
                    let bare = existential_bare(supertrait);
                    if AMBIENT_TRAIT_NAMES.contains(&bare) && seen.insert(bare) {
                        out.push(format!(
                            "supertrait `{bare}`'s methods take a second `Self` parameter — \
                             `Self` may appear only in receiver position"
                        ));
                    }
                }
            }
        }
    }
}

/// The rule-6 diagnostic for `dyn PartialEq` / `dyn Eq` / `dyn PartialOrd` /
/// `dyn Ord`: the ambient comparison traits are Self-binary by construction.
fn ambient_dyn_unsafe_msg(name: &str) -> String {
    format!(
        "trait `{name}` is not existential-safe as `dyn {name}`: its methods take a \
         second `Self` parameter — `Self` may appear only in receiver position"
    )
}

/// Check one trait method against the RFC-0081 existential-safety rules,
/// appending one entry per violated rule.
fn method_safety_violations(
    trait_typarams: &[String],
    method: &ast::MethodSig,
    out: &mut Vec<String>,
) {
    // Rule 1: the method must have a receiver (first parameter named `self`).
    let has_receiver = method.params.first().is_some_and(|p| p.name == "self");
    if !has_receiver {
        out.push(format!("method `{}` has no receiver", method.name));
    }
    let value_params = if has_receiver {
        &method.params[1..]
    } else {
        &method.params[..]
    };

    // Rule 2: no method-local type parameters — every type variable in the
    // non-receiver parameters and the return must be one of the trait's own.
    let mut vars = Vec::new();
    for p in value_params {
        if let Some(ty) = &p.ty {
            ast::collect_type_vars(ty, &mut vars);
        }
    }
    if let Some(ret) = &method.ret {
        ast::collect_type_vars(ret, &mut vars);
    }
    for var in vars {
        if !trait_typarams.contains(&var) {
            out.push(format!(
                "method `{}` introduces method-local type parameter `{var}`",
                method.name
            ));
        }
    }

    // Rule 3: the return type must not be bare `Self` (the hidden concrete
    // type cannot be reconstructed as the static result).
    let bare_self_ret = matches!(
        method.ret.as_ref().map(|r| r.unqualified()),
        Some(ast::Type::Named(n, a)) if n == "Self" && a.is_empty()
    );
    if bare_self_ret {
        out.push(format!("method `{}` returns bare `Self`", method.name));
    }

    // Rule 4: `Self` appears nowhere except the receiver — not in non-receiver
    // parameters, not nested in the return (bare returns are rule 3's).
    let self_in_params = value_params
        .iter()
        .any(|p| p.ty.as_ref().is_some_and(type_mentions_self));
    let self_nested_in_ret = !bare_self_ret && method.ret.as_ref().is_some_and(type_mentions_self);
    if self_in_params || self_nested_in_ret {
        out.push(format!(
            "method `{}` mentions `Self` outside the receiver",
            method.name
        ));
    }

    // Rule 5 (v1): no result borrowed from the hidden receiver.
    if matches!(
        method.ret,
        Some(ast::Type::Qualified(
            ast::TypeQual::Borrow(_) | ast::TypeQual::LegacyBorrow(_) | ast::TypeQual::BorrowMut(_),
            _,
        ))
    ) {
        out.push(format!(
            "method `{}` returns a result borrowed from the hidden receiver",
            method.name
        ));
    }
}
