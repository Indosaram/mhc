use maho_cli::experimental::mini::shared::protocol::*;
#[test]
fn auth_prompts_strip_signal_and_keep_wire_discriminant() {
    let prompt = maho_ai::auth::types::AuthPrompt {
        kind: maho_ai::auth::types::AuthPromptKind::ManualCode {
            message: "code".to_owned(),
            placeholder: None,
        },
        signal: Some(maho_ai::utils::abort::AbortController::new().signal()),
    };
    let value = serde_json::to_value(AuthPromptRequest::from(prompt)).unwrap();
    assert_eq!(
        value,
        serde_json::json!({"type":"manual_code", "message":"code"})
    );
}
#[test]
fn notices_keep_device_code_camelcase_fields() {
    let value = serde_json::json!({"type":"device_code", "userCode":"code", "verificationUri":"https://example.org", "intervalSeconds":5.0});
    let notice: AuthNotice = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(notice).unwrap(), value);
}
#[test]
fn command_result_rejects_failure_without_error() {
    let value = serde_json::json!({"ok":false, "error":"failed"});
    let result: CommandResult = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(result, CommandResult::Error("failed".to_owned()));
    assert_eq!(serde_json::to_value(result).unwrap(), value);
    assert!(serde_json::from_value::<CommandResult>(serde_json::json!({"ok":false})).is_err());
}
#[test]
fn model_ref_wire_fields_match_upstream() {
    let model = ModelRef {
        provider: "provider".to_owned(),
        model_id: "model".to_owned(),
    };
    assert_eq!(
        serde_json::to_value(model).unwrap(),
        serde_json::json!({"provider":"provider", "modelId":"model"})
    );
    assert_eq!(LANE.id, "lane");
    assert_eq!(SESSIONS.id, "sessions");
}
#[test]
fn absent_account_fields_are_not_serialized() {
    let account = ProviderAccount {
        id: "id".to_owned(),
        name: "name".to_owned(),
        auth_type: AuthType::ApiKey,
        configured: false,
        source: None,
        interactive: true,
        method_name: None,
    };
    let value = serde_json::to_value(&account).unwrap();
    assert_eq!(value["authType"], "api_key");
    assert!(value.get("source").is_none());
    assert_eq!(
        serde_json::from_value::<ProviderAccount>(value).unwrap(),
        account
    );
}
/// senpi `LaneSubscription`/`SessionSnapshot`/`LaneEvent`: camelCase wire fields; the lane snapshot and harness event
/// cross the wire as the harness's own JSON (maho_agent's `watch_lane` snapshot), carried verbatim.
#[test]
fn lane_subscription_and_event_keep_upstream_wire_shape() {
    let value = serde_json::json!({
        "subscriptionId": "sub-1",
        "snapshot": {
            "sessionId": "s", "cwd": "/w", "sessionPath": "/w/s.jsonl",
            "lane": {"status": "idle", "queue": {"steering": [], "followUp": []}},
            "models": {"models": [{"provider": "p", "modelId": "m", "name": "M"}], "accounts": [], "refreshing": false}
        }
    });
    let subscription: LaneSubscription = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(subscription.snapshot.lane["status"], "idle");
    assert_eq!(serde_json::to_value(&subscription).unwrap(), value);
    let event = serde_json::json!({"subscriptionId": "sub-1", "event": {"type": "agent_start"}});
    assert_eq!(
        serde_json::to_value(serde_json::from_value::<LaneEvent>(event.clone()).unwrap()).unwrap(),
        event
    );
    let described = serde_json::json!({"sessionId": "s"});
    assert_eq!(
        serde_json::to_value(
            serde_json::from_value::<WorkerDescription>(described.clone()).unwrap()
        )
        .unwrap(),
        described
    );
    let state = ModelsEvent::State {
        state: subscription.snapshot.models.clone(),
    };
    assert_eq!(AuthEventPayload::from_models_event(&state), None);
}
