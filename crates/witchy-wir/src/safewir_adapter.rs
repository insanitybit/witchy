//! Fail-closed mapping from final executable WIR to the declarative SafeWIR
//! candidate graph.
//!
//! This module is both the report boundary and the only production constructor
//! for final encoder authority. It supports only the void, parameter-rooted
//! semantic-memory subset implemented by the current validator, including
//! structured branches, blocks, loops, and void returns. Every unsupported
//! executable shape is rejected rather than omitted from the candidate graph.
//! Branch conditions remain limited to adapter-defined scalar constants; final
//! mutable-local reaching values still require the separate def-use track.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::layout::{LayoutBundle, LayoutInterner, LayoutSize};
use crate::safewir::{
    AuthenticatedReallocationTemplates, ByteRange, CallableMemoryContract, CallableMemoryResult, ContractExtent, ContractSizeExpr,
    DefinitionId, Extent, GenerationId, ObjectIdentity, ObjectRef, ParameterMemoryEntry, Place,
    Projection, ProjectionStep, Provenance, SizeExpr,
};
use crate::safewir_surface::{
    SafeCopyKind, SafeExtentExpr, SafePlaceExpr, SafeProjectionStep, SafeProvenanceExpr,
    SafeSizeExpr,
};
use crate::safewir_reaching_defs::{
    analyze_reaching_definitions, DefinitionKind as ReachingDefinitionKind, ReachingDefinitionBudget,
    structural_node_operands, ReachingDefinitionId, ReachingDefinitions, StructuralSite,
};
use crate::safewir_validate::{
    self, CandidateFunction, CandidateGcDeclarations, CandidateGcUse, CandidateModule, ControlId,
    CopyKind, HostArgument, MemoryWidth, Operation, ParameterBinding, PostValidationTransform,
    ValidationBudget, ValidationError, ValidationReport,
};
use crate::wir::{Kind, WirArrayDef, WirExpr, WirFunc, WirModule, WirNode, WirSeq, WirStructDef};

#[derive(Debug, Clone)]
pub struct FinalWirValidationInputs {
    pub layouts: LayoutBundle,
    /// Source-backed contracts for parameter- or result-bearing local
    /// functions. The adapter has not yet derived the facts needed to prove
    /// those shapes from executable WIR, so their declarations remain a
    /// separately checked input path.
    ///
    /// In contrast, a zero-parameter, void local helper has exactly one
    /// possible memory contract. It is mechanically derived from final WIR
    /// and must not appear here: supplying a duplicate declaration is
    /// rejected rather than treated as authority.
    pub functions: BTreeMap<String, CallableMemoryContract>,
    pub imports: BTreeMap<String, CallableMemoryContract>,
    pub host_imports: BTreeMap<String, CallableMemoryContract>,
    /// Opaque fixed-template certificates for coupled reallocation results.
    /// No issuer exists in the current stage, so the default empty catalog
    /// makes every such declared result reject before adaptation.
    pub reallocation_templates: AuthenticatedReallocationTemplates,
}

/// Final WIR and GC declarations authorized for the mechanical encoder.
///
/// The fields are private and this type is not `Clone`: downstream code cannot
/// separate authority from the exact module and GC declaration vectors checked
/// at construction. The production encoder consumes the value before applying
/// the one permitted post-validation transform.
///
/// ```compile_fail,E0451
/// use witchy_wir::safewir_adapter::ValidatedWirModule;
/// use witchy_wir::wir::WirModule;
///
/// fn forge(
///     module: WirModule,
///     report: witchy_wir::safewir_validate::ValidationReport,
/// ) -> ValidatedWirModule {
///     ValidatedWirModule {
///         module,
///         gc_structs: Vec::new(),
///         gc_arrays: Vec::new(),
///         report,
///     }
/// }
/// ```
///
/// ```compile_fail,E0616
/// use witchy_wir::safewir_adapter::ValidatedWirModule;
/// use witchy_wir::wir::WirStructDef;
///
/// fn substitute_after_validation(
///     mut validated: ValidatedWirModule,
///     forged: Vec<WirStructDef>,
/// ) -> ValidatedWirModule {
///     validated.gc_structs = forged;
///     validated
/// }
/// ```
pub struct ValidatedWirModule {
    module: WirModule,
    gc_structs: Vec<WirStructDef>,
    gc_arrays: Vec<WirArrayDef>,
    report: ValidationReport,
}

impl ValidatedWirModule {
    /// Metrics from validation of the exact owned module.
    pub const fn report(&self) -> &ValidationReport {
        &self.report
    }

    pub(crate) fn into_encoding_parts(
        self,
    ) -> (WirModule, Vec<WirStructDef>, Vec<WirArrayDef>) {
        (self.module, self.gc_structs, self.gc_arrays)
    }
}

#[derive(Debug)]
pub enum FinalWirValidationError {
    Adapter {
        function: Option<String>,
        message: String,
    },
    Validation(ValidationError),
}

impl FinalWirValidationError {
    fn module(message: impl Into<String>) -> Self {
        Self::Adapter { function: None, message: message.into() }
    }

    fn function(function: &str, message: impl Into<String>) -> Self {
        Self::Adapter { function: Some(function.into()), message: message.into() }
    }
}

