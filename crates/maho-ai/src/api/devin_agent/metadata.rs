//! Port of senpi packages/ai/src/api/devin-agent/metadata.ts.
//!
//! Cascade `Metadata` construction. Cascade gates behavior on the caller's identity tuple, and the
//! two identities here are the ones the released Devin CLI actually presents.

use crate::api::devin_agent::r#gen::cascade_pb::{DisplayOption, Metadata};

const DEVIN_SESSION_TOKEN_PREFIX: &str = "devin-session-token$";

fn devin_os() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "windows",
        _ => "linux",
    }
}

const DEVIN_LOCALE: &str = "en";

/// `DEVIN_CLI_IDENTITY`.
pub const DEVIN_CLI_IDENTITY: DevinIdentity = DevinIdentity {
    ide_name: "devin-cli",
    ide_type: "chisel",
    ide_version: "3000.6.2",
    extension_name: "chisel",
    extension_version: "3000.6.2",
};

/// `DEVIN_DISCOVERY_IDENTITY`.
pub const DEVIN_DISCOVERY_IDENTITY: DevinIdentity = DevinIdentity {
    ide_name: "chisel",
    ide_type: "",
    ide_version: "0.0.0-dev",
    extension_name: "chisel",
    extension_version: "0.0.0-dev",
};

/// One of the two identity tuples the released CLI announces.
pub struct DevinIdentity {
    pub ide_name: &'static str,
    pub ide_type: &'static str,
    pub ide_version: &'static str,
    pub extension_name: &'static str,
    pub extension_version: &'static str,
}

/// `DEVIN_SUPPORTED_MODEL_DISPLAYS`: asking for the internal slots is what makes Cascade return its
/// full catalog; the internal ones are filtered client-side, exactly as the native client does.
pub const DEVIN_SUPPORTED_MODEL_DISPLAYS: [DisplayOption; 5] = [
    DisplayOption::ModelRouter,
    DisplayOption::QuickReview,
    DisplayOption::InternalDefault,
    DisplayOption::Unclassified,
    DisplayOption::Normal,
];

/// `normalizeDevinSessionToken`: the session token as the wire format carries it - the scheme
/// prefix is required.
pub fn normalize_devin_session_token(api_key: Option<&str>) -> String {
    match api_key {
        None => String::new(),
        Some("") => String::new(),
        Some(api_key) if api_key.starts_with(DEVIN_SESSION_TOKEN_PREFIX) => api_key.to_owned(),
        Some(api_key) => format!("{DEVIN_SESSION_TOKEN_PREFIX}{api_key}"),
    }
}

fn metadata_of(identity: &DevinIdentity, api_key: Option<&str>) -> Metadata {
    Metadata {
        ide_name: identity.ide_name.to_owned(),
        extension_version: identity.extension_version.to_owned(),
        api_key: normalize_devin_session_token(api_key),
        locale: DEVIN_LOCALE.to_owned(),
        os: devin_os().to_owned(),
        disable_telemetry: false,
        ide_version: identity.ide_version.to_owned(),
        hardware: String::new(),
        request_id: 0,
        session_id: String::new(),
        extension_name: identity.extension_name.to_owned(),
        user_jwt: String::new(),
        force_team_id: String::new(),
        device_fingerprint: String::new(),
        ide_type: identity.ide_type.to_owned(),
        supported_model_displays: Vec::new(),
    }
}

/// `devinCliMetadata`: the released chat identity, with the session token and the optional JWT.
pub fn devin_cli_metadata(api_key: Option<&str>, user_jwt: &str) -> Metadata {
    Metadata { user_jwt: user_jwt.to_owned(), ..metadata_of(&DEVIN_CLI_IDENTITY, api_key) }
}

/// `devinDiscoveryMetadata`: the dev-channel `chisel` identity plus the native display slots.
pub fn devin_discovery_metadata(api_key: Option<&str>) -> Metadata {
    Metadata {
        supported_model_displays: DEVIN_SUPPORTED_MODEL_DISPLAYS.iter().map(|display| *display as i32).collect(),
        ..metadata_of(&DEVIN_DISCOVERY_IDENTITY, api_key)
    }
}
