use std::collections::BTreeSet;
use witchy_syntax::ast::{Convention, Expr, Function, Item, Pattern, Stmt, Type, UnOp};
use crate::pipeline::CheckedModule;
use crate::runtime_type::{DeclarationKind, PackageSource, RuntimeDeclarationCatalog};
use crate::typeck;
use crate::async_control_flow::{SourceAsyncControlFlow, SourceAsyncOperationKind, SourceControlKind, source_expression};
use crate::scalar_region_recipe::{scalar_expression, completion};
use crate::packet_region_recipe::{PacketActor, PacketShape};

#[derive(Debug, PartialEq)]
pub(crate) struct PacketRegionSource {
    pub(crate) root: Function,
    pub(crate) channel_statement: usize,
    pub(crate) scope_statement: usize,
    pub(crate) sender: String,
    pub(crate) receiver: String,
    pub(crate) capacity: Expr,
    pub(crate) actors: [Function; 2],
    pub(crate) arguments: [Vec<Expr>; 2],
    pub(crate) producer: usize,
    pub(crate) flows: Vec<SourceAsyncControlFlow>,
    pub(crate) shape: PacketShape,
}

fn awaited_call(expression: &Expr) -> Option<(&str, &[Expr])> {
    let Expr::Unary { op: UnOp::Await, expr } = expression else { return None };
    let Expr::Call { name, args } = expr.as_ref() else { return None };
    Some((name, args))
}

fn standard(catalog: &RuntimeDeclarationCatalog, name: &str, declaration: &str) -> bool {
    catalog.resolve(name, DeclarationKind::Function).is_some_and(|identity|
        identity.package().source() == &PackageSource::Toolchain
        && identity.package().name() == "witchy/std"
        && identity.module() == ["std", "chan"] && identity.name() == declaration)
}

fn scalar(ty: &Type) -> bool {
    matches!(ty, Type::Named(name, args) if args.is_empty() && matches!(name.as_str(), "Int" | "Bool"))
}

