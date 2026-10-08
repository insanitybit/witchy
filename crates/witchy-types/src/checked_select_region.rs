use std::collections::BTreeSet;
use witchy_syntax::ast::{Convention, Expr, Function, Item, Pattern, Stmt, Type, UnOp};
use crate::async_control_flow::{SourceAsyncControlFlow, SourceAsyncOperationKind, source_expression};
use crate::pipeline::CheckedModule;
use crate::runtime_type::{DeclarationKind, PackageSource, RuntimeDeclarationCatalog};
use crate::select_region_recipe::{producer, scalar, scalar_type};

#[derive(Debug, PartialEq)]
pub(crate) struct SelectRegionSource {
    pub root: Function,
    pub channels: [usize; 2],
    pub capacities: [Expr; 2],
    pub senders: [String; 2],
    pub receivers: [String; 2],
    pub actor_channels: [usize; 2],
    pub priority: [usize; 2],
    pub scope: usize,
    pub select_loop: usize,
    pub select_bind: String,
    pub select_tuple: Pattern,
    pub select_arms: [(Option<String>, Expr); 3],
    pub actors: [Function; 2],
    pub arguments: [Vec<Expr>; 2],
    pub auxiliary: Vec<Function>,
    pub flows: Vec<SourceAsyncControlFlow>,
}

fn standard(catalog: &RuntimeDeclarationCatalog, name: &str, declaration: &str, kind: DeclarationKind) -> bool {
    catalog.resolve(name, kind).is_some_and(|identity|
        identity.package().source() == &PackageSource::Toolchain && identity.package().name() == "witchy/std"
        && identity.module() == ["std", "chan"] && identity.name() == declaration)
}
fn awaited(expression: &Expr) -> Option<(&str, &[Expr])> {
    let Expr::Unary { op: UnOp::Await, expr } = expression else { return None };
    let Expr::Call { name, args } = expr.as_ref() else { return None };
    Some((name, args))
}
fn unit(function: &Function) -> bool {
    function.ret.as_ref().is_none_or(|ty| matches!(ty, Type::Tuple(values) if values.is_empty()))
}
fn ordinary(function: &Function) -> bool {
    !function.is_gen && function.type_params.is_empty() && function.bounds.is_empty() && function.body.region.is_none()
        && function.body.stmts.len() == function.body.lines.len()
        && function.params.iter().all(|parameter| parameter.convention == Convention::Let && parameter.default.is_none())
}
fn scalar_pattern(pattern: &Pattern, names: &mut BTreeSet<String>) -> bool {
    match pattern {
        Pattern::Var(name) => names.insert(name.clone()),
        Pattern::Tuple(values) => values.iter().all(|value| scalar_pattern(value, names)),
        _ => false,
    }
}
fn scalar_result(ty: &Type) -> bool {
    scalar_type(ty) || matches!(ty, Type::Tuple(values) if values.iter().all(scalar_result))
}
fn local_names(function: &Function) -> BTreeSet<String> {
    function.params.iter().filter(|parameter| parameter.ty.as_ref().is_some_and(scalar_type))
        .map(|parameter| parameter.name.clone()).collect()
}

