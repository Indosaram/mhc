pub const TASK_SUMMARY_MAX_LENGTH: usize = 80;

const ELLIPSIS: &str = "...";

// Harness-side enforcement of the schema maxLength: the tool advertises the limit, but an
// over-limit value is force-truncated here (prepareArguments runs before schema validation)
// instead of failing the spawn.
pub fn clamp_task_summary(value: Option<&str>) -> Option<String> {
    let normalized = value?.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return None;
    }
    if normalized.chars().count() <= TASK_SUMMARY_MAX_LENGTH {
        return Some(normalized);
    }
    let kept: String = normalized
        .chars()
        .take(TASK_SUMMARY_MAX_LENGTH - ELLIPSIS.len())
        .collect();
    Some(format!("{kept}{ELLIPSIS}"))
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn undefined_returns_none() {
        assert_eq!(clamp_task_summary(None), None);
    }

    #[test]
    fn blank_summary_collapses_to_none() {
        assert_eq!(clamp_task_summary(Some("")), None);
        assert_eq!(clamp_task_summary(Some("   \n\t ")), None);
    }

    #[test]
    fn multi_line_summary_collapses_whitespace() {
        assert_eq!(
            clamp_task_summary(Some("  Refactor auth\n  into sessions   module ")),
            Some("Refactor auth into sessions module".to_string())
        );
    }

    #[test]
    fn summary_at_limit_passes_through() {
        let summary = "x".repeat(TASK_SUMMARY_MAX_LENGTH);
        assert_eq!(clamp_task_summary(Some(&summary)), Some(summary));
    }

    #[test]
    fn over_limit_summary_is_force_truncated() {
        let clamped = clamp_task_summary(Some(&"y".repeat(200))).expect("clamped");
        assert_eq!(
            clamped,
            format!("{}...", "y".repeat(TASK_SUMMARY_MAX_LENGTH - 3))
        );
        assert_eq!(clamped.len(), TASK_SUMMARY_MAX_LENGTH);
    }
}
