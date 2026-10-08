use std::collections::BTreeMap;
use witchy_syntax::ast::{Block, Convention, Expr, Function, Item, Param, Pattern, Stmt, Type};
use crate::checked_select_region::SelectRegionSource;
use crate::select_region_recipe::{producer, source_block};
use crate::typeck::TypedModule;

pub(crate) const COMPANION: &str = "__witchy_closed_select_root";
fn named(name: &str) -> Type { Type::Named(name.into(), vec![]) }
fn renamed(mut expression: Expr, names: &BTreeMap<String, String>) -> Expr {
    witchy_syntax::ast::visit::visit_expr_mut(&mut expression, &mut |expression| {
        if let Expr::Var(name) = expression && let Some(replacement) = names.get(name) { *name = replacement.clone(); }
        Ok::<_, ()>(())
    }).expect("infallible select scalar renaming"); expression
}
fn renamed_block(mut block: Block, names: &BTreeMap<String, String>, initialize: bool) -> Option<Block> {
    for statement in &mut block.stmts {
        match statement {
            Stmt::Assign { name, value, .. } => { *name = names.get(name)?.clone(); *value = renamed(value.clone(), names); }
            Stmt::Let { name, value, .. } | Stmt::LetPattern { pattern: Pattern::Var(name), value } => {
                if initialize {
                    *statement = Stmt::Assign { name: names.get(name)?.clone(), value: renamed(value.clone(), names), place_depth: 0 };
                } else { *value = renamed(value.clone(), names); }
            }
            Stmt::Expr(value) => *value = renamed(value.clone(), names), _ => return None,
        }
    }
    Some(block)
}
fn append(target: &mut Block, source: Block) { target.stmts.extend(source.stmts); target.lines.extend(source.lines); }
fn parsed_body(source: &str) -> Option<Block> {
    let mut module = witchy_syntax::parser::parse_module(source).ok()?;
    let Item::Function(function) = module.items.remove(0) else { return None }; Some(function.body)
}
fn expression_block(expression: Expr, line: u32) -> Expr {
    Expr::Block(Block { stmts: vec![Stmt::Expr(expression)], lines: vec![line], region: None })
}

