use super::*;
use maho_ai::utils::abort::AbortController;
use std::{collections::BTreeMap, fs, os::unix::fs::PermissionsExt, path::PathBuf};
use tempfile::TempDir;
use tokio::time::timeout;

#[path = "claude_cli_process_account_tests.rs"]
mod account;

const MANUAL: &str =
    "https://claude.ai/oauth/authorize?scope=user%3Aprofile+user%3Ainference&state=fixture";
const AUTO: &str = "https://claude.ai/oauth/authorize?redirect_uri=http%3A%2F%2Flocalhost%3A54545%2Fcallback&state=fixture";
const TEST_TIMEOUT: Duration = Duration::from_secs(5);

struct Fixture {
    root: TempDir,
    executable: PathBuf,
    env: BTreeMap<String, String>,
}

impl Fixture {
    fn new(mode: &str) -> Self {
        let root = tempfile::tempdir().expect("fixture temp");
        let executable = root.path().join("fake-claude");
        fs::write(
            &executable,
            include_str!("../tests/fixtures/fake-claude-auth.sh"),
        )
        .expect("fixture");
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).expect("chmod");
        let env = BTreeMap::from([
            ("HOME".into(), root.path().display().to_string()),
            ("MODE".into(), mode.into()),
            (
                "RECORD".into(),
                root.path().join("record").display().to_string(),
            ),
        ]);
        Self {
            root,
            executable,
            env,
        }
    }

    fn run(&self) -> ClaudeCliRun<'_> {
        ClaudeCliRun {
            executable: &self.executable,
            config_dir: self.root.path().join("config"),
            parent_env: &self.env,
        }
    }

    fn options(&self, method: ClaudeCliLoginMethod) -> ClaudeCliLoginOptions<'_> {
        ClaudeCliLoginOptions {
            run: self.run(),
            method,
            signal: None,
            idle_timeout_ms: Some(10_000),
            kill_grace_ms: Some(25),
        }
    }

    fn record(&self) -> String {
        fs::read_to_string(self.root.path().join("record")).expect("fixture record")
    }
}

async fn next_event(login: &mut ClaudeCliLogin) -> Option<ClaudeCliLoginEvent> {
    timeout(TEST_TIMEOUT, login.events.recv())
        .await
        .expect("CLI URL event")
}

#[tokio::test]
async fn captures_both_urls_and_requires_the_exact_success_line() {
    for mode in ["browser", "inline", "terminal"] {
        let fixture = Fixture::new(mode);
        let login = start_claude_cli_login(fixture.options(ClaudeCliLoginMethod::Browser));
        let ClaudeCliLogin {
            mut events, result, ..
        } = login;
        assert_eq!(
            timeout(TEST_TIMEOUT, result).await.unwrap(),
            ClaudeCliLoginResult::Success,
            "{mode}"
        );
        let mut urls = Vec::new();
        while let Some(event) = events.recv().await {
            urls.push(match event {
                ClaudeCliLoginEvent::AutomaticUrl(url) => (true, url),
                ClaudeCliLoginEvent::ManualUrl(url) => (false, url),
            });
        }
        urls.sort();
        assert_eq!(
            urls,
            [(false, MANUAL.into()), (true, AUTO.into())],
            "{mode}"
        );
    }
}

#[tokio::test]
async fn delivers_browser_capture_without_waiting_for_stdout_or_exit() {
    let fixture = Fixture::new("watcher");
    let mut login = start_claude_cli_login(fixture.options(ClaudeCliLoginMethod::Browser));
    assert!(
        matches!(next_event(&mut login).await, Some(ClaudeCliLoginEvent::AutomaticUrl(url)) if url == AUTO)
    );
    assert!(login.codes.submit_code("fixture-release"));
    assert_eq!(
        timeout(TEST_TIMEOUT, login.result).await.unwrap(),
        ClaudeCliLoginResult::Success
    );
}

#[tokio::test]
async fn copy_code_discards_automatic_url_and_pastes_only_to_stdin() {
    let fixture = Fixture::new("paste");
    let mut login = start_claude_cli_login(fixture.options(ClaudeCliLoginMethod::CopyCode));
    assert!(
        matches!(next_event(&mut login).await, Some(ClaudeCliLoginEvent::ManualUrl(url)) if url == MANUAL)
    );
    assert!(
        login.codes.submit_code(
            "https://platform.claude.com/oauth/code/callback?code=pasted&state=fixture"
        )
    );
    assert_eq!(
        timeout(TEST_TIMEOUT, &mut login.result).await.unwrap(),
        ClaudeCliLoginResult::Success
    );
    assert!(login.events.recv().await.is_none());
    assert!(!login.codes.submit_code("too-late"));
    let record = fixture.record();
    let dir = fixture.run().config_dir;
    let dir = dir.to_str().unwrap();
    assert_eq!(
        record.lines().skip(1).collect::<Vec<_>>(),
        ["auth", "login", "--claudeai", dir, dir, "pasted#fixture"]
    );
}

