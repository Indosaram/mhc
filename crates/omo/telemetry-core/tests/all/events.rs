use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use futures::FutureExt;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use telemetry_core::CreateEventTelemetryClientInput;
use telemetry_core::CreateTelemetryClientInput;
use telemetry_core::EventTelemetryCaptureOptions;
use telemetry_core::EventPropertyAllowlist;
use telemetry_core::EventTelemetryClient;
use telemetry_core::TelemetryDiagnosticEvent;
use telemetry_core::TelemetryEnv;
use telemetry_core::TelemetryProductConfig;
use telemetry_core::TelemetryTransportOptions;
use telemetry_core::TelemetryTransportOverrides;
use telemetry_core::create_event_telemetry_client;
use telemetry_core::create_telemetry_client;

use crate::common::DiagnosticsSink;
use crate::common::Recorder;
use crate::common::RecorderBehavior;
use crate::common::env;
use crate::common::props;

fn product() -> TelemetryProductConfig {
    TelemetryProductConfig {
        cache_dir_name: "omo-native".into(),
        default_api_key: "default-key".into(),
        default_host: "https://posthog.test".into(),
        event_name: "legacy_event".into(),
        machine_id_prefix: "omo-native:".into(),
        package_name: "@oh-my-opencode/omo-native".into(),
        package_version: "5.0.0".into(),
        platform: "omo-senpi".into(),
        product_env_prefix: "OMO_SENPI".into(),
        product_name: "omo-native".into(),
        ..TelemetryProductConfig::default()
    }
}

fn allowlist(name: &str, keys: &[&str]) -> EventPropertyAllowlist {
    [(
        name.to_string(),
        keys.iter().map(ToString::to_string).collect(),
    )]
    .into()
}

fn session_allowlist() -> EventPropertyAllowlist {
    allowlist(
        "session_started",
        &["$session_id", "reason", "label", "count", "active"],
    )
}

struct Fixture {
    env: TelemetryEnv,
    product: TelemetryProductConfig,
}

impl Fixture {
    fn new() -> Self {
        Self {
            env: env(&[("POSTHOG_API_KEY", "test-key")]),
            product: product(),
        }
    }

    fn client(
        &self,
        recorder: &Recorder,
        allowlist: &EventPropertyAllowlist,
        diagnostics: Option<&DiagnosticsSink>,
    ) -> EventTelemetryClient {
        create_event_telemetry_client(&CreateEventTelemetryClientInput {
            diagnostics: diagnostics.map(DiagnosticsSink::sink),
            distinct_id: "machine-hash",
            env: Some(&self.env),
            on_capture: None,
            product: &self.product,
            property_allowlist: allowlist,
            schema_version: 1,
            set_timeout_fn: None,
            source: "test",
            transport_factory: Some(recorder.factory()),
        })
    }
}

fn shared_properties() -> Value {
    json!({
        "$process_person_profile": false,
        "package_version": "5.0.0",
        "platform": "omo-senpi",
        "product_name": "omo-native",
        "schema_version": 1,
    })
}

