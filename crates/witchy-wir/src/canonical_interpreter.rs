//! Executable reference semantics for validated canonical SafeWIR.
//!
//! This interpreter intentionally operates on semantic operations rather than
//! decoding or sharing logic with the Wasm backend. Unsupported semantics fail
//! closed so differential execution cannot accidentally bless backend behavior
//! that the oracle does not model.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::canonical_safewir::{
    AllocationClass, ArithmeticOperator, BasicBlock, BlockId, CallArgument, CanonicalModule,
    ComparisonOperator, Extent, Function, FunctionId, HeaderField, HostCallArgument, LoadTransfer,
    Operation, InitialValid, OverflowBehavior, Place, Projection, StoreTransfer, Terminator,
    ValidatedSafeWir, ValueId,
};
use crate::layout::{HeaderLayout, LayoutBundle, LayoutId, LayoutKind, ScalarKind};

const SIZE_MAX: u64 = u32::MAX as u64;

mod launch;
mod nullable;
pub use launch::interpret_entry_program;

/// The canonical capability operations the oracle serves, by import name.
const CAPABILITY_IMPORTS: [&str; 7] = [
    "dir_read_into",
    "dir_list_into",
    "dir_exists_at",
    "dir_write_at",
    "dir_subtree_at",
    "env_has_var",
    "env_get_into",
];

/// The fixed ABI kinds of the tokens those operations take.
const DIR_KIND: u32 = crate::canonical_safewir::DIR_CAPABILITY_KIND.get();
const ENV_KIND: u32 = crate::canonical_safewir::ENV_CAPABILITY_KIND.get();

/// A value observable at a SafeWIR function boundary.
#[derive(Debug, Clone, PartialEq)]
pub enum InterpreterValue {
    Int(i64),
    FloatBits(u64),
    Bool(bool),
    Size(u64),
    Unit,
    V128([u8; 16]),
    InlineAggregate { layout: LayoutId, fields: Vec<InterpreterValue> },
    ObjectRef(u64),
    Function(FunctionId),
    /// An opaque host token for a capability parameter. The program never
    /// reads it; only the oracle's host calls do, and only for the authority
    /// the token's kind and scope carry.
    Capability(CapabilityToken),
    NullableCapability(Option<CapabilityToken>),
}

/// A host-minted capability token. Kind 1 (`Dir`) carries the subtree it is
/// confined to — the empty string for a root grant — and kind 2 (`Env`)
/// carries none. A token is only ever minted here: by the launcher for an
/// entry parameter, or by the host narrowing one it was handed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityToken {
    kind: u32,
    scope: String,
}

impl CapabilityToken {
    /// The root token a launcher mints for one entry capability parameter.
    pub fn root(kind: u32) -> Self {
        Self { kind, scope: String::new() }
    }

    pub fn kind(&self) -> u32 {
        self.kind
    }

    /// The subtree prefix this token is confined to, `""` at the root.
    pub fn scope(&self) -> &str {
        &self.scope
    }
}

impl InterpreterValue {
    /// The launcher's entry token for a capability parameter of `kind`.
    pub fn capability(kind: u32) -> Self {
        Self::Capability(CapabilityToken::root(kind))
    }

    /// The token a launcher mints for one entry capability parameter: a
    /// `Dir` parameter receives its granted root, scoped to the whole tree.
    pub fn entry_capability(capability: crate::CanonicalEntryCapability) -> Self {
        Self::capability(match capability {
            crate::CanonicalEntryCapability::Console => 0,
            crate::CanonicalEntryCapability::Dir => DIR_KIND,
            crate::CanonicalEntryCapability::Env => ENV_KIND,
        })
    }
}

/// The confined world the oracle serves `Dir` and `Env` host calls from.
/// Paths are relative and "/"-separated, without a leading "/", exactly as a
/// `Dir` grant addresses its root.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OracleFixture {
    pub files: BTreeMap<String, Vec<u8>>,
    pub environment: BTreeMap<String, String>,
    /// A directory the oracle reads when a path is not in `files` (the
    /// parity third leg runs against the same root the backend was granted);
    /// writes stay in `files`, so a program sees its own writes first.
    pub root: Option<std::path::PathBuf>,
    /// Consult the process environment for a name not in `environment`.
    pub process_environment: bool,
    /// How many root `Dir` grants the launcher holds (`None`: unlimited, for
    /// fixtures that model no launcher); an entry with more `Dir` parameters
    /// is refused, as the runtime refuses to mint an ungranted one.
    pub dir_grants: Option<usize>,
}

impl OracleFixture {
    /// Resolve `path` under the fallback root without following a symbolic
    /// link anywhere along it: the runtime's confined directory refuses a
    /// link that leads out of the root, and the oracle must be no weaker.
    fn root_entry(&self, path: &str) -> Option<std::path::PathBuf> {
        let root = self.root.as_ref()?;
        let mut resolved = root.clone();
        for component in path.split('/').filter(|component| !component.is_empty()) {
            resolved.push(component);
            let metadata = std::fs::symlink_metadata(&resolved).ok()?;
            if metadata.file_type().is_symlink() {
                return None;
            }
        }
        Some(resolved)
    }

    fn file(&self, path: &str) -> Option<Vec<u8>> {
        if let Some(contents) = self.files.get(path) {
            return Some(contents.clone());
        }
        let entry = self.root_entry(path)?;
        if !std::fs::symlink_metadata(&entry).ok()?.is_file() {
            return None;
        }
        std::fs::read(entry).ok()
    }

    fn exists(&self, path: &str) -> bool {
        let prefix = format!("{path}/");
        self.files.contains_key(path)
            || self.files.keys().any(|name| name.starts_with(&prefix))
            || self.root_entry(path).is_some()
    }

    /// The entry names directly under `scope` (the fixture map's, then the
    /// fallback root's, links listed by name but never followed), sorted.
    fn entry_names(&self, scope: &str) -> Vec<String> {
        let prefix = if scope.is_empty() { String::new() } else { format!("{scope}/") };
        let mut names = std::collections::BTreeSet::new();
        for key in self.files.keys() {
            if let Some(rest) = key.strip_prefix(&prefix)
                && let Some(first) = rest.split('/').next()
                && !first.is_empty()
            {
                names.insert(first.to_owned());
            }
        }
        let root = if scope.is_empty() { self.root.clone() } else { self.root_entry(scope) };
        if let Some(root) = root
            && let Ok(entries) = std::fs::read_dir(root)
        {
            for entry in entries.flatten() {
                if let Ok(name) = entry.file_name().into_string() {
                    names.insert(name);
                }
            }
        }
        names.into_iter().collect()
    }

    fn is_directory(&self, path: &str) -> bool {
        let prefix = format!("{path}/");
        self.files.keys().any(|name| name.starts_with(&prefix))
            || self
                .root_entry(path)
                .and_then(|entry| std::fs::symlink_metadata(entry).ok())
                .is_some_and(|metadata| metadata.is_dir())
    }

    fn variable(&self, name: &str) -> Option<String> {
        if let Some(value) = self.environment.get(name) {
            return Some(value.clone());
        }
        if self.process_environment {
            return std::env::var(name).ok();
        }
        None
    }
}

/// The observable outcome of running a program entry to completion.
#[derive(Debug, Clone, PartialEq)]
pub struct ProgramRun {
    pub results: Vec<InterpreterValue>,
    /// Lines the program printed through the console host call, in order.
    pub output: Vec<String>,
    /// Reference-counted objects still live when the entry returned. Under
    /// callee-drops every token is released by exit, so any survivor is a
    /// reclamation defect in the program's lowering.
    pub leaked_objects: usize,
}

