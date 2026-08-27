//! Structured, source-faithful async control-flow inventory.
//!
//! This is an input to RFC-0161 typed lowering, not its final executable
//! `AsyncProgram`.  In particular it intentionally has no frame layout,
//! ownership transition, backend binding, or fingerprint.  Its job is narrower
//! and essential: preserve mutually exclusive source paths while the legacy
//! CPS lowering and its scalar recognizers still exist.

use witchy_syntax::ast::{Block, Expr, Function, Item, Stmt};
use witchy_syntax::async_surface::{AsyncSurfaceId, LinkedAsyncSurface};

/// Stable source identity of one async callable in a checked linked program.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AsyncCallableId {
    pub module: String,
    pub surface: AsyncSurfaceId,
}

/// A deterministic identity for one source control construct inside an async
/// callable.  This is assigned in lexical preorder, separately for every
/// callable.  A branch edge must name its owning construct: `then` alone is
/// ambiguous when a function contains two sequential `if`s.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SourceControlId(pub u32);

/// A source-expression coordinate within one async callable. The coordinate is
/// assigned by lexical preorder while preserving the source item, so a later
/// typed lowering pass can recover the exact operand expression without
/// inspecting generated CPS closures or relying on a callable spelling.
///
/// This is source metadata, not a canonical execution ID. Inserting an
/// unrelated expression may renumber later coordinates; final program IDs are
/// assigned only after typed layout and CFG construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SourceExpressionId(pub u32);

/// A nesting step that distinguishes sequential and mutually-exclusive paths.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ControlPathStep {
    IfThen(SourceControlId),
    IfElse(SourceControlId),
    MatchArm { control: SourceControlId, arm: u32 },
    LoopBody(SourceControlId),
}

/// Source operation category. Names are deliberately excluded: callable
/// spelling is diagnostic data, never a scheduler or fingerprint identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SourceAsyncOperationKind {
    Await,
    StaticCall,
    DynamicCall,
    Return,
}

/// The source construct which owns a set of mutually-exclusive or repeating
/// paths.  This is control topology, not an executable condition: the typed
/// producer supplies condition operands and final state edges later.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SourceControlKind {
    If,
    Match { arms: u32 },
    While,
    For,
    WhileLet,
}

/// One source control node in lexical preorder. `path` describes the outer
/// controls only; it never includes this node's own arm/body step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceAsyncControl {
    pub id: SourceControlId,
    pub path: Vec<ControlPathStep>,
    pub kind: SourceControlKind,
}

/// One operation in lexical source order, qualified by its structured path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceAsyncOperation {
    pub ordinal: u32,
    pub path: Vec<ControlPathStep>,
    pub kind: SourceAsyncOperationKind,
    /// The exact source expression that performs this operation. A terminal
    /// `return` without a value has no expression operand.
    pub expression: Option<SourceExpressionId>,
}

/// Operations retained for one source async callable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceAsyncControlFlow {
    pub callable: AsyncCallableId,
    pub controls: Vec<SourceAsyncControl>,
    pub operations: Vec<SourceAsyncOperation>,
}

/// Complete source control-flow inventory retained at the checked boundary.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceAsyncControlFlowInventory {
    pub callables: Vec<SourceAsyncControlFlow>,
}

impl SourceAsyncControlFlowInventory {
    pub fn from_surface(surface: &LinkedAsyncSurface) -> Self {
        let mut callables = Vec::new();
        for module in &surface.modules {
            for item in &module.items {
                let function = match (&item.item, item.id.method) {
                    (Item::Function(function), None) => Some(function),
                    (Item::Impl(definition), Some(method)) => definition.methods.get(method as usize),
                    _ => None,
                };
                let Some(function) = function else { continue };
                let (controls, operations) = collect_function(function);
                callables.push(SourceAsyncControlFlow {
                    callable: AsyncCallableId { module: module.module.clone(), surface: item.id.clone() },
                    controls,
                    operations,
                });
            }
        }
        callables.sort_by(|left, right| left.callable.cmp(&right.callable));
        Self { callables }
    }
}

