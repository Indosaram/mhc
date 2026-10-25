//! Memory tool provenance (#10488).
//!
//! Replaces model-supplied provenance with a verified claim receipt so neither identity keys
//! nor claim receipts can be forged from tool arguments.

use std::path::PathBuf;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::context::MemoryIdentityContext;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimReceipt {
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_quote: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approves: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub downgraded: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InjectedMemoryToolProvenance {
    pub session_id: String,
    pub user_turns: usize,
    pub identity_id: String,
    pub repo_path: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    pub claim: ClaimReceipt,
}

pub struct MemoryToolProvenanceInput<'a> {
    pub session_id: &'a str,
    pub user_turns: usize,
    pub context: &'a MemoryIdentityContext,
    pub tool_call_id: Option<&'a str>,
    pub input: &'a Value,
    pub entries: &'a [Value],
}

pub fn memory_tool_provenance(input: MemoryToolProvenanceInput<'_>) -> InjectedMemoryToolProvenance {
    let source_str = input.input.get("source").and_then(Value::as_str).unwrap_or("inferred");
    let user_quote = input.input.get("user_quote").and_then(Value::as_str);

    let claim = resolve_claim_receipt(source_str, user_quote, input.entries, input.tool_call_id);

    InjectedMemoryToolProvenance {
        session_id: input.session_id.to_string(),
        user_turns: input.user_turns,
        identity_id: input.context.identity.clone(),
        repo_path: input.context.identity_paths.repo.clone(),
        tool_call_id: input.tool_call_id.map(str::to_string),
        claim,
    }
}

pub fn resolve_claim_receipt(
    source: &str,
    user_quote: Option<&str>,
    entries: &[Value],
    tool_call_id: Option<&str>,
) -> ClaimReceipt {
    if source == "agent" {
        return ClaimReceipt {
            source: "agent".to_string(),
            user_quote: None,
            approves: None,
            downgraded: None,
        };
    }
    if source != "user" {
        return ClaimReceipt {
            source: "inferred".to_string(),
            user_quote: None,
            approves: None,
            downgraded: None,
        };
    }

    let exchanges = pending_consent_exchange(entries, tool_call_id);
    let quote = user_quote.unwrap_or("").trim();
    for (proposal, reply) in exchanges {
        if is_valid_user_reply(quote, &reply) {
            let approves = proposal_id(&proposal);
            return ClaimReceipt {
                source: "user".to_string(),
                user_quote: Some(quote.to_string()),
                approves: Some(approves),
                downgraded: None,
            };
        }
    }
    ClaimReceipt {
        source: "agent".to_string(),
        user_quote: None,
        approves: None,
        downgraded: Some(true),
    }
}

pub fn proposal_id(proposal: &str) -> String {
    let normalized: String = proposal.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut hasher = Sha256::new();
    hasher.update(normalized.as_bytes());
    let hash = format!("{:x}", hasher.finalize());
    hash[..16].to_string()
}

fn is_valid_user_reply(quote: &str, reply: &str) -> bool {
    let q = quote.trim().to_lowercase();
    let r = reply.trim().to_lowercase();
    if q.is_empty() || r.is_empty() {
        return false;
    }
    let bare_q = q.trim_end_matches(['.', '!']);
    let bare_r = r.trim_end_matches(['.', '!']);
    bare_q == bare_r && is_consent_phrase(bare_q)
}

fn is_consent_phrase(phrase: &str) -> bool {
    matches!(
        phrase,
        "yes"
            | "ok"
            | "okay"
            | "do it"
            | "go ahead"
            | "delete it"
            | "yes delete it"
            | "응"
            | "네"
            | "예"
            | "좋아"
            | "그래"
            | "지워"
            | "지워줘"
            | "삭제해"
            | "삭제해줘"
            | "진행해"
    )
}

struct ConversationMessage {
    role: String,
    text: String,
    tool_call_ids: Vec<String>,
}

