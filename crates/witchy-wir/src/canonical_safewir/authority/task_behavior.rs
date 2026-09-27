//! Candidate-side correspondence for source-proved deferred behavior.
//!
//! A source mode or result type is not a property of an arbitrary WIR value.
//! Facts below originate at an exact checked call target and travel through
//! the actual SSA operands and layout-derived aggregate storage. Unknown
//! transport does not acquire execution authority.

use std::collections::{BTreeMap, BTreeSet};

use witchy_canonical_source::source::{CanonicalSourceView, SourceExecutionMode, TaskExecutionProof};
use witchy_canonical_source::closure::ClosureBindingPathStep;
use witchy_syntax::ast::Type;

use super::{CandidateAbiBindingError, CanonicalModule, ModuleStage};
use crate::canonical_safewir::{
    CallArgument, Function, FunctionId, Operation, Projection, Terminator, ValueId, ValueType,
    closure_owner_contract::OwnerOperation,
    closure_owner_contract::OwnerFamilySource,
};
use crate::layout::{FieldKind, LayoutId, LayoutInterner, LayoutKind};

#[path = "task_behavior/value_graph.rs"]
mod value_graph;
use value_graph::{Node, ValueGraph};

type BoundFunctions<'a> = BTreeMap<FunctionId, &'a Function>;

fn call_value(argument: &CallArgument) -> ValueId {
    match argument { CallArgument::Value(value) | CallArgument::Move(value) => *value }
}

#[derive(Default)]
struct Flow {
    tasks: BTreeMap<ValueId, TaskExecutionProof>,
    aliases: BTreeMap<ValueId, ValueId>,
    slots: BTreeMap<(ValueId, u32), TaskExecutionProof>,
    callables: BTreeMap<ValueId, FunctionId>,
    callable_slots: BTreeMap<(ValueId, u32), FunctionId>,
    owner_captures: BTreeMap<(ValueId, usize), TaskExecutionProof>,
}

impl Flow {
    fn root(&self, value: ValueId) -> ValueId {
        self.aliases.get(&value).copied().unwrap_or(value)
    }

    fn task(&self, value: ValueId) -> TaskExecutionProof {
        self.tasks.get(&value).cloned().unwrap_or(TaskExecutionProof::Unknown)
    }

    fn task_with_fields(
        &self, value: ValueId, function: &Function, layouts: &LayoutInterner,
    ) -> TaskExecutionProof {
        let direct = self.task(value);
        let root = self.root(value);
        let Some(ValueType::Ref(layout)) = function.value_types.get(&root) else {
            return direct;
        };
        let Some(descriptor) = layouts.resolve_nominal(*layout).ok().and_then(|id| layouts.get(id)) else {
            return direct;
        };
        if !matches!(descriptor.kind(), LayoutKind::PackedRecord { .. } | LayoutKind::Tuple { .. }) {
            return direct;
        }
        let fields = (0..descriptor.fields().len())
            .map(|index| self.slots.get(&(root, index as u32)).cloned())
            .collect::<Option<Vec<_>>>();
        match (descriptor.kind(), fields) {
            // A packed Task carrier has already been authenticated as an
            // executable Task. Reinterpreting its storage as an arbitrary
            // record would erase that authority at the opt call boundary.
            (LayoutKind::PackedRecord { .. }, _) if matches!(&direct, TaskExecutionProof::Task(..)) =>
                direct,
            (LayoutKind::PackedRecord { .. }, Some(fields)) =>
                TaskExecutionProof::Variant { ordinal: 0, fields },
            (LayoutKind::Tuple { .. }, Some(fields)) => TaskExecutionProof::Elements(fields),
            _ => direct,
        }
    }

    fn set_task(&mut self, value: ValueId, proof: TaskExecutionProof) {
        self.tasks.insert(value, proof);
    }

    fn invalidate_root(&mut self, root: ValueId) {
        self.slots.retain(|(object, _), _| *object != root);
        self.callable_slots.retain(|(object, _), _| *object != root);
        self.invalidate_root_task(root);
    }

    fn invalidate_root_task(&mut self, root: ValueId) {
        let affected = self.aliases.iter().filter_map(|(value, object)| (*object == root).then_some(*value))
            .chain(std::iter::once(root)).collect::<Vec<_>>();
        for value in affected { self.set_task(value, TaskExecutionProof::Unknown); }
    }

    fn invalidate_unproved_call_effects(&mut self) {
        for proof in self.tasks.values_mut() { *proof = TaskExecutionProof::Unknown; }
        self.slots.clear();
        self.callable_slots.clear();
        self.owner_captures.clear();
    }

    fn carry(&mut self, destination: ValueId, source: ValueId) {
        self.aliases.insert(destination, self.root(source));
        self.set_task(destination, self.task(source));
        if let Some(callable) = self.callables.get(&source).copied() {
            self.callables.insert(destination, callable);
        }
    }

