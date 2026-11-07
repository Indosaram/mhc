use std::path::Path;

use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use sha2::Digest;
use telemetry_core::CreateTelemetryClientInput;
use telemetry_core::DEFAULT_POSTHOG_API_KEY;
use telemetry_core::DEFAULT_POSTHOG_HOST;
use telemetry_core::RecordDailyActiveInput;
use telemetry_core::TELEMETRY_INSTALL_ID_FILE_NAME;
use telemetry_core::TELEMETRY_RUNTIME;
use telemetry_core::TelemetryClientEnabledInput;
use telemetry_core::TelemetryEnv;
use telemetry_core::TelemetryProductConfig;
use telemetry_core::TrackActiveInput;
use telemetry_core::UNCONFIGURED_POSTHOG_API_KEY;
use telemetry_core::create_telemetry_client;
use telemetry_core::get_telemetry_activity_state_file_path;
use telemetry_core::get_telemetry_install_distinct_id;
use telemetry_core::is_telemetry_client_enabled;
use telemetry_core::record_daily_active;
use telemetry_core::to_telemetry_terminal_program;

use crate::common::FakeOs;
use crate::common::Recorder;
use crate::common::env;

fn product() -> TelemetryProductConfig {
    TelemetryProductConfig {
        cache_dir_name: "omo-codex".into(),
        default_api_key: DEFAULT_POSTHOG_API_KEY.into(),
        default_host: DEFAULT_POSTHOG_HOST.into(),
        event_name: "omo_codex_daily_active".into(),
        package_name: "@oh-my-opencode/omo-codex".into(),
        package_version: "4.9.2".into(),
        platform: "omo-codex".into(),
        product_name: "omo-codex".into(),
        product_env_prefix: "OMO_CODEX".into(),
        ..TelemetryProductConfig::default()
    }
}

fn enabled(env: &TelemetryEnv, default_api_key: &str, product_env_prefix: &str) -> bool {
    is_telemetry_client_enabled(&TelemetryClientEnabledInput {
        env: Some(env),
        default_api_key,
        product_env_prefix,
    })
}

fn record_input<'a>(
    env: &'a TelemetryEnv,
    product: &'a TelemetryProductConfig,
    state_dir: &'a Path,
    recorder: &Recorder,
) -> RecordDailyActiveInput<'a> {
    RecordDailyActiveInput {
        diagnostics: None,
        env: Some(env),
        now: Some("2026-05-25T01:02:03.000Z".parse().unwrap()),
        os_provider: Some(&FakeOs),
        product,
        reason: "session_start",
        source: "plugin",
        state_dir,
        transport_factory: Some(recorder.factory()),
    }
}

#[test]
fn unconfigured_placeholder_with_clean_env_fails_closed_without_transports() {
    let recorder = Recorder::new();
    let env = TelemetryEnv::new();
    let product = TelemetryProductConfig {
        default_api_key: UNCONFIGURED_POSTHOG_API_KEY.into(),
        product_env_prefix: "OMO_SENPI".into(),
        ..product()
    };
    let input = CreateTelemetryClientInput {
        diagnostics: None,
        env: Some(&env),
        os_provider: Some(&FakeOs),
        product: &product,
        source: "plugin",
        transport_factory: Some(recorder.factory()),
    };

    let first = create_telemetry_client(&input);
    let second = create_telemetry_client(&input);

    assert!(!enabled(&env, UNCONFIGURED_POSTHOG_API_KEY, "OMO_SENPI"));
    assert_eq!([first.enabled(), second.enabled()], [false, false]);
    assert_eq!(recorder.created(), 0);
}

#[test]
fn placeholder_default_with_real_env_override_is_enabled() {
    let env = env(&[("POSTHOG_API_KEY", "phc_test")]);
    assert!(enabled(&env, UNCONFIGURED_POSTHOG_API_KEY, "OMO_SENPI"));
}

