//! Provider-specific model id transforms (spec model-selection §4).

fn is_digits(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit())
}

/// `claude-<name>-<N>-<M>` → `claude-<name>-<N>.<M>`.
pub fn claude_dot_rule(id: &str) -> String {
    let Some(rest) = id.strip_prefix("claude-") else {
        return id.to_owned();
    };
    let parts: Vec<&str> = rest.split('-').collect();
    if parts.len() >= 3 {
        for i in 1..parts.len() - 1 {
            let name_ok = parts[..i]
                .iter()
                .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_alphabetic()));
            if name_ok && is_digits(parts[i]) && is_digits(parts[i + 1]) {
                let mut out = format!(
                    "claude-{}-{}.{}",
                    parts[..i].join("-"),
                    parts[i],
                    parts[i + 1]
                );
                for tail in &parts[i + 2..] {
                    out.push('-');
                    out.push_str(tail);
                }
                return out;
            }
        }
    }
    id.to_owned()
}

fn suffix_occurrences(id: &str, needle: &str, skip_after: Option<&str>) -> String {
    let mut out = String::with_capacity(id.len() + 8);
    let mut rest = id;
    let mut consumed = 0;
    while let Some(found) = rest.find(needle) {
        let absolute = consumed + found;
        let end = absolute + needle.len();
        out.push_str(&id[consumed..end]);
        let followed_by_dash = id.as_bytes().get(end) == Some(&b'-');
        let preceded = skip_after.is_some_and(|prefix| id[..absolute].ends_with(prefix));
        if !followed_by_dash && !preceded {
            out.push_str("-preview");
        }
        consumed = end;
        rest = &id[consumed..];
    }
    out.push_str(&id[consumed..]);
    out
}

/// `gemini-3.1-pro` not followed by `-` → `gemini-3.1-pro-preview`.
pub fn gemini_pro_rule(id: &str) -> String {
    suffix_occurrences(id, "gemini-3.1-pro", None)
}

/// `gemini-3-flash` not followed by `-` nor preceded by `antigravity-` → `…-preview`.
pub fn gemini_flash_rule(id: &str) -> String {
    suffix_occurrences(id, "gemini-3-flash", Some("antigravity-"))
}

fn vercel_sub_provider(model: &str) -> Option<&'static str> {
    [
        ("claude-", "anthropic"),
        ("gpt-", "openai"),
        ("gemini-", "google"),
        ("grok-", "xai"),
        ("minimax-", "minimax"),
        ("kimi-", "moonshotai"),
        ("k3", "moonshotai"),
        ("glm-", "zai"),
    ]
    .iter()
    .find(|(prefix, _)| model.starts_with(prefix))
    .map(|(_, sub)| *sub)
}

/// Adapts a canonical model id to how `provider` publishes it.
pub fn model_id_for_provider(provider: &str, model_id: &str) -> String {
    match provider {
        "vercel" => {
            if let Some((sub, model)) = model_id.split_once('/') {
                format!("{sub}/{}", gemini_pro_rule(&claude_dot_rule(model)))
            } else if let Some(sub) = vercel_sub_provider(model_id) {
                format!("{sub}/{}", gemini_pro_rule(&claude_dot_rule(model_id)))
            } else {
                model_id.to_owned()
            }
        }
        "github-copilot" => gemini_flash_rule(&gemini_pro_rule(&claude_dot_rule(model_id))),
        "google" => gemini_flash_rule(&gemini_pro_rule(model_id)),
        "kimi-coding" | "kimi-for-coding" => match model_id {
            "kimi-k3" => "k3".into(),
            "kimi-k3-256k" => "k3-256k".into(),
            other => other.to_owned(),
        },
        _ => model_id.to_owned(),
    }
}

/// [`model_id_for_provider`] after stripping one leading `provider/` from the model.
pub fn transform_for(provider: &str, model: &str) -> String {
    let stripped = model.strip_prefix(&format!("{provider}/")).unwrap_or(model);
    model_id_for_provider(provider, stripped)
}

/// The model as written with one leading `provider/` stripped.
pub fn strip_provider<'a>(provider: &str, model: &'a str) -> &'a str {
    model
        .strip_prefix(provider)
        .and_then(|rest| rest.strip_prefix('/'))
        .unwrap_or(model)
}
