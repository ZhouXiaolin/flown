use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Structured error info attached to a diagnostic, mirroring pi-ai's
/// `DiagnosticErrorInfo` (`utils/diagnostics.ts:1-6`): `name` / `message` /
/// `stack` / `code`. `code` is `string | number` in pi-ai and is modelled here
/// as an opaque JSON value (either a string or a number) so it round-trips
/// through serde without a bespoke enum.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosticErrorInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stack: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<serde_json::Value>,
}

/// A diagnostic entry recorded on an [`crate::AssistantMessage`] during
/// provider/runtime execution. Unlike a thrown error, diagnostics are
/// accumulated so partial recoveries and retries remain visible after the
/// final message is produced.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantMessageDiagnostic {
    #[serde(rename = "type")]
    pub kind: String,
    pub timestamp: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<DiagnosticErrorInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

/// Format any thrown value as a short human-readable message, mirroring
/// pi-ai's `formatThrownValue`.
pub fn format_thrown_value(value: &dyn std::fmt::Display) -> String {
    value.to_string()
}

/// Extract structured error info from a standard error, mirroring pi-ai's
/// `extractDiagnosticError`. The Rust equivalent of an arbitrary thrown value
/// is `anyhow::Error` / `dyn std::error::Error`: its `type_name` becomes the
/// diagnostic `name` and its `to_string()` the `message`.
pub fn extract_diagnostic_error(err: &dyn std::error::Error) -> DiagnosticErrorInfo {
    DiagnosticErrorInfo {
        name: Some(std::any::type_name_of_val(err).to_string()),
        message: err.to_string(),
        stack: None,
        code: None,
    }
}

/// Build a diagnostic entry from an error, mirroring pi-ai's
/// `createAssistantMessageDiagnostic(type, error, details)`.
pub fn create_diagnostic(
    kind: impl Into<String>,
    err: &dyn std::error::Error,
    details: Option<serde_json::Value>,
) -> AssistantMessageDiagnostic {
    AssistantMessageDiagnostic {
        kind: kind.into(),
        timestamp: Utc::now(),
        error: Some(extract_diagnostic_error(err)),
        details,
    }
}

/// Append a diagnostic to an [`crate::AssistantMessage`], mirroring pi-ai's
/// `appendAssistantMessageDiagnostic(message, diagnostic)` which mutates the
/// message in place.
pub fn append_diagnostic(
    message: &mut crate::types::AssistantMessage,
    diagnostic: AssistantMessageDiagnostic,
) {
    message
        .diagnostics
        .get_or_insert_with(Vec::new)
        .push(diagnostic);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_diagnostic_captures_kind_and_message() {
        let err = std::io::Error::new(std::io::ErrorKind::Other, "boom");
        let diag = create_diagnostic("provider_error", &err, None);
        assert_eq!(diag.kind, "provider_error");
        assert_eq!(diag.error.as_ref().unwrap().message, "boom");
        assert!(diag.timestamp.timestamp() > 0);
    }
}
