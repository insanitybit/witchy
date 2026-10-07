use std::collections::BTreeMap;
use witchy_syntax::ast::{Expr, Type, UnOp};
use crate::async_control_flow::{SourceAsyncOperationKind, source_expression};
use crate::pipeline::CheckedModule;
use crate::runtime_type::{DeclarationKind, PackageSource};
use crate::typed_async_lowering::{TypedAsyncLoweringInput, typed_source_function};
use crate::typeck;

#[derive(Debug, PartialEq)]
pub struct CheckedSelectPreparation {
    endpoint_types: [Type; 2],
}

impl CheckedSelectPreparation {
    pub fn endpoint_types(&self) -> &[Type; 2] { &self.endpoint_types }
}

pub(crate) fn bind_select_preparations(checked: &CheckedModule)
    -> Result<BTreeMap<usize, CheckedSelectPreparation>, String>
{
    let canonical = checked.canonical_source().ok_or("select preparation requires canonical source")?;
    let final_typed = canonical.typed_module();
    let mut preparations = Vec::new();
    for (item, definition) in final_typed.module().items.iter().enumerate() {
        let witchy_syntax::ast::Item::Function(function) = definition else { continue };
        witchy_syntax::ast::visit::visit_block(&function.body, &mut |expression| {
            if let Expr::Call { name, .. } = expression
                && crate::suspension_carrier::call_family(name, "task.__prepare_select")
            { preparations.push((item, expression)); }
            Ok::<_, String>(())
        })?;
    }
    if preparations.is_empty() { return Ok(BTreeMap::new()); }
    if !plain_select_implementation(checked.module()) {
        return Err("compiler select frame requires the exact checked standard select implementation".into());
    }
    let source_typed = checked.typed_async_source().ok_or("select preparation lost typed source")?;
    let input = TypedAsyncLoweringInput::from_checked(checked)?;
    let catalog = checked.runtime_declaration_catalog().map_err(|error| error.to_string())?;
    let view = canonical.view();
    let mut bindings = BTreeMap::new();
    for (item, expression) in preparations {
        let Expr::Call { args, .. } = expression else { unreachable!() };
        let [Expr::Block(block)] = args.as_slice() else {
            return Err("prepared select lacks marked original task".into());
        };
        let [witchy_syntax::ast::Stmt::Let { name, value: Expr::Int(identity), .. },
            witchy_syntax::ast::Stmt::Expr(Expr::Call { name: callee, args })] = block.stmts.as_slice()
            else { return Err("prepared select lacks exact marked original task".into()); };
        let identity = u32::try_from(*identity).map_err(|_| "invalid prepared select identity")?;
        if name.strip_prefix(witchy_syntax::suspension::SOURCE_TASK_OPERAND_PREFIX)
            .and_then(|encoded| encoded.parse::<u32>().ok()) != Some(identity)
            || args.len() != 2 || !crate::suspension_carrier::call_family(callee, "chan.select")
        { return Err("prepared select differs from its checked operation".into()); }
        let item = u32::try_from(item).map_err(|_| "prepared select item exceeds u32")?;
        let proofs = view.task_operand_proofs_at(item).filter(|proof| proof.source_identity().3 == identity)
            .collect::<Vec<_>>();
        let [proof] = proofs.as_slice() else { return Err("prepared select lacks one exact source proof".into()); };
        let (module, ordinal, method, suspension) = proof.source_identity();
        let flow = input.source.iter().find(|flow| flow.callable.module == module
            && flow.callable.surface.item == ordinal && flow.callable.surface.method == method)
            .ok_or("prepared select lost source control flow")?;
        if flow.mode != proof.source_mode() || (flow.mode.is_opt() && !proof.opt_compatible()) {
            return Err("prepared select has incompatible checked execution mode".into());
        }
        let mut awaits = flow.operations.iter().filter(|operation| operation.kind == SourceAsyncOperationKind::Await)
            .collect::<Vec<_>>();
        awaits.sort_by_key(|operation| operation.expression);
        let operation = awaits.get(suspension as usize).ok_or("prepared select lost source await")?;
        let coordinate = operation.expression.ok_or("prepared select lacks source coordinate")?;
        let function = typed_source_function(source_typed, &flow.callable).ok_or("prepared select lost source callable")?;
        let Some(Expr::Unary { op: UnOp::Await, expr }) = source_expression(function, coordinate)
            else { return Err("prepared select source is not an await".into()); };
        let Expr::Call { name, args: originals } = expr.as_ref() else {
            return Err("prepared select source is not a direct selection".into());
        };
        let declaration = catalog.resolve(name, DeclarationKind::Function)
            .ok_or("prepared select lacks authenticated declaration")?;
        if declaration.package().source() != &PackageSource::Toolchain
            || declaration.package().name() != "witchy/std" || declaration.module() != ["std", "chan"]
            || declaration.name() != "select" || originals.len() != 2
        { return Err("prepared select lacks exact standard select identity".into()); }
        let binding = input.operations.iter().find(|binding| binding.callable == flow.callable
            && binding.source_operation == operation.ordinal).ok_or("prepared select lacks final operation binding")?;
        if binding.expression != Some(coordinate) || binding.continuation.is_none() {
            return Err("prepared select lacks final suspension continuation".into());
        }
        let mut endpoints = Vec::new();
        for (original, lowered) in originals.iter().zip(args) {
            let source_type = source_typed.table().type_of(original).and_then(typeck::ty_to_ast)
                .ok_or("prepared select source endpoint lacks concrete type")?;
            let final_type = final_typed.table().type_of(lowered).and_then(typeck::ty_to_ast)
                .ok_or("prepared select endpoint lacks concrete type")?;
            let Type::Named(name, payload) = source_type.unqualified() else {
                return Err("prepared select endpoint is not nominal".into());
            };
            let endpoint = catalog.resolve(name, DeclarationKind::Type)
                .ok_or("prepared select endpoint lacks authenticated identity")?;
            if endpoint.package() != declaration.package() || endpoint.module() != declaration.module()
                || endpoint.name() != "Receiver" || payload.len() != 1 || source_type != final_type
                || catalog.type_identity(&source_type).map_err(|error| error.to_string())?
                    != catalog.type_identity(&final_type).map_err(|error| error.to_string())?
            { return Err("prepared select endpoint differs from checked source authority".into()); }
            endpoints.push(final_type);
        }
        if endpoints[0] != endpoints[1] { return Err("prepared select endpoint payloads disagree".into()); }
        let endpoint_types = endpoints.try_into().map_err(|_| "prepared select endpoint count")?;
        bindings.insert(expression as *const _ as usize, CheckedSelectPreparation { endpoint_types });
    }
    Ok(bindings)
}

