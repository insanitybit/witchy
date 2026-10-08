use witchy_canonical_source::source::TaskAwaitProof;
use witchy_syntax::ast::{Expr, Type, UnOp};

use crate::async_control_flow::{AsyncCallableId, SourceAsyncOperationKind, SourceExpressionId, source_expression};
use crate::pipeline::CheckedModule;
use crate::runtime_type::{DeclarationIdentity, DeclarationKind, PackageSource};
use crate::typed_async_lowering::{FinalCallableIndex, TypedAsyncLoweringInput, typed_source_function};
use crate::typeck;
use std::collections::BTreeMap;

#[derive(Debug, PartialEq)]
pub struct CheckedSendPreparation {
    endpoint_type: Type,
    payload_type: Type,
}

impl CheckedSendPreparation {
    pub fn endpoint_type(&self) -> &Type { &self.endpoint_type }
    pub fn payload_type(&self) -> &Type { &self.payload_type }
}

/// Exact source operands for a channel-send await, borrowed from its checked allocation.
/// This establishes source correspondence, not scheduler ordering or cancellation admission.
pub struct CheckedChannelSendSite<'a> {
    checked: &'a CheckedModule,
    callable: AsyncCallableId,
    operation: u32,
    expression: SourceExpressionId,
    endpoint: &'a Expr,
    payload: &'a Expr,
    payload_type: Type,
    declaration: DeclarationIdentity,
    proof: &'a TaskAwaitProof,
    continuation: Option<FinalCallableIndex>,
}

impl<'a> CheckedChannelSendSite<'a> {
    pub fn callable(&self) -> &AsyncCallableId { &self.callable }
    pub fn operation(&self) -> u32 { self.operation }
    pub fn expression(&self) -> SourceExpressionId { self.expression }
    pub fn endpoint(&self) -> &'a Expr { self.endpoint }
    pub fn payload(&self) -> &'a Expr { self.payload }
    pub fn payload_type(&self) -> &Type { &self.payload_type }
    pub fn declaration(&self) -> &DeclarationIdentity { &self.declaration }
    pub fn proof(&self) -> &'a TaskAwaitProof { self.proof }
    pub fn continuation(&self) -> Option<FinalCallableIndex> { self.continuation }

    pub fn validate_for(&self, checked: &CheckedModule) -> Result<(), String> {
        if !std::ptr::eq(self.checked, checked) {
            return Err("channel-send site belongs to a different checked allocation".into());
        }
        Ok(())
    }
}

