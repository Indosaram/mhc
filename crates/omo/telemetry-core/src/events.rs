use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use futures::FutureExt;
use futures::future::BoxFuture;
use serde_json::Map;
use serde_json::Value;

use crate::env::get_telemetry_api_key;
use crate::env::get_telemetry_host;
use crate::posthog_client::TelemetryClientEnabledInput;
use crate::posthog_client::build_transport;
use crate::posthog_client::is_telemetry_client_enabled;
use crate::posthog_client::report;
use crate::types::TelemetryCaptureMessage;
use crate::types::TelemetryCaptureProperties;
use crate::types::TelemetryDiagnosticEvent;
use crate::types::TelemetryDiagnostics;
use crate::types::TelemetryEnv;
use crate::types::TelemetryError;
use crate::types::TelemetryProductConfig;
use crate::types::TelemetryTransport;
use crate::types::TelemetryTransportFactory;
use crate::types::TelemetryTransportOptions;

pub type EventPropertyAllowlist = HashMap<String, Vec<String>>;
/// Caller-supplied properties; `Value::Null` stands in for both JS `null` and `undefined`.
pub type EventTelemetryProperties = Map<String, Value>;
/// Returns a future that resolves after the given delay (injectable for tests).
pub type EventTelemetrySetTimeout = Arc<dyn Fn(Duration) -> BoxFuture<'static, ()> + Send + Sync>;
pub type EventTelemetryOnCapture = Arc<dyn Fn(&TelemetryCaptureMessage) + Send + Sync>;

/// `uuid` makes a re-sent event idempotent: PostHog keeps one row per uuid. `timestamp` is the event's
/// own time; a re-send passes the original one so the event lands on the same day it first did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EventTelemetryCaptureOptions {
    pub uuid: Option<String>,
    pub timestamp: Option<chrono::DateTime<chrono::Utc>>,
    pub timestamp_ms: Option<f64>,
}

const EVENT_FLUSH_AT: u32 = 20;
const EVENT_FLUSH_INTERVAL_MS: u64 = 10_000;
const SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(1_000);
const MAX_STRING_UTF16_UNITS: usize = 64;
const ALLOWED_DOLLAR_KEYS: [&str; 4] = [
    "$os",
    "$os_version",
    "$process_person_profile",
    "$session_id",
];
const FORBIDDEN_SUFFIXES: [&str; 3] = ["_text", "_path", "_prompt"];

#[derive(Clone)]
pub struct CreateEventTelemetryClientInput<'a> {
    pub diagnostics: Option<TelemetryDiagnostics>,
    pub distinct_id: &'a str,
    pub env: Option<&'a TelemetryEnv>,
    pub on_capture: Option<EventTelemetryOnCapture>,
    pub product: &'a TelemetryProductConfig,
    pub property_allowlist: &'a EventPropertyAllowlist,
    pub schema_version: u32,
    pub set_timeout_fn: Option<EventTelemetrySetTimeout>,
    pub source: &'a str,
    pub transport_factory: Option<TelemetryTransportFactory>,
}

struct EnabledEventClient {
    transport: Arc<dyn TelemetryTransport>,
    allowlists: HashMap<String, HashSet<String>>,
    shared_properties: TelemetryCaptureProperties,
    distinct_id: String,
    diagnostics: Option<TelemetryDiagnostics>,
    on_capture: Option<EventTelemetryOnCapture>,
    set_timeout_fn: Option<EventTelemetrySetTimeout>,
    source: String,
}

/// Allowlist-projecting event client; a disabled client is a no-op whose `enabled` is `false`.
pub struct EventTelemetryClient {
    inner: Option<EnabledEventClient>,
}

impl EnabledEventClient {
    fn report(&self, event: TelemetryDiagnosticEvent, message: String) {
        report(
            self.diagnostics.as_ref(),
            event,
            &self.source,
            TelemetryError::new(message),
        );
    }

