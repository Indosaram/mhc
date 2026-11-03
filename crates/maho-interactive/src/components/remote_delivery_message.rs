//! Port of senpi `modes/interactive/components/remote-delivery-message.ts` (pin 903b8336), with
//! the `sessionControlSenderOf` / `isSessionControlDeliveryDetails` readers from
//! `core/extensions/session-control-types.ts` it depends on.
//!
//! A message another session delivered renders as its own block, never as the user's own input,
//! so a reader always sees which text came from outside this terminal. A delivery that names its
//! sender shows one label line ("Sent by another agent · <name>") over the message as it was
//! written. A delivery that does not name one keeps the generic heading over the text the model
//! read.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use maho_tui::components::box_::Box as TuiBox;
use maho_tui::components::markdown::{DefaultTextStyle, Markdown, MarkdownOptions};
use maho_tui::components::spacer::Spacer;
use maho_tui::components::text::Text;
use maho_tui::tui::Component;
use serde_json::Value;

use super::custom_message::MessageRenderer;
use super::markdown_transform::{MarkdownComponent, get_markdown_theme};
use crate::theme::{Theme, ThemeBg, ThemeColor};

/// `customType` of the transcript entry an admitted delivery becomes.
pub const SESSION_CONTROL_DELIVERY_TYPE: &str = "session_control_delivery";

/// senpi `SessionControlSender`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionControlSender {
    Agent {
        session_id: String,
        name: Option<String>,
    },
    CommandLine {
        user: Option<String>,
    },
    External {
        platform: String,
        author: Option<String>,
    },
}

/// senpi `isSessionControlDeliveryDetails`.
pub fn is_session_control_delivery_details(value: &Value) -> bool {
    value.get("delivery_id").is_some_and(Value::is_string)
        && value.get("source").and_then(Value::as_str) == Some("session_control")
        && matches!(
            value.get("deliverAs").and_then(Value::as_str),
            Some("steer" | "followUp")
        )
}

/// senpi `sessionControlSenderOf`: the sender a delivery's `details` names, when well-formed.
pub fn session_control_sender_of(details: &Value) -> Option<SessionControlSender> {
    if !is_session_control_delivery_details(details) {
        return None;
    }
    let sender = details.get("sender")?.as_object()?;
    let text = |key: &str| sender.get(key).and_then(Value::as_str).map(str::to_owned);
    match sender.get("kind").and_then(Value::as_str)? {
        "agent" => Some(SessionControlSender::Agent {
            session_id: text("session_id")?,
            name: text("name"),
        }),
        "command_line" => Some(SessionControlSender::CommandLine { user: text("user") }),
        "external" => Some(SessionControlSender::External {
            platform: text("platform")?,
            author: text("author"),
        }),
        _ => None,
    }
}

/// senpi `senderLabel`.
pub fn sender_label(sender: &SessionControlSender) -> String {
    fn named(value: &Option<String>) -> Option<&str> {
        value.as_deref().filter(|value| !value.trim().is_empty())
    }
    match sender {
        SessionControlSender::Agent { name, .. } => match named(name) {
            Some(name) => format!("Sent by another agent \u{b7} {name}"),
            None => "Sent by another agent".to_owned(),
        },
        SessionControlSender::CommandLine { .. } => "Sent from the command line".to_owned(),
        SessionControlSender::External { platform, author } => match named(author) {
            Some(author) => format!("Sent from {platform} \u{b7} {author}"),
            None => format!("Sent from {platform}"),
        },
    }
}

