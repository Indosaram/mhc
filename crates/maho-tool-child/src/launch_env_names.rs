//! senpi `launch-env-names.ts`: the internal environment variable names and the sanitizers over them.
use std::collections::HashMap;
use std::sync::Mutex;

/// A process environment as a name -> value map (`NodeJS.ProcessEnv` without the `undefined` values).
pub type ProcessEnv = HashMap<String, String>;

/// Where both sides write payload references (`<root>/<generation>/`); the server owns and tears it down.
pub const TOOL_CHILD_REFS_ROOT_ENV: &str = "SENPI_TOOL_CHILD_REFS_ROOT";

/// This generation's ledger path; the child reads it to sweep owned groups when its host disappears.
pub const TOOL_CHILD_LEDGER_ENV: &str = "SENPI_TOOL_CHILD_LEDGER";
/// How to re-enter this engine (set by the server when it starts the tool child); the launch shim is started the same way.
pub const TOOL_CHILD_REENTRY_ENV: &str = "SENPI_TOOL_CHILD_REENTRY";
/// The Windows command line a `launch-shim` starts outside the tool child's Job Object.
pub const LAUNCH_SHIM_COMMAND_LINE_ENV: &str = "SENPI_LAUNCH_SHIM_COMMAND_LINE";

/// Every variable the server sets only for the tool child itself. The child scrubs them from its own environment once it
/// has read them, so no command it starts inherits them (a nested senpi would otherwise enter the tool-child role).
/// `internal_env_tests` pins this list against the constants it names.
pub const TOOL_CHILD_INTERNAL_ENV: [&str; 8] = [
    "SENPI_INTERNAL_ROLE",
    "SENPI_TOOL_CHILD_CHANNEL",
    "SENPI_TOOL_CHILD_CHANNEL_SECRET",
    "SENPI_TOOL_CHILD_GENERATION",
    TOOL_CHILD_LEDGER_ENV,
    "SENPI_TOOL_CHILD_MODULE_ROOTS",
    TOOL_CHILD_REFS_ROOT_ENV,
    TOOL_CHILD_REENTRY_ENV,
];

/// Every internal variable: the server's, and the ones the tool child sets for its own launch shim.
pub const INTERNAL_ENV: [&str; 9] = [
    TOOL_CHILD_INTERNAL_ENV[0],
    TOOL_CHILD_INTERNAL_ENV[1],
    TOOL_CHILD_INTERNAL_ENV[2],
    TOOL_CHILD_INTERNAL_ENV[3],
    TOOL_CHILD_INTERNAL_ENV[4],
    TOOL_CHILD_INTERNAL_ENV[5],
    TOOL_CHILD_INTERNAL_ENV[6],
    TOOL_CHILD_INTERNAL_ENV[7],
    LAUNCH_SHIM_COMMAND_LINE_ENV,
];

/// The current process environment as a map (`process.env`); names or values that are not UTF-8 are skipped.
pub fn process_env() -> ProcessEnv {
    std::env::vars_os()
        .filter_map(|(name, value)| Some((name.into_string().ok()?, value.into_string().ok()?)))
        .collect()
}

/// The one sanitizer for the environment of a command a user (or user code) asked for: none of the internal variables,
/// whatever platform path starts it. A user command that kept one could enter a senpi internal role or re-enter the shim.
pub fn without_internal_env(env: &ProcessEnv) -> ProcessEnv {
    without_internal_env_on(env, cfg!(windows))
}

/// [`without_internal_env`] for an explicit platform (`win32` true for Windows).
pub fn without_internal_env_on(env: &ProcessEnv, win32: bool) -> ProcessEnv {
    // Windows environment names are case-insensitive, so `senpi_tool_child_reentry` is the same variable there.
    let key = |name: &str| {
        if win32 {
            name.to_uppercase()
        } else {
            name.to_string()
        }
    };
    let internal: Vec<String> = INTERNAL_ENV.iter().map(|name| key(name)).collect();
    env.iter()
        .filter(|(name, _)| !internal.contains(&key(name)))
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect()
}

