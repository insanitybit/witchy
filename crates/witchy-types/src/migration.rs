//! RFC-0087 migration census over resolved `var` calls.
//!
//! This is deliberately an AST/type-system facility, not a source-text search.
//! Call names come from trait/method lowering and parameter conventions come
//! from the resolved declaration in that same lowered module.

use std::collections::{BTreeMap, BTreeSet};

use witchy_syntax::{
    ast::{self, Block, Convention, Expr, Item, Module, Param, Pattern, Stmt, Type},
    intrinsics,
};

/// A source use that needs attention during the RFC-0087 one-cut migration.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Category {
    MechanicalSelfReassignment,
    ExpressionPositionMutator,
    ImmutableVarArgument,
    TemporaryVarArgument,
    AuxiliaryResultDiscard,
}

impl Category {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MechanicalSelfReassignment => "mechanical-self-reassignment",
            Self::ExpressionPositionMutator => "expression-position-mutator",
            Self::ImmutableVarArgument => "immutable-var-argument",
            Self::TemporaryVarArgument => "temporary-var-argument",
            Self::AuxiliaryResultDiscard => "auxiliary-result-discard",
        }
    }
}

/// One stable, source-facing census finding.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Finding {
    pub category: Category,
    pub function: String,
    pub line: u32,
    pub callee: String,
    /// One-based parameter position, when the finding concerns one argument.
    pub parameter: Option<usize>,
    pub parameter_name: Option<String>,
    pub root: Option<String>,
}

/// Counts and findings for one linked entry module.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Census {
    pub checked: bool,
    pub check_error: Option<String>,
    pub var_declarations: usize,
    pub resolved_var_calls: usize,
    pub findings: Vec<Finding>,
}

impl Census {
    pub fn counts(&self) -> BTreeMap<&'static str, usize> {
        let mut counts = BTreeMap::new();
        for category in [
            Category::MechanicalSelfReassignment,
            Category::ExpressionPositionMutator,
            Category::ImmutableVarArgument,
            Category::TemporaryVarArgument,
            Category::AuxiliaryResultDiscard,
        ] {
            counts.insert(
                category.as_str(),
                self.findings.iter().filter(|f| f.category == category).count(),
            );
        }
        counts
    }
}

#[derive(Clone)]
struct Signature {
    params: Vec<Param>,
    returns_nil: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Position {
    Statement,
    AssignmentRhs,
    Other,
}

/// Classify RFC-0087 migration uses in one linked module.
///
/// `entry_module` is the linker stem for the source being counted. Imported
/// functions are retained as callees but their bodies are not counted, which
/// lets a driver visit every corpus source exactly once without multiplying
/// standard-library findings.
pub fn census_linked(module: &Module, entry_module: &str) -> Census {
    let check_error = crate::typeck::check(module).err().map(|e| e.to_string());
    // The unchecked flavor still performs the same declaration/type-directed
    // method resolution. Keeping its output when the checker rejects a legacy
    // use is what lets the migration tool explain that use instead of stopping
    // at the first diagnostic.
    let lowered = crate::traits::lower(module.clone());
    let signatures = signatures(&lowered);
    let var_declarations = lowered
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Function(function) if belongs_to_entry(&function.name, entry_module) => {
                Some(function)
            }
            _ => None,
        })
        .filter(|function| {
            function
                .params
                .iter()
                .any(|param| param.convention == Convention::Var)
        })
        .count();

    let mut census = Census {
        checked: check_error.is_none(),
        check_error,
        var_declarations,
        ..Census::default()
    };

    for item in &lowered.items {
        let Item::Function(function) = item else { continue };
        if !belongs_to_entry(&function.name, entry_module) {
            continue;
        }
        let mut mutable = BTreeSet::new();
        for param in &function.params {
            if param.convention.binds_mutable() {
                mutable.insert(param.name.clone());
            }
        }
        walk_block(
            &function.body,
            &function.name,
            &signatures,
            &mut mutable,
            &mut census,
        );
    }
    census.findings.sort();
    census
}

fn belongs_to_entry(function: &str, entry: &str) -> bool {
    function == "main"
        || function.starts_with("main::<")
        || function.starts_with(&format!("{entry}."))
}