    /// The allowlist projection every capture path shares; `None` after reporting a rejected event.
    fn build_payload(
        &self,
        name: &str,
        properties: &EventTelemetryProperties,
        options: Option<&EventTelemetryCaptureOptions>,
    ) -> Option<TelemetryCaptureMessage> {
        let Some(allowlist) = self.allowlists.get(name).filter(|_| !name.is_empty()) else {
            self.report(
                TelemetryDiagnosticEvent::TelemetryEventRejected,
                "event name is empty or not allowlisted".to_string(),
            );
            return None;
        };

        let mut payload_properties = TelemetryCaptureProperties::new();
        for (key, value) in properties {
            if is_forbidden_key(key, value) {
                self.report(
                    TelemetryDiagnosticEvent::TelemetryEventPropertyRejected,
                    format!("forbidden event property: {key}"),
                );
                continue;
            }
            if !allowlist.contains(key) {
                self.report(
                    TelemetryDiagnosticEvent::TelemetryEventPropertyDropped,
                    format!("unknown event property: {key}"),
                );
                continue;
            }
            let Some(projected) = project_value(value) else {
                self.report(
                    TelemetryDiagnosticEvent::TelemetryEventPropertyRejected,
                    format!("invalid event property value: {key}"),
                );
                continue;
            };
            payload_properties.insert(key.clone(), projected);
        }
        for (key, value) in &self.shared_properties {
            payload_properties.insert(key.clone(), value.clone());
        }

        let requested_uuid = options.and_then(|options| options.uuid.as_deref());
        let uuid = match requested_uuid {
            Some(uuid) if is_valid_event_uuid(uuid) => uuid.to_string(),
            Some(_) => {
                self.report(
                    TelemetryDiagnosticEvent::TelemetryEventPropertyRejected,
                    "invalid event uuid".to_string(),
                );
                uuid::Uuid::now_v7().to_string()
            }
            None => uuid::Uuid::now_v7().to_string(),
        };
        // event_id is minted last so no event property can supply it; PostHog dedupes on `uuid`, so
        // mirroring it there makes a direct and a relayed delivery of one event collapse. A re-send
        // passes the uuid it first carried, so its event_id stays that same id.
        payload_properties.insert("event_id".into(), Value::String(uuid.clone()));
        let timestamp = options
            .and_then(|options| {
                options.timestamp.or_else(|| {
                    options.timestamp_ms.and_then(|ms| chrono::DateTime::from_timestamp_millis(ms as i64))
                })
            })
            .map_or_else(
                || crate::diagnostics::to_iso_string(chrono::Utc::now()),
                crate::diagnostics::to_iso_string,
            );
        Some(TelemetryCaptureMessage {
            uuid: Some(uuid),
            timestamp: Some(timestamp),
            distinct_id: self.distinct_id.clone(),
            event: name.to_string(),
            properties: payload_properties,
        })
    }

    async fn flush(&self) {
        let Some(flush) = self.transport.flush() else {
            return;
        };
        if let Err(error) = flush.await {
            report(
                self.diagnostics.as_ref(),
                TelemetryDiagnosticEvent::TelemetryShutdownFailed,
                &self.source,
                error,
            );
        }
    }

    async fn finalize(&self) {
        self.flush().await;
        if let Err(error) = self.transport.shutdown().await {
            report(
                self.diagnostics.as_ref(),
                TelemetryDiagnosticEvent::TelemetryShutdownFailed,
                &self.source,
                error,
            );
        }
    }
}

impl EventTelemetryClient {
    pub fn enabled(&self) -> bool {
        self.inner.is_some()
    }

    pub fn capture_event(&self, name: &str, properties: &EventTelemetryProperties) {
        self.capture_event_with_options(name, properties, None);
    }

    /// Captures with a caller-supplied identity: a valid `uuid` is reused as the event id and
    /// `timestamp` becomes the event's own time, so a re-send lands exactly where the first did.
    pub fn capture_event_with_options(
        &self,
        name: &str,
        properties: &EventTelemetryProperties,
        options: Option<&EventTelemetryCaptureOptions>,
    ) {
        let _acknowledged = self.capture_event_inner(name, properties, options, None);
    }

    /// Returns true only when the transport accepted the projected event.
    pub fn capture_event_acknowledged(
        &self,
        name: &str,
        properties: &EventTelemetryProperties,
    ) -> bool {
        self.capture_event_with_delivery(name, properties, None)
    }

