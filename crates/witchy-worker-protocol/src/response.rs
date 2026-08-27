use crate::ProtocolError;
use crate::io::{Input, frame, payload, put_bytes, put_u8};
use crate::limits::MAX_RESPONSE_BYTES;
use crate::request::{WORKER_PROTOCOL_SCHEMA_V2, WorkerRequest, sha256};

const RESPONSE_MAGIC: &[u8; 8] = b"WCHRSP02";
const RESPONSE_KIND: u8 = 2;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ResponseStatus {
    Candidate = 1,
    Rejected = 2,
}

impl ResponseStatus {
    fn decode(value: u8) -> Result<Self, ProtocolError> {
        match value {
            1 => Ok(Self::Candidate),
            2 => Ok(Self::Rejected),
            _ => Err(ProtocolError::UnknownResponseStatus(value)),
        }
    }
}

/// Untrusted worker output bound to one exact canonical request. The three
/// candidate blobs remain opaque; independent parent-side admission must parse
/// them before any artifact is published.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkerResponse {
    request_sha256: [u8; 32],
    limits: crate::WorkerLimits,
    status: ResponseStatus,
    candidate_wir: Vec<u8>,
    authority_wir: Vec<u8>,
    final_byte_plan: Vec<u8>,
    diagnostics: String,
}

impl WorkerResponse {
    pub fn candidate(
        request: &WorkerRequest,
        candidate_wir: Vec<u8>,
        authority_wir: Vec<u8>,
        final_byte_plan: Vec<u8>,
        diagnostics: String,
    ) -> Result<Self, ProtocolError> {
        if candidate_wir.is_empty() {
            return Err(ProtocolError::EmptyField("candidate WIR"));
        }
        if authority_wir.is_empty() {
            return Err(ProtocolError::EmptyField("AuthorityWIR"));
        }
        if final_byte_plan.is_empty() {
            return Err(ProtocolError::EmptyField("final-byte plan"));
        }
        Self::construct(
            request,
            ResponseStatus::Candidate,
            candidate_wir,
            authority_wir,
            final_byte_plan,
            diagnostics,
        )
    }

    pub fn rejected(request: &WorkerRequest, diagnostics: String) -> Result<Self, ProtocolError> {
        if diagnostics.is_empty() {
            return Err(ProtocolError::EmptyField("rejection diagnostics"));
        }
        Self::construct(
            request,
            ResponseStatus::Rejected,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            diagnostics,
        )
    }

    fn construct(
        request: &WorkerRequest,
        status: ResponseStatus,
        candidate_wir: Vec<u8>,
        authority_wir: Vec<u8>,
        final_byte_plan: Vec<u8>,
        diagnostics: String,
    ) -> Result<Self, ProtocolError> {
        let limits = request.limits();
        check_length(
            candidate_wir.len(),
            limits.max_candidate_wir_bytes,
            "candidate WIR bytes",
        )?;
        check_length(
            authority_wir.len(),
            limits.max_authority_wir_bytes,
            "AuthorityWIR bytes",
        )?;
        check_length(
            final_byte_plan.len(),
            limits.max_final_byte_plan_bytes,
            "final-byte plan bytes",
        )?;
        validate_diagnostics(&diagnostics, limits.max_diagnostic_bytes)?;
        let response = Self {
            request_sha256: request.sha256()?,
            limits,
            status,
            candidate_wir,
            authority_wir,
            final_byte_plan,
            diagnostics,
        };
        response.encode()?;
        Ok(response)
    }

    pub const fn status(&self) -> ResponseStatus {
        self.status
    }

    pub const fn request_sha256(&self) -> [u8; 32] {
        self.request_sha256
    }

    pub fn candidate_wir(&self) -> &[u8] {
        &self.candidate_wir
    }

    pub fn authority_wir(&self) -> &[u8] {
        &self.authority_wir
    }

    pub fn final_byte_plan(&self) -> &[u8] {
        &self.final_byte_plan
    }

    pub fn diagnostics(&self) -> &str {
        &self.diagnostics
    }

