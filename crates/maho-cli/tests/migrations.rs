use maho_cli::migrations::run_migrations;
use std::fs;
fn fixture() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
    let root = tempfile::tempdir().expect("tempdir"); let home = root.path().join("home"); let cwd = root.path().join("project"); let agent = home.join(".maho/agent"); fs::create_dir_all(&agent).expect("mkdir"); fs::create_dir_all(&cwd).expect("mkdir"); (root, home, cwd, agent)
}
#[test] fn first_run_moves_sessions_and_records_completed_scans() {
    let (_root, home, cwd, agent) = fixture(); fs::write(agent.join("planted.jsonl"), "{\"type\":\"session\",\"cwd\":\"/tmp/marker-planted-cwd\"}\n").expect("write"); fs::create_dir(cwd.join(".pi")).expect("mkdir"); fs::write(cwd.join(".pi/settings.json"), "{}").expect("write");
    run_migrations(&cwd, &home, &agent).expect("migrate"); assert!(agent.join("sessions/--tmp-marker-planted-cwd--/planted.jsonl").exists()); assert!(cwd.join(".maho/settings.json").exists()); assert!(!cwd.join(".pi").exists()); assert_eq!(maho_cli::migrations_state::read_completed_scan_migrations(&agent).len(), 2);
}
#[test] fn second_run_skips_scans_when_marker_is_present() {
    let (_root, home, cwd, agent) = fixture(); maho_cli::migrations_state::write_completed_scan_migrations(&maho_cli::migrations_state::SCAN_MIGRATIONS, &agent).expect("marker"); fs::write(agent.join("planted.jsonl"), "{\"type\":\"session\",\"cwd\":\"/tmp/example\"}\n").expect("write");
    run_migrations(&cwd, &home, &agent).expect("migrate"); assert!(agent.join("planted.jsonl").exists());
}
#[test] fn malformed_marker_runs_scans_when_schema_is_unknown() {
    let (_root, home, cwd, agent) = fixture(); fs::write(agent.join("migrations-state.json"), "{\"schemaVersion\":999,\"completed\":[]}").expect("marker"); fs::write(agent.join("planted.jsonl"), "{\"type\":\"session\",\"cwd\":\"/tmp/example\"}\n").expect("write");
    run_migrations(&cwd, &home, &agent).expect("migrate"); assert!(!agent.join("planted.jsonl").exists()); assert!(agent.join("sessions/--tmp-example--/planted.jsonl").exists());
}
#[test] fn preserves_existing_files_when_merging_nested_legacy_config() {
    let (_root, home, cwd, agent) = fixture(); fs::write(agent.join("settings.json"), "{}").expect("write"); let old = home.join(".maho/.pi/agent"); fs::create_dir_all(&old).expect("mkdir"); fs::write(old.join("settings.json"), "legacy").expect("write"); fs::write(old.join("models.json"), "{}").expect("write");
    run_migrations(&cwd, &home, &agent).expect("migrate"); assert_eq!(fs::read(agent.join("settings.json")).expect("read"), b"{}"); assert!(agent.join("models.json").exists()); assert!(old.join("settings.json").exists());
}
#[test] fn copies_brand_state_without_overwriting_or_copying_cache() {
    let root = tempfile::tempdir().expect("tempdir"); let old = root.path().join("old"); let new = root.path().join("new"); fs::create_dir_all(old.join("cache")).expect("mkdir"); fs::write(old.join("models.json"), "{}").expect("write");
    let result = maho_cli::brand_dir_migration::migrate_engine_state_to_brand_dir(&old, &new).expect("migrate"); assert!(result.migrated); assert!(old.join("models.json").exists()); assert!(new.join("models.json").exists()); assert!(!new.join("cache").exists());
}

#[cfg(unix)]
#[test]
fn brand_copy_preserves_symbolic_links_instead_of_dereferencing() {
    let root = tempfile::tempdir_in(".").expect("isolated relative tree");
    let old = root.path().join("old");
    let new = root.path().join("new");
    fs::create_dir(&old).expect("legacy directory");
    fs::write(root.path().join("external"), "external fixture").expect("external fixture");
    std::os::unix::fs::symlink("../external", old.join("linked")).expect("relative link");
    maho_cli::brand_dir_migration::migrate_engine_state_to_brand_dir(&old, &new).expect("migration");
    assert!(fs::symlink_metadata(new.join("linked")).expect("destination metadata").file_type().is_symlink());
    assert!(fs::read_link(new.join("linked")).expect("link target").is_absolute());
    assert_eq!(fs::canonicalize(new.join("linked")).expect("target"), fs::canonicalize(root.path().join("external")).expect("source path"));
    assert_eq!(fs::read_to_string(root.path().join("external")).expect("source"), "external fixture");
}
