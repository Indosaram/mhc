use senpi_task::manager::credential_failure::{
    is_credential_failure, runtime_fallback_candidates, terminal_failure_message,
};
use senpi_task::state::{
    create_task_record, ResolvedModelRecord, ResolvedModelSource, TaskRecord, TaskRecordInput,
};

const SUBSCRIPTION_REQUIRED: &str =
    "403: {\"type\":\"server_error\",\"message\":\"Upstream request failed: An active OpenCode Go subscription is required to use Go models.\"}";
const REFRESH_REJECTED: &str =
    "OAuth refresh failed for anthropic: Anthropic token refresh request failed. status=400; body={\"error\": \"invalid_grant\", \"error_description\": \"Refresh token not found or invalid\"}";

fn rung(provider: &str, model_id: &str) -> ResolvedModelRecord {
    ResolvedModelRecord::new(ResolvedModelSource::Category, provider, model_id)
}

fn record(primary: &ResolvedModelRecord, fallbacks: &[ResolvedModelRecord]) -> TaskRecord {
    create_task_record(
        TaskRecordInput {
            parent_session_id: "parent-1".into(),
            root_session_id: "root-1".into(),
            depth: 1,
            execution_mode: "in-process".into(),
            model: primary.display.clone(),
            requested_model: Some(primary.clone()),
            resolved_model: Some(primary.clone()),
            fallback_models: Some(fallbacks.to_vec()),
            ..Default::default()
        },
        None,
    )
    .expect("record")
}

#[test]
fn non_credential_failure_with_no_fallbacks_preserves_error() {
    let go_m3 = rung("opencode-go", "minimax-m3");
    let rec = record(&go_m3, &[]);
    let msg = "500: upstream overloaded";
    assert!(!is_credential_failure(msg, Some("minimax-m3")));
    let candidates = runtime_fallback_candidates(&rec, msg);
    assert!(candidates.remaining.is_empty());
    assert_eq!(terminal_failure_message(Some(&rec), msg), msg);
}

#[test]
fn provider_error_retains_same_provider_sibling() {
    let go_m3 = rung("opencode-go", "minimax-m3");
    let go_m27 = rung("opencode-go", "minimax-m2.7");
    let zai_glm = rung("zai", "glm-5.3");
    let rec = record(&go_m3, &[go_m27.clone(), zai_glm.clone()]);
    let msg = "500: upstream overloaded";
    let candidates = runtime_fallback_candidates(&rec, msg);
    assert_eq!(candidates.remaining, vec![go_m27, zai_glm]);
    assert!(candidates.skipped.is_empty());
}

#[test]
fn lapsed_subscription_skips_same_provider_rungs() {
    let go_m3 = rung("opencode-go", "minimax-m3");
    let go_m27 = rung("opencode-go", "minimax-m2.7");
    let zai_glm = rung("zai", "glm-5.3");
    let rec = record(&go_m3, &[go_m27.clone(), zai_glm.clone()]);
    assert!(is_credential_failure(SUBSCRIPTION_REQUIRED, Some("minimax-m3")));
    let candidates = runtime_fallback_candidates(&rec, SUBSCRIPTION_REQUIRED);
    assert_eq!(candidates.remaining, vec![zai_glm]);
    assert_eq!(candidates.skipped, vec![go_m27]);
}

#[test]
fn missing_key_skips_api_key_sibling_and_runs_subscription_parent() {
    let luna = rung("openai", "gpt-6-luna-fast");
    let sibling = rung("openai", "another");
    let parent = rung("chatgpt-subscription", "gpt-6-luna-fast");
    let mut rec = record(&luna, &[sibling.clone(), parent.clone()]);
    rec.parent_model = Some(parent.display.clone());
    let missing = "No API key found for openai.\n\nUse /login to log into a provider via OAuth or API key.";
    assert!(is_credential_failure(missing, Some("gpt-6-luna-fast")));
    let candidates = runtime_fallback_candidates(&rec, missing);
    assert_eq!(candidates.remaining, vec![parent]);
    assert_eq!(candidates.skipped, vec![sibling]);
}

#[test]
fn rejected_oauth_refresh_fails_terminally_with_reauth_guidance() {
    let opus = rung("anthropic", "claude-opus-5-5");
    let opus_5 = rung("anthropic", "claude-opus-5");
    let rec = record(&opus, std::slice::from_ref(&opus_5));
    assert!(is_credential_failure(REFRESH_REJECTED, Some("claude-opus-5-5")));
    let candidates = runtime_fallback_candidates(&rec, REFRESH_REJECTED);
    assert!(candidates.remaining.is_empty());
    assert_eq!(candidates.skipped, vec![opus_5]);
    let terminal = terminal_failure_message(Some(&rec), REFRESH_REJECTED);
    assert!(terminal.starts_with(REFRESH_REJECTED));
    assert!(terminal.contains("/login anthropic"));
}
