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
        let mut failure = None;
        visit_calls(&caller.body, &mut |expression| {
            if failure.is_some() {
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
                failure = Some(CodegenError {
                    message: format!(
                        "generic call `{name}` in `{}` has no checked access facts; physical specialization cannot fall back implicitly",
                        caller.name
                    ),
                });
                return;
            };
            let layout = match self.call_layout_signature(expression, args, caller, caller_key) {
                Ok(layout) => layout,
                Err(error) => {
                    failure = Some(error);
                    return;
                }
            };
            calls.push(ObservedGenericCall {
                expression: expression as *const Expr as usize,
                logical_name: name.clone(),
                key: CallableSpecializationKey {
                    logical: logical.clone(),
                    access: access.identity_key(),
                    layout,
                    optimization: witchy_syntax::opt::active_schema_key(),
                },
            });
        });
        match failure {
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
    ) -> Result<CallableLayoutSignature, CodegenError> {
        let parameters = arguments
            .iter()
            .enumerate()
            .map(|(position, argument)| {
                let Some(ty) = self.ast_type_from_table(argument) else {
                    return Err(CodegenError {
                        message: format!(
                            "generic call in `{}` has no checked concrete type for argument {position}; physical specialization cannot select an ordinary ABI",
                            caller.name
                        ),
                    });
                };
                self.layout_id_in_instance(&ty, caller, caller_key)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let Some(result_ty) = self.ast_type_from_table(call) else {
            return Err(CodegenError {
                message: format!(
                    "generic call in `{}` has no checked concrete result type; physical specialization cannot select an ordinary ABI",
                    caller.name
                ),
            });
        };
        let result = self.layout_id_in_instance(&result_ty, caller, caller_key)?;
        Ok(CallableLayoutSignature::new(parameters, result))
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
    ) -> Result<Option<LayoutId>, CodegenError> {
        if let Some(key) = caller_key {
            if let Some((_, selected)) = instance_type_layout_bindings(caller, key)?
                .into_iter()
                .find(|(known, _)| known.unqualified() == ty.unqualified())
            {
                // An explicit ordinary binding belongs to this physical
                // instance and must not fall through to a global layout.
                return Ok(selected);
            }
        }
        let mut selected = None;
        for (_, id) in self
            .specialized_type_ids
            .iter()
            .filter(|(known, _)| known.unqualified() == ty.unqualified())
        {
            if selected.is_some_and(|known| known != *id) {
                return Err(CodegenError {
                    message: format!(
                        "physical layout lookup for logical type {:?} is ambiguous: {} and {}",
                        ty.unqualified(),
                        selected.expect("conflicting layout has a first value"),
                        id
                    ),
                });
            }
            selected = Some(*id);
        }
        Ok(selected)
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
    Ok(instance_type_layout_bindings(function, key)?
        .into_iter()
        .filter_map(|(ty, layout)| layout.map(|layout| (ty, layout)))
        .collect())
}

fn substitute_specialization_type(
    ty: &Type,
    substitutions: &BTreeMap<String, Type>,
) -> Type {
    match ty {
        Type::Qualified(qualifier, inner) => Type::Qualified(
            qualifier.clone(),
            Box::new(substitute_specialization_type(inner, substitutions)),
        ),
        Type::Slice(inner) => Type::Slice(Box::new(substitute_specialization_type(
            inner,
            substitutions,
        ))),
        Type::Named(name, arguments) if arguments.is_empty() => substitutions
            .get(name)
            .cloned()
            .unwrap_or_else(|| Type::Named(name.clone(), Vec::new())),
        Type::Named(name, arguments) => Type::Named(
            name.clone(),
            arguments
                .iter()
                .map(|argument| substitute_specialization_type(argument, substitutions))
                .collect(),
        ),
        Type::Dyn(name, arguments) => Type::Dyn(
            name.clone(),
            arguments
                .iter()
                .map(|argument| substitute_specialization_type(argument, substitutions))
                .collect(),
        ),
        Type::Tuple(items) => Type::Tuple(
            items
                .iter()
                .map(|item| substitute_specialization_type(item, substitutions))
                .collect(),
        ),
        Type::RecordCompose { base, fields } => Type::RecordCompose {
            base: Box::new(substitute_specialization_type(base, substitutions)),
            fields: fields
                .iter()
                .map(|(name, field)| {
                    (
                        name.clone(),
                        substitute_specialization_type(field, substitutions),
                    )
                })
                .collect(),
        },
        Type::Fn(parameters, result, conventions, qualifiers) => Type::Fn(
            parameters
                .iter()
                .map(|parameter| substitute_specialization_type(parameter, substitutions))
                .collect(),
            Box::new(substitute_specialization_type(result, substitutions)),
            conventions.clone(),
            *qualifiers,
        ),
    }
}

fn instance_concrete_signature(
    function: &Function,
    key: &CallableSpecializationKey,
) -> Result<(Vec<Option<Type>>, Option<Type>), CodegenError> {
    let logical = &key.logical;
    let (parameters, result) = if let Some(template) = logical.template() {
        if template.type_parameters().len() != logical.concrete_types().len() {
            return Err(CodegenError {
                message: format!(
                    "generic callable `{}` has {} logical type parameters but {} concrete arguments",
                    function.name,
                    template.type_parameters().len(),
                    logical.concrete_types().len()
                ),
            });
        }
        let mut substitutions = BTreeMap::new();
        for (parameter, concrete) in template
            .type_parameters()
            .iter()
            .zip(logical.concrete_types())
        {
            if substitutions.insert(parameter.clone(), concrete.clone()).is_some() {
                return Err(CodegenError {
                    message: format!(
                        "generic callable `{}` repeats logical type parameter `{parameter}`",
                        function.name
                    ),
                });
            }
        }
        let parameters = template
            .parameters()
            .iter()
            .map(|parameter| {
                parameter
                    .as_ref()
                    .map(|ty| substitute_specialization_type(ty, &substitutions))
            })
            .collect::<Vec<_>>();
        let result = template
            .result()
            .map(|ty| substitute_specialization_type(ty, &substitutions));
        (parameters, result)
    } else {
        // Hand-built non-generic keys predate retained templates and remain
        // valid for closed signatures. A generic shell without its production
        // specialization evidence is never accepted.
        (
            function.params.iter().map(|parameter| parameter.ty.clone()).collect(),
            function.ret.clone(),
        )
    };

    for ty in parameters.iter().flatten().chain(result.iter()) {
        let mut residual = Vec::new();
        collect_type_vars(ty, &mut residual);
        if !residual.is_empty() {
            return Err(CodegenError {
                message: format!(
                    "generic callable `{}` retains unresolved logical types after specialization: {}",
                    function.name,
                    residual.join(", ")
                ),
            });
        }
    }
    if function.params.len() != parameters.len() {
        return Err(CodegenError {
            message: format!(
                "generic callable `{}` specialization has {} checked parameters but {} retained template parameters",
                function.name,
                function.params.len(),
                parameters.len()
            ),
        });
    }
    for (position, (parameter, expected)) in
        function.params.iter().zip(&parameters).enumerate()
    {
        if parameter.ty.as_ref() != expected.as_ref() {
            return Err(CodegenError {
                message: format!(
                    "generic callable `{}` parameter {position} does not match its substituted logical type: checked={:?}, expected={expected:?}",
                    function.name,
                    parameter.ty
                ),
            });
        }
    }
    if function.ret.as_ref() != result.as_ref() {
        return Err(CodegenError {
            message: format!(
                "generic callable `{}` result does not match its substituted logical type: checked={:?}, expected={result:?}",
                function.name,
                function.ret
            ),
        });
    }
    Ok((parameters, result))
}

fn instance_type_layout_bindings(
    function: &Function,
    key: &CallableSpecializationKey,
) -> Result<Vec<(Type, Option<LayoutId>)>, CodegenError> {
    fn install(
        layouts: &mut Vec<(Type, Option<LayoutId>)>,
        ty: Type,
        id: Option<LayoutId>,
    ) -> Result<(), CodegenError> {
        if let Some((_, existing)) = layouts
            .iter()
            .find(|(known, _)| known.unqualified() == ty.unqualified())
        {
            if *existing != id {
                return Err(CodegenError {
                    message: format!(
                        "generic callable instance assigns conflicting physical layouts to logical type {:?}: {existing:?} and {id:?}",
                        ty.unqualified()
                    ),
                });
            }
            return Ok(());
        }
        layouts.push((ty, id));
        Ok(())
    }

    let (parameters, result) = instance_concrete_signature(function, key)?;
    if key.layout.parameters().len() != parameters.len() {
        return Err(CodegenError {
            message: format!(
                "generic callable `{}` layout has {} parameter positions for a {}-parameter specialization",
                function.name,
                key.layout.parameters().len(),
                parameters.len()
            ),
        });
    }
    let mut layouts = Vec::new();
    for (position, parameter) in parameters.into_iter().enumerate() {
        let layout = key.layout.parameters()[position];
        match parameter {
            Some(ty) => install(&mut layouts, ty, layout)?,
            None if layout.is_some() => {
                return Err(CodegenError {
                    message: format!(
                        "generic callable `{}` parameter {position} has a physical layout but no substituted logical type",
                        function.name
                    ),
                });
            }
            None => {}
        }
    }
    match result {
        Some(ty) => install(&mut layouts, ty, key.layout.result())?,
        None if key.layout.result().is_some() => {
            return Err(CodegenError {
                message: format!(
                    "generic callable `{}` result has a physical layout but no substituted logical type",
                    function.name
                ),
            });
        }
        None => {}
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
            pure: false,
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

    fn generic_relay_template(parameter_count: usize) -> Function {
        let mut function = relay_function();
        function.name = "relay".to_string();
        function.params = (0..parameter_count)
            .map(|position| Param {
                name: format!("value{position}"),
                ty: Some(named("a")),
                convention: Convention::Let,
                default: None,
            })
            .collect();
        function.ret = Some(named("a"));
        function
    }

    fn specialized_relay(parameter_count: usize) -> Function {
        let mut function = generic_relay_template(parameter_count);
        function.name = "relay__List_Point".to_string();
        for parameter in &mut function.params {
            parameter.ty = Some(list_point());
        }
        function.ret = Some(list_point());
        function
    }

    fn logical_relay_identity(
        template: &Function,
        concrete_types: &[Type],
    ) -> LogicalSpecializationIdentity {
        LogicalSpecializationIdentity::from_types_with_template(
            concrete_types,
            vec!["a".to_string()],
            template
                .params
                .iter()
                .map(|parameter| parameter.ty.clone())
                .collect(),
            template.ret.clone(),
        )
    }

    fn physical_key(
        function: &Function,
        logical: LogicalSpecializationIdentity,
        parameters: Vec<Option<LayoutId>>,
        result: Option<LayoutId>,
    ) -> CallableSpecializationKey {
        let access = witchy_types::access::AccessSignature::from_function(function)
            .expect("closed specialization access signature");
        CallableSpecializationKey {
            logical,
            access: access.identity_key(),
            layout: CallableLayoutSignature::new(parameters, result),
            optimization: witchy_syntax::opt::active_schema_key(),
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
    fn generic_argument_binds_exact_packed_layout_to_substituted_signature() {
        let layout = packed_list_layout(&mut LayoutInterner::new(), RcHeader::Required);
        let template = generic_relay_template(1);
        let function = specialized_relay(1);
        let key = physical_key(
            &function,
            logical_relay_identity(&template, &[list_point()]),
            vec![Some(layout)],
            Some(layout),
        );

        assert_eq!(
            instance_type_layouts(&function, &key).expect("exact generic layout binding"),
            vec![(list_point(), layout)]
        );
    }

    #[test]
    fn production_monomorphization_retains_packed_layout_binding_evidence() {
        with_codegen(
            "type Point:\n    value: Int\n\nfn relay(value: a) -> a:\n    value\n\nfn main() -> Int:\n    let _ = relay([Point(1)])\n    0\n",
            |codegen, specializations| {
                let (logical_name, logical) = specializations
                    .iter()
                    .find(|(name, _)| name.starts_with("relay__"))
                    .expect("relay logical specialization");
                assert_eq!(logical.concrete_types(), &[list_point()]);
                let template = logical.template().expect("retained generic template");
                assert_eq!(template.type_parameters(), &["a".to_string()]);
                assert_eq!(template.parameters(), &[Some(named("a"))]);
                assert_eq!(template.result().cloned(), Some(named("a")));

                let function = function_named(codegen, logical_name).clone();
                let layout =
                    packed_list_layout(&mut codegen.specialized_layouts, RcHeader::Required);
                codegen
                    .specialized_type_ids
                    .push((list_point(), layout));
                codegen
                    .register_generic_callable_instances(specializations)
                    .expect("discover production packed physical instance");
                let instances = codegen
                    .generic_callable_instances
                    .for_function(logical_name);
                let [instance] = instances.as_slice() else {
                    panic!("one production packed physical instance")
                };
                assert_eq!(&instance.key.logical, logical);
                assert_eq!(instance.key.layout.parameters(), &[Some(layout)]);
                assert_eq!(instance.key.layout.result(), Some(layout));
                codegen
                    .with_callable_specialization(
                        &function,
                        &instance.emitted_name,
                        Some(&instance.key),
                        |codegen| {
                            assert_eq!(codegen.specialized_layout_id(&list_point()), Some(layout));
                            Ok(())
                        },
                    )
                    .expect("bind production packed physical instance");
            },
        );
    }

    #[test]
    fn repeated_bounds_share_one_logical_substitution_slot() {
        with_codegen(
            "fn relay(value: a) -> a where a: PartialEq, a: Show:\n    value\n\nfn main() -> Int:\n    relay(1)\n",
            |codegen, specializations| {
                let (logical_name, logical) = specializations
                    .iter()
                    .find(|(name, _)| name.starts_with("relay__"))
                    .expect("multi-bound relay specialization");
                assert_eq!(logical.concrete_types(), &[named("Int")]);
                assert_eq!(
                    logical
                        .template()
                        .expect("multi-bound retained template")
                        .type_parameters(),
                    &["a".to_string()]
                );
                codegen
                    .register_generic_callable_instances(specializations)
                    .expect("multi-bound physical discovery");
                let instances = codegen
                    .generic_callable_instances
                    .for_function(logical_name);
                let [instance] = instances.as_slice() else {
                    panic!("one multi-bound physical instance")
                };
                let function = function_named(codegen, logical_name).clone();
                codegen
                    .with_callable_specialization(
                        &function,
                        &instance.emitted_name,
                        Some(&instance.key),
                        |_| Ok(()),
                    )
                    .expect("multi-bound specialization setup");
            },
        );
    }

    #[test]
    fn conflicting_layouts_for_one_substituted_type_reject() {
        let required = packed_list_layout(&mut LayoutInterner::new(), RcHeader::Required);
        let elided = packed_list_layout(&mut LayoutInterner::new(), RcHeader::Elided);
        let template = generic_relay_template(2);
        let function = specialized_relay(2);
        let key = physical_key(
            &function,
            logical_relay_identity(&template, &[list_point()]),
            vec![Some(required), Some(elided)],
            Some(required),
        );

        let error = instance_type_layouts(&function, &key)
            .expect_err("one logical type cannot have conflicting physical IDs");
        assert!(error.message.contains("conflicting physical layouts"));
    }

    #[test]
    fn specialized_and_ordinary_positions_for_one_type_reject() {
        let layout = packed_list_layout(&mut LayoutInterner::new(), RcHeader::Required);
        let template = generic_relay_template(2);
        let function = specialized_relay(2);
        let key = physical_key(
            &function,
            logical_relay_identity(&template, &[list_point()]),
            vec![Some(layout), None],
            Some(layout),
        );

        let error = instance_type_layouts(&function, &key)
            .expect_err("type-directed body layout cannot mix specialized and ordinary ABI");
        assert!(error.message.contains("conflicting physical layouts"));
    }

    #[test]
    fn missing_and_out_of_range_logical_arguments_reject() {
        let layout = packed_list_layout(&mut LayoutInterner::new(), RcHeader::Required);
        let template = generic_relay_template(1);
        let function = specialized_relay(1);
        for concrete_types in [Vec::new(), vec![list_point(), named("Int")]] {
            let key = physical_key(
                &function,
                logical_relay_identity(&template, &concrete_types),
                vec![Some(layout)],
                Some(layout),
            );
            let error = instance_type_layouts(&function, &key)
                .expect_err("logical argument arity mismatch must reject");
            assert!(error.message.contains("logical type parameters"));
            assert!(error.message.contains("concrete arguments"));
        }
    }

    #[test]
    fn substituted_signature_mismatch_rejects() {
        let layout = packed_list_layout(&mut LayoutInterner::new(), RcHeader::Required);
        let template = generic_relay_template(1);
        let mut function = specialized_relay(1);
        function.ret = Some(named("Point"));
        let key = physical_key(
            &function,
            logical_relay_identity(&template, &[list_point()]),
            vec![Some(layout)],
            Some(layout),
        );

        let error = instance_type_layouts(&function, &key)
            .expect_err("checked function drift from logical substitution must reject");
        assert!(error.message.contains("result does not match"));
    }

    #[test]
    fn ambiguous_instance_lookup_cannot_downgrade_to_ordinary_abi() {
        with_codegen("fn main() -> Int:\n    0\n", |codegen, _| {
            let required =
                packed_list_layout(&mut codegen.specialized_layouts, RcHeader::Required);
            let elided = packed_list_layout(&mut codegen.specialized_layouts, RcHeader::Elided);
            let template = generic_relay_template(2);
            let function = specialized_relay(2);
            let key = physical_key(
                &function,
                logical_relay_identity(&template, &[list_point()]),
                vec![Some(required), Some(elided)],
                Some(required),
            );

            let error = codegen
                .layout_id_in_instance(&list_point(), &function, Some(&key))
                .expect_err("ambiguous instance lookup must propagate rejection");
            assert!(error.message.contains("conflicting physical layouts"));
        });
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
    fn missing_generic_call_type_facts_fail_closed() {
        with_codegen(
            "fn identity(value: a) -> a:\n    value\n\nfn main() -> Int:\n    identity(1)\n",
            |codegen, specializations| {
                let main = function_named(codegen, "main");
                let logical = specializations
                    .keys()
                    .find(|name| name.starts_with("identity__"))
                    .expect("identity specialization");
                let calls = calls_to(main, logical);
                let [call] = calls.as_slice() else {
                    panic!("one generic identity call")
                };
                let cloned_call = (*call).clone();
                let Expr::Call { args, .. } = &cloned_call else {
                    unreachable!("cloned generic call")
                };
                let error = codegen
                    .call_layout_signature(&cloned_call, args, main, None)
                    .expect_err("cloned expression has no checked type-table identity");
                assert!(error.message.contains("no checked concrete type"));
                assert!(error.message.contains("cannot select an ordinary ABI"));
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
