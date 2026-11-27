//! Extension UI response forwarding (`thread/ui-answer.ts`).

use serde_json::{Map, Value};

use super::adapter::{GatewayEndpointRef, UiAnswer, UiAnswerReply};
use super::types::GatewayEndpointKind;

pub async fn answer_ui_request<F, Fut>(
    call: F,
    endpoint: &GatewayEndpointRef,
    answer: &UiAnswer,
) -> UiAnswerReply
where
    F: FnOnce(String, String, Value) -> Fut,
    Fut: std::future::Future<Output = Result<Value, String>>,
{
    let mut data = match answer.fields.as_object() {
        Some(m) => m.clone(),
        None => Map::new(),
    };
    data.insert(
        "uiRequestId".to_owned(),
        Value::String(answer.ui_request_id.clone()),
    );
    if endpoint.kind == GatewayEndpointKind::RpcHost
        && let Some(ref rid) = endpoint.routing_id
    {
        data.insert("sessionId".to_owned(), Value::String(rid.clone()));
    }
    match call(
        endpoint.socket.clone(),
        "extension_ui_response".to_owned(),
        Value::Object(data),
    )
    .await
    {
        Ok(reply) if reply.get("success").and_then(Value::as_bool) == Some(true) => {
            UiAnswerReply::Delivered
        }
        Ok(reply) => UiAnswerReply::Refused {
            error: reply
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("refused")
                .to_owned(),
        },
        Err(err) => UiAnswerReply::Refused { error: err },
    }
}
