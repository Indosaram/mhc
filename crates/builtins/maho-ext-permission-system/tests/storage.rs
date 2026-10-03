use maho_ext_permission_system::{storage::*,types::*};
fn rule(action:Action)->Rule { Rule{permission:"bash".into(),pattern:"git *".into(),action} }
#[test]
fn missing_empty(){ let d=tempfile::tempdir().expect("dir"); assert!(load_approved(d.path()).expect("load").is_empty()); }
#[test]
fn roundtrip(){ let d=tempfile::tempdir().expect("dir"); let rules=vec![rule(Action::Allow)]; append_approved(d.path(),&rules).expect("append"); assert_eq!(load_approved(d.path()).expect("load"),rules); }
#[test]
fn malformed_skipped(){ let d=tempfile::tempdir().expect("dir"); append_approved(d.path(),&[rule(Action::Allow)]).expect("append"); let path=d.path().join(".maho/permissions-approved.jsonl"); let mut content=std::fs::read_to_string(&path).expect("read"); content.push_str("bad json\n"); std::fs::write(path,content).expect("write"); assert_eq!(load_approved(d.path()).expect("load"),vec![rule(Action::Allow)]); }
#[test]
fn multiple_appends(){ let d=tempfile::tempdir().expect("dir"); append_approved(d.path(),&[rule(Action::Allow)]).expect("append"); let other=Rule{permission:"write".into(),pattern:"*.ts".into(),action:Action::Ask}; append_approved(d.path(),std::slice::from_ref(&other)).expect("append"); assert_eq!(load_approved(d.path()).expect("load"),[rule(Action::Allow),other]); }
#[test]
fn empty_append_no_file(){ let d=tempfile::tempdir().expect("dir"); append_approved(d.path(),&[]).expect("append"); assert!(!d.path().join(".maho").exists()); }
#[test]
fn clear_missing(){ let d=tempfile::tempdir().expect("dir"); clear_approved(d.path()).expect("clear"); }
#[test]
fn compact_last_wins(){ let d=tempfile::tempdir().expect("dir"); append_approved(d.path(),&[rule(Action::Allow),rule(Action::Deny)]).expect("append"); compact_approved(d.path()).expect("compact"); assert_eq!(load_approved(d.path()).expect("load"),vec![rule(Action::Deny)]); }

#[test]
fn append_creates_directory_and_preserves_jsonl_order() {
    // Given distinct permission rules and a missing configuration directory.
    let dir = tempfile::tempdir().expect("project");
    let rules = vec![rule(Action::Allow), Rule { permission: "read".into(), pattern: "*.md".into(), action: Action::Allow }];
    // When appending the rules.
    append_approved(dir.path(), &rules).expect("append");
    // Then the directory exists and each serialized line retains its rule.
    assert!(dir.path().join(".maho").is_dir());
    let raw = std::fs::read_to_string(dir.path().join(".maho/permissions-approved.jsonl")).expect("raw JSONL");
    let actual: Vec<Rule> = raw.lines().map(|line| serde_json::from_str(line).expect("rule line")).collect();
    assert_eq!(actual, rules);
}

#[test]
fn clear_removes_existing_permissions_file() {
    // Given persisted approval.
    let dir = tempfile::tempdir().expect("project");
    append_approved(dir.path(), &[rule(Action::Allow)]).expect("append");
    // When clearing approvals.
    clear_approved(dir.path()).expect("clear");
    // Then no permissions file remains.
    assert!(!dir.path().join(".maho/permissions-approved.jsonl").exists());
}

#[test]
fn compact_retains_other_permissions_and_unrelated_rules() {
    // Given duplicates and identical patterns with different permissions.
    let dir = tempfile::tempdir().expect("project");
    let other = Rule { permission: "read".into(), pattern: "git *".into(), action: Action::Ask };
    append_approved(dir.path(), &[rule(Action::Allow), rule(Action::Deny), other.clone()]).expect("append");
    // When compacting.
    compact_approved(dir.path()).expect("compact");
    // Then the last duplicate wins and a different permission remains.
    assert_eq!(load_approved(dir.path()).expect("load"), [rule(Action::Deny), other]);
}

#[test]
fn compact_missing_file_creates_nothing() {
    // Given a fresh project.
    let dir = tempfile::tempdir().expect("project");
    // When compacting absent storage.
    compact_approved(dir.path()).expect("compact missing");
    // Then it remains absent.
    assert!(!dir.path().join(".maho").exists());
}

#[test]
fn malformed_middle_line_preserves_valid_neighbors() {
    // Given valid rules on each side of malformed JSON.
    let dir = tempfile::tempdir().expect("project");
    append_approved(dir.path(), &[rule(Action::Allow)]).expect("append");
    let path = dir.path().join(".maho/permissions-approved.jsonl");
    let mut raw = std::fs::read_to_string(&path).expect("read");
    raw.push_str("invalid json\n{\"permission\":\"write\",\"pattern\":\"*.ts\",\"action\":\"deny\"}\n");
    std::fs::write(&path, raw).expect("fixture");
    // When loading persisted rules.
    let actual = load_approved(dir.path()).expect("load");
    // Then malformed data does not discard either valid neighbor.
    assert_eq!(actual, [rule(Action::Allow), Rule { permission: "write".into(), pattern: "*.ts".into(), action: Action::Deny }]);
}

#[test]
fn roundtrip_preserves_full_mixed_rule_values() {
    // Given all three actions and different permissions/patterns.
    let dir = tempfile::tempdir().expect("project");
    let rules = [rule(Action::Allow), Rule { permission: "write".into(), pattern: "*.ts".into(), action: Action::Ask }, Rule { permission: "read".into(), pattern: "*.md".into(), action: Action::Deny }];
    // When saving and reopening.
    append_approved(dir.path(), &rules).expect("append");
    let actual = load_approved(dir.path()).expect("reopen");
    // Then no field or order is lost.
    assert_eq!(actual, rules);
}
