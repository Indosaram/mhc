//! Model-specific contract selection for model-facing context.

use crate::texts::{
    CORE_CONTRACT, DIRECTIVE_CLOSE_TAG, DIRECTIVE_MESSAGE_TYPE, DIRECTIVE_OPEN_TAG,
};
use maho_ext_api::{
    AgentMessage, ContentBlock, CustomAgentMessage, CustomMessageContent, Message, UserContent,
};

const GPT_CONTRACT: &str =
    include_str!("../../../assets/omo-latest/skills/ultrawork/references/gpt.md");

#[derive(Clone, Copy, Default)]
pub(crate) enum Variant {
    #[default]
    Default,
    Gpt,
}

impl Variant {
    pub(crate) fn for_model(model: Option<&str>) -> Self {
        let Some(name) = model.and_then(|id| id.rsplit('/').next()) else {
            return Self::Default;
        };
        let name = name.to_lowercase();
        if name.contains("gpt")
            || [
                "deepseek-flash",
                "deepseek-v4.1-flash",
                "deepseek-v4-1-flash",
                "deepseek-v4p1-flash",
            ]
            .into_iter()
            .any(|prefix| {
                name.strip_prefix(prefix)
                    .is_some_and(|tail| tail.is_empty() || tail.starts_with(['-', ':', '@']))
            })
        {
            Self::Gpt
        } else {
            Self::Default
        }
    }

    pub(crate) const fn contract(self) -> &'static str {
        match self {
            Self::Default => CORE_CONTRACT,
            Self::Gpt => GPT_CONTRACT,
        }
    }

    /// Returns whether the context carries a canonical directive, even if no edit was needed.
    pub(crate) fn project(self, messages: &mut [AgentMessage]) -> bool {
        let (previous, selected) = match self {
            Self::Default => (GPT_CONTRACT, CORE_CONTRACT),
            Self::Gpt => (CORE_CONTRACT, GPT_CONTRACT),
        };
        let previous = previous.trim_end();
        let selected = selected.trim_end();
        let mut found = false;
        let mut replace = |text: &mut String| {
            let old = format!("{DIRECTIVE_OPEN_TAG}\n{previous}\n\n{DIRECTIVE_CLOSE_TAG}");
            let new = format!("{DIRECTIVE_OPEN_TAG}\n{selected}\n\n{DIRECTIVE_CLOSE_TAG}");
            if text.contains(&old) {
                *text = text.replace(&old, &new);
                found = true;
            } else if text.contains(&new) {
                found = true;
            } else if text.starts_with("<skill-instruction name=\"ultrawork\" ")
                && text.contains("</skill-instruction>")
            {
                if text.contains(previous) {
                    *text = text.replacen(previous, selected, 1);
                    found = true;
                } else if text.contains(selected) {
                    found = true;
                }
            }
        };
        // Queued and explicit skill text arrives as a user message; idle injection is custom.
        // Edit only the owned request projection, not the queued payload or session transcript.
        for message in messages {
            let blocks = match message {
                AgentMessage::Llm(Message::User(user)) => match &mut user.content {
                    UserContent::Text(text) => {
                        replace(text);
                        continue;
                    }
                    UserContent::Blocks(content) => content,
                },
                AgentMessage::Custom(CustomAgentMessage::Custom(custom)) => {
                    if custom.custom_type != DIRECTIVE_MESSAGE_TYPE {
                        continue;
                    }
                    match &mut custom.content {
                        CustomMessageContent::Text(text) => {
                            replace(text);
                            continue;
                        }
                        CustomMessageContent::Blocks(content) => content,
                    }
                }
                _ => continue,
            };
            for block in blocks {
                if let ContentBlock::Text(text) = block {
                    replace(&mut text.text);
                }
            }
        }
        found
    }
}
