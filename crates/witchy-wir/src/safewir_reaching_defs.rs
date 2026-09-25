//! Deterministic reaching definitions for RFC-0162 structured WIR.
//!
//! This module deliberately derives identities from the function's structure.
//! Candidate-carried facts never choose a [`ReachingDefinitionId`]. The analysis keeps
//! structural keys through the loop fixpoint and assigns numeric IDs only after
//! convergence, in structural order. That makes repeated analysis stable and
//! prevents fixpoint iteration order from becoming authority.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;

use crate::wir::{WirExpr, WirFunc, WirNode, WirSeq};

/// Function-local identity in the reaching-definition analysis namespace.
///
/// This is intentionally not [`crate::safewir::DefinitionId`]. The final
/// adapter must perform an explicit checked binding between an executable WIR
/// site and a SafeWIR scalar fact; numeric equality cannot cross that boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ReachingDefinitionId(u32);

impl ReachingDefinitionId {
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u32 {
        self.0
    }
}

/// Explicit resource limits for one function analysis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReachingDefinitionBudget {
    pub max_structural_sites: usize,
    pub max_definitions: usize,
    /// Cumulative Rust recursion depth across sequences, nodes, expressions,
    /// and nested control regions (not merely expression-to-expression depth).
    pub max_expression_depth: usize,
    pub max_control_depth: usize,
    pub max_fixpoint_iterations: usize,
    pub max_fact_operations: usize,
    pub max_reaching_set_size: usize,
    /// Largest numeric ID the caller permits. This is independently bounded
    /// from `max_definitions` so exhaustion behavior can be tested cheaply.
    pub max_definition_id: u32,
}

