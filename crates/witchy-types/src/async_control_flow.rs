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

/// A nesting step that distinguishes sequential and mutually-exclusive paths.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ControlPathStep {
    Then,
    Else,
    MatchArm(u32),
    LoopBody,
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
                callables.push(SourceAsyncControlFlow {
                    callable: AsyncCallableId { module: module.module.clone(), surface: item.id.clone() },
                    operations: collect_function(function),
                });
            }
        }
        callables.sort_by(|left, right| left.callable.cmp(&right.callable));
        Self { callables }
    }
}

fn collect_function(function: &Function) -> Vec<SourceAsyncOperation> {
    let mut operations = Vec::new();
    collect_block(&function.body, &mut Vec::new(), &mut operations);
    for (ordinal, operation) in operations.iter_mut().enumerate() {
        operation.ordinal = u32::try_from(ordinal).expect("async operation count exceeds u32");
    }
    operations
}

fn push(operations: &mut Vec<SourceAsyncOperation>, path: &[ControlPathStep], kind: SourceAsyncOperationKind) {
    operations.push(SourceAsyncOperation { ordinal: 0, path: path.to_vec(), kind });
}

fn collect_block(block: &Block, path: &mut Vec<ControlPathStep>, operations: &mut Vec<SourceAsyncOperation>) {
    for statement in &block.stmts {
        match statement {
            Stmt::Let { value, .. } | Stmt::Assign { value, .. } | Stmt::LetPattern { value, .. } | Stmt::Expr(value) | Stmt::Yield(value) => collect_expr(value, path, operations),
            Stmt::Return(value) => {
                if let Some(value) = value { collect_expr(value, path, operations); }
                push(operations, path, SourceAsyncOperationKind::Return);
            }
            Stmt::Break | Stmt::Continue => {}
        }
    }
}

fn collect_expr(expression: &Expr, path: &mut Vec<ControlPathStep>, operations: &mut Vec<SourceAsyncOperation>) {
    match expression {
        Expr::Unary { op: witchy_syntax::ast::UnOp::Await, expr } => {
            collect_expr(expr, path, operations);
            push(operations, path, SourceAsyncOperationKind::Await);
        }
        Expr::Call { args, .. } => {
            for arg in args { collect_expr(arg, path, operations); }
            push(operations, path, SourceAsyncOperationKind::StaticCall);
        }
        Expr::LabeledCall { args, .. } => {
            for (_, arg) in args { collect_expr(arg, path, operations); }
            push(operations, path, SourceAsyncOperationKind::StaticCall);
        }
        Expr::Apply { func, args } => {
            collect_expr(func, path, operations);
            for arg in args { collect_expr(arg, path, operations); }
            push(operations, path, SourceAsyncOperationKind::DynamicCall);
        }
        Expr::If { cond, then_block, else_block } => {
            collect_expr(cond, path, operations);
            path.push(ControlPathStep::Then);
            collect_block(then_block, path, operations);
            path.pop();
            if let Some(else_block) = else_block {
                path.push(ControlPathStep::Else);
                collect_block(else_block, path, operations);
                path.pop();
            }
        }
        Expr::Match { scrutinee, arms } => {
            collect_expr(scrutinee, path, operations);
            for (arm, branch) in arms.iter().enumerate() {
                path.push(ControlPathStep::MatchArm(u32::try_from(arm).expect("match arm count exceeds u32")));
                if let Some(guard) = &branch.guard { collect_expr(guard, path, operations); }
                collect_expr(&branch.body, path, operations);
                path.pop();
            }
        }
        Expr::While { cond, body } => {
            collect_expr(cond, path, operations);
            path.push(ControlPathStep::LoopBody);
            collect_block(body, path, operations);
            path.pop();
        }
        Expr::For { iter, body, .. } => {
            collect_expr(iter, path, operations);
            path.push(ControlPathStep::LoopBody);
            collect_block(body, path, operations);
            path.pop();
        }
        Expr::Block(block) => collect_block(block, path, operations),
        Expr::Binary { lhs, rhs, .. } => { collect_expr(lhs, path, operations); collect_expr(rhs, path, operations); }
        Expr::Unary { expr, .. } | Expr::Try(expr) | Expr::As { expr, .. } => collect_expr(expr, path, operations),
        Expr::List(values) | Expr::Tuple(values) => for value in values { collect_expr(value, path, operations); },
        Expr::MethodCall { receiver, args, .. } => { collect_expr(receiver, path, operations); for arg in args { collect_expr(arg, path, operations); } push(operations, path, SourceAsyncOperationKind::StaticCall); }
        Expr::LabeledMethodCall { receiver, args, .. } => {
            collect_expr(receiver, path, operations);
            for (_, arg) in args { collect_expr(arg, path, operations); }
            push(operations, path, SourceAsyncOperationKind::StaticCall);
        }
        Expr::ExistentialCall { receiver, args, .. } => {
            collect_expr(receiver, path, operations);
            for arg in args { collect_expr(arg, path, operations); }
            push(operations, path, SourceAsyncOperationKind::DynamicCall);
        }
        Expr::Ctor { args, .. } | Expr::AnonCtor { args, .. } => {
            for arg in args { collect_expr(arg, path, operations); }
        }
        Expr::Field { base, .. } => collect_expr(base, path, operations),
        Expr::RecordUpdate { base, fields, .. } => {
            collect_expr(base, path, operations);
            for (_, value) in fields { collect_expr(value, path, operations); }
        }
        Expr::Record { fields, spread, .. } => {
            for (_, value) in fields { collect_expr(value, path, operations); }
            if let Some(spread) = spread { collect_expr(spread, path, operations); }
        }
        Expr::ExistentialPack { expr, .. } | Expr::ExistentialUpcast { expr, .. } => {
            collect_expr(expr, path, operations);
        }
        Expr::Range { lo, hi, .. } => { collect_expr(lo, path, operations); collect_expr(hi, path, operations); }
        Expr::Index { base, index } => { collect_expr(base, path, operations); collect_expr(index, path, operations); }
        Expr::WhileLet { scrutinee, body, .. } => {
            collect_expr(scrutinee, path, operations);
            path.push(ControlPathStep::LoopBody);
            collect_block(body, path, operations);
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
