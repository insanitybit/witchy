//! Call-scoped provenance for explicit `let` parameters (RFC-0167 §3).
//!
//! This runs on the retained, typed AST. An origin follows values through
//! bindings, containers, projections, and captures. Checked scalar results,
//! lawful fresh results, and proven owned copies remove it.

use std::collections::{BTreeMap, BTreeSet};

use foldhash::{HashMap, HashMapExt as _};
use witchy_canonical_source::access::{AccessKind, AccessQualifier};
use witchy_canonical_source::aggregates::type_definitions;
use witchy_canonical_source::storage::StoragePlan;
use witchy_syntax::ast::{
    BinOp, Block, Convention, Expr, Function, Item, Module, Pattern, Stmt, TypeDef, UnOp,
};
use witchy_syntax::lambda_scan::scan_lambda;

use crate::access::CheckedAccessFacts;
use crate::typeck::{Ty, TypeError, TypeTable, ty_to_ast};

type Origins = BTreeSet<String>;
type Environment = HashMap<String, Origins>;

struct Verifier<'a> {
    module: &'a Module,
    table: &'a TypeTable,
    access: &'a CheckedAccessFacts,
    definitions: &'a BTreeMap<String, TypeDef>,
    opt_mode: bool,
    function: &'a str,
}

impl Verifier<'_> {
    fn error(&self, origins: &Origins, action: &str) -> TypeError {
        TypeError {
            message: format!(
                "in `{}`: call-scoped `let` parameter access from `{}` cannot {} without explicit lawful `.owned()` materialization",
                self.function,
                origins.iter().cloned().collect::<Vec<_>>().join(", "),
                action,
            ),
        }
    }

    fn inline_scalar(&self, expression: &Expr) -> bool {
        matches!(
            self.table.type_of(expression),
            Some(Ty::Int | Ty::Float | Ty::Duration | Ty::Bool | Ty::Unit)
        )
    }

    fn result(&self, expression: &Expr, origins: Origins) -> Origins {
        if self.inline_scalar(expression) {
            Origins::new()
        } else {
            origins
        }
    }

    fn union(mut left: Origins, right: Origins) -> Origins {
        left.extend(right);
        left
    }

    fn bind(pattern: &Pattern, origins: &Origins, environment: &mut Environment) {
        let mut names = Vec::new();
        witchy_syntax::ast::pattern_binds(pattern, &mut names);
        for name in names {
            environment.insert(name, origins.clone());
        }
    }

    fn merge_existing(into: &mut Environment, branches: &[Environment]) -> bool {
        let mut changed = false;
        for (name, origins) in into.iter_mut() {
            for branch in branches {
                if let Some(other) = branch.get(name) {
                    let before = origins.len();
                    origins.extend(other.iter().cloned());
                    changed |= origins.len() != before;
                }
            }
        }
        changed
    }

    // Loop-carried aliases can move an origin through several bindings before
    // it reaches a return. Join until stable rather than inspecting one trip.
    fn loop_fixed_point(
        &self,
        environment: &mut Environment,
        mut transfer: impl FnMut(&mut Environment) -> Result<(), TypeError>,
    ) -> Result<(), TypeError> {
        for _ in 0..4096 {
            let mut iteration = environment.clone();
            transfer(&mut iteration)?;
            if !Self::merge_existing(environment, &[iteration]) {
                return Ok(());
            }
        }
        Err(TypeError {
            message: format!(
                "in `{}`: call-scoped loop provenance did not converge",
                self.function
            ),
        })
    }

    /// Whether every path through `block`'s value ends in a pure intrinsic
    /// call producing a String or Bytes, through `if`, `match` and block tails.
    fn block_computes_bytes(&self, block: &Block) -> bool {
        matches!(block.stmts.last(), Some(Stmt::Expr(tail) | Stmt::Return(Some(tail))) if self.computes_bytes(tail))
    }

    fn computes_bytes(&self, expression: &Expr) -> bool {
        match expression {
            Expr::If { then_block, else_block: Some(else_block), .. } =>
                self.block_computes_bytes(then_block) && self.block_computes_bytes(else_block),
            Expr::Match { arms, .. } => !arms.is_empty() && arms.iter().all(|arm| self.computes_bytes(&arm.body)),
            Expr::Block(block) => self.block_computes_bytes(block),
            Expr::Call { name, .. } => matches!(self.table.type_of(expression), Some(Ty::String | Ty::Bytes))
                && witchy_syntax::intrinsics::lookup(name).is_some_and(|spec|
                    spec.effect == witchy_syntax::intrinsics::IntrinsicEffect::Pure
                        && spec.capability_effect == witchy_syntax::intrinsics::CapabilityEffect::None),
            _ => false,
        }
    }

    fn normal_copy_repair(&self, expression: &Expr) -> bool {
        !self.opt_mode
            && self.table.type_of(expression).and_then(ty_to_ast).is_some_and(|ty| {
                StoragePlan::classify(&ty, self.definitions, |_| Some(0)).admits_call_scoped_copy()
            })
    }

    fn check_call(&self, expression: &Expr, values: &[&Expr], arguments: &[Origins]) -> Result<(), TypeError> {
        if arguments.iter().all(Origins::is_empty) {
            return Ok(());
        }
        let Some(signature) = self.access.call_at(self.module, expression) else {
            // A pure compiler intrinsic can inspect a scoped input without a
            // source declaration. Its result retains provenance below unless
            // it is an inline scalar; no host/task/dynamic operation gets this
            // exception.
            if let Expr::Call { name, .. } | Expr::LabeledCall { name, .. } = expression
                && let Some(spec) = witchy_syntax::intrinsics::lookup(name)
                && spec.effect == witchy_syntax::intrinsics::IntrinsicEffect::Pure
                && spec.capability_effect == witchy_syntax::intrinsics::CapabilityEffect::None
            {
                return Ok(());
            }
            if let Expr::Call { name, args } = expression
                && witchy_syntax::cap_ops::is_marked(name)
                && witchy_syntax::cap_ops::result_shape(name, args.len()).is_some()
            {
                // Checked capability operations may inspect borrowed input;
                // their result still carries the origin unless scalar.
                return Ok(());
            }
            let origins = arguments.iter().cloned().fold(Origins::new(), Self::union);
            return Err(self.error(&origins, "cross a call without a checked access contract"));
        };
        if signature.params().len() != arguments.len() || values.len() != arguments.len() {
            let origins = arguments.iter().cloned().fold(Origins::new(), Self::union);
            return Err(self.error(&origins, "cross a call with an incomplete access contract"));
        }
        for ((value, argument), parameter) in values.iter().zip(arguments).zip(signature.params()) {
            if !argument.is_empty()
                && parameter.kind() != AccessKind::SharedBorrow
                && !(parameter.kind() == AccessKind::OwnedImmutable && self.normal_copy_repair(value))
            {
                return Err(self.error(argument, "cross an owning or exclusive call parameter"));
            }
        }
        Ok(())
    }

    fn checked_call_result_is_independent(&self, expression: &Expr) -> bool {
        // The exact checked call signature is the public access contract for
        // direct and indirect calls alike. An owned result with no declared
        // borrow relation has its own ownership token; a view or dependent
        // result keeps every input origin below.
        let Some(signature) = self.access.call_at(self.module, expression) else {
            return false;
        };
        signature.borrow_relations().is_empty()
            && !signature.result().qualifiers().iter().any(|qualifier| {
                matches!(
                    qualifier,
                    AccessQualifier::View
                        | AccessQualifier::Borrow(_)
                        | AccessQualifier::BorrowMut(_)
                )
            })
    }

    fn block(&self, block: &Block, environment: &mut Environment) -> Result<Origins, TypeError> {
        let mut tail = Origins::new();
        for statement in &block.stmts {
            tail = match statement {
                Stmt::Let { name, value, .. } | Stmt::Assign { name, value, .. } => {
                    let origins = self.expr(value, environment)?;
                    environment.insert(name.clone(), origins);
                    Origins::new()
                }
                Stmt::LetPattern { pattern, value } => {
                    let origins = self.expr(value, environment)?;
                    Self::bind(pattern, &origins, environment);
                    Origins::new()
                }
                Stmt::Return(Some(value)) => {
                    let origins = self.expr(value, environment)?;
                    if !origins.is_empty() {
                        return Err(self.error(&origins, "escape through a return"));
                    }
                    Origins::new()
                }
                Stmt::Yield(value) => {
                    let origins = self.expr(value, environment)?;
                    if !origins.is_empty() {
                        return Err(self.error(&origins, "escape through a yield"));
                    }
                    Origins::new()
                }
                Stmt::Expr(value) => self.expr(value, environment)?,
                Stmt::Return(None) | Stmt::Break | Stmt::Continue => Origins::new(),
            };
        }
        Ok(tail)
    }

    fn place_origins(
        &self,
        place: &Expr,
        environment: &mut Environment,
    ) -> Result<Origins, TypeError> {
        match place {
            Expr::Var(name) => Ok(environment.get(name).cloned().unwrap_or_default()),
            Expr::Field { base, .. } => self.place_origins(base, environment),
            Expr::Index { base, index } => {
                let origins = self.place_origins(base, environment)?;
                self.expr(index, environment)?;
                Ok(origins)
            }
            Expr::Unary {
                op: UnOp::Deref,
                expr,
            } => self.expr(expr, environment),
            _ => self.expr(place, environment),
        }
    }

    fn expr(&self, expression: &Expr, environment: &mut Environment) -> Result<Origins, TypeError> {
        let origins = match expression {
            Expr::Int(_)
            | Expr::Float(_)
            | Expr::Duration(_)
            | Expr::Str(_)
            | Expr::Bool(_)
            | Expr::TaggedLit { .. } => Origins::new(),
            Expr::Var(name) => environment.get(name).cloned().unwrap_or_default(),
            Expr::List(values)
            | Expr::Tuple(values)
            | Expr::Ctor { args: values, .. }
            | Expr::AnonCtor { args: values, .. } => {
                let mut origins = Origins::new();
                for value in values {
                    origins.extend(self.expr(value, environment)?);
                }
                origins
            }
            Expr::Call { .. } | Expr::LabeledCall { .. } => {
                let values: Vec<&Expr> = match expression {
                    Expr::Call { args, .. } => args.iter().collect(),
                    Expr::LabeledCall { args, .. } => args.iter().map(|(_, value)| value).collect(),
                    _ => unreachable!(),
                };
                let arguments = values.iter().copied()
                    .map(|value| self.expr(value, environment))
                    .collect::<Result<Vec<_>, _>>()?;
                self.check_call(expression, &values, &arguments)?;
                if self.checked_call_result_is_independent(expression) {
                    Origins::new()
                } else {
                    let name = match expression {
                        Expr::Call { name, .. } | Expr::LabeledCall { name, .. } => name,
                        _ => unreachable!(),
                    };
                    arguments.into_iter().fold(
                        environment.get(name).cloned().unwrap_or_default(),
                        Self::union,
                    )
                }
            }
            Expr::Apply { func, args } => {
                let mut origins = self.expr(func, environment)?;
                let mut arguments = Vec::new();
                for arg in args {
                    arguments.push(self.expr(arg, environment)?);
                }
                self.check_call(expression, &args.iter().collect::<Vec<_>>(), &arguments)?;
                if self.checked_call_result_is_independent(expression) {
                    Origins::new()
                } else {
                    for argument in arguments {
                        origins.extend(argument);
                    }
                    origins
                }
            }
            Expr::ExistentialCall { receiver, args, .. } => {
                let mut arguments = vec![self.expr(receiver, environment)?];
                for arg in args {
                    arguments.push(self.expr(arg, environment)?);
                }
                let values = std::iter::once(receiver.as_ref()).chain(args.iter()).collect::<Vec<_>>();
                self.check_call(expression, &values, &arguments)?;
                if self.checked_call_result_is_independent(expression) {
                    Origins::new()
                } else {
                    arguments.into_iter().fold(Origins::new(), Self::union)
                }
            }
            Expr::MethodCall {
                receiver,
                method,
                args,
            } if method == crate::typeck::OWNED_MATERIALIZATION
                && args.is_empty()
                && self.table.is_compiler_owned_materialization(expression) =>
            {
                self.expr(receiver, environment)?;
                Origins::new()
            }
            Expr::MethodCall { receiver, args, .. } => {
                let mut arguments = vec![self.expr(receiver, environment)?];
                for arg in args {
                    arguments.push(self.expr(arg, environment)?);
                }
                let values = std::iter::once(receiver.as_ref()).chain(args.iter()).collect::<Vec<_>>();
                self.check_call(expression, &values, &arguments)?;
                if self.checked_call_result_is_independent(expression) {
                    Origins::new()
                } else {
                    arguments.into_iter().fold(Origins::new(), Self::union)
                }
            }
            Expr::LabeledMethodCall { receiver, args, .. } => {
                let mut arguments = vec![self.expr(receiver, environment)?];
                for (_, arg) in args {
                    arguments.push(self.expr(arg, environment)?);
                }
                let values = std::iter::once(receiver.as_ref()).chain(args.iter().map(|(_, arg)| arg)).collect::<Vec<_>>();
                self.check_call(expression, &values, &arguments)?;
                if self.checked_call_result_is_independent(expression) {
                    Origins::new()
                } else {
                    arguments.into_iter().fold(Origins::new(), Self::union)
                }
            }
            Expr::Unary {
                op: UnOp::Borrow,
                expr,
            } => self.place_origins(expr, environment)?,
            Expr::Unary { expr, .. }
            | Expr::As { expr, .. }
            | Expr::ExistentialPack { expr, .. }
            | Expr::ExistentialUpcast { expr, .. } => self.expr(expr, environment)?,
            Expr::Try(expr) => {
                let origins = self.expr(expr, environment)?;
                if !origins.is_empty() {
                    return Err(self.error(&origins, "escape through `?` propagation"));
                }
                origins
            }
            Expr::Field { base, .. } => self.expr(base, environment)?,
            Expr::Index { base, index } => {
                let base = self.expr(base, environment)?;
                self.expr(index, environment)?;
                base
            }
            Expr::Lambda { params, body, .. } => {
                let captures = scan_lambda(params, body).captures();
                let mut origins = Origins::new();
                for capture in captures {
                    if let Some(source) = environment.get(&capture) {
                        origins.extend(source.iter().cloned());
                    }
                }
                // A callback has its own call-scoped inputs. Inspect its body
                // now, while the checked access facts still address this AST;
                // merely marking the closure value misses an escape or an
                // owning call performed when the callback runs.
                let mut nested = environment.clone();
                for param in params {
                    nested.insert(param.name.clone(), if param.convention == Convention::Borrow {
                        BTreeSet::from([param.name.clone()])
                    } else {
                        Origins::new()
                    });
                }
                let returned = self.block(body, &mut nested)?;
                if !returned.is_empty() {
                    return Err(self.error(&returned, "escape through a callback result"));
                }
                origins
            }
            Expr::RecordUpdate { base, fields, .. } => {
                let mut origins = self.expr(base, environment)?;
                for (_, field) in fields {
                    origins.extend(self.expr(field, environment)?);
                }
                origins
            }
            Expr::Record { fields, spread, .. } => {
                let mut origins = Origins::new();
                if let Some(base) = spread {
                    origins.extend(self.expr(base, environment)?);
                }
                for (_, field) in fields {
                    origins.extend(self.expr(field, environment)?);
                }
                origins
            }
            Expr::Binary {
                op: BinOp::Coalesce,
                lhs,
                rhs,
            } => Self::union(self.expr(lhs, environment)?, self.expr(rhs, environment)?),
            Expr::Binary { lhs, rhs, .. }
            | Expr::Range {
                lo: lhs, hi: rhs, ..
            } => {
                let mut origins = self.expr(lhs, environment)?;
                origins.extend(self.expr(rhs, environment)?);
                if matches!(expression, Expr::Binary { op: BinOp::Add | BinOp::Concat, .. })
                    && matches!(self.table.type_of(expression), Some(Ty::String))
                {
                    // String concatenation allocates a fresh owned result.
                    Origins::new()
                } else {
                    origins
                }
            }
            Expr::If {
                cond,
                then_block,
                else_block,
            } => {
                self.expr(cond, environment)?;
                let mut yes = environment.clone();
                let yes_result = self.block(then_block, &mut yes)?;
                let mut no = environment.clone();
                let no_result = if let Some(block) = else_block {
                    self.block(block, &mut no)?
                } else {
                    Origins::new()
                };
                Self::merge_existing(environment, &[yes, no]);
                Self::union(yes_result, no_result)
            }
            Expr::Match { scrutinee, arms } => {
                let source = self.expr(scrutinee, environment)?;
                let mut results = Origins::new();
                let mut branches = Vec::new();
                for arm in arms {
                    let mut branch = environment.clone();
                    Self::bind(&arm.pattern, &source, &mut branch);
                    if let Some(guard) = &arm.guard {
                        self.expr(guard, &mut branch)?;
                    }
                    results.extend(self.expr(&arm.body, &mut branch)?);
                    branches.push(branch);
                }
                Self::merge_existing(environment, &branches);
                results
            }
            Expr::Block(block) => {
                let mut branch = environment.clone();
                let result = self.block(block, &mut branch)?;
                Self::merge_existing(environment, &[branch]);
                result
            }
            Expr::While { cond, body } => {
                self.loop_fixed_point(environment, |iteration| {
                    self.expr(cond, iteration)?;
                    self.block(body, iteration)?;
                    Ok(())
                })?;
                Origins::new()
            }
            Expr::For { var, iter, body } => {
                self.loop_fixed_point(environment, |iteration| {
                    let mut source = self.expr(iter, iteration)?;
                    if !source.is_empty()
                        && !self.opt_mode
                        && matches!(self.table.type_of(iter), Some(Ty::List(element))
                            if ty_to_ast(element).is_some_and(|ty|
                                StoragePlan::classify(&ty, self.definitions, |_| Some(0))
                                    .admits_call_scoped_copy()))
                    {
                        // Normal list iteration clones an owned leaf into the
                        // loop binding. Its new storage cannot retain the
                        // list parameter's call-scoped access.
                        source.clear();
                    }
                    iteration.insert(var.clone(), source);
                    self.block(body, iteration)?;
                    Ok(())
                })?;
                Origins::new()
            }
            Expr::WhileLet {
                pattern,
                scrutinee,
                body,
            } => {
                self.loop_fixed_point(environment, |iteration| {
                    let source = self.expr(scrutinee, iteration)?;
                    Self::bind(pattern, &source, iteration);
                    self.block(body, iteration)?;
                    Ok(())
                })?;
                Origins::new()
            }
        };
        Ok(self.result(expression, origins))
    }
}

