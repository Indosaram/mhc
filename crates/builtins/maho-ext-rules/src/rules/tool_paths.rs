use std::path::{Path, PathBuf};
use maho_ext_api::ToolResultEvent;
pub fn is_tracked_tool(name: &str) -> bool { super::constants::TRACKED_BUILTIN_TOOLS.contains(&name) }
pub fn extract_tool_paths(event: &ToolResultEvent, cwd: &Path) -> Vec<PathBuf> {
 if event.is_error || !is_tracked_tool(&event.tool_name) { return Vec::new(); }
 let first = match event.tool_name.as_str() { "read" | "edit" => event.details.as_ref().and_then(|details| details.get("filePath")), "write" => event.input.get("filePath"), _ => None };
 let mut paths = Vec::new();
 for value in [first, event.input.get("path")] {
  if let Some(path) = value.and_then(serde_json::Value::as_str).filter(|path| !path.is_empty()) {
   let path = Path::new(path); let resolved = if path.is_absolute() { path.to_path_buf() } else { cwd.join(path) };
   if !paths.contains(&resolved) { paths.push(resolved); }
  }
 }
 paths
}
