//! Port of upstream `test/cua-error-codes.test.ts`.
//!
//! Engine codes reach the model as public COMPUTER_* codes with their recovery hint; a code this
//! host does not know still fails the action as COMPUTER_ERROR instead of breaking the call
//! (ErrorCode amendment 1, #11461).

use std::collections::HashMap;

use maho_tools::definition::{AbortSignal, ToolCall};
use senpi_desktop_tool::cua_adapter::create_computer_actions_tool;
use senpi_desktop_tool::tool::ComputerToolDeps;
use serde_json::json;

use crate::fixtures::{TestToolContext, desktop_fixture};

async fn click_failure(engine_code: &str) -> (Option<String>, Option<String>) {
    let mut env = HashMap::new();
    env.insert("FAKE_ENGINE_INPUT_ERROR".into(), engine_code.to_owned());
    let fixture = desktop_fixture(None, env);
    let tool = create_computer_actions_tool(ComputerToolDeps {
        handle: fixture.handle.clone(),
        execute_tool: None,
    });
    let ctx = TestToolContext::new(
        env!("CARGO_MANIFEST_DIR"),
        None,
        "session-1",
        "/sessions/project",
    );
    let result = (tool.execute)(ToolCall {
        id: "call",
        params: json!({ "action": "click", "x": 10, "y": 10 }),
        signal: AbortSignal::default(),
        on_update: None,
        context: Some(&ctx),
    })
    .await
    .expect("execute");
    let details = result.details.expect("details");
    assert_eq!(
        details.get("isError").and_then(|value| value.as_bool()),
        Some(true)
    );
    let value = details.get("value").expect("value");
    let failure = value.get("failure").unwrap_or(value);
    let field = |key: &str| {
        failure
            .get(key)
            .and_then(|field| field.as_str())
            .map(str::to_owned)
    };
    let outcome = (field("code"), field("message"));
    fixture.handle.close().await.expect("close");
    outcome
}

#[tokio::test]
async fn maps_the_amended_engine_codes_to_public_codes_with_a_recovery_hint() {
    for (engine_code, public_code) in [
        ("UserIntervened", "COMPUTER_USER_INTERVENED"),
        ("AxUnconfirmed", "COMPUTER_AX_UNCONFIRMED"),
        ("SpaceUnsupported", "COMPUTER_SPACE_UNSUPPORTED"),
        ("SpaceMoveDenied", "COMPUTER_SPACE_MOVE_DENIED"),
    ] {
        let (code, message) = click_failure(engine_code).await;
        let bare = format!("{public_code}: input refused: {engine_code}");
        assert_eq!(
            (
                code.as_deref(),
                message
                    .as_deref()
                    .is_some_and(|message| message.starts_with(&format!("{bare} ")))
            ),
            (Some(public_code), true),
            "{engine_code}"
        );
    }
}

#[tokio::test]
async fn fails_an_action_whose_engine_code_this_host_does_not_know_as_computer_error() {
    assert_eq!(
        click_failure("SomeFutureCode").await,
        (
            Some("COMPUTER_ERROR".to_owned()),
            Some("COMPUTER_ERROR: input refused: SomeFutureCode".to_owned())
        )
    );
}
