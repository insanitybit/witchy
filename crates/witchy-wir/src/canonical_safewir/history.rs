use std::fmt;

use super::{CanonicalModule, FunctionId, InitialValid, ModuleStage, OptimizedValid, Revision,
    TransformedCandidate};

const OBSERVATION_MODEL_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ModuleDigest([u8; 32]);

impl ModuleDigest {
    pub const fn bytes(self) -> [u8; 32] {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OptimizationPassId {
    UnreachableBlockPruning,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DischargedObligation {
    PrunedBlocks { function: FunctionId, blocks: Vec<u32> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefinementReceipt {
    pass: OptimizationPassId,
    before_digest: ModuleDigest,
    after_digest: ModuleDigest,
    before_revision: Revision,
    after_revision: Revision,
    before_tail_digest: ModuleDigest,
    after_tail_digest: ModuleDigest,
    observation_model_version: u32,
    discharged_obligations: Vec<DischargedObligation>,
}

impl RefinementReceipt {
    pub const fn pass(&self) -> OptimizationPassId {
        self.pass
    }

    pub const fn before_digest(&self) -> ModuleDigest {
        self.before_digest
    }

    pub const fn after_digest(&self) -> ModuleDigest {
        self.after_digest
    }

    pub const fn before_revision(&self) -> Revision {
        self.before_revision
    }

    pub const fn after_revision(&self) -> Revision {
        self.after_revision
    }

    pub fn discharged_obligations(&self) -> &[DischargedObligation] {
        &self.discharged_obligations
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptimizationHistoryBody {
    checked_lowering_revision_digest: ModuleDigest,
    ordered: Vec<RefinementReceipt>,
    final_safewir_revision_digest: ModuleDigest,
    optimizer_policy_and_route_digest: ModuleDigest,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptimizationHistoryWitness {
    body: OptimizationHistoryBody,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistoryError {
    InitialDigestMismatch,
    BrokenChain { index: usize },
    InvalidObservationModel { index: usize },
    InvalidPass { index: usize },
    InvalidPolicyDigest,
    TailSetChanged { index: usize },
    FinalDigestMismatch,
    ZeroPassRevisionMismatch,
}

impl fmt::Display for HistoryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid canonical SafeWIR optimization history: {self:?}")
    }
}

impl std::error::Error for HistoryError {}

impl OptimizationHistoryWitness {
    pub fn ordered(&self) -> &[RefinementReceipt] {
        &self.body.ordered
    }

    pub const fn is_zero_pass(&self) -> bool {
        self.body.ordered.is_empty()
    }

    pub fn body(&self) -> &OptimizationHistoryBody {
        &self.body
    }

    pub fn validate(
        &self,
        initial: &CanonicalModule<InitialValid>,
        final_module: &CanonicalModule<OptimizedValid>,
    ) -> Result<(), HistoryError> {
        if self.body.optimizer_policy_and_route_digest != policy_and_route_digest() {
            return Err(HistoryError::InvalidPolicyDigest);
        }
        let initial_digest = module_digest(initial);
        if self.body.checked_lowering_revision_digest != initial_digest {
            return Err(HistoryError::InitialDigestMismatch);
        }

        let mut expected_digest = initial_digest;
        let mut expected_revision = initial.revision();
        for (index, receipt) in self.body.ordered.iter().enumerate() {
            if receipt.pass != OptimizationPassId::UnreachableBlockPruning {
                return Err(HistoryError::InvalidPass { index });
            }
            if receipt.before_digest != expected_digest
                || receipt.before_revision != expected_revision
            {
                return Err(HistoryError::BrokenChain { index });
            }
            if receipt.observation_model_version != OBSERVATION_MODEL_VERSION {
                return Err(HistoryError::InvalidObservationModel { index });
            }
            if receipt.before_tail_digest != receipt.after_tail_digest {
                return Err(HistoryError::TailSetChanged { index });
            }
            expected_digest = receipt.after_digest;
            expected_revision = receipt.after_revision;
        }

        let final_digest = module_digest(final_module);
        if self.body.final_safewir_revision_digest != final_digest
            || expected_digest != final_digest
            || expected_revision != final_module.revision()
        {
            return Err(HistoryError::FinalDigestMismatch);
        }
        if self.is_zero_pass() && initial.revision() != final_module.revision() {
            return Err(HistoryError::ZeroPassRevisionMismatch);
        }
        Ok(())
    }

    pub(crate) fn zero_pass(
        initial: &CanonicalModule<InitialValid>,
        final_module: &CanonicalModule<OptimizedValid>,
    ) -> Result<Self, HistoryError> {
        let initial_digest = module_digest(initial);
        let final_digest = module_digest(final_module);
        if initial_digest != final_digest || initial.revision() != final_module.revision() {
            return Err(HistoryError::FinalDigestMismatch);
        }
        Ok(Self {
            body: OptimizationHistoryBody {
                checked_lowering_revision_digest: initial_digest,
                ordered: Vec::new(),
                final_safewir_revision_digest: final_digest,
                optimizer_policy_and_route_digest: policy_and_route_digest(),
            },
        })
    }

    pub(crate) fn from_receipt(
        initial: &CanonicalModule<InitialValid>,
        final_module: &CanonicalModule<OptimizedValid>,
        receipt: RefinementReceipt,
    ) -> Result<Self, HistoryError> {
        let witness = Self {
            body: OptimizationHistoryBody {
                checked_lowering_revision_digest: module_digest(initial),
                ordered: vec![receipt],
                final_safewir_revision_digest: module_digest(final_module),
                optimizer_policy_and_route_digest: policy_and_route_digest(),
            },
        };
        witness.validate(initial, final_module)?;
        Ok(witness)
    }

    pub(crate) fn unreachable_receipt(
        before: &CanonicalModule<InitialValid>,
        after: &CanonicalModule<TransformedCandidate>,
        obligations: Vec<DischargedObligation>,
    ) -> RefinementReceipt {
        RefinementReceipt {
            pass: OptimizationPassId::UnreachableBlockPruning,
            before_digest: module_digest(before),
            after_digest: module_digest(after),
            before_revision: before.revision(),
            after_revision: after.revision(),
            before_tail_digest: tail_digest(before),
            after_tail_digest: tail_digest(after),
            observation_model_version: OBSERVATION_MODEL_VERSION,
            discharged_obligations: obligations,
        }
    }

    #[cfg(test)]
    pub(crate) fn tamper_first_after_digest(&mut self) {
        self.body.ordered[0].after_digest = ModuleDigest([0xa5; 32]);
    }
}

pub(crate) fn module_digest<S: ModuleStage>(module: &CanonicalModule<S>) -> ModuleDigest {
    let snapshot = format!(
        "{:?}",
        (
            module.revision(),
            module.checked_source(),
            module.layouts().canonical_bytes(),
            module.signatures(),
            module.imports(),
            module.functions(),
            module.proper_tail_sites(),
        )
    );
    digest_bytes(snapshot.as_bytes())
}

fn tail_digest<S: ModuleStage>(module: &CanonicalModule<S>) -> ModuleDigest {
    digest_bytes(format!("{:?}", module.proper_tail_sites()).as_bytes())
}

fn digest_bytes(bytes: &[u8]) -> ModuleDigest {
    let mut lanes = [
        0xcbf29ce484222325_u64,
        0x84222325cbf29ce4_u64,
        0x9e3779b185ebca87_u64,
        0xd6e8feb86659fd93_u64,
    ];
    for (index, byte) in bytes.iter().copied().enumerate() {
        let lane = index & 3;
        lanes[lane] = lanes[lane]
            .wrapping_mul(0x100000001b3)
            .wrapping_add(u64::from(byte) + 1);
        lanes[lane] ^= lanes[(lane + 1) & 3].rotate_left(13);
    }
    let mut output = [0; 32];
    for (index, lane) in lanes.into_iter().enumerate() {
        output[index * 8..(index + 1) * 8].copy_from_slice(&lane.to_le_bytes());
    }
    ModuleDigest(output)
}

fn policy_and_route_digest() -> ModuleDigest {
    digest_bytes(b"witchy-optimization-policy-v1:unreachable-block-pruning")
}
