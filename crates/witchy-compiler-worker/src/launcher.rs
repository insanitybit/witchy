use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use witchy_worker_protocol::{ResponseStatus, WorkerRequest, WorkerResponse, WorkerTarget};

use crate::WorkerBoundaryError;
use crate::state::PRODUCER_UNAVAILABLE_DIAGNOSTIC;

pub struct UntrustedCandidate<'a> {
    candidate_wir: &'a [u8],
    authority_wir: &'a [u8],
    final_byte_plan: &'a [u8],
}

impl<'a> UntrustedCandidate<'a> {
    pub fn candidate_wir(&self) -> &'a [u8] {
        self.candidate_wir
    }

    pub fn authority_wir(&self) -> &'a [u8] {
        self.authority_wir
    }

    pub fn final_byte_plan(&self) -> &'a [u8] {
        self.final_byte_plan
    }
}

/// The parent-side security boundary.  A candidate response is never returned
/// from this crate; it is handed directly to this independent checker.
pub trait CandidateAdmission {
    type Admitted;

    fn admit(
        &self,
        request: &WorkerRequest,
        candidate: UntrustedCandidate<'_>,
    ) -> Result<Self::Admitted, String>;
}

#[derive(Debug, Eq, PartialEq)]
pub enum WorkerOutcome<T> {
    Admitted(T),
    Rejected { diagnostics: String },
    ProducerUnavailable,
}

#[derive(Debug)]
pub struct LinuxNativeWorker {
    executable: PathBuf,
}

impl LinuxNativeWorker {
    pub fn new(executable: impl Into<PathBuf>) -> Result<Self, WorkerBoundaryError> {
        if !platform_supported() {
            return Err(WorkerBoundaryError::UnsupportedPlatform);
        }
        Ok(Self {
            executable: executable.into(),
        })
    }

    pub fn executable(&self) -> &Path {
        &self.executable
    }

    pub fn launch_and_admit<A: CandidateAdmission>(
        &self,
        request: &WorkerRequest,
        admission: &A,
    ) -> Result<WorkerOutcome<A::Admitted>, WorkerBoundaryError> {
        if request.target() != WorkerTarget::NativeRuntime {
            return Err(WorkerBoundaryError::UnsupportedTarget);
        }
        let request_bytes = request.encode()?;
        let mut command = Command::new(&self.executable);
        command
            .env_clear()
            .current_dir("/")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = command.spawn()?;
        let mut stdin = child.stdin.take().ok_or_else(|| {
            WorkerBoundaryError::Io(std::io::Error::other("worker stdin was not piped"))
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            WorkerBoundaryError::Io(std::io::Error::other("worker stdout was not piped"))
        })?;

        let writer = std::thread::spawn(move || -> std::io::Result<()> {
            stdin.write_all(&request_bytes)?;
            stdin.flush()
        });
        let maximum = usize::try_from(request.limits().max_response_bytes)
            .map_err(|_| WorkerBoundaryError::ResponseTooLarge)?;
        let reader = std::thread::spawn(move || read_bounded(stdout, maximum));
        let timeout = Duration::from_millis(request.limits().wall_time_ms);
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or(WorkerBoundaryError::TimedOut)?;

        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            let now = Instant::now();
            if now >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                let _ = writer.join();
                let _ = reader.join();
                return Err(WorkerBoundaryError::TimedOut);
            }
            std::thread::sleep((deadline - now).min(Duration::from_millis(5)));
        };

        let write_result = writer
            .join()
            .map_err(|_| WorkerBoundaryError::WorkerThread("request-writer"))?;
        let response_bytes = reader
            .join()
            .map_err(|_| WorkerBoundaryError::WorkerThread("response-reader"))??;
        if !status.success() {
            return Err(WorkerBoundaryError::WorkerExited(status.code()));
        }
        write_result?;
        let response = witchy_worker_protocol::decode_response(&response_bytes, request)?;
        admit_response(request, &response, admission)
    }
}