fn plain_select_implementation(module: &witchy_syntax::ast::Module) -> bool {
    use crate::checked_channel_contract::{exact_body, select_sequence_body};
    if !select_sequence_body(module) { return false; }
    [
        ("chan.select", r#"fn select(a, b):
    match a:
        Receiver(ia) ->
            match b:
                Receiver(ib) -> __channel_select2(ia, ib, fn(k, o): select2_result(k, o))
"#),
        ("task.__channel_select2", r#"fn select2(a, b, decode):
    match a:
        ChannelId(ia) ->
            match b:
                ChannelId(ib) -> Task(fn(): Pull2Ready(ia, ib, decode))
"#),
        ("task.poll", r#"fn poll(t):
    match t:
        Task(th) -> th()
"#),
        ("chan.done", r#"fn done(x):
    task.done(x)
"#),
        ("task.done", r#"fn done(x):
    Task(fn(): Done(x))
"#),
        ("chan.__rfc0152_selected_materialized", r#"fn marker():
    ()
"#),
        ("chan.select2_result", r#"fn result(k, o):
    match o:
        Some(msg) ->
            if k == 0:
                __rfc0152_selected_materialized()
                done(First(__unerase(msg)))
            else:
                __rfc0152_selected_materialized()
                done(Second(__unerase(msg)))
        None ->
            __rfc0152_selected_materialized()
            done(Closed)
"#),
        ("chan.select2_frame_decode", r#"fn decode(tag, o):
    match o:
        Some(msg) ->
            if tag == 0:
                __rfc0152_selected_materialized()
                First(__unerase(msg))
            else:
                __rfc0152_selected_materialized()
                Second(__unerase(msg))
        None ->
            __rfc0152_selected_materialized()
            Closed
"#),
        ("chan.__select2_lane_frame", r#"fn typed_frame(a, b, frame, prepare, resume, finish):
    let decode: fn(own Option((Int, __Msg)), Int, var List(Int)) -> Bool = fn(own o, base: Int, var state):
        match o:
            Some((tag, msg)) -> resume(select2_frame_decode(tag, Some(msg)), base, state)
            None -> resume(select2_frame_decode(0, None), base, state)
    __select_lane_frame(frame, prepare, decode, finish)
"#),
        ("task.__select_lane_frame", r#"fn frame_task(frame, prepare, resume, finish):
    let start: fn() -> task.Step(a) = fn():
        var state = frame
        match prepare(0, state):
            Some(channels) -> SelectLaneFrame((state, prepare, resume, finish, channels))
            None -> poll(finish(state))
    Task(start)
"#),
        ("task.__prepare_select", r#"fn prepare(t):
    match poll(t):
        Pull2Ready(a, b, _) -> [a, b]
        _ ->
            fail("invalid prepared channel select")
            []
"#),
    ].iter().all(|(name, recipe)| exact_body(module, name, recipe))
}
