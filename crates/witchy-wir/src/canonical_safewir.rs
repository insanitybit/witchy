//! Canonical, semantic SafeWIR for RFC-0165.
//!
//! This is the optimizing representation. Unlike the RFC-0162 retrofit
//! surface, it does not annotate raw WIR expressions or infer memory meaning
//! from pointer arithmetic. Memory is addressed through layout projections,
//! ownership changes are explicit, and proper tail calls are terminators.
//!
//! Candidate modules are intentionally constructible by lowering and
//! optimization code. Validated states are not: only the canonical checker may
//! mint those states, and no backend accepts this module until that checker and
//! the one-way lowering are implemented.

use std::collections::BTreeMap;
use std::marker::PhantomData;

use crate::layout::{LayoutBundle, LayoutId};

mod check;
pub use check::{InitialCheckError, check_initial, check_transform};
mod assurance;
pub use assurance::{
    AssuranceError, OptimizationPassClass, PassAssurance, RefinementMechanism, SafetyRecheck,
    production_registry,
};
mod authority;
pub use authority::{
    CheckedInitialSafeWir, CheckedLoweringWitness, FinalValidationError, ValidatedSafeWir,
    check_initial_with_witness, validate_final,
};
#[cfg(test)]
pub(crate) use authority::check_initial_with_test_witness;
mod history;
mod optimization;
mod refinement;

pub use history::{
    DischargedObligation, HistoryError, ModuleDigest, OptimizationHistoryBody,
    OptimizationHistoryWitness, OptimizationPassId, RefinementReceipt,
};
pub use optimization::{
    OptimizationError, OptimizationOutcome, OptimizationSession, optimize_constant_folding,
    optimize_unreachable_blocks,
};
pub use refinement::RefinementError;

macro_rules! id_type {
    ($name:ident, $inner:ty) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name($inner);

        impl $name {
            pub const fn new(value: $inner) -> Self {
                Self(value)
            }

            pub const fn get(self) -> $inner {
                self.0
            }
        }
    };
}

id_type!(Revision, u64);
id_type!(FunctionId, u32);
id_type!(BlockId, u32);
id_type!(ValueId, u32);
id_type!(ImportId, u32);
id_type!(StaticDataId, u32);
id_type!(GcTypeId, u32);
id_type!(CapabilityKindId, u32);

/// Stable identity supplied by canonical type checking. It identifies the
/// checked source but grants no validation or encoding authority by itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CheckedSourceIdentity([u8; 32]);

impl CheckedSourceIdentity {
    pub const fn from_digest(digest: [u8; 32]) -> Self {
        Self(digest)
    }

