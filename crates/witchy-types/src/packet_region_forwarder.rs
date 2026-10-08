use witchy_syntax::ast::{Convention, Expr, Function, Item, Stmt, Type, UnOp};
use crate::async_control_flow::{AsyncCallableId, SourceAsyncControlFlow, SourceAsyncOperationKind,
    SourceBindingId, SourceBindingKind, SourceExpressionId, source_expression};
use crate::packet_region_recipe::PacketShape;
use crate::pipeline::CheckedModule;
use crate::runtime_type::{DeclarationKind, PackageSource, RuntimeDeclarationCatalog};
use crate::typeck;

#[derive(Debug, PartialEq)]
pub(crate) struct PacketSendForwarder {
    pub(crate) helper: Function,
    pub(crate) helper_flow: SourceAsyncControlFlow,
    caller: AsyncCallableId,
    operation: u32,
    arguments: [SourceExpressionId; 2],
    reads: [Vec<SourceBindingId>; 2],
    formals: [SourceBindingId; 2],
    types: [Type; 2],
}

impl PacketSendForwarder {
    pub(crate) fn derive(checked: &CheckedModule, catalog: &RuntimeDeclarationCatalog,
        producer: &Function, shape: &PacketShape,
        flows: &[SourceAsyncControlFlow]) -> Option<Self>
    {
        if shape.definition.variants[0].fields.iter().any(|ty|
            !matches!(ty, Type::Named(name, args) if name == "Int" && args.is_empty())) { return None; }
        let typed = checked.typed_async_source()?;
        let caller_flow = flows.iter().find(|flow|
            crate::typed_async_lowering::typed_source_function(typed, &flow.callable)
                .is_some_and(|source| std::ptr::eq(source, producer) || source == producer))?;
        let producer = crate::typed_async_lowering::typed_source_function(typed, &caller_flow.callable)?;
        let operations = caller_flow.operations.iter().filter(|operation|
            operation.kind == SourceAsyncOperationKind::Await).collect::<Vec<_>>();
        let [operation] = operations.as_slice() else { return None };
        let Expr::Unary { op: UnOp::Await, expr } = source_expression(producer, operation.expression?)? else { return None };
        let Expr::Call { name, args } = expr.as_ref() else { return None };
        if typed.table().local_callee_type(expr).is_some() { return None; }
        let [endpoint, payload] = args.as_slice() else { return None };
        let Expr::Var(_) = endpoint else { return None };
        let Expr::Ctor { name: constructor, args: fields } = payload else { return None };
        if constructor != shape.constructor() || fields.len() != shape.definition.variants[0].fields.len() { return None; }
        let helper = typed.module().items.iter().find_map(|item| match item {
            Item::Function(function) if function.name == *name && function.is_async => Some(function), _ => None,
        })?;
        let helper_flow = flows.iter().find(|flow|
            crate::typed_async_lowering::typed_source_function(typed, &flow.callable)
                .is_some_and(|source| std::ptr::eq(source, helper)))?;
        if helper_flow.mode != caller_flow.mode || helper_flow.mode.is_opt()
            || helper.is_gen || !helper.type_params.is_empty() || !helper.bounds.is_empty()
            || helper.body.region.is_some() || helper.body.lines.len() != 1
            || helper.ret.as_ref().is_some_and(|ty| !matches!(ty, Type::Tuple(values) if values.is_empty()))
            || helper.params.len() != 2 || helper.params.iter().any(|parameter|
                parameter.convention != Convention::Let || parameter.default.is_some())
            || !helper_flow.controls.is_empty()
            || helper_flow.bindings.len() != 2
            || helper_flow.operations.iter().filter(|operation| operation.kind == SourceAsyncOperationKind::Await).count() != 1
            || helper_flow.operations.iter().any(|operation| matches!(operation.kind,
                SourceAsyncOperationKind::DynamicCall | SourceAsyncOperationKind::Return))
        { return None; }
        let [Stmt::Expr(Expr::Unary { op: UnOp::Await, expr: sending })] = helper.body.stmts.as_slice() else { return None };
        let Expr::Call { name: send, args: sent } = sending.as_ref() else { return None };
        let identity = catalog.resolve(send, DeclarationKind::Function)?;
        if identity.package().source() != &PackageSource::Toolchain || identity.package().name() != "witchy/std"
            || identity.module() != ["std", "chan"] || identity.name() != "send"
            || !matches!(sent.as_slice(), [Expr::Var(tx), Expr::Var(packet)]
                if tx == &helper.params[0].name && packet == &helper.params[1].name)
        { return None; }
        let concrete = |expression| typed.table().type_of(expression).and_then(typeck::ty_to_ast);
        let types = [concrete(endpoint)?, concrete(payload)?];
        if types != [Type::Named("chan.Sender".into(), vec![shape.payload_type()]), shape.payload_type()]
            || helper.params.iter().zip(&types).any(|(parameter, ty)| parameter.ty.as_ref() != Some(ty))
            || sent.iter().zip(&types).any(|(argument, ty)| concrete(argument).as_ref() != Some(ty))
        { return None; }
        let mut arguments = Vec::new();
        for argument in args {
            let expression = caller_flow.expression_spans.iter().find(|span|
                source_expression(producer, span.root).is_some_and(|source| std::ptr::eq(source, argument)))?.root;
            arguments.push(expression);
        }
        let arguments: [SourceExpressionId; 2] = arguments.try_into().ok()?;
        let reads = [caller_flow.bindings_read_by(arguments[0]).ok()?, caller_flow.bindings_read_by(arguments[1]).ok()?];
        let formals = helper.params.iter().map(|parameter| helper_flow.bindings.iter().find(|binding|
            binding.name == parameter.name && binding.kind == SourceBindingKind::Parameter).map(|binding| binding.id))
            .collect::<Option<Vec<_>>>()?.try_into().ok()?;
        for formal in formals {
            if helper_flow.binding_uses.iter().filter(|usage| usage.binding == formal).count() != 1 { return None; }
        }
        Some(Self { helper: helper.clone(), helper_flow: helper_flow.clone(), caller: caller_flow.callable.clone(),
            operation: operation.ordinal, arguments, reads, formals, types })
    }

