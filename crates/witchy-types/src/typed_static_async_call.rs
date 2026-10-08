use crate::async_control_flow::{AsyncCallableId, SourceBindingId, SourceExpressionId, source_expression};
use crate::pipeline::CheckedModule;
use crate::typed_async_lowering::{FinalCallableIndex, TypedAsyncLoweringInput, TypedAsyncOperationKind,
    TypedAsyncStateKind, typed_source_function};
use crate::typed_execution::LaneOwnership;
use witchy_canonical_source::source::SourceExecutionMode;
use witchy_canonical_source::source::{TaskAwaitProof, TaskBehaviorContract};
use witchy_canonical_source::closure::{ClosureBindingPathStep, ClosureCaptureTransfer};
use witchy_canonical_source::proper_tail::ProperTailCallableId;
use witchy_syntax::ast::{Convention, Expr, Type};

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct StaticAsyncCallGraph {
    edges: Vec<(AsyncCallableId, u32, AsyncCallableId)>,
    unresolved: std::collections::BTreeSet<AsyncCallableId>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StaticAsyncCallArgument {
    pub ordinal: u32,
    pub expression: SourceExpressionId,
    pub source_bindings: Vec<SourceBindingId>,
    pub callee_parameter: u32,
    pub callee_binding: SourceBindingId,
    pub ty: Type,
    pub convention: Convention,
    pub ownership: LaneOwnership,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StaticAsyncCallCapture {
    pub capture: u32,
    pub parameter: u32,
    pub ty: Type,
    pub transfer: ClosureCaptureTransfer,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StaticAsyncCallBinding {
    pub caller: AsyncCallableId,
    pub source_operation: u32,
    pub await_proof: TaskAwaitProof,
    pub caller_mode: SourceExecutionMode,
    pub callee: AsyncCallableId,
    pub callee_mode: SourceExecutionMode,
    pub callee_entry: FinalCallableIndex,
    pub canonical_callee_item: u32,
    pub callee_behavior: TaskBehaviorContract,
    pub callee_thunk: ProperTailCallableId,
    pub captures: Vec<StaticAsyncCallCapture>,
    pub arguments: Vec<StaticAsyncCallArgument>,
    pub result: Type,
    pub continuation: Option<FinalCallableIndex>,
    pub terminal: bool,
}

pub struct CheckedStaticAsyncCallSite<'a> {
    checked: &'a CheckedModule,
    binding: StaticAsyncCallBinding,
}

impl CheckedStaticAsyncCallSite<'_> {
    pub fn binding(&self) -> &StaticAsyncCallBinding { &self.binding }

    pub fn validate_for(&self, checked: &CheckedModule) -> Result<(), String> {
        if !std::ptr::eq(self.checked, checked) {
            return Err("static async call site belongs to a different checked allocation".into());
        }
        Ok(())
    }
}

pub fn checked_static_async_call_sites(checked: &CheckedModule) -> Result<Vec<CheckedStaticAsyncCallSite<'_>>, String> {
    let input = TypedAsyncLoweringInput::from_checked(checked)?;
    Ok(input.static_calls.into_iter().map(|binding| CheckedStaticAsyncCallSite { checked, binding }).collect())
}