#[tokio::test]
async fn terminal_program_capture_keeps_only_published_names() {
    const CHILD: &str = "MHC_TEST_TERMINAL_CAPTURE";
    if let Ok(expected) = std::env::var(CHILD) {
        let recorder = Recorder::new();
        let env = env(&[("POSTHOG_API_KEY", "test-key")]);
        let product = product();
        let client = create_telemetry_client(&CreateTelemetryClientInput {
            diagnostics: None,
            env: Some(&env),
            os_provider: Some(&FakeOs),
            product: &product,
            source: "cli",
            transport_factory: Some(recorder.factory()),
        });
        client.track_active(&TrackActiveInput {
            day_utc: "2026-10-28".into(),
            distinct_id: "distinct".into(),
            reason: "cli_run".into(),
        });
        let messages = recorder.messages();
        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0]
                .properties
                .get("terminal")
                .and_then(Value::as_str),
            (!expected.is_empty()).then_some(expected.as_str())
        );
        assert!(
            !serde_json::to_string(&messages[0])
                .expect("capture JSON")
                .contains("sentinel-user")
        );
        return;
    }

    let mut cases = telemetry_core::KNOWN_TERMINAL_PROGRAMS
        .into_iter()
        .map(|known| (Some(known), known))
        .collect::<Vec<_>>();
    cases.extend([
        (Some("/home/sentinel-user/bin/x"), "other"),
        (Some("my-private-terminal"), "other"),
        (Some("iterm.app"), "other"),
        (Some(" vscode "), "other"),
        (Some(""), ""),
        (None, ""),
    ]);
    let thread = std::thread::current();
    let test_name = thread.name().expect("test thread name");
    for (terminal, expected) in cases {
        let mut child = tokio::process::Command::new(std::env::current_exe().expect("test binary"));
        child
            .args(["--exact", test_name, "--nocapture"])
            .env(CHILD, expected)
            .kill_on_drop(true);
        match terminal {
            Some(value) => {
                child.env("TERM_PROGRAM", value);
            }
            None => {
                child.env_remove("TERM_PROGRAM");
            }
        }
        let output = tokio::time::timeout(std::time::Duration::from_secs(10), child.output())
            .await
            .expect("capture child deadline")
            .expect("capture test child");
        assert!(
            output.status.success(),
            "terminal {terminal:?}: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
    }
}

#[test]
fn legacy_shared_key_default_remains_enabled() {
    assert!(enabled(
        &TelemetryEnv::new(),
        DEFAULT_POSTHOG_API_KEY,
        "OMO_CODEX"
    ));
}

#[test]
fn real_default_replacing_the_placeholder_activates_without_another_flag() {
    assert!(enabled(
        &TelemetryEnv::new(),
        "phc_omo_native_project_key",
        "OMO_SENPI"
    ));
}

#[tokio::test]
async fn codex_daily_active_payload_matches_current_contract() {
    let recorder = Recorder::new();
    let env = env(&[
        ("POSTHOG_API_KEY", "test-key"),
        ("POSTHOG_HOST", "https://posthog.test"),
    ]);
    let product = product();
    let state_dir = tempfile::tempdir().unwrap();
    let distinct_id = get_telemetry_install_distinct_id(state_dir.path());
    let client = create_telemetry_client(&CreateTelemetryClientInput {
        diagnostics: None,
        env: Some(&env),
        os_provider: Some(&FakeOs),
        product: &product,
        source: "cli",
        transport_factory: Some(recorder.factory()),
    });

    client.track_active(&TrackActiveInput {
        day_utc: "2026-05-25".into(),
        distinct_id: distinct_id.clone(),
        reason: "cli_run".into(),
    });
    client.flush().await.unwrap();
    client.shutdown().await;

    let messages = recorder.messages();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].distinct_id, distinct_id);
    assert_eq!(distinct_id.len(), 64);
    assert_eq!(messages[0].event, "omo_codex_daily_active");
    let mut expected = json!({
        "platform": "omo-codex",
        "product_name": "omo-codex",
        "package_name": "@oh-my-opencode/omo-codex",
        "package_version": "4.9.2",
        "runtime": TELEMETRY_RUNTIME,
        "runtime_version": env!("CARGO_PKG_VERSION"),
        "source": "cli",
        "$os": "darwin",
        "$os_version": "26.0.0",
        "os_arch": "arm64",
        "os_type": "Darwin",
        "cpu_count": 1,
        "cpu_model": "Apple M-test",
        "total_memory_gb": 16,
        "ci": std::env::var("CI").is_ok_and(|value| !value.is_empty()),
        "$process_person_profile": false,
        "day_utc": "2026-05-25",
        "reason": "cli_run",
    });
    let optional = [
        ("locale", sys_locale::get_locale()),
        ("timezone", iana_time_zone::get_timezone().ok()),
        (
            "shell",
            std::env::var("SHELL")
                .ok()
                .and_then(|path| path.rsplit(['/', '\\']).next().map(str::to_owned))
                .filter(|name| !name.is_empty()),
        ),
        (
            "terminal",
            to_telemetry_terminal_program(std::env::var("TERM_PROGRAM").ok().as_deref())
                .map(str::to_owned),
        ),
    ];
    for (key, value) in optional {
        if let Some(value) = value {
            expected[key] = Value::String(value);
        }
    }
    assert_eq!(Value::Object(messages[0].properties.clone()), expected);
}

