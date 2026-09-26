//! The checked-source product SafeWIR consumes (RFC-0170).
//!
//! `witchy_types::pipeline::CanonicalCheckedSource` owns a checked program and
//! everything derived from it. SafeWIR, the Wasm emitter and their authority
//! checks read it only through [`CanonicalSourceView`], a borrowed projection
//! defined here so that `witchy-wir` need not depend on the checker. Both
//! [`CanonicalSourceView`] and [`CheckedSourceWitness`] are minted only by the
//! checked frontend; `witchy-wir`'s architecture tests pin their single
//! construction sites.

use std::collections::BTreeMap;

use witchy_syntax::ast::{Convention, Expr, Function, Item, Module, Type, TypeDef};

use crate::abi::{CanonicalCallableAbi, CanonicalCallableAbiManifest};
use crate::access::AccessSignature;
use crate::carrier::CarrierElement;
use crate::closure::ClosureWitnessManifest;
use crate::storage::{ManagedAggregateFieldTransfer, OpaqueOwnerEvidence, StoragePlan};
use crate::specialization::LogicalSpecializationIdentity;

/// Declaration-owned execution mode for a source callable. A deferred task
/// executes its body's mode; the site that constructs or awaits it cannot
/// relabel that body. This is a source judgment, not a Task value proof.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceExecutionMode { Normal, Opt }

impl SourceExecutionMode {
    pub fn is_opt(self) -> bool { self == Self::Opt }
}

/// Checked value-flow proof for a deferred task. A `Task(T)` result type does
/// not identify the declaration mode of the code which runs when it is driven.
/// Parameter references are symbolic until a checked direct call supplies its
/// exact argument proofs; unknown producers never acquire opt authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskCallableScope {
    owner: String,
    ordinal: u32,
}

impl TaskCallableScope {
    pub fn new(owner: String, ordinal: u32) -> Self { Self { owner, ordinal } }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskExecutionProof {
    Unknown,
    /// A control-flow join or invalidation lost the exact callable, but
    /// it may still carry an opt task-input obligation. It cannot escape or
    /// be invoked through an unproved function value.
    UnprovedCallableObligation,
    Task(SourceExecutionMode, Box<Self>),
    /// A checked local callable with a distinct lexical formal scope.
    Callable {
        scope: TaskCallableScope,
        parameters: usize,
        required_opt_inputs: Vec<Self>,
        result: Box<Self>,
    },
    /// An exact checked declaration reference. Its result and opt task-input
    /// obligations use the declaration's own symbolic parameters until this
    /// value is invoked with checked arguments.
    DeclaredCallable {
        conventions: Vec<Convention>,
        required_opt_inputs: Vec<Self>,
        result: Box<Self>,
    },
    Parameter(usize),
    /// A lambda formal cannot borrow its enclosing function's parameter proof.
    CallableParameter(TaskCallableScope, usize),
    /// Result of executing a symbolic task parameter or projection.
    Awaited(Box<Self>),
    /// Checked positional projection from a tuple or nominal aggregate.
    Slot(Box<Self>, usize),
    /// Checked field of one exact sum variant. A different variant is absent,
    /// never an interchangeable same-position payload.
    VariantSlot(Box<Self>, usize, usize),
    /// Dynamic index into a checked homogeneous sequence.
    AnyElement(Box<Self>),
    Elements(Vec<Self>),
    Fields(BTreeMap<String, Self>),
    Variant { ordinal: usize, fields: Vec<Self> },
    Variants(BTreeMap<usize, Vec<Self>>),
}

/// One checked pre-CPS await and the exact value-flow proof for its operand.
/// The source coordinate is stable across final function renaming; an optional
/// final item binding is established only after CPS and specialization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskAwaitProof {
    source_module: String,
    source_item: u32,
    source_method: Option<u32>,
    suspension: u32,
    source_mode: SourceExecutionMode,
    origin: TaskExecutionProof,
}

impl TaskAwaitProof {
    #[doc(hidden)]
    pub fn from_checked_frontend(
        source_module: String,
        source_item: u32,
        source_method: Option<u32>,
        suspension: u32,
        source_mode: SourceExecutionMode,
        origin: TaskExecutionProof,
    ) -> Self {
        Self { source_module, source_item, source_method, suspension, source_mode, origin }
    }

    pub fn source_identity(&self) -> (&str, u32, Option<u32>, u32) {
        (&self.source_module, self.source_item, self.source_method, self.suspension)
    }

