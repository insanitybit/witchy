use std::collections::BTreeMap;
use witchy_syntax::ast::{Block, Convention, Expr, Function, Item, Param, Stmt, Type};
use crate::checked_scalar_region::ScalarRegionSource;
use crate::scalar_region_recipe::ActorRecipe;

pub(crate) const COMPANION: &str = "__witchy_closed_scalar_root";

fn named(name: &str) -> Type { Type::Named(name.into(), vec![]) }
fn let_value(name: String, ty: Type, value: Expr, mutable: bool) -> Stmt {
    Stmt::Let { name, ty: Some(ty), value, mutable }
}
fn renamed_expression(mut expression: Expr, names: &BTreeMap<String, String>) -> Expr {
    witchy_syntax::ast::visit::visit_expr_mut(&mut expression, &mut |expression| {
        if let Expr::Var(name) = expression && let Some(replacement) = names.get(name) {
            *name = replacement.clone();
        }
        Ok::<_, ()>(())
    }).expect("infallible scalar expression renaming");
    expression
}
fn renamed_block(mut block: Block, names: &BTreeMap<String, String>, initialize: bool) -> Option<Block> {
    for statement in &mut block.stmts {
        match statement {
            Stmt::Assign { name, value, .. } => {
                *name = names.get(name)?.clone();
                *value = renamed_expression(value.clone(), names);
            }
            Stmt::Let { name, value, .. } if initialize => {
                *statement = Stmt::Assign { name: names.get(name)?.clone(),
                    value: renamed_expression(value.clone(), names), place_depth: 0 };
            }
            Stmt::LetPattern { pattern: witchy_syntax::ast::Pattern::Var(name), value } if initialize => {
                *statement = Stmt::Assign { name: names.get(name)?.clone(),
                    value: renamed_expression(value.clone(), names), place_depth: 0 };
            }
            Stmt::Expr(expression) => *expression = renamed_expression(expression.clone(), names),
            _ => return None,
        }
    }
    Some(block)
}
fn fragment(source: &Block, start: usize, end: usize) -> Block {
    Block { stmts: source.stmts[start..end].to_vec(), lines: source.lines[start..end].to_vec(), region: None }
}
fn append(target: &mut Block, source: Block) {
    target.stmts.extend(source.stmts);
    target.lines.extend(source.lines);
}
fn push(target: &mut Block, statement: Stmt, line: u32) {
    target.stmts.push(statement);
    target.lines.push(line);
}