    pub(crate) fn producer_projection(&self, original: &Function) -> Option<Function> {
        let mut surrogate = original.clone();
        let mut replaced = 0;
        witchy_syntax::ast::visit::visit_block_mut(&mut surrogate.body, &mut |expression| {
            if let Expr::Unary { op: UnOp::Await, expr } = expression
                && let Expr::Call { name, args } = expr.as_mut()
                && name == &self.helper.name && args.len() == 2
            {
                *name = "chan.send".into();
                replaced += 1;
            }
            Ok::<_, ()>(())
        }).ok()?;
        (replaced == 1).then_some(surrogate)
    }

    pub(crate) fn validate_checked(&self, checked: &CheckedModule) -> Result<(), String> {
        let input = crate::typed_async_lowering::TypedAsyncLoweringInput::from_checked(checked)?;
        let sites = crate::typed_static_async_call::checked_static_async_call_sites(checked)?;
        let matching = sites.iter().filter(|site| {
            let call = site.binding();
            call.caller == self.caller && call.source_operation == self.operation
        }).collect::<Vec<_>>();
        let [site] = matching.as_slice() else { return Err("packet forwarder lacks one actual checked static call binding".into()) };
        site.validate_for(checked)?;
        self.validate_call(checked, &input, site.binding())
    }