    pub fn source_mode(&self) -> SourceExecutionMode { self.source_mode }
    pub fn origin(&self) -> &TaskExecutionProof { &self.origin }

    pub fn opt_compatible(&self) -> bool {
        matches!(self.origin, TaskExecutionProof::Task(SourceExecutionMode::Opt, _))
            || self.origin.is_parameter_projection()
    }
}

impl TaskExecutionProof {
    /// A checked source summary may defer an opt requirement only when it is rooted
    /// in one of that callable's checked parameters. Unknown storage cannot
    /// become a deferred obligation by being wrapped in a projection.
    pub fn is_parameter_projection(&self) -> bool {
        match self {
            Self::Parameter(_) => true,
            Self::Slot(base, _) | Self::VariantSlot(base, _, _) | Self::AnyElement(base) | Self::Awaited(base) => base.is_parameter_projection(),
            _ => false,
        }
    }

    /// A lambda-local obligation may be deferred only to that exact lexical
    /// formal scope. An enclosing function's parameter is a separate proof.
    pub fn is_callable_parameter_projection(&self, scope: &TaskCallableScope) -> bool {
        let mut current = self;
        for _ in 0..=128 {
            match current {
                Self::CallableParameter(actual, _) => return actual == scope,
                Self::Slot(base, _) | Self::VariantSlot(base, _, _)
                | Self::AnyElement(base) | Self::Awaited(base) => current = base,
                _ => return false,
            }
        }
        false
    }

    /// A callable obligation cannot cross an opaque function-value ABI.
    /// Only a known local invocation can discharge it against actual inputs.
    fn has_callable_input_obligation(&self, only_local: bool) -> bool {
        fn contains(proof: &TaskExecutionProof, only_local: bool, depth: usize, remaining: &mut usize) -> bool {
            if depth > 128 || *remaining == 0 { return true; }
            *remaining -= 1;
            match proof {
                TaskExecutionProof::Callable { required_opt_inputs, result, .. } =>
                    !required_opt_inputs.is_empty() || contains(result, only_local, depth + 1, remaining),
                TaskExecutionProof::UnprovedCallableObligation => true,
                TaskExecutionProof::Task(_, result) | TaskExecutionProof::Awaited(result)
                | TaskExecutionProof::Slot(result, _)
                | TaskExecutionProof::VariantSlot(result, _, _)
                | TaskExecutionProof::AnyElement(result) => contains(result, only_local, depth + 1, remaining),
                TaskExecutionProof::DeclaredCallable { required_opt_inputs, result, .. } =>
                    (!only_local && !required_opt_inputs.is_empty())
                        || required_opt_inputs.iter().any(|input| contains(input, only_local, depth + 1, remaining))
                        || contains(result, only_local, depth + 1, remaining),
                TaskExecutionProof::Elements(values) | TaskExecutionProof::Variant { fields: values, .. } =>
                    values.iter().any(|value| contains(value, only_local, depth + 1, remaining)),
                TaskExecutionProof::Fields(fields) =>
                    fields.values().any(|value| contains(value, only_local, depth + 1, remaining)),
                TaskExecutionProof::Variants(variants) =>
                    variants.values().flatten().any(|value| contains(value, only_local, depth + 1, remaining)),
                _ => false,
            }
        }
        contains(self, only_local, 0, &mut 4096)
    }

    pub fn has_local_callable_obligation(&self) -> bool {
        self.has_callable_input_obligation(true)
    }

    pub fn has_any_callable_input_obligation(&self) -> bool {
        self.has_callable_input_obligation(false)
    }

    pub fn invalidated(&self) -> Self {
        if self.has_callable_input_obligation(false) { Self::UnprovedCallableObligation }
        else { Self::Unknown }
    }

