//! Error type, error codes and exit codes.
//!
//! Every failure a command can report is a [`CliError`]. Its `code` is a stable snake_case name that
//! agents can branch on, and `exit_code` follows the table in [`default_exit`].
use crate::catalog;
use serde_json::{json, Value};
use std::fmt;

pub const EXIT_INTERNAL: i32 = 1;
pub const EXIT_USAGE: i32 = 2;
pub const EXIT_AUTH: i32 = 3;
pub const EXIT_CONNECTION: i32 = 4;
pub const EXIT_CORE_REJECTED: i32 = 5;
pub const EXIT_NOT_FOUND: i32 = 6;
pub const EXIT_GATE: i32 = 7;
pub const EXIT_VERIFY: i32 = 8;

#[derive(Debug, Clone)]
pub struct CliError {
    pub code: String,
    pub message: String,
    pub exit_code: i32,
    pub core_error_code: Option<i64>,
    pub details: Value,
}

pub type Result<T> = std::result::Result<T, CliError>;

/// Default exit code for an error code. Any code not listed here is a Core error name (exit 5).
pub fn default_exit(code: &str) -> i32 {
    match code {
        "internal" => EXIT_INTERNAL,
        "usage" | "invalid_input" => EXIT_USAGE,
        "missing_config" | "login_failed" | "session_invalid" | "salt_missing" => EXIT_AUTH,
        "connection_failed" | "timeout" | "core_not_ready" | "protocol_error" => EXIT_CONNECTION,
        "not_found" | "ambiguous" => EXIT_NOT_FOUND,
        "unverified_operation" | "write_not_allowed" | "roundtrip_mismatch" => EXIT_GATE,
        "verify_failed" => EXIT_VERIFY,
        _ => EXIT_CORE_REJECTED,
    }
}

impl CliError {
    pub fn new(code: &str, message: impl Into<String>) -> CliError {
        CliError {
            code: code.to_string(),
            message: message.into(),
            exit_code: default_exit(code),
            core_error_code: None,
            details: json!({}),
        }
    }

    pub fn with_details(mut self, details: Value) -> CliError {
        self.details = details;
        self
    }

    pub fn with_core_code(mut self, code: i64) -> CliError {
        self.core_error_code = Some(code);
        self
    }

    pub fn with_exit(mut self, exit_code: i32) -> CliError {
        self.exit_code = exit_code;
        self
    }

    pub fn internal(message: impl Into<String>) -> CliError {
        CliError::new("internal", message)
    }

    pub fn invalid_input(message: impl Into<String>) -> CliError {
        CliError::new("invalid_input", message)
    }

    pub fn protocol(message: impl Into<String>) -> CliError {
        CliError::new("protocol_error", message)
    }

    pub fn to_json(&self) -> Value {
        json!({
            "code": self.code,
            "message": self.message,
            "core_error_code": self.core_error_code,
            "details": self.details,
        })
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for CliError {}

/// `AlreadyExist` becomes `already_exist`.
pub fn snake_case(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    for (i, c) in name.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// Turns a decoded `MessageGenericResponse` into Ok or a [`CliError`] named after the
/// `CommonErrorCodes` entry. `ErrorCode` 1 is success. `ResponseFlagsId` 2 (Failed) is an error even
/// then. Device actions answer `ErrorCode` 0 with flag 1 (Handled), which also counts as success.
pub fn check_generic_response(resp: &Value) -> Result<()> {
    let code = resp.get("ErrorCode").and_then(Value::as_i64).unwrap_or(0);
    let flags = resp.get("ResponseFlagsId").and_then(Value::as_i64).unwrap_or(0);
    let message = resp.get("ErrorMessage").and_then(Value::as_str).unwrap_or("");
    if code == 1 && flags != 2 || code == 0 && flags == 1 {
        return Ok(());
    }
    let (name, text) = if code == 1 || code == 0 {
        ("not_handled".to_string(), "the Core did not handle the request".to_string())
    } else {
        let name = catalog::enum_name("CommonErrorCodes", code)
            .map(snake_case)
            .unwrap_or_else(|| "core_error".to_string());
        let text = if message.is_empty() {
            format!("the Core answered ErrorCode {code}")
        } else {
            message.to_string()
        };
        (name, text)
    };
    Err(CliError::new(&name, text)
        .with_core_code(code)
        .with_details(json!({"response_flags": flags, "error_message": message})))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snake_case_names() {
        assert_eq!(snake_case("AlreadyExist"), "already_exist");
        assert_eq!(snake_case("Unauthorized"), "unauthorized");
        assert_eq!(snake_case("DeserializationException"), "deserialization_exception");
    }

    #[test]
    fn default_exit_codes() {
        assert_eq!(default_exit("usage"), 2);
        assert_eq!(default_exit("salt_missing"), 3);
        assert_eq!(default_exit("timeout"), 4);
        assert_eq!(default_exit("does_not_exist"), 5);
        assert_eq!(default_exit("ambiguous"), 6);
        assert_eq!(default_exit("write_not_allowed"), 7);
        assert_eq!(default_exit("verify_failed"), 8);
    }
}
