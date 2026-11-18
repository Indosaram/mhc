use crate::support;

use maho_ai::types::{ProviderDiagnostic, ProviderDiagnosticCategory, ProviderDiagnosticEvidence};
use serde_json::json;
use std::sync::Arc;

fn structured(
    category: ProviderDiagnosticCategory,
    status: Option<u16>,
    code: Option<&str>,
) -> ProviderDiagnostic {
    ProviderDiagnostic {
        category,
        http_status: status,
        code: code.map(str::to_owned),
        evidence: ProviderDiagnosticEvidence::StructuredCode,
        resets_at: None,
    }
}

/// Two scanned gateways (no stored `openai` key), so a 404/405 falls through to the second one.
fn two_gateways() -> Arc<support::FixtureRegistry> {
    Arc::new(support::FixtureRegistry {
        stored_api_key: false,
        provider_api_key: Some("gateway-secret".into()),
        provider_headers: None,
        models: vec![
            support::gateway_model(),
            serde_json::from_value(json!({"id":"gpt-5","name":"GPT-5","api":"openai-completions","provider":"second-openai","baseUrl":"https://second.example/openai/v1","reasoning":false,"input":["text"],"cost":{"input":0.0,"output":0.0,"cacheRead":0.0,"cacheWrite":0.0},"contextWindow":100000,"maxTokens":1000})).expect("second gateway model"),
        ],
    })
}

fn model_not_found(model: &str) -> support::StubReply {
    model_not_found_status(model, 400)
}

fn model_not_found_status(model: &str, status: u16) -> support::StubReply {
    support::StubReply {
        error: Some(format!(
            r#"{status} {{"error":{{"code":"model_not_found","message":"The model `{model}` does not exist"}}}}"#
        )),
        diagnostic: Some(ProviderDiagnostic {
            category: ProviderDiagnosticCategory::Unknown,
            http_status: Some(status),
            code: Some("model_not_found".into()),
            evidence: ProviderDiagnosticEvidence::StructuredCode,
            resets_at: None,
        }),
    }
}

#[tokio::test]
async fn retries_the_matching_2_5_id_once_when_the_endpoint_does_not_serve_2_6() {
    let _guard = support::GlobalStateGuard::acquire(None).await;
    let root = tempfile::tempdir().expect("temp");
    let stub = Arc::new(support::StubImages::one().with_reply(
        "gpt-image-2.6-sunburst",
        model_not_found("gpt-image-2.6-sunburst"),
    ));
    let result = support::run(
        root.path(),
        true,
        stub.clone(),
        "fallback",
        json!({"prompt":"a red fox","complexity":"low","output_path":"fox.png"}),
    )
    .await;

    assert_eq!(
        stub.model_ids(),
        ["gpt-image-2.6-sunburst", "gpt-image-2.5-sunburst"].map(str::to_owned)
    );
    let calls = stub
        .calls
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    assert_eq!(calls[0].2.extra["complexity"], json!("low"));
    assert!(calls[1].2.extra.get("complexity").is_none());
    assert_eq!(calls[1].0.name, "GPT Image 2.5 Sunburst");
    assert_eq!(result.details["model"], "gpt-image-2.6-sunburst");
    assert_eq!(result.details["fallbackModel"], "gpt-image-2.5-sunburst");
    assert_eq!(result.details["paths"], json!(["fox.png"]));
    assert_eq!(support::image_count(&result), 1);
    assert!(root.path().join("fox.png").exists());
    assert!(support::text_of(&result).contains("retried with gpt-image-2.5-sunburst"));
}

#[tokio::test]
async fn retries_flare_on_its_own_counterpart_from_model_error_wording() {
    let _guard = support::GlobalStateGuard::acquire(None).await;
    let root = tempfile::tempdir().expect("temp");
    let stub = Arc::new(
        support::StubImages::one().with_reply(
            "gpt-image-2.6-flare",
            support::StubReply {
                error: Some(
                    "404 The model `gpt-image-2.6-flare` does not exist or you do not have access"
                        .into(),
                ),
                diagnostic: None,
            },
        ),
    );
    let result = support::run(
        root.path(),
        true,
        stub.clone(),
        "flare",
        json!({"prompt":"a red fox","model":"gpt-image-2.6-flare","complexity":"medium"}),
    )
    .await;

    assert_eq!(
        stub.model_ids(),
        ["gpt-image-2.6-flare", "gpt-image-2.5-flare"].map(str::to_owned)
    );
    let calls = stub
        .calls
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    assert_eq!(calls[0].2.extra["complexity"], json!("medium"));
    assert!(calls[1].2.extra.get("complexity").is_none());
    assert_eq!(result.details["model"], "gpt-image-2.6-flare");
    assert_eq!(result.details["fallbackModel"], "gpt-image-2.5-flare");
}

