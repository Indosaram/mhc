use std::collections::HashMap;

pub const MEMBER_IDENTITY_ENV: &str = "SENPI_TASK_MEMBER";
pub const MEMBER_TASK_ID_ENV: &str = "SENPI_TASK_MEMBER_TASK_ID";
pub const MEMBER_TEAM_CONFIG_ENV: &str = "SENPI_TASK_TEAM_CONFIG";
pub const MEMBER_PROCESS_ENV_NAMES: [&str; 3] =
    [MEMBER_IDENTITY_ENV, MEMBER_TASK_ID_ENV, MEMBER_TEAM_CONFIG_ENV];
pub const MEMBER_EXTENSION_BUNDLE_NAME: &str = "omo-member.js";

/// Reports whether the given env carries a non-empty member identity.
pub fn is_team_member_process(env: &HashMap<String, String>) -> bool {
    env.get(MEMBER_IDENTITY_ENV)
        .is_some_and(|identity| !identity.is_empty())
}

/// Same as [`is_team_member_process`] but reads the current process env.
pub fn is_team_member_process_from_env() -> bool {
    std::env::var(MEMBER_IDENTITY_ENV).is_ok_and(|identity| !identity.is_empty())
}
