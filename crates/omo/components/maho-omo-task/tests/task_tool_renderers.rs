use crate::support;
use maho_ext_api::*;
use maho_omo_task::{
    member_extension::register_task_member_extension,
    tools::{
        task_cancel_renderers, task_output_renderers, task_send_renderers, task_tool_renderers,
    },
};
use serde_json::json;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

fn session(
    renderers: ToolRenderers<(), JsonValue>,
    args: JsonValue,
) -> ToolRendererSlots<(), JsonValue> {
    let mut context = ToolRenderContext {
        args,
        tool_call_id: "call".into(),
        invalidate: std::rc::Rc::new(|| {}),
        last_component: None,
        state: (),
        cwd: "/tmp".into(),
        execution_started: true,
        args_complete: true,
        is_partial: false,
        expanded: false,
        show_images: false,
        image_protocol: None,
        is_error: false,
        has_result: None,
        spinner_frame: None,
    };
    context.has_result = Some(true);
    ToolRendererSession {
        renderers: Arc::new(renderers),
        context,
    }
    .into_slots()
}
fn result(text: &str, details: JsonValue) -> AgentToolResult {
    AgentToolResult {
        content: vec![ContentBlock::Text(maho_ai::types::TextContent {
            text: text.into(),
            audience: None,
            text_signature: None,
        })],
        details,
        structured_content: None,
        usage: None,
        nested_calls: None,
        added_tool_names: None,
        terminate: None,
        is_error: None,
    }
}

#[test]
fn task_call_row_is_compact_and_tool_title_tinted() {
    let mut slots = session(
        task_tool_renderers(),
        json!({"prompt":"long prompt","task_summary":"Audit renderer parity","category":"quick","run_in_background":true}),
    );
    let line = slots
        .render_call(&Theme::default(), 120)
        .expect("call renderer");
    assert_eq!(line.len(), 1);
    assert!(line[0].contains("Audit renderer parity"), "{line:?}");
    assert!(line[0].contains("\x1b[3mbackground\x1b[23m"), "{line:?}");
}

#[test]
fn task_result_renders_aggregate_and_partial_rows() {
    let mut slots = session(task_tool_renderers(), json!({"prompt":"p"}));
    let details = json!({"task_id":"st_1","status":"running","mode":"spawn","run_in_background":true,"items":[{"task_id":"st_2","status":"completed"}]});
    let lines = slots
        .render_result(&result("spawned", details), &Theme::default(), 120)
        .expect("result renderer");
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(lines[1].contains("st_2"), "{lines:?}");
    let mut partial = session(task_tool_renderers(), json!({"prompt":"p"}));
    partial.session.context.is_partial = true;
    assert_eq!(
        partial
            .render_result(&result("one\ntwo", json!({})), &Theme::default(), 80)
            .expect("partial"),
        vec!["\x1b[39mone\x1b[39m", "\x1b[39mtwo\x1b[39m"]
    );
}

#[test]
fn control_tools_render_compact_call_and_outcome_rows() {
    let theme = Theme::default();
    let send = session(
        task_send_renderers(),
        json!({"to":"st_1","message":"hello"}),
    )
    .render_call(&theme, 80)
    .expect("send call");
    assert!(
        send[0].contains("task_send") && send[0].contains("to:st_1"),
        "{send:?}"
    );
    let steered = session(task_send_renderers(), json!({"to":"st_1"}))
        .render_result(
            &result(
                "ok",
                json!({"kind":"steered","task_id":"st_1","status":"running","delivered":"steer"}),
            ),
            &theme,
            80,
        )
        .expect("send result");
    assert_eq!(steered.len(), 1);
    assert!(steered[0].contains("st_1"), "{steered:?}");
    let cancel = session(task_cancel_renderers(), json!({"task_id":"st_1"})).render_result(&result("ok", json!({"kind":"cancelled","task_id":"st_1","previous_status":"running","status":"cancelled"})), &theme, 80).expect("cancel result");
    assert!(cancel[0].contains("st_1"), "{cancel:?}");
    let output = session(task_output_renderers(), json!({"task_id":"st_1"}))
        .render_call(&theme, 80)
        .expect("output call");
    assert!(output[0].contains("task_output target:st_1"), "{output:?}");
    let missing = session(task_output_renderers(), json!({"task_id":"st_1"}))
        .render_result(
            &result(
                "No task",
                json!({"kind":"not_found","reason":"No task","known_tasks":[]}),
            ),
            &theme,
            80,
        )
        .expect("output result");
    assert_eq!(missing.len(), 1);
}

