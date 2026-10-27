#[tokio::test]
async fn removed_app_server_command_exits_before_starting_a_session() {
    for args in [
        vec!["app-server"],
        vec!["app-server", "--help"],
        vec!["app-server", "--listen", "stdio://"],
        vec!["app-server", "--listen", "ws://127.0.0.1:0", "--ws-auth", "off"],
        vec!["app-server", "daemon", "status"],
    ] {
        let dir = tempfile::tempdir().expect("isolated CLI directory");
        let agent_dir = dir.path().join("agent");
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_mhc"));
        command
            .current_dir(dir.path())
            .env("HOME", dir.path())
            .env("USERPROFILE", dir.path())
            .env("MAHO_CODING_AGENT_DIR", &agent_dir)
            .env_remove("__PI_INTERNAL_SPAWN")
            .stdin(std::process::Stdio::null())
            .args(&args)
            .kill_on_drop(true);
        let output = tokio::time::timeout(std::time::Duration::from_secs(15), command.output())
            .await
            .expect("removed command must exit without waiting for protocol input")
            .expect("run real CLI");
        assert_eq!(output.status.code(), Some(1), "{args:?}: {output:?}");
        assert!(output.stdout.is_empty(), "{args:?}: {output:?}");
        let stderr = String::from_utf8(output.stderr).expect("utf8 stderr");
        assert!(stderr.contains("--mode rpc"), "{args:?}: {stderr}");
        assert!(!agent_dir.join("sessions").exists(), "{args:?}");
        assert!(!agent_dir.join("app-server-daemon").exists(), "{args:?}");
    }
}