    fn single_pointer_field(
        function: &Function,
        object: ValueId,
        layouts: &LayoutInterner,
    ) -> Option<u32> {
        fn pointer_count(layout: LayoutId, layouts: &LayoutInterner, depth: usize) -> Option<usize> {
            if depth > 32 { return None; }
            let concrete = layouts.resolve_nominal(layout).ok()?;
            let descriptor = layouts.get(concrete)?;
            let mut count = 0usize;
            for field in descriptor.fields() {
                count += match field.kind() {
                    FieldKind::Pointer { slot, .. } if slot.may_establish_object_provenance() => 1,
                    FieldKind::Inline(child) => pointer_count(child, layouts, depth + 1)?,
                    _ => 0,
                };
                if count > 1 { return Some(count); }
            }
            Some(count)
        }
        let ValueType::Ref(layout) = function.value_types.get(&object)? else { return None };
        let concrete = layouts.resolve_nominal(*layout).ok()?;
        let descriptor = layouts.get(concrete)?;
        let mut selected = None;
        for (index, field) in descriptor.fields().iter().enumerate() {
            let count = match field.kind() {
                FieldKind::Pointer { slot, .. } if slot.may_establish_object_provenance() => 1,
                FieldKind::Inline(child) => pointer_count(child, layouts, 1)?,
                _ => 0,
            };
            if count > 1 { return None; }
            if count == 1 {
                if selected.replace(index as u32).is_some() { return None; }
            }
        }
        selected
    }

    fn operation(
        &mut self,
        operation: &Operation,
        function: &Function,
        layouts: &LayoutInterner,
        results: &BTreeMap<FunctionId, TaskExecutionProof>,
        requirements: &BTreeMap<FunctionId, Vec<TaskExecutionProof>>,
        indirect: &BTreeMap<crate::canonical_safewir::FunctionSignatureId, Vec<FunctionId>>,
        owner: FunctionId,
    ) -> Result<(), CandidateAbiBindingError> {
        match operation {
            Operation::MoveValue { destination, source }
            | Operation::CloneRef { destination, source } => self.carry(*destination, *source),
            Operation::FunctionValue { destination, function } => {
                self.callables.insert(*destination, *function);
            }
            Operation::Load { destination, place, .. } => {
                if let Projection::Field { index } = place.projection() {
                    let object = self.root(place.object());
                    if let Some(proof) = self.slots.get(&(object, *index)).cloned() {
                        self.set_task(*destination, proof);
                    } else {
                        let parent = self.task(place.object());
                        let projected = match parent {
                            proof @ (TaskExecutionProof::Variant { .. } | TaskExecutionProof::Variants(_)) =>
                                proof.project_variant_slot(0, *index as usize),
                            proof @ TaskExecutionProof::Elements(_) => proof.project_slot(*index as usize),
                            proof if Self::single_pointer_field(function, place.object(), layouts) == Some(*index) => proof,
                            _ => TaskExecutionProof::Unknown,
                        };
                        self.set_task(*destination, projected);
                    }
                    if let Some(callable) = self.callable_slots.get(&(object, *index)).copied() {
                        self.callables.insert(*destination, callable);
                    }
                }
            }
            Operation::Store { place, value, .. } => {
                if let Projection::Field { index } = place.projection() {
                    let object = self.root(place.object());
                    let proof = self.task_with_fields(*value, function, layouts);
                    self.invalidate_root_task(object);
                    self.slots.insert((object, *index), proof.clone());
                    if Self::single_pointer_field(function, place.object(), layouts) == Some(*index) {
                        let aliases = self.aliases.iter().filter_map(|(alias, root)| (*root == object).then_some(*alias))
                            .chain(std::iter::once(object)).collect::<Vec<_>>();
                        for alias in aliases { self.set_task(alias, proof.clone()); }
                    }
                    if let Some(callable) = self.callables.get(value).copied() {
                        self.callable_slots.insert((object, *index), callable);
                    } else {
                        self.callable_slots.remove(&(object, *index));
                    }
                }
            }
            Operation::CopyBytes { destination, .. }
            | Operation::MoveRange { destination, .. }
            | Operation::CloneRange { destination, .. } => {
                self.invalidate_root(self.root(destination.object()));
            }
            Operation::MakeInlineAggregate { destination, fields, .. } => {
                self.set_task(*destination, TaskExecutionProof::Elements(
                    fields.iter().map(|field| self.task(*field)).collect(),
                ));
            }
            Operation::ExtractInlineField { destination, aggregate, index } => {
                self.set_task(*destination, self.task(*aggregate).project_slot(*index as usize));
            }
            Operation::ClosureOwner(OwnerOperation::Construct { destination, captures, .. }) => {
                for (ordinal, capture) in captures.iter().enumerate() {
                    self.owner_captures.insert((*destination, ordinal), self.task(*capture));
                }
            }
            Operation::ClosureOwner(
                OwnerOperation::ReadOwner { destination, owner, capture, .. }
                | OwnerOperation::ReadEnvironmentField { destination, owner, capture, .. }
            ) => {
                // Both operations project one checked constructor capture.
                // Keep its entire recursive proof: a copyable environment
                // field can itself contain a Task inside an aggregate.
                if let Some(proof) = self.owner_captures.get(&(self.root(*owner), capture.0 as usize)).cloned() {
                    self.set_task(*destination, proof);
                }
            }
            Operation::ClosureOwner(OwnerOperation::Clone { destination, owner, .. }) => {
                self.carry(*destination, *owner);
            }
            Operation::Call { destinations, callee, arguments } => {
                let arguments = arguments.iter().map(|argument|
                    self.task_with_fields(call_value(argument), function, layouts)).collect::<Vec<_>>();
                check_requirements(*callee, &arguments, requirements, owner)?;
                self.invalidate_unproved_call_effects();
                if let (Some(destination), Some(result)) = (destinations.first(), results.get(callee)) {
                    self.set_task(*destination, result.instantiate(&arguments));
                }
            }
            Operation::VarCall { destinations, callee, signature, arguments } => {
                let arguments = arguments.iter().map(|argument|
                    self.task_with_fields(call_value(argument), function, layouts)).collect::<Vec<_>>();
                if let Some(possible) = indirect.get(signature) {
                    let Some(actual) = self.callables.get(callee).copied() else {
                        return Err(CandidateAbiBindingError::TaskBehaviorMismatch { function: owner });
                    };
                    if !possible.contains(&actual) {
                        return Err(CandidateAbiBindingError::TaskBehaviorMismatch { function: owner });
                    }
                    check_requirements(actual, &arguments, requirements, owner)?;
                }
                self.invalidate_unproved_call_effects();
                if let (Some(destination), Some(actual)) = (destinations.first(), self.callables.get(callee)) {
                    if let Some(result) = results.get(actual) {
                        self.set_task(*destination, result.instantiate(&arguments));
                    }
                }
            }
            Operation::ExternalCall { .. } | Operation::HostCall { .. } => {
                self.invalidate_unproved_call_effects();
            }
            // Indirect/host calls and mutable unknown storage cannot produce
            // execution authority. Structural SafeWIR checks remain separate.
            _ => {}
        }
        Ok(())
    }
}

