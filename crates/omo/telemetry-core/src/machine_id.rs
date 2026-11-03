use crate::system_os::SystemTelemetryOsProvider;
use crate::types::TelemetryOsProvider;

pub fn get_default_telemetry_os_provider() -> &'static dyn TelemetryOsProvider {
    &SystemTelemetryOsProvider
}
