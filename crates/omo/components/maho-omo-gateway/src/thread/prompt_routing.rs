//! Prompt routing metadata between RPC session clients (`thread/prompt-routing.ts`).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tokio::sync::oneshot;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptRoutePolicy {
    AnswerHere,
    LeaveToOwnClient,
    AutoCancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptRouteError {
    DuplicatePrompt,
    PromptRouteLocked,
    PromptResponseNotAuthorized,
    PromptNotFound,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptRouteResponse {
    Ok,
    Err(PromptRouteError),
}

struct PendingPrompt {
    generation: Arc<()>,
    target_session_id: String,
    driving_session_id: String,
    policy: PromptRoutePolicy,
    tx: Option<oneshot::Sender<Value>>,
    reason: Option<String>,
}

pub struct PromptRouter {
    pending: Arc<Mutex<HashMap<String, PendingPrompt>>>,
    completed_reasons: Arc<Mutex<HashMap<String, String>>>,
    timeout_ms: u64,
}

impl PromptRouter {
    pub fn new(timeout_ms: Option<u64>) -> Self {
        Self {
            pending: Arc::new(Mutex::new(HashMap::new())),
            completed_reasons: Arc::new(Mutex::new(HashMap::new())),
            timeout_ms: timeout_ms.unwrap_or(30_000),
        }
    }

    pub fn route(
        &self,
        id: &str,
        target_session_id: &str,
        driving_session_id: &str,
        policy: PromptRoutePolicy,
        request: Value,
        deliver: impl FnOnce(&str, Value),
    ) -> Result<oneshot::Receiver<Value>, PromptRouteError> {
        let (tx, rx) = oneshot::channel();
        let generation = Arc::new(());
        {
            let mut guard = self
                .pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if guard.contains_key(id) {
                return Err(PromptRouteError::DuplicatePrompt);
            }
            self.completed_reasons
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(id);
            guard.insert(
                id.to_owned(),
                PendingPrompt {
                    generation: Arc::clone(&generation),
                    target_session_id: target_session_id.to_owned(),
                    driving_session_id: driving_session_id.to_owned(),
                    policy,
                    tx: Some(tx),
                    reason: None,
                },
            );
        }
        if policy == PromptRoutePolicy::AutoCancel {
            self.cancel(id, "auto-cancel policy");
        } else {
            let dest = match policy {
                PromptRoutePolicy::AnswerHere => driving_session_id,
                PromptRoutePolicy::LeaveToOwnClient => target_session_id,
                PromptRoutePolicy::AutoCancel => unreachable!(),
            };
            deliver(dest, request);
            let pending = self.pending.clone();
            let completed = self.completed_reasons.clone();
            let id_owned = id.to_owned();
            let timeout = std::time::Duration::from_millis(self.timeout_ms);
            tokio::spawn(async move {
                tokio::time::sleep(timeout).await;
                cancel_in_map(
                    &pending,
                    &completed,
                    &id_owned,
                    "prompt timeout",
                    Some(&generation),
                );
            });
        }
        Ok(rx)
    }

    pub fn respond(&self, session_id: &str, id: &str, response: Value) -> PromptRouteResponse {
        let mut guard = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(p) = guard.get(id) else {
            return PromptRouteResponse::Err(PromptRouteError::PromptNotFound);
        };
        let answerer = if p.policy == PromptRoutePolicy::AnswerHere {
            &p.driving_session_id
        } else {
            &p.target_session_id
        };
        if p.policy == PromptRoutePolicy::AutoCancel || session_id != answerer {
            return PromptRouteResponse::Err(PromptRouteError::PromptResponseNotAuthorized);
        }
        if let Some(mut entry) = guard.remove(id)
            && let Some(tx) = entry.tx.take()
        {
            let _ = tx.send(response);
        }
        PromptRouteResponse::Ok
    }

    pub fn change_policy(&self, id: &str, policy: PromptRoutePolicy) -> PromptRouteResponse {
        let guard = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match guard.get(id) {
            None => PromptRouteResponse::Err(PromptRouteError::PromptNotFound),
            Some(p) if p.policy != policy => {
                PromptRouteResponse::Err(PromptRouteError::PromptRouteLocked)
            }
            Some(_) => PromptRouteResponse::Ok,
        }
    }

    pub fn close(&self, reason: &str) {
        let ids: Vec<String> = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .keys()
            .cloned()
            .collect();
        for id in ids {
            self.cancel(&id, reason);
        }
    }

    pub fn reason(&self, id: &str) -> Option<String> {
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(id)
            .and_then(|p| p.reason.clone())
            .or_else(|| {
                self.completed_reasons
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .get(id)
                    .cloned()
            })
    }

    fn cancel(&self, id: &str, reason: &str) {
        cancel_in_map(&self.pending, &self.completed_reasons, id, reason, None);
    }
}

fn cancel_in_map(
    pending: &Mutex<HashMap<String, PendingPrompt>>,
    completed: &Mutex<HashMap<String, String>>,
    id: &str,
    reason: &str,
    expected: Option<&Arc<()>>,
) {
    let mut guard = pending
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if expected.is_some_and(|generation| {
        !guard
            .get(id)
            .is_some_and(|entry| Arc::ptr_eq(&entry.generation, generation))
    }) {
        return;
    }
    if let Some(mut entry) = guard.remove(id) {
        completed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(id.to_owned(), reason.to_owned());
        if let Some(tx) = entry.tx.take() {
            let _ = tx.send(json!({
                "type": "extension_ui_response",
                "id": id,
                "cancelled": true,
            }));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn duplicate_prompt_does_not_panic_or_replace_original() {
        let router = PromptRouter::new(None);
        let original = router
            .route(
                "same",
                "target",
                "driver",
                PromptRoutePolicy::AnswerHere,
                json!({}),
                |_, _| {},
            )
            .expect("original route");
        let duplicate = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            router.route(
                "same",
                "other",
                "other",
                PromptRoutePolicy::AnswerHere,
                json!({}),
                |_, _| panic!("duplicate must not be delivered"),
            )
        }));
        assert!(matches!(
            duplicate,
            Ok(Err(PromptRouteError::DuplicatePrompt))
        ));
        assert_eq!(
            router.respond("driver", "same", json!({"answer":"first"})),
            PromptRouteResponse::Ok,
        );
        assert_eq!(
            original.await.expect("original response")["answer"],
            "first"
        );
    }

    #[tokio::test]
    async fn stale_timeout_cannot_cancel_reused_prompt_id() {
        let router = PromptRouter::new(None);
        let first = router
            .route(
                "same",
                "target",
                "driver",
                PromptRoutePolicy::AnswerHere,
                json!({}),
                |_, _| {},
            )
            .expect("first route");
        let old_generation = router.pending.lock().expect("pending")["same"]
            .generation
            .clone();
        assert_eq!(
            router.respond("driver", "same", json!({"answer":"first"})),
            PromptRouteResponse::Ok
        );
        assert_eq!(first.await.expect("first response")["answer"], "first");
        let second = router
            .route(
                "same",
                "target",
                "driver",
                PromptRoutePolicy::AnswerHere,
                json!({}),
                |_, _| {},
            )
            .expect("second route");
        cancel_in_map(
            &router.pending,
            &router.completed_reasons,
            "same",
            "prompt timeout",
            Some(&old_generation),
        );
        assert_eq!(
            router.respond("driver", "same", json!({"answer":"second"})),
            PromptRouteResponse::Ok
        );
        assert_eq!(second.await.expect("second response")["answer"], "second");
    }

    #[tokio::test]
    async fn reused_prompt_id_has_no_previous_cancellation_reason() {
        let router = PromptRouter::new(None);
        let first = router
            .route(
                "same",
                "target",
                "driver",
                PromptRoutePolicy::AutoCancel,
                json!({}),
                |_, _| {},
            )
            .expect("first route");
        assert_eq!(first.await.expect("cancelled")["cancelled"], true);
        assert!(router.reason("same").is_some());
        let second = router
            .route(
                "same",
                "target",
                "driver",
                PromptRoutePolicy::AnswerHere,
                json!({}),
                |_, _| {},
            )
            .expect("second route");
        assert_eq!(router.reason("same"), None);
        assert_eq!(
            router.respond("driver", "same", json!({})),
            PromptRouteResponse::Ok
        );
        second.await.expect("second response");
    }
}
