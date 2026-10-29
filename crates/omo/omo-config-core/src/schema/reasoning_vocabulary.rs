pub const REASONING_LEVELS: [&str; 7] = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];

pub const REASONING_AUTO: &str = "auto";

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NormalizedReasoning {
    pub level: Option<String>,
    pub passthrough: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReasoningSuffix {
    pub base: String,
    pub level: Option<String>,
}

pub fn is_reasoning_level(value: &str) -> bool {
    REASONING_LEVELS.contains(&value)
}

pub fn is_reasoning_level_or_auto(value: &str) -> bool {
    is_reasoning_level(value) || value == REASONING_AUTO
}

pub fn normalize_reasoning(input: &str) -> NormalizedReasoning {
    let normalized = input.trim().to_lowercase();
    if normalized.is_empty() {
        return NormalizedReasoning::default();
    }
    if normalized == "none" {
        return level("off");
    }
    if normalized == REASONING_AUTO {
        return level(REASONING_AUTO);
    }
    if is_reasoning_level(&normalized) {
        return level(&normalized);
    }
    NormalizedReasoning {
        level: None,
        passthrough: Some(normalized),
    }
}

fn level(value: &str) -> NormalizedReasoning {
    NormalizedReasoning {
        level: Some(value.to_string()),
        passthrough: None,
    }
}

pub fn split_reasoning_suffix(model: &str, allow_max_suffix: Option<bool>) -> ReasoningSuffix {
    let trimmed = model.trim();
    if trimmed.is_empty() {
        return ReasoningSuffix::default();
    }
    let Some(separator_index) = trimmed.rfind(':') else {
        return ReasoningSuffix {
            base: trimmed.to_string(),
            level: None,
        };
    };
    let base = trimmed[..separator_index].trim();
    let token = trimmed[separator_index + 1..].trim().to_lowercase();
    if base.is_empty() || !is_reasoning_level_or_auto(&token) {
        return ReasoningSuffix {
            base: trimmed.to_string(),
            level: None,
        };
    }
    let allow_max = allow_max_suffix.unwrap_or_else(|| base.contains('/'));
    if token == "max" && !allow_max {
        return ReasoningSuffix {
            base: trimmed.to_string(),
            level: None,
        };
    }
    ReasoningSuffix {
        base: base.to_string(),
        level: Some(token),
    }
}
