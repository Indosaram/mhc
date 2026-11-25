//! Port of `packages/coding-agent/test/kill-guard/{check-command,shims-install,shims-path,shims-bash-env,shims-interactive-bash}.test.ts`
//! and `test/suite/regressions/5410-kill-guard-sleeper-exit.test.ts`.
//! No fixed sleeps or polling loops: child processes are waited on synchronously.

use maho_core::kill_guard::{
    KILL_GUARD_COMMAND, KILL_GUARD_SHIM_MARKER, KILL_GUARD_TAG, KillGuardCheckOptions,
    KillGuardContext, KillGuardRuntime, KillGuardShell, KillGuardVantage, ProcessRow, ProcessView,
    SelectResult, install_kill_guard_shims, kill_guard_environment, kill_guard_prompt_command,
    render_kill_guard_shims, run_kill_guard_check,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;

const AGENT: i64 = 500;
const PROTECTED: i64 = 4747;
const SYSTEM_PATH: &str = "/usr/bin:/bin";

fn synthetic_context(selected: Option<Vec<i64>>) -> KillGuardContext {
    let rows = vec![
        ProcessRow {
            pid: 1,
            ppid: 0,
            pgid: 1,
            sid: 1,
            uid: 0,
            comm: "launchd".into(),
            args: "/sbin/launchd".into(),
        },
        ProcessRow {
            pid: 300,
            ppid: 1,
            pgid: 300,
            sid: 300,
            uid: 501,
            comm: "tmux".into(),
            args: "tmux new -s work".into(),
        },
        ProcessRow {
            pid: 400,
            ppid: 300,
            pgid: 400,
            sid: 400,
            uid: 501,
            comm: "zsh".into(),
            args: "-zsh".into(),
        },
        ProcessRow {
            pid: AGENT,
            ppid: 400,
            pgid: AGENT,
            sid: 400,
            uid: 501,
            comm: "bun".into(),
            args: "bun senpi".into(),
        },
        ProcessRow {
            pid: 600,
            ppid: 300,
            pgid: 600,
            sid: 600,
            uid: 501,
            comm: "node".into(),
            args: "node vite --port 5173".into(),
        },
        ProcessRow {
            pid: 700,
            ppid: 1,
            pgid: 700,
            sid: 700,
            uid: 501,
            comm: "redis-server".into(),
            args: "redis-server *:6379".into(),
        },
        ProcessRow {
            pid: 800,
            ppid: AGENT,
            pgid: 800,
            sid: 800,
            uid: 501,
            comm: "sleep".into(),
            args: "sleep 30".into(),
        },
        ProcessRow {
            pid: 4001,
            ppid: 4000,
            pgid: 4000,
            sid: 4000,
            uid: 501,
            comm: "worker".into(),
            args: "omo-worker-2".into(),
        },
    ];
    KillGuardContext {
        platform: "linux".into(),
        agent_pid: AGENT,
        agent_ppid: Some(400),
        vantage: KillGuardVantage::FreshSession,
        shell: KillGuardShell::Bash,
        view: Arc::new(move || {
            let rows = rows.clone();
            let picked = selected.clone().unwrap_or_default();
            Some(ProcessView {
                rows,
                select: Arc::new(move |_| SelectResult::Ok {
                    pids: picked.clone(),
                }),
            })
        }),
    }
}

fn check(
    argv: &[&str],
    vantage: Option<i64>,
    selected: Option<Vec<i64>>,
    agent_pid: Option<&str>,
) -> (i32, String) {
    let mut args = vec![
        KILL_GUARD_COMMAND.to_owned(),
        "check".to_owned(),
        "--vantage".to_owned(),
        vantage.unwrap_or(800).to_string(),
        "--".to_owned(),
    ];
    args.extend(argv.iter().map(|s| (*s).to_owned()));

    let mut env = BTreeMap::new();
    env.insert(
        "SENPI_KILL_GUARD_AGENT_PID".to_owned(),
        agent_pid.unwrap_or(&AGENT.to_string()).to_owned(),
    );
    let mut stderr = String::new();
    let view = synthetic_context(selected).view;
    let status = {
        let mut write_err = |text: &str| stderr.push_str(text);
        run_kill_guard_check(
            &args,
            &mut KillGuardCheckOptions {
                env: &env,
                write_error: &mut write_err,
                platform: Some("linux".into()),
                view: Some(view),
            },
        )
    };
    (status, stderr)
}

#[test]
fn check_command_decides_expanded_argv_and_fails_closed_on_malformed() {
    assert_eq!(
        check(
            &["pkill", "-f", "vite --port 5173"],
            None,
            Some(vec![600]),
            None
        ),
        (0, String::new())
    );

    let (status, stderr) = check(
        &["pkill", "-f", r#"paid" 2>/dev/null || echo 1"#],
        None,
        None,
        None,
    );
    assert_eq!(status, 126);
    assert!(stderr.starts_with(&format!("{KILL_GUARD_TAG} refused (R1 BROAD_PATTERN)")));

    assert_eq!(check(&["kill", "600", "700"], None, None, None).0, 0);
    assert_eq!(check(&["kill", "-TERM", "4001"], None, None, None).0, 0);

    let (_, stderr) = check(&["kill", "400"], None, None, None);
    assert!(stderr.contains("(R2 PROTECTED_TARGET)"));
    assert_eq!(
        check(&["kill", "-9", &AGENT.to_string()], None, None, None).0,
        126
    );

    let (status, stderr) = check(
        &["pkill", "-u", "501"],
        None,
        Some(vec![300, 400, AGENT, 600]),
        None,
    );
    assert_eq!(status, 126);
    assert!(stderr.contains("(R2 PROTECTED_TARGET)"));

    let (_, stderr) = check(&["kill", "0"], Some(AGENT), None, None);
    assert!(stderr.contains("(R4 GROUP)"));
    assert_eq!(
        check(&["kill", "0"], Some(800), None, None),
        (0, String::new())
    );

    let (_, stderr) = check(&["kill", "--", "-1"], None, None, None);
    assert!(stderr.contains("(R3 ALL_PROCESSES)"));

    for bad_args in [
        vec![KILL_GUARD_COMMAND, "check", "--", "kill", "1"],
        vec![
            KILL_GUARD_COMMAND,
            "check",
            "--vantage",
            "x",
            "--",
            "kill",
            "1",
        ],
        vec![KILL_GUARD_COMMAND, "check", "--vantage", "800", "--"],
        vec![
            KILL_GUARD_COMMAND,
            "judge",
            "--vantage",
            "800",
            "--",
            "kill",
            "1",
        ],
    ] {
        let owned: Vec<String> = bad_args.into_iter().map(str::to_owned).collect();
        let mut env = BTreeMap::new();
        env.insert("SENPI_KILL_GUARD_AGENT_PID".into(), AGENT.to_string());
        let mut stderr = String::new();
        let status = {
            let mut write_err = |text: &str| stderr.push_str(text);
            run_kill_guard_check(
                &owned,
                &mut KillGuardCheckOptions {
                    env: &env,
                    write_error: &mut write_err,
                    platform: Some("linux".into()),
                    view: None,
                },
            )
        };
        assert_eq!(status, 2);
        assert!(stderr.contains(KILL_GUARD_COMMAND));
    }

    assert_eq!(check(&["kill", "4001"], None, None, Some("")).0, 2);
}

#[cfg(unix)]
mod unix_shims {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    const RECORD: &str = r#"printf '%s' "${0##*/}" >> "$LOG"; for a do printf ' [%s]' "$a" >> "$LOG"; done; printf '\n' >> "$LOG""#;
    const SHELL_KILL_RECORDER: &str = concat!(
        r#"__kg_record() { printf '%s' "$1" >> "$LOG"; shift; for a do printf ' [%s]' "$a" >> "$LOG"; done; printf '\n' >> "$LOG"; }"#,
        "\n",
        r#"__kg_shadow() { command() { if [ "${1-}" = kill ] && [ "${FUNCNAME[1]-}" = kill ]; then __kg_record builtin "$@"; else __kg_unshadowed command "$@"; fi; }; builtin() { if [ "${1-}" = kill ]; then __kg_record builtin-bypass "$@"; else __kg_unshadowed builtin "$@"; fi; }; }"#,
        "\n",
        r#"__kg_unshadowed() { local __kg_s=0; unset -f command builtin; "$@" || __kg_s=$?; __kg_shadow; return "$__kg_s"; }"#,
        "\n",
        "__kg_shadow",
    );

    fn write_executable(path: &Path, content: &str) {
        std::fs::write(path, content).expect("write executable");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod executable");
    }

    struct OwnedSleeper {
        pid: i64,
        child: Child,
    }

    impl OwnedSleeper {
        fn spawn() -> Self {
            // Try `setsid sleep 60` so sleeper sits in its own session and process group;
            // fall back to `sleep 60` if setsid isn't present.
            let child = Command::new("setsid")
                .args(["sleep", "60"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .or_else(|_| {
                    Command::new("sleep")
                        .arg("60")
                        .stdin(Stdio::null())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .spawn()
                })
                .expect("spawn sleeper");
            let pid = i64::from(child.id());
            Self { pid, child }
        }
    }

    impl Drop for OwnedSleeper {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    struct RunResult {
        status: Option<i32>,
        stdout: String,
        stderr: String,
        real: String,
        judge: String,
    }

    struct ShimFixture {
        _temp: tempfile::TempDir,
        root: PathBuf,
        real_dir: PathBuf,
        shims: PathBuf,
        fake_judge: PathBuf,
        real_log: PathBuf,
        judge_log: PathBuf,
    }

    impl ShimFixture {
        fn new(version: &str, deadline_seconds: Option<u64>) -> Self {
            let temp = tempfile::tempdir().expect("tempdir");
            let root = temp.path().to_path_buf();
            let real_dir = root.join("real");
            std::fs::create_dir_all(&real_dir).expect("mkdir real");
            let real_log = root.join("real.log");
            let judge_log = root.join("judge.log");
            for name in ["kill", "pkill", "killall"] {
                write_executable(&real_dir.join(name), &format!("#!/bin/sh\n{RECORD}\n"));
            }
            let fake_judge = root.join("fake-judge");
            write_executable(
                &fake_judge,
                &format!(
                    "#!/bin/sh\nLOG=\"$JUDGE_LOG\"\n{RECORD}\nprintf 'env ELECTRON_RUN_AS_NODE=%s BUN_BE_BUN=%s\\n' \"${{ELECTRON_RUN_AS_NODE-unset}}\" \"${{BUN_BE_BUN-unset}}\" >> \"$LOG\"\ncase \"$FAKE_JUDGE_MODE\" in\nallow) exit 0 ;;\nrefuse) echo \"{KILL_GUARD_TAG} refused (fake judge)\" >&2; exit 126 ;;\nfail) exit 3 ;;\nhang) exec sleep 30 ;;\nesac\nexit 9\n"
                ),
            );
            let shims = install_kill_guard_shims(
                &root.join("agent").join("kill-guard"),
                version,
                deadline_seconds,
            )
            .expect("install shims");
            Self {
                _temp: temp,
                root,
                real_dir,
                shims,
                fake_judge,
                real_log,
                judge_log,
            }
        }

        fn runtime(&self) -> KillGuardRuntime {
            KillGuardRuntime {
                dir: self.shims.clone(),
                bash_env: self.shims.join("bash_env.sh"),
                exec: self.fake_judge.to_string_lossy().into_owned(),
                entry: None,
                agent_pid: 99_999_999,
                protected_pids: vec![1, PROTECTED],
            }
        }

        fn env_with(
            &self,
            extra: &[(&str, &str)],
            runtime: Option<KillGuardRuntime>,
            path_override: Option<&str>,
        ) -> BTreeMap<String, String> {
            let path = path_override
                .map(str::to_owned)
                .unwrap_or_else(|| format!("{}:{SYSTEM_PATH}", self.real_dir.display()));
            let mut base = BTreeMap::new();
            base.insert("PATH".into(), path);
            base.insert("HOME".into(), self.root.to_string_lossy().into_owned());
            base.insert("LOG".into(), self.real_log.to_string_lossy().into_owned());
            base.insert(
                "JUDGE_LOG".into(),
                self.judge_log.to_string_lossy().into_owned(),
            );
            base.insert("FAKE_JUDGE_MODE".into(), "allow".into());
            for (k, v) in extra {
                base.insert((*k).to_owned(), (*v).to_owned());
            }
            let overlay = kill_guard_environment(&base, &runtime.unwrap_or_else(|| self.runtime()));
            base.extend(overlay);
            base
        }

        fn run(&self, shell: &str, script: &str, env: &BTreeMap<String, String>) -> RunResult {
            let _ = std::fs::remove_file(&self.real_log);
            let _ = std::fs::remove_file(&self.judge_log);
            let output = Command::new(shell)
                .args(["-c", script])
                .env_clear()
                .envs(env)
                .stdin(Stdio::null())
                .output()
                .expect("run shell");
            RunResult {
                status: output.status.code(),
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                real: std::fs::read_to_string(&self.real_log).unwrap_or_default(),
                judge: std::fs::read_to_string(&self.judge_log).unwrap_or_default(),
            }
        }

        fn run_interactive(
            &self,
            args: &[&str],
            input: &str,
            env: &BTreeMap<String, String>,
        ) -> RunResult {
            let _ = std::fs::remove_file(&self.real_log);
            let _ = std::fs::remove_file(&self.judge_log);
            // Detach controlling terminal with `setsid --wait` when available so `bash -i`
            // reading piped stdin does not receive SIGTTIN when the test runner holds a TTY.
            let mut cmd = if Command::new("setsid")
                .args(["--wait", "true"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
            {
                let mut c = Command::new("setsid");
                c.arg("--wait").arg("bash");
                c
            } else {
                Command::new("bash")
            };
            let mut child = cmd
                .args(args)
                .env_clear()
                .envs(env)
                .env("BASH_SILENCE_DEPRECATION_WARNING", "1")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("spawn interactive bash");
            if let Some(mut stdin) = child.stdin.take() {
                use std::io::Write;
                stdin.write_all(input.as_bytes()).expect("write stdin");
            }
            let output = child.wait_with_output().expect("wait interactive bash");
            RunResult {
                status: output.status.code(),
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                real: std::fs::read_to_string(&self.real_log).unwrap_or_default(),
                judge: std::fs::read_to_string(&self.judge_log).unwrap_or_default(),
            }
        }
    }

    fn bash_available() -> bool {
        Command::new("bash")
            .args(["-c", "true"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    #[test]
    fn install_shims_atomic_reuse_stale_replace_and_environment() {
        let f = ShimFixture::new("test-fast", Some(1));
        let mut base = BTreeMap::new();
        base.insert("PATH".into(), "/usr/bin:/bin".into());

        let mut rt_with_entry = f.runtime();
        rt_with_entry.entry = Some("/opt/senpi/kill-guard-check.js".into());
        let entries = kill_guard_environment(&base, &rt_with_entry);
        assert_eq!(
            entries.get("PATH").map(String::as_str),
            Some(format!("{}:/usr/bin:/bin", f.shims.display()).as_str())
        );
        assert_eq!(
            entries.get("BASH_ENV").map(String::as_str),
            Some(f.shims.join("bash_env.sh").to_string_lossy().as_ref())
        );
        assert_eq!(
            entries
                .get("SENPI_KILL_GUARD_USER_BASH_ENV")
                .map(String::as_str),
            Some("")
        );
        assert_eq!(
            entries.get("PROMPT_COMMAND").map(String::as_str),
            Some(kill_guard_prompt_command(&f.shims.to_string_lossy()).as_str())
        );
        assert_eq!(
            entries
                .get("SENPI_KILL_GUARD_USER_PROMPT_COMMAND")
                .map(String::as_str),
            Some("")
        );
        assert_eq!(
            entries
                .get("SENPI_KILL_GUARD_PROTECTED")
                .map(String::as_str),
            Some("1,4747")
        );
        assert_eq!(
            entries.get("SENPI_KILL_GUARD_EXEC").map(String::as_str),
            Some(f.fake_judge.to_string_lossy().as_ref())
        );
        assert_eq!(
            entries
                .get("SENPI_KILL_GUARD_EXEC_ENTRY")
                .map(String::as_str),
            Some("/opt/senpi/kill-guard-check.js")
        );
        assert_eq!(
            entries
                .get("SENPI_KILL_GUARD_AGENT_PID")
                .map(String::as_str),
            Some("99999999")
        );
        assert_eq!(entries.get("SHLVL").map(String::as_str), Some("1"));

        let mut dup_path = BTreeMap::new();
        dup_path.insert(
            "PATH".into(),
            format!("/usr/bin:{}:/bin", f.shims.display()),
        );
        let again = kill_guard_environment(&dup_path, &f.runtime());
        assert_eq!(
            again.get("PATH").map(String::as_str),
            Some(format!("{}:/usr/bin:/bin", f.shims.display()).as_str())
        );

        for (level, expected) in [
            ("2", None),
            ("998", None),
            ("0", Some("1")),
            ("", Some("1")),
            ("-3", Some("1")),
            ("x", Some("1")),
            ("999", Some("1")),
        ] {
            let mut env = BTreeMap::new();
            env.insert("PATH".into(), "/usr/bin:/bin".into());
            env.insert("SHLVL".into(), level.into());
            assert_eq!(
                kill_guard_environment(&env, &f.runtime())
                    .get("SHLVL")
                    .map(String::as_str),
                expected
            );
        }

        // Chaining BASH_ENV & PROMPT_COMMAND
        let mut user_env = BTreeMap::new();
        user_env.insert("PATH".into(), "/usr/bin:/bin".into());
        user_env.insert("BASH_ENV".into(), "/home/u/.bash_env".into());
        let user_out = kill_guard_environment(&user_env, &f.runtime());
        assert_eq!(
            user_out
                .get("SENPI_KILL_GUARD_USER_BASH_ENV")
                .map(String::as_str),
            Some("/home/u/.bash_env")
        );
        let nested_out = kill_guard_environment(&user_out, &f.runtime());
        assert_eq!(
            nested_out
                .get("SENPI_KILL_GUARD_USER_BASH_ENV")
                .map(String::as_str),
            Some("/home/u/.bash_env")
        );

        let mut user_prompt = BTreeMap::new();
        user_prompt.insert("PATH".into(), "/usr/bin:/bin".into());
        user_prompt.insert("PROMPT_COMMAND".into(), "history -a".into());
        let prompt_out = kill_guard_environment(&user_prompt, &f.runtime());
        assert_eq!(
            prompt_out
                .get("SENPI_KILL_GUARD_USER_PROMPT_COMMAND")
                .map(String::as_str),
            Some("history -a")
        );
        let mut inner_rt = f.runtime();
        inner_rt.dir = f.root.join("inner-agent");
        let nested_prompt = kill_guard_environment(&prompt_out, &inner_rt);
        assert_eq!(
            nested_prompt
                .get("SENPI_KILL_GUARD_USER_PROMPT_COMMAND")
                .map(String::as_str),
            Some("history -a")
        );
        assert_eq!(
            nested_prompt.get("PROMPT_COMMAND").map(String::as_str),
            Some(kill_guard_prompt_command(&inner_rt.dir.to_string_lossy()).as_str())
        );

        // Atomic install, reuse, and stale replacement
        let shim_root = f.root.join("install-reuse");
        let dir = install_kill_guard_shims(&shim_root, "2026.11.01-1", None).expect("install");
        assert_eq!(dir, shim_root.join("2026.11.01-1"));
        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .expect("readdir")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                KILL_GUARD_SHIM_MARKER,
                "bash_env.sh",
                "bash_kill.sh",
                "kill",
                "killall",
                "pkill",
            ]
        );
        for name in ["kill", "pkill", "killall"] {
            let mode = std::fs::metadata(dir.join(name))
                .expect("stat")
                .permissions()
                .mode();
            assert_ne!(mode & 0o111, 0);
        }

        let stale_root = f.root.join("install-stale");
        let stale_dir = install_kill_guard_shims(&stale_root, "dev", None).expect("install");
        std::fs::write(stale_dir.join("pkill"), "#!/bin/sh\nexit 0\n").expect("corrupt");
        let repaired = install_kill_guard_shims(&stale_root, "dev", None).expect("repair");
        let expected_files = render_kill_guard_shims(&repaired.to_string_lossy(), None);
        assert_eq!(
            std::fs::read_to_string(repaired.join("pkill")).expect("read repaired"),
            expected_files.pkill
        );

        let files = render_kill_guard_shims("/agent/kill-guard/v1", None);
        for content in [&files.kill, &files.pkill, &files.killall] {
            assert!(content.starts_with("#!/bin/sh\n"));
            assert!(content.contains("__senpi_kg_dir='/agent/kill-guard/v1'"));
        }
    }

    #[test]
    fn path_shims_judge_expanded_argv_and_fail_closed() {
        let f = ShimFixture::new("test-fast", Some(1));

        // Allowed dynamic pattern (C10)
        let r = f.run(
            "/bin/sh",
            r#"P='vite --port 5173'; pkill -f "$P""#,
            &f.env_with(&[], None, None),
        );
        assert_eq!(r.status, Some(0));
        assert!(r.judge.contains("[__kill-guard] [check] [--vantage]"));
        assert!(r.judge.contains("[--] [pkill] [-f] [vite --port 5173]\n"));
        assert_eq!(r.real, "pkill [-f] [vite --port 5173]\n");

        // Refused broad pattern (C30)
        let r = f.run(
            "/bin/sh",
            r#"P='a||b'; sh -c "pkill -f '$P'""#,
            &f.env_with(&[("FAKE_JUDGE_MODE", "refuse")], None, None),
        );
        assert_eq!(r.status, Some(126));
        assert!(r.judge.contains("[--] [pkill] [-f] [a||b]\n"));
        assert!(r.stderr.contains(KILL_GUARD_TAG));
        assert!(r.real.is_empty());

        for cmd in ["killall -u someone", "pkill -x node"] {
            let r = f.run(
                "/bin/sh",
                cmd,
                &f.env_with(&[("FAKE_JUDGE_MODE", "refuse")], None, None),
            );
            assert_eq!(r.status, Some(126));
            let bracketed = cmd.split(' ').collect::<Vec<_>>().join("] [");
            assert!(r.judge.contains(&format!("[--] [{bracketed}]\n")));
            assert!(r.real.is_empty());
        }

        // Judge fail / hang -> R5 UNKNOWN
        for mode in ["fail", "hang"] {
            let r = f.run(
                "/bin/sh",
                "pkill -f 'vite --port 5173'",
                &f.env_with(&[("FAKE_JUDGE_MODE", mode)], None, None),
            );
            assert_eq!(r.status, Some(126), "mode {mode} failed to refuse");
            assert!(
                r.stderr
                    .contains(&format!("{KILL_GUARD_TAG} refused (R5 UNKNOWN)"))
            );
            assert!(r.real.is_empty());
        }

        // Missing judge runs real binary with one notice for non-protected target
        let mut missing_rt = f.runtime();
        missing_rt.exec = f
            .root
            .join("uninstalled-senpi")
            .to_string_lossy()
            .into_owned();
        let r = f.run(
            "/bin/sh",
            "pkill -f 'vite --port 5173'",
            &f.env_with(&[], Some(missing_rt.clone()), None),
        );
        assert_eq!(r.status, Some(0));
        assert_eq!(r.real, "pkill [-f] [vite --port 5173]\n");
        assert_eq!(
            r.stderr
                .lines()
                .filter(|l| l.contains(KILL_GUARD_TAG))
                .count(),
            1
        );
        assert!(r.stderr.contains("not installed"));

        // Judge entry script configured
        let mut entry_rt = f.runtime();
        entry_rt.exec = "/bin/sh".into();
        entry_rt.entry = Some(f.fake_judge.to_string_lossy().into_owned());
        let r = f.run(
            "/bin/sh",
            "pkill -x node",
            &f.env_with(&[], Some(entry_rt), None),
        );
        assert_eq!(r.status, Some(0));
        assert!(r.judge.contains("[--] [pkill] [-x] [node]\n"));

        // Clears BUN_BE_BUN and sets ELECTRON_RUN_AS_NODE=1
        let r = f.run(
            "/bin/sh",
            "pkill -x node",
            &f.env_with(&[("BUN_BE_BUN", "1")], None, None),
        );
        assert_eq!(r.status, Some(0));
        assert!(
            r.judge
                .contains("env ELECTRON_RUN_AS_NODE=1 BUN_BE_BUN=unset\n")
        );

        // sh fast path for positive pids
        let r = f.run(
            "/bin/sh",
            "env kill -TERM 4242 4343",
            &f.env_with(&[], None, None),
        );
        assert_eq!(r.status, Some(0));
        assert!(r.judge.is_empty());
        assert_eq!(r.real, "kill [-TERM] [4242] [4343]\n");

        // R3 fast-path refusals in sh
        for cmd in [
            "kill -1",
            "kill -9 -1",
            "kill -- -1",
            "kill -s KILL -1",
            "kill -TERM 12 -1",
        ] {
            let r = f.run(
                "/bin/sh",
                &format!("env {cmd}"),
                &f.env_with(&[], None, None),
            );
            assert_eq!(r.status, Some(126));
            assert!(
                r.stderr
                    .contains(&format!("{KILL_GUARD_TAG} refused (R3 ALL_PROCESSES)"))
            );
            assert!(r.judge.is_empty());
            assert!(r.real.is_empty());
        }

        // Protected pid with judge vs missing judge
        let r = f.run(
            "/bin/sh",
            &format!("env kill {PROTECTED}"),
            &f.env_with(&[("FAKE_JUDGE_MODE", "refuse")], None, None),
        );
        assert_eq!(r.status, Some(126));
        assert!(r.judge.contains(&format!("[--] [kill] [{PROTECTED}]\n")));
        assert!(r.real.is_empty());

        let r = f.run(
            "/bin/sh",
            &format!("env kill {PROTECTED}"),
            &f.env_with(&[], Some(missing_rt), None),
        );
        assert_eq!(r.status, Some(126));
        assert!(
            r.stderr
                .contains(&format!("{KILL_GUARD_TAG} refused (R2 PROTECTED_TARGET)"))
        );
        assert!(r.real.is_empty());

        // xargs kill
        let r = f.run(
            "/bin/sh",
            "printf '4242\\n4343\\n' | xargs kill",
            &f.env_with(&[], None, None),
        );
        assert_eq!(r.status, Some(0));
        assert!(r.judge.is_empty());
        assert_eq!(r.real, "kill [4242] [4343]\n");

        let r = f.run(
            "/bin/sh",
            &format!("printf '{PROTECTED}\\n' | xargs kill"),
            &f.env_with(&[("FAKE_JUDGE_MODE", "refuse")], None, None),
        );
        assert_ne!(r.status, Some(0));
        assert!(r.judge.contains(&format!("[--] [kill] [{PROTECTED}]\n")));
        assert!(r.real.is_empty());

        // Skips other kill-guard shim dirs & symlink loops
        let other = f.root.join("other-version");
        std::fs::create_dir_all(&other).expect("mkdir");
        std::fs::write(other.join(KILL_GUARD_SHIM_MARKER), "").expect("marker");
        write_executable(
            &other.join("pkill"),
            "#!/bin/sh\necho shadow >> \"$LOG\"\nexit 0\n",
        );
        let path = format!(
            "{}:{}:{}:{SYSTEM_PATH}",
            f.shims.display(),
            other.display(),
            f.real_dir.display()
        );
        let r = f.run(
            "/bin/sh",
            "pkill -x node",
            &f.env_with(&[], None, Some(&path)),
        );
        assert_eq!(r.status, Some(0));
        assert_eq!(r.real, "pkill [-x] [node]\n");

        let link_a = f.root.join("links-a");
        let link_b = f.root.join("links-b");
        for dir in [&link_a, &link_b] {
            std::fs::create_dir_all(dir).expect("mkdir link");
            std::os::unix::fs::symlink(f.shims.join("pkill"), dir.join("pkill")).expect("symlink");
        }
        let path = format!(
            "{}:{}:{}:{}:{SYSTEM_PATH}",
            f.shims.display(),
            link_a.display(),
            link_b.display(),
            f.real_dir.display()
        );
        let r = f.run(
            "/bin/sh",
            "pkill -x node",
            &f.env_with(&[], None, Some(&path)),
        );
        assert_eq!(r.status, Some(0));
        assert_eq!(r.real, "pkill [-x] [node]\n");
    }

    #[test]
    fn bash_env_wraps_builtin_kill_when_bash_is_available() {
        if !bash_available() {
            return;
        }
        let f = ShimFixture::new("test-fast", Some(1));
        let target = OwnedSleeper::spawn();
        let guarded = OwnedSleeper::spawn();
        let prelude = format!(
            "{SHELL_KILL_RECORDER}\nif [ \"$(type -t kill)\" != function ]; then echo NOT-WRAPPED; exit 99; fi"
        );
        let mut guarded_rt = f.runtime();
        guarded_rt.protected_pids = vec![1, guarded.pid];

        let r = f.run(
            "bash",
            &format!("{prelude}\nPID={}; kill $PID", target.pid),
            &f.env_with(&[], None, None),
        );
        assert!(!r.stdout.contains("NOT-WRAPPED"));
        assert_eq!(r.status, Some(0));
        assert!(r.judge.is_empty());
        assert_eq!(r.real, format!("builtin [kill] [{}]\n", target.pid));

        let r = f.run(
            "bash",
            &format!(
                "{prelude}\nP={}; kill -9 $P; echo \"status=$?\"",
                guarded.pid
            ),
            &f.env_with(
                &[("FAKE_JUDGE_MODE", "refuse")],
                Some(guarded_rt.clone()),
                None,
            ),
        );
        assert!(r.stdout.contains("status=126"));
        assert!(
            r.judge
                .contains(&format!("[--] [kill] [-9] [{}]\n", guarded.pid))
        );
        assert!(r.real.is_empty());

        let r = f.run(
            "bash",
            &format!("{prelude}\nT=-1; kill -9 $T; echo \"status=$?\""),
            &f.env_with(&[], None, None),
        );
        assert!(r.stdout.contains("status=126"));
        assert!(
            r.stderr
                .contains(&format!("{KILL_GUARD_TAG} refused (R3 ALL_PROCESSES)"))
        );
        assert!(r.judge.is_empty());
        assert!(r.real.is_empty());

        let r = f.run(
            "bash",
            &format!("{prelude}\nkill %1; kill -- -{}", target.pid),
            &f.env_with(&[], None, None),
        );
        assert_eq!(r.status, Some(0));
        assert!(r.judge.contains("[--] [kill] [%1]\n"));
        assert!(
            r.judge
                .contains(&format!("[--] [kill] [--] [-{}]\n", target.pid))
        );
        assert_eq!(
            r.real,
            format!(
                "builtin [kill] [%1]\nbuiltin [kill] [--] [-{}]\n",
                target.pid
            )
        );

        let r = f.run(
            "bash",
            &format!(
                "{prelude}\nfunction kill {{ builtin kill \"$@\"; }}\nkill -TERM {}",
                target.pid
            ),
            &f.env_with(&[], None, None),
        );
        assert_eq!(r.status, Some(0));
        assert_eq!(
            r.real,
            format!("builtin-bypass [kill] [-TERM] [{}]\n", target.pid)
        );

        let r = f.run(
            "bash",
            &format!(
                "set -eu\n{prelude}\nkill {}\nkill {} || echo \"refused=$?\"\necho done",
                target.pid, guarded.pid
            ),
            &f.env_with(&[("FAKE_JUDGE_MODE", "refuse")], Some(guarded_rt), None),
        );
        assert_eq!(r.status, Some(0));
        assert!(r.stdout.contains("refused=126\ndone\n"));
        assert_eq!(r.real, format!("builtin [kill] [{}]\n", target.pid));

        let user_env = f.root.join("user-bash-env.sh");
        std::fs::write(&user_env, "USER_ENV_LOADED=yes\n").expect("write user bash_env");
        let r = f.run(
            "bash",
            "echo \"$USER_ENV_LOADED\"; type -t kill",
            &f.env_with(&[("BASH_ENV", &user_env.to_string_lossy())], None, None),
        );
        assert_eq!(r.stdout, "yes\nfunction\n");

        // /bin/sh does not source BASH_ENV
        let r = f.run("/bin/sh", "type kill", &f.env_with(&[], None, None));
        assert!(r.stdout.contains("kill"));
        assert!(!r.stdout.contains("function"));
    }

    #[test]
    fn interactive_bash_prompt_hook_wraps_kill() {
        if !bash_available() {
            return;
        }
        let f = ShimFixture::new("test-fast", Some(1));
        let target = OwnedSleeper::spawn();
        let wrapped =
            "if ! declare -f kill | grep -q __senpi_kg_decide; then echo NOT-WRAPPED; exit 99; fi";
        let all_processes = format!("{KILL_GUARD_TAG} refused (R3 ALL_PROCESSES)");

        for args in [&["-i"][..], &["--norc", "-i"][..], &["--posix", "-i"][..]] {
            let typed = format!(
                "{wrapped}\n{SHELL_KILL_RECORDER}\nkill -9 -1; echo \"status=$?\"\nkill {}\nexit\n",
                target.pid
            );
            let r = f.run_interactive(args, &typed, &f.env_with(&[], None, None));
            assert!(!r.stdout.contains("NOT-WRAPPED"), "args={args:?}");
            assert!(
                r.stdout.contains("status=126"),
                "args={args:?} out={}",
                r.stdout
            );
            assert!(r.stderr.contains(&all_processes));
            assert_eq!(r.real, format!("builtin [kill] [{}]\n", target.pid));
        }

        // ~/.bashrc override of kill and child interactive bash
        let home = f.root.join("home-rc-kill");
        std::fs::create_dir_all(&home).expect("mkdir");
        std::fs::write(home.join(".bashrc"), "kill() { :; }\n").expect("write bashrc");
        let r = f.run_interactive(
            &["-i"],
            &format!("{wrapped}\n{SHELL_KILL_RECORDER}\nkill -9 -1; echo \"status=$?\"\nexit\n"),
            &f.env_with(&[("HOME", &home.to_string_lossy())], None, None),
        );
        assert!(!r.stdout.contains("NOT-WRAPPED"));
        assert!(r.stdout.contains("status=126"));

        let child_input = format!(
            "bash --norc -i <<'CHILD'\n{wrapped}\n{SHELL_KILL_RECORDER}\nkill -9 -1; echo \"child=$?\"\nexit\nCHILD\nexit\n"
        );
        let r = f.run_interactive(
            &["--norc", "-i"],
            &child_input,
            &f.env_with(&[], None, None),
        );
        assert!(!r.stdout.contains("NOT-WRAPPED"));
        assert!(r.stdout.contains("child=126"));
        assert!(r.real.is_empty());

        // enable -n kill falls back to PATH shim
        let r = f.run_interactive(
            &["--norc", "-i"],
            &format!(
                "{wrapped}\nenable -n kill || exit 99\nkill {}; echo \"own=$?\"\nkill -9 -1; echo \"all=$?\"\nexit\n",
                target.pid
            ),
            &f.env_with(&[], None, None),
        );
        assert!(r.stdout.contains("own=0"));
        assert!(r.stdout.contains("all=126"));
        assert_eq!(r.real, format!("kill [{}]\n", target.pid));

        // User PROMPT_COMMAND runs after hook preserving $?
        let r = f.run_interactive(
            &["--norc", "-i"],
            &format!("{wrapped}\nfalse\nexit\n"),
            &f.env_with(
                &[("PROMPT_COMMAND", r#"echo "user-prompt rc=$?""#)],
                None,
                None,
            ),
        );
        assert!(r.stdout.contains("user-prompt rc=0\nuser-prompt rc=1\n"));
    }

    /// `kill -0` probe: works on every unix, unlike `/proc/<pid>` (Linux only).
    fn process_alive(pid: &str) -> bool {
        Command::new("kill")
            .args(["-0", pid])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    #[test]
    fn owned_sleeper_reaps_cleanly_on_drop_and_sigterm_without_polling() {
        let sleeper = OwnedSleeper::spawn();
        let pid_str = sleeper.pid.to_string();
        assert!(process_alive(&pid_str));
        drop(sleeper);
        // After Drop kills + waits on the child, it is reaped and no longer alive.
        assert!(!process_alive(&pid_str));

        // Process exit / trap cleanup leaves no orphan sleeper after wait().
        let temp = tempfile::tempdir().expect("tempdir");
        let pid_file = temp.path().join("sleeper.pid");
        let script = format!(
            "sleep 60 &\nspid=$!\nprintf '%s' \"$spid\" > '{}'\ntrap 'kill -KILL \"$spid\" 2>/dev/null || true; wait \"$spid\" 2>/dev/null || true' EXIT TERM INT\nexit 0\n",
            pid_file.display()
        );
        let status = Command::new("/bin/sh")
            .args(["-c", &script])
            .status()
            .expect("run probe");
        assert!(status.success());
        let recorded = std::fs::read_to_string(&pid_file).expect("read pidfile");
        assert!(!process_alive(recorded.trim()));
    }
}
