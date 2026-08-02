//! RFC-0111 physical specialization of already-monomorphized generic callables.
//!
//! Logical instantiation remains owned by `witchy-types`. This layer combines
//! that retained identity with checked RFC-0110 access identity, canonical WIR
//! layouts, and the active optimization schema, then selects exact emitted WIR
//! instances for direct calls.

use super::*;

use witchy_syntax::opt::OptSchemaKey;
use witchy_types::access::AccessIdentityKey;
use witchy_types::traits::LogicalSpecializationIdentity;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) struct CallableSpecializationKey {
    pub(super) logical: LogicalSpecializationIdentity,
    pub(super) access: AccessIdentityKey,
    pub(super) layout: CallableLayoutSignature,
    pub(super) optimization: OptSchemaKey,
}

#[derive(Clone, Debug)]
pub(super) struct GenericCallableInstance {
    pub(super) emitted_name: String,
    pub(super) key: CallableSpecializationKey,
}

#[derive(Default)]
pub(super) struct GenericCallableInstances {
    instances: BTreeMap<String, BTreeMap<CallableSpecializationKey, String>>,
    call_targets: HashMap<(String, usize), String>,
    logical_fallbacks: HashSet<String>,
}

impl GenericCallableInstances {
    pub(super) fn for_function(&self, logical_name: &str) -> Vec<GenericCallableInstance> {
        self.instances
            .get(logical_name)
            .map(|instances| {
                instances
                    .iter()
                    .map(|(key, emitted_name)| GenericCallableInstance {
                        emitted_name: emitted_name.clone(),
                        key: key.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(super) fn call_target(&self, caller: &str, expression: &Expr) -> Option<&str> {
        self.call_targets
            .get(&(caller.to_string(), expression as *const Expr as usize))
            .map(String::as_str)
    }

    pub(super) fn needs_logical_fallback(&self, logical_name: &str) -> bool {
        self.logical_fallbacks.contains(logical_name)
    }

    fn ensure(&mut self, logical_name: &str, key: CallableSpecializationKey) -> (String, bool) {
        let instances = self.instances.entry(logical_name.to_string()).or_default();
        if let Some(name) = instances.get(&key) {
            return (name.clone(), false);
        }
        // Discovery walks module/function/expression order and a newly reached
        // instance scans before the next queue element. That order is stable for
        // one checked module, making this compact discriminator deterministic.
        let emitted_name = format!("{logical_name}__phys{}", instances.len());
        instances.insert(key, emitted_name.clone());
        (emitted_name, true)
    }
}

#[derive(Clone)]
struct CallerInstance {
    function_name: String,
    emitted_name: String,
    key: Option<CallableSpecializationKey>,
    logical_fallback: bool,
}

#[derive(Debug)]
struct ObservedGenericCall {
    expression: usize,
    logical_name: String,
    key: CallableSpecializationKey,
}

impl Codegen<'_> {
    pub(super) fn register_generic_callable_instances(
        &mut self,
        logical_specializations: &BTreeMap<String, LogicalSpecializationIdentity>,
    ) -> Result<(), CodegenError> {
        if logical_specializations.is_empty() {
            return Ok(());
        }
        let function_names = self
            .checked_module
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Function(function) => Some(function.name.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();

        let mut pending = Vec::<CallerInstance>::new();
        let mut scheduled_logical_fallbacks = HashSet::new();
        // Ordinary callers are roots. Generic bodies are scanned only in the
        // context of an observed physical instance, which is what lets one AST
        // recursive-call node select a different target per caller instance.
        for function_name in &function_names {
            if !logical_specializations.contains_key(function_name) {
                pending.push(CallerInstance {
                    function_name: function_name.clone(),
                    emitted_name: function_name.clone(),
                    key: None,
                    logical_fallback: false,
                });
            } else if self
                .checked_module
                .items
                .iter()
                .any(|item| matches!(item, Item::Function(function) if function.name == function_name.as_str() && function.public))
            {
                self.generic_callable_instances
                    .logical_fallbacks
                    .insert(function_name.clone());
                scheduled_logical_fallbacks.insert(function_name.clone());
                pending.push(CallerInstance {
                    function_name: function_name.clone(),
                    emitted_name: function_name.clone(),
                    key: None,
                    logical_fallback: true,
                });
            }
        }

        let mut cursor = 0;
        while cursor < pending.len() {
            let caller = pending[cursor].clone();
            // Collect from the original checked AST, not a cloned Function:
            // lowering later keys lookups by these exact Expr addresses.
            let (fallbacks, calls) = {
                let function = self
                    .checked_module
                    .items
                    .iter()
                    .find_map(|item| match item {
                        Item::Function(function) if function.name == caller.function_name => {
                            Some(function)
                        }
                        _ => None,
                    })
                    .expect("pending callable comes from the checked module");
                self.observe_generic_calls(function, caller.key.as_ref(), logical_specializations)?
            };
            for fallback in fallbacks {
                self.generic_callable_instances
                    .logical_fallbacks
                    .insert(fallback.clone());
                if scheduled_logical_fallbacks.insert(fallback.clone()) {
                    if !function_names.contains(&fallback) {
                        return Err(CodegenError {
                            message: format!(
                                "generic callable fallback `{fallback}` has no checked function body"
                            ),
                        });
                    }
                    pending.push(CallerInstance {
                        function_name: fallback.clone(),
                        emitted_name: fallback,
                        key: None,
                        logical_fallback: true,
                    });
                }
            }
            for call in calls {
                if caller.logical_fallback {
                    self.generic_callable_instances.call_targets.insert(
                        (caller.emitted_name.clone(), call.expression),
                        call.logical_name.clone(),
                    );
                    self.generic_callable_instances
                        .logical_fallbacks
                        .insert(call.logical_name.clone());
                    if scheduled_logical_fallbacks.insert(call.logical_name.clone()) {
                        if !function_names.contains(&call.logical_name) {
                            return Err(CodegenError {
                                message: format!(
                                    "generic callable fallback `{}` has no checked function body",
                                    call.logical_name
                                ),
                            });
                        }
                        pending.push(CallerInstance {
                            function_name: call.logical_name.clone(),
                            emitted_name: call.logical_name,
                            key: None,
                            logical_fallback: true,
                        });
                    }
                    continue;
                }
                let (emitted_name, is_new) = self
                    .generic_callable_instances
                    .ensure(&call.logical_name, call.key.clone());
                self.generic_callable_instances.call_targets.insert(
                    (caller.emitted_name.clone(), call.expression),
                    emitted_name.clone(),
                );
                if is_new {
                    pending.push(CallerInstance {
                        function_name: call.logical_name,
                        emitted_name,
                        key: Some(call.key),
                        logical_fallback: false,
                    });
                }
            }
            cursor += 1;
        }
        Ok(())
    }

    fn observe_generic_calls(
        &self,
        caller: &Function,
        caller_key: Option<&CallableSpecializationKey>,
        logical_specializations: &BTreeMap<String, LogicalSpecializationIdentity>,
    ) -> Result<(Vec<String>, Vec<ObservedGenericCall>), CodegenError> {
        let mut fallbacks = Vec::new();
        let mut calls = Vec::new();
        let mut missing_access = None;
        visit_calls(&caller.body, &mut |expression| {
            if missing_access.is_some() {
                return;
            }
            if let Expr::Var(name) = expression
                && logical_specializations.contains_key(name)
            {
                // First-class callable lowering synthesizes a forwarding call
                // after type annotation, so it has no checked source Expr
                // address to specialize. Retain the logical instance; exact
                // specialized-layout function values still reject at their
                // existing boundary before reaching this fallback.
                fallbacks.push(name.clone());
            }
            let Expr::Call { name, args } = expression else {
                return;
            };
            let Some(logical) = logical_specializations.get(name) else {
                return;
            };
            let Some(access) = self.access_facts.call_at(self.checked_module, expression) else {
                missing_access = Some(CodegenError {
                    message: format!(
                        "generic call `{name}` in `{}` has no checked access facts; physical specialization cannot fall back implicitly",
                        caller.name
                    ),
                });
                return;
            };
            calls.push(ObservedGenericCall {
                expression: expression as *const Expr as usize,
                logical_name: name.clone(),
                key: CallableSpecializationKey {
                    logical: logical.clone(),
                    access: access.identity_key(),
                    layout: self.call_layout_signature(expression, args, caller, caller_key),
                    optimization: witchy_syntax::opt::active_schema_key(),
                },
            });
        });
        match missing_access {
            Some(error) => Err(error),
            None => Ok((fallbacks, calls)),
        }
    }

    fn call_layout_signature(
        &self,
        call: &Expr,
        arguments: &[Expr],
        caller: &Function,
        caller_key: Option<&CallableSpecializationKey>,
    ) -> CallableLayoutSignature {
        let parameters = arguments
            .iter()
            .map(|argument| {
                self.ast_type_from_table(argument)
                    .and_then(|ty| self.layout_id_in_instance(&ty, caller, caller_key))
            })
            .collect();
        let result = self
            .ast_type_from_table(call)
            .and_then(|ty| self.layout_id_in_instance(&ty, caller, caller_key));
        CallableLayoutSignature::new(parameters, result)
    }

    fn ast_type_from_table(&self, expression: &Expr) -> Option<Type> {
        self.type_table
            .type_of(expression)
            .and_then(witchy_types::typeck::ty_to_ast)
    }

    fn layout_id_in_instance(
        &self,
        ty: &Type,
        caller: &Function,
        caller_key: Option<&CallableSpecializationKey>,
    ) -> Option<LayoutId> {
        if let Some(key) = caller_key {
            let mut selected = None;
            for (position, parameter) in caller.params.iter().enumerate() {
                if parameter
                    .ty
                    .as_ref()
                    .is_some_and(|known| known.unqualified() == ty.unqualified())
                    && let Some(id) = key.layout.parameters().get(position).copied().flatten()
                {
                    if selected.is_some_and(|known| known != id) {
                        return None;
                    }
                    selected = Some(id);
                }
            }
            if caller
                .ret
                .as_ref()
                .is_some_and(|known| known.unqualified() == ty.unqualified())
                && let Some(id) = key.layout.result()
            {
                if selected.is_some_and(|known| known != id) {
                    return None;
                }
                selected = Some(id);
            }
            if selected.is_some() {
                return selected;
            }
        }
        self.specialized_type_ids
            .iter()
            .find(|(known, _)| known.unqualified() == ty.unqualified())
            .map(|(_, id)| *id)
    }

    pub(super) fn begin_callable_specialization(
        &mut self,
        function: &Function,
        emitted_name: &str,
        key: Option<&CallableSpecializationKey>,
    ) -> Result<(), CodegenError> {
        self.cur_emitted_fn_name = emitted_name.to_string();
        self.current_specialized_type_ids.clear();
        let Some(key) = key else { return Ok(()) };
        self.current_specialized_type_ids = instance_type_layouts(function, key)?;
        Ok(())
    }

    pub(super) fn end_callable_specialization(&mut self) {
        self.cur_emitted_fn_name.clear();
        self.current_specialized_type_ids.clear();
    }

    pub(super) fn with_callable_specialization<T>(
        &mut self,
        function: &Function,
        emitted_name: &str,
        key: Option<&CallableSpecializationKey>,
        body: impl FnOnce(&mut Self) -> Result<T, CodegenError>,
    ) -> Result<T, CodegenError> {
        let setup = self.begin_callable_specialization(function, emitted_name, key);
        if let Err(error) = setup {
            self.end_callable_specialization();
            return Err(error);
        }
        let result = body(self);
        self.end_callable_specialization();
        result
    }

    pub(super) fn generic_call_target<'a>(
        &'a self,
        expression: &Expr,
        logical_name: &'a str,
    ) -> &'a str {
        self.generic_callable_instances
            .call_target(&self.cur_emitted_fn_name, expression)
            .unwrap_or(logical_name)
    }
}

fn instance_type_layouts(
    function: &Function,
    key: &CallableSpecializationKey,
) -> Result<Vec<(Type, LayoutId)>, CodegenError> {
    fn install(
        layouts: &mut Vec<(Type, LayoutId)>,
        ty: Type,
        id: LayoutId,
    ) -> Result<(), CodegenError> {
        if let Some((_, existing)) = layouts
            .iter()
            .find(|(known, _)| known.unqualified() == ty.unqualified())
        {
            if *existing != id {
                return Err(CodegenError {
                    message: format!(
                        "generic callable instance assigns two physical layouts to logical type {:?}: {existing} and {id}",
                        ty.unqualified()
                    ),
                });
            }
            return Ok(());
        }
        layouts.push((ty, id));
        Ok(())
    }

    let mut layouts = Vec::new();
    for (position, parameter) in function.params.iter().enumerate() {
        if let (Some(ty), Some(id)) = (
            parameter.ty.as_ref(),
            key.layout.parameters().get(position).copied().flatten(),
        ) {
            install(&mut layouts, ty.clone(), id)?;
        }
    }
    if let (Some(ty), Some(id)) = (function.ret.as_ref(), key.layout.result()) {
        install(&mut layouts, ty.clone(), id)?;
    }
    Ok(layouts)
}

fn visit_calls<'a>(block: &'a Block, visitor: &mut impl FnMut(&'a Expr)) {
    for statement in &block.stmts {
        match statement {
            Stmt::Let { value, .. }
            | Stmt::Assign { value, .. }
            | Stmt::LetPattern { value, .. }
            | Stmt::Yield(value)
            | Stmt::Expr(value)
            | Stmt::Return(Some(value)) => visit_call_expr(value, visitor),
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
    }
}

fn visit_call_expr<'a>(expression: &'a Expr, visitor: &mut impl FnMut(&'a Expr)) {
    visitor(expression);
    match expression {
        Expr::List(items)
        | Expr::Tuple(items)
        | Expr::Ctor { args: items, .. }
        | Expr::AnonCtor { args: items, .. } => {
            for item in items {
                visit_call_expr(item, visitor);
            }
        }
        Expr::Call { args, .. } => {
            for argument in args {
                visit_call_expr(argument, visitor);
            }
        }
        Expr::MethodCall { receiver, args, .. } | Expr::ExistentialCall { receiver, args, .. } => {
            visit_call_expr(receiver, visitor);
            for argument in args {
                visit_call_expr(argument, visitor);
            }
        }
        Expr::LabeledCall { args, .. } => {
            for (_, argument) in args {
                visit_call_expr(argument, visitor);
            }
        }
        Expr::LabeledMethodCall { receiver, args, .. } => {
            visit_call_expr(receiver, visitor);
            for (_, argument) in args {
                visit_call_expr(argument, visitor);
            }
        }
        Expr::Apply { func, args } => {
            visit_call_expr(func, visitor);
            for argument in args {
                visit_call_expr(argument, visitor);
            }
        }
        Expr::Unary { expr, .. }
        | Expr::Try(expr)
        | Expr::As { expr, .. }
        | Expr::ExistentialPack { expr, .. }
        | Expr::ExistentialUpcast { expr, .. }
        | Expr::Field { base: expr, .. } => visit_call_expr(expr, visitor),
        Expr::Lambda { body, .. } | Expr::Block(body) => visit_calls(body, visitor),
        Expr::RecordUpdate { base, fields, .. } => {
            visit_call_expr(base, visitor);
            for (_, value) in fields {
                visit_call_expr(value, visitor);
            }
        }
        Expr::Record { fields, spread, .. } => {
            for (_, value) in fields {
                visit_call_expr(value, visitor);
            }
            if let Some(spread) = spread {
                visit_call_expr(spread, visitor);
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            visit_call_expr(lhs, visitor);
            visit_call_expr(rhs, visitor);
        }
        Expr::If {
            cond,
            then_block,
            else_block,
        } => {
            visit_call_expr(cond, visitor);
            visit_calls(then_block, visitor);
            if let Some(else_block) = else_block {
                visit_calls(else_block, visitor);
            }
        }
        Expr::Match { scrutinee, arms } => {
            visit_call_expr(scrutinee, visitor);
            for arm in arms {
                if let Some(guard) = &arm.guard {
                    visit_call_expr(guard, visitor);
                }
                visit_call_expr(&arm.body, visitor);
            }
        }
        Expr::While { cond, body } => {
            visit_call_expr(cond, visitor);
            visit_calls(body, visitor);
        }
        Expr::For { iter, body, .. } => {
            visit_call_expr(iter, visitor);
            visit_calls(body, visitor);
        }
        Expr::Range { lo, hi, .. } => {
            visit_call_expr(lo, visitor);
            visit_call_expr(hi, visitor);
        }
        Expr::Index { base, index } => {
            visit_call_expr(base, visitor);
            visit_call_expr(index, visitor);
        }
        Expr::WhileLet {
            scrutinee, body, ..
        } => {
            visit_call_expr(scrutinee, visitor);
            visit_calls(body, visitor);
        }
        Expr::Int(_)
        | Expr::Float(_)
        | Expr::Duration(_)
        | Expr::Str(_)
        | Expr::Bool(_)
        | Expr::Var(_)
        | Expr::TaggedLit { .. } => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_codegen<R>(
        source: &str,
        test: impl FnOnce(&mut Codegen<'_>, &BTreeMap<String, LogicalSpecializationIdentity>) -> R,
    ) -> R {
        let module =
            witchy_syntax::parser::parse_module(source).expect("parse specialization test");
        let (mut module, specializations) =
            witchy_types::traits::lower_for_wasm_with_specializations(module).into_parts();
        witchy_syntax::parser::lower_sugar_module(&mut module);
        alpha_rename_module(&mut module);
        let typed =
            witchy_types::typeck::annotate_checked(module).expect("annotate specialization test");
        let loans = witchy_types::loans::facts_with_types(typed.module(), typed.table())
            .expect("specialization loan facts");
        let access = witchy_types::access::checked_facts(typed.module(), typed.table())
            .expect("specialization access facts");
        let mut codegen = Codegen::new(typed.module(), typed.table(), loans, access);
        test(&mut codegen, &specializations)
    }

    fn named(name: &str) -> Type {
        Type::Named(name.to_string(), Vec::new())
    }

    fn list_point() -> Type {
        Type::Named("List".to_string(), vec![named("Point")])
    }

    fn relay_function() -> Function {
        Function {
            line: 0,
            public: false,
            comptime_only: false,
            attributes: Vec::new(),
            name: "relay__Point".to_string(),
            params: vec![Param {
                name: "values".to_string(),
                ty: Some(list_point()),
                convention: Convention::Let,
                default: None,
            }],
            ret: Some(list_point()),
            body: Block {
                stmts: vec![Stmt::Expr(Expr::Var("values".to_string()))],
                lines: vec![1],
                region: None,
            },
            bounds: Vec::new(),
            is_gen: false,
            is_async: false,
        }
    }

    fn key_with_access(layout: LayoutId, convention: Convention) -> CallableSpecializationKey {
        let mut function = relay_function();
        function.params[0].convention = convention;
        let access = witchy_types::access::AccessSignature::from_function(&function)
            .expect("closed relay access signature");
        CallableSpecializationKey {
            logical: LogicalSpecializationIdentity::from_types(&[named("Point")]),
            access: access.identity_key(),
            layout: CallableLayoutSignature::new(vec![Some(layout)], Some(layout)),
            optimization: witchy_syntax::opt::active_schema_key(),
        }
    }

    fn key(layout: LayoutId) -> CallableSpecializationKey {
        key_with_access(layout, Convention::Let)
    }

    fn packed_list_layout(layouts: &mut LayoutInterner, header: RcHeader) -> LayoutId {
        struct Resolver(RcHeader);
        impl witchy_wir::layout::ClosedTypeResolver for Resolver {
            fn resolve_named<'a>(
                &'a self,
                name: &str,
                _arguments: &[Type],
            ) -> Option<witchy_wir::layout::ResolvedNamed<'a>> {
                match name {
                    "Point" => Some(witchy_wir::layout::ResolvedNamed::Scalar(ScalarKind::Int)),
                    "List" => Some(witchy_wir::layout::ResolvedNamed::PackedList { rc: self.0 }),
                    _ => None,
                }
            }
        }

        layouts
            .intern_type(&list_point(), &Resolver(header))
            .expect("closed packed List(Point) layout")
    }

    fn function_named<'a>(codegen: &'a Codegen<'_>, name: &str) -> &'a Function {
        codegen
            .checked_module
            .items
            .iter()
            .find_map(|item| match item {
                Item::Function(function) if function.name == name => Some(function),
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing checked function {name}"))
    }

    fn calls_to<'a>(function: &'a Function, name: &str) -> Vec<&'a Expr> {
        let mut calls = Vec::new();
        visit_calls(&function.body, &mut |expression| {
            if matches!(expression, Expr::Call { name: callee, .. } if callee == name) {
                calls.push(expression);
            }
        });
        calls
    }

    fn allocation_target(nodes: &[witchy_wir::wir::WirNode]) -> &str {
        let Some(witchy_wir::wir::WirNode::SetLocal {
            value: witchy_wir::wir::WirExpr::Call { func, .. },
            ..
        }) = nodes.first()
        else {
            panic!("layout allocation must begin with a direct allocator call")
        };
        func
    }

    #[test]
    fn recursive_call_target_is_discovered_for_the_real_checked_expression() {
        with_codegen(
            "fn repeat(value: a) -> a:\n    repeat(value)\n\nfn main() -> Int:\n    repeat(1)\n",
            |codegen, specializations| {
                codegen
                    .register_generic_callable_instances(specializations)
                    .expect("discover recursive physical instance");
                let logical = specializations
                    .keys()
                    .find(|name| name.starts_with("repeat__"))
                    .expect("repeat specialization");
                let instances = codegen.generic_callable_instances.for_function(logical);
                let [instance] = instances.as_slice() else {
                    panic!("one recursive physical instance")
                };
                let recursive = calls_to(function_named(codegen, logical), logical);
                let [recursive] = recursive.as_slice() else {
                    panic!("one recursive source call")
                };
                assert_eq!(
                    codegen
                        .generic_callable_instances
                        .call_target(&instance.emitted_name, recursive),
                    Some(instance.emitted_name.as_str())
                );
            },
        );
    }

    #[test]
    fn required_and_elided_manual_keys_drive_distinct_body_layouts() {
        let required = packed_list_layout(&mut LayoutInterner::new(), RcHeader::Required);
        let elided = packed_list_layout(&mut LayoutInterner::new(), RcHeader::Elided);
        let function = relay_function();
        let required_key = key(required);
        let elided_key = key(elided);

        assert_ne!(required_key, elided_key);
        assert_eq!(
            instance_type_layouts(&function, &required_key).expect("required layout override")[0].1,
            required
        );
        assert_eq!(
            instance_type_layouts(&function, &elided_key).expect("elided layout override")[0].1,
            elided
        );

        let mut instances = GenericCallableInstances::default();
        let (required_name, _) = instances.ensure(&function.name, required_key);
        let (elided_name, _) = instances.ensure(&function.name, elided_key);
        assert_ne!(required_name, elided_name);
    }

    #[test]
    fn access_and_layout_distinct_instances_emit_distinct_allocator_behavior() {
        with_codegen("fn main() -> Int:\n    0\n", |codegen, _| {
            let required = packed_list_layout(&mut codegen.specialized_layouts, RcHeader::Required);
            let elided = packed_list_layout(&mut codegen.specialized_layouts, RcHeader::Elided);
            let function = relay_function();
            let required_key = key_with_access(required, Convention::Let);
            let elided_key = key_with_access(elided, Convention::Borrow);
            assert_ne!(required_key.access, elided_key.access);
            assert_ne!(required_key.layout, elided_key.layout);

            let (required_name, _) = codegen
                .generic_callable_instances
                .ensure(&function.name, required_key.clone());
            let (elided_name, _) = codegen
                .generic_callable_instances
                .ensure(&function.name, elided_key.clone());

            let emitted_allocator =
                |codegen: &mut Codegen<'_>, emitted_name: &str, key: &CallableSpecializationKey| {
                    codegen
                        .with_callable_specialization(
                            &function,
                            emitted_name,
                            Some(key),
                            |codegen| {
                                let id = codegen
                                    .specialized_layout_id(&list_point())
                                    .expect("instance body layout");
                                let rc = match codegen
                                    .specialized_layouts
                                    .get(id)
                                    .expect("instance descriptor")
                                    .header()
                                {
                                    HeaderLayout::PackedList { rc, .. } => rc,
                                    other => panic!("expected packed-list header, found {other:?}"),
                                };
                                let (nodes, _) = codegen.layout_alloc_nodes_with_header(32, rc);
                                Ok(allocation_target(&nodes).to_string())
                            },
                        )
                        .expect("emit specialized allocation")
                };
            assert_ne!(required_name, elided_name);
            assert_eq!(
                emitted_allocator(codegen, &required_name, &required_key),
                "rc_alloc"
            );
            assert_eq!(
                emitted_allocator(codegen, &elided_name, &elided_key),
                "bump_alloc"
            );
        });
    }

    #[test]
    fn logical_fallback_discovery_closes_transitively_over_generic_calls() {
        with_codegen(
            "fn g(value: a) -> a:\n    value\n\nfn f(value: a) -> a:\n    g(value)\n\nfn main() -> Int:\n    let indirect: fn(Int) -> Int = f\n    f(1) + indirect(2)\n",
            |codegen, specializations| {
                codegen
                    .register_generic_callable_instances(specializations)
                    .expect("discover physical and logical fallback closure");
                let f = specializations
                    .keys()
                    .find(|name| name.starts_with("f__"))
                    .expect("f specialization");
                let g = specializations
                    .keys()
                    .find(|name| name.starts_with("g__"))
                    .expect("g specialization");
                assert!(
                    !codegen
                        .generic_callable_instances
                        .for_function(f)
                        .is_empty()
                );
                assert!(
                    !codegen
                        .generic_callable_instances
                        .for_function(g)
                        .is_empty()
                );
                assert!(codegen.generic_callable_instances.needs_logical_fallback(f));
                assert!(codegen.generic_callable_instances.needs_logical_fallback(g));
                let calls = calls_to(function_named(codegen, f), g);
                let [call] = calls.as_slice() else {
                    panic!("logical f contains one direct g call")
                };
                assert_eq!(
                    codegen.generic_callable_instances.call_target(f, call),
                    Some(g.as_str()),
                    "logical f must call the guaranteed logical g fallback"
                );
            },
        );
    }

    #[test]
    fn missing_generic_call_access_facts_fail_closed() {
        with_codegen(
            "fn identity(value: a) -> a:\n    value\n\nfn main() -> Int:\n    identity(1)\n",
            |codegen, specializations| {
                let cloned_main = function_named(codegen, "main").clone();
                let error = codegen
                    .observe_generic_calls(&cloned_main, None, specializations)
                    .expect_err("cloned expressions have no authenticated access facts");
                assert!(error.message.contains("has no checked access facts"));
                assert!(error.message.contains("cannot fall back implicitly"));
            },
        );
    }

    #[test]
    fn setup_and_body_errors_both_clear_specialization_context() {
        with_codegen("fn main() -> Int:\n    0\n", |codegen, _| {
            let required = packed_list_layout(&mut codegen.specialized_layouts, RcHeader::Required);
            let elided = packed_list_layout(&mut codegen.specialized_layouts, RcHeader::Elided);
            let mut conflicting = relay_function();
            conflicting.params.push(Param {
                name: "other".to_string(),
                ty: Some(list_point()),
                convention: Convention::Let,
                default: None,
            });
            let access = witchy_types::access::AccessSignature::from_function(&conflicting)
                .expect("conflicting test access signature");
            let conflicting_key = CallableSpecializationKey {
                logical: LogicalSpecializationIdentity::from_types(&[named("Point")]),
                access: access.identity_key(),
                layout: CallableLayoutSignature::new(
                    vec![Some(required), Some(elided)],
                    Some(required),
                ),
                optimization: witchy_syntax::opt::active_schema_key(),
            };
            codegen
                .with_callable_specialization(
                    &conflicting,
                    "relay__Point__bad_setup",
                    Some(&conflicting_key),
                    |_| Ok(()),
                )
                .expect_err("conflicting logical type layouts fail setup");
            assert!(codegen.cur_emitted_fn_name.is_empty());
            assert!(codegen.current_specialized_type_ids.is_empty());

            let valid_key = key(required);
            codegen
                .with_callable_specialization(
                    &relay_function(),
                    "relay__Point__bad_body",
                    Some(&valid_key),
                    |codegen| {
                        assert_eq!(codegen.cur_emitted_fn_name, "relay__Point__bad_body");
                        Err::<(), _>(CodegenError {
                            message: "synthetic body failure".to_string(),
                        })
                    },
                )
                .expect_err("body failure propagates");
            assert!(codegen.cur_emitted_fn_name.is_empty());
            assert!(codegen.current_specialized_type_ids.is_empty());
        });
    }

    #[test]
    fn physical_instances_have_distinct_scalar_companion_names() {
        assert_ne!(
            Codegen::scalar_record_companion_name("produce__Point__phys0"),
            Codegen::scalar_record_companion_name("produce__Point__phys1")
        );
    }
}
