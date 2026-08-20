//! Typed, backend-neutral ABI description for compiler-owned suspension state.
//!
//! Syntax lowering assigns stable state identities, but it runs before type
//! inference and therefore cannot decide whether a frame can use the direct
//! Wasm carrier. This catalog is deliberately built from [`TypedModule`]: it
//! joins each generated callable with its finalized parameter signature and
//! flattens closed products/sums plus host capabilities into fixed-width lanes.
//! Async and generator lowering can target the same catalog without making a
//! generated function name or a closure layout part of the runtime contract.

use foldhash::{HashMap, HashMapExt as _, HashSet, HashSetExt as _};
use witchy_syntax::ast::{self, Convention, Function, Item, Type, TypeDef};
use witchy_syntax::suspension::{
    frame_state, FRAME_BOXED_ATTRIBUTE, FRAME_ENTRY_ATTRIBUTE, FRAME_FUNCTION_ATTRIBUTE,
};

use crate::typeck::TypedModule;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CarrierLane {
    I64,
    F64,
    ExternRef,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CarrierStateKind {
    Entry,
    Segment,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CarrierSlot {
    pub name: String,
    pub convention: Convention,
    pub ty: Type,
    /// `None` means the value needs the boxed fallback. `Some([])` is a
    /// zero-width `Nil`; all other `Some` values are fixed direct lanes.
    pub lanes: Option<Vec<CarrierLane>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CarrierState {
    pub id: usize,
    pub function: String,
    pub source_callable: String,
    pub kind: CarrierStateKind,
    pub direct: bool,
    pub slots: Vec<CarrierSlot>,
}

impl CarrierState {
    pub fn is_direct(&self) -> bool {
        self.direct && self.slots.iter().all(|slot| slot.lanes.is_some())
    }

    pub fn lane_width(&self) -> usize {
        self.slots
            .iter()
            .filter_map(|slot| slot.lanes.as_ref())
            .map(Vec::len)
            .sum()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SuspensionCarrierCatalog {
    states: Vec<CarrierState>,
    max_lane_width: usize,
    scalar_transitions: Vec<Result<Vec<ScalarTransition>, String>>,
}

/// Closed eligibility proof for RFC-0059's allocation-free compiled scheduler.
/// The lowering may consume this value without repeating a source-shape guess:
/// every frame state is direct, no frame column needs a floating/boxed lane, and
/// every typed channel endpoint carries an `Int` message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScalarExecutorPlan {
    pub state_count: usize,
    pub max_lane_width: usize,
    pub states: Vec<ScalarExecutorState>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScalarExecutorState {
    pub id: usize,
    pub function: String,
    pub source_callable: String,
    pub slots: Vec<ScalarExecutorSlot>,
    pub transitions: Vec<ScalarTransition>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScalarExecutorSlot {
    pub name: String,
    pub lane_start: usize,
    pub lanes: Vec<CarrierLane>,
}

/// One terminal edge from a compiler-generated suspension state. Conditions,
/// matches, and ordinary scalar statements remain in the state body; this is
/// the effect/jump ABI the closure-free dispatcher must implement at each leaf.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScalarTransition {
    Jump { target: usize },
    Done,
    ChannelOpen { resume: usize },
    ChannelSend { resume: usize },
    ChannelReceive { resume: usize },
    Spawn { child: usize, resume: usize },
    Join { resume: usize },
    Yield { resume: usize },
    Call { callee: usize, resume: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScalarExecutorRejection {
    NoSuspensionStates,
    BoxedState { function: String },
    NonScalarLane { function: String, slot: String, lane: CarrierLane },
    NonIntegerChannel { channel: String, message: String },
    UnsupportedTransition { function: String, detail: String },
}

impl SuspensionCarrierCatalog {
    pub fn from_typed(typed: &TypedModule) -> Result<Self, String> {
        let definitions = type_definitions(typed.module());
        let mut states = Vec::new();

        for item in &typed.module().items {
            let Item::Function(function) = item else { continue };
            let Some(source_state) = frame_state(function) else { continue };
            let kind = state_kind(function).ok_or_else(|| {
                format!(
                    "compiler suspension state {source_state} on `{}` has neither entry nor segment marker",
                    function.name
                )
            })?;
            // Lowering state numbers are module-local and may collide after
            // linking. Linked item order is already deterministic, so assign
            // the whole-program dispatch identity in that order instead of
            // re-sorting by a non-unique source-local number.
            let id = states.len();
            let direct = !function
                .attributes
                .iter()
                .any(|attribute| attribute == FRAME_BOXED_ATTRIBUTE);
            let inferred = inferred_parameters(typed, function);
            let slots = function
                .params
                .iter()
                .enumerate()
                .map(|(index, parameter)| {
                    let ty = inferred
                        .as_ref()
                        .and_then(|types| types.get(index))
                        .cloned()
                        .or_else(|| parameter.ty.clone())
                        .unwrap_or_else(|| Type::Named("__Unknown".into(), Vec::new()));
                    let lanes = direct_lanes(&ty, &definitions, &HashMap::new(), &mut HashSet::new());
                    CarrierSlot {
                        name: parameter.name.clone(),
                        convention: parameter.convention,
                        ty,
                        lanes,
                    }
                })
                .collect();
            states.push(CarrierState {
                id,
                function: function.name.clone(),
                source_callable: witchy_syntax::suspension::source_callable_name(function),
                kind,
                direct,
                slots,
            });
        }

        let max_lane_width = states.iter().map(CarrierState::lane_width).max().unwrap_or(0);
        let state_ids: HashMap<String, usize> = states
            .iter()
            .map(|state| (state.function.clone(), state.id))
            .collect();
        let scalar_transitions = states
            .iter()
            .map(|state| {
                let function = typed.module().items.iter().find_map(|item| match item {
                    Item::Function(function) if function.name == state.function => Some(function),
                    _ => None,
                });
                let function = function.ok_or_else(|| {
                    format!("carrier state `{}` has no typed function body", state.function)
                })?;
                scalar_transitions_for_function(function, &state_ids)
            })
            .collect();
        Ok(Self { states, max_lane_width, scalar_transitions })
    }

    pub fn states(&self) -> &[CarrierState] {
        &self.states
    }

    pub fn max_lane_width(&self) -> usize {
        self.max_lane_width
    }

    pub fn is_wholly_direct(&self) -> bool {
        !self.states.is_empty() && self.states.iter().all(CarrierState::is_direct)
    }

    /// Prove that the module can use the scalar/capability scheduler described by
    /// RFC-0059 increment 2. Capability lanes remain direct roots; all mutable
    /// per-task columns are integer lanes. Channel endpoints are checked from the
    /// finalized state-slot types, so `Sender(String)` cannot qualify merely
    /// because its runtime channel id is an integer wrapper.
    pub fn scalar_executor_plan(
        &self,
    ) -> Result<ScalarExecutorPlan, ScalarExecutorRejection> {
        if self.states.is_empty() {
            return Err(ScalarExecutorRejection::NoSuspensionStates);
        }
        for state in &self.states {
            if !state.direct {
                return Err(ScalarExecutorRejection::BoxedState {
                    function: state.function.clone(),
                });
            }
            for slot in &state.slots {
                let Some(lanes) = &slot.lanes else {
                    return Err(ScalarExecutorRejection::BoxedState {
                        function: state.function.clone(),
                    });
                };
                if let Some(lane) = lanes
                    .iter()
                    .copied()
                    .find(|lane| !matches!(lane, CarrierLane::I64 | CarrierLane::ExternRef))
                {
                    return Err(ScalarExecutorRejection::NonScalarLane {
                        function: state.function.clone(),
                        slot: slot.name.clone(),
                        lane,
                    });
                }
                if let Some((channel, message)) = non_integer_channel(&slot.ty) {
                    return Err(ScalarExecutorRejection::NonIntegerChannel {
                        channel,
                        message: witchy_syntax::format::type_str(message),
                    });
                }
            }
        }
        let states = self
            .states
            .iter()
            .enumerate()
            .map(|(state_index, state)| {
                let transitions = self.scalar_transitions[state_index]
                    .clone()
                    .map_err(|detail| ScalarExecutorRejection::UnsupportedTransition {
                        function: state.function.clone(),
                        detail,
                    })?;
                let mut lane_start = 0;
                let slots = state
                    .slots
                    .iter()
                    .map(|slot| {
                        let lanes = slot
                            .lanes
                            .clone()
                            .expect("qualification rejected boxed frame slots");
                        let planned = ScalarExecutorSlot {
                            name: slot.name.clone(),
                            lane_start,
                            lanes,
                        };
                        lane_start += planned.lanes.len();
                        planned
                    })
                    .collect();
                Ok(ScalarExecutorState {
                    id: state.id,
                    function: state.function.clone(),
                    source_callable: state.source_callable.clone(),
                    slots,
                    transitions,
                })
            })
            .collect::<Result<Vec<_>, ScalarExecutorRejection>>()?;
        Ok(ScalarExecutorPlan {
            state_count: self.states.len(),
            max_lane_width: self.max_lane_width,
            states,
        })
    }

    /// Versioned, name-independent carrier ABI consumed by compiled backends.
    /// Function and slot names remain diagnostics only; runtime dispatch is by
    /// dense state id and fixed lane position.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        const VERSION: u8 = 2;
        let mut bytes = vec![VERSION];
        push_u32(&mut bytes, self.states.len());
        push_u32(&mut bytes, self.max_lane_width);
        for state in &self.states {
            push_u32(&mut bytes, state.id);
            bytes.push(match state.kind {
                CarrierStateKind::Entry => 0,
                CarrierStateKind::Segment => 1,
            });
            bytes.push(u8::from(state.direct));
            push_u32(&mut bytes, state.slots.len());
            for slot in &state.slots {
                bytes.push(convention_tag(slot.convention));
                match &slot.lanes {
                    Some(lanes) => {
                        bytes.push(1);
                        push_u32(&mut bytes, lanes.len());
                        bytes.extend(lanes.iter().map(|lane| match lane {
                            CarrierLane::I64 => 0,
                            CarrierLane::F64 => 1,
                            CarrierLane::ExternRef => 2,
                        }));
                    }
                    None => bytes.push(0),
                }
            }
        }
        bytes
    }
}

fn scalar_transitions_for_function(
    function: &Function,
    state_ids: &HashMap<String, usize>,
) -> Result<Vec<ScalarTransition>, String> {
    let terminal = terminal_block_expr(&function.body).ok_or_else(|| {
        "state body has no terminal expression".to_string()
    })?;
    let mut transitions = Vec::new();
    collect_scalar_transitions(terminal, state_ids, &mut transitions)?;
    if transitions.is_empty() {
        return Err("state body has no terminal transition".into());
    }
    Ok(transitions)
}

fn terminal_block_expr(block: &ast::Block) -> Option<&ast::Expr> {
    match block.stmts.last()? {
        ast::Stmt::Expr(expression) => Some(expression),
        ast::Stmt::Return(Some(expression)) => Some(expression),
        ast::Stmt::Return(None) => None,
        _ => None,
    }
}

fn collect_scalar_transitions(
    expression: &ast::Expr,
    state_ids: &HashMap<String, usize>,
    transitions: &mut Vec<ScalarTransition>,
) -> Result<(), String> {
    match expression {
        ast::Expr::Block(block) => {
            let terminal = terminal_block_expr(block)
                .ok_or_else(|| "terminal block has no value expression".to_string())?;
            collect_scalar_transitions(terminal, state_ids, transitions)
        }
        ast::Expr::If { then_block, else_block: Some(else_block), .. } => {
            let then_terminal = terminal_block_expr(then_block)
                .ok_or_else(|| "suspension-state `if` then-branch has no terminal value".to_string())?;
            let else_terminal = terminal_block_expr(else_block)
                .ok_or_else(|| "suspension-state `if` else-branch has no terminal value".to_string())?;
            collect_scalar_transitions(then_terminal, state_ids, transitions)?;
            collect_scalar_transitions(else_terminal, state_ids, transitions)
        }
        ast::Expr::If { else_block: None, .. } => {
            Err("suspension-state `if` is missing an else transition".into())
        }
        ast::Expr::Match { arms, .. } if !arms.is_empty() => {
            for arm in arms {
                collect_scalar_transitions(&arm.body, state_ids, transitions)?;
            }
            Ok(())
        }
        ast::Expr::Tuple(items) if items.is_empty() => {
            transitions.push(ScalarTransition::Done);
            Ok(())
        }
        ast::Expr::Ctor { name, args } if name == "Nil" && args.is_empty() => {
            transitions.push(ScalarTransition::Done);
            Ok(())
        }
        ast::Expr::Call { name, args } if state_ids.contains_key(name) => {
            let _ = args;
            transitions.push(ScalarTransition::Jump { target: state_ids[name] });
            Ok(())
        }
        ast::Expr::Call { name, args } if call_family(name, "task.run") && args.len() == 1 => {
            collect_task_expression(&args[0], None, state_ids, transitions)
        }
        ast::Expr::Call { name, args } if call_family(name, "task.lazy") && args.len() == 1 => {
            let ast::Expr::Lambda { body, .. } = &args[0] else {
                return Err("`task.lazy` state entry does not contain a lambda body".into());
            };
            let terminal = terminal_block_expr(body)
                .ok_or_else(|| "`task.lazy` lambda has no terminal task".to_string())?;
            collect_scalar_transitions(terminal, state_ids, transitions)
        }
        ast::Expr::Call { name, args }
            if call_family(name, "task.and_then") && args.len() == 2 =>
        {
            let resume = continuation_state(&args[1], state_ids)?;
            collect_task_expression(&args[0], Some(resume), state_ids, transitions)
        }
        ast::Expr::Call { name, .. }
            if call_family(name, "task.done") || call_family(name, "task.ready_unit") =>
        {
            transitions.push(ScalarTransition::Done);
            Ok(())
        }
        ast::Expr::Call { name, .. } => {
            Err(format!("unsupported terminal task call `{name}`"))
        }
        other => Err(format!("unsupported terminal state expression `{other:?}`")),
    }
}

fn collect_task_expression(
    task: &ast::Expr,
    resume: Option<usize>,
    state_ids: &HashMap<String, usize>,
    transitions: &mut Vec<ScalarTransition>,
) -> Result<(), String> {
    let ast::Expr::Call { name, args } = task else {
        return Err(format!("unsupported awaited task expression `{task:?}`"));
    };
    if let Some(&callee) = state_ids.get(name) {
        if let Some(resume) = resume {
            transitions.push(ScalarTransition::Call { callee, resume });
        } else {
            transitions.push(ScalarTransition::Jump { target: callee });
        }
        return Ok(());
    }
    if call_family(name, "task.lazy") && args.len() == 1 {
        let ast::Expr::Lambda { body, .. } = &args[0] else {
            return Err("awaited `task.lazy` does not contain a lambda".into());
        };
        let terminal = terminal_block_expr(body)
            .ok_or_else(|| "awaited `task.lazy` lambda has no terminal task".to_string())?;
        return collect_task_expression(terminal, resume, state_ids, transitions);
    }
    if call_family(name, "task.and_then") && args.len() == 2 {
        let continuation = continuation_state(&args[1], state_ids)?;
        return collect_task_expression(&args[0], Some(continuation), state_ids, transitions);
    }
    let resume = resume.ok_or_else(|| {
        format!("effect task `{name}` has no compiler-owned resume state")
    })?;
    let transition = if call_family(name, "chan.channel") {
        ScalarTransition::ChannelOpen { resume }
    } else if call_family(name, "chan.send") {
        ScalarTransition::ChannelSend { resume }
    } else if call_family(name, "chan.recv") {
        ScalarTransition::ChannelReceive { resume }
    } else if call_family(name, "chan.spawn") {
        let child = args
            .first()
            .and_then(|child| task_entry_state(child, state_ids))
            .ok_or_else(|| "`chan.spawn` child is not a compiler-owned state entry".to_string())?;
        ScalarTransition::Spawn { child, resume }
    } else if call_family(name, "chan.join") {
        ScalarTransition::Join { resume }
    } else if call_family(name, "chan.yield_now") || call_family(name, "task.yield_now") {
        ScalarTransition::Yield { resume }
    } else if call_family(name, "task.done") || call_family(name, "task.ready_unit") {
        ScalarTransition::Jump { target: resume }
    } else {
        return Err(format!("unsupported awaited task call `{name}`"));
    };
    transitions.push(transition);
    Ok(())
}

fn continuation_state(
    continuation: &ast::Expr,
    state_ids: &HashMap<String, usize>,
) -> Result<usize, String> {
    let ast::Expr::Lambda { body, .. } = continuation else {
        return Err("`task.and_then` continuation is not a lambda".into());
    };
    let terminal = terminal_block_expr(body)
        .ok_or_else(|| "continuation lambda has no terminal state call".to_string())?;
    task_entry_state(terminal, state_ids)
        .ok_or_else(|| format!("continuation does not tail-call a compiler state: `{terminal:?}`"))
}

fn task_entry_state(
    expression: &ast::Expr,
    state_ids: &HashMap<String, usize>,
) -> Option<usize> {
    match expression {
        ast::Expr::Block(block) => terminal_block_expr(block)
            .and_then(|terminal| task_entry_state(terminal, state_ids)),
        ast::Expr::Call { name, .. } => state_ids.get(name).copied(),
        _ => None,
    }
}

fn call_family(name: &str, family: &str) -> bool {
    name == family || name.strip_prefix(family).is_some_and(|suffix| suffix.starts_with("__"))
}

fn non_integer_channel(ty: &Type) -> Option<(String, &Type)> {
    match ty.unqualified() {
        Type::Named(name, arguments)
            if matches!(name.rsplit('.').next(), Some("Sender" | "Receiver"))
                && arguments.len() == 1
                && !matches!(arguments[0].unqualified(), Type::Named(message, args)
                    if message == "Int" && args.is_empty()) =>
        {
            Some((name.clone(), &arguments[0]))
        }
        Type::Named(_, arguments) | Type::Tuple(arguments) | Type::Dyn(_, arguments) => {
            arguments.iter().find_map(non_integer_channel)
        }
        Type::Fn(parameters, result, _) => parameters
            .iter()
            .find_map(non_integer_channel)
            .or_else(|| non_integer_channel(result)),
        Type::RecordCompose { base, fields } => non_integer_channel(base).or_else(|| {
            fields
                .iter()
                .find_map(|(_, field)| non_integer_channel(field))
        }),
        Type::Qualified(_, _) => unreachable!("unqualified above"),
    }
}

fn push_u32(bytes: &mut Vec<u8>, value: usize) {
    bytes.extend_from_slice(
        &u32::try_from(value)
            .expect("suspension carrier dimensions fit u32")
            .to_le_bytes(),
    );
}

fn convention_tag(convention: Convention) -> u8 {
    match convention {
        Convention::Let => 0,
        Convention::Borrow => 1,
        Convention::Var => 2,
        Convention::Own => 3,
    }
}

fn state_kind(function: &Function) -> Option<CarrierStateKind> {
    if function.attributes.iter().any(|attribute| attribute == FRAME_ENTRY_ATTRIBUTE) {
        Some(CarrierStateKind::Entry)
    } else if function.attributes.iter().any(|attribute| attribute == FRAME_FUNCTION_ATTRIBUTE) {
        Some(CarrierStateKind::Segment)
    } else {
        None
    }
}

fn inferred_parameters(typed: &TypedModule, function: &Function) -> Option<Vec<Type>> {
    let Type::Fn(parameters, _, _) = typed.table().function_type(&function.name)? else {
        return None;
    };
    Some(parameters)
}

fn type_definitions(module: &ast::Module) -> HashMap<String, &TypeDef> {
    module
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Type(definition) => Some((definition.name.clone(), definition)),
            _ => None,
        })
        .collect()
}

fn direct_lanes(
    ty: &Type,
    definitions: &HashMap<String, &TypeDef>,
    substitutions: &HashMap<String, Type>,
    visiting: &mut HashSet<String>,
) -> Option<Vec<CarrierLane>> {
    if crate::typeck::is_capability_type(ty) {
        return Some(vec![CarrierLane::ExternRef]);
    }
    match ty {
        Type::Qualified(_, inner) => direct_lanes(inner, definitions, substitutions, visiting),
        Type::Tuple(items) => flatten_all(items, definitions, substitutions, visiting),
        Type::Named(name, arguments) if arguments.is_empty() => {
            if let Some(actual) = substitutions.get(name) {
                return direct_lanes(actual, definitions, substitutions, visiting);
            }
            match name.as_str() {
                "Int" | "Bool" | "Duration" => Some(vec![CarrierLane::I64]),
                "Float" => Some(vec![CarrierLane::F64]),
                "Nil" => Some(Vec::new()),
                _ => flatten_nominal(name, arguments, definitions, substitutions, visiting),
            }
        }
        Type::Named(name, arguments) => {
            flatten_nominal(name, arguments, definitions, substitutions, visiting)
        }
        Type::Fn(_, _, _) | Type::Dyn(_, _) | Type::RecordCompose { .. } => None,
    }
}

fn flatten_all(
    fields: &[Type],
    definitions: &HashMap<String, &TypeDef>,
    substitutions: &HashMap<String, Type>,
    visiting: &mut HashSet<String>,
) -> Option<Vec<CarrierLane>> {
    let mut lanes = Vec::new();
    for field in fields {
        lanes.extend(direct_lanes(field, definitions, substitutions, visiting)?);
    }
    Some(lanes)
}

fn flatten_nominal(
    name: &str,
    arguments: &[Type],
    definitions: &HashMap<String, &TypeDef>,
    substitutions: &HashMap<String, Type>,
    visiting: &mut HashSet<String>,
) -> Option<Vec<CarrierLane>> {
    let definition = definitions.get(name).copied().or_else(|| {
        let short = name.rsplit('.').next()?;
        definitions.get(short).copied()
    })?;
    if !visiting.insert(definition.name.clone()) {
        return None;
    }
    let parameters = ast::effective_type_def_params(definition);
    if parameters.len() != arguments.len() {
        visiting.remove(&definition.name);
        return None;
    }
    let mut nested = substitutions.clone();
    nested.extend(parameters.into_iter().zip(arguments.iter().cloned()));
    let result = if definition.variants.len() == 1 {
        flatten_all(
            &definition.variants[0].fields,
            definitions,
            &nested,
            visiting,
        )
    } else {
        flatten_sum(definition, definitions, &nested, visiting)
    };
    visiting.remove(&definition.name);
    result
}

fn flatten_sum(
    definition: &TypeDef,
    definitions: &HashMap<String, &TypeDef>,
    substitutions: &HashMap<String, Type>,
    visiting: &mut HashSet<String>,
) -> Option<Vec<CarrierLane>> {
    let variants = definition
        .variants
        .iter()
        .map(|variant| flatten_all(&variant.fields, definitions, substitutions, visiting))
        .collect::<Option<Vec<_>>>()?;
    let width = variants.iter().map(Vec::len).max().unwrap_or(0);
    let mut lanes = Vec::with_capacity(width + 1);
    lanes.push(CarrierLane::I64);
    for index in 0..width {
        let mut lane = None;
        for fields in &variants {
            let Some(field) = fields.get(index) else { continue };
            if lane.is_some_and(|existing| existing != *field) {
                return None;
            }
            lane = Some(*field);
        }
        lanes.push(lane?);
    }
    Some(lanes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_catalog_flattens_owned_scalar_wrappers_and_pins_state_order() {
        let mut module = witchy_syntax::parser::parse_module(
            "type Id:\n    Id(Int)\n\nfn entry(id: Id, n: Int) -> Nil:\n    ()\n\nfn resume(own id: Id, own n: Int) -> Nil:\n    ()\n",
        )
        .expect("carrier fixture parses");
        let Item::Function(entry) = &mut module.items[1] else { panic!("entry") };
        entry.attributes.push(FRAME_ENTRY_ATTRIBUTE.into());
        entry
            .attributes
            .push(witchy_syntax::suspension::frame_state_attribute(4));
        let Item::Function(segment) = &mut module.items[2] else { panic!("segment") };
        segment.attributes.push(FRAME_FUNCTION_ATTRIBUTE.into());
        segment
            .attributes
            .push(witchy_syntax::suspension::frame_state_attribute(2));
        let mut boxed_module = module.clone();

        let typed = crate::typeck::annotate_checked(module).expect("fixture type checks");
        let catalog = SuspensionCarrierCatalog::from_typed(&typed).expect("carrier catalog");

        assert_eq!(catalog.states().iter().map(|state| state.id).collect::<Vec<_>>(), [0, 1]);
        assert!(catalog.is_wholly_direct());
        assert_eq!(catalog.max_lane_width(), 2);
        assert_eq!(catalog.states()[0].kind, CarrierStateKind::Entry);
        assert_eq!(catalog.states()[1].kind, CarrierStateKind::Segment);
        assert_eq!(catalog.states()[1].slots[0].convention, Convention::Own);
        assert_eq!(catalog.states()[1].slots[0].lanes, Some(vec![CarrierLane::I64]));
        assert_eq!(catalog.canonical_bytes()[0], 2);
        assert_eq!(catalog.canonical_bytes(), catalog.canonical_bytes());

        let Item::Function(boxed_segment) = &mut boxed_module.items[2] else {
            panic!("boxed segment")
        };
        boxed_segment.attributes.push(FRAME_BOXED_ATTRIBUTE.into());
        let boxed_typed =
            crate::typeck::annotate_checked(boxed_module).expect("boxed fixture type checks");
        let boxed =
            SuspensionCarrierCatalog::from_typed(&boxed_typed).expect("boxed carrier catalog");
        assert!(!boxed.is_wholly_direct());
        assert!(!boxed.states()[1].direct);
        let scalar = catalog.scalar_executor_plan().expect("scalar frame plan");
        assert_eq!(scalar.state_count, 2);
        assert_eq!(scalar.max_lane_width, 2);
        assert_eq!(scalar.states[1].source_callable, "resume");
        assert_eq!(scalar.states[1].slots[0].lane_start, 0);
        assert_eq!(scalar.states[1].slots[1].lane_start, 1);
        assert_eq!(
            boxed.scalar_executor_plan(),
            Err(ScalarExecutorRejection::BoxedState {
                function: "resume".into(),
            }),
        );
    }

    #[test]
    fn typed_catalog_flattens_capability_and_sum_resume_lanes() {
        let mut module = witchy_syntax::parser::parse_module(
            "type Maybe(a):\n    None\n    Some(a)\n\nfn entry(console: Console) -> Nil:\n    ()\n\nfn resume(console: Console, own value: Maybe(Int)) -> Nil:\n    ()\n",
        )
        .expect("mixed carrier fixture parses");
        let Item::Function(entry) = &mut module.items[1] else { panic!("entry") };
        entry.attributes.push(FRAME_ENTRY_ATTRIBUTE.into());
        entry
            .attributes
            .push(witchy_syntax::suspension::frame_state_attribute(0));
        let Item::Function(segment) = &mut module.items[2] else { panic!("segment") };
        segment.attributes.push(FRAME_FUNCTION_ATTRIBUTE.into());
        segment
            .attributes
            .push(witchy_syntax::suspension::frame_state_attribute(1));

        let typed = crate::typeck::annotate_checked(module).expect("fixture type checks");
        let catalog = SuspensionCarrierCatalog::from_typed(&typed).expect("carrier catalog");

        assert!(catalog.is_wholly_direct());
        assert_eq!(catalog.max_lane_width(), 3);
        assert_eq!(catalog.states()[0].slots[0].lanes, Some(vec![CarrierLane::ExternRef]));
        assert_eq!(
            catalog.states()[1].slots[1].lanes,
            Some(vec![CarrierLane::I64, CarrierLane::I64]),
        );
        let scalar = catalog.scalar_executor_plan().expect("scalar mixed-lane plan");
        assert_eq!(scalar.state_count, 2);
        assert_eq!(scalar.max_lane_width, 3);
        assert_eq!(scalar.states[1].slots[0].lane_start, 0);
        assert_eq!(scalar.states[1].slots[1].lane_start, 1);
        assert_eq!(scalar.states[1].slots[1].lanes.len(), 2);
    }

    #[test]
    fn scalar_executor_qualification_rejects_float_and_non_integer_channels() {
        let mut float_module = witchy_syntax::parser::parse_module(
            "fn resume(value: Float) -> Nil:\n    ()\n",
        )
        .expect("float fixture parses");
        let Item::Function(float_state) = &mut float_module.items[0] else {
            panic!("float state")
        };
        float_state.attributes.push(FRAME_FUNCTION_ATTRIBUTE.into());
        float_state
            .attributes
            .push(witchy_syntax::suspension::frame_state_attribute(0));
        let float_typed = crate::typeck::annotate_checked(float_module)
            .expect("float fixture type checks");
        let float_catalog = SuspensionCarrierCatalog::from_typed(&float_typed)
            .expect("float catalog");
        assert_eq!(
            float_catalog.scalar_executor_plan(),
            Err(ScalarExecutorRejection::NonScalarLane {
                function: "resume".into(),
                slot: "value".into(),
                lane: CarrierLane::F64,
            }),
        );

        let mut channel_module = witchy_syntax::parser::parse_module(
            "type Sender(a):\n    Sender(Int)\n\nfn resume(tx: Sender(String)) -> Nil:\n    ()\n",
        )
        .expect("channel fixture parses");
        let Item::Function(channel_state) = &mut channel_module.items[1] else {
            panic!("channel state")
        };
        channel_state.attributes.push(FRAME_FUNCTION_ATTRIBUTE.into());
        channel_state
            .attributes
            .push(witchy_syntax::suspension::frame_state_attribute(0));
        let channel_typed = crate::typeck::annotate_checked(channel_module)
            .expect("channel fixture type checks");
        let channel_catalog = SuspensionCarrierCatalog::from_typed(&channel_typed)
            .expect("channel catalog");
        assert!(channel_catalog.is_wholly_direct());
        assert_eq!(
            channel_catalog.scalar_executor_plan(),
            Err(ScalarExecutorRejection::NonIntegerChannel {
                channel: "Sender".into(),
                message: "String".into(),
            }),
        );
    }

    #[test]
    fn scalar_transition_plan_unwraps_lazy_and_then_effect_edges() {
        let module = witchy_syntax::parser::parse_module(
            "fn state() -> Nil:\n    task.lazy__Nil(fn(): task.and_then__Int__Nil(chan.send__Int(tx, value), fn(ignored): resume()))\n",
        )
        .expect("transition fixture parses");
        let Item::Function(function) = &module.items[0] else {
            panic!("state function")
        };
        let state_ids = HashMap::from_iter([("resume".to_string(), 7)]);

        assert_eq!(
            scalar_transitions_for_function(function, &state_ids),
            Ok(vec![ScalarTransition::ChannelSend { resume: 7 }]),
        );
    }
}
