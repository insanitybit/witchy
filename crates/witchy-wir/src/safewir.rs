//! Compiler-only facts and stable rule identifiers for RFC-0162 SafeWIR.
//!
//! These types are intended to carry validation evidence through WIR
//! optimization. They are ghost state with no runtime representation; the
//! validator and erasing encoder are separate implementation tracks. The
//! normative meaning of each rule is specified independently in
//! `rfcs/0162-safewir-judgment.md`.

use std::collections::BTreeSet;

use crate::layout::LayoutId;

pub const SAFEWIR_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DefinitionId(u32);

impl DefinitionId {
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u32 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GenerationId(u32);

impl GenerationId {
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u32 {
        self.0
    }
}

/// A mathematical byte-size expression. Arithmetic remains unbounded until an
/// explicit narrowing node, whose validity is a validator obligation.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SizeExpr {
    Constant(u64),
    Definition(DefinitionId),
    Add(Box<SizeExpr>, Box<SizeExpr>),
    Multiply(Box<SizeExpr>, Box<SizeExpr>),
    NarrowU32(Box<SizeExpr>),
}

impl SizeExpr {
    pub fn add(lhs: Self, rhs: Self) -> Self {
        Self::Add(Box::new(lhs), Box::new(rhs))
    }

    pub fn multiply(lhs: Self, rhs: Self) -> Self {
        Self::Multiply(Box::new(lhs), Box::new(rhs))
    }

