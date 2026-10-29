use std::sync::LazyLock;

use model_core::{is_context_overflow_message, is_retryable_model_error, ErrorInfo};
use regex::Regex;

pub struct ModelMissResult<'a> {
    pub code: Option<i32>,
    pub stdout: &'a str,
    pub stderr: &'a str,
    pub timed_out: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetryableModelMiss {
    ModelNotVisible { id: String },
    AuthMissing { provider: String },
    ContextOverflow { detail: String },
    ProviderUnavailable { detail: String },
}

static ADVISORY_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\bunder (?:Bun|Node(?:\.js)?|Deno)\b|^(?:\[[^\]]*\]\s*)?(?:note|notice|info|hint|debug|warn|warning)\s*:").expect("valid regex")
});

static HTTP_STATUS_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:^|\s)(\d{3})\s*:").expect("valid regex")
});

const PROVIDER_DETAIL_MAX_CHARS: usize = 200;

fn truncate_utf16(s: &str, max_chars: usize) -> &str {
    let mut count = 0;
    for (idx, ch) in s.char_indices() {
        let len = ch.len_utf16();
        if count + len > max_chars {
            return &s[..idx];
        }
        count += len;
    }
    s
}

fn provider_failure_detail(result: &ModelMissResult<'_>) -> Option<String> {
    for stream in [result.stderr, result.stdout] {
        for line in stream.lines() {
            let trimmed = line.trim();
            if !trimmed.is_empty() && !ADVISORY_PATTERN.is_match(trimmed) {
                return Some(truncate_utf16(trimmed, PROVIDER_DETAIL_MAX_CHARS).to_string());
            }
        }
    }
    None
}

#[must_use]
pub fn classify_retryable_model_miss(result: &ModelMissResult<'_>) -> Option<RetryableModelMiss> {
    if result.timed_out || result.code == Some(0) {
        return None;
    }
    let output = format!("{}\n{}", result.stderr, result.stdout);
    for line in output.lines() {
        if let Some(id) = line
            .strip_prefix("Error: Model \"")
            .and_then(|value| value.strip_suffix("\" not found. Use --list-models to see available models."))
            && !id.is_empty()
            && !id.contains('"')
        {
            return Some(RetryableModelMiss::ModelNotVisible { id: id.to_owned() });
        }
    }
    for line in output.lines() {
        let line = line.strip_prefix("Error:").map(str::trim_start).unwrap_or(line);
        if let Some(value) = line
            .strip_prefix("No API key found for")
            .filter(|value| value.starts_with(char::is_whitespace))
        {
            let provider: String = value
                .trim_start()
                .chars()
                .take_while(|c| !c.is_whitespace() && *c != '.')
                .collect();
            if !provider.is_empty() {
                return Some(RetryableModelMiss::AuthMissing { provider });
            }
        }
    }

    let detail = provider_failure_detail(result)?;
    if is_context_overflow_message(&detail) {
        return Some(RetryableModelMiss::ContextOverflow { detail });
    }

    let status_code = HTTP_STATUS_PATTERN
        .captures(&detail)
        .and_then(|cap| cap.get(1))
        .and_then(|m| m.as_str().parse::<i64>().ok());

    let error_info = ErrorInfo {
        name: None,
        message: Some(detail.clone()),
        status_code,
    };

    if is_retryable_model_error(&error_info) {
        return Some(RetryableModelMiss::ProviderUnavailable { detail });
    }

    None
}

