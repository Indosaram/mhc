use maho_ext_nested_agents_md::core::{containment::resolve_and_contain, injection_cache::InjectionCache, inject_directory_context::inject_directory_context, truncate::truncate_bytes, types::InjectionConfig};
use std::path::Path;
#[test]
fn missing_target_is_not_contained() {
 let root = tempfile::tempdir().unwrap();
 let result = resolve_and_contain(Path::new("missing"), root.path());
 assert!(result.is_none());
}
#[test]
fn root_itself_is_not_contained() {
 let root = tempfile::tempdir().unwrap();
 let result = resolve_and_contain(root.path(), root.path());
 assert!(result.is_none());
}
#[test]
fn outside_target_is_not_contained() {
 let root = tempfile::tempdir().unwrap(); let outside = tempfile::NamedTempFile::new().unwrap();
 let result = resolve_and_contain(outside.path(), root.path());
 assert!(result.is_none());
}
#[test]
fn truncation_keeps_complete_utf8() {
 let result = truncate_bytes("a한b", 3);
 assert_eq!((result.result.as_str(), result.result_bytes, result.original_bytes, result.truncated), ("a", 1, 5, true));
}
#[test]
fn untruncated_content_preserves_replacement_character() {
 let result = truncate_bytes("�", 3);
 assert_eq!((result.result.as_str(), result.truncated), ("�", false));
}
#[test]
fn truncated_content_removes_trailing_replacement_character() {
 let result = truncate_bytes("a�b", 4);
 assert_eq!((result.result.as_str(), result.result_bytes), ("a", 1));
}
#[test]
fn cache_is_scoped_by_session() {
 let mut cache = InjectionCache::default(); cache.mark_injected("a", Path::new("/project/nested"));
 assert_eq!((cache.get_cache_size("a"), cache.get_cache_size("b")), (1, 0));
}
#[test]
fn cache_preserves_insertion_order_without_duplicates() {
 let mut cache = InjectionCache::default(); cache.mark_injected("a", Path::new("/z")); cache.mark_injected("a", Path::new("/a")); cache.mark_injected("a", Path::new("/z"));
 assert_eq!(cache.list_injected("a"), &[Path::new("/z").to_path_buf(), Path::new("/a").to_path_buf()]);
}
#[test]
fn injection_excludes_root_and_orders_parent_first() {
 let root = tempfile::tempdir().unwrap(); let child = root.path().join("child"); let leaf = child.join("leaf");
 std::fs::create_dir_all(&leaf).unwrap(); std::fs::write(root.path().join("AGENTS.md"), "root").unwrap(); std::fs::write(child.join("AGENTS.md"), "parent").unwrap(); std::fs::write(leaf.join("AGENTS.md"), "leaf").unwrap(); std::fs::write(leaf.join("file.rs"), "target").unwrap();
 let result = inject_directory_context(&leaf.join("file.rs"), root.path(), &mut InjectionCache::default(), "s", &InjectionConfig::default());
 assert_eq!(result.injected_files.iter().map(|file| file.absolute_path.clone()).collect::<Vec<_>>(), vec![child.join("AGENTS.md"), leaf.join("AGENTS.md")]);
}
#[test]
fn repeated_reads_do_not_inject_cached_directory() {
 let root = tempfile::tempdir().unwrap(); let child = root.path().join("child"); std::fs::create_dir(&child).unwrap(); std::fs::write(child.join("AGENTS.md"), "body").unwrap(); std::fs::write(child.join("target"), "target").unwrap(); let mut cache = InjectionCache::default(); inject_directory_context(&child.join("target"), root.path(), &mut cache, "s", &InjectionConfig::default());
 let result = inject_directory_context(&child.join("target"), root.path(), &mut cache, "s", &InjectionConfig::default());
 assert!(result.injected_files.is_empty());
}
#[test]
fn unreadable_instruction_is_recorded_without_caching() {
 let root = tempfile::tempdir().unwrap(); let child = root.path().join("child"); std::fs::create_dir_all(child.join("AGENTS.md")).unwrap(); std::fs::write(child.join("target"), "target").unwrap(); let mut cache = InjectionCache::default();
 let result = inject_directory_context(&child.join("target"), root.path(), &mut cache, "s", &InjectionConfig::default());
 assert_eq!((result.errors.len(), cache.get_cache_size("s")), (1, 0));
}
#[test]
fn per_read_budget_limits_injected_content() {
 let root = tempfile::tempdir().unwrap(); let child = root.path().join("child"); std::fs::create_dir(&child).unwrap(); std::fs::write(child.join("AGENTS.md"), "abcdef").unwrap(); std::fs::write(child.join("target"), "target").unwrap(); let config = InjectionConfig { max_bytes_per_read: 2, ..Default::default() };
 let result = inject_directory_context(&child.join("target"), root.path(), &mut InjectionCache::default(), "s", &config);
 assert_eq!((result.injected_files[0].injected_bytes, result.injected_files[0].truncated), (2, true));
}
#[cfg(unix)]
#[test]
fn symlink_target_outside_project_is_not_injected() {
 let root = tempfile::tempdir().unwrap(); let target = tempfile::NamedTempFile::new().unwrap(); std::os::unix::fs::symlink(target.path(), root.path().join("link")).unwrap();
 let result = resolve_and_contain(Path::new("link"), root.path());
 assert!(result.is_none());
}
