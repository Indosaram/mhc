use crate::env::ShouldDisableTelemetryInput;
use crate::env::get_telemetry_api_key;
use crate::env::get_telemetry_host;
use crate::env::has_telemetry_api_key;
use crate::env::should_disable_telemetry;
use crate::machine_id::get_default_telemetry_os_provider;
use crate::posthog_transport::create_default_posthog_transport;
use crate::types::TelemetryCaptureMessage;
use crate::types::TelemetryCaptureProperties;
use crate::types::TelemetryDiagnosticEvent;
use crate::types::TelemetryDiagnosticInput;
use crate::types::TelemetryDiagnostics;
use crate::types::TelemetryEnv;
use crate::types::TelemetryError;
use crate::types::TelemetryOsProvider;
use crate::types::TelemetryProductConfig;
use crate::types::TelemetryTransport;
use crate::types::TelemetryTransportFactory;
use crate::types::TelemetryTransportOptions;
use crate::types::TrackActiveInput;

/// Value reported as `runtime`; the TS package reported `"bun"`.
pub const TELEMETRY_RUNTIME: &str = "rust";
const BYTES_PER_GB: u64 = 1024 * 1024 * 1024;

#[derive(Clone)]
pub struct CreateTelemetryClientInput<'a> {
    pub diagnostics: Option<TelemetryDiagnostics>,
    pub env: Option<&'a TelemetryEnv>,
    pub os_provider: Option<&'a dyn TelemetryOsProvider>,
    pub product: &'a TelemetryProductConfig,
    pub source: &'a str,
    pub transport_factory: Option<TelemetryTransportFactory>,
}

#[derive(Debug, Clone, Copy)]
pub struct TelemetryClientEnabledInput<'a> {
    pub env: Option<&'a TelemetryEnv>,
    pub default_api_key: &'a str,
    pub product_env_prefix: &'a str,
}

impl<'a> TelemetryClientEnabledInput<'a> {
    pub fn for_product(env: Option<&'a TelemetryEnv>, product: &'a TelemetryProductConfig) -> Self {
        Self {
            env,
            default_api_key: &product.default_api_key,
            product_env_prefix: &product.product_env_prefix,
        }
    }
}

pub fn is_telemetry_client_enabled(input: &TelemetryClientEnabledInput<'_>) -> bool {
    !should_disable_telemetry(&ShouldDisableTelemetryInput {
        env: input.env,
        global_env_prefix: None,
        product_env_prefix: input.product_env_prefix,
    }) && has_telemetry_api_key(input.env, input.default_api_key)
}

pub(crate) fn report(
    diagnostics: Option<&TelemetryDiagnostics>,
    event: TelemetryDiagnosticEvent,
    source: &str,
    error: TelemetryError,
) {
    if let Some(diagnostics) = diagnostics {
        diagnostics(&TelemetryDiagnosticInput::failure(event, source, error));
    }
}

pub(crate) fn build_transport(
    factory: Option<&TelemetryTransportFactory>,
    api_key: &str,
    options: &TelemetryTransportOptions,
) -> Result<Box<dyn TelemetryTransport>, TelemetryError> {
    match factory {
        Some(factory) => factory(api_key, options),
        None => create_default_posthog_transport(api_key, options),
    }
}

struct EnabledTelemetryClient {
    transport: Box<dyn TelemetryTransport>,
    shared_properties: TelemetryCaptureProperties,
    event_name: String,
    diagnostics: Option<TelemetryDiagnostics>,
    source: String,
}

/// Daily-active client; a disabled client is a no-op whose `enabled` is `false`.
pub struct TelemetryClient {
    inner: Option<EnabledTelemetryClient>,
}

impl TelemetryClient {
    pub fn enabled(&self) -> bool {
        self.inner.is_some()
    }

    pub fn track_active(&self, input: &TrackActiveInput) {
        let Some(client) = &self.inner else {
            return;
        };
        let mut properties = client.shared_properties.clone();
        properties.insert("$process_person_profile".into(), false.into());
        properties.insert("day_utc".into(), input.day_utc.clone().into());
        properties.insert("reason".into(), input.reason.clone().into());
        let message = TelemetryCaptureMessage {
            distinct_id: input.distinct_id.clone(),
            event: client.event_name.clone(),
            properties,
        };
        if let Err(error) = client.transport.capture(&message) {
            report(
                client.diagnostics.as_ref(),
                TelemetryDiagnosticEvent::TelemetryCaptureFailed,
                &client.source,
                error,
            );
        }
    }

