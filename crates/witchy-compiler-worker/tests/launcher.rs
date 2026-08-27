#![cfg(all(
    target_os = "linux",
    any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        target_arch = "riscv64"
    )
))]

use std::cell::Cell;
use std::time::{Duration, Instant};

use witchy_compiler_worker::{
    CandidateAdmission, LinuxNativeWorker, UntrustedCandidate, WorkerBoundaryError, WorkerOutcome,
};
use witchy_worker_protocol::{AdmissionSchemaId, WorkerLimits, WorkerRequest, WorkerTarget};

struct Admission {
    called: Cell<bool>,
    accept: bool,
}

impl CandidateAdmission for Admission {
    type Admitted = [usize; 3];

    fn admit(
        &self,
        _request: &WorkerRequest,
        candidate: UntrustedCandidate<'_>,
    ) -> Result<Self::Admitted, String> {
        self.called.set(true);
        if !self.accept {
            return Err("independent checker rejected candidate".into());
        }
        Ok([
            candidate.candidate_wir().len(),
            candidate.authority_wir().len(),
            candidate.final_byte_plan().len(),
        ])
    }
}

fn request(wall_time_ms: u64, max_response_bytes: u64) -> WorkerRequest {
    let field_limit = u32::try_from(max_response_bytes.min(1024 * 1024)).unwrap();
    WorkerRequest::new(
        b"bounded canonical source bundle fixture".to_vec(),
        WorkerTarget::NativeRuntime,
        AdmissionSchemaId::v2([0x16; 32]).unwrap(),
        WorkerLimits {
            cpu_time_ms: 30_000,
            wall_time_ms,
            memory_bytes: 2 * 1024 * 1024 * 1024,
            max_source_bytes: 1024,
            max_response_bytes,
            max_source_files: 8,
            max_recursion_depth: 64,
            max_parser_nodes: 10_000,
            max_type_expansions: 10_000,
            max_specializations: 1_000,
            max_diagnostic_bytes: field_limit,
            max_candidate_wir_bytes: field_limit,
            max_authority_wir_bytes: field_limit,
            max_final_byte_plan_bytes: field_limit,
        },
    )
    .unwrap()
}

#[test]
fn actual_required_confined_worker_reports_producer_unavailable() {
    let worker = LinuxNativeWorker::new(env!("CARGO_BIN_EXE_witchy-compiler-worker")).unwrap();
    let admission = Admission {
        called: Cell::new(false),
        accept: true,
    };
    assert_eq!(
        worker
            .launch_and_admit(&request(5_000, 4096), &admission)
            .unwrap(),
        WorkerOutcome::ProducerUnavailable
    );
    assert!(!admission.called.get());
}

#[test]
fn hostile_candidate_must_cross_independent_admission() {
    let worker =
        LinuxNativeWorker::new(env!("CARGO_BIN_EXE_witchy-worker-fixture-candidate")).unwrap();
    let rejection = Admission {
        called: Cell::new(false),
        accept: false,
    };
    assert!(matches!(
        worker.launch_and_admit(&request(5_000, 4096), &rejection),
        Err(WorkerBoundaryError::AdmissionRejected(message))
            if message == "independent checker rejected candidate"
    ));
    assert!(rejection.called.get());

    let acceptance = Admission {
        called: Cell::new(false),
        accept: true,
    };
    assert_eq!(
        worker
            .launch_and_admit(&request(5_000, 4096), &acceptance)
            .unwrap(),
        WorkerOutcome::Admitted([23, 22, 25])
    );
    assert!(acceptance.called.get());
}

#[test]
fn required_worker_confinement_denies_environment_files_network_and_processes() {
    let worker =
        LinuxNativeWorker::new(env!("CARGO_BIN_EXE_witchy-worker-fixture-confined-probe")).unwrap();
    let admission = Admission {
        called: Cell::new(false),
        accept: true,
    };
    assert_eq!(
        worker
            .launch_and_admit(&request(5_000, 4096), &admission)
            .unwrap(),
        WorkerOutcome::Admitted([19, 18, 21])
    );
    assert!(admission.called.get());
}

#[test]
fn oversized_output_is_rejected_before_protocol_parsing() {
    let worker =
        LinuxNativeWorker::new(env!("CARGO_BIN_EXE_witchy-worker-fixture-oversized")).unwrap();
    let admission = Admission {
        called: Cell::new(false),
        accept: true,
    };
    assert!(matches!(
        worker.launch_and_admit(&request(5_000, 1024), &admission),
        Err(WorkerBoundaryError::ResponseTooLarge)
    ));
    assert!(!admission.called.get());
}

#[test]
fn wall_timeout_kills_and_reaps_an_unresponsive_worker() {
    let worker = LinuxNativeWorker::new(env!("CARGO_BIN_EXE_witchy-worker-fixture-stall")).unwrap();
    let admission = Admission {
        called: Cell::new(false),
        accept: true,
    };
    let start = Instant::now();
    assert!(matches!(
        worker.launch_and_admit(&request(50, 1024), &admission),
        Err(WorkerBoundaryError::TimedOut)
    ));
    assert!(start.elapsed() < Duration::from_secs(2));
    assert!(!admission.called.get());
}