fn collect_function(function: &Function) -> (Vec<SourceAsyncControl>, Vec<SourceAsyncOperation>) {
    let mut control_nodes = Vec::new();
    let mut operations = Vec::new();
    let mut controls = 0;
    let mut expressions = 0;
    collect_block(
        &function.body,
        &mut Vec::new(),
        &mut control_nodes,
        &mut operations,
        &mut controls,
        &mut expressions,
    );
    for (ordinal, operation) in operations.iter_mut().enumerate() {
        operation.ordinal = u32::try_from(ordinal).expect("async operation count exceeds u32");
    }
    (control_nodes, operations)
}

fn push(
    operations: &mut Vec<SourceAsyncOperation>,
    path: &[ControlPathStep],
    kind: SourceAsyncOperationKind,
    expression: Option<SourceExpressionId>,
) {
    operations.push(SourceAsyncOperation { ordinal: 0, path: path.to_vec(), kind, expression });
}

fn fresh_control(controls: &mut u32) -> SourceControlId {
    let id = SourceControlId(*controls);
    *controls = controls.checked_add(1).expect("async control count exceeds u32");
    id
}

fn collect_block(
    block: &Block,
    path: &mut Vec<ControlPathStep>,
    control_nodes: &mut Vec<SourceAsyncControl>,
    operations: &mut Vec<SourceAsyncOperation>,
    controls: &mut u32,
    expressions: &mut u32,
) {
    for statement in &block.stmts {
        match statement {
            Stmt::Let { value, .. } | Stmt::Assign { value, .. } | Stmt::LetPattern { value, .. } | Stmt::Expr(value) | Stmt::Yield(value) => {
                collect_expr(value, path, control_nodes, operations, controls, expressions);
            }
            Stmt::Return(value) => {
                let value_expression = value.as_ref().map(|value| {
                    collect_expr(value, path, control_nodes, operations, controls, expressions)
                });
                push(operations, path, SourceAsyncOperationKind::Return, value_expression);
            }
            Stmt::Break | Stmt::Continue => {}
        }
    }
}