pub(crate) fn companion(source: &SelectRegionSource, typed: &TypedModule) -> Option<Function> {
    if source.flows.iter().flat_map(|flow| &flow.bindings).any(|binding|
        binding.name.starts_with("select_region_") || binding.name == "original_select_recipe") { return None; }
    let mut body = source_block(&source.root.body, 0, source.scope);
    for (ordinal, &channel) in source.channels.iter().enumerate().rev() {
        body.stmts[channel] = Stmt::Let { name: format!("select_region_capacity_source_{ordinal}"),
            ty: Some(named("Int")), mutable: false, value: source.capacities[ordinal].clone() };
    }
    for ordinal in 0..2 {
        append(&mut body, parsed_body(&format!("fn capacity():\n    let select_region_capacity_{ordinal} = if select_region_capacity_source_{ordinal} < 0: 0 else: select_region_capacity_source_{ordinal}\n"))?);
    }
    let mut replacements = BTreeMap::new();
    let mut root_prefix = source_block(&source.root.body, source.scope + 1, source.select_loop);
    for statement in &mut root_prefix.stmts {
        let local = match statement {
            Stmt::Let { name, value, .. } | Stmt::LetPattern { pattern: Pattern::Var(name), value } => Some((name.clone(), value.clone())),
            _ => None,
        };
        if let Some((name, value)) = local {
            let ty = typed.module().items.iter().find_map(|item| match item {
                Item::Function(function) if function.name == source.root.name => function.body.stmts.iter().find_map(|statement| match statement {
                    Stmt::Let { name: original, value, .. } | Stmt::LetPattern { pattern: Pattern::Var(original), value } if original == &name => typed.table().type_of(value).and_then(crate::typeck::ty_to_ast), _ => None,
                }), _ => None,
            })?;
            let zero = if ty == named("Bool") { Expr::Bool(false) } else if ty == named("Int") { Expr::Int(0) } else { return None };
            body.stmts.push(Stmt::Let { name: name.clone(), ty: Some(ty), value: zero, mutable: true });
            body.lines.push(source.root.line);
            *statement = Stmt::Assign { name, value, place_depth: 0 };
        }
    }
    for ordinal in 0..2 {
        let actor = &source.actors[ordinal];
        let recipe = producer(actor, typed)?;
        let mut names = BTreeMap::new();
        for (parameter, argument) in actor.params.iter().zip(&source.arguments[ordinal]) {
            if parameter.name == recipe.endpoint { continue; }
            let name = format!("select_region_actor_{ordinal}_{}", parameter.name);
            body.stmts.push(Stmt::Let { name: name.clone(), ty: parameter.ty.clone(), value: argument.clone(), mutable: false });
            body.lines.push(source.root.body.lines[source.scope]); names.insert(parameter.name.clone(), name);
        }
        for (field, ty) in &recipe.fields {
            let name = format!("select_region_actor_{ordinal}_{field}");
            let value = if ty == &named("Bool") { Expr::Bool(false) } else { Expr::Int(0) };
            body.stmts.push(Stmt::Let { name: name.clone(), ty: Some(ty.clone()), value, mutable: true });
            body.lines.push(actor.line); names.insert(field.clone(), name);
        }
        replacements.insert(format!("select_region_initial_{ordinal}"), Expr::Block(renamed_block(recipe.initial, &names, true)?));
        replacements.insert(format!("select_region_condition_{ordinal}"), expression_block(renamed(recipe.condition?, &names), recipe.condition_line));
        let mut payload = renamed_block(recipe.loop_prefix, &names, false)?;
        payload.stmts.push(Stmt::Expr(renamed(recipe.payload?, &names))); payload.lines.push(recipe.payload_line);
        replacements.insert(format!("select_region_payload_{ordinal}"), Expr::Block(payload));
        replacements.insert(format!("select_region_suffix_{ordinal}"), Expr::Block(renamed_block(recipe.loop_suffix, &names, false)?));
        replacements.insert(format!("select_region_completion_{ordinal}"), Expr::Block(renamed_block(recipe.completion, &names, false)?));
    }
    let Stmt::Expr(Expr::While { cond, body: select_body }) = &source.root.body.stmts[source.select_loop] else { return None };
    replacements.insert("select_region_root_prefix".into(), Expr::Block(root_prefix));
    replacements.insert("select_region_root_condition".into(), expression_block((**cond).clone(), source.root.body.lines[source.select_loop]));
    let mut branches = Vec::new();
    for (binding, expression) in &source.select_arms {
        let mut branch = Block { stmts: vec![], lines: vec![], region: None };
        if let Some(name) = binding {
            branch.stmts.push(Stmt::Let { name: name.clone(), ty: Some(named("Int")), mutable: false, value: Expr::Var("select_region_value".into()) });
            branch.lines.push(select_body.lines[1]);
        }
        branch.stmts.push(Stmt::Expr(expression.clone())); branch.lines.push(select_body.lines[1]);
        branches.push(branch);
    }
    let conditional = Expr::If { cond: Box::new(Expr::Binary { op: witchy_syntax::ast::BinOp::Eq,
        lhs: Box::new(Expr::Var("select_region_tag".into())), rhs: Box::new(Expr::Int(0)) }),
        then_block: branches.remove(0), else_block: Some(Block {
            stmts: vec![Stmt::Expr(Expr::If { cond: Box::new(Expr::Binary { op: witchy_syntax::ast::BinOp::Eq,
                lhs: Box::new(Expr::Var("select_region_tag".into())), rhs: Box::new(Expr::Int(1)) }),
                then_block: branches.remove(0), else_block: Some(branches.remove(0)) })], lines: vec![select_body.lines[1]], region: None }) };
    let mut resume = source_block(select_body, 1, select_body.stmts.len());
    let Stmt::LetPattern { value, .. } = &mut resume.stmts[0] else { return None }; *value = conditional;
    replacements.insert("select_region_root_resume".into(), Expr::Block(resume));
    replacements.insert("select_region_root_epilogue".into(), Expr::Block(source_block(&source.root.body, source.select_loop + 1, source.root.body.stmts.len())));
    let mut scheduler = String::from("fn scheduler():\n");
    for channel in 0..2 { scheduler.push_str(&format!("    var select_region_queue_{channel} = [0]\n    var select_region_head_{channel} = 0\n    var select_region_count_{channel} = 0\n")); }
    scheduler.push_str(START);
    scheduler.push_str(PARENT);
    for ordinal in 0..2 {
        scheduler.push_str(&PRODUCER.replace("@actor@", &ordinal.to_string()).replace("@channel@", &source.actor_channels[ordinal].to_string()));
    }
    scheduler.push_str(END);
    for ordinal in 0..2 { scheduler.push_str(&CLOSE_PRODUCER.replace("@actor@", &ordinal.to_string())); }
    for (name, template) in [("select_region_probe", PROBE), ("select_region_append_0", APPEND), ("select_region_append_1", APPEND)] {
        let expanded = if name == "select_region_probe" { template.replace("@first@", &source.priority[0].to_string()).replace("@second@", &source.priority[1].to_string()) }
            else {
                let ordinal = usize::from(name.ends_with('1'));
                template.replace("@channel@", &source.actor_channels[ordinal].to_string()).replace("@actor@", &ordinal.to_string())
            };
        replacements.insert(name.into(), Expr::Block(parsed_body(&expanded)?));
    }
    let mut scheduler_body = parsed_body(&scheduler)?;
    witchy_syntax::ast::visit::visit_block_mut(&mut scheduler_body, &mut |expression| {
        if let Expr::Call { name, args } = expression && args.is_empty() && let Some(replacement) = replacements.get(name) { *expression = replacement.clone(); }
        Ok::<_, ()>(())
    }).ok()?;
    append(&mut body, scheduler_body);
    let mut params = vec![Param { name: "original_select_recipe".into(), ty: Some(Type::Named("task.Task".into(), vec![Type::Tuple(vec![])])), convention: Convention::Let, default: None }];
    params.extend(source.root.params.clone());
    Some(Function { line: source.root.line, name: COMPANION.into(), public: false, pure: false, comptime_only: false,
        attributes: vec![], type_params: vec![], params, ret: Some(Type::Tuple(vec![])), body, bounds: vec![], is_gen: false, is_async: false })
}

