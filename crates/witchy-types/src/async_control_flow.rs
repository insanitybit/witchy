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
    collect_block(&function.body, &mut Vec::new(), &mut control_nodes, &mut operations, &mut controls);
    for (ordinal, operation) in operations.iter_mut().enumerate() {
        operation.ordinal = u32::try_from(ordinal).expect("async operation count exceeds u32");
    }
    (control_nodes, operations)
}

fn push(operations: &mut Vec<SourceAsyncOperation>, path: &[ControlPathStep], kind: SourceAsyncOperationKind) {
    operations.push(SourceAsyncOperation { ordinal: 0, path: path.to_vec(), kind });
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
) {
    for statement in &block.stmts {
        match statement {
            Stmt::Let { value, .. } | Stmt::Assign { value, .. } | Stmt::LetPattern { value, .. } | Stmt::Expr(value) | Stmt::Yield(value) => collect_expr(value, path, control_nodes, operations, controls),
            Stmt::Return(value) => {
                if let Some(value) = value { collect_expr(value, path, control_nodes, operations, controls); }
                push(operations, path, SourceAsyncOperationKind::Return);
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
) {
    match expression {
        Expr::Unary { op: witchy_syntax::ast::UnOp::Await, expr } => {
            collect_expr(expr, path, control_nodes, operations, controls);
            push(operations, path, SourceAsyncOperationKind::Await);
        }
        Expr::Call { args, .. } => {
            for arg in args { collect_expr(arg, path, control_nodes, operations, controls); }
            push(operations, path, SourceAsyncOperationKind::StaticCall);
        }
        Expr::LabeledCall { args, .. } => {
            for (_, arg) in args { collect_expr(arg, path, control_nodes, operations, controls); }
            push(operations, path, SourceAsyncOperationKind::StaticCall);
        }
        Expr::Apply { func, args } => {
            collect_expr(func, path, control_nodes, operations, controls);
            for arg in args { collect_expr(arg, path, control_nodes, operations, controls); }
            push(operations, path, SourceAsyncOperationKind::DynamicCall);
        }
        Expr::If { cond, then_block, else_block } => {
            let control = fresh_control(controls);
            control_nodes.push(SourceAsyncControl { id: control, path: path.clone(), kind: SourceControlKind::If });
            collect_expr(cond, path, control_nodes, operations, controls);
            path.push(ControlPathStep::IfThen(control));
            collect_block(then_block, path, control_nodes, operations, controls);
            path.pop();
            if let Some(else_block) = else_block {
                path.push(ControlPathStep::IfElse(control));
                collect_block(else_block, path, control_nodes, operations, controls);
                path.pop();
            }
        }
        Expr::Match { scrutinee, arms } => {
            let control = fresh_control(controls);
            control_nodes.push(SourceAsyncControl { id: control, path: path.clone(), kind: SourceControlKind::Match { arms: u32::try_from(arms.len()).expect("match arm count exceeds u32") } });
            collect_expr(scrutinee, path, control_nodes, operations, controls);
            for (arm, branch) in arms.iter().enumerate() {
                path.push(ControlPathStep::MatchArm { control, arm: u32::try_from(arm).expect("match arm count exceeds u32") });
                if let Some(guard) = &branch.guard { collect_expr(guard, path, control_nodes, operations, controls); }
                collect_expr(&branch.body, path, control_nodes, operations, controls);
                path.pop();
            }
        }
        Expr::While { cond, body } => {
            let control = fresh_control(controls);
            control_nodes.push(SourceAsyncControl { id: control, path: path.clone(), kind: SourceControlKind::While });
            collect_expr(cond, path, control_nodes, operations, controls);
            path.push(ControlPathStep::LoopBody(control));
            collect_block(body, path, control_nodes, operations, controls);
            path.pop();
        }
        Expr::For { iter, body, .. } => {
            let control = fresh_control(controls);
            control_nodes.push(SourceAsyncControl { id: control, path: path.clone(), kind: SourceControlKind::For });
            collect_expr(iter, path, control_nodes, operations, controls);
            path.push(ControlPathStep::LoopBody(control));
            collect_block(body, path, control_nodes, operations, controls);
            path.pop();
        }
        Expr::Block(block) => collect_block(block, path, control_nodes, operations, controls),
        Expr::Binary { lhs, rhs, .. } => { collect_expr(lhs, path, control_nodes, operations, controls); collect_expr(rhs, path, control_nodes, operations, controls); }
        Expr::Unary { expr, .. } | Expr::Try(expr) | Expr::As { expr, .. } => collect_expr(expr, path, control_nodes, operations, controls),
        Expr::List(values) | Expr::Tuple(values) => for value in values { collect_expr(value, path, control_nodes, operations, controls); },
        Expr::MethodCall { receiver, args, .. } => { collect_expr(receiver, path, control_nodes, operations, controls); for arg in args { collect_expr(arg, path, control_nodes, operations, controls); } push(operations, path, SourceAsyncOperationKind::StaticCall); }
        Expr::LabeledMethodCall { receiver, args, .. } => {
            collect_expr(receiver, path, control_nodes, operations, controls);
            for (_, arg) in args { collect_expr(arg, path, control_nodes, operations, controls); }
            push(operations, path, SourceAsyncOperationKind::StaticCall);
        }
        Expr::ExistentialCall { receiver, args, .. } => {
            collect_expr(receiver, path, control_nodes, operations, controls);
            for arg in args { collect_expr(arg, path, control_nodes, operations, controls); }
            push(operations, path, SourceAsyncOperationKind::DynamicCall);
        }
        Expr::Ctor { args, .. } | Expr::AnonCtor { args, .. } => {
            for arg in args { collect_expr(arg, path, control_nodes, operations, controls); }
        }
        Expr::Field { base, .. } => collect_expr(base, path, control_nodes, operations, controls),
        Expr::RecordUpdate { base, fields, .. } => {
            collect_expr(base, path, control_nodes, operations, controls);
            for (_, value) in fields { collect_expr(value, path, control_nodes, operations, controls); }
        }
        Expr::Record { fields, spread, .. } => {
            for (_, value) in fields { collect_expr(value, path, control_nodes, operations, controls); }
            if let Some(spread) = spread { collect_expr(spread, path, control_nodes, operations, controls); }
        }
        Expr::ExistentialPack { expr, .. } | Expr::ExistentialUpcast { expr, .. } => {
            collect_expr(expr, path, control_nodes, operations, controls);
        }
        Expr::Range { lo, hi, .. } => { collect_expr(lo, path, control_nodes, operations, controls); collect_expr(hi, path, control_nodes, operations, controls); }
        Expr::Index { base, index } => { collect_expr(base, path, control_nodes, operations, controls); collect_expr(index, path, control_nodes, operations, controls); }
        Expr::WhileLet { scrutinee, body, .. } => {
            let control = fresh_control(controls);
            control_nodes.push(SourceAsyncControl { id: control, path: path.clone(), kind: SourceControlKind::WhileLet });
            collect_expr(scrutinee, path, control_nodes, operations, controls);
            path.push(ControlPathStep::LoopBody(control));
            collect_block(body, path, control_nodes, operations, controls);
            path.pop();
        }
        // A lambda has its own callable identity and is represented later as a
        // dynamic call frame. Traversing its body here would incorrectly assign
        // its operations to the enclosing async callable.
        Expr::Lambda { .. }
        | Expr::Int(_) | Expr::Float(_) | Expr::Duration(_) | Expr::Str(_) | Expr::Bool(_)
        | Expr::Var(_) | Expr::TaggedLit { .. } => {}
    }
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
}