fn signatures(module: &Module) -> BTreeMap<String, Signature> {
    module
        .items
        .iter()
        .filter_map(|item| {
            let Item::Function(function) = item else { return None };
            let returns_nil = function.ret.as_ref().is_none_or(|ty| {
                matches!(ty.unqualified(), Type::Named(name, _) if name == "Nil")
            });
            Some((
                function.name.clone(),
                Signature {
                    params: function.params.clone(),
                    returns_nil,
                },
            ))
        })
        .collect()
}

fn walk_block(
    block: &Block,
    function: &str,
    signatures: &BTreeMap<String, Signature>,
    mutable: &mut BTreeSet<String>,
    census: &mut Census,
) {
    for (index, stmt) in block.stmts.iter().enumerate() {
        let line = block.lines.get(index).copied().unwrap_or(0);
        match stmt {
            Stmt::Let { name, mutable: is_mutable, value, .. } => {
                walk_expr(
                    value,
                    function,
                    line,
                    Position::Other,
                    signatures,
                    mutable,
                    census,
                );
                if *is_mutable {
                    mutable.insert(name.clone());
                } else {
                    mutable.remove(name);
                }
            }
            Stmt::Assign { name, value } => {
                let mechanical = direct_var_call(value, signatures).is_some_and(|(_, args, sig)| {
                    sig.params
                        .iter()
                        .zip(args)
                        .any(|(param, arg)| param.convention == Convention::Var
                            && place_root(arg) == Some(name.as_str()))
                });
                if mechanical
                    && let Some((callee, _, _)) = direct_var_call(value, signatures)
                {
                    census.findings.push(Finding {
                        category: Category::MechanicalSelfReassignment,
                        function: function.to_string(),
                        line,
                        callee: callee.to_string(),
                        parameter: None,
                        parameter_name: None,
                        root: Some(name.clone()),
                    });
                }
                if let Some(callee) = discarded_intrinsic_callee(value) {
                    census.resolved_var_calls += 1;
                    if signatures.get(callee).is_some_and(|sig| !sig.returns_nil) {
                        census.findings.push(Finding {
                            category: Category::AuxiliaryResultDiscard,
                            function: function.to_string(),
                            line,
                            callee: callee.to_string(),
                            parameter: None,
                            parameter_name: None,
                            root: Some(name.clone()),
                        });
                    }
                }
                walk_expr(
                    value,
                    function,
                    line,
                    if mechanical { Position::AssignmentRhs } else { Position::Other },
                    signatures,
                    mutable,
                    census,
                );
            }
            Stmt::LetPattern { pattern, value } => {
                walk_expr(
                    value,
                    function,
                    line,
                    Position::Other,
                    signatures,
                    mutable,
                    census,
                );
                let mut names = Vec::new();
                ast::pattern_binds(pattern, &mut names);
                for name in names {
                    mutable.remove(&name);
                }
            }
            Stmt::Return(Some(value)) | Stmt::Yield(value) => walk_expr(
                value,
                function,
                line,
                Position::Other,
                signatures,
                mutable,
                census,
            ),
            Stmt::Expr(value) => {
                if let Some(callee) = discarded_intrinsic_callee(value)
                    && matches!(callee, "dict.insert" | "dict.remove")
                {
                    census.findings.push(Finding {
                        category: Category::AuxiliaryResultDiscard,
                        function: function.to_string(),
                        line,
                        callee: callee.to_string(),
                        parameter: None,
                        parameter_name: None,
                        root: place_root(value).map(str::to_string),
                    });
                }
                walk_expr(
                    value,
                    function,
                    line,
                    Position::Statement,
                    signatures,
                    mutable,
                    census,
                );
            }
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
    }
}

fn walk_expr(
    expr: &Expr,
    function: &str,
    line: u32,
    position: Position,
    signatures: &BTreeMap<String, Signature>,
    mutable: &BTreeSet<String>,
    census: &mut Census,
) {
    if let Some((callee, args, signature)) = direct_var_call(expr, signatures) {
        census.resolved_var_calls += 1;
        if position == Position::Other {
            census.findings.push(Finding {
                category: Category::ExpressionPositionMutator,
                function: function.to_string(),
                line,
                callee: callee.to_string(),
                parameter: None,
                parameter_name: None,
                root: None,
            });
        }
        if position == Position::Statement && !signature.returns_nil {
            census.findings.push(Finding {
                category: Category::AuxiliaryResultDiscard,
                function: function.to_string(),
                line,
                callee: callee.to_string(),
                parameter: None,
                parameter_name: None,
                root: None,
            });
        }
        for (index, (param, arg)) in signature.params.iter().zip(args).enumerate() {
            if param.convention != Convention::Var {
                continue;
            }
            match place_root(arg) {
                None => census.findings.push(Finding {
                    category: Category::TemporaryVarArgument,
                    function: function.to_string(),
                    line,
                    callee: callee.to_string(),
                    parameter: Some(index + 1),
                    parameter_name: Some(param.name.clone()),
                    root: None,
                }),
                Some(root) if !mutable.contains(root) => census.findings.push(Finding {
                    category: Category::ImmutableVarArgument,
                    function: function.to_string(),
                    line,
                    callee: callee.to_string(),
                    parameter: Some(index + 1),
                    parameter_name: Some(param.name.clone()),
                    root: Some(root.to_string()),
                }),
                Some(_) => {}
            }
        }
    }

    let nested_position = Position::Other;
    match expr {
        Expr::Int(_)
        | Expr::Float(_)
        | Expr::Duration(_)
        | Expr::Str(_)
        | Expr::Bool(_)
        | Expr::Var(_)
        | Expr::TaggedLit { .. } => {}
        Expr::List(values) | Expr::Tuple(values) => {
            for value in values {
                walk_expr(value, function, line, nested_position, signatures, mutable, census);
            }
        }
        Expr::Call { args, .. } | Expr::Ctor { args, .. } | Expr::AnonCtor { args, .. } => {
            for arg in args {
                walk_expr(arg, function, line, nested_position, signatures, mutable, census);
            }
        }
        Expr::LabeledCall { args, .. } => {
            for (_, arg) in args {
                walk_expr(arg, function, line, nested_position, signatures, mutable, census);
            }
        }
        Expr::MethodCall { receiver, args, .. } => {
            walk_expr(receiver, function, line, nested_position, signatures, mutable, census);
            for arg in args {
                walk_expr(arg, function, line, nested_position, signatures, mutable, census);
            }
        }
        Expr::Apply { func, args } => {
            walk_expr(func, function, line, nested_position, signatures, mutable, census);
            for arg in args {
                walk_expr(arg, function, line, nested_position, signatures, mutable, census);
            }
        }
        Expr::Unary { expr, .. }
        | Expr::Try(expr)
        | Expr::As { expr, .. }
        | Expr::ExistentialPack { expr, .. } => {
            walk_expr(expr, function, line, nested_position, signatures, mutable, census);
        }
        Expr::Field { base, .. } => {
            walk_expr(base, function, line, nested_position, signatures, mutable, census);
        }
        Expr::RecordUpdate { base, fields, .. } => {
            walk_expr(base, function, line, nested_position, signatures, mutable, census);
            for (_, value) in fields {
                walk_expr(value, function, line, nested_position, signatures, mutable, census);
            }
        }
        Expr::Record { fields, spread, .. } => {
            for (_, value) in fields {
                walk_expr(value, function, line, nested_position, signatures, mutable, census);
            }
            if let Some(spread) = spread {
                walk_expr(spread, function, line, nested_position, signatures, mutable, census);
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            walk_expr(lhs, function, line, nested_position, signatures, mutable, census);
            walk_expr(rhs, function, line, nested_position, signatures, mutable, census);
        }
        Expr::If { cond, then_block, else_block } => {
            walk_expr(cond, function, line, nested_position, signatures, mutable, census);
            let mut then_mutable = mutable.clone();
            walk_block(then_block, function, signatures, &mut then_mutable, census);
            if let Some(block) = else_block {
                let mut else_mutable = mutable.clone();
                walk_block(block, function, signatures, &mut else_mutable, census);
            }
        }
        Expr::Match { scrutinee, arms } => {
            walk_expr(scrutinee, function, line, nested_position, signatures, mutable, census);
            for arm in arms {
                let mut arm_mutable = mutable.clone();
                bind_pattern_immutable(&arm.pattern, &mut arm_mutable);
                if let Some(guard) = &arm.guard {
                    walk_expr(guard, function, arm.line, nested_position, signatures, &arm_mutable, census);
                }
                walk_expr(&arm.body, function, arm.line, nested_position, signatures, &arm_mutable, census);
            }
        }
        Expr::Block(block) => {
            let mut nested = mutable.clone();
            walk_block(block, function, signatures, &mut nested, census);
        }
        Expr::While { cond, body } => {
            walk_expr(cond, function, line, nested_position, signatures, mutable, census);
            let mut nested = mutable.clone();
            walk_block(body, function, signatures, &mut nested, census);
        }
        Expr::For { var, iter, body } => {
            walk_expr(iter, function, line, nested_position, signatures, mutable, census);
            let mut nested = mutable.clone();
            nested.remove(var);
            walk_block(body, function, signatures, &mut nested, census);
        }
        Expr::WhileLet { pattern, scrutinee, body } => {
            walk_expr(scrutinee, function, line, nested_position, signatures, mutable, census);
            let mut nested = mutable.clone();
            bind_pattern_immutable(pattern, &mut nested);
            walk_block(body, function, signatures, &mut nested, census);
        }
        Expr::Range { lo, hi, .. } => {
            walk_expr(lo, function, line, nested_position, signatures, mutable, census);
            walk_expr(hi, function, line, nested_position, signatures, mutable, census);
        }
        Expr::Index { base, index } => {
            walk_expr(base, function, line, nested_position, signatures, mutable, census);
            walk_expr(index, function, line, nested_position, signatures, mutable, census);
        }
        Expr::Lambda { params, body, .. } => {
            let mut nested = mutable.clone();
            for param in params {
                if param.convention.binds_mutable() {
                    nested.insert(param.name.clone());
                } else {
                    nested.remove(&param.name);
                }
            }
            walk_block(body, function, signatures, &mut nested, census);
        }
    }
}

fn direct_var_call<'a>(
    expr: &'a Expr,
    signatures: &'a BTreeMap<String, Signature>,
) -> Option<(&'a str, &'a [Expr], &'a Signature)> {
    let Expr::Call { name, args } = expr else { return None };
    let signature = signatures.get(name)?;
    signature
        .params
        .iter()
        .any(|p| p.convention == Convention::Var)
        .then_some((name, args, signature))
}

fn place_root(expr: &Expr) -> Option<&str> {
    match expr {
        Expr::Var(name) => Some(name),
        Expr::Field { base, .. } | Expr::Index { base, .. } => place_root(base),
        _ => None,
    }
}

fn bind_pattern_immutable(pattern: &Pattern, mutable: &mut BTreeSet<String>) {
    let mut names = Vec::new();
    ast::pattern_binds(pattern, &mut names);
    for name in names {
        mutable.remove(&name);
    }
}

fn discarded_intrinsic_callee(expr: &Expr) -> Option<&'static str> {
    let mut found = None;
    find_call(expr, &mut |name| {
        found = match name {
            intrinsics::LIST_PUSH => Some("list.push"),
            intrinsics::LIST_SET_AT => Some("list.set_at"),
            intrinsics::DICT_INSERT => Some("dict.insert"),
            intrinsics::DICT_UPDATE => Some("dict.update"),
            intrinsics::DICT_REMOVE => Some("dict.remove"),
            _ => found,
        };
    });
    found
}

