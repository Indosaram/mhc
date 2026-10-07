mod support;

use std::collections::BTreeMap;
use std::sync::Arc;

use maho_ext_api::{ExtensionFailure, ProviderAuthStatus};
use maho_omo_model_profile::credential_policy::credential_rotation_policy;
use maho_omo_model_profile::request_auth::{
    AuthFailureReason, credential_slots, probe_request_auth, sanitized_auth_error_detail,
};
use support::{
    PRIVATE_MARKER, SHORT_SECRET, TestRegistry, account, credential_failure, model, oauth_failure,
    request_configuration_failed,
};

fn registry() -> Arc<TestRegistry> {
    Arc::new(TestRegistry::new(vec![model("anthropic", ""), model("zai", "glm-5.3")]))
}

/// One provider-scope failure on ONE slot (`None` = the flat/default credential).
fn slot_failure(
    provider: &str,
    slot: Option<&str>,
    failure: ExtensionFailure,
) -> BTreeMap<String, BTreeMap<Option<String>, ExtensionFailure>> {
    BTreeMap::from([(provider.to_owned(), BTreeMap::from([(slot.map(str::to_owned), failure)]))])
}

#[tokio::test]
async fn a_healthy_provider_and_model_resolve_without_a_failure() {
    let registry = registry();
    let failure = probe_request_auth(registry.as_ref(), true, "anthropic", "", &model("anthropic", "")).await;
    assert_eq!(failure, None);
    assert_eq!(registry.calls(), ["anthropic", "anthropic/"]);
}

#[tokio::test]
async fn a_rejected_refresh_reports_the_refresh_reason_from_the_typed_oauth_code() {
    let registry = Arc::new(TestRegistry::new(vec![model("anthropic", "")]).dead_provider("anthropic"));
    let failure = probe_request_auth(registry.as_ref(), true, "anthropic", "", &model("anthropic", ""))
        .await
        .expect("provider failure");
    assert_eq!(failure.reason, AuthFailureReason::Refresh);
    assert_eq!(failure.reason.as_str(), "refresh");
    // senpi `${error.name}/${error.code}`, read from the typed classification only.
    assert_eq!(failure.error_kind, "ModelsError/oauth");
    assert_eq!(registry.calls(), ["anthropic"]);
}

#[tokio::test]
async fn a_credential_failure_without_the_oauth_code_reports_the_credentials_reason() {
    let registry = Arc::new(TestRegistry {
        models: vec![model("anthropic", "")],
        dead_slots: slot_failure(
            "anthropic",
            None,
            credential_failure("Failed to resolve provider credential from shell command: echo x"),
        ),
        ..Default::default()
    });
    let failure = probe_request_auth(registry.as_ref(), true, "anthropic", "", &model("anthropic", ""))
        .await
        .expect("provider failure");
    assert_eq!(failure.reason, AuthFailureReason::Credentials);
    assert_eq!(failure.error_kind, "ModelsError");
}

#[tokio::test]
async fn the_fingerprint_never_reads_the_message_so_a_key_shaped_leading_token_cannot_leak() {
    // A message whose FIRST token is key material: the removed message fingerprint would have put
    // it into `error_kind`, which IS serialized into notices and warn details.
    let registry = Arc::new(TestRegistry {
        models: vec![model("anthropic", "")],
        dead_slots: slot_failure(
            "anthropic",
            None,
            ExtensionFailure::new(format!("{SHORT_SECRET} rejected")),
        ),
        ..Default::default()
    });
    let failure = probe_request_auth(registry.as_ref(), true, "anthropic", "", &model("anthropic", ""))
        .await
        .expect("provider failure");
    assert_eq!(failure.error_kind, "Error");
    assert!(!failure.error_kind.contains(SHORT_SECRET));
}

#[tokio::test]
async fn a_model_whose_request_configuration_fails_reports_only_that_model() {
    let registry = Arc::new(TestRegistry::new(vec![model("anthropic", "")]).broken_model("anthropic", ""));
    let failure = probe_request_auth(registry.as_ref(), true, "anthropic", "", &model("anthropic", ""))
        .await
        .expect("request failure");
    assert_eq!(failure.reason, AuthFailureReason::Request);
    assert_eq!(failure.provider, "anthropic");
    assert_eq!(failure.model, "");
    assert_eq!(registry.calls(), ["anthropic", "anthropic/"]);
}

#[tokio::test]
async fn credential_slots_enumerate_the_named_accounts_and_the_pinned_one() {
    let registry = Arc::new(
        TestRegistry::new(vec![model("anthropic", "")]).with_accounts(
            "anthropic",
            vec![account("healthy", false), account("expired", true)],
        ),
    );
    let slots = credential_slots(registry.as_ref(), "anthropic").await;
    assert_eq!(slots.names, ["healthy", "expired"]);
    assert_eq!(slots.pinned.as_deref(), Some("expired"));
}

#[tokio::test]
async fn a_provider_with_no_pool_reports_no_slots() {
    let registry = registry();
    let slots = credential_slots(registry.as_ref(), "anthropic").await;
    assert!(slots.names.is_empty());
    assert_eq!(slots.pinned, None);
}

#[tokio::test]
async fn rotation_disabled_in_models_json_turns_rotation_off_for_that_provider_only() {
    let root = tempfile::tempdir().expect("temp dir");
    std::fs::write(
        root.path().join("models.json"),
        "{\n  // JSONC, as senpi reads it\n  \"providers\": { \"anthropic\": { \"credentials\": { \"rotation\": false } } }\n}\n",
    )
    .expect("models.json");
    let registry = TestRegistry::new(vec![model("anthropic", ""), model("zai", "glm-5.3")]);
    let policy = credential_rotation_policy(&registry, Some(root.path()));
    assert!(!policy.may_rotate("anthropic"));
    assert!(policy.may_rotate("zai"));
}