#[tokio::test]
async fn never_retries_billing_auth_limits_transport_transient_or_moderation_failures() {
    let _guard = support::GlobalStateGuard::acquire(None).await;
    for (label, reply) in [
        (
            "billing",
            support::StubReply {
                error: Some("402 billing hard limit reached".into()),
                diagnostic: Some(structured(
                    ProviderDiagnosticCategory::Unknown,
                    Some(402),
                    Some("billing_error"),
                )),
            },
        ),
        (
            "auth",
            support::StubReply {
                error: Some(
                    "401 Incorrect API key. The model `gpt-image-2.6-sunburst` does not exist".into(),
                ),
                diagnostic: Some(structured(
                    ProviderDiagnosticCategory::Auth,
                    Some(401),
                    Some("invalid_api_key"),
                )),
            },
        ),
        (
            "quota",
            support::StubReply {
                error: Some("429 insufficient_quota for model gpt-image-2.6-sunburst".into()),
                diagnostic: Some(structured(
                    ProviderDiagnosticCategory::Quota,
                    Some(429),
                    Some("insufficient_quota"),
                )),
            },
        ),
        (
            "rate limit",
            support::StubReply {
                error: Some("429 too many requests for model gpt-image-2.6-sunburst".into()),
                diagnostic: Some(structured(
                    ProviderDiagnosticCategory::RateLimit,
                    Some(429),
                    Some("rate_limit_error"),
                )),
            },
        ),
        (
            "transient",
            support::StubReply {
                error: Some("503 upstream overloaded".into()),
                diagnostic: Some(ProviderDiagnostic {
                    category: ProviderDiagnosticCategory::ProviderUnavailable,
                    http_status: Some(503),
                    code: None,
                    evidence: ProviderDiagnosticEvidence::StructuredStatus,
                    resets_at: None,
                }),
            },
        ),
        (
            "transport",
            support::StubReply {
                error: Some("connection reset by peer".into()),
                diagnostic: Some(ProviderDiagnostic {
                    category: ProviderDiagnosticCategory::Network,
                    http_status: None,
                    code: Some("ECONNRESET".into()),
                    evidence: ProviderDiagnosticEvidence::StructuredTransport,
                    resets_at: None,
                }),
            },
        ),
        (
            "moderation wording",
            support::StubReply {
                error: Some(
                    "400 Your request was rejected by the content moderation system. The model `gpt-image-2.6-sunburst` does not exist"
                        .into(),
                ),
                diagnostic: Some(ProviderDiagnostic {
                    category: ProviderDiagnosticCategory::InvalidRequest,
                    http_status: Some(400),
                    code: None,
                    evidence: ProviderDiagnosticEvidence::StructuredStatus,
                    resets_at: None,
                }),
            },
        ),
        (
            "moderation code",
            support::StubReply {
                error: Some("400 content_policy_violation".into()),
                diagnostic: Some(structured(
                    ProviderDiagnosticCategory::Unknown,
                    Some(400),
                    Some("content_policy_violation"),
                )),
            },
        ),
        (
            "unrelated 400",
            support::StubReply {
                error: Some("400 invalid value for 'size'".into()),
                diagnostic: Some(ProviderDiagnostic {
                    category: ProviderDiagnosticCategory::InvalidRequest,
                    http_status: Some(400),
                    code: None,
                    evidence: ProviderDiagnosticEvidence::StructuredStatus,
                    resets_at: None,
                }),
            },
        ),
    ] {
        let root = tempfile::tempdir().expect("temp");
        let stub = Arc::new(
            support::StubImages::one().with_reply("gpt-image-2.6-sunburst", reply.clone()),
        );
        let result = support::run(
            root.path(),
            true,
            stub.clone(),
            "no-retry",
            json!({"prompt":"a red fox","output_path":"fox.png"}),
        )
        .await;

        assert_eq!(
            stub.model_ids(),
            ["gpt-image-2.6-sunburst"].map(str::to_owned),
            "{label}"
        );
        assert_eq!(result.details["reason"], "provider_error", "{label}");
        assert!(result.details.get("fallbackModel").is_none(), "{label}");
        assert!(!root.path().join("fox.png").exists(), "{label}");
    }
}

#[tokio::test]
async fn models_without_a_2_5_counterpart_keep_their_single_attempt() {
    let _guard = support::GlobalStateGuard::acquire(None).await;
    for model in ["gpt-image-2.5-sunburst", "gpt-image-2"] {
        let root = tempfile::tempdir().expect("temp");
        let stub = Arc::new(support::StubImages::one().with_reply(model, model_not_found(model)));
        let result = support::run(
            root.path(),
            true,
            stub.clone(),
            "no-counterpart",
            json!({"prompt":"a red fox","model":model}),
        )
        .await;

        assert_eq!(stub.model_ids(), [model.to_owned()], "{model}");
        assert_eq!(result.details["reason"], "provider_error", "{model}");
    }
}

