//! Error codes surfaced to IPC clients.

use serde::Serialize;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ErrorCode {
    BadRequest,
    NotFound,
    AuthRequired,
    Offline,
    Cancelled,
    Server,
    Internal,
}

/// An error carrying a protocol-level code. Wrapped inside `anyhow::Error`
/// and recovered by the IPC layer via `downcast_ref`.
#[derive(Debug, Clone)]
pub struct CodedError {
    pub code: ErrorCode,
    pub message: String,
}

impl fmt::Display for CodedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CodedError {}

pub fn coded(code: ErrorCode, message: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(CodedError {
        code,
        message: message.into(),
    })
}

pub fn not_found(what: impl Into<String>) -> anyhow::Error {
    coded(ErrorCode::NotFound, what)
}

pub fn bad_request(what: impl Into<String>) -> anyhow::Error {
    coded(ErrorCode::BadRequest, what)
}

/// Finds the protocol code for an arbitrary error chain.
pub fn code_of(err: &anyhow::Error) -> ErrorCode {
    for cause in err.chain() {
        if let Some(c) = cause.downcast_ref::<CodedError>() {
            return c.code;
        }
        if cause.downcast_ref::<std::io::Error>().is_some() {
            return ErrorCode::Offline;
        }
    }
    ErrorCode::Internal
}