    /// Durable deliveries preserve their insertion identity and close time across retries.
    pub fn capture_event_with_delivery(
        &self,
        name: &str,
        properties: &EventTelemetryProperties,
        delivery: Option<(&str, f64)>,
    ) -> bool {
        self.capture_event_inner(name, properties, None, delivery)
    }

    fn capture_event_inner(
        &self,
        name: &str,
        properties: &EventTelemetryProperties,
        options: Option<&EventTelemetryCaptureOptions>,
        delivery: Option<(&str, f64)>,
    ) -> bool {
        let Some(client) = &self.inner else {
            return false;
        };
        let Some(mut payload) = client.build_payload(name, properties, options) else {
            return false;
        };
        if let Some((uuid, timestamp)) = delivery {
            payload.properties.insert("$insert_id".into(), Value::String(uuid.into()));
            payload.properties.insert("$time".into(), serde_json::json!(timestamp / 1000.0));
        }
        let delivered = if delivery.is_some() {
            // A dedicated runtime also works when the recorder runs inside a current-thread runtime.
            std::thread::scope(|scope| {
                scope
                    .spawn(|| {
                        let runtime = tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                            .map_err(|error| TelemetryError::new(error.to_string()))?;
                        runtime.block_on(async {
                            tokio::time::timeout(
                                std::time::Duration::from_secs(10),
                                client.transport.capture_durable(payload.clone()),
                            )
                            .await
                            .map_err(|_| {
                                TelemetryError::new("durable telemetry delivery timed out")
                            })?
                        })
                    })
                    .join()
                    .unwrap_or_else(|_| {
                        Err(TelemetryError::new("durable telemetry sender panicked"))
                    })
            })
        } else {
            client.transport.capture(&payload)
        };
        match delivered {
            Ok(()) => {
                if let Some(on_capture) = &client.on_capture {
                    on_capture(&payload);
                }
                true
            }
            Err(error) => {
                report(
                    client.diagnostics.as_ref(),
                    TelemetryDiagnosticEvent::TelemetryCaptureFailed,
                    &client.source,
                    error,
                );
                false
            }
        }
    }

    /// Sends one ack-gated event immediately and resolves only on the transport's 2xx answer. `None` when the
    /// client is disabled or the event/properties are rejected; an `Err` keeps the caller's pending record.
    pub fn capture_immediate(
        &self,
        name: &str,
        properties: &EventTelemetryProperties,
        options: &EventTelemetryCaptureOptions,
    ) -> Option<BoxFuture<'static, Result<(), crate::telemetry_delivery_error::TelemetryDeliveryError>>>
    {
        let client = self.inner.as_ref()?;
        let payload = client.build_payload(name, properties, Some(options))?;
        let transport = Arc::clone(&client.transport);
        let on_capture = client.on_capture.clone();
        let source = client.source.clone();
        let diagnostics = client.diagnostics.clone();
        Some(
            async move {
                let Some(send) = transport.capture_immediate(&payload) else {
                    return Err(
                        crate::telemetry_delivery_error::TelemetryDeliveryError::new(
                            None,
                            Some(TelemetryError::new("transport has no immediate send")),
                        ),
                    );
                };
                match send.await {
                    Ok(()) => {
                        if let Some(on_capture) = &on_capture {
                            on_capture(&payload);
                        }
                        Ok(())
                    }
                    Err(error) => {
                        report(
                            diagnostics.as_ref(),
                            TelemetryDiagnosticEvent::TelemetryCaptureFailed,
                            &source,
                            TelemetryError::new(error.to_string()),
                        );
                        Err(error)
                    }
                }
            }
            .boxed(),
        )
    }

    pub async fn flush(&self) {
        if let Some(client) = &self.inner {
            client.flush().await;
        }
    }

    /// Flushes and shuts the transport down, giving up after 1000 ms.
    pub async fn shutdown(&self) {
        let Some(client) = &self.inner else {
            return;
        };
        let timeout = match &client.set_timeout_fn {
            Some(set_timeout) => set_timeout(SHUTDOWN_TIMEOUT),
            None => tokio::time::sleep(SHUTDOWN_TIMEOUT).boxed(),
        };
        futures::future::select(client.finalize().boxed(), timeout).await;
    }
}

