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
    if missing.is_empty() {
        text.to_string()
    } else {
        format!(
            "{text}\n\nMissing skills: {}. Child started without them.",
            missing.join(", ")
        )
    }
}
