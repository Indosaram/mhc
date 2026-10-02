use maho_core::engine_build_identity::EngineBuildIdentity;
use serde::Serialize;
pub use crate::host_protocol_info::{HostProtocolInfo, RpcLaunchProfile, parse_host_protocol_info};

pub const HOST_PROTOCOL_VERSION: u32 = 1;
pub const GENERATION_HANDOFF_CAPABILITY: &str = "generation_handoff";
pub const REQUIRED_HOST_CAPABILITIES: [&str; 4] = ["multi_session", "extension_events", "session_context", "session_kind"];

pub struct HostDecisionClient {
    pub protocol_version: u32,
    pub required_capabilities: Vec<String>,
    pub identity: EngineBuildIdentity,
    pub launch_profile: Option<RpcLaunchProfile>,
    pub started_by_us: bool,
    pub platform: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostDecisionPolicy { Upgrade, Fallback, Never }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostDecisionWarning { ProfileNarrowerAttached, ProfileMismatchAttached }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "action", rename_all = "lowercase")]
pub enum HostDecision {
    Start { reason: StartReason, upgradeable: bool },
    Refuse { reason: RefuseReason, upgradeable: bool },
    Fallback { reason: FallbackReason, upgradeable: bool },
    Reuse { reason: ReuseReason, upgradeable: bool, #[serde(skip_serializing_if = "Option::is_none")] warning: Option<HostDecisionWarning> },
    Handoff { reason: HandoffReason, upgradeable: bool },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StartReason { NoHost, RestartOwnHost }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RefuseReason { Protocol, Capability }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FallbackReason { Capability, EngineMismatch }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReuseReason { Compatible, HandoffUnsupported, Win32AttachOnly }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HandoffReason { NewerEngine, Profile }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostRefusalReason { Protocol, Capability, ForeignWriter, LegacyHost, HostBusy }
impl HostRefusalReason {
    pub const fn as_str(self) -> &'static str {
        match self { Self::Protocol => "protocol", Self::Capability => "capability", Self::ForeignWriter => "foreign_writer", Self::LegacyHost => "legacy_host", Self::HostBusy => "host_busy" }
    }
}
#[derive(Debug, thiserror::Error)]
#[error("RPC socket {socket} refused: {} - {detail}", reason.as_str())]
pub struct HostEnsureRefusedError {
    pub socket: String,
    pub reason: HostRefusalReason,
    detail: String,
}
impl HostEnsureRefusedError {
    pub fn new(socket: &str, reason: HostRefusalReason, host: Option<&HostProtocolInfo>) -> Self {
        let detail = match reason {
            HostRefusalReason::Protocol => format!("the running host speaks protocol version {}, this build speaks {HOST_PROTOCOL_VERSION}",host.map_or_else(|| "unknown".into(),|host| host.protocol_version.to_string())),
            HostRefusalReason::Capability => {
                let missing = REQUIRED_HOST_CAPABILITIES.iter().filter(|capability| !host.is_some_and(|host| host.capabilities.iter().any(|value| value == **capability))).map(|value| serde_json::Value::String((*value).into())).collect::<Vec<_>>();
                format!("the running host is missing {}; it owns the socket, so no second host is started",serde_json::Value::Array(missing))
            }
            HostRefusalReason::ForeignWriter => "its pidfile was written by another process, so this one may not signal it; stop that host explicitly instead".into(),
            HostRefusalReason::LegacyHost => "a host from before the per-socket daemon directory is still running on this endpoint; it is never signalled, and no second host is started beside it".into(),
            HostRefusalReason::HostBusy => "its socket accepts connections but did not answer inside the probe budget: a live host under load, which is never ended to make room for a replacement".into(),
        };
        Self { socket: socket.into(), reason, detail }
    }
}

fn covers(candidate: Option<&RpcLaunchProfile>, running: Option<&RpcLaunchProfile>) -> bool {
    match (candidate, running) {
        (Some(candidate), Some(running)) => running.core.extensions.iter().all(|extension| candidate.core.extensions.contains(extension)),
        _ => false,
    }
}
fn profile_warning(client: &HostDecisionClient, host: &HostProtocolInfo) -> Option<HostDecisionWarning> {
    let (Some(candidate), Some(running)) = (&client.launch_profile, &host.launch_profile) else { return Some(HostDecisionWarning::ProfileMismatchAttached); };
    if candidate.profile_id == running.profile_id { return None; }
    Some(if covers(Some(candidate), Some(running)) { HostDecisionWarning::ProfileMismatchAttached } else { HostDecisionWarning::ProfileNarrowerAttached })
}
fn newer(client: &HostDecisionClient, host: &HostProtocolInfo) -> bool {
    let Some(ordinal) = host.engine_ordinal else { return false; };
    for (candidate, running) in client.identity.ordinal.iter().take(4).zip(ordinal.iter()) {
        let candidate = serde_json::Number::from(*candidate).as_f64().unwrap_or(0.0);
        if candidate > *running { return true; }
        if candidate < *running { return false; }
    }
    let epoch = serde_json::Number::from(client.identity.ordinal[4]).as_f64().unwrap_or(0.0);
    epoch > 0.0 && ordinal[4] > 0.0 && epoch > ordinal[4]
}
fn same_release(client: &HostDecisionClient, host: &HostProtocolInfo) -> bool {
    host.engine_ordinal.is_some_and(|ordinal| client.identity.ordinal.iter().take(4).zip(ordinal.iter()).all(|(a,b)| serde_json::Number::from(*a).as_f64().is_some_and(|a| a.total_cmp(b).is_eq())))
}
pub fn decide_host_action(client: &HostDecisionClient, host: Option<&HostProtocolInfo>, policy: HostDecisionPolicy) -> HostDecision {
    let Some(host) = host else { return HostDecision::Start { reason: if client.started_by_us { StartReason::RestartOwnHost } else { StartReason::NoHost }, upgradeable: false }; };
    if host.protocol_version.total_cmp(&f64::from(client.protocol_version)).is_ne() { return HostDecision::Refuse { reason: RefuseReason::Protocol, upgradeable: false }; }
    if !client.required_capabilities.iter().all(|capability| host.capabilities.contains(capability)) {
        return if policy == HostDecisionPolicy::Fallback { HostDecision::Fallback { reason: FallbackReason::Capability, upgradeable: false } } else { HostDecision::Refuse { reason: RefuseReason::Capability, upgradeable: false } };
    }
    let upgradeable = client.platform != "win32" && host.capabilities.iter().any(|capability| capability == GENERATION_HANDOFF_CAPABILITY);
    if policy == HostDecisionPolicy::Fallback && host.engine_version.as_deref() != Some(&client.identity.text) { return HostDecision::Fallback { reason: FallbackReason::EngineMismatch, upgradeable }; }
    let warning = profile_warning(client, host);
    if client.platform == "win32" { return HostDecision::Reuse { reason: ReuseReason::Win32AttachOnly, upgradeable: false, warning }; }
    if !upgradeable { return HostDecision::Reuse { reason: ReuseReason::HandoffUnsupported, upgradeable: false, warning }; }
    if policy == HostDecisionPolicy::Upgrade && newer(client, host) && covers(client.launch_profile.as_ref(), host.launch_profile.as_ref()) {
        return HostDecision::Handoff { reason: if same_release(client, host) { HandoffReason::Profile } else { HandoffReason::NewerEngine }, upgradeable: true };
    }
    HostDecision::Reuse { reason: ReuseReason::Compatible, upgradeable, warning }
}

#[cfg(test)]
mod tests {
    use super::*;
    use maho_core::engine_build_identity::{EngineBuildInput, engine_build_identity_from};
    use crate::host_protocol_info::{RpcLaunchProfileCore, SessionRuntimeKind};
    fn profile(extensions: &[&str]) -> RpcLaunchProfile { RpcLaunchProfile { profile_id: extensions.join(","), core: RpcLaunchProfileCore { extensions: extensions.iter().map(|v| (*v).into()).collect(), multi_session: true, session_runtime: SessionRuntimeKind::InProcess } } }
    fn client() -> HostDecisionClient { HostDecisionClient { protocol_version: 1, required_capabilities: REQUIRED_HOST_CAPABILITIES.iter().map(|v| (*v).into()).collect(), identity: engine_build_identity_from(&EngineBuildInput { version: "2026.9.17".into(), ..Default::default() }), launch_profile: Some(profile(&["/plugin"])), started_by_us: false, platform: "linux".into() } }
    fn host() -> HostProtocolInfo { HostProtocolInfo { protocol_version: 1.0, server_version: "deliberately different".into(), capabilities: REQUIRED_HOST_CAPABILITIES.iter().chain([GENERATION_HANDOFF_CAPABILITY].iter()).map(|v| (*v).into()).collect(), instance_id: None, generation: None, engine_version: Some("2026.9.16-3".into()), engine_ordinal: Some([2026.0,9.0,16.0,3.0,0.0]), launch_profile: Some(profile(&["/plugin"])) } }
    #[test] fn no_host_starts() { assert!(matches!(decide_host_action(&client(),None,HostDecisionPolicy::Never),HostDecision::Start { reason:StartReason::NoHost,.. })); }
    #[test] fn own_dead_host_starts_with_own_reason() { let mut c=client();c.started_by_us=true;assert!(matches!(decide_host_action(&c,None,HostDecisionPolicy::Never),HostDecision::Start { reason:StartReason::RestartOwnHost,.. })); }
    #[test] fn protocol_mismatch_refuses() { let mut h=host();h.protocol_version=2.0;assert!(matches!(decide_host_action(&client(),Some(&h),HostDecisionPolicy::Upgrade),HostDecision::Refuse { reason:RefuseReason::Protocol,.. })); }
    #[test] fn missing_capability_never_refuses() { let mut h=host();h.capabilities.clear();assert!(matches!(decide_host_action(&client(),Some(&h),HostDecisionPolicy::Never),HostDecision::Refuse { reason:RefuseReason::Capability,.. })); }
    #[test] fn missing_capability_falls_back() { let mut h=host();h.capabilities.clear();assert!(matches!(decide_host_action(&client(),Some(&h),HostDecisionPolicy::Fallback),HostDecision::Fallback { reason:FallbackReason::Capability,.. })); }
    #[test] fn missing_capability_upgrade_refuses() { let mut h=host();h.capabilities.clear();assert!(matches!(decide_host_action(&client(),Some(&h),HostDecisionPolicy::Upgrade),HostDecision::Refuse { reason:RefuseReason::Capability,.. })); }
    #[test] fn host_without_handoff_reuses() { let mut h=host();h.capabilities.retain(|v| v!=GENERATION_HANDOFF_CAPABILITY);assert!(matches!(decide_host_action(&client(),Some(&h),HostDecisionPolicy::Upgrade),HostDecision::Reuse { reason:ReuseReason::HandoffUnsupported,upgradeable:false,.. })); }
    #[test] fn newer_engine_hands_off() { assert!(matches!(decide_host_action(&client(),Some(&host()),HostDecisionPolicy::Upgrade),HostDecision::Handoff { reason:HandoffReason::NewerEngine,.. })); }
    #[test] fn same_release_newer_epoch_hands_off_profile() { let mut c=client();c.identity=engine_build_identity_from(&EngineBuildInput { version:"2026.9.16-3".into(),epoch:Some(200),sha7:None });let mut h=host();h.engine_ordinal=Some([2026.0,9.0,16.0,3.0,100.0]);assert!(matches!(decide_host_action(&c,Some(&h),HostDecisionPolicy::Upgrade),HostDecision::Handoff { reason:HandoffReason::Profile,.. })); }
    #[test] fn narrower_profile_never_hands_off() { let mut h=host();h.launch_profile=Some(profile(&["/plugin","/members"]));assert!(matches!(decide_host_action(&client(),Some(&h),HostDecisionPolicy::Upgrade),HostDecision::Reuse { warning:Some(HostDecisionWarning::ProfileNarrowerAttached),.. })); }
    #[test] fn absent_client_profile_reuses_with_warning() { let mut c=client();c.launch_profile=None;assert!(matches!(decide_host_action(&c,Some(&host()),HostDecisionPolicy::Upgrade),HostDecision::Reuse { warning:Some(HostDecisionWarning::ProfileMismatchAttached),.. })); }
    #[test] fn older_wider_client_reuses_with_warning() { let mut c=client();c.identity.ordinal[2]=15;c.launch_profile=Some(profile(&["/plugin","/members"]));assert!(matches!(decide_host_action(&c,Some(&host()),HostDecisionPolicy::Upgrade),HostDecision::Reuse { warning:Some(HostDecisionWarning::ProfileMismatchAttached),.. })); }
    #[test] fn equal_engine_reuses_without_warning() { let mut c=client();c.identity.ordinal=[2026,9,16,3,0];assert!(matches!(decide_host_action(&c,Some(&host()),HostDecisionPolicy::Upgrade),HostDecision::Reuse { warning:None,.. })); }
    #[test] fn older_engine_reuses() { let mut c=client();c.identity.ordinal=[2026,9,16,0,0];assert!(matches!(decide_host_action(&c,Some(&host()),HostDecisionPolicy::Upgrade),HostDecision::Reuse {..})); }
    #[test] fn never_policy_gates_newer_engine() { assert!(matches!(decide_host_action(&client(),Some(&host()),HostDecisionPolicy::Never),HostDecision::Reuse {..})); }
    #[test] fn fallback_rejects_different_build() { assert!(matches!(decide_host_action(&client(),Some(&host()),HostDecisionPolicy::Fallback),HostDecision::Fallback { reason:FallbackReason::EngineMismatch,.. })); }
    #[test] fn fallback_reuses_same_build() { let mut c=client();c.identity.text="2026.9.16-3".into();assert!(matches!(decide_host_action(&c,Some(&host()),HostDecisionPolicy::Fallback),HostDecision::Reuse {..})); }
    #[test] fn windows_attaches_only() { let mut c=client();c.platform="win32".into();assert!(matches!(decide_host_action(&c,Some(&host()),HostDecisionPolicy::Upgrade),HostDecision::Reuse { reason:ReuseReason::Win32AttachOnly,upgradeable:false,.. })); }
    #[test] fn absent_host_ordinal_never_wins_upgrade() { let mut h=host();h.engine_ordinal=None;assert!(matches!(decide_host_action(&client(),Some(&h),HostDecisionPolicy::Upgrade),HostDecision::Reuse {..})); }
    #[test] fn one_unknown_epoch_reuses() { let mut c=client();c.identity.ordinal=[2026,9,16,3,200];assert!(matches!(decide_host_action(&c,Some(&host()),HostDecisionPolicy::Upgrade),HostDecision::Reuse {..})); }
    #[test] fn required_capabilities_are_exact() { assert_eq!(REQUIRED_HOST_CAPABILITIES,["multi_session","extension_events","session_context","session_kind"]); }
}