fn check_requirements(
    callee: FunctionId,
    arguments: &[TaskExecutionProof],
    requirements: &BTreeMap<FunctionId, Vec<TaskExecutionProof>>,
    owner: FunctionId,
) -> Result<(), CandidateAbiBindingError> {
    if let Some(required) = requirements.get(&callee) {
        for requirement in required {
            let instantiated = requirement.instantiate(arguments);
            if !matches!(instantiated,
                TaskExecutionProof::Task(SourceExecutionMode::Opt, _) | TaskExecutionProof::InactiveVariant)
            {
                return Err(CandidateAbiBindingError::TaskBehaviorMismatch { function: owner });
            }
        }
    }
    Ok(())
}

fn expected_wrapper_target(
    source: &CanonicalSourceView<'_>,
    ordinal: u32,
    candidate: &CanonicalModule<impl ModuleStage>,
) -> Option<FunctionId> {
    use witchy_syntax::ast::{Expr, Item, Stmt};
    let Item::Function(factory) = source.module().items.get(ordinal as usize)? else { return None };
    let [Stmt::Expr(Expr::Call { name, .. })] = factory.body.stmts.as_slice() else { return None };
    let mut targets = candidate.source_abi_bindings().entries().iter().filter_map(|binding| {
        let function = source.closed_callable_declaration(binding.source_declaration_ordinal())?;
        (function.name == *name).then_some(binding.function())
    });
    let only = targets.next()?;
    targets.next().is_none().then_some(only)
}

/// The source closure witness fixes which executable lambda the wrapper puts
/// into its returned recipe. A same-signature lambda from another checked
/// declaration is not interchangeable, even when its environment layout is
/// identical. This verifies the executable selection, not the lambda body.
fn wrapper_selects_checked_callback<S: ModuleStage>(
    candidate: &CanonicalModule<S>,
    source: &CanonicalSourceView<'_>,
    wrapper: FunctionId,
    functions: &BoundFunctions<'_>,
) -> Option<FunctionId> {
    let Some(binding) = candidate.source_abi_bindings().entries().iter()
        .find(|binding| binding.function() == wrapper) else { return None };
    let Some(item) = source.closed_callable_item_ordinal(binding.source_declaration_ordinal()) else { return None };
    let mut expected_bindings = candidate.closure_bindings().entries().iter().filter_map(|binding| {
        let witness = source.closure_witness().entries().get(binding.closure() as usize)?;
        (witness.id().item == item && witness.id().method.is_none()).then_some(binding.function())
    });
    let expected = expected_bindings.next()?;
    if expected_bindings.next().is_some() { return None; }
    let function = functions.get(&wrapper)?;
    let [block] = function.blocks.as_slice() else { return None };
    let Terminator::Return { values } = &block.terminator else { return None };
    let [returned] = values.as_slice() else { return None };
    // Follow the actual returned object's stored fields, rather than merely
    // finding a correct but unused FunctionValue somewhere in the body.
    // Calls or reads would introduce another producer not proved by this
    // constructor-only path; those wrappers fail closed for now.
    let graph = ValueGraph::constructor(function)?;
    if block.parameters.len() != 1 { return None; }
    let task_fields = graph.fields(graph.node(*returned)).collect::<Vec<_>>();
    let [(_, poll_closure @ Node::Object(_))] = task_fields.as_slice() else { return None };
    let closure_fields = graph.fields(*poll_closure).collect::<Vec<_>>();
    let mut selected = None;
    let mut environment = None;
    for (_, field) in closure_fields {
        match field {
            Node::Function(function) if selected.replace(function).is_none() => {}
            object @ Node::Object(_) if environment.replace(object).is_none() => {}
            _ => return None,
        }
    }
    if selected != Some(expected) { return None; }
    let capture_fields = graph.fields(environment?).collect::<Vec<_>>();
    let captured_inputs = capture_fields.iter().filter_map(|(_, field)| match field {
        Node::Input(index) => Some(*index), _ => None,
    }).collect::<Vec<_>>();
    if captured_inputs != [0]
        || capture_fields.iter().any(|(_, field)| matches!(field, Node::Function(_) | Node::Object(_)))
    { return None; }
    Some(expected)
}

