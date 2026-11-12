use crate::quota_types::QuotaSnapshot;

pub fn format_quota(snapshot: &QuotaSnapshot) -> String {
    let mut lines = Vec::new();
    for (header, section) in [
        ("Credits", &snapshot.credits),
        ("Plan", &snapshot.plan),
        ("Usage", &snapshot.usage),
        ("Account", &snapshot.account),
    ] {
        if let Some(section) = section.as_deref().filter(|s| !s.is_empty()) {
            if !lines.is_empty() {
                lines.push(String::new());
            }
            lines.push(header.to_owned());
            lines.push(section.to_owned());
        }
    }
    if let Some(warning) = snapshot.warning.as_deref().filter(|s| !s.is_empty()) {
        if !lines.is_empty() {
            lines.push(String::new());
        }
        lines.push(warning.to_owned());
    }
    if !lines.is_empty() {
        lines.push(String::new());
    }
    lines.push("Full detail: https://commandcode.ai/usage".to_owned());
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn formats_sections() {
        let s = QuotaSnapshot {
            credits: Some("  Remaining: $1".into()),
            plan: Some("Plan: Pro".into()),
            usage: Some("  Cost: $2".into()),
            account: Some("  user".into()),
            warning: None,
        };
        let out = format_quota(&s);
        for h in [
            "Credits",
            "Plan",
            "Usage",
            "Account",
            "Full detail: https://commandcode.ai/usage",
        ] {
            assert!(out.contains(h));
        }
    }
    #[test]
    fn formats_empty() {
        let out = format_quota(&QuotaSnapshot::default());
        assert!(!out.contains("Credits\n"));
        assert!(out.ends_with("Full detail: https://commandcode.ai/usage"));
    }
}
