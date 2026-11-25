//! Port of senpi `packages/coding-agent/src/modes/rpc/mcp-presenter-bindings.ts`.
//!
//! Connection attachment, binding registration, and `connectionContext` lookup for
//! `McpPresenterAuthority`.

use crate::mcp_presenter_authority::McpPresenterContext;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentRecord {
    pub id: String,
    pub epoch: String,
}

#[derive(Debug, Default)]
pub struct PresenterBindingsState {
    pub instance: String,
    pub connections: HashMap<String, String>, // connection_id -> nonce
    pub bindings: HashMap<String, String>,    // session_id -> binding_epoch
    pub attachments: HashMap<(String, String), AttachmentRecord>, // (connection_id, session_id)
}

impl PresenterBindingsState {
    pub fn new(instance: impl Into<String>) -> Self {
        Self {
            instance: instance.into(),
            ..Self::default()
        }
    }

    pub fn connect(&mut self, connection_id: &str) -> String {
        let nonce = uuid::Uuid::new_v4().to_string();
        self.connections
            .insert(connection_id.to_owned(), nonce.clone());
        nonce
    }

    pub fn attach(&mut self, connection_id: &str, session_id: &str) -> Option<AttachmentRecord> {
        if !self.connections.contains_key(connection_id) || !self.bindings.contains_key(session_id)
        {
            return None;
        }
        let rec = self
            .attachments
            .entry((connection_id.to_owned(), session_id.to_owned()))
            .or_insert_with(|| AttachmentRecord {
                id: uuid::Uuid::new_v4().to_string(),
                epoch: uuid::Uuid::new_v4().to_string(),
            })
            .clone();
        Some(rec)
    }

    pub fn detach(&mut self, connection_id: &str, session_id: Option<&str>) {
        match session_id {
            Some(sid) => {
                self.attachments
                    .remove(&(connection_id.to_owned(), sid.to_owned()));
            }
            None => {
                self.attachments.retain(|(cid, _), _| cid != connection_id);
                self.connections.remove(connection_id);
            }
        }
    }

    pub fn bind(&mut self, session_id: &str) -> String {
        let epoch = uuid::Uuid::new_v4().to_string();
        self.bindings.insert(session_id.to_owned(), epoch.clone());
        epoch
    }

    pub fn unbind(&mut self, session_id: &str) {
        self.bindings.remove(session_id);
        self.attachments.retain(|(_, sid), _| sid != session_id);
    }

    pub fn connection_context(
        &self,
        connection_id: &str,
        session_id: &str,
    ) -> Option<McpPresenterContext> {
        let nonce = self.connections.get(connection_id)?;
        let binding_epoch = self.bindings.get(session_id)?;
        let attachment = self
            .attachments
            .get(&(connection_id.to_owned(), session_id.to_owned()))?;
        Some(McpPresenterContext {
            host_instance: self.instance.clone(),
            connection_nonce: nonce.clone(),
            binding_epoch: binding_epoch.clone(),
            attachment_id: attachment.id.clone(),
            attachment_epoch: attachment.epoch.clone(),
        })
    }
}