fn display_text_of(details: &Value) -> Option<String> {
    if !is_session_control_delivery_details(details) {
        return None;
    }
    details
        .get("display_text")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn delivery_id_of(details: &Value) -> &str {
    details
        .get("delivery_id")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
}

fn message_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        content => content
            .as_array()
            .into_iter()
            .flatten()
            .filter(|part| part["type"] == "text")
            .filter_map(|part| part["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

fn markdown(text: &str, theme: &Theme) -> Rc<RefCell<dyn Component>> {
    let color_theme = theme.clone();
    let style = DefaultTextStyle {
        color: Some(Arc::new(move |line| {
            color_theme.fg(ThemeColor::CustomMessageText, line)
        })),
        ..Default::default()
    };
    Rc::new(RefCell::new(MarkdownComponent(Markdown::new(
        text,
        0,
        0,
        get_markdown_theme(theme),
        Some(style),
        MarkdownOptions::default(),
    ))))
}

/// senpi `renderRemoteDelivery`.
pub fn render_remote_delivery(message: &Value, theme: &Theme) -> Rc<RefCell<dyn Component>> {
    let mut block = TuiBox::with_padding(1, 1);
    let bg_theme = theme.clone();
    block.set_bg_fn(Some(Rc::new(move |text| {
        bg_theme.bg(ThemeBg::CustomMessageBg, text)
    })));
    let details = message.get("details").unwrap_or(&Value::Null);
    let sender = session_control_sender_of(details);
    let shown = sender.as_ref().and_then(|_| display_text_of(details));
    if let (Some(sender), Some(shown)) = (sender, shown) {
        block.add_child(Rc::new(RefCell::new(Text::with_padding(
            theme.fg(ThemeColor::Dim, &sender_label(&sender)),
            0,
            0,
        ))));
        block.add_child(Rc::new(RefCell::new(Spacer::new(1))));
        block.add_child(markdown(&shown, theme));
        return Rc::new(RefCell::new(block));
    }
    let label = theme.fg(
        ThemeColor::CustomMessageLabel,
        &theme.bold("remote message"),
    );
    let suffix = theme.fg(
        ThemeColor::Dim,
        &format!(" \u{b7} delivery {}", delivery_id_of(details)),
    );
    block.add_child(Rc::new(RefCell::new(Text::with_padding(
        format!("{label}{suffix}"),
        0,
        0,
    ))));
    block.add_child(Rc::new(RefCell::new(Spacer::new(1))));
    block.add_child(markdown(
        &message_text(message.get("content").unwrap_or(&Value::Null)),
        theme,
    ));
    Rc::new(RefCell::new(block))
}

/// senpi `builtInMessageRenderer`.
pub fn built_in_message_renderer(custom_type: &str) -> Option<MessageRenderer> {
    (custom_type == SESSION_CONTROL_DELIVERY_TYPE).then(|| {
        Rc::new(
            |message: &Value, _expanded: bool, _pad: usize, theme: &Theme| {
                Ok::<_, String>(Some(render_remote_delivery(message, theme)))
            },
        ) as MessageRenderer
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::custom_message::CustomMessageComponent;
    use crate::theme::ColorMode;
    use serde_json::json;

    const HEADER: &str =
        "[OMO_GATEWAY v=1 source=peer_agent actor=planner sender_session=01a1 delivery=d-1]";

    fn delivered(extra: Value) -> Vec<String> {
        let theme = Theme::builtin("dark", ColorMode::Truecolor).expect("builtin theme");
        let mut details =
            json!({"delivery_id": "d-1", "source": "session_control", "deliverAs": "followUp"});
        if let (Some(details), Some(extra)) = (details.as_object_mut(), extra.as_object()) {
            details.extend(extra.clone());
        }
        let message = json!({
            "role": "custom",
            "customType": SESSION_CONTROL_DELIVERY_TYPE,
            "content": format!("{HEADER}\nA message from another session.\n\"review the diff\""),
            "display": true,
            "details": details,
            "timestamp": 0,
        });
        let mut component = CustomMessageComponent::new(
            message,
            built_in_message_renderer(SESSION_CONTROL_DELIVERY_TYPE),
            theme.clone(),
            get_markdown_theme(&theme),
            1,
        );
        component
            .render(100)
            .iter()
            .map(|line| {
                maho_tui::utils::strip_terminal_sequences(line)
                    .trim()
                    .to_owned()
            })
            .filter(|line| !line.is_empty())
            .collect()
    }

    #[test]
    fn labels_another_sessions_message_with_its_name_and_the_written_text() {
        let lines = delivered(json!({
            "sender": {"kind": "agent", "session_id": "01a1", "name": "planner"},
            "display_text": "review the diff",
        }));
        assert_eq!(
            lines,
            ["Sent by another agent \u{b7} planner", "review the diff"]
        );
    }

    #[test]
    fn labels_an_unnamed_sessions_message_without_a_name() {
        let lines = delivered(
            json!({"sender": {"kind": "agent", "session_id": "01a1"}, "display_text": "review the diff"}),
        );
        assert_eq!(lines[0], "Sent by another agent");
    }

    #[test]
    fn labels_a_command_line_send() {
        let lines = delivered(
            json!({"sender": {"kind": "command_line", "user": "tester"}, "display_text": "from the shell"}),
        );
        assert_eq!(lines, ["Sent from the command line", "from the shell"]);
    }

    #[test]
    fn labels_an_external_chat_with_its_platform_and_author() {
        assert_eq!(
            delivered(
                json!({"sender": {"kind": "external", "platform": "slack", "author": "Jane"}, "display_text": "ship it"})
            ),
            ["Sent from slack \u{b7} Jane", "ship it"]
        );
        assert_eq!(
            delivered(
                json!({"sender": {"kind": "external", "platform": "slack"}, "display_text": "ship it"})
            )[0],
            "Sent from slack"
        );
        assert_eq!(
            delivered(
                json!({"sender": {"kind": "external", "platform": "slack", "author": "  "}, "display_text": "ship it"})
            )[0],
            "Sent from slack"
        );
    }

    #[test]
    fn falls_back_to_the_generic_heading_for_a_sender_it_cannot_read() {
        for sender in [
            json!({"kind": "agent"}),
            json!({"kind": "agent", "session_id": 7}),
            json!({"kind": "external"}),
            json!({"kind": "automation", "session_id": "01a1"}),
            json!("agent"),
            Value::Null,
        ] {
            let lines = delivered(json!({"sender": sender, "display_text": "review the diff"}));
            assert_eq!(lines[0], "remote message \u{b7} delivery d-1");
            assert!(lines.join("\n").contains("OMO_GATEWAY v=1"));
        }
    }

    #[test]
    fn falls_back_when_a_sender_is_named_but_the_written_text_is_not() {
        let lines = delivered(
            json!({"sender": {"kind": "agent", "session_id": "01a1", "name": "planner"}}),
        );
        assert_eq!(lines[0], "remote message \u{b7} delivery d-1");
        assert!(lines.join("\n").contains("OMO_GATEWAY v=1"));
    }

    #[test]
    fn keeps_the_generic_heading_and_full_text_without_a_sender() {
        let lines = delivered(json!({}));
        assert_eq!(lines[0], "remote message \u{b7} delivery d-1");
        assert!(lines.join("\n").contains("OMO_GATEWAY v=1"));
    }

    #[test]
    fn only_session_control_deliveries_get_the_built_in_renderer() {
        assert!(built_in_message_renderer("note").is_none());
    }
}
