use indexmap::IndexMap;

use crate::types::{
    Context, Message, SystemMessage, Tool, ToolReference, TranscriptContext, UserContent,
};
use crate::utils::text::{get_system_message_text, user_content_text};

pub fn create_initial_system_message(
    system_prompt: Option<&str>,
    tools: Option<&[Tool]>,
) -> Option<SystemMessage> {
    if system_prompt.is_none_or(str::is_empty) && tools.is_none_or(<[Tool]>::is_empty) {
        return None;
    }
    Some(SystemMessage {
        content: UserContent::Text(system_prompt.unwrap_or_default().to_owned()),
        sections: IndexMap::new(),
        tools_added: tools.unwrap_or_default().to_vec(),
        tools_removed: Vec::new(),
        timestamp: 0,
    })
}

pub fn normalize_context(context: &Context) -> TranscriptContext {
    let initial =
        create_initial_system_message(context.system_prompt.as_deref(), context.tools.as_deref());
    TranscriptContext {
        messages: initial
            .map(Message::System)
            .into_iter()
            .chain(context.messages.iter().cloned())
            .collect(),
        active_tool_names: context.active_tool_names.clone(),
        _normalized: (),
    }
}

/// Resolve transcript state at a legacy native adapter boundary. The public context remains
/// accepted by extension providers; builtin adapters replay it before reading prompt or tools.
pub fn resolve_provider_context(
    context: &Context,
    supports_mid_convo_system_messages: bool,
) -> Context {
    let transcript = resolve_transcript(
        &normalize_context(context),
        supports_mid_convo_system_messages,
    );
    let prompt = get_initial_system_message(&transcript.messages)
        .map(|message| get_system_message_text(&message.content, &message.sections));
    let tools = if supports_mid_convo_system_messages {
        get_initial_system_message(&transcript.messages)
            .map(|message| message.tools_added.clone())
            .unwrap_or_default()
    } else {
        get_current_tools(&transcript.messages)
    };
    Context {
        system_prompt: prompt,
        tools: Some(tools),
        messages: without_initial_system_message(&transcript.messages).to_vec(),
        active_tool_names: transcript.active_tool_names,
    }
}

pub fn get_initial_system_message(messages: &[Message]) -> Option<&SystemMessage> {
    match messages.first() {
        Some(Message::System(message)) => Some(message),
        _ => None,
    }
}

pub fn without_initial_system_message(messages: &[Message]) -> &[Message] {
    if get_initial_system_message(messages).is_some() {
        &messages[1..]
    } else {
        messages
    }
}

pub fn get_current_tools(messages: &[Message]) -> Vec<Tool> {
    let mut tools = IndexMap::new();
    for message in messages {
        if let Message::System(message) = message {
            for tool in &message.tools_removed {
                tools.shift_remove(&tool.name);
            }
            for tool in &message.tools_added {
                tools.insert(tool.name.clone(), tool.clone());
            }
        }
    }
    tools.into_values().collect()
}

pub fn get_current_system_message(messages: &[Message]) -> Option<SystemMessage> {
    let mut content = Vec::new();
    let mut sections = IndexMap::new();
    let mut timestamp = None;
    for message in messages {
        if let Message::System(message) = message {
            timestamp.get_or_insert(message.timestamp);
            let text = user_content_text(&message.content, "\n");
            if !text.is_empty() {
                content.push(text);
            }
            for (name, value) in &message.sections {
                match value {
                    None => {
                        sections.shift_remove(name);
                    }
                    Some(value) => {
                        sections.insert(name.clone(), Some(value.clone()));
                    }
                }
            }
        }
    }
    let tools = get_current_tools(messages);
    if timestamp.is_none() && tools.is_empty() {
        return None;
    }
    Some(SystemMessage {
        content: UserContent::Text(content.join("\n\n")),
        sections,
        tools_added: tools,
        tools_removed: Vec::new(),
        timestamp: timestamp.unwrap_or(0),
    })
}

pub fn get_current_system_prompt(messages: &[Message]) -> String {
    get_current_system_message(messages)
        .map(|message| get_system_message_text(&message.content, &message.sections))
        .unwrap_or_default()
}

