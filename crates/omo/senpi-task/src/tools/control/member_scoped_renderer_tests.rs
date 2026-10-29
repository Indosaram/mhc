//! `tools/control/member-scoped-renderer.test.ts`

use crate::tools::control::renderers::render_member_scoped_task_send_call;
use crate::tools::control::send_schema::MemberScopedTaskSendInput;
use crate::tools::render::{LinesComponent, RendererTheme, ThemeColor};

/// Mirrors the TS `TEST_THEME`: wraps foreground-colored text in color tags
/// and italic text in `<i>` tags so rendered output is inspectable.
struct TestTheme;

impl RendererTheme for TestTheme {
    fn fg(&self, _color: ThemeColor, text: &str) -> String {
        format!("[color]{text}[/color]")
    }

    fn italic(&self, text: &str) -> String {
        format!("<i>{text}</i>")
    }
}

#[test]
fn given_a_member_scoped_team_message_when_its_tool_call_renders_then_delivery_mode_is_not_displayed() {
    // given
    let theme = TestTheme;
    let args = MemberScopedTaskSendInput {
        to: "lead".to_string(),
        message: "peer update".to_string(),
        summary: None,
    };

    // when
    let rendered = render_member_scoped_task_send_call(&args, &theme)
        .render(120)
        .first()
        .cloned()
        .unwrap_or_default();

    // then
    assert!(!rendered.contains("deliver:"), "unexpected delivery token in {rendered:?}");
    assert!(!rendered.contains("followUp"), "unexpected followUp token in {rendered:?}");
}
