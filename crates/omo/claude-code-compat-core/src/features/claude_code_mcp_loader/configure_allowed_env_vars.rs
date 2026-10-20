use std::{collections::BTreeSet, sync::RwLock};

pub const BUILTIN_ALLOWED_MCP_ENV_VARS: &[&str] = &[
    "PATH", "HOME", "USER", "SHELL", "TERM", "TMPDIR", "TMP", "TEMP", "PWD",
    "OLDPWD", "LANG", "LC_ALL", "LC_CTYPE", "EDITOR", "VISUAL", "XDG_CONFIG_HOME",
    "XDG_DATA_HOME", "XDG_CACHE_HOME", "HOSTNAME", "LOGNAME", "USERPROFILE", "APPDATA", "LOCALAPPDATA",
];
pub const SENSITIVE_MCP_ENV_VAR_PATTERN: &str = "KEY|TOKEN|SECRET|PASSWORD|AUTH|CREDENTIAL";
static ADDITIONAL_ALLOWED_MCP_ENV_VARS: RwLock<BTreeSet<String>> = RwLock::new(BTreeSet::new());

pub fn get_allowed_mcp_env_vars() -> BTreeSet<String> {
    let mut allowed: BTreeSet<String> = BUILTIN_ALLOWED_MCP_ENV_VARS.iter().map(|name| (*name).to_owned()).collect();
    let additional = ADDITIONAL_ALLOWED_MCP_ENV_VARS.read().unwrap_or_else(std::sync::PoisonError::into_inner);
    allowed.extend(additional.iter().cloned());
    allowed
}

pub fn is_sensitive_mcp_env_var(var_name: &str) -> bool {
    let upper = var_name.to_ascii_uppercase();
    SENSITIVE_MCP_ENV_VAR_PATTERN.split('|').any(|part| upper.contains(part))
}

pub fn is_allowed_mcp_env_var(var_name: &str) -> bool {
    get_allowed_mcp_env_vars().contains(var_name)
}

pub fn set_additional_allowed_mcp_env_vars(var_names: &[String]) {
    *ADDITIONAL_ALLOWED_MCP_ENV_VARS.write().unwrap_or_else(std::sync::PoisonError::into_inner) = var_names.iter().cloned().collect();
}

pub fn reset_additional_allowed_mcp_env_vars() {
    ADDITIONAL_ALLOWED_MCP_ENV_VARS.write().unwrap_or_else(std::sync::PoisonError::into_inner).clear();
}