/// A precise failure of the executable SafeWIR semantics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InterpretError {
    FunctionNotFound(FunctionId),
    BlockNotFound(BlockId),
    ArityMismatch {
        expected: usize,
        actual: usize,
    },
    MissingValue(ValueId),
    TypeMismatch(&'static str),
    ArithmeticTrap,
    GuardTrap,
    ExplicitTrap,
    UnreachableExecuted,
    StepLimitExceeded,
    ObjectNotFound(u64),
    ObjectNotReferenceCounted(u64),
    ReferenceCountOverflow(u64),
    ReferenceCountUnderflow(u64),
    ReclaimWithAliases {
        object: u64,
        references: u64,
    },
    ImmutableObject(u64),
    IndexOutOfBounds {
        index: u64,
        length: u64,
    },
    CountExceedsCapacity {
        count: u64,
        capacity: u64,
    },
    UninitializedPlace,
    UnsupportedOperation(&'static str),
}

impl fmt::Display for InterpretError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for InterpretError {}

/// Bounds execution so malformed or accidentally nonterminating fixtures do
/// not hang a differential test, and carries the host world the capability
/// imports observe.
#[derive(Debug, Clone)]
pub struct InterpreterConfig {
    pub max_steps: u64,
    pub fixture: OracleFixture,
}

impl Default for InterpreterConfig {
    fn default() -> Self {
        Self { max_steps: 1_000_000, fixture: OracleFixture::default() }
    }
}

/// Execute one function from an independently validated SafeWIR module.
pub fn interpret(
    module: &ValidatedSafeWir,
    function: FunctionId,
    arguments: Vec<InterpreterValue>,
) -> Result<Vec<InterpreterValue>, InterpretError> {
    interpret_with_config(module, function, arguments, InterpreterConfig::default())
}

pub fn interpret_with_config(
    module: &ValidatedSafeWir,
    function: FunctionId,
    arguments: Vec<InterpreterValue>,
    config: InterpreterConfig,
) -> Result<Vec<InterpreterValue>, InterpretError> {
    Engine::new(module.module(), config).execute(function, arguments)
}

/// Run a program entry as the launcher would: capability tokens in, console
/// output captured, and the heap audited for objects the program never
/// released. This is the execution oracle for admitted programs: its
/// semantics are the checker's, independent of the Wasm backend.
pub fn interpret_program(
    module: &ValidatedSafeWir,
    function: FunctionId,
    arguments: Vec<InterpreterValue>,
    config: InterpreterConfig,
) -> Result<ProgramRun, InterpretError> {
    // The launcher mints one root `Dir` token per grant; an entry asking for
    // more than the fixture grants is refused before it runs.
    if let Some(grants) = config.fixture.dir_grants {
        let roots = arguments
            .iter()
            .filter(|argument| matches!(argument, InterpreterValue::Capability(token) if token.kind == DIR_KIND && token.scope.is_empty()))
            .count();
        if roots > grants {
            return Err(InterpretError::ExplicitTrap);
        }
    }
    let mut engine = Engine::new(module.module(), config);
    let results = engine.execute(function, arguments)?;
    let leaked_objects = engine
        .objects
        .values()
        .filter(|object| matches!(object.storage, RuntimeStorage::Allocation(_)))
        .count();
    Ok(ProgramRun { results, output: engine.output, leaked_objects })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum CellKey {
    Field(u32),
    Element(u64),
}

#[derive(Debug, PartialEq, Eq)]
enum RuntimeExtent {
    FixedBytes(u64),
    HeaderPrefix(u32),
    Counted {
        header_bytes: u32,
        count: u64,
        capacity: u64,
        stride_bytes: u32,
    },
}

#[derive(Debug)]
struct Object {
    layout: LayoutId,
    storage: RuntimeStorage,
    extent: RuntimeExtent,
    references: u64,
    immutable: bool,
    cells: BTreeMap<CellKey, InterpreterValue>,
    // Initialized absence is storage state, never an SSA reference value.
    absent_owned_slots: BTreeSet<CellKey>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RangeTransfer {
    Copy,
    Move,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimeStorage {
    Allocation(AllocationClass),
    StaticData,
}

struct Engine<'a> {
    module: &'a CanonicalModule<InitialValid>,
    config: InterpreterConfig,
    steps: u64,
    next_object: u64,
    objects: BTreeMap<u64, Object>,
    static_objects: BTreeMap<crate::canonical_safewir::StaticDataId, u64>,
    output: Vec<String>,
}

enum FunctionExit {
    Return(Vec<InterpreterValue>),
    TailCall(FunctionId, Vec<InterpreterValue>),
}

impl<'a> Engine<'a> {
    fn new(module: &'a CanonicalModule<InitialValid>, config: InterpreterConfig) -> Self {
        Self {
            module,
            config,
            steps: 0,
            next_object: 0,
            objects: BTreeMap::new(),
            static_objects: BTreeMap::new(),
            output: Vec::new(),
        }
    }

    fn execute(
        &mut self,
        mut function: FunctionId,
        mut arguments: Vec<InterpreterValue>,
    ) -> Result<Vec<InterpreterValue>, InterpretError> {
        loop {
            match self.execute_frame(function, arguments)? {
                FunctionExit::Return(values) => return Ok(values),
                FunctionExit::TailCall(next, next_arguments) => {
                    function = next;
                    arguments = next_arguments;
                }
            }
        }
    }

    fn execute_frame(
        &mut self,
        function_id: FunctionId,
        arguments: Vec<InterpreterValue>,
    ) -> Result<FunctionExit, InterpretError> {
        let function = self.function(function_id)?.clone();
        let entry = self.block(&function, function.entry)?;
        let mut values = BTreeMap::new();
        self.bind_parameters(entry, arguments, &mut values)?;
        let mut block_id = function.entry;

        loop {
            let block = self.block(&function, block_id)?.clone();
            for operation in &block.operations {
                self.bump_step()?;
                self.execute_operation(operation, &mut values)?;
            }

            self.bump_step()?;
            match &block.terminator {
                Terminator::Branch { target, arguments } => {
                    let next = self.read_values(arguments, &values)?;
                    self.bind_parameters(self.block(&function, *target)?, next, &mut values)?;
                    block_id = *target;
                }
                Terminator::BranchIf {
                    condition,
                    then_target,
                    then_arguments,
                    else_target,
                    else_arguments,
                } => {
                    let condition = self.bool_value(*condition, &values)?;
                    let (target, arguments) = if condition {
                        (*then_target, then_arguments)
                    } else {
                        (*else_target, else_arguments)
                    };
                    let next = self.read_values(arguments, &values)?;
                    self.bind_parameters(self.block(&function, target)?, next, &mut values)?;
                    block_id = target;
                }
                Terminator::Return { values: returned } => {
                    return Ok(FunctionExit::Return(self.read_values(returned, &values)?));
                }
                Terminator::TailCall {
                    callee, arguments, ..
                } => {
                    let arguments = self.take_call_arguments(arguments, &mut values)?;
                    return Ok(FunctionExit::TailCall(*callee, arguments));
                }
                Terminator::VarTailCall {
                    callee, arguments, ..
                } => {
                    let callee = self.function_value(*callee, &values)?;
                    let arguments = self.take_call_arguments(arguments, &mut values)?;
                    return Ok(FunctionExit::TailCall(callee, arguments));
                }
                Terminator::Trap => return Err(InterpretError::ExplicitTrap),
                Terminator::Unreachable => return Err(InterpretError::UnreachableExecuted),
            }
        }
    }

    fn execute_operation(
        &mut self,
        operation: &Operation,
        values: &mut BTreeMap<ValueId, InterpreterValue>,
    ) -> Result<(), InterpretError> {
        match operation {
            Operation::ConstInt { destination, value } => {
                values.insert(*destination, InterpreterValue::Int(*value));
            }
            Operation::ConstFloat { destination, bits } => {
                values.insert(*destination, InterpreterValue::FloatBits(*bits));
            }
            Operation::ConstBool { destination, value } => {
                values.insert(*destination, InterpreterValue::Bool(*value));
            }
            Operation::ConstSize { destination, value } => {
                values.insert(*destination, InterpreterValue::Size(self.size_from_u64(*value)?));
            }
            Operation::CapabilityNone { destination, .. } => {
                values.insert(*destination, InterpreterValue::NullableCapability(None));
            }
            Operation::CapabilitySome { destination, source } => {
                let InterpreterValue::Capability(token) = self.value(*source, values)? else { return Err(InterpretError::TypeMismatch("capability token")); };
                values.insert(*destination, InterpreterValue::NullableCapability(Some(token.clone())));
            }
            Operation::CapabilityIsPresent { destination, source } => {
                let InterpreterValue::NullableCapability(value) = self.value(*source, values)? else { return Err(InterpretError::TypeMismatch("nullable capability")); };
                values.insert(*destination, InterpreterValue::Bool(value.is_some()));
            }
            Operation::CapabilityUnwrap { destination, source } => {
                let InterpreterValue::NullableCapability(value) = self.value(*source, values)? else { return Err(InterpretError::TypeMismatch("nullable capability")); };
                values.insert(*destination, InterpreterValue::Capability(value.clone().ok_or(InterpretError::ExplicitTrap)?));
            }
            Operation::MakeInlineAggregate { destination, layout, fields } => {
                values.insert(*destination, InterpreterValue::InlineAggregate {
                    layout: *layout,
                    fields: self.read_values(fields, values)?,
                });
            }
            Operation::ExtractInlineField { destination, aggregate, index } => {
                let InterpreterValue::InlineAggregate { fields, .. } = self.value(*aggregate, values)? else {
                    return Err(InterpretError::TypeMismatch("inline aggregate"));
                };
                let field = fields.get(*index as usize).cloned().ok_or(InterpretError::TypeMismatch("inline aggregate index"))?;
                values.insert(*destination, field);
            }
            Operation::Arithmetic {
                destination,
                operator,
                left,
                right,
                overflow,
            } => {
                let result = self.arithmetic(
                    *operator,
                    self.value(*left, values)?,
                    self.value(*right, values)?,
                    *overflow,
                )?;
                values.insert(*destination, result);
            }
            Operation::Compare {
                destination,
                operator,
                left,
                right,
            } => {
                let result = self.compare(
                    *operator,
                    self.value(*left, values)?,
                    self.value(*right, values)?,
                )?;
                values.insert(*destination, InterpreterValue::Bool(result));
            }
            Operation::NarrowSize { destination, value } => {
                let value = self.int_value(*value, values)?;
                let narrowed = u32::try_from(value).map_err(|_| InterpretError::ArithmeticTrap)?;
                values.insert(*destination, InterpreterValue::Size(u64::from(narrowed)));
            }
            Operation::WidenSize { destination, value } => {
                let value = self.size_value(*value, values)?;
                let widened = i64::try_from(value).map_err(|_| InterpretError::ArithmeticTrap)?;
                values.insert(*destination, InterpreterValue::Int(widened));
            }
            Operation::FloatUnary { destination, operator, value } => {
                let result = match operator {
                    crate::canonical_safewir::FloatUnaryOperator::FromInt => {
                        let operand = self.int_value(*value, values)?;
                        InterpreterValue::FloatBits((operand as f64).to_bits())
                    }
                    operator => {
                        let InterpreterValue::FloatBits(bits) = self.value(*value, values)? else {
                            return Err(InterpretError::TypeMismatch("FloatUnary operand"));
                        };
                        let operand = f64::from_bits(*bits);
                        match operator {
                            crate::canonical_safewir::FloatUnaryOperator::Sqrt => InterpreterValue::FloatBits(operand.sqrt().to_bits()),
                            // `as` truncates toward zero, saturates, and maps
                            // NaN to zero: the contract of `i64.trunc_sat_f64_s`.
                            crate::canonical_safewir::FloatUnaryOperator::TruncateToInt => InterpreterValue::Int(operand as i64),
                            crate::canonical_safewir::FloatUnaryOperator::FromInt => unreachable!("handled above"),
                        }
                    }
                };
                values.insert(*destination, result);
            }
            Operation::Guard { condition, .. } => {
                if !self.bool_value(*condition, values)? {
                    return Err(InterpretError::GuardTrap);
                }
            }
            Operation::Alloc {
                destination,
                layout,
                extent,
                class,
            } => {
                let extent = self.runtime_extent(extent, values)?;
                let object = self.allocate(*layout, *class, extent, false)?;
                values.insert(*destination, InterpreterValue::ObjectRef(object));
            }
            Operation::StaticData {
                destination,
                layout,
                extent,
                data,
            } => {
                let extent = self.runtime_extent(extent, values)?;
                let bytes = self
                    .module
                    .static_data_segments()
                    .iter()
                    .find(|segment| segment.id() == *data)
                    .map(|segment| segment.bytes())
                    .ok_or(InterpretError::UnsupportedOperation("StaticData missing segment"))?;
                let object = if let Some(object) = self.static_objects.get(data) {
                    let existing = self.object(*object)?;
                    if existing.layout != *layout || existing.extent != extent {
                        return Err(InterpretError::UnsupportedOperation(
                            "StaticData inconsistent segment reuse",
                        ));
                    }
                    *object
                } else {
                    let object = self.allocate_static(*layout, extent, bytes)?;
                    self.static_objects.insert(*data, object);
                    object
                };
                values.insert(*destination, InterpreterValue::ObjectRef(object));
            }
            Operation::Load {
                destination,
                place,
                transfer,
            } => {
                let value = self.load(place, *transfer, values)?;
                values.insert(*destination, value);
            }
            Operation::Store {
                place,
                value,
                transfer,
            } => self.store(place, *value, *transfer, values)?,
            Operation::InitNullableOwned { place } => {
                self.init_nullable_owned(place, values)?;
            }
            Operation::CopyBytes { destination, source, length } => {
                self.copy_range(destination, source, *length, RangeTransfer::Copy, values)?;
            }
            Operation::MoveRange { destination, source, count } => {
                self.copy_range(destination, source, *count, RangeTransfer::Move, values)?;
            }
            Operation::CloneRange { destination, source, count } => {
                self.copy_range(destination, source, *count, RangeTransfer::Copy, values)?;
            }
            Operation::UpdateCount { object, count } => {
                let object = self.object_value(*object, values)?;
                let count = self.size_value(*count, values)?;
                self.update_count(object, count)?;
            }
            Operation::MoveValue { destination, source } => {
                // A reference move transfers the token; a scalar move is a
                // copy, as the checker and the backend both treat it.
                let value = self.value(*source, values)?.clone();
                if matches!(value, InterpreterValue::ObjectRef(_)) {
                    values.remove(source);
                }
                values.insert(*destination, value);
            }
            Operation::CloneRef { destination, source } => {
                let object = self.object_value(*source, values)?;
                self.retain(object)?;
                values.insert(*destination, InterpreterValue::ObjectRef(object));
            }
            Operation::Retain { object } => {
                let object = self.object_value(*object, values)?;
                self.retain(object)?;
            }
            Operation::Release { object } | Operation::DropRef { object } => {
                let object = self.object_value(*object, values)?;
                self.release(object)?;
            }
            Operation::Reclaim { object } => {
                let object = self.object_value(*object, values)?;
                self.reclaim(object)?;
            }
            Operation::FunctionValue {
                destination,
                function,
            } => {
                values.insert(*destination, InterpreterValue::Function(*function));
            }
            Operation::Call {
                destinations,
                callee,
                arguments,
            } => {
                let arguments = self.take_call_arguments(arguments, values)?;
                let returned = self.execute(*callee, arguments)?;
                self.bind_results(destinations, returned, values)?;
            }
            Operation::VarCall {
                destinations,
                callee,
                arguments,
                ..
            } => {
                let callee = self.function_value(*callee, values)?;
                let arguments = self.take_call_arguments(arguments, values)?;
                let returned = self.execute(callee, arguments)?;
                self.bind_results(destinations, returned, values)?;
            }
            Operation::HostCall { destinations, import, arguments } => {
                self.host_call(*import, destinations, arguments, values)?;
            }
        }
        Ok(())
    }

    /// The two prelude imports are the host calls the oracle models: `print`
    /// reads the buffer's bytes and records a line; `float_to_str` writes a
    /// `Float`'s shortest round-trip decimal (the shared `render_float`
    /// rule) into the buffer and returns the byte length. Every other import
    /// is an effect the oracle does not model and fails closed.
    fn host_call(
        &mut self,
        import: crate::canonical_safewir::ImportId,
        destinations: &[ValueId],
        arguments: &[HostCallArgument],
        values: &mut BTreeMap<ValueId, InterpreterValue>,
    ) -> Result<(), InterpretError> {
        let contract = self
            .module
            .imports()
            .iter()
            .find(|contract| contract.id == import)
            .ok_or(InterpretError::UnsupportedOperation("HostCall unknown import"))?;
        if contract.module != "witchy" {
            return Err(InterpretError::UnsupportedOperation("HostCall import"));
        }
        if contract.name == "float_to_str" {
            let ([HostCallArgument::Value(value), HostCallArgument::Buffer { place, .. }], [destination]) = (arguments, destinations) else {
                return Err(InterpretError::UnsupportedOperation("HostCall float_to_str arguments"));
            };
            let InterpreterValue::FloatBits(bits) = self.value(*value, values)? else {
                return Err(InterpretError::TypeMismatch("float_to_str operand"));
            };
            let rendered = witchy_syntax::fmt::render_float(f64::from_bits(*bits)).into_bytes();
            let (object, start, length) = self.slice_place(place, values)?;
            self.check_range(object, start, length, true)?;
            let written = u64::try_from(rendered.len()).map_err(|_| InterpretError::ArithmeticTrap)?;
            if written > length {
                return Err(InterpretError::IndexOutOfBounds { index: written, length });
            }
            let cells = &mut self.object_mut(object)?.cells;
            for (offset, byte) in rendered.iter().enumerate() {
                cells.insert(CellKey::Element(start + offset as u64), InterpreterValue::Size(u64::from(*byte)));
            }
            values.insert(*destination, InterpreterValue::Size(written));
            return Ok(());
        }
        if let Some(name) = CAPABILITY_IMPORTS.iter().find(|name| **name == contract.name) {
            return self.capability_host_call(name, destinations, arguments, values);
        }
        if contract.name != "print" || !destinations.is_empty() {
            return Err(InterpretError::UnsupportedOperation("HostCall import"));
        }
        let [HostCallArgument::Buffer { place, .. }] = arguments else {
            return Err(InterpretError::UnsupportedOperation("HostCall print arguments"));
        };
        let (object, start, length) = self.slice_place(place, values)?;
        self.check_range(object, start, length, false)?;
        let cells = &self.object(object)?.cells;
        let mut bytes = Vec::with_capacity(length as usize);
        for index in start..start.checked_add(length).ok_or(InterpretError::ArithmeticTrap)? {
            match cells.get(&CellKey::Element(index)) {
                Some(InterpreterValue::Size(byte)) => bytes.push(u8::try_from(*byte).map_err(|_| InterpretError::TypeMismatch("byte cell"))?),
                Some(_) => return Err(InterpretError::TypeMismatch("byte cell")),
                None => return Err(InterpretError::UninitializedPlace),
            }
        }
        // The console host records a line with its trailing newlines removed.
        self.output.push(String::from_utf8_lossy(&bytes).trim_end_matches('\n').to_owned());
        Ok(())
    }

    /// The canonical `Dir`/`Env` operations, served from the fixture. Every
    /// refusal the runtime host would trap on — a missing file, an unset
    /// variable, a path that escapes the token's subtree — is an abort here,
    /// never a different answer.
    fn capability_host_call(
        &mut self,
        name: &str,
        destinations: &[ValueId],
        arguments: &[HostCallArgument],
        values: &mut BTreeMap<ValueId, InterpreterValue>,
    ) -> Result<(), InterpretError> {
        match (name, arguments, destinations) {
            (
                "dir_read_into",
                [token, HostCallArgument::Buffer { place: path, .. }, HostCallArgument::Buffer { place: out, .. }],
                [destination],
            ) => {
                let scope = self.capability_scope(token, DIR_KIND, values)?;
                let path = self.host_path(&scope, &self.host_string(path, values)?)?;
                // A `dir.read` is text: the runtime refuses a file that is
                // not UTF-8, so the oracle does too.
                let contents = self.config.fixture.file(&path).ok_or(InterpretError::ExplicitTrap)?;
                if std::str::from_utf8(&contents).is_err() {
                    return Err(InterpretError::ExplicitTrap);
                }
                let length = self.fill_host_buffer(out, &contents, values)?;
                values.insert(*destination, InterpreterValue::Size(length));
                Ok(())
            }
            ("dir_list_into", [token, HostCallArgument::Buffer { place: out, .. }], [destination]) => {
                let scope = self.capability_scope(token, DIR_KIND, values)?;
                let payload = self.config.fixture.entry_names(&scope).join("\0");
                let length = self.fill_host_buffer(out, payload.as_bytes(), values)?;
                values.insert(*destination, InterpreterValue::Size(length));
                Ok(())
            }
            ("dir_exists_at", [token, HostCallArgument::Buffer { place: path, .. }], [destination]) => {
                let scope = self.capability_scope(token, DIR_KIND, values)?;
                let path = self.host_path(&scope, &self.host_string(path, values)?)?;
                let exists = self.config.fixture.exists(&path);
                values.insert(*destination, InterpreterValue::Size(u64::from(exists)));
                Ok(())
            }
            (
                "dir_write_at",
                [token, HostCallArgument::Buffer { place: path, .. }, HostCallArgument::Buffer { place: data, .. }],
                [],
            ) => {
                let scope = self.capability_scope(token, DIR_KIND, values)?;
                let path = self.host_path(&scope, &self.host_string(path, values)?)?;
                let data = self.host_bytes(data, values)?;
                self.config.fixture.files.insert(path, data);
                Ok(())
            }
            ("dir_subtree_at", [token, HostCallArgument::Buffer { place: path, .. }], [destination]) => {
                let scope = self.capability_scope(token, DIR_KIND, values)?;
                let scope = self.host_path(&scope, &self.host_string(path, values)?)?;
                // Narrowing opens the directory: a missing one traps, as the
                // runtime's confined open does.
                if !self.config.fixture.is_directory(&scope) {
                    return Err(InterpretError::ExplicitTrap);
                }
                values.insert(*destination, InterpreterValue::Capability(CapabilityToken { kind: DIR_KIND, scope }));
                Ok(())
            }
            ("env_has_var", [token, HostCallArgument::Buffer { place: name, .. }], [destination]) => {
                let _ = self.capability_scope(token, ENV_KIND, values)?;
                let name = self.host_string(name, values)?;
                let present = self.config.fixture.variable(&name).is_some();
                values.insert(*destination, InterpreterValue::Size(u64::from(present)));
                Ok(())
            }
            (
                "env_get_into",
                [token, HostCallArgument::Buffer { place: name, .. }, HostCallArgument::Buffer { place: out, .. }],
                [destination],
            ) => {
                let _ = self.capability_scope(token, ENV_KIND, values)?;
                let name = self.host_string(name, values)?;
                let value = self.config.fixture.variable(&name).ok_or(InterpretError::ExplicitTrap)?;
                let length = self.fill_host_buffer(out, value.as_bytes(), values)?;
                values.insert(*destination, InterpreterValue::Size(length));
                Ok(())
            }
            _ => Err(InterpretError::UnsupportedOperation("HostCall capability arguments")),
        }
    }

    /// The subtree a capability argument of the expected kind is confined to.
    fn capability_scope(
        &self,
        argument: &HostCallArgument,
        kind: u32,
        values: &BTreeMap<ValueId, InterpreterValue>,
    ) -> Result<String, InterpretError> {
        let HostCallArgument::Capability(value) = argument else {
            return Err(InterpretError::UnsupportedOperation("HostCall capability argument"));
        };
        let InterpreterValue::Capability(token) = self.value(*value, values)? else {
            return Err(InterpretError::TypeMismatch("capability token"));
        };
        if token.kind != kind {
            return Err(InterpretError::TypeMismatch("capability kind"));
        }
        Ok(token.scope.clone())
    }

    /// A checked read slice's bytes.
    fn host_bytes(
        &self,
        place: &Place,
        values: &BTreeMap<ValueId, InterpreterValue>,
    ) -> Result<Vec<u8>, InterpretError> {
        let (object, start, length) = self.slice_place(place, values)?;
        self.check_range(object, start, length, false)?;
        let cells = &self.object(object)?.cells;
        let mut bytes = Vec::with_capacity(length as usize);
        for index in start..start.checked_add(length).ok_or(InterpretError::ArithmeticTrap)? {
            match cells.get(&CellKey::Element(index)) {
                Some(InterpreterValue::Size(byte)) => bytes.push(u8::try_from(*byte).map_err(|_| InterpretError::TypeMismatch("byte cell"))?),
                Some(_) => return Err(InterpretError::TypeMismatch("byte cell")),
                None => return Err(InterpretError::UninitializedPlace),
            }
        }
        Ok(bytes)
    }

    /// A path or variable name arrives as a string slice, so it is UTF-8 or
    /// the host refuses it.
    fn host_string(
        &self,
        place: &Place,
        values: &BTreeMap<ValueId, InterpreterValue>,
    ) -> Result<String, InterpretError> {
        String::from_utf8(self.host_bytes(place, values)?)
            .map_err(|_| InterpretError::TypeMismatch("host string is not UTF-8"))
    }

    /// Confine one relative path to a token's subtree, refusing exactly what
    /// the runtime's `dir_guard` refuses: an absolute path, a `..` or `.`
    /// component, and an empty component.
    fn host_path(&self, scope: &str, path: &str) -> Result<String, InterpretError> {
        if path.starts_with('/') {
            return Err(InterpretError::ExplicitTrap);
        }
        // An empty component (`a//b`) is tolerated like the runtime's
        // confined path; `.` and `..` are refused.
        let components = path.split('/').filter(|component| !component.is_empty()).collect::<Vec<_>>();
        if components.iter().any(|component| *component == "." || *component == "..") {
            return Err(InterpretError::ExplicitTrap);
        }
        let path = components.join("/");
        Ok(if scope.is_empty() { path } else if path.is_empty() { scope.to_owned() } else { format!("{scope}/{path}") })
    }

    /// Write the leading bytes that fit into a checked write slice and answer
    /// the payload's FULL length: the guest publishes `min(length, capacity)`
    /// itself, under its own guard.
    fn fill_host_buffer(
        &mut self,
        place: &Place,
        payload: &[u8],
        values: &BTreeMap<ValueId, InterpreterValue>,
    ) -> Result<u64, InterpretError> {
        let (object, start, capacity) = self.slice_place(place, values)?;
        self.check_range(object, start, capacity, true)?;
        let full = u64::try_from(payload.len()).map_err(|_| InterpretError::ArithmeticTrap)?;
        if full > i32::MAX as u64 {
            return Err(InterpretError::ArithmeticTrap);
        }
        let written = full.min(capacity);
        let cells = &mut self.object_mut(object)?.cells;
        for offset in 0..written {
            let byte = payload[offset as usize];
            cells.insert(CellKey::Element(start + offset), InterpreterValue::Size(u64::from(byte)));
        }
        Ok(full)
    }

    fn slice_place(
        &self,
        place: &Place,
        values: &BTreeMap<ValueId, InterpreterValue>,
    ) -> Result<(u64, u64, u64), InterpretError> {
        let object = self.object_value(place.object(), values)?;
        let Projection::Slice { start, length } = place.projection() else {
            return Err(InterpretError::UnsupportedOperation("range place"));
        };
        Ok((object, self.size_value(*start, values)?, self.size_value(*length, values)?))
    }

    /// A range must lie inside the logical count for reads and inside the
    /// capacity for writes, exactly as the checker's range rule bounds it.
    fn check_range(&self, object: u64, start: u64, length: u64, write: bool) -> Result<(), InterpretError> {
        let RuntimeExtent::Counted { count, capacity, .. } = &self.object(object)?.extent else {
            return Err(InterpretError::TypeMismatch("range on uncounted object"));
        };
        let bound = if write { *capacity } else { *count };
        let end = start.checked_add(length).ok_or(InterpretError::ArithmeticTrap)?;
        if end > bound {
            return Err(InterpretError::IndexOutOfBounds { index: end, length: bound });
        }
        Ok(())
    }

    /// `CopyBytes`/`CloneRange` copy cells; `MoveRange` moves them, which
    /// carries each child token across without a retain.
    fn copy_range(
        &mut self,
        destination: &Place,
        source: &Place,
        length: ValueId,
        transfer: RangeTransfer,
        values: &BTreeMap<ValueId, InterpreterValue>,
    ) -> Result<(), InterpretError> {
        let length = self.size_value(length, values)?;
        let (source_object, source_start, source_length) = self.slice_place(source, values)?;
        let (destination_object, destination_start, destination_length) = self.slice_place(destination, values)?;
        if source_length != length || destination_length != length {
            return Err(InterpretError::TypeMismatch("range length"));
        }
        if source_object == destination_object {
            return Err(InterpretError::UnsupportedOperation("overlapping range"));
        }
        self.check_range(source_object, source_start, length, false)?;
        self.check_range(destination_object, destination_start, length, true)?;
        if self.object(destination_object)?.immutable {
            return Err(InterpretError::ImmutableObject(destination_object));
        }
        let mut moved = Vec::with_capacity(length as usize);
        for offset in 0..length {
            let key = CellKey::Element(source_start.checked_add(offset).ok_or(InterpretError::ArithmeticTrap)?);
            let cell = match transfer {
                RangeTransfer::Copy => self.object(source_object)?.cells.get(&key).cloned(),
                RangeTransfer::Move => self.object_mut(source_object)?.cells.remove(&key),
            };
            moved.push(cell.ok_or(InterpretError::UninitializedPlace)?);
        }
        // A copied reference cell is a second owner of its object.
        if matches!(transfer, RangeTransfer::Copy) {
            for cell in &moved {
                if let InterpreterValue::ObjectRef(object) = cell {
                    self.retain(*object)?;
                }
            }
        }
        let cells = &mut self.object_mut(destination_object)?.cells;
        for (offset, cell) in moved.into_iter().enumerate() {
            let key = CellKey::Element(destination_start.checked_add(offset as u64).ok_or(InterpretError::ArithmeticTrap)?);
            // The checker requires the destination range to be empty; the
            // oracle states the same rule so an overwritten reference cell
            // can never be silently dropped.
            if matches!(cells.get(&key), Some(InterpreterValue::ObjectRef(_))) {
                return Err(InterpretError::UnsupportedOperation("range copy over a stored reference"));
            }
            cells.insert(key, cell);
        }
        Ok(())
    }

    fn arithmetic(
        &self,
        operator: ArithmeticOperator,
        left: &InterpreterValue,
        right: &InterpreterValue,
        overflow: OverflowBehavior,
    ) -> Result<InterpreterValue, InterpretError> {
        match (left, right) {
            (InterpreterValue::Int(left), InterpreterValue::Int(right)) => {
                self.int_arithmetic(operator, *left, *right, overflow)
                    .map(InterpreterValue::Int)
            }
            (InterpreterValue::Size(left), InterpreterValue::Size(right)) => {
                self.size_arithmetic(
                    operator,
                    self.size_from_u64(*left)? as u32,
                    self.size_from_u64(*right)? as u32,
                    overflow,
                )
                .map(|value| InterpreterValue::Size(u64::from(value)))
            }
            (InterpreterValue::FloatBits(left), InterpreterValue::FloatBits(right)) => {
                self.float_arithmetic(operator, *left, *right)
                    .map(InterpreterValue::FloatBits)
            }
            _ => Err(InterpretError::TypeMismatch("arithmetic operands")),
        }
    }

    fn int_arithmetic(
        &self,
        operator: ArithmeticOperator,
        left: i64,
        right: i64,
        overflow: OverflowBehavior,
    ) -> Result<i64, InterpretError> {
        let checked = |value: Option<i64>| value.ok_or(InterpretError::ArithmeticTrap);
        match operator {
            ArithmeticOperator::Add => match overflow {
                OverflowBehavior::Wrap => Ok(left.wrapping_add(right)),
                OverflowBehavior::Trap => checked(left.checked_add(right)),
            },
            ArithmeticOperator::Subtract => match overflow {
                OverflowBehavior::Wrap => Ok(left.wrapping_sub(right)),
                OverflowBehavior::Trap => checked(left.checked_sub(right)),
            },
            ArithmeticOperator::Multiply => match overflow {
                OverflowBehavior::Wrap => Ok(left.wrapping_mul(right)),
                OverflowBehavior::Trap => checked(left.checked_mul(right)),
            },
            ArithmeticOperator::DivideSigned => checked(left.checked_div(right)),
            ArithmeticOperator::DivideUnsigned => {
                let right = right as u64;
                if right == 0 {
                    Err(InterpretError::ArithmeticTrap)
                } else {
                    Ok(((left as u64) / right) as i64)
                }
            }
            ArithmeticOperator::RemainderSigned => {
                if right == 0 {
                    Err(InterpretError::ArithmeticTrap)
                } else if left == i64::MIN && right == -1 {
                    Ok(0)
                } else {
                    checked(left.checked_rem(right))
                }
            }
            ArithmeticOperator::RemainderUnsigned => {
                let right = right as u64;
                if right == 0 {
                    Err(InterpretError::ArithmeticTrap)
                } else {
                    Ok(((left as u64) % right) as i64)
                }
            }
            ArithmeticOperator::BitAnd => Ok(left & right),
            ArithmeticOperator::BitOr => Ok(left | right),
            ArithmeticOperator::BitXor => Ok(left ^ right),
            ArithmeticOperator::ShiftLeft => Ok(left.wrapping_shl((right as u32) & 63)),
            ArithmeticOperator::ShiftRightSigned => Ok(left.wrapping_shr((right as u32) & 63)),
            ArithmeticOperator::ShiftRightUnsigned => {
                Ok(((left as u64) >> ((right as u32) & 63)) as i64)
            }
        }
    }

    fn size_arithmetic(
        &self,
        operator: ArithmeticOperator,
        left: u32,
        right: u32,
        overflow: OverflowBehavior,
    ) -> Result<u32, InterpretError> {
        let checked = |value: Option<u32>| value.ok_or(InterpretError::ArithmeticTrap);
        let signed_checked = |value: Option<i32>| {
            value
                .map(|value| value as u32)
                .ok_or(InterpretError::ArithmeticTrap)
        };
        match operator {
            ArithmeticOperator::Add => match overflow {
                OverflowBehavior::Wrap => Ok(left.wrapping_add(right)),
                OverflowBehavior::Trap => checked(left.checked_add(right)),
            },
            ArithmeticOperator::Subtract => match overflow {
                OverflowBehavior::Wrap => Ok(left.wrapping_sub(right)),
                OverflowBehavior::Trap => checked(left.checked_sub(right)),
            },
            ArithmeticOperator::Multiply => match overflow {
                OverflowBehavior::Wrap => Ok(left.wrapping_mul(right)),
                OverflowBehavior::Trap => checked(left.checked_mul(right)),
            },
            ArithmeticOperator::DivideSigned => {
                signed_checked((left as i32).checked_div(right as i32))
            }
            ArithmeticOperator::DivideUnsigned => checked(left.checked_div(right)),
            ArithmeticOperator::RemainderSigned => {
                let left = left as i32;
                let right = right as i32;
                if right == 0 {
                    Err(InterpretError::ArithmeticTrap)
                } else if left == i32::MIN && right == -1 {
                    Ok(0)
                } else {
                    signed_checked(left.checked_rem(right))
                }
            }
            ArithmeticOperator::RemainderUnsigned => checked(left.checked_rem(right)),
            ArithmeticOperator::BitAnd => Ok(left & right),
            ArithmeticOperator::BitOr => Ok(left | right),
            ArithmeticOperator::BitXor => Ok(left ^ right),
            ArithmeticOperator::ShiftLeft => Ok(left.wrapping_shl(right & 31)),
            ArithmeticOperator::ShiftRightSigned | ArithmeticOperator::ShiftRightUnsigned => {
                if operator == ArithmeticOperator::ShiftRightSigned {
                    Ok((left as i32).wrapping_shr(right & 31) as u32)
                } else {
                    Ok(left.wrapping_shr(right & 31))
                }
            }
        }
    }

    fn float_arithmetic(
        &self,
        operator: ArithmeticOperator,
        left: u64,
        right: u64,
    ) -> Result<u64, InterpretError> {
        let left = f64::from_bits(left);
        let right = f64::from_bits(right);
        let value = match operator {
            ArithmeticOperator::Add => left + right,
            ArithmeticOperator::Subtract => left - right,
            ArithmeticOperator::Multiply => left * right,
            ArithmeticOperator::DivideSigned | ArithmeticOperator::DivideUnsigned => left / right,
            ArithmeticOperator::RemainderSigned | ArithmeticOperator::RemainderUnsigned => {
                left % right
            }
            _ => return Err(InterpretError::TypeMismatch("float arithmetic operator")),
        };
        Ok(value.to_bits())
    }

    fn compare(
        &self,
        operator: ComparisonOperator,
        left: &InterpreterValue,
        right: &InterpreterValue,
    ) -> Result<bool, InterpretError> {
        match (left, right) {
            (InterpreterValue::Int(left), InterpreterValue::Int(right)) => Ok(match operator {
                ComparisonOperator::Equal => left == right,
                ComparisonOperator::NotEqual => left != right,
                ComparisonOperator::LessSigned => left < right,
                ComparisonOperator::LessUnsigned => (*left as u64) < (*right as u64),
                ComparisonOperator::LessEqualSigned => left <= right,
                ComparisonOperator::LessEqualUnsigned => (*left as u64) <= (*right as u64),
                ComparisonOperator::GreaterSigned => left > right,
                ComparisonOperator::GreaterUnsigned => (*left as u64) > (*right as u64),
                ComparisonOperator::GreaterEqualSigned => left >= right,
                ComparisonOperator::GreaterEqualUnsigned => (*left as u64) >= (*right as u64),
            }),
            (InterpreterValue::Size(left), InterpreterValue::Size(right)) => {
                let left = self.size_from_u64(*left)? as u32;
                let right = self.size_from_u64(*right)? as u32;
                Ok(match operator {
                    ComparisonOperator::Equal => left == right,
                    ComparisonOperator::NotEqual => left != right,
                    ComparisonOperator::LessSigned => (left as i32) < (right as i32),
                    ComparisonOperator::LessUnsigned => left < right,
                    ComparisonOperator::LessEqualSigned => (left as i32) <= (right as i32),
                    ComparisonOperator::LessEqualUnsigned => left <= right,
                    ComparisonOperator::GreaterSigned => (left as i32) > (right as i32),
                    ComparisonOperator::GreaterUnsigned => left > right,
                    ComparisonOperator::GreaterEqualSigned => (left as i32) >= (right as i32),
                    ComparisonOperator::GreaterEqualUnsigned => left >= right,
                })
            }
            (InterpreterValue::Bool(left), InterpreterValue::Bool(right)) => match operator {
                ComparisonOperator::Equal => Ok(left == right),
                ComparisonOperator::NotEqual => Ok(left != right),
                _ => Err(InterpretError::TypeMismatch("ordered boolean comparison")),
            },
            (InterpreterValue::FloatBits(left), InterpreterValue::FloatBits(right)) => {
                let left = f64::from_bits(*left);
                let right = f64::from_bits(*right);
                Ok(match operator {
                    ComparisonOperator::Equal => left == right,
                    ComparisonOperator::NotEqual => left != right,
                    ComparisonOperator::LessSigned | ComparisonOperator::LessUnsigned => left < right,
                    ComparisonOperator::LessEqualSigned
                    | ComparisonOperator::LessEqualUnsigned => left <= right,
                    ComparisonOperator::GreaterSigned | ComparisonOperator::GreaterUnsigned => {
                        left > right
                    }
                    ComparisonOperator::GreaterEqualSigned
                    | ComparisonOperator::GreaterEqualUnsigned => left >= right,
                })
            }
            _ => Err(InterpretError::TypeMismatch("comparison operands")),
        }
    }

    fn runtime_extent(
        &self,
        extent: &Extent,
        values: &BTreeMap<ValueId, InterpreterValue>,
    ) -> Result<RuntimeExtent, InterpretError> {
        Ok(match extent {
            Extent::FixedBytes(bytes) => RuntimeExtent::FixedBytes(*bytes),
            Extent::HeaderPrefix { minimum_bytes } => RuntimeExtent::HeaderPrefix(*minimum_bytes),
            Extent::Counted {
                header_bytes,
                count,
                capacity,
                stride_bytes,
            } => {
                let count = self.size_value(*count, values)?;
                let capacity = self.size_value(*capacity, values)?;
                if count > capacity {
                    return Err(InterpretError::CountExceedsCapacity { count, capacity });
                }
                RuntimeExtent::Counted {
                    header_bytes: *header_bytes,
                    count,
                    capacity,
                    stride_bytes: *stride_bytes,
                }
            }
        })
    }

    fn allocate(
        &mut self,
        layout: LayoutId,
        class: AllocationClass,
        extent: RuntimeExtent,
        immutable: bool,
    ) -> Result<u64, InterpretError> {
        let object = self.next_object;
        self.next_object = self
            .next_object
            .checked_add(1)
            .ok_or(InterpretError::ArithmeticTrap)?;
        self.objects.insert(
            object,
            Object {
                layout,
                storage: RuntimeStorage::Allocation(class),
                extent,
                references: 1,
                immutable,
                cells: BTreeMap::new(),
                absent_owned_slots: BTreeSet::new(),
            },
        );
        Ok(object)
    }

    fn allocate_static(
        &mut self,
        layout: LayoutId,
        extent: RuntimeExtent,
        bytes: &[u8],
    ) -> Result<u64, InterpretError> {
        let cells = self.materialize_static_cells(layout, &extent, bytes)?;
        let object = self.next_object;
        self.next_object = self
            .next_object
            .checked_add(1)
            .ok_or(InterpretError::ArithmeticTrap)?;
        self.objects.insert(
            object,
            Object {
                layout,
                storage: RuntimeStorage::StaticData,
                extent,
                references: 0,
                immutable: true,
                cells,
                absent_owned_slots: BTreeSet::new(),
            },
        );
        Ok(object)
    }

    fn materialize_static_cells(
        &self,
        layout: LayoutId,
        extent: &RuntimeExtent,
        bytes: &[u8],
    ) -> Result<BTreeMap<CellKey, InterpreterValue>, InterpretError> {
        let (_, layouts) = LayoutBundle::decode_canonical(&self.module.layouts().canonical_bytes())
            .map_err(|_| InterpretError::UnsupportedOperation("StaticData layouts"))?;
        let descriptor = layouts
            .get(layout)
            .ok_or(InterpretError::UnsupportedOperation("StaticData unknown layout"))?;
        let (element, count, data_offset, stride) = match (descriptor.kind(), extent) {
            (
                LayoutKind::RuntimeBuffer { element },
                RuntimeExtent::Counted {
                    header_bytes,
                    count,
                    stride_bytes,
                    ..
                },
            ) => (element, *count, u64::from(*header_bytes), *stride_bytes),
            (
                LayoutKind::PackedList { element, .. },
                RuntimeExtent::Counted { count, stride_bytes, .. },
            ) => {
                let HeaderLayout::PackedList { data_offset, .. } = descriptor.header() else {
                    return Err(InterpretError::UnsupportedOperation("StaticData packed-list header"));
                };
                (element, *count, u64::from(data_offset), *stride_bytes)
            }
            _ => return Err(InterpretError::UnsupportedOperation("StaticData layout")),
        };
        let element_descriptor = layouts
            .get(*element)
            .ok_or(InterpretError::UnsupportedOperation("StaticData element layout"))?;
        let LayoutKind::Scalar(kind) = element_descriptor.kind() else {
            return Err(InterpretError::UnsupportedOperation("StaticData non-scalar element"));
        };
        if u64::from(kind.size()) != u64::from(stride) {
            return Err(InterpretError::UnsupportedOperation("StaticData element stride"));
        }
        let mut cells = BTreeMap::new();
        for index in 0_u64..count {
            let offset = data_offset
                .checked_add(index.checked_mul(u64::from(stride)).ok_or(InterpretError::ArithmeticTrap)?)
                .ok_or(InterpretError::ArithmeticTrap)?;
            cells.insert(CellKey::Element(index), static_scalar(bytes, offset, *kind)?);
        }
        Ok(cells)
    }

    fn load(
        &mut self,
        place: &Place,
        transfer: LoadTransfer,
        values: &BTreeMap<ValueId, InterpreterValue>,
    ) -> Result<InterpreterValue, InterpretError> {
        let object = self.object_value(place.object(), values)?;
        if transfer != LoadTransfer::CopyScalar {
            // A reference cell: a shared clone retains the child, a unique
            // move takes the cell (its token leaves the container).
            let key = match place.projection() {
                Projection::Field { index } => CellKey::Field(*index),
                Projection::Element { index } => {
                    let index = self.size_value(*index, values)?;
                    self.check_element(object, index)?;
                    CellKey::Element(index)
                }
                _ => return Err(InterpretError::UnsupportedOperation("reference-bearing Load place")),
            };
            return match transfer {
                LoadTransfer::ClonePresent => {
                    let root = self.object(object)?;
                    if root.absent_owned_slots.contains(&key) {
                        return Err(InterpretError::GuardTrap);
                    }
                    let value = root.cells.get(&key).cloned()
                        .ok_or(InterpretError::UninitializedPlace)?;
                    let InterpreterValue::ObjectRef(child) = value else {
                        return Err(InterpretError::TypeMismatch("ClonePresent reference cell"));
                    };
                    self.retain(child)?;
                    Ok(InterpreterValue::ObjectRef(child))
                }
                LoadTransfer::CloneShared => {
                    let value = self.object(object)?.cells.get(&key).cloned().ok_or(InterpretError::UninitializedPlace)?;
                    if let InterpreterValue::ObjectRef(child) = value {
                        self.retain(child)?;
                    }
                    Ok(value)
                }
                LoadTransfer::MoveUnique => {
                    if self.object(object)?.immutable {
                        return Err(InterpretError::ImmutableObject(object));
                    }
                    self.object_mut(object)?.cells.remove(&key).ok_or(InterpretError::UninitializedPlace)
                }
                LoadTransfer::CopyScalar => unreachable!("handled below"),
            };
        }
        match place.projection() {
            Projection::Header(field) => {
                let object = self.object(object)?;
                let RuntimeExtent::Counted {
                    count, capacity, ..
                } = &object.extent
                else {
                    return Err(InterpretError::TypeMismatch("header on uncounted object"));
                };
                Ok(InterpreterValue::Size(match field {
                    HeaderField::Length => *count,
                    HeaderField::Capacity => *capacity,
                }))
            }
            Projection::Field { index } => self
                .object(object)?
                .cells
                .get(&CellKey::Field(*index))
                .cloned()
                .ok_or(InterpretError::UninitializedPlace),
            Projection::Element { index } => {
                let index = self.size_value(*index, values)?;
                self.check_element(object, index)?;
                self.object(object)?
                    .cells
                    .get(&CellKey::Element(index))
                    .cloned()
                    .ok_or(InterpretError::UninitializedPlace)
            }
            Projection::Slice { .. } => Err(InterpretError::UnsupportedOperation("Slice load")),
        }
    }

    fn store(
        &mut self,
        place: &Place,
        source: ValueId,
        transfer: StoreTransfer,
        values: &BTreeMap<ValueId, InterpreterValue>,
    ) -> Result<(), InterpretError> {
        let object = self.object_value(place.object(), values)?;
        let value = self.value(source, values)?.clone();
        if self.object(object)?.immutable {
            return Err(InterpretError::ImmutableObject(object));
        }
        // A unique move stores the token itself; a shared store adds one.
        if let (StoreTransfer::CloneShared, InterpreterValue::ObjectRef(child)) = (transfer, &value) {
            self.retain(*child)?;
        }
        let key = match place.projection() {
            Projection::Header(_) => {
                return Err(InterpretError::UnsupportedOperation("direct header Store"));
            }
            Projection::Field { index } => CellKey::Field(*index),
            Projection::Element { index } => {
                // A store may initialize a slot past the logical count, up to
                // the capacity; the count follows through `UpdateCount`.
                let index = self.size_value(*index, values)?;
                self.check_range(object, index, 1, true)?;
                CellKey::Element(index)
            }
            Projection::Slice { .. } => {
                return Err(InterpretError::UnsupportedOperation("Slice store"));
            }
        };
        let root = self.object_mut(object)?;
        root.absent_owned_slots.remove(&key);
        root.cells.insert(key, value);
        Ok(())
    }

    fn update_count(&mut self, object: u64, count: u64) -> Result<(), InterpretError> {
        if self.object(object)?.immutable {
            return Err(InterpretError::ImmutableObject(object));
        }
        let RuntimeExtent::Counted {
            count: current,
            capacity,
            ..
        } = &mut self.object_mut(object)?.extent
        else {
            return Err(InterpretError::TypeMismatch("UpdateCount on uncounted object"));
        };
        if count > *capacity {
            return Err(InterpretError::CountExceedsCapacity {
                count,
                capacity: *capacity,
            });
        }
        *current = count;
        Ok(())
    }

    fn check_element(&self, object: u64, index: u64) -> Result<(), InterpretError> {
        let RuntimeExtent::Counted { count, .. } = &self.object(object)?.extent else {
            return Err(InterpretError::TypeMismatch("element on uncounted object"));
        };
        if index >= *count {
            return Err(InterpretError::IndexOutOfBounds {
                index,
                length: *count,
            });
        }
        Ok(())
    }

    fn retain(&mut self, object: u64) -> Result<(), InterpretError> {
        let object_state = self.object_mut(object)?;
        match object_state.storage {
            RuntimeStorage::StaticData => return Ok(()),
            RuntimeStorage::Allocation(AllocationClass::ReferenceCounted) => {}
            RuntimeStorage::Allocation(_) => {
                return Err(InterpretError::ObjectNotReferenceCounted(object));
            }
        }
        object_state.references = u32::try_from(object_state.references)
            .ok()
            .and_then(|references| references.checked_add(1))
            .map(u64::from)
            .ok_or(InterpretError::ReferenceCountOverflow(object))?;
        Ok(())
    }

    fn release(&mut self, object: u64) -> Result<(), InterpretError> {
        let mut pending = vec![object];
        while let Some(object) = pending.pop() {
            let remaining = {
                let object_state = self.object_mut(object)?;
                match object_state.storage {
                    RuntimeStorage::StaticData => continue,
                    RuntimeStorage::Allocation(AllocationClass::ReferenceCounted) => {}
                    RuntimeStorage::Allocation(_) => {
                        return Err(InterpretError::ObjectNotReferenceCounted(object));
                    }
                }
                object_state.references = u32::try_from(object_state.references)
                    .ok()
                    .and_then(|references| references.checked_sub(1))
                    .map(u64::from)
                    .ok_or(InterpretError::ReferenceCountUnderflow(object))?;
                object_state.references
            };
            if remaining == 0 {
                let cells = self.objects.remove(&object).map(|object| object.cells).unwrap_or_default();
                // Reverse insertion preserves the previous depth-first cell
                // order without using one Rust stack frame per nested value.
                // Each edge transfers one owned token, including repeated
                // edges to a shared child; do not deduplicate the worklist.
                for cell in cells.into_values().rev() {
                    if let InterpreterValue::ObjectRef(child) = cell {
                        pending.push(child);
                    }
                }
            }
        }
        Ok(())
    }

    fn reclaim(&mut self, object: u64) -> Result<(), InterpretError> {
        let object_state = self.object(object)?;
        match object_state.storage {
            RuntimeStorage::StaticData => return Err(InterpretError::ImmutableObject(object)),
            RuntimeStorage::Allocation(AllocationClass::ReferenceCounted) => {}
            RuntimeStorage::Allocation(_) => {
                return Err(InterpretError::ObjectNotReferenceCounted(object));
            }
        }
        if object_state.references != 1 {
            return Err(InterpretError::ReclaimWithAliases {
                object,
                references: object_state.references,
            });
        }
        self.objects.remove(&object);
        Ok(())
    }

    fn take_call_arguments(
        &mut self,
        arguments: &[CallArgument],
        values: &mut BTreeMap<ValueId, InterpreterValue>,
    ) -> Result<Vec<InterpreterValue>, InterpretError> {
        arguments
            .iter()
            .map(|argument| match argument {
                CallArgument::Value(value) => Ok(self.value(*value, values)?.clone()),
                CallArgument::Move(value) => values
                    .remove(value)
                    .ok_or(InterpretError::MissingValue(*value)),
            })
            .collect()
    }

    fn bind_results(
        &self,
        destinations: &[ValueId],
        returned: Vec<InterpreterValue>,
        values: &mut BTreeMap<ValueId, InterpreterValue>,
    ) -> Result<(), InterpretError> {
        if destinations.len() != returned.len() {
            return Err(InterpretError::ArityMismatch {
                expected: destinations.len(),
                actual: returned.len(),
            });
        }
        for (destination, value) in destinations.iter().zip(returned) {
            values.insert(*destination, value);
        }
        Ok(())
    }

    fn bind_parameters(
        &self,
        block: &BasicBlock,
        arguments: Vec<InterpreterValue>,
        values: &mut BTreeMap<ValueId, InterpreterValue>,
    ) -> Result<(), InterpretError> {
        if block.parameters.len() != arguments.len() {
            return Err(InterpretError::ArityMismatch {
                expected: block.parameters.len(),
                actual: arguments.len(),
            });
        }
        for (parameter, argument) in block.parameters.iter().zip(arguments) {
            if parameter.value_type == crate::canonical_safewir::ValueType::Size {
                match argument {
                    InterpreterValue::Size(value) => {
                        self.size_from_u64(value)?;
                    }
                    _ => return Err(InterpretError::TypeMismatch("Size")),
                }
            }
            values.insert(parameter.value, argument);
        }
        Ok(())
    }

    fn read_values(
        &self,
        ids: &[ValueId],
        values: &BTreeMap<ValueId, InterpreterValue>,
    ) -> Result<Vec<InterpreterValue>, InterpretError> {
        ids.iter().map(|id| Ok(self.value(*id, values)?.clone())).collect()
    }

    fn function(&self, id: FunctionId) -> Result<&Function, InterpretError> {
        self.module
            .functions()
            .iter()
            .find(|function| function.id == id)
            .ok_or(InterpretError::FunctionNotFound(id))
    }

    fn block<'b>(
        &self,
        function: &'b Function,
        id: BlockId,
    ) -> Result<&'b BasicBlock, InterpretError> {
        function
            .blocks
            .iter()
            .find(|block| block.id == id)
            .ok_or(InterpretError::BlockNotFound(id))
    }

    fn value<'b>(
        &self,
        id: ValueId,
        values: &'b BTreeMap<ValueId, InterpreterValue>,
    ) -> Result<&'b InterpreterValue, InterpretError> {
        values.get(&id).ok_or(InterpretError::MissingValue(id))
    }

    fn bool_value(
        &self,
        id: ValueId,
        values: &BTreeMap<ValueId, InterpreterValue>,
    ) -> Result<bool, InterpretError> {
        match self.value(id, values)? {
            InterpreterValue::Bool(value) => Ok(*value),
            _ => Err(InterpretError::TypeMismatch("Bool")),
        }
    }

    fn int_value(
        &self,
        id: ValueId,
        values: &BTreeMap<ValueId, InterpreterValue>,
    ) -> Result<i64, InterpretError> {
        match self.value(id, values)? {
            InterpreterValue::Int(value) => Ok(*value),
            _ => Err(InterpretError::TypeMismatch("Int")),
        }
    }

    fn size_value(
        &self,
        id: ValueId,
        values: &BTreeMap<ValueId, InterpreterValue>,
    ) -> Result<u64, InterpretError> {
        match self.value(id, values)? {
            InterpreterValue::Size(value) => self.size_from_u64(*value),
            _ => Err(InterpretError::TypeMismatch("Size")),
        }
    }

    fn size_from_u64(&self, value: u64) -> Result<u64, InterpretError> {
        if value > SIZE_MAX {
            return Err(InterpretError::ArithmeticTrap);
        }
        Ok(value)
    }

    fn object_value(
        &self,
        id: ValueId,
        values: &BTreeMap<ValueId, InterpreterValue>,
    ) -> Result<u64, InterpretError> {
        match self.value(id, values)? {
            InterpreterValue::ObjectRef(value) => Ok(*value),
            _ => Err(InterpretError::TypeMismatch("Ref")),
        }
    }

    fn function_value(
        &self,
        id: ValueId,
        values: &BTreeMap<ValueId, InterpreterValue>,
    ) -> Result<FunctionId, InterpretError> {
        match self.value(id, values)? {
            InterpreterValue::Function(value) => Ok(*value),
            _ => Err(InterpretError::TypeMismatch("Callable")),
        }
    }

    fn object(&self, id: u64) -> Result<&Object, InterpretError> {
        self.objects.get(&id).ok_or(InterpretError::ObjectNotFound(id))
    }

    fn object_mut(&mut self, id: u64) -> Result<&mut Object, InterpretError> {
        self.objects
            .get_mut(&id)
            .ok_or(InterpretError::ObjectNotFound(id))
    }

    fn bump_step(&mut self) -> Result<(), InterpretError> {
        self.steps = self
            .steps
            .checked_add(1)
            .ok_or(InterpretError::StepLimitExceeded)?;
        if self.steps > self.config.max_steps {
            return Err(InterpretError::StepLimitExceeded);
        }
        Ok(())
    }
}

