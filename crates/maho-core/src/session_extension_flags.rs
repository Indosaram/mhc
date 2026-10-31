//! Port of senpi `packages/coding-agent/src/core/session-extension-flags.ts`.

use std::collections::BTreeMap;

use maho_ext_api::FlagValue;

use crate::agent_session_runtime::AgentSessionLaunchProfile;

/// The host's flag values with the launch profile's permission preset laid over them.
pub fn session_extension_flag_values(
    host_flags: &BTreeMap<String, FlagValue>,
    launch_profile: Option<&AgentSessionLaunchProfile>,
) -> BTreeMap<String, FlagValue> {
    let mut flags = host_flags.clone();
    if let Some(preset) = launch_profile.and_then(|profile| profile.permission_preset.as_ref()) {
        flags.insert(
            "permission-preset".to_owned(),
            FlagValue::String(preset.clone()),
        );
    }
    flags
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(permission_preset: Option<&str>) -> AgentSessionLaunchProfile {
        AgentSessionLaunchProfile {
            cwd: "/w".into(),
            permission_preset: permission_preset.map(str::to_owned),
            creation_model: None,
            initial_thinking_level: None,
            auto_title: None,
            prompt_size_inventory: None,
        }
    }

    #[test]
    fn the_launch_preset_overrides_the_host_flag() {
        let host = BTreeMap::from([
            (
                "permission-preset".to_owned(),
                FlagValue::String("host".into()),
            ),
            ("verbose".to_owned(), FlagValue::Boolean(true)),
        ]);
        let flags = session_extension_flag_values(&host, Some(&profile(Some("strict"))));
        assert_eq!(
            flags.get("permission-preset"),
            Some(&FlagValue::String("strict".into()))
        );
        assert_eq!(flags.get("verbose"), Some(&FlagValue::Boolean(true)));
        assert_eq!(
            session_extension_flag_values(&host, Some(&profile(None))),
            host
        );
        assert_eq!(session_extension_flag_values(&host, None), host);
    }
}
