use std::collections::HashMap;

use pretty_assertions::assert_eq;
use telemetry_core::ShouldDisableTelemetryInput;
use telemetry_core::UNCONFIGURED_POSTHOG_API_KEY;
use telemetry_core::has_telemetry_api_key;
use telemetry_core::should_disable_telemetry;

use crate::common::env;

type OptOutCase = (&'static str, &'static [(&'static str, &'static str)], bool);

fn disabled(pairs: &[(&str, &str)], product_env_prefix: &str) -> bool {
    should_disable_telemetry(&ShouldDisableTelemetryInput {
        env: Some(&env(pairs)),
        global_env_prefix: None,
        product_env_prefix,
    })
}

#[test]
fn api_key_configuration_matches_expected_availability() {
    let longer = format!("{UNCONFIGURED_POSTHOG_API_KEY}_configured");
    let cases = [
        ("empty", "", false),
        ("whitespace-only", "   ", false),
        (
            "exact unconfigured placeholder",
            UNCONFIGURED_POSTHOG_API_KEY,
            false,
        ),
        (
            "longer key containing the placeholder",
            longer.as_str(),
            true,
        ),
    ];
    for (name, api_key, expected) in cases {
        let result = has_telemetry_api_key(Some(&env(&[("POSTHOG_API_KEY", api_key)])), "default");
        assert_eq!(result, expected, "{name}");
    }
}

#[test]
fn opt_out_env_matrix() {
    let cases: [OptOutCase; 15] = [
        ("unset env enables telemetry", &[], false),
        ("global disable 1", &[("OMO_DISABLE_POSTHOG", "1")], true),
        (
            "global disable true",
            &[("OMO_DISABLE_POSTHOG", "true")],
            true,
        ),
        (
            "global disable yes",
            &[("OMO_DISABLE_POSTHOG", "yes")],
            true,
        ),
        (
            "global send 0",
            &[("OMO_SEND_ANONYMOUS_TELEMETRY", "0")],
            true,
        ),
        (
            "global send false",
            &[("OMO_SEND_ANONYMOUS_TELEMETRY", "false")],
            true,
        ),
        (
            "global send no",
            &[("OMO_SEND_ANONYMOUS_TELEMETRY", "no")],
            true,
        ),
        (
            "codex disable 1",
            &[("OMO_CODEX_DISABLE_POSTHOG", "1")],
            true,
        ),
        (
            "codex disable true",
            &[("OMO_CODEX_DISABLE_POSTHOG", "true")],
            true,
        ),
        (
            "codex disable yes",
            &[("OMO_CODEX_DISABLE_POSTHOG", "yes")],
            true,
        ),
        (
            "codex send 0",
            &[("OMO_CODEX_SEND_ANONYMOUS_TELEMETRY", "0")],
            true,
        ),
        (
            "codex send false",
            &[("OMO_CODEX_SEND_ANONYMOUS_TELEMETRY", "false")],
            true,
        ),
        (
            "codex send no",
            &[("OMO_CODEX_SEND_ANONYMOUS_TELEMETRY", "no")],
            true,
        ),
        (
            "approved codex send yes convergence",
            &[("OMO_CODEX_SEND_ANONYMOUS_TELEMETRY", "yes")],
            true,
        ),
        (
            "invalid disable value",
            &[("OMO_CODEX_DISABLE_POSTHOG", "maybe")],
            false,
        ),
    ];
    for (name, pairs, expected) in cases {
        assert_eq!(disabled(pairs, "OMO_CODEX"), expected, "{name}");
    }
}

#[test]
fn do_not_track_one_disables_every_product() {
    for prefix in ["OMO_OPENCODE", "OMO_CODEX", "OMO_SENPI"] {
        assert!(disabled(&[("DO_NOT_TRACK", "1")], prefix), "{prefix}");
    }
}

#[test]
fn do_not_track_uses_flag_normalization_for_padded_mixed_case() {
    assert!(disabled(&[("DO_NOT_TRACK", " TRUE ")], "OMO_SENPI"));
}

#[test]
fn do_not_track_non_opt_out_values_keep_telemetry_enabled() {
    assert!(!disabled(&[("DO_NOT_TRACK", "0")], "OMO_SENPI"));
    assert!(!disabled(&[], "OMO_SENPI"));
}

#[test]
fn do_not_track_is_read_fresh_on_each_evaluation() {
    let mut env: HashMap<String, String> = HashMap::new();
    let mut evaluate = |value: &str| {
        env.insert("DO_NOT_TRACK".into(), value.into());
        should_disable_telemetry(&ShouldDisableTelemetryInput {
            env: Some(&env),
            global_env_prefix: None,
            product_env_prefix: "OMO_SENPI",
        })
    };

    let results = [evaluate("false"), evaluate("yes"), evaluate("")];

    assert_eq!(results, [false, true, false]);
}
