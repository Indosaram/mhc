//! Translated from deep-merge, config-merge, config-section-parser, env-expansion, record-type-guard,
//! tool-name, replace-tool-args, format-duration, classify-path-environment, archive-entry-validator.

use std::cell::RefCell;
use std::collections::HashMap;

use omo_config_core::internal::validate::{
    Node, array, optional, required, safe_parse, strict_object, string,
};
use pretty_assertions::assert_eq;
use serde_json::{Map, Value, json};
use utils::*;

fn obj(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        other => panic!("expected object, got {other}"),
    }
}

#[test]
fn is_plain_object_rejects_non_objects() {
    for value in [
        json!(null),
        json!("hello"),
        json!(42),
        json!(true),
        json!([1, 2, 3]),
    ] {
        assert!(!is_plain_object(&value), "{value}");
    }
}

#[test]
fn is_plain_object_accepts_objects() {
    for value in [json!({"a": 1}), json!({}), json!({"a": {"b": 1}})] {
        assert!(is_plain_object(&value));
    }
}

#[test]
fn deep_merge_basic_and_array_cases() {
    let cases = [
        (json!({"a": 1}), json!({"b": 2}), json!({"a": 1, "b": 2})),
        (json!({"a": 1}), json!({"a": 2}), json!({"a": 2})),
        (
            json!({"a": {"b": 1, "c": 2}}),
            json!({"a": {"b": 10}}),
            json!({"a": {"b": 10, "c": 2}}),
        ),
        (
            json!({"a": {"b": {"c": {"d": 1}}}}),
            json!({"a": {"b": {"c": {"e": 2}}}}),
            json!({"a": {"b": {"c": {"d": 1, "e": 2}}}}),
        ),
        (
            json!({"arr": [1, 2]}),
            json!({"arr": [3, 4, 5]}),
            json!({"arr": [3, 4, 5]}),
        ),
        (
            json!({"a": {"arr": [1, 2, 3]}}),
            json!({"a": {"arr": [4]}}),
            json!({"a": {"arr": [4]}}),
        ),
    ];
    for (base, over, expected) in cases {
        let merged = deep_merge(Some(&obj(base)), Some(&obj(over)));
        assert_eq!(merged.map(Value::Object), Some(expected));
    }
}

#[test]
fn deep_merge_edge_cases() {
    assert_eq!(deep_merge(None, None), None);
    assert_eq!(
        deep_merge(None, Some(&obj(json!({"a": 1})))),
        Some(obj(json!({"a": 1})))
    );
    assert_eq!(
        deep_merge(Some(&obj(json!({"a": 1}))), None),
        Some(obj(json!({"a": 1})))
    );
}

#[test]
fn deep_merge_keeps_base_value_for_keys_absent_from_override() {
    // Rust has no `undefined`; an absent key is the equivalent of `{ a: undefined }`.
    let merged = deep_merge(
        Some(&obj(json!({"a": 1, "b": 2}))),
        Some(&obj(json!({"b": 3}))),
    );
    assert_eq!(merged, Some(obj(json!({"a": 1, "b": 3}))));
}

#[test]
fn deep_merge_does_not_mutate_base() {
    let base = obj(json!({"a": 1, "b": {"c": 2}}));
    let snapshot = base.clone();
    let _ = deep_merge(Some(&base), Some(&obj(json!({"b": {"c": 10}}))));
    assert_eq!(base, snapshot);
}

#[test]
fn deep_merge_ignores_prototype_pollution_keys() {
    let over: Map<String, Value> = serde_json::from_str(
        r#"{"__proto__": {"polluted": true}, "constructor": {"polluted": true}, "prototype": {"polluted": true}, "b": 2}"#,
    )
    .unwrap_or_default();
    let merged = deep_merge(Some(&obj(json!({"a": 1}))), Some(&over));
    assert_eq!(merged, Some(obj(json!({"a": 1, "b": 2}))));
}

#[test]
fn deep_merge_returns_override_past_max_depth() {
    fn deep(depth: usize, leaf: Value) -> Value {
        (0..depth).fold(leaf, |inner, _| json!({"nested": inner}))
    }
    let merged = deep_merge(
        Some(&obj(deep(55, json!({"baseKey": "base"})))),
        Some(&obj(deep(55, json!({"overrideKey": "override"})))),
    )
    .map(Value::Object)
    .unwrap_or_default();
    let mut current = &merged;
    for _ in 0..55 {
        current = &current["nested"];
    }
    assert_eq!(current, &json!({"overrideKey": "override"}));
}

