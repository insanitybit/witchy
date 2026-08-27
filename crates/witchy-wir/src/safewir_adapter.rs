//! Fail-closed mapping from final executable WIR to the declarative SafeWIR
//! candidate graph.
//!
//! This module remains a report boundary. It returns no encoder authority and
//! supports only the void, parameter-rooted semantic-memory subset implemented
//! by the current validator, including structured branches, blocks, loops, and
//! void returns. Every unsupported executable shape is rejected rather than
//! omitted from the candidate graph.
//! Branch conditions remain limited to adapter-defined scalar constants; final
//! mutable-local reaching values still require the separate def-use track.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::layout::LayoutBundle;
use crate::safewir::{
    ByteRange, CallableMemoryContract, CallableMemoryResult, ContractExtent, ContractSizeExpr,
    DefinitionId, Extent, GenerationId, ObjectIdentity, ObjectRef, ParameterMemoryEntry, Place,
    Projection, ProjectionStep, Provenance, SizeExpr,
};
use crate::safewir_surface::{
    SafeCopyKind, SafeExtentExpr, SafePlaceExpr, SafeProjectionStep, SafeProvenanceExpr,
    SafeSizeExpr,
};
use crate::safewir_validate::{
    self, CandidateFunction, CandidateModule, ControlId, CopyKind, HostArgument, MemoryWidth,
    Operation, ParameterBinding, PostValidationTransform, ValidationBudget, ValidationError,
    ValidationReport,
};
use crate::wir::{Kind, WirExpr, WirFunc, WirModule, WirNode, WirSeq};

#[derive(Debug, Clone)]
pub struct FinalWirValidationInputs {
    pub layouts: LayoutBundle,
    pub functions: BTreeMap<String, CallableMemoryContract>,
    pub imports: BTreeMap<String, CallableMemoryContract>,
    pub host_imports: BTreeMap<String, CallableMemoryContract>,
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

/// Validate the final optimized executable WIR in report mode.
///
/// Success is diagnostic evidence only. It deliberately does not expose a
/// `ValidatedWirModule` or any encoder entrypoint.
pub fn validate_final_wir_report(
    module: &WirModule,
    inputs: FinalWirValidationInputs,
    budget: ValidationBudget,
) -> Result<ValidationReport, FinalWirValidationError> {
    if module.funcs.len() != inputs.functions.len()
        || module
            .funcs
            .iter()
            .any(|function| !inputs.functions.contains_key(&function.name))
    {
        return Err(FinalWirValidationError::module(
            "checked callable contracts must name every and only final function",
        ));
    }
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

    let functions = module
        .funcs
        .iter()
        .map(|function| {
            let contract = inputs
                .functions
                .get(&function.name)
                .expect("function contract census was checked")
                .clone();
            FunctionAdapter::new(function, contract)?.adapt()
        })
        .collect::<Result<Vec<_>, _>>()?;
    let candidate = CandidateModule {
        checked_source_entries: 1,
        layouts: inputs.layouts,
        imports: inputs.imports,
        host_imports: inputs.host_imports,
        functions,
        contains_raw_memory: false,
        contains_raw_body: false,
        post_validation_transform: PostValidationTransform::FixedTemplateExpansionAndErasure,
    };
    Ok(safewir_validate::validate_candidate(&candidate, budget)?)
}

struct FunctionAdapter<'a> {
    function: &'a WirFunc,
    contract: CallableMemoryContract,
    next_definition: u32,
    next_control: u32,
    local_names: BTreeMap<String, LocalId>,
    locals: Vec<LocalBinding>,
    controls: Vec<(String, ControlId)>,
    parameters: Vec<ParameterBinding>,
    operations: Vec<Operation>,
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
        if !function.ret.is_empty()
            || !matches!(contract.result, CallableMemoryResult::NoMemory)
        {
            return Err(FinalWirValidationError::function(
                &function.name,
                "value-returning callables require the deferred return-value def-use adapter",
            ));
        }
        let mut adapter = Self {
            function,
            contract,
            next_definition: 1,
            next_control: 1,
            local_names: BTreeMap::new(),
            locals: Vec::new(),
            controls: Vec::new(),
            parameters: Vec::new(),
            operations: Vec::new(),
        };
        let parameter_contracts = adapter.contract.parameters.clone();
        for (parameter, memory) in function.params.iter().zip(&parameter_contracts) {
            let definition = adapter.fresh_definition()?;
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
                    let extent = contract_extent(memory.extent.as_ref().ok_or_else(|| {
                        adapter.error("memory parameter has no authenticated extent")
                    })?)?;
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
        for local in &function.locals {
            let definition = adapter.fresh_definition()?;
            adapter.declare_local(&local.name, definition, false)?;
        }
        Ok(adapter)
    }