pub(crate) fn companion(source: &ScalarRegionSource) -> Option<Function> {
    if source.flows.iter().flat_map(|flow| &flow.bindings)
        .any(|binding| binding.name.starts_with("scalar_region_") || binding.name == "original_scalar_recipe") { return None; }
    let mut actors = Vec::new();
    let mut body = fragment(&source.root.body, 0, source.channel_statement);
    let capacity_line = source.root.body.lines[source.channel_statement];
    push(&mut body, let_value("scalar_region_capacity_source".into(), named("Int"), source.capacity.clone(), false), capacity_line);
    let mut capacity = witchy_syntax::parser::parse_module("fn capacity():\n    let scalar_region_capacity = if scalar_region_capacity_source < 0: 0 else: scalar_region_capacity_source\n").ok()?;
    let Item::Function(capacity) = capacity.items.remove(0) else { return None };
    append(&mut body, capacity.body);
    append(&mut body, fragment(&source.root.body, source.channel_statement + 1, source.scope_statement));
    for (ordinal, actor) in source.actors.iter().enumerate() {
        let recipe = ActorRecipe::derive(actor, ordinal == source.producer)?;
        let mut names = BTreeMap::new();
        for (parameter, argument) in actor.params.iter().zip(&source.arguments[ordinal]) {
            if parameter.name == recipe.endpoint { continue; }
            let name = format!("scalar_region_actor_{ordinal}_{}", parameter.name);
            push(&mut body, let_value(name.clone(), parameter.ty.clone()?, argument.clone(), false), source.root.body.lines[source.scope_statement]);
            names.insert(parameter.name.clone(), name);
        }
        for (field, ty) in &recipe.fields {
            let name = format!("scalar_region_actor_{ordinal}_{field}");
            let zero = if ty == &named("Bool") { Expr::Bool(false) } else { Expr::Int(0) };
            push(&mut body, let_value(name.clone(), ty.clone(), zero, true), actor.line);
            names.insert(field.clone(), name);
        }
        actors.push((recipe, names));
    }
    let (producer, producer_names) = &actors[source.producer];
    let (receiver, receiver_names) = &actors[1 - source.producer];
    let mut append_recipe = witchy_syntax::parser::parse_module(APPEND).ok()?;
    let Item::Function(append_recipe) = append_recipe.items.remove(0) else { return None };
    let mut replacements: BTreeMap<String, Expr> = BTreeMap::from([
        ("scalar_region_append".into(), Expr::Block(append_recipe.body)),
        ("scalar_region_producer_init".into(), Expr::Block(renamed_block(producer.initial.clone(), producer_names, true)?)),
        ("scalar_region_producer_prefix".into(), Expr::Block(renamed_block(producer.loop_prefix.clone(), producer_names, false)?)),
        ("scalar_region_producer_suffix".into(), Expr::Block(renamed_block(producer.loop_suffix.clone(), producer_names, false)?)),
        ("scalar_region_producer_completion".into(), Expr::Block(renamed_block(producer.completion.clone(), producer_names, false)?)),
        ("scalar_region_receiver_init".into(), Expr::Block(renamed_block(receiver.initial.clone(), receiver_names, true)?)),
        ("scalar_region_receiver_body".into(), Expr::Block(renamed_block(receiver.loop_suffix.clone(), receiver_names, false)?)),
        ("scalar_region_receiver_completion".into(), Expr::Block(renamed_block(receiver.completion.clone(), receiver_names, false)?)),
        ("scalar_region_condition".into(), Expr::Block(Block {
            stmts: vec![Stmt::Expr(renamed_expression(producer.condition.clone()?, producer_names))],
            lines: vec![producer.condition_line], region: None })),
        ("scalar_region_payload".into(), Expr::Block(Block {
            stmts: vec![Stmt::Expr(renamed_expression(producer.payload.clone()?, producer_names))],
            lines: vec![producer.payload_line], region: None })),
        ("scalar_region_epilogue".into(), Expr::Block(fragment(&source.root.body, source.scope_statement + 1, source.root.body.stmts.len()))),
    ]);
    let received = receiver_names.get(receiver.received.as_ref()?)?.clone();
    let scheduler_source = SCHEDULER.replace("scalar_region_received", &received)
        .replace("scalar_region_producer_ordinal", &source.producer.to_string());
    let mut scheduler = witchy_syntax::parser::parse_module(&scheduler_source).ok()?;
    let Item::Function(mut scheduler) = scheduler.items.remove(0) else { return None };
    witchy_syntax::ast::visit::visit_block_mut(&mut scheduler.body, &mut |expression| {
        if let Expr::Call { name, args } = expression
            && args.is_empty() && let Some(replacement) = replacements.get(name) {
            *expression = replacement.clone();
        }
        Ok::<_, ()>(())
    }).ok()?;
    replacements.clear();
    append(&mut body, scheduler.body);
    let mut parameters = vec![Param { name: "original_scalar_recipe".into(),
        ty: Some(Type::Named("task.Task".into(), vec![Type::Tuple(vec![])])),
        convention: Convention::Let, default: None }];
    parameters.extend(source.root.params.clone());
    Some(Function { line: source.root.line, public: false, pure: false, comptime_only: false,
        attributes: vec![], name: COMPANION.into(), type_params: vec![], params: parameters,
        ret: Some(Type::Tuple(vec![])), body, bounds: vec![], is_gen: false, is_async: false })
}

const APPEND: &str = r#"fn append_message():
    let scalar_region_physical = list.length(scalar_region_queue)
    if scalar_region_count == scalar_region_physical:
        var scalar_region_grown = list.repeat(0, scalar_region_physical * 2)
        var scalar_region_copy = 0
        while scalar_region_copy < scalar_region_count:
            list.set_at(scalar_region_grown, scalar_region_copy, list.at(scalar_region_queue, (scalar_region_head + scalar_region_copy) % scalar_region_physical))
            scalar_region_copy = scalar_region_copy + 1
        scalar_region_queue = move scalar_region_grown
        scalar_region_head = 0
    list.set_at(scalar_region_queue, (scalar_region_head + scalar_region_count) % list.length(scalar_region_queue), scalar_region_pending)
    scalar_region_count = scalar_region_count + 1
"#;

