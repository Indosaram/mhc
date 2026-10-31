//! senpi session-env.test.ts (pin 903b8336): the session environment contract shared with the bash tool.
use maho_codemode::kernels::session_env::*;

fn full_source() -> SessionEnvironmentSource<'static> {
    SessionEnvironmentSource {
        cwd: "/w",
        goal_store_file: Some("/g/x.json"),
        session_id: "session-77",
        session_file: Some("/tmp/sessions/session-77.jsonl"),
        model: Some(("fake-provider", "fake-model")),
        thinking_level: Some("high"),
        browser_engine: None,
    }
}

fn env(pairs: &[(&str, &str)]) -> SessionEnvironment {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect()
}

#[test]
fn resolves_every_pi_session_variable_the_bash_tool_exposes() {
    assert_eq!(
        session_environment_from(&full_source()),
        env(&[
            ("PI_SESSION_ID", "session-77"),
            ("PI_SESSION_CWD", "/w"),
            ("PI_GOAL_STORE_FILE", "/g/x.json"),
            ("PI_SESSION_FILE", "/tmp/sessions/session-77.jsonl"),
            ("PI_PROVIDER", "fake-provider"),
            ("PI_MODEL", "fake-model"),
            ("PI_REASONING_LEVEL", "high"),
        ])
    );
}

#[test]
fn omits_optional_variables_the_session_does_not_provide() {
    let resolved = session_environment_from(&SessionEnvironmentSource {
        cwd: "/w",
        goal_store_file: None,
        session_id: "ephemeral-1",
        session_file: None,
        model: None,
        thinking_level: None,
        browser_engine: None,
    });
    assert_eq!(
        resolved,
        env(&[("PI_SESSION_ID", "ephemeral-1"), ("PI_SESSION_CWD", "/w")])
    );
    for key in SESSION_ENVIRONMENT_KEYS {
        if key == "PI_SESSION_ID" || key == "PI_SESSION_CWD" {
            continue;
        }
        assert!(!resolved.contains_key(key), "{key}");
    }
}

#[test]
fn clears_inherited_cwd_and_goal_store_values_when_the_session_environment_is_empty() {
    let base = env(&[("PI_SESSION_CWD", "stale"), ("PI_GOAL_STORE_FILE", "stale")]);
    assert_eq!(
        apply_session_environment(&base, Some(&SessionEnvironment::new())),
        SessionEnvironment::new()
    );
}

#[test]
fn replaces_inherited_pi_values_instead_of_leaking_them() {
    let base = env(&[
        ("PATH", "/usr/bin"),
        ("PI_SESSION_ID", "stale-session"),
        ("PI_SESSION_FILE", "stale-file.jsonl"),
        ("PI_SESSION_CWD", "stale"),
        ("PI_GOAL_STORE_FILE", "stale"),
        ("PI_PROVIDER", "stale-provider"),
        ("PI_MODEL", "stale-model"),
        ("PI_REASONING_LEVEL", "stale-level"),
    ]);
    let applied = apply_session_environment(&base, Some(&env(&[("PI_SESSION_ID", "session-77")])));
    assert_eq!(
        applied,
        env(&[("PATH", "/usr/bin"), ("PI_SESSION_ID", "session-77")])
    );
    assert_eq!(base["PI_SESSION_ID"], "stale-session");
}

#[test]
fn carries_the_browser_engine_only_for_a_session_that_chose_one() {
    let chosen = SessionEnvironmentSource {
        browser_engine: Some("connected"),
        ..full_source()
    };
    assert_eq!(
        session_environment_from(&chosen)["OMO_BROWSER_ENGINE"],
        "connected"
    );
    assert!(!session_environment_from(&full_source()).contains_key("OMO_BROWSER_ENGINE"));
}

#[test]
fn replaces_an_inherited_browser_engine_with_the_sessions_own_and_clears_it_for_a_session_with_none()
 {
    let base = env(&[
        ("PATH", "/usr/bin"),
        ("OMO_BROWSER_ENGINE", "connected"),
        ("BSK_HOME", "/opt/bsk"),
    ]);
    assert_eq!(
        apply_session_environment(&base, Some(&env(&[("OMO_BROWSER_ENGINE", "builtin")]))),
        env(&[
            ("PATH", "/usr/bin"),
            ("OMO_BROWSER_ENGINE", "builtin"),
            ("BSK_HOME", "/opt/bsk")
        ])
    );
    assert_eq!(
        apply_session_environment(&base, Some(&SessionEnvironment::new())),
        env(&[("PATH", "/usr/bin"), ("BSK_HOME", "/opt/bsk")])
    );
}
