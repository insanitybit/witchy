use std::io::{Read, Write};

use witchy_worker_protocol::{
    REQUEST_PRELUDE_BYTES, RequestPrelude, WorkerRequest, WorkerResponse, WorkerTarget,
    decode_request, decode_request_prelude,
};

use crate::WorkerBoundaryError;
use crate::state::PRODUCER_UNAVAILABLE_DIAGNOSTIC;

pub fn serve_stdio() -> Result<(), WorkerBoundaryError> {
    let mut input = std::io::stdin().lock();
    let mut output = std::io::stdout().lock();
    serve_once(&mut input, &mut output, arm_required_worker)
}

fn serve_once(
    input: &mut impl Read,
    output: &mut impl Write,
    arm: impl FnOnce(RequestPrelude) -> Result<(), WorkerBoundaryError>,
) -> Result<(), WorkerBoundaryError> {
    let request = read_confined_request(input, arm)?;

    // There is intentionally no fall-through candidate path.  Adding one
    // requires changing IMPLEMENTATION_STATE and supplying canonical serializers
    // for all three blobs together with a real parent admission implementation.
    let response = WorkerResponse::rejected(&request, PRODUCER_UNAVAILABLE_DIAGNOSTIC.to_string())?;
    output.write_all(&response.encode()?)?;
    output.flush()?;
    Ok(())
}

fn arm_required_worker(prelude: RequestPrelude) -> Result<(), WorkerBoundaryError> {
    crate::limits::apply(prelude.limits())?;
    let report = witchy_confinement::apply(
        &witchy_confinement::Policy::default(),
        witchy_confinement::EnforcementMode::Required,
    )
    .map_err(|error| WorkerBoundaryError::Confinement(error.to_string()))?;
    if !report.fully_enforced() {
        return Err(WorkerBoundaryError::Confinement(format!(
            "provider returned a non-enforcing report: {:?}",
            report.layers
        )));
    }
    Ok(())
}

#[cfg(feature = "test-fixtures")]
#[doc(hidden)]
pub fn arm_test_fixture(request: &WorkerRequest) -> Result<(), WorkerBoundaryError> {
    if request.target() != WorkerTarget::NativeRuntime {
        return Err(WorkerBoundaryError::UnsupportedTarget);
    }
    arm_required_worker(decode_request_prelude(
        &request.encode()?[..REQUEST_PRELUDE_BYTES],
    )?)
}

