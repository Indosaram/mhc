use maho_ext_permission_system::{storage::*,types::*};
fn rule(action:Action)->Rule { Rule{permission:"bash".into(),pattern:"git *".into(),action} }
#[test]
fn missing_empty(){ let d=tempfile::tempdir().expect("dir"); assert!(load_approved(d.path()).expect("load").is_empty()); }
#[test]
fn roundtrip(){ let d=tempfile::tempdir().expect("dir"); let rules=vec![rule(Action::Allow)]; append_approved(d.path(),&rules).expect("append"); assert_eq!(load_approved(d.path()).expect("load"),rules); }
#[test]
fn malformed_skipped(){ let d=tempfile::tempdir().expect("dir"); append_approved(d.path(),&[rule(Action::Allow)]).expect("append"); let path=d.path().join(".maho/permissions-approved.jsonl"); let mut content=std::fs::read_to_string(&path).expect("read"); content.push_str("bad json\n"); std::fs::write(path,content).expect("write"); assert_eq!(load_approved(d.path()).expect("load"),vec![rule(Action::Allow)]); }
#[test]
fn multiple_appends(){ let d=tempfile::tempdir().expect("dir"); append_approved(d.path(),&[rule(Action::Allow)]).expect("append"); append_approved(d.path(),&[rule(Action::Deny)]).expect("append"); assert_eq!(load_approved(d.path()).expect("load").len(),2); }
#[test]
fn empty_append_no_file(){ let d=tempfile::tempdir().expect("dir"); append_approved(d.path(),&[]).expect("append"); assert!(!d.path().join(".maho").exists()); }
#[test]
fn clear_missing(){ let d=tempfile::tempdir().expect("dir"); clear_approved(d.path()).expect("clear"); }
#[test]
fn compact_last_wins(){ let d=tempfile::tempdir().expect("dir"); append_approved(d.path(),&[rule(Action::Allow),rule(Action::Deny)]).expect("append"); compact_approved(d.path()).expect("compact"); assert_eq!(load_approved(d.path()).expect("load"),vec![rule(Action::Deny)]); }
