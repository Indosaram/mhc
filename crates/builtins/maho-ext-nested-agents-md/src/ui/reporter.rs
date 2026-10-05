use maho_ext_api::{ExtensionContext, ExtensionWidgetOptions, WidgetContent};
use crate::core::{injection_cache::InjectionCache, types::InjectedFileInfo};
pub const STATUS_KEY: &str = "ext:nested-agents:status";
pub const WIDGET_KEY: &str = "ext:nested-agents:widget";
pub fn update_status(ctx: &ExtensionContext, cache: &InjectionCache, key: &str, has_errors: bool) {
 if !ctx.has_ui { return; }
 let count = cache.get_cache_size(key);
 let text = (count != 0).then(|| format!("🤖 {count}{}", if has_errors { " ⚠️" } else { "" }));
 ctx.ui.set_status(STATUS_KEY, text.as_deref());
}
pub fn clear_status(ctx: &ExtensionContext) { if ctx.has_ui { ctx.ui.set_status(STATUS_KEY, None); } }
pub fn update_widget(ctx: &ExtensionContext, visible: bool, files: &[InjectedFileInfo]) {
 if !ctx.has_ui { return; }
 let content = if visible && !files.is_empty() {
  let mut lines = vec!["Nested Context:".into()];
  lines.extend(files.iter().map(|file| {
   let display = file.absolute_path.strip_prefix(&ctx.cwd).unwrap_or(&file.absolute_path);
   format!("  {}{}", display.display(), if file.truncated { " (truncated)" } else { "" })
  }));
  Some(WidgetContent::Lines(lines))
 } else { None };
 ctx.ui.set_widget(WIDGET_KEY, content, ExtensionWidgetOptions::default());
}
pub fn build_debug_record(cache: &InjectionCache, key: &str, files: &[InjectedFileInfo]) -> serde_json::Value {
 serde_json::json!({"sessionKey": key, "cacheSize": cache.get_cache_size(key), "injectedDirectories": cache.list_injected(key), "injectedFiles": files.iter().map(|file| serde_json::json!({"path":file.absolute_path, "truncated":file.truncated})).collect::<Vec<_>>()})
}
