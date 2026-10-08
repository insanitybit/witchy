use std::collections::BTreeSet;
use witchy_syntax::ast::{BinOp, Block, Expr, Function, Pattern, Stmt, Type, UnOp};

pub(crate) struct ActorRecipe {
    pub initial: Block,
    pub loop_prefix: Block,
    pub condition: Option<Expr>,
    pub payload: Option<Expr>,
    pub payload_line: u32,
    pub condition_line: u32,
    pub loop_suffix: Block,
    pub completion: Block,
    pub fields: Vec<(String, Type)>,
    pub endpoint: String,
    pub received: Option<String>,
}

fn block(statements: &[Stmt], lines: &[u32]) -> Block {
    Block { stmts: statements.to_vec(), lines: lines.to_vec(), region: None }
}

pub(crate) fn scalar_expression(expression: &Expr, bindings: &BTreeSet<String>) -> bool {
    match expression {
        Expr::Int(_) | Expr::Bool(_) => true,
        Expr::Var(name) => bindings.contains(name),
        Expr::Binary { op: BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Eq | BinOp::NotEq
            | BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq | BinOp::And | BinOp::Or
            | BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor, lhs, rhs } => {
            scalar_expression(lhs, bindings) && scalar_expression(rhs, bindings)
        }
        Expr::Unary { op: UnOp::Neg | UnOp::Not, expr } => scalar_expression(expr, bindings),
        _ => false,
    }
}

pub(crate) fn printed_expression(expression: &Expr, bindings: &BTreeSet<String>) -> bool {
    match expression {
        Expr::Str(_) => true,
        Expr::Binary { lhs, rhs, .. } => printed_expression(lhs, bindings) && printed_expression(rhs, bindings),
        Expr::Call { name, args } if witchy_syntax::intrinsics::lookup(name)
            .is_some_and(|specification| specification.id == witchy_syntax::intrinsics::IntrinsicId::GeneratedRender) => {
            matches!(args.as_slice(), [value] if scalar_expression(value, bindings))
        }
        _ => false,
    }
}

pub(crate) fn completion(block: &Block, bindings: &BTreeSet<String>, consoles: &BTreeSet<String>, clocks: &BTreeSet<String>) -> bool {
    let mut bindings = bindings.clone();
    block.stmts.iter().all(|statement| match statement {
        Stmt::Let { name, ty, mutable: false, value } => {
            let clock = matches!(value, Expr::Call { name, args } if name == "__capop.now_monotonic"
                && matches!(args.as_slice(), [Expr::Var(receiver)] if clocks.contains(receiver)));
            ty.as_ref().is_none_or(|ty| matches!(ty, Type::Named(name, args) if args_empty_scalar(name, args)))
                && (clock || scalar_expression(value, &bindings)) && bindings.insert(name.clone())
        }
        Stmt::Expr(Expr::Tuple(values)) => values.is_empty(),
        Stmt::Expr(Expr::Call { name, args }) if name == "__capop.print" => {
            matches!(args.as_slice(), [Expr::Var(console), message]
                if consoles.contains(console) && printed_expression(message, &bindings))
        }
        _ => false,
    })
}

