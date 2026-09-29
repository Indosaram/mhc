mod support;

use lsp_core::LspClientOptions;
use pretty_assertions::assert_eq;
use serde_json::json;
use support::*;

#[tokio::test]
async fn concurrent_cold_diagnostics_open_once_and_both_receive_current_publish() {
    let context = make_client_with(
        json!({
            "publishDiagnostics": [{
                "trigger": "didOpen", "version": 1,
                "diagnostics": [diagnostic_json("exact-current")], "awaitClientDelivery": true,
            }],
        }),
        LspClientOptions {
            diagnostics_freshness_timeout_ms: Some(500),
            versionless_publish_quiescence_ms: Some(5.0),
            ..default_options()
        },
    )
    .await;

    let (first, second) = tokio::join!(
        context.client.diagnostics(&context.source, None),
        context.client.diagnostics(&context.source, None),
    );

    assert_eq!(
        first.expect("first").items,
        vec![diagnostic("exact-current")]
    );
    assert_eq!(
        second.expect("second").items,
        vec![diagnostic("exact-current")]
    );
    let opens = read_events(&context.events)
        .iter()
        .filter(|event| is_event(event, "clientNotification", "textDocument/didOpen"))
        .count();
    assert_eq!(opens, 1);
    context.stop().await;
}
