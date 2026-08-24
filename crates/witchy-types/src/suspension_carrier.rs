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

use crate::typeck::{Ty, TypeTable, TypedModule};

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

/// Direction is part of the channel ABI: a sender and receiver share the
/// compact runtime channel id, but have different ready and suspension rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelEndpointDirection {
    Send,
    Receive,
}

/// A buffer-root message occupies one direct root lane, while preserving its
/// language-level identity.  `String` and `Bytes` deliberately share their
/// ownership representation but not their typed equality or public API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelBufferRoot {
    String,
    Bytes,
}

/// Exact representation available at a typed-channel boundary.  `Packed`
/// includes the direct lanes of a closed product/single-variant record.  A
/// multi-variant or open representation is deliberately `Boxed`; later
/// ready-path emitters must not guess a layout from the erased `__Msg` slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelPayloadLayout {
    Scalar(CarrierLane),
    BufferRoot(ChannelBufferRoot),
    Packed(Vec<CarrierLane>),
    Boxed,
}

/// The ownership operation a future ready/wait emitter must perform when a
/// message crosses the endpoint.  This keeps the representation decision
/// explicit: a buffer root is retained/released as a root, whereas scalar and
/// packed values are transferred in lanes.  An erased fallback remains boxed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelOwnershipEnvelope {
    InlineValue,
    BufferRoot,
    BoxedValue,
}

/// Typed, source-independent description of one endpoint held by a generated
/// suspension state.  This is the common input for one-way ready operations,
/// fixed-arity select, and the eventual bounded wait registration; it is not a
/// promise that every layout has a promoted emitter yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypedChannelPlan {
    pub state: usize,
    pub endpoint_place: String,
    pub direction: ChannelEndpointDirection,
    pub message_type: String,
    pub payload_layout: ChannelPayloadLayout,
    pub ownership: ChannelOwnershipEnvelope,
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
    typed_channel_plans: Vec<TypedChannelPlan>,
    scalar_transitions: Vec<Result<Vec<ScalarTransition>, String>>,
}

/// Closed eligibility proof for RFC-0059's allocation-free compiled scheduler.
/// The lowering may consume this value without repeating a source-shape guess:
/// every frame state is direct, no frame column needs a floating/boxed lane, and
/// every typed channel endpoint has a payload layout claimed by a direct
/// emitter.  The emitter remains free to decline a particular graph; this
/// catalog is only the typed ABI proof and must not rediscover an erased message
/// layout from a runtime slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScalarExecutorPlan {
    pub state_count: usize,
    pub max_lane_width: usize,
    pub states: Vec<ScalarExecutorState>,
    /// The exact channel endpoint layouts retained for backend code generation.
    /// A current scalar executor may decline to synthesize a graph, but it may
    /// not rediscover this information from erased runtime slots.
    pub typed_channels: Vec<TypedChannelPlan>,
}

/// Typed eligibility proof for the RFC-0152 ready-path emitter.
///
/// This intentionally differs from [`ScalarExecutorPlan`].  The scalar
/// executor is an already-landed, narrow implementation whose data path is
/// limited to integer lanes.  A ready-path emitter needs the complete direct
/// representation contract before it can choose a ring read/write or a
/// bounded wait registration, including buffer roots and arbitrary packed
/// lanes.  Constructing this plan does not enable an optimization by itself;
/// it gives every later emitter the same typed rejection boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadyChannelRegionPlan {
    pub state_count: usize,
    pub max_lane_width: usize,
    pub states: Vec<ScalarExecutorState>,
    pub typed_channels: Vec<TypedChannelPlan>,
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
pub enum SelectResultUse {
    ImmediateMatch,
    Escape,
}

/// Whether a select arm names one stable endpoint place.  A dynamic expression
/// is a normal scheduler fallback, not a string sentinel that a later ready
/// emitter could mistake for an ordinary local named `unknown`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelEndpointPlace {
    Static(String),
    Dynamic,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AwaitSelect2Plan {
    /// Receiver arms in source order.  Ready-path lowering may promote only
    /// two static places; dynamic places retain the ordinary scheduler.
    pub first_channel: ChannelEndpointPlace,
    pub second_channel: ChannelEndpointPlace,
    /// Exact layout recovered from the two typed receiver endpoints.  The
    /// scheduler still uses its erased `__Msg` fallback until an emitter claims
    /// this plan; that fallback must never infer a replacement layout itself.
    pub payload_layout: ChannelPayloadLayout,
    /// The typed scalar lane recovered from both receiver endpoints.  `None`
    /// deliberately keeps the ordinary erased-message path; it is never an
    /// invitation to infer a representation from the runtime `__Msg` slot.
    pub payload_lane: Option<CarrierLane>,
    pub resume_target: usize,
    pub result_use: SelectResultUse,
}

/// Exact facts for a fixed-arity select with three or four receivers.
///
/// The endpoint vector is deliberately bounded at construction time. It is not
/// a dynamic receiver list: source order is part of the carrier ABI, and a
/// ready-path emitter can test each endpoint without reconstructing that list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AwaitFixedSelectPlan {
    pub endpoints: Vec<ChannelEndpointPlace>,
    pub payload_layout: ChannelPayloadLayout,
    pub payload_lane: Option<CarrierLane>,
    pub resume_target: usize,
    pub result_use: SelectResultUse,
}

