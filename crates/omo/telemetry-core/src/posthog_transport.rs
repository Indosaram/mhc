use std::sync::Arc;
use std::sync::Mutex;
use std::sync::PoisonError;
use std::time::Duration;

use futures::FutureExt;
use futures::future::BoxFuture;
use serde_json::Value;
use serde_json::json;
use tokio::runtime::Handle;

use crate::diagnostics::to_iso_string;
use crate::types::TelemetryCaptureMessage;
use crate::types::TelemetryError;
use crate::types::TelemetryTransport;
use crate::types::TelemetryTransportOptions;

const LIB_NAME: &str = "omo-telemetry-core-rs";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

struct Inner {
    api_key: String,
    options: TelemetryTransportOptions,
    http: reqwest::Client,
    queue: Mutex<Vec<Value>>,
    interval_flush_pending: Mutex<bool>,
}

/// Queueing PostHog `/batch/` transport honouring `flushAt` / `flushInterval` like `posthog-node`.
///
/// Automatic flushes only run when `capture` is called inside a Tokio runtime; explicit `flush`
/// and `shutdown` always deliver the queue.
pub struct PostHogTelemetryTransport {
    immediate: crate::posthog_immediate_send::ImmediateSender,
    inner: Arc<Inner>,
}

impl PostHogTelemetryTransport {
    pub fn new(api_key: &str, options: &TelemetryTransportOptions) -> Result<Self, TelemetryError> {
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|error| TelemetryError::new(error.to_string()))?;
        Ok(Self {
            immediate: crate::posthog_immediate_send::create_immediate_sender(api_key, options)?,
            inner: Arc::new(Inner {
                api_key: api_key.to_string(),
                options: options.clone(),
                http,
                queue: Mutex::new(Vec::new()),
                interval_flush_pending: Mutex::new(false),
            }),
        })
    }
}

impl Inner {
    fn to_batch_entry(&self, message: &TelemetryCaptureMessage) -> Value {
        let mut properties = message.properties.clone();
        properties.insert("$lib".into(), LIB_NAME.into());
        properties.insert("$lib_version".into(), env!("CARGO_PKG_VERSION").into());
        if self.options.disable_geoip {
            properties.insert("$geoip_disable".into(), true.into());
        }
        let mut entry = json!({
            "type": "capture",
            "event": message.event,
            "distinct_id": message.distinct_id,
            "properties": properties,
            "timestamp": message.timestamp.clone().unwrap_or_else(|| to_iso_string(chrono::Utc::now())),
        });
        if let Some(uuid) = &message.uuid {
            entry["uuid"] = uuid.clone().into();
        }
        entry
    }

    async fn flush(&self) -> Result<(), TelemetryError> {
        let batch = std::mem::take(&mut *self.queue.lock().unwrap_or_else(PoisonError::into_inner));
        if batch.is_empty() {
            return Ok(());
        }
        let url = format!("{}/batch/", self.options.host.trim_end_matches('/'));
        let response = self
            .http
            .post(url)
            .json(&json!({ "api_key": self.api_key, "batch": batch }))
            .send()
            .await
            .map_err(|error| TelemetryError::new(error.to_string()))?;
        let status = response.status();
        if status.is_success() {
            return Ok(());
        }
        Err(TelemetryError::new(format!(
            "PostHog batch request failed with status {status}"
        )))
    }

    fn schedule_interval_flush(self: &Arc<Self>, handle: &Handle) {
        let mut pending = self
            .interval_flush_pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if *pending {
            return;
        }
        *pending = true;
        let inner = Arc::clone(self);
        let delay = Duration::from_millis(self.options.flush_interval);
        handle.spawn(async move {
            tokio::time::sleep(delay).await;
            *inner
                .interval_flush_pending
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = false;
            let _best_effort = inner.flush().await;
        });
    }
}

impl TelemetryTransport for PostHogTelemetryTransport {
    fn capture(&self, message: &TelemetryCaptureMessage) -> Result<(), TelemetryError> {
        let entry = self.inner.to_batch_entry(message);
        let queued = {
            let mut queue = self
                .inner
                .queue
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            queue.push(entry);
            queue.len()
        };
        let Ok(handle) = Handle::try_current() else {
            return Ok(());
        };
        let flush_at = usize::try_from(self.inner.options.flush_at).unwrap_or(usize::MAX);
        if queued >= flush_at.max(1) {
            let inner = Arc::clone(&self.inner);
            handle.spawn(async move {
                let _best_effort = inner.flush().await;
            });
        } else if self.inner.options.flush_interval > 0 {
            self.inner.schedule_interval_flush(&handle);
        }
        Ok(())
    }

    fn capture_immediate<'a>(
        &'a self,
        message: &'a TelemetryCaptureMessage,
    ) -> Option<BoxFuture<'a, Result<(), crate::telemetry_delivery_error::TelemetryDeliveryError>>>
    {
        Some(self.immediate.send(message).boxed())
    }
    fn capture_durable(
        &self,
        message: TelemetryCaptureMessage,
    ) -> BoxFuture<'_, Result<(), TelemetryError>> {
        Box::pin(async move {
            let mut entry = self.inner.to_batch_entry(&message);
            if let Some(uuid) = message.properties.get("$insert_id").and_then(Value::as_str) {
                entry["uuid"] = json!(uuid);
            }
            if let Some(seconds) = message.properties.get("$time").and_then(Value::as_f64)
                && let Some(at) = chrono::DateTime::from_timestamp_millis((seconds * 1000.0) as i64)
                {
                    entry["timestamp"] = json!(to_iso_string(at));
                }
            let response = self
                .inner
                .http
                .post(format!(
                    "{}/batch/",
                    self.inner.options.host.trim_end_matches('/')
                ))
                .json(&json!({"api_key":self.inner.api_key,"batch":[entry]}))
                .send()
                .await
                .map_err(|error| TelemetryError::new(error.to_string()))?;
            let status = response.status();
            if status.is_success() {
                Ok(())
            } else {
                Err(TelemetryError::new(format!(
                    "PostHog durable request failed with status {status}"
                )))
            }
        })
    }

    fn flush(&self) -> Option<BoxFuture<'_, Result<(), TelemetryError>>> {
        Some(self.inner.flush().boxed())
    }

    fn shutdown(&self) -> BoxFuture<'_, Result<(), TelemetryError>> {
        async move {
            self.immediate.shutdown().await;
            self.inner.flush().await
        }
        .boxed()
    }
}

pub fn create_default_posthog_transport(
    api_key: &str,
    options: &TelemetryTransportOptions,
) -> Result<Box<dyn TelemetryTransport>, TelemetryError> {
    Ok(Box::new(PostHogTelemetryTransport::new(api_key, options)?))
}