fn checked_wrapper_dispatch_target<S: ModuleStage>(
    candidate: &CanonicalModule<S>, source: &CanonicalSourceView<'_>, wrapper: FunctionId,
) -> Option<FunctionId> {
    use witchy_syntax::ast::{Expr, Item, Stmt};
    let binding = candidate.source_abi_bindings().entries().iter()
        .find(|binding| binding.function() == wrapper)?;
    let item = source.closed_callable_item_ordinal(binding.source_declaration_ordinal())?;
    let Item::Function(declaration) = source.module().items.get(item as usize)? else { return None };
    let [parameter] = declaration.params.as_slice() else { return None };
    let [Stmt::Expr(Expr::Ctor { args, .. })] = declaration.body.stmts.as_slice() else { return None };
    let [Expr::Lambda { body, .. }] = args.as_slice() else { return None };
    let [Stmt::Expr(Expr::Call { name, args })] = body.stmts.as_slice() else { return None };
    let [Expr::Call { name: thunk, args: thunk_args }] = args.as_slice() else { return None };
    if thunk != &parameter.name || !thunk_args.is_empty() { return None; }
    let mut targets = candidate.source_abi_bindings().entries().iter().filter_map(|binding| {
        let function = source.closed_callable_declaration(binding.source_declaration_ordinal())?;
        (function.name == *name).then_some(binding.function())
    });
    let only = targets.next()?;
    targets.next().is_none().then_some(only)
}

/// A lazy poll adapter must invoke its captured thunk and pass that *result*
/// to the checked task poller. Merely retaining the right source lambda or
/// an unused VarCall is insufficient to authenticate deferred execution.
fn poll_adapter_invokes_captured_thunk(function: &Function, checked_target: FunctionId) -> bool {
    let [block] = function.blocks.as_slice() else { return false };
    let [entry] = block.parameters.as_slice() else { return false };
    #[derive(Clone, Copy)]
    enum Parent { Alias(ValueId), Field(ValueId, u32) }
    let mut parents = BTreeMap::<ValueId, Parent>::new();
    let mut invocation = None;
    for operation in &block.operations {
        match operation {
            Operation::Load { destination, place, .. } => {
                let Projection::Field { index } = place.projection() else { return false };
                parents.insert(*destination, Parent::Field(place.object(), *index));
            }
            Operation::CloneRef { destination, source }
            | Operation::MoveValue { destination, source } => {
                parents.insert(*destination, Parent::Alias(*source));
            }
            Operation::VarCall { destinations, callee, arguments, .. } => {
                let ([destination], [argument]) = (destinations.as_slice(), arguments.as_slice()) else { return false };
                if invocation.replace((*destination, *callee, call_value(argument))).is_some() { return false; }
            }
            Operation::ConstInt { .. } | Operation::Compare { .. }
            | Operation::Guard { .. } | Operation::DropRef { .. } => {}
            _ => return false,
        }
    }
    let Some((result, callee, argument)) = invocation else { return false };
    fn path_from(mut value: ValueId, root: ValueId, parents: &BTreeMap<ValueId, Parent>) -> Option<Vec<u32>> {
        let mut seen = BTreeSet::new();
        let mut path = Vec::new();
        while value != root {
            if !seen.insert(value) { return None; }
            value = match parents.get(&value)? {
                Parent::Alias(parent) => *parent,
                Parent::Field(parent, index) => { path.push(*index); *parent },
            };
        }
        path.reverse();
        Some(path)
    }
    let Some(callee_path) = path_from(callee, entry.value, &parents) else { return false };
    let Some(argument_path) = path_from(argument, entry.value, &parents) else { return false };
    if callee_path.len() != argument_path.len() + 1 || !callee_path.starts_with(&argument_path) { return false; }
    matches!(&block.terminator,
        Terminator::TailCall { callee, arguments, .. } if *callee == checked_target && matches!(arguments.as_slice(),
            [CallArgument::Move(value) | CallArgument::Value(value)] if *value == result))
}

