//! Checked execution-mode provenance for task values (RFC-0167 §3.11).
//!
//! Summaries are symbolic in parameters, then instantiated at each direct
//! checked call. Values retain the proof through bindings and aggregates.
//! Unknown callbacks, dynamic dispatch, or projections fail closed at an opt
//! await; a result type alone cannot authorize opt execution.

use std::collections::{BTreeMap, BTreeSet};

use witchy_canonical_source::aggregates::type_definitions;
use witchy_canonical_source::source::{
    SourceExecutionMode, TaskCallableScope, TaskExecutionProof as Proof, source_execution_mode,
};
use witchy_syntax::ast::{Block, Convention, Expr, Function, Item, Module, Pattern, Stmt, TypeDef, UnOp};

use crate::typeck::{Ty, TypeError, TypeTable, ty_to_ast};

type Environment = BTreeMap<String, Proof>;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Summary {
    result: Proof,
    required_opt_inputs: Vec<Proof>,
}

struct Analyzer<'a> {
    module: &'a Module,
    table: &'a TypeTable,
    definitions: &'a BTreeMap<String, TypeDef>,
    summaries: &'a BTreeMap<String, Summary>,
    function: &'a Function,
    check_await: bool,
    returns: Vec<Proof>,
    required_opt_inputs: Vec<Proof>,
    await_origins: BTreeMap<usize, Proof>,
    lambda_scopes: BTreeMap<usize, TaskCallableScope>,
}

