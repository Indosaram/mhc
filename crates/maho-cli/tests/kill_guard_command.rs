//! Port of `packages/coding-agent/test/kill-guard/{check-entry-points,shims-real-judge}.test.ts`.
//! Exercises the compiled `mhc` binary's early `__kill-guard` dispatch and verifies `--help` hides it.

#[cfg(unix)]
mod unix {
    use maho_core::kill_guard::{
        KILL_GUARD_COMMAND, KILL_GUARD_TAG, KillGuardRuntime, install_kill_guard_shims,
        kill_guard_environment,
    };
    use std::collections::BTreeMap;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::process::Command;

    fn write_executable(path: &Path, content: &str) {
        std::fs::write(path, content).expect("write executable");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod executable");
    }

    #[test]
    fn hidden_kill_guard_subcommand_judges_argv_and_leaves_agent_dir_and_home_untouched() {
        let root = tempfile::tempdir().expect("tempdir");
        let home = root.path().join("home");
        let agent_dir = root.path().join("agent");
        let fakes = root.path().join("fakes");
        for dir in [&home, &agent_dir, &fakes] {
            std::fs::create_dir_all(dir).expect("mkdir");
        }
        let pgrep_log = root.path().join("pgrep.log");
        write_executable(
            &fakes.join("ps"),
            "#!/bin/sh\necho '    1     0     1     1     0 init /sbin/init'\n",
        );
        write_executable(
            &fakes.join("pgrep"),
            &format!(
                "#!/bin/sh\necho \"$*\" >> '{}'\nexit 1\n",
                pgrep_log.display()
            ),
        );

        let inherited_path = std::env::var("PATH").unwrap_or_default();
        let test_path = format!("{}:{inherited_path}", fakes.display());
        let self_pid = std::process::id().to_string();

        let run_check = |argv: &[&str]| {
            let mut args = vec![
                KILL_GUARD_COMMAND.to_owned(),
                "check".to_owned(),
                "--vantage".to_owned(),
                self_pid.clone(),
                "--".to_owned(),
            ];
            args.extend(argv.iter().map(|s| (*s).to_owned()));
            Command::new(env!("CARGO_BIN_EXE_mhc"))
                .args(&args)
                .env("PATH", &test_path)
                .env("HOME", &home)
                .env("SENPI_CODING_AGENT_DIR", &agent_dir)
                .env("PI_CODING_AGENT_DIR", &agent_dir)
                .env("OMO_CODING_AGENT_DIR", &agent_dir)
                .env("SENPI_KILL_GUARD_AGENT_PID", &self_pid)
                .env("PI_OFFLINE", "1")
                .output()
                .expect("run mhc __kill-guard")
        };

        let narrow = run_check(&["pkill", "-x", "foo"]);
        assert_eq!(
            narrow.status.code(),
            Some(0),
            "stderr: {}",
            String::from_utf8_lossy(&narrow.stderr)
        );
        let pgrep_calls = std::fs::read_to_string(&pgrep_log).unwrap_or_default();
        assert!(pgrep_calls.contains("-x foo\n"), "pgrep log: {pgrep_calls}");

        let everything = run_check(&["kill", "--", "-1"]);
        assert_eq!(everything.status.code(), Some(126));
        let stderr = String::from_utf8_lossy(&everything.stderr);
        assert!(
            stderr.contains(&format!("{KILL_GUARD_TAG} refused (R3 ALL_PROCESSES)")),
            "stderr: {stderr}"
        );

        assert_eq!(
            std::fs::read_dir(&agent_dir)
                .expect("readdir agent")
                .count(),
            0,
            "agent_dir must stay untouched"
        );
        assert_eq!(
            std::fs::read_dir(&home).expect("readdir home").count(),
            0,
            "home must stay untouched"
        );
    }

    #[test]
    fn help_does_not_list_hidden_kill_guard_command() {
        let root = tempfile::tempdir().expect("tempdir");
        let output = Command::new(env!("CARGO_BIN_EXE_mhc"))
            .arg("--help")
            .env("HOME", root.path())
            .env("SENPI_CODING_AGENT_DIR", root.path().join("agent"))
            .output()
            .expect("run mhc --help");
        assert!(output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(!stdout.contains(KILL_GUARD_COMMAND));
    }

    #[test]
    fn real_judge_behind_shims_refuses_dynamic_broad_and_allows_narrow() {
        let root = tempfile::tempdir().expect("tempdir");
        let real_dir = root.path().join("real");
        std::fs::create_dir_all(&real_dir).expect("mkdir real");
        let real_log = root.path().join("real.log");
        let record = r#"printf '%s' "${0##*/}" >> "$LOG"; for a do printf ' [%s]' "$a" >> "$LOG"; done; printf '\n' >> "$LOG""#;
        for name in ["kill", "pkill", "killall"] {
            write_executable(&real_dir.join(name), &format!("#!/bin/sh\n{record}\n"));
        }

        let shims = install_kill_guard_shims(
            &root.path().join("agent").join("kill-guard"),
            "test-real",
            Some(30),
        )
        .expect("install shims");
        let self_pid = i64::from(std::process::id());
        let rt = KillGuardRuntime {
            dir: shims.clone(),
            bash_env: shims.join("bash_env.sh"),
            exec: env!("CARGO_BIN_EXE_mhc").to_owned(),
            entry: None,
            agent_pid: self_pid,
            protected_pids: vec![1, self_pid],
        };
        let mut base = BTreeMap::new();
        base.insert(
            "PATH".into(),
            format!("{}:/usr/bin:/bin", real_dir.display()),
        );
        base.insert("HOME".into(), root.path().to_string_lossy().into_owned());
        base.insert("LOG".into(), real_log.to_string_lossy().into_owned());
        let mut env = base.clone();
        env.extend(kill_guard_environment(&base, &rt));

        let refused = Command::new("/bin/sh")
            .args(["-c", r#"P='a||b'; sh -c "pkill -f '$P'""#])
            .env_clear()
            .envs(&env)
            .output()
            .expect("run broad pattern");
        assert_eq!(refused.status.code(), Some(126));
        assert!(
            String::from_utf8_lossy(&refused.stderr)
                .contains(&format!("{KILL_GUARD_TAG} refused (R1 BROAD_PATTERN)"))
        );
        assert!(
            std::fs::read_to_string(&real_log)
                .unwrap_or_default()
                .is_empty()
        );

        let allowed = Command::new("/bin/sh")
            .args(["-c", r#"N=senpi-kill-guard-no-such-process; pkill -x "$N""#])
            .env_clear()
            .envs(&env)
            .output()
            .expect("run narrow pattern");
        assert_eq!(
            allowed.status.code(),
            Some(0),
            "stderr: {}",
            String::from_utf8_lossy(&allowed.stderr)
        );
        assert!(!String::from_utf8_lossy(&allowed.stderr).contains(KILL_GUARD_TAG));
        assert_eq!(
            std::fs::read_to_string(&real_log).unwrap_or_default(),
            "pkill [-x] [senpi-kill-guard-no-such-process]\n"
        );
    }
}
