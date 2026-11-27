//! Local-only consumer start gate (`thread/wake/consumer-start.ts`).
//!
//! `O02` remote delivery service client (`device-service-core`) is excluded in `O01`; when no
//! external delivery port is supplied, `start_delivery_consumer` returns `None` cleanly.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use super::super::intake_admission::IntakeAdmission;
use super::super::store_api::GatewayStoreApi;
use super::engine_minimum::engine_drops_retained_record;

pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

pub trait DeliveryServicePort: Send + Sync {
    fn peer_admitted(&self, device: &str) -> BoxFuture<bool>;
    fn report_drained(&self) -> BoxFuture<Result<(), String>>;
    fn close(&self);
}

pub type DeliveriesPortFn =
    Arc<dyn Fn() -> BoxFuture<Option<Arc<dyn DeliveryServicePort>>> + Send + Sync>;
pub type ConsumerSameDeviceFn = Arc<dyn Fn(Option<&str>) -> bool + Send + Sync>;

pub struct ConsumerStartInput {
    pub store: Arc<dyn GatewayStoreApi>,
    pub agent_dir: PathBuf,
    pub sessions_directory: PathBuf,
    pub deliveries: Option<DeliveriesPortFn>,
    pub engine_version: Option<String>,
    pub minimum_engine: Option<String>,
    pub wake: Arc<dyn Fn(String) -> BoxFuture<()> + Send + Sync>,
    pub same_device: ConsumerSameDeviceFn,
}

pub struct ServingConsumer {
    port: Arc<dyn DeliveryServicePort>,
}

impl ServingConsumer {
    pub async fn admit_intake(&self, device: &str) -> IntakeAdmission {
        if self.port.peer_admitted(device).await {
            IntakeAdmission::Admitted
        } else {
            IntakeAdmission::Refused
        }
    }

    pub async fn report_drained(&self) -> Result<(), String> {
        self.port.report_drained().await
    }

    pub fn close(&self) {
        self.port.close();
    }
}

pub async fn start_delivery_consumer(input: &ConsumerStartInput) -> Option<ServingConsumer> {
    let deliveries = input.deliveries.as_ref()?;
    if !engine_drops_retained_record(
        input.engine_version.as_deref(),
        input.minimum_engine.as_deref(),
    ) {
        return None;
    }
    let port = deliveries().await?;
    Some(ServingConsumer { port })
}