impl Analyzer<'_> {
    fn checked_variant_ordinal(&self, expression: &Expr, constructor: &str) -> Option<usize> {
        let Ty::Named(owner, _) = self.table.type_of(expression)? else { return None };
        let short = constructor.rsplit('.').next()?;
        if let Some(definition) = self.definitions.get(owner.as_ref()) {
            return definition.variants.iter().position(|variant| {
                variant.name == constructor || variant.name.rsplit('.').next() == Some(short)
            });
        }
        witchy_syntax::synthetic_names::anon_union_synthetic_variants(owner.as_ref())?
            .iter().position(|(tag, _)| tag == short.trim_start_matches('.'))
    }

    fn checked_try_payload(&self, operand: &Expr, proof: Proof) -> Proof {
        let Some(Ty::Named(owner, _)) = self.table.type_of(operand) else {
            return Proof::Unknown;
        };
        let Some(definition) = self.definitions.get(owner.as_ref()) else {
            return Proof::Unknown;
        };
        if definition.variants.len() != 2 || definition.variants[0].fields.len() != 1 {
            return Proof::Unknown;
        }
        match proof {
            Proof::Variant { ordinal: 0, fields } if fields.len() == 1 => fields.into_iter().next().unwrap(),
            Proof::Variants(mut variants) => variants.remove(&0)
                .and_then(|fields| (fields.len() == 1).then(|| fields.into_iter().next().unwrap()))
                .unwrap_or(Proof::Unknown),
            Proof::Parameter(_) | Proof::Slot(_, _) | Proof::VariantSlot(_, _, _)
                | Proof::AnyElement(_) => Proof::VariantSlot(Box::new(proof), 0, 0),
            _ => Proof::Unknown,
        }
    }

    fn place_root(expression: &Expr) -> Option<&str> {
        match expression {
            Expr::Var(name) => Some(name),
            Expr::Field { base, .. }
            | Expr::Index { base, .. }
            | Expr::Unary {
                op: UnOp::Borrow | UnOp::BorrowMut | UnOp::Deref | UnOp::Move,
                expr: base,
            } => Self::place_root(base),
            _ => None,
        }
    }

    fn callable_conventions(&self, callable: Option<&Ty>, arity: usize) -> Option<Vec<Convention>> {
        let Ty::Fn(parameters, _, conventions, _, _) = callable? else { return None };
        if parameters.len() != arity { return None; }
        if conventions.is_empty() { Some(vec![Convention::Let; arity]) }
        else { (conventions.len() == arity).then(|| conventions.to_vec()) }
    }

    fn invalidate_arguments<'b>(
        &self,
        arguments: impl IntoIterator<Item = &'b Expr>,
        conventions: Option<&[Convention]>,
        environment: &mut Environment,
    ) {
        let arguments = arguments.into_iter().collect::<Vec<_>>();
        let conventions = conventions.filter(|items| items.len() == arguments.len());
        for (index, argument) in arguments.into_iter().enumerate() {
            if let Some(root) = Self::place_root(argument) {
                if environment.contains_key(root) {
                    // A checked immutable formal receives an independent
                    // replayable copy; it cannot consume or replace the
                    // caller's executable recipe. Unknown, var, and own
                    // contracts still revoke its provenance.
                    if conventions.is_some_and(|items| matches!(items[index], Convention::Let | Convention::Borrow))
                        && self.table.type_of(argument).and_then(ty_to_ast).is_some_and(|source| {
                            witchy_canonical_source::storage::replay_capture_eligibility(
                                &source, self.definitions,
                            ).is_ok()
                        })
                    {
                        continue;
                    }
                    environment.insert(root.to_owned(), Proof::Unknown);
                }
            }
        }
    }

    fn task_result(&self, expression: &Expr) -> bool {
        matches!(self.table.type_of(expression), Some(Ty::Named(name, args))
            if (name.as_ref() == "Task" || name.as_ref() == "task.Task") && args.len() == 1)
    }

    fn join_environment(environment: &mut Environment, branches: &[Environment]) -> bool {
        let mut changed = false;
        for (name, old) in environment.iter_mut() {
            for branch in branches {
                if let Some(next) = branch.get(name) {
                    let joined = old.clone().join(next.clone());
                    changed |= joined != *old;
                    *old = joined;
                }
            }
        }
        changed
    }

    fn join_values(values: impl IntoIterator<Item = Proof>) -> Proof {
        values
            .into_iter()
            .reduce(Proof::join)
            .unwrap_or(Proof::Unknown)
    }

    fn bind(pattern: &Pattern, proof: Proof, ordinal: Option<usize>, environment: &mut Environment) {
        match (pattern, proof) {
            (Pattern::Var(name), proof) if name != "_" => {
                environment.insert(name.clone(), proof);
            }
            (Pattern::Tuple(patterns), Proof::Elements(values))
                if patterns.len() == values.len() =>
            {
                for (pattern, value) in patterns.iter().zip(values) {
                    Self::bind(pattern, value, None, environment);
                }
            }
            (Pattern::Tuple(patterns), proof @ (Proof::Parameter(_) | Proof::Slot(_, _))) => {
                for (index, pattern) in patterns.iter().enumerate() {
                    Self::bind(
                        pattern,
                        Proof::Slot(Box::new(proof.clone()), index),
                        None,
                        environment,
                    );
                }
            }
            (Pattern::Ctor { args, .. } | Pattern::AnonCtor { args, .. },
                Proof::Variant { ordinal: actual, fields: values })
                if ordinal.is_none_or(|expected| expected == actual) && args.len() == values.len() => {
                for (pattern, value) in args.iter().zip(values) {
                    Self::bind(pattern, value, None, environment);
                }
            }
            (Pattern::Ctor { args, .. } | Pattern::AnonCtor { args, .. }, Proof::Variants(mut variants)) => {
                if let Some(values) = ordinal.and_then(|ordinal| variants.remove(&ordinal))
                    && values.len() == args.len()
                {
                    for (pattern, value) in args.iter().zip(values) {
                        Self::bind(pattern, value, None, environment);
                    }
                } else {
                    let mut names = Vec::new();
                    witchy_syntax::ast::pattern_binds(pattern, &mut names);
                    for name in names { environment.insert(name, Proof::Unknown); }
                }
            }
            (Pattern::Ctor { args, .. } | Pattern::AnonCtor { args, .. },
                proof @ (Proof::Parameter(_) | Proof::Slot(_, _) | Proof::VariantSlot(_, _, _)))
                if ordinal.is_some() => {
                let ordinal = ordinal.expect("checked above");
                for (index, pattern) in args.iter().enumerate() {
                    Self::bind(pattern, Proof::VariantSlot(Box::new(proof.clone()), ordinal, index), None, environment);
                }
            }
            (pattern, _) => {
                let mut names = Vec::new();
                witchy_syntax::ast::pattern_binds(pattern, &mut names);
                for name in names {
                    environment.insert(name, Proof::Unknown);
                }
            }
        }
    }

    fn block(&mut self, block: &Block, environment: &mut Environment) -> Result<Proof, TypeError> {
        let mut tail = Proof::Unknown;
        for statement in &block.stmts {
            tail = match statement {
                Stmt::Let { name, value, .. } | Stmt::Assign { name, value, .. } => {
                    let proof = self.expr(value, environment)?;
                    environment.insert(name.clone(), proof);
                    Proof::Unknown
                }
                Stmt::LetPattern { pattern, value } => {
                    let proof = self.expr(value, environment)?;
                    let ordinal = match pattern {
                        Pattern::Ctor { name, .. } => self.checked_variant_ordinal(value, name),
                        Pattern::AnonCtor { tag, .. } => self.checked_variant_ordinal(value, tag),
                        _ => None,
                    };
                    Self::bind(pattern, proof, ordinal, environment);
                    Proof::Unknown
                }
                Stmt::Return(Some(value)) => {
                    let proof = self.expr(value, environment)?;
                    self.returns.push(proof);
                    Proof::Unknown
                }
                Stmt::Yield(value) => {
                    self.expr(value, environment)?;
                    Proof::Unknown
                }
                Stmt::Expr(value) => self.expr(value, environment)?,
                Stmt::Return(None) | Stmt::Break | Stmt::Continue => Proof::Unknown,
            };
        }
        Ok(tail)
    }

    fn direct_call(
        &mut self,
        expression: &Expr,
        name: &str,
        args: &[Expr],
        environment: &mut Environment,
    ) -> Result<Proof, TypeError> {
        let proofs = args
            .iter()
            .map(|arg| self.expr(arg, environment))
            .collect::<Result<Vec<_>, _>>()?;
        // The checker records local callable resolution at the exact call
        // expression. A shadowing value cannot borrow same-spelled declaration
        // authority, even if its signature happens to match.
        if environment.contains_key(name) || self.table.local_callee_type(expression).is_some() {
            match environment.get(name).cloned() {
                Some(Proof::Callable { scope, parameters, result }) if parameters == proofs.len() => {
                    let result = result.instantiate_callback(&scope, &proofs).bounded(4096);
                    // A closure may change its captures when called. Its
                    // current result was checked above, but prior captured
                    // proofs need a fresh dataflow judgment after invocation.
                    environment.values_mut().for_each(|proof| *proof = Proof::Unknown);
                    return Ok(result);
                }
                Some(Proof::DeclaredCallable { conventions, result }) if conventions.len() == proofs.len() => {
                    let result = result.instantiate(&proofs).bounded(4096);
                    self.invalidate_arguments(args, Some(&conventions), environment);
                    return Ok(result);
                }
                _ => {}
            }
            self.invalidate_arguments(args, None, environment);
            return Ok(Proof::Unknown);
        }
        let Some(function) = self.module.items.iter().find_map(|item| match item {
            Item::Function(function) if function.name == name => Some(function),
            _ => None,
        }) else {
            self.invalidate_arguments(args, None, environment);
            return Ok(Proof::Unknown);
        };
        if proofs.len() != function.params.len() {
            self.invalidate_arguments(args, None, environment);
            return Ok(Proof::Unknown);
        }
        if let Some(summary) = self.summaries.get(name) {
            for requirement in &summary.required_opt_inputs {
                self.require_opt_task(requirement.instantiate(&proofs))?;
            }
        }
        let result = if function.is_async {
            let body_result = self.summaries.get(name).map_or(Proof::Unknown, |summary| {
                summary.result.instantiate(&proofs).bounded(4096)
            });
            Proof::Task(
                source_execution_mode(function, &self.module.modes),
                Box::new(body_result),
            )
        } else {
            self.summaries.get(name).map_or(Proof::Unknown, |summary| {
                summary.result.instantiate(&proofs).bounded(4096)
            })
        };
        let conventions = function.params.iter().map(|parameter| parameter.convention).collect::<Vec<_>>();
        self.invalidate_arguments(args, Some(&conventions), environment);
        Ok(result)
    }

    fn require_opt_task(&mut self, proof: Proof) -> Result<Proof, TypeError> {
        match proof {
            Proof::Task(SourceExecutionMode::Opt, result) => Ok(*result),
            proof if proof.is_parameter_projection() => {
                if !self.required_opt_inputs.contains(&proof) {
                    self.required_opt_inputs.push(proof.clone());
                }
                Ok(Proof::Awaited(Box::new(proof)))
            }
            _ if !self.check_await => Ok(Proof::Unknown),
            _ => Err(TypeError {
                message: format!(
                    "in `{}`: opt task execution requires checked opt-compatible task value provenance",
                    self.function.name,
                ),
            }),
        }
    }

    fn expr(
        &mut self,
        expression: &Expr,
        environment: &mut Environment,
    ) -> Result<Proof, TypeError> {
        let proof = match expression {
            Expr::Var(name) => {
                if let Some(local) = environment.get(name) {
                    local.clone()
                } else if self.table.declared_function_value(expression) == Some(name.as_str()) {
                    let declaration = self.module.items.iter().find_map(|item| match item {
                        Item::Function(function) if function.name == *name => Some(function),
                        _ => None,
                    });
                    match (declaration, self.summaries.get(name)) {
                        (Some(function), Some(summary)) if summary.required_opt_inputs.is_empty() => {
                            let result = summary.result.clone();
                            let result = if function.is_async {
                                Proof::Task(source_execution_mode(function, &self.module.modes), Box::new(result))
                            } else { result };
                            Proof::DeclaredCallable {
                                conventions: function.params.iter().map(|parameter| parameter.convention).collect(),
                                result: Box::new(result.bounded(4096)),
                            }
                        }
                        (Some(_), Some(_)) if self.check_await => {
                            return Err(TypeError { message: format!(
                                "in `{}`: callable `{name}` requires opt task input provenance and cannot be erased into an unchecked function value",
                                self.function.name,
                            ) });
                        }
                        _ => Proof::Unknown,
                    }
                } else {
                    Proof::Unknown
                }
            }
            Expr::Call { name, args } => self.direct_call(expression, name, args, environment)?,
            Expr::LabeledCall { args, .. } => {
                for (_, value) in args {
                    self.expr(value, environment)?;
                }
                let conventions = self.callable_conventions(self.table.local_callee_type(expression), args.len());
                self.invalidate_arguments(args.iter().map(|(_, value)| value), conventions.as_deref(), environment);
                Proof::Unknown
            }
            Expr::List(values) | Expr::Tuple(values) => {
                let values = values
                    .iter()
                    .map(|value| self.expr(value, environment))
                    .collect::<Result<Vec<_>, _>>()?;
                // A task constructor stores an executable closure. Its body
                // mode needs a separate callable proof; the constructor's
                // source spelling cannot mint one.
                if self.task_result(expression) {
                    Proof::Unknown
                } else {
                    Proof::Elements(values)
                }
            }
            Expr::Ctor { name, args } | Expr::AnonCtor { tag: name, args } => {
                let values = args.iter().map(|value| self.expr(value, environment))
                    .collect::<Result<Vec<_>, _>>()?;
                if self.task_result(expression) {
                    Proof::Unknown
                } else {
                    self.checked_variant_ordinal(expression, name)
                        .map(|ordinal| Proof::Variant { ordinal, fields: values })
                        .unwrap_or(Proof::Unknown)
                }
            }
            Expr::Record { fields, spread, .. } => {
                let mut values = if let Some(spread) = spread {
                    match self.expr(spread, environment)? {
                        Proof::Fields(fields) => fields,
                        _ => BTreeMap::new(),
                    }
                } else {
                    BTreeMap::new()
                };
                for (name, value) in fields {
                    values.insert(name.clone(), self.expr(value, environment)?);
                }
                Proof::Fields(values)
            }
            Expr::RecordUpdate { base, fields, .. } => {
                let mut values = match self.expr(base, environment)? {
                    Proof::Fields(fields) => fields,
                    _ => BTreeMap::new(),
                };
                for (name, value) in fields {
                    values.insert(name.clone(), self.expr(value, environment)?);
                }
                Proof::Fields(values)
            }
            Expr::Field { base, field } => match self.expr(base, environment)? {
                Proof::Fields(mut fields) => fields.remove(field).unwrap_or(Proof::Unknown),
                Proof::Elements(elements) if matches!(self.table.type_of(base), Some(Ty::Tuple(_))) =>
                    field.parse::<usize>().ok()
                        .and_then(|index| elements.get(index).cloned())
                        .unwrap_or(Proof::Unknown),
                proof @ (Proof::Variant { .. } | Proof::Parameter(_) | Proof::Slot(_, _) | Proof::VariantSlot(_, _, _)) => {
                    let index = match self.table.type_of(base) {
                        Some(Ty::Named(name, _)) => self
                            .definitions
                            .get(name.as_ref())
                            .filter(|definition| definition.variants.len() == 1)
                            .and_then(|definition| {
                                definition.variants[0]
                                    .field_names
                                    .iter()
                                    .position(|name| name == field)
                            }),
                        _ => None,
                    };
                    index
                        .map(|index| match proof {
                            Proof::Variant { ordinal: 0, fields } =>
                                fields.get(index).cloned().unwrap_or(Proof::Unknown),
                            Proof::Variant { .. } => Proof::Unknown,
                            other => Proof::VariantSlot(Box::new(other), 0, index),
                        })
                        .unwrap_or(Proof::Unknown)
                }
                _ => Proof::Unknown,
            },
            Expr::Index { base, index } => {
                let base = self.expr(base, environment)?;
                self.expr(index, environment)?;
                match base {
                    Proof::Elements(elements) => Self::join_values(elements),
                    proof @ (Proof::Parameter(_) | Proof::Slot(_, _)) => {
                        Proof::AnyElement(Box::new(proof))
                    }
                    _ => Proof::Unknown,
                }
            }
            Expr::Unary {
                op: UnOp::Await,
                expr,
            } => {
                let origin = self.expr(expr, environment)?;
                if self.check_await {
                    let address = expression as *const Expr as usize;
                    self.await_origins
                        .entry(address)
                        .and_modify(|previous| *previous = previous.clone().join(origin.clone()))
                        .or_insert_with(|| origin.clone());
                }
                if source_execution_mode(self.function, &self.module.modes)
                    == SourceExecutionMode::Opt
                {
                    self.require_opt_task(origin)?
                } else {
                    match origin {
                        Proof::Task(_, result) => *result,
                        _ => Proof::Unknown,
                    }
                }
            }
            Expr::Unary { op: UnOp::BorrowMut, expr } => {
                self.expr(expr, environment)?;
                // The checker represents an explicit exclusive reference by
                // its referent type. Its apparent replayable Task type cannot
                // prove that the original binding survives writes through
                // this handle, even when a callee's formal says `let`.
                if let Some(root) = Self::place_root(expr) {
                    environment.insert(root.to_owned(), Proof::Unknown);
                }
                Proof::Unknown
            }
            Expr::Unary { expr, .. }
            | Expr::As { expr, .. }
            | Expr::ExistentialPack { expr, .. }
            | Expr::ExistentialUpcast { expr, .. } => self.expr(expr, environment)?,
            Expr::If {
                cond,
                then_block,
                else_block,
            } => {
                self.expr(cond, environment)?;
                let mut yes = environment.clone();
                let left = self.block(then_block, &mut yes)?;
                let mut no = environment.clone();
                let right = if let Some(block) = else_block {
                    self.block(block, &mut no)?
                } else {
                    Proof::Unknown
                };
                Self::join_environment(environment, &[yes, no]);
                left.join(right)
            }
            Expr::Match { scrutinee, arms } => {
                let source = self.expr(scrutinee, environment)?;
                let mut values = Vec::new();
                let mut branches = Vec::new();
                for arm in arms {
                    let mut branch = environment.clone();
                    let ordinal = match &arm.pattern {
                        Pattern::Ctor { name, .. } => self.checked_variant_ordinal(scrutinee, name),
                        Pattern::AnonCtor { tag, .. } => self.checked_variant_ordinal(scrutinee, tag),
                        _ => None,
                    };
                    Self::bind(&arm.pattern, source.clone(), ordinal, &mut branch);
                    if let Some(guard) = &arm.guard {
                        self.expr(guard, &mut branch)?;
                    }
                    values.push(self.expr(&arm.body, &mut branch)?);
                    branches.push(branch);
                }
                Self::join_environment(environment, &branches);
                Self::join_values(values)
            }
            Expr::Block(block) => {
                let mut branch = environment.clone();
                let value = self.block(block, &mut branch)?;
                Self::join_environment(environment, &[branch]);
                value
            }
            Expr::Lambda { params, body, .. } => {
                let scope = self.lambda_scopes.get(&(expression as *const Expr as usize))
                    .cloned().ok_or_else(|| TypeError {
                        message: format!("in `{}`: checked lambda lacks a stable callable scope", self.function.name),
                    })?;
                let mut nested = environment.clone();
                for (index, parameter) in params.iter().enumerate() {
                    nested.insert(parameter.name.clone(), Proof::CallableParameter(scope.clone(), index));
                }
                // Audit deferred bodies even when the closure itself is only
                // stored. Their effects run under this source declaration.
                let old_returns = std::mem::take(&mut self.returns);
                let old_requirements = std::mem::take(&mut self.required_opt_inputs);
                let tail = self.block(body, &mut nested)?;
                let mut nested_returns = std::mem::replace(&mut self.returns, old_returns);
                if !matches!(body.stmts.last(), Some(Stmt::Return(_))) {
                    nested_returns.push(tail);
                }
                let nested_requirements =
                    std::mem::replace(&mut self.required_opt_inputs, old_requirements);
                for requirement in nested_requirements {
                    if !self.required_opt_inputs.contains(&requirement) {
                        self.required_opt_inputs.push(requirement);
                    }
                }
                Proof::Callable {
                    scope,
                    parameters: params.len(),
                    result: Box::new(Self::join_values(nested_returns).bounded(4096)),
                }
            }
            Expr::Apply { func, args } => {
                let callable = self.expr(func, environment)?;
                let proofs = args.iter().map(|arg| self.expr(arg, environment))
                    .collect::<Result<Vec<_>, _>>()?;
                match callable {
                    Proof::Callable { scope, parameters, result } if parameters == args.len() => {
                        environment.values_mut().for_each(|proof| *proof = Proof::Unknown);
                        result.instantiate_callback(&scope, &proofs).bounded(4096)
                    }
                    Proof::DeclaredCallable { conventions, result } if conventions.len() == args.len() => {
                        self.invalidate_arguments(args, Some(&conventions), environment);
                        result.instantiate(&proofs).bounded(4096)
                    }
                    _ => {
                        environment.values_mut().for_each(|proof| *proof = Proof::Unknown);
                        Proof::Unknown
                    }
                }
            }
            Expr::MethodCall { receiver, args, .. } => {
                self.expr(receiver, environment)?;
                for arg in args {
                    self.expr(arg, environment)?;
                }
                let conventions = self.callable_conventions(self.table.local_callee_type(expression), args.len() + 1);
                self.invalidate_arguments(
                    std::iter::once(receiver.as_ref()).chain(args.iter()),
                    conventions.as_deref(),
                    environment,
                );
                Proof::Unknown
            }
            Expr::LabeledMethodCall { receiver, args, .. } => {
                self.expr(receiver, environment)?;
                for (_, arg) in args {
                    self.expr(arg, environment)?;
                }
                let conventions = self.callable_conventions(self.table.local_callee_type(expression), args.len() + 1);
                self.invalidate_arguments(
                    std::iter::once(receiver.as_ref()).chain(args.iter().map(|(_, value)| value)),
                    conventions.as_deref(),
                    environment,
                );
                Proof::Unknown
            }
            Expr::ExistentialCall { receiver, args, .. } => {
                self.expr(receiver, environment)?;
                for arg in args {
                    self.expr(arg, environment)?;
                }
                let conventions = self.callable_conventions(self.table.local_callee_type(expression), args.len() + 1);
                self.invalidate_arguments(
                    std::iter::once(receiver.as_ref()).chain(args.iter()),
                    conventions.as_deref(),
                    environment,
                );
                Proof::Unknown
            }
            Expr::Binary { lhs, rhs, .. }
            | Expr::Range {
                lo: lhs, hi: rhs, ..
            } => {
                self.expr(lhs, environment)?;
                self.expr(rhs, environment)?;
                Proof::Unknown
            }
            Expr::Try(expr) => {
                let proof = self.expr(expr, environment)?;
                self.checked_try_payload(expr, proof)
            }
            Expr::While { cond, body } => {
                self.loop_body(environment, |this, environment| {
                    this.expr(cond, environment)?;
                    this.block(body, environment)?;
                    Ok(())
                })?;
                Proof::Unknown
            }
            Expr::For { var, iter, body } => {
                self.loop_body(environment, |this, environment| {
                    let source = this.expr(iter, environment)?;
                    let element = match source {
                        Proof::Elements(elements) => Self::join_values(elements),
                        proof @ (Proof::Parameter(_) | Proof::Slot(_, _)) => {
                            Proof::AnyElement(Box::new(proof))
                        }
                        _ => Proof::Unknown,
                    };
                    environment.insert(var.clone(), element);
                    this.block(body, environment)?;
                    Ok(())
                })?;
                Proof::Unknown
            }
            Expr::WhileLet {
                pattern,
                scrutinee,
                body,
            } => {
                self.loop_body(environment, |this, environment| {
                    let source = this.expr(scrutinee, environment)?;
                    let ordinal = match pattern {
                        Pattern::Ctor { name, .. } => this.checked_variant_ordinal(scrutinee, name),
                        Pattern::AnonCtor { tag, .. } => this.checked_variant_ordinal(scrutinee, tag),
                        _ => None,
                    };
                    Self::bind(pattern, source, ordinal, environment);
                    this.block(body, environment)?;
                    Ok(())
                })?;
                Proof::Unknown
            }
            Expr::Int(_)
            | Expr::Float(_)
            | Expr::Duration(_)
            | Expr::Str(_)
            | Expr::Bool(_)
            | Expr::TaggedLit { .. } => Proof::Unknown,
        };
        Ok(proof)
    }

    fn loop_body(
        &mut self,
        environment: &mut Environment,
        mut transfer: impl FnMut(&mut Self, &mut Environment) -> Result<(), TypeError>,
    ) -> Result<(), TypeError> {
        for _ in 0..128 {
            let mut iteration = environment.clone();
            transfer(self, &mut iteration)?;
            if !Self::join_environment(environment, &[iteration]) {
                return Ok(());
            }
        }
        Err(TypeError {
            message: format!(
                "in `{}`: task execution provenance did not converge",
                self.function.name
            ),
        })
    }
}