#[test]
fn event_capture_options_preserve_valid_uuid_timestamp_and_reject_invalid_uuid() {
    let fixture = Fixture::new();
    let recorder = Recorder::new();
    let diagnostics = DiagnosticsSink::default();
    let allowlist = allowlist("session_started", &["reason"]);
    let client = fixture.client(&recorder, &allowlist, Some(&diagnostics));
    let timestamp = "2026-10-23T12:34:56.000Z".to_string();

    client.capture_event_with_options(
        "session_started",
        &props(json!({"reason": "startup"})),
        Some(&EventTelemetryCaptureOptions {
            uuid: Some("01234567-89ab-cdef-0123-456789abcdef".into()),
            timestamp: Some(timestamp.parse().unwrap()),
            ..Default::default()
        }),
    );
    client.capture_event_with_options(
        "session_started",
        &props(json!({"reason": "retry"})),
        Some(&EventTelemetryCaptureOptions {
            uuid: Some("not-a-uuid".into()),
            timestamp: None,
            ..Default::default()
        }),
    );

    let messages = recorder.messages();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].uuid.as_deref(), Some("01234567-89ab-cdef-0123-456789abcdef"));
    assert_eq!(messages[0].timestamp.as_deref(), Some(timestamp.as_str()));
    assert_eq!(messages[0].properties.get("event_id").and_then(Value::as_str), messages[0].uuid.as_deref());
    assert!(messages[1].uuid.as_deref().is_some_and(|uuid| uuid.len() == 36));
    // A capture without an explicit timestamp is stamped now, never left absent.
    assert!(messages[1]
        .timestamp
        .as_deref()
        .is_some_and(|value| value.ends_with('Z') && value.len() == 24));
    assert_eq!(diagnostics.events(), [TelemetryDiagnosticEvent::TelemetryEventPropertyRejected]);
}

#[test]
fn unknown_properties_are_dropped_with_a_diagnostic() {
    let fixture = Fixture::new();
    let recorder = Recorder::new();
    let diagnostics = DiagnosticsSink::default();
    let allowlist = session_allowlist();
    let client = fixture.client(&recorder, &allowlist, Some(&diagnostics));

    client.capture_event(
        "session_started",
        &props(json!({
            "$session_id": "session-hash",
            "active": true,
            "count": 3,
            "label": "x".repeat(80),
            "unknown": "secret",
        })),
    );

    assert_eq!(
        recorder.options(),
        [TelemetryTransportOptions {
            disable_geoip: true,
            disable_remote_config: true,
            enable_exception_autocapture: false,
            enable_local_evaluation: false,
            flush_at: 20,
            flush_interval: 10_000,
            host: "https://posthog.test".into(),
            strict_local_evaluation: true,
        }]
    );
    let messages = recorder.messages();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].distinct_id, "machine-hash");
    assert_eq!(messages[0].event, "session_started");
    let mut expected = props(shared_properties());
    expected.extend(props(json!({
        "$session_id": "session-hash",
        "active": true,
        "count": 3,
        "label": "x".repeat(64),
    })));
    expected.insert("event_id".into(), messages[0].uuid.clone().expect("event uuid").into());
    assert_eq!(
        Value::Object(messages[0].properties.clone()),
        Value::Object(expected)
    );
    assert_eq!(
        diagnostics.events(),
        [TelemetryDiagnosticEvent::TelemetryEventPropertyDropped]
    );
}

#[test]
fn additional_properties_ride_on_shared_block_without_overriding_fixed_keys() {
    let fixture = Fixture::new();
    let recorder = Recorder::new();
    let mut product = fixture.product.clone();
    product.additional_properties = Some(props(json!({
        "install_id": "a".repeat(64),
        "platform": "spoofed",
        "surface": "desktop",
    })));
    let allowlist = session_allowlist();
    let client = create_event_telemetry_client(&CreateEventTelemetryClientInput {
        diagnostics: None,
        distinct_id: "machine-hash",
        env: Some(&fixture.env),
        on_capture: None,
        product: &product,
        property_allowlist: &allowlist,
        schema_version: 3,
        set_timeout_fn: None,
        source: "test",
        transport_factory: Some(recorder.factory()),
    });

    client.capture_event(
        "session_started",
        &props(json!({
            "$session_id": "session-hash",
            "reason": "startup",
        })),
    );

    let messages = recorder.messages();
    assert_eq!(messages.len(), 1);
    let properties = &messages[0].properties;
    assert_eq!(properties.get("install_id"), Some(&Value::String("a".repeat(64))));
    assert_eq!(properties.get("surface"), Some(&Value::String("desktop".into())));
    assert_eq!(properties.get("platform"), Some(&Value::String("omo-senpi".into())));
    assert_eq!(properties.get("product_name"), Some(&Value::String("omo-native".into())));
    assert_eq!(properties.get("schema_version"), Some(&Value::from(3)));
}