pub(crate) fn verify(
    module: &Module,
    table: &TypeTable,
    access: &CheckedAccessFacts,
) -> Result<(), TypeError> {
    let definitions = type_definitions(module);
    for item in &module.items {
        let Item::Function(function) = item else {
            continue;
        };
        verify_function(module, table, access, &definitions, function)?;
    }
    Ok(())
}

fn verify_function(
    module: &Module,
    table: &TypeTable,
    access: &CheckedAccessFacts,
    definitions: &BTreeMap<String, TypeDef>,
    function: &Function,
) -> Result<(), TypeError> {
    let verifier = Verifier {
        module,
        table,
        access,
        definitions,
        opt_mode: witchy_canonical_source::source::source_execution_mode(function, &module.modes).is_opt(),
        function: &function.name,
    };
    let mut environment = Environment::new();
    for param in &function.params {
        let origins = if param.convention == Convention::Borrow {
            BTreeSet::from([param.name.clone()])
        } else {
            Origins::new()
        };
        environment.insert(param.name.clone(), origins);
    }
    let tail = verifier.block(&function.body, &mut environment)?;
    // A `unique` String or Bytes computed by a pure intrinsic on every path
    // is its own sole token. A byte buffer holds no reference, so the only
    // alias it could carry is the borrowed root itself, which the canonical
    // checker refuses as a unique result. Any other value (a constructor, a
    // container, a call that may keep its argument) keeps this diagnostic.
    let unique_result = matches!(&function.ret, Some(witchy_syntax::ast::Type::Qualified(witchy_syntax::ast::TypeQual::Unique, _)));
    if !tail.is_empty() && !(unique_result && verifier.block_computes_bytes(&function.body)) {
        return Err(verifier.error(&tail, "escape through a returned value"));
    }
    Ok(())
}

