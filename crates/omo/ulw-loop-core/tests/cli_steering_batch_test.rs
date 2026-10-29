use std::fs;
use tempfile::tempdir;
use ulw_loop_core::cli_steering::parse_steering_proposals;

fn args(values: &[&str]) -> Vec<String> { values.iter().map(|v| (*v).into()).collect() }

#[test]
fn proposals_json_returns_every_proposal() {
    let proposals = parse_steering_proposals(&args(&["--proposals-json", r#"[{"kind":"annotate_ledger","evidence":"one","rationale":"because one"},{"kind":"annotate_ledger","source":"cli","evidence":"two","rationale":"because two"}]"#])).unwrap();
    assert_eq!(proposals.len(), 2);
    assert_eq!(proposals[0].evidence, "one");
    assert_eq!(proposals[1].evidence, "two");
    assert_eq!(proposals[0].source, "cli");
}

#[test]
fn proposals_json_path_reads_proposals_from_file() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("batch.json");
    fs::write(&file, r#"[{"kind":"annotate_ledger","source":"cli","evidence":"file","rationale":"because file"}]"#).unwrap();
    let proposals = parse_steering_proposals(&args(&["--proposals-json", file.to_str().unwrap()])).unwrap();
    assert_eq!(proposals.len(), 1);
    assert_eq!(proposals[0].evidence, "file");
}

#[test]
fn kind_and_proposals_json_conflict_without_mutation() {
    let error = parse_steering_proposals(&args(&["--kind", "annotate_ledger", "--evidence", "flag", "--rationale", "flag", "--proposals-json", "[]"])).unwrap_err();
    assert_eq!(error.code, "ULW_LOOP_STEERING_BATCH_CONFLICT");
}
