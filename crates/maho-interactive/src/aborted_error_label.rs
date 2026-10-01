//! Port of aborted-error-label.ts.
#[derive(Debug, Clone, Copy)]
pub enum AgentAbortSource {
    User,
    System,
    Provider,
}
pub fn aborted_error_label(
    persisted: Option<&str>,
    attempt: usize,
    source: Option<AgentAbortSource>,
) -> String {
    match source {
        Some(AgentAbortSource::User) => return "Operation aborted".into(),
        Some(AgentAbortSource::System) => return "System operation aborted".into(),
        Some(AgentAbortSource::Provider) => {
            return persisted.unwrap_or("Provider request failed").into();
        }
        None => {}
    }
    if let Some(p) = persisted {
        let provider_retry = regex::Regex::new(r"^Provider retry failed after \d+ attempts?$")
            .is_ok_and(|r| r.is_match(p));
        if matches!(
            p,
            "Operation aborted" | "System operation aborted" | "Provider request failed"
        ) || p.starts_with("Provider request failed:")
            || provider_retry
        {
            return p.into();
        }
        if let Some(n) = p
            .strip_prefix("Aborted after ")
            .and_then(|v| {
                v.strip_suffix(" retry attempts")
                    .or_else(|| v.strip_suffix(" retry attempt"))
            })
            .filter(|v| !v.is_empty() && v.chars().all(|c| c.is_ascii_digit()))
        {
            return format!(
                "Provider retry failed after {n} attempt{}",
                if n == "1" { "" } else { "s" }
            );
        }
    }
    if attempt > 0 {
        return format!(
            "Provider retry failed after {attempt} attempt{}",
            if attempt == 1 { "" } else { "s" }
        );
    }
    match persisted {
        Some(p) if p != "Request was aborted" => format!("Provider request failed: {p}"),
        _ => "Provider request failed".into(),
    }
}
pub fn aborted_message_for_rendering(
    message: &serde_json::Value,
    attempt: usize,
    source: Option<AgentAbortSource>,
) -> serde_json::Value {
    let mut rendered = message.clone();
    if message
        .get("stopReason")
        .and_then(serde_json::Value::as_str)
        == Some("aborted")
    {
        rendered["errorMessage"] = serde_json::Value::String(aborted_error_label(
            message
                .get("errorMessage")
                .and_then(serde_json::Value::as_str),
            attempt,
            source,
        ));
    }
    rendered
}
