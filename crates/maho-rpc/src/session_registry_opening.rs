//! senpi `modes/rpc/session-registry-opening.ts`: the "opening" entry an open records once its path is reserved
//! and claimed. Built in one synchronous step so the registry can give the reservation back if building it fails,
//! instead of leaving a reserved path no entry backs (senpi#3485).
//!
//! NATIVE-DIVERGENCE(senpi `entry.switchSession = createEntrySwitchSession(...)`): a native entry routes
//! `switch_session` through the session's own replacement (see `session_registry_switch.rs`), so no switch closure
//! is installed here.
use crate::session_registry::{
    RpcSessionEntry, RpcSessionLaunchProfile, RpcSessionState, SessionCloseState, frozen_profile,
    session_identity,
};

/// The settle half of an entry's `openCompletion`: `true` once it opened, `false` when the open failed.
pub type SettleOpen = tokio::sync::watch::Sender<Option<bool>>;
/// What a concurrent explicit open for the same path waits on to attach (senpi `openCompletion`).
pub type OpenCompletion = tokio::sync::watch::Receiver<Option<bool>>;

pub struct OpeningEntry {
    pub entry: RpcSessionEntry,
    pub stored_profile: RpcSessionLaunchProfile,
    /// What the runtime is built from: a resume drops the create-only model and thinking level.
    pub runtime_profile: RpcSessionLaunchProfile,
    pub session_start_event: Option<maho_ext_api::SessionStartEvent>,
    pub settle_open: SettleOpen,
    pub open_completion: OpenCompletion,
}

pub struct OpeningInput<'a> {
    pub profile: &'a RpcSessionLaunchProfile,
    pub session_path: Option<String>,
    pub requested_durable_id: Option<String>,
    pub retain_on_disconnect: bool,
    pub now: f64,
}

/// senpi `buildOpeningEntry`.
pub fn build_opening_entry(input: OpeningInput<'_>) -> OpeningEntry {
    let OpeningInput {
        profile,
        session_path,
        requested_durable_id,
        retain_on_disconnect,
        now,
    } = input;
    // Resume vs create parity: create-only launch semantics mirror classic startup flags. A resumed session
    // restores its persisted model and thinking level instead of being overridden by the new open_session request.
    let is_resume = session_path
        .as_deref()
        .is_some_and(|path| std::path::Path::new(path).exists());
    // Re-opening an existing session file is a resume, exactly like interactive /resume. Without the event the
    // session starts with reason "startup" and every extension that only rebuilds state on a resume stays
    // unreachable from the RPC restart path. A session created by this open stays "startup".
    let unattended = profile.attended == Some(false);
    let session_start_event = (is_resume || unattended).then(|| maho_ext_api::SessionStartEvent {
        sizes_only: false,
        reason: if is_resume {
            maho_ext_api::SessionReason::Resume
        } else {
            maho_ext_api::SessionReason::Startup
        },
        initial_model_provenance: None,
        previous_session_file: None,
        attended: unattended.then_some(false),
    });
    let mut stored_profile = frozen_profile(profile);
    if session_path.is_some() {
        stored_profile.session_path = session_path.clone();
    }
    let mut runtime_profile = frozen_profile(&stored_profile);
    if is_resume {
        runtime_profile.runtime.creation_model = None;
        runtime_profile.runtime.initial_thinking_level = None;
    }
    let (settle_open, open_completion) = tokio::sync::watch::channel(None);
    let entry = RpcSessionEntry {
        close: SessionCloseState {
            state: RpcSessionState::Opening,
            attachments: 1,
            retain_on_disconnect,
            reservation_key: session_path.clone(),
            writing: false,
        },
        identity: session_identity(&stored_profile),
        profile: stored_profile.clone(),
        scope: maho_ai::node::provider_scope::ProviderScope::new(),
        runtime: None,
        // Recorded before the first await so the synchronous collision guard sees an open that is still being
        // built; the runtime's manager overwrites it later with the authoritative value, which on a resume is
        // the header's id, not the requested one.
        durable_session_id: requested_durable_id,
        session_path,
        last_command_at: now,
        context_overlay: None,
        acceptance: None,
    };
    OpeningEntry {
        entry,
        stored_profile,
        runtime_profile,
        session_start_event,
        settle_open,
        open_completion,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile() -> RpcSessionLaunchProfile {
        let mut profile = RpcSessionLaunchProfile::default();
        profile.runtime.cwd = "/cwd".into();
        profile.runtime.creation_model = Some(("p".into(), "m".into()));
        profile.runtime.initial_thinking_level = Some("high".into());
        profile
    }

    #[test]
    fn a_fresh_path_opens_as_startup_with_its_create_only_choices() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("new.jsonl").to_string_lossy().into_owned();
        let opening = build_opening_entry(OpeningInput {
            profile: &profile(),
            session_path: Some(path.clone()),
            requested_durable_id: Some("durable".into()),
            retain_on_disconnect: true,
            now: 7.,
        });
        assert!(opening.session_start_event.is_none());
        assert_eq!(
            opening.runtime_profile.runtime.creation_model,
            Some(("p".into(), "m".into()))
        );
        assert_eq!(
            opening.stored_profile.session_path.as_deref(),
            Some(path.as_str())
        );
        let entry = &opening.entry;
        assert_eq!(entry.close.state, RpcSessionState::Opening);
        assert_eq!(entry.close.attachments, 1);
        assert!(entry.close.retain_on_disconnect);
        assert_eq!(entry.close.reservation_key.as_deref(), Some(path.as_str()));
        assert_eq!(entry.durable_session_id.as_deref(), Some("durable"));
        assert_eq!(entry.last_command_at, 7.);
        assert_eq!(*opening.open_completion.borrow(), None);
    }

    #[test]
    fn an_existing_file_resumes_and_drops_the_create_only_choices() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old.jsonl");
        std::fs::write(&path, "").unwrap();
        let opening = build_opening_entry(OpeningInput {
            profile: &profile(),
            session_path: Some(path.to_string_lossy().into_owned()),
            requested_durable_id: None,
            retain_on_disconnect: false,
            now: 0.,
        });
        let event = opening.session_start_event.expect("a resume event");
        assert_eq!(event.reason, maho_ext_api::SessionReason::Resume);
        assert_eq!(event.attended, None);
        assert_eq!(opening.runtime_profile.runtime.creation_model, None);
        assert_eq!(opening.runtime_profile.runtime.initial_thinking_level, None);
        // The stored profile keeps what the client asked for.
        assert_eq!(
            opening
                .stored_profile
                .runtime
                .initial_thinking_level
                .as_deref(),
            Some("high")
        );
    }

    #[test]
    fn an_unattended_create_starts_with_attended_false() {
        let mut unattended = profile();
        unattended.attended = Some(false);
        let opening = build_opening_entry(OpeningInput {
            profile: &unattended,
            session_path: None,
            requested_durable_id: None,
            retain_on_disconnect: false,
            now: 0.,
        });
        let event = opening.session_start_event.expect("an unattended start");
        assert_eq!(event.reason, maho_ext_api::SessionReason::Startup);
        assert_eq!(event.attended, Some(false));
    }

    #[test]
    fn settling_the_open_wakes_a_waiter() {
        let opening = build_opening_entry(OpeningInput {
            profile: &profile(),
            session_path: None,
            requested_durable_id: None,
            retain_on_disconnect: false,
            now: 0.,
        });
        let mut waiter = opening.open_completion.clone();
        opening.settle_open.send_replace(Some(true));
        assert!(waiter.has_changed().unwrap());
        assert_eq!(*waiter.borrow_and_update(), Some(true));
    }
}