impl fmt::Display for FinalWirValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Adapter { function: Some(function), message } => {
                write!(formatter, "unsupported final WIR in `{function}`: {message}")
            }
            Self::Adapter { function: None, message } => {
                write!(formatter, "unsupported final WIR module: {message}")
            }
            Self::Validation(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for FinalWirValidationError {}

impl From<ValidationError> for FinalWirValidationError {
    fn from(error: ValidationError) -> Self {
        Self::Validation(error)
    }
}

/// Validate final executable WIR and bind successful authority to the exact
/// module and GC declarations that the production encoder will consume.
///
/// There is intentionally no constructor on [`ValidatedWirModule`]. This
/// function is the only production construction path and delegates to the same
/// deterministic, budgeted adapter and validator as report mode.
pub fn validate_final_wir_module(
    mut module: WirModule,
    gc_structs: Vec<WirStructDef>,
    gc_arrays: Vec<WirArrayDef>,
    inputs: FinalWirValidationInputs,
    budget: ValidationBudget,
) -> Result<ValidatedWirModule, FinalWirValidationError> {
    // Local pruning is part of final candidate preparation, not encoding. The
    // authority therefore binds the exact pruned module that is validated and
    // later encoded, while retaining the historical mechanical byte shape.
    module.prune_unused_locals();
    let report = validate_final_wir_report(&module, &gc_structs, &gc_arrays, inputs, budget)?;
    Ok(ValidatedWirModule { module, gc_structs, gc_arrays, report })
}

/// Validate the final optimized executable WIR in report mode.
///
/// Success is diagnostic evidence only. It deliberately does not expose a
/// `ValidatedWirModule` or any encoder entrypoint.
pub fn validate_final_wir_report(
    module: &WirModule,
    gc_structs: &[WirStructDef],
    gc_arrays: &[WirArrayDef],
    inputs: FinalWirValidationInputs,
    budget: ValidationBudget,
) -> Result<ValidationReport, FinalWirValidationError> {
    let function_contracts = derive_final_function_contracts(module, &inputs.functions)?;
    let scalar_result_signatures = module
        .funcs
        .iter()
        .map(|function| {
            let results = function
                .ret
                .iter()
                .map(|result| {
                    if !is_scalar_result_type(result) {
                        return Err(FinalWirValidationError::function(
                            &function.name,
                            "value-returning callables require the deferred return-value def-use adapter",
                        ));
                    }
                    Ok(result.kind())
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok((function.name.clone(), results))
        })
        .collect::<Result<BTreeMap<_, _>, FinalWirValidationError>>()?;
    let declared_imports = module
        .imports
        .iter()
        .map(|import| import.name.as_str())
        .collect::<BTreeSet<_>>();
    let contracted_imports = inputs
        .imports
        .keys()
        .chain(inputs.host_imports.keys())
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    if declared_imports.len() != module.imports.len()
        || inputs
            .imports
            .keys()
            .any(|name| inputs.host_imports.contains_key(name))
        || declared_imports != contracted_imports
    {
        return Err(FinalWirValidationError::module(
            "checked import contracts must name every and only final import exactly once",
        ));
    }
    if inputs
        .functions
        .values()
        .chain(inputs.imports.values())
        .chain(inputs.host_imports.values())
        .any(|contract| !matches!(contract.result, CallableMemoryResult::NoMemory))
    {
        return Err(FinalWirValidationError::module(
            "callable object results require the deferred result-definition adapter",
        ));
    }
    authenticate_final_host_imports(module, &inputs)?;

    let functions = module
        .funcs
        .iter()
        .map(|function| {
            // This graph is deliberately produced before semantic-pattern
            // elaboration. It assigns identities from WIR structure, checks
            // every local read and structured edge, and reaches a bounded loop
            // fixpoint without trusting an adapter-maintained "current local"
            // table. Pattern handlers may only bind a scalar, range, object,
            // or callable fact to a definition supplied by this graph.
            //
            // The current narrow adapter still rejects mutable locals and the
            // patterns that need them. Running the common analysis here makes
            // that refusal a coverage limitation, never permission to guess a
            // reaching definition while the production elaborator expands.
            let reaching = analyze_reaching_definitions(function, ReachingDefinitionBudget::default())
                .map_err(|error| FinalWirValidationError::function(
                    &function.name,
                    format!("reaching-definition preflight rejected final WIR: {error}"),
                ))?;
            let contract = function_contracts
                .get(&function.name)
                .expect("derived and declared function contract census was checked")
                .clone();
            FunctionAdapter::new(
                function,
                contract,
                reaching,
                &inputs.layouts,
                &function_contracts,
                &scalar_result_signatures,
            )?.adapt()
        })
        .collect::<Result<Vec<_>, _>>()?;
    let candidate = CandidateModule {
        checked_source_entries: 1,
        layouts: inputs.layouts,
        imports: inputs.imports,
        host_imports: inputs.host_imports,
        reallocation_templates: inputs.reallocation_templates,
        functions,
        gc: CandidateGcDeclarations {
            structs: gc_structs.to_vec(),
            arrays: gc_arrays.to_vec(),
            uses: collect_encoded_gc_uses(module),
        },
        contains_raw_memory: false,
        contains_raw_body: false,
        post_validation_transform: PostValidationTransform::FixedTemplateExpansionAndErasure,
    };
    Ok(safewir_validate::validate_candidate(&candidate, budget)?)
}

fn is_scalar_result_type(ty: &crate::wir::WirTy) -> bool {
    matches!(
        ty,
        crate::wir::WirTy::Int
            | crate::wir::WirTy::Float
            | crate::wir::WirTy::Bool
            | crate::wir::WirTy::V128
            | crate::wir::WirTy::Slot
    )
}

/// Derive the only callable contract which final WIR determines without a
/// source-range or result-definition elaborator: a zero-argument, void local
/// helper is memory-free. Its supplied declaration is therefore not an input
/// to trust. Parameter-bearing functions remain on the explicitly distinct
/// source-backed path until their object/scalar roots, layouts, extents, and
/// return definitions can be derived from final WIR.
fn derive_final_function_contracts(
    module: &WirModule,
    declared: &BTreeMap<String, CallableMemoryContract>,
) -> Result<BTreeMap<String, CallableMemoryContract>, FinalWirValidationError> {
    let mut contracts = BTreeMap::new();
    for function in &module.funcs {
        let helper_only = function.params.is_empty() && function.ret.is_empty();
        let contract = if helper_only {
            if declared.contains_key(&function.name) {
                return Err(FinalWirValidationError::function(
                    &function.name,
                    "zero-argument void helper contract is derived from final WIR; supplied declaration is forbidden",
                ));
            }
            CallableMemoryContract {
                parameters: Vec::new(),
                result: CallableMemoryResult::NoMemory,
            }
        } else {
            declared.get(&function.name).cloned().ok_or_else(|| {
                FinalWirValidationError::function(
                    &function.name,
                    "parameter- or result-bearing function requires its separately checked source-backed callable contract",
                )
            })?
        };
        if contracts.insert(function.name.clone(), contract).is_some() {
            return Err(FinalWirValidationError::module(format!(
                "final WIR contains duplicate function `{}`",
                function.name
            )));
        }
    }
    if declared.keys().any(|name| !contracts.contains_key(name)) {
        return Err(FinalWirValidationError::module(
            "source-backed callable contracts must name only final parameter- or result-bearing functions",
        ));
    }
    Ok(contracts)
}

/// Bind each production host contract to the same canonical ABI entry that
/// declares its Wasm import. A familiar string alone is not authority: both
/// its exact signature and no-memory contract must be mechanically derived.
/// Descriptor-bearing buffer imports remain rejected until their dedicated
/// SafeWIR contract elaborator is available.
fn authenticate_final_host_imports(
    module: &WirModule,
    inputs: &FinalWirValidationInputs,
) -> Result<(), FinalWirValidationError> {
    for import in &module.imports {
        let Some(contract) = inputs.host_imports.get(&import.name) else {
            continue;
        };
        let Some(canonical) = crate::safewir_call::canonical_host_import_contract(&import.name)
        else {
            return Err(FinalWirValidationError::module(format!(
                "host import `{}` has no canonical ABI memory contract",
                import.name
            )));
        };
        let Some(abi) = crate::wir_prelude::prelude()
            .imports
            .iter()
            .find(|candidate| candidate.name == import.name)
        else {
            return Err(FinalWirValidationError::module(format!(
                "host import `{}` is absent from the canonical ABI table",
                import.name
            )));
        };
        let params = abi.params.iter().copied().map(wasm_kind).collect::<Vec<_>>();
        let results = abi.results.iter().copied().map(wasm_kind).collect::<Vec<_>>();
        if import.params != params || import.results != results {
            return Err(FinalWirValidationError::module(format!(
                "host import `{}` contradicts its canonical ABI signature",
                import.name
            )));
        }
        if contract != &canonical {
            return Err(FinalWirValidationError::module(format!(
                "host import `{}` contract is not the canonical no-memory ABI contract",
                import.name
            )));
        }
    }
    Ok(())
}

fn wasm_kind(kind: crate::wir_prelude::WasmTy) -> Kind {
    match kind {
        crate::wir_prelude::WasmTy::I32 => Kind::I32,
        crate::wir_prelude::WasmTy::I64 => Kind::I64,
        crate::wir_prelude::WasmTy::F32 | crate::wir_prelude::WasmTy::F64 => Kind::F64,
        crate::wir_prelude::WasmTy::ExternRef => Kind::ExternRef,
    }
}

fn collect_encoded_gc_uses(module: &WirModule) -> Vec<CandidateGcUse> {
    let mut uses = Vec::new();
    for import in &module.imports {
        collect_kinds(&import.params, &mut uses);
        collect_kinds(&import.results, &mut uses);
    }
    for global in &module.globals {
        collect_kind(global.kind, &mut uses);
    }
    for function in &module.funcs {
        for local in function.params.iter().chain(&function.locals) {
            collect_kind(local.ty.kind(), &mut uses);
        }
        for result in &function.ret {
            collect_kind(result.kind(), &mut uses);
        }
        collect_gc_uses_seq(&function.body, &mut uses);
    }
    uses
}

fn collect_kinds(kinds: &[Kind], uses: &mut Vec<CandidateGcUse>) {
    for kind in kinds {
        collect_kind(*kind, uses);
    }
}

fn collect_kind(kind: Kind, uses: &mut Vec<CandidateGcUse>) {
    if let Kind::GcRef(id) = kind {
        uses.push(CandidateGcUse::ConcreteType(id));
    }
}

fn collect_gc_uses_seq(nodes: &WirSeq, uses: &mut Vec<CandidateGcUse>) {
    for node in nodes {
        collect_gc_uses_node(node, uses);
    }
}

fn collect_gc_uses_node(node: &WirNode, uses: &mut Vec<CandidateGcUse>) {
    match node {
        WirNode::Source { body, .. } | WirNode::Loop { body, .. } => {
            collect_gc_uses_seq(body, uses);
        }
        WirNode::Block { result, body, .. } => {
            if let Some(result) = result {
                collect_kind(result.kind(), uses);
            }
            collect_gc_uses_seq(body, uses);
        }
        WirNode::SetLocal { value, .. }
        | WirNode::SetGlobal { value, .. }
        | WirNode::Drop(value)
        | WirNode::Do(value)
        | WirNode::Push(value) => collect_gc_uses_expr(value, uses),
        WirNode::Store { ptr, value, kind, .. } => {
            collect_kind(*kind, uses);
            collect_gc_uses_expr(ptr, uses);
            collect_gc_uses_expr(value, uses);
        }
        // A kernel operation carries ordinary WIR operands; a GC reference can
        // reach one, so traverse them rather than treating the node as opaque.
        WirNode::SafeKernel(operation) => {
            operation.for_each_wir_expr(|expression| collect_gc_uses_expr(expression, uses));
        }
        WirNode::SafeStore { place, value, kind } => {
            collect_kind(*kind, uses);
            collect_gc_uses_expr(&place.encoded_base, uses);
            collect_gc_uses_expr(value, uses);
        }
        WirNode::CallStoreMulti { args, .. } => {
            collect_gc_uses_exprs(args, uses);
        }
        WirNode::CallIndirectStoreMulti { signature, args, index, .. } => {
            collect_kinds(&signature.params, uses);
            collect_kinds(&signature.results, uses);
            collect_gc_uses_exprs(args, uses);
            collect_gc_uses_expr(index, uses);
        }
        WirNode::MemoryCopy { dest, src, len } => {
            collect_gc_uses_expr(dest, uses);
            collect_gc_uses_expr(src, uses);
            collect_gc_uses_expr(len, uses);
        }
        WirNode::SafeMemoryCopy { destination, source, len, .. } => {
            collect_gc_uses_expr(&destination.encoded_base, uses);
            collect_gc_uses_expr(&source.encoded_base, uses);
            collect_gc_uses_expr(len, uses);
        }
        WirNode::MemoryFill { dest, value, len } => {
            collect_gc_uses_expr(dest, uses);
            collect_gc_uses_expr(value, uses);
            collect_gc_uses_expr(len, uses);
        }
        WirNode::SafeMemoryFill { destination, value, len } => {
            collect_gc_uses_expr(&destination.encoded_base, uses);
            collect_gc_uses_expr(value, uses);
            collect_gc_uses_expr(len, uses);
        }
        WirNode::Store8 { ptr, value, .. } => {
            collect_gc_uses_expr(ptr, uses);
            collect_gc_uses_expr(value, uses);
        }
        WirNode::SafeStore8 { place, value } => {
            collect_gc_uses_expr(&place.encoded_base, uses);
            collect_gc_uses_expr(value, uses);
        }
        WirNode::If { cond, then_, els, result } => {
            collect_gc_uses_expr(cond, uses);
            if let Some(result) = result {
                collect_kind(result.kind(), uses);
            }
            collect_gc_uses_seq(then_, uses);
            collect_gc_uses_seq(els, uses);
        }
        WirNode::Br { cond, .. } | WirNode::Return(cond) => {
            if let Some(cond) = cond {
                collect_gc_uses_expr(cond, uses);
            }
        }
        WirNode::Unreachable => {}
        WirNode::StructSet { struct_id, field, base, value } => {
            uses.push(CandidateGcUse::StructField {
                struct_id: *struct_id,
                field: *field,
                write: true,
            });
            collect_gc_uses_expr(base, uses);
            collect_gc_uses_expr(value, uses);
        }
        WirNode::ArraySet { array_id, array, index, value } => {
            uses.push(CandidateGcUse::Array { array_id: *array_id, fixed_length: None });
            collect_gc_uses_expr(array, uses);
            collect_gc_uses_expr(index, uses);
            collect_gc_uses_expr(value, uses);
        }
    }
}

fn collect_gc_uses_exprs(expressions: &[WirExpr], uses: &mut Vec<CandidateGcUse>) {
    for expression in expressions {
        collect_gc_uses_expr(expression, uses);
    }
}

fn collect_gc_uses_expr(expression: &WirExpr, uses: &mut Vec<CandidateGcUse>) {
    match expression {
        WirExpr::ConstI64(_)
        | WirExpr::ConstF64(_)
        | WirExpr::ConstI32(_)
        | WirExpr::ConstV128(_)
        | WirExpr::StrPtr(_)
        | WirExpr::GetLocal(_)
        | WirExpr::GetGlobal(_)
        | WirExpr::MemorySize => {}
        WirExpr::Vector { args, .. }
        | WirExpr::Call { args, .. }
        | WirExpr::CallHost { args, .. } => collect_gc_uses_exprs(args, uses),
        WirExpr::ToSlot(value, kind) | WirExpr::FromSlot(value, kind) => {
            collect_kind(*kind, uses);
            collect_gc_uses_expr(value, uses);
        }
        WirExpr::Binary { kind, lhs, rhs, .. } => {
            collect_kind(*kind, uses);
            collect_gc_uses_expr(lhs, uses);
            collect_gc_uses_expr(rhs, uses);
        }
        WirExpr::Unary { kind, arg, .. } => {
            collect_kind(*kind, uses);
            collect_gc_uses_expr(arg, uses);
        }
        WirExpr::Convert { from, to, arg } => {
            collect_kind(*from, uses);
            collect_kind(*to, uses);
            collect_gc_uses_expr(arg, uses);
        }
        WirExpr::Load { ptr, kind, .. } => {
            collect_kind(*kind, uses);
            collect_gc_uses_expr(ptr, uses);
        }
        // Kernel operands are ordinary WIR; traverse them for GC uses.
        WirExpr::SafeKernel(operation) => {
            operation.for_each_wir_expr(|expression| collect_gc_uses_expr(expression, uses));
        }
        WirExpr::SafeLoad { place, kind } => {
            collect_kind(*kind, uses);
            collect_gc_uses_expr(&place.encoded_base, uses);
        }
        WirExpr::MemoryGrow(value)
        | WirExpr::ArrayLen(value)
        | WirExpr::RefIsNull(value) => collect_gc_uses_expr(value, uses),
        WirExpr::Load8U { ptr, .. } => collect_gc_uses_expr(ptr, uses),
        WirExpr::SafeLoad8U { place } => collect_gc_uses_expr(&place.encoded_base, uses),
        WirExpr::CallIndirect { signature, args, index } => {
            collect_kinds(&signature.params, uses);
            collect_kinds(&signature.results, uses);
            collect_gc_uses_exprs(args, uses);
            collect_gc_uses_expr(index, uses);
        }
        WirExpr::Control(node) => collect_gc_uses_node(node, uses),
        WirExpr::Seq(nodes) => collect_gc_uses_seq(nodes, uses),
        WirExpr::StructNew { struct_id, args } => {
            uses.push(CandidateGcUse::StructNew {
                struct_id: *struct_id,
                field_count: args.len(),
            });
            collect_gc_uses_exprs(args, uses);
        }
        WirExpr::StructGet { struct_id, field, base } => {
            uses.push(CandidateGcUse::StructField {
                struct_id: *struct_id,
                field: *field,
                write: false,
            });
            collect_gc_uses_expr(base, uses);
        }
        WirExpr::ArrayNew { array_id, value, len } => {
            uses.push(CandidateGcUse::Array { array_id: *array_id, fixed_length: None });
            collect_gc_uses_expr(value, uses);
            collect_gc_uses_expr(len, uses);
        }
        WirExpr::ArrayNewFixed { array_id, items } => {
            uses.push(CandidateGcUse::Array {
                array_id: *array_id,
                fixed_length: Some(items.len()),
            });
            collect_gc_uses_exprs(items, uses);
        }
        WirExpr::ArrayGet { array_id, array, index } => {
            uses.push(CandidateGcUse::Array { array_id: *array_id, fixed_length: None });
            collect_gc_uses_expr(array, uses);
            collect_gc_uses_expr(index, uses);
        }
        WirExpr::RefCast { struct_id, value }
        | WirExpr::RefCastNullable { struct_id, value } => {
            uses.push(CandidateGcUse::StructCast(*struct_id));
            collect_gc_uses_expr(value, uses);
        }
        WirExpr::RefNull(kind) => uses.push(CandidateGcUse::RefNull(*kind)),
    }
}

struct FunctionAdapter<'a> {
    function: &'a WirFunc,
    layouts: LayoutInterner,
    contract: CallableMemoryContract,
    function_contracts: &'a BTreeMap<String, CallableMemoryContract>,
    scalar_result_signatures: &'a BTreeMap<String, Vec<Kind>>,
    reaching: ReachingDefinitions,
    next_definition: u32,
    next_control: u32,
    local_names: BTreeMap<String, LocalId>,
    locals: Vec<LocalBinding>,
    controls: Vec<(String, ControlId)>,
    parameters: Vec<ParameterBinding>,
    operations: Vec<Operation>,
    reaching_scalar_definitions: BTreeMap<ReachingDefinitionId, DefinitionId>,
    active_index_guard: Option<(DefinitionId, DefinitionId)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct LocalId(u32);

#[derive(Debug, Clone, Copy)]
struct LocalBinding {
    definition: DefinitionId,
    memory_root: bool,
}

impl<'a> FunctionAdapter<'a> {
    fn new(
        function: &'a WirFunc,
        contract: CallableMemoryContract,
        reaching: ReachingDefinitions,
        layouts: &LayoutBundle,
        function_contracts: &'a BTreeMap<String, CallableMemoryContract>,
        scalar_result_signatures: &'a BTreeMap<String, Vec<Kind>>,
    ) -> Result<Self, FinalWirValidationError> {
        if function.raw_body.is_some() {
            return Err(FinalWirValidationError::function(
                &function.name,
                "raw function bodies cannot cross the SafeWIR boundary",
            ));
        }
        if function.params.len() != contract.parameters.len() {
            return Err(FinalWirValidationError::function(
                &function.name,
                "WIR parameters contradict the checked callable contract",
            ));
        }
        if !matches!(contract.result, CallableMemoryResult::NoMemory) {
            return Err(FinalWirValidationError::function(
                &function.name,
                "callable object results require the deferred result-definition adapter",
            ));
        }
        let (_, layouts) = LayoutBundle::decode_canonical(&layouts.canonical_bytes()).map_err(|error| {
            FinalWirValidationError::function(
                &function.name,
                format!("invalid canonical layout bundle: {error}"),
            )
        })?;
        let mut adapter = Self {
            function,
            layouts,
            contract,
            function_contracts,
            scalar_result_signatures,
            reaching,
            next_definition: 1,
            next_control: 1,
            local_names: BTreeMap::new(),
            locals: Vec::new(),
            controls: Vec::new(),
            parameters: Vec::new(),
            operations: Vec::new(),
            reaching_scalar_definitions: BTreeMap::new(),
            active_index_guard: None,
        };
        let parameter_contracts = adapter.contract.parameters.clone();
        let parameter_definitions = (0..function.params.len())
            .map(|_| adapter.fresh_definition())
            .collect::<Result<Vec<_>, _>>()?;
        for ((parameter, memory), definition) in function
            .params
            .iter()
            .zip(&parameter_contracts)
            .zip(parameter_definitions.iter().copied())
        {
            let object = match memory.entry {
                ParameterMemoryEntry::NoMemory => None,
                ParameterMemoryEntry::Unknown => {
                    return Err(adapter.error(
                        "unknown parameter memory shape is outside the checked adapter subset",
                    ));
                }
                ParameterMemoryEntry::BorrowedRead
                | ParameterMemoryEntry::BorrowedWrite
                | ParameterMemoryEntry::Shared
                | ParameterMemoryEntry::Unique => {
                    if parameter.ty.kind() != Kind::I32 {
                        return Err(adapter.error(
                            "memory-bearing parameters require an i32 object-root representation",
                        ));
                    }
                    let layout = memory
                        .layout
                        .ok_or_else(|| adapter.error("memory parameter has no LayoutId"))?;
                    let extent = adapter.contract_extent(memory.extent.as_ref().ok_or_else(|| {
                        adapter.error("memory parameter has no authenticated extent")
                    })?, &parameter_definitions)?;
                    Some(ObjectRef {
                        identity: ObjectIdentity {
                            root: definition,
                            generation: GenerationId::new(1),
                        },
                        layout,
                        extent,
                        provenance: Provenance::Unknown,
                    })
                }
            };
            adapter.declare_local(&parameter.name, definition, object.is_some())?;
            adapter.parameters.push(ParameterBinding { definition, object });
        }
        for (index, definition) in parameter_definitions.iter().copied().enumerate() {
            if adapter.count_parameter_is_authenticated(index) {
                adapter.operations.push(Operation::BindCountParameter { definition, parameter: index });
            }
        }
        for local in &function.locals {
            let definition = adapter.fresh_definition()?;
            adapter.declare_local(&local.name, definition, false)?;
        }
        Ok(adapter)
    }

    fn adapt(mut self) -> Result<CandidateFunction, FinalWirValidationError> {
        let body = self.function.body.clone();
        self.seq_at(&body, &StructuralSite::root())?;
        Ok(CandidateFunction {
            name: self.function.name.clone(),
            contract: self.contract,
            scalar_results: self.function.ret.iter().map(crate::wir::WirTy::kind).collect(),
            parameters: self.parameters,
            operations: self.operations,
        })
    }

    fn fresh_definition(&mut self) -> Result<DefinitionId, FinalWirValidationError> {
        let definition = DefinitionId::new(self.next_definition);
        self.next_definition = self
            .next_definition
            .checked_add(1)
            .ok_or_else(|| self.error("DefinitionId space exhausted"))?;
        Ok(definition)
    }

    fn fresh_control(&mut self) -> Result<ControlId, FinalWirValidationError> {
        let control = ControlId::new(self.next_control);
        self.next_control = self
            .next_control
            .checked_add(1)
            .ok_or_else(|| self.error("ControlId space exhausted"))?;
        Ok(control)
    }

    fn push_control(&mut self, label: &str) -> Result<ControlId, FinalWirValidationError> {
        if self.controls.iter().any(|(active, _)| active == label) {
            return Err(self.error(format!(
                "control label `{label}` shadows a lexically enclosing label"
            )));
        }
        let control = self.fresh_control()?;
        self.controls.push((label.into(), control));
        Ok(control)
    }

    fn resolve_control(&self, label: &str) -> Result<ControlId, FinalWirValidationError> {
        self.controls
            .iter()
            .rev()
            .find_map(|(active, control)| (active == label).then_some(*control))
            .ok_or_else(|| self.error(format!(
                "branch target `{label}` is not a lexically enclosing control"
            )))
    }

    fn declare_local(
        &mut self,
        name: &str,
        definition: DefinitionId,
        memory_root: bool,
    ) -> Result<(), FinalWirValidationError> {
        let index = u32::try_from(self.locals.len())
            .map_err(|_| self.error("local index space exhausted"))?;
        let local = LocalId(index);
        if self.local_names.insert(name.into(), local).is_some() {
            return Err(self.error("parameter and local names must be unique"));
        }
        self.locals.push(LocalBinding { definition, memory_root });
        Ok(())
    }

    fn local(&self, name: &str) -> Result<LocalBinding, FinalWirValidationError> {
        let local = self
            .local_names
            .get(name)
            .ok_or_else(|| self.error(format!("use of undeclared local `{name}`")))?;
        self.locals
            .get(local.0 as usize)
            .copied()
            .ok_or_else(|| self.error("resolved local index is outside the local table"))
    }

    fn count_parameter_is_authenticated(&self, parameter: usize) -> bool {
        self.contract.parameters.iter().any(|memory| {
            matches!(
                (&memory.extent, &memory.dynamic_header),
                (
                    Some(ContractExtent::Counted { count: ContractSizeExpr::Parameter(index), .. }),
                    Some(crate::safewir::DynamicHeaderInvariant::BoundedByCountedExtent { component: 0 }),
                ) if *index == parameter
            )
        })
    }

    fn contract_extent(
        &self,
        extent: &ContractExtent,
        parameters: &[DefinitionId],
    ) -> Result<Extent, FinalWirValidationError> {
        match extent {
            ContractExtent::Exact(size) => Ok(Extent::Exact(contract_size(size)?)),
            ContractExtent::Counted { header, count, stride } => {
                let ContractSizeExpr::Parameter(index) = count else {
                    return Err(self.error("counted parameter extent requires one source parameter count"));
                };
                if !self.count_parameter_is_authenticated(*index) {
                    return Err(self.error("counted parameter extent has no authenticated header-capacity invariant"));
                }
                let definition = parameters.get(*index).copied().ok_or_else(|| {
                    self.error("counted parameter extent names a parameter outside the callable")
                })?;
                Ok(Extent::Counted {
                    header: contract_size(header)?,
                    count: definition,
                    stride: contract_size(stride)?,
                })
            }
        }
    }

    /// Bind an executable scalar read to its parameter definition only when
    /// the shared structural graph proves that every reachable read and claim
    /// of that name still reaches the same parameter. This deliberately does
    /// not use a mutable "last assignment" map: writes, joins, calls, and
    /// loop-carried values stay unsupported until their exact graph binding is
    /// elaborated.
    fn stable_scalar_parameter_definition(
        &self,
        name: &str,
        role: &'static str,
    ) -> Result<DefinitionId, FinalWirValidationError> {
        let local = self.local(name)?;
        if local.memory_root {
            return Err(self.error(format!(
                "{role} names a memory root instead of a scalar reaching definition"
            )));
        }
        let Some(parameter_index) = self
            .function
            .params
            .iter()
            .position(|parameter| parameter.name == name)
        else {
            return Err(self.error(format!(
                "{role} requires exact reaching definitions; local `{name}` is not an unchanged scalar parameter"
            )));
        };
        let parameter_index = u32::try_from(parameter_index)
            .map_err(|_| self.error("parameter index space exhausted"))?;
        let Some(entry) = self.reaching.entry_environment().get(name) else {
            return Err(self.error(format!(
                "{role} requires exact reaching definitions; parameter `{name}` has no entry definition"
            )));
        };
        let mut definitions = entry.iter();
        let Some(reaching_definition) = definitions.next() else {
            return Err(self.error(format!(
                "{role} requires exact reaching definitions; parameter `{name}` has no entry definition"
            )));
        };
        if definitions.next().is_some()
            || !matches!(
                self.reaching.definition(reaching_definition).map(|definition| &definition.kind),
                Some(ReachingDefinitionKind::Parameter { local, index })
                    if local == name && *index == parameter_index
            )
        {
            return Err(self.error(format!(
                "{role} requires exact reaching definitions; parameter `{name}` has an invalid entry binding"
            )));
        }
        let stable_read = |reaching: &crate::safewir_reaching_defs::ReachingSet, reachable| {
            reachable && reaching.len() == 1 && reaching.contains(reaching_definition)
        };
        if self
            .reaching
            .reads_of(name)
            .any(|read| !stable_read(&read.reaching, read.reachable))
            || self
                .reaching
                .claim_local_uses_of(name)
                .any(|claim| !stable_read(&claim.reaching, claim.reachable))
        {
            return Err(self.error(format!(
                "{role} requires exact reaching definitions; parameter `{name}` is reassigned or joined"
            )));
        }
        Ok(local.definition)
    }

    /// Bind an executable object argument to its parameter root only when the
    /// reaching-definition graph proves the parameter was never replaced or
    /// joined.  A plain i32 is not object authority: this is deliberately
    /// available only for locals the checked caller contract established as
    /// memory roots.
    fn stable_object_parameter_identity(
        &self,
        name: &str,
        role: &'static str,
    ) -> Result<ObjectIdentity, FinalWirValidationError> {
        let local = self.local(name)?;
        if !local.memory_root {
            return Err(self.error(format!(
                "{role} names a scalar instead of an authenticated memory root"
            )));
        }
        let Some(parameter_index) = self
            .function
            .params
            .iter()
            .position(|parameter| parameter.name == name)
        else {
            return Err(self.error(format!(
                "{role} requires an authenticated parameter root; local `{name}` is not a parameter"
            )));
        };
        let parameter_index = u32::try_from(parameter_index)
            .map_err(|_| self.error("parameter index space exhausted"))?;
        let Some(entry) = self.reaching.entry_environment().get(name) else {
            return Err(self.error(format!(
                "{role} requires exact reaching definitions; parameter `{name}` has no entry definition"
            )));
        };
        let mut definitions = entry.iter();
        let Some(reaching_definition) = definitions.next() else {
            return Err(self.error(format!(
                "{role} requires exact reaching definitions; parameter `{name}` has no entry definition"
            )));
        };
        if definitions.next().is_some()
            || !matches!(
                self.reaching.definition(reaching_definition).map(|definition| &definition.kind),
                Some(ReachingDefinitionKind::Parameter { local, index })
                    if local == name && *index == parameter_index
            )
        {
            return Err(self.error(format!(
                "{role} requires exact reaching definitions; parameter `{name}` has an invalid entry binding"
            )));
        }
        let stable_read = |reaching: &crate::safewir_reaching_defs::ReachingSet, reachable| {
            reachable && reaching.len() == 1 && reaching.contains(reaching_definition)
        };
        if self
            .reaching
            .reads_of(name)
            .any(|read| !stable_read(&read.reaching, read.reachable))
            || self
                .reaching
                .claim_local_uses_of(name)
                .any(|claim| !stable_read(&claim.reaching, claim.reachable))
        {
            return Err(self.error(format!(
                "{role} requires exact reaching definitions; parameter `{name}` is reassigned or joined"
            )));
        }
        Ok(ObjectIdentity { root: local.definition, generation: GenerationId::new(1) })
    }

    /// The only direct-call operands the current final-WIR adapter can bind:
    /// literals and exact scalar parameters, plus exact parameter roots whose
    /// object shape came from the caller's checked contract.  In particular,
    /// arithmetic, conversions, locals written by a call, and raw pointer
    /// expressions cannot cross this boundary as callable arguments.
    fn direct_call_argument(
        &mut self,
        expression: &WirExpr,
    ) -> Result<crate::safewir_validate::CallArgument, FinalWirValidationError> {
        use crate::safewir_validate::CallArgument;

        match expression {
            WirExpr::ConstI32(value) => Ok(CallArgument::Scalar(self.constant_definition(i128::from(*value))?)),
            WirExpr::ConstI64(value) => Ok(CallArgument::Scalar(self.constant_definition(i128::from(*value))?)),
            WirExpr::GetLocal(name) if self.local(name)?.memory_root => Ok(CallArgument::Object(
                self.stable_object_parameter_identity(name, "direct call argument")?,
            )),
            WirExpr::GetLocal(name) => Ok(CallArgument::Scalar(
                self.stable_scalar_parameter_definition(name, "direct call argument")?,
            )),
            _ => Err(self.error(
                "direct call arguments require a literal, stable scalar parameter, or authenticated parameter root",
            )),
        }
    }

    /// Elaborate a statement-position direct local call.  Results are not
    /// represented by this stage: calls in value position and multi-result
    /// calls stay fail-closed until their result def-use adapter exists.
    fn direct_void_call(
        &mut self,
        func: &str,
        args: &[WirExpr],
    ) -> Result<(), FinalWirValidationError> {
        let arguments = args
            .iter()
            .map(|argument| self.direct_call_argument(argument))
            .collect::<Result<Vec<_>, _>>()?;
        self.operations.push(Operation::Call {
            callable: func.into(),
            arguments,
            result: None,
        });
        Ok(())
    }

    /// Elaborate a direct multi-result call only when every result is a
    /// mechanically typed scalar and the callee has a checked no-memory
    /// contract. Results receive fresh scalar DefinitionIds tied to the
    /// shared reaching-definition graph; they never become object roots.
    fn direct_scalar_call_results(
        &mut self,
        callable: &str,
        args: &[WirExpr],
        destinations: &[String],
        site: &StructuralSite,
    ) -> Result<(), FinalWirValidationError> {
        if destinations.is_empty() {
            return Err(self.error("multi-result call must bind at least one scalar result"));
        }
        let contract = self.function_contracts.get(callable).ok_or_else(|| {
            self.error(format!(
                "multi-result call `{callable}` requires one checked direct local callee contract"
            ))
        })?;
        if !matches!(contract.result, CallableMemoryResult::NoMemory) {
            return Err(self.error(format!(
                "multi-result call `{callable}` has a memory-bearing result contract"
            )));
        }
        let results = self.scalar_result_signatures.get(callable).ok_or_else(|| {
            self.error(format!(
                "multi-result call `{callable}` has no exact final-WIR scalar signature"
            ))
        })?;
        if results.is_empty() || results.len() != destinations.len() {
            return Err(self.error(format!(
                "multi-result call `{callable}` destination arity contradicts its exact scalar signature"
            )));
        }
        let mut seen_destinations = BTreeSet::new();
        let mut definitions = Vec::with_capacity(destinations.len());
        for (index, destination) in destinations.iter().enumerate() {
            if !seen_destinations.insert(destination) {
                return Err(self.error(format!(
                    "multi-result call `{callable}` repeats destination `{destination}`"
                )));
            }
            let binding = self.local(destination)?;
            if binding.memory_root {
                return Err(self.error(format!(
                    "multi-result call `{callable}` destination `{destination}` is an authenticated memory root"
                )));
            }
            let actual = self.local_type(destination)?;
            if !is_scalar_result_type(actual) || actual.kind() != results[index] {
                return Err(self.error(format!(
                    "multi-result call `{callable}` destination `{destination}` contradicts scalar result {index}"
                )));
            }
            let result = u32::try_from(index).map_err(|_| self.error("call result index space exhausted"))?;
            let destination_site = site.projected(
                crate::safewir_reaching_defs::SiteStep::Destination(result),
            );
            let reaching = self.reaching.definition_at(&destination_site).ok_or_else(|| {
                self.error("multi-result call destination has no structural reaching-definition identity")
            })?;
            if !matches!(
                self.reaching.definition(reaching).map(|definition| &definition.kind),
                Some(ReachingDefinitionKind::CallResult { local, result: found, indirect: false })
                    if local == destination && *found == result
            ) {
                return Err(self.error(format!(
                    "multi-result call `{callable}` destination `{destination}` does not match its exact direct call result"
                )));
            }
            let definition = self.fresh_definition()?;
            if self.reaching_scalar_definitions.insert(reaching, definition).is_some() {
                return Err(self.error("multi-result call result reaching definition was bound twice"));
            }
            definitions.push(definition);
        }
        let arguments = args
            .iter()
            .map(|argument| self.direct_call_argument(argument))
            .collect::<Result<Vec<_>, _>>()?;
        self.operations.push(Operation::Call {
            callable: callable.into(),
            arguments,
            result: None,
        });
        self.operations.push(Operation::BindCallScalarResults {
            callable: callable.into(),
            results: definitions,
        });
        Ok(())
    }

    fn local_type(&self, name: &str) -> Result<&crate::wir::WirTy, FinalWirValidationError> {
        self.function
            .params
            .iter()
            .chain(self.function.locals.iter())
            .find(|local| local.name == name)
            .map(|local| &local.ty)
            .ok_or_else(|| self.error(format!("unknown local `{name}`")))
    }

    fn error(&self, message: impl Into<String>) -> FinalWirValidationError {
        FinalWirValidationError::function(&self.function.name, message)
    }

    fn seq_at(
        &mut self,
        sequence: &WirSeq,
        parent: &StructuralSite,
    ) -> Result<(), FinalWirValidationError> {
        let mut index = 0;
        while index < sequence.len() {
            if self.try_counted_loop_at(sequence, index, parent)? {
                index += 2;
                continue;
            }
            let node = &sequence[index];
            let node_index = u32::try_from(index)
                .map_err(|_| self.error("structured node index space exhausted"))?;
            self.node_at(node, &parent.projected(crate::safewir_reaching_defs::SiteStep::Node(node_index)))?;
            index += 1;
        }
        Ok(())
    }

    /// Recognize exactly the one source-minted counted-loop shape.  This is a
    /// structural WIR pattern, not a helper-name policy: every spelling must
    /// prove the same init, signed exit, unit step, backedge, and def-use join.
    fn try_counted_loop_at(
        &mut self,
        sequence: &WirSeq,
        index: usize,
        parent: &StructuralSite,
    ) -> Result<bool, FinalWirValidationError> {
        let Some(WirNode::SetLocal { local, value: WirExpr::ConstI32(0) }) = sequence.get(index) else {
            return Ok(false);
        };
        let Some(WirNode::Block { label: exit, result: None, body }) = sequence.get(index + 1) else {
            return Ok(false);
        };
        let [WirNode::Loop { label: again, body: loop_body }] = body.as_slice() else {
            return Ok(false);
        };
        if loop_body.len() < 3 {
            return Err(self.error("counted loop requires exit, body, unit step, and backedge"));
        }
        let Some(WirNode::Br { target, cond: Some(condition) }) = loop_body.first() else {
            return Err(self.error("counted loop requires its signed exit branch first"));
        };
        if target != exit {
            return Err(self.error("counted loop exit must branch to its immediately enclosing block"));
        }
        let WirExpr::Binary { op: crate::wir::BinOp::Ge, kind: Kind::I32, lhs, rhs } = condition else {
            return Err(self.error("counted loop requires exact signed i32 `induction >= bound` exit semantics"));
        };
        if !matches!(lhs.as_ref(), WirExpr::GetLocal(name) if name == local) {
            return Err(self.error("counted loop exit lhs must be its induction local"));
        }
        let Some(WirNode::SetLocal { local: stepped, value: WirExpr::Binary { op: crate::wir::BinOp::Add, kind: Kind::I32, lhs: step_lhs, rhs: step_rhs } }) = loop_body.get(loop_body.len() - 2) else {
            return Err(self.error("counted loop requires exact unit induction update before its backedge"));
        };
        if stepped != local
            || !matches!(step_lhs.as_ref(), WirExpr::GetLocal(name) if name == local)
            || !matches!(step_rhs.as_ref(), WirExpr::ConstI32(1))
        {
            return Err(self.error("counted loop requires exact `induction = induction + 1` update"));
        }
        if !matches!(loop_body.last(), Some(WirNode::Br { target, cond: None }) if target == again) {
            return Err(self.error("counted loop requires an unconditional backedge after its unit update"));
        }

        let init_site = parent.projected(crate::safewir_reaching_defs::SiteStep::Node(
            u32::try_from(index).map_err(|_| self.error("counted-loop init index exhausted"))?,
        ));
        let block_site = parent.projected(crate::safewir_reaching_defs::SiteStep::Node(
            u32::try_from(index + 1).map_err(|_| self.error("counted-loop block index exhausted"))?,
        ));
        let loop_site = block_site
            .projected(crate::safewir_reaching_defs::SiteStep::Region(
                crate::safewir_reaching_defs::StructuredRegion::BlockBody,
            ))
            .projected(crate::safewir_reaching_defs::SiteStep::Node(0));
        let init = self.reaching.definition_at(&init_site.projected(
            crate::safewir_reaching_defs::SiteStep::Write,
        )).ok_or_else(|| self.error("counted-loop init has no structural write definition"))?;
        let step_site = loop_site
            .projected(crate::safewir_reaching_defs::SiteStep::Region(
                crate::safewir_reaching_defs::StructuredRegion::LoopBody,
            ))
            .projected(crate::safewir_reaching_defs::SiteStep::Node(
                u32::try_from(loop_body.len() - 2).map_err(|_| self.error("counted-loop step index exhausted"))?,
            ))
            .projected(crate::safewir_reaching_defs::SiteStep::Write);
        let step = self.reaching.definition_at(&step_site)
            .ok_or_else(|| self.error("counted-loop step has no structural write definition"))?;
        let local_ordinal = self.function.params.len().checked_add(
            self.function.locals.iter().position(|candidate| candidate.name == *local)
                .ok_or_else(|| self.error("counted-loop induction is not a declared local"))?,
        ).and_then(|ordinal| u32::try_from(ordinal).ok())
            .ok_or_else(|| self.error("counted-loop induction local ordinal exhausted"))?;
        let expected_join_site = loop_site.projected(crate::safewir_reaching_defs::SiteStep::Join {
            kind: crate::safewir_reaching_defs::JoinKind::LoopHeader,
            local: local_ordinal,
        });
        let join = self.reaching.joins().values().find(|join| {
            join.local == *local
                && join.kind == crate::safewir_reaching_defs::JoinKind::LoopHeader
                && join.site == expected_join_site
        }).ok_or_else(|| self.error("counted-loop induction has no exact loop-header join"))?;
        if join.predecessors.len() != 2
            || !join.predecessors[0].contains(init)
            || !join.predecessors[1].contains(step)
            || join.predecessors.iter().any(|predecessor| predecessor.len() != 1)
        {
            return Err(self.error("counted-loop induction join must have only init and unit-step predecessors"));
        }
        let local_binding = self.local(local)?;
        if local_binding.memory_root {
            return Err(self.error("counted-loop induction cannot be a memory root"));
        }
        let exit_operand = self.node_operand_site(
            &loop_body[0],
            &loop_site.projected(crate::safewir_reaching_defs::SiteStep::Region(
                crate::safewir_reaching_defs::StructuredRegion::LoopBody,
            )).projected(crate::safewir_reaching_defs::SiteStep::Node(0)),
            condition,
        )?;
        let condition_operands = crate::safewir_reaching_defs::structural_expr_operands(condition, &exit_operand)
            .ok_or_else(|| self.error("counted-loop exit has no structural operands"))?;
        let executable = condition_operands.into_iter().filter(|operand| !operand.claim_only).collect::<Vec<_>>();
        if executable.len() != 2 {
            return Err(self.error("counted-loop exit has ambiguous structural operands"));
        }
        let induction_read = self.reaching.reads().iter().find(|read| read.site == executable[0].site)
            .ok_or_else(|| self.error("counted-loop induction exit read is absent from reaching definitions"))?;
        if induction_read.reaching.len() != 1 || !induction_read.reaching.contains(join.definition) {
            return Err(self.error("counted-loop induction exit read does not reach its loop-header join"));
        }
        let bound = self.scalar_at_read(rhs, &executable[1].site, "counted-loop bound")?;

        self.operations.push(Operation::DefineScalar { definition: local_binding.definition, value: 0 });
        if self.reaching_scalar_definitions.insert(join.definition, local_binding.definition).is_some() {
            return Err(self.error("counted-loop loop-header join was bound twice"));
        }
        let control = self.push_control(again)?;
        let previous = self.active_index_guard.replace((local_binding.definition, bound));
        let body_site = loop_site.projected(crate::safewir_reaching_defs::SiteStep::Region(
            crate::safewir_reaching_defs::StructuredRegion::LoopBody,
        ));
        let outer = std::mem::take(&mut self.operations);
        for (body_index, node) in loop_body.iter().enumerate().skip(1).take(loop_body.len() - 3) {
            let body_index = u32::try_from(body_index)
                .map_err(|_| self.error("counted-loop body index exhausted"))?;
            self.node_at(node, &body_site.projected(crate::safewir_reaching_defs::SiteStep::Node(body_index)))?;
        }
        let body = std::mem::replace(&mut self.operations, outer);
        self.active_index_guard = previous;
        self.controls.pop();
        self.operations.push(Operation::CountedLoop {
            control,
            induction: local_binding.definition,
            bound,
            body,
        });
        Ok(true)
    }

    fn nested_seq_at(
        &mut self,
        sequence: &WirSeq,
        parent: &StructuralSite,
    ) -> Result<Vec<Operation>, FinalWirValidationError> {
        let outer = std::mem::take(&mut self.operations);
        self.seq_at(sequence, parent)?;
        let nested = std::mem::replace(&mut self.operations, outer);
        Ok(nested)
    }

    /// Bind a control condition to the one scalar definition structurally
    /// reaching this exact executable read.  The site is supplied by the
    /// shared reaching-definition traversal rather than reconstructed from
    /// the adapter's traversal, so erased claim operands cannot shift it.
    fn scalar_definition_at(
        &mut self,
        expression: &WirExpr,
        site: &StructuralSite,
    ) -> Result<DefinitionId, FinalWirValidationError> {
        match expression {
            WirExpr::GetLocal(name) => {
                let local = self.local(name)?;
                if local.memory_root {
                    return Err(self.error("branch conditions require scalar definitions"));
                }
                let read = self
                    .reaching
                    .reads()
                    .iter()
                    .find(|read| read.site == *site)
                    .ok_or_else(|| self.error(
                        "branch condition has no structural reaching-definition read",
                    ))?;
                if !read.reachable || read.local != *name || read.reaching.len() != 1 {
                    return Err(self.error(format!(
                        "branch condition requires one exact reaching scalar definition for local `{name}`"
                    )));
                }
                let reaching = read.reaching.iter().next().expect("length checked");
                if let Some(definition) = self.reaching_scalar_definitions.get(&reaching) {
                    return Ok(*definition);
                }
                let parameter_index = self
                    .function
                    .params
                    .iter()
                    .position(|parameter| parameter.name == *name)
                    .and_then(|index| u32::try_from(index).ok());
                if !matches!(
                    self.reaching.definition(reaching).map(|definition| &definition.kind),
                    Some(ReachingDefinitionKind::Parameter { local: parameter, index })
                        if parameter == name && Some(*index) == parameter_index
                ) {
                    return Err(self.error(format!(
                        "branch condition requires an unchanged scalar parameter; local `{name}` reaches a write, initializer, or join"
                    )));
                }
                Err(self.error(
                    "dynamic scalar parameters require the deferred validator scalar-fact lattice",
                ))
            }
            WirExpr::ConstI32(value) => {
                let definition = self.fresh_definition()?;
                self.operations.push(Operation::DefineScalar {
                    definition,
                    value: i128::from(*value),
                });
                Ok(definition)
            }
            WirExpr::ConstI64(value) => {
                let definition = self.fresh_definition()?;
                self.operations.push(Operation::DefineScalar {
                    definition,
                    value: i128::from(*value),
                });
                Ok(definition)
            }
            _ => Err(self.error(
                "branch conditions require one exact reaching scalar definition",
            )),
        }
    }

    fn node_operand_site(
        &self,
        node: &WirNode,
        node_site: &StructuralSite,
        expression: &WirExpr,
    ) -> Result<StructuralSite, FinalWirValidationError> {
        let expression = std::ptr::from_ref(expression);
        let operands = structural_node_operands(node, node_site).ok_or_else(|| {
            self.error("structured operand site space exhausted while binding a scalar read")
        })?;
        let mut matches = operands
            .into_iter()
            .filter(|operand| !operand.claim_only && operand.expression == expression);
        let Some(site) = matches.next().map(|operand| operand.site) else {
            return Err(self.error("scalar read is not an executable operand at its structural site"));
        };
        if matches.next().is_some() {
            return Err(self.error(
                "scalar read occurs more than once in one node; its structural operand is ambiguous",
            ));
        }
        Ok(site)
    }

    fn scalar_at_read(
        &self,
        expression: &WirExpr,
        site: &StructuralSite,
        role: &'static str,
    ) -> Result<DefinitionId, FinalWirValidationError> {
        let WirExpr::GetLocal(name) = expression else {
            return Err(self.error(format!("{role} requires one local scalar read")));
        };
        let local = self.local(name)?;
        if local.memory_root {
            return Err(self.error(format!("{role} names a memory root")));
        }
        let read = self.reaching.reads().iter().find(|read| read.site == *site).ok_or_else(|| {
            self.error(format!("{role} has no structural reaching-definition read"))
        })?;
        if !read.reachable || read.local != *name || read.reaching.len() != 1 {
            return Err(self.error(format!("{role} requires one exact reaching scalar definition")));
        }
        let reaching = read.reaching.iter().next().expect("length checked");
        if let Some(definition) = self.reaching_scalar_definitions.get(&reaching) {
            return Ok(*definition);
        }
        let parameter_index = self.function.params.iter().position(|parameter| parameter.name == *name)
            .and_then(|index| u32::try_from(index).ok());
        if matches!(
            self.reaching.definition(reaching).map(|definition| &definition.kind),
            Some(ReachingDefinitionKind::Parameter { local: parameter, index })
                if parameter == name && Some(*index) == parameter_index
        ) {
            return Ok(local.definition);
        }
        Err(self.error(format!("{role} reaches a write, initializer, call result, or join without an authenticated scalar binding")))
    }

    fn bind_header_count_local(
        &mut self,
        local: &str,
        place: &SafePlaceExpr,
        site: &StructuralSite,
    ) -> Result<(), FinalWirValidationError> {
        let local_binding = self.local(local)?;
        if local_binding.memory_root {
            return Err(self.error("header count destination cannot be a memory root"));
        }
        let write = site.projected(crate::safewir_reaching_defs::SiteStep::Write);
        let reaching = self.reaching.definition_at(&write).ok_or_else(|| {
            self.error("header count write has no structural reaching-definition identity")
        })?;
        if !matches!(self.reaching.definition(reaching).map(|definition| &definition.kind),
            Some(ReachingDefinitionKind::LocalWrite { local: written }) if written == local)
        {
            return Err(self.error("header count destination does not match its structural local write"));
        }
        let candidate = self.place(place)?;
        let [ProjectionStep::Header { component }] = candidate.projection.steps() else {
            return Err(self.error("header count load requires one canonical header projection"));
        };
        if !matches!(candidate.object.extent, Extent::Exact(SizeExpr::Constant(_)))
            || candidate.access != crate::safewir::AccessMode::Read
        {
            return Err(self.error("header count load requires a read-only header-only extent"));
        }
        let definition = self.fresh_definition()?;
        if self.reaching_scalar_definitions.insert(reaching, definition).is_some() {
            return Err(self.error("header count reaching definition was bound twice"));
        }
        self.operations.push(Operation::BindHeaderCount {
            definition,
            object: candidate.object.identity,
            component: *component,
        });
        Ok(())
    }

    fn index_below_condition(
        &self,
        node: &WirNode,
        condition: &WirExpr,
        site: &StructuralSite,
    ) -> Option<(DefinitionId, DefinitionId)> {
        let WirExpr::Binary { op: crate::wir::BinOp::LtU, kind: Kind::I32, lhs, rhs } = condition else {
            return None;
        };
        let condition_site = self.node_operand_site(node, site, condition).ok()?;
        let operands = crate::safewir_reaching_defs::structural_expr_operands(condition, &condition_site)?;
        let executable = operands.into_iter().filter(|operand| !operand.claim_only).collect::<Vec<_>>();
        if executable.len() != 2 {
            return None;
        }
        let index = self.scalar_at_read(lhs, &executable[0].site, "unsigned index guard").ok()?;
        let bound = self.scalar_at_read(rhs, &executable[1].site, "unsigned index guard").ok()?;
        Some((index, bound))
    }

    /// A host call may consume only an exact scalar identity from the shared
    /// reaching-definition graph. Mutable locals and computed expressions need
    /// richer elaboration; rejecting them keeps this ABI boundary fail closed.
    fn host_scalar_argument(
        &mut self,
        expression: &WirExpr,
    ) -> Result<HostArgument, FinalWirValidationError> {
        let definition = match expression {
            WirExpr::GetLocal(name) => {
                self.stable_scalar_parameter_definition(name, "host argument")?
            }
            WirExpr::ConstI32(_) | WirExpr::ConstI64(_) => self.scalar_definition_at(expression, &StructuralSite::root())?,
            _ => {
                return Err(self.error(
                    "host arguments require an exact scalar parameter or literal definition",
                ));
            }
        };
        Ok(HostArgument::Scalar(definition))
    }

    fn node_at(
        &mut self,
        node: &WirNode,
        site: &StructuralSite,
    ) -> Result<(), FinalWirValidationError> {
        match node {
            WirNode::Source { body, .. } => self.seq_at(
                body,
                &site.projected(crate::safewir_reaching_defs::SiteStep::Region(
                    crate::safewir_reaching_defs::StructuredRegion::SourceBody,
                )),
            ),
            WirNode::SafeStore { place, value, kind } => {
                self.expr(value)?;
                let place = self.place(place)?;
                self.operations.push(Operation::Store {
                    place,
                    width: memory_width(*kind)?,
                });
                Ok(())
            }
            WirNode::SafeStore8 { place, value } => {
                self.expr(value)?;
                let place = self.place(place)?;
                self.operations.push(Operation::Store { place, width: MemoryWidth::I8 });
                Ok(())
            }
            WirNode::SafeMemoryCopy { destination, source, len, kind } => {
                if destination.encoded_offset != 0 || source.encoded_offset != 0 {
                    return Err(self.error(
                        "bulk-memory templates require executable bases with zero immediate offset",
                    ));
                }
                self.require_constant_bulk_length(len, destination, source)?;
                let source = self.place(source)?;
                let destination = self.place(destination)?;
                self.operations.push(Operation::Copy {
                    source,
                    destination,
                    kind: match kind {
                        SafeCopyKind::Disjoint => CopyKind::Disjoint,
                        SafeCopyKind::MayOverlap => CopyKind::MayOverlap,
                    },
                });
                Ok(())
            }
            WirNode::SafeMemoryFill { .. } => Err(self.error(
                "typed memory fill requires the deferred valid-byte-pattern contract",
            )),
            WirNode::SafeKernel(operation) => Err(self.kernel_node_contract_error(operation)),
            WirNode::Store { .. }
            | WirNode::Store8 { .. }
            | WirNode::MemoryCopy { .. }
            | WirNode::MemoryFill { .. } => Err(self.error(
                "raw linear-memory operation bypasses the semantic SafeWIR dialect",
            )),
            WirNode::SetLocal { local, value: WirExpr::SafeLoad { place, kind: Kind::I32 } } => {
                self.bind_header_count_local(local, place, site)
            }
            WirNode::CallStoreMulti { func, args, dests } => {
                self.direct_scalar_call_results(func, args, dests, site)
            }
            WirNode::SetLocal { .. } => Err(self.error(
                "mutable local definitions require the deferred reaching-definition graph",
            )),
            WirNode::CallIndirectStoreMulti { .. } => Err(self.error(
                "indirect multi-result calls require checked callable-target facts",
            )),
            WirNode::SetGlobal { .. } => Err(self.error(
                "global writes require the sealed kernel/global contract boundary",
            )),
            WirNode::If { cond, then_, els, result: None }
                if els.is_empty() && self.index_below_condition(node, cond, site).is_some() =>
            {
                let (index, bound) = self.index_below_condition(node, cond, site).expect("guard was checked");
                let previous = self.active_index_guard.replace((index, bound));
                let body = self.nested_seq_at(
                    then_,
                    &site.projected(crate::safewir_reaching_defs::SiteStep::Region(
                        crate::safewir_reaching_defs::StructuredRegion::Then,
                    )),
                )?;
                self.active_index_guard = previous;
                self.operations.push(Operation::GuardIndexBelow { index, bound, body });
                Ok(())
            }
            WirNode::If { cond, then_, els, result: None } => {
                let operand = self.node_operand_site(node, site, cond)?;
                let condition = self.scalar_definition_at(cond, &operand)?;
                let then_operations = self.nested_seq_at(
                    then_,
                    &site.projected(crate::safewir_reaching_defs::SiteStep::Region(
                        crate::safewir_reaching_defs::StructuredRegion::Then,
                    )),
                )?;
                let else_operations = self.nested_seq_at(
                    els,
                    &site.projected(crate::safewir_reaching_defs::SiteStep::Region(
                        crate::safewir_reaching_defs::StructuredRegion::Else,
                    )),
                )?;
                self.operations.push(Operation::If {
                    condition,
                    then_operations,
                    else_operations,
                });
                Ok(())
            }
            WirNode::If { result: Some(_), .. } => Err(self.error(
                "value-producing branches require stack-result join definitions",
            )),
            WirNode::Block { result: Some(_), .. } => Err(self.error(
                "value-producing blocks require stack-result join definitions",
            )),
            WirNode::Block { label, result: None, body } => {
                let control = self.push_control(label)?;
                let body = self.nested_seq_at(
                    body,
                    &site.projected(crate::safewir_reaching_defs::SiteStep::Region(
                        crate::safewir_reaching_defs::StructuredRegion::BlockBody,
                    )),
                )?;
                self.controls.pop();
                self.operations.push(Operation::Block { control, body });
                Ok(())
            }
            WirNode::Loop { label, body } => {
                let control = self.push_control(label)?;
                let body = self.nested_seq_at(
                    body,
                    &site.projected(crate::safewir_reaching_defs::SiteStep::Region(
                        crate::safewir_reaching_defs::StructuredRegion::LoopBody,
                    )),
                )?;
                self.controls.pop();
                self.operations.push(Operation::Loop { control, body });
                Ok(())
            }
            WirNode::Br { target, cond } => {
                let target = self.resolve_control(target)?;
                let condition = cond
                    .as_ref()
                    .map(|condition| {
                        let operand = self.node_operand_site(node, site, condition)?;
                        self.scalar_definition_at(condition, &operand)
                    })
                    .transpose()?;
                self.operations.push(Operation::Branch { target, condition });
                Ok(())
            }
            WirNode::Push(value) | WirNode::Return(Some(value)) => self.expr(value),
            WirNode::Drop(WirExpr::Call { func, args }) | WirNode::Do(WirExpr::Call { func, args }) => {
                self.direct_void_call(func, args)
            }
            WirNode::Drop(expression) | WirNode::Do(expression) => self.expr(expression),
            WirNode::Return(None) => {
                self.operations.push(Operation::ReturnVoid);
                Ok(())
            }
            WirNode::Unreachable => Ok(()),
            WirNode::StructSet { base, value, .. } => {
                self.expr(base)?;
                self.expr(value)
            }
            WirNode::ArraySet { array, index, value, .. } => {
                self.expr(array)?;
                self.expr(index)?;
                self.expr(value)
            }
        }
    }

    fn expr(&mut self, expression: &WirExpr) -> Result<(), FinalWirValidationError> {
        match expression {
            WirExpr::SafeLoad { place, kind } => {
                let place = self.place(place)?;
                if let Some((index, bound)) = self.active_index_guard {
                    if matches!(place.projection.steps(), [ProjectionStep::Element { index: projected }] if *projected == index) {
                        self.operations.push(Operation::CheckedIndex {
                            object: place.object.identity,
                            index,
                            length_source: bound,
                            place,
                        });
                        return Ok(());
                    }
                }
                self.operations.push(Operation::Load { place, width: memory_width(*kind)? });
                Ok(())
            }
            WirExpr::SafeLoad8U { place } => {
                let place = self.place(place)?;
                self.operations.push(Operation::Load { place, width: MemoryWidth::I8 });
                Ok(())
            }
            WirExpr::SafeKernel(operation) => Err(self.kernel_expr_contract_error(operation)),
            WirExpr::Load { .. } | WirExpr::Load8U { .. } | WirExpr::MemoryGrow(_) => Err(
                self.error("raw linear-memory expression bypasses the semantic SafeWIR dialect"),
            ),
            WirExpr::Call { .. } => Err(self.error(
                "direct calls require statement position; value results require the deferred return-value def-use adapter",
            )),
            WirExpr::CallHost { import, args } => {
                let arguments = args
                    .iter()
                    .map(|argument| self.host_scalar_argument(argument))
                    .collect::<Result<Vec<_>, _>>()?;
                self.operations.push(Operation::HostCall {
                    import: import.clone(),
                    arguments,
                    result: None,
                });
                Ok(())
            }
            WirExpr::CallIndirect { .. } => Err(self.error(
                "indirect calls require checked callable-target and signature facts",
            )),
            WirExpr::Control(_) | WirExpr::Seq(_) => Err(self.error(
                "expression control flow requires the deferred join and fixpoint graph",
            )),
            WirExpr::Binary { lhs, rhs, .. } => {
                self.expr(lhs)?;
                self.expr(rhs)
            }
            WirExpr::Unary { arg, .. }
            | WirExpr::Convert { arg, .. }
            | WirExpr::ToSlot(arg, _)
            | WirExpr::FromSlot(arg, _)
            | WirExpr::ArrayLen(arg)
            | WirExpr::RefCast { value: arg, .. }
            | WirExpr::RefCastNullable { value: arg, .. }
            | WirExpr::RefIsNull(arg) => self.expr(arg),
            WirExpr::Vector { args, .. }
            | WirExpr::StructNew { args, .. }
            | WirExpr::ArrayNewFixed { items: args, .. } => {
                for argument in args {
                    self.expr(argument)?;
                }
                Ok(())
            }
            WirExpr::ArrayNew { value, len, .. } => {
                self.expr(value)?;
                self.expr(len)
            }
            WirExpr::ArrayGet { array, index, .. } => {
                self.expr(array)?;
                self.expr(index)
            }
            WirExpr::StructGet { base, .. } => self.expr(base),
            WirExpr::GetLocal(name) => {
                if self.local(name)?.memory_root {
                    return Err(self.error(
                        "memory-parameter pointers may appear only as checked SafeWIR place roots",
                    ));
                }
                Ok(())
            }
            WirExpr::ConstI64(_)
            | WirExpr::ConstF64(_)
            | WirExpr::ConstI32(_)
            | WirExpr::ConstV128(_)
            | WirExpr::StrPtr(_)
            | WirExpr::GetGlobal(_)
            | WirExpr::RefNull(_) => Ok(()),
            WirExpr::MemorySize => Err(
                self.error("raw linear-memory expression bypasses the semantic SafeWIR dialect"),
            ),
        }
    }

    fn place(&mut self, place: &SafePlaceExpr) -> Result<Place, FinalWirValidationError> {
        if self.scalar_key(&place.object.root)? != self.scalar_key(&place.range.object_root)? {
            return Err(self.error(
                "object root and range root are not the same final value",
            ));
        }
        let root_name = match place.object.root.as_ref() {
            WirExpr::GetLocal(name) => name,
            _ => {
                return Err(self.error(
                    "object roots must be parameter locals in the current adapter subset",
                ));
            }
        };
        let root = self.local(root_name)?;
        if !root.memory_root {
            return Err(self.error(
                "object roots require a checked memory-bearing parameter binding",
            ));
        }
        let identity = ObjectIdentity {
            root: root.definition,
            generation: GenerationId::new(1),
        };
        let projection = self.projection_at(place)?;
        let canonical_dynamic_element = self.matches_dynamic_element_base(place);
        let encoded_root = if canonical_dynamic_element {
            false
        } else {
            self.scalar_key(&place.object.root)? == self.scalar_key(&place.encoded_base)?
        };
        if !encoded_root && !canonical_dynamic_element {
            return Err(self.error(
                "object root and range root are not the same final value",
            ));
        }
        let start = self.size_at(&place.range.start)?;
        let expected_start = if canonical_dynamic_element {
            let [ProjectionStep::Element { index }] = projection.steps() else {
                return Err(self.error("canonical dynamic element address requires one element projection"));
            };
            let Some(descriptor) = self.contract_layout(place.object.layout) else {
                return Err(self.error("dynamic element layout is absent from the canonical bundle"));
            };
            let LayoutSize::Dynamic { base, stride } = descriptor.size() else {
                return Err(self.error("dynamic element address contradicts a fixed layout"));
            };
            SizeExpr::narrow_u32(SizeExpr::add(
                SizeExpr::Constant(u64::from(base)),
                SizeExpr::multiply(SizeExpr::Definition(*index), SizeExpr::Constant(u64::from(stride))),
            ))
        } else {
            SizeExpr::Constant(u64::from(place.encoded_offset))
        };
        if start != expected_start {
            return Err(self.error(
                "executable immediate offset contradicts the claimed byte-range start",
            ));
        }
        let provenance = match &place.object.provenance {
            SafeProvenanceExpr::Unknown => Provenance::Unknown,
            _ => {
                return Err(self.error(
                    "non-parameter provenance requires allocation/alias transfer operations",
                ));
            }
        };
        Ok(Place {
            object: ObjectRef {
                identity,
                layout: place.object.layout,
                extent: self.extent_at(&place.object.extent)?,
                provenance,
            },
            projection,
            range: ByteRange {
                object: identity,
                start,
                length: self.size_at(&place.range.length)?,
            },
            access: place.access,
        })
    }

    fn matches_dynamic_element_base(&self, place: &SafePlaceExpr) -> bool {
        let [SafeProjectionStep::Element { index }] = place.projection.steps() else {
            return false;
        };
        let Some(descriptor) = self.contract_layout(place.object.layout) else { return false };
        let LayoutSize::Dynamic { base, stride } = descriptor.size() else { return false };
        if place.encoded_offset != base {
            return false;
        }
        let WirExpr::GetLocal(root) = place.object.root.as_ref() else {
            return false;
        };
        let WirExpr::GetLocal(index) = index.as_ref() else {
            return false;
        };
        let Some(stride) = i32::try_from(stride).ok() else {
            return false;
        };
        let scaled = |expression: &WirExpr| matches!(expression,
            WirExpr::Binary { op: crate::wir::BinOp::Mul, kind: Kind::I32, lhs, rhs }
                if ((matches!(lhs.as_ref(), WirExpr::GetLocal(name) if name == index) && matches!(rhs.as_ref(), WirExpr::ConstI32(value) if *value == stride))
                    || (matches!(rhs.as_ref(), WirExpr::GetLocal(name) if name == index) && matches!(lhs.as_ref(), WirExpr::ConstI32(value) if *value == stride)))
        );
        matches!(place.encoded_base.as_ref(),
            WirExpr::Binary { op: crate::wir::BinOp::Add, kind: Kind::I32, lhs, rhs }
                if (matches!(lhs.as_ref(), WirExpr::GetLocal(name) if name == root) && scaled(rhs))
                    || (matches!(rhs.as_ref(), WirExpr::GetLocal(name) if name == root) && scaled(lhs))
        )
    }

    fn contract_layout(&self, layout: crate::layout::LayoutId) -> Option<&crate::layout::LayoutDescriptor> {
        self.layouts.get(layout)
    }

    fn require_constant_bulk_length(
        &mut self,
        executable: &WirExpr,
        first: &SafePlaceExpr,
        second: &SafePlaceExpr,
    ) -> Result<(), FinalWirValidationError> {
        let executable = constant_u64(executable)
            .ok_or_else(|| self.error("bulk length is not an exact nonnegative constant"))?;
        let first = self.size_at(&first.range.length)?;
        let second = self.size_at(&second.range.length)?;
        if first != SizeExpr::Constant(executable) || second != SizeExpr::Constant(executable) {
            return Err(self.error(
                "executable bulk length contradicts one or both checked ranges",
            ));
        }
        Ok(())
    }

    fn scalar_key(&self, expression: &WirExpr) -> Result<ScalarKey, FinalWirValidationError> {
        match expression {
            WirExpr::GetLocal(name) => Ok(ScalarKey::Definition(self.local(name)?.definition)),
            WirExpr::ConstI32(value) => Ok(ScalarKey::I32(*value)),
            WirExpr::ConstI64(value) => Ok(ScalarKey::I64(*value)),
            WirExpr::StrPtr(value) => Ok(ScalarKey::Static(*value)),
            _ => Err(self.error(
                "claim expressions must be pure scalar leaves in the current adapter subset",
            )),
        }
    }

    /// Kernel operations are deliberately opaque candidate nodes.  It is not
    /// sound to turn them into the superficially similar `Operation::{Alloc,
    /// Reuse, Retain, Release}` facts until the fixed template contract binds
    /// the executable allocator metadata accesses to the same root and
    /// generation as the ghost claim.  In particular, RC templates address
    /// `object - 4`, which is *not* a live-object `Place`; treating this as an
    /// ordinary `Release` would silently discard the load-bearing header
    /// range.  Keep the rejection per operation so a future elaborator has a
    /// precise, test-pinned contract to discharge rather than a generic escape
    /// hatch around the final boundary.
    fn kernel_expr_contract_error(
        &self,
        operation: &crate::safewir_kernel::SafeKernelExpr,
    ) -> FinalWirValidationError {
        use crate::safewir_kernel::{SafeAllocationClass, SafeKernelExpr};

        let message = match operation {
            SafeKernelExpr::RegionMark => {
                "region mark requires the checked kernel-contract adapter to bind its watermark result"
            }
            SafeKernelExpr::ScratchMark => {
                "scratch mark requires the checked kernel-contract adapter to bind its watermark result"
            }
            SafeKernelExpr::Allocate { class: SafeAllocationClass::Bump, .. } => {
                "bump allocation requires the checked kernel-contract adapter to bind its allocation result and fresh generation"
            }
            SafeKernelExpr::Allocate {
                class: SafeAllocationClass::ReferenceCounted,
                ..
            } => {
                "reference-counted allocation requires the checked kernel-contract adapter to bind its allocator-metadata header range, allocation result, and fresh generation"
            }
            SafeKernelExpr::ScratchAllocate { .. } => {
                "scratch allocation requires the checked kernel-contract adapter to bind its scope, borrowed range, and fresh generation"
            }
            SafeKernelExpr::Retain { .. } => {
                "retain requires the checked kernel-contract adapter to bind its alias group and reference-count metadata range"
            }
            SafeKernelExpr::Reuse { .. } => {
                "storage reuse requires the checked kernel-contract adapter to bind its allocator-metadata header range and old/new generation transfer"
            }
        };
        self.error(message)
    }

    fn kernel_node_contract_error(
        &self,
        operation: &crate::safewir_kernel::SafeKernelNode,
    ) -> FinalWirValidationError {
        use crate::safewir_kernel::{SafeKernelNode, SafeRegionRewind};

        let message = match operation {
            SafeKernelNode::RegionRewind(SafeRegionRewind::Iteration { .. }) => {
                "iteration region rewind requires the checked kernel-contract adapter to bind its watermark and invalidated generations"
            }
            SafeKernelNode::RegionRewind(SafeRegionRewind::Lexical { .. }) => {
                "lexical region rewind requires the checked kernel-contract adapter to bind its watermark and invalidated generations"
            }
            SafeKernelNode::RegionRewind(SafeRegionRewind::CopyOut { .. }) => {
                "copy-out region rewind requires the checked kernel-contract adapter to bind its watermark, escaping range, and generation transfer"
            }
            SafeKernelNode::ScratchRewind { .. } => {
                "scratch rewind requires the checked kernel-contract adapter to bind its watermark and borrowed-generation lifetime"
            }
            SafeKernelNode::Release { .. } => {
                "reference-counted release requires the checked kernel-contract adapter to bind its allocator-metadata header range and live object generation"
            }
            SafeKernelNode::Reclaim { .. } => {
                "reference-counted reclaim requires the checked kernel-contract adapter to bind its allocator-metadata header range and consumed object generation"
            }
        };
        self.error(message)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ScalarKey {
    Definition(DefinitionId),
    I32(i32),
    I64(i64),
    Static(u32),
}

fn constant_u64(expression: &WirExpr) -> Option<u64> {
    match expression {
        WirExpr::ConstI32(value) => u64::try_from(*value).ok(),
        WirExpr::ConstI64(value) => u64::try_from(*value).ok(),
        _ => None,
    }
}

fn memory_width(kind: Kind) -> Result<MemoryWidth, FinalWirValidationError> {
    match kind {
        Kind::I32 => Ok(MemoryWidth::I32),
        Kind::I64 => Ok(MemoryWidth::I64),
        Kind::F64 => Ok(MemoryWidth::F64),
        Kind::V128 => Ok(MemoryWidth::V128),
        Kind::ExternRef | Kind::StructRef | Kind::AnyRef | Kind::GcRef(_) => {
            Err(FinalWirValidationError::module(
                "Wasm references cannot be loaded from linear memory",
            ))
        }
    }
}

fn contract_size(size: &ContractSizeExpr) -> Result<SizeExpr, FinalWirValidationError> {
    match size {
        ContractSizeExpr::Constant(value) => Ok(SizeExpr::Constant(*value)),
        _ => Err(FinalWirValidationError::module(
            "parameter-dependent contract sizes require the deferred def-use adapter",
        )),
    }
}

/// The literal value of an executable operand, when it has one.
///
/// Deliberately only literals. A `GetLocal` also denotes a scalar, but naming
/// *which* definition reaches this read is the def-use adapter's job; guessing
/// here would bind a fact to whichever write happened to be last.
fn constant_operand(expression: &WirExpr) -> Option<i128> {
    match expression {
        WirExpr::ConstI32(value) => Some(i128::from(*value)),
        WirExpr::ConstI64(value) => Some(i128::from(*value)),
        _ => None,
    }
}

impl FunctionAdapter<'_> {
    /// Mint an exact scalar definition for a literal.
    fn constant_definition(
        &mut self,
        value: i128,
    ) -> Result<DefinitionId, FinalWirValidationError> {
        let definition = self.fresh_definition()?;
        self.operations.push(Operation::DefineScalar { definition, value });
        Ok(definition)
    }

    /// Bind one executable operand to an exact scalar definition, for operands
    /// whose value is a literal.
    fn constant_operand_definition(
        &mut self,
        expression: &WirExpr,
        role: &'static str,
    ) -> Result<DefinitionId, FinalWirValidationError> {
        match expression {
            WirExpr::GetLocal(name) => {
                if let Some((induction, _)) = self.active_index_guard {
                    if self.local(name)?.definition == induction {
                        return Ok(induction);
                    }
                }
                self.stable_scalar_parameter_definition(name, role)
            }
            _ => {
                let Some(value) = constant_operand(expression) else {
                    return Err(FinalWirValidationError::module(format!(
                        "{role} is not a literal or stable scalar parameter; a runtime operand requires exact reaching definitions"
                    )));
                };
                if value < 0 {
                    return Err(FinalWirValidationError::module(format!(
                        "{role} must not be negative"
                    )));
                }
                self.constant_definition(value)
            }
        }
    }

    fn extent_at(
        &mut self,
        extent: &SafeExtentExpr,
    ) -> Result<Extent, FinalWirValidationError> {
        match extent {
            SafeExtentExpr::Unknown => Ok(Extent::Unknown),
            SafeExtentExpr::Exact(value) => Ok(Extent::Exact(self.size_at(value)?)),
            SafeExtentExpr::Counted { header, count, stride } => {
                let header = self.size_at(header)?;
                let stride = self.size_at(stride)?;
                let count = self.constant_operand_definition(count, "a counted extent's count")?;
                Ok(Extent::Counted { header, count, stride })
            }
            SafeExtentExpr::ClosedSum => Err(FinalWirValidationError::module(
                "closed-sum extents require the deferred def-use adapter",
            )),
        }
    }

    fn size_at(
        &mut self,
        expression: &SafeSizeExpr,
    ) -> Result<SizeExpr, FinalWirValidationError> {
        match expression {
            SafeSizeExpr::Constant(value) => Ok(SizeExpr::Constant(*value)),
            SafeSizeExpr::Add(left, right) => {
                let left = self.size_at(left)?;
                let right = self.size_at(right)?;
                Ok(SizeExpr::add(left, right))
            }
            SafeSizeExpr::Multiply(left, right) => {
                let left = self.size_at(left)?;
                let right = self.size_at(right)?;
                Ok(SizeExpr::multiply(left, right))
            }
            SafeSizeExpr::NarrowU32(value) => {
                let value = self.size_at(value)?;
                Ok(SizeExpr::narrow_u32(value))
            }
            SafeSizeExpr::Value(expression) => {
                let definition =
                    self.constant_operand_definition(expression, "a symbolic size operand")?;
                Ok(SizeExpr::Definition(definition))
            }
        }
    }

    fn projection_at(
        &mut self,
        place: &SafePlaceExpr,
    ) -> Result<Projection, FinalWirValidationError> {
        let mut steps = Vec::with_capacity(place.projection.steps().len());
        for step in place.projection.steps() {
            steps.push(match step {
                SafeProjectionStep::Header { component } => {
                    ProjectionStep::Header { component: *component }
                }
                SafeProjectionStep::Field { index } => ProjectionStep::Field { index: *index },
                SafeProjectionStep::Element { index } => ProjectionStep::Element {
                    index: self.constant_operand_definition(index, "an element index")?,
                },
                SafeProjectionStep::Slice { start, length } => {
                    let start = self.constant_operand_definition(start, "a slice start")?;
                    let length = self.constant_operand_definition(length, "a slice length")?;
                    ProjectionStep::Slice { start, length }
                }
                // Scratch-kernel markers. These are not an indexed projection
                // with a literal index; they carry their own contract and the
                // scratch track owns them.
                SafeProjectionStep::DerivedElement { .. }
                | SafeProjectionStep::DerivedScratchElement { .. } => {
                    return Err(FinalWirValidationError::module(
                        "derived scratch projections require the deferred def-use adapter",
                    ));
                }
                SafeProjectionStep::SumTag | SafeProjectionStep::VariantField { .. } => {
                    return Err(FinalWirValidationError::module(
                        "closed-sum projections require the deferred def-use adapter",
                    ));
                }
                // A section's base is `constant_base + count_multiple * count`,
                // so its offset is a product of two runtime values. Discharging
                // it needs the count bound to a reaching definition, which is the
                // same adapter every other deferral here names.
                SafeProjectionStep::Section { .. } => {
                    return Err(FinalWirValidationError::module(
                        "section-major projections require the deferred def-use adapter",
                    ));
                }
            });
        }
        Ok(Projection::from_steps(steps))
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{
        ClosedTypeResolver, LayoutId, LayoutInterner, RcHeader, ReferenceKind, ResolvedNamed,
        ScalarKind,
    };
    use crate::safewir::{
        AccessMode, CallableMemoryResult, ContractExtent, ContractSizeExpr, DynamicHeaderInvariant,
        ParameterMemoryContract, ParameterMemoryEffect,
        ParameterMemoryEffects,
    };
    use crate::safewir_kernel::{
        SafeAllocationClass, SafeKernelExpr, SafeKernelNode, SafeKernelResultClaim, SafeKernelSize,
    };
    use crate::safewir_surface::{
        SafeEncodedAddress, SafeExactExtent, SafeObjectExpr, SafePlaceBuilder, SafeProjectionExpr,
        SafeRangeExpr,
    };
    use crate::wir::{WirLocal, WirStructDef, WirTy};
    use witchy_syntax::ast::Type;

    struct Resolver;

    impl ClosedTypeResolver for Resolver {
        fn resolve_named<'a>(
            &'a self,
            name: &str,
            _arguments: &[Type],
        ) -> Option<ResolvedNamed<'a>> {
            Some(match name {
                "Bool" => ResolvedNamed::Scalar(ScalarKind::Bool),
                "Int" => ResolvedNamed::Scalar(ScalarKind::Int),
                "Float" => ResolvedNamed::Scalar(ScalarKind::Float),
                "List" => ResolvedNamed::PackedList { rc: RcHeader::Required },
                "String" => ResolvedNamed::Reference(ReferenceKind::Owning),
                _ => return None,
            })
        }
    }

    fn layouts() -> (LayoutBundle, LayoutId) {
        layouts_for("Int")
    }

    fn byte_layouts() -> (LayoutBundle, LayoutId) {
        layouts_for("Bool")
    }

    fn layouts_for(name: &str) -> (LayoutBundle, LayoutId) {
        let mut interner = LayoutInterner::new();
        let layout = interner
            .intern_type(&Type::Named(name.into(), Vec::new()), &Resolver)
            .unwrap();
        (LayoutBundle::from_interner(&interner, [layout]).unwrap(), layout)
    }

    fn dynamic_list_layouts() -> (LayoutInterner, LayoutBundle, LayoutId, u32, u32) {
        let mut interner = LayoutInterner::new();
        let element = interner
            .intern_type(&Type::Named("Int".into(), Vec::new()), &Resolver)
            .expect("Int layout");
        let list = interner
            .intern_packed_list(element, RcHeader::Required)
            .expect("packed list layout");
        let crate::layout::LayoutSize::Dynamic { base, stride } = interner.get(list).unwrap().size() else {
            panic!("list must be dynamic")
        };
        let bundle = LayoutBundle::from_interner(&interner, [list, element]).unwrap();
        (interner, bundle, list, base, stride)
    }

    #[test]
    fn final_wir_elaborates_only_canonical_header_and_unsigned_guarded_dynamic_element() {
        let (interner, layouts, list, base, stride) = dynamic_list_layouts();
        let get = |name: &str| WirExpr::GetLocal(name.into());
        let header = SafePlaceBuilder::new(
            &interner,
            list,
            SafeExactExtent::header_only(),
            get("list"),
            AccessMode::Read,
        ).unwrap().header(0, SafeEncodedAddress::new(get("list"), 0)).unwrap();
        let address = WirExpr::Binary {
            op: crate::wir::BinOp::Add,
            kind: Kind::I32,
            lhs: Box::new(get("list")),
            rhs: Box::new(WirExpr::Binary {
                op: crate::wir::BinOp::Mul,
                kind: Kind::I32,
                lhs: Box::new(get("index")),
                rhs: Box::new(WirExpr::ConstI32(i32::try_from(stride).unwrap())),
            }),
        };
        let element = SafePlaceBuilder::new(
            &interner,
            list,
            SafeExactExtent::counted(get("cap")).unwrap(),
            get("list"),
            AccessMode::Read,
        ).unwrap().element(get("index"), SafeEncodedAddress::new(address, base)).unwrap();
        let module = WirModule {
            imports: Vec::new(),
            funcs: vec![WirFunc {
                name: "dynamic_access".into(),
                params: vec![
                    WirLocal { name: "cap".into(), ty: WirTy::Bool },
                    WirLocal { name: "list".into(), ty: WirTy::Bool },
                    WirLocal { name: "index".into(), ty: WirTy::Bool },
                ],
                ret: Vec::new(),
                locals: vec![WirLocal { name: "len".into(), ty: WirTy::Bool }],
                body: vec![
                    WirNode::SetLocal { local: "len".into(), value: WirExpr::SafeLoad { place: header, kind: Kind::I32 } },
                    WirNode::If {
                        cond: WirExpr::Binary {
                            op: crate::wir::BinOp::LtU,
                            kind: Kind::I32,
                            lhs: Box::new(get("index")),
                            rhs: Box::new(get("len")),
                        },
                        then_: vec![WirNode::Drop(WirExpr::SafeLoad { place: element, kind: Kind::I64 })],
                        els: Vec::new(),
                        result: None,
                    },
                ],
                raw_body: None,
            }],
            memory_pages: 1,
            data: Vec::new(),
            globals: Vec::new(),
            table: None,
            exports: Vec::new(),
        };
        let contract = CallableMemoryContract {
            parameters: vec![
                ParameterMemoryContract {
                    entry: ParameterMemoryEntry::NoMemory,
                    layout: None,
                    extent: None,
                    dynamic_header: None,
                    effects: ParameterMemoryEffects::none(),
                },
                ParameterMemoryContract {
                    entry: ParameterMemoryEntry::BorrowedRead,
                    layout: Some(list),
                    extent: Some(ContractExtent::Counted {
                        header: ContractSizeExpr::Constant(u64::from(base)),
                        count: ContractSizeExpr::Parameter(0),
                        stride: ContractSizeExpr::Constant(u64::from(stride)),
                    }),
                    dynamic_header: Some(DynamicHeaderInvariant::BoundedByCountedExtent { component: 0 }),
                    effects: ParameterMemoryEffects::only(ParameterMemoryEffect::Read),
                },
                ParameterMemoryContract {
                    entry: ParameterMemoryEntry::NoMemory,
                    layout: None,
                    extent: None,
                    dynamic_header: None,
                    effects: ParameterMemoryEffects::none(),
                },
            ],
            result: CallableMemoryResult::NoMemory,
        };
        let report = validate_final_wir_report(
            &module,
            &[],
            &[],
            FinalWirValidationInputs {
                layouts,
                functions: BTreeMap::from([("dynamic_access".into(), contract.clone())]),
                imports: BTreeMap::new(),
                host_imports: BTreeMap::new(),
                reallocation_templates: Default::default(),
            },
            ValidationBudget::default(),
        ).expect("the exact header/load/guard pattern must elaborate");
        assert!(report.metrics().operations >= 4);

        let mut signed = module;
        let WirNode::If { cond: WirExpr::Binary { op, .. }, .. } = &mut signed.funcs[0].body[1] else { panic!() };
        *op = crate::wir::BinOp::Lt;
        let error = validate_final_wir_report(
            &signed,
            &[],
            &[],
            FinalWirValidationInputs {
                layouts: dynamic_list_layouts().1,
                functions: BTreeMap::from([("dynamic_access".into(), contract)]),
                imports: BTreeMap::new(),
                host_imports: BTreeMap::new(),
                reallocation_templates: Default::default(),
            },
            ValidationBudget::default(),
        ).expect_err("signed guards stay outside the dynamic admission subset");
        assert!(error.to_string().contains("branch conditions require one exact reaching scalar definition"), "{error}");
    }

    #[test]
    fn counted_loop_matcher_rejects_mutated_source_shapes_before_admission() {
        let loop_function = |init: i32, exit_op: crate::wir::BinOp, step: i32, extra_write: bool| WirFunc {
            name: "counted".into(),
            params: Vec::new(),
            ret: Vec::new(),
            locals: vec![
                WirLocal { name: "i".into(), ty: WirTy::Bool },
                WirLocal { name: "bound".into(), ty: WirTy::Bool },
            ],
            body: vec![
                WirNode::SetLocal { local: "i".into(), value: WirExpr::ConstI32(init) },
                WirNode::Block {
                    label: "done".into(),
                    result: None,
                    body: vec![WirNode::Loop {
                        label: "again".into(),
                        body: [
                            vec![WirNode::Br {
                                target: "done".into(),
                                cond: Some(WirExpr::Binary {
                                    op: exit_op,
                                    kind: Kind::I32,
                                    lhs: Box::new(WirExpr::GetLocal("i".into())),
                                    rhs: Box::new(WirExpr::GetLocal("bound".into())),
                                }),
                            }],
                            extra_write.then_some(WirNode::If {
                                cond: WirExpr::ConstI32(1),
                                then_: vec![
                                    WirNode::SetLocal { local: "i".into(), value: WirExpr::ConstI32(7) },
                                    WirNode::Br { target: "again".into(), cond: None },
                                ],
                                els: Vec::new(),
                                result: None,
                            }).into_iter().collect(),
                            vec![
                                WirNode::SetLocal {
                                    local: "i".into(),
                                    value: WirExpr::Binary {
                                        op: crate::wir::BinOp::Add,
                                        kind: Kind::I32,
                                        lhs: Box::new(WirExpr::GetLocal("i".into())),
                                        rhs: Box::new(WirExpr::ConstI32(step)),
                                    },
                                },
                                WirNode::Br { target: "again".into(), cond: None },
                            ],
                        ].concat(),
                    }],
                },
            ],
            raw_body: None,
        };
        let check = |function: WirFunc| {
            let (layouts, _) = layouts();
            let contract = CallableMemoryContract {
                parameters: Vec::new(),
                result: CallableMemoryResult::NoMemory,
            };
            let contracts = BTreeMap::from([(function.name.clone(), contract.clone())]);
            let signatures = BTreeMap::from([(function.name.clone(), Vec::new())]);
            let reaching = analyze_reaching_definitions(&function, ReachingDefinitionBudget::default()).unwrap();
            FunctionAdapter::new(
                &function,
                contract,
                reaching,
                &layouts,
                &contracts,
                &signatures,
            ).unwrap().adapt()
                .expect_err("a source mutation must not receive SafeWIR admission")
                .to_string()
        };

        assert!(check(loop_function(1, crate::wir::BinOp::Ge, 1, false))
            .contains("mutable local definitions"));
        assert!(check(loop_function(0, crate::wir::BinOp::GeU, 1, false))
            .contains("exact signed i32"));
        assert!(check(loop_function(0, crate::wir::BinOp::Ge, 2, false))
            .contains("exact `induction = induction + 1`"));
        let bad_bound = check(loop_function(0, crate::wir::BinOp::Ge, 1, false));
        assert!(bad_bound.contains("counted-loop bound reaches a write, initializer, call result, or join"), "{bad_bound}");
        let bad_join = check(loop_function(0, crate::wir::BinOp::Ge, 1, true));
        assert!(bad_join.contains("only init and unit-step predecessors"), "{bad_join}");
    }

    fn semantic_place(layout: LayoutId) -> Box<SafePlaceExpr> {
        semantic_place_at(layout, AccessMode::Read, 0)
    }

    fn semantic_place_at(
        layout: LayoutId,
        access: AccessMode,
        encoded_offset: u32,
    ) -> Box<SafePlaceExpr> {
        semantic_place_for_extent(layout, access, encoded_offset, 8)
    }

    fn semantic_place_for_extent(
        layout: LayoutId,
        access: AccessMode,
        encoded_offset: u32,
        extent: u64,
    ) -> Box<SafePlaceExpr> {
        let root = WirExpr::GetLocal("object".into());
        Box::new(SafePlaceExpr {
            object: SafeObjectExpr {
                root: Box::new(root.clone()),
                layout,
                extent: SafeExtentExpr::Exact(SafeSizeExpr::Constant(extent)),
                provenance: SafeProvenanceExpr::Unknown,
            },
            projection: SafeProjectionExpr::root(),
            range: SafeRangeExpr {
                object_root: Box::new(root.clone()),
                start: SafeSizeExpr::Constant(u64::from(encoded_offset)),
                length: SafeSizeExpr::Constant(extent - u64::from(encoded_offset)),
            },
            access,
            encoded_base: Box::new(root),
            encoded_offset,
        })
    }

    fn read_write_inputs(layouts: LayoutBundle, layout: LayoutId) -> FinalWirValidationInputs {
        read_write_inputs_for_extent(layouts, layout, 8)
    }

    fn read_write_inputs_for_extent(
        layouts: LayoutBundle,
        layout: LayoutId,
        extent: u64,
    ) -> FinalWirValidationInputs {
        let mut inputs = inputs_for_extent(layouts, layout, extent);
        let parameter = &mut inputs.functions.get_mut("read").unwrap().parameters[0];
        parameter.entry = ParameterMemoryEntry::BorrowedWrite;
        parameter.effects = ParameterMemoryEffects::only(ParameterMemoryEffect::Read)
            .with(ParameterMemoryEffect::Write);
        inputs
    }

    fn module(expression: WirExpr) -> WirModule {
        WirModule {
            imports: Vec::new(),
            funcs: vec![WirFunc {
                name: "read".into(),
                params: vec![WirLocal { name: "object".into(), ty: WirTy::Str }],
                ret: Vec::new(),
                locals: Vec::new(),
                body: vec![WirNode::Drop(expression)],
                raw_body: None,
            }],
            memory_pages: 1,
            data: Vec::new(),
            globals: Vec::new(),
            table: None,
            exports: Vec::new(),
        }
    }

    fn inputs(layouts: LayoutBundle, layout: LayoutId) -> FinalWirValidationInputs {
        inputs_for_extent(layouts, layout, 8)
    }

    fn inputs_for_extent(
        layouts: LayoutBundle,
        layout: LayoutId,
        extent: u64,
    ) -> FinalWirValidationInputs {
        FinalWirValidationInputs {
            layouts,
            functions: BTreeMap::from([(
                "read".into(),
                CallableMemoryContract {
                    parameters: vec![ParameterMemoryContract {
                        entry: ParameterMemoryEntry::BorrowedRead,
                        layout: Some(layout),
                        extent: Some(ContractExtent::Exact(ContractSizeExpr::Constant(extent))),
                        dynamic_header: None,
                        effects: ParameterMemoryEffects::only(ParameterMemoryEffect::Read),
                    }],
                    result: CallableMemoryResult::NoMemory,
                },
            )]),
            imports: BTreeMap::new(),
            host_imports: BTreeMap::new(),
            reallocation_templates: Default::default(),
        }
    }

    /// Inputs for a body that never touches the borrowed parameter. The
    /// validator derives effects from the final body and rejects a declaration
    /// that claims more, so a GC-only module must declare none.
    fn inputs_without_parameter_effects(
        layouts: LayoutBundle,
        layout: LayoutId,
    ) -> FinalWirValidationInputs {
        let mut inputs = inputs(layouts, layout);
        inputs.functions.get_mut("read").expect("read contract").parameters[0].effects =
            ParameterMemoryEffects::none();
        inputs
    }

    // RFC-0162 A1-const: an indexed projection and a counted extent are
    // dischargeable when every operand is a literal. Before this the adapter
    // rejected them outright with "indexed projections require exact reaching
    // definitions", so even a constant index was unreachable and S1's runtime
    // buffer work could not be used end to end.
    //
    // Runtime operands still reject: naming WHICH definition reaches a
    // `GetLocal` read is the def-use adapter's job, and guessing here would
    // bind a fact to whichever write happened to be last.
    #[test]
    fn a_literal_index_binds_to_an_exact_scalar_definition() {
        let (layout_bundle, layout) = layouts();
        let mut place = semantic_place(layout);
        place.projection = SafeProjectionExpr::from_steps([SafeProjectionStep::Element {
            index: Box::new(WirExpr::ConstI32(0)),
        }]);
        let literal = validate_final_wir_report(
            &module(WirExpr::SafeLoad { place, kind: Kind::I64 }),
            &[],
            &[],
            inputs(layout_bundle.clone(), layout),
            ValidationBudget::default(),
        );
        assert!(
            !matches!(&literal, Err(error) if error.to_string().contains("exact reaching definitions")),
            "a literal index must not be turned away for lacking reaching definitions: {literal:?}",
        );

        let mut runtime = semantic_place(layout);
        runtime.projection = SafeProjectionExpr::from_steps([SafeProjectionStep::Element {
            index: Box::new(WirExpr::GetLocal("i".into())),
        }]);
        let mut runtime_module = module(WirExpr::SafeLoad { place: runtime, kind: Kind::I64 });
        runtime_module.funcs[0]
            .locals
            .push(WirLocal { name: "i".into(), ty: WirTy::Bool });
        let error = validate_final_wir_report(
            &runtime_module,
            &[],
            &[],
            inputs(layout_bundle, layout),
            ValidationBudget::default(),
        )
        .expect_err("a runtime index still requires exact reaching definitions");
        assert!(
            error.to_string().contains("exact reaching definitions"),
            "the runtime case must say what it is missing: {error}",
        );
    }

    #[test]
    fn a_stable_scalar_parameter_binds_to_its_structural_definition() {
        let (layout_bundle, layout) = layouts();
        let mut place = semantic_place(layout);
        place.projection = SafeProjectionExpr::from_steps([SafeProjectionStep::Element {
            index: Box::new(WirExpr::GetLocal("index".into())),
        }]);
        let mut final_module = module(WirExpr::SafeLoad { place, kind: Kind::I64 });
        final_module.funcs[0]
            .params
            .push(WirLocal { name: "index".into(), ty: WirTy::Bool });
        let mut final_inputs = inputs(layout_bundle, layout);
        final_inputs
            .functions
            .get_mut("read")
            .expect("read contract")
            .parameters
            .push(ParameterMemoryContract {
                entry: ParameterMemoryEntry::NoMemory,
                layout: None,
                extent: None,
                dynamic_header: None,
                effects: ParameterMemoryEffects::none(),
            });
        let error = validate_final_wir_report(
            &final_module,
            &[],
            &[],
            final_inputs,
            ValidationBudget::default(),
        )
        .expect_err("the scalar-layout fixture must reach projected-layout validation");
        assert!(
            error.to_string().contains("packed element sequence"),
            "the adapter must bind the stable parameter before the validator sees the projection: {error}",
        );
    }

    #[test]
    fn final_wir_report_maps_one_parameter_rooted_semantic_load() {
        let (layout_bundle, layout) = layouts();
        let report = validate_final_wir_report(
            &module(WirExpr::SafeLoad {
                place: semantic_place(layout),
                kind: Kind::I64,
            }),
            &[],
            &[],
            inputs(layout_bundle, layout),
            ValidationBudget::default(),
        )
        .unwrap();
        assert_eq!(report.metrics().operations, 1);
    }

    #[test]
    fn final_wir_report_runs_structural_reaching_preflight_before_elaboration() {
        let (layout_bundle, layout) = layouts();
        let error = validate_final_wir_report(
            &module(WirExpr::GetLocal("missing".into())),
            &[],
            &[],
            inputs(layout_bundle, layout),
            ValidationBudget::default(),
        )
        .expect_err("a final WIR local read without a reaching definition must reject");
        assert!(
            error.to_string().contains("reaching-definition preflight"),
            "the generic structural analysis must run before a pattern handler: {error}",
        );
    }

    #[test]
    fn validated_authority_erases_every_reachable_fixed_template_exactly() {
        let (layout_bundle, layout) = layouts();
        let mut semantic = module(WirExpr::ConstI32(0));
        semantic.funcs[0]
            .locals
            .push(WirLocal { name: "unused".into(), ty: WirTy::Int });
        semantic.funcs[0].body = vec![
            WirNode::Drop(WirExpr::SafeLoad {
                place: semantic_place(layout),
                kind: Kind::I64,
            }),
            WirNode::SafeStore {
                place: semantic_place_at(layout, AccessMode::Write, 0),
                value: WirExpr::ConstI64(9),
                kind: Kind::I64,
            },
            WirNode::SafeMemoryCopy {
                destination: semantic_place_at(layout, AccessMode::Write, 0),
                source: semantic_place(layout),
                len: WirExpr::ConstI32(8),
                kind: SafeCopyKind::MayOverlap,
            },
        ];
        let validated = validate_final_wir_module(
            semantic,
            Vec::new(),
            Vec::new(),
            read_write_inputs(layout_bundle, layout),
            ValidationBudget::default(),
        )
        .unwrap();
        assert_eq!(validated.report().metrics().operations, 3);

        let encoded = crate::wir_encode::try_encode_validated(validated).unwrap();
        wasmparser::validate(&encoded).unwrap();

        let mut raw = module(WirExpr::ConstI32(0));
        raw.funcs[0]
            .locals
            .push(WirLocal { name: "unused".into(), ty: WirTy::Int });
        raw.funcs[0].body = vec![
            WirNode::Drop(WirExpr::Load {
                ptr: Box::new(WirExpr::GetLocal("object".into())),
                kind: Kind::I64,
                offset: 0,
            }),
            WirNode::Store {
                ptr: WirExpr::GetLocal("object".into()),
                value: WirExpr::ConstI64(9),
                kind: Kind::I64,
                offset: 0,
            },
            WirNode::MemoryCopy {
                dest: WirExpr::GetLocal("object".into()),
                src: WirExpr::GetLocal("object".into()),
                len: WirExpr::ConstI32(8),
            },
        ];
        let raw_encoded = crate::wir_encode::try_encode_with_gc(&raw, &[], &[]).unwrap();
        assert_eq!(encoded, raw_encoded, "ghost erasure must use exact raw templates");

        let (layouts, layout) = byte_layouts();
        let mut semantic = module(WirExpr::ConstI32(0));
        semantic.funcs[0].body = vec![
            WirNode::Drop(WirExpr::SafeLoad8U {
                place: semantic_place_for_extent(layout, AccessMode::Read, 0, 1),
            }),
            WirNode::SafeStore8 {
                place: semantic_place_for_extent(layout, AccessMode::Write, 0, 1),
                value: WirExpr::ConstI32(7),
            },
        ];
        let validated = validate_final_wir_module(
            semantic,
            Vec::new(),
            Vec::new(),
            read_write_inputs_for_extent(layouts, layout, 1),
            ValidationBudget::default(),
        )
        .unwrap();
        assert_eq!(validated.report().metrics().operations, 2);
        let encoded = crate::wir_encode::try_encode_validated(validated).unwrap();

        let mut raw = module(WirExpr::ConstI32(0));
        raw.funcs[0].body = vec![
            WirNode::Drop(WirExpr::Load8U {
                ptr: Box::new(WirExpr::GetLocal("object".into())),
                offset: 0,
            }),
            WirNode::Store8 {
                ptr: WirExpr::GetLocal("object".into()),
                value: WirExpr::ConstI32(7),
                offset: 0,
            },
        ];
        let raw_encoded = crate::wir_encode::try_encode_with_gc(&raw, &[], &[]).unwrap();
        assert_eq!(encoded, raw_encoded, "byte templates must erase exactly");
    }

    #[test]
    fn validated_authority_rejects_bulk_places_with_unencoded_offsets() {
        let (layout_bundle, layout) = layouts();
        let mut semantic = module(WirExpr::ConstI32(0));
        semantic.funcs[0].body = vec![WirNode::SafeMemoryCopy {
            destination: semantic_place_at(layout, AccessMode::Write, 1),
            source: semantic_place(layout),
            len: WirExpr::ConstI32(7),
            kind: SafeCopyKind::MayOverlap,
        }];
        let error = validate_final_wir_module(
            semantic,
            Vec::new(),
            Vec::new(),
            read_write_inputs(layout_bundle, layout),
            ValidationBudget::default(),
        )
        .err()
        .expect("memory.copy has no immediate offset to encode");
        assert!(error.to_string().contains("zero immediate offset"), "{error}");
    }

    #[test]
    fn validated_authority_rejects_malformed_and_forged_gc_declarations() {
        let (layout_bundle, layout) = layouts();
        let error = validate_final_wir_module(
            module(WirExpr::SafeLoad {
                place: semantic_place(layout),
                kind: Kind::I64,
            }),
            vec![WirStructDef { fields: vec![Kind::GcRef(99)], mutable: false }],
            Vec::new(),
            inputs(layout_bundle, layout),
            ValidationBudget::default(),
        )
        .err()
        .expect("an out-of-band declaration reference must not grant authority");
        assert!(error.to_string().starts_with("SWIR-GC-001"), "{error}");

        let (layout_bundle, layout) = layouts();
        let error = validate_final_wir_module(
            module(WirExpr::StructNew { struct_id: 99, args: Vec::new() }),
            Vec::new(),
            Vec::new(),
            inputs(layout_bundle, layout),
            ValidationBudget::default(),
        )
        .err()
        .expect("an undeclared struct use must not grant authority");
        assert!(error.to_string().starts_with("SWIR-GC-001"), "{error}");

        let (layout_bundle, layout) = layouts();
        let error = validate_final_wir_module(
            module(WirExpr::StructNew { struct_id: 0, args: Vec::new() }),
            vec![WirStructDef { fields: vec![Kind::I32], mutable: false }],
            Vec::new(),
            inputs(layout_bundle, layout),
            ValidationBudget::default(),
        )
        .err()
        .expect("a mismatched struct constructor must not grant authority");
        assert!(error.to_string().contains("supplies 0 fields"), "{error}");

        let (layout_bundle, layout) = layouts();
        let error = validate_final_wir_module(
            module(WirExpr::RefNull(Kind::I32)),
            Vec::new(),
            Vec::new(),
            inputs(layout_bundle, layout),
            ValidationBudget::default(),
        )
        .err()
        .expect("an unsupported scalar ref.null must not reach the encoder");
        assert!(error.to_string().starts_with("SWIR-GC-001"), "{error}");
    }

    #[test]
    fn validated_authority_binds_and_encodes_exact_recursive_gc_declarations() {
        let (layout_bundle, layout) = layouts();
        let final_module = module(WirExpr::StructNew {
            struct_id: 0,
            args: vec![WirExpr::ConstI32(7), WirExpr::RefNull(Kind::GcRef(1))],
        });
        let gc_structs = vec![WirStructDef {
            fields: vec![Kind::I32, Kind::GcRef(1)],
            mutable: false,
        }];
        let gc_arrays = vec![WirArrayDef { element: Kind::GcRef(0) }];
        let raw_expected =
            crate::wir_encode::try_encode_with_gc(&final_module, &gc_structs, &gc_arrays).unwrap();
        let validated = validate_final_wir_module(
            final_module,
            gc_structs,
            gc_arrays,
            inputs_without_parameter_effects(layout_bundle, layout),
            ValidationBudget::default(),
        )
        .unwrap();
        assert_eq!(validated.report().metrics().gc_declarations, 2);
        assert_eq!(validated.report().metrics().gc_uses, 2);

        let encoded = crate::wir_encode::try_encode_validated(validated).unwrap();
        assert_eq!(encoded, raw_expected, "authority must consume the exact declarations checked");
        wasmparser::validate(&encoded).unwrap();
    }

    #[test]
    fn validated_authority_rejects_immutable_struct_writes_and_unknown_arrays() {
        let (layout_bundle, layout) = layouts();
        let mut write = module(WirExpr::ConstI32(0));
        write.funcs[0].body = vec![WirNode::StructSet {
            struct_id: 0,
            field: 0,
            base: WirExpr::RefNull(Kind::GcRef(0)),
            value: WirExpr::ConstI32(1),
        }];
        let error = validate_final_wir_module(
            write,
            vec![WirStructDef { fields: vec![Kind::I32], mutable: false }],
            Vec::new(),
            inputs(layout_bundle, layout),
            ValidationBudget::default(),
        )
        .err()
        .expect("writes must agree with the exact declaration's mutability");
        assert!(error.to_string().contains("immutable but is written"), "{error}");

        let (layout_bundle, layout) = layouts();
        let error = validate_final_wir_module(
            module(WirExpr::ArrayNewFixed {
                array_id: 0,
                items: vec![WirExpr::ConstI64(1)],
            }),
            Vec::new(),
            Vec::new(),
            inputs(layout_bundle, layout),
            ValidationBudget::default(),
        )
        .err()
        .expect("an undeclared array must not grant authority");
        assert!(error.to_string().contains("GC array 0 is not declared"), "{error}");

        let declarations = vec![WirStructDef { fields: Vec::new(), mutable: false }];
        let arrays = vec![WirArrayDef { element: Kind::I64 }];
        let (layout_bundle, layout) = layouts();
        let error = validate_final_wir_module(
            module(WirExpr::ArrayNewFixed { array_id: 1, items: Vec::new() }),
            declarations.clone(),
            arrays.clone(),
            inputs(layout_bundle, layout),
            ValidationBudget::default(),
        )
        .err()
        .expect("array instructions must use the array-relative index band");
        assert!(error.to_string().contains("GC array 1 is not declared"), "{error}");

        let (layout_bundle, layout) = layouts();
        let error = validate_final_wir_module(
            module(WirExpr::RefCastNullable {
                struct_id: 1,
                value: Box::new(WirExpr::RefNull(Kind::StructRef)),
            }),
            declarations,
            arrays,
            inputs(layout_bundle, layout),
            ValidationBudget::default(),
        )
        .err()
        .expect("struct casts must use the struct-relative index band");
        assert!(error.to_string().contains("GC struct 1 is not declared"), "{error}");
    }

    #[test]
    fn validated_authority_fails_closed_on_budget_raw_memory_and_raw_body() {
        let (layouts, layout) = layouts();
        let constrained = ValidationBudget {
            operations: 0,
            ..ValidationBudget::default()
        };
        let budget_error = validate_final_wir_module(
            module(WirExpr::SafeLoad {
                place: semantic_place(layout),
                kind: Kind::I64,
            }),
            Vec::new(),
            Vec::new(),
            inputs(layouts.clone(), layout),
            constrained,
        )
        .err()
        .expect("authority must not exist after budget exhaustion");
        assert!(budget_error.to_string().contains("operation budget exhausted"), "{budget_error}");

        let raw_memory_error = validate_final_wir_module(
            module(WirExpr::MemorySize),
            Vec::new(),
            Vec::new(),
            inputs(layouts.clone(), layout),
            ValidationBudget::default(),
        )
        .err()
        .expect("raw memory must not grant authority");
        assert!(raw_memory_error.to_string().contains("raw linear-memory"), "{raw_memory_error}");

        let mut raw_body = module(WirExpr::ConstI32(0));
        raw_body.funcs[0].raw_body = Some(vec![0x0b]);
        let raw_body_error = validate_final_wir_module(
            raw_body,
            Vec::new(),
            Vec::new(),
            inputs(layouts, layout),
            ValidationBudget::default(),
        )
        .err()
        .expect("raw bodies must not grant authority");
        assert!(raw_body_error.to_string().contains("raw function bodies"), "{raw_body_error}");
    }

    #[test]
    fn final_wir_report_rejects_raw_memory_and_executable_claim_divergence() {
        let (layouts, layout) = layouts();
        let raw = module(WirExpr::Load {
            ptr: Box::new(WirExpr::GetLocal("object".into())),
            kind: Kind::I64,
            offset: 0,
        });
        assert!(validate_final_wir_report(
            &raw,
            &[],
            &[],
            inputs(layouts.clone(), layout),
            ValidationBudget::default(),
        )
        .unwrap_err()
        .to_string()
        .contains("raw linear-memory"));

        let mut forged = semantic_place(layout);
        forged.encoded_base = Box::new(WirExpr::ConstI32(0));
        assert!(validate_final_wir_report(
            &module(WirExpr::SafeLoad { place: forged, kind: Kind::I64 }),
            &[],
            &[],
            inputs(layouts, layout),
            ValidationBudget::default(),
        )
        .unwrap_err()
        .to_string()
        .contains("not the same final value"));
    }

    #[test]
    fn final_wir_report_rejects_forged_rc_header_as_live_object_access() {
        // `$rc_alloc` and `$rc_free` repeatedly read the allocation-size word
        // at `object - 4`. That word is allocator metadata, outside the live
        // object's layout extent; a producer must not be able to disguise it
        // as an ordinary root-relative SafeWIR access merely by retaining the
        // object's descriptor and extent. Such a migration would make the
        // range claim untrue precisely at storage reuse.
        let (layouts, layout) = layouts();
        let mut forged = semantic_place(layout);
        forged.encoded_base = Box::new(WirExpr::Binary {
            op: crate::wir::BinOp::Sub,
            kind: Kind::I32,
            lhs: Box::new(WirExpr::GetLocal("object".into())),
            rhs: Box::new(WirExpr::ConstI32(4)),
        });
        let error = validate_final_wir_report(
            &module(WirExpr::SafeLoad { place: forged, kind: Kind::I32 }),
            &[],
            &[],
            inputs(layouts, layout),
            ValidationBudget::default(),
        )
        .expect_err("allocator-header arithmetic cannot mint live-object range authority");
        assert!(
            error.to_string().contains("claim expressions must be pure scalar leaves"),
            "{error}"
        );
    }

    #[test]
    fn final_wir_report_rejects_kernel_operations_until_contracts_are_checked() {
        let (layouts, layout) = layouts();
        let object = semantic_place(layout).object.clone();
        let expression = module(WirExpr::SafeKernel(SafeKernelExpr::Retain {
            object: object.clone(),
        }));
        let error = validate_final_wir_report(
            &expression,
            &[],
            &[],
            inputs(layouts.clone(), layout),
            ValidationBudget::default(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("checked kernel-contract adapter"), "{error}");

        let mut statement = module(WirExpr::ConstI32(0));
        statement.funcs[0].body = vec![WirNode::SafeKernel(SafeKernelNode::Release { object })];
        let error = validate_final_wir_report(
            &statement,
            &[],
            &[],
            inputs(layouts, layout),
            ValidationBudget::default(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("checked kernel-contract adapter"), "{error}");
    }

    #[test]
    fn final_wir_report_names_allocator_metadata_and_generation_contract_gaps() {
        // `Operation::Alloc` and `Operation::Release` check object-lifetime
        // facts, but the RC templates also access allocator-owned metadata at
        // `object - 4`.  Until fixed-template expansion can bind that range to
        // the same result/old generation, adapting the opaque nodes into those
        // operations would lose a memory access.  Pin the exact rejection so a
        // later kernel elaborator must add the missing proof instead of mapping
        // these nodes through a raw helper or a live-object Place.
        let (layouts, layout) = layouts();
        let result = SafeKernelResultClaim {
            layout,
            extent: SafeExtentExpr::Exact(SafeSizeExpr::Constant(8)),
        };
        let size = SafeKernelSize {
            checked: SafeSizeExpr::Constant(8),
            encoded: Box::new(WirExpr::ConstI32(8)),
        };
        let error = validate_final_wir_report(
            &module(WirExpr::SafeKernel(SafeKernelExpr::Allocate {
                class: SafeAllocationClass::ReferenceCounted,
                result,
                size,
            })),
            &[],
            &[],
            inputs(layouts.clone(), layout),
            ValidationBudget::default(),
        )
        .expect_err("RC allocation must not erase its allocator header access");
        let message = error.to_string();
        assert!(message.contains("reference-counted allocation"), "{message}");
        assert!(message.contains("allocator-metadata header range"), "{message}");
        assert!(message.contains("fresh generation"), "{message}");

        let object = semantic_place(layout).object.clone();
        let mut release = module(WirExpr::ConstI32(0));
        release.funcs[0].body = vec![WirNode::SafeKernel(SafeKernelNode::Release { object })];
        let error = validate_final_wir_report(
            &release,
            &[],
            &[],
            inputs(layouts, layout),
            ValidationBudget::default(),
        )
        .expect_err("RC release must not erase its allocator header access");
        let message = error.to_string();
        assert!(message.contains("reference-counted release"), "{message}");
        assert!(message.contains("allocator-metadata header range"), "{message}");
        assert!(message.contains("live object generation"), "{message}");
    }

    #[test]
    fn final_wir_report_rejects_same_width_scalar_reinterpretation() {
        let (layouts, layout) = layouts();
        let error = validate_final_wir_report(
            &module(WirExpr::SafeLoad {
                place: semantic_place(layout),
                kind: Kind::F64,
            }),
            &[],
            &[],
            inputs(layouts, layout),
            ValidationBudget::default(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("contradicts projected scalar"), "{error}");
    }

    #[test]
    fn final_wir_report_rejects_value_returns_and_memory_parameter_escape() {
        let (layouts, layout) = layouts();
        let mut forged = module(WirExpr::ConstI32(0));
        forged.funcs[0].ret = vec![WirTy::Str];
        forged.funcs[0].body = vec![WirNode::Return(Some(WirExpr::GetLocal("object".into())))];
        let error = validate_final_wir_report(
            &forged,
            &[],
            &[],
            inputs(layouts.clone(), layout),
            ValidationBudget::default(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("return-value def-use adapter"), "{error}");

        let embedded = module(WirExpr::StructNew {
            struct_id: 0,
            args: vec![WirExpr::GetLocal("object".into())],
        });
        let error = validate_final_wir_report(
            &embedded,
            &[],
            &[],
            inputs(layouts, layout),
            ValidationBudget::default(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("only as checked SafeWIR place roots"), "{error}");
    }

    #[test]
    fn final_wir_report_rejects_unbound_object_results_and_non_pointer_roots() {
        let (layouts, layout) = layouts();
        let mut unknown_result = inputs(layouts.clone(), layout);
        unknown_result.functions.get_mut("read").unwrap().result =
            CallableMemoryResult::Unknown;
        let error = validate_final_wir_report(
            &module(WirExpr::ConstI32(0)),
            &[],
            &[],
            unknown_result,
            ValidationBudget::default(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("result-definition adapter"), "{error}");

        let mut wrong_root = module(WirExpr::ConstI32(0));
        wrong_root.funcs[0].params[0].ty = WirTy::Int;
        let error = validate_final_wir_report(
            &wrong_root,
            &[],
            &[],
            inputs(layouts, layout),
            ValidationBudget::default(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("i32 object-root representation"), "{error}");
    }

    #[test]
    fn final_wir_report_preserves_void_branch_structure_and_effects() {
        let (layouts, layout) = layouts();
        let mut conditional = module(WirExpr::ConstI32(0));
        conditional.funcs[0].body = vec![WirNode::If {
            cond: WirExpr::ConstI32(1),
            then_: vec![WirNode::Drop(WirExpr::SafeLoad {
                place: semantic_place(layout),
                kind: Kind::I64,
            })],
            els: Vec::new(),
            result: None,
        }];
        let report = validate_final_wir_report(
            &conditional,
            &[],
            &[],
            inputs(layouts, layout),
            ValidationBudget::default(),
        )
        .unwrap();
        assert_eq!(report.metrics().operations, 3);
    }

    #[test]
    fn final_wir_report_rejects_scalar_parameter_control_through_nested_control_without_a_fact_lattice() {
        let (layouts, layout) = layouts();
        let mut nested = module(WirExpr::ConstI32(0));
        nested.funcs[0].params.push(WirLocal { name: "gate".into(), ty: WirTy::Bool });
        nested.funcs[0].body = vec![WirNode::Block {
            label: "exit".into(),
            result: None,
            body: vec![WirNode::Loop {
                label: "again".into(),
                body: vec![WirNode::If {
                    cond: WirExpr::GetLocal("gate".into()),
                    then_: Vec::new(),
                    els: Vec::new(),
                    result: None,
                }],
            }],
        }];
        let mut checked = inputs_without_parameter_effects(layouts, layout);
        checked.functions.get_mut("read").unwrap().parameters.push(ParameterMemoryContract {
            entry: ParameterMemoryEntry::NoMemory,
            layout: None,
            extent: None,
            dynamic_header: None,
            effects: ParameterMemoryEffects::none(),
        });
        let error = validate_final_wir_report(&nested, &[], &[], checked, ValidationBudget::default())
            .expect_err("a runtime scalar parameter must not be admitted as a literal scalar fact");
        assert!(
            error.to_string().contains("deferred validator scalar-fact lattice"),
            "the nested adapter path must reject before candidate validation: {error}",
        );
    }

    #[test]
    fn branch_condition_rejects_an_ambiguous_cfg_join_even_before_node_elaboration() {
        let (layouts, layout) = layouts();
        let mut conditional = module(WirExpr::ConstI32(0));
        conditional.funcs[0].params.push(WirLocal { name: "gate".into(), ty: WirTy::Bool });
        conditional.funcs[0].locals.push(WirLocal { name: "flag".into(), ty: WirTy::Bool });
        conditional.funcs[0].body = vec![
            WirNode::If {
                cond: WirExpr::GetLocal("gate".into()),
                then_: vec![WirNode::SetLocal {
                    local: "flag".into(),
                    value: WirExpr::ConstI32(1),
                }],
                els: vec![WirNode::SetLocal {
                    local: "flag".into(),
                    value: WirExpr::ConstI32(0),
                }],
                result: None,
            },
            WirNode::If {
                cond: WirExpr::GetLocal("flag".into()),
                then_: Vec::new(),
                els: Vec::new(),
                result: None,
            },
        ];
        let mut checked = inputs(layouts.clone(), layout);
        checked.functions.get_mut("read").unwrap().parameters.push(ParameterMemoryContract {
            entry: ParameterMemoryEntry::NoMemory,
            layout: None,
            extent: None,
            dynamic_header: None,
            effects: ParameterMemoryEffects::none(),
        });
        let function = &conditional.funcs[0];
        let contract = checked.functions.get("read").unwrap().clone();
        let signatures = BTreeMap::from([("read".into(), Vec::new())]);
        let reaching = analyze_reaching_definitions(function, ReachingDefinitionBudget::default())
            .expect("the structural graph must model the cfg join");
        let mut adapter = FunctionAdapter::new(
            function,
            contract,
            reaching,
            &layouts,
            &checked.functions,
            &signatures,
        )
            .expect("the adapter setup must not trust a mutable-local binding");
        let site = StructuralSite::root()
            .projected(crate::safewir_reaching_defs::SiteStep::Node(1))
            .projected(crate::safewir_reaching_defs::SiteStep::Operand(0));
        let error = adapter
            .scalar_definition_at(&WirExpr::GetLocal("flag".into()), &site)
            .expect_err("a control read after divergent writes must not choose either predecessor");
        assert!(error.to_string().contains("write, initializer, or join"), "{error}");
    }

    #[test]
    fn final_wir_report_maps_lexical_block_loop_branches_and_void_return() {
        let (layouts, layout) = layouts();
        let mut structured = module(WirExpr::ConstI32(0));
        structured.funcs[0].body = vec![
            WirNode::Block {
                label: "exit".into(),
                result: None,
                body: vec![WirNode::Loop {
                    label: "again".into(),
                    body: vec![
                        WirNode::Drop(WirExpr::SafeLoad {
                            place: semantic_place(layout),
                            kind: Kind::I64,
                        }),
                        WirNode::Br {
                            target: "exit".into(),
                            cond: Some(WirExpr::ConstI32(1)),
                        },
                        WirNode::Br { target: "again".into(), cond: None },
                    ],
                }],
            },
            WirNode::Return(None),
        ];
        let report = validate_final_wir_report(
            &structured,
            &[],
            &[],
            inputs(layouts, layout),
            ValidationBudget::default(),
        )
        .unwrap();
        assert_eq!(report.metrics().operations, 7);
    }

    #[test]
    fn final_wir_report_rejects_nonlexical_and_shadowed_control_targets() {
        let (layouts, layout) = layouts();
        let mut nonlexical = module(WirExpr::ConstI32(0));
        nonlexical.funcs[0].body = vec![WirNode::Br { target: "ghost".into(), cond: None }];
        let error = validate_final_wir_report(
            &nonlexical,
            &[],
            &[],
            inputs(layouts.clone(), layout),
            ValidationBudget::default(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("not a lexically enclosing control"), "{error}");

        let mut shadowed = module(WirExpr::ConstI32(0));
        shadowed.funcs[0].body = vec![WirNode::Block {
            label: "same".into(),
            result: None,
            body: vec![WirNode::Loop { label: "same".into(), body: Vec::new() }],
        }];
        let error = validate_final_wir_report(
            &shadowed,
            &[],
            &[],
            inputs(layouts, layout),
            ValidationBudget::default(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("shadows a lexically enclosing label"), "{error}");
    }

    #[test]
    fn final_wir_report_rejects_undeclared_or_duplicated_import_authority() {
        let (layouts, layout) = layouts();
        let mut forged = module(WirExpr::CallHost { import: "ghost".into(), args: Vec::new() });
        let mut forged_inputs = inputs(layouts.clone(), layout);
        forged_inputs.host_imports.insert(
            "ghost".into(),
            CallableMemoryContract {
                parameters: Vec::new(),
                result: CallableMemoryResult::NoMemory,
            },
        );
        assert!(validate_final_wir_report(
            &forged,
            &[],
            &[],
            forged_inputs,
            ValidationBudget::default(),
        )
        .unwrap_err()
        .to_string()
        .contains("every and only final import"));

        forged.imports = vec![
            crate::wir::WirImport { name: "ghost".into(), params: Vec::new(), results: Vec::new() },
            crate::wir::WirImport { name: "ghost".into(), params: Vec::new(), results: Vec::new() },
        ];
        let mut duplicated_inputs = inputs(layouts, layout);
        duplicated_inputs.host_imports.insert(
            "ghost".into(),
            CallableMemoryContract {
                parameters: Vec::new(),
                result: CallableMemoryResult::NoMemory,
            },
        );
        assert!(validate_final_wir_report(
            &forged,
            &[],
            &[],
            duplicated_inputs,
            ValidationBudget::default(),
        )
        .unwrap_err()
        .to_string()
        .contains("exactly once"));
    }

    #[test]
    fn final_wir_derives_void_helper_contracts_but_keeps_parameter_roots_source_backed() {
        let (layouts, layout) = layouts();
        let helper = WirFunc {
            name: "helper".into(),
            params: Vec::new(),
            ret: Vec::new(),
            locals: Vec::new(),
            body: vec![WirNode::Return(None)],
            raw_body: None,
        };
        let main = WirFunc {
            name: "main".into(),
            params: vec![WirLocal { name: "object".into(), ty: WirTy::Str }],
            ret: Vec::new(),
            locals: Vec::new(),
            body: vec![
                WirNode::Do(WirExpr::Call { func: "helper".into(), args: Vec::new() }),
                WirNode::Drop(WirExpr::SafeLoad {
                    place: semantic_place(layout),
                    kind: Kind::I64,
                }),
            ],
            raw_body: None,
        };
        let module = WirModule {
            imports: Vec::new(),
            funcs: vec![helper, main],
            memory_pages: 1,
            data: Vec::new(),
            globals: Vec::new(),
            table: None,
            exports: Vec::new(),
        };
        let mut inputs = inputs(layouts, layout);
        let read = inputs.functions.remove("read").expect("the parameter-root contract");
        inputs.functions.insert("main".into(), read);

        let report = validate_final_wir_report(
            &module,
            &[],
            &[],
            inputs.clone(),
            ValidationBudget::default(),
        )
        .expect("the helper must derive while the parameter root stays separately checked");
        assert_eq!(report.metrics().operations, 3);

        inputs.functions.insert(
            "helper".into(),
            CallableMemoryContract {
                parameters: Vec::new(),
                result: CallableMemoryResult::Fresh {
                    layout,
                    extent: ContractExtent::Exact(ContractSizeExpr::Constant(8)),
                },
            },
        );
        let error = validate_final_wir_report(
            &module,
            &[],
            &[],
            inputs,
            ValidationBudget::default(),
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("supplied declaration is forbidden"),
            "{error}"
        );
    }

    #[test]
    fn final_wir_direct_void_call_binds_literal_and_stable_scalar_arguments() {
        let (layouts, _) = layouts();
        let scalar = || ParameterMemoryContract {
            entry: ParameterMemoryEntry::NoMemory,
            layout: None,
            extent: None,
            dynamic_header: None,
            effects: ParameterMemoryEffects::none(),
        };
        let contract = || CallableMemoryContract {
            parameters: vec![scalar()],
            result: CallableMemoryResult::NoMemory,
        };
        let sink = WirFunc {
            name: "sink".into(),
            params: vec![WirLocal { name: "value".into(), ty: WirTy::Int }],
            ret: Vec::new(),
            locals: Vec::new(),
            body: vec![WirNode::Return(None)],
            raw_body: None,
        };
        let main = WirFunc {
            name: "main".into(),
            params: vec![WirLocal { name: "value".into(), ty: WirTy::Int }],
            ret: Vec::new(),
            locals: Vec::new(),
            body: vec![
                WirNode::Do(WirExpr::Call {
                    func: "sink".into(),
                    args: vec![WirExpr::GetLocal("value".into())],
                }),
                WirNode::Do(WirExpr::Call { func: "sink".into(), args: vec![WirExpr::ConstI64(7)] }),
                WirNode::Return(None),
            ],
            raw_body: None,
        };
        let report = validate_final_wir_report(
            &WirModule {
                imports: Vec::new(),
                funcs: vec![sink, main],
                memory_pages: 1,
                data: Vec::new(),
                globals: Vec::new(),
                table: None,
                exports: Vec::new(),
            },
            &[],
            &[],
            FinalWirValidationInputs {
                layouts,
                functions: BTreeMap::from([("sink".into(), contract()), ("main".into(), contract())]),
                imports: BTreeMap::new(),
                host_imports: BTreeMap::new(),
                reallocation_templates: Default::default(),
            },
            ValidationBudget::default(),
        )
        .expect("direct calls must retain exact scalar argument definitions");
        assert_eq!(report.metrics().operations, 5);
    }

    #[test]
    fn final_wir_direct_void_call_binds_authenticated_object_root_and_checks_authority() {
        let mut interner = LayoutInterner::new();
        let layout = interner
            .intern_type(&Type::Named("Int".into(), Vec::new()), &Resolver)
            .expect("Int layout");
        let mismatched_layout = interner
            .intern_type(&Type::Named("Float".into(), Vec::new()), &Resolver)
            .expect("Float layout");
        let layouts = LayoutBundle::from_interner(&interner, [layout, mismatched_layout])
            .expect("canonical layouts");
        let object_contract = |entry, object_layout, extent| CallableMemoryContract {
            parameters: vec![ParameterMemoryContract {
                entry,
                layout: Some(object_layout),
                extent: Some(ContractExtent::Exact(ContractSizeExpr::Constant(extent))),
                dynamic_header: None,
                effects: ParameterMemoryEffects::only(ParameterMemoryEffect::Read),
            }],
            result: CallableMemoryResult::NoMemory,
        };
        let function = |name: &str| WirFunc {
            name: name.into(),
            params: vec![WirLocal { name: "object".into(), ty: WirTy::Str }],
            ret: Vec::new(),
            locals: Vec::new(),
            body: if name == "main" {
                vec![
                    WirNode::Do(WirExpr::Call {
                        func: "sink".into(),
                        args: vec![WirExpr::GetLocal("object".into())],
                    }),
                    WirNode::Return(None),
                ]
            } else {
                vec![
                    WirNode::Drop(WirExpr::SafeLoad {
                        place: semantic_place(layout),
                        kind: Kind::I64,
                    }),
                    WirNode::Return(None),
                ]
            },
            raw_body: None,
        };
        let module = WirModule {
            imports: Vec::new(),
            funcs: vec![function("sink"), function("main")],
            memory_pages: 1,
            data: Vec::new(),
            globals: Vec::new(),
            table: None,
            exports: Vec::new(),
        };
        validate_final_wir_report(
            &module,
            &[],
            &[],
            FinalWirValidationInputs {
                layouts: layouts.clone(),
                functions: BTreeMap::from([
                    ("sink".into(), object_contract(ParameterMemoryEntry::BorrowedRead, layout, 8)),
                    ("main".into(), object_contract(ParameterMemoryEntry::BorrowedRead, layout, 8)),
                ]),
                imports: BTreeMap::new(),
                host_imports: BTreeMap::new(),
                reallocation_templates: Default::default(),
            },
            ValidationBudget::default(),
        )
        .expect("a checked parameter root must elaborate as an object argument");

        let authority = validate_final_wir_report(
            &module,
            &[],
            &[],
            FinalWirValidationInputs {
                layouts: layouts.clone(),
                functions: BTreeMap::from([
                    ("sink".into(), object_contract(ParameterMemoryEntry::Unique, layout, 8)),
                    ("main".into(), object_contract(ParameterMemoryEntry::BorrowedRead, layout, 8)),
                ]),
                imports: BTreeMap::new(),
                host_imports: BTreeMap::new(),
                reallocation_templates: Default::default(),
            },
            ValidationBudget::default(),
        )
        .expect_err("a borrowed root must not satisfy a unique call parameter");
        assert!(authority.to_string().contains("lacks required authority"), "{authority}");

        let extent = validate_final_wir_report(
            &module,
            &[],
            &[],
            FinalWirValidationInputs {
                layouts,
                functions: BTreeMap::from([
                    ("sink".into(), object_contract(ParameterMemoryEntry::BorrowedRead, mismatched_layout, 8)),
                    ("main".into(), object_contract(ParameterMemoryEntry::BorrowedRead, layout, 8)),
                ]),
                imports: BTreeMap::new(),
                host_imports: BTreeMap::new(),
                reallocation_templates: Default::default(),
            },
            ValidationBudget::default(),
        )
        .expect_err("a call cannot substitute a root with a contradictory extent");
        assert!(extent.to_string().contains("layout or extent"), "{extent}");
    }

    #[test]
    fn final_wir_direct_void_call_rejects_computed_and_value_position_calls() {
        let (layouts, _) = layouts();
        let scalar = || ParameterMemoryContract {
            entry: ParameterMemoryEntry::NoMemory,
            layout: None,
            extent: None,
            dynamic_header: None,
            effects: ParameterMemoryEffects::none(),
        };
        let contract = || CallableMemoryContract {
            parameters: vec![scalar()],
            result: CallableMemoryResult::NoMemory,
        };
        let sink = WirFunc {
            name: "sink".into(),
            params: vec![WirLocal { name: "value".into(), ty: WirTy::Int }],
            ret: Vec::new(),
            locals: Vec::new(),
            body: vec![WirNode::Return(None)],
            raw_body: None,
        };
        let main = WirFunc {
            name: "main".into(),
            params: vec![WirLocal { name: "value".into(), ty: WirTy::Int }],
            ret: Vec::new(),
            locals: Vec::new(),
            body: vec![WirNode::Do(WirExpr::Binary {
                op: crate::wir::BinOp::Add,
                kind: Kind::I64,
                lhs: Box::new(WirExpr::Call {
                    func: "sink".into(),
                    args: vec![WirExpr::GetLocal("value".into())],
                }),
                rhs: Box::new(WirExpr::ConstI64(1)),
            })],
            raw_body: None,
        };
        let error = validate_final_wir_report(
            &WirModule {
                imports: Vec::new(),
                funcs: vec![sink, main],
                memory_pages: 1,
                data: Vec::new(),
                globals: Vec::new(),
                table: None,
                exports: Vec::new(),
            },
            &[],
            &[],
            FinalWirValidationInputs {
                layouts,
                functions: BTreeMap::from([("sink".into(), contract()), ("main".into(), contract())]),
                imports: BTreeMap::new(),
                host_imports: BTreeMap::new(),
                reallocation_templates: Default::default(),
            },
            ValidationBudget::default(),
        )
        .expect_err("a value-position call must not discard an unmodeled result");
        assert!(error.to_string().contains("statement position"), "{error}");
    }

    #[test]
    fn final_wir_direct_void_call_rejects_reassigned_scalar_argument() {
        let (layouts, _) = layouts();
        let contract = || CallableMemoryContract {
            parameters: vec![ParameterMemoryContract {
                entry: ParameterMemoryEntry::NoMemory,
                layout: None,
                extent: None,
                dynamic_header: None,
                effects: ParameterMemoryEffects::none(),
            }],
            result: CallableMemoryResult::NoMemory,
        };
        let module = WirModule {
            imports: Vec::new(),
            funcs: vec![
                WirFunc {
                    name: "sink".into(),
                    params: vec![WirLocal { name: "value".into(), ty: WirTy::Int }],
                    ret: Vec::new(),
                    locals: Vec::new(),
                    body: vec![WirNode::Return(None)],
                    raw_body: None,
                },
                WirFunc {
                    name: "main".into(),
                    params: vec![WirLocal { name: "value".into(), ty: WirTy::Int }],
                    ret: Vec::new(),
                    locals: Vec::new(),
                    body: vec![
                        WirNode::SetLocal { local: "value".into(), value: WirExpr::ConstI64(9) },
                        WirNode::Do(WirExpr::Call {
                            func: "sink".into(),
                            args: vec![WirExpr::GetLocal("value".into())],
                        }),
                    ],
                    raw_body: None,
                },
            ],
            memory_pages: 1,
            data: Vec::new(),
            globals: Vec::new(),
            table: None,
            exports: Vec::new(),
        };
        let error = validate_final_wir_report(
            &module,
            &[],
            &[],
            FinalWirValidationInputs {
                layouts,
                functions: BTreeMap::from([("sink".into(), contract()), ("main".into(), contract())]),
                imports: BTreeMap::new(),
                host_imports: BTreeMap::new(),
                reallocation_templates: Default::default(),
            },
            ValidationBudget::default(),
        )
        .expect_err("a direct call must not bind a reassigned parameter as its original definition");
        assert!(error.to_string().contains("mutable local definitions"), "{error}");
    }

    #[test]
    fn final_wir_host_call_requires_the_canonical_abi_and_exact_scalar_binding() {
        let (layouts, _layout) = layouts();
        let host = "print_int";
        let canonical = crate::safewir_call::canonical_host_import_contract(host)
            .expect("print_int is a cataloged no-memory host ABI");
        let mut module = WirModule {
            imports: vec![crate::wir::WirImport {
                name: host.into(),
                params: vec![Kind::I64],
                results: Vec::new(),
            }],
            funcs: vec![WirFunc {
                name: "host_call".into(),
                params: vec![WirLocal { name: "value".into(), ty: WirTy::Int }],
                ret: Vec::new(),
                locals: Vec::new(),
                body: vec![WirNode::Do(WirExpr::CallHost {
                    import: host.into(),
                    args: vec![WirExpr::GetLocal("value".into())],
                })],
                raw_body: None,
            }],
            memory_pages: 1,
            data: Vec::new(),
            globals: Vec::new(),
            table: None,
            exports: Vec::new(),
        };
        let inputs = FinalWirValidationInputs {
            layouts,
            functions: BTreeMap::from([(
                "host_call".into(),
                CallableMemoryContract {
                    parameters: vec![ParameterMemoryContract {
                        entry: ParameterMemoryEntry::NoMemory,
                        layout: None,
                        extent: None,
                        dynamic_header: None,
                        effects: ParameterMemoryEffects::none(),
                    }],
                    result: CallableMemoryResult::NoMemory,
                },
            )]),
            imports: BTreeMap::new(),
            host_imports: BTreeMap::from([(host.into(), canonical.clone())]),
            reallocation_templates: Default::default(),
        };
        let report = validate_final_wir_report(
            &module,
            &[],
            &[],
            inputs.clone(),
            ValidationBudget::default(),
        )
        .expect("canonical scalar host call must elaborate");
        assert_eq!(report.metrics().operations, 1);

        module.imports[0].params = vec![Kind::I32];
        let error = validate_final_wir_report(
            &module,
            &[],
            &[],
            inputs,
            ValidationBudget::default(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("canonical ABI signature"), "{error}");
    }

    #[test]
    fn final_wir_host_call_rejects_reassigned_scalar_argument() {
        let (layouts, _layout) = layouts();
        let host = "print_int";
        let canonical = crate::safewir_call::canonical_host_import_contract(host).unwrap();
        let module = WirModule {
            imports: vec![crate::wir::WirImport {
                name: host.into(),
                params: vec![Kind::I64],
                results: Vec::new(),
            }],
            funcs: vec![WirFunc {
                name: "host_call".into(),
                params: vec![WirLocal { name: "value".into(), ty: WirTy::Int }],
                ret: Vec::new(),
                locals: vec![WirLocal { name: "copy".into(), ty: WirTy::Int }],
                body: vec![
                    WirNode::SetLocal {
                        local: "copy".into(),
                        value: WirExpr::GetLocal("value".into()),
                    },
                    WirNode::Do(WirExpr::CallHost {
                        import: host.into(),
                        args: vec![WirExpr::GetLocal("copy".into())],
                    }),
                ],
                raw_body: None,
            }],
            memory_pages: 1,
            data: Vec::new(),
            globals: Vec::new(),
            table: None,
            exports: Vec::new(),
        };
        let inputs = FinalWirValidationInputs {
            layouts,
            functions: BTreeMap::from([(
                "host_call".into(),
                CallableMemoryContract {
                    parameters: vec![ParameterMemoryContract {
                        entry: ParameterMemoryEntry::NoMemory,
                        layout: None,
                        extent: None,
                        dynamic_header: None,
                        effects: ParameterMemoryEffects::none(),
                    }],
                    result: CallableMemoryResult::NoMemory,
                },
            )]),
            imports: BTreeMap::new(),
            host_imports: BTreeMap::from([(host.into(), canonical)]),
            reallocation_templates: Default::default(),
        };
        let error = validate_final_wir_report(
            &module,
            &[],
            &[],
            inputs,
            ValidationBudget::default(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("mutable local definitions"), "{error}");
    }

    #[test]
    fn final_wir_host_call_rejects_i32_pointer_or_descriptor_abi() {
        let (layouts, _layout) = layouts();
        // `print` is cataloged but takes `(i32 ptr, i32 len)`. Name lookup
        // must never promote those pointer-shaped values to scalar authority.
        assert!(crate::safewir_call::canonical_host_import_contract("print").is_none());
        let no_memory = || ParameterMemoryContract {
            entry: ParameterMemoryEntry::NoMemory,
            layout: None,
            extent: None,
            dynamic_header: None,
            effects: ParameterMemoryEffects::none(),
        };
        let module = WirModule {
            imports: vec![crate::wir::WirImport {
                name: "print".into(),
                params: vec![Kind::I32, Kind::I32],
                results: Vec::new(),
            }],
            funcs: vec![WirFunc {
                name: "host_call".into(),
                params: Vec::new(),
                ret: Vec::new(),
                locals: Vec::new(),
                body: vec![WirNode::Do(WirExpr::CallHost {
                    import: "print".into(),
                    args: vec![WirExpr::ConstI32(0), WirExpr::ConstI32(0)],
                })],
                raw_body: None,
            }],
            memory_pages: 1,
            data: Vec::new(),
            globals: Vec::new(),
            table: None,
            exports: Vec::new(),
        };
        let inputs = FinalWirValidationInputs {
            layouts,
            // The zero-argument void function contract is mechanically
            // derived from this final WIR shape.
            functions: BTreeMap::new(),
            imports: BTreeMap::new(),
            host_imports: BTreeMap::from([(
                "print".into(),
                CallableMemoryContract {
                    parameters: vec![no_memory(), no_memory()],
                    result: CallableMemoryResult::NoMemory,
                },
            )]),
            reallocation_templates: Default::default(),
        };
        let error = validate_final_wir_report(
            &module,
            &[],
            &[],
            inputs,
            ValidationBudget::default(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("no canonical ABI memory contract"), "{error}");
    }

    #[test]
    fn final_wir_binds_direct_scalar_multi_call_results_to_exact_reaching_definitions() {
        let (layouts, _) = layouts();
        let module = WirModule {
            imports: Vec::new(),
            funcs: vec![
                WirFunc {
                    name: "pair".into(),
                    params: Vec::new(),
                    ret: vec![WirTy::Int, WirTy::Bool],
                    locals: Vec::new(),
                    body: vec![
                        WirNode::Push(WirExpr::ConstI64(7)),
                        WirNode::Push(WirExpr::ConstI32(1)),
                    ],
                    raw_body: None,
                },
                WirFunc {
                    name: "run".into(),
                    params: Vec::new(),
                    ret: Vec::new(),
                    locals: vec![
                        WirLocal { name: "value".into(), ty: WirTy::Int },
                        WirLocal { name: "flag".into(), ty: WirTy::Bool },
                    ],
                    body: vec![
                        WirNode::CallStoreMulti {
                            func: "pair".into(),
                            args: Vec::new(),
                            dests: vec!["value".into(), "flag".into()],
                        },
                        WirNode::If {
                            cond: WirExpr::GetLocal("flag".into()),
                            then_: Vec::new(),
                            els: Vec::new(),
                            result: None,
                        },
                    ],
                    raw_body: None,
                },
            ],
            memory_pages: 1,
            data: Vec::new(),
            globals: Vec::new(),
            table: None,
            exports: Vec::new(),
        };
        let inputs = FinalWirValidationInputs {
            layouts,
            functions: BTreeMap::from([(
                "pair".into(),
                CallableMemoryContract {
                    parameters: Vec::new(),
                    result: CallableMemoryResult::NoMemory,
                },
            )]),
            imports: BTreeMap::new(),
            host_imports: BTreeMap::new(),
            reallocation_templates: Default::default(),
        };
        validate_final_wir_report(
            &module,
            &[],
            &[],
            inputs,
            ValidationBudget::default(),
        ).expect("scalar direct call results must bind exact reaching definitions");
    }

    #[test]
    fn final_wir_rejects_scalar_multi_call_result_type_or_destination_forgery() {
        let (layouts, _) = layouts();
        let mut module = WirModule {
            imports: Vec::new(),
            funcs: vec![
                WirFunc {
                    name: "value".into(),
                    params: Vec::new(),
                    ret: vec![WirTy::Int],
                    locals: Vec::new(),
                    body: vec![WirNode::Push(WirExpr::ConstI64(1))],
                    raw_body: None,
                },
                WirFunc {
                    name: "run".into(),
                    params: Vec::new(),
                    ret: Vec::new(),
                    locals: vec![WirLocal { name: "out".into(), ty: WirTy::Float }],
                    body: vec![WirNode::CallStoreMulti {
                        func: "value".into(),
                        args: Vec::new(),
                        dests: vec!["out".into()],
                    }],
                    raw_body: None,
                },
            ],
            memory_pages: 1,
            data: Vec::new(),
            globals: Vec::new(),
            table: None,
            exports: Vec::new(),
        };
        let inputs = || FinalWirValidationInputs {
            layouts: layouts.clone(),
            functions: BTreeMap::from([(
                "value".into(),
                CallableMemoryContract { parameters: Vec::new(), result: CallableMemoryResult::NoMemory },
            )]),
            imports: BTreeMap::new(),
            host_imports: BTreeMap::new(),
            reallocation_templates: Default::default(),
        };
        let error = validate_final_wir_report(
            &module, &[], &[], inputs(), ValidationBudget::default(),
        ).unwrap_err();
        assert!(error.to_string().contains("contradicts scalar result"), "{error}");

        module.funcs[1].locals[0].ty = WirTy::Int;
        module.funcs[0].ret = vec![WirTy::Int, WirTy::Int];
        module.funcs[0].body.push(WirNode::Push(WirExpr::ConstI64(2)));
        module.funcs[1].body = vec![WirNode::CallStoreMulti {
            func: "value".into(),
            args: Vec::new(),
            dests: vec!["out".into(), "out".into()],
        }];
        let error = validate_final_wir_report(
            &module, &[], &[], inputs(), ValidationBudget::default(),
        ).unwrap_err();
        assert!(error.to_string().contains("repeats destination"), "{error}");
    }
}
