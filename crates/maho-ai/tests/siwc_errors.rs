//! Adapter and assistant-retry cases from packages/coding-agent/test/suite/regressions/5003-siwc-errors.test.ts.

use maho_ai::types::{
    ProviderDiagnosticCategory as Category, ProviderDiagnosticEvidence as Evidence,
    ProviderDiagnosticRoute, StopReason,
};
use maho_ai::utils::retry::is_retryable_assistant_error;
use maho_ai::utils::siwc_errors::{SiwcRecovery as Recovery, siwc_recovery_from_diagnostic};
use serde_json::json;

#[path = "support/siwc_reply.rs"]
mod siwc_reply;
use siwc_reply::{run, sse};

const CASES: [(&str, u16, Category, Recovery); 9] = [
    (
        "subscription_sharing_usage_limit_exceeded",
        429,
        Category::Quota,
        Recovery::UsageLimit,
    ),
    (
        "subscription_sharing_user_not_eligible",
        403,
        Category::Unknown,
        Recovery::AccountDisabled,
    ),
    (
        "subscription_sharing_usage_unavailable",
        503,
        Category::ProviderUnavailable,
        Recovery::Transient,
    ),
    (
        "subscription_sharing_user_unavailable",
        503,
        Category::ProviderUnavailable,
        Recovery::Transient,
    ),
    (
        "subscription_sharing_invalid_user",
        401,
        Category::Auth,
        Recovery::Auth,
    ),
    (
        "subscription_sharing_unsupported_capability",
        400,
        Category::InvalidRequest,
        Recovery::Request,
    ),
    (
        "subscription_sharing_route_not_supported",
        403,
        Category::InvalidRequest,
        Recovery::Request,
    ),
    (
        "chatpass_v2_scope_not_authorized",
        403,
        Category::InvalidRequest,
        Recovery::Request,
    ),
    (
        "chatpass_v2_invalid_authorization_context",
        403,
        Category::InvalidRequest,
        Recovery::Request,
    ),
];

#[tokio::test]
async fn classifies_exact_codes_through_http_failed_and_error_frames_before_conflicting_prose() {
    for form in ["http", "failed", "error"] {
        for (code, status, category, recovery) in CASES {
            let error = json!({
                "code":code, "type":"invalid_request_error", "param":"temperature",
                "message":"billing token unauthorized model_not_found aborted 503 retry later; resets in 999 hours"
            });
            let (status, body) = match form {
                "http" => (status, json!({"error":error}).to_string()),
                "failed" => (
                    200,
                    sse(&[
                        json!({"type":"response.failed","response":{"status":"failed","error":error}}),
                    ]),
                ),
                _ => (
                    200,
                    sse(&[
                        json!({"type":"error","code":code,"param":"temperature","message":error["message"]}),
                    ]),
                ),
            };
            let (result, _) = run(status, body, form != "http", true, 0).await;
            assert_eq!(result.stop_reason, StopReason::Error, "{form}/{code}");
            let diagnostic = result
                .provider_diagnostic
                .as_ref()
                .expect("SIWC diagnostic");
            assert_eq!(diagnostic.code.as_deref(), Some(code), "{form}/{code}");
            assert_eq!(diagnostic.category, category, "{form}/{code}");
            assert_eq!(diagnostic.evidence, Evidence::SiwcStructuredCode);
            assert_eq!(diagnostic.route(), Some(ProviderDiagnosticRoute::Siwc));
            assert_eq!(diagnostic.resets_at, None);
            assert_eq!(
                siwc_recovery_from_diagnostic(Some(diagnostic)),
                Some(recovery)
            );
            assert_eq!(
                is_retryable_assistant_error(&result),
                recovery == Recovery::Transient
            );
            let message = result.error_message.as_deref().expect("error message");
            if recovery == Recovery::UsageLimit {
                assert!(message.contains("https://chatgpt.com/settings/usage"));
            }
            if code == "subscription_sharing_unsupported_capability" {
                assert!(message.contains("Unsupported parameter: temperature"));
            }
        }
    }
}

