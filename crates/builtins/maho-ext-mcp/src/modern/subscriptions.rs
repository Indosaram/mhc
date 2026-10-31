//! Port of senpi mcp/modern/subscriptions.ts: connection-owned `subscriptions/listen` streams.
use super::form_schema::is_record;
use futures::future::BoxFuture;
use serde_json::{Map, Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, Weak},
};

const SUBSCRIPTION_KEY: &str = "io.modelcontextprotocol/subscriptionId";
const LIST_FILTERS: [(&str, &str); 3] = [
    ("notifications/tools/list_changed", "toolsListChanged"),
    ("notifications/prompts/list_changed", "promptsListChanged"),
    (
        "notifications/resources/list_changed",
        "resourcesListChanged",
    ),
];

/// Upstream `Filter`, serialized as the `notifications` param.
type Filter = Map<String, Value>;
#[derive(Clone, Debug)]
struct Subscription {
    id: String,
    requested: Filter,
    accepted: Option<Filter>,
}
#[derive(Default, Debug)]
struct ResourceSweep {
    uris: Vec<String>,
}
type Sweep = Arc<Mutex<ResourceSweep>>;

/// Sends one message; for `subscriptions/listen` the future completes when the stream ends.
pub type SubscriptionSend =
    Arc<dyn Fn(Value) -> BoxFuture<'static, Result<(), String>> + Send + Sync>;

#[derive(Default)]
struct State {
    filter: Filter,
    resource_lists: HashMap<String, Sweep>,
    resource_cursors: HashMap<String, Sweep>,
    latest_sweep: Option<Sweep>,
    active: Option<Subscription>,
    pending: Option<Subscription>,
    timer: Option<tokio::task::JoinHandle<()>>,
    /// Open `subscriptions/listen` sends; aborting one drops its HTTP stream (upstream attempt abort).
    streams: HashMap<String, tokio::task::JoinHandle<()>>,
    retry: u32,
    closed: bool,
    started: bool,
}

fn id_key(id: &Value) -> String {
    id.to_string()
}
fn truthy_filter(filter: &Filter) -> bool {
    filter.values().any(|value| *value == Value::Bool(true))
        || filter
            .get("resourceSubscriptions")
            .and_then(Value::as_array)
            .is_some_and(|uris| !uris.is_empty())
}

/// Connection-owned streams bypass the finite request timeout and MRTR consent lifecycle.
pub struct ModernSubscriptions {
    send: SubscriptionSend,
    capabilities: Value,
    send_completes_stream: bool,
    state: Mutex<State>,
    this: Weak<Self>,
}
impl ModernSubscriptions {
    pub fn new(
        send: SubscriptionSend,
        capabilities: Value,
        send_completes_stream: bool,
    ) -> Arc<Self> {
        Arc::new_cyclic(|this| Self {
            send,
            capabilities,
            send_completes_stream,
            state: Mutex::default(),
            this: this.clone(),
        })
    }
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    fn supports(&self, feature: &str, flag: &str) -> bool {
        self.capabilities.get(feature).is_some_and(|capability| {
            is_record(capability) && capability.get(flag) == Some(&Value::Bool(true))
        })
    }

    pub fn outgoing(&self, message: &Value) {
        let Some(method) = message.get("method").and_then(Value::as_str) else {
            return;
        };
        let mut state = self.lock();
        if method == "notifications/cancelled"
            && let Some(id) = message
                .get("params")
                .and_then(|params| params.get("requestId"))
                .filter(|id| id.is_string() || id.is_number())
        {
            state.resource_lists.remove(&id_key(id));
        }
        let Some(id) = message.get("id") else {
            return;
        };
        if method == "resources/list" && self.supports("resources", "subscribe") {
            match message
                .get("params")
                .and_then(|params| params.get("cursor"))
            {
                None => {
                    let sweep: Sweep = Arc::default();
                    state.latest_sweep = Some(sweep.clone());
                    state.resource_lists.insert(id_key(id), sweep);
                }
                Some(Value::String(cursor)) => {
                    if let Some(sweep) = state.resource_cursors.remove(cursor) {
                        state.resource_lists.insert(id_key(id), sweep);
                    }
                }
                Some(_) => {}
            }
        }
        if state.started || !["tools/list", "prompts/list", "resources/list"].contains(&method) {
            return;
        }
        state.started = true;
        // The builtin catalog uses all three lists. Never opt in to an undeclared feature.
        for (feature, key) in [
            ("tools", "toolsListChanged"),
            ("prompts", "promptsListChanged"),
            ("resources", "resourcesListChanged"),
        ] {
            if self.supports(feature, "listChanged") {
                state.filter.insert(key.into(), Value::Bool(true));
            }
        }
        self.open(&mut state);
    }

