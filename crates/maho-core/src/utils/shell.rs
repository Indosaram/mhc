//! Port of the environment helper of senpi `packages/coding-agent/src/utils/shell.ts`.

use std::collections::BTreeMap;

/// `getShellEnv()`: the process environment (including extension `process.env` writes) with the
/// agent bin dir prepended to PATH.
pub fn get_shell_env() -> BTreeMap<String, String> {
    let mut env = maho_ext_api::process_env::vars();
    let key = env.keys().find(|key| key.eq_ignore_ascii_case("path")).cloned().unwrap_or_else(|| "PATH".to_owned());
    let current = env.get(&key).cloned().unwrap_or_default();
    let bin = crate::config::get_bin_dir();
    let delimiter = if cfg!(windows) { ';' } else { ':' };
    if !current.split(delimiter).any(|entry| entry == bin) { env.insert(key, if current.is_empty() { bin } else { format!("{bin}{delimiter}{current}") }); }
    env
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_env_puts_the_agent_bin_dir_on_path_once() {
        let env = get_shell_env();
        let key = env.keys().find(|key| key.eq_ignore_ascii_case("path")).expect("path key");
        let bin = crate::config::get_bin_dir();
        let delimiter = if cfg!(windows) { ';' } else { ':' };
        assert_eq!(env[key].split(delimiter).filter(|entry| *entry == bin).count(), 1);
    }
}