#[tokio::test]
async fn a_registry_reporting_the_runtime_source_turns_rotation_off_for_that_provider() {
    let registry = TestRegistry::new(vec![model("anthropic", ""), model("zai", "glm-5.3")]).with_auth_status(
        "anthropic",
        ProviderAuthStatus { configured: true, source: Some("runtime".into()), label: None },
    );
    let policy = credential_rotation_policy(&registry, None);
    assert!(!policy.may_rotate("anthropic"));
    assert!(policy.may_rotate("zai"));
}

#[tokio::test]
async fn a_registry_reporting_no_runtime_source_keeps_rotation_on() {
    let registry = TestRegistry::new(vec![model("anthropic", "")]);
    let policy = credential_rotation_policy(&registry, None);
    assert!(policy.may_rotate("anthropic"));
}

#[tokio::test]
async fn the_sanitized_detail_drops_the_private_marker_the_short_secret_and_the_url_secrets() {
    let raw = format!(
        "OAuth refresh failed for anthropic: Authorization: Bearer {SHORT_SECRET} {{\"access_token\":\"{SHORT_SECRET}\"}} url=https://example.invalid/oauth/token?refresh_token={SHORT_SECRET}; details=Error: body={{\"error\":\"invalid_grant\",\"marker\":\"{PRIVATE_MARKER}\"}}"
    );
    let detail = sanitized_auth_error_detail(Some(&oauth_failure(raw)));
    assert!(!detail.contains(PRIVATE_MARKER), "private marker redacted: {detail}");
    assert!(!detail.contains(SHORT_SECRET), "short secret redacted: {detail}");
    assert!(!detail.contains("invalid_grant"), "response body redacted: {detail}");
    assert!(detail.starts_with("ModelsError: "), "the class name survives verbatim: {detail}");
    assert!(detail.contains("<redacted>"));
    assert!(detail.chars().count() <= 240);
}

#[tokio::test]
async fn a_shell_command_in_a_request_failure_is_redacted_whole() {
    let raw = request_configuration_failed("anthropic", "");
    let detail = sanitized_auth_error_detail(Some(&credential_failure(raw)));
    assert!(detail.contains("shell command: <redacted>"), "{detail}");
    assert!(!detail.contains(PRIVATE_MARKER));
    assert!(!detail.contains(SHORT_SECRET));
}

#[tokio::test]
async fn an_absent_error_yields_an_empty_detail() {
    assert_eq!(sanitized_auth_error_detail(None), "");
}

#[tokio::test]
async fn a_pool_rotates_onto_a_healthy_sibling_slot_and_threads_that_slot_into_the_model_probe() {
    let registry = Arc::new(
        TestRegistry::new(vec![model("anthropic", "")])
            .dead_provider("anthropic")
            .dead_slot("anthropic", "expired")
            .with_accounts("anthropic", vec![account("expired", false), account("healthy", false)]),
    );
    let failure = probe_request_auth(registry.as_ref(), true, "anthropic", "", &model("anthropic", "")).await;
    assert_eq!(failure, None);
    // The flat credential is never used: the walk probes the pool's slots in order, and the model
    // scope carries the slot that RESOLVED.
    assert_eq!(registry.calls(), ["anthropic#expired", "anthropic#healthy", "anthropic/#healthy"]);
}

#[tokio::test]
async fn a_pinned_pool_probes_only_the_pinned_slot_and_reports_its_refresh_failure() {
    let registry = Arc::new(
        TestRegistry::new(vec![model("anthropic", "")])
            .dead_slot("anthropic", "expired")
            .with_accounts("anthropic", vec![account("healthy", false), account("expired", true)]),
    );
    let failure = probe_request_auth(registry.as_ref(), true, "anthropic", "", &model("anthropic", ""))
        .await
        .expect("provider failure");
    assert_eq!(failure.reason, AuthFailureReason::Refresh);
    assert_eq!(failure.error_kind, "ModelsError/oauth");
    assert_eq!(registry.calls(), ["anthropic#expired"]);
}

#[tokio::test]
async fn rotation_off_keeps_the_flat_credential_path() {
    let registry = Arc::new(
        TestRegistry::new(vec![model("anthropic", "")])
            .dead_provider("anthropic")
            .with_accounts("anthropic", vec![account("expired", false), account("healthy", false)]),
    );
    let failure = probe_request_auth(registry.as_ref(), false, "anthropic", "", &model("anthropic", ""))
        .await
        .expect("provider failure");
    assert_eq!(failure.reason, AuthFailureReason::Refresh);
    assert_eq!(registry.calls(), ["anthropic"]);
}

#[tokio::test]
async fn a_registry_reporting_the_runtime_source_so_only_the_flat_credential_is_probed() {
    let registry = Arc::new(
        TestRegistry::new(vec![model("anthropic", "")])
            .dead_provider("anthropic")
            .with_accounts("anthropic", vec![account("expired", false), account("healthy", false)])
            .with_auth_status(
                "anthropic",
                ProviderAuthStatus { configured: true, source: Some("runtime".into()), label: None },
            ),
    );
    let policy = credential_rotation_policy(registry.as_ref(), None);
    let failure = probe_request_auth(
        registry.as_ref(),
        policy.may_rotate("anthropic"),
        "anthropic",
        "",
        &model("anthropic", ""),
    )
    .await
    .expect("provider failure");
    assert_eq!(failure.reason, AuthFailureReason::Refresh);
    assert_eq!(registry.calls(), ["anthropic"]);
}
