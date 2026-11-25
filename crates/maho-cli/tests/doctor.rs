use std::collections::BTreeMap;

use maho_cli::cli::doctor::doctor_lines;

#[test]
fn native_package_manifest_distinguishes_intact_tampered_and_missing_siblings() {
    use sha2::{Digest, Sha256};
    let root = tempfile::tempdir().expect("tempdir");
    let sibling = root.path().join("lsp-daemon");
    let bytes = b"native sibling fixture";
    std::fs::write(&sibling, bytes).expect("sibling");
    let manifest = serde_json::json!({"executables":[{"path":"lsp-daemon","sha256":format!("{:x}", Sha256::digest(bytes))}]});
    let manifest_bytes = serde_json::to_vec(&manifest).expect("manifest");
    let manifest_path = root.path().join("manifest.json");
    std::fs::write(&manifest_path, &manifest_bytes).expect("manifest file");
    let intact = maho_cli::cli::doctor::package_lines(root.path());
    assert_eq!(intact.len(), 1);
    assert!(intact[0].starts_with("PASS "));
    assert_eq!(
        std::fs::read(&manifest_path).expect("unchanged manifest"),
        manifest_bytes
    );
    assert_eq!(std::fs::read(&sibling).expect("unchanged sibling"), bytes);
    std::fs::write(&sibling, b"tampered").expect("tamper fixture");
    assert!(maho_cli::cli::doctor::package_lines(root.path())[0].starts_with("FAIL "));
    std::fs::remove_file(&sibling).expect("remove fixture");
    assert!(maho_cli::cli::doctor::package_lines(root.path())[0].starts_with("FAIL "));
}

#[test]
fn invalid_config_is_reported_without_values() {
    let root = tempfile::tempdir().expect("tempdir");
    let home = root.path().to_string_lossy().into_owned();
    std::fs::create_dir_all(root.path().join(".omo")).expect("mkdir");
    let config_path = root.path().join(".omo").join("omo.jsonc");
    let config = r#"{"task":{"host_engine_policy":"sometimes"}}"#;
    std::fs::write(&config_path, config).expect("write");
    let env = BTreeMap::from([
        (String::from("HOME"), home.clone()),
        (String::from("USERPROFILE"), home.clone()),
    ]);
    let agent_dir = root
        .path()
        .join(".omo")
        .join("agent")
        .to_string_lossy()
        .into_owned();
    let lines = doctor_lines(root.path(), &env, &agent_dir);
    assert!(
        lines.iter().any(
            |line| line.starts_with("WARN config:") && line.contains("task.host_engine_policy")
        )
    );
    assert!(lines.iter().all(|line| !line.contains("sometimes")));
    assert_eq!(
        std::fs::read_to_string(config_path).expect("config unchanged"),
        config
    );
    assert!(
        !root
            .path()
            .join(".omo")
            .join("agent")
            .join("auth.json")
            .exists()
    );
}

#[test]
fn malformed_config_is_reported_as_not_loaded() {
    let root = tempfile::tempdir().expect("tempdir");
    let home = root.path().to_string_lossy().into_owned();
    std::fs::create_dir_all(root.path().join(".omo")).expect("mkdir");
    std::fs::write(root.path().join(".omo").join("omo.jsonc"), r#"{"task":{"#).expect("write");
    let env = BTreeMap::from([
        (String::from("HOME"), home.clone()),
        (String::from("USERPROFILE"), home.clone()),
    ]);
    let agent_dir = root
        .path()
        .join(".omo")
        .join("agent")
        .to_string_lossy()
        .into_owned();
    let lines = doctor_lines(root.path(), &env, &agent_dir);
    assert!(
        lines
            .iter()
            .any(|line| line.contains("not loaded (JSONC parse error)"))
    );
}

#[test]
fn untrusted_project_resources_fail() {
    let root = tempfile::tempdir().expect("tempdir");
    let home = root.path().to_string_lossy().into_owned();
    std::fs::create_dir_all(root.path().join(".omo")).expect("mkdir");
    std::fs::write(root.path().join(".omo").join("settings.json"), "{}").expect("write");
    let env = BTreeMap::from([
        (String::from("HOME"), home.clone()),
        (String::from("USERPROFILE"), home.clone()),
    ]);
    let agent_dir = root
        .path()
        .join(".omo")
        .join("agent")
        .to_string_lossy()
        .into_owned();
    let lines = doctor_lines(root.path(), &env, &agent_dir);
    assert!(
        lines
            .iter()
            .any(|line| line == "WARN project resources: trust decision is not recorded")
    );
}

#[test]
fn missing_trust_store_does_not_create_agent_directory() {
    let root = tempfile::tempdir().expect("tempdir");
    let home = root.path().to_string_lossy().into_owned();
    std::fs::create_dir_all(root.path().join(".omo")).expect("mkdir");
    std::fs::write(root.path().join(".omo").join("settings.json"), "{}").expect("write");
    let env = BTreeMap::from([
        (String::from("HOME"), home.clone()),
        (String::from("USERPROFILE"), home.clone()),
    ]);
    let agent_dir = root
        .path()
        .join(".omo")
        .join("agent")
        .to_string_lossy()
        .into_owned();
    let lines = doctor_lines(root.path(), &env, &agent_dir);
    assert!(
        lines
            .iter()
            .any(|line| line == "WARN project resources: trust decision is not recorded")
    );
    assert!(!root.path().join(".omo").join("agent").exists());
}

#[test]
fn a_stored_anthropic_subscription_login_serves_its_pinned_category() {
    let root = tempfile::tempdir().expect("tempdir");
    let home = root.path().to_string_lossy().into_owned();
    let env = BTreeMap::from([
        (String::from("HOME"), home.clone()),
        (String::from("USERPROFILE"), home.clone()),
    ]);
    let agent = root.path().join(".omo").join("agent");
    std::fs::create_dir_all(&agent).expect("agent dir");
    std::fs::write(
        agent.join("auth.json"),
        r#"{"anthropic-subscription":{"type":"oauth","access":"fixture-access","refresh":"fixture-refresh","expires":4102444800000}}"#,
    )
    .expect("auth fixture");
    std::fs::write(
        root.path().join(".omo").join("omo.jsonc"),
        r#"{"categories":{"quick":{"model":"anthropic-subscription/claude-haiku-5-5"}}}"#,
    )
    .expect("config fixture");

    let lines = doctor_lines(root.path(), &env, &agent.to_string_lossy());

    let summary = lines
        .iter()
        .find(|line| line.contains("task categories:"))
        .expect("category summary");
    assert!(
        summary.starts_with("PASS ") || summary.contains("quick"),
        "{lines:?}"
    );
    assert!(
        !lines.iter().any(|line| line.contains("Category \"quick\"")),
        "{lines:?}"
    );
}

#[test]
fn category_report_is_warning_when_no_models_are_available() {
    let root = tempfile::tempdir().expect("tempdir");
    let home = root.path().to_string_lossy().into_owned();
    let env = BTreeMap::from([
        (String::from("HOME"), home.clone()),
        (String::from("USERPROFILE"), home.clone()),
    ]);
    let agent_dir = root
        .path()
        .join(".omo")
        .join("agent")
        .to_string_lossy()
        .into_owned();
    let lines = doctor_lines(root.path(), &env, &agent_dir);
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("WARN task categories:"))
    );
    assert!(!root.path().join(".omo").join("agent").exists());
}
