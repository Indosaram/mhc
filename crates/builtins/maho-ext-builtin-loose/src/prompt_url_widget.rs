use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptKind { Pr, Issue }

#[derive(Debug, PartialEq, Eq)]
pub struct PromptMatch<'a> { pub kind: PromptKind, pub url: &'a str }

pub fn extract_prompt_match(prompt: &str) -> Option<PromptMatch<'_>> {
    for (prefix, kind) in [
        ("You are given one or more GitHub PR URLs:", PromptKind::Pr),
        ("Analyze GitHub issue(s):", PromptKind::Issue),
    ] {
        for line in prompt.lines() {
            let line = line.trim_start();
            if line.len() >= prefix.len() && line.get(..prefix.len()).is_some_and(|start| start.eq_ignore_ascii_case(prefix))
                && let Some(url) = line[prefix.len()..].split_whitespace().next() {
                return Some(PromptMatch { kind, url });
            }
        }
    }
    None
}

pub fn format_author(author: Option<&Value>) -> Option<String> {
    let author = author?;
    let name = author.get("name").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty());
    let login = author.get("login").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty());
    match (name, login) {
        (Some(name), Some(login)) => Some(format!("{name} (@{login})")),
        (None, Some(login)) => Some(format!("@{login}")),
        (Some(name), None) => Some(name.to_owned()),
        (None, None) => None,
    }
}

pub fn desired_session_name(found: &PromptMatch<'_>, title: Option<&str>, current: Option<&str>) -> Option<String> {
    let label = match found.kind { PromptKind::Pr => "PR", PromptKind::Issue => "Issue" };
    let fallback = format!("{label}: {}", found.url);
    let current = current.map(str::trim).unwrap_or("");
    if !current.is_empty() && current != found.url && current != fallback { return None; }
    Some(match title.map(str::trim).filter(|title| !title.is_empty()) {
        Some(title) => format!("{label}: {title} ({})", found.url),
        None => fallback,
    })
}
