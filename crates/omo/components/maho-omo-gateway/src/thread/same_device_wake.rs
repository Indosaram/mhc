//! Same-device cold wake trigger (`thread/gateway/same-device-wake.ts`).

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use super::store_api::GatewayStoreApi;
use super::types::{
    DeliveryWait, DeliveryWaitKind, GatewayDeliveryDelivery, GatewayDeliveryResult,
};
use super::wake::session_target::{WakeSessionKind, WakeTarget, resolve_wake_target};

pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

pub type CarrierWakeFn = Arc<dyn Fn(String) -> BoxFuture<()> + Send + Sync>;

pub struct SameDeviceWake {
    pub store: Arc<dyn GatewayStoreApi>,
    pub sessions_directory: PathBuf,
    pub enabled: Arc<dyn Fn(&str) -> bool + Send + Sync>,
    pub carrier_wake: Option<CarrierWakeFn>,
}

impl SameDeviceWake {
    pub async fn after_send(&self, result: GatewayDeliveryResult) -> GatewayDeliveryResult {
        let GatewayDeliveryResult::Ok {
            thread_id,
            delivery_id,
            message_seq,
            mut delivery,
            effective_mode,
            endpoint_kind,
            deduplicated,
            collapsed,
        } = result
        else {
            return result;
        };
        let GatewayDeliveryDelivery::QueuedOffline { queue_position, .. } = delivery else {
            return GatewayDeliveryResult::Ok {
                thread_id,
                delivery_id,
                message_seq,
                delivery,
                effective_mode,
                endpoint_kind,
                deduplicated,
                collapsed,
            };
        };
        let target = resolve_wake_target(&self.sessions_directory, &thread_id);
        let WakeTarget::Found {
            session_path,
            cwd,
            session_kind,
            ..
        } = target
        else {
            return GatewayDeliveryResult::Ok {
                thread_id,
                delivery_id,
                message_seq,
                delivery,
                effective_mode,
                endpoint_kind,
                deduplicated,
                collapsed,
            };
        };
        if session_kind != WakeSessionKind::Interactive || !(self.enabled)(&cwd) {
            return GatewayDeliveryResult::Ok {
                thread_id,
                delivery_id,
                message_seq,
                delivery,
                effective_mode,
                endpoint_kind,
                deduplicated,
                collapsed,
            };
        }
        let _ = self.store.woken_request(&thread_id, &session_path);
        match &self.carrier_wake {
            None => {
                delivery = GatewayDeliveryDelivery::QueuedOffline {
                    queue_position,
                    reason: Some("wake_unavailable".to_owned()),
                };
            }
            Some(wake_fn) => {
                let id = thread_id.clone();
                let f = wake_fn.clone();
                tokio::spawn(async move {
                    f(id).await;
                });
                delivery = GatewayDeliveryDelivery::Queued {
                    queue_position,
                    waiting_on: Some(DeliveryWait {
                        kind: DeliveryWaitKind::Wake,
                        device: None,
                    }),
                };
            }
        }
        GatewayDeliveryResult::Ok {
            thread_id,
            delivery_id,
            message_seq,
            delivery,
            effective_mode,
            endpoint_kind,
            deduplicated,
            collapsed,
        }
    }
}
