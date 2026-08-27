use std::fmt;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProtocolError {
    BudgetExceeded(&'static str),
    DigestMismatch(&'static str),
    EmptyField(&'static str),
    InvalidDiagnostic,
    InvalidLimit(&'static str),
    InvalidMagic,
    InvalidResponseShape,
    InvalidUtf8,
    LengthOverflow(&'static str),
    NonCanonical,
    NonZeroReserved,
    RequestMismatch,
    TrailingInput,
    Truncated,
    UnknownMessageKind(u8),
    UnknownResponseStatus(u8),
    UnknownTarget(u16),
    UnsupportedAdmissionSchema(u16),
    UnsupportedProtocolSchema(u16),
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BudgetExceeded(field) => write!(formatter, "protocol budget exceeded: {field}"),
            Self::DigestMismatch(field) => write!(formatter, "digest mismatch: {field}"),
            Self::EmptyField(field) => write!(formatter, "required field is empty: {field}"),
            Self::InvalidDiagnostic => {
                write!(formatter, "diagnostic contains forbidden control text")
            }
            Self::InvalidLimit(field) => write!(formatter, "invalid worker limit: {field}"),
            Self::InvalidMagic => write!(formatter, "invalid worker protocol magic"),
            Self::InvalidResponseShape => write!(formatter, "invalid worker response shape"),
            Self::InvalidUtf8 => write!(formatter, "diagnostic is not valid UTF-8"),
            Self::LengthOverflow(field) => {
                write!(formatter, "field does not fit the wire length: {field}")
            }
            Self::NonCanonical => write!(formatter, "worker protocol frame is not canonical"),
            Self::NonZeroReserved => write!(formatter, "reserved worker protocol field is nonzero"),
            Self::RequestMismatch => {
                write!(formatter, "worker response belongs to a different request")
            }
            Self::TrailingInput => write!(formatter, "worker protocol frame has trailing input"),
            Self::Truncated => write!(formatter, "worker protocol frame is truncated"),
            Self::UnknownMessageKind(kind) => {
                write!(formatter, "unknown worker message kind {kind}")
            }
            Self::UnknownResponseStatus(status) => {
                write!(formatter, "unknown worker response status {status}")
            }
            Self::UnknownTarget(target) => write!(formatter, "unknown worker target {target}"),
            Self::UnsupportedAdmissionSchema(schema) => {
                write!(formatter, "unsupported admission schema {schema}")
            }
            Self::UnsupportedProtocolSchema(schema) => {
                write!(formatter, "unsupported worker protocol schema {schema}")
            }
        }
    }
}

impl std::error::Error for ProtocolError {}
