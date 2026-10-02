use maho_ext_api::{CustomMessage, EventBus, ExtensionApi, ExtensionRuntime, ExtensionSessionProfile, LoadedExtension, MessageRenderOptions, SourceInfo, Theme, ToolContent};
use maho_omo_task::renderers::{register_task_message_renderers, CATEGORY_UNAVAILABLE_MESSAGE_TYPE, TASK_COMPLETION_MESSAGE_TYPE, TEAM_MEMBER_LIVENESS_MESSAGE_TYPE};
use serde_json::json;

fn api() -> ExtensionApi {
    let mut api = ExtensionApi::new(
        LoadedExtension::new("task", "/tmp".into(), SourceInfo::default()),
        ExtensionSessionProfile::default(), EventBus::default(), ExtensionRuntime::default(),
    );
    register_task_message_renderers(&mut api);
    api
}

fn render(api: &ExtensionApi, message: CustomMessage, width: usize) -> Vec<String> {
    let renderer = &api.registered.message_renderers[&message.custom_type];
    renderer(&message, &MessageRenderOptions::default(), &Theme::default())
        .expect("valid message").render(width)
}

#[test]
fn registers_all_three_custom_message_types() {
    let api = api();
    assert_eq!(api.registered.message_renderers.len(), 3);
    for kind in [TASK_COMPLETION_MESSAGE_TYPE, TEAM_MEMBER_LIVENESS_MESSAGE_TYPE, CATEGORY_UNAVAILABLE_MESSAGE_TYPE] {
        assert!(api.registered.message_renderers.contains_key(kind));
    }
}

#[test]
fn category_renderer_uses_text_content_and_sanitizes_terminal_control() {
    let message = CustomMessage { custom_type: CATEGORY_UNAVAILABLE_MESSAGE_TYPE.into(),
        content: vec![ToolContent::text("unavailable\x1b[31m")], display: true, details: None };
    assert_eq!(render(&api(), message, 80), maho_omo_task::renderers::render_category_unavailable(Some("unavailable\x1b[31m")));
}

#[test]
fn liveness_renderer_decodes_camel_case_details() {
    let message = CustomMessage { custom_type: TEAM_MEMBER_LIVENESS_MESSAGE_TYPE.into(),
        content: vec![], display: false,
        details: Some(json!({"memberName":"worker", "lastKnownState":"lost", "reason":"process exited", "killed":true})) };
    assert_eq!(render(&api(), message, 80), vec!["team member liveness", "member:worker", "last state:lost", "reason:process exited"]);
}

#[test]
fn completion_renderer_decodes_envelope_and_renders_at_each_width() {
    let detail = senpi_task::completion::CompletionDetails {
        task_id: "st_test".into(), name: "worker".into(), status: senpi_task::state::TaskStatus::Completed,
        category: None, agent_type: None, model: "faux/faux-1".into(), requested_model: None,
        fallback_models: None, resolved_model: Some(senpi_task::state::ResolvedModelRecord::new(
            senpi_task::state::ResolvedModelSource::Explicit, "faux", "faux-1")),
        duration_ms: 1000, tokens: Some(12), run_stats: None, final_response: "finished".into(),
        final_response_file: None, continuation_hint: "read output".into(),
    };
    let api = api();
    let message = CustomMessage { custom_type: TASK_COMPLETION_MESSAGE_TYPE.into(),
        content: vec![], display: true, details: Some(serde_json::to_value(vec![detail.clone()]).expect("valid test state")) };
    for width in [40, 80, 120] {
        assert_eq!(render(&api, message.clone(), width),
            senpi_task::completion::completion_message_lines(std::slice::from_ref(&detail), Some(width)));
    }
}