#[test]
fn merge_unique_strings_preserves_first_occurrence_order() {
    assert_eq!(
        merge_unique_strings(Some(&["a", "b"][..]), Some(&["b", "c"][..])),
        vec!["a", "b", "c"]
    );
}

#[test]
fn merge_unique_strings_case_insensitive_keeps_first_casing() {
    assert_eq!(
        merge_unique_strings_case_insensitive(
            Some(&["GitHub-Copilot"][..]),
            Some(&["github-copilot", "vercel"][..])
        ),
        vec!["GitHub-Copilot", "vercel"]
    );
}

struct CoreSchema(Node);

impl ConfigSectionParser for CoreSchema {
    fn safe_parse(&self, input: &Value) -> Result<Map<String, Value>, Vec<ConfigParseIssue>> {
        safe_parse(&self.0, input).map(obj).map_err(|issues| {
            issues
                .into_iter()
                .map(|issue| ConfigParseIssue {
                    path: issue.path,
                    message: issue.message,
                })
                .collect()
        })
    }
}

fn section_schema() -> CoreSchema {
    CoreSchema(strict_object(vec![
        optional("nested", strict_object(vec![required("label", string())])),
        optional("names", array(string())),
    ]))
}

#[test]
fn parse_config_sections_keeps_valid_sections_and_reports_invalid() {
    let invalid = RefCell::new(Vec::new());
    let report = |sections: &[String]| invalid.borrow_mut().extend_from_slice(sections);
    let result = parse_config_sections(
        &section_schema(),
        &obj(json!({"nested": {"label": "kept"}, "names": ["valid", 42]})),
        &ParseConfigSectionsOptions {
            on_invalid_sections: Some(&report),
        },
    );
    assert_eq!(result, obj(json!({"nested": {"label": "kept"}})));
    assert_eq!(
        invalid.into_inner(),
        vec!["names: names.1: Invalid input: expected string, received number".to_string()]
    );
}

#[test]
fn parse_config_sections_ignores_unsafe_keys() {
    let raw: Map<String, Value> = serde_json::from_str(
        r#"{"__proto__":{"polluted":true},"constructor":{"polluted":true},"nested":{"label":"kept"}}"#,
    )
    .unwrap_or_default();
    let result = parse_config_sections(
        &section_schema(),
        &raw,
        &ParseConfigSectionsOptions::default(),
    );
    assert_eq!(result, obj(json!({"nested": {"label": "kept"}})));
}

#[test]
fn expand_env_keeps_command_looking_values_inert() {
    let env = HashMap::from([(
        "PAYLOAD".to_string(),
        "$(touch /tmp/omo-should-not-exist)".to_string(),
    )]);
    let allow = |name: &str| name == "PAYLOAD";
    let options = EnvExpansionOptions {
        env: Some(&env),
        is_allowed: Some(&allow),
        ..Default::default()
    };
    assert_eq!(
        expand_env_references("${PAYLOAD}", &options),
        "$(touch /tmp/omo-should-not-exist)"
    );
}

#[test]
fn expand_env_blocked_variable_uses_fallback_and_reports() {
    let env = HashMap::from([("SECRET_TOKEN".to_string(), "secret".to_string())]);
    let blocked = RefCell::new(Vec::new());
    let deny = |_: &str| false;
    let on_blocked = |name: &str, _reason: EnvExpansionBlockedReason| {
        blocked.borrow_mut().push(name.to_string())
    };
    let options = EnvExpansionOptions {
        env: Some(&env),
        trusted: false,
        is_allowed: Some(&deny),
        on_blocked: Some(&on_blocked),
    };
    assert_eq!(
        expand_env_references("${SECRET_TOKEN:-fallback}", &options),
        "fallback"
    );
    assert_eq!(blocked.into_inner(), vec!["SECRET_TOKEN".to_string()]);
}