pub(crate) fn expression(expression: &Expr, names: &BTreeSet<String>, auxiliary: &[Function]) -> bool {
    if scalar(expression, names) { return true; }
    match expression {
        Expr::Tuple(values) => values.iter().all(|value| self::expression(value, names, auxiliary)),
        Expr::Block(body) => scalar_body(body, &mut names.clone(), auxiliary, false),
        Expr::If { cond, then_block, else_block } => scalar(cond, names)
            && scalar_body(then_block, &mut names.clone(), auxiliary, false)
            && else_block.as_ref().is_some_and(|body| scalar_body(body, &mut names.clone(), auxiliary, false)),
        Expr::Call { name, args } => auxiliary.iter().any(|function| function.name == *name && function.params.len() == args.len()
            && args.iter().all(|argument| scalar(argument, names))),
        _ => false,
    }
}
fn scalar_body(body: &witchy_syntax::ast::Block, names: &mut BTreeSet<String>, auxiliary: &[Function], loops: bool) -> bool {
    if body.region.is_some() || body.stmts.len() != body.lines.len() { return false; }
    body.stmts.iter().all(|statement| match statement {
        Stmt::Let { name, value, .. } | Stmt::LetPattern { pattern: Pattern::Var(name), value } => {
            expression(value, names, auxiliary) && names.insert(name.clone())
        }
        Stmt::LetPattern { pattern, value } => expression(value, names, auxiliary) && scalar_pattern(pattern, names),
        Stmt::Assign { name, value, place_depth: 0 } => names.contains(name) && expression(value, names, auxiliary),
        Stmt::Expr(Expr::While { cond, body }) if loops => scalar(cond, names) && scalar_body(body, &mut names.clone(), auxiliary, true),
        Stmt::Expr(value) => expression(value, names, auxiliary),
        _ => false,
    })
}
fn printed(expression: &Expr, names: &BTreeSet<String>, auxiliary: &[Function]) -> bool {
    match expression {
        Expr::Str(_) => true,
        Expr::Binary { lhs, rhs, .. } => printed(lhs, names, auxiliary) && printed(rhs, names, auxiliary),
        Expr::Call { name, args } if witchy_syntax::intrinsics::lookup(name)
            .is_some_and(|specification| specification.id == witchy_syntax::intrinsics::IntrinsicId::GeneratedRender) => {
                matches!(args.as_slice(), [value] if self::expression(value, names, auxiliary))
            }
        _ => false,
    }
}
fn root_statement(statement: &Stmt, names: &mut BTreeSet<String>, consoles: &BTreeSet<String>, clocks: &BTreeSet<String>, auxiliary: &[Function]) -> bool {
    match statement {
        Stmt::Let { name, value, .. } | Stmt::LetPattern { pattern: Pattern::Var(name), value } => {
            let clock = matches!(value, Expr::Call { name, args } if name == "__capop.now_monotonic"
                && matches!(args.as_slice(), [Expr::Var(clock)] if clocks.contains(clock)));
            (expression(value, names, auxiliary) || clock) && names.insert(name.clone())
        }
        Stmt::Expr(Expr::Call { name, args }) if name == "__capop.print" => matches!(args.as_slice(),
            [Expr::Var(console), message] if consoles.contains(console) && printed(message, names, auxiliary)),
        Stmt::Expr(Expr::If { cond, then_block, else_block }) => scalar(cond, names)
            && then_block.stmts.iter().all(|statement| root_statement(statement, &mut names.clone(), consoles, clocks, auxiliary))
            && else_block.as_ref().is_none_or(|body| body.stmts.iter().all(|statement|
                root_statement(statement, &mut names.clone(), consoles, clocks, auxiliary))),
        Stmt::Expr(Expr::Tuple(values)) => values.is_empty(),
        _ => false,
    }
}

