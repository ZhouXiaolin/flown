//! Detect context-window overflow from assistant messages.
//!
//! Mirrors pi-ai's `utils/overflow.ts` 1:1: the same regex patterns (compiled
//! case-insensitive), the same non-overflow exclusions, and the same three
//! detection cases (explicit error message, silent overflow via usage, and
//! length-stop overflow with zero output).
//!
//! In addition to error-message matching, this module also detects *silent*
//! overflow cases: when a provider accepts the oversized request but reports
//! usage that already exceeds the context window, or when a provider truncates
//! the input to fit the context window and returns `stopReason: length` with
//! zero output tokens.
use crate::types::{AssistantMessage, StopReason};
use once_cell::sync::Lazy;
use regex::{Regex, RegexBuilder};

/// Regex patterns that reliably indicate a context-window overflow from a
/// supported provider. Translated verbatim (with the `i` flag) from pi-ai's
/// `OVERFLOW_PATTERNS` (`utils/overflow.ts:35-59`).
static OVERFLOW_PATTERNS: Lazy<Vec<Regex>> = Lazy::new(|| {
    [
        r"prompt is too long",                    // Anthropic token overflow
        r"request_too_large",                     // Anthropic request byte-size overflow (HTTP 413)
        r"input is too long for requested model", // Amazon Bedrock
        r"exceeds the context window",            // OpenAI (Completions & Responses API)
        r"exceeds (?:the )?(?:model'?s )?maximum context length(?: of [\d,]+ tokens?|\s*\([\d,]+\))", // OpenAI-compatible proxies (LiteLLM)
        r"input token count.*exceeds the maximum", // Google (Gemini)
        r"maximum prompt length is \d+",           // xAI (Grok)
        r"reduce the length of the messages",      // Groq
        r"maximum context length is \d+ tokens",   // OpenRouter (most backends)
        r"exceeds (?:the )?maximum allowed input length of [\d,]+ tokens?", // OpenRouter/Poolside
        r"input \(\d+ tokens\) is longer than the model'?s context length \(\d+ tokens\)", // Together AI
        r"exceeds the limit of \d+",           // GitHub Copilot
        r"exceeds the available context size", // llama.cpp server
        r"greater than the context length",    // LM Studio
        r"context window exceeds limit",       // MiniMax
        r"exceeded model token limit",         // Kimi For Coding
        r"too large for model with \d+ maximum context length", // Mistral
        r"model_context_window_exceeded", // z.ai non-standard finish_reason surfaced as error text
        r"prompt too long; exceeded (?:max )?context length", // Ollama explicit overflow error
        r"context[_ ]length[_ ]exceeded", // Generic fallback
        r"too many tokens",               // Generic fallback
        r"token limit exceeded",          // Generic fallback
        r"^4(?:00|13)\s*(?:status code)?\s*\(no body\)", // Cerebras: 400/413 with no body
    ]
    .iter()
    .map(|pat| {
        RegexBuilder::new(pat)
            .case_insensitive(true)
            .build()
            .expect("overflow pattern is valid regex")
    })
    .collect()
});

/// Patterns that indicate non-overflow errors (e.g. rate limiting, server
/// errors). Error messages matching any of these are excluded from overflow
/// detection even if they also match an `OVERFLOW_PATTERN`.
///
/// Example: Bedrock formats throttling errors as "ThrottlingException: Too
/// many tokens, please wait before trying again." which would match the
/// `/too many tokens/i` overflow pattern without this exclusion.
static NON_OVERFLOW_PATTERNS: Lazy<Vec<Regex>> = Lazy::new(|| {
    [
        r"^(Throttling error|Service unavailable):", // AWS Bedrock non-overflow errors (human-readable prefixes from formatBedrockError)
        r"rate limit",                               // Generic rate limiting
        r"too many requests",                        // Generic HTTP 429 style
    ]
    .iter()
    .map(|pat| {
        RegexBuilder::new(pat)
            .case_insensitive(true)
            .build()
            .expect("non-overflow pattern is valid regex")
    })
    .collect()
});

fn matches_any(haystack: &str, patterns: &[Regex]) -> bool {
    patterns.iter().any(|re| re.is_match(haystack))
}

/// Returns `true` if `message` indicates a context-window overflow.
///
/// Pass `context_window` (the model's context window in tokens) to also
/// detect silent overflow: providers that accept oversized requests but
/// report usage that already fills or exceeds the context window.
pub fn is_context_overflow(message: &AssistantMessage, context_window: Option<u32>) -> bool {
    // Case 1: explicit error message.
    if message.stop_reason == StopReason::Error {
        if let Some(text) = message.error_message.as_deref() {
            if !matches_any(text, &NON_OVERFLOW_PATTERNS) && matches_any(text, &OVERFLOW_PATTERNS) {
                return true;
            }
        }
    }

    let Some(window) = context_window else {
        return false;
    };

    // Case 2: silent overflow — successful response but usage already
    // exceeds the model's context window (OpenAI's z.ai-style behaviour).
    if message.stop_reason == StopReason::Stop {
        let input_tokens = message.usage.input.saturating_add(message.usage.cache_read);
        if input_tokens > window {
            return true;
        }
    }

    // Case 3: length-stop overflow — provider truncates input to fit the
    // context window, leaving no room for output.
    if message.stop_reason == StopReason::Length && message.usage.output == 0 {
        let input_tokens = message.usage.input.saturating_add(message.usage.cache_read);
        if (input_tokens as f64) >= (window as f64) * 0.99 {
            return true;
        }
    }

    false
}

