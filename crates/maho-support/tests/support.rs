use maho_support::*;
use serde_json::json;
use std::io;

#[test]
fn jsonc_comments_and_trailing_commas_parse() {
    let parsed = parse_jsonc_lenient("{ // c\n \"a\": [1, 2,], /* b */ \"b\": {\"c\": true,}, }");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    assert_eq!(parsed.value, Some(json!({"a": [1, 2], "b": {"c": true}})));
}

#[test]
fn jsonc_reports_every_missing_value_with_offsets() {
    let text = "{\"a\": , \"b\": }";
    let parsed = parse_jsonc_lenient(text);
    assert_eq!(parsed.errors.len(), 2, "{:?}", parsed.errors);
    assert_eq!(parsed.errors[0].offset, 6);
    assert_eq!(parsed.errors[1].offset, text.len() - 1);
    assert!(parse_jsonc(text).is_err());
}

#[test]
fn jsonc_empty_input_has_no_value_and_no_error() {
    let parsed = parse_jsonc_lenient("  // only a comment\n");
    assert!(parsed.value.is_none());
    assert!(parsed.errors.is_empty());
    assert!(parse_jsonc("").is_err());
}

#[test]
fn jsonc_truncated_document_is_an_error() {
    let parsed = parse_jsonc_lenient("{\"agents\":");
    assert!(!parsed.errors.is_empty());
}

#[test]
fn jsonc_strings_and_numbers_decode() {
    let value =
        parse_jsonc(r#"{"s": "a\nb\u00e9\ud83d\ude00", "n": -1.5e2, "i": 42, "k": "한글"}"#)
            .unwrap();
    assert_eq!(
        value,
        json!({"s": "a\nbé😀", "n": -150.0, "i": 42, "k": "한글"})
    );
}

#[test]
fn read_jsonc_file_missing_or_invalid_is_none() {
    let dir = tempfile::tempdir().unwrap();
    assert!(read_jsonc_file(&dir.path().join("missing.jsonc")).is_none());
    let bad = dir.path().join("bad.jsonc");
    std::fs::write(&bad, "{,}").unwrap();
    assert!(read_jsonc_file(&bad).is_none());
    let good = dir.path().join("good.jsonc");
    std::fs::write(&good, "{\"a\":1,}").unwrap();
    assert_eq!(read_jsonc_file(&good), Some(json!({"a": 1})));
}

fn temp_leftovers(dir: &std::path::Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .expect("read temp dir")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".tmp"))
        .collect()
}

#[test]
fn atomic_write_replaces_target() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("t.json");
    std::fs::write(&target, "old").unwrap();
    write_atomically(&target, b"new").unwrap();
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "new");
    assert!(temp_leftovers(dir.path()).is_empty());
}

#[test]
fn atomic_write_failure_removes_temp_and_keeps_target() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("t.json");
    std::fs::write(&target, "old").unwrap();
    let result = write_atomically_with(&target, b"new", &|step| {
        if step == AtomicStep::Write {
            Err(io::Error::other("disk full"))
        } else {
            Ok(())
        }
    });
    assert!(result.is_err());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "old");
    assert!(temp_leftovers(dir.path()).is_empty());
}

#[test]
fn atomic_write_tolerates_einval_fsync() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("t.json");
    write_atomically_with(&target, b"new", &|step| {
        if step == AtomicStep::FsyncFile {
            Err(io::Error::from(io::ErrorKind::InvalidInput))
        } else {
            Ok(())
        }
    })
    .unwrap();
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "new");
}

#[test]
fn atomic_write_aborts_on_eio_fsync() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("t.json");
    std::fs::write(&target, "old").unwrap();
    let result = write_atomically_with(&target, b"new", &|step| {
        if step == AtomicStep::FsyncFile {
            Err(io::Error::from_raw_os_error(5))
        } else {
            Ok(())
        }
    });
    assert!(result.is_err());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "old");
    assert!(temp_leftovers(dir.path()).is_empty());
}

#[test]
fn write_exclusive_refuses_existing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("lock");
    write_exclusive(&path, b"a").unwrap();
    assert!(write_exclusive(&path, b"b").is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "a");
}

#[cfg(unix)]
#[test]
fn contains_path_rules() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(root.join("sub")).unwrap();
    std::fs::create_dir_all(dir.path().join("rootx")).unwrap();
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&root, &link).unwrap();
    // symlinked root accepted when candidate is under its real path
    assert!(contains_path(&link, &root.join("sub")));
    // non-existent file under root
    assert!(contains_path(&root, &root.join("sub/new/file.txt")));
    // `..` escape
    assert!(!contains_path(&root, &root.join("sub/../../rootx")));
    // sibling with shared prefix
    assert!(!contains_path(&root, &dir.path().join("rootx")));
    assert!(is_within(&root.join("sub"), &root));
    assert!(contains_path(&root, &root));
}

#[test]
fn file_helpers() {
    assert!(is_markdown_file("a.md", true));
    assert!(!is_markdown_file("a.MD", true));
    assert!(!is_markdown_file(".a.md", true));
    assert!(!is_markdown_file("a.md", false));
}

#[cfg(unix)]
#[test]
fn which_safe_rejects_and_finds() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let tool = dir.path().join("mytool");
    std::fs::write(&tool, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
    let plain = dir.path().join("plain");
    std::fs::write(&plain, "x").unwrap();
    let search = format!("::{}", dir.path().display());
    assert_eq!(which_safe_in("mytool", &search), Some(tool));
    assert_eq!(which_safe_in("plain", &search), None);
    for bad in ["", ".", "a/b", "a\\b", "..x", "C:tool"] {
        assert_eq!(which_safe_in(bad, &search), None, "{bad}");
    }
}

#[test]
fn log_hook_is_noop_until_set() {
    log("ignored", None);
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink_seen = seen.clone();
    set_log_sink(Some(std::sync::Arc::new(
        move |message: &str, _data: Option<&serde_json::Value>| {
            sink_seen.lock().unwrap().push(message.to_string());
        },
    )));
    log("hello", Some(&json!({"a": 1})));
    set_log_sink(None);
    log("after", None);
    assert_eq!(*seen.lock().unwrap(), vec!["hello".to_string()]);
}

#[test]
fn small_helpers() {
    let a = vec!["x".to_string(), "Y".to_string()];
    let b = vec!["y".to_string(), "x".to_string(), "z".to_string()];
    assert_eq!(merge_unique(&a, &b), vec!["x", "Y", "y", "z"]);
    assert_eq!(merge_unique_ci(&a, &b), vec!["x", "Y", "z"]);
    assert_eq!(format_duration(-5), "0s");
    assert_eq!(format_duration(3_605_000), "1h 0m 5s");
    assert_eq!(format_duration(65_000), "1m 5s");
    assert_eq!(
        extract_semver("tool v1.2.3-beta.1 (build)"),
        Some("1.2.3-beta.1".into())
    );
    assert_eq!(
        extract_semver("started 00:24:25.202 version 4.5.6+abc"),
        Some("4.5.6+abc".into())
    );
    assert_eq!(extract_semver("   "), None);
    assert_eq!(escape_for_double_quotes("a$b\"c;d"), "a\\$b\\\"c\\;d");
    assert!(find_free_port(20000, None).is_ok());
}
