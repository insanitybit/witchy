use std::collections::BTreeMap;
use witchy_syntax::ast::{Expr, Type, UnOp};
use crate::async_control_flow::{SourceAsyncOperationKind, source_expression};
use crate::pipeline::CheckedModule;
use crate::runtime_type::{DeclarationKind, PackageSource};
use crate::typed_async_lowering::{TypedAsyncLoweringInput, typed_source_function};
use crate::typeck;

#[derive(Debug, PartialEq)]
pub struct CheckedReceivePreparation { endpoint_type: Type }

impl CheckedReceivePreparation { pub fn endpoint_type(&self) -> &Type { &self.endpoint_type } }

pub(crate) fn bind_receive_preparations(checked: &CheckedModule)
    -> Result<BTreeMap<usize, CheckedReceivePreparation>, String>
{
    let canonical = checked.canonical_source().ok_or("receive preparation requires canonical source")?;
    let final_typed = canonical.typed_module();
    let mut preparations = Vec::new();
    for (item, definition) in final_typed.module().items.iter().enumerate() {
        let witchy_syntax::ast::Item::Function(function) = definition else { continue };
        witchy_syntax::ast::visit::visit_block(&function.body, &mut |expression| {
            if let Expr::Call { name, .. } = expression
                && crate::suspension_carrier::call_family(name, "task.__prepare_recv")
            { preparations.push((item, expression)); }
            Ok::<_, String>(())
        })?;
    }
    if preparations.is_empty() { return Ok(BTreeMap::new()); }
    let catalog = checked.runtime_declaration_catalog().map_err(|error| error.to_string())?;
    let original = checked.typed_async_source().ok_or("receive preparation lost original projection")?;
    plain_receive_implementation(checked.module())?;
    plain_receive_implementation(original.module())?;
    crate::checked_scalar_contract::scheduler_contract(checked.module(), original.module(), &catalog)?;
    let source_typed = checked.typed_async_source().ok_or("receive preparation lost typed source")?;
    let input = TypedAsyncLoweringInput::from_checked(checked)?;
    let catalog = checked.runtime_declaration_catalog().map_err(|error| error.to_string())?;
    let view = canonical.view();

    let mut bindings = BTreeMap::new();
    for (item, expression) in preparations {
        let Expr::Call { args, .. } = expression else { unreachable!() };
        let [Expr::Block(block)] = args.as_slice() else {
            return Err("prepared receive lacks marked original task".into());
        };
        let [witchy_syntax::ast::Stmt::Let { name, value: Expr::Int(identity), .. },
            witchy_syntax::ast::Stmt::Expr(Expr::Call { name: callee, args })] = block.stmts.as_slice()
            else { return Err("prepared receive lacks exact marked original task".into()); };
        let identity = u32::try_from(*identity).map_err(|_| "invalid prepared receive identity")?;
        if name.strip_prefix(witchy_syntax::suspension::SOURCE_TASK_OPERAND_PREFIX)
            .and_then(|encoded| encoded.parse::<u32>().ok()) != Some(identity)
            || args.len() != 1 || !crate::suspension_carrier::call_family(callee, "chan.recv")
        { return Err("prepared receive differs from its checked operation".into()); }
        let item = u32::try_from(item).map_err(|_| "prepared receive item exceeds u32")?;
        let proofs = view.task_operand_proofs_at(item).filter(|proof| proof.source_identity().3 == identity)
            .collect::<Vec<_>>();
        let [proof] = proofs.as_slice() else { return Err("prepared receive lacks one exact source proof".into()); };
        let (module, ordinal, method, suspension) = proof.source_identity();
        let flow = input.source.iter().find(|flow| flow.callable.module == module
            && flow.callable.surface.item == ordinal && flow.callable.surface.method == method)
            .ok_or("prepared receive lost source control flow")?;
        if flow.mode != proof.source_mode() || (flow.mode.is_opt() && !proof.opt_compatible()) {
            return Err("prepared receive has incompatible checked execution mode".into());
        }
        let mut awaits = flow.operations.iter().filter(|operation| operation.kind == SourceAsyncOperationKind::Await)
            .collect::<Vec<_>>();
        awaits.sort_by_key(|operation| operation.expression);
        let operation = awaits.get(suspension as usize).ok_or("prepared receive lost source await")?;
        let coordinate = operation.expression.ok_or("prepared receive lacks source coordinate")?;
        let function = typed_source_function(source_typed, &flow.callable).ok_or("prepared receive lost source callable")?;
        let Some(Expr::Unary { op: UnOp::Await, expr }) = source_expression(function, coordinate)
            else { return Err("prepared receive source is not an await".into()); };
        let Expr::Call { name, args: originals } = expr.as_ref() else {
            return Err("prepared receive source is not a direct receive".into());
        };
        if originals != args { return Err("prepared receive endpoint differs from its source operand".into()); }
        let declaration = catalog.resolve(name, DeclarationKind::Function)
            .ok_or("prepared receive lacks authenticated declaration")?;
        if declaration.package().source() != &PackageSource::Toolchain
            || declaration.package().name() != "witchy/std" || declaration.module() != ["std", "chan"]
            || declaration.name() != "recv" || originals.len() != 1
        { return Err("prepared receive lacks exact standard receive identity".into()); }
        let binding = input.operations.iter().find(|binding| binding.callable == flow.callable
            && binding.source_operation == operation.ordinal).ok_or("prepared receive lacks final operation binding")?;
        if binding.expression != Some(coordinate) || binding.continuation.is_none() {
            return Err("prepared receive lacks final suspension continuation".into());
        }
        for (original, lowered) in originals.iter().zip(args) {
            let source_type = source_typed.table().type_of(original).and_then(typeck::ty_to_ast)
                .ok_or("prepared receive source endpoint lacks concrete type")?;
            let final_type = final_typed.table().type_of(lowered).and_then(typeck::ty_to_ast)
                .ok_or("prepared receive endpoint lacks concrete type")?;
            let Type::Named(name, payload) = source_type.unqualified() else {
                return Err("prepared receive endpoint is not nominal".into());
            };
            let endpoint = catalog.resolve(name, DeclarationKind::Type)
                .ok_or("prepared receive endpoint lacks authenticated identity")?;
            if endpoint.package() != declaration.package() || endpoint.module() != declaration.module()
                || endpoint.name() != "Receiver" || payload.len() != 1 || source_type != final_type
                || catalog.type_identity(&source_type).map_err(|error| error.to_string())?
                    != catalog.type_identity(&final_type).map_err(|error| error.to_string())?
            { return Err("prepared receive endpoint differs from checked source authority".into()); }
            bindings.insert(expression as *const _ as usize, CheckedReceivePreparation { endpoint_type: final_type });
        }

    }
    Ok(bindings)
}