/// Exact typed facts for one awaited send or receive.  The generic executor
/// still owns the fallback, but a ready-path emitter receives the endpoint
/// place, message representation, and transfer discipline together instead of
/// rediscovering any of them from an erased `__Msg` slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AwaitChannelOperationPlan {
    pub endpoint: ChannelEndpointPlace,
    pub payload_layout: ChannelPayloadLayout,
    pub ownership: ChannelOwnershipEnvelope,
    pub resume_target: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScalarTransition {
    Jump { target: usize },
    Done,
    ChannelOpen { resume: usize },
    ChannelSend(AwaitChannelOperationPlan),
    ChannelReceive(AwaitChannelOperationPlan),
    ChannelSelect2(AwaitSelect2Plan),
    ChannelSelect3(AwaitFixedSelectPlan),
    ChannelSelect4(AwaitFixedSelectPlan),
    Spawn { child: usize, resume: usize },
    Join { resume: usize },
    Yield { resume: usize },
    Call { callee: usize, resume: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScalarExecutorRejection {
    NoSuspensionStates,
    BoxedState {
        function: String,
    },
    NonScalarLane {
        function: String,
        slot: String,
        lane: CarrierLane,
    },
    NonIntegerChannel {
        channel: String,
        message: String,
    },
    UnsupportedTransition {
        function: String,
        detail: String,
    },
}

/// Why an async region must retain the ordinary scheduler until a later
/// RFC-0152 emitter supports its exact representation or endpoint lifetime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadyChannelRegionRejection {
    NoSuspensionStates,
    BoxedState {
        function: String,
    },
    BoxedChannel {
        channel: String,
        message: String,
    },
    DynamicEndpoint {
        function: String,
        operation: &'static str,
    },
    UnsupportedTransition {
        function: String,
        detail: String,
    },
}

impl SuspensionCarrierCatalog {
    pub fn from_typed(typed: &TypedModule) -> Result<Self, String> {
        let definitions = type_definitions(typed.module());
        let mut states = Vec::new();

        for item in &typed.module().items {
            let Item::Function(function) = item else {
                continue;
            };
            let Some(source_state) = frame_state(function) else {
                continue;
            };
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
                    let lanes =
                        direct_lanes(&ty, &definitions, &HashMap::new(), &mut HashSet::new());
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

        let max_lane_width = states
            .iter()
            .map(CarrierState::lane_width)
            .max()
            .unwrap_or(0);
        let typed_channel_plans = states
            .iter()
            .flat_map(|state| {
                state.slots.iter().filter_map(|slot| {
                    let (direction, message) = channel_endpoint_type(&slot.ty)?;
                    let payload_layout = channel_payload_layout(message, &definitions);
                    Some(TypedChannelPlan {
                        state: state.id,
                        endpoint_place: slot.name.clone(),
                        direction,
                        message_type: witchy_syntax::format::type_str(message),
                        ownership: channel_ownership_envelope(&payload_layout),
                        payload_layout,
                    })
                })
            })
            .collect();
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
                    format!(
                        "carrier state `{}` has no typed function body",
                        state.function
                    )
                })?;
                scalar_transitions_for_function(function, &state_ids, typed.table(), &definitions)
            })
            .collect();
        Ok(Self {
            states,
            max_lane_width,
            typed_channel_plans,
            scalar_transitions,
        })
    }

    pub fn states(&self) -> &[CarrierState] {
        &self.states
    }

    pub fn max_lane_width(&self) -> usize {
        self.max_lane_width
    }

    /// Recover every typed channel endpoint before source lowering erases the
    /// message to `__Msg`.  Results are in deterministic state/parameter order
    /// and can therefore be used as a compiler ABI without source-name sorting.
    pub fn typed_channel_plans(&self) -> &[TypedChannelPlan] {
        &self.typed_channel_plans
    }

    pub fn is_wholly_direct(&self) -> bool {
        !self.states.is_empty() && self.states.iter().all(CarrierState::is_direct)
    }

    /// Prove that the module can use the scalar/capability scheduler described by
    /// RFC-0059 increment 2. Capability lanes remain direct roots; all mutable
    /// per-task columns are integer lanes. Channel endpoints are checked from the
    /// finalized state-slot types, so `Sender(String)` cannot qualify merely
    /// because its runtime channel id is an integer wrapper.
    pub fn scalar_executor_plan(&self) -> Result<ScalarExecutorPlan, ScalarExecutorRejection> {
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
            }
        }
        for channel in &self.typed_channel_plans {
            if !scalar_executor_payload_supported(&channel.payload_layout) {
                return Err(ScalarExecutorRejection::NonIntegerChannel {
                    channel: channel.endpoint_place.clone(),
                    message: channel.message_type.clone(),
                });
            }
        }
        let states = self
            .states
            .iter()
            .enumerate()
            .map(|(state_index, state)| {
                let transitions =
                    self.scalar_transitions[state_index]
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
            typed_channels: self.typed_channel_plans.clone(),
        })
    }

    /// Recover the full typed input contract for a future compiler-owned ready
    /// region.  In contrast to [`Self::scalar_executor_plan`], every direct
    /// carrier lane is preserved and String/Bytes roots remain eligible with
    /// their explicit ownership envelope.  Dynamic endpoints and erased
    /// payloads reject before code generation; they never become a later
    /// runtime-layout guess.
    pub fn ready_channel_region_plan(
        &self,
    ) -> Result<ReadyChannelRegionPlan, ReadyChannelRegionRejection> {
        if self.states.is_empty() {
            return Err(ReadyChannelRegionRejection::NoSuspensionStates);
        }
        for state in &self.states {
            if !state.is_direct() {
                return Err(ReadyChannelRegionRejection::BoxedState {
                    function: state.function.clone(),
                });
            }
        }
        for channel in &self.typed_channel_plans {
            if matches!(channel.payload_layout, ChannelPayloadLayout::Boxed) {
                return Err(ReadyChannelRegionRejection::BoxedChannel {
                    channel: channel.endpoint_place.clone(),
                    message: channel.message_type.clone(),
                });
            }
        }
        let states = self
            .states
            .iter()
            .enumerate()
            .map(|(state_index, state)| {
                let transitions = self.scalar_transitions[state_index]
                    .clone()
                    .map_err(|detail| ReadyChannelRegionRejection::UnsupportedTransition {
                        function: state.function.clone(),
                        detail,
                    })?;
                for transition in &transitions {
                    ready_transition_is_static(transition).map_err(|operation| {
                        ReadyChannelRegionRejection::DynamicEndpoint {
                            function: state.function.clone(),
                            operation,
                        }
                    })?;
                }
                let mut lane_start = 0;
                let slots = state
                    .slots
                    .iter()
                    .map(|slot| {
                        let lanes = slot
                            .lanes
                            .clone()
                            .expect("ready-region qualification rejected boxed frame slots");
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
            .collect::<Result<Vec<_>, ReadyChannelRegionRejection>>()?;
        Ok(ReadyChannelRegionPlan {
            state_count: self.states.len(),
            max_lane_width: self.max_lane_width,
            states,
            typed_channels: self.typed_channel_plans.clone(),
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

fn ready_transition_is_static(transition: &ScalarTransition) -> Result<(), &'static str> {
    let is_static = |endpoint: &ChannelEndpointPlace| {
        matches!(endpoint, ChannelEndpointPlace::Static(_))
    };
    match transition {
        ScalarTransition::ChannelSend(operation) if !is_static(&operation.endpoint) => Err("send"),
        ScalarTransition::ChannelReceive(operation) if !is_static(&operation.endpoint) => {
            Err("receive")
        }
        ScalarTransition::ChannelSelect2(select)
            if !is_static(&select.first_channel) || !is_static(&select.second_channel) =>
        {
            Err("select")
        }
        ScalarTransition::ChannelSelect3(select) | ScalarTransition::ChannelSelect4(select)
            if select.endpoints.iter().any(|endpoint| !is_static(endpoint)) =>
        {
            Err("select")
        }
        _ => Ok(()),
    }
}

/// The first closure-free executor can transport a scalar integer or an exact
/// two-integer product.  String/Bytes roots and wider products stay on the
/// scheduler until their retain/release envelope is emitted, rather than being
/// silently treated as integer lanes.
fn scalar_executor_payload_supported(payload: &ChannelPayloadLayout) -> bool {
    match payload {
        ChannelPayloadLayout::Scalar(CarrierLane::I64) => true,
        ChannelPayloadLayout::Packed(lanes) => {
            lanes.as_slice() == [CarrierLane::I64, CarrierLane::I64]
        }
        _ => false,
    }
}

fn scalar_transitions_for_function(
    function: &Function,
    state_ids: &HashMap<String, usize>,
    table: &TypeTable,
    definitions: &HashMap<String, &TypeDef>,
) -> Result<Vec<ScalarTransition>, String> {
    let terminal = terminal_block_expr(&function.body)
        .ok_or_else(|| "state body has no terminal expression".to_string())?;
    let mut transitions = Vec::new();
    collect_scalar_transitions(terminal, state_ids, table, definitions, &mut transitions)?;
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
    table: &TypeTable,
    definitions: &HashMap<String, &TypeDef>,
    transitions: &mut Vec<ScalarTransition>,
) -> Result<(), String> {
    match expression {
        ast::Expr::Block(block) => {
            let terminal = terminal_block_expr(block)
                .ok_or_else(|| "terminal block has no value expression".to_string())?;
            collect_scalar_transitions(terminal, state_ids, table, definitions, transitions)
        }
        ast::Expr::If {
            then_block,
            else_block: Some(else_block),
            ..
        } => {
            let then_terminal = terminal_block_expr(then_block).ok_or_else(|| {
                "suspension-state `if` then-branch has no terminal value".to_string()
            })?;
            let else_terminal = terminal_block_expr(else_block).ok_or_else(|| {
                "suspension-state `if` else-branch has no terminal value".to_string()
            })?;
            collect_scalar_transitions(then_terminal, state_ids, table, definitions, transitions)?;
            collect_scalar_transitions(else_terminal, state_ids, table, definitions, transitions)
        }
        ast::Expr::If {
            else_block: None, ..
        } => Err("suspension-state `if` is missing an else transition".into()),
        ast::Expr::Match { arms, .. } if !arms.is_empty() => {
            for arm in arms {
                collect_scalar_transitions(&arm.body, state_ids, table, definitions, transitions)?;
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
            transitions.push(ScalarTransition::Jump {
                target: state_ids[name],
            });
            Ok(())
        }
        ast::Expr::Call { name, args } if call_family(name, "task.run") && args.len() == 1 => {
            collect_task_expression(&args[0], None, state_ids, table, definitions, transitions)
        }
        ast::Expr::Call { name, args } if call_family(name, "task.lazy") && args.len() == 1 => {
            let ast::Expr::Lambda { body, .. } = &args[0] else {
                return Err("`task.lazy` state entry does not contain a lambda body".into());
            };
            let terminal = terminal_block_expr(body)
                .ok_or_else(|| "`task.lazy` lambda has no terminal task".to_string())?;
            collect_scalar_transitions(terminal, state_ids, table, definitions, transitions)
        }
        ast::Expr::Call { name, args } if call_family(name, "task.and_then") && args.len() == 2 => {
            let resume = continuation_state(&args[1], state_ids)?;
            collect_task_expression(&args[0], Some(resume), state_ids, table, definitions, transitions)
        }
        ast::Expr::Call { name, .. } if call_family(name, "chan.__select2_map") => {
            collect_task_expression(expression, None, state_ids, table, definitions, transitions)
        }
        ast::Expr::Call { name, .. }
            if call_family(name, "task.done") || call_family(name, "task.ready_unit") =>
        {
            transitions.push(ScalarTransition::Done);
            Ok(())
        }
        ast::Expr::Call { name, .. } => Err(format!("unsupported terminal task call `{name}`")),
        other => Err(format!("unsupported terminal state expression `{other:?}`")),
    }
}

fn collect_task_expression(
    task: &ast::Expr,
    resume: Option<usize>,
    state_ids: &HashMap<String, usize>,
    table: &TypeTable,
    definitions: &HashMap<String, &TypeDef>,
    transitions: &mut Vec<ScalarTransition>,
) -> Result<(), String> {
    // An async entry block may execute a direct effectful prologue before it
    // constructs its first task. The carrier graph starts at that terminal
    // task; lowering preserves the WIR prologue before replacing only the
    // proved closed region. Do not require the prologue itself to be pure:
    // its once-only source-order execution is observable.
    if let ast::Expr::Block(block) = task {
        let terminal = terminal_block_expr(block)
            .ok_or_else(|| "awaited task block has no terminal task".to_string())?;
        return collect_task_expression(terminal, resume, state_ids, table, definitions, transitions);
    }
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
        return collect_task_expression(terminal, resume, state_ids, table, definitions, transitions);
    }
    if call_family(name, "task.and_then") && args.len() == 2 {
        let continuation = continuation_state(&args[1], state_ids)?;
        return collect_task_expression(&args[0], Some(continuation), state_ids, table, definitions, transitions);
    }
    if call_family(name, "chan.__select2_map") {
        let resume_target = continuation_state(
            args.last().ok_or_else(|| "fused select has no continuation".to_string())?,
            state_ids,
        )?;
        transitions.push(ScalarTransition::ChannelSelect2(AwaitSelect2Plan {
            first_channel: channel_endpoint_place(args.first()),
            second_channel: channel_endpoint_place(args.get(1)),
            payload_layout: select_payload_layout(table, args.first(), args.get(1), definitions),
            payload_lane: select_payload_lane(table, args.first(), args.get(1)),
            resume_target,
            result_use: SelectResultUse::ImmediateMatch,
        }));
        return Ok(());
    }
    if call_family(name, "chan.__select3_map") || call_family(name, "task.__channel_select3") {
        let resume_target = continuation_state(
            args.last().ok_or_else(|| "fused select has no continuation".to_string())?,
            state_ids,
        )?;
        transitions.push(ScalarTransition::ChannelSelect3(fixed_select_plan(
            table,
            args,
            3,
            definitions,
            resume_target,
        )));
        return Ok(());
    }
    if call_family(name, "chan.__select4_map") || call_family(name, "task.__channel_select4") {
        let resume_target = continuation_state(
            args.last().ok_or_else(|| "fused select has no continuation".to_string())?,
            state_ids,
        )?;
        transitions.push(ScalarTransition::ChannelSelect4(fixed_select_plan(
            table,
            args,
            4,
            definitions,
            resume_target,
        )));
        return Ok(());
    }
    let resume =
        resume.ok_or_else(|| format!("effect task `{name}` has no compiler-owned resume state"))?;
    let transition = if call_family(name, "chan.channel") {
        ScalarTransition::ChannelOpen { resume }
    } else if call_family(name, "chan.send") {
        ScalarTransition::ChannelSend(channel_operation_plan(
            table,
            args.first(),
            ChannelEndpointDirection::Send,
            definitions,
            resume,
        ))
    } else if call_family(name, "chan.recv") {
        ScalarTransition::ChannelReceive(channel_operation_plan(
            table,
            args.first(),
            ChannelEndpointDirection::Receive,
            definitions,
            resume,
        ))
    } else if call_family(name, "task.__channel_select2") {
        let first_channel = channel_endpoint_place(args.first());
        let second_channel = channel_endpoint_place(args.get(1));
        ScalarTransition::ChannelSelect2(AwaitSelect2Plan {
            first_channel,
            second_channel,
            payload_layout: select_payload_layout(table, args.first(), args.get(1), definitions),
            payload_lane: select_payload_lane(table, args.first(), args.get(1)),
            resume_target: resume,
            result_use: SelectResultUse::ImmediateMatch,
        })
    } else if call_family(name, "task.__channel_select3") {
        ScalarTransition::ChannelSelect3(fixed_select_plan(
            table,
            args,
            3,
            definitions,
            resume,
        ))
    } else if call_family(name, "task.__channel_select4") {
        ScalarTransition::ChannelSelect4(fixed_select_plan(
            table,
            args,
            4,
            definitions,
            resume,
        ))
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

fn channel_endpoint_place(endpoint: Option<&ast::Expr>) -> ChannelEndpointPlace {
    match endpoint {
        Some(ast::Expr::Var(name)) => ChannelEndpointPlace::Static(name.clone()),
        _ => ChannelEndpointPlace::Dynamic,
    }
}

fn channel_operation_plan(
    table: &TypeTable,
    endpoint: Option<&ast::Expr>,
    direction: ChannelEndpointDirection,
    definitions: &HashMap<String, &TypeDef>,
    resume_target: usize,
) -> AwaitChannelOperationPlan {
    let payload_layout = endpoint
        .and_then(|endpoint| table.type_of(endpoint))
        .and_then(|ty| channel_endpoint_payload_layout(ty, direction, definitions))
        .unwrap_or(ChannelPayloadLayout::Boxed);
    let ownership = channel_ownership_envelope(&payload_layout);
    AwaitChannelOperationPlan {
        endpoint: channel_endpoint_place(endpoint),
        payload_layout,
        ownership,
        resume_target,
    }
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

fn task_entry_state(expression: &ast::Expr, state_ids: &HashMap<String, usize>) -> Option<usize> {
    match expression {
        ast::Expr::Block(block) => {
            terminal_block_expr(block).and_then(|terminal| task_entry_state(terminal, state_ids))
        }
        ast::Expr::Call { name, .. } => state_ids.get(name).copied(),
        _ => None,
    }
}

fn select_payload_lane(
    table: &TypeTable,
    first: Option<&ast::Expr>,
    second: Option<&ast::Expr>,
) -> Option<CarrierLane> {
    select_payload_lane_types(
        first.and_then(|expr| table.type_of(expr)),
        second.and_then(|expr| table.type_of(expr)),
    )
}

fn select_payload_layout(
    table: &TypeTable,
    first: Option<&ast::Expr>,
    second: Option<&ast::Expr>,
    definitions: &HashMap<String, &TypeDef>,
) -> ChannelPayloadLayout {
    let Some((first, second)) = first
        .and_then(|expr| table.type_of(expr))
        .zip(second.and_then(|expr| table.type_of(expr)))
    else {
        return ChannelPayloadLayout::Boxed;
    };
    let (Ty::Named(first_name, first_args), Ty::Named(second_name, second_args)) = (first, second)
    else {
        return ChannelPayloadLayout::Boxed;
    };
    if first_name.rsplit('.').next() != Some("Receiver")
        || second_name.rsplit('.').next() != Some("Receiver")
        || first_args.len() != 1
        || second_args.len() != 1
        || first_args != second_args
    {
        return ChannelPayloadLayout::Boxed;
    }
    crate::typeck::ty_to_ast(&first_args[0])
        .map(|message| channel_payload_layout(&message, definitions))
        .unwrap_or(ChannelPayloadLayout::Boxed)
}

fn fixed_select_plan(
    table: &TypeTable,
    args: &[ast::Expr],
    arity: usize,
    definitions: &HashMap<String, &TypeDef>,
    resume_target: usize,
) -> AwaitFixedSelectPlan {
    let endpoints = args
        .iter()
        .take(arity)
        .map(|endpoint| channel_endpoint_place(Some(endpoint)))
        .collect::<Vec<_>>();
    let endpoint_types = args
        .iter()
        .take(arity)
        .map(|endpoint| table.type_of(endpoint))
        .collect::<Vec<_>>();
    let payload_layout = fixed_select_payload_layout(&endpoint_types, definitions);
    let payload_lane = fixed_select_payload_lane(&endpoint_types);
    AwaitFixedSelectPlan {
        endpoints,
        payload_layout,
        payload_lane,
        resume_target,
        result_use: SelectResultUse::ImmediateMatch,
    }
}

fn fixed_select_payload_layout(
    endpoints: &[Option<&Ty>],
    definitions: &HashMap<String, &TypeDef>,
) -> ChannelPayloadLayout {
    let Some(Some(Ty::Named(first_name, first_args))) = endpoints.first() else {
        return ChannelPayloadLayout::Boxed;
    };
    if endpoints.len() < 3
        || first_name.rsplit('.').next() != Some("Receiver")
        || first_args.len() != 1
        || endpoints.iter().skip(1).any(|endpoint| {
            !matches!(endpoint, Some(Ty::Named(name, args))
                if name.rsplit('.').next() == Some("Receiver")
                    && args.len() == 1
                    && args == first_args)
        })
    {
        return ChannelPayloadLayout::Boxed;
    }
    crate::typeck::ty_to_ast(&first_args[0])
        .map(|message| channel_payload_layout(&message, definitions))
        .unwrap_or(ChannelPayloadLayout::Boxed)
}

fn fixed_select_payload_lane(endpoints: &[Option<&Ty>]) -> Option<CarrierLane> {
    (endpoints.len() >= 3
        && endpoints.iter().all(|endpoint| {
            matches!(endpoint, Some(Ty::Named(name, args))
                if name.rsplit('.').next() == Some("Receiver") && args.as_slice() == [Ty::Int])
        }))
    .then_some(CarrierLane::I64)
}

fn channel_endpoint_payload_layout(
    ty: &Ty,
    direction: ChannelEndpointDirection,
    definitions: &HashMap<String, &TypeDef>,
) -> Option<ChannelPayloadLayout> {
    let Ty::Named(name, arguments) = ty else { return None };
    let expected_name = match direction {
        ChannelEndpointDirection::Send => "Sender",
        ChannelEndpointDirection::Receive => "Receiver",
    };
    if name.rsplit('.').next() != Some(expected_name) || arguments.len() != 1 {
        return None;
    }
    crate::typeck::ty_to_ast(&arguments[0])
        .map(|message| channel_payload_layout(&message, definitions))
}

fn select_payload_lane_types(first: Option<&Ty>, second: Option<&Ty>) -> Option<CarrierLane> {
    match (first, second) {
        (Some(Ty::Named(first_name, first_args)), Some(Ty::Named(second_name, second_args)))
            if first_name.rsplit('.').next() == Some("Receiver")
                && second_name.rsplit('.').next() == Some("Receiver")
                && first_args.as_slice() == [Ty::Int]
                && second_args.as_slice() == [Ty::Int] =>
        {
            Some(CarrierLane::I64)
        }
        _ => None,
    }
}

fn call_family(name: &str, family: &str) -> bool {
    name == family
        || name
            .strip_prefix(family)
            .is_some_and(|suffix| suffix.starts_with("__"))
}

fn channel_endpoint_type(ty: &Type) -> Option<(ChannelEndpointDirection, &Type)> {
    let Type::Named(name, arguments) = ty.unqualified() else {
        return None;
    };
    let direction = match name.rsplit('.').next()? {
        "Sender" => ChannelEndpointDirection::Send,
        "Receiver" => ChannelEndpointDirection::Receive,
        _ => return None,
    };
    (arguments.len() == 1).then_some((direction, &arguments[0]))
}

fn channel_payload_layout(
    message: &Type,
    definitions: &HashMap<String, &TypeDef>,
) -> ChannelPayloadLayout {
    if let Type::Named(name, arguments) = message.unqualified()
        && arguments.is_empty()
    {
        match name.as_str() {
            "Int" | "Bool" | "Duration" => return ChannelPayloadLayout::Scalar(CarrierLane::I64),
            "Float" => return ChannelPayloadLayout::Scalar(CarrierLane::F64),
            "String" => return ChannelPayloadLayout::BufferRoot(ChannelBufferRoot::String),
            "Bytes" => return ChannelPayloadLayout::BufferRoot(ChannelBufferRoot::Bytes),
            "Nil" => return ChannelPayloadLayout::Packed(Vec::new()),
            _ => {}
        }
    }
    direct_lanes(message, definitions, &HashMap::new(), &mut HashSet::new())
        .filter(|lanes| !lanes.is_empty())
        .map(ChannelPayloadLayout::Packed)
        .unwrap_or(ChannelPayloadLayout::Boxed)
}

fn channel_ownership_envelope(layout: &ChannelPayloadLayout) -> ChannelOwnershipEnvelope {
    match layout {
        ChannelPayloadLayout::Scalar(_) | ChannelPayloadLayout::Packed(_) => {
            ChannelOwnershipEnvelope::InlineValue
        }
        ChannelPayloadLayout::BufferRoot(_) => ChannelOwnershipEnvelope::BufferRoot,
        ChannelPayloadLayout::Boxed => ChannelOwnershipEnvelope::BoxedValue,
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
    if function
        .attributes
        .iter()
        .any(|attribute| attribute == FRAME_ENTRY_ATTRIBUTE)
    {
        Some(CarrierStateKind::Entry)
    } else if function
        .attributes
        .iter()
        .any(|attribute| attribute == FRAME_FUNCTION_ATTRIBUTE)
    {
        Some(CarrierStateKind::Segment)
    } else {
        None
    }
}

fn inferred_parameters(typed: &TypedModule, function: &Function) -> Option<Vec<Type>> {
    let Type::Fn(parameters, _, _, _) = typed.table().function_type(&function.name)? else {
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
        Type::Slice(..) | Type::Fn(..) | Type::Dyn(_, _) | Type::RecordCompose { .. } => None,
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
            let Some(field) = fields.get(index) else {
                continue;
            };
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
    fn select_endpoint_places_do_not_use_a_local_name_as_a_dynamic_sentinel() {
        assert_eq!(
            channel_endpoint_place(Some(&ast::Expr::Var("unknown".into()))),
            ChannelEndpointPlace::Static("unknown".into()),
            "a real local may use the former sentinel spelling"
        );
        assert_eq!(
            channel_endpoint_place(Some(&ast::Expr::Int(0))),
            ChannelEndpointPlace::Dynamic,
            "non-place receiver expressions must retain the scheduler fallback"
        );
    }

    #[test]
    fn typed_catalog_flattens_owned_scalar_wrappers_and_pins_state_order() {
        let mut module = witchy_syntax::parser::parse_module(
            "type Id:\n    Id(Int)\n\nfn entry(id: Id, n: Int) -> Nil:\n    ()\n\nfn resume(own id: Id, own n: Int) -> Nil:\n    ()\n",
        )
        .expect("carrier fixture parses");
        let Item::Function(entry) = &mut module.items[1] else {
            panic!("entry")
        };
        entry.attributes.push(FRAME_ENTRY_ATTRIBUTE.into());
        entry
            .attributes
            .push(witchy_syntax::suspension::frame_state_attribute(4));
        let Item::Function(segment) = &mut module.items[2] else {
            panic!("segment")
        };
        segment.attributes.push(FRAME_FUNCTION_ATTRIBUTE.into());
        segment
            .attributes
            .push(witchy_syntax::suspension::frame_state_attribute(2));
        let mut boxed_module = module.clone();

        let typed = crate::typeck::annotate_checked(module).expect("fixture type checks");
        let catalog = SuspensionCarrierCatalog::from_typed(&typed).expect("carrier catalog");

        assert_eq!(
            catalog
                .states()
                .iter()
                .map(|state| state.id)
                .collect::<Vec<_>>(),
            [0, 1]
        );
        assert!(catalog.is_wholly_direct());
        assert_eq!(catalog.max_lane_width(), 2);
        assert_eq!(catalog.states()[0].kind, CarrierStateKind::Entry);
        assert_eq!(catalog.states()[1].kind, CarrierStateKind::Segment);
        assert_eq!(catalog.states()[1].slots[0].convention, Convention::Own);
        assert_eq!(
            catalog.states()[1].slots[0].lanes,
            Some(vec![CarrierLane::I64])
        );
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
        let Item::Function(entry) = &mut module.items[1] else {
            panic!("entry")
        };
        entry.attributes.push(FRAME_ENTRY_ATTRIBUTE.into());
        entry
            .attributes
            .push(witchy_syntax::suspension::frame_state_attribute(0));
        let Item::Function(segment) = &mut module.items[2] else {
            panic!("segment")
        };
        segment.attributes.push(FRAME_FUNCTION_ATTRIBUTE.into());
        segment
            .attributes
            .push(witchy_syntax::suspension::frame_state_attribute(1));

        let typed = crate::typeck::annotate_checked(module).expect("fixture type checks");
        let catalog = SuspensionCarrierCatalog::from_typed(&typed).expect("carrier catalog");

        assert!(catalog.is_wholly_direct());
        assert_eq!(catalog.max_lane_width(), 3);
        assert_eq!(
            catalog.states()[0].slots[0].lanes,
            Some(vec![CarrierLane::ExternRef])
        );
        assert_eq!(
            catalog.states()[1].slots[1].lanes,
            Some(vec![CarrierLane::I64, CarrierLane::I64]),
        );
        let scalar = catalog
            .scalar_executor_plan()
            .expect("scalar mixed-lane plan");
        assert_eq!(scalar.state_count, 2);
        assert_eq!(scalar.max_lane_width, 3);
        assert_eq!(scalar.states[1].slots[0].lane_start, 0);
        assert_eq!(scalar.states[1].slots[1].lane_start, 1);
        assert_eq!(scalar.states[1].slots[1].lanes.len(), 2);
    }

    #[test]
    fn scalar_executor_qualification_rejects_float_and_unsupported_channel_payloads() {
        let mut float_module =
            witchy_syntax::parser::parse_module("fn resume(value: Float) -> Nil:\n    ()\n")
                .expect("float fixture parses");
        let Item::Function(float_state) = &mut float_module.items[0] else {
            panic!("float state")
        };
        float_state.attributes.push(FRAME_FUNCTION_ATTRIBUTE.into());
        float_state
            .attributes
            .push(witchy_syntax::suspension::frame_state_attribute(0));
        let float_typed =
            crate::typeck::annotate_checked(float_module).expect("float fixture type checks");
        let float_catalog =
            SuspensionCarrierCatalog::from_typed(&float_typed).expect("float catalog");
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
        channel_state
            .attributes
            .push(FRAME_FUNCTION_ATTRIBUTE.into());
        channel_state
            .attributes
            .push(witchy_syntax::suspension::frame_state_attribute(0));
        let channel_typed =
            crate::typeck::annotate_checked(channel_module).expect("channel fixture type checks");
        let channel_catalog =
            SuspensionCarrierCatalog::from_typed(&channel_typed).expect("channel catalog");
        assert!(channel_catalog.is_wholly_direct());
        assert_eq!(
            channel_catalog.scalar_executor_plan(),
            Err(ScalarExecutorRejection::NonIntegerChannel {
                channel: "tx".into(),
                message: "String".into(),
            }),
        );
        let string_ready = channel_catalog
            .ready_channel_region_plan()
            .expect("a String root has an explicit ready-path envelope");
        assert_eq!(
            string_ready.typed_channels[0].payload_layout,
            ChannelPayloadLayout::BufferRoot(ChannelBufferRoot::String),
        );

        let mut packed_module = witchy_syntax::parser::parse_module(
            "type Sender(a):\n    Sender(Int)\n\ntype Receiver(a):\n    Receiver(Int)\n\ntype Pair:\n    Pair(Int, Int)\n\nfn resume(tx: Sender(Pair), rx: Receiver(Pair)) -> Nil:\n    ()\n",
        )
        .expect("packed endpoint fixture parses");
        let Item::Function(packed_state) = &mut packed_module.items[3] else {
            panic!("packed state")
        };
        packed_state
            .attributes
            .push(FRAME_FUNCTION_ATTRIBUTE.into());
        packed_state
            .attributes
            .push(witchy_syntax::suspension::frame_state_attribute(0));
        let packed_typed =
            crate::typeck::annotate_checked(packed_module).expect("packed fixture type checks");
        let packed_catalog =
            SuspensionCarrierCatalog::from_typed(&packed_typed).expect("packed catalog");
        let packed_plan = packed_catalog
            .scalar_executor_plan()
            .expect("two-integer packed endpoints retain direct lanes");
        assert!(packed_plan.typed_channels.iter().all(|channel| {
            channel.payload_layout == ChannelPayloadLayout::Packed(vec![CarrierLane::I64, CarrierLane::I64])
        }));
        assert!(packed_catalog.ready_channel_region_plan().is_ok());
    }

    #[test]
    fn ready_channel_region_rejects_erased_payloads_and_dynamic_operations() {
        let mut module = witchy_syntax::parser::parse_module(
            "type Sender(a):\n    Sender(Int)\n\nfn state(tx: Sender(List(Int))) -> Nil:\n    ()\n",
        )
        .expect("boxed endpoint fixture parses");
        let Item::Function(state) = &mut module.items[1] else {
            panic!("state")
        };
        state.attributes.push(FRAME_FUNCTION_ATTRIBUTE.into());
        state
            .attributes
            .push(witchy_syntax::suspension::frame_state_attribute(0));
        let typed = crate::typeck::annotate_checked(module).expect("boxed endpoint checks");
        let catalog = SuspensionCarrierCatalog::from_typed(&typed).expect("boxed catalog");
        assert_eq!(
            catalog.ready_channel_region_plan(),
            Err(ReadyChannelRegionRejection::BoxedChannel {
                channel: "tx".into(),
                message: "List(Int)".into(),
            }),
        );

        let dynamic = ScalarTransition::ChannelReceive(AwaitChannelOperationPlan {
            endpoint: ChannelEndpointPlace::Dynamic,
            payload_layout: ChannelPayloadLayout::Scalar(CarrierLane::I64),
            ownership: ChannelOwnershipEnvelope::InlineValue,
            resume_target: 1,
        });
        assert_eq!(ready_transition_is_static(&dynamic), Err("receive"));
    }

    #[test]
    fn typed_channel_plans_preserve_endpoint_direction_and_buffer_root_identity() {
        let mut module = witchy_syntax::parser::parse_module(
            "type Sender(a):\n    Sender(Int)\n\ntype Receiver(a):\n    Receiver(Int)\n\ntype Packet:\n    Packet(Int, Bool)\n\nfn state(tx: Sender(String), rx: Receiver(Bytes), ints: Receiver(Int), packets: Receiver(Packet)) -> Nil:\n    ()\n",
        )
        .expect("channel plan fixture parses");
        let Item::Function(state) = &mut module.items[3] else {
            panic!("state")
        };
        state.attributes.push(FRAME_FUNCTION_ATTRIBUTE.into());
        state
            .attributes
            .push(witchy_syntax::suspension::frame_state_attribute(0));

        let typed = crate::typeck::annotate_checked(module).expect("channel plan fixture checks");
        let catalog = SuspensionCarrierCatalog::from_typed(&typed).expect("carrier catalog");
        assert_eq!(
            catalog.typed_channel_plans(),
            &[
                TypedChannelPlan {
                    state: 0,
                    endpoint_place: "tx".into(),
                    direction: ChannelEndpointDirection::Send,
                    message_type: "String".into(),
                    payload_layout: ChannelPayloadLayout::BufferRoot(ChannelBufferRoot::String),
                    ownership: ChannelOwnershipEnvelope::BufferRoot,
                },
                TypedChannelPlan {
                    state: 0,
                    endpoint_place: "rx".into(),
                    direction: ChannelEndpointDirection::Receive,
                    message_type: "Bytes".into(),
                    payload_layout: ChannelPayloadLayout::BufferRoot(ChannelBufferRoot::Bytes),
                    ownership: ChannelOwnershipEnvelope::BufferRoot,
                },
                TypedChannelPlan {
                    state: 0,
                    endpoint_place: "ints".into(),
                    direction: ChannelEndpointDirection::Receive,
                    message_type: "Int".into(),
                    payload_layout: ChannelPayloadLayout::Scalar(CarrierLane::I64),
                    ownership: ChannelOwnershipEnvelope::InlineValue,
                },
                TypedChannelPlan {
                    state: 0,
                    endpoint_place: "packets".into(),
                    direction: ChannelEndpointDirection::Receive,
                    message_type: "Packet".into(),
                    payload_layout: ChannelPayloadLayout::Packed(vec![
                        CarrierLane::I64,
                        CarrierLane::I64,
                    ]),
                    ownership: ChannelOwnershipEnvelope::InlineValue,
                },
            ],
        );
    }

    #[test]
    fn scalar_transition_plan_unwraps_lazy_and_then_effect_edges() {
        let module = witchy_syntax::parser::parse_module(
            "fn state() -> Nil:\n    task.lazy__Nil(fn(): task.and_then__Int__Nil(chan.send__Int(tx, value), fn(ignored): resume()))\n",
        )
        .expect("transition fixture parses");
        let table = TypeTable::default();
        let Item::Function(function) = &module.items[0] else {
            panic!("state function")
        };
        let state_ids = HashMap::from_iter([("resume".to_string(), 7)]);

        assert_eq!(
            scalar_transitions_for_function(function, &state_ids, &table, &HashMap::new()),
            Ok(vec![ScalarTransition::ChannelSend(AwaitChannelOperationPlan {
                endpoint: ChannelEndpointPlace::Static("tx".into()),
                payload_layout: ChannelPayloadLayout::Boxed,
                ownership: ChannelOwnershipEnvelope::BoxedValue,
                resume_target: 7,
            })]),
        );
    }

    #[test]
    fn one_way_channel_plan_keeps_direction_and_exact_message_layout() {
        let sender = Ty::Named("Sender".into(), vec![Ty::String]);
        let receiver = Ty::Named("Receiver".into(), vec![Ty::Int]);
        assert_eq!(
            channel_endpoint_payload_layout(
                &sender,
                ChannelEndpointDirection::Send,
                &HashMap::new(),
            ),
            Some(ChannelPayloadLayout::BufferRoot(ChannelBufferRoot::String)),
        );
        assert_eq!(
            channel_endpoint_payload_layout(
                &receiver,
                ChannelEndpointDirection::Receive,
                &HashMap::new(),
            ),
            Some(ChannelPayloadLayout::Scalar(CarrierLane::I64)),
        );
        assert_eq!(
            channel_endpoint_payload_layout(
                &sender,
                ChannelEndpointDirection::Receive,
                &HashMap::new(),
            ),
            None,
            "a sender must not be misclassified as a receive-ready endpoint",
        );
    }

    #[test]
    fn select_payload_lane_requires_two_typed_integer_receivers() {
        let receiver_int = Ty::Named("Receiver".into(), vec![Ty::Int]);
        let receiver_string = Ty::Named("Receiver".into(), vec![Ty::String]);
        assert_eq!(
            select_payload_lane_types(Some(&receiver_int), Some(&receiver_int)),
            Some(CarrierLane::I64)
        );
        assert_eq!(
            select_payload_lane_types(Some(&receiver_int), Some(&receiver_string)),
            None
        );
        assert_eq!(select_payload_lane_types(None, Some(&receiver_int)), None);
    }
}
