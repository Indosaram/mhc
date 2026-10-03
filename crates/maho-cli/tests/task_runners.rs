use std::collections::BTreeMap;

use maho_cli::cli::task_runners::native_rpc_options;
use senpi_task::runners::types::RpcRunnerSpec;

#[test]
fn native_rpc_spawn_preserves_isolation_and_explicit_member_profile() {
    let dir = tempfile::tempdir().expect("isolated task state");
    let executable = dir.path().join("mhc");
    let agent_dir = dir.path().join("agent").to_string_lossy().into_owned();
    let options = native_rpc_options(executable.clone(), &agent_dir, BTreeMap::from([
        ("SENPI_TASK_MEMBER".into(), "stale-parent".into()),
        ("MAHO_CODING_AGENT_SESSION_DIR".into(), "parent-transcript".into()),
    ]), Vec::new());
    let spec = RpcRunnerSpec {
        task_id: "isolated-child".into(), cwd: dir.path().to_string_lossy().into_owned(),
        state_dir: dir.path().join("state").to_string_lossy().into_owned(),
        model: Some("provider/model".into()),
        extensions: Some(vec!["native-team".into()]),
        member_env: Some(BTreeMap::from([("SENPI_TASK_MEMBER".into(), "child-member".into())])),
        ..Default::default()
    };

    let descriptor = options.build_spawn.expect("native spawn builder")(&spec);

    assert_eq!(descriptor.command, executable.to_string_lossy());
    assert_eq!(descriptor.cwd, spec.cwd);
    assert_eq!(descriptor.env["MAHO_CODING_AGENT_DIR"], agent_dir);
    assert_eq!(descriptor.env["SENPI_TASK_MEMBER"], "child-member");
    assert_eq!(descriptor.env["MAHO_CODING_AGENT_SESSION_DIR"], descriptor.env["SENPI_CODING_AGENT_SESSION_DIR"]);
    assert!(std::path::Path::new(&descriptor.env["MAHO_CODING_AGENT_SESSION_DIR"])
        .starts_with(dir.path().join("state/sessions/isolated-child")));
    assert_eq!(descriptor.args, ["--mode", "rpc", "--no-extensions", "--extension", "native-team", "--model", "provider/model"]);
}

#[test]
fn native_rpc_spawn_removes_inherited_member_identity() {
    let dir = tempfile::tempdir().expect("isolated task state");
    let options = native_rpc_options(dir.path().join("mhc"), "native-agent", BTreeMap::from([
        ("SENPI_TASK_MEMBER".into(), "parent-member".into()),
        ("SENPI_TASK_MEMBER_TASK_ID".into(), "parent-task".into()),
        ("SENPI_TASK_TEAM_CONFIG".into(), "parent-config".into()),
    ]), Vec::new());
    let spec = RpcRunnerSpec {
        task_id: "child".into(), cwd: dir.path().to_string_lossy().into_owned(),
        state_dir: dir.path().to_string_lossy().into_owned(), ..Default::default()
    };

    let descriptor = options.build_spawn.expect("native spawn builder")(&spec);

    for name in ["SENPI_TASK_MEMBER", "SENPI_TASK_MEMBER_TASK_ID", "SENPI_TASK_TEAM_CONFIG"] {
        assert!(!descriptor.env.contains_key(name));
    }
}