/// The checked Task poll projection must be the callable actually invoked by
/// the selected dispatcher. A source-level `poll(thunk())` only names a target;
/// it does not authenticate that target's candidate body.
fn task_poller_dispatches_stored_callable(
    function: &Function,
    projection: &witchy_canonical_source::source::DeferredPollProjection,
) -> bool {
    // The checked Task role has one constructor. Its WIR carrier is the
    // aggregate's variant payload, followed by the checked stored child.
    if projection.variant() != 0 { return false; }
    let [block] = function.blocks.as_slice() else { return false };
    let [entry] = block.parameters.as_slice() else { return false };
    let Terminator::VarTailCall { callee, signature, arguments, .. } = &block.terminator else { return false };
    let [argument] = arguments.as_slice() else { return false };
    let mut loads = Vec::new();
    let mut aliases = BTreeMap::<ValueId, ValueId>::new();
    for operation in &block.operations {
        match operation {
            Operation::Load { destination, place, .. } => {
                let Projection::Field { index } = place.projection() else { return false };
                loads.push((*destination, place.object(), *index));
            }
            Operation::MoveValue { destination, source }
            | Operation::CloneRef { destination, source } => {
                aliases.insert(*destination, aliases.get(source).copied().unwrap_or(*source));
            }
            Operation::DropRef { .. } => {}
            _ => return false,
        }
    }
    let [(payload, root, variant), (stored, owner, field)] = loads.as_slice() else { return false };
    if *root != entry.value || *variant != projection.variant() as u32
        || *owner != *payload || *field != projection.field() as u32 { return false; }
    let canonical = |value: ValueId| aliases.get(&value).copied().unwrap_or(value);
    if canonical(*callee) != *stored || canonical(call_value(argument)) != *payload { return false; }
    matches!(function.value_types.get(stored), Some(ValueType::Callable(actual)) if actual == signature)
}

/// A checked poll promise is useful only if the returned candidate Step has
/// the same authenticated variant and carries no hidden invocation. This
/// bounded leaf handles passive variants; yielded/forked work must be proved
/// recursively, rather than treated as another spelling of Done.
fn poll_returns_passive_step<S: ModuleStage>(
    candidate: &CanonicalModule<S>, source: &CanonicalSourceView<'_>,
    poll: FunctionId, functions: &BoundFunctions<'_>,
) -> bool {
    let Some(role) = source.deferred_task_role() else { return false };
    let Some(binding) = candidate.closure_bindings().entries().iter()
        .find(|binding| binding.function() == poll) else { return false };
    let Some(witness) = source.closure_witness().entries().get(binding.closure() as usize) else { return false };
    let Some(contract) = witness.execution_contract() else { return false };
    let TaskExecutionProof::Step { role: actual_role, value } = contract.result() else { return false };
    if actual_role != role.step_identity() || !contract.required_opt_inputs().is_empty()
        || role.step_requirements(contract.result()) != Some(vec![]) {
        return false;
    }
    let TaskExecutionProof::Variant { ordinal, fields } = value.as_ref() else { return false };
    let Some(function) = functions.get(&poll) else { return false };
    let [block] = function.blocks.as_slice() else { return false };
    let Terminator::Return { values } = &block.terminator else { return false };
    let [returned] = values.as_slice() else { return false };
    let mut aliases = BTreeMap::<ValueId, ValueId>::new();
    let mut built = None;
    for operation in &block.operations {
        match operation {
            Operation::MoveValue { destination, source }
            | Operation::CloneRef { destination, source } => {
                aliases.insert(*destination, aliases.get(source).copied().unwrap_or(*source));
            }
            Operation::ClosureOwner(OwnerOperation::Construct { destination, constructor, captures }) => {
                if built.replace((*destination, *constructor, captures.len())).is_some() { return false; }
            }
            Operation::Load { .. } | Operation::ConstUnit { .. }
            | Operation::ConstInt { .. } | Operation::ConstFloat { .. }
            | Operation::ConstBool { .. } | Operation::ConstSize { .. }
            | Operation::Compare { .. } | Operation::Guard { .. }
            | Operation::DropRef { .. } => {}
            _ => return false,
        }
    }
    let Some((built, constructor, arity)) = built else { return false };
    if aliases.get(returned).copied().unwrap_or(*returned) != built || arity != fields.len() { return false; }
    candidate.owner_families().iter().any(|family| {
        let OwnerFamilySource::Aggregate(Type::Named(name, _)) = &family.source else { return false };
        name == role.step_name() && family.constructors.iter().any(|entry|
            entry.id == constructor && entry.environment_tag == *ordinal as i64
                && entry.captures.len() == arity)
    })
}

