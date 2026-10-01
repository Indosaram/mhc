use crate::{containment::resolve_and_contain, find_agents_md_up::find_agents_md_up, format::format_directory_context, injection_cache::InjectionCache, truncate::truncate_bytes};
use std::path::Path;
pub use crate::types::{InjectedFileInfo, InjectionResult, InjectionConfig};

pub fn inject_directory_context(file: &Path, root: &Path, cache: &mut InjectionCache, session: &str, config: &InjectionConfig<'_>) -> InjectionResult {
    let mut result = InjectionResult::default();
    let Some((canonical_path, canonical_root)) = resolve_and_contain(file, root) else { return result; };
    let Some(parent) = canonical_path.parent() else { return result; };
    let candidates = find_agents_md_up(parent, &canonical_root, config.file_names);
    let mut budget = config.max_bytes_per_read;
    for agents_path in candidates {
        let Some(directory) = agents_path.parent() else { continue; };
        let key = directory.to_string_lossy();
        if cache.has_injected(session, &key) { continue; }
        if budget == 0 { break; }
        let bytes = match std::fs::read(&agents_path) {
            Ok(bytes) => bytes,
            Err(error) => { result.errors.push((agents_path.clone(), crate::errors::InjectionFileReadError { path: agents_path, cause: error })); continue; }
        };
        let content = String::from_utf8_lossy(&bytes);
        let truncated = truncate_bytes(&content, config.max_bytes_per_file.min(budget));
        result.injected_text.push_str(&format_directory_context(&agents_path.to_string_lossy(), &truncated.result, truncated.truncated));
        result.injected_files.push(InjectedFileInfo { absolute_path: agents_path.clone(), directory: directory.to_owned(), truncated: truncated.truncated, original_bytes: truncated.original_bytes, injected_bytes: truncated.result_bytes });
        cache.mark_injected(session, &key);
        budget -= truncated.result_bytes;
    }
    result
}