fn plain_receive_implementation(module: &witchy_syntax::ast::Module) -> Result<(), String> {
    use crate::checked_channel_contract::{exact_body, receive_sequence_body};
    let refuse = |name: &str| format!("compiler receive frame requires the exact checked standard receive implementation: {name}");
    if !receive_sequence_body(module) { return Err(refuse("task.and_then_step")); }
    let recipes = [
        ("chan.recv", r#"fn recv(rx):
    match rx:
        Receiver(id) -> task.__channel_recv(id, fn(o): unerase_option(o))
"#),
        ("task.__channel_recv", r#"fn recv(channel, decode):
    match channel:
        ChannelId(id) -> Task(fn(): Pull(id, fn(msg): task.done(decode(msg))))
"#),
        ("chan.unerase_option", r#"fn decode(o):
    match o:
        Some(x) -> Some(__unerase(x))
        None -> None
"#),
        ("task.poll", r#"fn poll(t):
    match t:
        Task(th) -> th()
"#),
        ("task.done", r#"fn done(x):
    Task(fn(): Done(x))
"#),
        ("task.__prepare_recv", r#"fn prepare(t):
    match poll(t):
        Pull(ch, _) -> ch
        _ ->
            fail("invalid prepared channel receive")
            0
"#),
        ("task.__receive_state_frame", r#"fn frame(own state: __Msg, prepare, resume, finish):
    let start: fn() -> task.Step(a) = fn():
        let (channel, prepared) = prepare(state)
        match channel:
            Some(ch) -> ReceiveStateFrame((prepared, prepare, resume, finish, ch))
            None -> poll(finish(prepared))

    Task(start)
"#),
        ("chan.__recv_state_frame", r#"fn frame(own state: s, prepare, resume, finish):
    let preparing: fn(own __Msg) -> (Option(Int), __Msg) = fn(own encoded):
        var current: s = __unerase(encoded)
        let channel = prepare(current)
        (channel, __erase(current))

    let resuming: fn(own Option(__Msg), own __Msg) -> __Msg = fn(own message, own encoded):
        var current: s = __unerase(encoded)
        resume(unerase_option(message), current)
        __erase(current)

    let finishing: fn(own __Msg) -> task.Task(a) = fn(own encoded): finish(__unerase(encoded))
    task.__receive_state_frame(__erase(state), preparing, resuming, finishing)
"#),
    ];
    for (name, recipe) in recipes {
        if !exact_body(module, name, recipe) { return Err(refuse(name)); }
    }
    Ok(())
}
