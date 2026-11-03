//! Tests for the v5.1.52 delta of spec model-selection (D1-D4).

use maho_ext_api::NotificationType;
use maho_model_select::ext::{
    APPLIED_TYPE, OutcomeHost, UNAVAILABLE_TYPE, deliver_report, registry_missing_report,
    report_for,
};
use maho_model_select::probe::{
    Account, AuthProbe, ProbeError, ProbeFailure, ProbeReason, RefreshFailure,
    probe_and_resolve_detailed,
};
use maho_model_select::profile::gate_rungs;
use maho_model_select::*;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::future::Future;
use std::sync::Mutex;

fn list(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

fn heavy(profiles: &[ModelProfile]) -> &ModelProfile {
    profiles
        .iter()
        .find(|p| p.id == "daily-heavy")
        .expect("daily-heavy")
}

// ---- D1: launch-gated rungs ------------------------------------------------------------

#[test]
fn gated_id_comparison_normalises_dots_and_provider() {
    assert!(is_launch_gated("claude-fable-5-5"));
    assert!(is_launch_gated("claude-fable-5.5"));
    assert!(is_launch_gated("anthropic/claude-fable-5.5"));
    assert!(!is_launch_gated("claude-fable-5-1"));
}

#[test]
fn flag_off_removes_the_gated_rung_everywhere() {
    let mut warnings = Vec::new();
    let built = build_builtin_profiles(false, &mut |m: &str| warnings.push(m.to_owned()));
    assert!(built.dropped.is_empty());
    assert!(warnings.is_empty());
    for profile in &built.profiles {
        assert!(profile.rungs.iter().all(|r| !is_launch_gated(&r.model)));
    }
    // The shipped flag is off, so the default builder matches.
    assert_eq!(builtin_profiles(), built.profiles);

    // Starts with the same-family rung at xhigh on the Anthropic providers.
    let first = &heavy(&built.profiles).rungs[0];
    assert!(first.model.starts_with("claude-fable-"));
    assert_eq!(first.reasoning.as_deref(), Some("xhigh"));
    assert_eq!(first.providers[0], "anthropic-subscription");

    let profiles = merge_profiles(None);
    let available = list(&["anthropic-subscription/claude-fable-5-5"]);
    let r = resolve_profile(&profiles, "daily-heavy", &available, &Exclusions::default());
    match &r {
        ProfileResolution::Resolved {
            model_id, skipped, ..
        } => {
            assert!(!is_launch_gated(model_id));
            assert!(skipped.iter().all(|s| !is_launch_gated(s)));
        }
        ProfileResolution::Unavailable { chain, .. } => {
            assert!(chain.iter().all(|s| !is_launch_gated(s)));
        }
        other => panic!("{other:?}"),
    }
    if let ProfileResolution::Resolved { model_id, .. } =
        resolve_default(&profiles, &available, &Exclusions::default())
    {
        assert!(!is_launch_gated(&model_id));
    }
}

#[test]
fn flag_on_with_xhigh_starts_with_the_gated_rung() {
    let mut warnings = Vec::new();
    let built = build_builtin_profiles(true, &mut |m: &str| warnings.push(m.to_owned()));
    let first = &heavy(&built.profiles).rungs[0];
    assert!(is_launch_gated(&first.model));
    assert_eq!(first.reasoning.as_deref(), Some("xhigh"));
    assert!(built.dropped.is_empty());
    assert!(warnings.is_empty());
}

#[test]
fn flag_on_rejects_a_gated_rung_at_another_level() {
    let rung = |model: &str, level: Option<&str>| ProfileRung {
        providers: list(&["anthropic"]),
        model: model.into(),
        reasoning: level.map(str::to_owned),
    };
    let mut dropped = Vec::new();
    let mut warnings = Vec::new();
    let kept = gate_rungs(
        "daily-heavy",
        vec![
            rung("claude-fable-5-5", Some("medium")),
            rung("claude-fable-5-1", Some("xhigh")),
            rung("claude-opus-5-5", Some("high")),
        ],
        true,
        &mut dropped,
        &mut |m: &str| warnings.push(m.to_owned()),
    );
    assert_eq!(
        kept.iter().map(|r| r.model.as_str()).collect::<Vec<_>>(),
        ["claude-fable-5-1", "claude-opus-5-5"]
    );
    assert_eq!(dropped.len(), 1);
    assert_eq!(dropped[0].reasoning.as_deref(), Some("medium"));
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("medium"));

    // `max` is accepted; no level is rejected.
    let mut dropped = Vec::new();
    let kept = gate_rungs(
        "p",
        vec![
            rung("claude-fable-5.5", Some("max")),
            rung("claude-fable-5-5", None),
        ],
        true,
        &mut dropped,
        &mut |_| {},
    );
    assert_eq!(kept.len(), 1);
    assert_eq!(dropped.len(), 1);
}

