use super::{AbortController, Fixture, fs};
use crate::{
    claude_cli_account_dir::{
        RemoveAccountDirOptions, RemoveAccountDirOutcome, account_dir_for, ensure_account_dir,
        is_valid_account_dir_id, new_account_dir_id, remove_account_dir,
    },
    claude_cli_auth_status::{ClaudeCliAuthStatus, ClaudeCliProfile, read_claude_cli_auth_status},
    claude_cli_login_outcome::{
        ClaudeCliLoginOutcome, record_claude_cli_login_outcome, take_claude_cli_login_outcome,
    },
};

#[tokio::test]
async fn status_spawn_pins_dir_and_drops_inherited_host_secrets() {
    let mut fixture = Fixture::new("status");
    for key in [
        "ANTHROPIC_API_KEY",
        "CLAUDE_SECURESTORAGE_CONFIG_DIR",
        "CLAUDE_CONFIG_DIR",
        "CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR",
        "BROWSER",
    ] {
        fixture.env.insert(key.into(), "fixture-leak".into());
    }
    let run = fixture.run();
    fs::create_dir(&run.config_dir).unwrap();
    assert_eq!(
        read_claude_cli_auth_status(&run, 5_000).await,
        ClaudeCliAuthStatus::SignedIn(ClaudeCliProfile {
            email: Some("fixture@example.test".into()),
            ..ClaudeCliProfile::default()
        })
    );
    let dir = run.config_dir.to_str().unwrap();
    assert_eq!(
        fixture.record().lines().skip(1).collect::<Vec<_>>(),
        ["auth", "status", "--json", dir, dir]
    );
}

#[tokio::test]
async fn signs_out_exact_dir_even_when_missing_and_preserves_shared_files() {
    use RemoveAccountDirOutcome::*;
    for (existing, executable, exit, expected) in [
        (true, true, "0", Removed),
        (true, true, "1", RemovedWithoutSignOut),
        (true, false, "0", RemovedWithoutSignOut),
        (false, true, "0", Removed),
    ] {
        let mut fixture = Fixture::new("logout");
        fixture.env.insert("LOGOUT_EXIT".into(), exit.into());
        let id = new_account_dir_id();
        assert!(is_valid_account_dir_id(&id));
        let account = account_dir_for(fixture.root.path(), &id).unwrap();
        assert!(
            account.starts_with(fixture.root.path()),
            "override must not write outside fixture"
        );
        let shared = fixture.root.path().join("shared");
        if existing {
            ensure_account_dir(fixture.root.path(), &id, &shared).unwrap();
            fs::write(shared.join("projects").join("keep"), "shared transcript").unwrap();
        }
        assert_eq!(
            remove_account_dir(RemoveAccountDirOptions {
                agent_dir: fixture.root.path(),
                dir_id: &id,
                executable: executable.then_some(fixture.executable.as_path()),
                parent_env: &fixture.env,
                signal: None,
            })
            .await
            .unwrap(),
            expected
        );
        assert!(!account.exists());
        if executable {
            let dir = account.to_str().unwrap();
            assert_eq!(
                fixture.record().lines().skip(1).collect::<Vec<_>>(),
                ["auth", "logout", dir, dir]
            );
        } else {
            assert!(!fixture.root.path().join("record").exists());
        }
        if existing {
            assert_eq!(
                fs::read_to_string(shared.join("projects").join("keep")).unwrap(),
                "shared transcript"
            );
        }
    }
}

#[tokio::test]
async fn refuses_linked_account_dirs_and_leaves_aborted_removals_intact() {
    let fixture = Fixture::new("logout");
    let id = new_account_dir_id();
    let account = account_dir_for(fixture.root.path(), &id).unwrap();
    assert!(
        account.starts_with(fixture.root.path()),
        "override must not write outside fixture"
    );
    let shared = fixture.root.path().join("shared");
    fs::create_dir(&shared).unwrap();
    fs::create_dir_all(account.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&shared, &account).unwrap();
    let options = RemoveAccountDirOptions {
        agent_dir: fixture.root.path(),
        dir_id: &id,
        executable: Some(&fixture.executable),
        parent_env: &fixture.env,
        signal: None,
    };
    assert_eq!(
        remove_account_dir(RemoveAccountDirOptions { ..options })
            .await
            .unwrap(),
        RemoveAccountDirOutcome::NotAnAccountDir
    );
    assert_eq!(fs::read_link(&account).unwrap(), shared);
    fs::remove_file(&account).unwrap();
    fs::create_dir(&account).unwrap();
    let controller = AbortController::new();
    controller.abort(None);
    assert!(
        remove_account_dir(RemoveAccountDirOptions {
            signal: Some(&controller.signal()),
            ..options
        })
        .await
        .is_err()
    );
    assert!(account.is_dir());
    assert!(!fixture.root.path().join("record").exists());
}

#[test]
fn outcome_is_scoped_to_signal_identity_and_consumed_once() {
    let a = AbortController::new().signal();
    let b = AbortController::new().signal();
    for (signal, name) in [(&a, "first"), (&b, "second")] {
        record_claude_cli_login_outcome(
            signal,
            ClaudeCliLoginOutcome {
                account_name: name.into(),
                check: serde_json::json!({"outcome":"ok"}),
            },
        );
    }
    assert!(take_claude_cli_login_outcome(None).is_none());
    assert_eq!(
        take_claude_cli_login_outcome(Some(&b))
            .unwrap()
            .account_name,
        "second"
    );
    assert!(take_claude_cli_login_outcome(Some(&b)).is_none());
    assert_eq!(
        take_claude_cli_login_outcome(Some(&a.clone()))
            .unwrap()
            .account_name,
        "first"
    );
}
