use pretty_assertions::assert_eq;
use tempfile::tempdir;

use super::*;

#[test]
fn test_install_hooks_when_installed_twice_then_both_hooks_stay_executable_and_identical() {
    let tmp = tempdir().unwrap();
    let repo_dir = tmp.path().canonicalize().unwrap();
    let git_dir = repo_dir.join(".git");
    let hooks_dir = git_dir.join("hooks");
    std::fs::create_dir_all(&hooks_dir).unwrap();

    let pre_commit_path = hooks_dir.join("pre-commit");
    let commit_msg_path = hooks_dir.join("commit-msg");
    let post_commit_path = hooks_dir.join("post-commit");

    let first_installed = install_hooks(&repo_dir).unwrap();
    let names: Vec<String> = first_installed.into_iter().map(|h| h.name).collect();
    assert_eq!(
        names,
        vec![
            "pre-commit".to_string(),
            "commit-msg".to_string(),
            "post-commit".to_string()
        ]
    );

    assert_eq!(
        std::fs::read_to_string(&pre_commit_path).unwrap(),
        PRE_COMMIT_HOOK_SCRIPT
    );
    assert_eq!(
        std::fs::read_to_string(&commit_msg_path).unwrap(),
        COMMIT_MSG_HOOK_SCRIPT
    );
    assert_eq!(
        std::fs::read_to_string(&post_commit_path).unwrap(),
        POST_COMMIT_HOOK_SCRIPT
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&pre_commit_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        assert_eq!(
            std::fs::metadata(&commit_msg_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        assert_eq!(
            std::fs::metadata(&post_commit_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
    }

    std::fs::write(&pre_commit_path, "#!/bin/sh\nexit 1\n").unwrap();

    let second_installed = install_hooks(&repo_dir).unwrap();
    let second_names: Vec<String> = second_installed.into_iter().map(|h| h.name).collect();
    assert_eq!(
        second_names,
        vec![
            "pre-commit".to_string(),
            "commit-msg".to_string(),
            "post-commit".to_string()
        ]
    );

    assert_eq!(
        std::fs::read_to_string(&pre_commit_path).unwrap(),
        PRE_COMMIT_HOOK_SCRIPT
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&pre_commit_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
    }
}

#[test]
fn test_with_memory_hook_checker_when_configured_then_injects_runtime_and_script() {
    let checker = MemoryHookChecker {
        exec_path: "/usr/bin/bun".to_string(),
        script: "/opt/omo/hook-cli.js".to_string(),
    };

    let pre_commit = with_memory_hook_checker(PRE_COMMIT_HOOK_SCRIPT, "pre-commit", Some(&checker));
    assert!(pre_commit.contains("omo_hook_runtime='/usr/bin/bun'"));
    assert!(pre_commit.contains("omo_hook_script='/opt/omo/hook-cli.js'"));
    assert!(pre_commit.contains("--memory-hook pre-commit"));

    let commit_msg = with_memory_hook_checker(COMMIT_MSG_HOOK_SCRIPT, "commit-msg", Some(&checker));
    assert!(commit_msg.contains("omo_hook_runtime='/usr/bin/bun'"));
    assert!(commit_msg.contains("--memory-hook commit-msg \"$1\""));
    assert!(commit_msg.contains("exit 0"));
}

#[test]
fn test_hook_scripts_when_checked_then_parse_as_posix_sh_without_bash_only_constructs() {
    for script in [
        PRE_COMMIT_HOOK_SCRIPT,
        COMMIT_MSG_HOOK_SCRIPT,
        POST_COMMIT_HOOK_SCRIPT,
    ] {
        assert!(script.starts_with("#!/bin/sh\n"));
        assert!(!script.contains("<<<"));
        assert!(!script.contains("echo -e"));
        assert!(!script.contains("disown"));
        assert!(!script.contains("env bash"));
    }
    assert!(
        PRE_COMMIT_HOOK_SCRIPT.contains("safe YAML plain scalar")
            || PRE_COMMIT_HOOK_SCRIPT.contains("safe YAML scalar")
    );
    assert!(PRE_COMMIT_HOOK_SCRIPT.contains("secret screening"));
    assert!(COMMIT_MSG_HOOK_SCRIPT.contains("Claim receipt validation failed"));
}

#[test]
fn test_install_hooks_when_linked_worktree_then_land_in_shared_hooks_directory() {
    let tmp = tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();

    let main_repo = root.join("main");
    let main_git = main_repo.join(".git");
    let shared_hooks = main_git.join("hooks");
    std::fs::create_dir_all(&shared_hooks).unwrap();

    let wt_dot_git_dir = main_git.join("worktrees").join("checkout");
    std::fs::create_dir_all(&wt_dot_git_dir).unwrap();
    std::fs::write(wt_dot_git_dir.join("commondir"), "../..\n").unwrap();

    let worktree_dir = root.join("worktree");
    std::fs::create_dir_all(&worktree_dir).unwrap();
    let pointer = format!("gitdir: {}\n", wt_dot_git_dir.display());
    std::fs::write(worktree_dir.join(".git"), pointer).unwrap();

    assert_eq!(resolve_hooks_dir(&worktree_dir), shared_hooks);

    let installed = install_hooks(&worktree_dir).unwrap();
    assert_eq!(installed[0].path, shared_hooks.join("pre-commit"));
    assert_eq!(installed[1].path, shared_hooks.join("commit-msg"));
    assert_eq!(installed[2].path, shared_hooks.join("post-commit"));
    assert!(!worktree_dir.join(".git").join("hooks").exists());
}

// --- Real-git ports of the pre-commit, commit-msg, and post-commit hook behaviour tests.

mod hook_behaviour {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    use super::super::install_hooks;

    struct Run {
        code: i32,
        stdout: String,
        stderr: String,
    }

    impl Run {
        fn output(&self) -> String {
            format!("{}{}", self.stdout, self.stderr)
        }
    }

    fn run(argv: &[&str], cwd: &Path, env: &[(&str, &str)]) -> Run {
        let out = Command::new(argv[0])
            .args(&argv[1..])
            .current_dir(cwd)
            .envs(env.iter().copied())
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .expect("spawn");
        Run {
            code: out.status.code().unwrap_or(1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    pub fn disable_fixture_auto_maintenance(dir: &Path, bare: bool) {
        for (key, val) in [("maintenance.auto", "false"), ("gc.auto", "0")] {
            run(&["git", "config", "--local", key, val], dir, &[]);
        }
        if bare {
            run(&["git", "config", "--local", "receive.autogc", "false"], dir, &[]);
        }
    }

    fn create_repo() -> (TempDir, PathBuf) {
        let tmp = TempDir::new().expect("tempdir");
        let dir = crate::support::paths::strip_verbatim_prefix(
            &tmp.path().canonicalize().expect("canonical"),
        );
        for argv in [
            &["git", "init", "--quiet"][..],
            &["git", "symbolic-ref", "HEAD", "refs/heads/main"],
            &["git", "config", "user.email", "agent@omo.local"],
            &["git", "config", "user.name", "Omo Agent"],
            &["git", "config", "commit.gpgsign", "false"],
        ] {
            run(argv, &dir, &[]);
        }
        disable_fixture_auto_maintenance(&dir, false);
        install_hooks(&dir).expect("install hooks");
        (tmp, dir)
    }

    fn write_files(dir: &Path, files: &BTreeMap<&str, &str>) {
        for (rel, content) in files {
            let full = dir.join(rel);
            std::fs::create_dir_all(full.parent().expect("parent")).expect("mkdir");
            std::fs::write(full, content).expect("write");
        }
    }

    fn commit(dir: &Path, files: &[(&str, &str)], message: &str, env: &[(&str, &str)]) -> Run {
        write_files(dir, &files.iter().copied().collect());
        run(&["git", "add", "-A"], dir, &[]);
        run(&["git", "commit", "-m", message], dir, env)
    }

    fn seed_server_file(dir: &Path, rel: &str, content: &str) {
        write_files(dir, &BTreeMap::from([(rel, content)]));
        run(&["git", "add", "-A"], dir, &[]);
        let result = run(
            &["git", "commit", "--no-verify", "-m", "sync from server"],
            dir,
            &[],
        );
        assert_eq!(result.code, 0, "seed failed: {}", result.output());
    }

    const VALID: &str = "---\ndescription: Persona of the agent\n---\n\nbody\n";
    const LOCKED: &str = "---\ndescription: Locked\nread_only: true\n---\n\nbody\n";
    const SYNC: &[(&str, &str)] = &[("OMO_MEMORY_PUSH_SYNC", "1")];

    #[test]
    fn given_fixture_repo_then_auto_maintenance_is_disabled() {
        let (_tmp, dir) = create_repo();
        let m = run(&["git", "config", "--local", "maintenance.auto"], &dir, &[]);
        assert_eq!(m.stdout.trim(), "false");
        let gc = run(&["git", "config", "--local", "gc.auto"], &dir, &[]);
        assert_eq!(gc.stdout.trim(), "0");
    }

    #[test]
    fn given_valid_memory_markdown_when_committed_then_hook_accepts_every_supported_shape() {
        let (_tmp, dir) = create_repo();

        let result = commit(
            &dir,
            &[
                ("system/persona.md", VALID),
                (
                    "reference/notes.md",
                    "---\ndescription: Notes\nlimit: 5000\n---\n\nnotes\n",
                ),
                (
                    "memory/system/legacy.md",
                    "---\ndescription: Legacy layout\n---\n\nlegacy\n",
                ),
                (
                    "skills/deploy/SKILL.md",
                    "---\ndescription: Deploy skill\nname: deploy\nunknown_key: fine\n---\n\nsteps\n",
                ),
                ("README.md", "no frontmatter here\n"),
            ],
            "memory write",
            &[],
        );

        assert_eq!(result.code, 0, "{}", result.output());
        assert!(
            run(&["git", "log", "--oneline"], &dir, &[])
                .stdout
                .contains("memory write")
        );
    }

    #[test]
    fn given_staged_provenance_header_when_committed_with_memory_tool_trailer_then_accepted() {
        let (_tmp, dir) = create_repo();
        let note = "---\ndescription: Note\nprovenance: \"user replied \\\"yes\\\"\"\n---\n\nbody\n";
        let result = commit(
            &dir,
            &[("reference/approved.md", note)],
            "memory write\n\nOmo-Writer: memory-tool",
            &[],
        );
        assert_eq!(result.code, 0, "{}", result.output());
    }

    #[test]
    fn given_staged_provenance_header_when_committed_by_hand_without_trailer_then_rejected() {
        let (_tmp, dir) = create_repo();
        let note = "---\ndescription: Note\nprovenance: \"user replied \\\"yes\\\"\"\n---\n\nbody\n";
        let result = commit(&dir, &[("reference/forged.md", note)], "hand commit", &[]);
        assert_ne!(result.code, 0);
        assert!(
            result.output().contains("Claim receipt validation failed:"),
            "{}",
            result.output()
        );
    }

    #[test]
    fn given_read_only_server_file_when_unrelated_file_changes_then_commit_still_succeeds() {
        let (_tmp, dir) = create_repo();
        seed_server_file(&dir, "system/locked.md", LOCKED);

        let result = commit(&dir, &[("system/persona.md", VALID)], "add persona", &[]);

        assert_eq!(result.code, 0, "{}", result.output());
    }

    #[test]
    fn given_invalid_frontmatter_rules_when_commit_runs_then_hook_rejects_each() {
        let cases: &[(&str, &str, &str, &str)] = &[
            (
                "missing frontmatter",
                "system/persona.md",
                "just a body\n",
                "missing frontmatter (must start with ---)",
            ),
            (
                "unclosed frontmatter",
                "system/persona.md",
                "---\ndescription: Persona\n\nbody\n",
                "frontmatter opened but never closed",
            ),
            (
                "empty description",
                "system/persona.md",
                "---\ndescription:\n---\n\nbody\n",
                "'description' must not be empty",
            ),
            (
                "multi-line description",
                "system/persona.md",
                "---\ndescription: >\n  wrapped text\n---\n\nbody\n",
                "'description' must be a non-empty single line",
            ),
            (
                "missing description",
                "reference/notes.md",
                "---\nlimit: 5000\n---\n\nbody\n",
                "missing required field 'description'",
            ),
            (
                "unknown key",
                "system/persona.md",
                "---\ndescription: Persona\npriority: high\n---\n\nbody\n",
                "unknown frontmatter key 'priority'",
            ),
            (
                "read_only added by the agent",
                "system/persona.md",
                "---\ndescription: Persona\nread_only: true\n---\n\nbody\n",
                "'read_only' is a protected field and cannot be set by the agent",
            ),
            (
                "flat skill file",
                "skills/deploy.md",
                "---\ndescription: Deploy\n---\n\nsteps\n",
                "invalid skill path (skills must be folders)",
            ),
            (
                "legacy flat skill file",
                "memory/skills/deploy.md",
                "---\ndescription: Deploy\n---\n\nsteps\n",
                "invalid skill path (skills must be folders)",
            ),
        ];
        for (rule, path, content, reason) in cases {
            let (_tmp, dir) = create_repo();

            let result = commit(&dir, &[(path, content)], "memory write", &[]);

            assert_ne!(result.code, 0, "{rule}");
            assert!(
                result.output().contains("Frontmatter validation failed:"),
                "{rule}: {}",
                result.output()
            );
            assert!(
                result.output().contains(reason),
                "{rule}: {}",
                result.output()
            );
            assert_ne!(
                run(&["git", "log", "--oneline"], &dir, &[]).code,
                0,
                "{rule}"
            );
        }
    }

    #[test]
    fn given_committed_read_only_file_when_tampered_then_hook_rejects_commit() {
        let cases: &[(&str, &str, &str, &str)] = &[
            (
                "body edited",
                LOCKED,
                "---\ndescription: Locked\nread_only: true\n---\n\ntampered\n",
                "file is read_only and cannot be modified",
            ),
            (
                "description edited",
                LOCKED,
                "---\ndescription: Renamed\nread_only: true\n---\n\nbody\n",
                "file is read_only and cannot be modified",
            ),
            (
                "unlocked read_only key removed",
                "---\ndescription: Tunable\nread_only: false\n---\n\nbody\n",
                "---\ndescription: Tunable\n---\n\nedited\n",
                "'read_only' is a protected field and cannot be removed by the agent",
            ),
        ];
        for (rule, seeded, content, reason) in cases {
            let (_tmp, dir) = create_repo();
            seed_server_file(&dir, "system/locked.md", seeded);

            let result = commit(&dir, &[("system/locked.md", content)], "tamper", &[]);

            assert_ne!(result.code, 0, "{rule}");
            assert!(
                result.output().contains(reason),
                "{rule}: {}",
                result.output()
            );
            assert_eq!(
                run(&["git", "show", "HEAD:system/locked.md"], &dir, &[]).stdout,
                *seeded,
                "{rule}"
            );
        }
    }

    #[test]
    fn given_secret_reference_values_when_committed_then_allowed_and_credentials_rejected() {
        let (_tmp, dir) = create_repo();
        let references = [
            "client_secret=[FILTERED]",
            "Authorization: Bearer <key>",
            "token: $GITHUB_TOKEN",
            "token: process.env.GITHUB_TOKEN",
            "api_key: env.API_KEY",
            "Authorization: Bearer $TOKEN",
        ];
        for ref_line in references {
            let note = format!("---\ndescription: Reference\n---\n\n{ref_line}\n");
            let result = commit(&dir, &[("reference/note.md", &note)], "ref write", &[]);
            assert_eq!(result.code, 0, "should accept reference: {ref_line}");
        }

        let credentials = [
            "password: hunter2Abrx9",
            "token=nullable-9fQ2xK7",
            "Authorization: Bearer abcdef123456",
            "password=hunter2Xyz9Qreal:true",
            "password=process.env.X||hunter2real",
            "token: ${FOO}realsecret",
        ];
        for cred in credentials {
            let note = format!("---\ndescription: Cred\n---\n\n{cred}\n");
            let result = commit(&dir, &[("reference/bad.md", &note)], "bad write", &[]);
            assert_ne!(result.code, 0, "should reject credential: {cred}");
            assert!(
                result.output().contains("contains secret-like content"),
                "expected secret error for {cred}"
            );
        }
    }

    #[test]
    fn given_pathspec_environment_when_staged_then_commands_succeed() {
        for var in ["GIT_GLOB_PATHSPECS", "GIT_ICASE_PATHSPECS"] {
            let (_tmp, dir) = create_repo();
            let note = "---\ndescription: Pathspec\n---\n\nbody\n";
            write_files(&dir, &BTreeMap::from([("reference/z.md", note)]));
            run(&["git", "add", "-A"], &dir, &[]);
            let entries = crate::memfs::memory_hook_git::staged_entries(&dir).expect("staged");
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].path, "reference/z.md");
            let result = commit(&dir, &[("reference/z.md", note)], "pathspec write", &[(var, "1")]);
            assert_eq!(result.code, 0, "pathspec commit failed with {var}=1");
        }
    }

    fn create_mirror() -> (TempDir, PathBuf) {
        let tmp = TempDir::new().expect("tempdir");
        let dir = crate::support::paths::strip_verbatim_prefix(
            &tmp.path().canonicalize().expect("canonical"),
        );
        run(
            &["git", "init", "--bare", "--quiet", "--initial-branch=main"],
            &dir,
            &[],
        );
        disable_fixture_auto_maintenance(&dir, true);
        (tmp, dir)
    }

    fn log_path(repo: &Path) -> PathBuf {
        repo.join(".git").join("memory-repository-push.log")
    }

    fn configure_mirror(repo: &Path, mirror: &Path) {
        let clean_mirror = crate::support::paths::strip_verbatim_prefix(mirror);
        let url = format!("file://{}", clean_mirror.display());
        run(
            &["git", "config", "--local", "omo.memoryRepository.url", &url],
            repo,
            &[],
        );
    }

    #[test]
    fn given_configured_mirror_when_commit_lands_on_main_then_it_is_pushed_and_logged() {
        let (_tmp, dir) = create_repo();
        let (_mtmp, mirror) = create_mirror();
        configure_mirror(&dir, &mirror);

        let result = commit(&dir, &[("system/persona.md", VALID)], "remember", SYNC);

        assert_eq!(result.code, 0, "{}", result.output());
        let log = std::fs::read_to_string(log_path(&dir)).expect("push log");
        assert!(log.contains("on main") && log.contains("exit=0"), "{log}");
        assert_eq!(
            run(&["git", "show", "main:system/persona.md"], &mirror, &[]).stdout,
            VALID
        );
    }

    #[test]
    fn given_no_configured_mirror_when_commit_lands_then_hook_noops_without_log() {
        let (_tmp, dir) = create_repo();

        let result = commit(&dir, &[("system/persona.md", VALID)], "remember", SYNC);

        assert_eq!(result.code, 0);
        assert!(!log_path(&dir).exists());
    }

    #[test]
    fn given_reflection_branch_and_detached_head_when_commits_land_then_nothing_is_pushed() {
        let (_tmp, dir) = create_repo();
        let (_mtmp, mirror) = create_mirror();
        configure_mirror(&dir, &mirror);
        commit(&dir, &[("system/persona.md", VALID)], "seed", SYNC);
        std::fs::remove_file(log_path(&dir)).expect("remove log");

        run(
            &["git", "checkout", "--quiet", "-b", "memory/reflection"],
            &dir,
            &[],
        );
        commit(
            &dir,
            &[(
                "reference/notes.md",
                "---\ndescription: Notes\n---\n\nnotes\n",
            )],
            "branch write",
            SYNC,
        );
        run(&["git", "checkout", "--quiet", "--detach"], &dir, &[]);
        commit(
            &dir,
            &[("reference/more.md", "---\ndescription: More\n---\n\nmore\n")],
            "detached write",
            SYNC,
        );

        assert!(!log_path(&dir).exists());
        assert!(
            run(&["git", "log", "--oneline", "main"], &mirror, &[])
                .stdout
                .contains("seed")
        );
    }

    #[test]
    fn given_unreachable_mirror_when_commit_lands_then_commit_succeeds_and_failure_is_logged() {
        let (_tmp, dir) = create_repo();
        let missing = TempDir::new().expect("tempdir");
        let gone = missing.path().join("gone.git");
        configure_mirror(&dir, &gone);

        let result = commit(&dir, &[("system/persona.md", VALID)], "remember", SYNC);

        assert_eq!(result.code, 0);
        let log = std::fs::read_to_string(log_path(&dir)).expect("push log");
        assert!(!log.contains("exit=0"), "{log}");
        assert!(
            run(&["git", "log", "--oneline"], &dir, &[])
                .stdout
                .contains("remember")
        );
    }

    #[test]
    fn given_default_background_push_when_commit_lands_then_mirror_receives_it_without_blocking() {
        let (_tmp, dir) = create_repo();
        let (_mtmp, mirror) = create_mirror();
        configure_mirror(&dir, &mirror);

        let result = commit(
            &dir,
            &[("system/persona.md", VALID)],
            "background remember",
            &[],
        );

        assert_eq!(result.code, 0);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let log = std::fs::read_to_string(log_path(&dir)).unwrap_or_default();
            if log.contains("exit=0") {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "push never completed: {log}"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(
            run(&["git", "show", "main:system/persona.md"], &mirror, &[]).stdout,
            VALID
        );
    }

    #[cfg(unix)]
    #[test]
    fn given_hook_scripts_when_parsed_by_sh_then_syntax_check_passes() {
        let tmp = TempDir::new().expect("tempdir");
        std::fs::write(
            tmp.path().join("pre-commit"),
            super::super::PRE_COMMIT_HOOK_SCRIPT,
        )
        .expect("write");
        std::fs::write(
            tmp.path().join("commit-msg"),
            super::super::COMMIT_MSG_HOOK_SCRIPT,
        )
        .expect("write");
        std::fs::write(
            tmp.path().join("post-commit"),
            super::super::POST_COMMIT_HOOK_SCRIPT,
        )
        .expect("write");
        for hook in ["pre-commit", "commit-msg", "post-commit"] {
            assert_eq!(run(&["sh", "-n", hook], tmp.path(), &[]).code, 0, "{hook}");
        }
    }
}
