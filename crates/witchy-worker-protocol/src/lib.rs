//! Canonical bounded wire format for an RFC-0163 adversarial-source compiler
//! worker.
//!
//! The protocol deliberately transports only an opaque exact source bundle,
//! target, admission identity, resource ceilings, opaque candidate blobs, and
//! inert diagnostics. It has no fields for filesystem paths, environment,
//! network, process/tool commands, capabilities, or grant objects. This crate
//! does not launch or confine a process and therefore does not close RFC-0163
//! T15 by itself.

#![deny(unsafe_code)]

mod error;
mod io;
mod limits;
mod request;
mod response;

pub use error::ProtocolError;
pub use limits::{
    MAX_AUTHORITY_WIR_BYTES, MAX_CANDIDATE_WIR_BYTES, MAX_DIAGNOSTIC_BYTES,
    MAX_FINAL_BYTE_PLAN_BYTES, MAX_RESPONSE_BYTES, MAX_SOURCE_BUNDLE_BYTES, WorkerLimits,
};
pub use request::{
    ADMISSION_SCHEMA_V2, AdmissionSchemaId, WORKER_PROTOCOL_SCHEMA_V2, WorkerRequest, WorkerTarget,
    decode_request,
};
pub use response::{ResponseStatus, WorkerResponse, blob_sha256, decode_response};
