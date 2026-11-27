//! Bounded await for first prompt admission on `thread_create` (`thread/control/prompt-start.ts`).

use std::time::{Duration, Instant};

use super::super::contracts_results::ThreadDelivery;
use super::super::result::admitted_wait;
use super::super::session_wait_types::{QueuedWaitKind, QueuedWaitingOn};
use super::super::store_api::GatewayStoreApi;
use super::super::types::{DeliveryState, DeliveryWaitKind};

pub const FIRST_PROMPT_START_WAIT_MS: u64 = 20_000;
pub const FIRST_PROMPT_START_RESERVE_MS: u64 = 2_000;
const RECHECK_MS: u64 = 250;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptStart {
    Started { delivery: ThreadDelivery },
    Queued { waiting_on: Option<QueuedWaitingOn> },
    NotStarted { state: String },
}

pub async fn await_prompt_start(
    store: &dyn GatewayStoreApi,
    delivery_id: &str,
    wait_ms: u64,
) -> PromptStart {
    let deadline = Instant::now() + Duration::from_millis(wait_ms);
    loop {
        let row = match store.delivery_view(delivery_id) {
            Ok(Some(view)) => view.row,
            Ok(None) | Err(_) => {
                return PromptStart::NotStarted {
                    state: "missing".to_owned(),
                };
            }
        };
        if matches!(row.state, DeliveryState::Admitted | DeliveryState::Applied) {
            match row.admission_kind.as_deref() {
                Some("started") => {
                    return PromptStart::Started {
                        delivery: ThreadDelivery::Started {
                            turn_id: row.turn_epoch.unwrap_or(0).to_string(),
                        },
                    };
                }
                Some("steered") => {
                    return PromptStart::Started {
                        delivery: ThreadDelivery::Steered {
                            turn_id: row.turn_epoch.unwrap_or(0).to_string(),
                        },
                    };
                }
                Some("queued" | "held") => {
                    return PromptStart::Queued {
                        waiting_on: admitted_wait(&row).map(|dw| QueuedWaitingOn {
                            kind: match dw.kind {
                                DeliveryWaitKind::Approval => QueuedWaitKind::Approval,
                                DeliveryWaitKind::Wake => QueuedWaitKind::Wake,
                                DeliveryWaitKind::Hold => QueuedWaitKind::Hold,
                            },
                        }),
                    };
                }
                _ => {}
            }
        }
        if matches!(row.state, DeliveryState::Refused | DeliveryState::Uncertain) {
            return PromptStart::NotStarted {
                state: row.state.as_str().to_owned(),
            };
        }
        let now = Instant::now();
        if now >= deadline {
            return PromptStart::NotStarted {
                state: row.state.as_str().to_owned(),
            };
        }
        let remaining = deadline.saturating_duration_since(now);
        tokio::time::sleep(remaining.min(Duration::from_millis(RECHECK_MS))).await;
    }
}
