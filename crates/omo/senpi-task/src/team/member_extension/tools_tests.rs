//! `team/member-extension/tools.test.ts`

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use pretty_assertions::assert_eq;
use team_core::config::TeamModeConfig;
use team_core::team_mailbox::list_unread_messages;

use crate::team::member_extension::index::{
    MemberExtensionConfigError, parse_member_extension_env,
};
use crate::team::member_extension::self_poller::TeamMessageEvent;
use crate::team::member_extension::tools::{
    MemberTaskSendDeps, MemberTaskSendDetails, MemberTaskSendError, MemberTaskSendInput,
    run_member_task_send,
};

const TEAM_RUN_ID: &str = "66666666-6666-4666-8666-666666666666";

struct Harness {
    _root: tempfile::TempDir,
    config: TeamModeConfig,
    events: Arc<Mutex<Vec<TeamMessageEvent>>>,
}

fn create_harness() -> Harness {
    let root = tempfile::tempdir().expect("tempdir");
    let base_dir = root.path().join("teams");
    let session_dir = root.path().join("sessions");
    std::fs::create_dir_all(&session_dir).expect("mkdir sessions");
    let config = TeamModeConfig::with_base_dir(base_dir.to_string_lossy().to_string());
    Harness {
        _root: root,
        config,
        events: Arc::new(Mutex::new(Vec::new())),
    }
}

fn basic_deps(harness: &Harness) -> MemberTaskSendDeps {
    MemberTaskSendDeps {
        team_run_id: TEAM_RUN_ID.to_string(),
        member_name: "alice".to_string(),
        task_id: "st_00000001".to_string(),
        config: harness.config.clone(),
        members: vec!["alice".to_string(), "bob".to_string()],
        append_event: None,
        now: None,
        new_message_id: None,
    }
}

fn input(to: &str, message: &str) -> MemberTaskSendInput {
    MemberTaskSendInput {
        to: to.to_string(),
        message: message.to_string(),
        summary: None,
    }
}

#[test]
fn given_member_and_lead_recipients_when_task_send_runs_then_each_durable_inbox_receives_its_unchanged_message()
 {
    // given
    let harness = create_harness();
    let ids = Arc::new(Mutex::new(vec![
        "99999999-9999-4999-8999-999999999999".to_string(),
        "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa".to_string(),
    ]));
    let events = Arc::clone(&harness.events);
    let mut deps = basic_deps(&harness);
    deps.append_event = Some(Box::new(move |_task_id: &str, event: TeamMessageEvent| {
        events.lock().unwrap().push(event);
    }));
    let id_source = Arc::clone(&ids);
    deps.new_message_id = Some(Box::new(move || {
        let mut ids = id_source.lock().unwrap();
        if ids.is_empty() {
            "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb".to_string()
        } else {
            ids.remove(0)
        }
    }));
    deps.now = Some(Box::new(|| 1));

    // when
    let member_result = run_member_task_send(&deps, &input("bob", "member note")).unwrap();
    let lead_result = run_member_task_send(&deps, &input("lead", "lead note")).unwrap();

    // then
    assert_eq!(
        member_result.details,
        MemberTaskSendDetails {
            kind: "team_message",
            message_id: "99999999-9999-4999-8999-999999999999".to_string(),
            to: "bob".to_string(),
        }
    );
    assert_eq!(
        lead_result.details,
        MemberTaskSendDetails {
            kind: "team_message",
            message_id: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa".to_string(),
            to: "lead".to_string(),
        }
    );
    let bob_bodies: Vec<String> = list_unread_messages(TEAM_RUN_ID, "bob", &harness.config)
        .unwrap()
        .into_iter()
        .map(|entry| entry.body)
        .collect();
    assert_eq!(bob_bodies, vec!["member note".to_string()]);
    let lead_bodies: Vec<String> = list_unread_messages(TEAM_RUN_ID, "lead", &harness.config)
        .unwrap()
        .into_iter()
        .map(|entry| entry.body)
        .collect();
    assert_eq!(lead_bodies, vec!["lead note".to_string()]);
    let event_types: Vec<String> = harness
        .events
        .lock()
        .unwrap()
        .iter()
        .map(|event| event.event_type.clone())
        .collect();
    assert_eq!(
        event_types,
        vec!["team_message_sent".to_string(), "team_message_sent".to_string()]
    );
}

#[test]
fn given_an_unknown_recipient_when_task_send_runs_then_it_rejects_without_writing_mail() {
    // given
    let harness = create_harness();
    let deps = basic_deps(&harness);

    // when / then
    let error = run_member_task_send(&deps, &input("nobody", "nope"))
        .expect_err("should reject");
    assert!(matches!(error, MemberTaskSendError::UnknownRecipient(_)));
    assert!(error.to_string().contains("Unknown team recipient"));
    assert!(!harness.config.base_dir.as_deref().is_some_and(|dir| {
        std::path::Path::new(dir).join(TEAM_RUN_ID).exists()
    }));
}

#[test]
fn given_an_unknown_recipient_when_task_send_runs_then_the_error_lists_the_valid_recipients() {
    // given
    let harness = create_harness();
    let deps = basic_deps(&harness);

    // when
    let caught = run_member_task_send(&deps, &input("ghost", "hello"))
        .expect_err("should reject");

    // then
    let message = caught.to_string();
    assert!(message.contains("ghost"));
    assert!(message.contains("alice"));
    assert!(message.contains("bob"));
    assert!(message.contains("lead"));
}

#[test]
fn given_malformed_member_identity_env_when_parsed_then_typed_configuration_errors_reject_malformed_values()
 {
    // given
    let mut base_env: HashMap<String, String> = HashMap::new();
    base_env.insert(
        "SENPI_TASK_MEMBER".to_string(),
        format!("{TEAM_RUN_ID}::alice"),
    );
    base_env.insert(
        "SENPI_TASK_MEMBER_TASK_ID".to_string(),
        "st_00000001".to_string(),
    );
    base_env.insert(
        "SENPI_TASK_TEAM_CONFIG".to_string(),
        serde_json::json!({
            "stateDir": "/tmp/state",
            "base_dir": "/tmp/state/teams",
            "members": ["alice", "bob"],
        })
        .to_string(),
    );
    base_env.insert(
        "SENPI_CODING_AGENT_SESSION_DIR".to_string(),
        "/tmp/state/sessions/st_00000001/".to_string(),
    );

    // when / then
    assert_eq!(
        parse_member_extension_env(&base_env).unwrap().member_name,
        "alice"
    );
    let identities = [
        String::new(),
        "alice".to_string(),
        format!("{TEAM_RUN_ID}::"),
        "::alice".to_string(),
        format!("{TEAM_RUN_ID}::alice::extra"),
    ];
    for identity in identities {
        let mut env = base_env.clone();
        env.insert("SENPI_TASK_MEMBER".to_string(), identity.clone());
        let error: MemberExtensionConfigError = parse_member_extension_env(&env)
            .err()
            .unwrap_or_else(|| panic!("identity {identity:?} should be rejected"));
        assert_eq!(error.name(), "MemberExtensionConfigError");
    }
    let mut env = base_env.clone();
    env.insert(
        "SENPI_TASK_MEMBER_TASK_ID".to_string(),
        "st_bad".to_string(),
    );
    let error: MemberExtensionConfigError = parse_member_extension_env(&env)
        .expect_err("bad task id should be rejected");
    assert_eq!(error.name(), "MemberExtensionConfigError");
}
