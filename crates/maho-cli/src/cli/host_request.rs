//! Port of senpi `cli/host-command.ts` (`runHostCommand` -> `runHostRequest`): builds the
//! `maho_rpc::host_runner::HostRequest` for `mhc host ensure|status|stop|handoff` and performs it.
//!
//! The machine-first contract is the pinned one: exactly one JSON line on stdout, an exit code that
//! classifies the outcome (`0` ok, `1` failed, `2` usage/launch-spec, `3` refused, `4` fallback),
//! and diagnostics on stderr. The launch spec is a FILE PATH whose owner/mode make it trustworthy,
//! and the daemon environment is an allowlist of names, never this process's whole environment.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use maho_rpc::host_daemon_env::{daemon_env_keys, daemon_env_overrides};
use maho_rpc::host_daemon_paths::create_host_daemon_paths;
use maho_rpc::host_daemon_state::HostDaemonSettings;
use maho_rpc::host_decision::{HostDecisionClient, HostDecisionPolicy, HOST_PROTOCOL_VERSION, REQUIRED_HOST_CAPABILITIES};
use maho_rpc::host_ensure::{ensure_supervisor_args, host_env, HostStartOptions};
use maho_rpc::host_handoff::HandoffOptions;
use maho_rpc::host_launch::{default_host_launch, PINNED_HOST_CLIENT_CAPABILITIES};
use maho_rpc::host_launch_spec::{load_host_launch_spec, HostLaunchSpecError, ResolvedHostLaunchSpec};
use maho_rpc::host_lifecycle::{HostLifecyclePolicy, DEFAULT_HOST_IDLE_EXIT_MS};
use maho_rpc::host_protocol_info::{RpcLaunchProfileCore, SessionRuntimeKind};
use maho_rpc::host_runner::{run_host_request, EnsureRequest, HostOutcome, HostRequest, HostTarget};
use maho_rpc::protocol_identity::{launch_profile_from_core, resolve_instance_id, HOST_INSTANCE_ID_ENV};

use super::host_command::{HostSubcommand, ParsedHostArgs};

pub const DEFAULT_READINESS_MS: u64 = 10_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostCommandError {
    LaunchSpec { reason: String, detail: String },
    Host(String),
}

impl HostCommandError {
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::LaunchSpec { .. } => 2,
            Self::Host(_) => 1,
        }
    }

    pub fn payload(&self) -> serde_json::Value {
        match self {
            Self::LaunchSpec { reason, detail } => serde_json::json!({ "action": "error", "reason": reason, "detail": detail }),
            Self::Host(detail) => serde_json::json!({ "action": "error", "reason": "host_error", "detail": detail }),
        }
    }
}

fn platform() -> &'static str {
    if cfg!(windows) { "win32" } else { std::env::consts::OS }
}

/// senpi `launchSpec(specPath)`: the trusted resolved spec, or the daemon defaults.
pub fn resolve_launch_spec(spec_path: Option<&str>) -> Result<ResolvedHostLaunchSpec, HostCommandError> {
    match spec_path {
        None => Ok(ResolvedHostLaunchSpec { host_args: Vec::new(), policy: Default::default(), env: BTreeMap::new() }),
        Some(path) => load_host_launch_spec(Path::new(path), platform()).map_err(|error: HostLaunchSpecError| {
            HostCommandError::LaunchSpec { reason: error.reason.to_owned(), detail: error.detail }
        }),
    }
}

fn decision_policy(policy: &str) -> HostDecisionPolicy {
    match policy {
        "fallback" => HostDecisionPolicy::Fallback,
        "never" => HostDecisionPolicy::Never,
        _ => HostDecisionPolicy::Upgrade,
    }
}

fn process_env() -> BTreeMap<String, Option<String>> {
    std::env::vars().map(|(name, value)| (name, Some(value))).collect()
}

fn extension_paths(host_args: &[String]) -> Vec<String> {
    let mut paths = Vec::new();
    let mut index = 0;
    while index < host_args.len() {
        if host_args[index] == "--extension" {
            if let Some(value) = host_args.get(index + 1) {
                paths.push(value.clone());
            }
            index += 2;
        } else {
            index += 1;
        }
    }
    paths
}

pub fn host_target(socket: &str, agent_dir: &Path) -> HostTarget {
    HostTarget { socket: socket.to_owned(), agent_dir: agent_dir.to_path_buf() }
}

