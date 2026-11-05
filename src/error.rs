use std::fmt;

/// Why a failure happened, for callers that decide what to do next.
///
/// The kind is coarse on purpose: it lets the chat screen offer `/login` for an
/// authentication failure, and a future turn controller decide whether a retry
/// is ever safe. It never carries upstream text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ErrorKind {
    /// Unclassified.
    #[default]
    Other,
    /// A credential is missing, expired, rejected, or misconfigured.
    Auth,
    /// Operator policy or an approval denied the operation.
    Policy,
    /// Network, TLS, timeout, or an HTTP status that is not a protocol fault.
    Transport,
    /// The remote spoke the protocol incorrectly, or truncated it.
    Protocol,
    /// A local size, count, or round limit was reached.
    Limit,
}

/// A failure a caller can show to a person. Panics are reserved for bugs.
#[derive(Debug, Clone)]
pub struct Error {
    message: String,
    kind: ErrorKind,
}

impl Error {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            kind: ErrorKind::Other,
        }
    }

    pub fn with_kind(message: impl Into<String>, kind: ErrorKind) -> Self {
        Self {
            message: message.into(),
            kind,
        }
    }

    pub fn auth(message: impl Into<String>) -> Self {
        Self::with_kind(message, ErrorKind::Auth)
    }

    pub fn transport(message: impl Into<String>) -> Self {
        Self::with_kind(message, ErrorKind::Transport)
    }

    pub fn protocol(message: impl Into<String>) -> Self {
        Self::with_kind(message, ErrorKind::Protocol)
    }

    pub fn limit(message: impl Into<String>) -> Self {
        Self::with_kind(message, ErrorKind::Limit)
    }

    pub fn policy(message: impl Into<String>) -> Self {
        Self::with_kind(message, ErrorKind::Policy)
    }

    pub fn kind(&self) -> ErrorKind {
        self.kind
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;
