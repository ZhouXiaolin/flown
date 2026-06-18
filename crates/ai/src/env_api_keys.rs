//! Environment-variable API key lookup.
//!
//! Mirrors pi-ai's `env-api-keys.ts`, scoped to the two providers this
//! crate supports (Anthropic + OpenAI completions). For each known
//! provider, [`find_env_keys`] reports which environment variables could
//! supply an API key, and [`get_env_api_key`] returns the first one that
//! is actually set in the current process.
//!
//! Only ambient API-key sources are reported here — credential chains
//! like AWS profiles or Google Application Default Credentials are
//! handled inside their respective providers and are deliberately
//! excluded from this module, matching pi-ai.
use crate::types::{KnownProvider, Provider};
use std::env;

/// Return the environment-variable names that can supply an API key for
/// `provider`, in precedence order. Returns `None` if the provider is
/// not recognized by this module.
pub fn find_env_keys(provider: &Provider) -> Option<Vec<&'static str>> {
    let known = match provider {
        Provider::Known(k) => k,
        Provider::Custom(name) => return find_env_keys_by_name(name),
    };
    match known {
        // ANTHROPIC_OAUTH_TOKEN takes precedence over ANTHROPIC_API_KEY.
        KnownProvider::Anthropic => Some(vec!["ANTHROPIC_OAUTH_TOKEN", "ANTHROPIC_API_KEY"]),
        KnownProvider::OpenAi => Some(vec!["OPENAI_API_KEY"]),
        _ => None,
    }
}

fn find_env_keys_by_name(name: &str) -> Option<Vec<&'static str>> {
    match name {
        "anthropic" => Some(vec!["ANTHROPIC_OAUTH_TOKEN", "ANTHROPIC_API_KEY"]),
        "openai" => Some(vec!["OPENAI_API_KEY"]),
        _ => None,
    }
}

/// Look up the current process's environment for an API key for
/// `provider`. Returns `None` if the provider is unknown or none of its
/// known env vars are set.
pub fn get_env_api_key(provider: &Provider) -> Option<String> {
    let vars = find_env_keys(provider)?;
    vars.into_iter()
        .find_map(|name| env::var(name).ok().filter(|v| !v.trim().is_empty()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anthropic_reports_two_candidate_vars() {
        let keys = find_env_keys(&Provider::Known(KnownProvider::Anthropic)).unwrap();
        assert_eq!(keys, vec!["ANTHROPIC_OAUTH_TOKEN", "ANTHROPIC_API_KEY"]);
    }

    #[test]
    fn openai_reports_one_candidate_var() {
        let keys = find_env_keys(&Provider::Known(KnownProvider::OpenAi)).unwrap();
        assert_eq!(keys, vec!["OPENAI_API_KEY"]);
    }

    #[test]
    fn unknown_provider_returns_none() {
        assert!(find_env_keys(&Provider::Known(KnownProvider::Google)).is_none());
    }

    #[test]
    fn custom_provider_lookup_by_name() {
        let keys = find_env_keys(&Provider::Custom("anthropic".into())).unwrap();
        assert_eq!(keys, vec!["ANTHROPIC_OAUTH_TOKEN", "ANTHROPIC_API_KEY"]);
    }
}
