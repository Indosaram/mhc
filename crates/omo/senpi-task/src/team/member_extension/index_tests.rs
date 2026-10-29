//! `team/member-extension/index.test.ts`

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use pretty_assertions::assert_eq;
use serde_json::json;
use team_core::team_registry::get_inbox_dir;

use crate::team::member_extension::index::{
    CustomMessage, ExtensionApi, ExtensionEventHandler, IntervalCallback, IntervalHandle,
    IntervalScheduler, MemberExtensionOptions, SendMessageOptions, TaskEventAppender,
    register_member_extension,
};
use crate::team::member_extension::self_poller::TeamMessageEvent;
use crate::team::member_extension::tools::MemberTaskSendTool;

const TEAM_RUN_ID: &str = "77777777-7777-4777-8777-777777777777";
const MESSAGE_ID: &str = "88888888-8888-4888-8888-888888888888";

#[derive(Default)]
struct FakeApi {
    handlers: Mutex<HashMap<String, Vec<ExtensionEventHandler>>>,
    tool_names: Mutex<Vec<String>>,
    injected: Mutex<Vec<(CustomMessage, SendMessageOptions)>>,
    loading: AtomicBool,
}

impl ExtensionApi for FakeApi {
    fn send_message(&self, message: CustomMessage, options: SendMessageOptions) {
        assert!(
            !self.loading.load(Ordering::SeqCst),
            "runtime action called during extension loading"
        );
        self.injected.lock().unwrap().push((message, options));
    }

    fn register_tool(&self, tool: MemberTaskSendTool) {
        self.tool_names.lock().unwrap().push(tool.name.to_string());
    }

    fn on(&self, event: &str, handler: ExtensionEventHandler) {
        self.handlers
            .lock()
            .unwrap()
            .entry(event.to_string())
            .or_default()
            .push(handler);
    }
}

impl FakeApi {
    fn dispatch(&self, event: &str) {
        let handlers = self.handlers.lock().unwrap();
        for handler in handlers.get(event).map(Vec::as_slice).unwrap_or_default() {
            handler().expect("handler should succeed");
        }
    }
}

struct NoopHandle;

impl IntervalHandle for NoopHandle {
    fn clear(&self) {}
}

/// Records intervals without running them (no background threads, no sleeps).
struct ManualScheduler;

impl IntervalScheduler for ManualScheduler {
    fn set_interval(&self, _interval_ms: u64, _callback: IntervalCallback) -> Box<dyn IntervalHandle> {
        Box::new(NoopHandle)
    }
}

#[test]
fn given_unread_mail_during_extension_loading_when_session_start_fires_then_inbound_team_mail_steers_at_the_lifecycle_edge()
 {
    let root = tempfile::tempdir().unwrap();
    let state_dir = root.path().join("state");
    let session_dir = root.path().join("sessions");
    let base_dir = state_dir.join("teams");
    std::fs::create_dir_all(&session_dir).unwrap();

    // Equivalent of team-core sendMessage from the lead to alice's inbox.
    let inbox_dir = get_inbox_dir(&base_dir, TEAM_RUN_ID, "alice").unwrap();
    std::fs::create_dir_all(&inbox_dir).unwrap();
    let message = json!({
        "version": 1,
        "messageId": MESSAGE_ID,
        "from": "lead",
        "to": "alice",
        "kind": "message",
        "body": "start only after bind",
        "timestamp": 1,
    });
    std::fs::write(
        inbox_dir.join(format!("{MESSAGE_ID}.json")),
        serde_json::to_string(&message).unwrap(),
    )
    .unwrap();

    let api = Arc::new(FakeApi {
        loading: AtomicBool::new(true),
        ..FakeApi::default()
    });
    let pi: Arc<dyn ExtensionApi> = api.clone();

    let mut env: HashMap<String, String> = HashMap::new();
    env.insert("SENPI_TASK_MEMBER".to_string(), format!("{TEAM_RUN_ID}::alice"));
    env.insert("SENPI_TASK_MEMBER_TASK_ID".to_string(), "st_00000001".to_string());
    env.insert(
        "SENPI_TASK_TEAM_CONFIG".to_string(),
        json!({
            "base_dir": base_dir.to_string_lossy(),
            "stateDir": state_dir.to_string_lossy(),
            "members": ["alice"],
        })
        .to_string(),
    );
    env.insert(
        "SENPI_CODING_AGENT_SESSION_DIR".to_string(),
        session_dir.to_string_lossy().into_owned(),
    );

    register_member_extension(
        &pi,
        MemberExtensionOptions {
            env,
            create_event_appender: Box::new(|_| {
                let appender: TaskEventAppender = Arc::new(|_: &str, _: TeamMessageEvent| {});
                appender
            }),
            scheduler: Some(Arc::new(ManualScheduler)),
        },
    )
    .unwrap();

    assert_eq!(api.injected.lock().unwrap().len(), 0);
    assert_eq!(*api.tool_names.lock().unwrap(), vec!["task_send".to_string()]);

    api.loading.store(false, Ordering::SeqCst);
    api.dispatch("session_start");

    {
        let injected = api.injected.lock().unwrap();
        assert_eq!(injected.len(), 1);
        let (message, options) = &injected[0];
        assert_eq!(message.custom_type, "senpi-task:team-message");
        assert!(message.content.contains(MESSAGE_ID));
        assert!(!message.display);
        assert_eq!(
            options,
            &SendMessageOptions {
                trigger_turn: true,
                deliver_as: "steer".to_string(),
            }
        );
    }

    api.dispatch("session_shutdown");
}