fn static_scalar(
    bytes: &[u8],
    offset: u64,
    kind: ScalarKind,
) -> Result<InterpreterValue, InterpretError> {
    let offset = usize::try_from(offset).map_err(|_| InterpretError::ArithmeticTrap)?;
    let end = offset
        .checked_add(kind.size() as usize)
        .ok_or(InterpretError::ArithmeticTrap)?;
    let bytes = bytes
        .get(offset..end)
        .ok_or(InterpretError::UnsupportedOperation("StaticData scalar bounds"))?;
    let size = |width: usize| bytes.get(..width).ok_or(InterpretError::UnsupportedOperation("StaticData scalar width"));
    match kind {
        ScalarKind::Bool => match bytes {
            [0] => Ok(InterpreterValue::Bool(false)),
            [1] => Ok(InterpreterValue::Bool(true)),
            _ => Err(InterpretError::UnsupportedOperation("StaticData bool encoding")),
        },
        ScalarKind::Int | ScalarKind::Duration => Ok(InterpreterValue::Int(i64::from_le_bytes(
            size(8)?.try_into().expect("validated scalar width"),
        ))),
        ScalarKind::Float => Ok(InterpreterValue::FloatBits(u64::from_le_bytes(
            size(8)?.try_into().expect("validated scalar width"),
        ))),
        ScalarKind::U32 | ScalarKind::Tag32 => Ok(InterpreterValue::Size(u64::from(u32::from_le_bytes(
            size(4)?.try_into().expect("validated scalar width"),
        )))),
        ScalarKind::Tag16 => Ok(InterpreterValue::Size(u64::from(u16::from_le_bytes(
            size(2)?.try_into().expect("validated scalar width"),
        )))),
        ScalarKind::Tag8 | ScalarKind::RawByte => Ok(InterpreterValue::Size(u64::from(bytes[0]))),
        ScalarKind::BoolValueSlot => match u64::from_le_bytes(size(8)?.try_into().expect("validated scalar width")) {
            0 => Ok(InterpreterValue::Bool(false)),
            1 => Ok(InterpreterValue::Bool(true)),
            _ => Err(InterpretError::UnsupportedOperation("StaticData bool slot encoding")),
        },
        ScalarKind::V128 => Ok(InterpreterValue::V128(
            size(16)?.try_into().expect("validated scalar width"),
        )),
        // There are no static closures: a callable lane is only ever written
        // by a `FunctionValue` at run time.
        ScalarKind::Callable => Err(InterpretError::UnsupportedOperation("StaticData callable lane")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canonical_safewir::{
        check_initial_with_test_witness, validate_final_for_test, BasicBlock,
        AccessMode, BlockParameter, CallableContract, CheckedSourceIdentity, FunctionSignature,
        FunctionSignatureId, LoweredModuleParts, Operation, ParameterContract, ParameterOwnership,
        ProperTailSite, ResultContract, Revision, TailSiteId, ValueType,
    };
    use crate::layout::{LayoutBundle, LayoutInterner, RcHeader, ScalarKind};

    const FUNCTION: FunctionId = FunctionId::new(0);
    const SIGNATURE: FunctionSignatureId = FunctionSignatureId::new(0);
    const ENTRY: BlockId = BlockId::new(0);

    fn validated(
        layouts: LayoutBundle,
        parameters: Vec<BlockParameter>,
        value_types: BTreeMap<ValueId, ValueType>,
        operations: Vec<Operation>,
        terminator: Terminator,
        result_types: Vec<ValueType>,
    ) -> ValidatedSafeWir {
        let parameter_types = parameters
            .iter()
            .map(|parameter| parameter.value_type)
            .collect::<Vec<_>>();
        let parameter_contracts = parameter_types
            .iter()
            .copied()
            .map(|value_type| ParameterContract {
                value_type,
                ownership: ParameterOwnership::Value,
            })
            .collect();
        let result_contracts = result_types
            .iter()
            .copied()
            .map(ResultContract::Value)
            .collect();
        let candidate = CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(0),
            checked_source: CheckedSourceIdentity::from_digest([31; 32]),
            layouts,
            signatures: vec![FunctionSignature {
                id: SIGNATURE,
                parameters: parameter_types,
                results: result_types,
                contract: CallableContract {
                    parameters: parameter_contracts,
                    results: result_contracts,
                    parameter_effects: BTreeMap::new(),
                },
            }],
            imports: Vec::new(),
            functions: vec![Function {
                id: FUNCTION,
                signature: SIGNATURE,
                entry: ENTRY,
                value_types,
                blocks: vec![BasicBlock {
                    id: ENTRY,
                    parameters,
                    operations,
                    terminator,
                }],
            }],
            proper_tail_sites: Vec::new(),
        });
        let checked = check_initial_with_test_witness(candidate).expect("initial validation");
        validate_final_for_test(checked).expect("final validation")
    }

    fn empty_layouts() -> LayoutBundle {
        LayoutBundle::from_interner(&LayoutInterner::new(), [])
            .expect("empty canonical layout bundle")
    }

    fn release_test_object(references: u64, children: &[u64]) -> Object {
        Object {
            layout: LayoutId::from_bytes([0; 32]),
            storage: RuntimeStorage::Allocation(AllocationClass::ReferenceCounted),
            extent: RuntimeExtent::FixedBytes(children.len() as u64 * 8),
            references,
            immutable: false,
            cells: children.iter().enumerate().map(|(index, child)|
                (CellKey::Field(index as u32), InterpreterValue::ObjectRef(*child))
            ).collect(),
            absent_owned_slots: BTreeSet::new(),
        }
    }

    #[test]
    fn nullable_owned_oracle_distinguishes_absent_uninitialized_and_present() {
        let module = validated(empty_layouts(), vec![], BTreeMap::new(), vec![], Terminator::Return { values: vec![] }, vec![]);
        let mut engine = Engine::new(module.module(), InterpreterConfig::default());
        let root = ValueId::new(100);
        let child = ValueId::new(101);
        let layout = LayoutId::from_bytes([0; 32]);
        let place = Place::projected(root, Projection::Field { index: 0 }, ValueType::Ref(layout), crate::canonical_safewir::AccessMode::Write);
        let values = BTreeMap::from([
            (root, InterpreterValue::ObjectRef(10)),
            (child, InterpreterValue::ObjectRef(11)),
        ]);
        engine.objects.insert(10, release_test_object(1, &[]));
        engine.objects.insert(11, release_test_object(1, &[]));
        assert!(matches!(engine.load(&place, LoadTransfer::ClonePresent, &values), Err(InterpretError::UninitializedPlace)));
        engine.init_nullable_owned(&place, &values).unwrap();
        assert!(engine.init_nullable_owned(&place, &values).is_err());
        assert!(matches!(engine.load(&place, LoadTransfer::ClonePresent, &values), Err(InterpretError::GuardTrap)));
        assert_eq!(engine.objects[&11].references, 1);
        engine.store(&place, child, StoreTransfer::MoveUnique, &values).unwrap();
        assert!(engine.objects[&10].absent_owned_slots.is_empty());
        assert!(engine.init_nullable_owned(&place, &values).is_err());
        assert_eq!(engine.load(&place, LoadTransfer::ClonePresent, &values).unwrap(), InterpreterValue::ObjectRef(11));
        assert_eq!(engine.objects[&11].references, 2);
        engine.release(10).unwrap();
        assert_eq!(engine.objects[&11].references, 1);
        engine.release(11).unwrap();
        assert!(engine.objects.is_empty());
    }

    #[test]
    fn nullable_owned_oracle_absence_drops_without_a_child_and_requires_uniqueness() {
        let module = validated(empty_layouts(), vec![], BTreeMap::new(), vec![], Terminator::Return { values: vec![] }, vec![]);
        let mut engine = Engine::new(module.module(), InterpreterConfig::default());
        let root = ValueId::new(100);
        let place = Place::projected(root, Projection::Field { index: 0 }, ValueType::Ref(LayoutId::from_bytes([0; 32])), crate::canonical_safewir::AccessMode::Write);
        let values = BTreeMap::from([(root, InterpreterValue::ObjectRef(10))]);
        engine.objects.insert(10, release_test_object(2, &[]));
        assert!(matches!(engine.init_nullable_owned(&place, &values), Err(InterpretError::ReclaimWithAliases { .. })));
        engine.release(10).unwrap();
        engine.init_nullable_owned(&place, &values).unwrap();
        engine.release(10).unwrap();
        assert!(engine.objects.is_empty());
    }

    #[test]
    fn iterative_release_drops_a_deep_chain_without_host_recursion() {
        let module = validated(empty_layouts(), vec![], BTreeMap::new(), vec![], Terminator::Return { values: vec![] }, vec![]);
        let mut engine = Engine::new(module.module(), InterpreterConfig::default());
        const DEPTH: u64 = 25_000;
        for object in 0..DEPTH {
            let children = if object + 1 < DEPTH { vec![object + 1] } else { vec![] };
            engine.objects.insert(object, release_test_object(1, &children));
        }
        engine.release(0).unwrap();
        assert!(engine.objects.is_empty());
    }

    #[test]
    fn iterative_release_counts_shared_edges_and_keeps_external_owners() {
        let module = validated(empty_layouts(), vec![], BTreeMap::new(), vec![], Terminator::Return { values: vec![] }, vec![]);
        let mut engine = Engine::new(module.module(), InterpreterConfig::default());
        engine.objects.insert(0, release_test_object(1, &[1, 1]));
        engine.objects.insert(1, release_test_object(3, &[2]));
        engine.objects.insert(2, release_test_object(1, &[]));
        engine.release(0).unwrap();
        assert_eq!(engine.objects.len(), 2);
        assert_eq!(engine.objects[&1].references, 1);
        engine.release(1).unwrap();
        assert!(engine.objects.is_empty());
    }

    #[test]
    fn iterative_release_preserves_static_children_and_refuses_underflow() {
        let module = validated(empty_layouts(), vec![], BTreeMap::new(), vec![], Terminator::Return { values: vec![] }, vec![]);
        let mut engine = Engine::new(module.module(), InterpreterConfig::default());
        engine.objects.insert(0, release_test_object(1, &[1]));
        let mut immortal = release_test_object(1, &[]);
        immortal.storage = RuntimeStorage::StaticData;
        engine.objects.insert(1, immortal);
        engine.release(0).unwrap();
        assert_eq!(engine.objects.len(), 1);
        assert_eq!(engine.objects[&1].references, 1);
        engine.objects.insert(2, release_test_object(0, &[]));
        assert!(matches!(engine.release(2), Err(InterpretError::ReferenceCountUnderflow(2))));
        assert_eq!(engine.objects[&2].references, 0);
    }

    fn packed_u32_layout() -> (LayoutBundle, LayoutId) {
        let mut layouts = LayoutInterner::new();
        let element = layouts
            .intern_scalar(ScalarKind::U32)
            .expect("u32 layout");
        let list = layouts
            .intern_packed_list(element, RcHeader::Required)
            .expect("packed u32 list layout");
        let bundle = LayoutBundle::from_interner(&layouts, [list]).expect("layout bundle");
        (bundle, list)
    }

    fn size_arithmetic_module(
        operator: ArithmeticOperator,
        left_value: u64,
        right_value: u64,
        overflow: OverflowBehavior,
    ) -> ValidatedSafeWir {
        let left = ValueId::new(0);
        let right = ValueId::new(1);
        let result = ValueId::new(2);
        validated(
            empty_layouts(),
            Vec::new(),
            [
                (left, ValueType::Size),
                (right, ValueType::Size),
                (result, ValueType::Size),
            ]
            .into_iter()
            .collect(),
            vec![
                Operation::ConstSize {
                    destination: left,
                    value: left_value,
                },
                Operation::ConstSize {
                    destination: right,
                    value: right_value,
                },
                Operation::Arithmetic {
                    destination: result,
                    operator,
                    left,
                    right,
                    overflow,
                },
            ],
            Terminator::Return {
                values: vec![result],
            },
            vec![ValueType::Size],
        )
    }

    fn int_arithmetic_module(
        operator: ArithmeticOperator,
        left_value: i64,
        right_value: i64,
    ) -> ValidatedSafeWir {
        let left = ValueId::new(0);
        let right = ValueId::new(1);
        let result = ValueId::new(2);
        validated(
            empty_layouts(),
            Vec::new(),
            [
                (left, ValueType::Int),
                (right, ValueType::Int),
                (result, ValueType::Int),
            ]
            .into_iter()
            .collect(),
            vec![
                Operation::ConstInt {
                    destination: left,
                    value: left_value,
                },
                Operation::ConstInt {
                    destination: right,
                    value: right_value,
                },
                Operation::Arithmetic {
                    destination: result,
                    operator,
                    left,
                    right,
                    overflow: OverflowBehavior::Trap,
                },
            ],
            Terminator::Return {
                values: vec![result],
            },
            vec![ValueType::Int],
        )
    }

    #[test]
    fn validated_size_arithmetic_uses_i32_width_and_signed_bits() {
        let cases = [
            (
                ArithmeticOperator::Add,
                u64::from(u32::MAX),
                1,
                OverflowBehavior::Wrap,
                Ok(vec![InterpreterValue::Size(0)]),
            ),
            (
                ArithmeticOperator::ShiftLeft,
                1,
                32,
                OverflowBehavior::Wrap,
                Ok(vec![InterpreterValue::Size(1)]),
            ),
            (
                ArithmeticOperator::ShiftRightSigned,
                u64::from(u32::MAX),
                1,
                OverflowBehavior::Wrap,
                Ok(vec![InterpreterValue::Size(u64::from(u32::MAX))]),
            ),
            (
                ArithmeticOperator::DivideSigned,
                u64::from(u32::MAX - 1),
                2,
                OverflowBehavior::Wrap,
                Ok(vec![InterpreterValue::Size(u64::from(u32::MAX))]),
            ),
        ];

        for (operator, left, right, overflow, expected) in cases {
            let module = size_arithmetic_module(operator, left, right, overflow);
            assert_eq!(interpret(&module, FUNCTION, Vec::new()), expected);
        }
    }

    #[test]
    fn validated_size_arithmetic_traps_at_i32_boundary() {
        let wrap_trap = size_arithmetic_module(
            ArithmeticOperator::Add,
            u64::from(u32::MAX),
            1,
            OverflowBehavior::Trap,
        );
        assert_eq!(
            interpret(&wrap_trap, FUNCTION, Vec::new()),
            Err(InterpretError::ArithmeticTrap)
        );

        let signed_division_trap = size_arithmetic_module(
            ArithmeticOperator::DivideSigned,
            u64::from(i32::MIN as u32),
            u64::from(u32::MAX),
            OverflowBehavior::Wrap,
        );
        assert_eq!(
            interpret(&signed_division_trap, FUNCTION, Vec::new()),
            Err(InterpretError::ArithmeticTrap)
        );
    }

    #[test]
    fn signed_remainder_min_over_negative_one_returns_zero() {
        let size_module = size_arithmetic_module(
            ArithmeticOperator::RemainderSigned,
            u64::from(i32::MIN as u32),
            u64::from(u32::MAX),
            OverflowBehavior::Trap,
        );
        assert_eq!(
            interpret(&size_module, FUNCTION, Vec::new()),
            Ok(vec![InterpreterValue::Size(0)])
        );

        let mut wasm = wasm_encoder::Module::new();
        let mut types = wasm_encoder::TypeSection::new();
        types.ty().function([], [wasm_encoder::ValType::I32]);
        wasm.section(&types);
        let mut functions = wasm_encoder::FunctionSection::new();
        functions.function(0);
        wasm.section(&functions);
        let mut exports = wasm_encoder::ExportSection::new();
        exports.export("remainder", wasm_encoder::ExportKind::Func, 0);
        wasm.section(&exports);
        let mut function = wasm_encoder::Function::new([]);
        function.instruction(&wasm_encoder::Instruction::I32Const(i32::MIN));
        function.instruction(&wasm_encoder::Instruction::I32Const(-1));
        function.instruction(&wasm_encoder::Instruction::I32RemS);
        function.instruction(&wasm_encoder::Instruction::End);
        let mut code = wasm_encoder::CodeSection::new();
        code.function(&function);
        wasm.section(&code);
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, wasm.finish()).expect("emitted Wasm");
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = wasmtime::Instance::new(&mut store, &module, &[]).expect("instance");
        let remainder = instance
            .get_typed_func::<(), i32>(&mut store, "remainder")
            .expect("Size result")
            .call(&mut store, ())
            .expect("i32.rem_s does not trap for MIN % -1");
        assert_eq!(remainder, 0);

        let int_module = int_arithmetic_module(ArithmeticOperator::RemainderSigned, i64::MIN, -1);
        assert_eq!(
            interpret(&int_module, FUNCTION, Vec::new()),
            Ok(vec![InterpreterValue::Int(0)])
        );
    }

    #[test]
    fn signed_remainder_by_zero_traps() {
        let size_module = size_arithmetic_module(
            ArithmeticOperator::RemainderSigned,
            1,
            0,
            OverflowBehavior::Trap,
        );
        assert_eq!(
            interpret(&size_module, FUNCTION, Vec::new()),
            Err(InterpretError::ArithmeticTrap)
        );

        let int_module = int_arithmetic_module(ArithmeticOperator::RemainderSigned, 1, 0);
        assert_eq!(
            interpret(&int_module, FUNCTION, Vec::new()),
            Err(InterpretError::ArithmeticTrap)
        );
    }

    #[test]
    fn validated_size_values_reject_values_wider_than_i32() {
        let input = ValueId::new(0);
        let zero = ValueId::new(1);
        let result = ValueId::new(2);
        let module = validated(
            empty_layouts(),
            vec![BlockParameter {
                value: input,
                value_type: ValueType::Size,
                ownership: None,
            }],
            [
                (input, ValueType::Size),
                (zero, ValueType::Size),
                (result, ValueType::Size),
            ]
            .into_iter()
            .collect(),
            vec![
                Operation::ConstSize {
                    destination: zero,
                    value: 0,
                },
                Operation::Arithmetic {
                    destination: result,
                    operator: ArithmeticOperator::Add,
                    left: input,
                    right: zero,
                    overflow: OverflowBehavior::Trap,
                },
            ],
            Terminator::Return {
                values: vec![result],
            },
            vec![ValueType::Size],
        );

        assert_eq!(
            interpret(
                &module,
                FUNCTION,
                vec![InterpreterValue::Size(SIZE_MAX + 1)],
            ),
            Err(InterpretError::ArithmeticTrap)
        );
    }

    #[test]
    fn reference_counts_trap_at_i32_maximum() {
        let (layouts, layout) = packed_u32_layout();
        let module = validated(
            layouts,
            Vec::new(),
            BTreeMap::new(),
            Vec::new(),
            Terminator::Return { values: Vec::new() },
            Vec::new(),
        );
        let mut engine = Engine::new(module.module(), InterpreterConfig::default());
        let object = engine
            .allocate(
                layout,
                AllocationClass::ReferenceCounted,
                RuntimeExtent::FixedBytes(0),
                false,
            )
            .expect("allocate reference-counted object");
        engine.object_mut(object).expect("object").references = SIZE_MAX;

        assert_eq!(
            engine.retain(object),
            Err(InterpretError::ReferenceCountOverflow(object))
        );
    }

    #[test]
    fn static_shared_clone_and_drop_do_not_touch_rc_state() {
        let (layouts, layout) = packed_u32_layout();
        let module = validated(
            layouts,
            Vec::new(),
            BTreeMap::new(),
            Vec::new(),
            Terminator::Return { values: Vec::new() },
            Vec::new(),
        );
        let source = ValueId::new(0);
        let clone = ValueId::new(1);
        let mut engine = Engine::new(module.module(), InterpreterConfig::default());
        let mut values = BTreeMap::new();

        let object = engine
            .allocate_static(
                layout,
                RuntimeExtent::Counted {
                    header_bytes: 0,
                    count: 0,
                    capacity: 0,
                    stride_bytes: 4,
                },
                &[],
            )
            .expect("create static object");
        values.insert(source, InterpreterValue::ObjectRef(object));
        assert_eq!(engine.object(object).expect("static state").references, 0);

        engine
            .execute_operation(&Operation::CloneRef { destination: clone, source }, &mut values)
            .expect("static clone is a no-op");
        engine
            .execute_operation(&Operation::DropRef { object: clone }, &mut values)
            .expect("static clone drop is a no-op");
        engine
            .execute_operation(&Operation::DropRef { object: source }, &mut values)
            .expect("static drop is a no-op");

        assert_eq!(engine.object(object).expect("static state").references, 0);
        assert_eq!(engine.reclaim(object), Err(InterpretError::ImmutableObject(object)));
    }

    #[test]
    fn validated_scalar_arithmetic_executes() {
        let left = ValueId::new(0);
        let right = ValueId::new(1);
        let result = ValueId::new(2);
        let module = validated(
            empty_layouts(),
            Vec::new(),
            [
                (left, ValueType::Int),
                (right, ValueType::Int),
                (result, ValueType::Int),
            ]
            .into_iter()
            .collect(),
            vec![
                Operation::ConstInt {
                    destination: left,
                    value: 40,
                },
                Operation::ConstInt {
                    destination: right,
                    value: 2,
                },
                Operation::Arithmetic {
                    destination: result,
                    operator: ArithmeticOperator::Add,
                    left,
                    right,
                    overflow: OverflowBehavior::Trap,
                },
            ],
            Terminator::Return {
                values: vec![result],
            },
            vec![ValueType::Int],
        );

        assert_eq!(
            interpret(&module, FUNCTION, Vec::new()),
            Ok(vec![InterpreterValue::Int(42)])
        );
    }

    #[test]
    fn validated_false_guard_traps() {
        let index = ValueId::new(0);
        let length = ValueId::new(1);
        let condition = ValueId::new(2);
        let module = validated(
            empty_layouts(),
            vec![
                BlockParameter {
                    value: index,
                    value_type: ValueType::Size,
                    ownership: None,
                },
                BlockParameter {
                    value: length,
                    value_type: ValueType::Size,
                    ownership: None,
                },
            ],
            [
                (index, ValueType::Size),
                (length, ValueType::Size),
                (condition, ValueType::Bool),
            ]
            .into_iter()
            .collect(),
            vec![
                Operation::Compare {
                    destination: condition,
                    operator: ComparisonOperator::LessUnsigned,
                    left: index,
                    right: length,
                },
                Operation::Guard {
                    condition,
                    kind: crate::canonical_safewir::GuardKind::Bounds,
                },
            ],
            Terminator::Return { values: Vec::new() },
            Vec::new(),
        );

        assert_eq!(
            interpret(
                &module,
                FUNCTION,
                vec![InterpreterValue::Size(2), InterpreterValue::Size(1)],
            ),
            Err(InterpretError::GuardTrap)
        );
    }

    #[test]
    fn validated_counted_element_store_and_load_execute() {
        let count = ValueId::new(0);
        let capacity = ValueId::new(1);
        let object = ValueId::new(2);
        let index = ValueId::new(3);
        let stored = ValueId::new(4);
        let loaded = ValueId::new(5);
        let within_length = ValueId::new(6);
        let within_capacity = ValueId::new(7);
        let (layouts, layout) = packed_u32_layout();
        let module = validated(
            layouts,
            Vec::new(),
            [
                (count, ValueType::Size),
                (capacity, ValueType::Size),
                (object, ValueType::Ref(layout)),
                (index, ValueType::Size),
                (stored, ValueType::Size),
                (loaded, ValueType::Size),
                (within_length, ValueType::Bool),
                (within_capacity, ValueType::Bool),
            ]
            .into_iter()
            .collect(),
            vec![
                Operation::ConstSize {
                    destination: count,
                    value: 1,
                },
                Operation::ConstSize {
                    destination: capacity,
                    value: 3,
                },
                Operation::Alloc {
                    destination: object,
                    layout,
                    extent: Extent::Counted {
                        header_bytes: 8,
                        count,
                        capacity,
                        stride_bytes: 4,
                    },
                    class: AllocationClass::ReferenceCounted,
                },
                Operation::ConstSize {
                    destination: index,
                    value: 0,
                },
                Operation::ConstSize {
                    destination: stored,
                    value: 7,
                },
                Operation::Compare {
                    destination: within_length,
                    operator: ComparisonOperator::LessUnsigned,
                    left: index,
                    right: count,
                },
                Operation::Guard {
                    condition: within_length,
                    kind: crate::canonical_safewir::GuardKind::Bounds,
                },
                Operation::Compare {
                    destination: within_capacity,
                    operator: ComparisonOperator::LessUnsigned,
                    left: index,
                    right: capacity,
                },
                Operation::Guard {
                    condition: within_capacity,
                    kind: crate::canonical_safewir::GuardKind::Bounds,
                },
                Operation::Store {
                    place: Place::projected(
                        object,
                        Projection::Element { index },
                        ValueType::Size,
                        AccessMode::Write,
                    ),
                    value: stored,
                    transfer: StoreTransfer::CopyScalar,
                },
                Operation::Load {
                    destination: loaded,
                    place: Place::projected(
                        object,
                        Projection::Element { index },
                        ValueType::Size,
                        AccessMode::Read,
                    ),
                    transfer: LoadTransfer::CopyScalar,
                },
                Operation::DropRef { object },
            ],
            Terminator::Return {
                values: vec![loaded],
            },
            vec![ValueType::Size],
        );

        assert_eq!(
            interpret(&module, FUNCTION, Vec::new()),
            Ok(vec![InterpreterValue::Size(7)])
        );
    }

    #[test]
    fn validated_clone_and_two_drops_execute() {
        let count = ValueId::new(0);
        let capacity = ValueId::new(1);
        let object = ValueId::new(2);
        let clone = ValueId::new(3);
        let (layouts, layout) = packed_u32_layout();
        let module = validated(
            layouts,
            Vec::new(),
            [
                (count, ValueType::Size),
                (capacity, ValueType::Size),
                (object, ValueType::Ref(layout)),
                (clone, ValueType::Ref(layout)),
            ]
            .into_iter()
            .collect(),
            vec![
                Operation::ConstSize {
                    destination: count,
                    value: 0,
                },
                Operation::ConstSize {
                    destination: capacity,
                    value: 1,
                },
                Operation::Alloc {
                    destination: object,
                    layout,
                    extent: Extent::Counted {
                        header_bytes: 8,
                        count,
                        capacity,
                        stride_bytes: 4,
                    },
                    class: AllocationClass::ReferenceCounted,
                },
                Operation::CloneRef {
                    destination: clone,
                    source: object,
                },
                Operation::DropRef { object: clone },
                Operation::DropRef { object },
            ],
            Terminator::Return { values: Vec::new() },
            Vec::new(),
        );

        assert_eq!(interpret(&module, FUNCTION, Vec::new()), Ok(Vec::new()));
    }

    #[test]
    fn validated_closure_record_calls_through_its_callable_slot() {
        use crate::canonical_safewir::{
            AllocationClass, CallArgument, Extent, LoadTransfer, MemoryEffect, Ownership,
            Place, Projection, StoreTransfer,
        };
        use crate::layout::LayoutId as WirLayoutId;

        let body_id = FunctionId::new(1);
        let environment_signature = FunctionSignatureId::new(1);
        let capture = ValueId::new(0);
        let closure = ValueId::new(1);
        let function_value = ValueId::new(2);
        let loaded = ValueId::new(3);
        let clone = ValueId::new(4);
        let call_result = ValueId::new(5);
        let environment = ValueId::new(6);
        let capture_read = ValueId::new(7);

        let mut layouts = LayoutInterner::new();
        let callable = layouts.intern_scalar(ScalarKind::Callable).expect("callable lane");
        let int = layouts.intern_scalar(ScalarKind::Int).expect("int lane");
        let record: WirLayoutId = layouts
            .intern_packed_record(vec![callable, int])
            .expect("closure record");
        let bundle = LayoutBundle::from_interner(&layouts, [record]).expect("layout bundle");

        let candidate = CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(0),
            checked_source: CheckedSourceIdentity::from_digest([33; 32]),
            layouts: bundle,
            signatures: vec![
                FunctionSignature {
                    id: SIGNATURE,
                    parameters: Vec::new(),
                    results: vec![ValueType::Int],
                    contract: CallableContract {
                        parameters: Vec::new(),
                        results: vec![ResultContract::Value(ValueType::Int)],
                        parameter_effects: BTreeMap::new(),
                    },
                },
                FunctionSignature {
                    id: environment_signature,
                    parameters: vec![ValueType::Ref(record)],
                    results: vec![ValueType::Int],
                    contract: CallableContract {
                        parameters: vec![ParameterContract {
                            value_type: ValueType::Ref(record),
                            ownership: ParameterOwnership::Shared,
                        }],
                        results: vec![ResultContract::Value(ValueType::Int)],
                        parameter_effects: [(0, vec![MemoryEffect::Read, MemoryEffect::Release])]
                            .into_iter()
                            .collect(),
                    },
                },
            ],
            imports: Vec::new(),
            functions: vec![
                Function {
                    id: FUNCTION,
                    signature: SIGNATURE,
                    entry: ENTRY,
                    value_types: [
                        (capture, ValueType::Int),
                        (closure, ValueType::Ref(record)),
                        (function_value, ValueType::Callable(environment_signature)),
                        (loaded, ValueType::Callable(environment_signature)),
                        (clone, ValueType::Ref(record)),
                        (call_result, ValueType::Int),
                    ]
                    .into_iter()
                    .collect(),
                    blocks: vec![BasicBlock {
                        id: ENTRY,
                        parameters: Vec::new(),
                        operations: vec![
                            Operation::Alloc {
                                destination: closure,
                                layout: record,
                                extent: Extent::FixedBytes(16),
                                class: AllocationClass::ReferenceCounted,
                            },
                            Operation::ConstInt { destination: capture, value: 7 },
                            Operation::Store {
                                place: Place::projected(closure, Projection::Field { index: 1 }, ValueType::Int, AccessMode::Write),
                                value: capture,
                                transfer: StoreTransfer::CopyScalar,
                            },
                            Operation::FunctionValue { destination: function_value, function: body_id },
                            Operation::Store {
                                place: Place::projected(closure, Projection::Field { index: 0 }, ValueType::Callable(environment_signature), AccessMode::Write),
                                value: function_value,
                                transfer: StoreTransfer::CopyScalar,
                            },
                            Operation::Load {
                                destination: loaded,
                                place: Place::projected(closure, Projection::Field { index: 0 }, ValueType::Callable(environment_signature), AccessMode::Read),
                                transfer: LoadTransfer::CopyScalar,
                            },
                            Operation::CloneRef { destination: clone, source: closure },
                            Operation::VarCall {
                                destinations: vec![call_result],
                                callee: loaded,
                                signature: environment_signature,
                                arguments: vec![CallArgument::Move(clone)],
                            },
                            Operation::DropRef { object: closure },
                        ],
                        terminator: Terminator::Return { values: vec![call_result] },
                    }],
                },
                Function {
                    id: body_id,
                    signature: environment_signature,
                    entry: ENTRY,
                    value_types: [
                        (environment, ValueType::Ref(record)),
                        (capture_read, ValueType::Int),
                    ]
                    .into_iter()
                    .collect(),
                    blocks: vec![BasicBlock {
                        id: ENTRY,
                        parameters: vec![BlockParameter {
                            value: environment,
                            value_type: ValueType::Ref(record),
                            ownership: Some(Ownership::Shared),
                        }],
                        operations: vec![
                            Operation::Load {
                                destination: capture_read,
                                place: Place::projected(environment, Projection::Field { index: 1 }, ValueType::Int, AccessMode::Read),
                                transfer: LoadTransfer::CopyScalar,
                            },
                            Operation::DropRef { object: environment },
                        ],
                        terminator: Terminator::Return { values: vec![capture_read] },
                    }],
                },
            ],
            proper_tail_sites: Vec::new(),
        })
        .with_closure_layouts([(record, environment_signature)].into_iter().collect());

        let checked = check_initial_with_test_witness(candidate).expect("initial validation");
        let module = validate_final_for_test(checked).expect("final validation");

        assert_eq!(
            interpret(&module, FUNCTION, Vec::new()),
            Ok(vec![InterpreterValue::Int(7)])
        );
    }

    #[test]
    fn validated_direct_call_executes_callee() {
        let callee_id = FunctionId::new(1);
        let callee_signature = FunctionSignatureId::new(1);
        let caller_result = ValueId::new(0);
        let callee_result = ValueId::new(0);
        let scalar_contract = || CallableContract {
            parameters: Vec::new(),
            results: vec![ResultContract::Value(ValueType::Int)],
            parameter_effects: BTreeMap::new(),
        };
        let candidate = CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(0),
            checked_source: CheckedSourceIdentity::from_digest([32; 32]),
            layouts: empty_layouts(),
            signatures: vec![
                FunctionSignature {
                    id: SIGNATURE,
                    parameters: Vec::new(),
                    results: vec![ValueType::Int],
                    contract: scalar_contract(),
                },
                FunctionSignature {
                    id: callee_signature,
                    parameters: Vec::new(),
                    results: vec![ValueType::Int],
                    contract: scalar_contract(),
                },
            ],
            imports: Vec::new(),
            functions: vec![
                Function {
                    id: FUNCTION,
                    signature: SIGNATURE,
                    entry: ENTRY,
                    value_types: [(caller_result, ValueType::Int)].into_iter().collect(),
                    blocks: vec![BasicBlock {
                        id: ENTRY,
                        parameters: Vec::new(),
                        operations: vec![Operation::Call {
                            destinations: vec![caller_result],
                            callee: callee_id,
                            arguments: Vec::new(),
                        }],
                        terminator: Terminator::Return {
                            values: vec![caller_result],
                        },
                    }],
                },
                Function {
                    id: callee_id,
                    signature: callee_signature,
                    entry: ENTRY,
                    value_types: [(callee_result, ValueType::Int)].into_iter().collect(),
                    blocks: vec![BasicBlock {
                        id: ENTRY,
                        parameters: Vec::new(),
                        operations: vec![Operation::ConstInt {
                            destination: callee_result,
                            value: 42,
                        }],
                        terminator: Terminator::Return {
                            values: vec![callee_result],
                        },
                    }],
                },
            ],
            proper_tail_sites: Vec::new(),
        });
        let checked = check_initial_with_test_witness(candidate).expect("initial validation");
        let module = validate_final_for_test(checked).expect("final validation");

        assert_eq!(
            interpret(&module, FUNCTION, Vec::new()),
            Ok(vec![InterpreterValue::Int(42)])
        );
    }

    #[test]
    fn validated_tail_call_trampolines_three_hundred_thousand_times() {
        let base = BlockId::new(1);
        let recurse = BlockId::new(2);
        let n = ValueId::new(0);
        let zero = ValueId::new(1);
        let one = ValueId::new(2);
        let done = ValueId::new(3);
        let next = ValueId::new(4);
        let site = TailSiteId::from_digest([33; 32]);
        let candidate = CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(0),
            checked_source: CheckedSourceIdentity::from_digest([34; 32]),
            layouts: empty_layouts(),
            signatures: vec![FunctionSignature {
                id: SIGNATURE,
                parameters: vec![ValueType::Int],
                results: vec![ValueType::Int],
                contract: CallableContract {
                    parameters: vec![ParameterContract {
                        value_type: ValueType::Int,
                        ownership: ParameterOwnership::Value,
                    }],
                    results: vec![ResultContract::Value(ValueType::Int)],
                    parameter_effects: BTreeMap::new(),
                },
            }],
            imports: Vec::new(),
            functions: vec![Function {
                id: FUNCTION,
                signature: SIGNATURE,
                entry: ENTRY,
                value_types: [
                    (n, ValueType::Int),
                    (zero, ValueType::Int),
                    (one, ValueType::Int),
                    (done, ValueType::Bool),
                    (next, ValueType::Int),
                ]
                .into_iter()
                .collect(),
                blocks: vec![
                    BasicBlock {
                        id: ENTRY,
                        parameters: vec![BlockParameter {
                            value: n,
                            value_type: ValueType::Int,
                            ownership: None,
                        }],
                        operations: vec![
                            Operation::ConstInt {
                                destination: zero,
                                value: 0,
                            },
                            Operation::ConstInt {
                                destination: one,
                                value: 1,
                            },
                            Operation::Compare {
                                destination: done,
                                operator: ComparisonOperator::LessEqualSigned,
                                left: n,
                                right: zero,
                            },
                        ],
                        terminator: Terminator::BranchIf {
                            condition: done,
                            then_target: base,
                            then_arguments: Vec::new(),
                            else_target: recurse,
                            else_arguments: Vec::new(),
                        },
                    },
                    BasicBlock {
                        id: base,
                        parameters: Vec::new(),
                        operations: Vec::new(),
                        terminator: Terminator::Return { values: vec![zero] },
                    },
                    BasicBlock {
                        id: recurse,
                        parameters: Vec::new(),
                        operations: vec![Operation::Arithmetic {
                            destination: next,
                            operator: ArithmeticOperator::Subtract,
                            left: n,
                            right: one,
                            overflow: OverflowBehavior::Trap,
                        }],
                        terminator: Terminator::TailCall {
                            site,
                            callee: FUNCTION,
                            arguments: vec![CallArgument::Value(next)],
                        },
                    },
                ],
            }],
            proper_tail_sites: vec![ProperTailSite {
                id: site,
                function: FUNCTION, discharged_by: None,
            }],
        });
        let checked = check_initial_with_test_witness(candidate).expect("initial validation");
        let module = validate_final_for_test(checked).expect("final validation");

        assert_eq!(
            interpret_with_config(
                &module,
                FUNCTION,
                vec![InterpreterValue::Int(300_000)],
                InterpreterConfig {
                    max_steps: 2_000_010,
                    ..Default::default()
                },
            ),
            Ok(vec![InterpreterValue::Int(0)])
        );
    }
}

#[cfg(test)]
#[path = "canonical_interpreter/capability_tests.rs"]
mod capability_tests;