    /// Returns true only for application messages that may reach the ordinary notification handlers.
    pub fn incoming(&self, message: &Value) -> bool {
        let mut state = self.lock();
        let id = message.get("id").filter(|id| !id.is_null());
        let sweep = id.and_then(|id| state.resource_lists.remove(&id_key(id)));
        if let (Some(sweep), Some(result)) = (sweep, message.get("result"))
            && let Some(resources) = result.get("resources").and_then(Value::as_array)
        {
            sweep
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .uris
                .extend(
                    resources
                        .iter()
                        .filter_map(|resource| resource.get("uri").and_then(Value::as_str))
                        .map(str::to_owned),
                );
            match result
                .get("nextCursor")
                .and_then(Value::as_str)
                .filter(|cursor| !cursor.is_empty())
            {
                Some(cursor) => {
                    state.resource_cursors.insert(cursor.into(), sweep);
                }
                None if state
                    .latest_sweep
                    .as_ref()
                    .is_some_and(|latest| Arc::ptr_eq(latest, &sweep)) =>
                {
                    let mut uris = sweep
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .uris
                        .clone();
                    uris.sort();
                    uris.dedup();
                    let uris = Value::from(uris);
                    if Some(&uris)
                        != state
                            .filter
                            .get("resourceSubscriptions")
                            .or(Some(&json!([])))
                    {
                        state.filter.insert("resourceSubscriptions".into(), uris);
                        self.open(&mut state);
                    }
                }
                None => {}
            }
        }
        if let Some(id) = message.get("id") {
            let stream = [state.active.as_ref(), state.pending.as_ref()]
                .into_iter()
                .flatten()
                .find(|entry| Value::String(entry.id.clone()) == *id)
                .map(|entry| entry.id.clone());
            let Some(stream) = stream else {
                return true;
            };
            // A response ends the long-lived request; graceful server shutdown is not a retry trigger.
            Self::retire(&mut state, &stream);
            return false;
        }
        let Some(method) = message.get("method").and_then(Value::as_str) else {
            return true;
        };
        let params = message.get("params");
        if method == "notifications/cancelled" {
            let request = params.and_then(|params| params.get("requestId"));
            let stream = [state.active.as_ref(), state.pending.as_ref()]
                .into_iter()
                .flatten()
                .find(|entry| request == Some(&Value::String(entry.id.clone())))
                .map(|entry| entry.id.clone());
            if let Some(stream) = stream {
                Self::retire(&mut state, &stream);
                return false;
            }
        }
        let subscription = params
            .and_then(|params| params.get("_meta"))
            .filter(|meta| is_record(meta))
            .and_then(|meta| meta.get(SUBSCRIPTION_KEY))
            .and_then(Value::as_str);
        let acknowledged = method == "notifications/subscriptions/acknowledged";
        let list = LIST_FILTERS
            .iter()
            .find(|(name, _)| *name == method)
            .map(|(_, key)| *key);
        let resource = method == "notifications/resources/updated";
        if !acknowledged && list.is_none() && !resource {
            return true;
        }
        let Some(active_id) = [state.active.as_ref(), state.pending.as_ref()]
            .into_iter()
            .flatten()
            .find(|entry| Some(entry.id.as_str()) == subscription)
            .map(|entry| entry.id.clone())
        else {
            return false;
        };
        if acknowledged {
            let is_active = state
                .active
                .as_ref()
                .is_some_and(|active| active.id == active_id);
            let entry = if is_active {
                state.active.clone()
            } else {
                state.pending.clone()
            };
            let Some(mut entry) = entry else {
                return false;
            };
            if entry.accepted.is_some() {
                return false;
            }
            let Some(offered) = params
                .and_then(|params| params.get("notifications"))
                .and_then(Value::as_object)
            else {
                self.failed_locked(&mut state, &active_id);
                self.cancel(&mut state, &active_id);
                return false;
            };
            let mut accepted = Filter::new();
            for (_, key) in LIST_FILTERS {
                if entry.requested.get(key) == Some(&Value::Bool(true))
                    && offered.get(key) == Some(&Value::Bool(true))
                {
                    accepted.insert(key.into(), Value::Bool(true));
                }
            }
            if let Some(uris) = offered
                .get("resourceSubscriptions")
                .and_then(Value::as_array)
            {
                let kept = entry
                    .requested
                    .get("resourceSubscriptions")
                    .and_then(Value::as_array)
                    .map(|requested| {
                        requested
                            .iter()
                            .filter(|uri| uris.contains(uri))
                            .cloned()
                            .collect::<Vec<_>>()
                    });
                if let Some(kept) = kept {
                    accepted.insert("resourceSubscriptions".into(), Value::from(kept));
                }
            }
            entry.accepted = Some(accepted.clone());
            let previous = state.active.take();
            state.pending = None;
            state.active = Some(entry);
            if let Some(previous) = previous.filter(|previous| previous.id != active_id) {
                self.cancel(&mut state, &previous.id);
            }
            if !truthy_filter(&accepted) {
                self.cancel(&mut state, &active_id);
            }
            return false;
        }
        let Some(accepted) = state
            .active
            .as_ref()
            .filter(|active| active.id == active_id)
            .and_then(|active| active.accepted.clone())
        else {
            return false;
        };
        let deliver = if resource {
            params
                .and_then(|params| params.get("uri"))
                .and_then(Value::as_str)
                .is_some_and(|uri| {
                    accepted
                        .get("resourceSubscriptions")
                        .and_then(Value::as_array)
                        .is_some_and(|uris| uris.iter().any(|entry| entry.as_str() == Some(uri)))
                })
        } else {
            list.is_some_and(|key| accepted.get(key) == Some(&Value::Bool(true)))
        };
        if deliver {
            state.retry = 0;
        }
        deliver
    }

