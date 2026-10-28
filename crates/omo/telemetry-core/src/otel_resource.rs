//! Rust port of `packages/telemetry-core/src/otel-resource.ts`.

use crate::otel_time::OTEL_SDK_LANGUAGE;
use crate::otel_time::OTEL_SDK_NAME;
use crate::otel_time::OTEL_SDK_VERSION;
use crate::otlp_encode::ResourceAttributes;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TelemetryResourceAttribution {
    pub service_name: String,
    pub service_version: String,
    pub instance_id: String,
    pub install_hash: Option<String>,
    pub platform: String,
    pub os_version: Option<String>,
    pub arch: String,
    pub schema_version: i64,
    pub surface: String,
    pub ci: bool,
}

pub fn otel_os_type(platform: &str) -> Option<&'static str> {
    match platform {
        "darwin" => Some("darwin"),
        "linux" => Some("linux"),
        "win32" => Some("windows"),
        _ => None,
    }
}

pub fn otel_host_arch(arch: &str) -> &'static str {
    match arch {
        "x64" => "amd64",
        "arm64" => "arm64",
        "ia32" => "x86",
        "arm" => "arm32",
        _ => "other",
    }
}

pub fn create_telemetry_resource(attribution: &TelemetryResourceAttribution) -> ResourceAttributes {
    let mut resource = ResourceAttributes::new();
    resource.insert("service.name".into(), attribution.service_name.clone().into());
    resource.insert(
        "service.version".into(),
        attribution.service_version.clone().into(),
    );
    resource.insert(
        "service.instance.id".into(),
        attribution.instance_id.clone().into(),
    );
    if let Some(install_hash) = &attribution.install_hash {
        resource.insert("omo.install.hash".into(), install_hash.clone().into());
    }
    if let Some(os_type) = otel_os_type(&attribution.platform) {
        resource.insert("os.type".into(), os_type.into());
    }
    if let Some(os_version) = &attribution.os_version {
        resource.insert("os.version".into(), os_version.clone().into());
    }
    resource.insert("host.arch".into(), otel_host_arch(&attribution.arch).into());
    resource.insert("telemetry.sdk.name".into(), OTEL_SDK_NAME.into());
    resource.insert(
        "telemetry.sdk.language".into(),
        OTEL_SDK_LANGUAGE.into(),
    );
    resource.insert("telemetry.sdk.version".into(), OTEL_SDK_VERSION.into());
    resource.insert(
        "omo.schema.version".into(),
        attribution.schema_version.into(),
    );
    resource.insert("omo.surface".into(), attribution.surface.clone().into());
    resource.insert("omo.ci".into(), attribution.ci.into());
    resource
}