    /// Flush failures propagate to the caller, as in the TS original.
    pub async fn flush(&self) -> Result<(), TelemetryError> {
        match self
            .inner
            .as_ref()
            .and_then(|client| client.transport.flush())
        {
            Some(flush) => flush.await,
            None => Ok(()),
        }
    }

    pub async fn shutdown(&self) {
        let Some(client) = &self.inner else {
            return;
        };
        if let Err(error) = client.transport.shutdown().await {
            report(
                client.diagnostics.as_ref(),
                TelemetryDiagnosticEvent::TelemetryShutdownFailed,
                &client.source,
                error,
            );
        }
    }
}

pub fn create_telemetry_client(input: &CreateTelemetryClientInput<'_>) -> TelemetryClient {
    let disabled = TelemetryClient { inner: None };
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
        flush_at: 1,
        flush_interval: 0,
        host: get_telemetry_host(input.env, &product.default_host),
        disable_geoip: product.disable_geoip.unwrap_or(false),
    };
    if let Some(overrides) = &product.transport_options {
        options.apply(overrides);
    }
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

    TelemetryClient {
        inner: Some(EnabledTelemetryClient {
            transport,
            shared_properties: get_shared_properties(input),
            event_name: product.event_name.clone(),
            diagnostics: input.diagnostics.clone(),
            source: input.source.to_string(),
        }),
    }
}

fn insert_optional(properties: &mut TelemetryCaptureProperties, key: &str, value: Option<String>) {
    if let Some(value) = value {
        properties.insert(key.into(), value.into());
    }
}

fn get_shared_properties(input: &CreateTelemetryClientInput<'_>) -> TelemetryCaptureProperties {
    let os_provider = input
        .os_provider
        .unwrap_or(get_default_telemetry_os_provider());
    let product = input.product;
    let (cpu_count, cpu_model) = match os_provider.cpus() {
        Ok(cpus) => (cpus.len(), cpus.first().map(|cpu| cpu.model.clone())),
        Err(error) => {
            report(
                input.diagnostics.as_ref(),
                TelemetryDiagnosticEvent::TelemetryCpuInfoUnavailable,
                "shared",
                error,
            );
            (0, None)
        }
    };
    let total_memory_gb = os_provider.totalmem().saturating_add(BYTES_PER_GB / 2) / BYTES_PER_GB;

    let mut properties = TelemetryCaptureProperties::new();
    properties.insert("platform".into(), product.platform.clone().into());
    properties.insert("product_name".into(), product.product_name.clone().into());
    properties.insert("package_name".into(), product.package_name.clone().into());
    properties.insert(
        "package_version".into(),
        product.package_version.clone().into(),
    );
    properties.insert("runtime".into(), TELEMETRY_RUNTIME.into());
    properties.insert("runtime_version".into(), env!("CARGO_PKG_VERSION").into());
    properties.insert("source".into(), input.source.into());
    properties.insert("$os".into(), os_provider.platform().into());
    properties.insert("$os_version".into(), os_provider.release().into());
    properties.insert("os_arch".into(), os_provider.arch().into());
    properties.insert("os_type".into(), os_provider.os_type().into());
    properties.insert("cpu_count".into(), cpu_count.into());
    insert_optional(&mut properties, "cpu_model", cpu_model);
    properties.insert("total_memory_gb".into(), total_memory_gb.into());
    insert_optional(&mut properties, "locale", sys_locale::get_locale());
    insert_optional(
        &mut properties,
        "timezone",
        iana_time_zone::get_timezone().ok(),
    );
    insert_optional(&mut properties, "shell", std::env::var("SHELL").ok());
    let ci = std::env::var("CI").is_ok_and(|value| !value.is_empty());
    properties.insert("ci".into(), ci.into());
    insert_optional(
        &mut properties,
        "terminal",
        std::env::var("TERM_PROGRAM").ok(),
    );
    if let Some(additional) = &product.additional_properties {
        for (key, value) in additional {
            properties.insert(key.clone(), value.clone());
        }
    }
    properties
}
