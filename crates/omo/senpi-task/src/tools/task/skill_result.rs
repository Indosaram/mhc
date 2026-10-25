//! `tools/task/skill-result.ts`: per-task skill summaries and the missing-skills notice.

use crate::tools::task::types::{SkillResolution, TaskSkillSummary};

/// Summarises a skill request; `None` when no skills were requested.
pub fn task_skill_summary(
    requested: &[String],
    resolution: &SkillResolution,
) -> Option<TaskSkillSummary> {
    if requested.is_empty() {
        return None;
    }
    Some(TaskSkillSummary {
        requested: requested.to_vec(),
        resolved: resolution.resolved.clone(),
        missing: resolution.missing.clone(),
        rejected: resolution
            .rejected
            .clone()
            .filter(|entries| !entries.is_empty()),
    })
}

/// Appends a deduplicated "Missing skills" notice when any summary reports missing names.
/// A single summary is passed as a one-element slice; `undefined` is an empty slice.
pub fn append_missing_skills(text: &str, summaries: &[Option<&TaskSkillSummary>]) -> String {
    let mut missing: Vec<&str> = Vec::new();
    for name in summaries
        .iter()
        .flatten()
        .flat_map(|summary| summary.missing.iter())
    {
        if !missing.contains(&name.as_str()) {
            missing.push(name.as_str());
        }
    }
    let mut reasons = Vec::new();
    for rejection in summaries
        .iter()
        .flatten()
        .flat_map(|summary| summary.rejected.iter().flatten())
    {
        if !reasons.contains(&rejection.reason.as_str()) {
            reasons.push(rejection.reason.as_str());
        }
    }
    let text = if reasons.is_empty() {
        text.to_string()
    } else {
        format!("{text}\n\n{}", reasons.join("\n"))
    };
    if missing.is_empty() {
        text.to_string()
    } else {
        format!(
            "{text}\n\nMissing skills: {}. Child started without them.",
            missing.join(", ")
        )
    }
}

#[cfg(test)]
mod parity_tests {
    use super::*;
    use crate::tools::task::types::SkillRejection;
    #[test]
    fn summary_preserves_rejections_without_marking_them_missing() {
        let rejection = SkillRejection {
            name: "escape".into(),
            reason: "blocked".into(),
        };
        let resolution = SkillResolution {
            rejected: Some(vec![rejection.clone()]),
            ..Default::default()
        };
        let summary = task_skill_summary(&["escape".into()], &resolution).expect("summary");
        assert_eq!(summary.rejected, Some(vec![rejection]));
        assert!(summary.missing.is_empty());
        let combined = append_missing_skills("started", &[Some(&summary), Some(&summary)]);
        assert_eq!(combined.matches("blocked").count(), 1);
    }
}
