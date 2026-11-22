use maho_ai::types::transcript::{
    collapse_system_messages, get_current_system_prompt, get_current_tools,
    get_initial_system_message, normalize_context,
};
use maho_ai::types::{Context, Message};

/// senpi `provider-context.ts` `toBuiltinProviderContext`: replay a provider transcript into the
/// `{ systemPrompt, tools, messages }` shape the fork's subscription providers read. `tools` stays `None` only when the
/// transcript has no system message at all.
///
/// The agent loop carries the prompt and tool declarations in transcript system messages and leaves `Context.tools`
/// unset (`build_provider_context`); senpi hands providers the normalized transcript the same way. A legacy context
/// with `system_prompt`/`tools` set is folded first, so the replay accepts both shapes.
pub fn to_builtin_provider_context(context: &Context) -> Context {
    let transcript = collapse_system_messages(&normalize_context(context));
    let head = get_initial_system_message(&transcript.messages).is_some();
    let system_prompt = if head {
        get_current_system_prompt(&transcript.messages)
    } else {
        String::new()
    };
    let tools = head.then(|| get_current_tools(&transcript.messages));
    Context {
        system_prompt: (!system_prompt.is_empty()).then_some(system_prompt),
        messages: transcript
            .messages
            .into_iter()
            .filter(|message| !matches!(message, Message::System(_)))
            .collect(),
        tools,
        active_tool_names: transcript.active_tool_names,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use maho_ai::types::{SystemMessage, Tool, UserContent, UserMessage};

    fn tool(name: &str) -> Tool {
        serde_json::from_value(
            serde_json::json!({"name":name,"description":name,"parameters":{"type":"object"}}),
        )
        .expect("tool")
    }
    fn user(text: &str) -> Message {
        Message::User(UserMessage {
            content: UserContent::Text(text.into()),
            timestamp: 1,
        })
    }

    #[test]
    fn transcript_system_messages_become_the_prompt_and_current_tools() {
        let system = |text: &str, added: Vec<Tool>| {
            Message::System(SystemMessage {
                content: UserContent::Text(text.into()),
                sections: Default::default(),
                tools_added: added,
                tools_removed: Vec::new(),
                timestamp: 0,
            })
        };
        let context = Context {
            messages: vec![
                system("base", vec![tool("read")]),
                user("request"),
                system("updated rules", vec![tool("repoSearch")]),
            ],
            active_tool_names: Some(vec!["read".into()]),
            ..Default::default()
        };
        let resolved = to_builtin_provider_context(&context);
        assert_eq!(
            resolved.system_prompt.as_deref(),
            Some("base\n\nupdated rules")
        );
        let names: Vec<_> = resolved
            .tools
            .as_ref()
            .expect("tools")
            .iter()
            .map(|tool| tool.name.as_str())
            .collect();
        assert_eq!(names, ["read", "repoSearch"]);
        assert_eq!(resolved.messages, vec![user("request")]);
        assert_eq!(resolved.active_tool_names, Some(vec!["read".into()]));
        assert_eq!(
            to_builtin_provider_context(&resolved)
                .tools
                .map(|t| t.len()),
            Some(2)
        );
    }

    #[test]
    fn a_transcript_without_a_system_message_declares_no_tools() {
        let resolved = to_builtin_provider_context(&Context {
            messages: vec![user("request")],
            ..Default::default()
        });
        assert_eq!(resolved.system_prompt, None);
        assert_eq!(resolved.tools, None);
    }
}
