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
    /// Stable parameter position in the compiler-generated suspension state.
    /// This is a physical carrier reference, not a source-name lookup.
    pub endpoint_slot: usize,
    /// First direct carrier lane for `endpoint_slot`, when the complete state
    /// is direct. A future ready emitter must reject `None` rather than trying
    /// to recover a channel id from the erased scheduler representation.
    pub endpoint_lane: Option<usize>,
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
    /// Stable async-region identity parallel to `states`. Generator states
    /// deliberately have no async region and are excluded from ready-region
    /// formation.
    async_regions: Vec<Option<usize>>,
    max_lane_width: usize,
    typed_channel_plans: Vec<TypedChannelPlan>,
    /// Terminal control-flow paths retained independently for ready-region
    /// formation. The legacy aggregate below deliberately remains
    /// all-or-nothing for its existing scalar-executor consumers.
    region_transition_paths: Vec<Vec<RegionTransitionPath>>,
    scalar_transitions: Vec<Result<Vec<ScalarTransition>, String>>,
}

#[derive(Debug, Clone, PartialEq)]
struct RegionTransitionPath {
    guarded: bool,
    transitions: Result<Vec<ScalarTransition>, String>,
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

/// One independently promotable, suspension-bounded async region.
///
/// Async lowering assigns the identity before source lowering erases the
/// ordinary statements around the terminal task expression.  The region owns
/// only the transition in its state; effects before that transition remain in
/// its entry state and effects after it remain in the successor state.  An
/// emitter therefore has a narrow boundary on which it may decline without
/// moving, dropping, or duplicating either set of effects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadyChannelRegion {
    /// Existing dense carrier-state identity.  Consumers dispatch by this id;
    /// formation never invents a parallel region or runtime identity.
    pub id: usize,
    /// Stable ordinal of this terminal control-flow path within `id`. This is
    /// ordering metadata, not a runtime identity; dispatch remains by `id`.
    pub path_ordinal: usize,
    /// Source/suspension boundary identity attached by async lowering.
    pub lowering_region: usize,
    pub state: ScalarExecutorState,
    /// Exact endpoint facts used by this state, in parameter order.
    pub typed_channels: Vec<TypedChannelPlan>,
    /// Typed channel operations in their existing transition/source order.
    /// A consumer must preserve this order rather than rebuilding it from
    /// names, an erased message, or a runtime receiver list.
    pub operations: Vec<ReadyChannelOperation>,
    /// The state ids to which this region can transfer after its terminal
    /// transition.  They are boundaries, not a license to inline the next
    /// region or its ordinary epilogue.
    pub successor_states: Vec<usize>,
}

/// A rejected region is recorded independently so one unsupported statement
/// cannot make a neighbouring eligible channel region disappear from the
/// carrier catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadyChannelRegionExclusion {
    /// Existing dense carrier-state identity.
    pub id: usize,
    /// Terminal control-flow path matching [`ReadyChannelRegion::path_ordinal`].
    pub path_ordinal: usize,
    /// Source/suspension boundary identity attached by async lowering.
    pub lowering_region: usize,
    pub function: String,
    pub reason: ReadyChannelRegionExclusionReason,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadyChannelRegionExclusionReason {
    /// A terminal branch or opaque task construction could require re-evaluating
    /// an effectful predicate. Keep it on the original state dispatcher until a
    /// later emitter can lower the construction exactly once.
    GuardedControlFlow,
    BoxedState,
    BoxedChannel {
        channel: String,
        message: String,
    },
    DynamicEndpoint {
        operation: &'static str,
    },
    /// A computed channel capacity would have to be re-evaluated before a
    /// typed ring can be allocated. Keep that state on the ordinary task path
    /// until the scheduler-context emitter has an exact evaluated-value lane.
    DynamicChannelCapacity,
    UnsupportedTransition {
        detail: String,
    },
}

/// Region-formation result for RFC-0152 Phase 1.
///
/// This is intentionally separate from [`ReadyChannelRegionPlan`].  The
/// latter is the pre-existing all-or-nothing graph proof used by the narrow
/// scalar executor.  Formation makes the split explicit and is non-enabling:
/// a later ready emitter chooses which individual regions it can execute.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReadyChannelRegionFormation {
    pub regions: Vec<ReadyChannelRegion>,
    pub excluded: Vec<ReadyChannelRegionExclusion>,
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

/// The exact source operand supplied to a send transition. A promoted emitter
/// may consume a named carrier/WIR value or a literal directly, but it must
/// leave a computed expression on the ordinary scheduler rather than evaluate
/// it a second time while probing a ready ring.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelMessageOperand {
    Static(String),
    Int(i64),
    Duration(i64),
    Bool(bool),
    String(String),
    Dynamic,
}

/// Where an awaited receive value is delivered after the direct ring probe.
/// The successor state and its parameter ordinal are stable carrier identity;
/// the parameter spelling is retained only as a codegen diagnostic. Any
/// transformation or escape of the continuation value remains a fallback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelReceiveResult {
    SuccessorArgument {
        state: usize,
        argument: usize,
        parameter: String,
    },
    Discarded,
    Dynamic,
}

/// A reusable select loop either resumes a compiler-owned state or completes
/// the surrounding task after its frame is drained.  Keeping completion in the
/// typed graph lets a ready emitter avoid inventing a synthetic continuation
/// state solely for a terminal loop. The original WIR frame, step closure, and
/// completion-prefix effects remain authoritative; `Done` is not permission to
/// skip them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectResume {
    State(usize),
    Done,
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
    pub resume_target: SelectResume,
    pub result_use: SelectResultUse,
}

/// Compiler-owned ABI for the reusable two-way select frame loop.
///
/// Unlike an ordinary one-shot select, the frame loop has observable work in
/// both closures: `step` updates the captured frame after every selected value
/// and `done` performs the post-loop continuation exactly once.  Their names
/// are compiler-reserved local identities, not user-provided hints; an emitter
/// may promote this transition only when it can invoke all three values in the
/// original order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AwaitSelect2FrameLoopPlan {
    pub first_channel: ChannelEndpointPlace,
    pub second_channel: ChannelEndpointPlace,
    pub frame: String,
    pub step: String,
    pub done: String,
    pub payload_layout: ChannelPayloadLayout,
    pub payload_lane: Option<CarrierLane>,
    pub resume_target: SelectResume,
}

/// Compiler-owned ABI for a reusable three-way select frame loop.
///
/// This is deliberately distinct from [`AwaitFixedSelectPlan`]: the latter
/// describes one awaited selection, while a frame loop must retain the frame
/// and both closures whose work is observable on every ready value and on the
/// single terminal close/cancellation wake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AwaitSelect3FrameLoopPlan {
    pub endpoints: Vec<ChannelEndpointPlace>,
    pub frame: String,
    pub step: String,
    pub done: String,
    pub payload_layout: ChannelPayloadLayout,
    pub payload_lane: Option<CarrierLane>,
    pub resume_target: SelectResume,
}

/// Compiler-owned ABI for a reusable one-way receive frame loop.  The typed
/// payload facts travel with the erased scheduler fallback so a direct emitter
/// can preserve the existing scalar-channel fusion instead of rediscovering a
/// representation from `__Msg`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AwaitReceiveFrameLoopPlan {
    pub channel: ChannelEndpointPlace,
    pub frame: String,
    pub step: String,
    pub done: String,
    pub payload_layout: ChannelPayloadLayout,
    pub payload_lane: Option<CarrierLane>,
    pub resume_target: SelectResume,
}
/// Four-way frame-loop facts have the same typed shape as the three-way form;
/// arity is fixed by the transition variant and the endpoint vector is checked
/// at construction. Keeping the alias prevents a second, structurally-identical
/// carrier ABI while preserving an explicit codegen-facing four-way variant.
pub type AwaitSelect4FrameLoopPlan = AwaitSelect3FrameLoopPlan;

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
    /// `None` for receive. Send retains the source operand independently of
    /// the endpoint so a direct emitter never has to recover it from erased
    /// `__Msg` storage or re-walk the source expression.
    pub message: Option<ChannelMessageOperand>,
    /// `None` for send. A receive result is promotable only when its
    /// continuation passes the parameter straight into one successor slot.
    pub result: Option<ChannelReceiveResult>,
    pub payload_layout: ChannelPayloadLayout,
    pub ownership: ChannelOwnershipEnvelope,
    pub resume_target: usize,
}

