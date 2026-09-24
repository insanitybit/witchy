//! Closure capture vocabulary: how a checked binding reaches a closure and how
//! its value crosses into the closure environment.

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ClosureBindingPathStep {
    Parameter(u32),
    Statement(u32),
    Pattern(u32),
    LoopVariable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ClosureCaptureTransfer {
    CopyScalar,
    CopyCapability,
    ManagedReferenceByValue,
    /// An already-authenticated immutable root-plus-projection carrier.
    ///
    /// This is deliberately not the generic managed-reference case: lowering
    /// may retain the carrier, but must never recover a root or projection
    /// from its physical layout and synthesize a new view.
    RootedViewByValue,
    ExplicitOwnedMove,
}