/// Match a deferred thunk's terminal recipe or direct async-factory handoff to
/// the exact checked declaration it invokes. Scalar operands cannot contain
/// Task authority; managed payloads need recursive value proof and fail closed.
fn thunk_returns_checked_recipe<S: ModuleStage>(
    candidate: &CanonicalModule<S>, source: &CanonicalSourceView<'_>,
    thunk: FunctionId, functions: &BoundFunctions<'_>, layouts: &LayoutInterner,
    visited: &mut BTreeSet<FunctionId>,
) -> bool {
    let Some(binding) = candidate.closure_bindings().entries().iter()
        .find(|binding| binding.function() == thunk) else { return false };
    let Some(witness) = source.closure_witness().entries().get(binding.closure() as usize) else { return false };
    let Some(contract) = witness.execution_contract() else { return false };
    if contract.source_mode() != SourceExecutionMode::Opt { return false; }
    let TaskExecutionProof::Recipe { .. } = contract.result() else { return false };
    let Some(function) = functions.get(&thunk) else { return false };
    let [block] = function.blocks.as_slice() else { return false };
    let Terminator::TailCall { callee, arguments, .. } = &block.terminator else { return false };
    if block.operations.iter().any(|operation| !matches!(operation,
        Operation::Load { .. } | Operation::MoveValue { .. } | Operation::CloneRef { .. }
        | Operation::ConstUnit { .. } | Operation::ConstInt { .. }
        | Operation::ConstFloat { .. } | Operation::ConstBool { .. }
        | Operation::ConstSize { .. } | Operation::Compare { .. }
        | Operation::Guard { .. } | Operation::DropRef { .. }
    )) { return false; }
    let arguments = arguments.iter().map(|argument| {
        let value = call_value(argument);
        match function.value_types.get(&value) {
            Some(ValueType::Int | ValueType::Float | ValueType::Bool | ValueType::Unit | ValueType::Size) =>
                Some(TaskExecutionProof::Unknown),
            _ => None,
        }
    }).collect::<Option<Vec<_>>>();
    let Some(arguments) = arguments else { return false };
    let Some(runtime_binding) = candidate.source_abi_bindings().entries().iter()
        .find(|binding| binding.function() == *callee) else { return false };
    let Some(item) = source.closed_callable_item_ordinal(runtime_binding.source_declaration_ordinal()) else { return false };
    let Some(callee_contract) = source.final_task_behavior_at(item) else { return false };
    if callee_contract.result().instantiate(&arguments) != *contract.result() { return false; }
    if source.task_behavior_at(item).is_some_and(|behavior| behavior.async_thunk_callable().is_some()) {
        return functions.get(callee).is_some_and(|callee_function|
            prove_async_factory_inner(candidate, source, callee_function, item, layouts, functions, visited));
    }
    let Some(poll) = wrapper_selects_checked_callback(candidate, source, *callee, functions) else { return false };
    poll_returns_passive_step(candidate, source, poll, functions)
}

/// A generated thunk may forward its captured Task without constructing new
/// deferred work. The factory must store the exact checked formal beside the
/// selected thunk, and the thunk must return that same physical capture lane.
/// This leaf handles one direct formal capture; aggregate paths and joins need
/// a recursive candidate value proof rather than guessing their layouts.
fn thunk_returns_checked_capture<S: ModuleStage>(
    candidate: &CanonicalModule<S>, source: &CanonicalSourceView<'_>,
    factory: &Function, thunk: FunctionId, functions: &BoundFunctions<'_>,
) -> bool {
    let Some(binding) = candidate.closure_bindings().entries().iter()
        .find(|binding| binding.function() == thunk) else { return false };
    let Some(witness) = source.closure_witness().entries().get(binding.closure() as usize) else { return false };
    let Some(contract) = witness.execution_contract() else { return false };
    let TaskExecutionProof::Capture(owner, index) = contract.result() else { return false };
    if contract.source_mode() != SourceExecutionMode::Opt || owner != witness.id() || *index != 0 { return false; }
    let [capture] = witness.captures() else { return false };
    let [ClosureBindingPathStep::Parameter(parameter)] = capture.binding().path() else { return false };
    let parameter = *parameter as usize;
    let Some(factory_binding) = candidate.source_abi_bindings().entries().iter()
        .find(|binding| binding.function() == factory.id) else { return false };
    let Some(item) = source.closed_callable_item_ordinal(factory_binding.source_declaration_ordinal()) else { return false };
    let Some(factory_contract) = source.task_behavior_at(item) else { return false };
    if !factory_contract.required_opt_inputs().contains(&TaskExecutionProof::Parameter(parameter)) { return false; }
    let [factory_block] = factory.blocks.as_slice() else { return false };
    if factory_block.parameters.get(parameter).is_none() { return false; }
    let Terminator::TailCall { arguments, .. } = &factory_block.terminator else { return false };
    let [argument] = arguments.as_slice() else { return false };
    let Some(graph) = ValueGraph::constructor(factory) else { return false };
    let closure_fields = graph.fields(graph.node(call_value(argument))).collect::<Vec<_>>();
    let mut executable = None;
    let mut environment = None;
    let mut environment_field = None;
    for (field, value) in closure_fields {
        match value {
            Node::Function(function) if executable.replace(function).is_none() => {}
            object @ Node::Object(_) if environment.replace(object).is_none() => environment_field = Some(field),
            _ => return false,
        }
    }
    if executable != Some(thunk) { return false; }
    let Some(environment) = environment else { return false };
    let mut capture_field = None;
    for (field, value) in graph.fields(environment) {
        match value {
            Node::Input(actual) if actual == parameter && capture_field.replace(field).is_none() => {}
            Node::Unknown => {}
            _ => return false,
        }
    }
    let Some(function) = functions.get(&thunk) else { return false };
    let [block] = function.blocks.as_slice() else { return false };
    let [entry] = block.parameters.as_slice() else { return false };
    let Terminator::Return { values } = &block.terminator else { return false };
    let [returned] = values.as_slice() else { return false };
    #[derive(Clone, Copy)]
    enum Parent { Alias(ValueId), Field(ValueId, u32) }
    let mut parents = BTreeMap::<ValueId, Parent>::new();
    for operation in &block.operations {
        match operation {
            Operation::Load { destination, place, .. } => {
                let Projection::Field { index } = place.projection() else { return false };
                parents.insert(*destination, Parent::Field(place.object(), *index));
            }
            Operation::MoveValue { destination, source } | Operation::CloneRef { destination, source } => {
                parents.insert(*destination, Parent::Alias(*source));
            }
            Operation::ConstUnit { .. } | Operation::ConstInt { .. }
            | Operation::ConstFloat { .. } | Operation::ConstBool { .. }
            | Operation::ConstSize { .. } | Operation::Compare { .. }
            | Operation::Guard { .. } | Operation::DropRef { .. } => {}
            _ => return false,
        }
    }
    let mut path = Vec::new();
    let mut value = *returned;
    let mut seen = BTreeSet::new();
    while value != entry.value {
        if !seen.insert(value) { return false; }
        value = match parents.get(&value) {
            Some(Parent::Alias(parent)) => *parent,
            Some(Parent::Field(parent, field)) => { path.push(*field); *parent },
            None => return false,
        };
    }
    path.reverse();
    let (Some(environment_field), Some(capture_field)) = (environment_field, capture_field) else { return false };
    path == [environment_field, capture_field]
}