#[tokio::test]
async fn daily_active_recorded_twice_same_day_sends_one_event() {
    let recorder = Recorder::new();
    let state_dir = tempfile::tempdir().unwrap();
    let env = env(&[("POSTHOG_API_KEY", "test-key")]);
    let product = product();
    let input = record_input(&env, &product, state_dir.path(), &recorder);

    record_daily_active(&input).await.unwrap();
    record_daily_active(&input).await.unwrap();

    let messages = recorder.messages();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].properties["day_utc"], "2026-05-25");
    assert_eq!(messages[0].properties["reason"], "session_start");
}

#[test]
fn blank_api_key_after_trimming_never_constructs_a_transport() {
    let recorder = Recorder::new();
    let env = env(&[("POSTHOG_API_KEY", " ")]);
    let product = product();

    let client = create_telemetry_client(&CreateTelemetryClientInput {
        diagnostics: None,
        env: Some(&env),
        os_provider: Some(&FakeOs),
        product: &product,
        source: "install",
        transport_factory: Some(recorder.factory()),
    });
    client.track_active(&TrackActiveInput {
        day_utc: "2026-05-25".into(),
        distinct_id: "distinct".into(),
        reason: "install_completed".into(),
    });

    assert_eq!(recorder.created(), 0);
}

#[tokio::test]
async fn disabled_telemetry_does_not_write_dedup_state() {
    let recorder = Recorder::new();
    let state_dir = tempfile::tempdir().unwrap();
    let env = env(&[
        ("OMO_CODEX_DISABLE_POSTHOG", "1"),
        ("POSTHOG_API_KEY", "test-key"),
    ]);
    let product = product();

    record_daily_active(&record_input(&env, &product, state_dir.path(), &recorder))
        .await
        .unwrap();

    assert_eq!(recorder.messages().len(), 0);
    assert!(!get_telemetry_activity_state_file_path(state_dir.path()).exists());
}

#[tokio::test]
async fn blank_api_key_does_not_write_dedup_state() {
    let recorder = Recorder::new();
    let state_dir = tempfile::tempdir().unwrap();
    let env = env(&[("POSTHOG_API_KEY", " ")]);
    let product = product();

    record_daily_active(&record_input(&env, &product, state_dir.path(), &recorder))
        .await
        .unwrap();

    assert_eq!(recorder.messages().len(), 0);
    assert!(!get_telemetry_activity_state_file_path(state_dir.path()).exists());
}

#[tokio::test]
async fn daily_active_distinct_id_is_the_hash_of_the_state_dir_install_id() {
    let recorder = Recorder::new();
    let state_dir = tempfile::tempdir().unwrap();
    let env = env(&[("POSTHOG_API_KEY", "test-key")]);
    let product = product();

    record_daily_active(&record_input(&env, &product, state_dir.path(), &recorder))
        .await
        .unwrap();

    let install_id =
        std::fs::read_to_string(state_dir.path().join(TELEMETRY_INSTALL_ID_FILE_NAME)).unwrap();
    let expected = hex::encode(sha2::Sha256::digest(install_id.trim().as_bytes()));
    let messages = recorder.messages();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].distinct_id, expected);
}

#[tokio::test]
async fn disabled_or_keyless_daily_active_creates_no_install_id() {
    for pairs in [
        &[
            ("OMO_CODEX_DISABLE_POSTHOG", "1"),
            ("POSTHOG_API_KEY", "test-key"),
        ][..],
        &[("DO_NOT_TRACK", "1"), ("POSTHOG_API_KEY", "test-key")][..],
        &[("POSTHOG_API_KEY", " ")][..],
    ] {
        let recorder = Recorder::new();
        let state_dir = tempfile::tempdir().unwrap();
        let env = env(pairs);
        let product = product();

        record_daily_active(&record_input(&env, &product, state_dir.path(), &recorder))
            .await
            .unwrap();

        assert_eq!(recorder.messages().len(), 0, "{pairs:?}");
        assert!(
            !state_dir
                .path()
                .join(TELEMETRY_INSTALL_ID_FILE_NAME)
                .exists(),
            "{pairs:?}"
        );
    }
}
