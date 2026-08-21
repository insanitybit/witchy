//! Monomorphization walk for substitution-directed trait dispatch.
//!
//! Extracted verbatim from `traits.rs` (RFC-0046 typed trait dispatch). `Mono`
//! walks each function body and, using typeck's `TypeTable` plus the declared
//! signatures, resolves bounded trait-method calls (`where a: Show`) to the
//! concrete impl method for the receiver's inferred type, generating the needed
//! monomorphic specializations. The `Mono` struct itself lives in `traits.rs`
//! (constructed by `lower_with`); this module carries only its `impl` block.

use foldhash::{HashMap, HashMapExt as _, HashSet, HashSetExt as _};

use witchy_syntax::ast::*;

use super::*;

impl Mono<'_> {
    fn type_carries_once_identity(&self, ty: &Type) -> bool {
        fn go(mono: &Mono<'_>, ty: &Type, seen: &mut HashSet<String>) -> bool {
            match ty {
                Type::Qualified(_, inner) | Type::Slice(inner) => go(mono, inner, seen),
                Type::Fn(_, _, _, qualifiers) => qualifiers.once,
                Type::Tuple(items) => items.iter().any(|item| go(mono, item, seen)),
                // Existential arguments describe the method surface, not the
                // hidden concrete payload. Packing is checked separately.
                Type::Dyn(..) => false,
                Type::RecordCompose { base, fields } => {
                    go(mono, base, seen)
                        || fields.iter().any(|(_, field)| go(mono, field, seen))
                }
                Type::Named(name, arguments) => {
                    // Recursing into an opaque/builtin argument may specialize
                    // a phantom parameter unnecessarily, but never classifies
                    // storage as affine; the concrete type checker owns that
                    // decision. Declared payloads below close zero-argument and
                    // nominal-field cases such as `OnceBox(once fn())`.
                    if arguments.iter().any(|argument| go(mono, argument, seen)) {
                        return true;
                    }
                    if !seen.insert(name.clone()) {
                        return false;
                    }
                    let found = mono
                        .ctor_infos
                        .values()
                        .filter(|info| info.owner == *name)
                        .any(|info| {
                            let substitution = info
                                .params
                                .iter()
                                .cloned()
                                .zip(arguments.iter().cloned())
                                .collect::<HashMap<_, _>>();
                            info.fields.iter().any(|field| {
                                go(mono, &subst_trait_params(field, &substitution), seen)
                            })
                        });
                    seen.remove(name);
                    found
                }
            }
        }
        go(self, ty, &mut HashSet::new())
    }

    pub(super) fn seed_specialization(&mut self, name: &str, type_args: Vec<Type>) {
        self.specialize(name, type_args);
    }

    pub(super) fn run(&mut self, items: &mut [Item]) {
        for item in items.iter_mut() {
            if let Item::Function(f) = item {
                if self.skip_walk.contains(&f.name) {
                    continue;
                }
                self.current_function.clone_from(&f.name);
                let mut s = Scope::new();
                seed_typed_params(&f.params, &mut s);
                self.walk_block(&mut f.body, &mut s);
            }
        }
        // A specialization may itself call a template; walk the bodies we
        // generate (the list grows as we go).
        let mut i = 0;
        while i < self.generated.len() {
            self.current_function.clone_from(&self.generated[i].name);
            let params = self.generated[i].params.clone();
            let mut body = std::mem::replace(
                &mut self.generated[i].body,
                Block { stmts: Vec::new(), lines: Vec::new(), region: None },
            );
            let mut s = Scope::new();
            seed_typed_params(&params, &mut s);
            self.walk_block(&mut body, &mut s);
            self.generated[i].body = body;
            i += 1;
        }
    }

    fn type_ast(&self, e: &Expr, scope: &Scope<Type>) -> Option<Type> {
        table_ast_type(self.table, e)
            .or_else(|| declared_expr_type(e, &self.fn_sigs, &|arg| self.type_ast(arg, scope)))
            .or_else(|| {
                local_expr_type(
                    e,
                    scope,
                    self.ctor_infos,
                    &self.fn_sigs,
                    self.record_fields,
                    &|arg| self.type_ast(arg, scope),
                )
            })
            .or_else(|| cap_op_result_type(e, &|arg| self.type_ast(arg, scope)))
    }

    fn refine_var_call_args(&self, name: &str, args: &[Expr], scope: &mut Scope<Type>) {
        let Some((params, _, conventions, _)) = self.fn_sigs.get(name) else { return };
        let mut bindings = HashMap::new();
        for (param, arg) in params.iter().zip(args) {
            let (Some(pattern), Some(actual)) = (param, self.type_ast(arg, scope)) else {
                continue;
            };
            let _ = bind_ast_type_vars(pattern, &actual, &mut bindings);
        }
        for ((param, convention), arg) in params.iter().zip(conventions).zip(args) {
            if *convention != Convention::Var {
                continue;
            }
            let (Some(pattern), Expr::Var(binding)) = (param, arg) else { continue };
            let refined = subst_trait_params(pattern, &bindings);
            refine_ast_scope_type(scope, binding, &refined);
        }
    }

    fn resolve_type_args(
        &self,
        template: &Function,
        args: &[Expr],
        scope: &Scope<Type>,
        result_ty: Option<&Type>,
    ) -> Option<Vec<Type>> {
        let requires_specialization =
            !template.bounds.is_empty() || self.skip_walk.contains(&template.name);
        let mut bindings = HashMap::new();
        let mut table_confirmed = HashSet::new();

        for (param, arg) in template.params.iter().zip(args) {
            let Some(pattern) = &param.ty else { continue };
            let from_table = table_ast_type(self.table, arg);
            let actual = from_table.clone().or_else(|| self.type_ast(arg, scope));
            let Some(actual) = actual else { continue };
            if !bind_ast_type_vars(pattern, &actual, &mut bindings) {
                return None;
            }
            if let Some(table_ty) = from_table {
                let mut confirmed = HashMap::new();
                if bind_ast_type_vars(pattern, &table_ty, &mut confirmed) {
                    table_confirmed.extend(confirmed.into_keys());
                }
            }
        }

        if let (Some(ret), Some(actual)) = (&template.ret, result_ty) {
            if !bind_ast_type_vars(ret, actual, &mut bindings) {
                return None;
            }
            let mut confirmed = HashMap::new();
            if bind_ast_type_vars(ret, actual, &mut confirmed) {
                table_confirmed.extend(confirmed.into_keys());
            }
        }

        type_var_list(template)
            .into_iter()
            .map(|var| {
                let ty = bindings.get(&var)?.clone();
                let mut unresolved = Vec::new();
                collect_type_vars(&ty, &mut unresolved);
                if !unresolved.is_empty() {
                    return None;
                }
                let key = type_key(ty.unqualified());
                if !requires_specialization
                    && !self.type_carries_once_identity(&ty)
                    && (!self.mono_unbounded
                        || (!table_confirmed.contains(&var)
                            && !is_specializable_type_arg(&key)))
                {
                    return None;
                }
                Some(ty)
            })
            .collect()
    }

    fn resolve_function_value_type_args(
        &self,
        template: &Function,
        actual: &Type,
    ) -> Option<Vec<Type>> {
        let Type::Fn(actual_params, actual_ret, actual_conventions, actual_qualifiers) = actual.unqualified() else {
            return None;
        };
        if template.params.len() != actual_params.len()
            || *actual_qualifiers != CallableQualifiers::new(template.pure, false)
            || template
                .params
                .iter()
                .map(|param| param.convention)
                .ne(actual_conventions.iter().copied())
        {
            return None;
        }
        let mut bindings = HashMap::new();
        for (param, actual_param) in template.params.iter().zip(actual_params) {
            if let Some(pattern) = &param.ty
                && !bind_ast_type_vars(pattern, actual_param, &mut bindings)
            {
                return None;
            }
        }
        if let Some(ret) = &template.ret
            && !bind_ast_type_vars(ret, actual_ret, &mut bindings)
        {
            return None;
        }
        type_var_list(template)
            .into_iter()
            .map(|var| {
                let ty = bindings.get(&var)?.clone();
                let mut unresolved = Vec::new();
                collect_type_vars(&ty, &mut unresolved);
                unresolved.is_empty().then_some(ty)
            })
            .collect()
    }

    fn specialize(&mut self, name: &str, type_args: Vec<Type>) -> String {
        let affine = type_args
            .iter()
            .any(|argument| self.type_carries_once_identity(argument));
        let identity = LogicalSpecializationIdentity::from_types(&type_args);
        let key = (name.to_string(), identity.clone());
        if let Some(m) = self.memo.get(&key) {
            return m.clone();
        }
        // The canonical type rendering is emitted only as an identifier key; it
        // is never parsed back into a type.
        let safe: Vec<String> = identity
            .types()
            .iter()
            .map(|key| mangle_type_key(key.as_str()))
            .collect();
        let mangled = format!("{name}__{}", safe.join("__"));
        self.memo.insert(key, mangled.clone());
        self.specializations.insert(mangled.clone(), identity);
        if affine {
            self.affine_specializations.insert(mangled.clone());
        }

        let mut f = self.templates[name].clone();
        f.name = mangled.clone();
        // Substitute over the same variable list `resolve_type_args` resolved:
        // the `where`-bound variables for a bounded generic, otherwise the free
        // type variables of the signature.
        let subst: HashMap<String, Type> = type_var_list(&f).into_iter().zip(type_args).collect();
        for p in &mut f.params {
            if let Some(t) = &p.ty {
                p.ty = Some(subst_trait_params(t, &subst));
            }
        }
        f.ret = f.ret.as_ref().map(|t| subst_trait_params(t, &subst));
        if let Some(ret) = &f.ret {
            let params = f.params.iter().map(|param| param.ty.clone()).collect();
            let conventions = f.params.iter().map(|param| param.convention).collect();
            self.fn_sigs
                .insert(
                    mangled.clone(),
                    (
                        params,
                        ret.clone(),
                        conventions,
                        CallableQualifiers::new(f.pure, false),
                    ),
                );
        }
        // Substitute the body's type ANNOTATIONS too (`var items: List(a) = []`,
        // `x as T`), so a specialization's body type-checks at the concrete type.
        subst_block_types(&mut f.body, &subst);
        // Substitution-directed trait dispatch: a bound variable's trait
        // methods resolve by the SUBSTITUTED type — not by any argument — so a
        // constructor-style method (`from_iter`, which mentions its bound
        // variable only in the RESULT) dispatches correctly, and the impl's
        // own generic method is specialized at the bound's type arguments.
        let trait_method_pairs: Vec<(String, String)> = self
            .trait_methods
            .iter()
            .flat_map(|(m, infos)| infos.iter().map(|info| (m.clone(), info.owner.clone())))
            .collect();
        let bounds_snapshot = f.bounds.clone();
        // Keyed by (concrete receiver-type head, method) — NOT method alone — so
        // two same-trait bounds (`where a: Named, b: Named`) each rewrite their own
        // variable's calls to their own impl instead of the last bound clobbering
        // the target for every call site (BUG-298).
        let mut renames: HashMap<(String, String), String> = HashMap::new();
        for (bvar, btrait, btargs) in &bounds_snapshot {
            let Some(concrete) = subst.get(bvar.as_str()) else { continue };
            let Some(head) = type_head_key(concrete) else { continue };
            for (method, owner) in &trait_method_pairs {
                // The bound discharges its own trait's methods AND those of every
                // supertrait (a `where a: Ord` bound also supplies `eq`/`less`).
                let owned_by_bound = owner == btrait
                    || self.supertraits.get(btrait).is_some_and(|s| s.contains(owner));
                if !owned_by_bound {
                    continue;
                }
                // The impl that defines this method is registered under its actual
                // owning trait. Parameterized traits can have several impls for
                // the same receiver head (`From(JsonError) for String`,
                // `From(TomlDecodeError) for String`), so choose the candidate
                // whose trait arguments match the substituted bound arguments.
                let mut candidates = self
                    .trait_impl_table
                    .get(&(owner.clone(), method.clone(), head.clone()))
                    .cloned()
                    .unwrap_or_default();
                if candidates.is_empty() {
                    candidates = self
                        .trait_impl_table
                        .iter()
                        .filter(|((tr, m, k), _)| {
                            tr == owner
                                && m == method
                                && k.chars().next().is_some_and(char::is_lowercase)
                                && !k.contains('.')
                        })
                        .flat_map(|(_, methods)| methods.clone())
                        .collect();
                }
                for candidate in candidates {
                    let mangled = candidate.mangled;
                    // Bind the impl method's own type variables by STRUCTURAL
                    // matching: each impl trait-argument pattern against the
                    // bound's (substituted) concrete argument. Anything that
                    // doesn't bind falls back to the generic impl function.
                    let mut bound_map: HashMap<String, Type> = HashMap::new();
                    let mut ok = candidate.trait_args.len() == btargs.len();
                    if ok {
                        for (pat, targ) in candidate.trait_args.iter().zip(btargs) {
                            let concrete_arg = subst_trait_params(targ, &subst);
                            if !bind_ast_type_vars(pat, &concrete_arg, &mut bound_map) {
                                ok = false;
                                break;
                            }
                        }
                    }
                    if !ok {
                        continue;
                    }

                    let mut target = mangled.clone();
                    let Some(tmpl) = self.templates.get(&mangled).cloned() else {
                        if self.known_fns.contains(&mangled) {
                            renames.insert((head.clone(), method.clone()), mangled.clone());
                            renames.insert((head.clone(), static_bound_marker(bvar, method)), mangled);
                        }
                        continue;
                    };
                    if ok {
                        let mut targs_out: Vec<Type> = Vec::new();
                        for v in type_var_list(&tmpl) {
                            match bound_map.get(&v) {
                                Some(c) => targs_out.push(c.clone()),
                                None => {
                                    ok = false;
                                    break;
                                }
                            }
                        }
                        if ok && !targs_out.is_empty() {
                            target = self.specialize(&mangled, targs_out);
                        }
                    }
                    if target == mangled {
                        if let Some(targs_out) =
                            type_args_from_receiver(&tmpl, concrete)
                                .filter(|args| !args.is_empty())
                        {
                            target = self.specialize(&mangled, targs_out);
                        }
                    }
                    if target != mangled {
                        renames.insert((head.clone(), mangled.clone()), target.clone());
                    }
                    renames.insert((head.clone(), method.clone()), target.clone());
                    renames.insert((head.clone(), static_bound_marker(bvar, method)), target);
                }
            }
        }
        if !renames.is_empty() {
            // Seed the specialization's own parameters as bound locals, so a
            // `fn`-typed parameter named like a trait method (a comparator) is
            // invoked as the passed function, not rewritten to the impl (BUG-001).
            let mut rename_scope = Scope::new();
            seed_typed_params(&f.params, &mut rename_scope);
            // Resolve a call's receiver type through the checker's tables and make
            // the (possibly generic) result concrete with THIS specialization's
            // substitution — so a field-access receiver (`self.fst`) resolves to
            // its instantiated type and each same-trait bound dispatches to its
            // own impl (BUG-298).
            let this = &*self;
            let resolve = move |e: &Expr, sc: &Scope<Type>| this.type_ast(e, sc);
            let rename_ctx = RenameCallContext {
                renames: &renames,
                resolve: &resolve,
                ctor_infos: self.ctor_infos,
            };
            rename_calls_block(&mut f.body, &mut rename_scope, &rename_ctx);
        }
        // Monomorphization discharges the `where` bounds: every bound type
        // variable is now a concrete type, and the trait obligation is satisfied
        // by the impl whose method this specialization's body resolves to.
        // Clearing them lets the (fully concrete) specialization compile on the
        // compiled backend, which has no notion of an unsatisfied generic bound.
        f.bounds = Vec::new();
        self.generated.push(f);
        mangled
    }

    fn walk_block(&mut self, b: &mut Block, scope: &mut Scope<Type>) {
        for (index, stmt) in b.stmts.iter_mut().enumerate() {
            self.current_line = b.lines.get(index).copied().unwrap_or(0);
            match stmt {
                Stmt::Let { name, ty, value, mutable } => {
                    self.walk_expr(value, scope);
                    // Prefer the type ascription (`var items: List(a) = []`): it
                    // carries the element type an empty/ambiguous value loses. The
                    // value's inferred type is the fallback.
                    let resolved = ty.clone().or_else(|| self.type_ast(value, scope));
                    match (resolved, *mutable) {
                        (Some(t), true) => scope.insert_mut(name.clone(), t),
                        (Some(t), false) => scope.insert(name.clone(), t),
                        (None, true) => scope.bind_local_mut(name),
                        (None, false) => scope.bind_local(name),
                    }
                }
                Stmt::Assign { name, value } => {
                    self.walk_expr(value, scope);
                    if let Some(t) = self.type_ast(value, scope) {
                        scope.insert(name.clone(), t);
                    }
                }
                // `let PAT = t` seeds each destructured name from the value's type
                // so a destructured part monomorphizes (e.g. a tuple impl's
                // `reflect_one(x0)`). A tuple pattern recurses per slot; other
                // patterns clear their names (untyped) so a stale outer binding
                // doesn't leak in.
                Stmt::LetPattern { pattern, value } => {
                    self.walk_expr(value, scope);
                    let ty = self.type_ast(value, scope);
                    bind_typed_pattern(pattern, self.ctor_infos, ty.as_ref(), scope);
                }
                Stmt::Return(Some(e)) | Stmt::Expr(e) | Stmt::Yield(e) => self.walk_expr(e, scope),
                Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
            }
        }
    }

    fn walk_expr(&mut self, e: &mut Expr, scope: &mut Scope<Type>) {
        let result_ty = table_ast_type(self.table, e);
        match e {
            Expr::Call { name, args } => {
                for a in args.iter_mut() {
                    self.walk_expr(a, scope);
                }
                if !scope.is_local(name) {
                    self.refine_var_call_args(name, args, scope);
                }
                // (RFC-0053) Interpolation desugars to the internal render intrinsic,
                // the structural fallback. At this point monomorphization has concrete
                // type evidence for `x`, so values whose public display model is `Show`
                // route through `show.render` and then specialize like any other
                // bounded generic. Production linking always supplies `show`.
                if is_render_intrinsic(name) && args.len() == 1 {
                    if self.render_available {
                        if let Some(ty) = self.table.type_of(&args[0]) {
                            if render_needs_show(ty, self.show_types) {
                                *name = "show.render".to_string();
                            }
                        }
                    }
                }
                if matches!(
                    name.as_str(),
                    "template_child_value" | "glamour.template_child_value"
                ) && args.len() == 1
                {
                    let argument_type = self.type_ast(&args[0], scope);
                    let replacement = argument_type.as_ref().and_then(|ty| {
                        let Type::Named(type_name, arguments) = ty.unqualified() else {
                            return None;
                        };
                        match (type_name.as_str(), arguments.len()) {
                            ("String", 0) => Some("template_child_string"),
                            ("glamour.VNode", 1) => Some("template_child_vnode"),
                            ("glamour.Ui", 1) => Some("template_child_ui"),
                            _ => None,
                        }
                    });
                    match replacement {
                        Some(replacement) => {
                            let prefix = if name.starts_with("glamour.") {
                                "glamour."
                            } else {
                                ""
                            };
                            *name = format!("{prefix}{replacement}");
                        }
                        None => self.diagnostics.push(format!(
                            "{}Glamour child holes accept only `String`, `glamour.VNode(msg)`, or `glamour.Ui(msg)`",
                            self.current_location_prefix(),
                        )),
                    }
                }
                if !scope.is_local(name)
                    && let Some(template) = self.templates.get(name.as_str()).cloned()
                {
                    match self.resolve_type_args(&template, args, scope, result_ty.as_ref()) {
                        Some(type_args) => {
                            let subst: HashMap<String, Type> = type_var_list(&template)
                                .into_iter()
                                .zip(type_args.iter().cloned())
                                .collect();
                            for (param, arg) in template.params.iter().zip(args.iter()) {
                                if param.convention != Convention::Var {
                                    continue;
                                }
                                let (Some(pattern), Expr::Var(binding)) = (&param.ty, arg) else {
                                    continue;
                                };
                                let refined = subst_trait_params(pattern, &subst);
                                refine_ast_scope_type(scope, binding, &refined);
                            }
                            *name = self.specialize(name, type_args);
                        }
                        // A BOUNDED template has no generic fallback (its body
                        // can't compile unresolved), so failing to infer is an
                        // error — and for a result-position variable the fix
                        // is an ascription.
                        None if !template.bounds.is_empty() => {
                            if std::env::var_os("WITCHY_DEBUG_MONO").is_some() {
                                eprintln!(
                                    "mono: `{name}` unresolved; result_ty={:?}; vars={:?}",
                                    result_ty,
                                    type_var_list(&template)
                                );
                            }
                            self.diagnostics.push(format!(
                                "cannot infer the result type for `{name}` — give the \
                                 expected type, e.g. ascribe the binding \
                                 (`let x: List(Int) = {name}(…)`)"
                            ));
                        }
                        None => {}
                    }
                }
            }
            Expr::Apply { func, args } => {
                self.walk_expr(func, scope);
                for a in args.iter_mut() {
                    self.walk_expr(a, scope);
                }
            }
            Expr::Ctor { args, .. }
            | Expr::AnonCtor { args, .. }
            | Expr::List(args)
            | Expr::Tuple(args) => {
                for a in args.iter_mut() {
                    self.walk_expr(a, scope);
                }
            }
            Expr::Unary { expr, .. }
            | Expr::Try(expr)
            | Expr::As { expr, .. }
            | Expr::ExistentialPack { expr, .. }
            | Expr::ExistentialUpcast { expr, .. }
            | Expr::Field { base: expr, .. } => {
                self.walk_expr(expr, scope)
            }
            Expr::RecordUpdate { name: _, base, fields } => {
                self.walk_expr(base, scope);
                for (_, v) in fields.iter_mut() {
                    self.walk_expr(v, scope);
                }
            }
            Expr::LabeledCall { args, .. } => {
                for (_, a) in args.iter_mut() {
                    self.walk_expr(a, scope);
                }
            }
            Expr::LabeledMethodCall { receiver, args, .. } => {
                self.walk_expr(receiver, scope);
                for (_, a) in args.iter_mut() {
                    self.walk_expr(a, scope);
                }
            }
            Expr::Record { fields, spread, .. } => {
                for (_, v) in fields.iter_mut() {
                    self.walk_expr(v, scope);
                }
                if let Some(s) = spread {
                    self.walk_expr(s, scope);
                }
            }
            Expr::Binary { lhs, rhs, .. } => {
                self.walk_expr(lhs, scope);
                self.walk_expr(rhs, scope);
            }
            Expr::Range { lo, hi, .. } => {
                self.walk_expr(lo, scope);
                self.walk_expr(hi, scope);
            }
            Expr::Index { base, index } => {
                self.walk_expr(base, scope);
                self.walk_expr(index, scope);
            }
            Expr::MethodCall { receiver, args, .. } => {
                self.walk_expr(receiver, scope);
                for a in args.iter_mut() {
                    self.walk_expr(a, scope);
                }
            }
            Expr::ExistentialCall { receiver, args, .. } => {
                self.walk_expr(receiver, scope);
                for a in args.iter_mut() {
                    self.walk_expr(a, scope);
                }
            }
            Expr::WhileLet { pattern, scrutinee, body } => {
                self.walk_expr(scrutinee, scope);
                let mut s = scope.clone();
                let scrutinee_ty = self.type_ast(scrutinee, scope);
                bind_typed_pattern(
                    pattern,
                    self.ctor_infos,
                    scrutinee_ty.as_ref(),
                    &mut s,
                );
                self.walk_block(body, &mut s);
                merge_refined_outer_ast_types(scope, &s);
            }
            Expr::If {
                cond,
                then_block,
                else_block,
            } => {
                self.walk_expr(cond, scope);
                let mut then_scope = scope.clone();
                self.walk_block(then_block, &mut then_scope);
                merge_refined_outer_ast_types(scope, &then_scope);
                if let Some(b) = else_block {
                    let mut else_scope = scope.clone();
                    self.walk_block(b, &mut else_scope);
                    merge_refined_outer_ast_types(scope, &else_scope);
                }
            }
            Expr::While { cond, body } => {
                self.walk_expr(cond, scope);
                let mut body_scope = scope.clone();
                self.walk_block(body, &mut body_scope);
                merge_refined_outer_ast_types(scope, &body_scope);
            }
            Expr::For { var, iter, body } => {
                self.walk_expr(iter, scope);
                let mut s = scope.clone();
                match self.type_ast(iter, scope).as_ref().and_then(iterable_item_type) {
                    Some(item_ty) => s.insert(var.clone(), item_ty),
                    None => s.bind_local(var),
                }
                self.walk_block(body, &mut s);
                merge_refined_outer_ast_types(scope, &s);
            }
            Expr::Match { scrutinee, arms } => {
                self.walk_expr(scrutinee, scope);
                let scrutinee_ty = self.type_ast(scrutinee, scope);
                for arm in arms.iter_mut() {
                    let mut s = scope.clone();
                    bind_typed_pattern(
                        &arm.pattern,
                        self.ctor_infos,
                        scrutinee_ty.as_ref(),
                        &mut s,
                    );
                    if let Some(g) = &mut arm.guard {
                        self.walk_expr(g, &mut s);
                    }
                    self.walk_expr(&mut arm.body, &mut s);
                    merge_refined_outer_ast_types(scope, &s);
                }
            }
            Expr::Lambda { params, body, .. } => {
                let mut s = scope.clone();
                seed_typed_params(params, &mut s);
                self.walk_block(body, &mut s);
            }
            Expr::Block(b) => {
                let mut block_scope = scope.clone();
                self.walk_block(b, &mut block_scope);
                merge_refined_outer_ast_types(scope, &block_scope);
            }
            Expr::Var(name) => {
                if scope.is_local(name) {
                    return;
                }
                let original = name.clone();
                let Some(template) = self.templates.get(&original).cloned() else { return };
                let Some(actual) = result_ty.as_ref() else { return };
                let Some(type_args) = self.resolve_function_value_type_args(&template, actual) else {
                    return;
                };
                if std::env::var_os("WITCHY_DEBUG_MONO").is_some() {
                    eprintln!("mono: function value `{original}` -> {type_args:?}");
                }
                *name = self.specialize(&original, type_args);
            }
            Expr::Int(_) | Expr::Duration(_) | Expr::Float(_) | Expr::Str(_) | Expr::Bool(_) | Expr::TaggedLit { .. } => {}
        }
    }

    fn current_location_prefix(&self) -> String {
        if self.current_line == 0 {
            return String::new();
        }
        let function = self
            .current_function
            .rsplit('.')
            .next()
            .unwrap_or(&self.current_function);
        if function.is_empty() {
            format!("line {}: ", self.current_line)
        } else {
            format!("`{function}`, line {}: ", self.current_line)
        }
    }
}