fn read_confined_request(
    input: &mut impl Read,
    arm: impl FnOnce(RequestPrelude) -> Result<(), WorkerBoundaryError>,
) -> Result<WorkerRequest, WorkerBoundaryError> {
    let mut frame = vec![0_u8; REQUEST_PRELUDE_BYTES];
    input.read_exact(&mut frame)?;
    let prelude = decode_request_prelude(&frame)?;
    if prelude.target() != WorkerTarget::NativeRuntime {
        return Err(WorkerBoundaryError::UnsupportedTarget);
    }
    arm(prelude)?;
    frame.resize(prelude.frame_bytes(), 0);
    input.read_exact(&mut frame[REQUEST_PRELUDE_BYTES..])?;
    let mut trailing = [0_u8; 1];
    if input.read(&mut trailing)? != 0 {
        return Err(WorkerBoundaryError::Protocol(
            witchy_worker_protocol::ProtocolError::TrailingInput,
        ));
    }
    Ok(decode_request(&frame)?)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::io::Cursor;

    use witchy_worker_protocol::{
        AdmissionSchemaId, ProtocolError, WorkerLimits, WorkerRequest, WorkerTarget,
        decode_response,
    };

    use super::*;

    fn limits() -> WorkerLimits {
        WorkerLimits {
            cpu_time_ms: 30_000,
            wall_time_ms: 60_000,
            memory_bytes: 512 * 1024 * 1024,
            max_source_bytes: 1024,
            max_response_bytes: 4096,
            max_source_files: 8,
            max_recursion_depth: 64,
            max_parser_nodes: 10_000,
            max_type_expansions: 10_000,
            max_specializations: 1_000,
            max_diagnostic_bytes: 1024,
            max_candidate_wir_bytes: 1024,
            max_authority_wir_bytes: 1024,
            max_final_byte_plan_bytes: 1024,
        }
    }

    fn request(target: WorkerTarget) -> WorkerRequest {
        WorkerRequest::new(
            b"canonical-bundle".to_vec(),
            target,
            AdmissionSchemaId::v2([7; 32]).unwrap(),
            limits(),
        )
        .unwrap()
    }

    #[test]
    fn producer_absence_is_a_canonical_rejection_after_arming() {
        let request = request(WorkerTarget::NativeRuntime);
        let mut input = Cursor::new(request.encode().unwrap());
        let mut output = Vec::new();
        let mut armed = false;
        serve_once(&mut input, &mut output, |_| {
            armed = true;
            Ok(())
        })
        .unwrap();
        assert!(armed);
        let response = decode_response(&output, &request).unwrap();
        assert_eq!(
            response.status(),
            witchy_worker_protocol::ResponseStatus::Rejected
        );
        assert_eq!(response.diagnostics(), PRODUCER_UNAVAILABLE_DIAGNOSTIC);
    }

    #[test]
    fn malformed_prefix_and_unsupported_targets_never_arm_but_trailing_is_post_arm() {
        let mut malformed = request(WorkerTarget::NativeRuntime).encode().unwrap();
        malformed[0] = b'X';
        let mut armed = false;
        let error = serve_once(&mut Cursor::new(malformed), &mut Vec::new(), |_| {
            armed = true;
            Ok(())
        })
        .unwrap_err();
        assert!(matches!(
            error,
            WorkerBoundaryError::Protocol(ProtocolError::InvalidMagic)
        ));
        assert!(!armed);

        let native_request = request(WorkerTarget::NativeRuntime);
        let mut trailing = native_request.encode().unwrap();
        trailing.push(0);
        let error = serve_once(&mut Cursor::new(trailing), &mut Vec::new(), |_| {
            armed = true;
            Ok(())
        })
        .unwrap_err();
        assert!(matches!(
            error,
            WorkerBoundaryError::Protocol(ProtocolError::TrailingInput)
        ));
        assert!(armed);

        let request = request(WorkerTarget::BrowserBase);
        armed = false;
        let error = serve_once(
            &mut Cursor::new(request.encode().unwrap()),
            &mut Vec::new(),
            |_| {
                armed = true;
                Ok(())
            },
        )
        .unwrap_err();
        assert!(matches!(error, WorkerBoundaryError::UnsupportedTarget));
        assert!(!armed);
    }

    #[test]
    fn oversized_declared_input_is_rejected_before_allocation() {
        let request = request(WorkerTarget::NativeRuntime);
        let mut bytes = request.encode().unwrap();
        bytes[176..180].copy_from_slice(&u32::MAX.to_le_bytes());
        let armed = Cell::new(false);
        assert!(matches!(
            read_confined_request(&mut Cursor::new(bytes), |_| {
                armed.set(true);
                Ok(())
            }),
            Err(WorkerBoundaryError::Protocol(
                ProtocolError::BudgetExceeded("source bundle bytes")
            ))
        ));
        assert!(!armed.get());
    }

    struct GateReader<'a> {
        bytes: Cursor<Vec<u8>>,
        armed: &'a Cell<bool>,
    }

    impl Read for GateReader<'_> {
        fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
            if self.bytes.position() as usize >= REQUEST_PRELUDE_BYTES && !self.armed.get() {
                panic!("source bytes were read before confinement");
            }
            self.bytes.read(output)
        }
    }

    #[test]
    fn limits_and_confinement_are_applied_before_source_bytes_are_read() {
        let request = request(WorkerTarget::NativeRuntime);
        let armed = Cell::new(false);
        let mut input = GateReader {
            bytes: Cursor::new(request.encode().unwrap()),
            armed: &armed,
        };
        let decoded = read_confined_request(&mut input, |_| {
            armed.set(true);
            Ok(())
        })
        .unwrap();
        assert!(armed.get());
        assert_eq!(decoded, request);
    }
}