#[must_use]
pub fn is_retryable_model_miss(result: &ModelMissResult<'_>) -> bool {
    classify_retryable_model_miss(result).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(stderr: &str) -> ModelMissResult<'_> {
        ModelMissResult {
            code: Some(1),
            stdout: "",
            stderr,
            timed_out: false,
        }
    }

    #[test]
    fn model_not_visible() {
        assert_eq!(
            classify_retryable_model_miss(&result(
                "Error: Model \"extension-only/primary\" not found. Use --list-models to see available models."
            )),
            Some(RetryableModelMiss::ModelNotVisible {
                id: "extension-only/primary".into()
            })
        );
    }

    #[test]
    fn auth_missing() {
        assert_eq!(
            classify_retryable_model_miss(&result("No API key found for anthropic")),
            Some(RetryableModelMiss::AuthMissing {
                provider: "anthropic".into()
            })
        );
    }

    #[test]
    fn openai_context_overflow() {
        assert_eq!(
            classify_retryable_model_miss(&result("Your input exceeds the context window of this model")),
            Some(RetryableModelMiss::ContextOverflow {
                detail: "Your input exceeds the context window of this model".into()
            })
        );
    }

    #[test]
    fn anthropic_context_overflow() {
        assert_eq!(
            classify_retryable_model_miss(&result("prompt is too long: 213462 tokens > 200000 maximum")),
            Some(RetryableModelMiss::ContextOverflow {
                detail: "prompt is too long: 213462 tokens > 200000 maximum".into()
            })
        );
    }

    #[test]
    fn provider_cooldown_503() {
        assert_eq!(
            classify_retryable_model_miss(&result("503: {\"message\":\"All providers are temporarily cooling down\"}")),
            Some(RetryableModelMiss::ProviderUnavailable {
                detail: "503: {\"message\":\"All providers are temporarily cooling down\"}".into()
            })
        );
    }

    #[test]
    fn exhausted_senpi_fallback_chain() {
        assert_eq!(
            classify_retryable_model_miss(&result("All configured providers are temporarily unavailable")),
            Some(RetryableModelMiss::ProviderUnavailable {
                detail: "All configured providers are temporarily unavailable".into()
            })
        );
    }

    #[test]
    fn quota_exhaustion_failure() {
        assert_eq!(
            classify_retryable_model_miss(&result("Error: quota exceeded for this organization")),
            Some(RetryableModelMiss::ProviderUnavailable {
                detail: "Error: quota exceeded for this organization".into()
            })
        );
    }

    #[test]
    fn prompt_shaped_context_overflow() {
        assert_eq!(
            classify_retryable_model_miss(&result("Error: context length exceeded for the submitted transcript")),
            Some(RetryableModelMiss::ContextOverflow {
                detail: "Error: context length exceeded for the submitted transcript".into()
            })
        );
    }

    #[test]
    fn timeout_and_success_not_retryable() {
        for (code, timed_out) in [(Some(1), true), (Some(0), false)] {
            assert!(classify_retryable_model_miss(&ModelMissResult {
                code,
                stdout: "",
                stderr: "No API key found for anthropic",
                timed_out
            })
            .is_none());
        }
    }

    const REAPER_ADVISORY: &str = "child reaper unavailable under Bun on win32: children orphaned by a terminated worker thread stay as zombies until this host exits";

    #[test]
    fn reaper_advisory_then_non_provider_error() {
        let text = format!("{REAPER_ADVISORY}\nError: reflection worker could not open its session file");
        let child = result(&text);
        assert!(classify_retryable_model_miss(&child).is_none());
    }

    #[test]
    fn reaper_advisory_then_rate_limit() {
        let text = format!("{REAPER_ADVISORY}\n429: {{\"error\":{{\"type\":\"rate_limit_error\",\"message\":\"Too many requests\"}}}}");
        let child = result(&text);
        assert_eq!(
            classify_retryable_model_miss(&child),
            Some(RetryableModelMiss::ProviderUnavailable {
                detail: "429: {\"error\":{\"type\":\"rate_limit_error\",\"message\":\"Too many requests\"}}".into()
            })
        );
    }

    #[test]
    fn only_reaper_advisory() {
        let child = result(REAPER_ADVISORY);
        assert!(classify_retryable_model_miss(&child).is_none());
    }

    #[test]
    fn unknown_advisory_then_real_error() {
        let child = result("note: response cache temporarily unavailable, continuing without it\nError: reflection worker could not open its session file");
        assert!(classify_retryable_model_miss(&child).is_none());
    }

    #[test]
    fn provider_outage_then_stack_error_line() {
        let child = result("The model is temporarily unavailable\nError: request failed\n    at send (provider.js:10:3)");
        assert_eq!(
            classify_retryable_model_miss(&child),
            Some(RetryableModelMiss::ProviderUnavailable {
                detail: "The model is temporarily unavailable".into()
            })
        );
    }
}