#[tokio::test]
async fn foreign_manual_urls_are_not_emitted() {
    for url in [
        "https://claude.ai.evil.example/oauth/authorize",
        "https://claude.ai@evil.example/oauth/authorize",
        "https://evil.example/oauth/authorize?redirect=https%3A%2F%2Fclaude.ai",
        "http://claude.ai/oauth/authorize",
        "https://claude.ai:8443/oauth/authorize",
    ] {
        let mut fixture = Fixture::new("foreign");
        fixture.env.insert("MANUAL_URL".into(), url.into());
        let mut login = start_claude_cli_login(fixture.options(ClaudeCliLoginMethod::CopyCode));
        assert_eq!(
            timeout(TEST_TIMEOUT, &mut login.result).await.unwrap(),
            ClaudeCliLoginResult::Success
        );
        assert!(login.events.recv().await.is_none(), "{url}");
    }
}

#[tokio::test]
async fn maps_failure_without_treating_exit_zero_or_stdout_as_sufficient() {
    use ClaudeCliLoginFailure::*;
    for (mode, reason) in [
        ("silent", UnexpectedExit),
        ("exit-one", UnexpectedExit),
        ("wrong-prefix", UnexpectedExit),
        ("org", OrgNotAllowed),
        ("provider", ProviderNotAllowed),
        ("oauth", OAuthFailed),
    ] {
        let fixture = Fixture::new(mode);
        let mut login = start_claude_cli_login(fixture.options(ClaudeCliLoginMethod::CopyCode));
        if mode == "silent" {
            assert!(matches!(
                next_event(&mut login).await,
                Some(ClaudeCliLoginEvent::ManualUrl(_))
            ));
            assert!(login.codes.submit_code("fixture"));
        }
        assert_eq!(
            timeout(TEST_TIMEOUT, login.result).await.unwrap(),
            ClaudeCliLoginResult::Failed(reason),
            "{mode}"
        );
    }
}

#[tokio::test]
async fn cancels_and_reaps_a_cli_which_ignores_sigterm() {
    let fixture = Fixture::new("hang");
    let controller = AbortController::new();
    let mut options = fixture.options(ClaudeCliLoginMethod::CopyCode);
    options.signal = Some(controller.signal());
    let mut login = start_claude_cli_login(options);
    assert!(matches!(
        next_event(&mut login).await,
        Some(ClaudeCliLoginEvent::ManualUrl(_))
    ));
    controller.abort(None);
    assert_eq!(
        timeout(TEST_TIMEOUT, login.result).await.unwrap(),
        ClaudeCliLoginResult::Cancelled
    );
    assert_reaped(&fixture);
}

#[tokio::test]
async fn idle_deadline_reaps_the_cli() {
    let fixture = Fixture::new("hang");
    let mut login = start_claude_cli_login(fixture.options(ClaudeCliLoginMethod::CopyCode));
    assert!(matches!(
        next_event(&mut login).await,
        Some(ClaudeCliLoginEvent::ManualUrl(_))
    ));
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(10)).await;
    tokio::time::resume();
    assert_eq!(
        timeout(TEST_TIMEOUT, login.result).await.unwrap(),
        ClaudeCliLoginResult::Timeout
    );
    assert_reaped(&fixture);
}

fn assert_reaped(fixture: &Fixture) {
    use nix::{errno::Errno, sys::signal::kill, unistd::Pid};
    let pid: i32 = fixture
        .record()
        .lines()
        .next()
        .expect("fake CLI recorded its PID")
        .parse()
        .expect("fake CLI recorded a numeric PID");
    assert!(pid > 0);
    assert_eq!(kill(Pid::from_raw(pid), None), Err(Errno::ESRCH));
}

#[test]
fn normalizes_callback_inputs_without_allowing_extra_stdin_lines() {
    for (input, expected) in [
        ("abc#def", Some("abc#def")),
        ("  abc  ", Some("abc")),
        (
            "https://platform.claude.com/oauth/code/callback?code=abc&state=def",
            Some("abc#def"),
        ),
        ("code=abc&state=def", Some("abc#def")),
        ("", None),
        ("abc\ndef", None),
        ("code=abc%0Adef", None),
        ("code=abc&state=d%0Def", None),
    ] {
        assert_eq!(
            normalize_pasted_code(input).as_deref(),
            expected,
            "{input:?}"
        );
    }
}
