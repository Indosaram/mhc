use std::path::Path;
use super::{containment::resolve_and_contain, errors::InjectionFileReadError, find_agents_md_up::find_agents_md_up, format::format_directory_context, injection_cache::InjectionCache, truncate::truncate_bytes, types::{InjectedFileInfo, InjectionConfig, InjectionResult}};
pub fn inject_directory_context(file_path: &Path, root_dir: &Path, cache: &mut InjectionCache, session_key: &str, config: &InjectionConfig) -> InjectionResult {
 let mut result = InjectionResult::default();
 let Some(contained) = resolve_and_contain(file_path, root_dir) else { return result; };
 let Some(start) = contained.canonical_path.parent() else { return result; };
 let candidates = find_agents_md_up(start, &contained.canonical_root, &config.file_names);
 let mut budget = config.max_bytes_per_read;
 for path in candidates {
  let Some(directory) = path.parent() else { continue; };
  if cache.has_injected(session_key, directory) { continue; }
  if budget == 0 { break; }
  let content = match std::fs::read(&path) {
   Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
   Err(cause) => { result.errors.push(InjectionFileReadError { path, cause }); continue; }
  };
  let truncated = truncate_bytes(&content, config.max_bytes_per_file.min(budget));
  result.injected_text.push_str(&format_directory_context(&path, &truncated.result, truncated.truncated));
  cache.mark_injected(session_key, directory);
  budget = budget.saturating_sub(truncated.result_bytes);
  result.injected_files.push(InjectedFileInfo { directory: directory.to_path_buf(), absolute_path: path, truncated: truncated.truncated, original_bytes: truncated.original_bytes, injected_bytes: truncated.result_bytes });
 }
 result
}
