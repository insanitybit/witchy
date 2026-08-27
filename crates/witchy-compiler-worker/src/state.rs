pub(crate) const PRODUCER_UNAVAILABLE_DIAGNOSTIC: &str = "RFC-0163 source-to-candidate producer unavailable: canonical candidate serializers and combined parent admission are not linked";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProducerState {
    Unavailable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublicationState {
    RequiresIndependentAdmission,
}

/// Machine-readable implementation truth.  This must not report a candidate
/// producer until all three canonical candidate serializers are linked.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkerImplementationState {
    pub producer: ProducerState,
    pub publication: PublicationState,
    pub linux_native_runtime_only: bool,
    pub t15_complete: bool,
}

pub const IMPLEMENTATION_STATE: WorkerImplementationState = WorkerImplementationState {
    producer: ProducerState::Unavailable,
    publication: PublicationState::RequiresIndependentAdmission,
    linux_native_runtime_only: true,
    t15_complete: false,
};