#[test]
fn user_profiles_naming_the_gated_id_are_not_filtered() {
    let profiles = merge_profiles(Some(
        &json!({"mine": {"models": ["anthropic/claude-fable-5-5:medium"]}}),
    ));
    let mine = profiles.iter().find(|p| p.id == "mine").expect("mine");
    assert!(is_launch_gated(&mine.rungs[0].model));
    let r = resolve_profile(
        &profiles,
        "mine",
        &list(&["anthropic/claude-fable-5-5"]),
        &Exclusions::default(),
    );
    assert!(
        matches!(r, ProfileResolution::Resolved { ref model_id, .. } if model_id == "claude-fable-5-5")
    );
}

// ---- D2: outcomes are session entries, not model input ------------------------------------

#[derive(Default)]
struct RecordingHost {
    ui: bool,
    append_fails: bool,
    entries: Mutex<Vec<(String, Value)>>,
    notifications: Mutex<Vec<(String, NotificationType)>>,
    logs: Mutex<Vec<String>>,
}

impl OutcomeHost for RecordingHost {
    fn append_entry(&self, custom_type: &str, details: Value) -> Result<(), String> {
        if self.append_fails {
            return Err("not bound".into());
        }
        self.entries
            .lock()
            .expect("lock")
            .push((custom_type.to_owned(), details));
        Ok(())
    }
    fn has_ui(&self) -> bool {
        self.ui
    }
    fn notify(&self, text: &str, kind: NotificationType) {
        self.notifications
            .lock()
            .expect("lock")
            .push((text.to_owned(), kind));
    }
    fn log(&self, line: &str) {
        self.logs.lock().expect("lock").push(line.to_owned());
    }
}

fn applied() -> ProfileResolution {
    ProfileResolution::Resolved {
        profile: "daily-normal".into(),
        provider: "anthropic".into(),
        model_id: "claude-opus-5-5".into(),
        reasoning: Some("medium".into()),
        skipped: Vec::new(),
        pinned: false,
    }
}

fn auth_failure() -> ProbeFailure {
    ProbeFailure {
        provider: "a".into(),
        model: "m".into(),
        reason: ProbeReason::Refresh,
        fingerprint: "AuthError:oauth".into(),
    }
}

#[test]
fn clean_applied_outcome_is_one_entry_and_no_notification() {
    let host = RecordingHost {
        ui: true,
        ..RecordingHost::default()
    };
    let profiles = merge_profiles(None);
    deliver_report(&host, &report_for(&applied(), &profiles, &[], false, true));
    let entries = host.entries.lock().expect("lock");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].0, APPLIED_TYPE);
    assert_eq!(entries[0].1["model"], "anthropic/claude-opus-5-5");
    assert!(host.notifications.lock().expect("lock").is_empty());
    assert_eq!(host.logs.lock().expect("lock").len(), 1);
}

#[test]
fn applied_with_an_auth_failure_warns_once() {
    let host = RecordingHost {
        ui: true,
        ..RecordingHost::default()
    };
    let profiles = merge_profiles(None);
    deliver_report(
        &host,
        &report_for(&applied(), &profiles, &[auth_failure()], false, true),
    );
    let notes = host.notifications.lock().expect("lock");
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].1, NotificationType::Warning);
}

#[test]
fn unavailable_default_without_auth_failures_is_info() {
    let host = RecordingHost {
        ui: true,
        ..RecordingHost::default()
    };
    let profiles = merge_profiles(None);
    let r = resolve_default(&profiles, &[], &Exclusions::default());
    deliver_report(&host, &report_for(&r, &profiles, &[], false, true));
    let notes = host.notifications.lock().expect("lock");
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].1, NotificationType::Info);
    assert_eq!(host.entries.lock().expect("lock")[0].0, UNAVAILABLE_TYPE);

    // Configured (not default) or with auth failures → warning.
    let configured = report_for(&r, &profiles, &[], false, false);
    assert_eq!(configured.notification, Some(NotificationType::Warning));
    let failed = report_for(&r, &profiles, &[auth_failure()], false, true);
    assert_eq!(failed.notification, Some(NotificationType::Warning));
    let unknown = resolve_profile(&profiles, "nope", &[], &Exclusions::default());
    assert_eq!(
        report_for(&unknown, &profiles, &[], false, false).notification,
        Some(NotificationType::Warning)
    );
}