    /// Keep recursively composed summaries finite even for recursive source
    /// functions that repeatedly wrap their own result.
    pub fn bounded(self, max_nodes: usize) -> Self {
        fn within(proof: &TaskExecutionProof, remaining: &mut usize, depth: usize) -> bool {
            if depth > 128 || *remaining == 0 { return false; }
            *remaining -= 1;
            match proof {
                TaskExecutionProof::Elements(elements) => elements.iter().all(|child| within(child, remaining, depth + 1)),
                TaskExecutionProof::Fields(fields) => fields.values().all(|child| within(child, remaining, depth + 1)),
                TaskExecutionProof::Variant { fields, .. } => fields.iter().all(|child| within(child, remaining, depth + 1)),
                TaskExecutionProof::Variants(variants) => variants.values().flatten().all(|child| within(child, remaining, depth + 1)),
                TaskExecutionProof::Task(_, result) | TaskExecutionProof::Awaited(result)
                    | TaskExecutionProof::Slot(result, _) | TaskExecutionProof::VariantSlot(result, _, _)
                    | TaskExecutionProof::AnyElement(result) => within(result, remaining, depth + 1),
                TaskExecutionProof::Callable { required_opt_inputs, result, .. } =>
                    required_opt_inputs.iter().all(|child| within(child, remaining, depth + 1))
                        && within(result, remaining, depth + 1),
                TaskExecutionProof::DeclaredCallable { required_opt_inputs, result, .. } =>
                    required_opt_inputs.iter().all(|child| within(child, remaining, depth + 1))
                        && within(result, remaining, depth + 1),
                _ => true,
            }
        }
        let mut remaining = max_nodes;
        if within(&self, &mut remaining, 0) { self }
        else if self.has_callable_input_obligation(false) { Self::UnprovedCallableObligation }
        else { Self::Unknown }
    }

    /// Join alternative control-flow paths. A mixed or missing proof is
    /// unknown, including one path which lacks task provenance.
    pub fn join(self, other: Self) -> Self {
        match (self, other) {
            (Self::Task(left_mode, left_result), Self::Task(right_mode, right_result)) if left_mode == right_mode => {
                Self::Task(left_mode, Box::new((*left_result).join(*right_result)))
            }
            (Self::Callable { scope: left_scope, parameters: left_arity, required_opt_inputs: left_requirements, result: left },
                Self::Callable { scope: right_scope, parameters: right_arity, required_opt_inputs: right_requirements, result: right })
                if left_scope == right_scope && left_arity == right_arity
                    && left_requirements == right_requirements => {
                Self::Callable {
                    scope: left_scope,
                    parameters: left_arity,
                    required_opt_inputs: left_requirements,
                    result: Box::new((*left).join(*right)),
                }
            }
            (Self::DeclaredCallable { conventions: left_conventions, required_opt_inputs: left_requirements, result: left },
                Self::DeclaredCallable { conventions: right_conventions, required_opt_inputs: right_requirements, result: right })
                if left_conventions == right_conventions && left_requirements == right_requirements => {
                Self::DeclaredCallable {
                    conventions: left_conventions,
                    required_opt_inputs: left_requirements,
                    result: Box::new((*left).join(*right)),
                }
            }
            (Self::Awaited(left), Self::Awaited(right)) => Self::Awaited(Box::new((*left).join(*right))),
            (Self::Elements(left), Self::Elements(right)) if left.len() == right.len() => {
                Self::Elements(left.into_iter().zip(right).map(|(a, b)| a.join(b)).collect())
            }
            (Self::Fields(left), Self::Fields(mut right)) if left.keys().eq(right.keys()) => {
                Self::Fields(left.into_iter().map(|(key, value)| {
                    let other = right.remove(&key).expect("matching field keys");
                    (key, value.join(other))
                }).collect())
            }
            (Self::Variant { ordinal: a, fields: left }, Self::Variant { ordinal: b, fields: right }) => {
                if a == b {
                    if left.len() != right.len() {
                        return if left.iter().chain(&right).any(Self::has_any_callable_input_obligation) {
                            Self::UnprovedCallableObligation
                        } else { Self::Unknown };
                    }
                    Self::Variant { ordinal: a, fields: left.into_iter().zip(right).map(|(a, b)| a.join(b)).collect() }
                } else {
                    Self::Variants(BTreeMap::from([(a, left), (b, right)]))
                }
            }
            (Self::Variants(mut variants), Self::Variant { ordinal, fields })
            | (Self::Variant { ordinal, fields }, Self::Variants(mut variants)) => {
                if let Some(old) = variants.remove(&ordinal) {
                    if old.len() != fields.len() {
                        return if old.iter().chain(&fields).any(Self::has_any_callable_input_obligation) {
                            Self::UnprovedCallableObligation
                        } else { Self::Unknown };
                    }
                    variants.insert(ordinal, old.into_iter().zip(fields).map(|(a, b)| a.join(b)).collect());
                } else {
                    variants.insert(ordinal, fields);
                }
                Self::Variants(variants)
            }
            (Self::Variants(mut left), Self::Variants(right)) => {
                for (ordinal, fields) in right {
                    if let Some(old) = left.remove(&ordinal) {
                        if old.len() != fields.len() {
                            return if old.iter().chain(&fields).any(Self::has_any_callable_input_obligation) {
                                Self::UnprovedCallableObligation
                            } else { Self::Unknown };
                        }
                        left.insert(ordinal, old.into_iter().zip(fields).map(|(a, b)| a.join(b)).collect());
                    } else {
                        left.insert(ordinal, fields);
                    }
                }
                Self::Variants(left)
            }
            (Self::Slot(left, a), Self::Slot(right, b)) if a == b => Self::Slot(Box::new((*left).join(*right)), a),
            (Self::VariantSlot(left, a, field_a), Self::VariantSlot(right, b, field_b))
                if a == b && field_a == field_b => Self::VariantSlot(Box::new((*left).join(*right)), a, field_a),
            (Self::AnyElement(left), Self::AnyElement(right)) => Self::AnyElement(Box::new((*left).join(*right))),
            (left, right) if left == right => left,
            (left, right) if left.has_callable_input_obligation(false)
                || right.has_callable_input_obligation(false) => Self::UnprovedCallableObligation,
            _ => Self::Unknown,
        }
    }