fn analyze_function(
    module: &Module,
    table: &TypeTable,
    definitions: &BTreeMap<String, TypeDef>,
    summaries: &BTreeMap<String, Summary>,
    function: &Function,
    check_await: bool,
) -> Result<(Summary, BTreeMap<usize, Proof>), TypeError> {
    // AST preorder is stable across summary iterations. A lexical scope key
    // keeps lambda formals distinct from enclosing function parameters and
    // from formals captured by a nested lambda.
    let mut lambda_scopes = BTreeMap::new();
    let mut next_lambda = 0_u32;
    witchy_syntax::ast::visit::visit_block(&function.body, &mut |expression| {
        if matches!(expression, Expr::Lambda { .. }) {
            let ordinal = next_lambda;
            next_lambda = next_lambda.checked_add(1).ok_or_else(|| TypeError {
                message: format!("in `{}`: checked lambda count exceeds u32", function.name),
            })?;
            lambda_scopes.insert(expression as *const Expr as usize,
                TaskCallableScope::new(function.name.clone(), ordinal));
        }
        Ok::<(), TypeError>(())
    })?;
    let mut analyzer = Analyzer {
        module,
        table,
        definitions,
        summaries,
        function,
        check_await,
        returns: Vec::new(),
        required_opt_inputs: Vec::new(),
        await_origins: BTreeMap::new(),
        lambda_scopes,
    };
    let mut environment = function
        .params
        .iter()
        .enumerate()
        .map(|(index, param)| (param.name.clone(), Proof::Parameter(index)))
        .collect();
    let tail = analyzer.block(&function.body, &mut environment)?;
    if !matches!(function.body.stmts.last(), Some(Stmt::Return(_))) {
        analyzer.returns.push(tail);
    }
    Ok((Summary {
        result: Analyzer::join_values(analyzer.returns).bounded(4096),
        required_opt_inputs: analyzer.required_opt_inputs,
    }, analyzer.await_origins))
}

