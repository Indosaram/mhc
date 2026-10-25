//! Shared native/manager parent-model hop facts.
use crate::state::{ParentModelFallback, ParentModelFallbackReason};
pub fn base_selector(selector: &str) -> &str {
    match (selector.find('/'), selector.rfind(':')) {
        (Some(slash), Some(colon)) if colon > slash => &selector[..colon],
        (None, Some(colon)) => &selector[..colon],
        _ => selector,
    }
}

pub fn reason_of_failure(message: &str, model: Option<&str>) -> ParentModelFallbackReason {
    if super::credential_failure::usage_limit_scope(message).is_some() {
        ParentModelFallbackReason::UsageLimit
    } else if super::credential_failure::is_credential_failure(message, model) {
        ParentModelFallbackReason::NotSignedIn
    } else {
        ParentModelFallbackReason::Error
    }
}
pub fn build(
    from_model: String,
    reason: ParentModelFallbackReason,
    message: Option<&str>,
    tools: &[String],
    at: String,
) -> ParentModelFallback {
    ParentModelFallback {
        phase: None,
        from_model,
        reason,
        at,
        detail: message
            .and_then(|text| text.lines().map(str::trim).find(|line| !line.is_empty()))
            .map(|line| {
                if line.encode_utf16().count() > 200 {
                    format!("{}\u{2026}", crate::shared::utf16_prefix(line, 199))
                } else {
                    line.into()
                }
            }),
        resets: message.and_then(|text| {
            regex::Regex::new(r"(?i)\bresets?\s+((?:at|in)\s+)?([^.\n;]+)")
                .expect("reset pattern")
                .captures(text)
                .and_then(|capture| {
                    capture.get(2).and_then(|matched| {
                        let value = matched.as_str().trim();
                        let value = if value.matches(')').count() > value.matches('(').count() {
                            value.strip_suffix(')').unwrap_or(value).trim()
                        } else {
                            value
                        };
                        if value.is_empty() {
                            return None;
                        }
                        Some(format!(
                            "{}{}",
                            capture.get(1).map_or("", |prefix| prefix.as_str()),
                            value
                        ))
                    })
                })
        }),
        interrupted_tools: (!tools.is_empty()).then(|| tools.to_vec()),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quota_overrides_transient_and_detail_is_bounded() {
        assert_eq!(
            reason_of_failure("weekly limit exhausted", None),
            ParentModelFallbackReason::UsageLimit
        );
        let note = build(
            "p/m".into(),
            ParentModelFallbackReason::UsageLimit,
            Some(&format!("{}\nnext", "x".repeat(250))),
            &["write".into()],
            "now".into(),
        );
        assert_eq!(note.detail.unwrap().chars().count(), 200);
        assert_eq!(note.interrupted_tools.unwrap(), vec!["write"]);
    }
}

#[cfg(test)]
mod upstream_cases {
    use super::*;
    #[test]
    fn reset_and_selector_matrix() {
        for (message, expected) in [
            ("(resets 3:20am)", Some("3:20am")),
            ("resets in 2 hours; retry", Some("in 2 hours")),
            ("reset at 3pm (Asia/Seoul)", Some("at 3pm (Asia/Seoul)")),
            ("resets )", None),
            ("no reset", None),
        ] {
            assert_eq!(
                build(
                    "p/m".into(),
                    ParentModelFallbackReason::Error,
                    Some(message),
                    &[],
                    "now".into()
                )
                .resets
                .as_deref(),
                expected
            );
        }
        assert_eq!(base_selector("account:one/model:high"), "account:one/model");
        let detail = build(
            "p/m".into(),
            ParentModelFallbackReason::Error,
            Some(&"😀".repeat(110)),
            &[],
            "now".into(),
        )
        .detail
        .unwrap();
        assert!(detail.encode_utf16().count() <= 200);
        assert!(detail.ends_with('…'));
    }
}
