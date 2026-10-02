use maho_ext_builtin_loose::files::collect_files;
use serde_json::json;

#[test]
fn executed_files_coalesce_operations_and_use_result_timestamp() {
    let branch = vec![
        json!({"type":"message","message":{"role":"assistant","content":[
            {"type":"toolCall","id":"r","name":"read","arguments":{"path":"a"}},
            {"type":"toolCall","id":"w","name":"write","arguments":{"path":"a"}},
            {"type":"toolCall","id":"e","name":"edit","arguments":{"path":"b"}},
            {"type":"toolCall","id":"unused","name":"read","arguments":{"path":"unexecuted"}}
        ]}}),
        json!({"type":"message","message":{"role":"toolResult","toolCallId":"r","timestamp":1}}),
        json!({"type":"message","message":{"role":"toolResult","toolCallId":"w","timestamp":3}}),
        json!({"type":"message","message":{"role":"toolResult","toolCallId":"e","timestamp":2}}),
    ];
    let files = collect_files(&branch);
    assert_eq!(files.len(), 2);
    assert_eq!(files[0].path, "a");
    assert_eq!(files[0].last_timestamp, 3);
    assert_eq!(files[0].operations.iter().map(String::as_str).collect::<Vec<_>>(), vec!["read", "write"]);
    assert_eq!(files[1].path, "b");
}
