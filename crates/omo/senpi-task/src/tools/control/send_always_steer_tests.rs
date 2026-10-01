//! `tools/control/send-always-steer.test.ts`

use std::sync::{Arc, Mutex};

use pretty_assertions::assert_eq;

use crate::state::TaskStatus;
use crate::tools::control::renderers::render_task_send_call;
use crate::tools::control::send::{TaskSendDeps, create_task_send_tool, run_task_send};
use crate::tools::control::send_schema::{TaskSendInput, TaskSendMessage};
use crate::tools::control::types::{
    ControlListScope, ControlSendInput, ControlTaskRecord, SendManager, SendOutcome, SendResultDetails,
};
use crate::tools::render::{LinesComponent, RendererTheme, ThemeColor};

struct UnusedManager;

impl SendManager for UnusedManager {
    fn send_to_task(&self, _input: &ControlSendInput) -> Result<SendOutcome, String> {
        Ok(SendOutcome::NotFound {
            reason: "unused".to_string(),
        })
    }

    fn list(&self, _scope: &ControlListScope) -> Vec<ControlTaskRecord> {
        Vec::new()
    }
}

struct RecordingManager {
    send_calls: Mutex<Vec<ControlSendInput>>,
}

impl SendManager for RecordingManager {
    fn send_to_task(&self, input: &ControlSendInput) -> Result<SendOutcome, String> {
        self.send_calls.lock().unwrap().push(input.clone());
        Ok(SendOutcome::Steered {
            task_id: "st_1".to_string(),
            status: TaskStatus::Running,
            delivered: "steer".to_string(),
        })
    }

    fn list(&self, _scope: &ControlListScope) -> Vec<ControlTaskRecord> {
        Vec::new()
    }
}

struct PlainTheme;

impl RendererTheme for PlainTheme {
    fn fg(&self, _color: ThemeColor, text: &str) -> String {
        text.to_string()
    }

    fn italic(&self, text: &str) -> String {
        text.to_string()
    }
}

fn plain_input(to: &str, message: &str) -> TaskSendInput {
    TaskSendInput {
        to: to.to_string(),
        message: Some(TaskSendMessage::Plain(message.to_string())),
        team_run_id: None,
        summary: None,
        all_scope: None,
    }
}

#[test]
fn given_the_lead_task_send_tool_when_its_public_contract_is_inspected_then_delivery_mode_is_absent() {
    let tool = create_task_send_tool(TaskSendDeps {
        manager: Arc::new(UnusedManager),
        team_routing: None,
        resolve_caller_session_id: None,
    });
    let parameter_names: Vec<String> = tool.parameters["properties"]
        .as_object()
        .expect("properties object")
        .keys()
        .cloned()
        .collect();

    assert!(!parameter_names.iter().any(|name| name == "deliver_as"));
    assert!(!tool.description.contains("deliver_as"));
    assert!(!tool.description.contains("followUp"));
    assert!(!tool.description.contains("interrupt"));
}

#[test]
fn given_a_plain_child_message_when_task_send_routes_it_then_it_requests_steer_unconditionally() {
    let manager = RecordingManager {
        send_calls: Mutex::new(Vec::new()),
    };

    let result = run_task_send(&manager, &plain_input("st_1", "new direction"), Some("parent-1"), None)
        .expect("task_send succeeds");

    assert_eq!(result.details.kind(), "steered");
    match &result.details {
        SendResultDetails::Steered { delivered, .. } => assert_eq!(delivered, "steer"),
        other => panic!("unexpected details: {other:?}"),
    }
    assert_eq!(
        *manager.send_calls.lock().unwrap(),
        vec![ControlSendInput {
            id_or_name: "st_1".to_string(),
            message: "new direction".to_string(),
            caller_session_id: Some("parent-1".to_string()),
            all_scope: None,
        }]
    );
}

#[test]
fn given_a_plain_task_send_call_when_rendered_then_no_delivery_option_is_shown() {
    let theme = PlainTheme;
    let component = render_task_send_call(&plain_input("st_1", "new direction"), &theme);

    let line = component.render(120).into_iter().next().unwrap_or_default();
    assert!(line.contains("task_send to:st_1"));
    assert!(!line.contains("deliver:"));
    assert!(line.contains("new direction"));
}
