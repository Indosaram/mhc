use model_core::NormalizedReasoning;
use model_core::SplitReasoningSuffix;
use model_core::SplitReasoningSuffixOptions;
use model_core::clamp_reasoning_level;
use model_core::normalize_reasoning;
use model_core::split_reasoning_suffix;
use pretty_assertions::assert_eq;

use crate::support::strings;

fn split(base: &str, level: Option<&str>) -> SplitReasoningSuffix {
    SplitReasoningSuffix {
        base: base.to_string(),
        level: level.map(str::to_string),
    }
}

#[test]
fn none_normalizes_to_off() {
    assert_eq!(
        normalize_reasoning("none"),
        NormalizedReasoning {
            level: Some("off".to_string()),
            passthrough: None
        }
    );
}

#[test]
fn unknown_token_passes_through() {
    assert_eq!(
        normalize_reasoning("thinking"),
        NormalizedReasoning {
            level: None,
            passthrough: Some("thinking".to_string())
        }
    );
}

#[test]
fn requested_reasoning_downgrades_within_one_ladder() {
    assert_eq!(
        clamp_reasoning_level("xhigh", &strings(&["low", "medium"])).as_deref(),
        Some("medium")
    );
}

#[test]
fn provider_suffix_extracts_reasoning_level() {
    assert_eq!(
        split_reasoning_suffix(
            "anthropic/claude:high",
            SplitReasoningSuffixOptions::default()
        ),
        split("anthropic/claude", Some("high"))
    );
}

#[test]
fn max_suffix_without_provider_prefix_stays_attached() {
    assert_eq!(
        split_reasoning_suffix("model:max", SplitReasoningSuffixOptions::default()),
        split("model:max", None)
    );
}

#[test]
fn max_suffix_with_provider_prefix_is_extracted() {
    assert_eq!(
        split_reasoning_suffix(
            "anthropic/claude:max",
            SplitReasoningSuffixOptions::default()
        ),
        split("anthropic/claude", Some("max"))
    );
}

#[test]
fn max_suffix_without_provider_prefix_is_extracted_when_allowed() {
    let options = SplitReasoningSuffixOptions {
        allow_max_suffix: Some(true),
    };
    assert_eq!(
        split_reasoning_suffix("gpt-5.6-sol:max", options),
        split("gpt-5.6-sol", Some("max"))
    );
}

#[test]
fn max_suffix_without_provider_prefix_and_no_option_stays_attached() {
    assert_eq!(
        split_reasoning_suffix("gpt-5.6-sol:max", SplitReasoningSuffixOptions::default()),
        split("gpt-5.6-sol:max", None)
    );
}
