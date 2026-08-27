use std::fmt;

#[derive(Debug)]
pub enum WorkerBoundaryError {
    AdmissionRejected(String),
    Confinement(String),
    Io(std::io::Error),
    Protocol(witchy_worker_protocol::ProtocolError),
    ResourceLimit {
        resource: &'static str,
        detail: String,
    },
    ResponseTooLarge,
    TimedOut,
    UnsupportedPlatform,
    UnsupportedTarget,
    WorkerExited(Option<i32>),
    WorkerThread(&'static str),
}

impl fmt::Display for WorkerBoundaryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AdmissionRejected(message) => {
                write!(
                    formatter,
                    "independent candidate admission rejected: {message}"
                )
            }
            Self::Confinement(message) => {
                write!(formatter, "required worker confinement failed: {message}")
            }
            Self::Io(error) => write!(formatter, "worker I/O failed: {error}"),
            Self::Protocol(error) => write!(formatter, "worker protocol failed: {error}"),
            Self::ResourceLimit { resource, detail } => {
                write!(
                    formatter,
                    "cannot tighten worker {resource} limit: {detail}"
                )
            }
            Self::ResponseTooLarge => {
                formatter.write_str("worker response exceeds the request ceiling")
            }
            Self::TimedOut => formatter.write_str("worker exceeded its wall-time limit"),
            Self::UnsupportedPlatform => formatter.write_str(
                "the RFC-0163 compiler worker currently requires Linux x86_64, aarch64, or riscv64",
            ),
            Self::UnsupportedTarget => {
                formatter.write_str("the confined worker currently supports only native-runtime")
            }
            Self::WorkerExited(code) => write!(
                formatter,
                "worker exited without a valid response{}",
                code.map_or_else(String::new, |code| format!(" (status {code})"))
            ),
            Self::WorkerThread(name) => write!(formatter, "worker {name} thread panicked"),
        }
    }
}

impl std::error::Error for WorkerBoundaryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Protocol(error) => Some(error),
            _ => None,
        }
    }
}

impl From<std::io::Error> for WorkerBoundaryError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<witchy_worker_protocol::ProtocolError> for WorkerBoundaryError {
    fn from(error: witchy_worker_protocol::ProtocolError) -> Self {
        Self::Protocol(error)
    }
}
