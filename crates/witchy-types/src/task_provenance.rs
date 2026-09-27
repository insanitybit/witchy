//! Checked execution-mode provenance for task values (RFC-0167 §3.11).
//!
//! Summaries are symbolic in parameters, then instantiated at each direct
//! checked call. Values retain the proof through bindings and aggregates.
//! Unknown callbacks, dynamic dispatch, or projections fail closed at an opt
//! await; a result type alone cannot authorize opt execution.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use witchy_canonical_source::aggregates::type_definitions;
use witchy_canonical_source::storage::{StoredChildren, stored_children};
use witchy_canonical_source::source::{
    DeferredTaskRole, SourceExecutionMode, TaskCallableScope, TaskExecutionProof as Proof, source_execution_mode,
};
use witchy_syntax::ast::{Block, Convention, Expr, Function, Item, Module, Pattern, Stmt, Type, TypeDef, UnOp};
use witchy_syntax::lambda_scan::scan_lambda;

use crate::typeck::{Ty, TypeError, TypeTable, ty_to_ast};

type Environment = BTreeMap<String, Proof>;

#[derive(Clone, Copy)]
enum TaskInputStep { Slot(usize), VariantSlot(usize, usize), AnyElement }

/// Project actual invocation values through the shared checked stored-child
/// relation. A sum or mapping needs a conditional/keyed proof we do not have;
/// refuse that formal instead of treating its task child as an ordinary value.
fn task_input_paths(ty: &Type, definitions: &BTreeMap<String, TypeDef>) -> Result<Vec<Vec<TaskInputStep>>, ()> {
    fn walk(ty: &Type, definitions: &BTreeMap<String, TypeDef>,
        active: &mut HashSet<Type>, depth: usize, remaining: &mut usize) -> Result<Vec<Vec<TaskInputStep>>, ()> {
        if depth > 128 || *remaining == 0 { return Err(()); }
        *remaining -= 1;
        let source = ty.unqualified();
        if let Type::Named(name, arguments) = source
            && (name == "Task" || name == "task.Task") && arguments.len() == 1 {
            return Ok(vec![Vec::new()]);
        }
        if let Type::Fn(inputs, _, _, _) = source {
            for input in inputs {
                if !walk(input, definitions, active, depth + 1, remaining)?.is_empty() {
                    return Err(());
                }
            }
            return Ok(Vec::new());
        }
        if !active.insert(source.clone()) { return Err(()); }
        let result = match source {
            Type::Tuple(_) => match stored_children(source, definitions)? {
                Some(StoredChildren::Aggregate(fields)) => {
                    let mut paths = Vec::new();
                    for (index, field) in fields.iter().enumerate() {
                        for mut path in walk(field, definitions, active, depth + 1, remaining)? {
                            path.insert(0, TaskInputStep::Slot(index));
                            paths.push(path);
                        }
                    }
                    Ok(paths)
                }
                _ => Err(()),
            },
            Type::Named(name, _) => match stored_children(source, definitions)? {
                Some(StoredChildren::Sequence(element)) => {
                    let mut paths = walk(&element, definitions, active, depth + 1, remaining)?;
                    for path in &mut paths { path.insert(0, TaskInputStep::AnyElement); }
                    Ok(paths)
                }
                Some(StoredChildren::Mapping(key, value)) => {
                    if !walk(&key, definitions, active, depth + 1, remaining)?.is_empty()
                        || !walk(&value, definitions, active, depth + 1, remaining)?.is_empty() {
                        Err(())
                    } else { Ok(Vec::new()) }
                }
                Some(StoredChildren::Aggregate(fields)) => {
                    let mut paths = Vec::new();
                    let variants = definitions.get(name).ok_or(())?.variants.as_slice();
                    let mut offset = 0_usize;
                    for (ordinal, variant) in variants.iter().enumerate() {
                        if offset.checked_add(variant.fields.len()).is_none_or(|end| end > fields.len()) {
                            return Err(());
                        }
                        for index in 0..variant.fields.len() {
                            for mut path in walk(&fields[offset + index], definitions, active, depth + 1, remaining)? {
                                path.insert(0, TaskInputStep::VariantSlot(ordinal, index));
                                paths.push(path);
                            }
                        }
                        offset += variant.fields.len();
                    }
                    if offset != fields.len() { Err(()) } else { Ok(paths) }
                }
                None if name.chars().next().is_some_and(char::is_lowercase) && !name.contains('.') => Err(()),
                None => Ok(Vec::new()),
            },
            Type::Slice(inner) => {
                if walk(inner, definitions, active, depth + 1, remaining)?.is_empty() { Ok(Vec::new()) }
                else { Err(()) }
            }
            Type::Dyn(_, arguments) => {
                for argument in arguments {
                    if !walk(argument, definitions, active, depth + 1, remaining)?.is_empty() { return Err(()); }
                }
                Ok(Vec::new())
            }
            Type::RecordCompose { .. } => Err(()),
            Type::Qualified(_, _) | Type::Fn(..) => unreachable!("unqualified callable storage"),
        };
        active.remove(source);
        result
    }
    walk(ty, definitions, &mut HashSet::new(), 0, &mut 4_096)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Summary {
    result: Proof,
    required_opt_inputs: Vec<Proof>,
}

/// A declaration summary from the exact checked AST. Unlike the internal
/// fixpoint's body result, `result` is the value produced by calling it.
pub(crate) struct TaskBehaviorSummary {
    pub source_mode: SourceExecutionMode,
    pub conventions: Vec<Convention>,
    pub result: Proof,
    pub required_opt_inputs: Vec<Proof>,
}

impl TaskBehaviorSummary {
    fn from_summary(module: &Module, function: &Function, summary: Summary) -> Self {
        let source_mode = source_execution_mode(function, &module.modes);
        let result = if function.is_async {
            Proof::Task(source_mode, Box::new(summary.result))
        } else { summary.result };
        Self {
            source_mode,
            conventions: function.params.iter().map(|parameter| parameter.convention).collect(),
            result: result.bounded(4096),
            required_opt_inputs: summary.required_opt_inputs,
        }
    }
}

/// Pointer keys are transient identities in the exact checked AST allocation,
/// never serialized IDs. The pipeline binds both maps to checked source
/// coordinates before moving or transforming that AST.
pub(crate) struct CheckedTaskBehavior {
    pub await_origins: BTreeMap<usize, Proof>,
    pub declarations: BTreeMap<usize, TaskBehaviorSummary>,
}

pub(crate) struct LambdaBehaviorSummary {
    pub source_mode: SourceExecutionMode,
    pub captures: Vec<witchy_canonical_source::closure::ClosureBindingId>,
    pub conventions: Vec<Convention>,
    pub formal_scope: TaskCallableScope,
    pub result: Proof,
    pub required_opt_inputs: Vec<Proof>,
}

pub(crate) struct CheckedFinalBehavior {
    pub lambdas: BTreeMap<witchy_canonical_source::proper_tail::ProperTailCallableId, LambdaBehaviorSummary>,
    pub declarations: BTreeMap<usize, TaskBehaviorSummary>,
}

struct Analyzer<'a> {
    module: &'a Module,
    table: &'a TypeTable,
    definitions: &'a BTreeMap<String, TypeDef>,
    summaries: &'a BTreeMap<String, Summary>,
    function: &'a Function,
    check_await: bool,
    deferred_task_role: Option<&'a DeferredTaskRole>,
    export_execution_terms: bool,
    returns: Vec<Proof>,
    required_opt_inputs: Vec<Proof>,
    await_origins: BTreeMap<usize, Proof>,
    lambda_scopes: &'a BTreeMap<usize, TaskCallableScope>,
    active_lambda_scopes: Vec<TaskCallableScope>,
    lambda_parameter_scopes: Vec<BTreeSet<String>>,
    capture_owner: Option<&'a witchy_canonical_source::proper_tail::ProperTailCallableId>,
}

