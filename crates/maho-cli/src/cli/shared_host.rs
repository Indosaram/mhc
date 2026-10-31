//! The extension session profile the classic launch hands its loader, and the env-flag reader
//! `main.ts` shares with its other brand-prefixed switches.
//!
//! senpi 58a08c18fa (before the v2026.10.27-2 pin) removed the interactive shared-host join: an
//! interactive launch always runs on its own local runtime, `ENABLE_SHARED_HOST` /
//! `DISABLE_SHARED_HOST` and the `experimental.sharedHost` setting are read nowhere, and
//! `resourceLoaderOptions` no longer carries `sharedHostEnabled`. `ExtensionSessionProfile` still has
//! the field (owned by maho-ext-api); a classic launch always sends `false`.

use maho_core::project_trust::AppMode;

/// senpi `main.ts` `isTruthyEnvFlag`.
pub fn is_truthy_env_flag(value: Option<&str>) -> bool {
    let Some(value) = value else { return false };
    if value.is_empty() {
        return false;
    }
    value == "1" || value.eq_ignore_ascii_case("true") || value.eq_ignore_ascii_case("yes")
}

/// senpi `createCliRuntimeFactory` `resourceLoaderOptions`: the per-session identity
/// (`sessionKind`, `sessionContext`) that reaches the loaded extensions. The app mode and settings
/// no longer select a shared host.
pub fn session_profile(
    _app_mode: AppMode,
    _settings: &maho_core::settings_manager::SettingsManager,
    session_kind: Option<maho_ext_api::SessionKind>,
    session_context: Option<maho_ext_api::SessionContext>,
) -> maho_ext_api::ExtensionSessionProfile {
    maho_ext_api::ExtensionSessionProfile {
        shared_host_enabled: false,
        session_kind: session_kind.unwrap_or_default(),
        session_context: session_context.unwrap_or_default(),
    }
}
