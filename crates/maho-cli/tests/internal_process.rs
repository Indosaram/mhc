use maho_cli::experimental::process::*;
#[test]
fn explicit_legacy_directory_lane_wins_without_altering_unrelated_env() {
    let mut env = std::collections::BTreeMap::from([("PI_CODING_AGENT_DIR".to_owned(), "chosen".to_owned()), ("MAHO_CODING_AGENT_DIR".to_owned(), "other".to_owned()), ("KEEP".to_owned(), "value".to_owned())]);
    normalize_agent_dir_lane(&mut env);
    assert_eq!(env.len(), 2); assert_eq!(env["PI_CODING_AGENT_DIR"], "chosen");
}
#[test]
fn consumption_validates_role_before_removing_it() {
    let mut env = std::collections::BTreeMap::from([(INTERNAL_PROCESS_ENV.to_owned(), "server".to_owned())]);
    assert!(consume_internal_process_role(&mut env).unwrap() == Some(InternalProcessRole::Server));
    assert!(!env.contains_key(INTERNAL_PROCESS_ENV));
    env.insert(INTERNAL_PROCESS_ENV.to_owned(), "invalid".to_owned());
    assert!(consume_internal_process_role(&mut env).is_err());
    assert!(env.contains_key(INTERNAL_PROCESS_ENV));
}
/// senpi experimental-internal-process.test.ts "waits for a failed activation child to terminate": the spawned internal
/// child is killed (SIGKILL) and reaped before `terminate_internal_process` returns. `spawn_internal_process` launches
/// `current_exe`, the test binary here, so the child runs a probe that parks until it is killed: it can never exit first.
#[cfg(unix)]
#[test]
fn terminate_waits_for_a_spawned_internal_child_to_be_killed() {
    use std::os::unix::process::ExitStatusExt;
    const PROBE: &str = "MAHO_TERMINATE_INTERNAL_PROBE";
    if std::env::var_os(PROBE).is_some() {
        loop { std::thread::park(); }
    }
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut env: std::collections::BTreeMap<String, String> = std::env::vars().collect();
    env.insert(PROBE.into(), "1".into());
    let args = ["--exact".to_owned(), "internal_process::terminate_waits_for_a_spawned_internal_child_to_be_killed".to_owned()];
    runtime.block_on(async {
        let mut child = spawn_internal_process(InternalProcessRole::Coordinator, &args, dir.path(), &env).unwrap();
        assert!(child.id().is_some_and(|pid| pid != std::process::id()));
        terminate_internal_process(&mut child).await.unwrap();
        let status = child.try_wait().unwrap().expect("reaped before terminate returns");
        assert_eq!(status.signal(), Some(9));
    });
}
/// The process-level consume validates first: an invalid inherited role is an error and the variable stays set. Run in
/// an isolated child so the test process environment is never mutated.
#[cfg(unix)]
#[test]
fn process_role_consumption_removes_a_valid_role_and_keeps_an_invalid_one() {
    const PROBE: &str = "MAHO_PROCESS_ROLE_CONSUME_PROBE";
    if let Ok(expected) = std::env::var(PROBE) {
        if expected == "invalid" {
            assert!(consume_process_internal_role().is_err());
            assert_eq!(std::env::var(INTERNAL_PROCESS_ENV).as_deref(), Ok("bogus"));
        } else {
            assert!(get_internal_process_role().unwrap() == Some(InternalProcessRole::Server));
            assert!(consume_process_internal_role().unwrap() == Some(InternalProcessRole::Server));
            assert!(std::env::var_os(INTERNAL_PROCESS_ENV).is_none());
            assert!(consume_process_internal_role().unwrap().is_none());
        }
        return;
    }
    for (probe, role) in [("valid", "server"), ("invalid", "bogus")] {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "internal_process::process_role_consumption_removes_a_valid_role_and_keeps_an_invalid_one", "--nocapture"])
            .env(PROBE, probe).env(INTERNAL_PROCESS_ENV, role).output().unwrap();
        assert!(output.status.success(), "{probe}: {}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    }
}