const START: &str = r#"    var select_region_pending_0 = 0
    var select_region_pending_1 = 0
    var select_region_phase_0 = 0
    var select_region_phase_1 = 0
    var select_region_active_0 = false
    var select_region_active_1 = false
    var select_region_parent = 0
    var select_region_tag = 0
    var select_region_value = 0
    var select_region_running = true
    while select_region_running:
        let select_region_step_0 = select_region_active_0
        let select_region_step_1 = select_region_active_1
        var select_region_progress = false
"#;
const PARENT: &str = r#"        if select_region_parent == 0:
            select_region_active_0 = true
            select_region_parent = 1
            select_region_progress = true
        else if select_region_parent == 1:
            select_region_parent = 2
            select_region_progress = true
        else if select_region_parent == 2:
            select_region_active_1 = true
            select_region_parent = 3
            select_region_progress = true
        else if select_region_parent == 3:
            select_region_parent = 4
            select_region_progress = true
        else if select_region_parent == 4:
            if select_region_phase_0 == 5:
                select_region_parent = 5
                select_region_progress = true
        else if select_region_parent == 5:
            select_region_parent = 6
            select_region_progress = true
        else if select_region_parent == 6:
            if select_region_phase_1 == 5:
                select_region_parent = 7
                select_region_progress = true
        else if select_region_parent == 7:
            select_region_parent = 8
            select_region_progress = true
        else if select_region_parent == 8:
            select_region_root_prefix()
            select_region_parent = 9
            select_region_progress = true
        else if select_region_parent == 9:
            if select_region_root_condition():
                select_region_probe()
            else:
                select_region_root_epilogue()
                select_region_parent = 12
                select_region_progress = true
        else if select_region_parent == 10:
            select_region_root_resume()
            if select_region_root_condition():
                select_region_parent = 15
            else:
                select_region_parent = 11
            select_region_progress = true
        else if select_region_parent == 14 || select_region_parent == 15:
            select_region_probe()
        else if select_region_parent == 11:
            select_region_root_epilogue()
            select_region_parent = 12
            select_region_progress = true
        else if select_region_parent == 12:
            select_region_parent = 13
            select_region_progress = true