fn find_call(expr: &Expr, visit: &mut impl FnMut(&str)) {
    match expr {
        Expr::Call { name, args } => {
            visit(name);
            for arg in args {
                find_call(arg, visit);
            }
        }
        Expr::List(values) | Expr::Tuple(values) | Expr::Ctor { args: values, .. }
        | Expr::AnonCtor { args: values, .. } => {
            for value in values {
                find_call(value, visit);
            }
        }
        Expr::LabeledCall { args, .. } => {
            for (_, value) in args {
                find_call(value, visit);
            }
        }
        Expr::MethodCall { receiver, args, .. } => {
            find_call(receiver, visit);
            for arg in args {
                find_call(arg, visit);
            }
        }
        Expr::Apply { func, args } => {
            find_call(func, visit);
            for arg in args {
                find_call(arg, visit);
            }
        }
        Expr::Unary { expr, .. }
        | Expr::Try(expr)
        | Expr::As { expr, .. }
        | Expr::ExistentialPack { expr, .. }
        | Expr::Field { base: expr, .. } => find_call(expr, visit),
        Expr::RecordUpdate { base, fields, .. } => {
            find_call(base, visit);
            for (_, value) in fields {
                find_call(value, visit);
            }
        }
        Expr::Record { fields, spread, .. } => {
            for (_, value) in fields {
                find_call(value, visit);
            }
            if let Some(spread) = spread {
                find_call(spread, visit);
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            find_call(lhs, visit);
            find_call(rhs, visit);
        }
        Expr::If { cond, then_block, else_block } => {
            find_call(cond, visit);
            find_calls_block(then_block, visit);
            if let Some(block) = else_block {
                find_calls_block(block, visit);
            }
        }
        Expr::Match { scrutinee, arms } => {
            find_call(scrutinee, visit);
            for arm in arms {
                if let Some(guard) = &arm.guard {
                    find_call(guard, visit);
                }
                find_call(&arm.body, visit);
            }
        }
        Expr::Block(block) => find_calls_block(block, visit),
        Expr::While { cond, body } => {
            find_call(cond, visit);
            find_calls_block(body, visit);
        }
        Expr::For { iter, body, .. } => {
            find_call(iter, visit);
            find_calls_block(body, visit);
        }
        Expr::WhileLet { scrutinee, body, .. } => {
            find_call(scrutinee, visit);
            find_calls_block(body, visit);
        }
        Expr::Range { lo, hi, .. } => {
            find_call(lo, visit);
            find_call(hi, visit);
        }
        Expr::Index { base, index } => {
            find_call(base, visit);
            find_call(index, visit);
        }
        Expr::Lambda { body, .. } => find_calls_block(body, visit),
        Expr::Int(_) | Expr::Float(_) | Expr::Duration(_) | Expr::Str(_) | Expr::Bool(_)
        | Expr::Var(_) | Expr::TaggedLit { .. } => {}
    }
}

fn find_calls_block(block: &Block, visit: &mut impl FnMut(&str)) {
    for stmt in &block.stmts {
        match stmt {
            Stmt::Let { value, .. } | Stmt::Assign { value, .. }
            | Stmt::LetPattern { value, .. } | Stmt::Return(Some(value))
            | Stmt::Yield(value) | Stmt::Expr(value) => find_call(value, visit),
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use witchy_syntax::parser;

    fn census(source: &str) -> Census {
        let module = parser::parse_module(source).expect("parse");
        census_linked(&module, "main")
    }

    #[test]
    fn classifies_all_migration_categories_from_resolved_declarations() {
        let report = census(
            "fn bump(var value: Int) -> Int:\n    value = value + 1\n    value\n\n\
             fn main():\n    var mutable = 1\n    let immutable = 2\n    mutable = bump(mutable)\n    let result = bump(mutable)\n    bump(immutable)\n    bump(3)\n    bump(mutable)\n",
        );
        assert!(!report.checked, "immutable and temporary arguments must reject");
        let counts = report.counts();
        assert_eq!(counts["mechanical-self-reassignment"], 1);
        assert_eq!(counts["expression-position-mutator"], 1);
        assert_eq!(counts["immutable-var-argument"], 1);
        assert_eq!(counts["temporary-var-argument"], 1);
        assert_eq!(counts["auxiliary-result-discard"], 3);
        assert!(report.findings.iter().all(|finding| finding.callee == "bump"));
        assert!(report.findings.iter().any(|finding| {
            finding.category == Category::ImmutableVarArgument
                && finding.parameter == Some(1)
                && finding.parameter_name.as_deref() == Some("value")
                && finding.root.as_deref() == Some("immutable")
        }));
    }

    #[test]
    fn same_named_non_var_function_is_not_counted() {
        let report = census(
            "fn bump(value: Int) -> Int:\n    value + 1\n\n\
             fn main():\n    let value = bump(1)\n",
        );
        assert!(report.checked, "{:?}", report.check_error);
        assert_eq!(report.resolved_var_calls, 0);
        assert!(report.findings.is_empty());
    }
}