#[test]
fn headless_host_records_the_entry_and_skips_the_notification() {
    let host = RecordingHost::default();
    let profiles = merge_profiles(None);
    deliver_report(
        &host,
        &report_for(&applied(), &profiles, &[auth_failure()], false, true),
    );
    assert_eq!(host.entries.lock().expect("lock").len(), 1);
    assert!(host.notifications.lock().expect("lock").is_empty());
    assert_eq!(host.logs.lock().expect("lock").len(), 1);
}

#[test]
fn runtime_without_entries_logs_and_notifies() {
    let host = RecordingHost {
        ui: true,
        append_fails: true,
        ..RecordingHost::default()
    };
    let profiles = merge_profiles(None);
    deliver_report(
        &host,
        &report_for(&applied(), &profiles, &[auth_failure()], false, true),
    );
    assert!(host.entries.lock().expect("lock").is_empty());
    assert_eq!(host.logs.lock().expect("lock").len(), 2);
    assert_eq!(host.notifications.lock().expect("lock").len(), 1);
}

#[test]
fn registry_missing_is_one_warning() {
    let host = RecordingHost {
        ui: true,
        ..RecordingHost::default()
    };
    deliver_report(
        &host,
        &registry_missing_report("daily-heavy", "p", "m", false),
    );
    let notes = host.notifications.lock().expect("lock");
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].1, NotificationType::Warning);
}

// ---- D3: notice facts ------------------------------------------------------------------

#[test]
fn unavailable_next_step_depends_on_the_run_mode() {
    let profiles = merge_profiles(None);
    let r = resolve_default(&profiles, &[], &Exclusions::default());
    let rpc = report_for(&r, &profiles, &[], true, true).text;
    assert!(rpc.contains("provider settings"), "{rpc}");
    assert!(rpc.contains("anthropic-subscription"), "{rpc}");
    assert!(!rpc.contains("/login"), "{rpc}");
    let print = report_for(&r, &profiles, &[], false, true).text;
    assert!(print.contains("/login"), "{print}");
    assert!(print.contains("--model"), "{print}");
}

#[test]
fn registry_missing_notice_facts() {
    for rpc in [true, false] {
        let report = registry_missing_report("daily-heavy", "anthropic", "claude-opus-5-5", rpc);
        assert!(report.text.contains("daily-heavy"));
        assert!(report.text.contains("anthropic/claude-opus-5-5"));
        assert!(report.text.contains("keeps its current model"));
        assert!(report.text.contains("mhc"));
        assert_eq!(report.text.contains("/model"), !rpc);
        assert_eq!(report.text.contains("settings"), rpc);
    }
}

// ---- D4: transient OAuth refresh ---------------------------------------------------------

#[derive(Default)]
struct FakeProbe {
    provider_errors: HashMap<String, ProbeError>,
    request_errors: HashMap<String, ProbeError>,
}

impl AuthProbe for FakeProbe {
    async fn rotation_allowed(&self, _provider: &str) -> bool {
        false
    }
    async fn accounts(&self, _provider: &str) -> Vec<Account> {
        Vec::new()
    }
    fn provider_auth(
        &self,
        provider: &str,
        _slot: Option<&str>,
    ) -> impl Future<Output = Result<(), ProbeError>> + Send {
        let result = self
            .provider_errors
            .get(provider)
            .cloned()
            .map_or(Ok(()), Err);
        async move { result }
    }
    fn request_auth(
        &self,
        provider: &str,
        model: &str,
        _slot: Option<&str>,
    ) -> impl Future<Output = Result<(), ProbeError>> + Send {
        let result = self
            .request_errors
            .get(&format!("{provider}/{model}"))
            .cloned()
            .map_or(Ok(()), Err);
        async move { result }
    }
}

fn refresh_error(refresh: RefreshFailure) -> ProbeError {
    ProbeError {
        class: Some("AuthError".into()),
        code: Some("oauth".into()),
        refresh: Some(refresh),
        cause: None,
    }
}