fn collect_expr(
    expression: &Expr,
    path: &mut Vec<ControlPathStep>,
    control_nodes: &mut Vec<SourceAsyncControl>,
    operations: &mut Vec<SourceAsyncOperation>,
    controls: &mut u32,
    expressions: &mut u32,
) -> SourceExpressionId {
    let expression_id = SourceExpressionId(*expressions);
    *expressions = expressions.checked_add(1).expect("async source expression count exceeds u32");
    match expression {
        Expr::Unary { op: witchy_syntax::ast::UnOp::Await, expr } => {
            collect_expr(expr, path, control_nodes, operations, controls, expressions);
            push(operations, path, SourceAsyncOperationKind::Await, Some(expression_id));
        }
        Expr::Call { args, .. } => {
            for arg in args { collect_expr(arg, path, control_nodes, operations, controls, expressions); }
            push(operations, path, SourceAsyncOperationKind::StaticCall, Some(expression_id));
        }
        Expr::LabeledCall { args, .. } => {
            for (_, arg) in args { collect_expr(arg, path, control_nodes, operations, controls, expressions); }
            push(operations, path, SourceAsyncOperationKind::StaticCall, Some(expression_id));
        }
        Expr::Apply { func, args } => {
            collect_expr(func, path, control_nodes, operations, controls, expressions);
            for arg in args { collect_expr(arg, path, control_nodes, operations, controls, expressions); }
            push(operations, path, SourceAsyncOperationKind::DynamicCall, Some(expression_id));
        }
        Expr::If { cond, then_block, else_block } => {
            let control = fresh_control(controls);
            control_nodes.push(SourceAsyncControl { id: control, path: path.clone(), kind: SourceControlKind::If });
            collect_expr(cond, path, control_nodes, operations, controls, expressions);
            path.push(ControlPathStep::IfThen(control));
            collect_block(then_block, path, control_nodes, operations, controls, expressions);
            path.pop();
            if let Some(else_block) = else_block {
                path.push(ControlPathStep::IfElse(control));
                collect_block(else_block, path, control_nodes, operations, controls, expressions);
                path.pop();
            }
        }
        Expr::Match { scrutinee, arms } => {
            let control = fresh_control(controls);
            control_nodes.push(SourceAsyncControl { id: control, path: path.clone(), kind: SourceControlKind::Match { arms: u32::try_from(arms.len()).expect("match arm count exceeds u32") } });
            collect_expr(scrutinee, path, control_nodes, operations, controls, expressions);
            for (arm, branch) in arms.iter().enumerate() {
                path.push(ControlPathStep::MatchArm { control, arm: u32::try_from(arm).expect("match arm count exceeds u32") });
                if let Some(guard) = &branch.guard { collect_expr(guard, path, control_nodes, operations, controls, expressions); }
                collect_expr(&branch.body, path, control_nodes, operations, controls, expressions);
                path.pop();
            }
        }
        Expr::While { cond, body } => {
            let control = fresh_control(controls);
            control_nodes.push(SourceAsyncControl { id: control, path: path.clone(), kind: SourceControlKind::While });
            collect_expr(cond, path, control_nodes, operations, controls, expressions);
            path.push(ControlPathStep::LoopBody(control));
            collect_block(body, path, control_nodes, operations, controls, expressions);
            path.pop();
        }
        Expr::For { iter, body, .. } => {
            let control = fresh_control(controls);
            control_nodes.push(SourceAsyncControl { id: control, path: path.clone(), kind: SourceControlKind::For });
            collect_expr(iter, path, control_nodes, operations, controls, expressions);
            path.push(ControlPathStep::LoopBody(control));
            collect_block(body, path, control_nodes, operations, controls, expressions);
            path.pop();
        }
        Expr::Block(block) => collect_block(block, path, control_nodes, operations, controls, expressions),
        Expr::Binary { lhs, rhs, .. } => { collect_expr(lhs, path, control_nodes, operations, controls, expressions); collect_expr(rhs, path, control_nodes, operations, controls, expressions); }
        Expr::Unary { expr, .. } | Expr::Try(expr) | Expr::As { expr, .. } => { collect_expr(expr, path, control_nodes, operations, controls, expressions); }
        Expr::List(values) | Expr::Tuple(values) => for value in values { collect_expr(value, path, control_nodes, operations, controls, expressions); },
        Expr::MethodCall { receiver, args, .. } => { collect_expr(receiver, path, control_nodes, operations, controls, expressions); for arg in args { collect_expr(arg, path, control_nodes, operations, controls, expressions); } push(operations, path, SourceAsyncOperationKind::StaticCall, Some(expression_id)); }
        Expr::LabeledMethodCall { receiver, args, .. } => {
            collect_expr(receiver, path, control_nodes, operations, controls, expressions);
            for (_, arg) in args { collect_expr(arg, path, control_nodes, operations, controls, expressions); }
            push(operations, path, SourceAsyncOperationKind::StaticCall, Some(expression_id));
        }
        Expr::ExistentialCall { receiver, args, .. } => {
            collect_expr(receiver, path, control_nodes, operations, controls, expressions);
            for arg in args { collect_expr(arg, path, control_nodes, operations, controls, expressions); }
            push(operations, path, SourceAsyncOperationKind::DynamicCall, Some(expression_id));
        }
        Expr::Ctor { args, .. } | Expr::AnonCtor { args, .. } => {
            for arg in args { collect_expr(arg, path, control_nodes, operations, controls, expressions); }
        }
        Expr::Field { base, .. } => { collect_expr(base, path, control_nodes, operations, controls, expressions); }
        Expr::RecordUpdate { base, fields, .. } => {
            collect_expr(base, path, control_nodes, operations, controls, expressions);
            for (_, value) in fields { collect_expr(value, path, control_nodes, operations, controls, expressions); }
        }
        Expr::Record { fields, spread, .. } => {
            for (_, value) in fields { collect_expr(value, path, control_nodes, operations, controls, expressions); }
            if let Some(spread) = spread { collect_expr(spread, path, control_nodes, operations, controls, expressions); }
        }
        Expr::ExistentialPack { expr, .. } | Expr::ExistentialUpcast { expr, .. } => {
            collect_expr(expr, path, control_nodes, operations, controls, expressions);
        }
        Expr::Range { lo, hi, .. } => { collect_expr(lo, path, control_nodes, operations, controls, expressions); collect_expr(hi, path, control_nodes, operations, controls, expressions); }
        Expr::Index { base, index } => { collect_expr(base, path, control_nodes, operations, controls, expressions); collect_expr(index, path, control_nodes, operations, controls, expressions); }
        Expr::WhileLet { scrutinee, body, .. } => {
            let control = fresh_control(controls);
            control_nodes.push(SourceAsyncControl { id: control, path: path.clone(), kind: SourceControlKind::WhileLet });
            collect_expr(scrutinee, path, control_nodes, operations, controls, expressions);
            path.push(ControlPathStep::LoopBody(control));
            collect_block(body, path, control_nodes, operations, controls, expressions);
            path.pop();
        }
        // A lambda has its own callable identity and is represented later as a
        // dynamic call frame. Traversing its body here would incorrectly assign
        // its operations to the enclosing async callable.
        Expr::Lambda { .. }
        | Expr::Int(_) | Expr::Float(_) | Expr::Duration(_) | Expr::Str(_) | Expr::Bool(_)
        | Expr::Var(_) | Expr::TaggedLit { .. } => {}
    }
    expression_id
}