/// Get the overflow regex patterns, mirroring pi-ai's `getOverflowPatterns()`.
pub fn get_overflow_patterns() -> &'static [Regex] {
    &OVERFLOW_PATTERNS
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Api, Cost, Provider, Usage};
    use chrono::Utc;

    fn message(stop_reason: StopReason, error: Option<&str>) -> AssistantMessage {
        AssistantMessage {
            role: "assistant".to_string(),
            content: vec![],
            api: Api::Known(crate::types::KnownApi::OpenAiCompletions),
            provider: Provider::Known(crate::types::KnownProvider::OpenAi),
            model: "gpt-test".to_string(),
            response_model: None,
            response_id: None,
            usage: Usage::default(),
            stop_reason,
            error_message: error.map(|s| s.to_string()),
            diagnostics: None,
            timestamp: Utc::now(),
        }
    }

    fn message_with_usage(stop_reason: StopReason, usage: Usage) -> AssistantMessage {
        AssistantMessage {
            role: "assistant".to_string(),
            content: vec![],
            api: Api::Known(crate::types::KnownApi::OpenAiCompletions),
            provider: Provider::Known(crate::types::KnownProvider::OpenAi),
            model: "gpt-test".to_string(),
            response_model: None,
            response_id: None,
            usage,
            stop_reason,
            error_message: None,
            diagnostics: None,
            timestamp: Utc::now(),
        }
    }

    #[test]
    fn anthropic_prompt_too_long_is_detected() {
        let msg = message(
            StopReason::Error,
            Some("prompt is too long: 213462 tokens > 200000 maximum"),
        );
        assert!(is_context_overflow(&msg, None));
    }

    #[test]
    fn anthropic_request_too_large_is_detected() {
        let msg = message(
            StopReason::Error,
            Some("413 {\"error\":{\"type\":\"request_too_large\"}}"),
        );
        assert!(is_context_overflow(&msg, None));
    }

    #[test]
    fn openai_context_window_exceeded_is_detected() {
        let msg = message(
            StopReason::Error,
            Some("Your input exceeds the context window of this model"),
        );
        assert!(is_context_overflow(&msg, None));
    }

    #[test]
    fn openai_maximum_context_length_is_detected() {
        let msg = message(
            StopReason::Error,
            Some(
                "Requested token count exceeds the model's maximum context length of 131072 tokens",
            ),
        );
        assert!(is_context_overflow(&msg, None));
    }

    #[test]
    fn rate_limit_is_not_overflow() {
        let msg = message(
            StopReason::Error,
            Some("Rate limit exceeded, too many tokens, please retry"),
        );
        assert!(!is_context_overflow(&msg, None));
    }

    #[test]
    fn silent_overflow_via_usage_is_detected() {
        let usage = Usage {
            input: 140_000,
            output: 100,
            cache_read: 0,
            cache_write: 0,
            cache_write_1h: None,
            total_tokens: 140_100,
            cost: Cost::default(),
        };
        let msg = message_with_usage(StopReason::Stop, usage);
        assert!(is_context_overflow(&msg, Some(128_000)));
    }

    #[test]
    fn length_stop_with_zero_output_and_full_context_is_overflow() {
        let usage = Usage {
            input: 128_000,
            output: 0,
            cache_read: 0,
            cache_write: 0,
            cache_write_1h: None,
            total_tokens: 128_000,
            cost: Cost::default(),
        };
        let msg = message_with_usage(StopReason::Length, usage);
        assert!(is_context_overflow(&msg, Some(128_000)));
    }

    #[test]
    fn normal_stop_is_not_overflow() {
        let msg = message(StopReason::Stop, None);
        assert!(!is_context_overflow(&msg, Some(128_000)));
    }

    #[test]
    fn cerebras_no_body_status_is_detected() {
        // The `^4(?:00|13)...(no body)` pattern is anchored at the start,
        // matching Cerebras' terse 400/413-with-no-body errors.
        let msg = message(StopReason::Error, Some("413 status code (no body)"));
        assert!(is_context_overflow(&msg, None));
    }

    #[test]
    fn together_ai_pattern_is_detected() {
        let msg = message(
            StopReason::Error,
            Some("The input (123 tokens) is longer than the model's context length (100 tokens)."),
        );
        assert!(is_context_overflow(&msg, None));
    }
}
