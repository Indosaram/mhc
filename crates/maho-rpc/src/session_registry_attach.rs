//! senpi `modes/rpc/session-registry-attach.ts`: attach-on-open. A live session outlives individual client
//! attachments, so a resume (or a second surface) for an already-hosted path joins the existing runtime instead of
//! failing. An entry still opening holds the reservation, and an explicit open for that path waits on the entry's
//! open completion (bounded on the waiter only, default 45s) and attaches once it opens; on that wait expiring, only
//! the one waiting reopen fails with a retryable `session_path_in_use { reason: "open_in_progress",
//! retry_after_ms }` naming an attached current owner, and the in-flight open keeps running untouched.
use crate::host_reservations::{BoundPathReservations, SessionPathOwner};
use crate::session_registry::{
    RpcSessionEntry, RpcSessionLaunchProfile, RpcSessionRegistryError, RpcSessionState,
};
use crate::session_registry_opening::OpenCompletion;

/// How long ONE explicit reopen waits on an in-flight open before giving up (the open keeps going).
const DEFAULT_OPEN_IN_PROGRESS_WAIT_MS: u64 = 45_000;
/// How long the caller is told to wait before retrying after an `open_in_progress` rejection.
const OPEN_IN_PROGRESS_WAIT_RETRY_MS: u64 = 2_000;

/// The wait is read at call time so a test can shorten it via `<BRAND>_OPEN_IN_PROGRESS_WAIT_MS`.
fn open_in_progress_wait_ms() -> u64 {
    let env: std::collections::HashMap<String, String> = std::env::vars().collect();
    maho_core::brand::env_value("OPEN_IN_PROGRESS_WAIT_MS", &env)
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value > 0.)
        .map_or(DEFAULT_OPEN_IN_PROGRESS_WAIT_MS, |value| value as u64)
}

/// The owner a still-opening path's rejection names: the current host, attached (the session is being served, not
/// stranded). This is a signal, not a protection: what keeps the opening session alive is that this host refuses
/// abort and close on an "opening" entry.
fn opening_owner(session_path: &str) -> SessionPathOwner {
    SessionPathOwner {
        instance_id: crate::protocol_identity::host_instance_id().to_owned(),
        host_socket: None,
        pid: std::process::id(),
        process_start_time: None,
        session_path: session_path.to_owned(),
        attached: Some(true),
        current: Some(true),
    }
}

/// The entries an attach looks at, by routing handle, with each one's open completion while it is still opening.
pub struct AttachCandidate<'a> {
    pub handle: &'a str,
    pub entry: &'a mut RpcSessionEntry,
    pub open_completion: Option<OpenCompletion>,
}

/// What the attach leaves for the caller to do once the entry lock is released.
#[derive(Debug, PartialEq)]
pub enum AttachDecision {
    /// Joined the open entry; `resumed` when the attach ended a retained session's parked window, so the caller
    /// emits `session_resumed` through the entry's lifecycle owner.
    Attached {
        session_id: String,
        durable_session_id: String,
        session_path: Option<String>,
        resumed: bool,
    },
    /// The path is still opening: wait on this completion (with `wait_open_completion`), then attach again.
    Wait(OpenCompletionHandle),
}

