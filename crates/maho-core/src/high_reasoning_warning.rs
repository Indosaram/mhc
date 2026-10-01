use maho_ai::types::ModelThinkingLevel;

fn matches_variant(id: &str, stem: &str) -> bool {
    id.match_indices(stem).any(|(start, matched)| {
        !id.as_bytes().get(start + matched.len()).is_some_and(u8::is_ascii_lowercase)
    })
}

pub fn is_sensitive_high_reasoning_model(id: &str) -> bool {
    let id = id.to_lowercase();
    if matches_variant(&id, "gpt-6-sol") || matches_variant(&id, "gpt-6-astra") { return true; }
    id.match_indices("gpt-5").any(|(start, _)| {
        let remainder = &id[start + 5..];
        let suffix = if let Some(version) = remainder.strip_prefix('.') {
            let digits = version.bytes().take_while(u8::is_ascii_digit).count();
            if digits == 0 { return false; }
            &version[digits..]
        } else { remainder };
        suffix.strip_prefix("-sol").is_some_and(|tail| !tail.as_bytes().first().is_some_and(u8::is_ascii_lowercase))
    })
}

pub fn should_warn_high_reasoning(id: &str, level: ModelThinkingLevel) -> bool {
    let warning_level = if matches_variant(&id.to_lowercase(), "gpt-6-astra") {
        level == ModelThinkingLevel::Max
    } else { matches!(level, ModelThinkingLevel::Xhigh | ModelThinkingLevel::Max) };
    warning_level && is_sensitive_high_reasoning_model(id)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HighReasoningWarningContent { pub title: String, pub body: Vec<String> }

pub fn build_high_reasoning_warning(provider: &str, id: &str, level: ModelThinkingLevel) -> HighReasoningWarningContent {
    let level = level.as_str();
    HighReasoningWarningContent {
        title: format!("⚠ HIGH-REASONING MODEL WARNING — {provider}/{id} @ {level}"),
        body: vec![
            format!("{id} is a frontier reasoning model. Running it at \"{level}\" effort makes it acutely sensitive to prompt quality."),
            "Driving this model directly from a human prompt is NOT recommended. Risks include:".into(),
            "  • The model may refuse to stop, looping or working far past the stated goal.".into(),
            "  • It may perform unrequested actions in order to \"complete\" the task.".into(),
            "  • It may take risky, irreversible, or dangerous actions to force completion.".into(),
            "Strongly recommended: use this model ONLY through the ultrabrain subagent.".into(),
            "Human prompts leave gaps; an agent-authored prompt is denser and stricter than a human's — exactly what these high-effort models need to stay bounded.".into(),
            "From the main agent, delegate instead: \"query ultrabrain with <task>\".".into(),
            format!("If you drive this model directly at {level}, YOU assume ALL responsibility for the consequences."),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sol_variants_are_sensitive() {
        for id in ["gpt-5.6-sol", "gpt-5.6-sol-fast", "openai.gpt-5.6-sol", "openai/gpt-5.6-sol", "openai/gpt-5.6-sol-pro"] {
            assert!(is_sensitive_high_reasoning_model(id));
        }
    }
    #[test]
    fn unrelated_models_are_not_sensitive() {
        for id in ["upstage/solar-pro-3", "gpt-6-luna", "gpt-6-solaris", "gpt-5.6-luna-fast", "gpt-5.6-terra", "gpt-5.6", "gpt-5.5", "gpt-5.2", "gpt-5.3-codex-spark", "gpt-4o", "claude-fable-5", "claude-opus-4-8", "opus-4.7", "opus-5", "claude-sonnet-5", "deepseek-v4-pro", "deepseek-v4-flash"] {
            assert!(!is_sensitive_high_reasoning_model(id), "{id}");
            assert!(!should_warn_high_reasoning(id, ModelThinkingLevel::Max), "{id}");
        }
    }
    #[test]
    fn gpt_six_sol_warns_at_extended_levels() {
        for id in ["gpt-6-sol", "gpt-6-sol-fast", "openai/gpt-6-sol", "openai/gpt-6-sol-pro", "global.openai.gpt-6-sol"] {
            assert!(should_warn_high_reasoning(id, ModelThinkingLevel::Xhigh));
            assert!(should_warn_high_reasoning(id, ModelThinkingLevel::Max));
            assert!(!should_warn_high_reasoning(id, ModelThinkingLevel::Medium));
        }
    }
    #[test]
    fn astra_warns_only_at_max() {
        for id in ["gpt-6-astra", "gpt-6-astra-fast", "openai/gpt-6-astra"] {
            assert!(!should_warn_high_reasoning(id, ModelThinkingLevel::Xhigh));
            assert!(should_warn_high_reasoning(id, ModelThinkingLevel::Max));
        }
    }
    #[test]
    fn ordinary_levels_do_not_warn() {
        for level in [ModelThinkingLevel::Off, ModelThinkingLevel::Minimal, ModelThinkingLevel::Low, ModelThinkingLevel::Medium, ModelThinkingLevel::High] {
            assert!(!should_warn_high_reasoning("gpt-5.6-sol", level));
        }
    }
    #[test]
    fn matching_is_case_insensitive() { assert!(is_sensitive_high_reasoning_model("OPENAI/GPT-5.6-SOL")); }
    #[test]
    fn sol_extended_levels_warn() {
        assert!(should_warn_high_reasoning("gpt-5.6-sol", ModelThinkingLevel::Xhigh));
        assert!(should_warn_high_reasoning("gpt-5.6-sol", ModelThinkingLevel::Max));
    }
}