pub fn checked_channel_send_sites(checked: &CheckedModule) -> Result<Vec<CheckedChannelSendSite<'_>>, String> {
    let canonical = checked.canonical_source()
        .ok_or("channel-send sites require canonical checked source")?;
    let Some(typed) = checked.typed_async_source() else { return Ok(Vec::new()) };
    let catalog = checked.runtime_declaration_catalog().map_err(|error| error.to_string())?;
    let input = TypedAsyncLoweringInput::from_checked(checked)?;
    let view = canonical.view();
    let mut sites = Vec::new();
    for flow in &input.source {
        let function = typed_source_function(typed, &flow.callable)
            .ok_or("channel-send site lost its typed source callable")?;
        let mut awaits = flow.operations.iter().filter(|operation| {
            operation.kind == SourceAsyncOperationKind::Await
        }).collect::<Vec<_>>();
        awaits.sort_by_key(|operation| operation.expression);
        for (suspension, operation) in awaits.into_iter().enumerate() {
            let coordinate = operation.expression.ok_or("channel-send await lacks source expression")?;
            let expression = source_expression(function, coordinate)
                .ok_or("channel-send await lost its typed source expression")?;
            let Expr::Unary { op: UnOp::Await, expr } = expression else {
                return Err("channel-send source operation is not an await".into());
            };
            let Expr::Call { name, args } = expr.as_ref() else { continue };
            let Some(declaration) = catalog.resolve(name, DeclarationKind::Function) else { continue };
            if declaration.package().source() != &PackageSource::Toolchain
                || declaration.package().name() != "witchy/std"
                || declaration.module() != ["std", "chan"] || declaration.name() != "send"
            { continue; }
            let [endpoint, payload] = args.as_slice() else {
                return Err("checked channel-send operation does not have two operands".into());
            };
            let concrete_type = |expression: &Expr| {
                typed.table().type_of(expression).and_then(typeck::ty_to_ast)
                    .ok_or_else(|| "checked channel-send operand lacks a concrete type".to_string())
            };
            let endpoint_type = concrete_type(endpoint)?;
            let payload_type = concrete_type(payload)?;
            let Type::Named(endpoint_name, payloads) = endpoint_type.unqualified() else {
                return Err("checked channel-send endpoint is not nominal".into());
            };
            let endpoint_declaration = catalog.resolve(endpoint_name, DeclarationKind::Type)
                .ok_or("checked channel-send endpoint lacks authenticated identity")?;
            if endpoint_declaration.package() != declaration.package()
                || endpoint_declaration.module() != declaration.module()
                || endpoint_declaration.name() != "Sender" || payloads.as_slice() != [payload_type.clone()]
            {
                return Err("checked channel-send endpoint and payload identities disagree".into());
            }
            let suspension = u32::try_from(suspension).map_err(|_| "channel-send suspension exceeds u32")?;
            let proof = view.task_await_proofs().iter().find(|proof| {
                proof.source_identity() == (flow.callable.module.as_str(), flow.callable.surface.item,
                    flow.callable.surface.method, suspension)
            }).ok_or("channel-send await lacks exact checked task proof")?;
            if proof.source_mode() != flow.mode || (flow.mode.is_opt() && !proof.opt_compatible()) {
                return Err("channel-send await has incompatible checked execution mode".into());
            }
            let binding = input.operations.iter().find(|binding| {
                binding.callable == flow.callable && binding.source_operation == operation.ordinal
            }).ok_or("channel-send await lacks final typed operation binding")?;
            if binding.expression != Some(coordinate) || (!binding.terminal && binding.continuation.is_none()) {
                return Err("channel-send await lacks exact final continuation binding".into());
            }
            sites.push(CheckedChannelSendSite { checked, callable: flow.callable.clone(),
                operation: operation.ordinal, expression: coordinate, endpoint, payload, payload_type,
                declaration: declaration.clone(), proof, continuation: binding.continuation });
        }
    }
    Ok(sites)
}

