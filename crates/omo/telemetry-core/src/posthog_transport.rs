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

/// The send-time consent gate of `createConsentGatedFetch`: read at the moment a batch would leave the
/// process, so consent withdrawn after an event was queued still stops that event. A dropped batch is
/// reported by size only (never content) and answered as accepted, so it is not retried: dropping IS the
/// outcome, not a failure.
#[derive(Clone)]
pub struct ConsentGate {
    pub is_send_allowed: Arc<dyn Fn() -> bool + Send + Sync>,
    pub on_dropped: Option<Arc<dyn Fn(usize) + Send + Sync>>,
}

impl ConsentGate {
    /// True when the send may proceed; otherwise reports `count` events dropped.
    pub fn allows(&self, count: usize) -> bool {
        if (self.is_send_allowed)() {
            return true;
        }
        if let Some(on_dropped) = &self.on_dropped {
            on_dropped(count);
        }
        false
    }
}

struct Inner {
    gate: Option<ConsentGate>,
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
    /// Gates every send this transport makes (batch flush, durable and immediate) on `gate`.
    #[must_use]
    pub fn with_consent_gate(mut self, gate: ConsentGate) -> Self {
        if let Some(inner) = Arc::get_mut(&mut self.inner) {
            inner.gate = Some(gate.clone());
        }
        self.immediate.set_gate(gate);
        self
    }

    pub fn new(api_key: &str, options: &TelemetryTransportOptions) -> Result<Self, TelemetryError> {
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|error| TelemetryError::new(error.to_string()))?;
        Ok(Self {
            immediate: crate::posthog_immediate_send::create_immediate_sender(api_key, options)?,
            inner: Arc::new(Inner {
                gate: None,
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
        if self
            .gate
            .as_ref()
            .is_some_and(|gate| !gate.allows(batch.len()))
        {
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
            if self.inner.gate.as_ref().is_some_and(|gate| !gate.allows(1)) {
                return Ok(());
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Answers `requests` POSTs with 200 and returns their JSON bodies.
    async fn server(requests: usize) -> (String, tokio::task::JoinHandle<Vec<Value>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let host = format!("http://{}", listener.local_addr().expect("address"));
        let task = tokio::spawn(async move {
            let mut bodies = Vec::new();
            for _ in 0..requests {
                let (mut stream, _) = listener.accept().await.expect("accept");
                let mut bytes = Vec::new();
                let mut buffer = [0; 4096];
                let (offset, length) = loop {
                    let size = stream.read(&mut buffer).await.expect("request");
                    assert_ne!(size, 0);
                    bytes.extend_from_slice(&buffer[..size]);
                    if let Some(index) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                        let header = String::from_utf8_lossy(&bytes[..index]).to_lowercase();
                        let length: usize = header
                            .lines()
                            .find_map(|line| line.strip_prefix("content-length:"))
                            .expect("content length")
                            .trim()
                            .parse()
                            .expect("length");
                        break (index + 4, length);
                    }
                };
                while bytes.len() < offset + length {
                    let size = stream.read(&mut buffer).await.expect("body");
                    assert_ne!(size, 0);
                    bytes.extend_from_slice(&buffer[..size]);
                }
                bodies.push(serde_json::from_slice(&bytes[offset..offset + length]).expect("json"));
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                    .await
                    .expect("response");
            }
            bodies
        });
        (host, task)
    }

    fn message(event: &str) -> TelemetryCaptureMessage {
        TelemetryCaptureMessage {
            uuid: None,
            timestamp: None,
            distinct_id: "install".into(),
            event: event.into(),
            properties: Default::default(),
        }
    }

    fn events(bodies: &[Value]) -> Vec<String> {
        bodies
            .iter()
            .flat_map(|body| body["batch"].as_array().cloned().unwrap_or_default())
            .map(|entry| entry["event"].as_str().unwrap_or_default().to_string())
            .collect()
    }

    #[tokio::test]
    async fn consent_gate_drops_every_send_path_until_consent_is_given() {
        tokio::time::timeout(Duration::from_secs(5), async {
            let (host, server) = server(2).await;
            let options = TelemetryTransportOptions {
                host,
                disable_geoip: true,
                enable_exception_autocapture: false,
                enable_local_evaluation: false,
                strict_local_evaluation: true,
                disable_remote_config: true,
                flush_at: 100,
                flush_interval: 0,
            };
            let consent = Arc::new(AtomicBool::new(false));
            let allowed = Arc::clone(&consent);
            let dropped = Arc::new(Mutex::new(Vec::new()));
            let observed = Arc::clone(&dropped);
            let transport = PostHogTelemetryTransport::new("key", &options)
                .expect("transport")
                .with_consent_gate(ConsentGate {
                    is_send_allowed: Arc::new(move || allowed.load(Ordering::SeqCst)),
                    on_dropped: Some(Arc::new(move |count| {
                        observed.lock().expect("dropped").push(count);
                    })),
                });

            transport.capture(&message("dropped_a")).expect("capture");
            transport.capture(&message("dropped_b")).expect("capture");
            transport
                .flush()
                .expect("flush")
                .await
                .expect("dropped flush is Ok");
            transport
                .capture_immediate(&message("dropped_now"))
                .expect("immediate")
                .await
                .expect("dropped immediate is Ok");
            assert_eq!(*dropped.lock().expect("dropped"), vec![2, 1]);

            consent.store(true, Ordering::SeqCst);
            transport.capture(&message("sent")).expect("capture");
            transport.flush().expect("flush").await.expect("sent flush");
            transport
                .capture_immediate(&message("sent_now"))
                .expect("immediate")
                .await
                .expect("sent immediate");

            let bodies = server.await.expect("server");
            assert_eq!(events(&bodies), vec!["sent", "sent_now"]);
            assert_eq!(*dropped.lock().expect("dropped"), vec![2, 1]);
        })
        .await
        .expect("bounded consent gate");
    }
}