    pub fn narrow_u32(value: Self) -> Self {
        Self::NarrowU32(Box::new(value))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Extent {
    Unknown,
    Exact(SizeExpr),
    Counted {
        header: SizeExpr,
        count: DefinitionId,
        stride: SizeExpr,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ProjectionStep {
    /// A descriptor-defined header component. The canonical layout graph,
    /// rather than this ordinal, supplies its byte offset and width.
    Header { component: u32 },
    Field { index: u32 },
    Element { index: DefinitionId },
    Slice {
        start: DefinitionId,
        length: DefinitionId,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Projection(Vec<ProjectionStep>);

impl Projection {
    pub fn root() -> Self {
        Self::default()
    }

    pub fn from_steps(steps: impl IntoIterator<Item = ProjectionStep>) -> Self {
        Self(steps.into_iter().collect())
    }

    pub fn steps(&self) -> &[ProjectionStep] {
        &self.0
    }

    pub fn project(&self, step: ProjectionStep) -> Self {
        let mut result = self.clone();
        result.0.push(step);
        result
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Provenance {
    /// Fresh storage for the containing [`ObjectRef::identity`]. Carrying the
    /// identity again would permit contradictory candidate roots.
    Fresh,
    Aliases {
        object: ObjectIdentity,
        projection: Projection,
    },
    MayAlias(BTreeSet<ObjectIdentity>),
    StaticData(ObjectIdentity),
    Unknown,
}

impl Provenance {
    /// Freshness is only one input to validation. Layout, ownership, alias,
    /// borrow, and generation obligations remain independent checks in the
    /// validator's private state.
    pub const fn is_fresh(&self) -> bool {
        matches!(self, Self::Fresh)
    }

    pub fn has_nonempty_alias_set(&self) -> bool {
        !matches!(self, Self::MayAlias(objects) if objects.is_empty())
    }
}

/// A state in the validator's private ownership map. This enum is descriptive,
/// not an authority token; candidate WIR cannot attach it to an [`ObjectRef`]
/// or authorize a transition by constructing the enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Ownership {
    Unique,
    Shared,
    BorrowedRead,
    BorrowedWrite,
    Consumed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ObjectIdentity {
    pub root: DefinitionId,
    pub generation: GenerationId,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
/// An untrusted candidate description. Constructing this value grants no
/// ownership or memory-access authority.
pub struct ObjectRef {
    pub identity: ObjectIdentity,
    pub layout: LayoutId,
    pub extent: Extent,
    pub provenance: Provenance,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
/// A candidate claim/check pair. The validator must compare `object` with the
/// containing place identity and derive `start`/`length`; redundant fields are
/// deliberate and cannot be trusted independently.
pub struct ByteRange {
    pub object: ObjectIdentity,
    pub start: SizeExpr,
    pub length: SizeExpr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AccessMode {
    Read,
    Write,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
/// An untrusted candidate access. The validator independently derives and
/// checks every field before creating encoding authority.
pub struct Place {
    pub object: ObjectRef,
    pub projection: Projection,
    pub range: ByteRange,
    pub access: AccessMode,
}

impl Place {
    pub fn range_matches_object_generation(&self) -> bool {
        self.object.identity == self.range.object
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ParameterMemoryEffect {
    Read,
    Write,
    Consume,
    Retain,
    Escape,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ParameterMemoryEffects(u8);

impl ParameterMemoryEffects {
    const READ: u8 = 1 << ParameterMemoryEffect::Read as u8;
    const WRITE: u8 = 1 << ParameterMemoryEffect::Write as u8;
    const CONSUME: u8 = 1 << ParameterMemoryEffect::Consume as u8;
    const RETAIN: u8 = 1 << ParameterMemoryEffect::Retain as u8;
    const ESCAPE: u8 = 1 << ParameterMemoryEffect::Escape as u8;
    const ALL: u8 = Self::READ | Self::WRITE | Self::CONSUME | Self::RETAIN | Self::ESCAPE;

    pub const fn none() -> Self {
        Self(0)
    }

    pub const fn only(effect: ParameterMemoryEffect) -> Self {
        Self(1 << effect as u8)
    }

    /// Conservative top: the callee may perform every memory effect.
    pub const fn unknown() -> Self {
        Self(Self::ALL)
    }

    pub const fn with(self, effect: ParameterMemoryEffect) -> Self {
        Self(self.0 | (1 << effect as u8))
    }

    pub const fn contains(self, effect: ParameterMemoryEffect) -> bool {
        self.0 & (1 << effect as u8) != 0
    }

    /// Recursive/call-graph joins are set union; missing information starts at
    /// [`Self::unknown`] rather than the empty set.
    pub const fn join(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub const fn is_unknown(self) -> bool {
        self.0 == Self::ALL
    }

    /// Read and write without consume/retain/escape preserve unique ownership.
    /// Alias and borrow rules remain independent validator checks.
    pub const fn preserves_unique_ownership(self) -> bool {
        self.0 & (Self::CONSUME | Self::RETAIN | Self::ESCAPE) == 0
    }

    /// Only no-effect and read-only calls preserve facts loaded from an
    /// object's mutable headers. Write and every less precise effect kill them.
    pub const fn preserves_object_range_facts(self) -> bool {
        self.0 & !Self::READ == 0
    }
}

impl Default for ParameterMemoryEffects {
    fn default() -> Self {
        Self::unknown()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ParameterMemoryEntry {
    NoMemory,
    BorrowedRead,
    BorrowedWrite,
    Shared,
    Unique,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ParameterMemoryContract {
    pub entry: ParameterMemoryEntry,
    pub effects: ParameterMemoryEffects,
}

impl ParameterMemoryContract {
    pub const fn unknown() -> Self {
        Self {
            entry: ParameterMemoryEntry::Unknown,
            effects: ParameterMemoryEffects::unknown(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
/// Size expression whose parameter indices are substituted with caller
/// definitions when a callable contract is applied.
pub enum ContractSizeExpr {
    Constant(u64),
    Parameter(usize),
    Add(Box<ContractSizeExpr>, Box<ContractSizeExpr>),
    Multiply(Box<ContractSizeExpr>, Box<ContractSizeExpr>),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
/// A caller-substitutable fresh-result extent. Callee-local definitions cannot
/// cross the contract boundary.
pub enum ContractExtent {
    Exact(ContractSizeExpr),
    Counted {
        header: ContractSizeExpr,
        count: ContractSizeExpr,
        stride: ContractSizeExpr,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
/// Definition-free projection steps for callable results.
pub enum ContractProjectionStep {
    Header { component: u32 },
    Field { index: u32 },
    Element { index: u32 },
    Slice { start: u32, length: u32 },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ContractProjection(Vec<ContractProjectionStep>);

impl ContractProjection {
    pub fn from_steps(steps: impl IntoIterator<Item = ContractProjectionStep>) -> Self {
        Self(steps.into_iter().collect())
    }

    pub fn steps(&self) -> &[ContractProjectionStep] {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CallableMemoryResult {
    Fresh {
        layout: LayoutId,
        extent: ContractExtent,
    },
    Aliases {
        parameter: usize,
        projection: ContractProjection,
    },
    Shared(BTreeSet<usize>),
    NoMemory,
    Unknown,
}

impl Default for CallableMemoryResult {
    fn default() -> Self {
        Self::Unknown
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CallableMemoryContract {
    pub parameters: Vec<ParameterMemoryContract>,
    pub result: CallableMemoryResult,
}

impl CallableMemoryContract {
    pub fn unknown(parameter_count: usize) -> Self {
        Self {
            parameters: vec![ParameterMemoryContract::unknown(); parameter_count],
            result: CallableMemoryResult::Unknown,
        }
    }

    /// Structural callable-contract consistency checked again by the final
    /// validator against the body and source access signature.
    pub fn result_effects_are_consistent(&self) -> bool {
        let escapes = |parameter: usize| {
            self.parameters
                .get(parameter)
                .is_some_and(|contract| contract.effects.contains(ParameterMemoryEffect::Escape))
        };
        match &self.result {
            CallableMemoryResult::Aliases { parameter, .. } => escapes(*parameter),
            CallableMemoryResult::Shared(parameters) => {
                !parameters.is_empty() && parameters.iter().copied().all(escapes)
            }
            CallableMemoryResult::Fresh { .. }
            | CallableMemoryResult::NoMemory
            | CallableMemoryResult::Unknown => true,
        }
    }
}

macro_rules! define_rule_ids {
    ($( $variant:ident = $ordinal:literal => $code:literal, )+) => {
        /// Stable identifiers cited by validator diagnostics, fixtures, and
        /// mutation tests. Ordinals and codes are append-only within a schema
        /// version. The single catalog declaration generates the enum, complete
        /// iterator, and diagnostic code mapping together.
        #[repr(u8)]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum RuleId {
            $( $variant = $ordinal, )+
        }

        impl RuleId {
            pub const ALL: &'static [Self] = &[$( Self::$variant, )+];
            pub const COUNT: usize = Self::ALL.len();

            pub const fn code(self) -> &'static str {
                match self {
                    $( Self::$variant => $code, )+
                }
            }
        }
    };
}

define_rule_ids! {
    CheckedSourceEntry = 0 => "SWIR-MOD-001",
    CanonicalLayout = 1 => "SWIR-LAY-001",
    DestructiveProvenance = 2 => "SWIR-PROV-001",
    UniqueAllocation = 3 => "SWIR-OWN-001",
    ConsumeOnce = 4 => "SWIR-OWN-002",
    LiveGeneration = 5 => "SWIR-GEN-001",
    NonWrappingSize = 6 => "SWIR-RNG-001",
    ExactRangeSource = 7 => "SWIR-RNG-002",
    ConservativeJoin = 8 => "SWIR-CTL-001",
    LoopFixpoint = 9 => "SWIR-CTL-002",
    CheckedCallableContract = 10 => "SWIR-CALL-001",
    UnknownResultIsNotFresh = 11 => "SWIR-CALL-002",
    ObjectRange = 12 => "SWIR-MEM-001",
    ProjectedLayout = 13 => "SWIR-MEM-002",
    BulkRanges = 14 => "SWIR-BULK-001",
    ReferenceSeparation = 15 => "SWIR-REF-001",
    HostBufferAbi = 16 => "SWIR-HOST-001",
    SealedRawMemory = 17 => "SWIR-KERN-001",
    ValidatedEncoderAuthority = 18 => "SWIR-ENC-001",
    BudgetExhaustionFailsClosed = 19 => "SWIR-BUD-001",
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(byte: u8) -> LayoutId {
        LayoutId::from_bytes([byte; 32])
    }

    #[test]
    fn rule_codes_are_unique_and_schema_stable() {
        const SCHEMA_1_GOLDEN: &[(u8, &str)] = &[
            (0, "SWIR-MOD-001"),
            (1, "SWIR-LAY-001"),
            (2, "SWIR-PROV-001"),
            (3, "SWIR-OWN-001"),
            (4, "SWIR-OWN-002"),
            (5, "SWIR-GEN-001"),
            (6, "SWIR-RNG-001"),
            (7, "SWIR-RNG-002"),
            (8, "SWIR-CTL-001"),
            (9, "SWIR-CTL-002"),
            (10, "SWIR-CALL-001"),
            (11, "SWIR-CALL-002"),
            (12, "SWIR-MEM-001"),
            (13, "SWIR-MEM-002"),
            (14, "SWIR-BULK-001"),
            (15, "SWIR-REF-001"),
            (16, "SWIR-HOST-001"),
            (17, "SWIR-KERN-001"),
            (18, "SWIR-ENC-001"),
            (19, "SWIR-BUD-001"),
        ];
        let codes: BTreeSet<_> = RuleId::ALL.iter().copied().map(RuleId::code).collect();
        assert_eq!(RuleId::ALL.len(), RuleId::COUNT);
        assert_eq!(codes.len(), RuleId::COUNT);
        let actual: Vec<_> = RuleId::ALL
            .iter()
            .copied()
            .map(|rule| (rule as u8, rule.code()))
            .collect();
        assert_eq!(SAFEWIR_SCHEMA_VERSION, 1, "update the golden under a new schema");
        assert_eq!(actual, SCHEMA_1_GOLDEN, "schema-1 rule IDs are append-only");
    }

    #[test]
    fn unknown_callable_result_never_claims_freshness() {
        let contract = CallableMemoryContract::unknown(2);
        assert_eq!(contract.result, CallableMemoryResult::Unknown);
        assert_eq!(
            contract.parameters,
            vec![ParameterMemoryContract::unknown(), ParameterMemoryContract::unknown()]
        );
        assert!(
            contract
                .parameters
                .iter()
                .all(|parameter| !parameter.effects.preserves_unique_ownership())
        );
        assert!(
            contract
                .parameters
                .iter()
                .all(|parameter| parameter.entry == ParameterMemoryEntry::Unknown)
        );
        assert!(contract.parameters.iter().all(|parameter| parameter.effects.is_unknown()));
        assert!(contract.result_effects_are_consistent());
    }

    #[test]
    fn callable_effect_join_is_conservative_and_kills_mutable_range_facts() {
        assert_eq!(ParameterMemoryEffects::default(), ParameterMemoryEffects::unknown());
        let read_write = ParameterMemoryEffects::only(ParameterMemoryEffect::Read)
            .join(ParameterMemoryEffects::only(ParameterMemoryEffect::Write));
        assert!(read_write.contains(ParameterMemoryEffect::Read));
        assert!(read_write.contains(ParameterMemoryEffect::Write));
        assert!(read_write.preserves_unique_ownership());
        assert!(!read_write.preserves_object_range_facts());
        assert!(!ParameterMemoryEffects::unknown().preserves_unique_ownership());
        assert!(!ParameterMemoryEffects::unknown().preserves_object_range_facts());
    }

    #[test]
    fn candidate_object_description_carries_no_ownership_authority() {
        let root = DefinitionId::new(7);
        let object = ObjectRef {
            identity: ObjectIdentity { root, generation: GenerationId::new(1) },
            layout: layout(1),
            extent: Extent::Exact(SizeExpr::Constant(16)),
            provenance: Provenance::Fresh,
        };
        assert!(object.provenance.is_fresh());
    }

    #[test]
    fn range_authority_is_generation_specific() {
        let root = DefinitionId::new(9);
        let object = ObjectRef {
            identity: ObjectIdentity { root, generation: GenerationId::new(2) },
            layout: layout(2),
            extent: Extent::Exact(SizeExpr::Constant(8)),
            provenance: Provenance::Fresh,
        };
        let mut place = Place {
            range: ByteRange {
                object: object.identity,
                start: SizeExpr::Constant(0),
                length: SizeExpr::Constant(8),
            },
            object,
            projection: Projection::root(),
            access: AccessMode::Read,
        };
        assert!(place.range_matches_object_generation());
        place.range.object.generation = GenerationId::new(1);
        assert!(!place.range_matches_object_generation());
    }

    #[test]
    fn projections_compose_without_forging_offsets() {
        let index = DefinitionId::new(3);
        let projection = Projection::root()
            .project(ProjectionStep::Header { component: 0 })
            .project(ProjectionStep::Element { index })
            .project(ProjectionStep::Field { index: 1 });
        assert_eq!(
            projection.steps(),
            &[
                ProjectionStep::Header { component: 0 },
                ProjectionStep::Element { index },
                ProjectionStep::Field { index: 1 },
            ]
        );
    }

    #[test]
    fn aliases_are_generation_specific_and_empty_sets_are_invalid() {
        let identity = ObjectIdentity {
            root: DefinitionId::new(11),
            generation: GenerationId::new(4),
        };
        let alias = Provenance::Aliases {
            object: identity,
            projection: Projection::root(),
        };
        assert!(alias.has_nonempty_alias_set());
        assert!(!Provenance::MayAlias(BTreeSet::new()).has_nonempty_alias_set());

        let contract = CallableMemoryContract {
            parameters: vec![ParameterMemoryContract {
                entry: ParameterMemoryEntry::Shared,
                effects: ParameterMemoryEffects::none(),
            }],
            result: CallableMemoryResult::Shared(BTreeSet::new()),
        };
        assert!(!contract.result_effects_are_consistent());
    }

    #[test]
    fn contract_facts_are_caller_substitutable() {
        let projection = ContractProjection::from_steps([
            ContractProjectionStep::Header { component: 1 },
            ContractProjectionStep::Field { index: 2 },
            ContractProjectionStep::Element { index: 3 },
        ]);
        assert_eq!(projection.steps().len(), 3);

        let result = CallableMemoryResult::Fresh {
            layout: layout(4),
            extent: ContractExtent::Counted {
                header: ContractSizeExpr::Constant(8),
                count: ContractSizeExpr::Parameter(0),
                stride: ContractSizeExpr::Constant(4),
            },
        };
        assert!(matches!(result, CallableMemoryResult::Fresh { .. }));
        assert_ne!(Extent::Unknown, Extent::Exact(SizeExpr::Constant(0)));
    }

    #[test]
    fn alias_results_require_an_escape_effect_and_valid_parameter() {
        let projection = ContractProjection::default();
        let mut contract = CallableMemoryContract {
            parameters: vec![ParameterMemoryContract {
                entry: ParameterMemoryEntry::BorrowedRead,
                effects: ParameterMemoryEffects::only(ParameterMemoryEffect::Read),
            }],
            result: CallableMemoryResult::Aliases { parameter: 0, projection },
        };
        assert!(!contract.result_effects_are_consistent());
        contract.parameters[0].effects =
            contract.parameters[0].effects.with(ParameterMemoryEffect::Escape);
        assert!(contract.result_effects_are_consistent());
        contract.result = CallableMemoryResult::Aliases {
            parameter: 1,
            projection: ContractProjection::default(),
        };
        assert!(!contract.result_effects_are_consistent());
    }
}
