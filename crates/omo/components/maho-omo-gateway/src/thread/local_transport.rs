//! Same-device gateway delivery transport (`thread/gateway/local-transport.ts`).

use std::pin::Pin;
use std::sync::Arc;

use super::engine::{GatewayDeliverRequest, GatewayEngine};
use super::errors::thread_tool_failure;
use super::same_device_wake::SameDeviceWake;
use super::transport::{DeliveryOutcome, DeliveryTarget, DeliveryTransport, OutboundDelivery};
use super::types::{GatewayDeliveryResult, GatewaySender};

pub struct LocalTransport {
    pub engine: Arc<GatewayEngine>,
    pub same_device_wake: Option<Arc<SameDeviceWake>>,
}

impl DeliveryTransport for LocalTransport {
    fn deliver<'a>(
        &'a self,
        request: OutboundDelivery,
    ) -> Pin<Box<dyn std::future::Future<Output = DeliveryOutcome> + Send + 'a>> {
        Box::pin(async move {
            let DeliveryTarget::Local {
                thread_id,
                all_scope,
            } = request.target
            else {
                return GatewayDeliveryResult::Error {
                    error: thread_tool_failure(
                        "internal_error",
                        "The local transport delivers only to sessions this device holds.",
                        "Use the local thread address.",
                        None,
                    ),
                };
            };

            if matches!(request.sender, GatewaySender::Person { .. }) {
                return GatewayDeliveryResult::Error {
                    error: thread_tool_failure(
                        "unsupported",
                        "A person's own input reaches a session on this device through its TUI, not through the gateway.",
                        "Attach to the session, or address a session on another device.",
                        None,
                    ),
                };
            }

            let expected_turn_id = match request.expected_turn_id.as_deref() {
                None => None,
                Some(raw) => match raw.parse::<i64>() {
                    Ok(n) if raw.bytes().all(|b| b.is_ascii_digit()) => Some(n),
                    _ => {
                        return GatewayDeliveryResult::Error {
                            error: thread_tool_failure(
                                "turn_conflict",
                                format!("Turn {raw} is not a turn of the target session."),
                                "Read the target again and steer with the turn_id a started or steered result returned.",
                                None,
                            ),
                        };
                    }
                },
            };

            let mut deliver = GatewayDeliverRequest::new(request.sender, thread_id, request.text);
            deliver.mode = Some(request.mode);
            deliver.expected_turn_id = expected_turn_id;
            deliver.all_scope = all_scope;
            deliver.idempotency_key = Some(request.idempotency_key);
            deliver.collapse_key = request.collapse_key;
            deliver.inbound_hop = request.inbound_hop;
            deliver.authority = request.authority;

            let result = self.engine.deliver(deliver).await;
            match &self.same_device_wake {
                Some(wake) => wake.after_send(result).await,
                None => result,
            }
        })
    }
}