    /// Instantiate an interprocedural summary with the exact checked call's
    /// argument proofs. Bounded recursion keeps malformed summaries closed.
    fn substitute(&self, arguments: &[Self], formal_scope: Option<&TaskCallableScope>) -> Self {
        fn go(proof: &TaskExecutionProof, arguments: &[TaskExecutionProof], formal_scope: Option<&TaskCallableScope>, depth: usize) -> TaskExecutionProof {
            if depth > 128 {
                return if proof.has_callable_input_obligation(false) {
                    TaskExecutionProof::UnprovedCallableObligation
                } else { TaskExecutionProof::Unknown };
            }
            match proof {
                TaskExecutionProof::Unknown => TaskExecutionProof::Unknown,
                TaskExecutionProof::UnprovedCallableObligation => TaskExecutionProof::UnprovedCallableObligation,
                TaskExecutionProof::Task(mode, result) => TaskExecutionProof::Task(*mode, Box::new(go(result, arguments, formal_scope, depth + 1))),
                TaskExecutionProof::Callable { scope, parameters, required_opt_inputs, result } => TaskExecutionProof::Callable {
                    scope: scope.clone(), parameters: *parameters,
                    required_opt_inputs: required_opt_inputs.iter()
                        .map(|input| go(input, arguments, formal_scope, depth + 1)).collect(),
                    result: Box::new(go(result, arguments, formal_scope, depth + 1)),
                },
                // A declared callable has no lexical captures. Substituting
                // the enclosing function must not bind its own formals.
                TaskExecutionProof::DeclaredCallable { .. } => proof.clone(),
                TaskExecutionProof::Parameter(index) if formal_scope.is_none() =>
                    arguments.get(*index).cloned().unwrap_or(TaskExecutionProof::Unknown),
                TaskExecutionProof::Parameter(index) => TaskExecutionProof::Parameter(*index),
                TaskExecutionProof::CallableParameter(scope, index) if formal_scope == Some(scope) =>
                    arguments.get(*index).cloned().unwrap_or(TaskExecutionProof::Unknown),
                TaskExecutionProof::CallableParameter(scope, index) =>
                    TaskExecutionProof::CallableParameter(scope.clone(), *index),
                TaskExecutionProof::Awaited(base) => match go(base, arguments, formal_scope, depth + 1) {
                    TaskExecutionProof::Task(_, result) => *result,
                    TaskExecutionProof::Unknown => TaskExecutionProof::Unknown,
                    other => TaskExecutionProof::Awaited(Box::new(other)),
                },
                TaskExecutionProof::Slot(base, index) => match go(base, arguments, formal_scope, depth + 1) {
                    TaskExecutionProof::Elements(elements) => elements.get(*index).cloned().unwrap_or(TaskExecutionProof::Unknown),
                    TaskExecutionProof::Unknown => TaskExecutionProof::Unknown,
                    other => TaskExecutionProof::Slot(Box::new(other), *index),
                },
                TaskExecutionProof::VariantSlot(base, ordinal, index) => match go(base, arguments, formal_scope, depth + 1) {
                    TaskExecutionProof::Variant { ordinal: actual, fields } if actual == *ordinal =>
                        fields.get(*index).cloned().unwrap_or(TaskExecutionProof::Unknown),
                    TaskExecutionProof::Variants(mut variants) => variants.remove(ordinal)
                        .and_then(|fields| fields.get(*index).cloned()).unwrap_or(TaskExecutionProof::Unknown),
                    TaskExecutionProof::Variant { .. } => TaskExecutionProof::Unknown,
                    TaskExecutionProof::Unknown => TaskExecutionProof::Unknown,
                    other => TaskExecutionProof::VariantSlot(Box::new(other), *ordinal, *index),
                },
                TaskExecutionProof::AnyElement(base) => match go(base, arguments, formal_scope, depth + 1) {
                    TaskExecutionProof::Elements(elements) => elements.into_iter().reduce(TaskExecutionProof::join).unwrap_or(TaskExecutionProof::Unknown),
                    TaskExecutionProof::Unknown => TaskExecutionProof::Unknown,
                    other => TaskExecutionProof::AnyElement(Box::new(other)),
                },
                TaskExecutionProof::Elements(elements) => TaskExecutionProof::Elements(
                    elements.iter().map(|child| go(child, arguments, formal_scope, depth + 1)).collect(),
                ),
                TaskExecutionProof::Fields(fields) => TaskExecutionProof::Fields(
                    fields.iter().map(|(name, child)| (name.clone(), go(child, arguments, formal_scope, depth + 1))).collect(),
                ),
                TaskExecutionProof::Variant { ordinal, fields } => TaskExecutionProof::Variant {
                    ordinal: *ordinal,
                    fields: fields.iter().map(|child| go(child, arguments, formal_scope, depth + 1)).collect(),
                },
                TaskExecutionProof::Variants(variants) => TaskExecutionProof::Variants(
                    variants.iter().map(|(ordinal, fields)| (*ordinal,
                        fields.iter().map(|child| go(child, arguments, formal_scope, depth + 1)).collect())).collect(),
                ),
            }
        }
        go(self, arguments, formal_scope, 0)
    }