#[test]
fn expand_env_in_object_recurses() {
    let env = HashMap::from([
        ("HOME".to_string(), "/Users/tester".to_string()),
        ("TOKEN".to_string(), "secret".to_string()),
    ]);
    let allow = |name: &str| name == "HOME";
    let options = EnvExpansionOptions {
        env: Some(&env),
        is_allowed: Some(&allow),
        ..Default::default()
    };
    let result = expand_env_references_in_object(
        &json!({"args": ["--cwd", "${HOME}"], "headers": {"Authorization": "Bearer ${TOKEN:-redacted}"}}),
        &options,
    );
    assert_eq!(
        result,
        json!({"args": ["--cwd", "/Users/tester"], "headers": {"Authorization": "Bearer redacted"}})
    );
}

#[test]
fn expand_env_in_object_drops_unsafe_keys() {
    let input: Value = serde_json::from_str(
        r#"{"__proto__":{"polluted":"${PAYLOAD}"},"constructor":{"polluted":"${PAYLOAD}"},"prototype":{"polluted":"${PAYLOAD}"},"safe":"ok"}"#,
    )
    .unwrap_or_default();
    let env = HashMap::from([("PAYLOAD".to_string(), "EXPANDED".to_string())]);
    let options = EnvExpansionOptions {
        env: Some(&env),
        ..Default::default()
    };
    assert_eq!(
        expand_env_references_in_object(&input, &options),
        json!({"safe": "ok"})
    );
}

#[test]
fn record_guards_differ_on_arrays() {
    assert!(is_record(&json!([])));
    assert!(!is_plain_record(&json!([])));
}

#[test]
fn transform_tool_name_cases() {
    let cases = [
        (" delegate_task", "DelegateTask"),
        ("delegate_task ", "DelegateTask"),
        (" delegate_task ", "DelegateTask"),
        (" webfetch", "WebFetch"),
        (" read ", "Read"),
        ("webfetch", "WebFetch"),
        ("websearch", "WebSearch"),
        ("todoread", "TodoRead"),
        ("todowrite", "TodoWrite"),
        ("delegate_task", "DelegateTask"),
        ("call-omo-agent", "CallOmoAgent"),
        ("read", "Read"),
        ("Write", "Write"),
    ];
    for (input, expected) in cases {
        assert_eq!(transform_tool_name(input), expected, "{input:?}");
    }
}

#[test]
fn replace_tool_args_patches_a_copy() {
    let original = obj(json!({"command": "git status", "timeout": 30}));
    let mut args = original.clone();
    replace_tool_args(&mut args, &obj(json!({"command": "git log"})));
    assert_eq!(args, obj(json!({"command": "git log", "timeout": 30})));
    assert_eq!(original["command"], json!("git status"));
}

#[test]
fn replace_tool_args_patch_cases() {
    let todos = json!([{"content": "Real task 1", "status": "in_progress"}, {"content": "Real task 2", "status": "pending"}]);
    let truncated =
        json!([{"question": "Pick", "options": [{"label": "A very long label that sho..."}]}]);
    let cases = [
        (
            json!({"url": "http://old.com", "format": "text"}),
            json!({"url": "http://new.com", "format": "markdown"}),
            json!({"url": "http://new.com", "format": "markdown"}),
        ),
        (
            json!({"todos": "[]"}),
            json!({"todos": [{"id": "1", "content": "test", "status": "pending"}]}),
            json!({"todos": [{"id": "1", "content": "test", "status": "pending"}]}),
        ),
        (
            json!({"url": "http://old.com", "format": "markdown"}),
            json!({"url": "http://redirected.com"}),
            json!({"url": "http://redirected.com", "format": "markdown"}),
        ),
        (
            json!({"command": "git rebase --continue"}),
            json!({"command": "GIT_EDITOR=: git rebase --continue"}),
            json!({"command": "GIT_EDITOR=: git rebase --continue"}),
        ),
        (
            json!({"prompt": "Do the thing", "category": "quick"}),
            json!({"prompt": "[DIRECTIVE] Do the thing"}),
            json!({"prompt": "[DIRECTIVE] Do the thing", "category": "quick"}),
        ),
        (
            json!({"command": "echo \u{0}hello"}),
            json!({"command": "echo hello"}),
            json!({"command": "echo hello"}),
        ),
        (
            json!({"questions": [{"question": "Pick", "options": [{"label": "A very long label that should be truncated"}]}]}),
            json!({"questions": truncated}),
            json!({"questions": truncated}),
        ),
        (
            json!({"filePath": "/old/path.ts"}),
            json!({"filePath": "/new/path.ts"}),
            json!({"filePath": "/new/path.ts"}),
        ),
        (
            json!({"todos": [{"content": "bootstrap", "status": "pending"}]}),
            json!({"todos": todos}),
            json!({"todos": todos}),
        ),
    ];
    for (start, patch, expected) in cases {
        let mut args = obj(start);
        replace_tool_args(&mut args, &obj(patch));
        assert_eq!(Value::Object(args), expected);
    }
}

