//! Port of senpi packages/agent/src/harness/session/context.ts.

use std::collections::BTreeMap;

use crate::harness::context::Context;
use crate::harness::messages::{create_branch_summary_message, create_compaction_summary_message};
use crate::types::{AgentMessage, CustomAgentMessage};

use super::types::{Entry, EntryKind, EntryProjector};

#[derive(Default)]
pub struct SessionContextBuildOptions {
    pub entry_projectors: BTreeMap<String, EntryProjector>,
}

pub fn build_context_entries(path_entries: &[Entry]) -> Vec<Entry> {
    let mut compaction: Option<&Entry> = None;
    let mut compaction_index: Option<usize> = None;
    for (index, entry) in path_entries.iter().enumerate().rev() {
        if entry.entry_type() == super::types::EntryType::Compaction {
            compaction = Some(entry);
            compaction_index = Some(index);
            break;
        }
    }
    match (compaction, compaction_index) {
        (Some(compaction), Some(index)) => {
            let mut entries = vec![compaction.clone()];
            entries.extend(path_entries[index + 1..].iter().cloned());
            entries
        }
        _ => path_entries.to_vec(),
    }
}

fn is_context_message(message: &AgentMessage) -> bool {
    match message {
        AgentMessage::Llm(maho_ai::types::Message::Assistant(assistant)) => !matches!(
            assistant.stop_reason,
            maho_ai::types::StopReason::Error
                | maho_ai::types::StopReason::Aborted
                | maho_ai::types::StopReason::Deferred
        ),
        _ => true,
    }
}

pub fn session_entry_to_context_messages(entry: &Entry) -> Vec<AgentMessage> {
    match &entry.kind {
        EntryKind::Message { message, .. } => {
            if is_context_message(message) {
                vec![message.clone()]
            } else {
                Vec::new()
            }
        }
        EntryKind::Compaction {
            summary,
            retained_tail,
            tokens_before,
            ..
        } => {
            let mut messages = vec![AgentMessage::Custom(CustomAgentMessage::CompactionSummary(
                create_compaction_summary_message(summary.clone(), *tokens_before, entry.timestamp),
            ))];
            messages.extend(retained_tail.iter().filter(|m| is_context_message(m)).cloned());
            messages
        }
        EntryKind::BranchSummary { from_id, summary, .. } => {
            if summary.is_empty() {
                Vec::new()
            } else {
                vec![AgentMessage::Custom(CustomAgentMessage::BranchSummary(
                    create_branch_summary_message(summary.clone(), from_id.clone(), entry.timestamp),
                ))]
            }
        }
        EntryKind::Custom { .. } => Vec::new(),
    }
}

pub async fn build_session_context(
    path_entries: &[Entry],
    options: Option<&SessionContextBuildOptions>,
    context: &Context,
) -> Vec<AgentMessage> {
    let entries = build_context_entries(path_entries);
    let mut messages: Vec<AgentMessage> = Vec::new();
    for entry in entries {
        let EntryKind::Custom { custom_type, .. } = &entry.kind else {
            messages.extend(session_entry_to_context_messages(&entry));
            continue;
        };
        let projector = options.and_then(|options| options.entry_projectors.get(custom_type));
        if let Some(projector) = projector
            && let Some(projected) = projector(&entry, context).await {
                messages.extend(projected);
            }
    }
    messages
}
