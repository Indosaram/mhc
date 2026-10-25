//! Port of `packages/omo-senpi/src/components/telemetry/session-role-resolver.ts`.
//!
//! The reader of what the task engine writes into a session's context. In the Rust host that context is
//! `ExtensionSessionProfile::session_context`, with the per-child process environment as the fallback for a
//! runner where one process really is one child.

use std::collections::BTreeMap;

use maho_ext_api::ExtensionSessionProfile;

use crate::session_role::TelemetrySessionRole;

/// Session-context key of the throwaway session that warms a fresh task host.
pub const HOST_WARMUP_CONTEXT: &str = "host_warmup";
/// Session-context keys the task engine writes for an omo-spawned session.
pub const SESSION_CONTEXT_ROLE: &str = "role";
pub const SESSION_CONTEXT_TRACEPARENT: &str = "traceparent";
pub const SESSION_ROLES: [&str; 3] = ["child", "dag_child", "member"];
/// Process-environment fallbacks of the per-child runner.
pub const OMO_SENPI_TASK_RPC_CHILD: &str = "OMO_SENPI_TASK_RPC_CHILD";
pub const OMO_SENPI_TASK_TRACEPARENT: &str = "OMO_SENPI_TASK_TRACEPARENT";
pub const OMO_SENPI_TASK_MEMBER: &str = "SENPI_TASK_MEMBER";

/// The per-session labels the opener attached; `None` when the host reports none.
pub fn read_session_context(
    profile: Option<&ExtensionSessionProfile>,
) -> Option<&BTreeMap<String, String>> {
    profile
        .map(|profile| &profile.session_context)
        .filter(|context| !context.is_empty())
}

/// A host warm-up session: no user and no task behind it, so nothing may report it as a session.
pub fn is_host_warmup_session(profile: Option<&ExtensionSessionProfile>) -> bool {
    read_session_context(profile)
        .and_then(|context| context.get(HOST_WARMUP_CONTEXT))
        .is_some_and(|value| value == "1")
}

/// Which omo-spawned role this session serves, or `None` for an ordinary interactive session. An
/// unrecognized future role reads as the plain `child`: it is still omo-spawned work.
pub fn read_session_role(
    profile: Option<&ExtensionSessionProfile>,
    env: &telemetry_core::TelemetryEnv,
) -> Option<&'static str> {
    if let Some(role) = read_session_context(profile)
        .and_then(|context| context.get(SESSION_CONTEXT_ROLE))
        .filter(|role| !role.is_empty())
    {
        return Some(match role.as_str() {
            "dag_child" => "dag_child",
            "member" => "member",
            _ => "child",
        });
    }
    if is_team_member_process(env) {
        return Some("member");
    }
    (env.get(OMO_SENPI_TASK_RPC_CHILD).map(String::as_str) == Some("1")).then_some("child")
}

/// The delegating turn's traceparent this omo-spawned session inherited. An ordinary session never
/// reads the variable, so an ambient value cannot parent it.
pub fn read_session_traceparent(
    profile: Option<&ExtensionSessionProfile>,
    env: &telemetry_core::TelemetryEnv,
) -> Option<String> {
    if let Some(context) = read_session_context(profile)
        && context
            .get(SESSION_CONTEXT_ROLE)
            .is_some_and(|role| !role.is_empty())
    {
        return context
            .get(SESSION_CONTEXT_TRACEPARENT)
            .filter(|value| !value.is_empty())
            .cloned();
    }
    if !is_team_member_process(env) && env.get(OMO_SENPI_TASK_RPC_CHILD).map(String::as_str) != Some("1")
    {
        return None;
    }
    env.get(OMO_SENPI_TASK_TRACEPARENT)
        .filter(|value| !value.is_empty())
        .cloned()
}

/// The session's place in the task tree, collapsed to the closed wire vocabulary: an ordinary session is
/// `root`, every omo-spawned session is `child`, and a host whose session context cannot be read is
/// `unknown` rather than a guess. `None` stands for the unreadable host context the TS reader catches.
pub fn resolve_telemetry_session_role(
    profile: Option<&ExtensionSessionProfile>,
    env: &telemetry_core::TelemetryEnv,
) -> TelemetrySessionRole {
    match read_session_role(profile, env) {
        None => TelemetrySessionRole::Root,
        Some(_) => TelemetrySessionRole::Child,
    }
}

/// The same read, for a host that may not report a session context at all.
pub fn resolve_telemetry_session_role_checked(
    profile: Option<&ExtensionSessionProfile>,
    env: &telemetry_core::TelemetryEnv,
) -> TelemetrySessionRole {
    if profile.is_none() {
        return TelemetrySessionRole::Unknown;
    }
    resolve_telemetry_session_role(profile, env)
}

/// A host whose session context cannot be read is not a warm-up session: it reports, as `unknown`.
pub fn is_telemetry_host_warmup(profile: Option<&ExtensionSessionProfile>) -> bool {
    profile.is_some() && is_host_warmup_session(profile)
}

