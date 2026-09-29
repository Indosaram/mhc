use std::path::Path;

use crate::plan_checklist::parse_current_top_level_task;
use crate::types::TopLevelTaskRef;

/// First unchecked top-level task of a structured plan; `None` for a missing,
/// unreadable or unstructured plan.
pub fn read_current_top_level_task(plan_path: &Path) -> Option<TopLevelTaskRef> {
    let bytes = std::fs::read(plan_path).ok()?;
    parse_current_top_level_task(&String::from_utf8_lossy(&bytes))
}