    pub fn instantiate(&self, arguments: &[Self]) -> Self {
        self.substitute(arguments, None)
    }

    pub fn instantiate_callback(&self, scope: &TaskCallableScope, arguments: &[Self]) -> Self {
        self.substitute(arguments, Some(scope))
    }
}

/// Resolve linked callables against the module that declared them, including
/// generated async resumes and closure bodies. Unlinked source uses its own
/// module mode. This same judgment serves checking and canonical lowering.
pub fn source_execution_mode(function: &Function, modes: &[String]) -> SourceExecutionMode {
    let opt = if let Some(owner) = witchy_syntax::linker::linked_source_module(function) {
        modes.iter().any(|mode| mode == &format!("@opt:{owner}"))
    } else if !function.name.contains('.') {
        modes.iter().any(|mode| mode == "opt")
    } else {
        modes.iter().any(|mode| {
            mode.strip_prefix("@opt:")
                .and_then(|owner| function.name.strip_prefix(owner))
                .is_some_and(|suffix| suffix.starts_with('.'))
        })
    };
    if opt { SourceExecutionMode::Opt } else { SourceExecutionMode::Normal }
}

/// Resolve a diagnostic name only when its enclosing declaration is present
/// in this exact module. Copy repair carries the mode directly in its fact.
pub fn named_source_execution_mode(module: &Module, name: &str) -> Option<SourceExecutionMode> {
    // The no-copy walker names nested lambda diagnostics by appending one
    // `::<lambda>` per lexical body. Attribute those diagnostics to their
    // checked enclosing declaration rather than interpreting the generated
    // name's spelling as a source-module mode.
    let mut enclosing = name;
    for _ in 0..=128 {
        if let Some(mode) = module.items.iter().find_map(|item| match item {
            Item::Function(function) if function.name == enclosing => {
                Some(source_execution_mode(function, &module.modes))
            }
            Item::Impl(implementation) => implementation.methods.iter().find_map(|method| {
                (format!("{}__{}", implementation.type_name, method.name) == enclosing)
                    .then(|| source_execution_mode(method, &module.modes))
            }),
            _ => None,
        }) {
            return Some(mode);
        }
        enclosing = enclosing.strip_suffix("::<lambda>")?;
    }
    None
}

/// Evidence that exact source bytes crossed the checked type-checking boundary.
///
/// Its fields are private so a caller cannot choose a digest and present it as
/// checked-source authority. The one constructor,
/// [`CheckedSourceWitness::mint_for_checked_frontend`], is called only by the
/// source-bearing checked-link entry point in `witchy_types::pipeline`; the
/// `checked_source_authority_has_one_frontend_minting_site` architecture test
/// pins that.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckedSourceWitness {
    source_bundle_digest: [u8; 32],
    proper_tail_manifest_digest: [u8; 32],
    callable_abi_manifest_digest: [u8; 32],
    closure_witness_digest: [u8; 32],
    launch_contract_digest: [u8; 32],
    obligations_digest: [u8; 32],
    precompiled_declarations_digest: [u8; 32],
}