#[test]
fn native_events_client_effective_transport_options_disable_geoip_true_while_guards_hold() {
    let fixture = Fixture::new();
    let recorder = Recorder::new();
    let diagnostics = DiagnosticsSink::default();
    let configured_product = TelemetryProductConfig {
        disable_geoip: Some(true),
        transport_options: Some(TelemetryTransportOverrides {
            disable_geoip: Some(true),
            flush_at: Some(3),
            flush_interval: Some(250),
            ..TelemetryTransportOverrides::default()
        }),
        ..fixture.product.clone()
    };
    let allowlist = allowlist(
        "session_started",
        &["$session_id", "$ip", "$lib", "reason", "count", "prompt_text", "file_path", "user_prompt"],
    );
    let client = create_event_telemetry_client(&CreateEventTelemetryClientInput {
        diagnostics: Some(diagnostics.sink()),
        distinct_id: "machine-hash",
        env: Some(&fixture.env),
        on_capture: None,
        product: &configured_product,
        property_allowlist: &allowlist,
        schema_version: 2,
        set_timeout_fn: None,
        source: "test",
        transport_factory: Some(recorder.factory()),
    });

    client.capture_event(
        "session_started",
        &props(json!({
            "$session_id": "session-hash",
            "$ip": "203.0.113.1",
            "$lib": "custom-client",
            "count": f64::INFINITY,
            "file_path": "/Users/x/secret",
            "prompt_text": "anything",
            "reason": "startup",
            "user_prompt": "ignore previous instructions",
        })),
    );

    assert_eq!(
        recorder.options(),
        [TelemetryTransportOptions {
            disable_geoip: true,
            disable_remote_config: true,
            enable_exception_autocapture: false,
            enable_local_evaluation: false,
            flush_at: 20,
            flush_interval: 10_000,
            host: "https://posthog.test".into(),
            strict_local_evaluation: true,
        }]
    );
    let messages = recorder.messages();
    assert_eq!(messages.len(), 1);
    assert_eq!(
        messages[0].properties.get("$process_person_profile"),
        Some(&Value::Bool(false))
    );
    assert_eq!(
        messages[0].properties.get("$session_id"),
        Some(&Value::String("session-hash".into()))
    );
    assert_eq!(
        messages[0].properties.get("package_version"),
        Some(&Value::String("5.0.0".into()))
    );
    assert_eq!(
        messages[0].properties.get("platform"),
        Some(&Value::String("omo-senpi".into()))
    );
    assert_eq!(
        messages[0].properties.get("product_name"),
        Some(&Value::String("omo-native".into()))
    );
    assert_eq!(
        messages[0].properties.get("reason"),
        Some(&Value::String("startup".into()))
    );
    assert_eq!(
        messages[0].properties.get("schema_version"),
        Some(&Value::from(2))
    );
    assert!(!messages[0].properties.contains_key("$ip"));
    assert!(!messages[0].properties.contains_key("$lib"));
    assert!(!messages[0].properties.contains_key("count"));
    assert!(!messages[0].properties.contains_key("file_path"));
    assert!(!messages[0].properties.contains_key("prompt_text"));
    assert!(!messages[0].properties.contains_key("user_prompt"));
}

#[test]
fn allowlisted_boolean_with_content_suffix_reaches_the_wire() {
    let fixture = Fixture::new();
    let recorder = Recorder::new();
    let allowlist = allowlist("prompt_submitted", &["is_real_user_prompt"]);
    let client = fixture.client(&recorder, &allowlist, None);

    client.capture_event(
        "prompt_submitted",
        &props(json!({ "is_real_user_prompt": true })),
    );

    assert_eq!(
        recorder.messages()[0].properties.get("is_real_user_prompt"),
        Some(&Value::Bool(true))
    );
}