#[tokio::test]
async fn a_failed_2_5_retry_is_final_and_is_not_retried_again() {
    let _guard = support::GlobalStateGuard::acquire(None).await;
    let root = tempfile::tempdir().expect("temp");
    let stub = Arc::new(
        support::StubImages::one()
            .with_reply(
                "gpt-image-2.6-sunburst",
                model_not_found("gpt-image-2.6-sunburst"),
            )
            .with_reply(
                "gpt-image-2.5-sunburst",
                model_not_found("gpt-image-2.5-sunburst"),
            ),
    );
    let result = support::run(
        root.path(),
        true,
        stub.clone(),
        "final",
        json!({"prompt":"a red fox","output_path":"fox.png"}),
    )
    .await;

    assert_eq!(
        stub.model_ids(),
        ["gpt-image-2.6-sunburst", "gpt-image-2.5-sunburst"].map(str::to_owned)
    );
    assert_eq!(result.details["reason"], "provider_error");
    assert_eq!(result.details["paths"], json!([]));
    assert!(!root.path().join("fox.png").exists());
}

#[tokio::test]
async fn a_gateway_without_an_images_endpoint_still_moves_to_the_next_candidate() {
    let _guard = support::GlobalStateGuard::acquire(None).await;
    let root = tempfile::tempdir().expect("temp");
    let stub = Arc::new(support::StubImages::one().with_reply(
        "gpt-image-2.6-sunburst",
        support::StubReply {
            error: Some("404 page not found".into()),
            diagnostic: Some(ProviderDiagnostic {
                category: ProviderDiagnosticCategory::InvalidRequest,
                http_status: Some(404),
                code: None,
                evidence: ProviderDiagnosticEvidence::StructuredStatus,
                resets_at: None,
            }),
        },
    ));
    let result = support::run_with(
        root.path(),
        two_gateways(),
        stub.clone(),
        "gateway",
        json!({"prompt":"a red fox","output_path":"fox.png"}),
    )
    .await;

    assert_eq!(
        stub.model_ids(),
        ["gpt-image-2.6-sunburst", "gpt-image-2.6-sunburst"].map(str::to_owned)
    );
    assert_eq!(result.details["model"], "gpt-image-2.6-sunburst");
    assert!(result.details.get("fallbackModel").is_none());
    assert_eq!(result.details["paths"], json!(["fox.png"]));
    assert!(support::text_of(&result).contains("no image generation endpoint"));
}

#[tokio::test]
async fn a_gateway_that_serves_images_but_not_the_2_6_id_retries_on_itself() {
    let _guard = support::GlobalStateGuard::acquire(None).await;
    let root = tempfile::tempdir().expect("temp");
    let stub = Arc::new(support::StubImages::one().with_reply(
        "gpt-image-2.6-sunburst",
        model_not_found_status("gpt-image-2.6-sunburst", 404),
    ));
    let result = support::run_with(
        root.path(),
        two_gateways(),
        stub.clone(),
        "gateway-model",
        json!({"prompt":"a red fox","output_path":"fox.png","complexity":"low"}),
    )
    .await;

    assert_eq!(
        stub.model_ids(),
        ["gpt-image-2.6-sunburst", "gpt-image-2.5-sunburst"].map(str::to_owned)
    );
    let calls = stub
        .calls
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    assert_eq!(calls[0].0.base_url, "https://gateway.example/openai/v1");
    assert_eq!(calls[1].0.base_url, "https://gateway.example/openai/v1");
    assert!(calls[1].2.extra.get("complexity").is_none());
    assert_eq!(result.details["fallbackModel"], "gpt-image-2.5-sunburst");
    assert_eq!(result.details["paths"], json!(["fox.png"]));
    assert!(!support::text_of(&result).contains("no image generation endpoint"));
}

#[tokio::test]
async fn a_retry_that_also_misses_the_images_endpoint_still_falls_through() {
    let _guard = support::GlobalStateGuard::acquire(None).await;
    let root = tempfile::tempdir().expect("temp");
    let stub = Arc::new(
        support::StubImages::one()
            .with_reply(
                "gpt-image-2.6-sunburst",
                model_not_found_status("gpt-image-2.6-sunburst", 404),
            )
            .with_reply(
                "gpt-image-2.5-sunburst",
                support::StubReply {
                    error: Some("405 method not allowed".into()),
                    diagnostic: Some(ProviderDiagnostic {
                        category: ProviderDiagnosticCategory::InvalidRequest,
                        http_status: Some(405),
                        code: None,
                        evidence: ProviderDiagnosticEvidence::StructuredStatus,
                        resets_at: None,
                    }),
                },
            ),
    );
    let result = support::run_with(
        root.path(),
        two_gateways(),
        stub.clone(),
        "retry-then-skip",
        json!({"prompt":"a red fox","output_path":"fox.png"}),
    )
    .await;

    assert_eq!(
        stub.model_ids(),
        [
            "gpt-image-2.6-sunburst",
            "gpt-image-2.5-sunburst",
            "gpt-image-2.6-sunburst"
        ]
        .map(str::to_owned)
    );
    let calls = stub
        .calls
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    assert_eq!(calls[2].0.base_url, "https://second.example/openai/v1");
    assert!(result.details.get("fallbackModel").is_none());
    assert_eq!(result.details["paths"], json!(["fox.png"]));
    let text = support::text_of(&result);
    assert!(text.contains("no image generation endpoint"));
    assert!(text.contains("quotio-openai"));
}