impl Analyzer<'_> {
    fn opt_callable_task_formal(&self, source: Option<&Type>) -> bool {
        let Some(Type::Fn(inputs, _, _, _)) = source.map(Type::unqualified) else { return false };
        inputs.iter().any(|input| task_input_paths(input, self.definitions)
            .map_or(true, |paths| !paths.is_empty()))
    }

    fn require_opaque_opt_callback_inputs(
        &mut self, callable: Option<&Ty>, proofs: &[Proof],
    ) -> Result<(), TypeError> {
        fn project(proof: Proof, step: TaskInputStep) -> Proof {
            match step {
                TaskInputStep::Slot(index) => proof.project_slot(index),
                TaskInputStep::VariantSlot(ordinal, index) => proof.project_variant_slot(ordinal, index),
                TaskInputStep::AnyElement => proof.project_any_element(),
            }
        }
        let Some(Ty::Fn(inputs, _, _, _, _)) = callable else {
            return Err(TypeError { message: format!(
                "in `{}`: opt callback invocation lacks a checked callable input type", self.function.name,
            ) });
        };
        if inputs.len() != proofs.len() {
            return Err(TypeError { message: format!(
                "in `{}`: opt callback invocation has an incomplete checked input contract", self.function.name,
            ) });
        }
        for (input, proof) in inputs.iter().zip(proofs) {
            let source = ty_to_ast(input).ok_or_else(|| TypeError { message: format!(
                "in `{}`: opt callback task input has no checked source type", self.function.name,
            ) })?;
            let paths = task_input_paths(&source, self.definitions).map_err(|()| TypeError { message: format!(
                "in `{}`: opt callback task input lacks a recursive stored-value proof", self.function.name,
            ) })?;
            for path in paths {
                let mut projected = proof.clone();
                for step in path {
                    projected = project(projected, step);
                }
                self.satisfy_opt_requirement(projected)?;
            }
        }
        Ok(())
    }

    fn opaque_result_may_hide_task_input(&self, expression: &Expr) -> bool {
        fn walk(ty: &Type, definitions: &BTreeMap<String, TypeDef>, parameter: bool,
            active: &mut HashSet<(Type, bool)>, depth: usize, remaining: &mut usize) -> bool {
            if depth > 128 || *remaining == 0 { return true; }
            *remaining -= 1;
            let source = ty.unqualified();
            // The same checked type in the same input/result position has no
            // further children to discover on a second visit. Growing generic
            // instantiations still reach the depth/node limit and refuse.
            let key = (source.clone(), parameter);
            if !active.insert(key.clone()) { return false; }
            let hidden = match source {
                Type::Named(name, arguments)
                    if (name == "Task" || name == "task.Task") && arguments.len() == 1 =>
                    parameter || walk(&arguments[0], definitions, false, active, depth + 1, remaining),
                Type::Fn(inputs, result, _, _) => inputs.iter().any(|input|
                    walk(input, definitions, true, active, depth + 1, remaining))
                    || walk(result, definitions, false, active, depth + 1, remaining),
                Type::Slice(inner) => walk(inner, definitions, parameter, active, depth + 1, remaining),
                source => match stored_children(source, definitions) {
                    Ok(Some(children)) => children.into_types().iter().any(|child|
                        walk(child, definitions, parameter, active, depth + 1, remaining)),
                    Ok(None) => false,
                    Err(()) => true,
                },
            };
            active.remove(&key);
            hidden
        }
        let Some(ty) = self.table.type_of(expression) else { return false };
        let Some(source) = ty_to_ast(ty) else { return matches!(ty, Ty::Fn(..)) };
        walk(&source, self.definitions, false, &mut HashSet::new(), 0, &mut 4_096)
    }

    fn unknown_opaque_result(&self, expression: &Expr) -> Proof {
        if self.check_await && self.opaque_result_may_hide_task_input(expression) {
            Proof::UnprovedCallableObligation
        } else { Proof::Unknown }
    }

    fn reject_opaque_callable_transfer(&self, proof: &Proof, boundary: &str) -> Result<(), TypeError> {
        if proof.has_any_callable_input_obligation() {
            return Err(TypeError { message: format!(
                "in `{}`: callback with an opt task input obligation cannot {boundary} without a checked callable contract",
                self.function.name,
            ) });
        }
        Ok(())
    }

    fn reject_local_callable_transfer(&self, proof: &Proof, boundary: &str) -> Result<(), TypeError> {
        if proof.has_local_callable_obligation() {
            return Err(TypeError { message: format!(
                "in `{}`: local callback with an opt task input obligation cannot {boundary} without a checked callable contract",
                self.function.name,
            ) });
        }
        Ok(())
    }

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
            return proof.invalidated();
        };
        let Some(definition) = self.definitions.get(owner.as_ref()) else {
            return proof.invalidated();
        };
        if definition.variants.len() != 2 || definition.variants[0].fields.len() != 1 {
            return proof.invalidated();
        }
        match proof {
            Proof::Variant { ordinal: 0, fields } if fields.len() == 1 => fields.into_iter().next().unwrap(),
            Proof::Variant { fields, .. } => if fields.iter().any(Proof::has_any_callable_input_obligation) {
                Proof::UnprovedCallableObligation
            } else { Proof::Unknown },
            Proof::Variants(mut variants) => {
                let tainted = variants.values().flatten().any(Proof::has_any_callable_input_obligation);
                variants.remove(&0)
                .and_then(|fields| (fields.len() == 1).then(|| fields.into_iter().next().unwrap()))
                .unwrap_or(if tainted { Proof::UnprovedCallableObligation } else { Proof::Unknown })
            }
            Proof::UnprovedCallableObligation => Proof::UnprovedCallableObligation,
            Proof::Parameter(_) | Proof::Slot(_, _) | Proof::VariantSlot(_, _, _)
                | Proof::AnyElement(_) => proof.project_variant_slot(0, 0),
            other => other.invalidated(),
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
        if matches!(pattern, Pattern::Ctor { .. } | Pattern::AnonCtor { .. })
            && let Proof::Step { value, .. } = proof {
            return Self::bind(pattern, *value, ordinal, environment);
        }
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
            (Pattern::Tuple(patterns), proof @ (Proof::Parameter(_) | Proof::Capture(..) | Proof::Slot(_, _)
                | Proof::VariantSlot(_, _, _) | Proof::AnyElement(_))) => {
                for (index, pattern) in patterns.iter().enumerate() {
                    Self::bind(
                        pattern,
                        proof.clone().project_slot(index),
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
            (Pattern::Ctor { args, .. } | Pattern::AnonCtor { args, .. }, proof @ Proof::Recipe { .. })
                if ordinal == Some(0) && args.len() == 1 => {
                Self::bind(&args[0], proof.project_variant_slot(0, 0), None, environment);
            }
            (Pattern::Ctor { args, .. } | Pattern::AnonCtor { args, .. }, Proof::Variants(mut variants)) => {
                let tainted = variants.values().flatten().any(Proof::has_any_callable_input_obligation);
                if let Some(values) = ordinal.and_then(|ordinal| variants.remove(&ordinal))
                    && values.len() == args.len()
                {
                    for (pattern, value) in args.iter().zip(values) {
                        Self::bind(pattern, value, None, environment);
                    }
                } else {
                    let mut names = Vec::new();
                    witchy_syntax::ast::pattern_binds(pattern, &mut names);
                    let fallback = if tainted {
                        Proof::UnprovedCallableObligation
                    } else { Proof::Unknown };
                    for name in names { environment.insert(name, fallback.clone()); }
                }
            }
            (Pattern::Ctor { args, .. } | Pattern::AnonCtor { args, .. },
                proof @ (Proof::Parameter(_) | Proof::Capture(..) | Proof::Slot(_, _) | Proof::VariantSlot(_, _, _)
                    | Proof::AnyElement(_)))
                if ordinal.is_some() => {
                let ordinal = ordinal.expect("checked above");
                for (index, pattern) in args.iter().enumerate() {
                    Self::bind(pattern, proof.clone().project_variant_slot(ordinal, index), None, environment);
                }
            }
            (pattern, proof) => {
                let mut names = Vec::new();
                witchy_syntax::ast::pattern_binds(pattern, &mut names);
                let fallback = if proof.has_any_callable_input_obligation() {
                    Proof::UnprovedCallableObligation
                } else { Proof::Unknown };
                for name in names {
                    environment.insert(name, fallback.clone());
                }
            }
        }
    }

    fn block(&mut self, block: &Block, environment: &mut Environment) -> Result<Proof, TypeError> {
        let mut tail = Proof::Unknown;
        for statement in &block.stmts {
            tail = match statement {
                Stmt::Let { name, value, .. } => {
                    let proof = self.expr(value, environment)?;
                    environment.insert(name.clone(), proof);
                    Proof::Unknown
                }
                Stmt::Assign { name, value, place_depth } => {
                    let proof = self.expr(value, environment)?;
                    if proof.has_any_callable_input_obligation()
                        && (*place_depth != 0
                            || self.function.params.iter().any(|parameter| parameter.name == *name)
                            || self.lambda_parameter_scopes.iter().any(|scope| scope.contains(name))
                            || !environment.contains_key(name))
                    {
                        self.reject_opaque_callable_transfer(&proof, "cross a write-back or projected assignment")?;
                    }
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
                    self.reject_local_callable_transfer(&proof, "escape through a return")?;
                    self.returns.push(proof);
                    Proof::Unknown
                }
                Stmt::Yield(value) => {
                    let proof = self.expr(value, environment)?;
                    self.reject_opaque_callable_transfer(&proof, "escape through a yield")?;
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
        let direct_target = if environment.contains_key(name)
            || self.table.local_callee_type(expression).is_some() { None } else {
            self.module.items.iter().find_map(|item| match item {
                Item::Function(function) if function.name == name => Some(function),
                _ => None,
            })
        };
        for (index, proof) in proofs.iter().enumerate() {
            let checked_opt_formal = direct_target.is_some_and(|function|
                source_execution_mode(function, &self.module.modes) == SourceExecutionMode::Opt
                    && function.params.get(index).is_some_and(|param|
                        self.opt_callable_task_formal(param.ty.as_ref())));
            let exact_callback = matches!(proof, Proof::DeclaredCallable { .. } | Proof::Callable { .. })
                || proof.is_opaque_opt_callable_projection();
            if !checked_opt_formal || !exact_callback {
                self.reject_opaque_callable_transfer(proof, "cross a call argument")?;
            }
        }
        // The checker records local callable resolution at the exact call
        // expression. A shadowing value cannot borrow same-spelled declaration
        // authority, even if its signature happens to match.
        if environment.contains_key(name) || self.table.local_callee_type(expression).is_some() {
            match environment.get(name).cloned() {
                Some(Proof::Callable { scope, parameters, required_opt_inputs, result, .. }) if parameters == proofs.len() => {
                    for requirement in required_opt_inputs {
                        self.satisfy_opt_requirement(requirement.instantiate_callback(&scope, &proofs))?;
                    }
                    let result = result.instantiate_callback(&scope, &proofs).bounded(4096);
                    // A closure may change its captures when called. Its
                    // current result was checked above, but prior captured
                    // proofs need a fresh dataflow judgment after invocation.
                    environment.values_mut().for_each(|proof| *proof = proof.invalidated());
                    return Ok(result);
                }
                Some(Proof::DeclaredCallable { conventions, required_opt_inputs, result, .. }) if conventions.len() == proofs.len() => {
                    for requirement in required_opt_inputs {
                        self.satisfy_opt_requirement(requirement.instantiate(&proofs))?;
                    }
                    let result = result.instantiate(&proofs).bounded(4096);
                    self.invalidate_arguments(args, Some(&conventions), environment);
                    return Ok(result);
                }
                Some(proof) if proof.is_opaque_opt_callable_projection()
                    && source_execution_mode(self.function, &self.module.modes) == SourceExecutionMode::Opt => {
                    let callable = self.table.local_callee_type(expression).cloned();
                    self.require_opaque_opt_callback_inputs(callable.as_ref(), &proofs)?;
                    environment.values_mut().for_each(|value| *value = value.invalidated());
                    return Ok(self.unknown_opaque_result(expression));
                }
                Some(proof) if proof.has_local_callable_obligation() => {
                    self.reject_local_callable_transfer(&proof, "invoke after provenance was lost")?;
                }
                _ => {}
            }
            let callable = environment.get(name).cloned().unwrap_or(Proof::Unknown);
            self.invalidate_arguments(args, None, environment);
            return Ok(if self.export_execution_terms && callable != Proof::Unknown {
                Proof::Invocation { callable: Box::new(callable), arguments: proofs }.bounded(4096)
            } else { Proof::Unknown });
        }
        let Some(function) = direct_target else {
            self.invalidate_arguments(args, None, environment);
            return Ok(self.unknown_opaque_result(expression));
        };
        if proofs.len() != function.params.len() {
            self.invalidate_arguments(args, None, environment);
            return Ok(Proof::Unknown);
        }
        if let Some(summary) = self.summaries.get(name) {
            for requirement in &summary.required_opt_inputs {
                self.satisfy_opt_requirement(requirement.instantiate(&proofs))?;
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

    /// A requirement from one checked sum arm is vacuous only when the exact
    /// argument proves that arm inactive. An actual await still goes through
    /// `require_opt_task` and cannot execute an inactive or unknown value.
    fn satisfy_opt_requirement(&mut self, proof: Proof) -> Result<(), TypeError> {
        if proof == Proof::InactiveVariant { return Ok(()); }
        self.require_opt_task(proof).map(|_| ())
    }

    fn require_opt_task(&mut self, proof: Proof) -> Result<Proof, TypeError> {
        match proof {
            Proof::Task(SourceExecutionMode::Opt, result) => Ok(*result),
            proof if proof.is_parameter_projection()
                || self.capture_owner.is_some_and(|owner| proof.is_capture_projection(owner))
                || self.active_lambda_scopes.iter().any(|scope|
                    proof.is_callable_parameter_projection(scope)) => {
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
                        (Some(function), Some(summary)) => {
                            let result = summary.result.clone();
                            let result = if function.is_async {
                                Proof::Task(source_execution_mode(function, &self.module.modes), Box::new(result))
                            } else { result };
                            Proof::DeclaredCallable {
                                execution_mode: source_execution_mode(function, &self.module.modes),
                                conventions: function.params.iter().map(|parameter| parameter.convention).collect(),
                                required_opt_inputs: summary.required_opt_inputs.clone(),
                                result: Box::new(result.bounded(4096)),
                            }
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
                    let proof = self.expr(value, environment)?;
                    self.reject_opaque_callable_transfer(&proof, "cross a labeled call argument")?;
                }
                let conventions = self.callable_conventions(self.table.local_callee_type(expression), args.len());
                self.invalidate_arguments(args.iter().map(|(_, value)| value), conventions.as_deref(), environment);
                self.unknown_opaque_result(expression)
            }
            Expr::List(values) | Expr::Tuple(values) => {
                let values = values
                    .iter()
                    .map(|value| self.expr(value, environment))
                    .collect::<Result<Vec<_>, _>>()?;
                for value in &values {
                    self.reject_local_callable_transfer(value, "enter aggregate storage")?;
                }
                // A task constructor stores an executable closure. Its body
                // mode needs a separate callable proof; the constructor's
                // source spelling cannot mint one.
                if self.task_result(expression) {
                    for value in &values {
                        self.reject_opaque_callable_transfer(value, "enter a task recipe without a callable contract")?;
                    }
                    Proof::Unknown
                } else {
                    Proof::Elements(values)
                }
            }
            Expr::Ctor { name, args } | Expr::AnonCtor { tag: name, args } => {
                let values = args.iter().map(|value| self.expr(value, environment))
                    .collect::<Result<Vec<_>, _>>()?;
                for value in &values {
                    self.reject_local_callable_transfer(value, "enter aggregate storage")?;
                }
                let recipe_role = self.deferred_task_role.filter(|role|
                    matches!(self.table.type_of(expression), Some(Ty::Named(actual, parameters))
                        if actual.as_ref() == role.task_name() && parameters.len() == 1)
                    && self.checked_variant_ordinal(expression, name) == Some(0));
                if let (Some(role), [poll]) = (recipe_role, values.as_slice()) {
                    self.reject_opaque_callable_transfer(poll, "enter a task recipe without a callable contract")?;
                    return Ok(Proof::Recipe { role: role.task_identity().clone(), poll: Box::new(poll.clone()) });
                }
                if let Some(role) = self.deferred_task_role
                    && matches!(self.table.type_of(expression), Some(Ty::Named(actual, parameters))
                        if actual.as_ref() == role.step_name() && parameters.len() == 1)
                    && let Some(ordinal) = self.checked_variant_ordinal(expression, name)
                    && role.step_variants().get(&ordinal).is_some_and(|fields| fields.len() == values.len())
                {
                    return Ok(Proof::Step { role: role.step_identity().clone(),
                        value: Box::new(Proof::Variant { ordinal, fields: values }) });
                }
                if self.task_result(expression) {
                    for value in &values {
                        self.reject_opaque_callable_transfer(value, "enter a task recipe without a callable contract")?;
                    }
                    Proof::Unknown
                } else {
                    let tainted = values.iter().any(Proof::has_any_callable_input_obligation);
                    self.checked_variant_ordinal(expression, name)
                        .map(|ordinal| Proof::Variant { ordinal, fields: values })
                        .unwrap_or(if tainted { Proof::UnprovedCallableObligation } else { Proof::Unknown })
                }
            }
            Expr::Record { fields, spread, .. } => {
                let mut values = if let Some(spread) = spread {
                    let source = self.expr(spread, environment)?;
                    self.reject_local_callable_transfer(&source, "enter aggregate storage")?;
                    match source {
                        Proof::Fields(fields) => fields,
                        _ => BTreeMap::new(),
                    }
                } else {
                    BTreeMap::new()
                };
                for (name, value) in fields {
                    let proof = self.expr(value, environment)?;
                    self.reject_local_callable_transfer(&proof, "enter aggregate storage")?;
                    values.insert(name.clone(), proof);
                }
                Proof::Fields(values)
            }
            Expr::RecordUpdate { base, fields, .. } => {
                let source = self.expr(base, environment)?;
                self.reject_local_callable_transfer(&source, "enter aggregate storage")?;
                let mut values = match source {
                    Proof::Fields(fields) => fields,
                    _ => BTreeMap::new(),
                };
                for (name, value) in fields {
                    let proof = self.expr(value, environment)?;
                    self.reject_local_callable_transfer(&proof, "enter aggregate storage")?;
                    values.insert(name.clone(), proof);
                }
                Proof::Fields(values)
            }
            Expr::Field { base, field } => match self.expr(base, environment)? {
                Proof::UnprovedCallableObligation => Proof::UnprovedCallableObligation,
                Proof::Fields(mut fields) => fields.remove(field).unwrap_or(Proof::Unknown),
                proof => {
                    if matches!(self.table.type_of(base), Some(Ty::Tuple(_))) {
                        field.parse::<usize>().ok()
                            .map(|index| proof.project_slot(index))
                            .unwrap_or(Proof::Unknown)
                    } else {
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
                        index.map(|index| proof.project_variant_slot(0, index))
                            .unwrap_or(Proof::Unknown)
                    }
                }
            },
            Expr::Index { base, index } => {
                let base = self.expr(base, environment)?;
                self.expr(index, environment)?;
                base.project_any_element()
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
                        Proof::UnprovedCallableObligation => Proof::UnprovedCallableObligation,
                        _ => Proof::Unknown,
                    }
                }
            }
            Expr::Unary { op: UnOp::BorrowMut, expr } => {
                let proof = self.expr(expr, environment)?;
                self.reject_opaque_callable_transfer(&proof, "enter an exclusive reference")?;
                // The checker represents an explicit exclusive reference by
                // its referent type. Its apparent replayable Task type cannot
                // prove that the original binding survives writes through
                // this handle, even when a callee's formal says `let`.
                if let Some(root) = Self::place_root(expr) {
                    environment.insert(root.to_owned(), Proof::Unknown);
                }
                Proof::Unknown
            }
            Expr::Unary { op, expr } => {
                let proof = self.expr(expr, environment)?;
                if matches!(op, UnOp::Borrow | UnOp::Deref) {
                    self.reject_opaque_callable_transfer(&proof, "cross a reference conversion")?;
                } else {
                    self.reject_local_callable_transfer(&proof, "cross a unary value conversion")?;
                }
                proof
            }
            Expr::As { expr, .. }
            | Expr::ExistentialPack { expr, .. }
            | Expr::ExistentialUpcast { expr, .. } => {
                let proof = self.expr(expr, environment)?;
                self.reject_opaque_callable_transfer(&proof, "cross a cast or erased value")?;
                proof
            }
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
                for capture in scan_lambda(params, body).captures() {
                    if let Some(proof) = environment.get(&capture) {
                        self.reject_opaque_callable_transfer(proof, "enter another closure capture")?;
                    }
                }
                let mut nested = environment.clone();
                for (index, parameter) in params.iter().enumerate() {
                    nested.insert(parameter.name.clone(), Proof::CallableParameter(scope.clone(), index));
                }
                // Audit deferred bodies even when the closure itself is only
                // stored. Their effects run under this source declaration.
                let old_returns = std::mem::take(&mut self.returns);
                let old_requirements = std::mem::take(&mut self.required_opt_inputs);
                self.active_lambda_scopes.push(scope.clone());
                self.lambda_parameter_scopes.push(params.iter().map(|param| param.name.clone()).collect());
                let tail = self.block(body, &mut nested)?;
                self.lambda_parameter_scopes.pop();
                self.active_lambda_scopes.pop();
                let mut nested_returns = std::mem::replace(&mut self.returns, old_returns);
                if !matches!(body.stmts.last(), Some(Stmt::Return(_))) {
                    self.reject_local_callable_transfer(&tail, "escape through a callback result")?;
                    nested_returns.push(tail);
                }
                let nested_requirements =
                    std::mem::replace(&mut self.required_opt_inputs, old_requirements);
                let mut required_opt_inputs = Vec::new();
                for requirement in nested_requirements {
                    if requirement.is_callable_parameter_projection(&scope) {
                        if !required_opt_inputs.contains(&requirement) {
                            required_opt_inputs.push(requirement);
                        }
                    } else if (requirement.is_parameter_projection()
                        || self.active_lambda_scopes.iter().any(|outer|
                            requirement.is_callable_parameter_projection(outer)))
                        && !self.required_opt_inputs.contains(&requirement) {
                        self.required_opt_inputs.push(requirement);
                    } else if !requirement.is_parameter_projection()
                        && !self.active_lambda_scopes.iter().any(|outer|
                            requirement.is_callable_parameter_projection(outer)) {
                        return Err(TypeError { message: format!(
                            "in `{}`: callback task input obligation has no checked formal scope",
                            self.function.name,
                        ) });
                    }
                }
                Proof::Callable {
                    scope,
                    execution_mode: source_execution_mode(self.function, &self.module.modes),
                    parameters: params.len(),
                    required_opt_inputs,
                    result: Box::new(Self::join_values(nested_returns).bounded(4096)),
                }
            }
            Expr::Apply { func, args } => {
                let callable = self.expr(func, environment)?;
                let proofs = args.iter().map(|arg| self.expr(arg, environment))
                    .collect::<Result<Vec<_>, _>>()?;
                for proof in &proofs {
                    self.reject_opaque_callable_transfer(proof, "cross a callback argument")?;
                }
                match callable {
                    Proof::Callable { scope, parameters, required_opt_inputs, result, .. } if parameters == args.len() => {
                        for requirement in required_opt_inputs {
                            self.satisfy_opt_requirement(requirement.instantiate_callback(&scope, &proofs))?;
                        }
                        environment.values_mut().for_each(|proof| *proof = proof.invalidated());
                        result.instantiate_callback(&scope, &proofs).bounded(4096)
                    }
                    Proof::DeclaredCallable { conventions, required_opt_inputs, result, .. } if conventions.len() == args.len() => {
                        for requirement in required_opt_inputs {
                            self.satisfy_opt_requirement(requirement.instantiate(&proofs))?;
                        }
                        self.invalidate_arguments(args, Some(&conventions), environment);
                        result.instantiate(&proofs).bounded(4096)
                    }
                    other if other.is_opaque_opt_callable_projection()
                        && source_execution_mode(self.function, &self.module.modes) == SourceExecutionMode::Opt => {
                        let callable_type = self.table.type_of(func).cloned();
                        self.require_opaque_opt_callback_inputs(callable_type.as_ref(), &proofs)?;
                        environment.values_mut().for_each(|value| *value = value.invalidated());
                        self.unknown_opaque_result(expression)
                    }
                    other => {
                        self.reject_local_callable_transfer(&other, "invoke after provenance was lost")?;
                        environment.values_mut().for_each(|proof| *proof = proof.invalidated());
                        if self.export_execution_terms && other != Proof::Unknown {
                            Proof::Invocation { callable: Box::new(other), arguments: proofs }.bounded(4096)
                        } else { Proof::Unknown }
                    }
                }
            }
            Expr::MethodCall { receiver, args, .. } => {
                let proof = self.expr(receiver, environment)?;
                self.reject_opaque_callable_transfer(&proof, "cross a method receiver")?;
                for arg in args {
                    let proof = self.expr(arg, environment)?;
                    self.reject_opaque_callable_transfer(&proof, "cross a method argument")?;
                }
                let conventions = self.callable_conventions(self.table.local_callee_type(expression), args.len() + 1);
                self.invalidate_arguments(
                    std::iter::once(receiver.as_ref()).chain(args.iter()),
                    conventions.as_deref(),
                    environment,
                );
                self.unknown_opaque_result(expression)
            }
            Expr::LabeledMethodCall { receiver, args, .. } => {
                let proof = self.expr(receiver, environment)?;
                self.reject_opaque_callable_transfer(&proof, "cross a method receiver")?;
                for (_, arg) in args {
                    let proof = self.expr(arg, environment)?;
                    self.reject_opaque_callable_transfer(&proof, "cross a method argument")?;
                }
                let conventions = self.callable_conventions(self.table.local_callee_type(expression), args.len() + 1);
                self.invalidate_arguments(
                    std::iter::once(receiver.as_ref()).chain(args.iter().map(|(_, value)| value)),
                    conventions.as_deref(),
                    environment,
                );
                self.unknown_opaque_result(expression)
            }
            Expr::ExistentialCall { receiver, args, .. } => {
                let proof = self.expr(receiver, environment)?;
                self.reject_opaque_callable_transfer(&proof, "cross existential dispatch")?;
                for arg in args {
                    let proof = self.expr(arg, environment)?;
                    self.reject_opaque_callable_transfer(&proof, "cross existential dispatch")?;
                }
                let conventions = self.callable_conventions(self.table.local_callee_type(expression), args.len() + 1);
                self.invalidate_arguments(
                    std::iter::once(receiver.as_ref()).chain(args.iter()),
                    conventions.as_deref(),
                    environment,
                );
                self.unknown_opaque_result(expression)
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
                // The propagated failure is absent from the ordinary result
                // summary, so an obligation on either variant cannot escape.
                self.reject_opaque_callable_transfer(&proof, "propagate through `?` without a checked error contract")?;
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
                    let element = source.project_any_element();
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
    deferred_task_role: Option<&DeferredTaskRole>,
    export_execution_terms: bool,
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
        deferred_task_role,
        export_execution_terms,
        returns: Vec::new(),
        required_opt_inputs: Vec::new(),
        await_origins: BTreeMap::new(),
        lambda_scopes: &lambda_scopes,
        active_lambda_scopes: Vec::new(),
        lambda_parameter_scopes: Vec::new(),
        capture_owner: None,
    };
    let mut environment = function
        .params
        .iter()
        .enumerate()
        .map(|(index, param)| {
            let proof = if source_execution_mode(function, &module.modes) == SourceExecutionMode::Opt
                && analyzer.opt_callable_task_formal(param.ty.as_ref()) {
                Proof::OpaqueOptCallableParameter(index)
            } else { Proof::Parameter(index) };
            (param.name.clone(), proof)
        })
        .collect();
    let tail = analyzer.block(&function.body, &mut environment)?;
    if !matches!(function.body.stmts.last(), Some(Stmt::Return(_))) {
        analyzer.reject_local_callable_transfer(&tail, "escape through a result")?;
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
    analyze_task_behavior(module, table, false, None).map(|behavior| behavior.await_origins)
}

pub(crate) fn checked_task_behavior(
    module: &Module,
    table: &TypeTable,
    deferred_task_role: Option<&DeferredTaskRole>,
) -> Result<CheckedTaskBehavior, TypeError> {
    analyze_task_behavior(module, table, true, deferred_task_role)
}

/// Derive contracts on the final typed AST, whose lambda coordinates are the
/// coordinates carried by the closure witness. Failure to summarize a body
/// leaves its contract absent; it never changes source acceptance or grants
/// execution from its signature. Existing source verification runs separately.
pub(crate) fn checked_final_execution_contracts(
    typed: &crate::typeck::TypedModule,
    witness: &witchy_canonical_source::closure::ClosureWitnessManifest,
    sites: &[crate::canonical_closure::LambdaSite],
    deferred_task_role: Option<&DeferredTaskRole>,
) -> CheckedFinalBehavior {
    let module = typed.module();
    let table = typed.table();
    let definitions = type_definitions(module);
    let functions = module.items.iter().filter_map(|item| match item {
        Item::Function(function) if !function.is_async => Some((function.name.as_str(), function)),
        _ => None,
    }).collect::<BTreeMap<_, _>>();
    let mut demand = LambdaSummaryDemand {
        module, table, definitions: &definitions, functions, deferred_task_role,
        summaries: BTreeMap::new(), unavailable: BTreeSet::new(), active: BTreeSet::new(),
    };
    // Index each enclosing body once. Do not rerun the lexical inventory for
    // every captured lambda in a large function.
    let mut expressions = BTreeMap::new();
    let mut scopes_by_function = BTreeMap::new();
    let checked_scopes = sites.iter().map(|site|
        (site.expr_address, TaskCallableScope::checked_closure(site.id.clone()))).collect::<BTreeMap<_, _>>();
    for item in &module.items {
        let functions: &[Function] = match item {
            Item::Function(function) => std::slice::from_ref(function),
            Item::Impl(definition) => &definition.methods,
            _ => continue,
        };
        for function in functions {
            let mut scopes = BTreeMap::new();
            let walked = witchy_syntax::ast::visit::visit_block(&function.body, &mut |value| {
                if matches!(value, Expr::Lambda { .. }) {
                    let address = value as *const Expr as usize;
                    scopes.insert(address, checked_scopes.get(&address).cloned().ok_or(())?);
                    expressions.insert(address, value);
                }
                Ok::<(), ()>(())
            });
            if walked.is_ok() { scopes_by_function.insert(function as *const Function as usize, scopes); }
        }
    }
    let mut contracts = BTreeMap::new();
    for entry in witness.entries() {
        let Some(site) = sites.iter().find(|site| &site.id == entry.id()) else { continue };
        let function = match (module.items.get(entry.id().item as usize), entry.id().method) {
            (Some(Item::Function(function)), None) => function,
            (Some(Item::Impl(definition)), Some(method)) => {
                let Some(function) = definition.methods.get(method as usize) else { continue };
                function
            }
            _ => continue,
        };
        let Some(lambda_scopes) = scopes_by_function.get(&(function as *const Function as usize)) else { continue };
        let Some(Expr::Lambda { params, body, .. }) = expressions.get(&site.expr_address).copied() else { continue };
        let Some(scope) = lambda_scopes.get(&site.expr_address).cloned() else { continue };
        if demand.body(body, &mut 4096, 0).is_err() { continue; }
        let mut environment = entry.captures().iter().enumerate().map(|(index, capture)|
            (capture.binding().name().to_owned(), Proof::Capture(entry.id().clone(), index))).collect::<Environment>();
        for (index, parameter) in params.iter().enumerate() {
            environment.insert(parameter.name.clone(), Proof::CallableParameter(scope.clone(), index));
        }
        let mut analyzer = Analyzer {
            module, table, definitions: &definitions, summaries: &demand.summaries, function,
            check_await: false, deferred_task_role, export_execution_terms: true,
            returns: Vec::new(), required_opt_inputs: Vec::new(), await_origins: BTreeMap::new(),
            lambda_scopes, active_lambda_scopes: vec![scope.clone()],
            lambda_parameter_scopes: vec![params.iter().map(|param| param.name.clone()).collect()],
            capture_owner: Some(entry.id()),
        };
        let Ok(tail) = analyzer.block(body, &mut environment) else { continue };
        if !matches!(body.stmts.last(), Some(Stmt::Return(_))) { analyzer.returns.push(tail); }
        let result = Analyzer::join_values(analyzer.returns).bounded(4096);
        contracts.insert(entry.id().clone(), LambdaBehaviorSummary {
            source_mode: source_execution_mode(function, &module.modes),
            captures: entry.captures().iter().map(|capture| capture.binding().clone()).collect(),
            conventions: params.iter().map(|param| param.convention).collect(), formal_scope: scope,
            result, required_opt_inputs: analyzer.required_opt_inputs,
        });
    }
    // Export only successful, independently derived dependencies already
    // needed by these exact lambdas. No additional whole-module analysis and
    // no identity inferred from a specialization's printed name.
    let declarations = demand.summaries.into_iter().filter_map(|(name, summary)| {
        let function = demand.functions.get(name.as_str())?;
        Some((*function as *const Function as usize,
            TaskBehaviorSummary::from_summary(module, function, summary)))
    }).collect();
    CheckedFinalBehavior { lambdas: contracts, declarations }
}

/// Only exact checked call targets needed by a particular final lambda are
/// summarized. A failed or recursive target never enters the available map.
/// This is deliberately conservative, not a second whole-module fixpoint or
/// a spelling-based whitelist for standard scheduler helpers.
struct LambdaSummaryDemand<'a> {
    module: &'a Module,
    table: &'a TypeTable,
    definitions: &'a BTreeMap<String, TypeDef>,
    functions: BTreeMap<&'a str, &'a Function>,
    deferred_task_role: Option<&'a DeferredTaskRole>,
    summaries: BTreeMap<String, Summary>,
    unavailable: BTreeSet<String>,
    active: BTreeSet<String>,
}

impl LambdaSummaryDemand<'_> {
    fn body(&mut self, body: &Block, remaining: &mut usize, depth: usize) -> Result<(), ()> {
        if depth > 128 || *remaining == 0 { return Err(()); }
        *remaining -= 1;
        let mut targets = BTreeSet::new();
        witchy_syntax::ast::visit::visit_block(body, &mut |expression| {
            match expression {
                Expr::Call { name, .. } if self.table.local_callee_type(expression).is_none() => {
                    if !self.functions.contains_key(name.as_str()) { return Err(()); }
                    targets.insert(name.clone());
                }
                Expr::Var(name) if self.table.declared_function_value(expression) == Some(name.as_str()) => {
                    if !self.functions.contains_key(name.as_str()) { return Err(()); }
                    targets.insert(name.clone());
                }
                _ => {}
            }
            Ok::<(), ()>(())
        })?;
        for target in targets { self.function(&target, remaining, depth + 1)?; }
        Ok(())
    }

    fn function(&mut self, name: &str, remaining: &mut usize, depth: usize) -> Result<(), ()> {
        if self.summaries.contains_key(name) { return Ok(()); }
        if self.unavailable.contains(name) || !self.active.insert(name.to_owned()) { return Err(()); }
        let result = (|| {
            let function = *self.functions.get(name).ok_or(())?;
            self.body(&function.body, remaining, depth)?;
            analyze_function(self.module, self.table, self.definitions, &self.summaries,
                function, false, self.deferred_task_role, true).map(|(summary, _)| summary).map_err(|_| ())
        })();
        self.active.remove(name);
        match result {
            Ok(summary) => { self.summaries.insert(name.to_owned(), summary); Ok(()) }
            Err(()) => { self.unavailable.insert(name.to_owned()); Err(()) }
        }
    }
}

fn analyze_task_behavior(
    module: &Module,
    table: &TypeTable,
    export_declarations: bool,
    deferred_task_role: Option<&DeferredTaskRole>,
) -> Result<CheckedTaskBehavior, TypeError> {
    let definitions = type_definitions(module);
    let functions: Vec<_> = module
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Function(function) => Some(function),
            _ => None,
        })
        .collect();
    // Summary lookup reads only names from direct calls and function values.
    // A summary change therefore requires another pass only through bodies
    // that mention that name. Scan nested lambdas too: their bodies can read
    // the same summaries when analyzed with the enclosing function. Duplicate
    // declaration names keep the original full scan because later inserts
    // overwrite earlier entries in the name-keyed summary map.
    let mut names = HashSet::new();
    let selective = functions
        .iter()
        .all(|function| names.insert(function.name.as_str()));
    let mut dependents = BTreeMap::<String, BTreeSet<usize>>::new();
    if selective {
        for (index, function) in functions.iter().enumerate() {
            let mut referenced = BTreeSet::new();
            witchy_syntax::ast::visit::visit_block(&function.body, &mut |expression| {
                match expression {
                    Expr::Call { name, .. } | Expr::Var(name) => {
                        referenced.insert(name.clone());
                    }
                    _ => {}
                }
                Ok::<(), TypeError>(())
            })?;
            for name in referenced {
                dependents.entry(name).or_default().insert(index);
            }
        }
    }
    let mut summaries = BTreeMap::new();
    // A recursive value producer can construct an unbounded symbolic result
    // (for example, an iterator whose step contains the next iterator). Its
    // finite summary must lose result precision, never opt requirements. Mark
    // only the still-growing results Unknown and rerun their consumers. An
    // unstable requirement itself has no lawful widening and stays refused.
    let mut widened = BTreeSet::new();
    let mut converged = false;
    for _round in 0..=functions.len() {
        let mut dirty = (0..functions.len()).collect::<BTreeSet<_>>();
        // A unary recursive result reaches the depth boundary on round 128;
        // the following round observes the stable truncated child.
        for _ in 0..=128 {
            let mut next = summaries.clone();
            let mut changed = BTreeSet::new();
            for (index, function) in functions.iter().enumerate() {
                if !dirty.contains(&index) {
                    continue;
                }
                let mut summary = analyze_function(module, table, &definitions, &summaries, function, false, deferred_task_role, export_declarations)?.0;
                if widened.contains(&function.name) {
                    summary.result = summary.result.invalidated();
                }
                let differs = next.get(&function.name) != Some(&summary);
                next.insert(function.name.clone(), summary);
                if differs {
                    changed.insert(function.name.clone());
                }
            }
            if next == summaries {
                converged = true;
                break;
            }
            summaries = next;
            if selective {
                dirty = changed
                    .into_iter()
                    .filter_map(|name| dependents.get(&name))
                    .flat_map(|users| users.iter().copied()).collect();
                if dirty.is_empty() {
                    converged = true;
                    break;
                }
            }
        }
        if converged { break; }
        let mut growing = BTreeSet::new();
        for function in &functions {
            let next = analyze_function(module, table, &definitions, &summaries, function, false, deferred_task_role, export_declarations)?.0;
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
    for function in &functions {
        awaits.extend(analyze_function(module, table, &definitions, &summaries, function, true, deferred_task_role, export_declarations)?.1);
    }
    let mut declarations = BTreeMap::new();
    for item in &module.items {
        if let Item::Impl(definition) = item {
            for method in &definition.methods {
                if method.is_async {
                    let (summary, origins) = analyze_function(module, table, &definitions, &summaries, method, true, deferred_task_role, export_declarations)?;
                    awaits.extend(origins);
                    if export_declarations {
                        declarations.insert(method as *const Function as usize,
                            TaskBehaviorSummary::from_summary(module, method, summary));
                    }
                }
            }
        }
    }
    if export_declarations {
        for function in functions {
            // Export the converged result, including any conservative widening,
            // rather than a fresh unfolding of a recursive body. The separate
            // validation pass above still checks every opt requirement.
            let summary = summaries.remove(&function.name).ok_or_else(|| TypeError {
                message: format!("checked task behavior lacks declaration `{}`", function.name),
            })?;
            declarations.insert(function as *const Function as usize,
                TaskBehaviorSummary::from_summary(module, function, summary));
        }
    }
    Ok(CheckedTaskBehavior { await_origins: awaits, declarations })
}

pub(crate) fn verify(module: &Module, table: &TypeTable) -> Result<(), TypeError> {
    checked_await_origins(module, table).map(|_| ())
}