/// The digests one checked-source witness binds together.
pub struct CheckedSourceDigests {
    pub source_bundle_digest: [u8; 32],
    pub proper_tail_manifest_digest: [u8; 32],
    pub callable_abi_manifest_digest: [u8; 32],
    pub closure_witness_digest: [u8; 32],
    pub launch_contract_digest: [u8; 32],
    pub obligations_digest: [u8; 32],
    pub precompiled_declarations_digest: [u8; 32],
}

impl CheckedSourceWitness {
    /// Checked-frontend only; see the type's documentation.
    #[doc(hidden)]
    pub fn mint_for_checked_frontend(digests: CheckedSourceDigests) -> Self {
        let CheckedSourceDigests {
            source_bundle_digest,
            proper_tail_manifest_digest,
            callable_abi_manifest_digest,
            closure_witness_digest,
            launch_contract_digest,
            obligations_digest,
            precompiled_declarations_digest,
        } = digests;
        Self {
            source_bundle_digest,
            proper_tail_manifest_digest,
            callable_abi_manifest_digest,
            closure_witness_digest,
            launch_contract_digest,
            obligations_digest,
            precompiled_declarations_digest,
        }
    }

    /// Rebind the launch contract after the checked build-recipe frontend
    /// replaced the payload with its version-2 build contract. Called only by
    /// `witchy_types::pipeline::build_recipe` (pinned with the minting site).
    #[doc(hidden)]
    pub fn rebind_launch_contract_for_build_recipe(&mut self, launch_contract_digest: [u8; 32]) {
        self.launch_contract_digest = launch_contract_digest;
    }

    /// Production uses the exact raw bundle identity. A compiler-owned test
    /// driver uses its domain-separated recipe-derived identity; that driver's
    /// TestDriverIdentity separately retains the original raw bundle identity.
    pub fn source_bundle_digest(&self) -> [u8; 32] {
        self.source_bundle_digest
    }

    /// Digest of the sorted `CanonicalObligation` set this checked source
    /// recorded. Bound into SafeWIR authority so the set cannot be edited
    /// between checked-source construction and canonical admission.
    pub fn obligations_digest(&self) -> [u8; 32] {
        self.obligations_digest
    }

    pub fn proper_tail_manifest_digest(&self) -> [u8; 32] {
        self.proper_tail_manifest_digest
    }

    pub fn callable_abi_manifest_digest(&self) -> [u8; 32] {
        self.callable_abi_manifest_digest
    }

    pub fn closure_witness_digest(&self) -> [u8; 32] {
        self.closure_witness_digest
    }

    pub fn launch_contract_digest(&self) -> [u8; 32] {
        self.launch_contract_digest
    }

    /// Digest of the authenticated external declaration environment joined to
    /// this exact source link. The empty environment has its own canonical
    /// digest; it is omitted only from the historical bundle-input hash for
    /// backward-compatible empty-source identities.
    pub fn precompiled_declarations_digest(&self) -> [u8; 32] {
        self.precompiled_declarations_digest
    }

    pub fn verifies_closure_witness(&self, manifest: &ClosureWitnessManifest) -> bool {
        self.closure_witness_digest == manifest.digest()
    }
}

/// The finalized access-signature queries SafeWIR authority makes against the
/// checked facts of the exact module in the same view.
pub trait CheckedAccessQuery {
    /// The finalized access identity of a checked direct declaration.
    fn declaration(&self, name: &str) -> Option<&AccessSignature>;
    /// The callable selected at one exact checked call expression of `module`.
    fn call_at(&self, module: &Module, expression: &Expr) -> Option<&AccessSignature>;
}

/// One export of an authenticated precompiled library joined to the source
/// link, as the Wasm import it binds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrecompiledExport {
    pub library_alias: String,
    pub export: String,
    pub library_binding_digest: [u8; 32],
    pub wasm_sha256: [u8; 32],
    pub sidecar_sha256: [u8; 32],
}