"#;
const PRODUCER: &str = r#"        if select_region_step_@actor@:
            var select_region_probe_send_@actor@ = false
            if select_region_phase_@actor@ == 0:
                select_region_initial_@actor@()
                select_region_phase_@actor@ = 6
                select_region_progress = true
            else if select_region_phase_@actor@ == 6:
                if select_region_condition_@actor@():
                    select_region_pending_@actor@ = select_region_payload_@actor@()
                    select_region_phase_@actor@ = 2
                    select_region_probe_send_@actor@ = true
                else:
                    select_region_completion_@actor@()
                    select_region_phase_@actor@ = 4
                    select_region_progress = true
            else if select_region_phase_@actor@ == 1:
                select_region_suffix_@actor@()
                if select_region_condition_@actor@():
                    select_region_pending_@actor@ = select_region_payload_@actor@()
                    select_region_phase_@actor@ = 2
                else:
                    select_region_phase_@actor@ = 3
                select_region_progress = true
            else if select_region_phase_@actor@ == 2:
                select_region_probe_send_@actor@ = true
            else if select_region_phase_@actor@ == 3:
                select_region_completion_@actor@()
                select_region_phase_@actor@ = 4
                select_region_progress = true
            else if select_region_phase_@actor@ == 4:
                select_region_phase_@actor@ = 5
                select_region_progress = true
            if select_region_probe_send_@actor@ && (select_region_capacity_@channel@ == 0 || select_region_count_@channel@ < select_region_capacity_@channel@):
                select_region_append_@actor@()
                select_region_phase_@actor@ = 1
                select_region_progress = true
"#;
const END: &str = r#"        select_region_running = select_region_parent != 13 || (select_region_active_0 && select_region_phase_0 != 5) || (select_region_active_1 && select_region_phase_1 != 5)
        if select_region_running && !select_region_progress:
            if select_region_parent == 4:
                select_region_parent = 5
            else if select_region_parent == 6:
                select_region_parent = 7
            else if select_region_parent == 14:
                select_region_tag = 2
                select_region_parent = 10
"#;
const CLOSE_PRODUCER: &str = r#"            if select_region_phase_@actor@ == 2:
                select_region_append_@actor@()
                select_region_phase_@actor@ = 1
"#;
const PROBE: &str = r#"fn probe():
    if select_region_count_@first@ > 0:
        select_region_tag = 0
        select_region_value = list.at(select_region_queue_@first@, select_region_head_@first@)
        select_region_head_@first@ = (select_region_head_@first@ + 1) % list.length(select_region_queue_@first@)
        select_region_count_@first@ = select_region_count_@first@ - 1
        select_region_parent = 10
        select_region_progress = true
    else if select_region_count_@second@ > 0:
        select_region_tag = 1
        select_region_value = list.at(select_region_queue_@second@, select_region_head_@second@)
        select_region_head_@second@ = (select_region_head_@second@ + 1) % list.length(select_region_queue_@second@)
        select_region_count_@second@ = select_region_count_@second@ - 1
        select_region_parent = 10
        select_region_progress = true
    else:
        select_region_parent = 14
"#;
const APPEND: &str = r#"fn append_message():
    let select_region_physical = list.length(select_region_queue_@channel@)
    if select_region_count_@channel@ == select_region_physical:
        var select_region_grown = list.repeat(0, select_region_physical * 2)
        var select_region_copy = 0
        while select_region_copy < select_region_count_@channel@:
            list.set_at(select_region_grown, select_region_copy, list.at(select_region_queue_@channel@, (select_region_head_@channel@ + select_region_copy) % select_region_physical))
            select_region_copy = select_region_copy + 1
        select_region_queue_@channel@ = move select_region_grown
        select_region_head_@channel@ = 0
    list.set_at(select_region_queue_@channel@, (select_region_head_@channel@ + select_region_count_@channel@) % list.length(select_region_queue_@channel@), select_region_pending_@actor@)
    select_region_count_@channel@ = select_region_count_@channel@ + 1
"#;
