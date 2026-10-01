//! `team/member-extension/residency.test.ts`

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use pretty_assertions::assert_eq;
use serde_json::json;
use team_core::config::TeamModeConfig;
use team_core::team_mailbox::{SendContext, send_message};
use team_core::types::Message;

use crate::team::member_extension::self_poller::{MemberSelfPollerDeps, create_member_self_poller};

const TEAM_RUN_ID: &str = "11111111-1111-4111-8111-111111111111";
const MESSAGE_ID: &str = "22222222-2222-4222-8222-222222222222";

#[derive(Debug, Default)]
struct FakeSessionState {
    busy: bool,
    last_assistant_text: Option<String>,
    follow_ups: Vec<String>,
    disposed: bool,
}

/// In-memory model of a resident RPC member session mirroring the fake RPC child contract:
/// a prompt completes a turn echoing the prompt, a follow-up revives the session into a
/// working turn, and `steer("complete")` finishes that turn with `steered-complete`.
#[derive(Debug, Default)]
struct FakeResidentSession {
    state: Mutex<FakeSessionState>,
}

impl FakeResidentSession {
    fn start(prompt: &str) -> Arc<Self> {
        let session = Arc::new(Self::default());
        {
            let mut state = session.state.lock().expect("state lock");
            state.busy = false;
            state.last_assistant_text = Some(prompt.to_string());
        }
        session
    }

    fn follow_up(&self, content: &str) {
        let mut state = self.state.lock().expect("state lock");
        state.follow_ups.push(content.to_string());
        state.busy = true;
    }

    fn steer(&self, instruction: &str) {
        let mut state = self.state.lock().expect("state lock");
        if state.busy {
            state.busy = false;
            state.last_assistant_text = Some(format!("steered-{instruction}"));
        }
    }

    /// Synchronous stand-in for `waitForIdle()`: reports whether the current turn completed.
    fn is_idle(&self) -> bool {
        !self.state.lock().expect("state lock").busy
    }

    fn last_assistant_text(&self) -> Option<String> {
        self.state.lock().expect("state lock").last_assistant_text.clone()
    }

    fn follow_ups(&self) -> Vec<String> {
        self.state.lock().expect("state lock").follow_ups.clone()
    }

    fn dispose(&self) {
        self.state.lock().expect("state lock").disposed = true;
    }
}

#[test]
fn given_a_member_whose_initial_turn_ended_without_a_wait_when_lead_mail_is_injected_then_its_resident_rpc_session_revives_into_a_working_turn()
 {
    // given an ended but resident member session
    let root = tempfile::tempdir().expect("tempdir");
    let base_dir = root.path().join("teams");
    let config = TeamModeConfig::parse(&json!({ "base_dir": base_dir.to_string_lossy() }))
        .expect("config parses");
    let session_dir: PathBuf = root.path().join("sessions");

    let handle = FakeResidentSession::start("first");
    assert!(handle.is_idle());
    assert_eq!(handle.last_assistant_text(), Some("first".to_string()));

    let inject_handle = Arc::clone(&handle);
    let poller = create_member_self_poller(MemberSelfPollerDeps {
        team_run_id: TEAM_RUN_ID.to_string(),
        member_name: "alice".to_string(),
        config: config.clone(),
        session_dir,
        inject: Box::new(move |content: &str| inject_handle.follow_up(content)),
        append_event: None,
        after_inject: None,
    });

    let message = Message::safe_parse(&json!({
        "version": 1,
        "messageId": MESSAGE_ID,
        "from": "lead",
        "to": "alice",
        "kind": "message",
        "body": "continue with injected work",
        "timestamp": 1,
    }))
    .expect("message parses");
    send_message(
        &message,
        TEAM_RUN_ID,
        &config,
        &SendContext {
            is_lead: true,
            active_members: vec!["alice".to_string()],
            ..Default::default()
        },
    )
    .expect("send succeeds");

    // when the member inbox poller injects the lead mail
    poller.poll_once(None).expect("poll succeeds");

    let follow_ups = handle.follow_ups();
    assert_eq!(follow_ups.len(), 1);
    assert!(follow_ups[0].contains("continue with injected work"));
    assert_eq!(poller.pending_message_ids(), vec![MESSAGE_ID.to_string()]);

    // then the prior completed turn did not satisfy the new turn, and work can proceed
    assert!(!handle.is_idle());
    handle.steer("complete");
    assert!(handle.is_idle());
    assert_eq!(handle.last_assistant_text(), Some("steered-complete".to_string()));
    handle.dispose();
    poller.shutdown();
}