    pub fn failed_send(&self, message: &Value) {
        if let Some(id) = message.get("id").filter(|id| !id.is_null()) {
            self.lock().resource_lists.remove(&id_key(id));
        }
    }

    pub fn dispose(&self) {
        let mut state = self.lock();
        state.closed = true;
        if let Some(timer) = state.timer.take() {
            timer.abort();
        }
        if let Some(pending) = state.pending.clone() {
            self.cancel(&mut state, &pending.id);
        }
        if let Some(active) = state.active.clone() {
            self.cancel(&mut state, &active.id);
        }
        state.resource_lists.clear();
        state.resource_cursors.clear();
        state.latest_sweep = None;
        state.streams.drain().for_each(|(_, stream)| stream.abort());
    }

    fn retire(state: &mut State, id: &str) {
        if state.active.as_ref().is_some_and(|active| active.id == id) {
            state.active = None;
        }
        if state
            .pending
            .as_ref()
            .is_some_and(|pending| pending.id == id)
        {
            state.pending = None;
        }
    }
    fn send_cancel(&self, id: &str) {
        let send = self.send.clone();
        let message = json!({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": id}});
        tokio::spawn(async move {
            let _ = send(message).await;
        });
    }
    fn cancel(&self, state: &mut State, id: &str) {
        Self::retire(state, id);
        if let Some(stream) = state.streams.remove(id) {
            stream.abort();
        }
        self.send_cancel(id);
    }

    fn open(&self, state: &mut State) {
        let current = state
            .pending
            .as_ref()
            .or(state.active.as_ref())
            .map(|entry| &entry.requested);
        if state.closed || current == Some(&state.filter) {
            return;
        }
        if let Some(timer) = state.timer.take() {
            timer.abort();
        }
        if let Some(pending) = state.pending.clone() {
            self.cancel(state, &pending.id);
        }
        if !truthy_filter(&state.filter) {
            if let Some(active) = state.active.clone() {
                self.cancel(state, &active.id);
            }
            return;
        }
        let mut bytes = [0u8; 16];
        let _ = getrandom::fill(&mut bytes);
        let id = format!(
            "senpi-subscription-{}",
            bytes
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        );
        let requested = state.filter.clone();
        state.pending = Some(Subscription {
            id: id.clone(),
            requested: requested.clone(),
            accepted: None,
        });
        // HTTP send remains pending for the entire SSE lifetime. No caller awaits it.
        let send = self.send.clone();
        let this = self.this.clone();
        let completes = self.send_completes_stream;
        let key = id.clone();
        let stream = tokio::spawn(async move {
            let result = send(json!({"jsonrpc": "2.0", "id": id, "method": "subscriptions/listen", "params": {"notifications": requested}})).await;
            if let Some(this) = this.upgrade() {
                let mut state = this.lock();
                state.streams.remove(&id);
                if result.is_err() || completes {
                    this.failed_locked(&mut state, &id);
                }
            }
        });
        state.streams.insert(key, stream);
    }

    fn failed_locked(&self, state: &mut State, id: &str) {
        let tracked = state.active.as_ref().is_some_and(|active| active.id == id)
            || state
                .pending
                .as_ref()
                .is_some_and(|pending| pending.id == id);
        if state.closed || !tracked {
            return;
        }
        Self::retire(state, id);
        if state.pending.is_some() || state.timer.is_some() {
            return;
        }
        let delay = (1000u64 * 2u64.pow(state.retry.min(5))).min(30_000);
        state.retry += 1;
        let this = self.this.clone();
        state.timer = Some(tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
            if let Some(this) = this.upgrade() {
                let mut state = this.lock();
                state.timer = None;
                this.open(&mut state);
            }
        }));
    }
}
impl Drop for ModernSubscriptions {
    fn drop(&mut self) {
        let state = self
            .state
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(timer) = state.timer.take() {
            timer.abort();
        }
        state.streams.drain().for_each(|(_, stream)| stream.abort());
    }
}
