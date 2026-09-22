//! Exit codes double as the machine-readable `code:` field in the error envelope. An agent
//! branches on these, so they must stay stable.

use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorCode {
    Error = 1,
    Network = 2,
    AuthRequired = 3,
    NotFound = 4,
    RateLimited = 5,
    InvalidInput = 6,
    NoAccount = 7,
}

impl ErrorCode {
    pub fn name(self) -> &'static str {
        match self {
            ErrorCode::Error => "error",
            ErrorCode::Network => "network",
            ErrorCode::AuthRequired => "auth_required",
            ErrorCode::NotFound => "not_found",
            ErrorCode::RateLimited => "rate_limited",
            ErrorCode::InvalidInput => "invalid_input",
            ErrorCode::NoAccount => "no_account",
        }
    }
}

#[derive(Debug)]
pub struct Error {
    pub code: ErrorCode,
    pub message: String,
    /// Extra context - the upstream response body, the list of known accounts, and so on.
    pub detail: Option<String>,
    /// A literal command that fixes this. Agents surface it verbatim.
    pub remediation: Option<String>,
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Error { code, message: message.into(), detail: None, remediation: None }
    }

    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub fn fix(mut self, remediation: impl Into<String>) -> Self {
        self.remediation = Some(remediation.into());
        self
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Error::new(ErrorCode::InvalidInput, message)
    }

    pub fn other(message: impl Into<String>) -> Self {
        Error::new(ErrorCode::Error, message)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        match e.kind() {
            std::io::ErrorKind::NotFound => Error::invalid(e.to_string()),
            _ => Error::other(e.to_string()).detail("io"),
        }
    }
}
