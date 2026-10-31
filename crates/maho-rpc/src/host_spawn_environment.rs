//! The environment a host generation is spawned with (senpi `rpc/host-spawn-environment.ts`).
use crate::custom_capability::RPC_CLIENT_CAPABILITIES_ENV;
use crate::host_daemon_env::daemon_env_is_allowed;
use crate::host_daemon_paths::{HOST_DAEMON_DIR_ENV, HostDaemonPaths};
use crate::host_idle_handover::EXPECTED_RUNTIME_BUILD_ID_ENV;
use crate::host_launch::PINNED_HOST_CLIENT_CAPABILITIES;
use crate::protocol_identity::{HOST_GENERATION_ENV, HOST_INSTANCE_ID_ENV};
use std::collections::BTreeMap;

pub type HostEnvironment = BTreeMap<String, String>;

fn delete_name(env: &mut HostEnvironment, name: &str, platform: &str) {
    if platform == "win32" {
        env.retain(|existing, _| !existing.eq_ignore_ascii_case(name));
    } else {
        env.remove(name);
    }
}

/// senpi `daemonEnvironment`: the allowed slice of the process env, the caller's overrides (allowed names
/// only; `None` deletes) and the forced names last.
pub fn daemon_environment(
    process_env: &BTreeMap<String, String>,
    overrides: &BTreeMap<String, Option<String>>,
    forced: &BTreeMap<String, String>,
    platform: &str,
) -> HostEnvironment {
    let mut env: HostEnvironment = process_env
        .iter()
        .filter(|(name, _)| daemon_env_is_allowed(name, platform))
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    for (name, value) in overrides {
        if !daemon_env_is_allowed(name, platform) {
            continue;
        }
        delete_name(&mut env, name, platform);
        if let Some(value) = value {
            env.insert(name.clone(), value.clone());
        }
    }
    for (name, value) in forced {
        delete_name(&mut env, name, platform);
        env.insert(name.clone(), value.clone());
    }
    env
}

pub struct InitialHostEnvironment<'a> {
    pub agent_dir: Option<&'a str>,
    pub env: &'a BTreeMap<String, Option<String>>,
    pub paths: &'a HostDaemonPaths,
    pub instance_id: &'a str,
    pub generation: u64,
}

fn identity(paths: &HostDaemonPaths, instance_id: &str, generation: u64) -> HostEnvironment {
    BTreeMap::from([
        (
            RPC_CLIENT_CAPABILITIES_ENV.to_owned(),
            PINNED_HOST_CLIENT_CAPABILITIES.join(","),
        ),
        // Always SET, never inherited: two generations claiming one instance id make the handoff
        // completion signal impossible to observe.
        (HOST_INSTANCE_ID_ENV.to_owned(), instance_id.to_owned()),
        (HOST_GENERATION_ENV.to_owned(), generation.to_string()),
        (
            HOST_DAEMON_DIR_ENV.to_owned(),
            paths.dir.to_string_lossy().into_owned(),
        ),
    ])
}

fn process_env() -> BTreeMap<String, String> {
    std::env::vars().collect()
}

pub fn initial_host_environment(
    options: &InitialHostEnvironment<'_>,
    platform: &str,
) -> HostEnvironment {
    initial_host_environment_from(&process_env(), options, platform)
}

pub fn initial_host_environment_from(
    process_env: &BTreeMap<String, String>,
    options: &InitialHostEnvironment<'_>,
    platform: &str,
) -> HostEnvironment {
    let mut forced = identity(options.paths, options.instance_id, options.generation);
    let agent_dir = options
        .agent_dir
        .map_or_else(maho_core::config::get_agent_dir, str::to_owned);
    forced.insert(maho_core::config::env_agent_dir_var(), agent_dir);
    daemon_environment(process_env, options.env, &forced, platform)
}

pub struct SuccessorHostEnvironment<'a> {
    pub agent_dir: Option<&'a str>,
    pub env: &'a BTreeMap<String, Option<String>>,
    pub expected_runtime_build_id: Option<&'a str>,
    pub paths: &'a HostDaemonPaths,
    pub instance_id: &'a str,
    pub generation: u64,
}

pub fn successor_host_environment(
    options: &SuccessorHostEnvironment<'_>,
    platform: &str,
) -> HostEnvironment {
    successor_host_environment_from(&process_env(), options, platform)
}

pub fn successor_host_environment_from(
    process_env: &BTreeMap<String, String>,
    options: &SuccessorHostEnvironment<'_>,
    platform: &str,
) -> HostEnvironment {
    let mut forced = identity(options.paths, options.instance_id, options.generation);
    if let Some(agent_dir) = options.agent_dir.filter(|dir| !dir.is_empty()) {
        forced.insert(maho_core::config::env_agent_dir_var(), agent_dir.to_owned());
    }
    if let Some(build_id) = options.expected_runtime_build_id {
        forced.insert(
            EXPECTED_RUNTIME_BUILD_ID_ENV.to_owned(),
            build_id.to_owned(),
        );
    }
    daemon_environment(process_env, options.env, &forced, platform)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host_daemon_paths::host_daemon_directory_paths;
    use std::path::Path;

    #[test]
    fn identity_is_forced_and_transient_names_are_dropped() {
        let paths = host_daemon_directory_paths(Path::new("/agent/rpc-host-daemon/abc"));
        let process = BTreeMap::from([
            ("PATH".to_owned(), "/bin".to_owned()),
            (HOST_INSTANCE_ID_ENV.to_owned(), "parent".to_owned()),
            ("SECRET_TOKEN".to_owned(), "x".to_owned()),
        ]);
        let overrides = BTreeMap::from([
            ("PATH".to_owned(), None),
            ("SENPI_X".to_owned(), Some("1".to_owned())),
            ("NOT_ALLOWED".to_owned(), Some("1".to_owned())),
        ]);
        let env = initial_host_environment_from(
            &process,
            &InitialHostEnvironment {
                agent_dir: Some("/agent"),
                env: &overrides,
                paths: &paths,
                instance_id: "new",
                generation: 3,
            },
            "linux",
        );
        assert_eq!(env[HOST_INSTANCE_ID_ENV], "new");
        assert_eq!(env[HOST_GENERATION_ENV], "3");
        assert_eq!(env[HOST_DAEMON_DIR_ENV], "/agent/rpc-host-daemon/abc");
        assert_eq!(env[&maho_core::config::env_agent_dir_var()], "/agent");
        assert_eq!(env["SENPI_X"], "1");
        assert!(
            !env.contains_key("PATH")
                && !env.contains_key("SECRET_TOKEN")
                && !env.contains_key("NOT_ALLOWED")
        );
    }

    #[test]
    fn a_successor_names_the_expected_build_and_keeps_an_unset_agent_dir_unset() {
        let paths = host_daemon_directory_paths(Path::new("/d/x"));
        let env = successor_host_environment_from(
            &BTreeMap::new(),
            &SuccessorHostEnvironment {
                agent_dir: None,
                env: &BTreeMap::new(),
                expected_runtime_build_id: Some("b1"),
                paths: &paths,
                instance_id: "i",
                generation: 1,
            },
            "linux",
        );
        assert_eq!(env[EXPECTED_RUNTIME_BUILD_ID_ENV], "b1");
        assert!(!env.contains_key(&maho_core::config::env_agent_dir_var()));
    }
}
