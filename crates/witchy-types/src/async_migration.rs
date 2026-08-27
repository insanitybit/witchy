//! Type-resolved RFC-0161 async migration census.
//!
//! This deliberately visits the linked, trait-lowered module rather than
//! matching source text.  The report is the stable input to the one-cut source
//! migration: it identifies old task/channel semantics without guessing how a
//! caller should be rewritten.

use std::collections::BTreeMap;

use witchy_syntax::ast::{Block, Expr, Item, Module, Stmt, UnOp};

/// A changed RFC-0129 surface use that requires an RFC-0161 migration decision.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Category {
    AsyncCallable,
    TaskConstruction,
    TaskConsumption,
    LegacyHandle,
    LegacyCancellation,
    LegacySelect,
    LegacyChannelUnbounded,
    LegacyChannelBounded,
    LegacyChannelDynamicCapacity,
    LegacyReceiveQuiescence,
}

impl Category {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AsyncCallable => "async-callable",
            Self::TaskConstruction => "task-construction",
            Self::TaskConsumption => "task-consumption",
            Self::LegacyHandle => "legacy-handle",
            Self::LegacyCancellation => "legacy-cancellation",
            Self::LegacySelect => "legacy-select",
            Self::LegacyChannelUnbounded => "legacy-channel-unbounded",
            Self::LegacyChannelBounded => "legacy-channel-bounded",
            Self::LegacyChannelDynamicCapacity => "legacy-channel-dynamic-capacity",
            Self::LegacyReceiveQuiescence => "legacy-receive-quiescence",
        }
    }
}

/// One source-facing finding, after import resolution and trait lowering.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Finding {
    pub category: Category,
    pub function: String,
    pub line: u32,
    pub callee: String,
}

/// Complete report for one linked entry module.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Census {
    pub checked: bool,
    pub check_error: Option<String>,
    pub findings: Vec<Finding>,
}

impl Census {
    pub fn counts(&self) -> BTreeMap<&'static str, usize> {
        let mut counts = BTreeMap::new();
        for category in [
            Category::AsyncCallable,
            Category::TaskConstruction,
            Category::TaskConsumption,
            Category::LegacyHandle,
            Category::LegacyCancellation,
            Category::LegacySelect,
            Category::LegacyChannelUnbounded,
            Category::LegacyChannelBounded,
            Category::LegacyChannelDynamicCapacity,
            Category::LegacyReceiveQuiescence,
        ] {
            counts.insert(category.as_str(), self.findings.iter().filter(|finding| finding.category == category).count());
        }
        counts
    }
}

/// Census all resolved changed-concurrency calls owned by `entry_module`.
///
/// The checker result is retained even when legacy source does not type-check,
/// so the migration command can report all actionable callers in one run.
pub fn census_linked(module: &Module, entry_module: &str) -> Census {
    let check_error = crate::typeck::check(module).err().map(|error| error.to_string());
    let lowered = crate::traits::lower(module.clone());
    let mut census = Census { checked: check_error.is_none(), check_error, findings: Vec::new() };
    for item in &lowered.items {
        let Item::Function(function) = item else { continue };
        if !belongs_to_entry(&function.name, entry_module) { continue; }
        if function.is_async {
            census.findings.push(Finding {
                category: Category::AsyncCallable,
                function: function.name.clone(),
                line: function.line,
                callee: function.name.clone(),
            });
        }
        walk_block(&function.body, &function.name, &mut census);
    }
    census.findings.sort();
    census
}

fn belongs_to_entry(function: &str, entry: &str) -> bool {
    function == "main" || function.starts_with("main::<") || function.starts_with(&format!("{entry}."))
}

fn report(census: &mut Census, category: Category, function: &str, line: u32, callee: &str) {
    census.findings.push(Finding { category, function: function.to_string(), line, callee: callee.to_string() });
}