impl SelectRegionSource {
    pub(crate) fn derive(checked: &CheckedModule, catalog: &RuntimeDeclarationCatalog) -> Option<Self> {
        let typed = checked.typed_async_source()?;
        typed.module().linked_entry.as_deref()?;
        let root = typed.module().items.iter().find_map(|item| match item {
            Item::Function(function) if function.name == "main" && function.is_async => Some(function), _ => None,
        })?;
        if !ordinary(root) || !unit(root) || root.params.iter().any(|parameter| !parameter.ty.as_ref().is_some_and(|ty|
            scalar_type(ty) || matches!(ty, Type::Named(name, args) if args.is_empty() && matches!(name.as_str(), "Console" | "Clock")))) {
            return None;
        }
        let channels = root.body.stmts.iter().enumerate().filter_map(|(index, statement)| {
            let Stmt::LetPattern { pattern: Pattern::Tuple(values), value } = statement else { return None };
            let [Pattern::Var(sender), Pattern::Var(receiver)] = values.as_slice() else { return None };
            let (name, args) = awaited(value)?;
            let [capacity] = args else { return None };
            standard(catalog, name, "channel", DeclarationKind::Function).then_some((index, sender.clone(), receiver.clone(), capacity.clone()))
        }).collect::<Vec<_>>();
        let [first, second] = channels.as_slice() else { return None };
        let scopes = root.body.stmts.iter().enumerate().filter_map(|(index, statement)| {
            let Stmt::Expr(value) = statement else { return None };
            let (name, args) = awaited(value)?;
            let [Expr::List(children)] = args else { return None };
            standard(catalog, name, "scope", DeclarationKind::Function).then_some((index, children))
        }).collect::<Vec<_>>();
        let [(scope, children)] = scopes.as_slice() else { return None };
        let [_, _] = children.as_slice() else { return None };
        let loops = root.body.stmts.iter().enumerate().filter_map(|(index, statement)| match statement {
            Stmt::Expr(Expr::While { cond, body }) => Some((index, cond.as_ref(), body)), _ => None,
        }).collect::<Vec<_>>();
        let [(select_loop, condition, body)] = loops.as_slice() else { return None };
        if second.0 >= *scope || scope >= select_loop || body.stmts.len() < 2 || body.lines.len() != body.stmts.len() || body.region.is_some() { return None; }
        let mut actor_channels = Vec::new();
        let mut actors = Vec::new();
        let mut arguments = Vec::new();
        for child in children.iter() {
            let Expr::Call { name, args } = child else { return None };
            let actor = typed.module().items.iter().find_map(|item| match item {
                Item::Function(function) if function.name == *name && function.is_async => Some(function), _ => None,
            })?;
            if !ordinary(actor) || !unit(actor) || args.len() != actor.params.len() { return None; }
            let recipe = producer(actor, typed)?;
            let endpoint = actor.params.iter().position(|parameter| parameter.name == recipe.endpoint)?;
            let Type::Named(endpoint_name, payloads) = actor.params[endpoint].ty.as_ref()? else { return None };
            if !standard(catalog, endpoint_name, "Sender", DeclarationKind::Type)
                || payloads != &[Type::Named("Int".into(), vec![])] {
                return None;
            }
            let Expr::Var(sender) = &args[endpoint] else { return None };
            let channel = if sender == &first.1 { 0 } else if sender == &second.1 { 1 } else { return None };
            if actor_channels.contains(&channel) { return None; }
            actor_channels.push(channel);
            actors.push(actor.clone()); arguments.push(args.clone());
        }
        let auxiliary = typed.module().items.iter().filter_map(|item| match item {
            Item::Function(function) if !function.is_async && !function.is_gen && function.type_params.is_empty()
                && function.bounds.is_empty() && function.body.region.is_none()
                && function.body.stmts.len() == function.body.lines.len()
                && function.params.iter().all(|parameter| parameter.default.is_none()
                    && matches!(parameter.convention, Convention::Let | Convention::Borrow)
                    && parameter.ty.as_ref().is_some_and(scalar_type))
                && function.ret.as_ref().is_some_and(scalar_result)
                && scalar_body(&function.body, &mut local_names(function), &[], true) => Some(function.clone()),
            _ => None,
        }).collect::<Vec<_>>();
        let consoles = root.params.iter().filter(|parameter| matches!(parameter.ty.as_ref(), Some(Type::Named(name, args)) if name == "Console" && args.is_empty()))
            .map(|parameter| parameter.name.clone()).collect::<BTreeSet<_>>();
        let clocks = root.params.iter().filter(|parameter| matches!(parameter.ty.as_ref(), Some(Type::Named(name, args)) if name == "Clock" && args.is_empty()))
            .map(|parameter| parameter.name.clone()).collect::<BTreeSet<_>>();
        let mut names = local_names(root);
        for (index, statement) in root.body.stmts.iter().enumerate() {
            if index == first.0 || index == second.0 {
                if !scalar(&channels[usize::from(index == second.0)].3, &names) { return None; }
                continue;
            }
            if index == *scope {
                for (actor, args) in actors.iter().zip(&arguments) {
                    let recipe = producer(actor, typed)?;
                    for (parameter, argument) in actor.params.iter().zip(args) {
                        if parameter.name == recipe.endpoint { continue; }
                        if parameter.ty.as_ref().is_some_and(scalar_type) {
                            if !scalar(argument, &names) { return None; }
                        } else if !matches!(parameter.ty.as_ref(), Some(Type::Named(name, args)) if name == "Console" && args.is_empty())
                            || !matches!(argument, Expr::Var(name) if consoles.contains(name)) { return None; }
                    }
                }
                continue;
            }
            if index == *select_loop { break; }
            if !root_statement(statement, &mut names, &consoles, &clocks, &auxiliary) { return None; }
        }
        if !scalar(condition, &names) { return None; }
        let (selected, awaited_select) = match &body.stmts[0] {
            Stmt::Let { name, value, mutable: false, .. } | Stmt::LetPattern { pattern: Pattern::Var(name), value } => (name, value),
            _ => return None,
        };
        let (select_name, select_args) = awaited(awaited_select)?;
        if !standard(catalog, select_name, "select", DeclarationKind::Function) || !names.insert(selected.clone()) { return None; }
        let priority = match select_args {
            [Expr::Var(a), Expr::Var(b)] if a == &first.2 && b == &second.2 => [0, 1],
            [Expr::Var(a), Expr::Var(b)] if a == &second.2 && b == &first.2 => [1, 0],
            _ => return None,
        };
        let Stmt::LetPattern { pattern, value: Expr::Match { scrutinee, arms } } = &body.stmts[1] else { return None };
        if !matches!(scrutinee.as_ref(), Expr::Var(name) if name == selected) || arms.len() != 3 { return None; }
        names.remove(selected);
        let mut selected_arms: [Option<(Option<String>, Expr)>; 3] = [None, None, None];
        for arm in arms {
            let Pattern::Ctor { name, args } = &arm.pattern else { return None };
            let ordinal = match name.as_str() { "chan.First" => 0, "chan.Second" => 1, "chan.Closed" => 2, _ => return None };
            let mut branch_names = names.clone();
            let binding = if ordinal == 2 {
                if !args.is_empty() { return None; } None
            } else {
                let [Pattern::Var(name)] = args.as_slice() else { return None };
                if !branch_names.insert(name.clone()) { return None; } Some(name.clone())
            };
            if arm.guard.is_some() || selected_arms[ordinal].is_some() || !expression(&arm.body, &branch_names, &[]) { return None; }
            selected_arms[ordinal] = Some((binding, arm.body.clone()));
        }
        if !scalar_pattern(pattern, &mut names) { return None; }
        let suffix = crate::select_region_recipe::source_block(body, 2, body.stmts.len());
        if !scalar_body(&suffix, &mut names, &[], false) { return None; }
        for statement in &root.body.stmts[*select_loop + 1..] {
            if !root_statement(statement, &mut names, &consoles, &clocks, &auxiliary) { return None; }
        }
        let inventory = checked.source_async_control_flow().ok()?;
        let flows = [root, &actors[0], &actors[1]].into_iter().map(|function|
            inventory.callables.iter().find(|flow| crate::typed_async_lowering::typed_source_function(typed, &flow.callable)
                .is_some_and(|source| source.name == function.name)).cloned()).collect::<Option<Vec<_>>>()?;
        for (ordinal, flow) in flows.iter().enumerate() {
            if flow.mode.is_opt() || flow.operations.iter().any(|operation| matches!(operation.kind, SourceAsyncOperationKind::DynamicCall | SourceAsyncOperationKind::Return))
                || flow.operations.iter().filter(|operation| operation.kind == SourceAsyncOperationKind::Await).count() != if ordinal == 0 { 4 } else { 1 } {
                return None;
            }
            let function = crate::typed_async_lowering::typed_source_function(typed, &flow.callable)?;
            for operation in &flow.operations {
                let site = operation.expression?;
                typed.table().type_of(source_expression(function, site)?).and_then(crate::typeck::ty_to_ast)?;
                flow.bindings_read_by(site).ok()?;
            }
            for endpoint in if ordinal == 0 { vec![first.1.clone(), second.1.clone(), first.2.clone(), second.2.clone()] }
                else { vec![actor_endpoint(&actors[ordinal - 1], typed)?] } {
                let binding = flow.bindings.iter().filter(|binding| binding.name == endpoint).collect::<Vec<_>>();
                let [binding] = binding.as_slice() else { return None };
                if flow.binding_uses.iter().filter(|usage| usage.binding == binding.id).count() != 1 { return None; }
            }
        }
        let mut referenced = false;
        witchy_syntax::ast::visit::visit_module_exprs(typed.module(), &mut |expression| {
            referenced |= matches!(expression, Expr::Call { name, .. } | Expr::Var(name) if name == "main"); Ok::<_, ()>(())
        }).ok()?;
        if referenced { return None; }
        Some(Self { root: root.clone(), channels: [first.0, second.0], capacities: [first.3.clone(), second.3.clone()],
            senders: [first.1.clone(), second.1.clone()], receivers: [first.2.clone(), second.2.clone()],
            actor_channels: actor_channels.try_into().ok()?, priority, scope: *scope,
            select_loop: *select_loop, select_bind: selected.clone(), select_tuple: pattern.clone(),
            select_arms: selected_arms.map(|arm| arm.expect("exhaustive validated select")), actors: actors.try_into().ok()?,
            arguments: arguments.try_into().ok()?, auxiliary, flows })
    }
}
fn actor_endpoint(function: &Function, typed: &crate::typeck::TypedModule) -> Option<String> {
    producer(function, typed).map(|recipe| recipe.endpoint)
}