pub fn collapse_system_messages(context: &TranscriptContext) -> TranscriptContext {
    TranscriptContext {
        messages: get_current_system_message(&context.messages)
            .map(Message::System)
            .into_iter()
            .chain(
                context
                    .messages
                    .iter()
                    .filter(|message| !matches!(message, Message::System(_)))
                    .cloned(),
            )
            .collect(),
        active_tool_names: context.active_tool_names.clone(),
        _normalized: (),
    }
}

pub fn resolve_transcript(
    context: &TranscriptContext,
    supports_mid_convo_system_messages: bool,
) -> TranscriptContext {
    if supports_mid_convo_system_messages {
        context.clone()
    } else {
        collapse_system_messages(context)
    }
}

pub fn to_tool_declaration(tool: &Tool) -> Tool {
    Tool {
        name: tool.name.clone(),
        description: tool.description.clone(),
        parameters: tool.parameters.clone(),
        constrained_sampling: tool.constrained_sampling.clone(),
        output_schema: None,
        freeform: None,
    }
}

pub fn declarations_equal(left: &Tool, right: &Tool) -> bool {
    to_tool_declaration(left) == to_tool_declaration(right)
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolStateChanges {
    pub tools_added: Vec<Tool>,
    pub tools_removed: Vec<ToolReference>,
}

pub fn get_tool_state_changes(previous: &[Tool], current: &[Tool]) -> ToolStateChanges {
    let previous_tools: IndexMap<_, _> = previous
        .iter()
        .map(|tool| (tool.name.as_str(), tool))
        .collect();
    let current_tools: IndexMap<_, _> = current
        .iter()
        .map(|tool| (tool.name.as_str(), tool))
        .collect();
    ToolStateChanges {
        tools_added: current
            .iter()
            .filter(|tool| {
                previous_tools
                    .get(tool.name.as_str())
                    .is_none_or(|previous| !declarations_equal(previous, tool))
            })
            .map(to_tool_declaration)
            .collect(),
        tools_removed: previous
            .iter()
            .filter(|tool| {
                current_tools
                    .get(tool.name.as_str())
                    .is_none_or(|current| !declarations_equal(tool, current))
            })
            .map(|tool| ToolReference {
                name: tool.name.clone(),
            })
            .collect(),
    }
}

pub fn get_declared_tools(messages: &[Message]) -> Vec<Tool> {
    let mut tools = IndexMap::new();
    for message in messages {
        if let Message::System(message) = message {
            for tool in &message.tools_added {
                tools.insert(tool.name.clone(), tool.clone());
            }
        }
    }
    tools.into_values().collect()
}

pub fn has_tool_redefinitions(messages: &[Message]) -> bool {
    let mut declared: IndexMap<&str, &Tool> = IndexMap::new();
    for message in messages {
        if let Message::System(message) = message {
            for tool in &message.tools_added {
                if declared
                    .get(tool.name.as_str())
                    .is_some_and(|previous| !declarations_equal(previous, tool))
                {
                    return true;
                }
                declared.insert(&tool.name, tool);
            }
        }
    }
    false
}

pub fn has_non_additive_tool_changes(messages: &[Message]) -> bool {
    let mut declared = std::collections::HashSet::new();
    for message in messages {
        if let Message::System(message) = message {
            if !message.tools_removed.is_empty() {
                return true;
            }
            for tool in &message.tools_added {
                if !declared.insert(&tool.name) {
                    return true;
                }
            }
        }
    }
    false
}

#[derive(Debug, Clone, PartialEq)]
pub struct TranscriptTools {
    pub request_tools: Vec<Tool>,
    pub anchors_additions: bool,
}

pub fn resolve_transcript_tools(
    messages: &[Message],
    supports_tool_additions: bool,
) -> TranscriptTools {
    let anchors_additions = supports_tool_additions && !has_non_additive_tool_changes(messages);
    TranscriptTools {
        request_tools: if anchors_additions {
            get_initial_system_message(messages)
                .map(|message| message.tools_added.clone())
                .unwrap_or_default()
        } else {
            get_current_tools(messages)
        },
        anchors_additions,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tool(name: &str, version: u32) -> Tool {
        Tool {
            name: name.into(),
            description: String::new(),
            parameters: json!({"version": version}),
            output_schema: None,
            freeform: None,
            constrained_sampling: None,
        }
    }

    fn system(added: Vec<Tool>, removed: &[&str]) -> Message {
        Message::System(SystemMessage {
            content: UserContent::Text(String::new()),
            sections: IndexMap::new(),
            tools_added: added,
            tools_removed: removed
                .iter()
                .map(|name| ToolReference {
                    name: (*name).into(),
                })
                .collect(),
            timestamp: 7,
        })
    }

    #[test]
    fn replay_keeps_replacement_position_but_readdition_moves_to_end() {
        let messages = vec![
            system(vec![tool("a", 1), tool("b", 1)], &[]),
            system(vec![tool("a", 2)], &[]),
            system(vec![tool("a", 3)], &["a"]),
        ];
        let current = get_current_tools(&messages);
        assert_eq!(current, vec![tool("b", 1), tool("a", 3)]);
        assert_eq!(
            get_declared_tools(&messages),
            vec![tool("a", 3), tool("b", 1)]
        );
        assert!(has_tool_redefinitions(&messages));
        assert!(!resolve_transcript_tools(&messages, true).anchors_additions);
    }

    #[test]
    fn replay_patches_sections_in_insertion_order() {
        let mut first = match system(Vec::new(), &[]) {
            Message::System(message) => message,
            _ => unreachable!(),
        };
        first.sections = IndexMap::from([
            ("z".into(), Some("1".into())),
            ("a".into(), Some("2".into())),
        ]);
        let mut second = first.clone();
        second.sections = IndexMap::from([
            ("z".into(), None),
            ("a".into(), Some("3".into())),
            ("b".into(), Some("4".into())),
        ]);
        let replayed =
            get_current_system_message(&[Message::System(first), Message::System(second)])
                .expect("system");
        assert_eq!(
            replayed.sections.into_iter().collect::<Vec<_>>(),
            vec![
                ("a".into(), Some("3".into())),
                ("b".into(), Some("4".into()))
            ]
        );
        assert_eq!(replayed.timestamp, 7);
    }

    #[test]
    fn normalization_preserves_explicit_empty_active_subset() {
        let context = Context {
            tools: Some(vec![tool("a", 1)]),
            active_tool_names: Some(Vec::new()),
            ..Context::default()
        };
        let normalized = normalize_context(&context);
        assert_eq!(normalized.active_tool_names, Some(Vec::new()));
        assert_eq!(get_current_tools(&normalized.messages), vec![tool("a", 1)]);
        assert!(normalize_context(&Context::default()).messages.is_empty());
    }

    #[test]
    fn provider_replay_is_idempotent_and_keeps_native_updates_in_place() {
        let context = Context {
            system_prompt: Some("base".into()),
            tools: Some(vec![tool("a", 1)]),
            messages: vec![system(vec![tool("b", 1)], &[])],
            active_tool_names: Some(vec!["b".into()]),
        };
        let native = resolve_provider_context(&context, true);
        assert_eq!(native.tools, context.tools);
        assert_eq!(native.messages, context.messages);
        assert_eq!(resolve_provider_context(&native, true), native);
        let collapsed = resolve_provider_context(&context, false);
        assert!(collapsed.messages.is_empty());
        assert_eq!(collapsed.tools, Some(vec![tool("a", 1), tool("b", 1)]));
        assert_eq!(collapsed.active_tool_names, context.active_tool_names);
        assert_eq!(resolve_provider_context(&collapsed, false), collapsed);
    }

    #[test]
    fn changed_definition_is_removed_and_added_but_display_fields_are_ignored() {
        let previous = tool("a", 1);
        let mut current = previous.clone();
        current.output_schema = Some(json!({"type":"string"}));
        assert!(
            get_tool_state_changes(&[previous.clone()], &[current])
                .tools_added
                .is_empty()
        );
        let changes = get_tool_state_changes(&[previous], &[tool("a", 2)]);
        assert_eq!(
            changes.tools_removed,
            vec![ToolReference { name: "a".into() }]
        );
        assert_eq!(changes.tools_added, vec![tool("a", 2)]);
    }
}
