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
fn lane_index(expression: Expr, width: i64, lane: i64) -> Expr {
    let base = binary(BinOp::Mul, expression, Expr::Int(width));
    if lane == 0 { base } else { binary(BinOp::Add, base, Expr::Int(lane)) }
}
fn physical(width: i64) -> Expr {
    binary(BinOp::Div, call("list.length", vec![variable("scalar_region_queue")]), Expr::Int(width))
}
fn assign(name: &str, value: Expr) -> Stmt { Stmt::Assign { name: name.into(), value, place_depth: 0 } }
fn renamed(mut expression: Expr, names: &BTreeMap<String, String>) -> Expr {
    witchy_syntax::ast::visit::visit_expr_mut(&mut expression, &mut |expression| {
        if let Expr::Var(name) = expression && let Some(replacement) = names.get(name) { *name = replacement.clone(); }
        Ok::<_, ()>(())
    }).expect("infallible packet operand renaming");
    expression
}

pub(crate) fn companion(source: &PacketRegionSource) -> Option<Function> {
    let producer = PacketActor::derive_source(&source.actors[source.producer], &source.shape, true, source.forwarder.as_ref())?;
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
    let payloads = producer.remaining_payloads.into_iter().map(|payload| renamed(payload, &names)).collect::<Vec<_>>();
    let first_received = crate::scalar_region_recipe::ActorRecipe::derive(&receiver.function, false)?.received?;
    let first_received = format!("scalar_region_actor_{}_{}", 1 - source.producer, first_received);
    let received = receiver.remaining_received.iter().map(|name|
        format!("scalar_region_actor_{}_{}", 1 - source.producer, name)).collect::<Vec<_>>();
    if payloads.len() != received.len() { return None; }
    let width = i64::try_from(payloads.len()).ok()?.checked_add(1)?;
    transform(&mut companion.body, &first_received, &received, &payloads, &source.shape.definition.variants[0].fields, width)?;
    Some(companion)
}

fn transform(body: &mut Block, first_received: &str, remaining_received: &[String], payloads: &[Expr], types: &[witchy_syntax::ast::Type], width: i64) -> Option<()> {
    let statements = std::mem::take(&mut body.stmts);
    let lines = std::mem::take(&mut body.lines);
    if statements.len() != lines.len() { return None; }
    let pending = |lane: usize| format!("scalar_region_pending_{lane}");
    for (mut statement, line) in statements.into_iter().zip(lines) {
        if let Stmt::Expr(expression) = &mut statement {
            match expression {
                Expr::Block(block) | Expr::While { body: block, .. } => {
                    transform(block, first_received, remaining_received, payloads, types, width)?;
                }
                Expr::If { then_block, else_block, .. } => {
                    transform(then_block, first_received, remaining_received, payloads, types, width)?;
                    if let Some(block) = else_block {
                        transform(block, first_received, remaining_received, payloads, types, width)?;
                    }
                }
                _ => {}
            }
        }
        let mut extra = Vec::new();
        match &mut statement {
            Stmt::Let { name, value, .. } if name == "scalar_region_queue" => {
                if *value != Expr::List(vec![Expr::Int(0)]) { return None; }
                *value = Expr::List(vec![Expr::Int(0); usize::try_from(width).ok()?]);
            }
            Stmt::Let { name, value, .. } if name == "scalar_region_physical" => *value = physical(width),
            Stmt::Let { name, value, .. } if name == "scalar_region_grown" => {
                *value = call("list.repeat", vec![Expr::Int(0), binary(BinOp::Mul,
                    variable("scalar_region_physical"), Expr::Int(width.checked_mul(2)?))]);
            }
            Stmt::Let { name, .. } if name == "scalar_region_pending" => {
                for lane in 1..=payloads.len() {
                    let mut field = statement.clone();
                    let Stmt::Let { name, .. } = &mut field else { return None };
                    *name = pending(lane);
                    extra.push(field);
                }
            }
            Stmt::Assign { name, .. } if name == "scalar_region_pending" => {
                for (lane, payload) in payloads.iter().enumerate() {
                    extra.push(assign(&pending(lane + 1), payload.clone()));
                }
            }
            Stmt::Assign { name, value, .. } if name == first_received => {
                *value = call("list.at", vec![variable("scalar_region_queue"), lane_index(variable("scalar_region_head"), width, 0)]);
                for (lane, received) in remaining_received.iter().enumerate() {
                    extra.push(assign(received, crate::packet_region_recipe::decode(call("list.at", vec![variable("scalar_region_queue"),
                        lane_index(variable("scalar_region_head"), width, i64::try_from(lane + 1).ok()?)]), &types[lane + 1])));
                }
            }
            Stmt::Assign { name, value, .. } if name == "scalar_region_head" => {
                if matches!(value, Expr::Binary { op: BinOp::Mod, .. }) {
                    *value = binary(BinOp::Mod, binary(BinOp::Add, variable("scalar_region_head"), Expr::Int(1)), physical(width));
                }
            }
            Stmt::Expr(Expr::Call { name, args }) if name == "list.set_at" => {
                let [Expr::Var(buffer), index, value] = args.as_slice() else { return None };
                if buffer == "scalar_region_grown" {
                    let Expr::Call { name, args: read } = value else { return None };
                    let [queue, read_index] = read.as_slice() else { return None };
                    if name != "list.at" || queue != &variable("scalar_region_queue") { return None; }
                    for lane in 1..width {
                        extra.push(Stmt::Expr(call("list.set_at", vec![variable(buffer),
                            lane_index(index.clone(), width, lane),
                            call("list.at", vec![queue.clone(), lane_index(read_index.clone(), width, lane)])])));
                    }
                    *args = vec![variable(buffer), lane_index(index.clone(), width, 0),
                        call("list.at", vec![queue.clone(), lane_index(read_index.clone(), width, 0)])];
                } else if buffer == "scalar_region_queue" && value == &variable("scalar_region_pending") {
                    let index = binary(BinOp::Mod, binary(BinOp::Add, variable("scalar_region_head"), variable("scalar_region_count")), physical(width));
                    for lane in 1..=payloads.len() {
                        extra.push(Stmt::Expr(call("list.set_at", vec![variable(buffer),
                            lane_index(index.clone(), width, i64::try_from(lane).ok()?), variable(&pending(lane))])));
                    }
                    *args = vec![variable(buffer), lane_index(index, width, 0), variable("scalar_region_pending")];
                } else { return None; }
            }
            _ => {}
        }
        body.stmts.push(statement);
        body.lines.push(line);
        body.lines.extend(std::iter::repeat_n(line, extra.len()));
        body.stmts.extend(extra);
    }
    Some(())
}
