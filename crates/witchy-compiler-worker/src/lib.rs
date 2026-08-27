//! Linux process boundary for RFC-0163 adversarial-source compilation.
//!
//! This crate owns transport, required zero-authority confinement, child
//! resource ceilings, timeout/reaping, and the parent-side rule that candidate
//! bytes cross an independent admission callback before they can be returned.
//! The source-to-candidate producer is deliberately unavailable until the
//! canonical WIR, AuthorityWIR, and final-byte-plan serializers land.  A real
//! worker process therefore rejects safely rather than inventing an admission
//! representation.

#![deny(unsafe_code)]

mod error;
mod launcher;
mod limits;
mod service;
mod state;

pub use error::WorkerBoundaryError;
pub use launcher::{CandidateAdmission, LinuxNativeWorker, UntrustedCandidate, WorkerOutcome};
#[cfg(feature = "test-fixtures")]
#[doc(hidden)]
pub use service::arm_test_fixture;
pub use service::serve_stdio;
pub use state::{IMPLEMENTATION_STATE, ProducerState, PublicationState, WorkerImplementationState};