    pub const fn digest(self) -> [u8; 32] {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ValueType {
    Int,
    Float,
    Bool,
    Unit,
    Size,
    V128,
    Ref(LayoutId),
    Gc(GcTypeId),
    Capability(CapabilityKindId),
    Callable(FunctionSignatureId),
}

id_type!(FunctionSignatureId, u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TailSiteId([u8; 32]);

impl TailSiteId {
    pub const fn from_digest(digest: [u8; 32]) -> Self {
        Self(digest)
    }

    pub const fn digest(self) -> [u8; 32] {
        self.0
    }
}

/// Overflow is part of program meaning, never an optimizer annotation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OverflowBehavior {
    Wrap,
    Trap,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ArithmeticOperator {
    Add,
    Subtract,
    Multiply,
    DivideSigned,
    DivideUnsigned,
    RemainderSigned,
    RemainderUnsigned,
    BitAnd,
    BitOr,
    BitXor,
    ShiftLeft,
    ShiftRightSigned,
    ShiftRightUnsigned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ComparisonOperator {
    Equal,
    NotEqual,
    LessSigned,
    LessUnsigned,
    LessEqualSigned,
    LessEqualUnsigned,
    GreaterSigned,
    GreaterUnsigned,
    GreaterEqualSigned,
    GreaterEqualUnsigned,
}

/// The object's live logical extent and its allocated capacity are distinct.
/// Bounds checks use `count`; allocation and growth use `capacity`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Extent {
    FixedBytes(u64),
    /// A catalog-authenticated prefix that may be read before a dynamic count
    /// has been loaded. It grants no access beyond `minimum_bytes`.
    HeaderPrefix { minimum_bytes: u32 },
    Counted {
        header_bytes: u32,
        count: ValueId,
        capacity: ValueId,
        stride_bytes: u32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Ownership {
    Unique,
    Shared,
    Consumed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AllocationClass {
    Bump,
    ReferenceCounted,
    Scratch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AccessMode {
    Read,
    Write,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Projection {
    Header(HeaderField),
    Field { index: u32 },
    Element { index: ValueId },
    Slice { start: ValueId, length: ValueId },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HeaderField {
    Length,
    Capacity,
}

/// The only v1 representation of an addressable memory location.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Place {
    object: ValueId,
    projection: Projection,
    value_type: ValueType,
    access: AccessMode,
}

impl Place {
    pub fn projected(
        object: ValueId,
        projection: Projection,
        value_type: ValueType,
        access: AccessMode,
    ) -> Self {
        Self {
            object,
            projection,
            value_type,
            access,
        }
    }

    pub const fn object(&self) -> ValueId {
        self.object
    }

    pub const fn projection(&self) -> &Projection {
        &self.projection
    }

    pub const fn value_type(&self) -> ValueType {
        self.value_type
    }

    pub const fn access(&self) -> AccessMode {
        self.access
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GuardKind {
    Bounds,
    Arithmetic,
    NonNull,
    DynamicType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LoadTransfer {
    CopyScalar,
    CloneShared,
    MoveUnique,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StoreTransfer {
    CopyScalar,
    CloneShared,
    MoveUnique,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BorrowMode {
    Read,
    Write,
}

/// A call-scoped, non-escaping view. It can occur only inside a call argument
/// and never becomes an SSA value, heap field, return value, or block argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CallBorrow {
    source: ValueId,
    mode: BorrowMode,
}

impl CallBorrow {
    pub const fn new(source: ValueId, mode: BorrowMode) -> Self {
        Self { source, mode }
    }

    pub const fn source(self) -> ValueId {
        self.source
    }

    pub const fn mode(self) -> BorrowMode {
        self.mode
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallArgument {
    Value(ValueId),
    Move(ValueId),
    Share(ValueId),
    Borrow(CallBorrow),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ParameterOwnership {
    Value,
    Unique,
    Shared,
    CallBorrow { mode: BorrowMode },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParameterContract {
    pub value_type: ValueType,
    pub ownership: ParameterOwnership,
    pub non_escaping: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResultContract {
    Value(ValueType),
    Fresh { layout: LayoutId },
    Shared { layout: LayoutId },
    AliasesParameter { parameter: u32, layout: LayoutId },
    ReallocatesParameter { parameter: u32, layout: LayoutId },
    StaticData { layout: LayoutId },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MemoryEffect {
    Read,
    Write,
    Retain,
    Release,
    Reclaim,
}

/// A declaration constrains callers. The checker derives the implementation
/// summary from the body and verifies conformance; a name never supplies it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallableContract {
    pub parameters: Vec<ParameterContract>,
    pub results: Vec<ResultContract>,
    pub parameter_effects: BTreeMap<u32, Vec<MemoryEffect>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionSignature {
    pub id: FunctionSignatureId,
    pub parameters: Vec<ValueType>,
    pub results: Vec<ValueType>,
    pub contract: CallableContract,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportContractIdentity {
    /// The canonical catalog authenticates the ABI contract only. Host
    /// implementation authentication remains a launch-context dependency.
    AuthenticatedAbi([u8; 32]),
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HostBufferExtent {
    /// The host may access exactly the checked runtime slice passed at this
    /// call site. The slice bounds remain part of SafeWIR and are independently
    /// re-established by the memory checker.
    Slice,
    Exact(u64),
    AtLeast(u64),
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostParameterRole {
    Value(ValueType),
    Capability(CapabilityKindId),
    Buffer {
        value_type: ValueType,
        access: AccessMode,
        extent: HostBufferExtent,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostResultRole {
    Value(ValueType),
    Capability(CapabilityKindId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HostMutation {
    ReadOnly,
    WriteBuffers,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HostReentrancy {
    Forbidden,
    Allowed,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostCallArgument {
    Value(ValueId),
    Capability(ValueId),
    Buffer {
        place: Place,
        extent: HostBufferExtent,
    },
}

/// Imported contracts are keyed by authenticated ABI identity. The concrete
/// host implementation identity belongs to launch context, not artifact ID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportContract {
    pub id: ImportId,
    pub signature: FunctionSignatureId,
    pub abi_identity: ImportContractIdentity,
    pub parameter_roles: Vec<HostParameterRole>,
    pub result_roles: Vec<HostResultRole>,
    pub mutation: HostMutation,
    pub reentrancy: HostReentrancy,
}

/// Trusted ABI facts are supplied by the canonical host catalog, never by a
/// candidate module. The constructor is intentionally private to this module;
/// production callers must obtain the catalog from the canonical host layer.
pub(crate) struct CanonicalAbiCatalog {
    imports: BTreeMap<ImportId, ImportContract>,
}

#[cfg(test)]
impl CanonicalAbiCatalog {
    fn from_imports(imports: Vec<ImportContract>) -> Self {
        Self {
            imports: imports.into_iter().map(|import| (import.id, import)).collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Operation {
    ConstInt { destination: ValueId, value: i64 },
    ConstFloat { destination: ValueId, bits: u64 },
    ConstBool { destination: ValueId, value: bool },
    ConstSize { destination: ValueId, value: u64 },
    Arithmetic {
        destination: ValueId,
        operator: ArithmeticOperator,
        left: ValueId,
        right: ValueId,
        overflow: OverflowBehavior,
    },
    Compare {
        destination: ValueId,
        operator: ComparisonOperator,
        left: ValueId,
        right: ValueId,
    },
    NarrowSize { destination: ValueId, value: ValueId },
    Guard { condition: ValueId, kind: GuardKind },
    Alloc {
        destination: ValueId,
        layout: LayoutId,
        extent: Extent,
        class: AllocationClass,
    },
    StaticData {
        destination: ValueId,
        layout: LayoutId,
        extent: Extent,
        data: StaticDataId,
    },
    Load {
        destination: ValueId,
        place: Place,
        transfer: LoadTransfer,
    },
    Store {
        place: Place,
        value: ValueId,
        transfer: StoreTransfer,
    },
    /// Raw bytes only. The checker rejects layouts containing references.
    CopyBytes { destination: Place, source: Place, length: ValueId },
    /// Transfers ownership of each typed element and consumes the source range.
    MoveRange { destination: Place, source: Place, count: ValueId },
    /// Clones each typed element, including required retains for shared refs.
    CloneRange { destination: Place, source: Place, count: ValueId },
    UpdateCount { object: ValueId, count: ValueId },
    MoveValue { destination: ValueId, source: ValueId },
    Share { destination: ValueId, source: ValueId },
    Retain { object: ValueId },
    Release { object: ValueId },
    Reclaim { object: ValueId },
    Call {
        destinations: Vec<ValueId>,
        callee: FunctionId,
        arguments: Vec<CallArgument>,
    },
    VarCall {
        destinations: Vec<ValueId>,
        callee: ValueId,
        signature: FunctionSignatureId,
        arguments: Vec<CallArgument>,
    },
    HostCall {
        destinations: Vec<ValueId>,
        import: ImportId,
        arguments: Vec<HostCallArgument>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Terminator {
    Branch { target: BlockId, arguments: Vec<ValueId> },
    BranchIf {
        condition: ValueId,
        then_target: BlockId,
        then_arguments: Vec<ValueId>,
        else_target: BlockId,
        else_arguments: Vec<ValueId>,
    },
    Return { values: Vec<ValueId> },
    /// Proper tail position is semantic, not an optional optimization.
    TailCall {
        site: TailSiteId,
        callee: FunctionId,
        arguments: Vec<CallArgument>,
    },
    VarTailCall {
        site: TailSiteId,
        callee: ValueId,
        signature: FunctionSignatureId,
        arguments: Vec<CallArgument>,
    },
    Trap,
    Unreachable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockParameter {
    pub value: ValueId,
    pub value_type: ValueType,
    pub ownership: Option<Ownership>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BasicBlock {
    pub id: BlockId,
    pub parameters: Vec<BlockParameter>,
    pub operations: Vec<Operation>,
    pub terminator: Terminator,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Function {
    pub id: FunctionId,
    pub signature: FunctionSignatureId,
    pub entry: BlockId,
    /// The canonical type of every SSA definition in this function. Operations
    /// do not infer a type from a Wasm carrier or instruction spelling.
    pub value_types: BTreeMap<ValueId, ValueType>,
    pub blocks: Vec<BasicBlock>,
}

/// Independently derived from typed source. Initial validation requires a
/// bijection between these obligations and SafeWIR tail-call terminators.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProperTailSite {
    pub id: TailSiteId,
    pub function: FunctionId,
}

/// Immutable bytes installed by the canonical backend before heap allocation.
/// Segment identity is part of the authenticated module digest; callers can
/// only borrow the bytes after construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaticDataSegment {
    id: StaticDataId,
    bytes: Vec<u8>,
}

impl StaticDataSegment {
    pub fn new(id: StaticDataId, bytes: Vec<u8>) -> Self {
        Self { id, bytes }
    }

    pub const fn id(&self) -> StaticDataId {
        self.id
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoweredModuleParts {
    pub revision: Revision,
    pub checked_source: CheckedSourceIdentity,
    pub layouts: LayoutBundle,
    pub signatures: Vec<FunctionSignature>,
    pub imports: Vec<ImportContract>,
    pub functions: Vec<Function>,
    pub proper_tail_sites: Vec<ProperTailSite>,
}

mod stage {
    pub trait Sealed {}
}

pub trait ModuleStage: stage::Sealed {}

#[derive(Debug, Clone, Copy)]
pub enum LoweredCandidate {}
impl stage::Sealed for LoweredCandidate {}
impl ModuleStage for LoweredCandidate {}

#[derive(Debug, Clone, Copy)]
pub enum TransformedCandidate {}
impl stage::Sealed for TransformedCandidate {}
impl ModuleStage for TransformedCandidate {}

#[derive(Debug, Clone, Copy)]
pub enum InitialValid {}
impl stage::Sealed for InitialValid {}
impl ModuleStage for InitialValid {}

#[derive(Debug, Clone, Copy)]
pub enum OptimizedValid {}
impl stage::Sealed for OptimizedValid {}
impl ModuleStage for OptimizedValid {}

/// One representation with typestate authority. Its fields are private so a
/// validated module cannot be assembled or retagged outside this module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalModule<S: ModuleStage> {
    revision: Revision,
    checked_source: CheckedSourceIdentity,
    layouts: LayoutBundle,
    signatures: Vec<FunctionSignature>,
    imports: Vec<ImportContract>,
    functions: Vec<Function>,
    proper_tail_sites: Vec<ProperTailSite>,
    static_data_segments: Vec<StaticDataSegment>,
    stage: PhantomData<fn() -> S>,
}

impl CanonicalModule<LoweredCandidate> {
    pub fn from_lowering(parts: LoweredModuleParts) -> Self {
        Self::from_lowering_with_static_data(parts, Vec::new())
    }

    /// Construct a candidate with module-level immutable static data. This is
    /// intentionally separate from source lowering: only canonical producers
    /// that already own the exact bytes may bind them here.
    pub fn from_lowering_with_static_data(
        parts: LoweredModuleParts,
        static_data_segments: Vec<StaticDataSegment>,
    ) -> Self {
        Self {
            revision: parts.revision,
            checked_source: parts.checked_source,
            layouts: parts.layouts,
            signatures: parts.signatures,
            imports: parts.imports,
            functions: parts.functions,
            proper_tail_sites: parts.proper_tail_sites,
            static_data_segments,
            stage: PhantomData,
        }
    }

    fn into_initial_valid(self) -> CanonicalModule<InitialValid> {
        CanonicalModule {
            revision: self.revision,
            checked_source: self.checked_source,
            layouts: self.layouts,
            signatures: self.signatures,
            imports: self.imports,
            functions: self.functions,
            proper_tail_sites: self.proper_tail_sites,
            static_data_segments: self.static_data_segments,
            stage: PhantomData,
        }
    }
}

impl CanonicalModule<InitialValid> {
    pub(crate) fn into_transformed_candidate(
        self,
        revision: Revision,
        functions: Vec<Function>,
    ) -> CanonicalModule<TransformedCandidate> {
        CanonicalModule {
            revision,
            checked_source: self.checked_source,
            layouts: self.layouts,
            signatures: self.signatures,
            imports: self.imports,
            functions,
            proper_tail_sites: self.proper_tail_sites,
            static_data_segments: self.static_data_segments,
            stage: PhantomData,
        }
    }
}

impl CanonicalModule<TransformedCandidate> {
    fn into_optimized_valid(self) -> CanonicalModule<OptimizedValid> {
        CanonicalModule {
            revision: self.revision,
            checked_source: self.checked_source,
            layouts: self.layouts,
            signatures: self.signatures,
            imports: self.imports,
            functions: self.functions,
            proper_tail_sites: self.proper_tail_sites,
            static_data_segments: self.static_data_segments,
            stage: PhantomData,
        }
    }
}

impl<S: ModuleStage> CanonicalModule<S> {
    pub const fn revision(&self) -> Revision {
        self.revision
    }

    pub const fn checked_source(&self) -> CheckedSourceIdentity {
        self.checked_source
    }

    pub const fn layouts(&self) -> &LayoutBundle {
        &self.layouts
    }

    pub fn signatures(&self) -> &[FunctionSignature] {
        &self.signatures
    }

    pub fn imports(&self) -> &[ImportContract] {
        &self.imports
    }

    pub fn functions(&self) -> &[Function] {
        &self.functions
    }

    pub fn proper_tail_sites(&self) -> &[ProperTailSite] {
        &self.proper_tail_sites
    }

    pub fn static_data_segments(&self) -> &[StaticDataSegment] {
        &self.static_data_segments
    }
}
