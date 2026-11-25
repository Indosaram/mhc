//! Unified local+remote delivery pipeline (`pipeline.ts`).

use std::sync::Arc;

use super::errors::thread_tool_failure;
use super::transport::{DeliveryOutcome, DeliveryTarget, DeliveryTransport, OutboundDelivery};
use super::types::GatewayDeliveryResult;

pub struct DeliveryPipeline {
    local: Arc<dyn DeliveryTransport>,
    relay: Option<Arc<dyn DeliveryTransport>>,
}

impl DeliveryPipeline {
    pub fn new(
        local: Arc<dyn DeliveryTransport>,
        relay: Option<Arc<dyn DeliveryTransport>>,
    ) -> Self {
        Self { local, relay }
    }

    pub async fn deliver(&self, delivery: OutboundDelivery) -> DeliveryOutcome {
        match &delivery.target {
            DeliveryTarget::Local { .. } => self.local.deliver(delivery).await,
            DeliveryTarget::Remote { row } => match &self.relay {
                Some(relay) => relay.deliver(delivery).await,
                None => GatewayDeliveryResult::Error {
                    error: thread_tool_failure(
                        "host_unavailable",
                        format!(
                            "Thread {} is on device {}, which this device cannot reach.",
                            row.session_id, row.device
                        ),
                        "Cross-device relay is not available in this build (`O02` remote deferred); target a local thread.",
                        None,
                    ),
                },
            },
        }
    }

    pub fn dispose(&self) {
        self.local.dispose();
        if let Some(relay) = &self.relay {
            relay.dispose();
        }
    }
}
