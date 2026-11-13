//! Typed service errors shared by every transport (REST, MCP, CLI).
//!
//! `code` is a stable snake_case machine code. The web UI translates it and
//! MCP clients branch on it, so a code is never renamed once released.

use std::fmt;

use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    NotFound,
    Conflict,
    Invalid,
    Unauthorized,
    Forbidden,
    Unavailable,
    Upstream,
    Internal,
    RateLimited,
    PayloadTooLarge,
}

#[derive(Debug, Clone)]
pub struct ServiceError {
    pub kind: ErrorKind,
    pub code: &'static str,
    pub message: String,
    pub details: Option<Value>,
}

pub type Result<T> = std::result::Result<T, ServiceError>;

macro_rules! constructors {
    ($($fn_name:ident => $kind:ident),+ $(,)?) => {
        $(
            pub fn $fn_name(code: &'static str, message: impl Into<String>) -> Self {
                Self::new(ErrorKind::$kind, code, message)
            }
        )+
    };
}

impl ServiceError {
    pub fn new(kind: ErrorKind, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            kind,
            code,
            message: message.into(),
            details: None,
        }
    }

    constructors! {
        not_found => NotFound,
        conflict => Conflict,
        invalid => Invalid,
        unauthorized => Unauthorized,
        forbidden => Forbidden,
        unavailable => Unavailable,
        upstream => Upstream,
        internal => Internal,
        rate_limited => RateLimited,
        payload_too_large => PayloadTooLarge,
    }

    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }
}

impl fmt::Display for ServiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ServiceError {}

impl From<sqlx::Error> for ServiceError {
    fn from(e: sqlx::Error) -> Self {
        // The detail goes to the log; callers only learn that storage failed.
        tracing::error!(error = %e, "database error");
        ServiceError::internal("database_error", "database error")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn constructors_set_kind_and_code() {
        let e = ServiceError::not_found("document_not_found", "no such document");
        assert_eq!(e.kind, ErrorKind::NotFound);
        assert_eq!(e.code, "document_not_found");
        assert!(e.details.is_none());
        assert_eq!(ServiceError::conflict("x", "m").kind, ErrorKind::Conflict);
        assert_eq!(ServiceError::invalid("x", "m").kind, ErrorKind::Invalid);
        assert_eq!(
            ServiceError::unauthorized("x", "m").kind,
            ErrorKind::Unauthorized
        );
        assert_eq!(ServiceError::forbidden("x", "m").kind, ErrorKind::Forbidden);
        assert_eq!(
            ServiceError::unavailable("x", "m").kind,
            ErrorKind::Unavailable
        );
        assert_eq!(ServiceError::upstream("x", "m").kind, ErrorKind::Upstream);
        assert_eq!(ServiceError::internal("x", "m").kind, ErrorKind::Internal);
        assert_eq!(
            ServiceError::rate_limited("x", "m").kind,
            ErrorKind::RateLimited
        );
        assert_eq!(
            ServiceError::payload_too_large("x", "m").kind,
            ErrorKind::PayloadTooLarge
        );
    }

    #[test]
    fn details_attach_and_display_shows_code() {
        let e = ServiceError::conflict("duplicate", "already there")
            .with_details(json!({"document_id": "abc"}));
        assert_eq!(e.details.as_ref().unwrap()["document_id"], "abc");
        assert_eq!(e.to_string(), "duplicate: already there");
    }
}
