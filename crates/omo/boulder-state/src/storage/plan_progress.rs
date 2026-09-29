//! Plan discovery and progress (`storage/plan-progress.ts`).

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::constants::{LEGACY_PROMETHEUS_PLANS_DIR, PROMETHEUS_PLANS_DIR};
use crate::plan_checklist::parse_plan_checklist;
use crate::types::PlanProgress;

/// Markdown plans under `.omo/plans` and the legacy `.sisyphus/plans`, newest first.
/// Any filesystem failure yields an empty list, as in the TypeScript package.
pub fn find_prometheus_plans(directory: &Path) -> Vec<PathBuf> {
    collect_plans(directory).unwrap_or_default()
}

fn collect_plans(directory: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut plans: Vec<(PathBuf, SystemTime)> = Vec::new();
    for plan_dir in [PROMETHEUS_PLANS_DIR, LEGACY_PROMETHEUS_PLANS_DIR] {
        let plans_dir = directory.join(plan_dir);
        if !plans_dir.exists() {
            continue;
        }
        for entry in std::fs::read_dir(&plans_dir)? {
            let entry = entry?;
            if entry.file_name().to_string_lossy().ends_with(".md") {
                let path = plans_dir.join(entry.file_name());
                let modified = std::fs::metadata(&path)?.modified()?;
                plans.push((path, modified));
            }
        }
    }
    plans.sort_by_key(|(_, modified)| std::cmp::Reverse(*modified));
    Ok(plans.into_iter().map(|(path, _)| path).collect())
}

/// Node's `path.basename(planPath, ".md")`.
pub fn get_plan_name(plan_path: &str) -> String {
    const EXTENSION: &str = ".md";
    if plan_path == EXTENSION {
        return String::new();
    }
    let trimmed = plan_path.trim_end_matches(std::path::is_separator);
    let segment = trimmed
        .rsplit(std::path::is_separator)
        .next()
        .unwrap_or_default();
    match segment.strip_suffix(EXTENSION) {
        Some(stem) if !stem.is_empty() => stem.to_string(),
        Some(_) | None => segment.to_string(),
    }
}

/// Top-level checklist progress of the plan at `plan_path`; a missing or unreadable
/// plan counts as empty.
pub fn get_plan_progress(plan_path: &Path) -> PlanProgress {
    let checklist = std::fs::read_to_string(plan_path)
        .map(|content| parse_plan_checklist(&content))
        .ok();
    match checklist {
        Some(checklist) => PlanProgress {
            total: checklist.total,
            completed: checklist.completed,
            is_complete: checklist.total > 0 && checklist.remaining == 0,
        },
        None => PlanProgress {
            total: 0,
            completed: 0,
            is_complete: false,
        },
    }
}