/// One compiler-owned channel boundary in deterministic state order.
///
/// This is the codegen-facing form of a [`ReadyChannelRegionPlan`].  It is
/// deliberately separate from the state graph: an emitter can build direct
/// ready probes from these typed facts while retaining the original graph for
/// ordinary scheduler fallback and for all non-channel transitions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadyChannelOperation {
    pub state: usize,
    /// Position in the state transition list before non-channel transitions
    /// were filtered. This lets a scheduler-context emitter return to the
    /// exact original terminal/fallback boundary without name-based recovery.
    pub transition_ordinal: usize,
    pub kind: ReadyChannelOperationKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadyChannelOperationKind {
    /// The open transition owns the layout selected for the new runtime
    /// channel id. Later states may rename or destructure the endpoint pair,
    /// so an emitter must use this boundary rather than infer the payload from
    /// a later local spelling.
    Open {
        resume_target: usize,
        capacity: ChannelMessageOperand,
        payload_layout: ChannelPayloadLayout,
        ownership: ChannelOwnershipEnvelope,
    },
    Send(AwaitChannelOperationPlan),
    Receive(AwaitChannelOperationPlan),
    ReceiveFrameLoop(AwaitReceiveFrameLoopPlan),
    Select2(AwaitSelect2Plan),
    Select2FrameLoop(AwaitSelect2FrameLoopPlan),
    Select3FrameLoop(AwaitSelect3FrameLoopPlan),
    Select4FrameLoop(AwaitSelect4FrameLoopPlan),
    Select3(AwaitFixedSelectPlan),
    Select4(AwaitFixedSelectPlan),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScalarTransition {
    Jump { target: usize },
    Done,
    /// A new channel's exact representation is attached to the allocation
    /// edge, before the `(Sender(m), Receiver(m))` result is destructured in
    /// the successor state.
    ChannelOpen {
        resume: usize,
        capacity: ChannelMessageOperand,
        payload_layout: ChannelPayloadLayout,
        ownership: ChannelOwnershipEnvelope,
    },
    ChannelSend(AwaitChannelOperationPlan),
    ChannelReceive(AwaitChannelOperationPlan),
    ChannelReceiveFrameLoop(AwaitReceiveFrameLoopPlan),
    ChannelSelect2(AwaitSelect2Plan),
    ChannelSelect2FrameLoop(AwaitSelect2FrameLoopPlan),
    ChannelSelect3FrameLoop(AwaitSelect3FrameLoopPlan),
    ChannelSelect4FrameLoop(AwaitSelect4FrameLoopPlan),
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
        let mut async_regions = Vec::new();

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
            let async_region = witchy_syntax::suspension::async_region(function);
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
            async_regions.push(async_region);
        }

        let max_lane_width = states
            .iter()
            .map(CarrierState::lane_width)
            .max()
            .unwrap_or(0);
        let definitions_ref = &definitions;
        let typed_channel_plans = states
            .iter()
            .flat_map(|state| {
                let state_is_direct = state.direct && state.slots.iter().all(|slot| slot.lanes.is_some());
                let mut next_lane = 0usize;
                state.slots.iter().enumerate().filter_map(move |(endpoint_slot, slot)| {
                    let endpoint_lane = state_is_direct.then_some(next_lane);
                    next_lane += slot.lanes.as_ref().map_or(0, Vec::len);
                    let (direction, message) = channel_endpoint_type(&slot.ty)?;
                    let payload_layout = channel_payload_layout(message, definitions_ref);
                    Some(TypedChannelPlan {
                        state: state.id,
                        endpoint_slot,
                        endpoint_lane,
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
        let region_transition_paths = states
            .iter()
            .map(|state| {
                let function = typed.module().items.iter().find_map(|item| match item {
                    Item::Function(function) if function.name == state.function => Some(function),
                    _ => None,
                });
                let Some(function) = function else {
                    return vec![RegionTransitionPath {
                        guarded: false,
                        transitions: Err(format!(
                            "carrier state `{}` has no typed function body",
                            state.function
                        )),
                    }];
                };
                scalar_transition_paths_for_function(function, &state_ids, typed.table(), &definitions)
            })
            .collect::<Vec<_>>();
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
                scalar_transitions_for_function(function, &state_ids, typed.table(), &definitions)
            })
            .collect();
        Ok(Self {
            states,
            async_regions,
            max_lane_width,
            typed_channel_plans,
            region_transition_paths,
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

    /// Form independently promotable async regions without changing the
    /// existing all-or-nothing ready-plan ABI.
    ///
    /// Region ids and typed-operation order are inherited verbatim from async
    /// lowering and the carrier graph.  A failed qualification is local to one
    /// suspension-bounded region: adjacent regions continue to be reported,
    /// and their ordinary prologue/epilogue states stay in the original graph.
    /// This is formation metadata only; it does not select a runtime path.
    pub fn ready_channel_regions(&self) -> ReadyChannelRegionFormation {
        let mut formation = ReadyChannelRegionFormation::default();

        for (state_index, state) in self.states.iter().enumerate() {
            let Some(lowering_region) = self.async_regions.get(state_index).copied().flatten() else {
                continue;
            };
            let rejection = |path_ordinal, reason| ReadyChannelRegionExclusion {
                id: state.id,
                path_ordinal,
                lowering_region,
                function: state.function.clone(),
                reason,
            };
            if !state.is_direct() {
                formation.excluded.push(rejection(0, ReadyChannelRegionExclusionReason::BoxedState));
                continue;
            }
            let typed_channels = self
                .typed_channel_plans
                .iter()
                .filter(|channel| channel.state == state.id)
                .cloned()
                .collect::<Vec<_>>();
            for (path_ordinal, path) in self.region_transition_paths[state_index].iter().enumerate() {
                if path.guarded {
                    formation.excluded.push(rejection(
                        path_ordinal,
                        ReadyChannelRegionExclusionReason::GuardedControlFlow,
                    ));
                    continue;
                }
                let transitions = match &path.transitions {
                    Ok(transitions) => transitions,
                    Err(detail) => {
                        let reason = if detail.starts_with("unsupported awaited task expression `Var(") {
                            ReadyChannelRegionExclusionReason::GuardedControlFlow
                        } else {
                            ReadyChannelRegionExclusionReason::UnsupportedTransition {
                                detail: detail.clone(),
                            }
                        };
                        formation.excluded.push(rejection(
                            path_ordinal,
                            reason,
                        ));
                        continue;
                    }
                };
                if let Some(channel) = typed_channels.iter().find(|channel| {
                    matches!(channel.payload_layout, ChannelPayloadLayout::Boxed)
                }) {
                    formation.excluded.push(rejection(
                        path_ordinal,
                        ReadyChannelRegionExclusionReason::BoxedChannel {
                            channel: channel.endpoint_place.clone(),
                            message: channel.message_type.clone(),
                        },
                    ));
                    continue;
                }
                if transitions.iter().any(|transition| matches!(
                    transition,
                    ScalarTransition::ChannelOpen { payload_layout: ChannelPayloadLayout::Boxed, .. }
                )) {
                    formation.excluded.push(rejection(
                        path_ordinal,
                        ReadyChannelRegionExclusionReason::BoxedChannel {
                            channel: "chan.channel result".into(),
                            message: "unknown".into(),
                        },
                    ));
                    continue;
                }
                if let Some(operation) = transitions.iter().find_map(|transition| {
                    ready_transition_is_static(transition).err()
                }) {
                    let reason = if operation == "channel capacity" {
                        ReadyChannelRegionExclusionReason::DynamicChannelCapacity
                    } else {
                        ReadyChannelRegionExclusionReason::DynamicEndpoint { operation }
                    };
                    formation.excluded.push(rejection(
                        path_ordinal,
                        reason,
                    ));
                    continue;
                }
                if !transitions.iter().any(is_ready_channel_transition) {
                    // A non-channel state is an ordinary boundary, not a failed
                    // channel region.  Omitting it lets a later emitter preserve
                    // its effects exactly instead of attempting to consume it.
                    continue;
                }
                formation.regions.push(ReadyChannelRegion {
                    id: state.id,
                    path_ordinal,
                    lowering_region,
                    state: planned_state(state, transitions),
                    typed_channels: typed_channels.clone(),
                    operations: ready_channel_operations(state.id, transitions),
                    successor_states: transition_successors(transitions),
                });
            }
        }
        formation.regions.sort_by_key(|region| (region.id, region.path_ordinal));
        formation.excluded.sort_by_key(|region| (region.id, region.path_ordinal));
        formation
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

impl ReadyChannelRegionPlan {
    /// Flatten only the typed channel boundaries in stable state/transition
    /// order.  `Jump`, `Spawn`, `Join`, and ordinary control remain in the
    /// state graph; they are not silently reinterpreted as ready operations.
    pub fn operations(&self) -> Vec<ReadyChannelOperation> {
        self.states
            .iter()
            .flat_map(|state| ready_channel_operations(state.id, &state.transitions))
            .collect()
    }
}

fn ready_channel_operations(
    state: usize,
    transitions: &[ScalarTransition],
) -> Vec<ReadyChannelOperation> {
    transitions
        .iter()
        .enumerate()
        .filter_map(|(transition_ordinal, transition)| {
            let kind = match transition {
                ScalarTransition::ChannelOpen { resume, capacity, payload_layout, ownership } => {
                    ReadyChannelOperationKind::Open {
                        resume_target: *resume,
                        capacity: capacity.clone(),
                        payload_layout: payload_layout.clone(),
                        ownership: *ownership,
                    }
                }
                ScalarTransition::ChannelSend(operation) => {
                    ReadyChannelOperationKind::Send(operation.clone())
                }
                ScalarTransition::ChannelReceive(operation) => {
                    ReadyChannelOperationKind::Receive(operation.clone())
                }
                ScalarTransition::ChannelReceiveFrameLoop(receive) => {
                    ReadyChannelOperationKind::ReceiveFrameLoop(receive.clone())
                }
                ScalarTransition::ChannelSelect2(select) => {
                    ReadyChannelOperationKind::Select2(select.clone())
                }
                ScalarTransition::ChannelSelect2FrameLoop(select) => {
                    ReadyChannelOperationKind::Select2FrameLoop(select.clone())
                }
                ScalarTransition::ChannelSelect3FrameLoop(select) => {
                    ReadyChannelOperationKind::Select3FrameLoop(select.clone())
                }
                ScalarTransition::ChannelSelect4FrameLoop(select) => {
                    ReadyChannelOperationKind::Select4FrameLoop(select.clone())
                }
                ScalarTransition::ChannelSelect3(select) => {
                    ReadyChannelOperationKind::Select3(select.clone())
                }
                ScalarTransition::ChannelSelect4(select) => {
                    ReadyChannelOperationKind::Select4(select.clone())
                }
                _ => return None,
            };
            Some(ReadyChannelOperation {
                state,
                transition_ordinal,
                kind,
            })
        })
        .collect()
}

fn planned_state(state: &CarrierState, transitions: &[ScalarTransition]) -> ScalarExecutorState {
    let mut lane_start = 0;
    let slots = state
        .slots
        .iter()
        .map(|slot| {
            let lanes = slot
                .lanes
                .clone()
                .expect("ready-region formation rejected boxed frame slots");
            let planned = ScalarExecutorSlot {
                name: slot.name.clone(),
                lane_start,
                lanes,
            };
            lane_start += planned.lanes.len();
            planned
        })
        .collect();
    ScalarExecutorState {
        id: state.id,
        function: state.function.clone(),
        source_callable: state.source_callable.clone(),
        slots,
        transitions: transitions.to_vec(),
    }
}

fn is_ready_channel_transition(transition: &ScalarTransition) -> bool {
    matches!(
        transition,
        ScalarTransition::ChannelOpen { .. }
            | ScalarTransition::ChannelSend(_)
            | ScalarTransition::ChannelReceive(_)
            | ScalarTransition::ChannelReceiveFrameLoop(_)
            | ScalarTransition::ChannelSelect2(_)
            | ScalarTransition::ChannelSelect2FrameLoop(_)
            | ScalarTransition::ChannelSelect3(_)
            | ScalarTransition::ChannelSelect3FrameLoop(_)
            | ScalarTransition::ChannelSelect4(_)
            | ScalarTransition::ChannelSelect4FrameLoop(_)
    )
}

fn transition_successors(transitions: &[ScalarTransition]) -> Vec<usize> {
    let mut successors = transitions
        .iter()
        .flat_map(|transition| match transition {
            ScalarTransition::Jump { target } => vec![*target],
            ScalarTransition::ChannelOpen { resume, .. }
            | ScalarTransition::Join { resume }
            | ScalarTransition::Yield { resume }
                => vec![*resume],
            ScalarTransition::Spawn { child, resume } => vec![*child, *resume],
            ScalarTransition::Call { callee, resume } => vec![*callee, *resume],
            ScalarTransition::ChannelSend(operation)
            | ScalarTransition::ChannelReceive(operation) => vec![operation.resume_target],
            ScalarTransition::ChannelSelect3(select)
            | ScalarTransition::ChannelSelect4(select) => vec![select.resume_target],
            ScalarTransition::ChannelSelect2(select) => {
                select_resume_target(select.resume_target).into_iter().collect()
            }
            ScalarTransition::ChannelReceiveFrameLoop(receive) => {
                select_resume_target(receive.resume_target).into_iter().collect()
            }
            ScalarTransition::ChannelSelect2FrameLoop(select) => {
                select_resume_target(select.resume_target).into_iter().collect()
            }
            ScalarTransition::ChannelSelect3FrameLoop(select)
            | ScalarTransition::ChannelSelect4FrameLoop(select) => {
                select_resume_target(select.resume_target).into_iter().collect()
            }
            ScalarTransition::Done => Vec::new(),
        })
        .collect::<Vec<_>>();
    successors.sort_unstable();
    successors.dedup();
    successors
}

fn select_resume_target(resume: SelectResume) -> Option<usize> {
    match resume {
        SelectResume::State(state) => Some(state),
        SelectResume::Done => None,
    }
}

fn ready_transition_is_static(transition: &ScalarTransition) -> Result<(), &'static str> {
    let is_static = |endpoint: &ChannelEndpointPlace| {
        matches!(endpoint, ChannelEndpointPlace::Static(_))
    };
    match transition {
        ScalarTransition::ChannelOpen { capacity: ChannelMessageOperand::Dynamic, .. } => {
            Err("channel capacity")
        }
        ScalarTransition::ChannelSend(operation) if !is_static(&operation.endpoint) => Err("send"),
        ScalarTransition::ChannelSend(operation)
            if matches!(operation.message.as_ref(), None | Some(ChannelMessageOperand::Dynamic)) =>
        {
            Err("send payload")
        }
        ScalarTransition::ChannelReceive(operation) if !is_static(&operation.endpoint) => {
            Err("receive")
        }
        ScalarTransition::ChannelReceive(operation)
            if matches!(operation.result.as_ref(), None | Some(ChannelReceiveResult::Dynamic)) =>
        {
            Err("receive result")
        }
        ScalarTransition::ChannelReceiveFrameLoop(receive) if !is_static(&receive.channel) => {
            Err("receive")
        }
        ScalarTransition::ChannelSelect2(select)
            if !is_static(&select.first_channel) || !is_static(&select.second_channel) =>
        {
            Err("select")
        }
        ScalarTransition::ChannelSelect2FrameLoop(select)
            if !is_static(&select.first_channel) || !is_static(&select.second_channel) =>
        {
            Err("select")
        }
        ScalarTransition::ChannelSelect3FrameLoop(select)
            if select.endpoints.iter().any(|endpoint| !is_static(endpoint)) =>
        {
            Err("select")
        }
        ScalarTransition::ChannelSelect4FrameLoop(select)
            if select.endpoints.iter().any(|endpoint| !is_static(endpoint)) =>
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
    let mut transitions = Vec::new();
    let bindings = HashMap::new();
    collect_scalar_transitions_in_block(
        &function.body,
        state_ids,
        table,
        definitions,
        &bindings,
        &mut transitions,
    )?;
    if transitions.is_empty() {
        return Err("state body has no terminal transition".into());
    }
    Ok(transitions)
}

/// Collect terminal paths independently for formation. The legacy
/// scalar-executor proof remains on its original collector, so conservative
/// formation metadata cannot change existing ready-plan eligibility.
fn scalar_transition_paths_for_function(
    function: &Function,
    state_ids: &HashMap<String, usize>,
    table: &TypeTable,
    definitions: &HashMap<String, &TypeDef>,
) -> Vec<RegionTransitionPath> {
    let bindings = HashMap::new();
    scalar_transition_paths_in_block(
        &function.body,
        state_ids,
        table,
        definitions,
        &bindings,
        false,
    )
}

fn scalar_transition_paths_in_block<'a, 'd>(
    block: &'a ast::Block,
    state_ids: &HashMap<String, usize>,
    table: &TypeTable,
    definitions: &HashMap<String, &'d TypeDef>,
    inherited: &HashMap<String, &'a ast::Expr>,
    guarded: bool,
) -> Vec<RegionTransitionPath> {
    let mut bindings = inherited.clone();
    for statement in &block.stmts {
        if let ast::Stmt::Let { name, value, .. } = statement {
            // Region formation needs the terminal task's construction shape as
            // well as compiler-generated closures: `let work = if ...; work`
            // must retain its conditional fallback instead of hiding it behind
            // a source local. This is formation-only; the legacy aggregate
            // collector below remains intentionally conservative.
            bindings.insert(name.clone(), value);
        }
    }
    let Some(terminal) = terminal_block_expr(block) else {
        return vec![RegionTransitionPath {
            guarded,
            transitions: Err("state body has no terminal expression".into()),
        }];
    };
    scalar_transition_paths(terminal, state_ids, table, definitions, &bindings, guarded)
}

fn scalar_transition_paths<'a, 'd>(
    expression: &'a ast::Expr,
    state_ids: &HashMap<String, usize>,
    table: &TypeTable,
    definitions: &HashMap<String, &'d TypeDef>,
    bindings: &HashMap<String, &'a ast::Expr>,
    guarded: bool,
) -> Vec<RegionTransitionPath> {
    match expression {
        ast::Expr::Call { name, args }
            if (call_family(name, "task.run") && args.len() == 1)
                || (call_family(name, "task.and_then") && args.len() == 2) =>
        {
            if let Some(paths) = guarded_task_construction_paths(&args[0], bindings) {
                return paths;
            }
            let mut transitions = Vec::new();
            vec![RegionTransitionPath {
                guarded,
                transitions: collect_scalar_transitions(
                    expression,
                    state_ids,
                    table,
                    definitions,
                    bindings,
                    &mut transitions,
                )
                .map(|()| transitions),
            }]
        }
        ast::Expr::Var(name) if bindings.contains_key(name) => scalar_transition_paths(
            bindings[name],
            state_ids,
            table,
            definitions,
            bindings,
            guarded,
        ),
        ast::Expr::Block(block) => {
            scalar_transition_paths_in_block(block, state_ids, table, definitions, bindings, guarded)
        }
        ast::Expr::If {
            then_block,
            else_block: Some(else_block),
            ..
        } => {
            let mut paths = scalar_transition_paths_in_block(
                then_block,
                state_ids,
                table,
                definitions,
                bindings,
                true,
            );
            paths.extend(scalar_transition_paths_in_block(
                else_block,
                state_ids,
                table,
                definitions,
                bindings,
                true,
            ));
            paths
        }
        ast::Expr::Match { arms, .. } if !arms.is_empty() => {
            arms.iter().flat_map(|arm| {
                scalar_transition_paths(
                    &arm.body,
                    state_ids,
                    table,
                    definitions,
                    bindings,
                    true,
                )
            }).collect()
        }
        _ => {
            let mut transitions = Vec::new();
            vec![RegionTransitionPath {
                guarded,
                transitions: collect_scalar_transitions(
                expression,
                state_ids,
                table,
                definitions,
                bindings,
                &mut transitions,
            )
                .map(|()| transitions),
            }]
        }
    }
}

/// Detect a conditional task construction nested below the terminal scheduler
/// wrapper. The current scalar transition ABI cannot re-emit that predicate,
/// so every such path is deliberately kept on the original dispatcher.
fn guarded_task_construction_paths<'a>(
    expression: &'a ast::Expr,
    inherited: &HashMap<String, &'a ast::Expr>,
) -> Option<Vec<RegionTransitionPath>> {
    match expression {
        ast::Expr::Var(name) => inherited
            .get(name)
            .and_then(|bound| guarded_task_construction_paths(bound, inherited)),
        ast::Expr::Block(block) => {
            let mut bindings = inherited.clone();
            for statement in &block.stmts {
                if let ast::Stmt::Let { name, value, .. } = statement {
                    bindings.insert(name.clone(), value);
                }
            }
            terminal_block_expr(block)
                .and_then(|terminal| guarded_task_construction_paths(terminal, &bindings))
        }
        ast::Expr::If { .. } => Some(vec![
            RegionTransitionPath {
                guarded: true,
                transitions: Err("guarded terminal path".into()),
            },
            RegionTransitionPath {
                guarded: true,
                transitions: Err("guarded terminal path".into()),
            },
        ]),
        ast::Expr::Match { arms, .. } if !arms.is_empty() => Some(
            arms.iter().map(|_| RegionTransitionPath {
                guarded: true,
                transitions: Err("guarded terminal path".into()),
            }).collect(),
        ),
        _ => None,
    }
}

/// Follow the reserved compiler closure bindings in a generated state. The
/// async lowerer intentionally builds a reusable select frame as a block of
/// `let __await... = fn` bindings followed by one task expression, so looking
/// only at that expression loses the frame's `done` continuation. This is not
/// a source-level name resolver: source identifiers cannot use the reserved
/// `__await` prefix, and aliases deliberately retain scheduler fallback.
fn collect_scalar_transitions_in_block<'a, 'd>(
    block: &'a ast::Block,
    state_ids: &HashMap<String, usize>,
    table: &TypeTable,
    definitions: &HashMap<String, &'d TypeDef>,
    inherited: &HashMap<String, &'a ast::Expr>,
    transitions: &mut Vec<ScalarTransition>,
) -> Result<(), String> {
    let mut bindings = inherited.clone();
    for statement in &block.stmts {
        if let ast::Stmt::Let {
            name,
            value: value @ ast::Expr::Lambda { .. },
            ..
        } = statement
            && is_generated_closure_binding(name)
        {
            bindings.insert(name.clone(), value);
        }
    }
    let terminal = terminal_block_expr(block)
        .ok_or_else(|| "state body has no terminal expression".to_string())?;
    collect_scalar_transitions(
        terminal,
        state_ids,
        table,
        definitions,
        &bindings,
        transitions,
    )
}

fn terminal_block_expr(block: &ast::Block) -> Option<&ast::Expr> {
    match block.stmts.last()? {
        ast::Stmt::Expr(expression) => Some(expression),
        ast::Stmt::Return(Some(expression)) => Some(expression),
        ast::Stmt::Return(None) => None,
        _ => None,
    }
}

fn collect_scalar_transitions<'a, 'd>(
    expression: &'a ast::Expr,
    state_ids: &HashMap<String, usize>,
    table: &TypeTable,
    definitions: &HashMap<String, &'d TypeDef>,
    bindings: &HashMap<String, &'a ast::Expr>,
    transitions: &mut Vec<ScalarTransition>,
) -> Result<(), String> {
    match expression {
        ast::Expr::Block(block) => collect_scalar_transitions_in_block(
            block, state_ids, table, definitions, bindings, transitions,
        ),
        ast::Expr::If {
            then_block,
            else_block: Some(else_block),
            ..
        } => {
            collect_scalar_transitions_in_block(
                then_block, state_ids, table, definitions, bindings, transitions,
            )?;
            collect_scalar_transitions_in_block(
                else_block, state_ids, table, definitions, bindings, transitions,
            )
        }
        ast::Expr::If {
            else_block: None, ..
        } => Err("suspension-state `if` is missing an else transition".into()),
        ast::Expr::Match { arms, .. } if !arms.is_empty() => {
            for arm in arms {
                collect_scalar_transitions(&arm.body, state_ids, table, definitions, bindings, transitions)?;
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
            collect_task_expression(&args[0], None, state_ids, table, definitions, bindings, transitions)
        }
        ast::Expr::Call { name, args } if call_family(name, "task.lazy") && args.len() == 1 => {
            let ast::Expr::Lambda { body, .. } = &args[0] else {
                return Err("`task.lazy` state entry does not contain a lambda body".into());
            };
            collect_scalar_transitions_in_block(body, state_ids, table, definitions, bindings, transitions)
        }
        ast::Expr::Call { name, args } if call_family(name, "task.and_then") && args.len() == 2 => {
            let resume = continuation_state(&args[1], state_ids, bindings)?;
            collect_task_expression(&args[0], Some(resume), state_ids, table, definitions, bindings, transitions)?;
            attach_receive_result(transitions, &args[1], state_ids, bindings)
        }
        ast::Expr::Call { name, .. } if call_family(name, "chan.__select2_map") => {
            collect_task_expression(expression, None, state_ids, table, definitions, bindings, transitions)
        }
        ast::Expr::Call { name, .. }
            if call_family(name, "chan.select2_frame_loop")
                || call_family(name, "chan.select3_frame_loop")
                || call_family(name, "chan.select4_frame_loop") =>
        {
            collect_task_expression(expression, None, state_ids, table, definitions, bindings, transitions)
        }
        ast::Expr::Call { name, .. } if call_family(name, "chan.recv_frame_loop") => {
            collect_task_expression(expression, None, state_ids, table, definitions, bindings, transitions)
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

fn collect_task_expression<'a, 'd>(
    task: &'a ast::Expr,
    resume: Option<usize>,
    state_ids: &HashMap<String, usize>,
    table: &TypeTable,
    definitions: &HashMap<String, &'d TypeDef>,
    bindings: &HashMap<String, &'a ast::Expr>,
    transitions: &mut Vec<ScalarTransition>,
) -> Result<(), String> {
    // An async entry block may execute a direct effectful prologue before it
    // constructs its first task. The carrier graph starts at that terminal
    // task; lowering preserves the WIR prologue before replacing only the
    // proved closed region. Do not require the prologue itself to be pure:
    // its once-only source-order execution is observable.
    if let ast::Expr::Block(block) = task {
        return collect_task_block(
            block, resume, state_ids, table, definitions, bindings, transitions,
        );
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
        return collect_task_block(
            body, resume, state_ids, table, definitions, bindings, transitions,
        );
    }
    if call_family(name, "task.and_then") && args.len() == 2 {
        let continuation = continuation_state(&args[1], state_ids, bindings)?;
        collect_task_expression(&args[0], Some(continuation), state_ids, table, definitions, bindings, transitions)?;
        return attach_receive_result(transitions, &args[1], state_ids, bindings);
    }
    if call_family(name, "chan.__select2_map") {
        let resume_target = SelectResume::State(continuation_state(
            args.last().ok_or_else(|| "fused select has no continuation".to_string())?,
            state_ids,
            bindings,
        )?);
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
            bindings,
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
            bindings,
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
    if call_family(name, "chan.select2_frame_loop") && args.len() == 5 {
        transitions.push(ScalarTransition::ChannelSelect2FrameLoop(select2_frame_loop_plan(
            table,
            args,
            definitions,
            state_ids,
            bindings,
        )?));
        return Ok(());
    }
    if call_family(name, "chan.select3_frame_loop") && args.len() == 6 {
        transitions.push(ScalarTransition::ChannelSelect3FrameLoop(select3_frame_loop_plan(
            table,
            args,
            definitions,
            state_ids,
            bindings,
        )?));
        return Ok(());
    }
    if call_family(name, "chan.recv_frame_loop") && args.len() == 4 {
        transitions.push(ScalarTransition::ChannelReceiveFrameLoop(recv_frame_loop_plan(
            table,
            args,
            definitions,
            state_ids,
            bindings,
        )?));
        return Ok(());
    }
    if call_family(name, "chan.select4_frame_loop") && args.len() == 7 {
        transitions.push(ScalarTransition::ChannelSelect4FrameLoop(select4_frame_loop_plan(
            table,
            args,
            definitions,
            state_ids,
            bindings,
        )?));
        return Ok(());
    }
    let resume =
        resume.ok_or_else(|| format!("effect task `{name}` has no compiler-owned resume state"))?;
    let transition = if call_family(name, "chan.channel") {
        channel_open_plan(table, task, args.first(), definitions, resume)
    } else if call_family(name, "chan.send") {
        ScalarTransition::ChannelSend(channel_operation_plan(
            table,
            args.first(),
            args.get(1),
            ChannelEndpointDirection::Send,
            definitions,
            resume,
        ))
    } else if call_family(name, "chan.recv") {
        ScalarTransition::ChannelReceive(channel_operation_plan(
            table,
            args.first(),
            None,
            ChannelEndpointDirection::Receive,
            definitions,
            resume,
        ))
    } else if call_family(name, "task.__channel_select2") {
        ScalarTransition::ChannelSelect2(AwaitSelect2Plan {
            first_channel: channel_endpoint_place(args.first()),
            second_channel: channel_endpoint_place(args.get(1)),
            payload_layout: select_payload_layout(table, args.first(), args.get(1), definitions),
            payload_lane: select_payload_lane(table, args.first(), args.get(1)),
            resume_target: SelectResume::State(resume),
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

fn collect_task_block<'a, 'd>(
    block: &'a ast::Block,
    resume: Option<usize>,
    state_ids: &HashMap<String, usize>,
    table: &TypeTable,
    definitions: &HashMap<String, &'d TypeDef>,
    inherited: &HashMap<String, &'a ast::Expr>,
    transitions: &mut Vec<ScalarTransition>,
) -> Result<(), String> {
    let mut bindings = inherited.clone();
    for statement in &block.stmts {
        if let ast::Stmt::Let {
            name,
            value: value @ ast::Expr::Lambda { .. },
            ..
        } = statement
            && is_generated_closure_binding(name)
        {
            bindings.insert(name.clone(), value);
        }
    }
    let terminal = terminal_block_expr(block)
        .ok_or_else(|| "awaited task block has no terminal task".to_string())?;
    collect_task_expression(
        terminal,
        resume,
        state_ids,
        table,
        definitions,
        &bindings,
        transitions,
    )
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
    message: Option<&ast::Expr>,
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
        message: message.map(channel_message_operand),
        result: matches!(direction, ChannelEndpointDirection::Receive)
            .then_some(ChannelReceiveResult::Dynamic),
        payload_layout,
        ownership,
        resume_target,
    }
}

fn attach_receive_result(
    transitions: &mut [ScalarTransition],
    continuation: &ast::Expr,
    state_ids: &HashMap<String, usize>,
    bindings: &HashMap<String, &ast::Expr>,
) -> Result<(), String> {
    let Some(ScalarTransition::ChannelReceive(operation)) = transitions.last_mut() else {
        return Ok(());
    };
    operation.result = Some(continuation_receive_result(continuation, state_ids, bindings)?);
    Ok(())
}

fn continuation_receive_result(
    continuation: &ast::Expr,
    state_ids: &HashMap<String, usize>,
    bindings: &HashMap<String, &ast::Expr>,
) -> Result<ChannelReceiveResult, String> {
    let continuation = resolve_binding(continuation, bindings);
    let ast::Expr::Lambda { params, body, .. } = continuation else {
        return Ok(ChannelReceiveResult::Dynamic);
    };
    let Some(parameter) = params.first().map(|parameter| parameter.name.clone()) else {
        return Ok(ChannelReceiveResult::Discarded);
    };
    let Some((terminal, body_bindings)) = terminal_with_bindings(body, bindings) else {
        return Ok(ChannelReceiveResult::Dynamic);
    };
    let terminal = resolve_binding(terminal, &body_bindings);
    let ast::Expr::Call { name, args } = terminal else {
        return Ok(ChannelReceiveResult::Dynamic);
    };
    let Some(&state) = state_ids.get(name) else {
        return Ok(ChannelReceiveResult::Dynamic);
    };
    let arguments = args
        .iter()
        .enumerate()
        .filter_map(|(index, argument)| matches!(argument, ast::Expr::Var(name) if name == &parameter).then_some(index))
        .collect::<Vec<_>>();
    match arguments.as_slice() {
        [] => Ok(ChannelReceiveResult::Discarded),
        [argument] => Ok(ChannelReceiveResult::SuccessorArgument {
            state,
            argument: *argument,
            parameter,
        }),
        _ => Ok(ChannelReceiveResult::Dynamic),
    }
}

fn channel_message_operand(message: &ast::Expr) -> ChannelMessageOperand {
    match message {
        ast::Expr::Var(name) => ChannelMessageOperand::Static(name.clone()),
        ast::Expr::Int(value) => ChannelMessageOperand::Int(*value),
        ast::Expr::Duration(value) => ChannelMessageOperand::Duration(*value),
        ast::Expr::Bool(value) => ChannelMessageOperand::Bool(*value),
        ast::Expr::Str(value) => ChannelMessageOperand::String(value.clone()),
        _ => ChannelMessageOperand::Dynamic,
    }
}

fn select2_frame_loop_plan(
    table: &TypeTable,
    args: &[ast::Expr],
    definitions: &HashMap<String, &TypeDef>,
    state_ids: &HashMap<String, usize>,
    bindings: &HashMap<String, &ast::Expr>,
) -> Result<AwaitSelect2FrameLoopPlan, String> {
    let frame = compiler_generated_place(
        args.get(2),
        "select frame",
        bindings,
        false,
    )?;
    let step = compiler_generated_place(
        args.get(3),
        "select step closure",
        bindings,
        true,
    )?;
    let done = compiler_generated_place(
        args.get(4),
        "select completion closure",
        bindings,
        true,
    )?;
    Ok(AwaitSelect2FrameLoopPlan {
        first_channel: channel_endpoint_place(args.first()),
        second_channel: channel_endpoint_place(args.get(1)),
        frame,
        step,
        done: done.clone(),
        payload_layout: select_payload_layout(table, args.first(), args.get(1), definitions),
        payload_lane: select_payload_lane(table, args.first(), args.get(1)),
        resume_target: select_loop_resume(
            args.get(4).expect("five-argument frame loop has completion closure"),
            state_ids,
            bindings,
        )?,
    })
}

fn select3_frame_loop_plan(
    table: &TypeTable,
    args: &[ast::Expr],
    definitions: &HashMap<String, &TypeDef>,
    state_ids: &HashMap<String, usize>,
    bindings: &HashMap<String, &ast::Expr>,
) -> Result<AwaitSelect3FrameLoopPlan, String> {
    let frame = compiler_generated_place(args.get(3), "select frame", bindings, false)?;
    let step = compiler_generated_place(args.get(4), "select step closure", bindings, true)?;
    let done = compiler_generated_place(args.get(5), "select completion closure", bindings, true)?;
    let endpoint_types = args.iter().take(3).map(|endpoint| table.type_of(endpoint)).collect::<Vec<_>>();
    Ok(AwaitSelect3FrameLoopPlan {
        endpoints: args.iter().take(3).map(|endpoint| channel_endpoint_place(Some(endpoint))).collect(),
        frame,
        step,
        done: done.clone(),
        payload_layout: fixed_select_payload_layout(&endpoint_types, definitions),
        payload_lane: fixed_select_payload_lane(&endpoint_types),
        resume_target: select_loop_resume(
            args.get(5).expect("six-argument frame loop has completion closure"),
            state_ids,
            bindings,
        )?,
    })
}

fn recv_frame_loop_plan(
    table: &TypeTable,
    args: &[ast::Expr],
    definitions: &HashMap<String, &TypeDef>,
    state_ids: &HashMap<String, usize>,
    bindings: &HashMap<String, &ast::Expr>,
) -> Result<AwaitReceiveFrameLoopPlan, String> {
    let frame = compiler_generated_place(args.get(1), "receive frame", bindings, false)?;
    let step = compiler_generated_place(args.get(2), "receive step closure", bindings, true)?;
    let done = compiler_generated_place(args.get(3), "receive completion closure", bindings, true)?;
    let payload_layout = args
        .first()
        .and_then(|endpoint| table.type_of(endpoint))
        .and_then(|ty| channel_endpoint_payload_layout(ty, ChannelEndpointDirection::Receive, definitions))
        .unwrap_or(ChannelPayloadLayout::Boxed);
    let payload_lane = match &payload_layout {
        ChannelPayloadLayout::Scalar(lane) => Some(*lane),
        _ => None,
    };
    Ok(AwaitReceiveFrameLoopPlan {
        channel: channel_endpoint_place(args.first()),
        frame,
        step,
        done: done.clone(),
        payload_layout,
        payload_lane,
        resume_target: select_loop_resume(args.get(3).expect("four-argument receive frame loop has completion closure"), state_ids, bindings)?,
    })
}

fn select4_frame_loop_plan(
    table: &TypeTable,
    args: &[ast::Expr],
    definitions: &HashMap<String, &TypeDef>,
    state_ids: &HashMap<String, usize>,
    bindings: &HashMap<String, &ast::Expr>,
) -> Result<AwaitSelect4FrameLoopPlan, String> {
    let frame = compiler_generated_place(args.get(4), "select frame", bindings, false)?;
    let step = compiler_generated_place(args.get(5), "select step closure", bindings, true)?;
    let done = compiler_generated_place(args.get(6), "select completion closure", bindings, true)?;
    let endpoint_types = args.iter().take(4).map(|endpoint| table.type_of(endpoint)).collect::<Vec<_>>();
    Ok(AwaitSelect4FrameLoopPlan {
        endpoints: args.iter().take(4).map(|endpoint| channel_endpoint_place(Some(endpoint))).collect(),
        frame,
        step,
        done: done.clone(),
        payload_layout: fixed_select_payload_layout(&endpoint_types, definitions),
        payload_lane: fixed_select_payload_lane(&endpoint_types),
        resume_target: select_loop_resume(
            args.get(6).expect("seven-argument frame loop has completion closure"),
            state_ids,
            bindings,
        )?,
    })
}

fn compiler_generated_place(
    expression: Option<&ast::Expr>,
    role: &str,
    bindings: &HashMap<String, &ast::Expr>,
    require_lambda: bool,
) -> Result<String, String> {
    let Some(ast::Expr::Var(name)) = expression else {
        return Err(format!("{role} is not a compiler-generated local"));
    };
    if !is_generated_closure_binding(name) {
        return Err(format!("{role} is not a reserved compiler local"));
    }
    if require_lambda && !matches!(bindings.get(name), Some(ast::Expr::Lambda { .. })) {
        return Err(format!("{role} is not a direct compiler-generated lambda binding"));
    }
    Ok(name.clone())
}

fn continuation_state(
    continuation: &ast::Expr,
    state_ids: &HashMap<String, usize>,
    bindings: &HashMap<String, &ast::Expr>,
) -> Result<usize, String> {
    let continuation = resolve_binding(continuation, bindings);
    let ast::Expr::Lambda { body, .. } = continuation else {
        return Err("`task.and_then` continuation is not a lambda".into());
    };
    let (terminal, body_bindings) = terminal_with_bindings(body, bindings)
        .ok_or_else(|| "continuation lambda has no terminal state call".to_string())?;
    task_entry_state(resolve_binding(terminal, &body_bindings), state_ids)
        .ok_or_else(|| format!("continuation does not tail-call a compiler state: `{terminal:?}`"))
}

fn select_loop_resume(
    done: &ast::Expr,
    state_ids: &HashMap<String, usize>,
    bindings: &HashMap<String, &ast::Expr>,
) -> Result<SelectResume, String> {
    let done = resolve_binding(done, bindings);
    let ast::Expr::Lambda { body, .. } = done else {
        return Err("select frame completion is not a locally-bound lambda".into());
    };
    let (terminal, body_bindings) = terminal_with_bindings(body, bindings)
        .ok_or_else(|| "select frame completion has no terminal task".to_string())?;
    let terminal = resolve_binding(terminal, &body_bindings);
    if let Some(target) = task_entry_state(terminal, state_ids) {
        return Ok(SelectResume::State(target));
    }
    match terminal {
        ast::Expr::Call { name, args }
            if call_family(name, "task.and_then") && args.len() == 2 =>
        {
            Ok(SelectResume::State(continuation_state(&args[1], state_ids, &body_bindings)?))
        }
        ast::Expr::Call { name, .. }
            if call_family(name, "task.done") || call_family(name, "task.ready_unit") =>
        {
            Ok(SelectResume::Done)
        }
        ast::Expr::Tuple(items) if items.is_empty() => Ok(SelectResume::Done),
        ast::Expr::Ctor { name, args } if name == "Nil" && args.is_empty() => Ok(SelectResume::Done),
        _ => Err(format!("select frame completion does not finish or tail-call a state: `{terminal:?}`")),
    }
}

fn terminal_with_bindings<'a>(
    block: &'a ast::Block,
    inherited: &HashMap<String, &'a ast::Expr>,
) -> Option<(&'a ast::Expr, HashMap<String, &'a ast::Expr>)> {
    let mut bindings = inherited.clone();
    for statement in &block.stmts {
        if let ast::Stmt::Let {
            name,
            value: value @ ast::Expr::Lambda { .. },
            ..
        } = statement
            && is_generated_closure_binding(name)
        {
            bindings.insert(name.clone(), value);
        }
    }
    let terminal = terminal_block_expr(block)?;
    if let ast::Expr::Block(nested) = terminal {
        return terminal_with_bindings(nested, &bindings);
    }
    Some((terminal, bindings))
}

fn resolve_binding<'a>(expression: &'a ast::Expr, bindings: &HashMap<String, &'a ast::Expr>) -> &'a ast::Expr {
    match expression {
        ast::Expr::Var(name) => bindings.get(name).copied().unwrap_or(expression),
        _ => expression,
    }
}

fn is_generated_closure_binding(name: &str) -> bool {
    name.starts_with("__await")
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

/// Capture the type of `chan.channel` before async lowering replaces its
/// `(Sender(m), Receiver(m))` result with an unannotated continuation bind.
/// The call result is the only source-independent allocation boundary: neither
/// capacity nor later endpoint names can recover `m` safely.
fn channel_open_plan(
    table: &TypeTable,
    task: &ast::Expr,
    capacity: Option<&ast::Expr>,
    definitions: &HashMap<String, &TypeDef>,
    resume: usize,
) -> ScalarTransition {
    let payload_layout = table
        .type_of(task)
        .and_then(crate::typeck::ty_to_ast)
        .map(|task_type| channel_open_payload_layout(&task_type, definitions))
        .unwrap_or(ChannelPayloadLayout::Boxed);
    let ownership = channel_ownership_envelope(&payload_layout);
    ScalarTransition::ChannelOpen {
        resume,
        capacity: capacity.map(channel_message_operand).unwrap_or(ChannelMessageOperand::Dynamic),
        payload_layout,
        ownership,
    }
}

fn channel_open_payload_layout(
    task_type: &Type,
    definitions: &HashMap<String, &TypeDef>,
) -> ChannelPayloadLayout {
    let Type::Named(name, arguments) = task_type.unqualified() else {
        return ChannelPayloadLayout::Boxed;
    };
    if name.rsplit('.').next() != Some("Task") || arguments.len() != 1 {
        return ChannelPayloadLayout::Boxed;
    }
    channel_pair_payload_layout(&arguments[0], definitions)
        .unwrap_or(ChannelPayloadLayout::Boxed)
}

fn channel_pair_payload_layout(
    ty: &Type,
    definitions: &HashMap<String, &TypeDef>,
) -> Option<ChannelPayloadLayout> {
    let Type::Tuple(fields) = ty.unqualified() else {
        return None;
    };
    let [first, second] = fields.as_slice() else {
        return None;
    };
    let (first_direction, first_message) = channel_endpoint_type(first)?;
    let (second_direction, second_message) = channel_endpoint_type(second)?;
    if first_direction != ChannelEndpointDirection::Send
        || second_direction != ChannelEndpointDirection::Receive
        || first_message != second_message
    {
        return None;
    }
    Some(channel_payload_layout(first_message, definitions))
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
    fn channel_send_operands_preserve_static_and_literal_values() {
        assert_eq!(
            channel_message_operand(&ast::Expr::Var("value".into())),
            ChannelMessageOperand::Static("value".into())
        );
        assert_eq!(
            channel_message_operand(&ast::Expr::Int(42)),
            ChannelMessageOperand::Int(42)
        );
        assert_eq!(
            channel_message_operand(&ast::Expr::Call {
                name: "effectful".into(),
                args: Vec::new(),
            }),
            ChannelMessageOperand::Dynamic,
            "computed values remain on the ordinary scheduler until the emitter can preserve their evaluation exactly once"
        );
    }

    #[test]
    fn channel_open_binds_layout_from_its_typed_task_result() {
        let pair = Type::Tuple(vec![
            Type::Named("Sender".into(), vec![Type::Named("String".into(), Vec::new())]),
            Type::Named("Receiver".into(), vec![Type::Named("String".into(), Vec::new())]),
        ]);
        let task = Type::Named("Task".into(), vec![pair]);
        assert_eq!(
            channel_open_payload_layout(&task, &HashMap::default()),
            ChannelPayloadLayout::BufferRoot(ChannelBufferRoot::String),
        );
    }

    #[test]
    fn channel_open_refuses_mismatched_endpoint_pairs() {
        let pair = Type::Tuple(vec![
            Type::Named("Sender".into(), vec![Type::Named("Int".into(), Vec::new())]),
            Type::Named("Receiver".into(), vec![Type::Named("String".into(), Vec::new())]),
        ]);
        assert_eq!(
            channel_open_payload_layout(
                &Type::Named("Task".into(), vec![pair]),
                &HashMap::default(),
            ),
            ChannelPayloadLayout::Boxed,
        );
    }

    #[test]
    fn ready_region_formation_keeps_supported_neighbours_when_one_region_is_unsupported() {
        let state = |id, function: &str| CarrierState {
            id,
            function: function.into(),
            source_callable: "main".into(),
            kind: CarrierStateKind::Segment,
            direct: true,
            slots: Vec::new(),
        };
        let catalog = SuspensionCarrierCatalog {
            states: vec![state(4, "open"), state(9, "unsupported"), state(12, "recv")],
            async_regions: vec![Some(40), Some(41), Some(42)],
            max_lane_width: 0,
            typed_channel_plans: Vec::new(),
            region_transition_paths: vec![
                vec![RegionTransitionPath { guarded: false, transitions: Ok(vec![ScalarTransition::ChannelOpen { resume: 9, capacity: ChannelMessageOperand::Int(8), payload_layout: ChannelPayloadLayout::Scalar(CarrierLane::I64), ownership: ChannelOwnershipEnvelope::InlineValue }]) }],
                vec![RegionTransitionPath { guarded: false, transitions: Err("unsupported awaited task call `task.map`".into()) }],
                vec![RegionTransitionPath { guarded: false, transitions: Ok(vec![ScalarTransition::ChannelReceive(AwaitChannelOperationPlan {
                    endpoint: ChannelEndpointPlace::Static("rx".into()),
                    message: None,
                    result: Some(ChannelReceiveResult::Discarded),
                    payload_layout: ChannelPayloadLayout::Scalar(CarrierLane::I64),
                    ownership: ChannelOwnershipEnvelope::InlineValue,
                    resume_target: 13,
                })]) }],
            ],
            scalar_transitions: vec![
                Ok(vec![ScalarTransition::ChannelOpen { resume: 9, capacity: ChannelMessageOperand::Int(8), payload_layout: ChannelPayloadLayout::Scalar(CarrierLane::I64), ownership: ChannelOwnershipEnvelope::InlineValue }]),
                Err("unsupported awaited task call `task.map`".into()),
                Ok(vec![ScalarTransition::ChannelReceive(AwaitChannelOperationPlan {
                    endpoint: ChannelEndpointPlace::Static("rx".into()),
                    message: None,
                    result: Some(ChannelReceiveResult::Discarded),
                    payload_layout: ChannelPayloadLayout::Scalar(CarrierLane::I64),
                    ownership: ChannelOwnershipEnvelope::InlineValue,
                    resume_target: 13,
                })]),
            ],
        };

        let formation = catalog.ready_channel_regions();
        assert_eq!(
            formation.regions.iter().map(|region| region.id).collect::<Vec<_>>(),
            [4, 12],
            "region identity remains the existing carrier-state id"
        );
        assert_eq!(
            formation
                .regions
                .iter()
                .map(|region| region.lowering_region)
                .collect::<Vec<_>>(),
            [40, 42],
            "source suspension boundaries remain visible to the emitter"
        );
        assert_eq!(formation.regions[0].successor_states, [9]);
        assert_eq!(formation.regions[1].successor_states, [13]);
        assert!(matches!(
            formation.regions[0].operations.as_slice(),
            [ReadyChannelOperation {
                state: 4,
                transition_ordinal: 0,
                kind: ReadyChannelOperationKind::Open {
                    resume_target: 9,
                    capacity: ChannelMessageOperand::Int(8),
                    payload_layout: ChannelPayloadLayout::Scalar(CarrierLane::I64),
                    ownership: ChannelOwnershipEnvelope::InlineValue,
                },
            }]
        ));
        assert!(matches!(
            formation.regions[1].operations.as_slice(),
            [ReadyChannelOperation {
                state: 12,
                transition_ordinal: 0,
                kind: ReadyChannelOperationKind::Receive(_),
            }]
        ));
        assert_eq!(formation.excluded.len(), 1);
        assert_eq!(formation.excluded[0].id, 9);
        assert_eq!(formation.excluded[0].lowering_region, 41);
        assert_eq!(
            formation.excluded[0].reason,
            ReadyChannelRegionExclusionReason::UnsupportedTransition {
                detail: "unsupported awaited task call `task.map`".into(),
            },
            "one unsupported interior region must not reject its supported neighbours"
        );
    }

    #[test]
    fn ready_region_formation_keeps_supported_sibling_path() {
        let state = CarrierState {
            id: 4,
            function: "branch".into(),
            source_callable: "main".into(),
            kind: CarrierStateKind::Segment,
            direct: true,
            slots: Vec::new(),
        };
        let send = ScalarTransition::ChannelSend(AwaitChannelOperationPlan {
            endpoint: ChannelEndpointPlace::Static("tx".into()),
            message: Some(ChannelMessageOperand::Static("value".into())),
            result: None,
            payload_layout: ChannelPayloadLayout::Scalar(CarrierLane::I64),
            ownership: ChannelOwnershipEnvelope::InlineValue,
            resume_target: 8,
        });
        let catalog = SuspensionCarrierCatalog {
            states: vec![state],
            async_regions: vec![Some(40)],
            max_lane_width: 0,
            typed_channel_plans: Vec::new(),
            region_transition_paths: vec![vec![
                RegionTransitionPath { guarded: false, transitions: Ok(vec![send.clone()]) },
                RegionTransitionPath { guarded: false, transitions: Err("unsupported awaited task call `task.map`".into()) },
            ]],
            // The existing scalar proof remains all-or-nothing; ready-region
            // formation must instead retain the supported terminal path.
            scalar_transitions: vec![Err("unsupported awaited task call `task.map`".into())],
        };

        let formation = catalog.ready_channel_regions();
        assert_eq!(formation.regions.len(), 1);
        assert_eq!(formation.regions[0].id, 4);
        assert_eq!(formation.regions[0].path_ordinal, 0);
        assert_eq!(formation.regions[0].operations, ready_channel_operations(4, &[send]));
        assert_eq!(formation.excluded.len(), 1);
        assert_eq!(formation.excluded[0].id, 4);
        assert_eq!(formation.excluded[0].path_ordinal, 1);
        assert!(matches!(
            formation.excluded[0].reason,
            ReadyChannelRegionExclusionReason::UnsupportedTransition { .. }
        ));
    }

    #[test]
    fn guarded_terminal_paths_stay_on_the_original_dispatcher() {
        let catalog = SuspensionCarrierCatalog {
            states: vec![CarrierState {
                id: 4,
                function: "branch".into(),
                source_callable: "main".into(),
                kind: CarrierStateKind::Segment,
                direct: true,
                slots: Vec::new(),
            }],
            async_regions: vec![Some(40)],
            max_lane_width: 0,
            typed_channel_plans: Vec::new(),
            region_transition_paths: vec![vec![
                RegionTransitionPath {
                    guarded: true,
                    transitions: Ok(vec![ScalarTransition::ChannelOpen { resume: 8, capacity: ChannelMessageOperand::Int(8), payload_layout: ChannelPayloadLayout::Scalar(CarrierLane::I64), ownership: ChannelOwnershipEnvelope::InlineValue }]),
                },
                RegionTransitionPath {
                    guarded: true,
                    transitions: Err("unsupported awaited task call `task.map`".into()),
                },
            ]],
            scalar_transitions: vec![Err("unsupported awaited task call `task.map`".into())],
        };

        let formation = catalog.ready_channel_regions();
        assert!(formation.regions.is_empty());
        assert_eq!(formation.excluded.len(), 2);
        assert!(formation.excluded.iter().all(|excluded| matches!(
            excluded.reason,
            ReadyChannelRegionExclusionReason::GuardedControlFlow
        )));
    }

    #[test]
    fn region_successors_include_all_spawn_and_call_targets() {
        assert_eq!(
            transition_successors(&[
                ScalarTransition::Spawn { child: 9, resume: 4 },
                ScalarTransition::Call { callee: 12, resume: 4 },
            ]),
            [4, 9, 12]
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
            message: None,
            result: Some(ChannelReceiveResult::Discarded),
            payload_layout: ChannelPayloadLayout::Scalar(CarrierLane::I64),
            ownership: ChannelOwnershipEnvelope::InlineValue,
            resume_target: 1,
        });
        assert_eq!(ready_transition_is_static(&dynamic), Err("receive"));

        let computed_send = ScalarTransition::ChannelSend(AwaitChannelOperationPlan {
            endpoint: ChannelEndpointPlace::Static("tx".into()),
            message: Some(ChannelMessageOperand::Dynamic),
            result: None,
            payload_layout: ChannelPayloadLayout::Scalar(CarrierLane::I64),
            ownership: ChannelOwnershipEnvelope::InlineValue,
            resume_target: 1,
        });
        assert_eq!(ready_transition_is_static(&computed_send), Err("send payload"));
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
                    endpoint_slot: 0,
                    endpoint_lane: Some(0),
                    endpoint_place: "tx".into(),
                    direction: ChannelEndpointDirection::Send,
                    message_type: "String".into(),
                    payload_layout: ChannelPayloadLayout::BufferRoot(ChannelBufferRoot::String),
                    ownership: ChannelOwnershipEnvelope::BufferRoot,
                },
                TypedChannelPlan {
                    state: 0,
                    endpoint_slot: 1,
                    endpoint_lane: Some(1),
                    endpoint_place: "rx".into(),
                    direction: ChannelEndpointDirection::Receive,
                    message_type: "Bytes".into(),
                    payload_layout: ChannelPayloadLayout::BufferRoot(ChannelBufferRoot::Bytes),
                    ownership: ChannelOwnershipEnvelope::BufferRoot,
                },
                TypedChannelPlan {
                    state: 0,
                    endpoint_slot: 2,
                    endpoint_lane: Some(2),
                    endpoint_place: "ints".into(),
                    direction: ChannelEndpointDirection::Receive,
                    message_type: "Int".into(),
                    payload_layout: ChannelPayloadLayout::Scalar(CarrierLane::I64),
                    ownership: ChannelOwnershipEnvelope::InlineValue,
                },
                TypedChannelPlan {
                    state: 0,
                    endpoint_slot: 3,
                    endpoint_lane: Some(3),
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
                message: Some(ChannelMessageOperand::Static("value".into())),
                result: None,
                payload_layout: ChannelPayloadLayout::Boxed,
                ownership: ChannelOwnershipEnvelope::BoxedValue,
                resume_target: 7,
            })]),
        );
    }

    #[test]
    fn scalar_receive_transition_maps_a_direct_continuation_argument() {
        let module = witchy_syntax::parser::parse_module(
            "fn state() -> Nil:\n    task.lazy__Nil(fn(): task.and_then__Int__Nil(chan.recv__Int(rx), fn(value): resume(value)))\n",
        )
        .expect("receive transition fixture parses");
        let Item::Function(function) = &module.items[0] else {
            panic!("state function")
        };
        let state_ids = HashMap::from_iter([("resume".to_string(), 7)]);
        assert_eq!(
            scalar_transitions_for_function(function, &state_ids, &TypeTable::default(), &HashMap::new()),
            Ok(vec![ScalarTransition::ChannelReceive(AwaitChannelOperationPlan {
                endpoint: ChannelEndpointPlace::Static("rx".into()),
                message: None,
                result: Some(ChannelReceiveResult::SuccessorArgument {
                    state: 7,
                    argument: 0,
                    parameter: "value".into(),
                }),
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

    #[test]
    fn ready_region_operations_keep_state_order_and_typed_channel_facts() {
        let send = AwaitChannelOperationPlan {
            endpoint: ChannelEndpointPlace::Static("tx".into()),
            message: Some(ChannelMessageOperand::String("ready".into())),
            result: None,
            payload_layout: ChannelPayloadLayout::BufferRoot(ChannelBufferRoot::String),
            ownership: ChannelOwnershipEnvelope::BufferRoot,
            resume_target: 4,
        };
        let receive = AwaitChannelOperationPlan {
            endpoint: ChannelEndpointPlace::Static("rx".into()),
            message: None,
            result: Some(ChannelReceiveResult::Discarded),
            payload_layout: ChannelPayloadLayout::Scalar(CarrierLane::I64),
            ownership: ChannelOwnershipEnvelope::InlineValue,
            resume_target: 5,
        };
        let select3 = AwaitFixedSelectPlan {
            endpoints: vec![
                ChannelEndpointPlace::Static("a".into()),
                ChannelEndpointPlace::Static("b".into()),
                ChannelEndpointPlace::Static("c".into()),
            ],
            payload_layout: ChannelPayloadLayout::Scalar(CarrierLane::I64),
            payload_lane: Some(CarrierLane::I64),
            resume_target: 6,
            result_use: SelectResultUse::ImmediateMatch,
        };
        let select4 = AwaitFixedSelectPlan {
            endpoints: vec![
                ChannelEndpointPlace::Static("d".into()),
                ChannelEndpointPlace::Static("e".into()),
                ChannelEndpointPlace::Static("f".into()),
                ChannelEndpointPlace::Static("g".into()),
            ],
            payload_layout: ChannelPayloadLayout::Packed(vec![CarrierLane::I64, CarrierLane::I64]),
            payload_lane: None,
            resume_target: 7,
            result_use: SelectResultUse::Escape,
        };
        let plan = ReadyChannelRegionPlan {
            state_count: 3,
            max_lane_width: 1,
            states: vec![
                ScalarExecutorState {
                    id: 3,
                    function: "open-and-send".into(),
                    source_callable: "producer".into(),
                    slots: Vec::new(),
                    transitions: vec![
                        ScalarTransition::Jump { target: 3 },
                        ScalarTransition::ChannelOpen { resume: 2, capacity: ChannelMessageOperand::Int(8), payload_layout: ChannelPayloadLayout::Scalar(CarrierLane::I64), ownership: ChannelOwnershipEnvelope::InlineValue },
                        ScalarTransition::ChannelSend(send.clone()),
                    ],
                },
                ScalarExecutorState {
                    id: 8,
                    function: "receive".into(),
                    source_callable: "consumer".into(),
                    slots: Vec::new(),
                    transitions: vec![ScalarTransition::ChannelReceive(receive.clone())],
                },
                ScalarExecutorState {
                    id: 13,
                    function: "fixed-selects".into(),
                    source_callable: "consumer".into(),
                    slots: Vec::new(),
                    transitions: vec![
                        ScalarTransition::ChannelSelect3(select3.clone()),
                        ScalarTransition::ChannelSelect4(select4.clone()),
                    ],
                },
            ],
            typed_channels: Vec::new(),
        };

        assert_eq!(
            plan.operations(),
            vec![
                ReadyChannelOperation {
                    state: 3,
                    transition_ordinal: 1,
                    kind: ReadyChannelOperationKind::Open {
                        resume_target: 2,
                        capacity: ChannelMessageOperand::Int(8),
                        payload_layout: ChannelPayloadLayout::Scalar(CarrierLane::I64),
                        ownership: ChannelOwnershipEnvelope::InlineValue,
                    },
                },
                ReadyChannelOperation {
                    state: 3,
                    transition_ordinal: 2,
                    kind: ReadyChannelOperationKind::Send(send),
                },
                ReadyChannelOperation {
                    state: 8,
                    transition_ordinal: 0,
                    kind: ReadyChannelOperationKind::Receive(receive),
                },
                ReadyChannelOperation {
                    state: 13,
                    transition_ordinal: 0,
                    kind: ReadyChannelOperationKind::Select3(select3),
                },
                ReadyChannelOperation {
                    state: 13,
                    transition_ordinal: 1,
                    kind: ReadyChannelOperationKind::Select4(select4),
                },
            ],
        );
    }
}
