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
    transport: Box<dyn TelemetryTransport>,
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
        let Some(client) = &self.inner else {
            return;
        };
        let Some(allowlist) = client.allowlists.get(name).filter(|_| !name.is_empty()) else {
            client.report(
                TelemetryDiagnosticEvent::TelemetryEventRejected,
                "event name is empty or not allowlisted".to_string(),
            );
            return;
        };

        let mut payload_properties = TelemetryCaptureProperties::new();
        for (key, value) in properties {
            if is_forbidden_key(key, value) {
                client.report(
                    TelemetryDiagnosticEvent::TelemetryEventPropertyRejected,
                    format!("forbidden event property: {key}"),
                );
                continue;
            }
            if !allowlist.contains(key) {
                client.report(
                    TelemetryDiagnosticEvent::TelemetryEventPropertyDropped,
                    format!("unknown event property: {key}"),
                );
                continue;
            }
            let Some(projected) = project_value(value) else {
                client.report(
                    TelemetryDiagnosticEvent::TelemetryEventPropertyRejected,
                    format!("invalid event property value: {key}"),
                );
                continue;
            };
            payload_properties.insert(key.clone(), projected);
        }
        for (key, value) in &client.shared_properties {
            payload_properties.insert(key.clone(), value.clone());
        }

        let payload = TelemetryCaptureMessage {
            distinct_id: client.distinct_id.clone(),
            event: name.to_string(),
            properties: payload_properties,
        };
        match client.transport.capture(&payload) {
            Ok(()) => {
                if let Some(on_capture) = &client.on_capture {
                    on_capture(&payload);
                }
            }
            Err(error) => report(
                client.diagnostics.as_ref(),
                TelemetryDiagnosticEvent::TelemetryCaptureFailed,
                &client.source,
                error,
            ),
        }
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
    options.disable_geoip = true;
    options.flush_at = EVENT_FLUSH_AT;
    options.flush_interval = EVENT_FLUSH_INTERVAL_MS;

    let api_key = get_telemetry_api_key(input.env, &product.default_api_key);
    let transport = match build_transport(input.transport_factory.as_ref(), &api_key, &options) {
        Ok(transport) => transport,
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
