use sha2::Digest;
use sha2::Sha256;

use crate::system_os::SystemTelemetryOsProvider;
use crate::types::TelemetryOsProvider;

pub fn get_default_telemetry_os_provider() -> &'static dyn TelemetryOsProvider {
    &SystemTelemetryOsProvider
}

/// `sha256(prefix + hostname)` as lowercase hex.
pub fn get_telemetry_distinct_id(
    machine_id_prefix: &str,
    os_provider: &dyn TelemetryOsProvider,
) -> String {
    let digest = Sha256::digest(format!("{machine_id_prefix}{}", os_provider.hostname()));
    hex::encode(digest)
}