/// Admit a source async factory result only when its actual WIR body passes
/// the exact checked thunk closure to the checked wrapper call. This is one
/// compiler-generated async shape, independent of the task's result type.
fn prove_async_factory<S: ModuleStage>(
    candidate: &CanonicalModule<S>,
    source: &CanonicalSourceView<'_>,
    function: &Function,
    ordinal: u32,
    layouts: &LayoutInterner,
    functions: &BoundFunctions<'_>,
) -> bool {
    prove_async_factory_inner(candidate, source, function, ordinal, layouts, functions, &mut BTreeSet::new())
}

fn prove_async_factory_inner<S: ModuleStage>(
    candidate: &CanonicalModule<S>,
    source: &CanonicalSourceView<'_>,
    function: &Function,
    ordinal: u32,
    layouts: &LayoutInterner,
    functions: &BoundFunctions<'_>,
    visited: &mut BTreeSet<FunctionId>,
) -> bool {
    if !visited.insert(function.id) { return false; }
    let Some(thunk) = source.task_behavior_at(ordinal).and_then(|contract| contract.async_thunk_callable()) else { return false };
    let mut thunks = candidate.closure_bindings().entries().iter().filter_map(|binding| {
        source.closure_witness().entries().get(binding.closure() as usize)
            .and_then(|witness| (witness.id() == thunk).then_some(binding.function()))
    });
    let Some(thunk_function) = thunks.next() else { return false };
    if thunks.next().is_some() { return false; }
    if !thunk_returns_checked_recipe(candidate, source, thunk_function, functions, layouts, visited)
        && !thunk_returns_checked_capture(candidate, source, function, thunk_function, functions) {
        return false;
    }
    let Some(wrapper) = expected_wrapper_target(source, ordinal, candidate) else { return false };
    let Some(poll) = wrapper_selects_checked_callback(candidate, source, wrapper, functions) else { return false };
    let Some(checked_target) = checked_wrapper_dispatch_target(candidate, source, wrapper) else { return false };
    if !functions.get(&poll).is_some_and(|function| poll_adapter_invokes_captured_thunk(function, checked_target)) { return false; }
    let Some(role) = source.deferred_task_role() else { return false };
    if !functions.get(&checked_target).is_some_and(|function|
        task_poller_dispatches_stored_callable(function, role.poll_projection())) { return false; }
    let [block] = function.blocks.as_slice() else { return false };
    let mut flow = Flow::default();
    for operation in &block.operations {
        if flow.operation(operation, function, layouts, &BTreeMap::new(), &BTreeMap::new(), &BTreeMap::new(), function.id).is_err() {
            return false;
        }
    }
    let Terminator::TailCall { callee, arguments, .. } = &block.terminator else { return false };
    if *callee != wrapper || arguments.len() != 1 { return false; }
    let envelope = flow.root(call_value(&arguments[0]));
    let actual = flow.callable_slots.iter().filter_map(|((object, _), callable)|
        (*object == envelope).then_some(*callable)).collect::<BTreeSet<_>>();
    actual.len() == 1 && actual.contains(&thunk_function)
}