impl PacketRegionSource {
    pub(crate) fn scalar_projection(&self) -> Option<crate::checked_scalar_region::ScalarRegionSource> {
        Some(crate::checked_scalar_region::ScalarRegionSource {
            root: self.root.clone(), channel_statement: self.channel_statement, scope_statement: self.scope_statement,
            sender: self.sender.clone(), receiver: self.receiver.clone(), capacity: self.capacity.clone(),
            actors: self.actors.clone(), arguments: self.arguments.clone(), producer: self.producer, flows: self.flows.clone(),
        })
    }
    pub(crate) fn derive(checked: &CheckedModule, catalog: &RuntimeDeclarationCatalog) -> Option<Self> {
        let typed = checked.typed_async_source()?;
        typed.module().linked_entry.as_deref()?;
        let root = typed.module().items.iter().find_map(|item| match item {
            Item::Function(function) if function.is_async && function.name == "main" => Some(function),
            _ => None,
        })?;
        if root.is_gen || !root.type_params.is_empty() || !root.bounds.is_empty()
            || root.body.region.is_some() || root.body.lines.len() != root.body.stmts.len()
            || !unit_return(root) { return None; }
        if root.params.iter().any(|parameter| parameter.convention != Convention::Let || parameter.default.is_some()) {
            return None;
        }
        if root.params.iter().any(|parameter| !parameter.ty.as_ref().is_some_and(|ty|
            scalar(ty) || matches!(ty, Type::Named(name, args) if args.is_empty()
                && matches!(name.as_str(), "Console" | "Clock")))) { return None; }
        let channels = root.body.stmts.iter().enumerate().filter_map(|(index, statement)| {
            let Stmt::LetPattern { pattern: Pattern::Tuple(endpoints), value } = statement else { return None };
            let [Pattern::Var(sender), Pattern::Var(receiver)] = endpoints.as_slice() else { return None };
            let (name, arguments) = awaited_call(value)?;
            let [capacity] = arguments else { return None };
            standard(catalog, name, "channel").then_some((index, sender, receiver, capacity))
        }).collect::<Vec<_>>();
        let [(channel_statement, sender, receiver, capacity)] = channels.as_slice() else { return None };
        let scopes = root.body.stmts.iter().enumerate().filter_map(|(index, statement)| {
            let Stmt::Expr(expression) = statement else { return None };
            let (name, arguments) = awaited_call(expression)?;
            let [Expr::List(children)] = arguments else { return None };
            standard(catalog, name, "scope").then_some((index, children))
        }).collect::<Vec<_>>();
        let [(scope_statement, children)] = scopes.as_slice() else { return None };
        if channel_statement >= scope_statement || children.len() != 2 { return None; }
        let mut names = root.params.iter().filter(|parameter| parameter.ty.as_ref().is_some_and(scalar))
            .map(|parameter| parameter.name.clone()).collect::<BTreeSet<_>>();
        let clocks = capability_parameters(root, "Clock");
        let consoles = capability_parameters(root, "Console");
        for (index, statement) in root.body.stmts.iter().enumerate() {
            if index == *channel_statement || index == *scope_statement { continue; }
            match statement {
                Stmt::Let { name, value, mutable: false, .. }
                | Stmt::LetPattern { pattern: Pattern::Var(name), value } => {
                    let clock = matches!(value, Expr::Call { name, args } if name == "__capop.now_monotonic"
                        && matches!(args.as_slice(), [Expr::Var(receiver)] if clocks.contains(receiver)));
                    if !(scalar_expression(value, &names) || clock) || !names.insert(name.clone())
                        || typed.table().type_of(value).and_then(typeck::ty_to_ast).is_none_or(|ty| !scalar(&ty)) {
                        return None;
                    }
                }
                Stmt::Expr(_) => {
                    let fragment = witchy_syntax::ast::Block { stmts: vec![statement.clone()], lines: vec![root.body.lines[index]], region: None };
                    if !completion(&fragment, &names, &consoles) { return None; }
                }
                _ => return None,
            }
        }
        if !scalar_expression(capacity, &names)
            || typed.table().type_of(capacity).and_then(typeck::ty_to_ast) != Some(Type::Named("Int".into(), vec![])) {
            return None;
        }
        let mut actors = Vec::new();
        let mut arguments = Vec::new();
        let mut producer = None;
        let mut shape: Option<PacketShape> = None;
        for (ordinal, child) in children.iter().enumerate() {
            let Expr::Call { name, args } = child else { return None };
            let actor = typed.module().items.iter().find_map(|item| match item {
                Item::Function(function) if function.name == *name && function.is_async => Some(function),
                _ => None,
            })?;
            if actor.is_gen || !actor.type_params.is_empty() || !actor.bounds.is_empty() || !unit_return(actor)
                || actor.params.iter().any(|parameter| parameter.convention != Convention::Let || parameter.default.is_some())
                || actor.params.len() != args.len() { return None; }
            let mut endpoint = None;
            for (parameter, argument) in actor.params.iter().zip(args) {
                let ty = parameter.ty.as_ref()?;
                if let Type::Named(endpoint_name, payloads) = ty {
                    if let [Type::Named(payload_name, arguments)] = payloads.as_slice()
                        && arguments.is_empty() {
                        let declaration = catalog.resolve(endpoint_name, DeclarationKind::Type)?;
                        if declaration.package().source() == &PackageSource::Toolchain
                            && declaration.package().name() == "witchy/std" && declaration.module() == ["std", "chan"]
                        {
                            let identity = catalog.resolve(payload_name, DeclarationKind::Type)?;
                            if identity.package().source() != &PackageSource::Workspace { return None; }
                            let definition = typed.module().items.iter().find_map(|item| match item {
                                Item::Type(definition) if definition.name == *payload_name => Some(definition), _ => None,
                            })?;
                            let actual = PacketShape::derive(definition)?;
                            if shape.as_ref().is_some_and(|expected| expected != &actual
                                || format!("{expected:?}") != format!("{actual:?}")) { return None; }
                            shape = Some(actual);
                            let expected = match declaration.name() {
                                "Sender" if producer.is_none() => { producer = Some(ordinal); *sender },
                                "Receiver" => *receiver,
                                _ => return None,
                            };
                            if endpoint.replace(parameter.name.clone()).is_some()
                                || !matches!(argument, Expr::Var(name) if name == expected) { return None; }
                            continue;
                        }
                    }
                }
                if !scalar(ty) && !matches!(ty, Type::Named(name, args) if name == "Console" && args.is_empty()) {
                    return None;
                }
                if scalar(ty) {
                    if !scalar_expression(argument, &names) { return None; }
                } else if !matches!(argument, Expr::Var(name) if consoles.contains(name)) { return None; }
            }
            endpoint?;
            actors.push(actor.clone());
            arguments.push(args.clone());
        }
        let producer = producer?;
        let shape = shape?;
        PacketActor::derive(&actors[producer], &shape, true)?;
        PacketActor::derive(&actors[1 - producer], &shape, false)?;
        let inventory = checked.source_async_control_flow().ok()?;
        let flows = [root, &actors[0], &actors[1]].into_iter().map(|function|
            inventory.callables.iter().find(|flow| crate::typed_async_lowering::typed_source_function(typed, &flow.callable)
                .is_some_and(|source| std::ptr::eq(source, function) || source.name == function.name)).cloned())
            .collect::<Option<Vec<_>>>()?;
        for (index, flow) in flows.iter().enumerate() {
            if flow.mode.is_opt() { return None; }
            if flow.bindings.iter().map(|binding| binding.name.as_str()).collect::<BTreeSet<_>>().len() != flow.bindings.len()
                || flow.operations.iter().any(|operation| matches!(operation.kind, SourceAsyncOperationKind::DynamicCall | SourceAsyncOperationKind::Return)) {
                return None;
            }
            let expected = if index == 0 { 2 } else if index - 1 == producer { 1 } else { 0 };
            if flow.operations.iter().filter(|operation| operation.kind == SourceAsyncOperationKind::Await).count() != expected {
                return None;
            }
            if index == 0 {
                if !flow.controls.is_empty() { return None; }
            } else {
                let function = crate::typed_async_lowering::typed_source_function(typed, &flow.callable)?;
                if index - 1 == producer {
                    let [control] = flow.controls.as_slice() else { return None };
                    let selector = source_expression(function, control.selector)?;
                    if !control.path.is_empty() || control.kind != SourceControlKind::While
                        || typed.table().type_of(selector).and_then(typeck::ty_to_ast) != Some(Type::Named("Bool".into(), vec![])) {
                        return None;
                    }
                } else {
                    let [receive, destructure] = flow.controls.as_slice() else { return None };
                    let selector = source_expression(function, receive.selector)?;
                    let selector_type = typed.table().type_of(selector).and_then(typeck::ty_to_ast)?;
                    if !receive.path.is_empty() || receive.kind != SourceControlKind::For
                        || !matches!(selector, Expr::Call { name, .. } if standard(catalog, name, "__recv_stream"))
                        || !matches!(selector_type.unqualified(), Type::Named(name, arguments)
                            if name == "List" && arguments.as_slice() == [shape.payload_type()])
                        || destructure.kind != (SourceControlKind::Match { arms: 1 })
                        || destructure.path != [crate::async_control_flow::ControlPathStep::LoopBody(receive.id)] {
                        return None;
                    }
                    let selector = source_expression(function, destructure.selector)?;
                    if typed.table().type_of(selector).and_then(typeck::ty_to_ast) != Some(shape.payload_type()) {
                        return None;
                    }
                }
            }
            let function = crate::typed_async_lowering::typed_source_function(typed, &flow.callable)?;
            for operation in &flow.operations {
                let expression = operation.expression?;
                let operand = source_expression(function, expression)?;
                typed.table().type_of(operand).and_then(typeck::ty_to_ast)?;
                flow.bindings_read_by(expression).ok()?;
            }
            let endpoints = if index == 0 { vec![(*sender).clone(), (*receiver).clone()] } else {
                vec![crate::scalar_region_recipe::ActorRecipe::derive(&PacketActor::derive(&actors[index - 1], &shape, index - 1 == producer)?.function, index - 1 == producer)?.endpoint]
            };
            for endpoint in endpoints {
                let binding = flow.bindings.iter().find(|binding| binding.name == endpoint)?;
                if flow.binding_uses.iter().filter(|usage| usage.binding == binding.id).count() != 1 { return None; }
            }
        }
        let mut root_referenced = false;
        witchy_syntax::ast::visit::visit_module_exprs(typed.module(), &mut |expression| {
            root_referenced |= matches!(expression, Expr::Call { name, .. } | Expr::Var(name) if name == "main");
            Ok::<_, ()>(())
        }).ok()?;
        if root_referenced { return None; }
        Some(Self { root: root.clone(), channel_statement: *channel_statement, scope_statement: *scope_statement,
            sender: (*sender).clone(), receiver: (*receiver).clone(), capacity: (*capacity).clone(),
            actors: actors.try_into().ok()?, arguments: arguments.try_into().ok()?, producer, flows, shape })
    }
}

fn unit_return(function: &Function) -> bool {
    function.ret.as_ref().is_none_or(|ty| matches!(ty, Type::Tuple(values) if values.is_empty()))
}

fn capability_parameters(function: &Function, capability: &str) -> BTreeSet<String> {
    function.params.iter().filter(|parameter|
        matches!(parameter.ty.as_ref(), Some(Type::Named(name, args)) if name == capability && args.is_empty()))
        .map(|parameter| parameter.name.clone()).collect()
}
