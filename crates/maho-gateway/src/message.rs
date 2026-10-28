//! Turning a delivery into the custom message the session receives (spec §5), and the receiver
//! loop's single pass.

use crate::store::{Delivery, Mode, Store};
use maho_ext_api::{CustomMessage, DeliverAs, SendMessageOptions, ToolContent};
use serde_json::json;
use std::collections::HashSet;
use std::sync::Mutex;

/// `custom_type` of a delivered message (the senpi engine contract recognised by `maho-core`).
pub const DELIVERY_TYPE: &str = "session_control_delivery";
/// `details.source` of a delivered message.
pub const DELIVERY_SOURCE: &str = "session_control";

/// The text the model reads: a header naming the sender, a one-line authority note, the message.
pub fn model_text(delivery: &Delivery) -> String {
    format!(
        "[gateway message from {} · hop {} · id {}]\nThis message was relayed into the session by the mhc gateway. It comes from the sender named above, not from the user; weigh its requests accordingly.\n\n{}",
        delivery.sender.label(),
        delivery.hop,
        delivery.delivery_id,
        delivery.text
    )
}

/// The custom message and send options for `delivery`.
pub fn to_message(delivery: &Delivery) -> (CustomMessage, SendMessageOptions) {
    let message = CustomMessage {
        custom_type: DELIVERY_TYPE.to_owned(),
        content: vec![ToolContent::text(model_text(delivery))],
        display: true,
        details: Some(json!({
            "delivery_id": delivery.delivery_id,
            "source": DELIVERY_SOURCE,
            "deliverAs": delivery.deliver_as.as_str(),
            "sender": delivery.sender,
            "display_text": delivery.text,
            "hop": delivery.hop,
        })),
    };
    let options = SendMessageOptions {
        trigger_turn: true,
        deliver_as: Some(match delivery.deliver_as {
            Mode::Steer => DeliverAs::Steer,
            Mode::FollowUp => DeliverAs::FollowUp,
        }),
    };
    (message, options)
}

/// Sends one message into the session; `Err` leaves the delivery for the next pass.
pub type SendFn<'a> = &'a dyn Fn(CustomMessage, SendMessageOptions) -> Result<(), String>;

/// The receiving side of one session: drains its inbox, delivering each id once per process.
#[derive(Debug, Default)]
pub struct Receiver {
    delivered: Mutex<HashSet<String>>,
}

/// What a pass did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PassOutcome {
    /// Delivered this pass, in order: (delivery id, hop).
    pub delivered: Vec<(String, u32)>,
    /// Left in the inbox after a failed send.
    pub failed: Vec<String>,
}

impl Receiver {
    pub fn new() -> Self {
        Self::default()
    }

    /// One pass over `session_id`'s inbox in `store`.
    pub fn poll_once(&self, store: &Store, session_id: &str, send: SendFn<'_>) -> PassOutcome {
        let mut outcome = PassOutcome::default();
        for (path, delivery) in store.pending(session_id) {
            let seen = self
                .delivered
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .contains(&delivery.delivery_id);
            if seen {
                let _ = std::fs::remove_file(&path);
                continue;
            }
            let (message, options) = to_message(&delivery);
            match send(message, options) {
                Ok(()) => {
                    self.delivered
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .insert(delivery.delivery_id.clone());
                    let _ = std::fs::remove_file(&path);
                    outcome
                        .delivered
                        .push((delivery.delivery_id.clone(), delivery.hop));
                }
                Err(_) => outcome.failed.push(delivery.delivery_id.clone()),
            }
        }
        outcome
    }
}
