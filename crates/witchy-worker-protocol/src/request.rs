use sha2::{Digest, Sha256};

use crate::io::{Input, frame, payload, put_bytes, put_u16};
use crate::limits::MAX_SOURCE_BUNDLE_BYTES;
use crate::{ProtocolError, WorkerLimits};

pub const WORKER_PROTOCOL_SCHEMA_V2: u16 = 2;
pub const ADMISSION_SCHEMA_V2: u16 = 2;

pub(crate) const REQUEST_MAGIC: &[u8; 8] = b"WCHREQ02";
pub(crate) const REQUEST_KIND: u8 = 1;
const MAX_REQUEST_FRAME_BYTES: usize = MAX_SOURCE_BUNDLE_BYTES as usize + 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AdmissionSchemaId {
    sha256: [u8; 32],
}

impl AdmissionSchemaId {
    /// Construct the current schema-2 admission identity. All-zero is reserved
    /// so an absent/uninitialized schema can never become an admitted request.
    pub fn v2(sha256: [u8; 32]) -> Result<Self, ProtocolError> {
        if sha256 == [0; 32] {
            return Err(ProtocolError::DigestMismatch("admission schema"));
        }
        Ok(Self { sha256 })
    }

    pub const fn version(self) -> u16 {
        ADMISSION_SCHEMA_V2
    }

    pub const fn sha256(self) -> [u8; 32] {
        self.sha256
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u16)]
pub enum WorkerTarget {
    NativeRuntime = 0x0001,
    BrowserBase = 0x0002,
    BrowserCapabilityProvider = 0x0003,
    NativeBuildWorker = 0x0004,
}

impl WorkerTarget {
    pub const fn name(self) -> &'static str {
        match self {
            Self::NativeRuntime => "native-runtime",
            Self::BrowserBase => "browser-base",
            Self::BrowserCapabilityProvider => "browser-capability-provider",
            Self::NativeBuildWorker => "native-build-worker",
        }
    }

    fn decode(value: u16) -> Result<Self, ProtocolError> {
        match value {
            0x0001 => Ok(Self::NativeRuntime),
            0x0002 => Ok(Self::BrowserBase),
            0x0003 => Ok(Self::BrowserCapabilityProvider),
            0x0004 => Ok(Self::NativeBuildWorker),
            _ => Err(ProtocolError::UnknownTarget(value)),
        }
    }
}

/// A validated worker request. Private fields and the absence of serde support
/// prevent a decoded summary from restoring a checked request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkerRequest {
    admission_schema: AdmissionSchemaId,
    target: WorkerTarget,
    limits: WorkerLimits,
    source_bundle_sha256: [u8; 32],
    source_bundle: Vec<u8>,
}

impl WorkerRequest {
    pub fn new(
        source_bundle: Vec<u8>,
        target: WorkerTarget,
        admission_schema: AdmissionSchemaId,
        limits: WorkerLimits,
    ) -> Result<Self, ProtocolError> {
        let limits = limits.validate()?;
        if source_bundle.is_empty() {
            return Err(ProtocolError::EmptyField("source bundle"));
        }
        if source_bundle.len() as u64 > limits.max_source_bytes
            || source_bundle.len() as u64 > MAX_SOURCE_BUNDLE_BYTES
        {
            return Err(ProtocolError::BudgetExceeded("source bundle bytes"));
        }
        let source_bundle_sha256 = sha256(&source_bundle);
        Ok(Self {
            admission_schema,
            target,
            limits,
            source_bundle_sha256,
            source_bundle,
        })
    }

    pub const fn admission_schema(&self) -> AdmissionSchemaId {
        self.admission_schema
    }

    pub const fn target(&self) -> WorkerTarget {
        self.target
    }

    pub const fn limits(&self) -> WorkerLimits {
        self.limits
    }

    pub fn source_bundle(&self) -> &[u8] {
        &self.source_bundle
    }

    pub const fn source_bundle_sha256(&self) -> [u8; 32] {
        self.source_bundle_sha256
    }

    pub fn encode(&self) -> Result<Vec<u8>, ProtocolError> {
        let mut body = Vec::with_capacity(self.source_bundle.len() + 160);
        put_u16(&mut body, ADMISSION_SCHEMA_V2);
        put_u16(&mut body, 0);
        body.extend_from_slice(&self.admission_schema.sha256);
        put_u16(&mut body, self.target as u16);
        put_u16(&mut body, 0);
        body.extend_from_slice(&self.source_bundle_sha256);
        self.limits.encode(&mut body);
        put_bytes(&mut body, &self.source_bundle, "source bundle")?;
        let encoded = frame(
            REQUEST_MAGIC,
            WORKER_PROTOCOL_SCHEMA_V2,
            REQUEST_KIND,
            &body,
        )?;
        if encoded.len() > MAX_REQUEST_FRAME_BYTES {
            return Err(ProtocolError::BudgetExceeded("request frame bytes"));
        }
        Ok(encoded)
    }

    pub fn sha256(&self) -> Result<[u8; 32], ProtocolError> {
        Ok(sha256(&self.encode()?))
    }
}

pub fn decode_request(bytes: &[u8]) -> Result<WorkerRequest, ProtocolError> {
    let body = payload(
        bytes,
        MAX_REQUEST_FRAME_BYTES,
        REQUEST_MAGIC,
        WORKER_PROTOCOL_SCHEMA_V2,
        REQUEST_KIND,
    )?;
    let mut input = Input::new(body);
    let admission_version = input.u16()?;
    if admission_version != ADMISSION_SCHEMA_V2 {
        return Err(ProtocolError::UnsupportedAdmissionSchema(admission_version));
    }
    if input.u16()? != 0 {
        return Err(ProtocolError::NonZeroReserved);
    }
    let admission_schema = AdmissionSchemaId::v2(input.array_32()?)?;
    let target = WorkerTarget::decode(input.u16()?)?;
    if input.u16()? != 0 {
        return Err(ProtocolError::NonZeroReserved);
    }
    let expected_source_digest = input.array_32()?;
    let limits = WorkerLimits::decode(&mut input)?;
    let source_limit = usize::try_from(limits.max_source_bytes)
        .map_err(|_| ProtocolError::BudgetExceeded("source bundle bytes"))?;
    let source_bundle = input.bytes(source_limit, "source bundle bytes")?;
    input.finish()?;
    if sha256(&source_bundle) != expected_source_digest {
        return Err(ProtocolError::DigestMismatch("source bundle"));
    }
    let request = WorkerRequest::new(source_bundle, target, admission_schema, limits)?;
    if request.source_bundle_sha256 != expected_source_digest {
        return Err(ProtocolError::DigestMismatch("source bundle"));
    }
    if request.encode()?.as_slice() != bytes {
        return Err(ProtocolError::NonCanonical);
    }
    Ok(request)
}

pub(crate) fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}