    fn validate_call(&self, checked: &CheckedModule, input: &crate::typed_async_lowering::TypedAsyncLoweringInput,
        call: &crate::typed_static_async_call::StaticAsyncCallBinding) -> Result<(), String>
    {
        if call.caller != self.caller || call.source_operation != self.operation {
            return Err("packet forwarder changes actual caller source operation".into());
        }
        if call.callee != self.helper_flow.callable || call.caller_mode != self.helper_flow.mode
            || call.callee_mode != self.helper_flow.mode || call.result != Type::Tuple(Vec::new())
            || call.arguments.len() != 2 || call.continuation.is_none() || call.terminal
            || !input.static_call_is_acyclic(call)
        { return Err("packet forwarder differs from actual source mode, callee or continuation".into()); }
        for (index, argument) in call.arguments.iter().enumerate() {
            if argument.ordinal != index as u32 || argument.expression != self.arguments[index]
                || argument.source_bindings != self.reads[index] || argument.callee_parameter != index as u32
                || argument.callee_binding != self.formals[index] || argument.ty != self.types[index]
                || argument.convention != Convention::Let
            { return Err("packet forwarder changes actual ordered operand or formal binding".into()); }
        }
        let canonical = checked.canonical_source().ok_or("packet forwarder has no canonical source authority")?;
        let Some(Item::Function(factory)) = canonical.typed_module().module().items.get(call.canonical_callee_item as usize)
            else { return Err("packet forwarder lost its actual callee factory".into()) };
        if factory.name != self.helper.name || factory.params != self.helper.params
            || call.callee_behavior.async_source_identity() != Some((self.helper_flow.callable.module.as_str(),
                self.helper_flow.callable.surface.item, self.helper_flow.callable.surface.method))
            || call.callee_behavior.source_mode() != self.helper_flow.mode
            || call.callee_behavior.async_thunk_callable() != Some(&call.callee_thunk)
            || call.captures.iter().any(|capture| capture.parameter >= 2 || capture.ty != self.types[capture.parameter as usize])
        { return Err("packet forwarder lost actual callee factory, capture or behavior correspondence".into()); }
        let entry = input.states.iter().find(|state| state.callable == self.helper_flow.callable
            && state.kind == crate::typed_async_lowering::TypedAsyncStateKind::Entry)
            .ok_or("packet forwarder lost actual callee entry frame")?;
        if entry.final_callable != call.callee_entry || entry.frame.len() != 2
            || entry.frame.iter().enumerate().any(|(index, field)| field.parameter != index as u32
                || field.source_binding != Some(self.formals[index]) || field.ty != self.types[index]
                || field.convention != Convention::Let || field.ownership != call.arguments[index].ownership)
        { return Err("packet forwarder changes actual callee frame layout or ownership".into()); }
        let witness = canonical.view().closure_witness().entries().iter().find(|entry| entry.id() == &call.callee_thunk)
            .ok_or("packet forwarder lost actual callee capture witness")?;
        if witness.captures().len() != call.captures.len() { return Err("packet forwarder changed callee capture count".into()); }
        for (ordinal, (actual, retained)) in witness.captures().iter().zip(&call.captures).enumerate() {
            let binding = actual.binding();
            let [witchy_canonical_source::closure::ClosureBindingPathStep::Parameter(parameter)] = binding.path()
                else { return Err("packet forwarder capture is not an original callee parameter".into()) };
            if binding.owner().item != call.canonical_callee_item || binding.owner().method.is_some()
                || !binding.owner().lambda_path.is_empty() || retained.capture != ordinal as u32
                || retained.parameter != *parameter || retained.ty != *actual.checked_type()
                || retained.transfer != actual.transfer()
            { return Err("packet forwarder changed actual callee capture transfer".into()); }
        }
        Ok(())
    }
}


pub(crate) fn assert_binding_forgery_refused(checked: &CheckedModule) {
    let catalog = checked.runtime_declaration_catalog().expect("checked catalog");
    let source = crate::checked_packet_region::PacketRegionSource::derive(checked, &catalog).expect("forwarded packet source");
    let forwarder = source.forwarder.expect("source forwarding facet");
    forwarder.validate_checked(checked).expect("actual static forwarding binding");
    let input = crate::typed_async_lowering::TypedAsyncLoweringInput::from_checked(checked).expect("actual typed input");
    let position = input.static_calls.iter().position(|call|
        call.caller == forwarder.caller && call.source_operation == forwarder.operation).expect("forwarding call");
    for mutation in 0..9 {
        let mut input = input.clone();
        let call = &mut input.static_calls[position];
        match mutation {
            0 => call.arguments[1].expression.0 += 1,
            1 => call.arguments[1].ty = Type::Named("Bool".into(), Vec::new()),
            2 => call.callee_mode = witchy_canonical_source::source::SourceExecutionMode::Opt,
            3 => call.arguments[0].convention = Convention::Own,
            4 => call.canonical_callee_item += 1,
            5 => call.callee_entry.0 += 1,
            6 => call.captures.clear(),
            7 => call.arguments[1].callee_binding.0 += 1,
            _ => call.caller_mode = witchy_canonical_source::source::SourceExecutionMode::Opt,
        }
        assert!(forwarder.validate_call(checked, &input, &input.static_calls[position]).is_err());
    }
}