impl ActorRecipe {
    pub(crate) fn derive(function: &Function, producer: bool) -> Option<Self> {
        if function.body.lines.len() != function.body.stmts.len() { return None; }
        let loops = function.body.stmts.iter().enumerate().filter_map(|(index, statement)| match statement {
            Stmt::Expr(Expr::While { cond, body }) if producer => Some((index, Some(cond.as_ref()), None, body)),
            Stmt::Expr(Expr::For { var, iter, body }) if !producer => {
                let Expr::Call { name, args } = iter.as_ref() else { return None };
                if name != "chan.__recv_stream" || args.len() != 1 { return None; }
                Some((index, None, Some((var, &args[0])), body))
            }
            _ => None,
        }).collect::<Vec<_>>();
        let [(loop_index, condition, receive, body)] = loops.as_slice() else { return None };
        if body.region.is_some() || function.body.region.is_some()
            || body.lines.len() != body.stmts.len() { return None; }
        let mut names = BTreeSet::new();
        let mut fields = Vec::new();
        let mut endpoint = None;
        for parameter in &function.params {
            let ty = parameter.ty.as_ref()?;
            if let Type::Named(name, payload) = ty {
                if matches!(name.as_str(), "chan.Sender" | "chan.Receiver")
                    && matches!(payload.as_slice(), [Type::Named(name, args)] if name == "Int" && args.is_empty())
                {
                    endpoint = Some(parameter.name.clone());
                    continue;
                }
                if args_empty_scalar(name, payload) {
                    names.insert(parameter.name.clone());
                }
            }
        }
        let endpoint = endpoint?;
        let clocks = function.params.iter().filter(|parameter|
            matches!(parameter.ty.as_ref(), Some(Type::Named(name, args)) if name == "Clock" && args.is_empty()))
            .map(|parameter| parameter.name.clone()).collect::<BTreeSet<_>>();
        let consoles = function.params.iter().filter(|parameter|
            matches!(parameter.ty.as_ref(), Some(Type::Named(name, args)) if name == "Console" && args.is_empty()))
            .map(|parameter| parameter.name.clone()).collect::<BTreeSet<_>>();
        for statement in &function.body.stmts[..*loop_index] {
            let (name, ty, value) = match statement {
                Stmt::Let { name, ty, value, .. } => (name, ty.clone(), value),
                Stmt::LetPattern { pattern: Pattern::Var(name), value } => (name, None, value),
                _ => return None,
            };
            if !scalar_expression(value, &names) || !names.insert(name.clone()) { return None; }
            let ty = ty.or_else(|| match value { Expr::Int(_) => Some(named("Int")), Expr::Bool(_) => Some(named("Bool")), _ => None })?;
            if !matches!(&ty, Type::Named(name, args) if args_empty_scalar(name, args)) { return None; }
            fields.push((name.clone(), ty));
        }
        let (prefix, prefix_lines, payload, payload_line, suffix, suffix_lines, received) = if producer {
            let sends = body.stmts.iter().enumerate().filter_map(|(index, statement)| {
                let Stmt::Expr(Expr::Unary { op: UnOp::Await, expr }) = statement else { return None };
                let Expr::Call { name, args } = expr.as_ref() else { return None };
                let [Expr::Var(sender), payload] = args.as_slice() else { return None };
                (name == "chan.send" && sender == &endpoint).then_some((index, payload))
            }).collect::<Vec<_>>();
            let [(index, payload)] = sends.as_slice() else { return None };
            if !scalar_expression(payload, &names) { return None; }
            (&body.stmts[..*index], &body.lines[..*index], Some((*payload).clone()), body.lines[*index],
                &body.stmts[*index + 1..], &body.lines[*index + 1..], None)
        } else {
            let (variable, receiver) = receive.as_ref()?;
            if !matches!(receiver, Expr::Var(name) if name == &endpoint) || !names.insert((*variable).clone()) { return None; }
            fields.push(((*variable).clone(), named("Int")));
            (&[][..], &[][..], None, 0, body.stmts.as_slice(), body.lines.as_slice(), Some((*variable).clone()))
        };
        for statement in prefix.iter().chain(suffix) {
            match statement {
                Stmt::Assign { name, value, place_depth: 0 } if names.contains(name) && scalar_expression(value, &names) => {},
                Stmt::Expr(Expr::Tuple(values)) if values.is_empty() => {},
                _ => return None,
            }
        }
        if condition.is_some_and(|condition| !scalar_expression(condition, &names)) { return None; }
        let completion_body = block(&function.body.stmts[*loop_index + 1..], &function.body.lines[*loop_index + 1..]);
        if !completion(&completion_body, &names, &consoles, &clocks) { return None; }
        Some(Self { initial: block(&function.body.stmts[..*loop_index], &function.body.lines[..*loop_index]),
            loop_prefix: block(prefix, prefix_lines), condition: condition.map(|condition| (*condition).clone()), payload,
            payload_line, condition_line: function.body.lines[*loop_index],
            loop_suffix: block(suffix, suffix_lines), completion: completion_body,
            fields, endpoint, received })
    }
}

fn args_empty_scalar(name: &str, args: &[Type]) -> bool {
    args.is_empty() && matches!(name, "Int" | "Bool")
}
fn named(name: &str) -> Type { Type::Named(name.into(), Vec::new()) }
