mod common;

use common::Tree;
use maho_config::writer::{apply_edits, backup_timestamp};
use maho_config::*;
use serde_json::json;

fn parse(text: &str) -> serde_json::Value {
    maho_support::parse_jsonc(text).expect("valid jsonc")
}

#[test]
fn tabs_crlf_and_comments_are_preserved() {
    let t = Tree::new();
    let original = "{\r\n\t\"telemetry\": {\"enabled\": true},\r\n\t/* keep me */\r\n\t\"agents\": {\r\n\t\t\"a\": {\"model\": \"x/y\"}\r\n\t}\r\n}\r\n";
    let path = t.write("home/.omo/omo.jsonc", original);
    let outcome = update_config(
        &[
            Edit::set(&["agents", "new"], json!({"model": "p/q"})),
            Edit::remove(&["telemetry"]),
        ],
        &WriteOptions::new(WriteTarget::User, t.env()),
    )
    .expect("write");
    assert_eq!(outcome.path, path);
    let text = std::fs::read_to_string(&path).expect("read");
    assert!(text.contains("/* keep me */"), "{text:?}");
    assert!(!text.contains("telemetry"));
    assert!(
        !text.replace("\r\n", "").contains('\n'),
        "only CRLF endings: {text:?}"
    );
    assert!(text.contains("\t\t\"new\""), "{text:?}");
    assert!(text.contains("\t\t\"a\": {\"model\": \"x/y\"}"));
    let value = parse(&text);
    assert_eq!(value["agents"]["new"]["model"], json!("p/q"));
    assert_eq!(value["agents"]["a"]["model"], json!("x/y"));
    let backup = outcome.backup.expect("backup");
    assert_eq!(std::fs::read_to_string(backup).expect("backup"), original);
}

#[test]
fn parse_errors_refuse_and_leave_the_file() {
    let t = Tree::new();
    let path = t.write("home/.omo/omo.jsonc", "{\"a\": ,}");
    let error = update_config(
        &[Edit::set(&["x"], json!(1))],
        &WriteOptions::new(WriteTarget::User, t.env()),
    )
    .expect_err("refused");
    assert_eq!(error.op, WriteOp::Parse);
    assert_eq!(std::fs::read_to_string(path).expect("read"), "{\"a\": ,}");
}

#[cfg(unix)]
#[test]
fn symlinked_target_is_refused() {
    let t = Tree::new();
    let real = t.write("real.jsonc", "{}");
    t.mkdir("home/.omo");
    std::os::unix::fs::symlink(&real, t.root.join("home/.omo/omo.jsonc")).expect("symlink");
    assert!(
        update_config(
            &[Edit::set(&["x"], json!(1))],
            &WriteOptions::new(WriteTarget::User, t.env())
        )
        .is_err()
    );
    assert_eq!(std::fs::read_to_string(real).expect("read"), "{}");

    let t = Tree::new();
    t.mkdir("elsewhere");
    t.mkdir("proj");
    std::os::unix::fs::symlink(t.root.join("elsewhere"), t.root.join("proj/.omo"))
        .expect("symlink");
    assert!(
        update_config(
            &[Edit::set(&["x"], json!(1))],
            &WriteOptions::new(WriteTarget::Project(t.root.join("proj")), t.env())
        )
        .is_err()
    );
}

#[test]
fn new_file_is_created_with_a_header() {
    let t = Tree::new();
    let outcome = update_config(
        &[Edit::set(&["telemetry", "enabled"], json!(true))],
        &WriteOptions::new(WriteTarget::Project(t.root.join("proj")), t.env()),
    )
    .expect("write");
    assert!(outcome.path.ends_with("proj/.omo/omo.jsonc"));
    assert_eq!(outcome.backup, None);
    let text = std::fs::read_to_string(&outcome.path).expect("read");
    assert!(text.starts_with("//"));
    assert_eq!(parse(&text), json!({"telemetry": {"enabled": true}}));
}

#[test]
fn existing_json_file_is_the_target() {
    let t = Tree::new();
    let path = t.write("home/.omo/omo.json", "{}");
    let outcome = update_config(
        &[Edit::set(&["a"], json!(1))],
        &WriteOptions::new(WriteTarget::User, t.env()),
    )
    .expect("write");
    assert_eq!(outcome.path, path);
}

#[test]
fn backup_names_avoid_collisions_and_reuse_identical_planned_paths() {
    let t = Tree::new();
    let path = t.write("home/.omo/omo.jsonc", "{\"a\": 1}");
    let now = chrono::DateTime::parse_from_rfc3339("2026-10-27T01:02:03.456Z")
        .expect("time")
        .to_utc();
    let stamp = backup_timestamp(now);
    assert_eq!(stamp, "2026-10-27T01-02-03-456Z");
    let taken = t.root.join(format!("home/.omo/omo.jsonc.bak.{stamp}"));
    std::fs::write(&taken, "other").expect("write");
    let mut options = WriteOptions::new(WriteTarget::User, t.env());
    options.now = Some(now);
    let outcome = update_config(&[Edit::set(&["a"], json!(2))], &options).expect("write");
    assert_eq!(
        outcome.backup,
        Some(t.root.join(format!("home/.omo/omo.jsonc.bak.{stamp}.1")))
    );

    let planned = t.root.join("planned.bak");
    std::fs::write(&planned, std::fs::read(&path).expect("read")).expect("write");
    let mut options = WriteOptions::new(WriteTarget::Path(path.clone()), t.env());
    options.planned_backup = Some(planned.clone());
    let outcome = update_config(&[Edit::set(&["a"], json!(3))], &options).expect("write");
    assert_eq!(outcome.backup, Some(planned));
}

#[test]
fn edits_keep_layout() {
    let text = "{\n  // lead\n  \"a\": 1, // trailing\n  \"b\": {\n    \"c\": true\n  }\n}\n";
    let out = apply_edits(text, &[Edit::set(&["b", "d"], json!([1, 2]))]).expect("edit");
    assert_eq!(
        out,
        "{\n  // lead\n  \"a\": 1, // trailing\n  \"b\": {\n    \"c\": true,\n    \"d\": [\n      1,\n      2\n    ]\n  }\n}\n"
    );
    let out = apply_edits(text, &[Edit::remove(&["a"])]).expect("edit");
    assert_eq!(out, "{\n  // lead\n  \"b\": {\n    \"c\": true\n  }\n}\n");
    let out = apply_edits(text, &[Edit::remove(&["b"])]).expect("edit");
    assert_eq!(parse(&out), json!({"a": 1}));
    assert!(out.contains("// trailing"));
    let out = apply_edits(text, &[Edit::set(&["a"], json!("x"))]).expect("edit");
    assert!(out.contains("\"a\": \"x\", // trailing"));
    let out = apply_edits("{}", &[Edit::set(&["z", "y"], json!(1))]).expect("edit");
    assert_eq!(parse(&out), json!({"z": {"y": 1}}));
}

#[test]
fn trailing_commas_stay() {
    let text = "{\n  \"a\": 1,\n}\n";
    let out = apply_edits(text, &[Edit::set(&["b"], json!(2))]).expect("edit");
    assert_eq!(out, "{\n  \"a\": 1,\n  \"b\": 2,\n}\n");
}