/// Each key is an await expression in the exact checked AST allocation. The
/// address is transient: callers bind it to a source coordinate before the
/// typed module is moved into the canonical source product.
pub(crate) fn checked_await_origins(
    module: &Module,
    table: &TypeTable,
) -> Result<BTreeMap<usize, Proof>, TypeError> {
    let definitions = type_definitions(module);
    let functions: Vec<_> = module
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Function(function) => Some(function),
            _ => None,
        })
        .collect();
    let mut summaries = BTreeMap::new();
    // A recursive value producer can construct an unbounded symbolic result
    // (for example, an iterator whose step contains the next iterator). Its
    // finite summary must lose result precision, never opt requirements. Mark
    // only the still-growing results Unknown and rerun their consumers. An
    // unstable requirement itself has no lawful widening and stays refused.
    let mut widened = BTreeSet::new();
    let mut converged = false;
    for _round in 0..=functions.len() {
        for _ in 0..128 {
            let mut next = summaries.clone();
            for function in &functions {
                let mut summary = analyze_function(module, table, &definitions, &summaries, function, false)?.0;
                if widened.contains(&function.name) { summary.result = Proof::Unknown; }
                next.insert(function.name.clone(), summary);
            }
            if next == summaries {
                converged = true;
                break;
            }
            summaries = next;
        }
        if converged { break; }
        let mut growing = BTreeSet::new();
        for function in &functions {
            let next = analyze_function(module, table, &definitions, &summaries, function, false)?.0;
            let prior = summaries.get(&function.name);
            if prior.is_some_and(|prior| prior.required_opt_inputs != next.required_opt_inputs) {
                return Err(TypeError {
                    message: format!("task execution provenance requirements did not converge in `{}`", function.name),
                });
            }
            if prior.is_none_or(|prior| prior.result != next.result) && !widened.contains(&function.name) {
                growing.insert(function.name.clone());
            }
        }
        if growing.is_empty() { break; }
        widened.extend(growing);
    }
    if !converged {
        return Err(TypeError { message: "task execution provenance summaries did not converge after bounded result widening".into() });
    }
    let mut awaits = BTreeMap::new();
    for function in functions {
        awaits.extend(analyze_function(module, table, &definitions, &summaries, function, true)?.1);
    }
    for item in &module.items {
        if let Item::Impl(definition) = item {
            for method in &definition.methods {
                if method.is_async {
                    awaits.extend(analyze_function(module, table, &definitions, &summaries, method, true)?.1);
                }
            }
        }
    }
    Ok(awaits)
}

pub(crate) fn verify(module: &Module, table: &TypeTable) -> Result<(), TypeError> {
    checked_await_origins(module, table).map(|_| ())
}