pub fn create_event_telemetry_client(
    input: &CreateEventTelemetryClientInput<'_>,
) -> EventTelemetryClient {
    let disabled = EventTelemetryClient { inner: None };
    if !is_telemetry_client_enabled(&TelemetryClientEnabledInput::for_product(
        input.env,
        input.product,
    )) {
        return disabled;
    }

    let product = input.product;
    let mut options = TelemetryTransportOptions {
        enable_exception_autocapture: false,
        enable_local_evaluation: false,
        strict_local_evaluation: true,
        disable_remote_config: true,
        flush_at: EVENT_FLUSH_AT,
        flush_interval: EVENT_FLUSH_INTERVAL_MS,
        host: get_telemetry_host(input.env, &product.default_host),
        disable_geoip: true,
    };
    if let Some(overrides) = &product.transport_options {
        options.apply(overrides);
    }
    // SAFETY-DIVERGENCE(oh-my-openagent packages/telemetry-core/src/events.ts): disable_geoip forced to false; mhc forces disable_geoip to true.
    options.disable_geoip = true;
    options.flush_at = EVENT_FLUSH_AT;
    options.flush_interval = EVENT_FLUSH_INTERVAL_MS;

    let api_key = get_telemetry_api_key(input.env, &product.default_api_key);
    let transport: Arc<dyn TelemetryTransport> =
        match build_transport(input.transport_factory.as_ref(), &api_key, &options) {
            Ok(transport) => transport.into(),
            Err(error) => {
                report(
                    input.diagnostics.as_ref(),
                    TelemetryDiagnosticEvent::TelemetryPosthogInitFailed,
                    input.source,
                    error,
                );
                return disabled;
            }
        };

    let allowlists = input
        .property_allowlist
        .iter()
        .map(|(name, keys)| (name.clone(), keys.iter().cloned().collect()))
        .collect();
    let mut shared_properties = TelemetryCaptureProperties::new();
    if let Some(additional) = &product.additional_properties {
        for (key, value) in additional {
            shared_properties.insert(key.clone(), value.clone());
        }
    }
    shared_properties.insert("platform".into(), product.platform.clone().into());
    shared_properties.insert("product_name".into(), product.product_name.clone().into());
    shared_properties.insert(
        "package_version".into(),
        product.package_version.clone().into(),
    );
    shared_properties.insert("schema_version".into(), input.schema_version.into());
    shared_properties.insert("$process_person_profile".into(), false.into());

    EventTelemetryClient {
        inner: Some(EnabledEventClient {
            transport,
            allowlists,
            shared_properties,
            distinct_id: input.distinct_id.to_string(),
            diagnostics: input.diagnostics.clone(),
            on_capture: input.on_capture.clone(),
            set_timeout_fn: input.set_timeout_fn.clone(),
            source: input.source.to_string(),
        }),
    }
}

/// `EVENT_UUID` of the TS original: lowercase hex groups, exactly 36 characters.
fn is_valid_event_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte),
        })
}

fn is_forbidden_key(key: &str, value: &Value) -> bool {
    key == "$ip"
        || (value.is_string()
            && FORBIDDEN_SUFFIXES
                .iter()
                .any(|suffix| key.ends_with(suffix)))
        || (key.starts_with('$') && !ALLOWED_DOLLAR_KEYS.contains(&key))
}

/// Keeps strings (cut to 64 UTF-16 units, like `String.prototype.slice`), finite numbers and booleans.
fn project_value(value: &Value) -> Option<Value> {
    match value {
        Value::String(text) => {
            let mut units = 0;
            let truncated: String = text
                .chars()
                .take_while(|character| {
                    units += character.len_utf16();
                    units <= MAX_STRING_UTF16_UNITS
                })
                .collect();
            Some(Value::String(truncated))
        }
        Value::Bool(_) | Value::Number(_) => Some(value.clone()),
        Value::Null | Value::Array(_) | Value::Object(_) => None,
    }
}

/// Unused map type kept for parity with the TS `EventTelemetryProperties` alias.
pub type EventTelemetryPropertyMap = Map<String, Value>;
