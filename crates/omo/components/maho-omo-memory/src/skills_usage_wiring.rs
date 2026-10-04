use std::{collections::BTreeMap, path::PathBuf, sync::{Arc, Mutex}};
use maho_ext_api::{EventKind, EventResult, ExtensionApi, ExtensionEvent, ToolCallEvent};
use crate::{context::MemoryIdentityContext, skills_usage_ledger::skills_usage_paths, skills_usage_tracker::{SkillsUsageTracker, extract_skill_id}};

pub type SkillsUsageTrackers = Arc<Mutex<BTreeMap<String, SkillsUsageTracker>>>;
pub type ResolveSkillsUsageContext = Arc<dyn Fn(&ToolCallEvent) -> Option<MemoryIdentityContext> + Send + Sync>;
#[derive(Clone)]
pub struct SkillsUsageOptions {
    pub resolve_context: ResolveSkillsUsageContext,
    pub resolve_cwd: Arc<dyn Fn() -> PathBuf + Send + Sync>,
    pub now_ms: Arc<dyn Fn() -> u64 + Send + Sync>,
}

fn is_read_tool(name: &str) -> bool {
    let name = name.trim().to_lowercase().replace('-', "_");
    ["read", "cat", "less"].iter().any(|tool| name == *tool || ["_", ":", "/"].iter().any(|separator| name.ends_with(&format!("{separator}{tool}"))))
}

pub fn record_skill_tool_call(trackers: &SkillsUsageTrackers, options: &SkillsUsageOptions, event: &ToolCallEvent) {
    let Some(context) = (options.resolve_context)(event) else { return; };
    if !is_read_tool(&event.tool_name) || !event.input.is_object() { return; }
    let paths = ["path", "filePath", "file_path", "target"].iter().filter_map(|field| event.input.get(field).and_then(serde_json::Value::as_str)).chain(event.input.get("paths").and_then(serde_json::Value::as_array).into_iter().flatten().filter_map(serde_json::Value::as_str));
    let cwd = (options.resolve_cwd)();
    let mut trackers = trackers.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    for path in paths {
        if path.is_empty() || extract_skill_id(&context.identity_paths.repo, path, &cwd).is_none() { continue; }
        trackers.entry(context.identity.clone()).or_insert_with(|| SkillsUsageTracker::new(skills_usage_paths(&context.identity_paths.runtime, &context.identity_paths.locks), context.identity_paths.repo.clone())).record_read(path, &cwd, (options.now_ms)());
    }
}

pub fn register_skills_usage(api: &mut ExtensionApi, options: SkillsUsageOptions) -> SkillsUsageTrackers {
    let trackers = Arc::new(Mutex::new(BTreeMap::new()));
    let handler_trackers = Arc::clone(&trackers);
    api.on(EventKind::ToolCall, Arc::new(move |event, _| {
        if let ExtensionEvent::ToolCall(event) = event { record_skill_tool_call(&handler_trackers, &options, event); }
        Box::pin(async { Ok(EventResult::None) })
    }));
    trackers
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(root: &std::path::Path, bound: bool) -> (SkillsUsageTrackers, SkillsUsageOptions, MemoryIdentityContext) {
        let paths = memory_core::identity::layout::build_identity_paths(root, "agent");
        let context = MemoryIdentityContext::new("agent".into(), paths, crate::binding::MemorySessionBinding { identity: "agent".into(), repo_path_hash: "hash".into(), bound_at: 0.0 });
        let resolve = context.clone();
        let cwd = context.identity_paths.repo.clone();
        (Arc::new(Mutex::new(BTreeMap::new())), SkillsUsageOptions { resolve_context: Arc::new(move |_| bound.then(|| resolve.clone())), resolve_cwd: Arc::new(move || cwd.clone()), now_ms: Arc::new(|| 0) }, context)
    }
    fn event(tool: &str, path: &str) -> ToolCallEvent { ToolCallEvent { tool_call_id: "call".into(), tool_name: tool.into(), input: serde_json::json!({"path":path}) } }
    #[test] fn read_skill_persists() {
        let root = tempfile::tempdir().unwrap(); let (trackers, options, context) = fixture(root.path(), true);
        record_skill_tool_call(&trackers, &options, &event("read", "skills/foo/SKILL.md"));
        trackers.lock().unwrap().get_mut("agent").unwrap().flush(|| "2026-01-15T10:00:00.000Z".into(), None, |error| panic!("{error}"));
        let ledger = crate::skills_usage_ledger::read_skills_usage_ledger(&context.identity_paths.runtime.join("skills-usage.json"));
        assert_eq!(ledger["foo"].count, 1.0);
        assert_eq!(ledger["foo"].last_used_at, "2026-01-15T10:00:00.000Z");
    }
    #[test] fn non_skill_read_creates_no_tracker() { let root = tempfile::tempdir().unwrap(); let (trackers, options, _) = fixture(root.path(), true); record_skill_tool_call(&trackers, &options, &event("read", "notes/facts.md")); assert!(trackers.lock().unwrap().is_empty()); }
    #[test] fn write_creates_no_tracker() { let root = tempfile::tempdir().unwrap(); let (trackers, options, _) = fixture(root.path(), true); record_skill_tool_call(&trackers, &options, &event("write", "skills/foo/SKILL.md")); assert!(trackers.lock().unwrap().is_empty()); }
    #[test] fn unbound_creates_no_tracker() { let root = tempfile::tempdir().unwrap(); let (trackers, options, _) = fixture(root.path(), false); record_skill_tool_call(&trackers, &options, &event("read", "skills/foo/SKILL.md")); assert!(trackers.lock().unwrap().is_empty()); }
    #[test] fn aborted_flush_does_not_write() { let root = tempfile::tempdir().unwrap(); let (trackers, options, context) = fixture(root.path(), true); record_skill_tool_call(&trackers, &options, &event("read", "skills/foo/SKILL.md")); trackers.lock().unwrap().get_mut("agent").unwrap().flush(|| "now".into(), Some(&|| true), |error| panic!("{error}")); assert!(!context.identity_paths.runtime.join("skills-usage.json").exists()); }
    #[test] fn namespaced_read_names_match() { for name in ["Read", "mcp-read", "tools:cat", "tools/less"] { assert!(is_read_tool(name)); } assert!(!is_read_tool("thread")); }
}
