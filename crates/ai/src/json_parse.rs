//! Public facade over the provider-level JSON helpers.
//!
//! The actual parsing/repair logic lives in `providers::json`; this module
//! exposes the two entry points callers outside this crate need when
//! processing streaming tool-call arguments or malformed provider payloads.

pub use crate::providers::json::{parse_streaming_json, repair_json};

use crate::error::Result;

/// Parse JSON, first attempting strict parsing, then falling back to repair.
/// Mirrors pi-ai's `parseJsonWithRepair`.
///
/// Errors are returned as [`crate::error::AiError::Json`] (converted from
/// `serde_json::Error` via `?`).
pub fn parse_json_with_repair(raw: &str) -> Result<serde_json::Value> {
    Ok(serde_json::from_str(raw).or_else(|_| serde_json::from_str(&repair_json(raw)))?)
}
