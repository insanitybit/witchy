use witchy_syntax::ast::{Block, Expr, Item, Module, Pattern};

pub(crate) fn exact_body(module: &Module, name: &str, recipe: &str) -> bool {
    let Some(function) = module.items.iter().find_map(|item| match item {
        Item::Function(function) if function.name == name => Some(function), _ => None,
    }) else { return false };
    matches_recipe(&function.body, recipe)
}

fn matches_recipe(body: &Block, recipe: &str) -> bool {
    let Ok(expected) = witchy_syntax::parser::parse_module(recipe) else { return false };
    let [Item::Function(expected)] = expected.items.as_slice() else { return false };
    normalize(body.clone()) == normalize(expected.body.clone())
}

fn normalized_name(name: &str) -> String {
    if let Some(spec) = witchy_syntax::intrinsics::lookup(name) {
        return format!("%intrinsic:{:?}", spec.id);
    }
    match name {
        "Task" | "Done" | "Yield" | "Push" | "Pull2Ready" | "SelectLaneFrame" | "ChannelId" | "unit"
        | "poll" | "ready_unit" | "and_then" | "and_then_step" | "__channel_select2"
        | "__prepare_select" | "__select_lane_frame" => format!("task.{name}"),
        "done" | "Receiver" | "First" | "Second" | "Closed" | "select2_result" | "select2_frame_decode"
        | "__rfc0152_selected_materialized" => format!("chan.{name}"),
        _ => name.into(),
    }
}

fn normalize_pattern(pattern: &mut Pattern) {
    match pattern {
        Pattern::Ctor { name, args } => {
            *name = normalized_name(name);
            for argument in args { normalize_pattern(argument); }
        }
        Pattern::AnonCtor { args, .. } | Pattern::Tuple(args) | Pattern::Or(args) => {
            for argument in args { normalize_pattern(argument); }
        }
        Pattern::List { elems, .. } => {
            for element in elems { normalize_pattern(element); }
        }
        _ => {}
    }
}

fn normalize(mut block: Block) -> Block {
    block.lines.clear();
    witchy_syntax::ast::visit::visit_block_mut(&mut block, &mut |expression| {
        match expression {
            Expr::Call { name, .. } | Expr::Ctor { name, .. } => *name = normalized_name(name),
            Expr::Var(name) if matches!(name.as_str(), "Closed" | "chan.Closed" | "None") => {
                *expression = Expr::Call { name: normalized_name(name), args: Vec::new() };
            }
            Expr::Block(block) | Expr::Lambda { body: block, .. } => block.lines.clear(),
            Expr::If { then_block, else_block, .. } => {
                then_block.lines.clear();
                if let Some(block) = else_block { block.lines.clear(); }
            }
            Expr::Match { arms, .. } => {
                for arm in arms { normalize_pattern(&mut arm.pattern); }
            }
            _ => {}
        }
        if let Expr::Ctor { name, args } = expression {
            *expression = Expr::Call { name: name.clone(), args: args.clone() };
        }
        Ok::<_, ()>(())
    }).expect("infallible standard body normalization");
    block
}

pub(crate) fn select_sequence_body(module: &Module) -> bool {
    if !exact_body(module, "task.and_then", "fn sequence(t, k):\n    Task(fn(): and_then_step(poll(t), k))\n") {
        return false;
    }
    let Some(function) = module.items.iter().find_map(|item| match item {
        Item::Function(function) if function.name == "task.and_then_step" => Some(function), _ => None,
    }) else { return false };
    let mut function = function.clone();
    let [witchy_syntax::ast::Stmt::Expr(Expr::Match { arms, .. })] = function.body.stmts.as_mut_slice()
        else { return false };
    if arms.iter().any(|arm| !matches!(arm.pattern, Pattern::Ctor { .. })) { return false; }
    arms.retain(|arm| matches!(&arm.pattern, Pattern::Ctor { name, .. }
        if matches!(normalized_name(name).as_str(), "task.Done" | "task.Yield" | "task.Pull2Ready")));
    matches_recipe(&function.body, r#"fn sequence(s, k):
    match s:
        Done(v) -> Yield(k(v))
        Yield(t2) -> Yield(and_then(t2, k))
        Pull2Ready(ch0, ch1, cont) -> Pull2Ready(ch0, ch1, fn(tag, o): and_then(cont(tag, o), k))
"#)
}
