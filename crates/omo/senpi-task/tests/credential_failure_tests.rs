use senpi_task::manager::credential_failure::{
    UsageLimitScope, is_credential_failure, runtime_fallback_candidates, terminal_failure_message,
};
use senpi_task::state::{
    ResolvedModelRecord, ResolvedModelSource, TaskRecord, TaskRecordInput, create_task_record,
};

fn rung(provider: &str, model_id: &str) -> ResolvedModelRecord {
    ResolvedModelRecord::new(ResolvedModelSource::Category, provider, model_id)
}

fn record_with(primary: &ResolvedModelRecord, fallbacks: &[ResolvedModelRecord]) -> TaskRecord {
    create_task_record(
        TaskRecordInput {
            parent_session_id: "session-parent".into(),
            root_session_id: "session-root".into(),
            depth: 1,
            execution_mode: "in-process".into(),
            notify_on_terminal: false,
            model: primary.display.clone(),
            requested_model: Some(primary.clone()),
            resolved_model: Some(primary.clone()),
            fallback_models: Some(fallbacks.to_vec()),
            ..Default::default()
        },
        None,
    )
    .expect("task record")
}

#[test]
fn rejected_credentials_are_credential_failures() {
    let cases = [
        "401: Unauthorized",
        "401: {\"error\":{\"message\":\"Invalid API key provided\"}}",
        "OAuth refresh failed for anthropic: status=400; body={\"error\": \"invalid_grant\"}",
        "403: {\"message\":\"An active OpenCode Go subscription is required to use Go models.\"}",
        "403: Your organization does not have access to this API",
        "403: This API key does not have permission to use the completions endpoint",
        "403 Forbidden: this account has been suspended",
    ];
    for msg in cases {
        assert!(is_credential_failure(msg, None), "{msg}");
    }
}

#[test]
fn model_scoped_403_are_not_credential_failures() {
    let cases = [
        "403: Your organization must be verified to use this model",
        "403: The API key is valid, but access to restricted-model is forbidden. Use an allowed model.",
        "403: account tier does not include minimax-m3",
    ];
    for msg in cases {
        assert!(!is_credential_failure(msg, Some("minimax-m3")), "{msg}");
    }
}

#[test]
fn non_credential_errors_are_not_credential_failures() {
    let cases = [
        "403: {\"error\":{\"message\":\"This model is not enabled for your project. Contact support to request access.\"}}",
        "403 Forbidden",
        "403: gpt-6-astra is not available in your region",
        "500: upstream overloaded",
        "429: rate limited, retry after 3s",
        "400: prompt is too long: 213000 tokens > 200000 maximum",
    ];
    for msg in cases {
        assert!(!is_credential_failure(msg, None), "{msg}");
    }
}

#[test]
fn model_scoped_403_retains_sibling_model() {
    let go_m3 = rung("opencode-go", "minimax-m3");
    let go_m27 = rung("opencode-go", "minimax-m2.7");
    let zai_glm = rung("zai", "glm-5.3");
    let rec = record_with(&go_m3, &[go_m27.clone(), zai_glm.clone()]);
    let candidates =
        runtime_fallback_candidates(&rec, "403: This model is not enabled for your project.");
    assert_eq!(candidates.remaining, vec![go_m27, zai_glm]);
    assert!(candidates.skipped.is_empty());
}

#[test]
fn model_scoped_403_naming_org_or_key_preserves_sibling_and_message() {
    let go_m3 = rung("opencode-go", "minimax-m3");
    let go_m27 = rung("opencode-go", "minimax-m2.7");
    let zai_glm = rung("zai", "glm-5.3");
    for msg in [
        "403: Your organization must be verified to use this model",
        "403: The API key is valid, but access to restricted-model is forbidden. Use an allowed model.",
    ] {
        let rec = record_with(&go_m3, &[go_m27.clone(), zai_glm.clone()]);
        let candidates = runtime_fallback_candidates(&rec, msg);
        assert_eq!(candidates.remaining, vec![go_m27.clone(), zai_glm.clone()]);
        assert!(candidates.skipped.is_empty());
        let terminal = record_with(&go_m3, &[]);
        assert_eq!(terminal_failure_message(Some(&terminal), msg), msg);
    }
}

#[test]
fn missing_key_on_api_key_rung_skips_sibling_and_falls_back_to_parent() {
    let selected = rung("openai", "gpt-6-luna-fast");
    let sibling = rung("openai", "another");
    let parent = rung("chatgpt-subscription", "gpt-6-luna-fast");
    let mut rec = record_with(&selected, &[sibling.clone(), parent.clone()]);
    rec.parent_model = Some(parent.display.clone());
    let candidates = runtime_fallback_candidates(&rec, "No API key found for openai.");
    assert_eq!(candidates.remaining, vec![parent]);
    assert_eq!(candidates.skipped, vec![sibling]);
}

#[test]
fn credential_rejection_skips_entire_provider() {
    let go_m3 = rung("opencode-go", "minimax-m3");
    let go_m27 = rung("opencode-go", "minimax-m2.7");
    let zai_glm = rung("zai", "glm-5.3");
    let rec = record_with(&go_m3, &[go_m27.clone(), zai_glm.clone()]);
    let candidates = runtime_fallback_candidates(&rec, "401: Unauthorized");
    assert_eq!(candidates.remaining, vec![zai_glm]);
    assert_eq!(candidates.skipped, vec![go_m27]);
}

