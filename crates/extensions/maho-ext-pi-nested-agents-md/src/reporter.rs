use crate::injection_cache::InjectionCache;
use std::path::{Path, PathBuf};

pub const STATUS_KEY: &str = "ext:nested-agents:status";
pub const WIDGET_KEY: &str = "ext:nested-agents:widget";
#[derive(Clone)]
pub struct InjectedFileMeta { pub absolute_path: PathBuf, pub truncated: bool }

pub fn update_status(ctx: &maho_ext_api::ExtensionContext, cache: &InjectionCache, session: &str, has_errors: bool) {
    if !ctx.has_ui { return; }
    let count = cache.cache_size(session);
    let text = (count > 0).then(||format!("🤖 {count}{}",if has_errors { " ⚠️" } else { "" }));
    ctx.ui.set_status(STATUS_KEY,text.as_deref());
}
pub fn update_widget(ctx: &maho_ext_api::ExtensionContext, visible: bool, files: &[InjectedFileMeta]) {
    if !ctx.has_ui { return; }
    let lines = if visible && !files.is_empty() {
        let mut lines = vec!["Nested Context:".into()];
        lines.extend(files.iter().map(|file|format!("  {}{}",display_path(&ctx.cwd,&file.absolute_path),if file.truncated { " (truncated)" } else { "" })));
        Some(maho_ext_api::WidgetContent::Lines(lines))
    } else { None };
    ctx.ui.set_widget(WIDGET_KEY,lines,Default::default());
}

pub fn build_debug_record(cache: &InjectionCache, session: &str, files: &[InjectedFileMeta]) -> serde_json::Value {
    serde_json::json!({
        "sessionKey":session, "cacheSize":cache.cache_size(session), "injectedDirectories":cache.list_injected(session),
        "injectedFiles":files.iter().map(|file| serde_json::json!({"path":file.absolute_path,"truncated":file.truncated})).collect::<Vec<_>>()
    })
}

pub fn display_path(cwd: &Path, absolute_path: &Path) -> String {
    if !absolute_path.is_absolute() { return absolute_path.to_string_lossy().into_owned(); }
    match absolute_path.strip_prefix(cwd) {
        Ok(relative) if !relative.as_os_str().is_empty() => relative.to_string_lossy().into_owned(),
        _ => absolute_path.to_string_lossy().into_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn relative_inside_project() { assert_eq!(display_path(Path::new("/repo"), Path::new("/repo/src/AGENTS.md")), "src/AGENTS.md"); }
    #[test] fn absolute_outside_project() { assert_eq!(display_path(Path::new("/repo"), Path::new("/repo-evil/AGENTS.md")), "/repo-evil/AGENTS.md"); }
    #[test] fn debug_record_preserves_cache_order() {
        let mut cache = InjectionCache::default();
        cache.mark_injected("session", "/repo/src");
        let result = build_debug_record(&cache, "session", &[InjectedFileMeta { absolute_path: "/repo/src/AGENTS.md".into(), truncated: true }]);
        assert_eq!(result["cacheSize"], 1);
        assert_eq!(result["injectedDirectories"], serde_json::json!(["/repo/src"]));
        assert_eq!(result["injectedFiles"][0]["truncated"], true);
    }
}
