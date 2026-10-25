pub fn map_claude_model_to_provider(
    model: Option<&str>,
    anthropic_provider: Option<&str>,
) -> Option<(String, String)> {
    let model = model?.trim();
    if model.is_empty() || model == "inherit" {
        return None;
    }
    let prefix = anthropic_provider.unwrap_or("anthropic");
    let mapped = match model.to_ascii_lowercase().as_str() {
        "sonnet" => format!("{prefix}/{}", crate::latest_models::CLAUDE_SONNET),
        "opus" => format!("{prefix}/{}", crate::latest_models::CLAUDE_OPUS),
        "haiku" => format!("{prefix}/{}", crate::latest_models::CLAUDE_HAIKU),
        _ if model.contains('/') => {
            let (provider, id) = model.split_once('/')?;
            if provider.is_empty() || id.is_empty() {
                return Some((provider.into(), id.into()));
            }
            if !id.starts_with("claude-") {
                return Some((provider.into(), id.into()));
            }
            format!("{provider}/{}", crate::normalize_model_id(id))
        }
        _ => {
            let normalized = crate::normalize_model_id(model);
            if !normalized.starts_with("claude-") {
                return None;
            }
            format!("{prefix}/{normalized}")
        }
    };
    let (provider, model) = mapped.split_once('/')?;
    Some((provider.to_owned(), model.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::map_claude_model_to_provider;

    #[test]
    fn maps_claude_code_aliases_to_current_models() {
        assert_eq!(
            map_claude_model_to_provider(Some("opus"), None),
            Some(("anthropic".into(), "claude-opus-5-5".into()))
        );
        assert_eq!(
            map_claude_model_to_provider(Some("HAIKU"), Some("anthropic-subscription")),
            Some(("anthropic-subscription".into(), "claude-haiku-5-5".into()))
        );
    }
}
