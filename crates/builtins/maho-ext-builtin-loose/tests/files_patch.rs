use maho_ext_builtin_loose::files::collect_files;
use serde_json::json;

#[test]
fn executed_patch_collects_moved_and_updated_paths() {
    let branch = vec![
        json!({"type":"message","message":{"role":"assistant","content":[{"type":"toolCall","id":"patch","name":"apply_patch","arguments":{"input":"*** Begin Patch\n*** Update File: old.txt\n*** Move to: new.txt\n@@\n-old\n+new\n*** Add File: added.txt\n+added\n*** End Patch"}}]}}),
        json!({"type":"message","message":{"role":"toolResult","toolCallId":"patch","timestamp":7}}),
    ];
    let files = collect_files(&branch);
    assert_eq!(files.iter().map(|file|file.path.as_str()).collect::<Vec<_>>(), ["old.txt","new.txt","added.txt"]);
    assert!(files.iter().all(|file|file.operations.contains("edit") && file.last_timestamp == 7));
}
