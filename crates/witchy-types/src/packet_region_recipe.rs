use std::collections::BTreeSet;
use witchy_syntax::ast::{Block, Expr, Function, Pattern, Stmt, Type, TypeDef, UnOp};
use crate::scalar_region_recipe::{ActorRecipe, scalar_expression};

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PacketShape {
    pub definition: TypeDef,
}

impl PacketShape {
    pub(crate) fn derive(definition: &TypeDef) -> Option<Self> {
        let [variant] = definition.variants.as_slice() else { return None };
        if definition.must_consume || definition.sealed || definition.is_capability
            || definition.grantable || definition.packed || !definition.params.is_empty()
            || !definition.derives.is_empty() || definition.partial_eq_derived || definition.public_state_derived
            || !variant.field_names.is_empty()
            || variant.fields != [named("Int"), named("Int")] { return None; }
        Some(Self { definition: definition.clone() })
    }
    pub(crate) fn payload_type(&self) -> Type { named(&self.definition.name) }
    pub(crate) fn constructor(&self) -> &str { &self.definition.variants[0].name }
}

pub(crate) struct PacketActor {
    pub function: Function,
    pub second_payload: Option<Expr>,
    pub second_received: Option<String>,
}

fn named(name: &str) -> Type { Type::Named(name.into(), vec![]) }

impl PacketActor {
    pub(crate) fn derive(original: &Function, shape: &PacketShape, producer: bool) -> Option<Self> {
        let mut function = original.clone();
        let endpoint_name = if producer { "chan.Sender" } else { "chan.Receiver" };
        let mut endpoint = None;
        for parameter in &mut function.params {
            if let Some(Type::Named(name, arguments)) = &mut parameter.ty
                && name == endpoint_name && arguments.as_slice() == [shape.payload_type()] {
                if endpoint.replace(parameter.name.clone()).is_some() { return None; }
                *arguments = vec![named("Int")];
            }
        }
        let endpoint = endpoint?;
        let mut second_payload = None;
        let mut second_received = None;
        if producer {
            let loops = function.body.stmts.iter_mut().filter_map(|statement| match statement {
                Stmt::Expr(Expr::While { body, .. }) => Some(body), _ => None,
            }).collect::<Vec<_>>();
            let [body]: [&mut Block; 1] = loops.try_into().ok()?;
            let sends = body.stmts.iter_mut().filter_map(|statement| match statement {
                Stmt::Expr(Expr::Unary { op: UnOp::Await, expr }) => match expr.as_mut() {
                    Expr::Call { name, args } if name == "chan.send" && args.len() == 2
                        && matches!(&args[0], Expr::Var(name) if name == &endpoint) => Some(&mut args[1]),
                    _ => None,
                },
                _ => None,
            }).collect::<Vec<_>>();
            let [payload]: [&mut Expr; 1] = sends.try_into().ok()?;
            let Expr::Ctor { name, args } = payload else { return None };
            let [first, second] = args.as_slice() else { return None };
            if name != shape.constructor() { return None; }
            second_payload = Some(second.clone());
            *payload = first.clone();
        } else {
            let loops = function.body.stmts.iter().enumerate().filter_map(|(index, statement)| match statement {
                Stmt::Expr(Expr::For { .. }) => Some(index), _ => None,
            }).collect::<Vec<_>>();
            let [index] = loops.as_slice() else { return None };
            let index = *index;
            let Stmt::Expr(Expr::For { var, iter, body }) = &mut function.body.stmts[index] else { return None };
            if !matches!(iter.as_ref(), Expr::Call { name, args } if name == "chan.__recv_stream"
                && matches!(args.as_slice(), [Expr::Var(name)] if name == &endpoint)) { return None; }
            let [Stmt::Expr(Expr::Match { scrutinee, arms })] = body.stmts.as_slice() else { return None };
            if body.lines.len() != 1 || body.region.is_some()
                || !matches!(scrutinee.as_ref(), Expr::Var(name) if name == var) { return None; }
            let [arm] = arms.as_slice() else { return None };
            let Pattern::Ctor { name, args } = &arm.pattern else { return None };
            let [Pattern::Var(first), Pattern::Var(second)] = args.as_slice() else { return None };
            if name != shape.constructor() || arm.guard.is_some() || first == second { return None; }
            let Expr::Block(flattened) = &arm.body else { return None };
            if flattened.region.is_some() || flattened.lines.len() != flattened.stmts.len() { return None; }
            let mut leaks_packet = false;
            witchy_syntax::ast::visit::visit_block(flattened, &mut |expression| {
                leaks_packet |= matches!(expression, Expr::Var(name) if name == var);
                Ok::<_, ()>(())
            }).ok()?;
            if leaks_packet { return None; }
            let second = second.clone();
            *var = first.clone();
            *body = flattened.clone();
            second_received = Some(second.clone());
            function.body.stmts.insert(index, Stmt::Let {
                name: second, ty: Some(named("Int")), mutable: true, value: Expr::Int(0),
            });
            function.body.lines.insert(index, function.body.lines[index]);
        }
        let recipe = ActorRecipe::derive(&function, producer)?;
        let mut bindings = function.params.iter().filter(|parameter|
            matches!(parameter.ty.as_ref(), Some(Type::Named(name, args)) if args.is_empty() && matches!(name.as_str(), "Int" | "Bool")))
            .map(|parameter| parameter.name.clone()).collect::<BTreeSet<_>>();
        bindings.extend(recipe.fields.iter().map(|(name, _)| name.clone()));
        if second_payload.as_ref().is_some_and(|expression| !scalar_expression(expression, &bindings)) { return None; }
        Some(Self { function, second_payload, second_received })
    }
}