/// Everything a view borrows, as the checked frontend assembles it.
pub struct CanonicalSourceParts<'s> {
    pub module: &'s Module,
    pub type_definitions: &'s BTreeMap<String, TypeDef>,
    pub source_witness: &'s CheckedSourceWitness,
    pub closure_witness: &'s ClosureWitnessManifest,
    pub signatures: &'s BTreeMap<String, Type>,
    pub generic_instances: &'s BTreeMap<String, LogicalSpecializationIdentity>,
    pub abi_manifest: &'s CanonicalCallableAbiManifest,
    pub closed_abis: &'s BTreeMap<String, CanonicalCallableAbi>,
    pub runtime_functions: &'s [String],
    pub launch_contract_payload: &'s [u8],
    pub build_entry: Option<&'s str>,
    /// `None` when the checked access facts are unavailable.
    pub access: Option<&'s dyn CheckedAccessQuery>,
    pub loans_checked: bool,
    pub precompiled_exports: &'s [PrecompiledExport],
    /// Recursive carriers derived from exact checked rooted-view formations.
    pub slice_carriers: &'s [CarrierElement],
    /// Concrete expression types retained from this exact checked module.
    pub concrete_types: &'s [Type],
    pub task_awaits: &'s [TaskAwaitProof],
    pub task_await_items: &'s BTreeMap<u32, usize>,
    pub task_operand_items: &'s BTreeMap<u32, Vec<usize>>,
}

/// A borrowed, read-only view of one checked canonical source.
#[derive(Clone, Copy)]
pub struct CanonicalSourceView<'s> {
    module: &'s Module,
    type_definitions: &'s BTreeMap<String, TypeDef>,
    source_witness: &'s CheckedSourceWitness,
    closure_witness: &'s ClosureWitnessManifest,
    signatures: &'s BTreeMap<String, Type>,
    generic_instances: &'s BTreeMap<String, LogicalSpecializationIdentity>,
    abi_manifest: &'s CanonicalCallableAbiManifest,
    closed_abis: &'s BTreeMap<String, CanonicalCallableAbi>,
    runtime_functions: &'s [String],
    launch_contract_payload: &'s [u8],
    build_entry: Option<&'s str>,
    access: Option<&'s dyn CheckedAccessQuery>,
    loans_checked: bool,
    precompiled_exports: &'s [PrecompiledExport],
    slice_carriers: &'s [CarrierElement],
    concrete_types: &'s [Type],
    task_awaits: &'s [TaskAwaitProof],
    task_await_items: &'s BTreeMap<u32, usize>,
    task_operand_items: &'s BTreeMap<u32, Vec<usize>>,
}

impl<'s> CanonicalSourceView<'s> {
    /// Checked-frontend only: `CanonicalCheckedSource::view` is the single
    /// caller (pinned by an architecture test).
    #[doc(hidden)]
    pub fn from_checked_frontend(parts: CanonicalSourceParts<'s>) -> Self {
        let CanonicalSourceParts {
            module,
            type_definitions,
            source_witness,
            closure_witness,
            signatures,
            generic_instances,
            abi_manifest,
            closed_abis,
            runtime_functions,
            launch_contract_payload,
            build_entry,
            access,
            loans_checked,
            precompiled_exports,
            slice_carriers,
            concrete_types,
            task_awaits,
            task_await_items,
            task_operand_items,
        } = parts;
        Self {
            module,
            type_definitions,
            source_witness,
            closure_witness,
            signatures,
            generic_instances,
            abi_manifest,
            closed_abis,
            runtime_functions,
            launch_contract_payload,
            build_entry,
            access,
            loans_checked,
            precompiled_exports,
            slice_carriers,
            concrete_types,
            task_awaits,
            task_await_items,
            task_operand_items,
        }
    }

    /// The exact checked AST every other fact in this view is keyed on.
    pub fn module(&self) -> &'s Module { self.module }

    pub fn task_await_proofs(&self) -> &'s [TaskAwaitProof] { self.task_awaits }