#[test]
fn format_duration_human_cases() {
    let cases = [
        (0, "0s"),
        (999, "0s"),
        (1000, "1s"),
        (60_000, "1m 0s"),
        (3_600_000, "1h 0m 0s"),
        (3_723_456, "1h 2m 3s"),
        (86_400_000, "24h 0m 0s"),
    ];
    for (ms, expected) in cases {
        assert_eq!(format_duration_human(ms), expected);
    }
}

#[test]
fn classify_path_environment_cases() {
    let cases = [
        (
            "/Users/x/Library/Mobile Documents/com~apple~CloudDocs/project/file.txt",
            PathClassification::Icloud,
        ),
        ("/Users/x/OneDrive/foo", PathClassification::Onedrive),
        ("C:\\Users\\x\\OneDrive\\foo", PathClassification::Onedrive),
        ("/Users/x/Desktop/foo", PathClassification::DesktopSync),
        (
            "/Volumes/NetworkShare/foo",
            PathClassification::NetworkDrive,
        ),
        ("/tmp/foo", PathClassification::Unknown),
        ("", PathClassification::Unknown),
        ("/Users/x/oNeDrIvE/foo", PathClassification::Onedrive),
    ];
    for (path, expected) in cases {
        assert_eq!(classify_path_environment(path), expected, "{path}");
    }
}

#[test]
fn describe_path_classification_labels() {
    let all = [
        PathClassification::Icloud,
        PathClassification::Onedrive,
        PathClassification::DesktopSync,
        PathClassification::NetworkDrive,
        PathClassification::Unknown,
    ];
    assert_eq!(
        all.map(describe_path_classification),
        [
            "iCloud Drive",
            "OneDrive",
            "Desktop sync (macOS)",
            "Network drive",
            "filesystem that does not support fsync"
        ]
    );
}

fn entry(path: &str, entry_type: ArchiveEntryType, link: Option<&str>) -> ArchiveEntry {
    ArchiveEntry {
        path: path.to_string(),
        entry_type,
        link_path: link.map(str::to_string),
    }
}

fn archive_error(entries: &[ArchiveEntry]) -> String {
    validate_archive_entries(entries, "/tmp/archive-root")
        .err()
        .map(|error| error.message)
        .unwrap_or_default()
        .to_lowercase()
}

#[test]
fn archive_rejects_absolute_and_traversal_paths() {
    assert!(
        archive_error(&[entry("/etc/passwd", ArchiveEntryType::File, None)])
            .contains("absolute path")
    );
    assert!(
        archive_error(&[entry("nested/../../evil.txt", ArchiveEntryType::File, None)])
            .contains("path traversal")
    );
}

#[test]
fn archive_rejects_escaping_symlink_target() {
    let entries = [entry(
        "bin/tool",
        ArchiveEntryType::Symlink,
        Some("../../outside/tool"),
    )];
    assert!(archive_error(&entries).contains("symlink target"));
}

#[test]
fn archive_rejects_escaping_hard_link_target() {
    let entries = [entry(
        "bin/tool",
        ArchiveEntryType::Hardlink,
        Some("../../etc/passwd"),
    )];
    assert!(archive_error(&entries).contains("hard link target"));
}

#[test]
fn archive_accepts_contained_entries() {
    let entries = [
        entry("bin/", ArchiveEntryType::Directory, None),
        entry("bin/tool", ArchiveEntryType::File, None),
        entry("bin/tool-link", ArchiveEntryType::Symlink, Some("tool")),
    ];
    assert_eq!(
        validate_archive_entries(&entries, "/tmp/archive-root"),
        Ok(())
    );
}