/// The in-flight open a reopen waits on.
#[derive(Debug)]
pub struct OpenCompletionHandle(pub OpenCompletion);
impl PartialEq for OpenCompletionHandle {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

/// senpi `attachToOpenSession`, the synchronous half: joins the open entry holding `session_path`, or hands back
/// the in-flight open to wait on, or refuses.
pub fn attach_to_open_session(
    entries: Vec<AttachCandidate<'_>>,
    session_path: &str,
    profile: &RpcSessionLaunchProfile,
    retain_on_disconnect: bool,
    path_reservations: Option<&mut BoundPathReservations>,
    now: f64,
) -> Result<AttachDecision, RpcSessionRegistryError> {
    let mut opening = None;
    let mut open = None;
    for candidate in entries {
        if candidate.entry.close.reservation_key.as_deref() != Some(session_path) {
            continue;
        }
        match candidate.entry.close.state {
            RpcSessionState::Open if open.is_none() => open = Some(candidate),
            RpcSessionState::Opening if opening.is_none() => opening = Some(candidate),
            _ => {}
        }
    }
    let Some(AttachCandidate { handle, entry, .. }) = open else {
        // An in-flight open for the same path holds the reservation: an explicit reopen WAITS for it to finish and
        // attaches, rather than failing on a healthy in-flight open (senpi#3282).
        if let Some(completion) = opening.and_then(|candidate| candidate.open_completion) {
            return Ok(AttachDecision::Wait(OpenCompletionHandle(completion)));
        }
        // The path is still reserved by a session that is not serving (a teardown past its grace window): never
        // a bare refusal, so the caller knows to retry (senpi#3485).
        return Err(RpcSessionRegistryError::new(
            "session_path_in_use",
            Some("the session is still being released"),
            Some(serde_json::json!({
                "reason": "path_still_held",
                "retry_after_ms": OPEN_IN_PROGRESS_WAIT_RETRY_MS,
            })),
        ));
    };
    let was_parked = entry.close.retain_on_disconnect && entry.close.attachments == 0;
    entry.close.attachments += 1;
    // The claim carries the attachment state another generation decides on: a path this host is actively serving a
    // client on is never reclaimable from it.
    if let Some(claims) = path_reservations {
        claims.set_attached(session_path, true);
    }
    // Retention is a property of the live session: any attach may ask for it, and no attach may revoke it for the
    // clients that already rely on it.
    if retain_on_disconnect {
        entry.close.retain_on_disconnect = true;
    }
    let Some(durable_session_id) = entry.durable_session_id.clone() else {
        return Err(RpcSessionRegistryError::new(
            "session_path_in_use",
            None,
            None,
        ));
    };
    // The surface, browser engine and permission preset follow the client: an attach that names one moves the
    // live session to it; an attach without one keeps what the session has.
    if let Some(surface) = profile
        .prompt_surface
        .as_ref()
        .filter(|surface| entry.profile.prompt_surface.as_ref() != Some(*surface))
    {
        entry.profile.prompt_surface = Some(surface.clone());
        if let Some(runtime) = &entry.runtime
            && let Some(surface) = prompt_surface(surface)
        {
            runtime.session().set_prompt_surface(surface);
        }
    }
    if let Some(engine) = profile
        .browser_engine
        .as_ref()
        .filter(|engine| entry.profile.browser_engine.as_ref() != Some(*engine))
    {
        entry.profile.browser_engine = Some(engine.clone());
        if let Some(runtime) = &entry.runtime {
            runtime.session().set_browser_engine(engine.clone());
        }
    }
    // The preset is recorded even when the record already names it (senpi#2842).
    // NATIVE-DIVERGENCE(senpi `entry.runtime?.setPermissionPreset`): maho-core exposes no live preset seam, so
    // only the record moves.
    if let Some(preset) = &profile.runtime.permission_preset {
        entry.profile.runtime.permission_preset = Some(preset.clone());
    }
    entry.last_command_at = now;
    Ok(AttachDecision::Attached {
        session_id: handle.to_owned(),
        durable_session_id,
        session_path: entry.session_path.clone(),
        resumed: was_parked,
    })
}

fn prompt_surface(surface: &str) -> Option<maho_ext_api::PromptSurface> {
    match surface {
        "terminal" => Some(maho_ext_api::PromptSurface::Terminal),
        "app" => Some(maho_ext_api::PromptSurface::App),
        "chat" => Some(maho_ext_api::PromptSurface::Chat),
        _ => None,
    }
}

/// senpi `attachToOpenSession`'s wait: the deadline is on THIS waiter only, never on the open, so a slow-but-healthy
/// open still completes and a later retry attaches to it. `Ok(())` when the in-flight open opened (attach again).
pub async fn wait_open_completion(
    completion: OpenCompletionHandle,
    session_path: &str,
) -> Result<(), RpcSessionRegistryError> {
    let mut completion = completion.0;
    let wait = completion.wait_for(Option::is_some);
    let settled = tokio::time::timeout(
        std::time::Duration::from_millis(open_in_progress_wait_ms()),
        wait,
    )
    .await;
    match settled {
        Err(_) => Err(RpcSessionRegistryError::new(
            "session_path_in_use",
            Some("the session is still opening"),
            Some(serde_json::json!({
                "reason": "open_in_progress",
                "retry_after_ms": OPEN_IN_PROGRESS_WAIT_RETRY_MS,
                // The rejection names the current host as an attached owner. A client that reclaims the path
                // anyway cannot close the opening session: abort and close on an "opening" entry are refused.
                "owner": opening_owner(session_path),
            })),
        )),
        Ok(Ok(opened)) if *opened == Some(true) => Ok(()),
        // The in-flight open FAILED (or vanished): its rollback released the reservation, so the path is free and
        // the caller's retry opens it fresh. This open reports session_path_in_use so the caller retries, not the
        // failed opener's own error.
        Ok(_) => Err(RpcSessionRegistryError::new(
            "session_path_in_use",
            Some("the in-flight open failed"),
            Some(serde_json::json!({
                "reason": "open_failed",
                "retry_after_ms": OPEN_IN_PROGRESS_WAIT_RETRY_MS,
            })),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_registry_opening::{OpeningInput, build_opening_entry};

    fn opening(path: &str) -> crate::session_registry_opening::OpeningEntry {
        let mut profile = RpcSessionLaunchProfile::default();
        profile.runtime.cwd = "/cwd".into();
        build_opening_entry(OpeningInput {
            profile: &profile,
            session_path: Some(path.to_owned()),
            requested_durable_id: None,
            retain_on_disconnect: false,
            now: 0.,
        })
    }

    fn open_entry(path: &str) -> RpcSessionEntry {
        let mut entry = opening(path).entry;
        entry.close.state = RpcSessionState::Open;
        entry.durable_session_id = Some("durable".into());
        entry
    }

    #[test]
    fn an_open_session_is_joined_and_retention_only_ever_turns_on() {
        let mut entry = open_entry("/p");
        let requested = RpcSessionLaunchProfile {
            prompt_surface: Some("app".into()),
            runtime: maho_core::agent_session_runtime::AgentSessionLaunchProfile {
                permission_preset: Some("guarded".into()),
                ..Default::default()
            },
            ..Default::default()
        };
        let decision = attach_to_open_session(
            vec![AttachCandidate {
                handle: "rpc-1",
                entry: &mut entry,
                open_completion: None,
            }],
            "/p",
            &requested,
            true,
            None,
            9.,
        )
        .unwrap();
        assert_eq!(
            decision,
            AttachDecision::Attached {
                session_id: "rpc-1".into(),
                durable_session_id: "durable".into(),
                session_path: Some("/p".into()),
                resumed: false,
            }
        );
        assert_eq!(entry.close.attachments, 2);
        assert!(entry.close.retain_on_disconnect);
        assert_eq!(entry.last_command_at, 9.);
        assert_eq!(entry.profile.prompt_surface.as_deref(), Some("app"));
        assert_eq!(
            entry.profile.runtime.permission_preset.as_deref(),
            Some("guarded")
        );
        attach_to_open_session(
            vec![AttachCandidate {
                handle: "rpc-1",
                entry: &mut entry,
                open_completion: None,
            }],
            "/p",
            &RpcSessionLaunchProfile::default(),
            false,
            None,
            10.,
        )
        .unwrap();
        assert!(entry.close.retain_on_disconnect);
    }

    #[test]
    fn joining_a_parked_retained_session_reports_the_resume() {
        let mut entry = open_entry("/p");
        entry.close.attachments = 0;
        entry.close.retain_on_disconnect = true;
        let decision = attach_to_open_session(
            vec![AttachCandidate {
                handle: "rpc-1",
                entry: &mut entry,
                open_completion: None,
            }],
            "/p",
            &RpcSessionLaunchProfile::default(),
            false,
            None,
            0.,
        )
        .unwrap();
        assert!(matches!(
            decision,
            AttachDecision::Attached { resumed: true, .. }
        ));
    }

    #[test]
    fn a_path_held_by_no_serving_session_is_a_retryable_refusal() {
        let mut entry = open_entry("/p");
        entry.close.state = RpcSessionState::Closing;
        let refused = attach_to_open_session(
            vec![AttachCandidate {
                handle: "rpc-1",
                entry: &mut entry,
                open_completion: None,
            }],
            "/p",
            &RpcSessionLaunchProfile::default(),
            false,
            None,
            0.,
        )
        .unwrap_err();
        assert_eq!(refused.code, "session_path_in_use");
        assert_eq!(refused.detail.unwrap()["reason"], "path_still_held");
    }

    #[tokio::test]
    async fn a_reopen_waits_for_an_in_flight_open_and_then_attaches() {
        let opening = opening("/p");
        let mut entry = opening.entry;
        let decision = attach_to_open_session(
            vec![AttachCandidate {
                handle: "rpc-1",
                entry: &mut entry,
                open_completion: Some(opening.open_completion.clone()),
            }],
            "/p",
            &RpcSessionLaunchProfile::default(),
            false,
            None,
            0.,
        )
        .unwrap();
        let AttachDecision::Wait(completion) = decision else {
            panic!("an opening path is waited on");
        };
        opening.settle_open.send_replace(Some(true));
        assert_eq!(wait_open_completion(completion, "/p").await, Ok(()));
    }

    // rpc-attach-open-in-progress.test.ts: a never-settling open times the waiter out with a retryable refusal
    // naming an attached current owner, and leaves the entry alone.
    #[tokio::test(start_paused = true)]
    async fn a_never_settling_open_times_the_waiter_out_naming_an_attached_owner() {
        let opening = opening("/p");
        let refused = wait_open_completion(OpenCompletionHandle(opening.open_completion), "/p")
            .await
            .unwrap_err();
        assert_eq!(refused.code, "session_path_in_use");
        let detail = refused.detail.unwrap();
        assert_eq!(detail["reason"], "open_in_progress");
        assert_eq!(detail["retry_after_ms"], OPEN_IN_PROGRESS_WAIT_RETRY_MS);
        assert_eq!(detail["owner"]["attached"], true);
        assert_eq!(detail["owner"]["current"], true);
        assert_eq!(detail["owner"]["pid"], std::process::id());
        assert_eq!(opening.entry.close.state, RpcSessionState::Opening);
    }

    // rpc-attach-open-in-progress.test.ts: a waiter on a FAILED in-flight open gets session_path_in_use again,
    // not the opener's own open_failed.
    #[tokio::test]
    async fn a_waiter_on_a_failed_open_is_told_to_retry() {
        let opening = opening("/p");
        opening.settle_open.send_replace(Some(false));
        let refused = wait_open_completion(OpenCompletionHandle(opening.open_completion), "/p")
            .await
            .unwrap_err();
        assert_eq!(refused.code, "session_path_in_use");
        assert_eq!(refused.detail.unwrap()["reason"], "open_failed");
    }
}