fn admit_response<A: CandidateAdmission>(
    request: &WorkerRequest,
    response: &WorkerResponse,
    admission: &A,
) -> Result<WorkerOutcome<A::Admitted>, WorkerBoundaryError> {
    match response.status() {
        ResponseStatus::Candidate => admission
            .admit(
                request,
                UntrustedCandidate {
                    candidate_wir: response.candidate_wir(),
                    authority_wir: response.authority_wir(),
                    final_byte_plan: response.final_byte_plan(),
                },
            )
            .map(WorkerOutcome::Admitted)
            .map_err(WorkerBoundaryError::AdmissionRejected),
        ResponseStatus::Rejected if response.diagnostics() == PRODUCER_UNAVAILABLE_DIAGNOSTIC => {
            Ok(WorkerOutcome::ProducerUnavailable)
        }
        ResponseStatus::Rejected => Ok(WorkerOutcome::Rejected {
            diagnostics: response.diagnostics().to_string(),
        }),
    }
}

fn read_bounded(mut reader: impl Read, maximum: usize) -> Result<Vec<u8>, WorkerBoundaryError> {
    let mut output = Vec::new();
    let mut chunk = [0_u8; 8192];
    loop {
        let count = reader.read(&mut chunk)?;
        if count == 0 {
            return Ok(output);
        }
        let next = output
            .len()
            .checked_add(count)
            .ok_or(WorkerBoundaryError::ResponseTooLarge)?;
        if next > maximum {
            return Err(WorkerBoundaryError::ResponseTooLarge);
        }
        output.extend_from_slice(&chunk[..count]);
    }
}

const fn platform_supported() -> bool {
    cfg!(all(
        target_os = "linux",
        any(
            target_arch = "x86_64",
            target_arch = "aarch64",
            target_arch = "riscv64"
        )
    ))
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use witchy_worker_protocol::{AdmissionSchemaId, WorkerLimits};

    use super::*;

    struct Admission {
        called: Cell<bool>,
        accept: bool,
    }

    impl CandidateAdmission for Admission {
        type Admitted = usize;

        fn admit(
            &self,
            _request: &WorkerRequest,
            candidate: UntrustedCandidate<'_>,
        ) -> Result<Self::Admitted, String> {
            self.called.set(true);
            if self.accept {
                Ok(candidate.candidate_wir().len())
            } else {
                Err("forged candidate".into())
            }
        }
    }

    fn request() -> WorkerRequest {
        WorkerRequest::new(
            b"bundle".to_vec(),
            WorkerTarget::NativeRuntime,
            AdmissionSchemaId::v2([8; 32]).unwrap(),
            WorkerLimits {
                cpu_time_ms: 1_000,
                wall_time_ms: 1_000,
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
            },
        )
        .unwrap()
    }

    #[test]
    fn candidate_bytes_cannot_bypass_the_admission_callback() {
        let request = request();
        let response =
            WorkerResponse::candidate(&request, vec![1], vec![2], vec![3], String::new()).unwrap();
        let admission = Admission {
            called: Cell::new(false),
            accept: false,
        };
        assert!(matches!(
            admit_response(&request, &response, &admission),
            Err(WorkerBoundaryError::AdmissionRejected(message)) if message == "forged candidate"
        ));
        assert!(admission.called.get());
    }

    #[test]
    fn rejection_and_unavailable_states_never_call_admission() {
        let request = request();
        let admission = Admission {
            called: Cell::new(false),
            accept: true,
        };
        let unavailable =
            WorkerResponse::rejected(&request, PRODUCER_UNAVAILABLE_DIAGNOSTIC.to_string())
                .unwrap();
        assert_eq!(
            admit_response(&request, &unavailable, &admission).unwrap(),
            WorkerOutcome::ProducerUnavailable
        );
        let rejected = WorkerResponse::rejected(&request, "ordinary rejection".into()).unwrap();
        assert_eq!(
            admit_response(&request, &rejected, &admission).unwrap(),
            WorkerOutcome::Rejected {
                diagnostics: "ordinary rejection".into()
            }
        );
        assert!(!admission.called.get());
    }

    #[test]
    fn bounded_reader_fails_closed() {
        assert_eq!(read_bounded(&b"1234"[..], 4).unwrap(), b"1234");
        assert!(matches!(
            read_bounded(&b"12345"[..], 4),
            Err(WorkerBoundaryError::ResponseTooLarge)
        ));
    }
}