fn pending_consent_exchange(entries: &[Value], tool_call_id: Option<&str>) -> Vec<(String, String)> {
    let mut messages: Vec<ConversationMessage> = Vec::new();
    for entry in entries {
        if let Some(msg) = conversation_message(entry) {
            messages.push(msg);
        }
    }
    if let Some(last) = messages.last() {
        if last.role == "assistant" && tool_call_id.is_some() && last.tool_call_ids.iter().any(|id| id == tool_call_id.unwrap()) {
            messages.pop();
        }
    }
    if messages.len() < 2 {
        return Vec::new();
    }
    let reply = &messages[messages.len() - 1];
    let proposal = &messages[messages.len() - 2];
    if reply.role == "user" && proposal.role == "assistant" && !proposal.text.trim().is_empty() {
        vec![(proposal.text.clone(), reply.text.clone())]
    } else {
        Vec::new()
    }
}

fn conversation_message(entry: &Value) -> Option<ConversationMessage> {
    if let Some(role) = entry.get("role").and_then(Value::as_str) {
        let text = entry.get("text").or_else(|| entry.get("content")).and_then(Value::as_str).unwrap_or("").to_string();
        return Some(ConversationMessage {
            role: role.to_string(),
            text,
            tool_call_ids: Vec::new(),
        });
    }
    if entry.get("type").and_then(Value::as_str) == Some("message") {
        let message = entry.get("message")?;
        let role = message.get("role").and_then(Value::as_str)?;
        let mut tool_call_ids = Vec::new();
        let mut text_parts = Vec::new();
        if let Some(content) = message.get("content").and_then(Value::as_array) {
            for block in content {
                if block.get("type").and_then(Value::as_str) == Some("toolCall") {
                    if let Some(id) = block.get("id").and_then(Value::as_str) {
                        tool_call_ids.push(id.to_string());
                    }
                }
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    text_parts.push(text);
                }
            }
        } else if let Some(content_str) = message.get("content").and_then(Value::as_str) {
            text_parts.push(content_str);
        }
        return Some(ConversationMessage {
            role: role.to_string(),
            text: text_parts.join(" "),
            tool_call_ids,
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use memory_core::identity::layout::build_identity_paths;
    use crate::binding::MemorySessionBinding;

    fn fixture_context(root: &std::path::Path) -> MemoryIdentityContext {
        let paths = build_identity_paths(root, "agent-test");
        MemoryIdentityContext::new(
            "agent-test".to_string(),
            paths,
            MemorySessionBinding {
                identity: "agent-test".to_string(),
                repo_path_hash: "hash".to_string(),
                bound_at: 0.0,
            },
        )
    }

    #[test]
    fn test_resolve_claim_receipt_user_matched() {
        let entries = vec![
            json!({ "role": "assistant", "text": "Shall I delete plugins X and Y?" }),
            json!({ "role": "user", "text": "yes delete it" }),
        ];
        let receipt = resolve_claim_receipt("user", Some("yes delete it"), &entries, None);
        assert_eq!(receipt.source, "user");
        assert_eq!(receipt.user_quote.as_deref(), Some("yes delete it"));
        assert!(receipt.approves.is_some());
    }

    #[test]
    fn test_resolve_claim_receipt_unverified_downgraded() {
        let entries = vec![
            json!({ "role": "assistant", "text": "Shall I delete plugins X and Y?" }),
            json!({ "role": "user", "text": "remove plugins X and Y" }),
        ];
        let receipt = resolve_claim_receipt("user", Some("remove plugins X and Y"), &entries, None);
        assert_eq!(receipt.source, "agent");
        assert_eq!(receipt.downgraded, Some(true));
    }

    #[test]
    fn test_memory_tool_provenance_construction() {
        let root = tempfile::tempdir().unwrap();
        let context = fixture_context(root.path());
        let input = json!({ "source": "agent" });
        let prov = memory_tool_provenance(MemoryToolProvenanceInput {
            session_id: "s1",
            user_turns: 3,
            context: &context,
            tool_call_id: Some("call-1"),
            input: &input,
            entries: &[],
        });
        assert_eq!(prov.session_id, "s1");
        assert_eq!(prov.user_turns, 3);
        assert_eq!(prov.claim.source, "agent");
    }
}