pub(crate) fn bind_send_preparations(checked: &CheckedModule) -> Result<BTreeMap<usize, CheckedSendPreparation>, String> {
    let canonical = checked.canonical_source().ok_or("send preparation requires canonical source")?;
    let typed = canonical.typed_module();
    let mut preparations = Vec::new();
    for (item, definition) in typed.module().items.iter().enumerate() {
        let witchy_syntax::ast::Item::Function(function) = definition else { continue };
        witchy_syntax::ast::visit::visit_block(&function.body, &mut |expression| {
            if let Expr::Call { name, .. } = expression
                && crate::suspension_carrier::call_family(name, "task.__prepare_send")
            {
                preparations.push((item, expression));
            }
            Ok::<_, String>(())
        })?;
    }
    if preparations.is_empty() { return Ok(BTreeMap::new()); }
    let has_state_factory = checked.module().items.iter().any(|item| {
        let witchy_syntax::ast::Item::Function(function) = item else { return false };
        let mut found = false;
        witchy_syntax::ast::visit::visit_block(&function.body, &mut |expression| {
            if let Expr::Call { name, .. } = expression {
                found |= crate::suspension_carrier::call_family(name, "task.__producer_state_frame")
                    || crate::suspension_carrier::call_family(name, "task.__terminal_producer_state_frame");
            }
            Ok::<_, ()>(())
        }).expect("infallible producer factory inventory");
        found
    });
    if has_state_factory {
        let original = checked.typed_async_source().ok_or("producer state factory lost its original typed source")?;
        let catalog = checked.runtime_declaration_catalog().map_err(|error| error.to_string())?;
        crate::checked_scalar_contract::scheduler_contract(checked.module(), original.module(), &catalog)?;
    }
    if !plain_send_implementation(checked.module()) {
        return Err("compiler producer frame requires the exact checked standard send implementation".into());
    }
    let sites = checked_channel_send_sites(checked)?;
    let catalog = checked.runtime_declaration_catalog().map_err(|error| error.to_string())?;
    let view = canonical.view();
    let mut bindings = BTreeMap::new();
    for (item, expression) in preparations {
        let Expr::Call { args, .. } = expression else { unreachable!() };
        let [Expr::Block(block)] = args.as_slice() else { continue };
        let [witchy_syntax::ast::Stmt::Let { name, value: Expr::Int(identity), .. },
            witchy_syntax::ast::Stmt::Expr(Expr::Call { name: callee, args })] = block.stmts.as_slice()
            else { continue };
        let Some(encoded) = name.strip_prefix(witchy_syntax::suspension::SOURCE_TASK_OPERAND_PREFIX) else { continue };
        let identity = u32::try_from(*identity).map_err(|_| "invalid prepared send identity")?;
        if encoded.parse::<u32>().ok() != Some(identity) || args.len() != 2
            || !crate::suspension_carrier::call_family(callee, "chan.send")
        { return Err("prepared send differs from its checked operation".into()); }
        let item = u32::try_from(item).map_err(|_| "prepared send item exceeds u32")?;
        let matching = sites.iter().filter(|site| {
            site.proof().source_identity().3 == identity && view.task_operand_proofs_at(item)
                .any(|proof| proof.source_identity() == site.proof().source_identity())
        }).collect::<Vec<_>>();
        let [site] = matching.as_slice() else {
            return Err("prepared send lacks one exact authenticated source operation".into());
        };
        let concrete = |expression| typed.table().type_of(expression).and_then(typeck::ty_to_ast)
            .ok_or_else(|| "prepared send operand lacks a concrete checked type".to_string());
        let endpoint_type = concrete(&args[0])?;
        let payload_type = concrete(&args[1])?;
        if payload_type != *site.payload_type() || catalog.type_identity(&payload_type).map_err(|error| error.to_string())?
            != catalog.type_identity(site.payload_type()).map_err(|error| error.to_string())?
        { return Err("prepared send payload differs from its checked source type".into()); }
        bindings.insert(expression as *const _ as usize, CheckedSendPreparation { endpoint_type, payload_type });
    }
    Ok(bindings)
}