/// A transient refresh wrapped by another error.
fn wrapped_transient() -> ProbeError {
    ProbeError {
        class: Some("ProviderAuthError".into()),
        code: Some("auth".into()),
        refresh: None,
        cause: Some(Box::new(refresh_error(RefreshFailure::Transient {
            transport: "http_503".into(),
        }))),
    }
}

fn lane() -> Vec<ModelProfile> {
    merge_profiles(Some(&json!({"lane": {"models": ["a/m1", "b/m2"]}})))
}

#[tokio::test]
async fn transient_refresh_keeps_the_provider() {
    let probe = FakeProbe {
        provider_errors: HashMap::from([("a".to_string(), wrapped_transient())]),
        ..FakeProbe::default()
    };
    let profiles = lane();
    let available = list(&["a/m1", "b/m2"]);
    let outcome = probe_and_resolve_detailed(&probe, |ex| {
        resolve_profile(&profiles, "lane", &available, ex)
    })
    .await;
    assert!(
        matches!(&outcome.result, ProfileResolution::Resolved { provider, .. } if provider == "a")
    );
    assert!(outcome.failures.is_empty());
    let report = report_for(&outcome.result, &profiles, &outcome.failures, false, false);
    assert_eq!(report.custom_type, APPLIED_TYPE);
    assert_eq!(report.details["authFailures"], json!([]));
    assert_eq!(outcome.kept.len(), 1);
    let line = outcome.kept[0].warning_line();
    assert!(line.contains("a/m1"), "{line}");
    assert!(line.contains("ProviderAuthError:auth"), "{line}");
    assert!(line.contains("http_503"), "{line}");
    assert!(outcome.kept[0].model_checked);
}

#[tokio::test]
async fn transient_refresh_with_failing_headers_excludes_only_the_model() {
    let probe = FakeProbe {
        provider_errors: HashMap::from([("a".to_string(), wrapped_transient())]),
        request_errors: HashMap::from([(
            "a/m1".to_string(),
            ProbeError {
                class: Some("HeaderError".into()),
                ..ProbeError::default()
            },
        )]),
    };
    let profiles = merge_profiles(Some(&json!({"lane": {"models": ["a/m1", "a/m3"]}})));
    let available = list(&["a/m1", "a/m3"]);
    let outcome = probe_and_resolve_detailed(&probe, |ex| {
        resolve_profile(&profiles, "lane", &available, ex)
    })
    .await;
    assert_eq!(outcome.failures.len(), 1);
    assert_eq!(outcome.failures[0].reason, ProbeReason::Request);
    assert_eq!(outcome.failures[0].model, "m1");
    assert!(
        matches!(&outcome.result, ProfileResolution::Resolved { model_id, .. } if model_id == "m3")
    );
}

#[tokio::test]
async fn transient_refresh_in_the_model_check_keeps_without_a_scope_result() {
    let probe = FakeProbe {
        provider_errors: HashMap::from([("a".to_string(), wrapped_transient())]),
        request_errors: HashMap::from([("a/m1".to_string(), wrapped_transient())]),
    };
    let profiles = lane();
    let available = list(&["a/m1", "b/m2"]);
    let outcome = probe_and_resolve_detailed(&probe, |ex| {
        resolve_profile(&profiles, "lane", &available, ex)
    })
    .await;
    assert!(outcome.failures.is_empty());
    assert!(
        matches!(&outcome.result, ProfileResolution::Resolved { provider, .. } if provider == "a")
    );
    assert!(!outcome.kept[0].model_checked);
}

#[tokio::test]
async fn refused_refresh_still_excludes_the_provider() {
    for error in [
        refresh_error(RefreshFailure::Refused),
        ProbeError {
            class: Some("AuthError".into()),
            code: Some("oauth".into()),
            ..ProbeError::default()
        },
    ] {
        let probe = FakeProbe {
            provider_errors: HashMap::from([("a".to_string(), error)]),
            ..FakeProbe::default()
        };
        let profiles = lane();
        let available = list(&["a/m1", "b/m2"]);
        let outcome = probe_and_resolve_detailed(&probe, |ex| {
            resolve_profile(&profiles, "lane", &available, ex)
        })
        .await;
        assert!(
            matches!(&outcome.result, ProfileResolution::Resolved { provider, .. } if provider == "b")
        );
        assert_eq!(outcome.failures.len(), 1);
        assert_eq!(outcome.failures[0].reason, ProbeReason::Refresh);
        assert!(outcome.kept.is_empty());
    }
}