/// The engine's own role name (the span attribute vocabulary), separate from the closed event
/// vocabulary: `main` for an ordinary session.
pub fn session_role_name(
    profile: Option<&ExtensionSessionProfile>,
    env: &telemetry_core::TelemetryEnv,
) -> &'static str {
    read_session_role(profile, env).unwrap_or("main")
}

fn is_team_member_process(env: &telemetry_core::TelemetryEnv) -> bool {
    env.get(OMO_SENPI_TASK_MEMBER)
        .is_some_and(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use maho_ext_api::ExtensionSessionProfile;

    fn profile(context: &[(&str, &str)]) -> ExtensionSessionProfile {
        ExtensionSessionProfile {
            session_context: context
                .iter()
                .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
                .collect(),
            ..Default::default()
        }
    }

    fn env(pairs: &[(&str, &str)]) -> telemetry_core::TelemetryEnv {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect()
    }

    #[test]
    fn ordinary_session_is_root() {
        let empty = ExtensionSessionProfile::default();
        assert_eq!(
            resolve_telemetry_session_role(Some(&empty), &env(&[])),
            TelemetrySessionRole::Root
        );
    }

    #[test]
    fn child_and_unknown_host_context() {
        assert_eq!(
            resolve_telemetry_session_role(Some(&profile(&[("role", "child")])), &env(&[])),
            TelemetrySessionRole::Child
        );
        assert_eq!(
            resolve_telemetry_session_role(Some(&profile(&[("role", "dag_child")])), &env(&[])),
            TelemetrySessionRole::Child
        );
        assert_eq!(
            resolve_telemetry_session_role(Some(&profile(&[("role", "member")])), &env(&[])),
            TelemetrySessionRole::Child
        );
        assert_eq!(
            resolve_telemetry_session_role(Some(&profile(&[("role", "future_role")])), &env(&[])),
            TelemetrySessionRole::Child
        );
        assert_eq!(
            resolve_telemetry_session_role_checked(None, &env(&[])),
            TelemetrySessionRole::Unknown
        );
        assert_eq!(
            resolve_telemetry_session_role_checked(None, &env(&[("role", "child")])),
            TelemetrySessionRole::Unknown
        );
    }

    #[test]
    fn env_fallback_names_the_role() {
        assert_eq!(
            resolve_telemetry_session_role(none_or_empty(), &env(&[("OMO_SENPI_TASK_RPC_CHILD", "1")])),
            TelemetrySessionRole::Child
        );
        assert_eq!(
            resolve_telemetry_session_role(none_or_empty(), &env(&[("SENPI_TASK_MEMBER", "team-a")])),
            TelemetrySessionRole::Child
        );
        assert_eq!(
            resolve_telemetry_session_role(none_or_empty(), &env(&[])),
            TelemetrySessionRole::Root
        );
    }

    fn none_or_empty() -> Option<&'static ExtensionSessionProfile> {
        None
    }

    #[test]
    fn warmup_sessions_are_excluded_and_unreadable_hosts_are_not_warmups() {
        assert!(is_telemetry_host_warmup(Some(&profile(&[(
            "host_warmup", "1"
        )]))));
        assert!(!is_telemetry_host_warmup(Some(&profile(&[(
            "host_warmup", "0"
        )]))));
        assert!(!is_telemetry_host_warmup(Some(&profile(&[("role", "child")]))));
        assert!(!is_telemetry_host_warmup(None));
        assert!(!is_host_warmup_session(None));
    }

    #[test]
    fn inherited_traceparent_requires_an_omo_spawned_session() {
        let spawned = profile(&[("role", "child"), ("traceparent", "00-abc-def-01")]);
        assert_eq!(
            read_session_traceparent(Some(&spawned), &env(&[])).as_deref(),
            Some("00-abc-def-01")
        );
        let ambient = profile(&[]);
        assert_eq!(
            read_session_traceparent(
                Some(&ambient),
                &env(&[("OMO_SENPI_TASK_TRACEPARENT", "00-abc-def-01")])
            ),
            None
        );
        assert_eq!(
            read_session_traceparent(
                None,
                &env(&[
                    ("OMO_SENPI_TASK_RPC_CHILD", "1"),
                    ("OMO_SENPI_TASK_TRACEPARENT", "00-abc-def-01"),
                ])
            )
            .as_deref(),
            Some("00-abc-def-01")
        );
    }

    #[test]
    fn engine_role_name_defaults_to_main() {
        assert_eq!(session_role_name(Some(&profile(&[])), &env(&[])), "main");
        assert_eq!(
            session_role_name(Some(&profile(&[("role", "member")])), &env(&[])),
            "member"
        );
        assert_eq!(
            session_role_name(Some(&profile(&[("role", "dag_child")])), &env(&[])),
            "dag_child"
        );
    }
}