    fn adapt(mut self) -> Result<CandidateFunction, FinalWirValidationError> {
        let body = self.function.body.clone();
        self.seq(&body)?;
        Ok(CandidateFunction {
            name: self.function.name.clone(),
            contract: self.contract,
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

    fn error(&self, message: impl Into<String>) -> FinalWirValidationError {
        FinalWirValidationError::function(&self.function.name, message)
    }

    fn seq(&mut self, sequence: &WirSeq) -> Result<(), FinalWirValidationError> {
        for node in sequence {
            self.node(node)?;
        }
        Ok(())
    }

    fn nested_seq(&mut self, sequence: &WirSeq) -> Result<Vec<Operation>, FinalWirValidationError> {
        let outer = std::mem::take(&mut self.operations);
        self.seq(sequence)?;
        let nested = std::mem::replace(&mut self.operations, outer);
        Ok(nested)
    }

    fn scalar_definition(
        &mut self,
        expression: &WirExpr,
    ) -> Result<DefinitionId, FinalWirValidationError> {
        match expression {
            WirExpr::GetLocal(name) => {
                let local = self.local(name)?;
                if local.memory_root {
                    return Err(self.error("branch conditions require scalar definitions"));
                }
                Ok(local.definition)
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

    fn node(&mut self, node: &WirNode) -> Result<(), FinalWirValidationError> {
        match node {
            WirNode::Source { body, .. } => self.seq(body),
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
            WirNode::Store { .. }
            | WirNode::Store8 { .. }
            | WirNode::MemoryCopy { .. }
            | WirNode::MemoryFill { .. } => Err(self.error(
                "raw linear-memory operation bypasses the semantic SafeWIR dialect",
            )),
            WirNode::SetLocal { .. } | WirNode::CallStoreMulti { .. } => Err(self.error(
                "mutable local definitions require the deferred reaching-definition graph",
            )),
            WirNode::CallIndirectStoreMulti { .. } => Err(self.error(
                "indirect multi-result calls require checked callable-target facts",
            )),
            WirNode::SetGlobal { .. } => Err(self.error(
                "global writes require the sealed kernel/global contract boundary",
            )),
            WirNode::If { cond, then_, els, result: None } => {
                let condition = self.scalar_definition(cond)?;
                let then_operations = self.nested_seq(then_)?;
                let else_operations = self.nested_seq(els)?;
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
                let body = self.nested_seq(body)?;
                self.controls.pop();
                self.operations.push(Operation::Block { control, body });
                Ok(())
            }
            WirNode::Loop { label, body } => {
                let control = self.push_control(label)?;
                let body = self.nested_seq(body)?;
                self.controls.pop();
                self.operations.push(Operation::Loop { control, body });
                Ok(())
            }
            WirNode::Br { target, cond } => {
                let target = self.resolve_control(target)?;
                let condition = cond
                    .as_ref()
                    .map(|condition| self.scalar_definition(condition))
                    .transpose()?;
                self.operations.push(Operation::Branch { target, condition });
                Ok(())
            }
            WirNode::Push(_) | WirNode::Return(Some(_)) => Err(self.error(
                "value returns require the deferred return-value def-use adapter",
            )),
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
                self.operations.push(Operation::Load {
                    place,
                    width: memory_width(*kind)?,
                });
                Ok(())
            }
            WirExpr::SafeLoad8U { place } => {
                let place = self.place(place)?;
                self.operations.push(Operation::Load { place, width: MemoryWidth::I8 });
                Ok(())
            }
            WirExpr::Load { .. } | WirExpr::Load8U { .. } | WirExpr::MemoryGrow(_) => Err(
                self.error("raw linear-memory expression bypasses the semantic SafeWIR dialect"),
            ),
            WirExpr::Call { func, args } => {
                if !args.is_empty() {
                    return Err(self.error(
                        "memory/scalar call arguments require the deferred def-use adapter",
                    ));
                }
                self.operations.push(Operation::Call {
                    callable: func.clone(),
                    arguments: Vec::new(),
                    result: None,
                });
                Ok(())
            }
            WirExpr::CallHost { import, args } => {
                if !args.is_empty() {
                    return Err(self.error(
                        "host arguments require authenticated scalar or buffer bindings",
                    ));
                }
                self.operations.push(Operation::HostCall {
                    import: import.clone(),
                    arguments: Vec::<HostArgument>::new(),
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
            | WirExpr::MemorySize
            | WirExpr::GetGlobal(_)
            | WirExpr::RefNull(_) => Ok(()),
        }
    }

    fn place(&self, place: &SafePlaceExpr) -> Result<Place, FinalWirValidationError> {
        if self.scalar_key(&place.object.root)? != self.scalar_key(&place.range.object_root)?
            || self.scalar_key(&place.object.root)? != self.scalar_key(&place.encoded_base)?
        {
            return Err(self.error(
                "object root, range root, and executable base are not the same final value",
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
        let start = size(&place.range.start)?;
        if start != SizeExpr::Constant(u64::from(place.encoded_offset)) {
            return Err(self.error(
                "executable immediate offset contradicts the claimed byte-range start",
            ));
        }
        let identity = ObjectIdentity {
            root: root.definition,
            generation: GenerationId::new(1),
        };
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
                extent: extent(&place.object.extent)?,
                provenance,
            },
            projection: projection(place)?,
            range: ByteRange {
                object: identity,
                start,
                length: size(&place.range.length)?,
            },
            access: place.access,
        })
    }

    fn require_constant_bulk_length(
        &self,
        executable: &WirExpr,
        first: &SafePlaceExpr,
        second: &SafePlaceExpr,
    ) -> Result<(), FinalWirValidationError> {
        let executable = constant_u64(executable)
            .ok_or_else(|| self.error("bulk length is not an exact nonnegative constant"))?;
        let first = size(&first.range.length)?;
        let second = size(&second.range.length)?;
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

fn contract_extent(extent: &ContractExtent) -> Result<Extent, FinalWirValidationError> {
    match extent {
        ContractExtent::Exact(size) => Ok(Extent::Exact(contract_size(size)?)),
        ContractExtent::Counted { .. } => Err(FinalWirValidationError::module(
            "symbolic counted parameter extents require the deferred def-use adapter",
        )),
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

fn extent(extent: &SafeExtentExpr) -> Result<Extent, FinalWirValidationError> {
    match extent {
        SafeExtentExpr::Unknown => Ok(Extent::Unknown),
        SafeExtentExpr::Exact(value) => Ok(Extent::Exact(size(value)?)),
        SafeExtentExpr::Counted { .. } => Err(FinalWirValidationError::module(
            "counted executable extents require exact scalar definition bindings",
        )),
    }
}

fn size(expression: &SafeSizeExpr) -> Result<SizeExpr, FinalWirValidationError> {
    match expression {
        SafeSizeExpr::Constant(value) => Ok(SizeExpr::Constant(*value)),
        SafeSizeExpr::Add(left, right) => Ok(SizeExpr::add(size(left)?, size(right)?)),
        SafeSizeExpr::Multiply(left, right) => {
            Ok(SizeExpr::multiply(size(left)?, size(right)?))
        }
        SafeSizeExpr::NarrowU32(value) => Ok(SizeExpr::narrow_u32(size(value)?)),
        SafeSizeExpr::Value(_) => Err(FinalWirValidationError::module(
            "symbolic size expressions require exact reaching definitions",
        )),
    }
}

fn projection(place: &SafePlaceExpr) -> Result<Projection, FinalWirValidationError> {
    let mut steps = Vec::with_capacity(place.projection.steps().len());
    for step in place.projection.steps() {
        steps.push(match step {
            SafeProjectionStep::Header { component } => {
                ProjectionStep::Header { component: *component }
            }
            SafeProjectionStep::Field { index } => ProjectionStep::Field { index: *index },
            SafeProjectionStep::Element { .. } | SafeProjectionStep::Slice { .. } => {
                return Err(FinalWirValidationError::module(
                    "indexed projections require exact reaching definitions",
                ));
            }
        });
    }
    Ok(Projection::from_steps(steps))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{
        ClosedTypeResolver, LayoutId, LayoutInterner, RcHeader, ReferenceKind, ResolvedNamed,
        ScalarKind,
    };
    use crate::safewir::{
        AccessMode, CallableMemoryResult, ParameterMemoryContract, ParameterMemoryEffect,
        ParameterMemoryEffects,
    };
    use crate::safewir_surface::{
        SafeObjectExpr, SafeProjectionExpr, SafeRangeExpr,
    };
    use crate::wir::{WirLocal, WirTy};
    use witchy_syntax::ast::Type;

    struct Resolver;

    impl ClosedTypeResolver for Resolver {
        fn resolve_named<'a>(
            &'a self,
            name: &str,
            _arguments: &[Type],
        ) -> Option<ResolvedNamed<'a>> {
            Some(match name {
                "Int" => ResolvedNamed::Scalar(ScalarKind::Int),
                "List" => ResolvedNamed::PackedList { rc: RcHeader::Required },
                "String" => ResolvedNamed::Reference(ReferenceKind::Owning),
                _ => return None,
            })
        }
    }

    fn layouts() -> (LayoutBundle, LayoutId) {
        let mut interner = LayoutInterner::new();
        let layout = interner
            .intern_type(&Type::Named("Int".into(), Vec::new()), &Resolver)
            .unwrap();
        (LayoutBundle::from_interner(&interner, [layout]).unwrap(), layout)
    }

    fn semantic_place(layout: LayoutId) -> Box<SafePlaceExpr> {
        let root = WirExpr::GetLocal("object".into());
        Box::new(SafePlaceExpr {
            object: SafeObjectExpr {
                root: Box::new(root.clone()),
                layout,
                extent: SafeExtentExpr::Exact(SafeSizeExpr::Constant(8)),
                provenance: SafeProvenanceExpr::Unknown,
            },
            projection: SafeProjectionExpr::root(),
            range: SafeRangeExpr {
                object_root: Box::new(root.clone()),
                start: SafeSizeExpr::Constant(0),
                length: SafeSizeExpr::Constant(8),
            },
            access: AccessMode::Read,
            encoded_base: Box::new(root),
            encoded_offset: 0,
        })
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
        FinalWirValidationInputs {
            layouts,
            functions: BTreeMap::from([(
                "read".into(),
                CallableMemoryContract {
                    parameters: vec![ParameterMemoryContract {
                        entry: ParameterMemoryEntry::BorrowedRead,
                        layout: Some(layout),
                        extent: Some(ContractExtent::Exact(ContractSizeExpr::Constant(8))),
                        effects: ParameterMemoryEffects::only(ParameterMemoryEffect::Read),
                    }],
                    result: CallableMemoryResult::NoMemory,
                },
            )]),
            imports: BTreeMap::new(),
            host_imports: BTreeMap::new(),
        }
    }

    #[test]
    fn final_wir_report_maps_one_parameter_rooted_semantic_load() {
        let (layouts, layout) = layouts();
        let report = validate_final_wir_report(
            &module(WirExpr::SafeLoad {
                place: semantic_place(layout),
                kind: Kind::I64,
            }),
            inputs(layouts, layout),
            ValidationBudget::default(),
        )
        .unwrap();
        assert_eq!(report.metrics().operations, 1);
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
            inputs(layouts, layout),
            ValidationBudget::default(),
        )
        .unwrap_err()
        .to_string()
        .contains("not the same final value"));
    }

    #[test]
    fn final_wir_report_rejects_same_width_scalar_reinterpretation() {
        let (layouts, layout) = layouts();
        let error = validate_final_wir_report(
            &module(WirExpr::SafeLoad {
                place: semantic_place(layout),
                kind: Kind::F64,
            }),
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
            unknown_result,
            ValidationBudget::default(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("result-definition adapter"), "{error}");

        let mut wrong_root = module(WirExpr::ConstI32(0));
        wrong_root.funcs[0].params[0].ty = WirTy::Int;
        let error = validate_final_wir_report(
            &wrong_root,
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
            inputs(layouts, layout),
            ValidationBudget::default(),
        )
        .unwrap();
        assert_eq!(report.metrics().operations, 3);
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
            duplicated_inputs,
            ValidationBudget::default(),
        )
        .unwrap_err()
        .to_string()
        .contains("exactly once"));
    }
}