fn ensure_request(target: &HostTarget, spec: &ResolvedHostLaunchSpec, policy: HostDecisionPolicy) -> Result<EnsureRequest, HostCommandError> {
    let agent_dir_text = target.agent_dir.to_string_lossy().into_owned();
    let daemon_dir = create_host_daemon_paths(&target.socket, &target.agent_dir).dir;
    let instance_id = resolve_instance_id(std::env::var(HOST_INSTANCE_ID_ENV).ok().as_deref());
    let generation = 0u64;
    let build = maho_core::engine_build_identity::engine_build_identity().clone();
    let launch_profile = launch_profile_from_core(RpcLaunchProfileCore {
        extensions: extension_paths(&spec.host_args),
        multi_session: true,
        session_runtime: SessionRuntimeKind::InProcess,
    })
    .ok();
    let client = HostDecisionClient {
        protocol_version: HOST_PROTOCOL_VERSION,
        required_capabilities: REQUIRED_HOST_CAPABILITIES.iter().map(|value| (*value).into()).collect(),
        identity: build,
        launch_profile: launch_profile.clone(),
        started_by_us: false,
        platform: platform().into(),
    };
    let supervisor_args = ensure_supervisor_args(&target.socket, &agent_dir_text, &spec.host_args);
    let launch = default_host_launch(&supervisor_args).map_err(|error| HostCommandError::Host(error.to_string()))?;
    let host_policy = HostLifecyclePolicy {
        cold_start: spec.policy.cold_start.clone().unwrap_or_else(|| "transient".into()),
        idle_exit_ms: spec.policy.idle_exit_ms.unwrap_or(DEFAULT_HOST_IDLE_EXIT_MS),
    };
    let settings = HostDaemonSettings {
        socket: target.socket.clone(),
        capabilities: PINNED_HOST_CLIENT_CAPABILITIES.iter().map(|value| (*value).into()).collect(),
        cold_start: host_policy.cold_start.clone(),
        idle_exit_ms: host_policy.idle_exit_ms,
        generation: generation as f64,
        instance_id: instance_id.clone(),
    };
    let overrides = daemon_env_overrides(&process_env(), &spec.env, platform());
    let env = host_env(
        std::env::vars().collect(),
        &overrides.iter().map(|(name, value)| (name.clone(), value.clone())).collect::<HashMap<_, _>>(),
        &agent_dir_text,
        &daemon_dir.to_string_lossy(),
        &instance_id,
        generation,
    );
    let launch_profile_id = launch_profile.map_or_else(String::new, |profile| profile.profile_id);
    let start = HostStartOptions {
        env,
        settings,
        launch_profile_id: launch_profile_id.clone(),
        timeout: std::time::Duration::from_millis(DEFAULT_READINESS_MS),
    };
    let handoff = HandoffOptions {
        host_args: supervisor_args,
        policy: Some(host_policy),
        env: overrides.into_iter().collect(),
        launch_profile_id,
        readiness_ms: DEFAULT_READINESS_MS,
    };
    let env_keys = daemon_env_keys(&process_env(), &spec.env, platform());
    Ok(EnsureRequest { client, policy, launch, start, handoff, env_keys })
}

/// senpi `hostRequest(parsed)`: the one request this invocation performs.
pub fn build_host_request(parsed: &ParsedHostArgs, socket: &str, agent_dir: &Path) -> Result<HostRequest, HostCommandError> {
    let target = host_target(socket, agent_dir);
    match parsed.subcommand {
        HostSubcommand::Ensure => {
            let spec = resolve_launch_spec(parsed.spec_path.as_deref())?;
            let policy = decision_policy(&parsed.policy);
            let request = ensure_request(&target, &spec, policy)?;
            Ok(HostRequest::Ensure { target, request: Box::new(request) })
        }
        HostSubcommand::Status => Ok(HostRequest::Status { target, include_workers: parsed.include_workers }),
        HostSubcommand::Stop => Ok(HostRequest::Stop { target, drain: parsed.drain, force: parsed.force }),
        HostSubcommand::Handoff => {
            let spec = resolve_launch_spec(parsed.spec_path.as_deref())?;
            let host_policy = HostLifecyclePolicy {
                cold_start: spec.policy.cold_start.clone().unwrap_or_else(|| "transient".into()),
                idle_exit_ms: spec.policy.idle_exit_ms.unwrap_or(DEFAULT_HOST_IDLE_EXIT_MS),
            };
            let overrides = daemon_env_overrides(&process_env(), &spec.env, platform());
            let options = HandoffOptions {
                host_args: ensure_supervisor_args(socket, &agent_dir.to_string_lossy(), &spec.host_args),
                policy: Some(host_policy),
                env: overrides.into_iter().collect(),
                launch_profile_id: String::new(),
                readiness_ms: DEFAULT_READINESS_MS,
            };
            let env_keys = daemon_env_keys(&process_env(), &spec.env, platform());
            Ok(HostRequest::Handoff { target, options, env_keys })
        }
    }
}

/// senpi `runHostCommand` body: perform the request and return its outcome.
pub async fn run(parsed: &ParsedHostArgs, socket: &str, agent_dir: &Path) -> Result<HostOutcome, HostCommandError> {
    let request = build_host_request(parsed, socket, agent_dir)?;
    run_host_request(request).await.map_err(|error| HostCommandError::Host(error.to_string()))
}