/// Recover a source expression from the preserved async item using its lexical
/// coordinate. This is the only supported way for later typed lowering to
/// obtain operation operands: it walks the retained source syntax, never the
/// generated CPS projection.
pub fn source_expression(function: &Function, target: SourceExpressionId) -> Option<&Expr> {
    fn walk_block<'a>(block: &'a Block, target: SourceExpressionId, next: &mut u32) -> Option<&'a Expr> {
        for statement in &block.stmts {
            let value = match statement {
                Stmt::Let { value, .. } | Stmt::Assign { value, .. }
                | Stmt::LetPattern { value, .. } | Stmt::Expr(value) | Stmt::Yield(value) => Some(value),
                Stmt::Return(value) => value.as_ref(),
                Stmt::Break | Stmt::Continue => None,
            };
            if let Some(value) = value
                && let Some(found) = walk_expr(value, target, next)
            {
                return Some(found);
            }
        }
        None
    }

    fn walk_expr<'a>(expression: &'a Expr, target: SourceExpressionId, next: &mut u32) -> Option<&'a Expr> {
        let id = SourceExpressionId(*next);
        *next = next.checked_add(1).expect("async source expression count exceeds u32");
        if id == target {
            return Some(expression);
        }
        macro_rules! visit {
            ($child:expr) => {
                walk_expr($child, target, next)
            };
        }
        match expression {
            Expr::Unary { expr, .. } | Expr::Try(expr) | Expr::As { expr, .. }
            | Expr::Field { base: expr, .. } | Expr::ExistentialPack { expr, .. }
            | Expr::ExistentialUpcast { expr, .. } => visit!(expr),
            Expr::Call { args, .. } | Expr::Ctor { args, .. } | Expr::AnonCtor { args, .. }
            | Expr::List(args) | Expr::Tuple(args) => args.iter().find_map(|value| visit!(value)),
            Expr::LabeledCall { args, .. } => args.iter().find_map(|(_, value)| visit!(value)),
            Expr::Apply { func, args } => visit!(func).or_else(|| args.iter().find_map(|value| visit!(value))),
            Expr::Binary { lhs, rhs, .. } | Expr::Range { lo: lhs, hi: rhs, .. }
            | Expr::Index { base: lhs, index: rhs } => visit!(lhs).or_else(|| visit!(rhs)),
            Expr::If { cond, then_block, else_block } => visit!(cond)
                .or_else(|| walk_block(then_block, target, next))
                .or_else(|| else_block.as_ref().and_then(|else_body| walk_block(else_body, target, next))),
            Expr::Match { scrutinee, arms } => visit!(scrutinee).or_else(|| arms.iter().find_map(|arm| {
                arm.guard.as_ref().and_then(|guard| visit!(guard)).or_else(|| visit!(&arm.body))
            })),
            Expr::While { cond, body } => visit!(cond).or_else(|| walk_block(body, target, next)),
            Expr::For { iter, body, .. } => visit!(iter).or_else(|| walk_block(body, target, next)),
            Expr::Block(block_value) => walk_block(block_value, target, next),
            Expr::MethodCall { receiver, args, .. } | Expr::ExistentialCall { receiver, args, .. } => {
                visit!(receiver).or_else(|| args.iter().find_map(|value| visit!(value)))
            }
            Expr::LabeledMethodCall { receiver, args, .. } => {
                visit!(receiver).or_else(|| args.iter().find_map(|(_, value)| visit!(value)))
            }
            Expr::RecordUpdate { base, fields, .. } => {
                visit!(base).or_else(|| fields.iter().find_map(|(_, value)| visit!(value)))
            }
            Expr::Record { fields, spread, .. } => fields.iter().find_map(|(_, value)| visit!(value))
                .or_else(|| spread.as_ref().and_then(|value| visit!(value))),
            Expr::WhileLet { scrutinee, body, .. } => visit!(scrutinee).or_else(|| walk_block(body, target, next)),
            // Lambda bodies belong to their own eventual callable and are
            // intentionally not part of this callable's lexical coordinates.
            Expr::Lambda { .. } | Expr::Int(_) | Expr::Float(_) | Expr::Duration(_)
            | Expr::Str(_) | Expr::Bool(_) | Expr::Var(_) | Expr::TaggedLit { .. } => None,
        }
    }

    let mut next = 0;
    walk_block(&function.body, target, &mut next)
}