/// Removes every internal variable from `env`.
pub fn scrub_tool_child_env(env: &mut ProcessEnv) {
    for name in INTERNAL_ENV {
        env.remove(name);
    }
}

/// `scrubToolChildEnv(process.env)`: removes every internal variable from the real process environment.
/// Like every process-environment write it must only run while the process is single-threaded (`maho_process_env`).
pub fn scrub_process_env() {
    let current = process_env();
    let mut next = current.clone();
    scrub_tool_child_env(&mut next);
    maho_process_env::apply(&current, &next);
}

static REENTRY: Mutex<Option<String>> = Mutex::new(None);

/// The tool child reads its re-entry command once, before the scrub; job workers get it through their worker data.
pub fn remember_tool_child_reentry(raw: Option<String>) {
    *REENTRY
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = raw;
}

pub fn tool_child_reentry() -> Option<String> {
    REENTRY
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_env_lists_exactly_the_variables_the_server_sets_for_the_child() {
        let mut listed: Vec<&str> = TOOL_CHILD_INTERNAL_ENV.to_vec();
        listed.sort_unstable();
        let mut expected = vec![
            crate::channel::TOOL_CHILD_ROLE_ENV,
            crate::channel::TOOL_CHILD_CHANNEL_ENV,
            crate::channel::TOOL_CHILD_SECRET_ENV,
            crate::entry::TOOL_CHILD_GENERATION_ENV,
            TOOL_CHILD_LEDGER_ENV,
            crate::module_job::TOOL_CHILD_MODULE_ROOTS_ENV,
            TOOL_CHILD_REFS_ROOT_ENV,
            TOOL_CHILD_REENTRY_ENV,
        ];
        expected.sort_unstable();
        assert_eq!(listed, expected);
    }

    #[test]
    fn internal_env_adds_the_shim_command_line() {
        assert!(INTERNAL_ENV.contains(&LAUNCH_SHIM_COMMAND_LINE_ENV));
        assert_eq!(INTERNAL_ENV.len(), TOOL_CHILD_INTERNAL_ENV.len() + 1);
    }

    fn env(pairs: &[(&str, &str)]) -> ProcessEnv {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn without_internal_env_drops_every_internal_name() {
        let mut source = env(&[("PATH", "/bin"), ("HOME", "/h")]);
        for name in INTERNAL_ENV {
            source.insert(name.to_string(), "x".to_string());
        }
        assert_eq!(
            without_internal_env_on(&source, false),
            env(&[("PATH", "/bin"), ("HOME", "/h")])
        );
    }

    #[test]
    fn without_internal_env_is_case_insensitive_only_on_windows() {
        let source = env(&[("senpi_tool_child_reentry", "x"), ("KEEP", "1")]);
        assert_eq!(
            without_internal_env_on(&source, true),
            env(&[("KEEP", "1")])
        );
        assert_eq!(without_internal_env_on(&source, false), source);
    }

    #[test]
    fn scrub_removes_internal_names_in_place() {
        let mut source = env(&[
            (TOOL_CHILD_REENTRY_ENV, "x"),
            (LAUNCH_SHIM_COMMAND_LINE_ENV, "y"),
            ("KEEP", "1"),
        ]);
        scrub_tool_child_env(&mut source);
        assert_eq!(source, env(&[("KEEP", "1")]));
    }

    #[test]
    fn reentry_is_remembered_once_read() {
        remember_tool_child_reentry(Some("{\"command\":\"c\",\"args\":[]}".to_string()));
        assert_eq!(
            tool_child_reentry().as_deref(),
            Some("{\"command\":\"c\",\"args\":[]}")
        );
        remember_tool_child_reentry(None);
        assert_eq!(tool_child_reentry(), None);
    }
}