impl TypedAsyncLoweringInput {
    pub(super) fn checked_static_calls(&self, checked: &CheckedModule)
        -> Result<(Vec<StaticAsyncCallBinding>, StaticAsyncCallGraph), String>
    {
        let Some(typed) = checked.typed_async_source() else { return Ok((Vec::new(), StaticAsyncCallGraph::default())) };
        let canonical = checked.canonical_source().ok_or("static async calls have no canonical source")?;
        let view = canonical.view();
        let callees = self.source.iter().filter_map(|source| {
            typed_source_function(typed, &source.callable).map(|function| (function.name.as_str(), source))
        }).collect::<std::collections::BTreeMap<_, _>>();
        let mut factories = std::collections::BTreeMap::<AsyncCallableId, Vec<(u32, Type)>>::new();
        for (item, definition) in canonical.typed_module().module().items.iter().enumerate() {
            let witchy_syntax::ast::Item::Function(function) = definition else { continue };
            if !function.attributes.iter().any(|attribute| attribute == witchy_syntax::suspension::FRAME_ENTRY_ATTRIBUTE) {
                continue;
            }
            let Some((source_item, method)) = witchy_syntax::suspension::async_surface(function) else { continue };
            let Some(module) = witchy_syntax::suspension::async_surface_module(function) else { continue };
            let Some(signature) = canonical.typed_module().table().function_type(&function.name) else { continue };
            let callable = AsyncCallableId { module: module.to_owned(), surface: witchy_syntax::async_surface::AsyncSurfaceId {
                item: u32::try_from(source_item).map_err(|_| "static async factory source item exceeds u32")?,
                method: method.map(u32::try_from).transpose().map_err(|_| "static async factory source method exceeds u32")?,
            }};
            factories.entry(callable).or_default().push((u32::try_from(item)
                .map_err(|_| "static async call canonical factory item exceeds u32")?, signature));
        }
        let mut calls = Vec::new();
        let mut graph = StaticAsyncCallGraph::default();
        'operations: for operation in &self.operations {
            if operation.runtime_kind == TypedAsyncOperationKind::DynamicAsyncCall {
                graph.unresolved.insert(operation.callable.clone());
                continue;
            }
            if operation.runtime_kind != TypedAsyncOperationKind::StaticAsyncCall { continue; }
            let (awaited, _) = Self::typed_source_operation(checked, operation)?;
            let Expr::Unary { op: witchy_syntax::ast::UnOp::Await, expr } = awaited else {
                graph.unresolved.insert(operation.callable.clone());
                continue;
            };
            let Expr::Call { name, args } = expr.as_ref() else {
                graph.unresolved.insert(operation.callable.clone());
                continue;
            };
            if typed.table().local_callee_type(expr).is_some() {
                graph.unresolved.insert(operation.callable.clone());
                continue;
            }
            let Some(callee_source) = callees.get(name.as_str()).copied() else {
                graph.unresolved.insert(operation.callable.clone());
                continue;
            };
            graph.edges.push((operation.callable.clone(), operation.source_operation, callee_source.callable.clone()));
            let Some(Type::Fn(parameters, output, conventions, _)) = typed.table().function_type(name) else {
                return Err(format!("static async call {name} has no checked declaration signature"));
            };
            if parameters.len() != args.len() || conventions.len() != args.len() {
                return Err(format!("static async call {name} has inconsistent checked argument arity"));
            }
            let entry = self.states.iter().find(|state| state.callable == callee_source.callable
                && state.kind == TypedAsyncStateKind::Entry)
                .ok_or_else(|| format!("static async call {name} has no final entry frame"))?;
            if entry.frame.len() != args.len() {
                return Err(format!("static async call {name} has inconsistent final entry arity"));
            }
            let canonical_factories = factories.get(&callee_source.callable).into_iter().flatten().filter_map(|(item, signature)| {
                let Type::Fn(actual_parameters, actual_output, actual_conventions, _) = signature else { return None };
                (actual_parameters == &parameters && actual_conventions == &conventions && actual_output == &output)
                    .then_some(*item)
            }).collect::<Vec<_>>();
            let [canonical_callee_item] = canonical_factories.as_slice() else { continue };
            let canonical_callee_item = *canonical_callee_item;
            let Some(callee_behavior) = view.task_behavior_at(canonical_callee_item) else { continue };
            if callee_behavior.async_source_identity() != Some((callee_source.callable.module.as_str(),
                callee_source.callable.surface.item, callee_source.callable.surface.method))
                || callee_behavior.source_mode() != callee_source.mode
                || callee_behavior.conventions() != conventions
            {
                return Err("static async call callee behavior differs from its source and final frame".into());
            }
            let Some(thunk) = callee_behavior.async_thunk_callable() else { continue };
            let Some(witness) = view.closure_witness().entries().iter().find(|entry| entry.id() == thunk) else { continue };
            let mut captures = Vec::with_capacity(witness.captures().len());
            for (ordinal, capture) in witness.captures().iter().enumerate() {
                let binding = capture.binding();
                if binding.owner().item != canonical_callee_item || binding.owner().method.is_some()
                    || !binding.owner().lambda_path.is_empty()
                { continue 'operations; }
                let [ClosureBindingPathStep::Parameter(parameter)] = binding.path() else {
                    continue 'operations;
                };
                if parameters.get(*parameter as usize) != Some(capture.checked_type()) {
                    continue 'operations;
                }
                captures.push(StaticAsyncCallCapture {
                    capture: u32::try_from(ordinal).map_err(|_| "static async factory capture ordinal exceeds u32")?,
                    parameter: *parameter, ty: capture.checked_type().clone(), transfer: capture.transfer(),
                });
            }
            let caller_source = self.source.iter().find(|source| source.callable == operation.callable)
                .ok_or("static async call has no source caller")?;
            let caller = typed_source_function(typed, &operation.callable)
                .ok_or("static async call has no checked source caller")?;
            let suspension = caller_source.operations.iter().filter(|candidate| {
                candidate.kind == crate::async_control_flow::SourceAsyncOperationKind::Await
            }).position(|candidate| candidate.ordinal == operation.source_operation)
                .and_then(|index| u32::try_from(index).ok())
                .ok_or("static async call has no source suspension coordinate")?;
            let await_proof = view.task_await_proofs().iter().find(|proof| proof.source_identity()
                == (operation.callable.module.as_str(), operation.callable.surface.item,
                    operation.callable.surface.method, suspension))
                .ok_or("static async call lacks exact checked task await proof")?;
            if await_proof.source_mode() != caller_source.mode
                || (caller_source.mode.is_opt() && !await_proof.opt_compatible())
            {
                return Err("static async call has incompatible checked execution mode".into());
            }
            let result = operation.operand_type.clone().ok_or("static async call has no completed result type")?;
            if !matches!(output.as_ref(), Type::Named(name, arguments)
                if (name == "Task" || name == "task.Task") && arguments.as_slice() == [result.clone()])
            {
                return Err(format!("static async call {name} has inconsistent completed result type"));
            }
            let mut arguments = Vec::with_capacity(args.len());
            let mut argument_coordinate = operation.expression
                .and_then(|coordinate| coordinate.0.checked_add(2)).map(SourceExpressionId)
                .ok_or("static async call argument coordinate exceeds u32")?;
            for (ordinal, (((argument, parameter), convention), field)) in args.iter()
                .zip(&parameters).zip(&conventions).zip(&entry.frame).enumerate()
            {
                let actual = typed.table().type_of(argument).and_then(crate::typeck::ty_to_ast)
                    .ok_or("static async call argument has no concrete checked type")?;
                if actual != *parameter || field.ty != *parameter || field.convention != *convention {
                    return Err(format!("static async call {name} argument {ordinal} differs from its final entry field"));
                }
                let expression = argument_coordinate;
                if !source_expression(caller, expression).is_some_and(|candidate| std::ptr::eq(candidate, argument)) {
                    return Err("static async call argument differs from its exact source expression coordinate".into());
                }
                argument_coordinate = caller_source.expression_spans.iter().find(|span| span.root == expression)
                    .map(|span| span.end).ok_or("static async call argument has no source expression span")?;
                arguments.push(StaticAsyncCallArgument {
                    ordinal: u32::try_from(ordinal).map_err(|_| "static async call argument ordinal exceeds u32")?,
                    expression,
                    source_bindings: caller_source.bindings_read_by(expression)?,
                    callee_parameter: field.parameter,
                    callee_binding: field.source_binding.ok_or("static async call entry field has no source binding")?,
                    ty: actual,
                    convention: *convention,
                    ownership: field.ownership,
                });
            }
            calls.push(StaticAsyncCallBinding {
                caller: operation.callable.clone(), source_operation: operation.source_operation,
                await_proof: await_proof.clone(),
                caller_mode: caller_source.mode, callee: callee_source.callable.clone(),
                callee_mode: callee_source.mode, callee_entry: entry.final_callable,
                canonical_callee_item,
                callee_behavior: callee_behavior.clone(),
                callee_thunk: thunk.clone(), captures,
                arguments, result, continuation: operation.continuation, terminal: operation.terminal,
            });
        }
        Ok((calls, graph))
    }

    pub(crate) fn static_call_contract_matches(&self, other: &Self) -> bool {
        self.static_calls == other.static_calls && self.static_call_graph == other.static_call_graph
    }

    pub fn static_call_is_acyclic(&self, binding: &StaticAsyncCallBinding) -> bool {
        if !self.static_calls.contains(binding) { return false; }
        let mut pending = vec![(&binding.caller, false)];
        let mut visited = std::collections::BTreeSet::new();
        let mut active = std::collections::BTreeSet::new();
        while let Some((callable, leaving)) = pending.pop() {
            if leaving {
                active.remove(callable);
                visited.insert(callable);
                continue;
            }
            if self.static_call_graph.unresolved.contains(callable) { return false; }
            if visited.contains(callable) { continue; }
            if !active.insert(callable) { return false; }
            pending.push((callable, true));
            pending.extend(self.static_call_graph.edges.iter().filter(|(caller, _, _)| caller == callable)
                .map(|(_, _, callee)| (callee, false)));
        }
        true
    }
}
