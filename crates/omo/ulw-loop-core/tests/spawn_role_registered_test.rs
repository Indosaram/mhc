#![allow(clippy::unwrap_used)]

use std::fs;
use serde_json::{json, Value};
use ulw_loop_core::registered_agent_roles::registered_agent_roles;
use ulw_loop_core::spawn_role_guard::spawn_role_denial;

fn setup() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir_all(root.path().join("project/src/deep")).unwrap();
    root
}

fn role(dir: &std::path::Path, file: &str, name: Option<&str>) {
    fs::create_dir_all(dir).unwrap();
    let header = name.map(|n| format!("name = \"{n}\"\n")).unwrap_or_default();
    fs::write(dir.join(file), format!("{header}description = \"custom role\"\ndeveloper_instructions = \"Do the work.\"\n")).unwrap();
}

fn spawn(root: &tempfile::TempDir, cwd: &std::path::Path, input: Value) -> Option<String> {
    spawn_role_denial(&input, || registered_agent_roles(cwd, Some(root.path().join("codex-home").to_str().unwrap())))
}

fn denial(reason: Option<String>) -> Option<String> {
    reason
}

#[test]
fn each_project_registered_role_spawns_from_nested_directory() {
    let root = setup(); let project = root.path().join("project");
    let agents = project.join(".codex/agents");
    for name in ["cashflow_planner", "debt_planner", "wealth_planner", "finance_reviewer"] { role(&agents, &format!("{name}.toml"), Some(name)); }
    for name in ["cashflow_planner", "debt_planner", "wealth_planner", "finance_reviewer"] {
        assert!(denial(spawn(&root, &project, json!({"agent_type": name}))).is_none());
        assert!(denial(spawn(&root, &project.join("src/deep"), json!({"agent_type": name}))).is_none());
    }
}

#[test]
fn declared_role_name_beats_filename() {
    let root = setup(); let project = root.path().join("project");
    role(&project.join(".codex/agents"), "planner-file.toml", Some("wealth_planner"));
    assert!(denial(spawn(&root, &project, json!({"agent_type":"wealth_planner"}))).is_none());
    assert!(denial(spawn(&root, &project, json!({"agent_type":"planner-file"}))).unwrap().contains("planner-file"));
}

#[test]
fn unnamed_role_uses_filename() {
    let root = setup(); let project = root.path().join("project");
    role(&project.join(".codex/agents"), "cashflow_planner.toml", None);
    assert!(denial(spawn(&root, &project, json!({"agent_type":"cashflow_planner"}))).is_none());
}

#[test]
fn codex_home_standalone_and_config_roles_spawn() {
    let root = setup(); let project = root.path().join("project"); let home = root.path().join("codex-home");
    role(&home.join("agents"), "finance_reviewer.toml", Some("finance_reviewer"));
    fs::write(home.join("config.toml"), "[agents]\nmax_threads = 4\n\n[agents.debt_planner]\nconfig_file = \"./agents/debt.toml\"\n").unwrap();
    assert!(denial(spawn(&root, &project, json!({"agent_type":"finance_reviewer"}))).is_none());
    assert!(denial(spawn(&root, &project, json!({"agent_type":"debt_planner"}))).is_none());
}

#[test]
fn unregistered_role_denial_lists_registered_and_bundled_roles() {
    let root = setup(); let project = root.path().join("project");
    role(&project.join(".codex/agents"), "cashflow_planner.toml", Some("cashflow_planner"));
    let reason = denial(spawn(&root, &project, json!({"agent_type":"worker"}))).unwrap();
    assert!(reason.contains("worker") && reason.contains("cashflow_planner") && reason.contains("lazycodex-worker-medium"));
}

#[test]
fn missing_agent_type_is_denied_with_custom_roles() {
    let root = setup(); let project = root.path().join("project");
    role(&project.join(".codex/agents"), "cashflow_planner.toml", Some("cashflow_planner"));
    assert!(denial(spawn(&root, &project, json!({}))).unwrap().contains("no agent_type"));
}

#[test]
fn role_registered_in_other_project_is_not_visible() {
    let root = setup(); let project = root.path().join("project");
    role(&root.path().join("other-project/.codex/agents"), "cashflow_planner.toml", Some("cashflow_planner"));
    assert!(denial(spawn(&root, &project, json!({"agent_type":"cashflow_planner"}))).unwrap().contains("cashflow_planner"));
}

#[test]
fn bundled_roles_spawn_without_agents_directory() {
    let root = setup(); let project = root.path().join("project");
    assert!(denial(spawn(&root, &project, json!({"agent_type":"lazycodex-worker-medium"}))).is_none());
    assert!(denial(spawn(&root, &project, json!({"agent_type":"explorer"}))).is_none());
}


