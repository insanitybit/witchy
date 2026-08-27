use witchy_worker_protocol::{
    AdmissionSchemaId, ProtocolError, ResponseStatus, WorkerLimits, WorkerRequest, WorkerResponse,
    WorkerTarget, blob_sha256, decode_request, decode_response,
};

const SOURCE: &[u8] = b"WASRC004\x02\0canonical source fixture";
const ADMISSION_DIGEST: [u8; 32] = [0xa5; 32];

fn limits() -> WorkerLimits {
    WorkerLimits {
        cpu_time_ms: 30_000,
        wall_time_ms: 60_000,
        memory_bytes: 512 * 1024 * 1024,
        max_source_bytes: 1024 * 1024,
        max_response_bytes: 4 * 1024 * 1024,
        max_source_files: 128,
        max_recursion_depth: 256,
        max_parser_nodes: 1_000_000,
        max_type_expansions: 100_000,
        max_specializations: 50_000,
        max_diagnostic_bytes: 64 * 1024,
        max_candidate_wir_bytes: 2 * 1024 * 1024,
        max_authority_wir_bytes: 1024 * 1024,
        max_final_byte_plan_bytes: 1024 * 1024,
    }
}

fn request() -> WorkerRequest {
    WorkerRequest::new(
        SOURCE.to_vec(),
        WorkerTarget::NativeRuntime,
        AdmissionSchemaId::v2(ADMISSION_DIGEST).unwrap(),
        limits(),
    )
    .unwrap()
}

fn candidate(request: &WorkerRequest) -> WorkerResponse {
    WorkerResponse::candidate(
        request,
        b"candidate-wir-v2".to_vec(),
        b"authority-wir-v2".to_vec(),
        b"final-byte-plan-v2".to_vec(),
        "warning: inert diagnostic\n".into(),
    )
    .unwrap()
}