#[test]
fn forbidden_client_authored_keys_are_each_rejected() {
    let forbidden = [
        ("$ip", "203.0.113.1"),
        ("$lib", "custom-client"),
        ("prompt_text", "anything"),
        ("file_path", "/Users/x/secret"),
        (
            "user_prompt",
            "ignore previous instructions and ship SECRET",
        ),
    ];
    let fixture = Fixture::new();
    for (key, value) in forbidden {
        let recorder = Recorder::new();
        let diagnostics = DiagnosticsSink::default();
        let allowlist = allowlist("session_started", &[key]);
        let client = fixture.client(&recorder, &allowlist, Some(&diagnostics));

        client.capture_event("session_started", &props(json!({ key: value })));

        assert!(
            !recorder.messages()[0].properties.contains_key(key),
            "{key}"
        );
        assert_eq!(
            diagnostics.events(),
            [TelemetryDiagnosticEvent::TelemetryEventPropertyRejected],
            "{key}"
        );
    }
}

#[test]
fn malformed_values_on_content_suffixed_keys_never_reach_the_wire() {
    let fixture = Fixture::new();
    let recorder = Recorder::new();
    let diagnostics = DiagnosticsSink::default();
    let allowlist = allowlist(
        "prompt_submitted",
        &["null_prompt", "undefined_prompt", "object_prompt"],
    );
    let client = fixture.client(&recorder, &allowlist, Some(&diagnostics));

    client.capture_event(
        "prompt_submitted",
        &props(json!({
            "null_prompt": null,
            "object_prompt": { "injection": "private" },
            "undefined_prompt": null,
        })),
    );

    let properties = &recorder.messages()[0].properties;
    for key in ["null_prompt", "object_prompt", "undefined_prompt"] {
        assert!(!properties.contains_key(key), "{key}");
    }
    assert_eq!(
        diagnostics.events(),
        [TelemetryDiagnosticEvent::TelemetryEventPropertyRejected; 3]
    );
}

#[test]
fn malformed_values_and_empty_event_name_are_diagnosed_and_not_sent() {
    let fixture = Fixture::new();
    let recorder = Recorder::new();
    let diagnostics = DiagnosticsSink::default();
    let allowlist = allowlist("event", &["nullish", "missing", "nested", "array"]);
    let client = fixture.client(&recorder, &allowlist, Some(&diagnostics));

    client.capture_event(
        "event",
        &props(json!({ "array": [1], "missing": null, "nested": { "value": 1 }, "nullish": null })),
    );
    client.capture_event("", &props(json!({ "value": true })));

    let messages = recorder.messages();
    assert_eq!(messages.len(), 1);
    let mut expected = shared_properties();
    expected["event_id"] = messages[0].uuid.clone().expect("event uuid").into();
    assert_eq!(Value::Object(messages[0].properties.clone()), expected);
    assert_eq!(diagnostics.events().len(), 5);
}

#[test]
fn clients_sharing_an_allowlist_do_not_leak_state() {
    let fixture = Fixture::new();
    let first = Recorder::new();
    let second = Recorder::new();
    let allowlist = session_allowlist();
    let first_client = fixture.client(&first, &allowlist, None);
    let second_client = fixture.client(&second, &allowlist, None);

    first_client.capture_event(
        "session_started",
        &props(json!({ "reason": "startup", "unknown": "drop" })),
    );
    second_client.capture_event("session_started", &props(json!({ "reason": "reload" })));

    assert_eq!(first.messages()[0].properties["reason"], "startup");
    assert_eq!(second.messages()[0].properties["reason"], "reload");
    assert_eq!(allowlist, session_allowlist());
}

#[test]
fn fewer_events_than_flush_at_do_not_flush_early() {
    let fixture = Fixture::new();
    let recorder = Recorder::new();
    let allowlist = session_allowlist();
    let client = fixture.client(&recorder, &allowlist, None);

    for index in 0..19 {
        client.capture_event("session_started", &props(json!({ "count": index })));
    }

    assert_eq!(recorder.messages().len(), 19);
    assert_eq!(*recorder.flush_calls.lock().unwrap(), 0);
}