impl Default for ReachingDefinitionBudget {
    fn default() -> Self {
        Self {
            max_structural_sites: 1_000_000,
            max_definitions: 500_000,
            max_expression_depth: 256,
            max_control_depth: 256,
            max_fixpoint_iterations: 256,
            max_fact_operations: 10_000_000,
            max_reaching_set_size: 65_536,
            max_definition_id: u32::MAX,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum JoinKind {
    IfExit,
    BlockExit,
    LoopHeader,
    FunctionExit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StructuredRegion {
    SourceBody,
    Then,
    Else,
    BlockBody,
    LoopBody,
    ControlExpression,
    SequenceExpression,
}

/// One component of a producer-independent path through a `WirFunc`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SiteStep {
    Parameter(u32),
    LocalInitializer(u32),
    Node(u32),
    /// Operand retained by SafeWIR erasure and therefore part of executable
    /// identity. Its ordinal is independent of erased claim operands.
    Operand(u32),
    /// Redundant SafeWIR evidence inspected during validation but erased
    /// before encoding. Claim sites can diagnose malformed candidates, but
    /// never receive definition identity.
    ClaimOperand(u32),
    Region(StructuredRegion),
    Destination(u32),
    Write,
    Join { kind: JoinKind, local: u32 },
}

/// Stable structural identity before it is mapped to a numeric definition ID.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StructuralSite(Vec<SiteStep>);

impl StructuralSite {
    pub fn root() -> Self {
        Self::default()
    }

    pub fn from_steps(steps: impl IntoIterator<Item = SiteStep>) -> Self {
        Self(steps.into_iter().collect())
    }

    pub fn steps(&self) -> &[SiteStep] {
        &self.0
    }

    pub fn projected(&self, step: SiteStep) -> Self {
        self.child(step)
    }

    fn child(&self, step: SiteStep) -> Self {
        let mut result = self.clone();
        result.0.push(step);
        result
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ExpressionForm {
    ConstI64,
    ConstF64,
    ConstI32,
    ConstV128,
    StringPointer,
    GetLocal,
    GetGlobal,
    Vector,
    ToSlot,
    FromSlot,
    Binary,
    Unary,
    Convert,
    Load,
    SafeLoad,
    MemorySize,
    MemoryGrow,
    Load8U,
    SafeLoad8U,
    SafeKernel,
    Call,
    CallHost,
    CallIndirect,
    Control,
    Sequence,
    StructNew,
    StructGet,
    ArrayNew,
    ArrayNewFixed,
    ArrayGet,
    ArrayLen,
    RefCast,
    RefCastNullable,
    RefNull,
    RefIsNull,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DefinitionKind {
    Parameter { local: String, index: u32 },
    /// Wasm body locals are zero-initialized and therefore have a real entry
    /// definition even when no `SetLocal` dominates a read.
    LocalInitializer { local: String, index: u32 },
    LocalWrite { local: String },
    CallResult { local: String, result: u32, indirect: bool },
    Expression { form: ExpressionForm },
    Join { local: String, kind: JoinKind },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefinitionSite {
    pub id: ReachingDefinitionId,
    pub site: StructuralSite,
    pub kind: DefinitionKind,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReachingSet(BTreeSet<ReachingDefinitionId>);

impl ReachingSet {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn contains(&self, definition: ReachingDefinitionId) -> bool {
        self.0.contains(&definition)
    }

    pub fn iter(&self) -> impl Iterator<Item = ReachingDefinitionId> + '_ {
        self.0.iter().copied()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReachingEnvironment(BTreeMap<String, ReachingSet>);

impl ReachingEnvironment {
    pub fn get(&self, local: &str) -> Option<&ReachingSet> {
        self.0.get(local)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &ReachingSet)> {
        self.0.iter().map(|(local, definitions)| (local.as_str(), definitions))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalRead {
    pub expression: ReachingDefinitionId,
    pub site: StructuralSite,
    pub local: String,
    pub reaching: ReachingSet,
    /// False for a structurally present read after an unconditional transfer.
    pub reachable: bool,
}

/// A local leaf referenced only by redundant SafeWIR evidence.
///
/// Claim uses retain the reaching producer set needed to authenticate the
/// evidence, but deliberately have no expression definition ID and never
/// appear in [`ReachingDefinitions::reads`]. They are erased before encoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimLocalUse {
    pub site: StructuralSite,
    pub local: String,
    pub reaching: ReachingSet,
    /// False for a structurally present claim after an unconditional transfer.
    pub reachable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinDefinition {
    pub definition: ReachingDefinitionId,
    pub site: StructuralSite,
    pub local: String,
    pub kind: JoinKind,
    /// Ordered incoming definition sets, one per reachable structured
    /// predecessor. The order is semantic and matches the control construct:
    /// then/else for an if, branch exits followed by fallthrough for a block,
    /// entry followed by backedges for a loop header, and explicit returns
    /// followed by ordinary fallthrough for a function exit.
    pub predecessors: Vec<ReachingSet>,
    /// Immediate incoming definitions. Loop-header joins may refer directly
    /// or transitively back to themselves, so consumers must not recursively
    /// walk this set without cycle detection. Use
    /// [`ReachingDefinitions::producer_closure`] for the finite leaf set.
    pub inputs: ReachingSet,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReachingDefinitionStats {
    pub structural_sites: usize,
    pub definitions: usize,
    pub reads: usize,
    pub claim_local_uses: usize,
    pub joins: usize,
    pub loop_iterations: usize,
    pub fact_operations: usize,
}

/// Complete, immutable result of one successful function analysis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReachingDefinitions {
    function: String,
    definitions: BTreeMap<ReachingDefinitionId, DefinitionSite>,
    ids_by_site: BTreeMap<StructuralSite, ReachingDefinitionId>,
    reads: Vec<LocalRead>,
    claim_local_uses: Vec<ClaimLocalUse>,
    joins: BTreeMap<ReachingDefinitionId, JoinDefinition>,
    entry: ReachingEnvironment,
    exit: Option<ReachingEnvironment>,
    stats: ReachingDefinitionStats,
}

impl ReachingDefinitions {
    pub fn function(&self) -> &str {
        &self.function
    }

    pub fn definition_sites(&self) -> &BTreeMap<ReachingDefinitionId, DefinitionSite> {
        &self.definitions
    }

    pub fn definition(&self, id: ReachingDefinitionId) -> Option<&DefinitionSite> {
        self.definitions.get(&id)
    }

    pub fn definition_at(&self, site: &StructuralSite) -> Option<ReachingDefinitionId> {
        self.ids_by_site.get(site).copied()
    }

    pub fn reads(&self) -> &[LocalRead] {
        &self.reads
    }

    pub fn reads_of<'a>(&'a self, local: &'a str) -> impl Iterator<Item = &'a LocalRead> + 'a {
        self.reads.iter().filter(move |read| read.local == local)
    }

    pub fn claim_local_uses(&self) -> &[ClaimLocalUse] {
        &self.claim_local_uses
    }

    pub fn claim_local_use_at(&self, site: &StructuralSite) -> Option<&ClaimLocalUse> {
        self.claim_local_uses
            .binary_search_by(|claim| claim.site.cmp(site))
            .ok()
            .map(|index| &self.claim_local_uses[index])
    }

    pub fn claim_local_uses_of<'a>(
        &'a self,
        local: &'a str,
    ) -> impl Iterator<Item = &'a ClaimLocalUse> + 'a {
        self.claim_local_uses
            .iter()
            .filter(move |claim| claim.local == local)
    }

    pub fn joins(&self) -> &BTreeMap<ReachingDefinitionId, JoinDefinition> {
        &self.joins
    }

    /// Expand a reaching set to its finite non-join producer closure.
    ///
    /// Join nodes are structural merge witnesses rather than scalar
    /// producers. Loop fixpoints can make their immediate-input graph cyclic;
    /// each join is therefore expanded at most once. The result contains only
    /// parameter, initializer, write, call-result, and expression definitions.
    /// Unknown IDs are retained as leaves so a caller cannot turn malformed
    /// external input into an empty, apparently safe producer set.
    pub fn producer_closure(&self, reaching: &ReachingSet) -> ReachingSet {
        let mut pending = reaching.iter().collect::<Vec<_>>();
        let mut visited_joins = BTreeSet::new();
        let mut producers = BTreeSet::new();
        while let Some(definition) = pending.pop() {
            let Some(join) = self.joins.get(&definition) else {
                producers.insert(definition);
                continue;
            };
            if visited_joins.insert(definition) {
                pending.extend(join.inputs.iter());
            }
        }
        ReachingSet(producers)
    }

    pub fn entry_environment(&self) -> &ReachingEnvironment {
        &self.entry
    }

    /// Environment common to every reachable explicit return and ordinary
    /// fallthrough. `None` means the function has no reachable exit.
    pub fn exit_environment(&self) -> Option<&ReachingEnvironment> {
        self.exit.as_ref()
    }

    pub fn stats(&self) -> ReachingDefinitionStats {
        self.stats
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReachingDefinitionDiagnosticCode {
    RawBody,
    DuplicateLocal,
    UnknownLocal,
    DuplicateControlLabel,
    UnknownBranchTarget,
    UnsupportedStructure,
    StructuralBudgetExceeded,
    DefinitionBudgetExceeded,
    ExpressionDepthExceeded,
    ControlDepthExceeded,
    FixpointBudgetExceeded,
    FactBudgetExceeded,
    ReachingSetBudgetExceeded,
    DefinitionIdExhausted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReachingDefinitionDiagnostic {
    pub code: ReachingDefinitionDiagnosticCode,
    pub function: String,
    pub site: Option<StructuralSite>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReachingDefinitionError {
    diagnostic: ReachingDefinitionDiagnostic,
}

impl ReachingDefinitionError {
    pub fn diagnostic(&self) -> &ReachingDefinitionDiagnostic {
        &self.diagnostic
    }
}

impl fmt::Display for ReachingDefinitionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.diagnostic.function, self.diagnostic.message)
    }
}

impl Error for ReachingDefinitionError {}

type RawSet = BTreeSet<StructuralSite>;
type RawEnvironment = BTreeMap<String, RawSet>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OperandMode {
    Executable,
    ClaimOnly,
}

/// One expression operand paired with the exact structural site used by the
/// reaching-definition analysis. This keeps final-adapter traversal from
/// reimplementing claim/executable ordinals independently.
// Consumed by the final-adapter binding (a later RFC-0162 slice); kept here so
// that traversal cannot reimplement claim/executable ordinals independently.
// Exercised by `structural_operands_*` below.
#[allow(dead_code)]
pub(crate) struct StructuralOperand {
    /// Address identity only; this pointer is never dereferenced. The adapter
    /// compares it while the borrowed WIR node or expression remains live.
    pub expression: *const WirExpr,
    pub claim_only: bool,
    pub site: StructuralSite,
}

impl OperandMode {
    fn site_step(self, index: u32) -> SiteStep {
        match self {
            Self::Executable => SiteStep::Operand(index),
            Self::ClaimOnly => SiteStep::ClaimOperand(index),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct OperandCounters {
    executable: usize,
    claims: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RawRead {
    local: String,
    reaching: RawSet,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RawJoin {
    local: String,
    kind: JoinKind,
    predecessors: Vec<RawSet>,
    inputs: RawSet,
}

#[derive(Debug, Clone, Default)]
struct Outcome {
    fallthrough: Option<RawEnvironment>,
    branches: BTreeMap<String, Vec<RawEnvironment>>,
    returns: Vec<RawEnvironment>,
}

impl Outcome {
    fn normal(environment: RawEnvironment) -> Self {
        Self { fallthrough: Some(environment), ..Self::default() }
    }

    fn absorb_abrupt(&mut self, other: Outcome) {
        for (target, mut edges) in other.branches {
            self.branches.entry(target).or_default().append(&mut edges);
        }
        self.returns.extend(other.returns);
    }
}

struct Analyzer {
    function: String,
    budget: ReachingDefinitionBudget,
    locals: Vec<String>,
    local_ordinals: BTreeMap<String, u32>,
    definitions: BTreeMap<StructuralSite, DefinitionKind>,
    reads: BTreeMap<StructuralSite, RawRead>,
    claim_local_uses: BTreeMap<StructuralSite, RawRead>,
    joins: BTreeMap<StructuralSite, RawJoin>,
    structural_sites: usize,
    fact_operations: usize,
    loop_iterations: usize,
}

/// Build the analysis-only def-use graph for one structured WIR function.
pub fn analyze_reaching_definitions(
    function: &WirFunc,
    budget: ReachingDefinitionBudget,
) -> Result<ReachingDefinitions, ReachingDefinitionError> {
    Analyzer::new(function, budget)?.run(function)
}

impl Analyzer {
    fn new(
        function: &WirFunc,
        budget: ReachingDefinitionBudget,
    ) -> Result<Self, ReachingDefinitionError> {
        if function.raw_body.is_some() {
            return Err(Self::error_for(
                function,
                ReachingDefinitionDiagnosticCode::RawBody,
                None,
                "raw function bodies have no structured reaching-definition graph",
            ));
        }

        let local_count = function.params.len().checked_add(function.locals.len()).ok_or_else(|| {
            Self::error_for(
                function,
                ReachingDefinitionDiagnosticCode::DefinitionIdExhausted,
                None,
                "combined parameter and local count overflowed",
            )
        })?;
        if local_count > budget.max_definitions {
            return Err(Self::error_for(
                function,
                ReachingDefinitionDiagnosticCode::DefinitionBudgetExceeded,
                None,
                format!(
                    "parameter and local definitions exceed configured limit {}",
                    budget.max_definitions
                ),
            ));
        }
        if local_count > budget.max_structural_sites {
            return Err(Self::error_for(
                function,
                ReachingDefinitionDiagnosticCode::StructuralBudgetExceeded,
                None,
                format!(
                    "parameter and local sites exceed configured limit {}",
                    budget.max_structural_sites
                ),
            ));
        }
        if local_count > 0
            && u64::try_from(local_count - 1).map_or(true, |last| {
                last > u64::from(budget.max_definition_id)
            })
        {
            return Err(Self::error_for(
                function,
                ReachingDefinitionDiagnosticCode::DefinitionIdExhausted,
                None,
                format!(
                    "parameter and local definitions exhaust configured DefinitionId maximum {}",
                    budget.max_definition_id
                ),
            ));
        }
        let mut locals = Vec::with_capacity(local_count);
        let mut local_ordinals = BTreeMap::new();
        for local in function.params.iter().chain(&function.locals) {
            if local_ordinals.contains_key(local.name.as_str()) {
                return Err(Self::error_for(
                    function,
                    ReachingDefinitionDiagnosticCode::DuplicateLocal,
                    None,
                    format!("local `{}` is declared more than once", local.name),
                ));
            }
            let ordinal = u32::try_from(locals.len()).map_err(|_| {
                Self::error_for(
                    function,
                    ReachingDefinitionDiagnosticCode::DefinitionIdExhausted,
                    None,
                    "local ordinal space exhausted",
                )
            })?;
            local_ordinals.insert(local.name.clone(), ordinal);
            locals.push(local.name.clone());
        }

        Ok(Self {
            function: function.name.clone(),
            budget,
            locals,
            local_ordinals,
            definitions: BTreeMap::new(),
            reads: BTreeMap::new(),
            claim_local_uses: BTreeMap::new(),
            joins: BTreeMap::new(),
            structural_sites: 0,
            fact_operations: 0,
            loop_iterations: 0,
        })
    }

    fn run(mut self, function: &WirFunc) -> Result<ReachingDefinitions, ReachingDefinitionError> {
        let root = StructuralSite::root();
        let mut entry = RawEnvironment::new();

        for (index, parameter) in function.params.iter().enumerate() {
            let index = self.u32_index(index, None, "parameter index")?;
            let site = root.child(SiteStep::Parameter(index));
            self.charge_structural(&site)?;
            self.register_definition(
                site.clone(),
                DefinitionKind::Parameter { local: parameter.name.clone(), index },
            )?;
            entry.insert(parameter.name.clone(), BTreeSet::from([site]));
        }
        for (index, local) in function.locals.iter().enumerate() {
            let index = self.u32_index(index, None, "body-local index")?;
            let site = root.child(SiteStep::LocalInitializer(index));
            self.charge_structural(&site)?;
            self.register_definition(
                site.clone(),
                DefinitionKind::LocalInitializer { local: local.name.clone(), index },
            )?;
            entry.insert(local.name.clone(), BTreeSet::from([site]));
        }

        self.preindex_seq(&function.body, &root, &mut Vec::new(), 0, 0)?;
        let mut outcome = self.analyze_seq(&function.body, &root, entry.clone())?;
        let mut exits = outcome.returns;
        if let Some(fallthrough) = outcome.fallthrough.take() {
            exits.push(fallthrough);
        }
        if !outcome.branches.is_empty() {
            return Err(self.error(
                ReachingDefinitionDiagnosticCode::UnsupportedStructure,
                None,
                "an enclosing branch edge escaped the function",
            ));
        }
        let exit = self.merge_environments(&root, JoinKind::FunctionExit, exits)?;
        self.finish(entry, exit)
    }

    fn finish(
        mut self,
        entry: RawEnvironment,
        exit: Option<RawEnvironment>,
    ) -> Result<ReachingDefinitions, ReachingDefinitionError> {
        // A loop may tentatively need a join before its final fixed point says
        // otherwise. Such a key never becomes authority merely because an
        // intermediate iteration mentioned it.
        let active_joins = &self.joins;
        self.definitions.retain(|site, kind| {
            !matches!(kind, DefinitionKind::Join { .. }) || active_joins.contains_key(site)
        });
        let mut ids_by_site = BTreeMap::new();
        for (index, site) in self.definitions.keys().enumerate() {
            let index = u64::try_from(index).map_err(|_| self.error(
                ReachingDefinitionDiagnosticCode::DefinitionIdExhausted,
                Some(site.clone()),
                "DefinitionId index does not fit in u64",
            ))?;
            if index > u64::from(self.budget.max_definition_id) {
                return Err(self.error(
                    ReachingDefinitionDiagnosticCode::DefinitionIdExhausted,
                    Some(site.clone()),
                    format!(
                        "DefinitionId space exhausted at configured maximum {}",
                        self.budget.max_definition_id
                    ),
                ));
            }
            ids_by_site.insert(site.clone(), ReachingDefinitionId::new(index as u32));
        }

        let mut definitions = BTreeMap::new();
        for (site, kind) in &self.definitions {
            let id = ids_by_site[site];
            definitions.insert(id, DefinitionSite { id, site: site.clone(), kind: kind.clone() });
        }

        let reads = self.reads.iter().map(|(site, read)| {
            Ok(LocalRead {
                expression: self.resolve_site(&ids_by_site, site)?,
                site: site.clone(),
                local: read.local.clone(),
                reaching: self.resolve_set(&ids_by_site, &read.reaching)?,
                reachable: !read.reaching.is_empty(),
            })
        }).collect::<Result<Vec<_>, ReachingDefinitionError>>()?;

        let claim_local_uses = self.claim_local_uses.iter().map(|(site, claim)| {
            Ok(ClaimLocalUse {
                site: site.clone(),
                local: claim.local.clone(),
                reaching: self.resolve_set(&ids_by_site, &claim.reaching)?,
                reachable: !claim.reaching.is_empty(),
            })
        }).collect::<Result<Vec<_>, ReachingDefinitionError>>()?;

        let mut joins = BTreeMap::new();
        for (site, join) in &self.joins {
            let definition = self.resolve_site(&ids_by_site, site)?;
            joins.insert(definition, JoinDefinition {
                definition,
                site: site.clone(),
                local: join.local.clone(),
                kind: join.kind,
                predecessors: join
                    .predecessors
                    .iter()
                    .map(|predecessor| self.resolve_set(&ids_by_site, predecessor))
                    .collect::<Result<Vec<_>, _>>()?,
                inputs: self.resolve_set(&ids_by_site, &join.inputs)?,
            });
        }

        let entry = self.resolve_environment(&ids_by_site, &entry)?;
        let exit = exit.as_ref().map(|environment| {
            self.resolve_environment(&ids_by_site, environment)
        }).transpose()?;
        let stats = ReachingDefinitionStats {
            structural_sites: self.structural_sites,
            definitions: definitions.len(),
            reads: reads.len(),
            claim_local_uses: claim_local_uses.len(),
            joins: joins.len(),
            loop_iterations: self.loop_iterations,
            fact_operations: self.fact_operations,
        };

        Ok(ReachingDefinitions {
            function: self.function.clone(),
            definitions,
            ids_by_site,
            reads,
            claim_local_uses,
            joins,
            entry,
            exit,
            stats,
        })
    }

    fn resolve_site(
        &self,
        ids: &BTreeMap<StructuralSite, ReachingDefinitionId>,
        site: &StructuralSite,
    ) -> Result<ReachingDefinitionId, ReachingDefinitionError> {
        ids.get(site).copied().ok_or_else(|| self.error(
            ReachingDefinitionDiagnosticCode::UnsupportedStructure,
            Some(site.clone()),
            "internal reaching-definition site was not finalized",
        ))
    }

    fn resolve_set(
        &self,
        ids: &BTreeMap<StructuralSite, ReachingDefinitionId>,
        set: &RawSet,
    ) -> Result<ReachingSet, ReachingDefinitionError> {
        let resolved = set.iter().map(|site| self.resolve_site(ids, site))
            .collect::<Result<BTreeSet<_>, _>>()?;
        Ok(ReachingSet(resolved))
    }

    fn resolve_environment(
        &self,
        ids: &BTreeMap<StructuralSite, ReachingDefinitionId>,
        environment: &RawEnvironment,
    ) -> Result<ReachingEnvironment, ReachingDefinitionError> {
        let resolved = environment.iter().map(|(local, set)| {
            Ok((local.clone(), self.resolve_set(ids, set)?))
        }).collect::<Result<BTreeMap<_, _>, ReachingDefinitionError>>()?;
        Ok(ReachingEnvironment(resolved))
    }

    fn preindex_seq(
        &mut self,
        nodes: &WirSeq,
        parent: &StructuralSite,
        labels: &mut Vec<String>,
        control_depth: usize,
        nesting_depth: usize,
    ) -> Result<(), ReachingDefinitionError> {
        for (index, node) in nodes.iter().enumerate() {
            let index = self.u32_index(index, Some(parent.clone()), "node index")?;
            let site = parent.child(SiteStep::Node(index));
            let node_depth = self.next_nesting_depth(nesting_depth, &site)?;
            self.preindex_node(node, &site, labels, control_depth, node_depth)?;
        }
        Ok(())
    }

    fn preindex_node(
        &mut self,
        node: &WirNode,
        site: &StructuralSite,
        labels: &mut Vec<String>,
        control_depth: usize,
        nesting_depth: usize,
    ) -> Result<(), ReachingDefinitionError> {
        self.charge_structural(site)?;
        self.charge_node_claim_structure(node, site)?;
        for_each_node_operand(node, |index, mode, expression| {
            let index = self.u32_index(index, Some(site.clone()), "node operand index")?;
            let expression_site = site.child(mode.site_step(index));
            let expression_depth = self.next_nesting_depth(nesting_depth, &expression_site)?;
            match mode {
                OperandMode::Executable => self.preindex_expr(
                    expression,
                    &expression_site,
                    labels,
                    control_depth,
                    expression_depth,
                ),
                OperandMode::ClaimOnly => self.preindex_claim_expr(
                    expression,
                    &expression_site,
                    expression_depth,
                ),
            }
        })?;

        match node {
            WirNode::SetLocal { local, .. } => {
                self.require_local(local, site)?;
                self.register_definition(
                    site.child(SiteStep::Write),
                    DefinitionKind::LocalWrite { local: local.clone() },
                )?;
            }
            WirNode::CallStoreMulti { dests, .. }
            | WirNode::CallIndirectStoreMulti { dests, .. } => {
                let indirect = matches!(node, WirNode::CallIndirectStoreMulti { .. });
                for (result, local) in dests.iter().enumerate() {
                    self.require_local(local, site)?;
                    let result = self.u32_index(result, Some(site.clone()), "call result index")?;
                    self.register_definition(
                        site.child(SiteStep::Destination(result)),
                        DefinitionKind::CallResult { local: local.clone(), result, indirect },
                    )?;
                }
            }
            WirNode::Source { body, .. } => {
                let next_depth = self.next_control_depth(control_depth, site)?;
                let body_site = site.child(SiteStep::Region(StructuredRegion::SourceBody));
                self.preindex_seq(body, &body_site, labels, next_depth, nesting_depth)?;
            }
            WirNode::If { then_, els, .. } => {
                let next_depth = self.next_control_depth(control_depth, site)?;
                self.preindex_seq(
                    then_,
                    &site.child(SiteStep::Region(StructuredRegion::Then)),
                    labels,
                    next_depth,
                    nesting_depth,
                )?;
                self.preindex_seq(
                    els,
                    &site.child(SiteStep::Region(StructuredRegion::Else)),
                    labels,
                    next_depth,
                    nesting_depth,
                )?;
            }
            WirNode::Block { label, body, .. } => {
                let next_depth = self.next_control_depth(control_depth, site)?;
                self.push_label(label, labels, site)?;
                let result = self.preindex_seq(
                    body,
                    &site.child(SiteStep::Region(StructuredRegion::BlockBody)),
                    labels,
                    next_depth,
                    nesting_depth,
                );
                labels.pop();
                result?;
            }
            WirNode::Loop { label, body } => {
                let next_depth = self.next_control_depth(control_depth, site)?;
                self.push_label(label, labels, site)?;
                let result = self.preindex_seq(
                    body,
                    &site.child(SiteStep::Region(StructuredRegion::LoopBody)),
                    labels,
                    next_depth,
                    nesting_depth,
                );
                labels.pop();
                result?;
            }
            WirNode::Br { target, .. } => {
                if !labels.iter().rev().any(|label| label == target) {
                    return Err(self.error(
                        ReachingDefinitionDiagnosticCode::UnknownBranchTarget,
                        Some(site.clone()),
                        format!("branch target `{target}` is not a lexically enclosing control"),
                    ));
                }
            }
            WirNode::SetGlobal { .. }
            | WirNode::Store { .. }
            | WirNode::SafeStore { .. }
            | WirNode::MemoryCopy { .. }
            | WirNode::SafeMemoryCopy { .. }
            | WirNode::MemoryFill { .. }
            | WirNode::SafeMemoryFill { .. }
            | WirNode::Store8 { .. }
            | WirNode::SafeStore8 { .. }
            | WirNode::SafeKernel(_)
            | WirNode::Drop(_)
            | WirNode::Do(_)
            | WirNode::Push(_)
            | WirNode::Return(_)
            | WirNode::Unreachable
            | WirNode::StructSet { .. }
            | WirNode::ArraySet { .. } => {}
        }
        Ok(())
    }

    fn preindex_expr(
        &mut self,
        expression: &WirExpr,
        site: &StructuralSite,
        labels: &mut Vec<String>,
        control_depth: usize,
        nesting_depth: usize,
    ) -> Result<(), ReachingDefinitionError> {
        self.charge_structural(site)?;
        self.charge_expr_claim_structure(expression, site)?;
        self.register_definition(
            site.clone(),
            DefinitionKind::Expression { form: expression_form(expression) },
        )?;
        if let WirExpr::GetLocal(local) = expression {
            self.require_local(local, site)?;
            self.reads.insert(site.clone(), RawRead { local: local.clone(), reaching: RawSet::new() });
        }

        for_each_expr_operand(expression, |index, mode, child| {
            let index = self.u32_index(index, Some(site.clone()), "expression operand index")?;
            let child_site = site.child(mode.site_step(index));
            let child_depth = self.next_nesting_depth(nesting_depth, &child_site)?;
            match mode {
                OperandMode::Executable => self.preindex_expr(
                    child,
                    &child_site,
                    labels,
                    control_depth,
                    child_depth,
                ),
                OperandMode::ClaimOnly => {
                    self.preindex_claim_expr(child, &child_site, child_depth)
                }
            }
        })?;

        match expression {
            WirExpr::Control(node) => {
                let control_site = site.child(SiteStep::Region(StructuredRegion::ControlExpression));
                let control_nesting = self.next_nesting_depth(nesting_depth, &control_site)?;
                self.preindex_node(
                    node,
                    &control_site,
                    labels,
                    control_depth,
                    control_nesting,
                )?;
            }
            WirExpr::Seq(nodes) => {
                let sequence_site = site.child(SiteStep::Region(StructuredRegion::SequenceExpression));
                self.preindex_seq(
                    nodes,
                    &sequence_site,
                    labels,
                    control_depth,
                    nesting_depth,
                )?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Validate erased evidence without creating executable definitions or
    /// reads. Local leaves are retained only in the separate non-executable
    /// claim-use table. Control-bearing expressions are not valid vocabulary:
    /// accepting them would require interpreting executable transfers that
    /// erasure discards.
    fn preindex_claim_expr(
        &mut self,
        expression: &WirExpr,
        site: &StructuralSite,
        nesting_depth: usize,
    ) -> Result<(), ReachingDefinitionError> {
        self.charge_structural(site)?;
        self.charge_expr_claim_structure(expression, site)?;
        if let WirExpr::GetLocal(local) = expression {
            self.require_local(local, site)?;
            self.claim_local_uses.insert(
                site.clone(),
                RawRead { local: local.clone(), reaching: RawSet::new() },
            );
        }
        if matches!(expression, WirExpr::Control(_) | WirExpr::Seq(_)) {
            return Err(self.error(
                ReachingDefinitionDiagnosticCode::UnsupportedStructure,
                Some(site.clone()),
                "an erased SafeWIR claim contains control flow",
            ));
        }
        for_each_claim_expr_operand(expression, |index, child| {
            let index = self.u32_index(index, Some(site.clone()), "claim operand index")?;
            let child_site = site.child(SiteStep::ClaimOperand(index));
            let child_depth = self.next_nesting_depth(nesting_depth, &child_site)?;
            self.preindex_claim_expr(child, &child_site, child_depth)
        })
    }

    fn charge_node_claim_structure(
        &mut self,
        node: &WirNode,
        site: &StructuralSite,
    ) -> Result<(), ReachingDefinitionError> {
        match node {
            WirNode::SafeStore { place, .. } | WirNode::SafeStore8 { place, .. } => {
                self.charge_place_claim_structure(place, site)
            }
            WirNode::SafeMemoryCopy { destination, source, .. } => {
                self.charge_place_claim_structure(destination, site)?;
                self.charge_place_claim_structure(source, site)
            }
            WirNode::SafeMemoryFill { destination, .. } => {
                self.charge_place_claim_structure(destination, site)
            }
            WirNode::SafeKernel(operation) => {
                use crate::safewir_kernel::SafeKernelNode;
                match operation {
                    SafeKernelNode::RegionRewind(_) | SafeKernelNode::ScratchRewind { .. } => Ok(()),
                    SafeKernelNode::Release { object }
                    | SafeKernelNode::Reclaim { object } => {
                        self.charge_object_claim_structure(object, site)
                    }
                }
            }
            _ => Ok(()),
        }
    }

    fn charge_expr_claim_structure(
        &mut self,
        expression: &WirExpr,
        site: &StructuralSite,
    ) -> Result<(), ReachingDefinitionError> {
        match expression {
            WirExpr::SafeLoad { place, .. } | WirExpr::SafeLoad8U { place } => {
                self.charge_place_claim_structure(place, site)
            }
            WirExpr::SafeKernel(operation) => {
                use crate::safewir_kernel::SafeKernelExpr;
                match operation {
                    SafeKernelExpr::RegionMark | SafeKernelExpr::ScratchMark => Ok(()),
                    SafeKernelExpr::Allocate { result, size, .. } => {
                        self.charge_structural(site)?;
                        self.charge_extent_claim_structure(&result.extent, site)?;
                        self.charge_structural(site)?;
                        self.charge_size_claim_structure(&size.checked, site)
                    }
                    SafeKernelExpr::ScratchAllocate { result, size } => {
                        self.charge_structural(site)?;
                        self.charge_extent_claim_structure(&result.extent, site)?;
                        self.charge_structural(site)?;
                        self.charge_size_claim_structure(&size.checked, site)
                    }
                    SafeKernelExpr::Retain { object } => {
                        self.charge_object_claim_structure(object, site)
                    }
                    SafeKernelExpr::Reuse { object, result, size } => {
                        self.charge_object_claim_structure(object, site)?;
                        self.charge_structural(site)?;
                        self.charge_extent_claim_structure(&result.extent, site)?;
                        self.charge_structural(site)?;
                        self.charge_size_claim_structure(&size.checked, site)
                    }
                }
            }
            _ => Ok(()),
        }
    }

    fn charge_place_claim_structure(
        &mut self,
        place: &crate::safewir_surface::SafePlaceExpr,
        site: &StructuralSite,
    ) -> Result<(), ReachingDefinitionError> {
        self.charge_structural(site)?;
        self.charge_object_claim_structure(&place.object, site)?;
        self.charge_projection_claim_structure(&place.projection, site)?;
        self.charge_structural(site)?;
        self.charge_size_claim_structure(&place.range.start, site)?;
        self.charge_size_claim_structure(&place.range.length, site)
    }

    fn charge_object_claim_structure(
        &mut self,
        object: &crate::safewir_surface::SafeObjectExpr,
        site: &StructuralSite,
    ) -> Result<(), ReachingDefinitionError> {
        use crate::safewir_surface::SafeProvenanceExpr;

        self.charge_structural(site)?;
        self.charge_extent_claim_structure(&object.extent, site)?;
        self.charge_structural(site)?;
        if let SafeProvenanceExpr::Aliases { projection, .. } = &object.provenance {
            self.charge_projection_claim_structure(projection, site)?;
        }
        Ok(())
    }

    fn charge_extent_claim_structure(
        &mut self,
        extent: &crate::safewir_surface::SafeExtentExpr,
        site: &StructuralSite,
    ) -> Result<(), ReachingDefinitionError> {
        use crate::safewir_surface::SafeExtentExpr;

        self.charge_structural(site)?;
        match extent {
            SafeExtentExpr::Unknown | SafeExtentExpr::ClosedSum => Ok(()),
            SafeExtentExpr::Exact(size) => self.charge_size_claim_structure(size, site),
            SafeExtentExpr::Counted { header, stride, .. } => {
                self.charge_size_claim_structure(header, site)?;
                self.charge_size_claim_structure(stride, site)
            }
        }
    }

    fn charge_size_claim_structure(
        &mut self,
        size: &crate::safewir_surface::SafeSizeExpr,
        site: &StructuralSite,
    ) -> Result<(), ReachingDefinitionError> {
        use crate::safewir_surface::SafeSizeExpr;

        let mut pending = vec![size];
        while let Some(current) = pending.pop() {
            self.charge_structural(site)?;
            match current {
                SafeSizeExpr::Constant(_) | SafeSizeExpr::Value(_) => {}
                SafeSizeExpr::Add(left, right) | SafeSizeExpr::Multiply(left, right) => {
                    pending.push(right.as_ref());
                    pending.push(left.as_ref());
                }
                SafeSizeExpr::NarrowU32(inner) => pending.push(inner.as_ref()),
            }
        }
        Ok(())
    }

    fn charge_projection_claim_structure(
        &mut self,
        projection: &crate::safewir_surface::SafeProjectionExpr,
        site: &StructuralSite,
    ) -> Result<(), ReachingDefinitionError> {
        self.charge_structural(site)?;
        for _ in projection.steps() {
            self.charge_structural(site)?;
        }
        Ok(())
    }

    fn analyze_seq(
        &mut self,
        nodes: &WirSeq,
        parent: &StructuralSite,
        environment: RawEnvironment,
    ) -> Result<Outcome, ReachingDefinitionError> {
        let mut result = Outcome::normal(environment);
        for (index, node) in nodes.iter().enumerate() {
            let Some(current) = result.fallthrough.take() else { break };
            let index = self.u32_index(index, Some(parent.clone()), "node index")?;
            let site = parent.child(SiteStep::Node(index));
            let node_result = self.analyze_node(node, &site, current)?;
            result.fallthrough = node_result.fallthrough.clone();
            result.absorb_abrupt(node_result);
        }
        Ok(result)
    }

    fn analyze_node(
        &mut self,
        node: &WirNode,
        site: &StructuralSite,
        environment: RawEnvironment,
    ) -> Result<Outcome, ReachingDefinitionError> {
        self.charge_facts(environment.len().max(1), site)?;
        let mut operands = Outcome::normal(environment);
        for_each_node_operand(node, |index, mode, expression| {
            let Some(current) = operands.fallthrough.take() else { return Ok(()) };
            let index = self.u32_index(index, Some(site.clone()), "node operand index")?;
            let expression_site = site.child(mode.site_step(index));
            match mode {
                OperandMode::ClaimOnly => {
                    self.analyze_claim_expr(expression, &expression_site, &current)?;
                    operands.fallthrough = Some(current);
                }
                OperandMode::Executable => {
                    let expression_result = self.analyze_expr(
                        expression,
                        &expression_site,
                        current,
                    )?;
                    operands.fallthrough = expression_result.fallthrough.clone();
                    operands.absorb_abrupt(expression_result);
                }
            }
            Ok(())
        })?;
        let Some(mut current) = operands.fallthrough.take() else { return Ok(operands) };

        let mut result = match node {
            WirNode::SetLocal { local, .. } => {
                current.insert(local.clone(), BTreeSet::from([site.child(SiteStep::Write)]));
                Outcome::normal(current)
            }
            WirNode::CallStoreMulti { dests, .. }
            | WirNode::CallIndirectStoreMulti { dests, .. } => {
                // The encoder pops results and writes destinations in reverse.
                // Applying the same order also gives exact behavior for repeated
                // destination names instead of silently assuming uniqueness.
                for (result_index, local) in dests.iter().enumerate().rev() {
                    let result_index = self.u32_index(
                        result_index,
                        Some(site.clone()),
                        "call result index",
                    )?;
                    current.insert(
                        local.clone(),
                        BTreeSet::from([site.child(SiteStep::Destination(result_index))]),
                    );
                }
                Outcome::normal(current)
            }
            WirNode::Source { body, .. } => self.analyze_seq(
                body,
                &site.child(SiteStep::Region(StructuredRegion::SourceBody)),
                current,
            )?,
            WirNode::If { then_, els, .. } => {
                let then_result = self.analyze_seq(
                    then_,
                    &site.child(SiteStep::Region(StructuredRegion::Then)),
                    current.clone(),
                )?;
                let else_result = self.analyze_seq(
                    els,
                    &site.child(SiteStep::Region(StructuredRegion::Else)),
                    current,
                )?;
                let mut branch_exits = Vec::new();
                if let Some(environment) = then_result.fallthrough.clone() {
                    branch_exits.push(environment);
                }
                if let Some(environment) = else_result.fallthrough.clone() {
                    branch_exits.push(environment);
                }
                let fallthrough = self.merge_environments(site, JoinKind::IfExit, branch_exits)?;
                let mut joined = Outcome { fallthrough, ..Outcome::default() };
                joined.absorb_abrupt(then_result);
                joined.absorb_abrupt(else_result);
                joined
            }
            WirNode::Block { label, body, .. } => {
                let mut body_result = self.analyze_seq(
                    body,
                    &site.child(SiteStep::Region(StructuredRegion::BlockBody)),
                    current,
                )?;
                let mut exits = body_result.branches.remove(label).unwrap_or_default();
                if let Some(environment) = body_result.fallthrough.take() {
                    exits.push(environment);
                }
                body_result.fallthrough = self.merge_environments(site, JoinKind::BlockExit, exits)?;
                body_result
            }
            WirNode::Loop { label, body } => self.analyze_loop(label, body, site, current)?,
            WirNode::Br { target, cond } => {
                let mut branch = Outcome::default();
                branch.branches.entry(target.clone()).or_default().push(current.clone());
                if cond.is_some() {
                    branch.fallthrough = Some(current);
                }
                branch
            }
            WirNode::Return(_) => Outcome { returns: vec![current], ..Outcome::default() },
            WirNode::Unreachable => Outcome::default(),
            WirNode::SetGlobal { .. }
            | WirNode::Store { .. }
            | WirNode::SafeStore { .. }
            | WirNode::MemoryCopy { .. }
            | WirNode::SafeMemoryCopy { .. }
            | WirNode::MemoryFill { .. }
            | WirNode::SafeMemoryFill { .. }
            | WirNode::Store8 { .. }
            | WirNode::SafeStore8 { .. }
            | WirNode::SafeKernel(_)
            | WirNode::Drop(_)
            | WirNode::Do(_)
            | WirNode::Push(_)
            | WirNode::StructSet { .. }
            | WirNode::ArraySet { .. } => Outcome::normal(current),
        };
        result.absorb_abrupt(operands);
        Ok(result)
    }

    fn analyze_expr(
        &mut self,
        expression: &WirExpr,
        site: &StructuralSite,
        environment: RawEnvironment,
    ) -> Result<Outcome, ReachingDefinitionError> {
        self.charge_facts(environment.len().max(1), site)?;
        let mut result = Outcome::normal(environment);
        for_each_expr_operand(expression, |index, mode, child| {
            let Some(current) = result.fallthrough.take() else { return Ok(()) };
            let index = self.u32_index(index, Some(site.clone()), "expression operand index")?;
            let child_site = site.child(mode.site_step(index));
            match mode {
                OperandMode::ClaimOnly => {
                    self.analyze_claim_expr(child, &child_site, &current)?;
                    result.fallthrough = Some(current);
                }
                OperandMode::Executable => {
                    let child_result = self.analyze_expr(child, &child_site, current)?;
                    result.fallthrough = child_result.fallthrough.clone();
                    result.absorb_abrupt(child_result);
                }
            }
            Ok(())
        })?;

        let Some(current) = result.fallthrough.take() else { return Ok(result) };
        let expression_result = match expression {
            WirExpr::GetLocal(local) => {
                let reaching = current.get(local).cloned().ok_or_else(|| self.error(
                    ReachingDefinitionDiagnosticCode::UnknownLocal,
                    Some(site.clone()),
                    format!("local `{local}` has no entry or reaching definition"),
                ))?;
                if reaching.len() > self.budget.max_reaching_set_size {
                    return Err(self.error(
                        ReachingDefinitionDiagnosticCode::ReachingSetBudgetExceeded,
                        Some(site.clone()),
                        format!(
                            "local `{local}` reaches {} definitions, above configured limit {}",
                            reaching.len(), self.budget.max_reaching_set_size
                        ),
                    ));
                }
                self.reads.insert(site.clone(), RawRead { local: local.clone(), reaching });
                Outcome::normal(current)
            }
            WirExpr::Control(node) => self.analyze_node(
                node,
                &site.child(SiteStep::Region(StructuredRegion::ControlExpression)),
                current,
            )?,
            WirExpr::Seq(nodes) => self.analyze_seq(
                nodes,
                &site.child(SiteStep::Region(StructuredRegion::SequenceExpression)),
                current,
            )?,
            _ => Outcome::normal(current),
        };
        result.fallthrough = expression_result.fallthrough.clone();
        result.absorb_abrupt(expression_result);
        Ok(result)
    }

    fn analyze_claim_expr(
        &mut self,
        expression: &WirExpr,
        site: &StructuralSite,
        environment: &RawEnvironment,
    ) -> Result<(), ReachingDefinitionError> {
        self.charge_facts(environment.len().max(1), site)?;
        if let WirExpr::GetLocal(local) = expression {
            let reaching = environment.get(local).cloned().ok_or_else(|| self.error(
                ReachingDefinitionDiagnosticCode::UnknownLocal,
                Some(site.clone()),
                format!("claim local `{local}` has no entry or reaching definition"),
            ))?;
            if reaching.len() > self.budget.max_reaching_set_size {
                return Err(self.error(
                    ReachingDefinitionDiagnosticCode::ReachingSetBudgetExceeded,
                    Some(site.clone()),
                    format!(
                        "claim local `{local}` reaches {} definitions, above configured limit {}",
                        reaching.len(), self.budget.max_reaching_set_size
                    ),
                ));
            }
            self.claim_local_uses.insert(
                site.clone(),
                RawRead { local: local.clone(), reaching },
            );
        }
        debug_assert!(!matches!(expression, WirExpr::Control(_) | WirExpr::Seq(_)));
        for_each_claim_expr_operand(expression, |index, child| {
            let index = self.u32_index(index, Some(site.clone()), "claim operand index")?;
            let child_site = site.child(SiteStep::ClaimOperand(index));
            self.analyze_claim_expr(child, &child_site, environment)
        })
    }

    fn analyze_loop(
        &mut self,
        label: &str,
        body: &WirSeq,
        site: &StructuralSite,
        entry: RawEnvironment,
    ) -> Result<Outcome, ReachingDefinitionError> {
        let body_site = site.child(SiteStep::Region(StructuredRegion::LoopBody));
        let mut header = entry.clone();
        for iteration in 0..self.budget.max_fixpoint_iterations {
            self.loop_iterations = self.loop_iterations.checked_add(1).ok_or_else(|| self.error(
                ReachingDefinitionDiagnosticCode::FactBudgetExceeded,
                Some(site.clone()),
                "loop-iteration counter overflowed",
            ))?;
            self.charge_facts(1, site)?;

            let mut body_result = self.analyze_seq(body, &body_site, header.clone())?;
            let back_edges = body_result.branches.remove(label).unwrap_or_default();
            let header_capacity = back_edges.len().checked_add(1).ok_or_else(|| self.error(
                ReachingDefinitionDiagnosticCode::FactBudgetExceeded,
                Some(site.clone()),
                "loop header input count overflowed",
            ))?;
            let mut header_inputs = Vec::with_capacity(header_capacity);
            header_inputs.push(entry.clone());
            header_inputs.extend(back_edges);
            let next_header = self.merge_environments(
                site,
                JoinKind::LoopHeader,
                header_inputs,
            )?.ok_or_else(|| self.error(
                ReachingDefinitionDiagnosticCode::UnsupportedStructure,
                Some(site.clone()),
                "loop entry did not produce a header environment",
            ))?;

            if next_header == header {
                return Ok(body_result);
            }
            header = next_header;

            if iteration + 1 == self.budget.max_fixpoint_iterations {
                break;
            }
        }
        Err(self.error(
            ReachingDefinitionDiagnosticCode::FixpointBudgetExceeded,
            Some(site.clone()),
            format!(
                "loop `{label}` did not converge within {} iterations",
                self.budget.max_fixpoint_iterations
            ),
        ))
    }

    fn merge_environments(
        &mut self,
        site: &StructuralSite,
        kind: JoinKind,
        environments: Vec<RawEnvironment>,
    ) -> Result<Option<RawEnvironment>, ReachingDefinitionError> {
        if environments.is_empty() {
            return Ok(None);
        }
        let fact_cost = self.locals.len().checked_mul(environments.len()).ok_or_else(|| self.error(
            ReachingDefinitionDiagnosticCode::FactBudgetExceeded,
            Some(site.clone()),
            "join fact cost overflowed",
        ))?;
        self.charge_facts(fact_cost, site)?;

        let mut merged = RawEnvironment::new();
        for local_index in 0..self.locals.len() {
            let local = self.locals[local_index].clone();
            let first = environments[0].get(local.as_str()).ok_or_else(|| self.error(
                ReachingDefinitionDiagnosticCode::UnsupportedStructure,
                Some(site.clone()),
                format!("join input omitted declared local `{local}`"),
            ))?;
            let same = environments.iter().skip(1).all(|environment| {
                environment.get(local.as_str()) == Some(first)
            });
            let local_ordinal = self.local_ordinals[local.as_str()];
            let join_site = site.child(SiteStep::Join { kind, local: local_ordinal });
            if same {
                self.joins.remove(&join_site);
                self.definitions.remove(&join_site);
                merged.insert(local, first.clone());
                continue;
            }

            let mut predecessors = Vec::with_capacity(environments.len());
            let mut inputs = RawSet::new();
            for environment in &environments {
                let definitions = environment.get(local.as_str()).ok_or_else(|| self.error(
                    ReachingDefinitionDiagnosticCode::UnsupportedStructure,
                    Some(site.clone()),
                    format!("join input omitted declared local `{local}`"),
                ))?;
                self.charge_facts(definitions.len(), site)?;
                predecessors.push(definitions.clone());
                inputs.extend(definitions.iter().cloned());
            }
            if inputs.len() > self.budget.max_reaching_set_size {
                return Err(self.error(
                    ReachingDefinitionDiagnosticCode::ReachingSetBudgetExceeded,
                    Some(join_site),
                    format!(
                        "join for `{local}` has {} inputs, above configured limit {}",
                        inputs.len(), self.budget.max_reaching_set_size
                    ),
                ));
            }
            self.register_definition(
                join_site.clone(),
                DefinitionKind::Join { local: local.clone(), kind },
            )?;
            self.joins.insert(
                join_site.clone(),
                RawJoin { local: local.clone(), kind, predecessors, inputs },
            );
            merged.insert(local, BTreeSet::from([join_site]));
        }
        Ok(Some(merged))
    }

    fn register_definition(
        &mut self,
        site: StructuralSite,
        kind: DefinitionKind,
    ) -> Result<(), ReachingDefinitionError> {
        if let Some(existing) = self.definitions.get(&site) {
            if existing == &kind {
                return Ok(());
            }
            return Err(self.error(
                ReachingDefinitionDiagnosticCode::UnsupportedStructure,
                Some(site),
                "two definition kinds occupy one structural site",
            ));
        }
        if self.definitions.len() >= self.budget.max_definitions {
            return Err(self.error(
                ReachingDefinitionDiagnosticCode::DefinitionBudgetExceeded,
                Some(site),
                format!(
                    "definition count exceeds configured limit {}",
                    self.budget.max_definitions
                ),
            ));
        }
        self.definitions.insert(site, kind);
        Ok(())
    }

    fn charge_structural(&mut self, site: &StructuralSite) -> Result<(), ReachingDefinitionError> {
        self.structural_sites = self.structural_sites.checked_add(1).ok_or_else(|| self.error(
            ReachingDefinitionDiagnosticCode::StructuralBudgetExceeded,
            Some(site.clone()),
            "structural-site counter overflowed",
        ))?;
        if self.structural_sites > self.budget.max_structural_sites {
            return Err(self.error(
                ReachingDefinitionDiagnosticCode::StructuralBudgetExceeded,
                Some(site.clone()),
                format!(
                    "structural-site count exceeds configured limit {}",
                    self.budget.max_structural_sites
                ),
            ));
        }
        Ok(())
    }

    fn charge_facts(
        &mut self,
        amount: usize,
        site: &StructuralSite,
    ) -> Result<(), ReachingDefinitionError> {
        self.fact_operations = self.fact_operations.checked_add(amount).ok_or_else(|| self.error(
            ReachingDefinitionDiagnosticCode::FactBudgetExceeded,
            Some(site.clone()),
            "fact-operation counter overflowed",
        ))?;
        if self.fact_operations > self.budget.max_fact_operations {
            return Err(self.error(
                ReachingDefinitionDiagnosticCode::FactBudgetExceeded,
                Some(site.clone()),
                format!(
                    "fact operations exceed configured limit {}",
                    self.budget.max_fact_operations
                ),
            ));
        }
        Ok(())
    }

    fn require_local(
        &self,
        local: &str,
        site: &StructuralSite,
    ) -> Result<(), ReachingDefinitionError> {
        if self.local_ordinals.contains_key(local) {
            return Ok(());
        }
        Err(self.error(
            ReachingDefinitionDiagnosticCode::UnknownLocal,
            Some(site.clone()),
            format!("local `{local}` is not declared by the function"),
        ))
    }

    fn push_label(
        &self,
        label: &str,
        labels: &mut Vec<String>,
        site: &StructuralSite,
    ) -> Result<(), ReachingDefinitionError> {
        if labels.iter().any(|active| active == label) {
            return Err(self.error(
                ReachingDefinitionDiagnosticCode::DuplicateControlLabel,
                Some(site.clone()),
                format!("control label `{label}` shadows a lexically enclosing label"),
            ));
        }
        labels.push(label.to_string());
        Ok(())
    }

    fn next_control_depth(
        &self,
        depth: usize,
        site: &StructuralSite,
    ) -> Result<usize, ReachingDefinitionError> {
        let next = depth.checked_add(1).ok_or_else(|| self.error(
            ReachingDefinitionDiagnosticCode::ControlDepthExceeded,
            Some(site.clone()),
            "control depth overflowed",
        ))?;
        if next > self.budget.max_control_depth {
            return Err(self.error(
                ReachingDefinitionDiagnosticCode::ControlDepthExceeded,
                Some(site.clone()),
                format!(
                    "control depth exceeds configured limit {}",
                    self.budget.max_control_depth
                ),
            ));
        }
        Ok(next)
    }

    fn next_nesting_depth(
        &self,
        depth: usize,
        site: &StructuralSite,
    ) -> Result<usize, ReachingDefinitionError> {
        let next = depth.checked_add(1).ok_or_else(|| self.error(
            ReachingDefinitionDiagnosticCode::ExpressionDepthExceeded,
            Some(site.clone()),
            "structured-WIR nesting depth overflowed",
        ))?;
        if next > self.budget.max_expression_depth {
            return Err(self.error(
                ReachingDefinitionDiagnosticCode::ExpressionDepthExceeded,
                Some(site.clone()),
                format!(
                    "structured-WIR nesting depth exceeds configured limit {}",
                    self.budget.max_expression_depth
                ),
            ));
        }
        Ok(next)
    }

    fn u32_index(
        &self,
        value: usize,
        site: Option<StructuralSite>,
        description: &str,
    ) -> Result<u32, ReachingDefinitionError> {
        u32::try_from(value).map_err(|_| self.error(
            ReachingDefinitionDiagnosticCode::DefinitionIdExhausted,
            site,
            format!("{description} does not fit in the structural ID space"),
        ))
    }

    fn error(
        &self,
        code: ReachingDefinitionDiagnosticCode,
        site: Option<StructuralSite>,
        message: impl Into<String>,
    ) -> ReachingDefinitionError {
        ReachingDefinitionError {
            diagnostic: ReachingDefinitionDiagnostic {
                code,
                function: self.function.clone(),
                site,
                message: message.into(),
            },
        }
    }

    fn error_for(
        function: &WirFunc,
        code: ReachingDefinitionDiagnosticCode,
        site: Option<StructuralSite>,
        message: impl Into<String>,
    ) -> ReachingDefinitionError {
        ReachingDefinitionError {
            diagnostic: ReachingDefinitionDiagnostic {
                code,
                function: function.name.clone(),
                site,
                message: message.into(),
            },
        }
    }
}

fn expression_form(expression: &WirExpr) -> ExpressionForm {
    match expression {
        WirExpr::ConstI64(_) => ExpressionForm::ConstI64,
        WirExpr::ConstF64(_) => ExpressionForm::ConstF64,
        WirExpr::ConstI32(_) => ExpressionForm::ConstI32,
        WirExpr::ConstV128(_) => ExpressionForm::ConstV128,
        WirExpr::StrPtr(_) => ExpressionForm::StringPointer,
        WirExpr::GetLocal(_) => ExpressionForm::GetLocal,
        WirExpr::GetGlobal(_) => ExpressionForm::GetGlobal,
        WirExpr::Vector { .. } => ExpressionForm::Vector,
        WirExpr::ToSlot(_, _) => ExpressionForm::ToSlot,
        WirExpr::FromSlot(_, _) => ExpressionForm::FromSlot,
        WirExpr::Binary { .. } => ExpressionForm::Binary,
        WirExpr::Unary { .. } => ExpressionForm::Unary,
        WirExpr::Convert { .. } => ExpressionForm::Convert,
        WirExpr::Load { .. } => ExpressionForm::Load,
        WirExpr::SafeLoad { .. } => ExpressionForm::SafeLoad,
        WirExpr::MemorySize => ExpressionForm::MemorySize,
        WirExpr::MemoryGrow(_) => ExpressionForm::MemoryGrow,
        WirExpr::Load8U { .. } => ExpressionForm::Load8U,
        WirExpr::SafeLoad8U { .. } => ExpressionForm::SafeLoad8U,
        WirExpr::SafeKernel(_) => ExpressionForm::SafeKernel,
        WirExpr::Call { .. } => ExpressionForm::Call,
        WirExpr::CallHost { .. } => ExpressionForm::CallHost,
        WirExpr::CallIndirect { .. } => ExpressionForm::CallIndirect,
        WirExpr::Control(_) => ExpressionForm::Control,
        WirExpr::Seq(_) => ExpressionForm::Sequence,
        WirExpr::StructNew { .. } => ExpressionForm::StructNew,
        WirExpr::StructGet { .. } => ExpressionForm::StructGet,
        WirExpr::ArrayNew { .. } => ExpressionForm::ArrayNew,
        WirExpr::ArrayNewFixed { .. } => ExpressionForm::ArrayNewFixed,
        WirExpr::ArrayGet { .. } => ExpressionForm::ArrayGet,
        WirExpr::ArrayLen(_) => ExpressionForm::ArrayLen,
        WirExpr::RefCast { .. } => ExpressionForm::RefCast,
        WirExpr::RefCastNullable { .. } => ExpressionForm::RefCastNullable,
        WirExpr::RefNull(_) => ExpressionForm::RefNull,
        WirExpr::RefIsNull(_) => ExpressionForm::RefIsNull,
    }
}

fn for_each_expr_operand(
    expression: &WirExpr,
    mut visit: impl FnMut(usize, OperandMode, &WirExpr) -> Result<(), ReachingDefinitionError>,
) -> Result<(), ReachingDefinitionError> {
    let mut index = OperandCounters::default();
    match expression {
        WirExpr::Vector { args, .. }
        | WirExpr::Call { args, .. }
        | WirExpr::CallHost { args, .. }
        | WirExpr::StructNew { args, .. }
        | WirExpr::ArrayNewFixed { items: args, .. } => {
            for argument in args {
                visit_operand(&mut index, OperandMode::Executable, argument, &mut visit)?;
            }
        }
        WirExpr::ToSlot(inner, _)
        | WirExpr::FromSlot(inner, _)
        | WirExpr::Unary { arg: inner, .. }
        | WirExpr::Convert { arg: inner, .. }
        | WirExpr::MemoryGrow(inner)
        | WirExpr::ArrayLen(inner)
        | WirExpr::RefIsNull(inner) => {
            visit_operand(&mut index, OperandMode::Executable, inner, &mut visit)?;
        }
        WirExpr::Binary { lhs, rhs, .. } => {
            visit_operand(&mut index, OperandMode::Executable, lhs, &mut visit)?;
            visit_operand(&mut index, OperandMode::Executable, rhs, &mut visit)?;
        }
        WirExpr::Load { ptr, .. } | WirExpr::Load8U { ptr, .. } => {
            visit_operand(&mut index, OperandMode::Executable, ptr, &mut visit)?;
        }
        WirExpr::SafeLoad { place, .. } | WirExpr::SafeLoad8U { place } => {
            visit_safe_place_operands(place, &mut index, &mut visit)?;
        }
        WirExpr::SafeKernel(operation) => {
            visit_safe_kernel_expr_operands(operation, &mut index, &mut visit)?;
        }
        WirExpr::CallIndirect { args, index: call_index, .. } => {
            for argument in args {
                visit_operand(&mut index, OperandMode::Executable, argument, &mut visit)?;
            }
            visit_operand(&mut index, OperandMode::Executable, call_index, &mut visit)?;
        }
        WirExpr::ArrayNew { value, len, .. } => {
            visit_operand(&mut index, OperandMode::Executable, value, &mut visit)?;
            visit_operand(&mut index, OperandMode::Executable, len, &mut visit)?;
        }
        WirExpr::ArrayGet { array, index: array_index, .. } => {
            visit_operand(&mut index, OperandMode::Executable, array, &mut visit)?;
            visit_operand(&mut index, OperandMode::Executable, array_index, &mut visit)?;
        }
        WirExpr::StructGet { base, .. } => {
            visit_operand(&mut index, OperandMode::Executable, base, &mut visit)?;
        }
        WirExpr::RefCast { value, .. } | WirExpr::RefCastNullable { value, .. } => {
            visit_operand(&mut index, OperandMode::Executable, value, &mut visit)?;
        }
        WirExpr::ConstI64(_)
        | WirExpr::ConstF64(_)
        | WirExpr::ConstI32(_)
        | WirExpr::ConstV128(_)
        | WirExpr::StrPtr(_)
        | WirExpr::GetLocal(_)
        | WirExpr::GetGlobal(_)
        | WirExpr::MemorySize
        | WirExpr::Control(_)
        | WirExpr::Seq(_)
        | WirExpr::RefNull(_) => {}
    }
    Ok(())
}

fn for_each_claim_expr_operand(
    expression: &WirExpr,
    mut visit: impl FnMut(usize, &WirExpr) -> Result<(), ReachingDefinitionError>,
) -> Result<(), ReachingDefinitionError> {
    let mut claim_index = 0usize;
    for_each_expr_operand(expression, |_erasure_mode_index, _mode, child| {
        let current = claim_index;
        claim_index = claim_index.saturating_add(1);
        visit(current, child)
    })
}

fn for_each_node_operand(
    node: &WirNode,
    mut visit: impl FnMut(usize, OperandMode, &WirExpr) -> Result<(), ReachingDefinitionError>,
) -> Result<(), ReachingDefinitionError> {
    let mut index = OperandCounters::default();
    match node {
        WirNode::SetLocal { value, .. }
        | WirNode::SetGlobal { value, .. }
        | WirNode::Drop(value)
        | WirNode::Do(value)
        | WirNode::Push(value)
        | WirNode::Return(Some(value)) => {
            visit_operand(&mut index, OperandMode::Executable, value, &mut visit)?;
        }
        WirNode::Store { ptr, value, .. } | WirNode::Store8 { ptr, value, .. } => {
            visit_operand(&mut index, OperandMode::Executable, ptr, &mut visit)?;
            visit_operand(&mut index, OperandMode::Executable, value, &mut visit)?;
        }
        WirNode::SafeStore { place, value, .. } | WirNode::SafeStore8 { place, value } => {
            visit_safe_place_operands(place, &mut index, &mut visit)?;
            visit_operand(&mut index, OperandMode::Executable, value, &mut visit)?;
        }
        WirNode::CallStoreMulti { args, .. } => {
            for argument in args {
                visit_operand(&mut index, OperandMode::Executable, argument, &mut visit)?;
            }
        }
        WirNode::CallIndirectStoreMulti { args, index: call_index, .. } => {
            for argument in args {
                visit_operand(&mut index, OperandMode::Executable, argument, &mut visit)?;
            }
            visit_operand(&mut index, OperandMode::Executable, call_index, &mut visit)?;
        }
        WirNode::MemoryCopy { dest, src, len } => {
            visit_operand(&mut index, OperandMode::Executable, dest, &mut visit)?;
            visit_operand(&mut index, OperandMode::Executable, src, &mut visit)?;
            visit_operand(&mut index, OperandMode::Executable, len, &mut visit)?;
        }
        WirNode::SafeMemoryCopy { destination, source, len, .. } => {
            visit_safe_place_operands(destination, &mut index, &mut visit)?;
            visit_safe_place_operands(source, &mut index, &mut visit)?;
            visit_operand(&mut index, OperandMode::Executable, len, &mut visit)?;
        }
        WirNode::MemoryFill { dest, value, len } => {
            visit_operand(&mut index, OperandMode::Executable, dest, &mut visit)?;
            visit_operand(&mut index, OperandMode::Executable, value, &mut visit)?;
            visit_operand(&mut index, OperandMode::Executable, len, &mut visit)?;
        }
        WirNode::SafeMemoryFill { destination, value, len } => {
            visit_safe_place_operands(destination, &mut index, &mut visit)?;
            visit_operand(&mut index, OperandMode::Executable, value, &mut visit)?;
            visit_operand(&mut index, OperandMode::Executable, len, &mut visit)?;
        }
        WirNode::SafeKernel(operation) => {
            visit_safe_kernel_node_operands(operation, &mut index, &mut visit)?;
        }
        WirNode::If { cond, .. } | WirNode::Br { cond: Some(cond), .. } => {
            visit_operand(&mut index, OperandMode::Executable, cond, &mut visit)?;
        }
        WirNode::StructSet { base, value, .. } => {
            visit_operand(&mut index, OperandMode::Executable, base, &mut visit)?;
            visit_operand(&mut index, OperandMode::Executable, value, &mut visit)?;
        }
        WirNode::ArraySet { array, index: array_index, value, .. } => {
            visit_operand(&mut index, OperandMode::Executable, array, &mut visit)?;
            visit_operand(&mut index, OperandMode::Executable, array_index, &mut visit)?;
            visit_operand(&mut index, OperandMode::Executable, value, &mut visit)?;
        }
        WirNode::Source { .. }
        | WirNode::Block { .. }
        | WirNode::Loop { .. }
        | WirNode::Br { cond: None, .. }
        | WirNode::Return(None)
        | WirNode::Unreachable => {}
    }
    Ok(())
}

#[allow(dead_code)]
pub(crate) fn structural_node_operands(
    node: &WirNode,
    parent: &StructuralSite,
) -> Option<Vec<StructuralOperand>> {
    let mut output = Vec::new();
    let mut overflowed = false;
    for_each_node_operand(node, |index, mode, expression| {
        push_structural_operand(
            &mut output,
            &mut overflowed,
            parent,
            index,
            mode,
            expression,
        );
        Ok(())
    })
    .ok()?;
    (!overflowed).then_some(output)
}

#[allow(dead_code)]
pub(crate) fn structural_expr_operands(
    expression: &WirExpr,
    parent: &StructuralSite,
) -> Option<Vec<StructuralOperand>> {
    let mut output = Vec::new();
    let mut overflowed = false;
    for_each_expr_operand(expression, |index, mode, child| {
        push_structural_operand(
            &mut output,
            &mut overflowed,
            parent,
            index,
            mode,
            child,
        );
        Ok(())
    })
    .ok()?;
    (!overflowed).then_some(output)
}

#[allow(dead_code)]
fn push_structural_operand(
    output: &mut Vec<StructuralOperand>,
    overflowed: &mut bool,
    parent: &StructuralSite,
    index: usize,
    mode: OperandMode,
    expression: &WirExpr,
) {
    let Some(index) = u32::try_from(index).ok() else {
        *overflowed = true;
        return;
    };
    output.push(StructuralOperand {
        expression: std::ptr::from_ref(expression),
        claim_only: mode == OperandMode::ClaimOnly,
        site: parent.projected(mode.site_step(index)),
    });
}

fn visit_operand(
    index: &mut OperandCounters,
    mode: OperandMode,
    expression: &WirExpr,
    visit: &mut impl FnMut(usize, OperandMode, &WirExpr) -> Result<(), ReachingDefinitionError>,
) -> Result<(), ReachingDefinitionError> {
    let counter = match mode {
        OperandMode::Executable => &mut index.executable,
        OperandMode::ClaimOnly => &mut index.claims,
    };
    let current = *counter;
    let result = visit(current, mode, expression);
    // A saturated counter is rejected by the caller's structural u32
    // conversion before it can alias an earlier operand site.
    *counter = counter.saturating_add(1);
    result
}

fn visit_safe_place_operands(
    place: &crate::safewir_surface::SafePlaceExpr,
    index: &mut OperandCounters,
    visit: &mut impl FnMut(usize, OperandMode, &WirExpr) -> Result<(), ReachingDefinitionError>,
) -> Result<(), ReachingDefinitionError> {
    visit_safe_object_operands(
        &place.object,
        OperandMode::ClaimOnly,
        OperandMode::ClaimOnly,
        index,
        visit,
    )?;
    visit_safe_projection_operands(&place.projection, OperandMode::ClaimOnly, index, visit)?;
    visit_operand(
        index,
        OperandMode::ClaimOnly,
        place.range.object_root.as_ref(),
        visit,
    )?;
    visit_safe_size_operands(&place.range.start, OperandMode::ClaimOnly, index, visit)?;
    visit_safe_size_operands(&place.range.length, OperandMode::ClaimOnly, index, visit)?;
    // Erasure retains only `encoded_base`; every preceding field is an
    // untrusted redundant claim.
    visit_operand(index, OperandMode::Executable, place.encoded_base.as_ref(), visit)
}

fn visit_safe_kernel_expr_operands(
    operation: &crate::safewir_kernel::SafeKernelExpr,
    index: &mut OperandCounters,
    visit: &mut impl FnMut(usize, OperandMode, &WirExpr) -> Result<(), ReachingDefinitionError>,
) -> Result<(), ReachingDefinitionError> {
    use crate::safewir_kernel::SafeKernelExpr;

    match operation {
        SafeKernelExpr::RegionMark | SafeKernelExpr::ScratchMark => Ok(()),
        SafeKernelExpr::Allocate { result, size, .. } => {
            visit_safe_extent_operands(&result.extent, OperandMode::ClaimOnly, index, visit)?;
            visit_safe_size_operands(&size.checked, OperandMode::ClaimOnly, index, visit)?;
            visit_operand(index, OperandMode::Executable, size.encoded.as_ref(), visit)
        }
        SafeKernelExpr::ScratchAllocate { result, size } => {
            visit_safe_extent_operands(&result.extent, OperandMode::ClaimOnly, index, visit)?;
            visit_safe_size_operands(&size.checked, OperandMode::ClaimOnly, index, visit)?;
            visit_operand(index, OperandMode::Executable, size.encoded.as_ref(), visit)
        }
        SafeKernelExpr::Retain { object } => visit_safe_object_operands(
            object,
            OperandMode::Executable,
            OperandMode::ClaimOnly,
            index,
            visit,
        ),
        SafeKernelExpr::Reuse { object, result, size } => {
            visit_safe_object_operands(
                object,
                OperandMode::Executable,
                OperandMode::ClaimOnly,
                index,
                visit,
            )?;
            visit_safe_extent_operands(&result.extent, OperandMode::ClaimOnly, index, visit)?;
            visit_safe_size_operands(&size.checked, OperandMode::ClaimOnly, index, visit)?;
            visit_operand(index, OperandMode::Executable, size.encoded.as_ref(), visit)
        }
    }
}

fn visit_safe_kernel_node_operands(
    operation: &crate::safewir_kernel::SafeKernelNode,
    index: &mut OperandCounters,
    visit: &mut impl FnMut(usize, OperandMode, &WirExpr) -> Result<(), ReachingDefinitionError>,
) -> Result<(), ReachingDefinitionError> {
    use crate::safewir_kernel::{SafeKernelNode, SafeRegionRewind};

    match operation {
        SafeKernelNode::RegionRewind(rewind) => match rewind {
            SafeRegionRewind::Iteration { watermark }
            | SafeRegionRewind::Lexical { watermark } => {
                visit_operand(index, OperandMode::Executable, watermark.as_ref(), visit)
            }
            SafeRegionRewind::CopyOut { watermark, source, retained_bytes } => {
                visit_operand(index, OperandMode::Executable, watermark.as_ref(), visit)?;
                visit_operand(index, OperandMode::Executable, source.as_ref(), visit)?;
                visit_operand(index, OperandMode::Executable, retained_bytes.as_ref(), visit)
            }
        },
        SafeKernelNode::ScratchRewind { watermark } => {
            visit_operand(index, OperandMode::Executable, watermark.as_ref(), visit)
        }
        SafeKernelNode::Release { object } | SafeKernelNode::Reclaim { object } => {
            visit_safe_object_operands(
                object,
                OperandMode::Executable,
                OperandMode::ClaimOnly,
                index,
                visit,
            )
        }
    }
}

fn visit_safe_object_operands(
    object: &crate::safewir_surface::SafeObjectExpr,
    root_mode: OperandMode,
    metadata_mode: OperandMode,
    index: &mut OperandCounters,
    visit: &mut impl FnMut(usize, OperandMode, &WirExpr) -> Result<(), ReachingDefinitionError>,
) -> Result<(), ReachingDefinitionError> {
    use crate::safewir_surface::SafeProvenanceExpr;

    visit_operand(index, root_mode, object.root.as_ref(), visit)?;
    visit_safe_extent_operands(&object.extent, metadata_mode, index, visit)?;
    match &object.provenance {
        SafeProvenanceExpr::Fresh | SafeProvenanceExpr::Unknown => Ok(()),
        SafeProvenanceExpr::Aliases { object, projection } => {
            visit_operand(index, metadata_mode, object.as_ref(), visit)?;
            visit_safe_projection_operands(projection, metadata_mode, index, visit)
        }
        SafeProvenanceExpr::MayAlias(objects) => {
            for object in objects {
                visit_operand(index, metadata_mode, object, visit)?;
            }
            Ok(())
        }
        SafeProvenanceExpr::StaticData { object } => {
            visit_operand(index, metadata_mode, object.as_ref(), visit)
        }
    }
}

fn visit_safe_extent_operands(
    extent: &crate::safewir_surface::SafeExtentExpr,
    mode: OperandMode,
    index: &mut OperandCounters,
    visit: &mut impl FnMut(usize, OperandMode, &WirExpr) -> Result<(), ReachingDefinitionError>,
) -> Result<(), ReachingDefinitionError> {
    use crate::safewir_surface::SafeExtentExpr;

    match extent {
        SafeExtentExpr::Unknown | SafeExtentExpr::ClosedSum => Ok(()),
        SafeExtentExpr::Exact(size) => visit_safe_size_operands(size, mode, index, visit),
        SafeExtentExpr::Counted { header, count, stride } => {
            visit_safe_size_operands(header, mode, index, visit)?;
            visit_operand(index, mode, count.as_ref(), visit)?;
            visit_safe_size_operands(stride, mode, index, visit)
        }
    }
}

fn visit_safe_size_operands(
    size: &crate::safewir_surface::SafeSizeExpr,
    mode: OperandMode,
    index: &mut OperandCounters,
    visit: &mut impl FnMut(usize, OperandMode, &WirExpr) -> Result<(), ReachingDefinitionError>,
) -> Result<(), ReachingDefinitionError> {
    use crate::safewir_surface::SafeSizeExpr;

    // Iterative traversal is intentional: candidate claim arithmetic is also
    // untrusted and must not overflow the Rust stack before WIR budgets apply.
    let mut pending = vec![size];
    while let Some(current) = pending.pop() {
        match current {
            SafeSizeExpr::Constant(_) => {}
            SafeSizeExpr::Value(value) => {
                visit_operand(index, mode, value.as_ref(), visit)?;
            }
            SafeSizeExpr::Add(left, right) | SafeSizeExpr::Multiply(left, right) => {
                pending.push(right.as_ref());
                pending.push(left.as_ref());
            }
            SafeSizeExpr::NarrowU32(value) => pending.push(value.as_ref()),
        }
    }
    Ok(())
}

fn visit_safe_projection_operands(
    projection: &crate::safewir_surface::SafeProjectionExpr,
    mode: OperandMode,
    index: &mut OperandCounters,
    visit: &mut impl FnMut(usize, OperandMode, &WirExpr) -> Result<(), ReachingDefinitionError>,
) -> Result<(), ReachingDefinitionError> {
    use crate::safewir_surface::SafeProjectionStep;

    for step in projection.steps() {
        match step {
            SafeProjectionStep::Header { .. }
            | SafeProjectionStep::SumTag
            | SafeProjectionStep::VariantField { .. }
            | SafeProjectionStep::Field { .. } => {}
            SafeProjectionStep::Element { index: element }
            | SafeProjectionStep::Section { index: element, .. }
            | SafeProjectionStep::DerivedElement { index: element } => {
                visit_operand(index, mode, element.as_ref(), visit)?;
            }
            SafeProjectionStep::Slice { start, length } => {
                visit_operand(index, mode, start.as_ref(), visit)?;
                visit_operand(index, mode, length.as_ref(), visit)?;
            }
            SafeProjectionStep::DerivedScratchElement { cursor } => {
                visit_operand(index, mode, cursor.as_ref(), visit)?;
            }
        }
    }
    Ok(())
}