fn hex(bytes: [u8; 32]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[test]
fn stable_schema_v2_fixtures_and_round_trips() {
    let request = request();
    let request_bytes = request.encode().unwrap();
    assert_eq!(&request_bytes[..8], b"WCHREQ02");
    assert_eq!(
        u16::from_le_bytes(request_bytes[8..10].try_into().unwrap()),
        2
    );
    assert_eq!(request.source_bundle(), SOURCE);
    assert_eq!(request.source_bundle_sha256(), blob_sha256(SOURCE));
    assert_eq!(decode_request(&request_bytes).unwrap(), request);
    assert_eq!(
        hex(blob_sha256(&request_bytes)),
        "c6b1aed0fbae14a0ee352bcdd7552f142e5a2d54b12751404add47e6a437347c"
    );

    let response = candidate(&request);
    let response_bytes = response.encode().unwrap();
    assert_eq!(&response_bytes[..8], b"WCHRSP02");
    assert_eq!(
        decode_response(&response_bytes, &request).unwrap(),
        response
    );
    assert_eq!(
        hex(blob_sha256(&response_bytes)),
        "07db343f97195947e649b968e45471a8173a89f6489dd9f38f582507e310f4c2"
    );
    assert_eq!(response.status(), ResponseStatus::Candidate);
    assert_eq!(response.candidate_wir(), b"candidate-wir-v2");
    assert_eq!(response.authority_wir(), b"authority-wir-v2");
    assert_eq!(response.final_byte_plan(), b"final-byte-plan-v2");
}

#[test]
fn every_truncation_and_trailing_byte_rejects() {
    let request = request();
    let request_bytes = request.encode().unwrap();
    for end in 0..request_bytes.len() {
        assert!(
            decode_request(&request_bytes[..end]).is_err(),
            "request prefix {end}"
        );
    }
    let mut trailing = request_bytes.clone();
    trailing.push(0);
    assert_eq!(decode_request(&trailing), Err(ProtocolError::TrailingInput));

    let mut unknown_request_field = request_bytes;
    unknown_request_field.push(0);
    let payload_length = u32::from_le_bytes(unknown_request_field[12..16].try_into().unwrap()) + 1;
    unknown_request_field[12..16].copy_from_slice(&payload_length.to_le_bytes());
    assert_eq!(
        decode_request(&unknown_request_field),
        Err(ProtocolError::TrailingInput)
    );

    let response_bytes = candidate(&request).encode().unwrap();
    for end in 0..response_bytes.len() {
        assert!(
            decode_response(&response_bytes[..end], &request).is_err(),
            "response prefix {end}"
        );
    }
    let mut trailing = response_bytes.clone();
    trailing.push(0);
    assert_eq!(
        decode_response(&trailing, &request),
        Err(ProtocolError::TrailingInput)
    );

    let mut unknown_response_field = response_bytes;
    unknown_response_field.push(0);
    let payload_length = u32::from_le_bytes(unknown_response_field[12..16].try_into().unwrap()) + 1;
    unknown_response_field[12..16].copy_from_slice(&payload_length.to_le_bytes());
    assert_eq!(
        decode_response(&unknown_response_field, &request),
        Err(ProtocolError::TrailingInput)
    );
}

#[test]
fn request_header_schema_reserved_target_digest_and_length_mutations_reject() {
    let canonical = request().encode().unwrap();
    let mutations: &[(usize, u8, ProtocolError)] = &[
        (0, b'X', ProtocolError::InvalidMagic),
        (8, 1, ProtocolError::UnsupportedProtocolSchema(1)),
        (10, 99, ProtocolError::UnknownMessageKind(99)),
        (11, 1, ProtocolError::NonZeroReserved),
        (16, 1, ProtocolError::UnsupportedAdmissionSchema(1)),
        (18, 1, ProtocolError::NonZeroReserved),
        (20, 0, ProtocolError::DigestMismatch("admission schema")),
        (52, 99, ProtocolError::UnknownTarget(99)),
        (54, 1, ProtocolError::NonZeroReserved),
        (56, 1, ProtocolError::DigestMismatch("source bundle")),
        (88, 0, ProtocolError::InvalidLimit("cpu time")),
    ];
    for (offset, replacement, expected) in mutations {
        let mut bytes = canonical.clone();
        match *offset {
            20 => bytes[20..52].fill(0),
            88 => bytes[88..96].fill(0),
            _ => bytes[*offset] = *replacement,
        }
        assert_eq!(
            decode_request(&bytes),
            Err(expected.clone()),
            "offset {offset}"
        );
    }

    let mut too_long = canonical.clone();
    too_long[176..180].copy_from_slice(&u32::MAX.to_le_bytes());
    assert_eq!(
        decode_request(&too_long),
        Err(ProtocolError::BudgetExceeded("source bundle bytes"))
    );

    let mut source_limit_too_small = canonical.clone();
    source_limit_too_small[112..120].copy_from_slice(&1_u64.to_le_bytes());
    assert_eq!(
        decode_request(&source_limit_too_small),
        Err(ProtocolError::BudgetExceeded("source bundle bytes"))
    );

    let mut short_payload = canonical.clone();
    let payload = u32::from_le_bytes(short_payload[12..16].try_into().unwrap());
    short_payload[12..16].copy_from_slice(&(payload - 1).to_le_bytes());
    assert_eq!(
        decode_request(&short_payload),
        Err(ProtocolError::TrailingInput)
    );
}

#[test]
fn all_targets_are_closed_and_unknown_targets_reject() {
    for target in [
        WorkerTarget::NativeRuntime,
        WorkerTarget::BrowserBase,
        WorkerTarget::BrowserCapabilityProvider,
        WorkerTarget::NativeBuildWorker,
    ] {
        let request = WorkerRequest::new(
            SOURCE.to_vec(),
            target,
            AdmissionSchemaId::v2(ADMISSION_DIGEST).unwrap(),
            limits(),
        )
        .unwrap();
        assert_eq!(
            decode_request(&request.encode().unwrap()).unwrap().target(),
            target
        );
        assert!(!target.name().is_empty());
    }

    let mut non_shippable = request().encode().unwrap();
    non_shippable[52..54].copy_from_slice(&0x7fff_u16.to_le_bytes());
    assert_eq!(
        decode_request(&non_shippable),
        Err(ProtocolError::UnknownTarget(0x7fff))
    );
}

#[test]
fn response_is_request_bound_bounded_and_shape_checked() {
    let request = request();
    let canonical = candidate(&request).encode().unwrap();

    let other = WorkerRequest::new(
        b"different source".to_vec(),
        WorkerTarget::NativeRuntime,
        AdmissionSchemaId::v2(ADMISSION_DIGEST).unwrap(),
        limits(),
    )
    .unwrap();
    assert_eq!(
        decode_response(&canonical, &other),
        Err(ProtocolError::RequestMismatch)
    );

    let mutations: &[(usize, u8, ProtocolError)] = &[
        (0, b'X', ProtocolError::InvalidMagic),
        (8, 1, ProtocolError::UnsupportedProtocolSchema(1)),
        (10, 99, ProtocolError::UnknownMessageKind(99)),
        (11, 1, ProtocolError::NonZeroReserved),
        (16, 0, ProtocolError::RequestMismatch),
        (48, 99, ProtocolError::UnknownResponseStatus(99)),
        (49, 1, ProtocolError::NonZeroReserved),
        (48, 2, ProtocolError::InvalidResponseShape),
    ];
    for (offset, replacement, expected) in mutations {
        let mut bytes = canonical.clone();
        bytes[*offset] = *replacement;
        assert_eq!(
            decode_response(&bytes, &request),
            Err(expected.clone()),
            "offset {offset}"
        );
    }

    let mut oversized = canonical;
    oversized[52..56].copy_from_slice(&u32::MAX.to_le_bytes());
    assert_eq!(
        decode_response(&oversized, &request),
        Err(ProtocolError::BudgetExceeded("candidate WIR bytes"))
    );
}

#[test]
fn diagnostics_are_utf8_bounded_inert_text() {
    let request = request();
    for forbidden in ["escape \u{1b}[31m", "carriage\rreturn", "bidi \u{202e}txt"] {
        assert_eq!(
            WorkerResponse::rejected(&request, forbidden.into()),
            Err(ProtocolError::InvalidDiagnostic)
        );
    }

    let rejected = WorkerResponse::rejected(&request, "ordinary failure".into()).unwrap();
    let mut invalid_utf8 = rejected.encode().unwrap();
    *invalid_utf8.last_mut().unwrap() = 0xff;
    assert_eq!(
        decode_response(&invalid_utf8, &request),
        Err(ProtocolError::InvalidUtf8)
    );

    let mut tiny = limits();
    tiny.max_response_bytes = 64;
    tiny.max_diagnostic_bytes = 32;
    tiny.max_candidate_wir_bytes = 32;
    tiny.max_authority_wir_bytes = 32;
    tiny.max_final_byte_plan_bytes = 32;
    let tiny_request = WorkerRequest::new(
        SOURCE.to_vec(),
        WorkerTarget::NativeRuntime,
        AdmissionSchemaId::v2(ADMISSION_DIGEST).unwrap(),
        tiny,
    )
    .unwrap();
    assert_eq!(
        WorkerResponse::rejected(&tiny_request, "does not fit the complete frame".into()),
        Err(ProtocolError::BudgetExceeded("response frame bytes"))
    );
}

#[test]
fn constructors_reject_empty_and_over_budget_values() {
    assert_eq!(
        AdmissionSchemaId::v2([0; 32]),
        Err(ProtocolError::DigestMismatch("admission schema"))
    );
    assert_eq!(
        WorkerRequest::new(
            Vec::new(),
            WorkerTarget::NativeRuntime,
            AdmissionSchemaId::v2(ADMISSION_DIGEST).unwrap(),
            limits(),
        ),
        Err(ProtocolError::EmptyField("source bundle"))
    );

    let request = request();
    assert_eq!(
        WorkerResponse::candidate(&request, Vec::new(), vec![1], vec![1], String::new(),),
        Err(ProtocolError::EmptyField("candidate WIR"))
    );
    assert_eq!(
        WorkerResponse::rejected(&request, String::new()),
        Err(ProtocolError::EmptyField("rejection diagnostics"))
    );
}