const SCHEDULER: &str = r#"fn scheduler():
    var scalar_region_queue = [0]
    var scalar_region_head = 0
    var scalar_region_count = 0
    var scalar_region_pending = 0
    var scalar_region_parent = 0
    var scalar_region_producer = 0
    var scalar_region_receiver = 0
    var scalar_region_active_0 = false
    var scalar_region_active_1 = false
    var scalar_region_running = true
    while scalar_region_running:
        let scalar_region_step_0 = scalar_region_active_0
        let scalar_region_step_1 = scalar_region_active_1
        var scalar_region_progress = false
        if scalar_region_parent == 0:
            scalar_region_active_0 = true
            scalar_region_parent = 1
            scalar_region_progress = true
        else if scalar_region_parent == 1:
            scalar_region_parent = 2
            scalar_region_progress = true
        else if scalar_region_parent == 2:
            scalar_region_active_1 = true
            scalar_region_parent = 3
            scalar_region_progress = true
        else if scalar_region_parent == 3:
            scalar_region_parent = 4
            scalar_region_progress = true
        else if scalar_region_parent == 4:
            let scalar_region_first_done = if scalar_region_producer_ordinal == 0: scalar_region_producer == 5 else: scalar_region_receiver == 6
            if scalar_region_first_done:
                scalar_region_parent = 5
                scalar_region_progress = true
        else if scalar_region_parent == 5:
            scalar_region_parent = 6
            scalar_region_progress = true
        else if scalar_region_parent == 6:
            let scalar_region_second_done = if scalar_region_producer_ordinal == 1: scalar_region_producer == 5 else: scalar_region_receiver == 6
            if scalar_region_second_done:
                scalar_region_parent = 7
                scalar_region_progress = true
        else if scalar_region_parent == 7:
            scalar_region_parent = 8
            scalar_region_progress = true
        else if scalar_region_parent == 8:
            scalar_region_epilogue()
            scalar_region_parent = 9
            scalar_region_progress = true
        else if scalar_region_parent == 9:
            scalar_region_parent = 10
            scalar_region_progress = true
        var scalar_region_ordinal = 0
        while scalar_region_ordinal < 2:
            let scalar_region_active = if scalar_region_ordinal == 0: scalar_region_step_0 else: scalar_region_step_1
            if scalar_region_active && scalar_region_ordinal == scalar_region_producer_ordinal:
                var scalar_region_probe_send = false
                if scalar_region_producer == 0:
                    scalar_region_producer_init()
                    scalar_region_producer = 6
                    scalar_region_progress = true
                else if scalar_region_producer == 6:
                    if scalar_region_condition():
                        scalar_region_producer_prefix()
                        scalar_region_pending = scalar_region_payload()
                        scalar_region_producer = 2
                        scalar_region_probe_send = true
                    else:
                        scalar_region_producer_completion()
                        scalar_region_producer = 4
                        scalar_region_progress = true
                else if scalar_region_producer == 1:
                    scalar_region_producer_suffix()
                    if scalar_region_condition():
                        scalar_region_producer_prefix()
                        scalar_region_pending = scalar_region_payload()
                        scalar_region_producer = 2
                    else:
                        scalar_region_producer = 3
                    scalar_region_progress = true
                else if scalar_region_producer == 2:
                    scalar_region_probe_send = true
                else if scalar_region_producer == 3:
                    scalar_region_producer_completion()
                    scalar_region_producer = 4
                    scalar_region_progress = true
                else if scalar_region_producer == 4:
                    scalar_region_producer = 5
                    scalar_region_progress = true
                if scalar_region_probe_send && (scalar_region_capacity == 0 || scalar_region_count < scalar_region_capacity):
                    scalar_region_append()
                    scalar_region_producer = 1
                    scalar_region_progress = true
            else if scalar_region_active:
                var scalar_region_probe_receive = false
                if scalar_region_receiver == 0:
                    scalar_region_receiver_init()
                    scalar_region_receiver = 7
                    scalar_region_progress = true
                else if scalar_region_receiver == 7:
                    scalar_region_receiver = 2
                    scalar_region_probe_receive = true
                else if scalar_region_receiver == 1:
                    scalar_region_receiver_body()
                    scalar_region_receiver = 2
                    scalar_region_progress = true
                else if scalar_region_receiver == 2:
                    scalar_region_probe_receive = true
                else if scalar_region_receiver == 3:
                    scalar_region_receiver = 4
                    scalar_region_progress = true
                else if scalar_region_receiver == 4:
                    scalar_region_receiver_completion()
                    scalar_region_receiver = 5
                    scalar_region_progress = true
                else if scalar_region_receiver == 5:
                    scalar_region_receiver = 6
                    scalar_region_progress = true
                if scalar_region_probe_receive && scalar_region_count > 0:
                    scalar_region_received = list.at(scalar_region_queue, scalar_region_head)
                    scalar_region_head = (scalar_region_head + 1) % list.length(scalar_region_queue)
                    scalar_region_count = scalar_region_count - 1
                    scalar_region_receiver = 1
                    scalar_region_progress = true
            scalar_region_ordinal = scalar_region_ordinal + 1
        scalar_region_running = scalar_region_parent < 10 || (scalar_region_active_0 && (scalar_region_producer != 5 || scalar_region_receiver != 6)) || (scalar_region_active_1 && (scalar_region_producer != 5 || scalar_region_receiver != 6))
        if scalar_region_running && !scalar_region_progress:
            if scalar_region_parent == 4:
                scalar_region_parent = 5
            else if scalar_region_parent == 6:
                scalar_region_parent = 7
            if scalar_region_receiver == 2:
                scalar_region_receiver = 3
            if scalar_region_producer == 2:
                scalar_region_append()
                scalar_region_producer = 1
"#;