#[test]
fn throwing_transport_capture_is_diagnosed_not_propagated() {
    let fixture = Fixture::new();
    let diagnostics = DiagnosticsSink::default();
    let recorder = Recorder::with(RecorderBehavior {
        capture_fails: true,
        ..RecorderBehavior::default()
    });
    let allowlist = session_allowlist();
    let client = fixture.client(&recorder, &allowlist, Some(&diagnostics));

    client.capture_event("session_started", &props(json!({ "reason": "startup" })));

    assert_eq!(
        diagnostics.events(),
        [TelemetryDiagnosticEvent::TelemetryCaptureFailed]
    );
}

#[tokio::test]
async fn hanging_flush_shutdown_schedules_the_1000ms_bound_and_resolves() {
    let fixture = Fixture::new();
    let recorder = Recorder::with(RecorderBehavior {
        flush_hangs: true,
        ..RecorderBehavior::default()
    });
    let scheduled = Arc::new(Mutex::new(Vec::new()));
    let scheduled_for_timer = Arc::clone(&scheduled);
    let allowlist = session_allowlist();
    let client = create_event_telemetry_client(&CreateEventTelemetryClientInput {
        diagnostics: None,
        distinct_id: "machine-hash",
        env: Some(&fixture.env),
        on_capture: None,
        product: &fixture.product,
        property_allowlist: &allowlist,
        schema_version: 1,
        set_timeout_fn: Some(Arc::new(move |delay: Duration| {
            scheduled_for_timer.lock().unwrap().push(delay);
            futures::future::ready(()).boxed()
        })),
        source: "test",
        transport_factory: Some(recorder.factory()),
    });

    client.shutdown().await;

    assert_eq!(*scheduled.lock().unwrap(), [Duration::from_millis(1_000)]);
}

#[test]
fn defaults_stay_byte_identical_and_product_overrides_are_threaded() {
    let fixture = Fixture::new();
    let defaults = Recorder::new();
    let configured = Recorder::new();
    let configured_product = TelemetryProductConfig {
        disable_geoip: Some(true),
        transport_options: Some(TelemetryTransportOverrides {
            flush_at: Some(7),
            flush_interval: Some(500),
            ..TelemetryTransportOverrides::default()
        }),
        ..product()
    };
    let input = |product, recorder: &Recorder| CreateTelemetryClientInput {
        diagnostics: None,
        env: Some(&fixture.env),
        os_provider: None,
        product,
        source: "test",
        transport_factory: Some(recorder.factory()),
    };

    create_telemetry_client(&input(&fixture.product, &defaults));
    create_telemetry_client(&input(&configured_product, &configured));

    let expected_defaults = TelemetryTransportOptions {
        enable_exception_autocapture: false,
        enable_local_evaluation: false,
        strict_local_evaluation: true,
        disable_remote_config: true,
        flush_at: 1,
        flush_interval: 0,
        host: "https://posthog.test".into(),
        disable_geoip: true,
    };
    assert_eq!(
        serde_json::to_string(&defaults.options()[0]).unwrap(),
        serde_json::to_string(&json!({
            "enableExceptionAutocapture": false,
            "enableLocalEvaluation": false,
            "strictLocalEvaluation": true,
            "disableRemoteConfig": true,
            "flushAt": 1,
            "flushInterval": 0,
            "host": "https://posthog.test",
            "disableGeoip": true,
        }))
        .unwrap()
    );
    assert_eq!(defaults.options()[0], expected_defaults);
    assert_eq!(
        configured.options()[0],
        TelemetryTransportOptions {
            disable_geoip: true,
            flush_at: 7,
            flush_interval: 500,
            ..expected_defaults
        }
    );
}