    pub fn task_await_proof_at(&self, item: u32) -> Option<&'s TaskAwaitProof> {
        self.task_awaits.get(*self.task_await_items.get(&item)?)
    }

    /// Source-authenticated proofs for the executable task operands in this
    /// checked final function, including direct-return awaits.
    pub fn task_operand_proofs_at(&self, item: u32) -> impl Iterator<Item = &'s TaskAwaitProof> + '_ {
        self.task_operand_items.get(&item).into_iter().flatten()
            .filter_map(|proof| self.task_awaits.get(*proof))
    }

    /// Only a function occupying this ordinal in the checked module can
    /// authorize a body mode. Generated lambdas use their checked enclosing
    /// function's ordinal, so they cannot supply a forged owner marker.
    pub fn source_execution_mode_at(&self, ordinal: u32) -> Option<SourceExecutionMode> {
        match self.module.items.get(ordinal as usize)? {
            Item::Function(function) => Some(source_execution_mode(function, &self.module.modes)),
            _ => None,
        }
    }

    /// The declared types of the checked module, keyed by bare and
    /// entry-qualified name (see `aggregates::type_definitions`).
    pub fn type_definitions(&self) -> &'s BTreeMap<String, TypeDef> { self.type_definitions }

    /// A candidate slice family must match one of these exact checked
    /// formation payloads before its layout can be considered.
    pub fn admits_slice_carrier(&self, element: &CarrierElement) -> bool {
        self.slice_carriers.contains(element)
    }

    pub fn slice_carriers(&self) -> &'s [CarrierElement] { self.slice_carriers }

    /// Checked source types that can authorize a local physical ownership
    /// plan only after complete layout correspondence succeeds.
    pub fn concrete_types(&self) -> &'s [Type] { self.concrete_types }

    /// Classify one type using this exact checked module's definitions and
    /// nominal ordinals. The caller must still establish that the type comes
    /// from checked source and match its physical SafeWIR layout.
    pub fn storage_plan(&self, ty: &Type) -> StoragePlan {
        StoragePlan::classify(ty, self.type_definitions, |name| {
            self.module.items.iter().enumerate().find_map(|(ordinal, item)| {
                match item {
                    Item::Type(definition) if definition.name == name => u32::try_from(ordinal).ok(),
                    _ => None,
                }
            })
        })
    }

    /// The recursive checked ownership operation for a managed aggregate
    /// field. A candidate transfer must also match its physical layout and
    /// owner family before SafeWIR admits it.
    pub fn aggregate_managed_field_transfer(&self, ty: &Type) -> Option<ManagedAggregateFieldTransfer> {
        crate::storage::aggregate_managed_field_transfer(ty, self.type_definitions)
    }

    /// Checked closure captures and declared fields jointly determine which
    /// aggregate families need opaque GC ownership.
    pub fn opaque_owner_evidence(&self) -> Option<OpaqueOwnerEvidence<'s>> {
        OpaqueOwnerEvidence::from_checked_source(self.type_definitions, self.closure_witness)
    }

    pub fn source_witness(&self) -> &'s CheckedSourceWitness { self.source_witness }

    pub fn closure_witness(&self) -> &'s ClosureWitnessManifest { self.closure_witness }

    pub fn entry_declaration(&self) -> &'s str { self.build_entry.unwrap_or("main") }
    pub fn is_build_recipe(&self) -> bool { self.build_entry.is_some() }

    pub fn launch_contract_payload(&self) -> &'s [u8] { self.launch_contract_payload }

    /// The checked callable signature of a concrete named declaration.
    pub fn callable_signature(&self, name: &str) -> Option<&'s Type> { self.signatures.get(name) }

    pub fn generic_specialization(&self, name: &str) -> Option<&'s LogicalSpecializationIdentity> {
        self.generic_instances.get(name)
    }

    pub fn callable_abi_manifest(&self) -> &'s CanonicalCallableAbiManifest { self.abi_manifest }

    pub fn closed_callable_abi(&self, name: &str) -> Option<&'s CanonicalCallableAbi> {
        self.closed_abis.get(name)
    }

    pub fn closed_callable_signature(&self, ordinal: u32) -> Option<&'s Type> {
        let name = self.runtime_functions.get(ordinal as usize)?;
        self.signatures.get(name)
    }

    /// The exact retained source declaration for a closed callable. Unlike the
    /// checked callable signature, this preserves source loan spelling needed
    /// by independent ABI correspondence.
    pub fn closed_callable_declaration(&self, ordinal: u32) -> Option<&'s Function> {
        let name = self.runtime_functions.get(ordinal as usize)?;
        self.module.items.iter().find_map(|item| match item {
            Item::Function(function) if &function.name == name => Some(function),
            _ => None,
        })
    }

    /// The finalized access facts of [`Self::module`], if the checker produced
    /// them.
    pub fn access_facts(&self) -> Option<&'s dyn CheckedAccessQuery> { self.access }

    /// Whether the checked loan graph for [`Self::module`] is available.
    pub fn loans_checked(&self) -> bool { self.loans_checked }

    pub fn precompiled_exports(&self) -> &'s [PrecompiledExport] { self.precompiled_exports }
}
