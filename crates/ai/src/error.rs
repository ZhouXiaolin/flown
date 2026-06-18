//! Crate-level error type for the `flown-ai` public API surface.
//!
//! All public functions that can fail return [`Result<T, AiError>`]. The
//! variants mirror pi-ai's failure modes: a missing provider registration,
//! tool-argument validation failures, malformed JSON, and IO errors.
//!
//! `thiserror` derives the [`Error`](std::error::Error) + `Display` impls so
//! callers can propagate with `?` and pattern-match precisely. `anyhow` is
//! reserved for provider-internal error aggregation and never appears in a
//! public signature.

use crate::types::Api;
use crate::images_types::ImagesApi;

/// Unified error returned by every fallible public API in this crate.
#[derive(Debug, thiserror::Error)]
pub enum AiError {
    /// No API provider has been registered for the requested [`Api`].
    ///
    /// Mirrors pi-ai's `throw new Error("No API provider registered for api: …")`
    /// (`stream.ts:32-38`); the Rust-idiomatic translation of `throw` is a
    /// `Result` the caller propagates with `?`. The message text matches pi-ai
    /// verbatim so downstream error-matching logic behaves identically.
    #[error("No API provider registered for api: {api}")]
    MissingProvider { api: Api },

    /// No image API provider has been registered for the requested [`ImagesApi`].
    #[error("No API provider registered for api: {api}")]
    MissingImagesProvider { api: ImagesApi },

    /// A registered provider was invoked with a model whose `api` does not
    /// match the provider registration key.
    ///
    /// Mirrors pi-ai's registry wrapper guard:
    /// `Mismatched api: ${model.api} expected ${api}`.
    #[error("Mismatched api: {actual} expected {expected}")]
    MismatchedApi { actual: Api, expected: Api },

    /// A registered image provider was invoked with a model whose `api` does not
    /// match the provider registration key.
    #[error("Mismatched api: {actual} expected {expected}")]
    MismatchedImagesApi {
        actual: ImagesApi,
        expected: ImagesApi,
    },

    /// Tool-argument validation failed. The inner string is the formatted,
    /// human-readable validation report (mirrors pi-ai's thrown `Error`
    /// message from `validateToolArguments`).
    #[error("tool validation failed: {0}")]
    Validation(String),

    /// Malformed JSON encountered while parsing provider payloads or tool
    /// arguments. Converted from [`serde_json::Error`] via `?`.
    #[error(transparent)]
    Json(#[from] serde_json::Error),

    /// IO failure (e.g. reading ambient credentials from disk).
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// HTTP failure from provider requests.
    #[error(transparent)]
    Http(#[from] reqwest::Error),
}

/// Convenience alias used throughout the crate.
pub type Result<T> = std::result::Result<T, AiError>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::KnownApi;

    #[test]
    fn missing_provider_displays_api() {
        let err = AiError::MissingProvider {
            api: Api::Known(KnownApi::AnthropicMessages),
        };
        assert_eq!(
            err.to_string(),
            "No API provider registered for api: anthropic-messages"
        );
    }

    #[test]
    fn validation_carries_message() {
        let err = AiError::Validation("Missing required field: path".into());
        assert!(err.to_string().contains("Missing required field: path"));
    }

    #[test]
    fn json_error_converts_via_from() {
        let json_err = serde_json::from_str::<serde_json::Value>("{bad}").unwrap_err();
        let ai_err: AiError = json_err.into();
        assert!(matches!(ai_err, AiError::Json(_)));
    }
}