#[cfg(test)]
mod tests {
    use super::*;
    use witchy_syntax::parser::parse_module;

    #[test]
    fn assigns_distinct_control_ids_to_sequential_conditionals() {
        let module = parse_module(
            "async fn choose(first: Bool, second: Bool) -> Int:\n    if first:\n        task.done(1).await\n    if second:\n        task.done(2).await\n    3\n",
        )
        .expect("parse async source");
        let Item::Function(function) = &module.items[0] else {
            panic!("source function")
        };
        let (controls, operations) = collect_function(function);
        assert_eq!(controls.len(), 2);
        assert_eq!(controls[0].kind, SourceControlKind::If);
        assert_eq!(controls[1].kind, SourceControlKind::If);
        let awaits = operations
            .iter()
            .filter(|operation| operation.kind == SourceAsyncOperationKind::Await)
            .collect::<Vec<_>>();
        assert_eq!(awaits.len(), 2);
        assert_eq!(awaits[0].path, vec![ControlPathStep::IfThen(SourceControlId(0))]);
        assert_eq!(awaits[1].path, vec![ControlPathStep::IfThen(SourceControlId(1))]);
    }

    #[test]
    fn source_operation_locator_recovers_the_exact_pre_cps_operand() {
        let module = parse_module(
            "async fn run() -> Int:\n    let value = task.done(41).await\n    return value\n",
        )
        .expect("parse async source");
        let Item::Function(function) = &module.items[0] else {
            panic!("source function")
        };
        let (_, operations) = collect_function(function);
        let await_operation = operations
            .iter()
            .find(|operation| operation.kind == SourceAsyncOperationKind::Await)
            .expect("await operation");
        let call_operation = operations
            .iter()
            .find(|operation| operation.kind == SourceAsyncOperationKind::StaticCall)
            .expect("call operation");
        assert!(matches!(
            source_expression(function, await_operation.expression.expect("await expression")),
            Some(Expr::Unary { op: witchy_syntax::ast::UnOp::Await, .. })
        ));
        assert!(matches!(
            source_expression(function, call_operation.expression.expect("call expression")),
            Some(Expr::Call { name, .. }) if name == "task.done"
        ));
        let return_operation = operations
            .iter()
            .find(|operation| operation.kind == SourceAsyncOperationKind::Return)
            .expect("return operation");
        assert!(matches!(
            source_expression(function, return_operation.expression.expect("return value")),
            Some(Expr::Var(name)) if name == "value"
        ));
    }

    #[test]
    fn source_operation_coordinates_do_not_depend_on_local_spelling() {
        let parse = |source| {
            let module = parse_module(source).expect("parse async source");
            let Item::Function(function) = &module.items[0] else {
                panic!("source function")
            };
            collect_function(function).1.into_iter().map(|operation| {
                (operation.kind, operation.path, operation.expression)
            }).collect::<Vec<_>>()
        };
        let first = parse(
            "async fn run(seed: Int) -> Int:\n    let kept = task.done(seed).await\n    kept\n",
        );
        let renamed = parse(
            "async fn run(input: Int) -> Int:\n    let result = task.done(input).await\n    result\n",
        );
        assert_eq!(first, renamed);
    }
}