#[tokio::test]
async fn preserves_direct_admission_detail_and_limits_status_recovery_to_siwc() {
    for (status, recovery) in [
        (401, Recovery::Auth),
        (403, Recovery::Request),
        (503, Recovery::Transient),
    ] {
        let (result, _) = run(
            status,
            json!({"detail":"token subscription policy denied"}).to_string(),
            false,
            true,
            0,
        )
        .await;
        assert!(
            result
                .error_message
                .as_deref()
                .expect("error message")
                .contains("token subscription policy denied")
        );
        let diagnostic = result
            .provider_diagnostic
            .as_ref()
            .expect("routed diagnostic");
        assert_eq!(diagnostic.evidence, Evidence::SiwcStructuredStatus);
        assert_eq!(diagnostic.http_status, Some(status));
        assert_eq!(
            siwc_recovery_from_diagnostic(Some(diagnostic)),
            Some(recovery)
        );
        assert_eq!(is_retryable_assistant_error(&result), status == 503);
    }
    let (result, _) = run(
        403,
        json!({"detail":"subscription token denied"}).to_string(),
        false,
        false,
        0,
    )
    .await;
    assert_eq!(
        siwc_recovery_from_diagnostic(result.provider_diagnostic.as_ref()),
        None
    );
}

#[tokio::test]
async fn terminal_usage_and_eligibility_failures_are_not_replayed_despite_retry_headers() {
    for (code, status, _, _) in CASES.into_iter().take(2) {
        let (_, calls) = run(
            status,
            json!({"error":{"code":code,"message":"retry later"}}).to_string(),
            false,
            true,
            3,
        )
        .await;
        assert_eq!(calls, 1, "{code}");
    }
}

#[tokio::test]
async fn completion_incomplete_and_interruption_are_distinct() {
    for kind in ["response.completed", "response.incomplete", "interrupted"] {
        let body = if kind == "interrupted" {
            String::new()
        } else {
            sse(&[json!({
                "type":kind, "response":{
                    "id":"synthetic", "status": if kind == "response.completed" {"completed"} else {"incomplete"},
                    "incomplete_details":{"reason":"max_output_tokens"},
                    "output":[], "usage":{"input_tokens":9,"output_tokens":4,"total_tokens":13}
                }
            })])
        };
        let (result, _) = run(200, body, true, true, 0).await;
        if kind == "response.completed" {
            assert_eq!(result.stop_reason, StopReason::Stop);
        } else {
            assert_eq!(result.stop_reason, StopReason::Error);
            let expected = if kind == "interrupted" {
                "terminal response event"
            } else {
                "incomplete"
            };
            assert!(
                result
                    .error_message
                    .as_deref()
                    .expect("completion error")
                    .contains(expected)
            );
        }
        if kind != "interrupted" {
            assert_eq!(result.usage.total_tokens, 13);
        }
    }
}

#[tokio::test]
async fn finished_tool_without_completion_cannot_succeed_on_either_route() {
    for siwc in [true, false] {
        let item = json!({"type":"function_call","id":"fc_1","call_id":"call_1","name":"synthetic","arguments":"{}"});
        let body = sse(&[
            json!({"type":"response.output_item.added","output_index":0,"item":item}),
            json!({"type":"response.output_item.done","output_index":0,"item":item}),
        ]);
        let (result, _) = run(200, body, true, siwc, 0).await;
        assert_eq!(result.stop_reason, StopReason::Error);
        let message = result.error_message.as_deref().expect("completion error");
        assert_eq!(message.contains("terminal response event"), siwc);
    }
}

#[tokio::test]
async fn missing_responses_scope_guidance_handles_a_list_of_dotted_scopes() {
    for scopes in ["api.responses.write", "api.model.read, api.responses.write"] {
        let message = format!(
            "You have insufficient permissions for this operation. Missing scopes: {scopes}. Check your role."
        );
        let (result, _) = run(
            401,
            json!({"error":{"message":message,"code":null}}).to_string(),
            false,
            true,
            0,
        )
        .await;
        assert!(
            result
                .error_message
                .as_deref()
                .expect("scope error")
                .contains("Signing in again will not change that")
        );
    }
}
