/// The reasoning ladder, lowest to highest.
pub const REASONING_LEVELS: [&str; 7] = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];

pub const REASONING_AUTO: &str = "auto";

/// Options for [`split_reasoning_suffix`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SplitReasoningSuffixOptions {
    /// Whether a bare `:max` suffix is a reasoning level. Defaults to "the base contains `/`".
    pub allow_max_suffix: Option<bool>,
}

/// `{ base; level? }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitReasoningSuffix {
    pub base: String,
    pub level: Option<String>,
}

/// `{ level?; passthrough? }`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NormalizedReasoning {
    pub level: Option<String>,
    pub passthrough: Option<String>,
}

#[must_use]
pub fn is_reasoning_level(value: &str) -> bool {
    REASONING_LEVELS.contains(&value)
}

#[must_use]
pub fn is_reasoning_level_or_auto(value: &str) -> bool {
    is_reasoning_level(value) || value == REASONING_AUTO
}

#[must_use]
pub fn normalize_reasoning(input: &str) -> NormalizedReasoning {
    let normalized = input.trim().to_lowercase();
    if normalized.is_empty() {
        return NormalizedReasoning::default();
    }
    if normalized == "none" {
        return NormalizedReasoning {
            level: Some("off".to_string()),
            passthrough: None,
        };
    }
    if is_reasoning_level_or_auto(&normalized) {
        return NormalizedReasoning {
            level: Some(normalized),
            passthrough: None,
        };
    }
    NormalizedReasoning {
        level: None,
        passthrough: Some(normalized),
    }
}

/// Highest allowed level at or below `value` on the ladder.
#[must_use]
pub fn clamp_reasoning_level(value: &str, allowed: &[String]) -> Option<String> {
    let requested_index = REASONING_LEVELS.iter().position(|level| *level == value)?;
    REASONING_LEVELS[..=requested_index]
        .iter()
        .rev()
        .find(|level| allowed.iter().any(|entry| entry == *level))
        .map(|level| (*level).to_string())
}

#[must_use]
pub fn split_reasoning_suffix(
    model: &str,
    options: SplitReasoningSuffixOptions,
) -> SplitReasoningSuffix {
    let trimmed = model.trim();
    let unsplit = || SplitReasoningSuffix {
        base: trimmed.to_string(),
        level: None,
    };
    let Some(separator_index) = trimmed.rfind(':') else {
        return unsplit();
    };
    let base = trimmed[..separator_index].trim();
    let token = trimmed[separator_index + 1..].trim().to_lowercase();
    if base.is_empty() || !is_reasoning_level_or_auto(&token) {
        return unsplit();
    }
    // ":max" doubles as a real model-id ending, so bare ids keep it attached unless the
    // caller knows the string is provider-prefixed.
    if token == "max"
        && !options
            .allow_max_suffix
            .unwrap_or_else(|| base.contains('/'))
    {
        return unsplit();
    }
    SplitReasoningSuffix {
        base: base.to_string(),
        level: Some(token),
    }
}
