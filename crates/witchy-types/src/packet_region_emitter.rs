use std::collections::BTreeMap;
use witchy_syntax::ast::{BinOp, Block, Expr, Function, Stmt};
use crate::checked_packet_region::PacketRegionSource;
use crate::packet_region_recipe::PacketActor;

pub(crate) const COMPANION: &str = "__witchy_closed_packet_root";

fn call(name: &str, args: Vec<Expr>) -> Expr { Expr::Call { name: name.into(), args } }
fn variable(name: &str) -> Expr { Expr::Var(name.into()) }
fn binary(op: BinOp, lhs: Expr, rhs: Expr) -> Expr {
    Expr::Binary { op, lhs: Box::new(lhs), rhs: Box::new(rhs) }
}
fn doubled(expression: Expr) -> Expr { binary(BinOp::Mul, expression, Expr::Int(2)) }
fn physical() -> Expr { binary(BinOp::Div, call("list.length", vec![variable("scalar_region_queue")]), Expr::Int(2)) }
fn assign(name: &str, value: Expr) -> Stmt { Stmt::Assign { name: name.into(), value, place_depth: 0 } }
fn renamed(mut expression: Expr, names: &BTreeMap<String, String>) -> Expr {
    witchy_syntax::ast::visit::visit_expr_mut(&mut expression, &mut |expression| {
        if let Expr::Var(name) = expression && let Some(replacement) = names.get(name) { *name = replacement.clone(); }
        Ok::<_, ()>(())
    }).expect("infallible packet operand renaming");
    expression
}

pub(crate) fn companion(source: &PacketRegionSource) -> Option<Function> {
    let producer = PacketActor::derive(&source.actors[source.producer], &source.shape, true)?;
    let receiver = PacketActor::derive(&source.actors[1 - source.producer], &source.shape, false)?;
    let mut scalar = source.scalar_projection()?;
    scalar.actors[source.producer] = producer.function.clone();
    scalar.actors[1 - source.producer] = receiver.function.clone();
    let mut companion = crate::scalar_region_emitter::companion(&scalar)?;
    companion.name = COMPANION.into();
    let mut names = BTreeMap::new();
    let recipe = crate::scalar_region_recipe::ActorRecipe::derive(&producer.function, true)?;
    for parameter in &producer.function.params {
        if parameter.name != recipe.endpoint {
            names.insert(parameter.name.clone(), format!("scalar_region_actor_{}_{}", source.producer, parameter.name));
        }
    }
    for (name, _) in &recipe.fields {
        names.insert(name.clone(), format!("scalar_region_actor_{}_{}", source.producer, name));
    }
    let second_payload = renamed(producer.second_payload?, &names);
    let first_received = crate::scalar_region_recipe::ActorRecipe::derive(&receiver.function, false)?.received?;
    let first_received = format!("scalar_region_actor_{}_{}", 1 - source.producer, first_received);
    let second_received = format!("scalar_region_actor_{}_{}", 1 - source.producer, receiver.second_received?);
    transform(&mut companion.body, &first_received, &second_received, &second_payload)?;
    Some(companion)
}

fn transform(body: &mut Block, first_received: &str, second_received: &str, second_payload: &Expr) -> Option<()> {
    let statements = std::mem::take(&mut body.stmts);
    let lines = std::mem::take(&mut body.lines);
    if statements.len() != lines.len() { return None; }
    for (mut statement, line) in statements.into_iter().zip(lines) {
        if let Stmt::Expr(expression) = &mut statement {
            match expression {
                Expr::Block(block) | Expr::While { body: block, .. } => {
                    transform(block, first_received, second_received, second_payload)?;
                }
                Expr::If { then_block, else_block, .. } => {
                    transform(then_block, first_received, second_received, second_payload)?;
                    if let Some(block) = else_block {
                        transform(block, first_received, second_received, second_payload)?;
                    }
                }
                _ => {}
            }
        }
        let mut extra = None;
        match &mut statement {
            Stmt::Let { name, value, .. } if name == "scalar_region_queue" => {
                if *value != Expr::List(vec![Expr::Int(0)]) { return None; }
                *value = Expr::List(vec![Expr::Int(0), Expr::Int(0)]);
            }
            Stmt::Let { name, value, .. } if name == "scalar_region_physical" => *value = physical(),
            Stmt::Let { name, value, .. } if name == "scalar_region_grown" => {
                *value = call("list.repeat", vec![Expr::Int(0), binary(BinOp::Mul, variable("scalar_region_physical"), Expr::Int(4))]);
            }
            Stmt::Let { name, .. } if name == "scalar_region_pending" => {
                let mut second = statement.clone();
                let Stmt::Let { name, .. } = &mut second else { return None };
                *name = "scalar_region_pending_second".into();
                extra = Some(second);
            }
            Stmt::Assign { name, .. } if name == "scalar_region_pending" => {
                extra = Some(assign("scalar_region_pending_second", second_payload.clone()));
            }
            Stmt::Assign { name, value, .. } if name == first_received => {
                *value = call("list.at", vec![variable("scalar_region_queue"), doubled(variable("scalar_region_head"))]);
                extra = Some(assign(second_received, call("list.at", vec![variable("scalar_region_queue"),
                    binary(BinOp::Add, doubled(variable("scalar_region_head")), Expr::Int(1))])));
            }
            Stmt::Assign { name, value, .. } if name == "scalar_region_head" => {
                if matches!(value, Expr::Binary { op: BinOp::Mod, .. }) {
                    *value = binary(BinOp::Mod, binary(BinOp::Add, variable("scalar_region_head"), Expr::Int(1)), physical());
                }
            }
            Stmt::Expr(Expr::Call { name, args }) if name == "list.set_at" => {
                let [Expr::Var(buffer), index, value] = args.as_slice() else { return None };
                if buffer == "scalar_region_grown" {
                    let Expr::Call { name, args: read } = value else { return None };
                    let [queue, read_index] = read.as_slice() else { return None };
                    if name != "list.at" || queue != &variable("scalar_region_queue") { return None; }
                    extra = Some(Stmt::Expr(call("list.set_at", vec![variable(buffer),
                        binary(BinOp::Add, doubled(index.clone()), Expr::Int(1)),
                        call("list.at", vec![queue.clone(), binary(BinOp::Add, doubled(read_index.clone()), Expr::Int(1))])])));
                    *args = vec![variable(buffer), doubled(index.clone()), call("list.at", vec![queue.clone(), doubled(read_index.clone())])];
                } else if buffer == "scalar_region_queue" && value == &variable("scalar_region_pending") {
                    let index = binary(BinOp::Mod, binary(BinOp::Add, variable("scalar_region_head"), variable("scalar_region_count")), physical());
                    extra = Some(Stmt::Expr(call("list.set_at", vec![variable(buffer),
                        binary(BinOp::Add, doubled(index.clone()), Expr::Int(1)), variable("scalar_region_pending_second")])));
                    *args = vec![variable(buffer), doubled(index), variable("scalar_region_pending")];
                } else { return None; }
            }
            _ => {}
        }
        body.stmts.push(statement);
        body.lines.push(line);
        if let Some(statement) = extra {
            body.stmts.push(statement);
            body.lines.push(line);
        }
    }
    Some(())
}
