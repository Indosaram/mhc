pub fn map_claude_model_to_open_code(
    model: Option<&str>,
    anthropic_provider: Option<&str>,
) -> Option<(String, String)> {
    let (provider, model) = model_core::map_claude_model_to_provider(model, anthropic_provider)?;
    if provider.trim().is_empty() || model.trim().is_empty() {
        return None;
    }
    Some((provider, model))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn aliases_and_provider_models_map_while_unqualified_non_claude_models_do_not() {
        // Given upstream alias, normalized, qualified, and invalid inputs.
        let cases = [
            (None, None),
            (Some(""), None),
            (Some("  "), None),
            (Some("inherit"), None),
            (
                Some("claude-sonnet-4-5-20250514"),
                Some(("anthropic", "claude-sonnet-4-5-20250514")),
            ),
            (
                Some("claude-opus-4-7"),
                Some(("anthropic", "claude-opus-4-7")),
            ),
            (
                Some("claude-haiku-4-5-20251001"),
                Some(("anthropic", "claude-haiku-4-5-20251001")),
            ),
            (
                Some("claude-3-5-sonnet-20241022"),
                Some(("anthropic", "claude-3-5-sonnet-20241022")),
            ),
            (
                Some("claude-3.5-sonnet-20241022"),
                Some(("anthropic", "claude-3-5-sonnet-20241022")),
            ),
            (
                Some("anthropic/claude-3.5-sonnet"),
                Some(("anthropic", "claude-3-5-sonnet")),
            ),
            (Some("sonnet"), Some(("anthropic", "claude-sonnet-5-5"))),
            (Some("Sonnet"), Some(("anthropic", "claude-sonnet-5-5"))),
            (Some("opus"), Some(("anthropic", "claude-opus-5-5"))),
            (Some("haiku"), Some(("anthropic", "claude-haiku-5-5"))),
            (
                Some("claude-3.5-sonnet"),
                Some(("anthropic", "claude-3-5-sonnet")),
            ),
            (Some("openai/gpt-5.5"), Some(("openai", "gpt-5.5"))),
            (Some("gpt-5.5"), None),
            (Some("gemini-3-flash"), None),
            (Some("constructor"), None),
            (Some("  sonnet  "), Some(("anthropic", "claude-sonnet-5-5"))),
            (Some("toString"), None),
            (Some("valueOf"), None),
            (Some("/gpt-5.5"), None),
            (Some("openai/"), None),
            (Some("opencode/   "), None),
            (Some(" /model"), None),
            (
                Some("claude-sonnet-5-5"),
                Some(("anthropic", "claude-sonnet-5-5")),
            ),
            (
                Some("claude-opus-5-5"),
                Some(("anthropic", "claude-opus-5-5")),
            ),
            (
                Some("claude-3-opus-20240229"),
                Some(("anthropic", "claude-3-opus-20240229")),
            ),
            (
                Some("claude-sonnet-5.5"),
                Some(("anthropic", "claude-sonnet-5-5")),
            ),
            (
                Some("anthropic/claude-sonnet-5.5"),
                Some(("anthropic", "claude-sonnet-5-5")),
            ),
            (
                Some("anthropic/claude-sonnet-5-5"),
                Some(("anthropic", "claude-sonnet-5-5")),
            ),
            (
                Some("claude-haiku-5-5-20251001"),
                Some(("anthropic", "claude-haiku-5-5-20251001")),
            ),
        ];
        // When mapping each input, then the provider/model pair matches upstream.
        for (input, expected) in cases {
            assert_eq!(
                map_claude_model_to_open_code(input, None),
                expected.map(|(p, m)| (p.to_owned(), m.to_owned()))
            );
        }
    }

    #[test]
    fn anthropic_provider_override_does_not_rewrite_explicit_providers() {
        for (input, provider, model) in [
            ("sonnet", "kiro", "claude-sonnet-5-5"),
            ("opus", "kiro", "claude-opus-5-5"),
            ("haiku", "kiro", "claude-haiku-5-5"),
            ("claude-sonnet-5-5", "kiro", "claude-sonnet-5-5"),
            (
                "anthropic/claude-sonnet-5-5",
                "anthropic",
                "claude-sonnet-5-5",
            ),
        ] {
            assert_eq!(
                map_claude_model_to_open_code(Some(input), Some("kiro")),
                Some((provider.into(), model.into()))
            );
        }
    }
}