#[test]
fn undecodable_details_fall_back_to_result_text() {
    let lines = session(task_send_renderers(), json!({"to":"st_1"}))
        .render_result(
            &result("plain\nbody", json!({"kind":"unknown"})),
            &Theme::default(),
            80,
        )
        .expect("fallback");
    assert_eq!(lines, vec!["plain", "body"]);
}

#[test]
fn member_extension_registers_nothing_outside_a_member_process() {
    let mut api = support::api();
    register_task_member_extension(&mut api, HashMap::new()).expect("non-member");
    assert!(api.registered.tools.is_empty());
    assert!(api.registered.handlers.is_empty());
}

#[derive(Default)]
struct Injected(Mutex<Vec<(CustomMessage, SendMessageOptions)>>);
impl ExtensionActions for Injected {
    fn send_message(
        &self,
        message: CustomMessage,
        options: SendMessageOptions,
    ) -> Result<(), ExtensionFailure> {
        self.0.lock().expect("injected").push((message, options));
        Ok(())
    }
    fn send_user_message(
        &self,
        _: UserMessageContent,
        _: SendUserMessageOptions,
    ) -> Result<(), ExtensionFailure> {
        panic!("unexpected user message")
    }
    fn append_entry(&self, _: &str, _: Option<JsonValue>) -> Result<(), ExtensionFailure> {
        Ok(())
    }
    fn get_all_tools(&self) -> Result<Vec<ToolInfo>, ExtensionFailure> {
        Ok(vec![])
    }
}

#[tokio::test]
async fn member_process_registers_scoped_send_and_steers_inbox_mail_on_session_start() {
    const TEAM_RUN_ID: &str = "77777777-7777-4777-8777-777777777777";
    const MESSAGE_ID: &str = "88888888-8888-4888-8888-888888888888";
    let root = tempfile::tempdir().expect("root");
    let state_dir = root.path().join("state");
    let session_dir = root.path().join("sessions");
    let base_dir = state_dir.join("teams");
    std::fs::create_dir_all(&session_dir).expect("session dir");
    let inbox =
        team_core::team_registry::get_inbox_dir(&base_dir, TEAM_RUN_ID, "alice").expect("inbox");
    std::fs::create_dir_all(&inbox).expect("inbox dir");
    std::fs::write(inbox.join(format!("{MESSAGE_ID}.json")), json!({"version":1,"messageId":MESSAGE_ID,"from":"lead","to":"alice","kind":"message","body":"start","timestamp":1}).to_string()).expect("message");
    let env = HashMap::from([
        ("SENPI_TASK_MEMBER".to_owned(), format!("{TEAM_RUN_ID}::alice")),
        ("SENPI_TASK_MEMBER_TASK_ID".to_owned(), "st_00000001".to_owned()),
        ("SENPI_TASK_TEAM_CONFIG".to_owned(), json!({"base_dir":base_dir.to_string_lossy(),"stateDir":state_dir.to_string_lossy(),"members":["alice"]}).to_string()),
        ("SENPI_CODING_AGENT_SESSION_DIR".to_owned(), session_dir.to_string_lossy().into_owned()),
    ]);
    let mut api = support::api();
    register_task_member_extension(&mut api, env).expect("member registration");
    let tool = api
        .registered
        .tools
        .iter()
        .find(|tool| tool.definition.name == "task_send")
        .expect("member task_send");
    assert_eq!(tool.definition.label, "Task Send");
    let injected = Arc::new(Injected::default());
    api.runtime.bind(injected.clone());
    let context = support::context();
    let mut start = ExtensionEvent::SessionStart(SessionStartEvent {
        reason: SessionReason::Startup,
        initial_model_provenance: None,
        previous_session_file: None,
        attended: None,
    });
    for handler in &api.registered.handlers[&EventKind::SessionStart] {
        handler(&mut start, &context).await.expect("session_start");
    }
    {
        let injected = injected.0.lock().expect("injected");
        assert_eq!(injected.len(), 1);
        assert_eq!(injected[0].0.custom_type, "senpi-task:team-message");
        assert!(!injected[0].0.display);
        assert!(injected[0].1.trigger_turn);
        assert_eq!(injected[0].1.deliver_as, Some(DeliverAs::Steer));
    }
    let mut stop = ExtensionEvent::SessionShutdown(SessionShutdownEvent {
        reason: SessionReason::Quit,
        target_session_file: None,
        signal: None,
    });
    for handler in &api.registered.handlers[&EventKind::SessionShutdown] {
        handler(&mut stop, &context)
            .await
            .expect("session_shutdown");
    }
}