/// Match a checked recursive result proof to the actual value graph.
/// A result type or container's defining mode alone cannot authenticate a
/// stored callback/Task: every symbolic formal must occupy its promised
/// physical field of the returned object. Unsupported producers fail closed.
fn returns_checked_proof(
    function: &Function,
    parameter_count: usize,
    expected: &TaskExecutionProof,
    layouts: &LayoutInterner,
) -> bool {
    let [block] = function.blocks.as_slice() else { return false };
    let Terminator::Return { values } = &block.terminator else { return false };
    let Some(graph) = ValueGraph::constructor(function) else { return false };
    if block.parameters.len() != parameter_count { return false; }
    let [returned] = values.as_slice() else { return false };
    fn matches(
        proof: &TaskExecutionProof, node: Node, graph: &ValueGraph,
        function: &Function, layouts: &LayoutInterner, depth: usize,
    ) -> bool {
        if depth > 32 { return false; }
        match proof {
            TaskExecutionProof::Parameter(index) => node == Node::Input(*index),
            // Unknown does not grant execution authority. It may occupy any
            // physical field without authenticating its behavior.
            TaskExecutionProof::Unknown => true,
            TaskExecutionProof::Variant { ordinal: 0, fields } | TaskExecutionProof::Elements(fields) => {
                let Node::Object(object) = node else { return false };
                let Some(ValueType::Ref(layout)) = function.value_types.get(&object) else { return false };
                let Some(descriptor) = layouts.resolve_nominal(*layout).ok().and_then(|id| layouts.get(id)) else {
                    return false;
                };
                if !matches!(descriptor.kind(), LayoutKind::PackedRecord { .. } | LayoutKind::Tuple { .. })
                    || descriptor.fields().len() != fields.len() { return false; }
                let stored = graph.fields(node).collect::<Vec<_>>();
                stored.len() == fields.len() && stored.iter().enumerate().all(|(index, (slot, child))| {
                    *slot as usize == index && matches(&fields[index], *child, graph, function, layouts, depth + 1)
                })
            }
            _ => false,
        }
    }
    matches(expected, graph.node(*returned), &graph, function, layouts, 0)
}

pub(super) fn validate<S: ModuleStage>(
    candidate: &CanonicalModule<S>,
    source: &CanonicalSourceView<'_>,
    functions: &BoundFunctions<'_>,
) -> Result<(), CandidateAbiBindingError> {
    let (_, layouts) = crate::layout::LayoutBundle::decode_canonical(&candidate.layouts().canonical_bytes())
        .map_err(|_| CandidateAbiBindingError::MissingCanonicalLayouts)?;
    let mut results = BTreeMap::new();
    let mut requirements = BTreeMap::new();
    let mut indirect = BTreeMap::<crate::canonical_safewir::FunctionSignatureId, Vec<FunctionId>>::new();
    for binding in candidate.source_abi_bindings().entries() {
        let Some(item) = source.closed_callable_item_ordinal(binding.source_declaration_ordinal()) else { continue };
        let Some(contract) = source.task_behavior_at(item)
            .or_else(|| source.final_task_behavior_at(item)) else { continue };
        let Some(function) = functions.get(&binding.function()) else { continue };
        let result = contract.result();
        if (matches!(result, TaskExecutionProof::Task(SourceExecutionMode::Opt, _))
            || !contract.required_opt_inputs().is_empty())
            && source.deferred_task_role().is_none()
        {
            return Err(CandidateAbiBindingError::TaskBehaviorMismatch { function: binding.function() });
        }
        let grant = match result {
            TaskExecutionProof::Task(SourceExecutionMode::Opt, _) => prove_async_factory(
                candidate, source, function, item, &layouts, functions,
            ),
            // Normal is not opt authority. Symbolic synchronous relays need
            // body correspondence before their result may grant opt authority.
            TaskExecutionProof::Task(SourceExecutionMode::Normal, _) => true,
            TaskExecutionProof::Parameter(_)
            | TaskExecutionProof::Variant { .. }
            | TaskExecutionProof::Elements(_) =>
                source.closed_callable_declaration(binding.source_declaration_ordinal())
                .is_some_and(|declaration| returns_checked_proof(
                    function, declaration.params.len(), result, &layouts,
                )),
            _ => false,
        };
        if !grant && matches!(result, TaskExecutionProof::Task(SourceExecutionMode::Opt, _)) {
            return Err(CandidateAbiBindingError::TaskBehaviorMismatch { function: binding.function() });
        }
        if grant { results.insert(binding.function(), result.clone()); }
        if !contract.required_opt_inputs().is_empty() {
            requirements.insert(binding.function(), contract.required_opt_inputs().to_vec());
            indirect.entry(function.signature).or_default().push(binding.function());
        }
    }
    for function in candidate.functions() {
        for block in &function.blocks {
            let mut flow = Flow::default();
            for operation in &block.operations {
                flow.operation(operation, function, &layouts, &results, &requirements, &indirect, function.id)?;
            }
            if let Terminator::TailCall { callee, arguments, .. } = &block.terminator {
                let arguments = arguments.iter().map(|argument|
                    flow.task_with_fields(call_value(argument), function, &layouts)).collect::<Vec<_>>();
                check_requirements(*callee, &arguments, &requirements, function.id)?;
            }
            if let Terminator::VarTailCall { callee, signature, arguments, .. } = &block.terminator
                && let Some(possible) = indirect.get(signature)
            {
                let Some(actual) = flow.callables.get(callee).copied() else {
                    return Err(CandidateAbiBindingError::TaskBehaviorMismatch { function: function.id });
                };
                if !possible.contains(&actual) {
                    return Err(CandidateAbiBindingError::TaskBehaviorMismatch { function: function.id });
                }
                let arguments = arguments.iter().map(|argument|
                    flow.task_with_fields(call_value(argument), function, &layouts)).collect::<Vec<_>>();
                check_requirements(actual, &arguments, &requirements, function.id)?;
            }
        }
    }
    Ok(())
}
