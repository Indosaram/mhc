use std::path::Path;

use chrono::DateTime;
use chrono::Utc;

use crate::activity_state::DailyActiveCaptureStateInput;
use crate::activity_state::get_daily_active_capture_state;
use crate::machine_id::get_default_telemetry_os_provider;
use crate::machine_id::get_telemetry_distinct_id;
use crate::posthog_client::CreateTelemetryClientInput;
use crate::posthog_client::TelemetryClientEnabledInput;
use crate::posthog_client::create_telemetry_client;
use crate::posthog_client::is_telemetry_client_enabled;
use crate::types::TelemetryDiagnosticInput;
use crate::types::TelemetryDiagnostics;
use crate::types::TelemetryEnv;
use crate::types::TelemetryError;
use crate::types::TelemetryOsProvider;
use crate::types::TelemetryProductConfig;
use crate::types::TelemetryTransportFactory;
use crate::types::TrackActiveInput;

#[derive(Clone)]
pub struct RecordDailyActiveInput<'a> {
    pub diagnostics: Option<TelemetryDiagnostics>,
    pub env: Option<&'a TelemetryEnv>,
    pub now: Option<DateTime<Utc>>,
    pub os_provider: Option<&'a dyn TelemetryOsProvider>,
    pub product: &'a TelemetryProductConfig,
    pub reason: &'a str,
    pub source: &'a str,
    pub state_dir: &'a Path,
    pub transport_factory: Option<TelemetryTransportFactory>,
}

/// Sends the product's daily-active event at most once per UTC day; state is untouched when disabled.
///
/// Only a transport flush failure is returned, matching the TS promise rejection.
pub async fn record_daily_active(input: &RecordDailyActiveInput<'_>) -> Result<(), TelemetryError> {
    if !is_telemetry_client_enabled(&TelemetryClientEnabledInput::for_product(
        input.env,
        input.product,
    )) {
        return Ok(());
    }

    let client = create_telemetry_client(&CreateTelemetryClientInput {
        diagnostics: input.diagnostics.clone(),
        env: input.env,
        os_provider: input.os_provider,
        product: input.product,
        source: input.source,
        transport_factory: input.transport_factory.clone(),
    });
    if !client.enabled() {
        return Ok(());
    }

    let diagnostics: Option<&dyn Fn(&TelemetryDiagnosticInput)> = match &input.diagnostics {
        Some(sink) => Some(sink.as_ref()),
        None => None,
    };
    let activity_state = get_daily_active_capture_state(&DailyActiveCaptureStateInput {
        state_dir: input.state_dir,
        now: input.now,
        diagnostics,
    });

    if !activity_state.capture_daily {
        client.shutdown().await;
        return Ok(());
    }

    let os_provider = input
        .os_provider
        .unwrap_or(get_default_telemetry_os_provider());
    client.track_active(&TrackActiveInput {
        day_utc: activity_state.day_utc,
        distinct_id: get_telemetry_distinct_id(&input.product.machine_id_prefix, os_provider),
        reason: input.reason.to_string(),
    });
    client.flush().await?;
    client.shutdown().await;
    Ok(())
}