fn plain_send_implementation(module: &witchy_syntax::ast::Module) -> bool {
    use witchy_syntax::ast::{Block, Function, Item, Pattern, Stmt};
    fn function<'a>(module: &'a witchy_syntax::ast::Module, name: &str) -> Option<&'a Function> {
        module.items.iter().find_map(|item| match item {
            Item::Function(function) if function.name == name => Some(function),
            _ => None,
        })
    }
    fn tail(block: &Block) -> Option<&Expr> {
        match block.stmts.as_slice() { [Stmt::Expr(expression)] => Some(expression), _ => None }
    }
    fn single_match<'a>(function: &'a Function, parameter: &str, constructor: &str) -> Option<(&'a str, &'a Expr)> {
        let Expr::Match { scrutinee, arms } = tail(&function.body)? else { return None };
        if !matches!(scrutinee.as_ref(), Expr::Var(name) if name == parameter) { return None; }
        let [arm] = arms.as_slice() else { return None };
        let Pattern::Ctor { name, args } = &arm.pattern else { return None };
        if !crate::suspension_carrier::call_family(name, constructor) { return None; }
        let [Pattern::Var(binding)] = args.as_slice() else { return None };
        if arm.guard.is_some() { return None; }
        Some((binding, &arm.body))
    }
    fn ctor_args<'a>(expression: &'a Expr, family: &str) -> Option<&'a [Expr]> {
        match expression {
            Expr::Ctor { name, args } | Expr::Call { name, args }
                if crate::suspension_carrier::call_family(name, family) => Some(args),
            _ => None,
        }
    }
    let Some(unit) = function(module, "task.unit") else { return false };
    if !unit.params.is_empty() || !matches!(unit.body.stmts.as_slice(),
        [Stmt::Let { name, mutable: false, value: Expr::Int(0), .. }]
        | [Stmt::LetPattern { pattern: Pattern::Var(name), value: Expr::Int(0) }] if name == "_ignore")
    { return false; }
    let Some(ready) = function(module, "task.ready_unit") else { return false };
    let Some([Expr::Lambda { params, body, .. }]) = tail(&ready.body)
        .and_then(|expression| ctor_args(expression, "task.Task")) else { return false };
    if !ready.params.is_empty() || !params.is_empty() { return false; }
    let Some([Expr::Call { name, args }]) = tail(body)
        .and_then(|expression| ctor_args(expression, "task.Done")) else { return false };
    if !crate::suspension_carrier::call_family(name, "task.unit") || !args.is_empty() { return false; }
    let Some(poll) = function(module, "task.poll") else { return false };
    let [task] = poll.params.as_slice() else { return false };
    let Some((thunk, expression)) = single_match(poll, &task.name, "task.Task") else { return false };
    if !matches!(expression, Expr::Call { name, args } if name == thunk && args.is_empty()) { return false; }
    let Some(sequence) = function(module, "task.and_then") else { return false };
    let [task, continuation] = sequence.params.as_slice() else { return false };
    let Some([Expr::Lambda { params, body, .. }]) = tail(&sequence.body)
        .and_then(|expression| ctor_args(expression, "task.Task")) else { return false };
    let Some(Expr::Call { name, args }) = tail(body) else { return false };
    if !params.is_empty() || !crate::suspension_carrier::call_family(name, "task.and_then_step")
        || !matches!(args.as_slice(), [Expr::Call { name, args }, Expr::Var(k)]
            if crate::suspension_carrier::call_family(name, "task.poll")
            && matches!(args.as_slice(), [Expr::Var(t)] if t == &task.name) && k == &continuation.name)
    { return false; }
    let Some(sequence_step) = function(module, "task.and_then_step") else { return false };
    let [step, continuation] = sequence_step.params.as_slice() else { return false };
    let Some(Expr::Match { scrutinee, arms }) = tail(&sequence_step.body) else { return false };
    if !matches!(scrutinee.as_ref(), Expr::Var(name) if name == &step.name) { return false; }
    if arms.iter().any(|arm| !matches!(arm.pattern, Pattern::Ctor { .. })) { return false; }
    let done = arms.iter().filter(|arm| matches!(&arm.pattern, Pattern::Ctor { name, .. }
        if crate::suspension_carrier::call_family(name, "task.Done"))).collect::<Vec<_>>();
    let [done] = done.as_slice() else { return false };
    let Pattern::Ctor { args, .. } = &done.pattern else { return false };
    let [Pattern::Var(value)] = args.as_slice() else { return false };
    let Some([Expr::Call { name, args }]) = ctor_args(&done.body, "task.Yield") else { return false };
    if done.guard.is_some() || name != &continuation.name
        || !matches!(args.as_slice(), [Expr::Var(argument)] if argument == value) { return false; }
    let yielded = arms.iter().filter(|arm| matches!(&arm.pattern, Pattern::Ctor { name, .. }
        if crate::suspension_carrier::call_family(name, "task.Yield"))).collect::<Vec<_>>();
    let [yielded] = yielded.as_slice() else { return false };
    let Pattern::Ctor { args, .. } = &yielded.pattern else { return false };
    let [Pattern::Var(task)] = args.as_slice() else { return false };
    let Some([Expr::Call { name, args }]) = ctor_args(&yielded.body, "task.Yield") else { return false };
    if yielded.guard.is_some() || !crate::suspension_carrier::call_family(name, "task.and_then")
        || !matches!(args.as_slice(), [Expr::Var(argument), Expr::Var(k)]
            if argument == task && k == &continuation.name) { return false; }
    let push = arms.iter().filter(|arm| matches!(&arm.pattern, Pattern::Ctor { name, .. }
        if crate::suspension_carrier::call_family(name, "task.Push"))).collect::<Vec<_>>();
    let [push] = push.as_slice() else { return false };
    let Pattern::Ctor { args, .. } = &push.pattern else { return false };
    let [Pattern::Var(channel), Pattern::Var(payload), Pattern::Var(next)] = args.as_slice() else { return false };
    let Some([Expr::Var(channel_arg), Expr::Var(payload_arg), Expr::Lambda { params, body, .. }])
        = ctor_args(&push.body, "task.Push") else { return false };
    let [unit] = params.as_slice() else { return false };
    if push.guard.is_some() || channel_arg != channel || payload_arg != payload { return false; }
    let Some(Expr::Call { name, args }) = tail(body) else { return false };
    if !crate::suspension_carrier::call_family(name, "task.and_then")
        || !matches!(args.as_slice(), [Expr::Call { name, args }, Expr::Var(k)]
            if name == next && matches!(args.as_slice(), [Expr::Var(value)] if value == &unit.name)
            && k == &continuation.name) { return false; }
    let Some(send) = function(module, "chan.send") else { return false };
    let [endpoint, payload] = send.params.as_slice() else { return false };
    let Some((channel, body)) = single_match(send, &endpoint.name, "chan.Sender") else { return false };
    let Some([Expr::Var(channel_arg), erased]) = ctor_args(body, "task.__channel_send") else { return false };
    if channel_arg != channel { return false; }
    let Expr::Call { name, args } = erased else { return false };
    if !witchy_syntax::intrinsics::lookup(name).is_some_and(|spec| {
        spec.id == witchy_syntax::intrinsics::IntrinsicId::Erase
    }) || !matches!(args.as_slice(), [Expr::Var(name)] if name == &payload.name) { return false; }
    let Some(sender) = function(module, "task.__channel_send") else { return false };
    let [endpoint, payload] = sender.params.as_slice() else { return false };
    let Some((channel, body)) = single_match(sender, &endpoint.name, "task.ChannelId") else { return false };
    let Some([Expr::Lambda { params, body, .. }]) = ctor_args(body, "task.Task") else { return false };
    if !params.is_empty() { return false; }
    let Some(body) = tail(body) else { return false };
    let Some([Expr::Var(channel_arg), Expr::Var(payload_arg), Expr::Lambda { params, body, .. }])
        = ctor_args(body, "task.Push") else { return false };
    if channel_arg != channel || payload_arg != &payload.name || params.len() != 1 { return false; }
    if !matches!(tail(body), Some(Expr::Call { name, args })
        if crate::suspension_carrier::call_family(name, "task.ready_unit") && args.is_empty()) { return false; }
    let Some(prepare) = function(module, "task.__prepare_send") else { return false };
    let [task] = prepare.params.as_slice() else { return false };
    let Some(Expr::Match { scrutinee, arms }) = tail(&prepare.body) else { return false };
    if !matches!(scrutinee.as_ref(), Expr::Call { name, args }
        if crate::suspension_carrier::call_family(name, "task.poll")
        && matches!(args.as_slice(), [Expr::Var(name)] if name == &task.name)) { return false; }
    let [success, _] = arms.as_slice() else { return false };
    let Pattern::Ctor { name, args } = &success.pattern else { return false };
    if !crate::suspension_carrier::call_family(name, "task.Push") { return false; }
    let [Pattern::Var(channel), Pattern::Var(payload), Pattern::Wildcard] = args.as_slice() else { return false };
    success.guard.is_none() && matches!(&success.body, Expr::Tuple(fields)
        if matches!(fields.as_slice(), [Expr::Var(first), Expr::Var(second)] if first == channel && second == payload))
}