#[test]
fn account_usage_limits_deprioritize_spent_provider() {
    let go_m3 = rung("opencode-go", "minimax-m3");
    let go_m27 = rung("opencode-go", "minimax-m2.7");
    let zai_glm = rung("zai", "glm-5.3");
    for msg in [
        "You've hit your session limit · resets 3pm (Asia/Seoul)",
        "429 Monthly usage limit reached. It will reset in 2 days 8 hours",
        "429 {\"type\":\"usage_limit_reached\",\"message\":\"The usage limit has been reached\"}",
        "The anthropic-subscription account usage limit has been reached (resets 3:20am). Switch to another provider with /model, or configure another provider in retry.fallbackChains.",
    ] {
        let rec = record_with(&go_m3, &[go_m27.clone(), zai_glm.clone()]);
        let candidates = runtime_fallback_candidates(&rec, msg);
        assert_eq!(candidates.remaining, vec![zai_glm.clone(), go_m27.clone()]);
        assert!(candidates.skipped.is_empty());
        assert_eq!(candidates.limit, Some(UsageLimitScope::Account));
    }
}

#[test]
fn model_scoped_usage_limit_retains_sibling_first() {
    let go_m3 = rung("opencode-go", "minimax-m3");
    let go_m27 = rung("opencode-go", "minimax-m2.7");
    let zai_glm = rung("zai", "glm-5.3");
    let rec = record_with(&go_m3, &[go_m27.clone(), zai_glm.clone()]);
    let candidates = runtime_fallback_candidates(
        &rec,
        "You've hit your Fable weekly limit · resets Oct 2, 9am",
    );
    assert_eq!(candidates.remaining, vec![go_m27, zai_glm]);
    assert!(candidates.skipped.is_empty());
    assert_eq!(candidates.limit, Some(UsageLimitScope::Model));
}

#[test]
fn parent_session_model_retention_and_exclusion() {
    let go_m3 = rung("opencode-go", "minimax-m3");
    let zai_glm = rung("zai", "glm-5.3");
    let parent = rung("chatgpt-subscription", "gpt-6.1-sol");
    let spent_parent = rung("opencode-go", "minimax-m2.7");

    for msg in [
        "429 Monthly usage limit reached. It will reset in 2 days 8 hours",
        "401: Unauthorized",
    ] {
        let mut rec = record_with(&go_m3, &[zai_glm.clone(), parent.clone()]);
        rec.parent_model = Some(parent.display.clone());
        let candidates = runtime_fallback_candidates(&rec, msg);
        assert_eq!(candidates.remaining, vec![zai_glm.clone(), parent.clone()]);
    }

    let mut rec = record_with(&go_m3, &[zai_glm.clone(), parent.clone()]);
    rec.parent_model = Some(parent.display.clone());
    let candidates = runtime_fallback_candidates(&rec, "500: upstream overloaded");
    assert_eq!(candidates.remaining, vec![zai_glm.clone()]);

    let mut rec_spent = record_with(&go_m3, &[zai_glm.clone(), spent_parent.clone()]);
    rec_spent.parent_model = Some(spent_parent.display.clone());
    let candidates = runtime_fallback_candidates(
        &rec_spent,
        "429 Monthly usage limit reached. It will reset in 2 days 8 hours",
    );
    assert_eq!(candidates.remaining, vec![zai_glm]);
    assert_eq!(candidates.skipped, vec![spent_parent]);
    assert_eq!(candidates.limit, Some(UsageLimitScope::Account));
}

#[test]
fn terminal_failure_message_formats_reauthentication_guidance() {
    let go_m3 = rung("opencode-go", "minimax-m3");
    let rec = record_with(&go_m3, &[]);
    let rejection = terminal_failure_message(Some(&rec), "401: Unauthorized");
    assert!(rejection.contains("/login opencode-go"));
    assert!(rejection.contains("omo.json"));

    let missing = terminal_failure_message(Some(&rec), "No API key found for opencode-go.");
    assert!(missing.contains("/login opencode-go"));
}

#[test]
fn terminal_failure_message_formats_usage_limit_exhaustion() {
    let astra = rung("chatgpt-subscription", "gpt-6-astra");
    let rec_exhausted = record_with(&astra, &[]);
    let limit = "403: {\"message\":\"You've reached your usage limit for this billing cycle.\",\"type\":\"access_terminated_error\"}";
    let msg = terminal_failure_message(Some(&rec_exhausted), limit);
    assert!(msg.starts_with(limit));
    assert!(msg.contains("chatgpt-subscription/gpt-6-astra"));
    assert!(msg.contains("omo.json"));

    let go_m3 = rung("opencode-go", "minimax-m3");
    let zai_glm = rung("zai", "glm-5.3");
    let rec_remaining = record_with(&go_m3, &[zai_glm]);
    let msg_remaining =
        terminal_failure_message(Some(&rec_remaining), "You've hit your session limit");
    assert!(msg_remaining.contains("opencode-go/minimax-m3"));
    assert!(
        !runtime_fallback_candidates(&rec_remaining, "You've hit your session limit")
            .remaining
            .is_empty()
    );
}