fn walk_block(block: &Block, function: &str, census: &mut Census) {
    for (index, statement) in block.stmts.iter().enumerate() {
        let line = block.lines.get(index).copied().unwrap_or(0);
        match statement {
            Stmt::Let { value, .. } | Stmt::Assign { value, .. } | Stmt::LetPattern { value, .. }
            | Stmt::Return(Some(value)) | Stmt::Yield(value) | Stmt::Expr(value) => walk_expr(value, function, line, census),
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
    }
}

fn walk_expr(expr: &Expr, function: &str, line: u32, census: &mut Census) {
    match expr {
        Expr::Call { name, args } => {
            classify_call(name, args, function, line, census);
            for arg in args { walk_expr(arg, function, line, census); }
        }
        Expr::LabeledCall { name, args } => {
            // Labeled calls have the same resolved callee identity. None of
            // the legacy constructors takes a labeled capacity argument, but
            // their argument expressions still need their own census visit.
            classify_call(name, &[], function, line, census);
            for (_, arg) in args { walk_expr(arg, function, line, census); }
        }
        Expr::MethodCall { receiver, args, .. } | Expr::ExistentialCall { receiver, args, .. } => {
            walk_expr(receiver, function, line, census);
            for arg in args { walk_expr(arg, function, line, census); }
        }
        Expr::LabeledMethodCall { receiver, args, .. } => {
            walk_expr(receiver, function, line, census);
            for (_, arg) in args { walk_expr(arg, function, line, census); }
        }
        Expr::Apply { func, args } => {
            walk_expr(func, function, line, census);
            for arg in args { walk_expr(arg, function, line, census); }
        }
        Expr::Unary { op: UnOp::Await, expr } => {
            report(census, Category::TaskConsumption, function, line, "await");
            walk_expr(expr, function, line, census);
        }
        Expr::Unary { expr, .. } | Expr::Try(expr) | Expr::As { expr, .. }
        | Expr::ExistentialPack { expr, .. } | Expr::ExistentialUpcast { expr, .. } => walk_expr(expr, function, line, census),
        Expr::List(values) | Expr::Tuple(values) => for value in values { walk_expr(value, function, line, census); },
        Expr::Ctor { args, .. } | Expr::AnonCtor { args, .. } => for arg in args { walk_expr(arg, function, line, census); },
        Expr::Field { base, .. } => walk_expr(base, function, line, census),
        Expr::RecordUpdate { base, fields, .. } => {
            walk_expr(base, function, line, census);
            for (_, value) in fields { walk_expr(value, function, line, census); }
        }
        Expr::Record { fields, spread, .. } => {
            for (_, value) in fields { walk_expr(value, function, line, census); }
            if let Some(value) = spread { walk_expr(value, function, line, census); }
        }
        Expr::Binary { lhs, rhs, .. } | Expr::Range { lo: lhs, hi: rhs, .. } | Expr::Index { base: lhs, index: rhs } => {
            walk_expr(lhs, function, line, census);
            walk_expr(rhs, function, line, census);
        }
        Expr::If { cond, then_block, else_block } => {
            walk_expr(cond, function, line, census);
            walk_block(then_block, function, census);
            if let Some(block) = else_block { walk_block(block, function, census); }
        }
        Expr::Match { scrutinee, arms } => {
            walk_expr(scrutinee, function, line, census);
            for arm in arms {
                if let Some(guard) = &arm.guard { walk_expr(guard, function, arm.line, census); }
                walk_expr(&arm.body, function, arm.line, census);
            }
        }
        Expr::Block(block) | Expr::While { body: block, .. } | Expr::For { body: block, .. }
        | Expr::WhileLet { body: block, .. } | Expr::Lambda { body: block, .. } => walk_block(block, function, census),
        Expr::Int(_) | Expr::Float(_) | Expr::Duration(_) | Expr::Str(_) | Expr::Bool(_) | Expr::Var(_) | Expr::TaggedLit { .. } => {}
    }
}

fn classify_call(name: &str, args: &[Expr], function: &str, line: u32, census: &mut Census) {
    match name {
        "task.done" | "task.ready_unit" | "task.lazy" | "task.and_then" | "task.map" | "task.for_each" | "chan.done" | "chan.yield_now" => report(census, Category::TaskConstruction, function, line, name),
        "task.run" => report(census, Category::TaskConsumption, function, line, name),
        "chan.spawn" | "task.spawn" | "chan.join" | "task.join" => report(census, Category::LegacyHandle, function, line, name),
        "chan.cancel" | "task.cancel" => report(census, Category::LegacyCancellation, function, line, name),
        "chan.select" | "chan.select3" | "chan.select4" | "chan.select_any" => report(census, Category::LegacySelect, function, line, name),
        "chan.recv" => report(census, Category::LegacyReceiveQuiescence, function, line, name),
        "chan.channel" => match args.first() {
            Some(Expr::Int(value)) if *value <= 0 => report(census, Category::LegacyChannelUnbounded, function, line, name),
            Some(Expr::Int(_)) => report(census, Category::LegacyChannelBounded, function, line, name),
            _ => report(census, Category::LegacyChannelDynamicCapacity, function, line, name),
        },
        "chan.unbounded" => report(census, Category::LegacyChannelUnbounded, function, line, name),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use witchy_syntax::parser::parse_module;

    #[test]
    fn reports_changed_async_surfaces_by_resolved_callee_and_capacity_class() {
        let source = "async fn main():\n    let (tx, rx) = chan.channel(4).await\n    let _ = chan.channel(0).await\n    let cap = 1\n    let _ = chan.channel(cap).await\n    let _ = chan.recv(rx).await\n    let child = chan.spawn(task.done(unit())).await\n    chan.cancel(child).await\n";
        let module = parse_module(source).expect("parse migration source");
        let census = census_linked(&module, "main");
        let counts = census.counts();
        assert_eq!(counts["async-callable"], 1);
        assert_eq!(counts["legacy-channel-bounded"], 1);
        assert_eq!(counts["legacy-channel-unbounded"], 1);
        assert_eq!(counts["legacy-channel-dynamic-capacity"], 1);
        assert_eq!(counts["legacy-handle"], 1);
        assert_eq!(counts["legacy-cancellation"], 1);
        assert_eq!(counts["legacy-receive-quiescence"], 1);
        assert_eq!(counts["task-consumption"], 6);
    }
}
