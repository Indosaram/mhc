#[derive(Debug, Clone, Copy)]
pub struct Fable55Launch {
    pub model_id: &'static str,
    pub mythos_model_id: &'static str,
    pub snapshot_id: Option<&'static str>,
    pub minimum_claude_code_version: Option<&'static str>,
    pub released: bool,
    pub promote_defaults: bool,
    pub refusal_fallback_models: &'static [&'static str],
    pub metadata_source: &'static str,
}

pub const FABLE55_LAUNCH: Fable55Launch = Fable55Launch {
    model_id: "claude-fable-5-5",
    mythos_model_id: "claude-mythos-5-5",
    snapshot_id: None,
    minimum_claude_code_version: None,
    released: false,
    promote_defaults: false,
    refusal_fallback_models: &["claude-opus-4-8", "claude-opus-5"],
    metadata_source: "models.dev",
};

pub fn is_fable55_model_id(model_id: &str) -> bool {
    let mut normalized = String::with_capacity(model_id.len());
    let mut in_whitespace = false;
    for character in model_id.to_lowercase().chars() {
        if character.is_whitespace() {
            if !in_whitespace {
                normalized.push('-');
            }
            in_whitespace = true;
        } else {
            normalized.push(character);
            in_whitespace = false;
        }
    }
    let separator = |character: Option<char>| character.is_none_or(|c| "/@:._-".contains(c));
    [
        Some(FABLE55_LAUNCH.model_id),
        Some(FABLE55_LAUNCH.mythos_model_id),
        FABLE55_LAUNCH.snapshot_id,
    ]
    .into_iter()
    .flatten()
    .filter(|id| !id.is_empty())
    .any(|id| {
        let family = id.strip_prefix("claude-").unwrap_or(id);
        let dotted = match family.rsplit_once('-') {
            Some((head, minor)) if minor.chars().all(|c| c.is_ascii_digit()) => {
                match head.rsplit_once('-') {
                    Some((_, major)) if major.chars().all(|c| c.is_ascii_digit()) => {
                        format!("{head}.{minor}")
                    }
                    _ => family.to_owned(),
                }
            }
            _ => family.to_owned(),
        };
        [family, dotted.as_str()].iter().any(|marker| {
            normalized.find(*marker).is_some_and(|at| {
                separator(normalized[..at].chars().next_back())
                    && separator(normalized[at + marker.len()..].chars().next())
            })
        })
    })
}