    pub fn encode(&self) -> Result<Vec<u8>, ProtocolError> {
        let mut body = Vec::with_capacity(
            40 + self.candidate_wir.len()
                + self.authority_wir.len()
                + self.final_byte_plan.len()
                + self.diagnostics.len(),
        );
        body.extend_from_slice(&self.request_sha256);
        put_u8(&mut body, self.status as u8);
        body.extend_from_slice(&[0; 3]);
        put_bytes(&mut body, &self.candidate_wir, "candidate WIR")?;
        put_bytes(&mut body, &self.authority_wir, "AuthorityWIR")?;
        put_bytes(&mut body, &self.final_byte_plan, "final-byte plan")?;
        put_bytes(&mut body, self.diagnostics.as_bytes(), "diagnostics")?;
        let encoded = frame(
            RESPONSE_MAGIC,
            WORKER_PROTOCOL_SCHEMA_V2,
            RESPONSE_KIND,
            &body,
        )?;
        let maximum = usize::try_from(self.limits.max_response_bytes)
            .map_err(|_| ProtocolError::BudgetExceeded("response frame bytes"))?;
        if encoded.len() > maximum || encoded.len() > MAX_RESPONSE_BYTES as usize {
            return Err(ProtocolError::BudgetExceeded("response frame bytes"));
        }
        Ok(encoded)
    }
}

pub fn decode_response(
    bytes: &[u8],
    expected_request: &WorkerRequest,
) -> Result<WorkerResponse, ProtocolError> {
    let maximum = usize::try_from(expected_request.limits().max_response_bytes)
        .map_err(|_| ProtocolError::BudgetExceeded("response frame bytes"))?
        .min(MAX_RESPONSE_BYTES as usize);
    let body = payload(
        bytes,
        maximum,
        RESPONSE_MAGIC,
        WORKER_PROTOCOL_SCHEMA_V2,
        RESPONSE_KIND,
    )?;
    let mut input = Input::new(body);
    let request_sha256 = input.array_32()?;
    if request_sha256 != expected_request.sha256()? {
        return Err(ProtocolError::RequestMismatch);
    }
    let status = ResponseStatus::decode(input.u8()?)?;
    if input.take(3)? != [0; 3] {
        return Err(ProtocolError::NonZeroReserved);
    }
    let limits = expected_request.limits();
    let candidate_wir = input.bytes(
        limits.max_candidate_wir_bytes as usize,
        "candidate WIR bytes",
    )?;
    let authority_wir = input.bytes(
        limits.max_authority_wir_bytes as usize,
        "AuthorityWIR bytes",
    )?;
    let final_byte_plan = input.bytes(
        limits.max_final_byte_plan_bytes as usize,
        "final-byte plan bytes",
    )?;
    let diagnostic_bytes = input.bytes(limits.max_diagnostic_bytes as usize, "diagnostic bytes")?;
    input.finish()?;
    let diagnostics =
        String::from_utf8(diagnostic_bytes).map_err(|_| ProtocolError::InvalidUtf8)?;
    let response = match status {
        ResponseStatus::Candidate => WorkerResponse::candidate(
            expected_request,
            candidate_wir,
            authority_wir,
            final_byte_plan,
            diagnostics,
        )?,
        ResponseStatus::Rejected => {
            if !candidate_wir.is_empty() || !authority_wir.is_empty() || !final_byte_plan.is_empty()
            {
                return Err(ProtocolError::InvalidResponseShape);
            }
            WorkerResponse::rejected(expected_request, diagnostics)?
        }
    };
    if response.request_sha256 != request_sha256 {
        return Err(ProtocolError::RequestMismatch);
    }
    if response.encode()?.as_slice() != bytes {
        return Err(ProtocolError::NonCanonical);
    }
    Ok(response)
}

fn check_length(length: usize, maximum: u32, field: &'static str) -> Result<(), ProtocolError> {
    if length > maximum as usize {
        Err(ProtocolError::BudgetExceeded(field))
    } else {
        Ok(())
    }
}

fn validate_diagnostics(value: &str, maximum: u32) -> Result<(), ProtocolError> {
    check_length(value.len(), maximum, "diagnostic bytes")?;
    if value.chars().any(forbidden_diagnostic_character) {
        Err(ProtocolError::InvalidDiagnostic)
    } else {
        Ok(())
    }
}

fn forbidden_diagnostic_character(character: char) -> bool {
    (character.is_control() && !matches!(character, '\n' | '\t'))
        || matches!(
            character,
            '\u{061c}'
                | '\u{200e}'
                | '\u{200f}'
                | '\u{202a}'..='\u{202e}'
                | '\u{2066}'..='\u{2069}'
        )
}

pub fn blob_sha256(bytes: &[u8]) -> [u8; 32] {
    sha256(bytes)
}
