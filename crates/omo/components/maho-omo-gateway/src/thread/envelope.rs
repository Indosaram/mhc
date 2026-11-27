//! Sender envelope fact extraction (`envelope.ts`).

use super::types::{BindingRef, EnvelopeOrigin, ExternalOrigin, GatewaySender, Provenance};

#[derive(Debug, Clone)]
pub struct SenderFacts {
    pub provenance: Provenance,
    pub principal: String,
    pub rate_principal: String,
    pub node: String,
    pub turn: Option<String>,
    pub cause: Option<String>,
    pub actor: String,
    pub binding: Option<BindingRef>,
    sender: GatewaySender,
}

impl SenderFacts {
    pub fn origin(&self, delivery_id: &str) -> EnvelopeOrigin {
        match &self.sender {
            GatewaySender::Session { durable_id, .. } => EnvelopeOrigin::Session {
                session: durable_id.clone(),
            },
            GatewaySender::Person { user, .. } => EnvelopeOrigin::External {
                external: ExternalOrigin {
                    platform: "cli".to_owned(),
                    account_id: user.clone(),
                    chat_id: "@person".to_owned(),
                    thread_id: "@chat".to_owned(),
                    message_id: delivery_id.to_owned(),
                    author: None,
                },
            },
            GatewaySender::Cli { user, .. } => EnvelopeOrigin::External {
                external: ExternalOrigin {
                    platform: "cli".to_owned(),
                    account_id: user.clone(),
                    chat_id: "@cli".to_owned(),
                    thread_id: "@chat".to_owned(),
                    message_id: delivery_id.to_owned(),
                    author: None,
                },
            },
            GatewaySender::External { origin, .. } => EnvelopeOrigin::External {
                external: origin.clone(),
            },
        }
    }
}

/// Computes canonical principal, rate-bucket principal, actor, origin builder, and binding ref
/// for any `GatewaySender`.
pub fn sender_facts(sender: &GatewaySender, mut resolved: Provenance) -> SenderFacts {
    if let GatewaySender::Session { name: Some(n), .. } = sender {
        let trimmed = n.trim();
        if !trimmed.is_empty() {
            resolved.session = Some(trimmed.to_owned());
        }
    }
    match sender {
        GatewaySender::Session {
            durable_id,
            name,
            turn_id,
            cause_delivery_id,
        } => {
            let principal = format!("session:{durable_id}");
            SenderFacts {
                provenance: resolved,
                rate_principal: principal.clone(),
                principal,
                node: durable_id.clone(),
                turn: turn_id.clone(),
                cause: cause_delivery_id.clone(),
                actor: name.clone().unwrap_or_else(|| durable_id.clone()),
                binding: None,
                sender: sender.clone(),
            }
        }
        GatewaySender::Person { uid, user } => {
            let principal = format!("person:{uid}");
            SenderFacts {
                provenance: resolved,
                rate_principal: principal.clone(),
                node: principal.clone(),
                principal,
                turn: None,
                cause: None,
                actor: user.clone(),
                binding: None,
                sender: sender.clone(),
            }
        }
        GatewaySender::Cli { uid, user } => {
            let principal = format!("cli:{uid}");
            SenderFacts {
                provenance: resolved,
                rate_principal: principal.clone(),
                node: principal.clone(),
                principal,
                turn: None,
                cause: None,
                actor: user.clone(),
                binding: None,
                sender: sender.clone(),
            }
        }
        GatewaySender::External {
            origin,
            binding_id,
            binding_revision,
        } => {
            let principal = format!("binding:{binding_id}");
            let rate_principal = match &origin.author {
                Some(author) => format!("{principal}#author:{}", author.platform_user_id),
                None => principal.clone(),
            };
            SenderFacts {
                provenance: resolved,
                principal: principal.clone(),
                rate_principal,
                node: principal,
                turn: None,
                cause: None,
                actor: origin.account_id.clone(),
                binding: Some(BindingRef {
                    binding_id: binding_id.clone(),
                    revision: *binding_revision,
                }),
                sender: sender.clone(),
            }
        }
    }
}
