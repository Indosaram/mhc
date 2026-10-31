//! Host-level `session_status` subscriptions (senpi `session-status-stream.ts`).
//!
//! A subscription is connection-scoped and activity-neutral: it never attaches to a session, never counts toward
//! `attachments` and never refreshes a session's idle window. Its records ride the subscribing connection's own FIFO,
//! and every publish is called by the writer AFTER it enqueued the record that caused the change. `seq` is per
//! subscription: the snapshot carries `seq: 0` and every delta is strictly +1.
//!
//! A new subscription answers BEFORE it goes live: the router calls [`SessionStatusStream::subscribe`], enqueues the
//! response carrying the snapshot, then [`SessionStatusStream::activate`]. Deltas published in between are held and
//! flushed onto the connection the moment `activate` runs, so no `seq > 0` record overtakes the `seq 0` snapshot.
//!
//! `effects: true` subscriptions also receive `effect_status` records ([`SessionStatusStream::publish_effect`]) on
//! the same per-subscription `seq`, and every `session_status` record carries `lastEffect` when the host installed a
//! provider ([`SessionStatusStream::set_last_effect_provider`]).
use crate::session_status::{
    SessionStatusActor, SessionStatusCause, SessionStatusTerminal, SessionView,
    derive_session_status,
};
use crate::session_status_subscriptions::{
    SubscriptionOptions, mirrors_record, subscription_matches,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// Where the stream reads sessions from (senpi `SessionStatusSource`).
pub trait SessionStatusSource: Send + Sync {
    fn session_ids(&self) -> Vec<String>;
    fn view(&self, session_id: &str) -> Option<SessionView>;
}

/// How the stream reaches a connection (senpi `SessionStatusDelivery`).
pub trait SessionStatusDelivery {
    /// Queues one JSONL line on the connection's FIFO; `false` drops the subscription (the connection is gone).
    fn enqueue(&self, connection_id: &str, line: &str) -> bool;
    /// The fanout's pending `question` frames for the session.
    fn pending_questions(&self, session_id: &str) -> usize;
}

fn iso_now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Fills `session_status.lastEffect` (senpi `SessionLastEffectProvider`): `{ effectId, kind, state }`.
pub type SessionLastEffectProvider = Box<dyn Fn(&str) -> Option<Value> + Send + Sync>;

/// One connection may hold at most this many subscriptions.
pub const MAX_SUBSCRIPTIONS_PER_CONNECTION: usize = 64;

struct Subscription {
    connection_id: String,
    options: SubscriptionOptions,
    seq: u64,
    /// `Some` while pending: deltas held until `activate`.
    held: Option<Vec<String>>,
}

/// The answer [`SessionStatusStream::subscribe`] returns.
pub struct Subscribed {
    pub subscription_id: String,
    pub snapshot: Vec<Value>,
    pub seq: u64,
}

pub struct SessionStatusStream {
    subscriptions: BTreeMap<String, Subscription>,
    terminals: BTreeMap<String, SessionStatusTerminal>,
    last_views: BTreeMap<String, SessionView>,
    last_changes: BTreeMap<String, Value>,
    next_subscription: u64,
    now: Box<dyn Fn() -> String + Send + Sync>,
    last_effect: Option<SessionLastEffectProvider>,
}

impl Default for SessionStatusStream {
    fn default() -> Self {
        Self::new(Box::new(iso_now))
    }
}

impl SessionStatusStream {
    pub fn new(now: Box<dyn Fn() -> String + Send + Sync>) -> Self {
        Self {
            subscriptions: BTreeMap::new(),
            terminals: BTreeMap::new(),
            last_views: BTreeMap::new(),
            last_changes: BTreeMap::new(),
            next_subscription: 0,
            now,
            last_effect: None,
        }
    }

    pub fn set_last_effect_provider(&mut self, provider: Option<SessionLastEffectProvider>) {
        self.last_effect = provider;
    }

    /// Live subscriptions (pending ones excluded).
    pub fn size(&self) -> usize {
        self.subscriptions
            .values()
            .filter(|subscription| subscription.held.is_none())
            .count()
    }

    pub fn connection_size(&self, connection_id: &str) -> usize {
        self.subscriptions
            .values()
            .filter(|subscription| subscription.connection_id == connection_id)
            .count()
    }

    pub fn subscribe(
        &mut self,
        source: &dyn SessionStatusSource,
        delivery: &dyn SessionStatusDelivery,
        connection_id: &str,
        options: SubscriptionOptions,
    ) -> Subscribed {
        self.next_subscription += 1;
        let id = format!("sub-{}", self.next_subscription);
        let mut snapshot = Vec::new();
        for session_id in source.session_ids() {
            let Some(view) = self.current_view(source, &session_id) else {
                continue;
            };
            if !subscription_matches(&options, &session_id, &view.entry) {
                continue;
            }
            let change = self.last_change_of(&session_id);
            let questions = delivery.pending_questions(&session_id);
            snapshot.push(self.record(
                &id,
                0,
                &options,
                &session_id,
                &view,
                questions,
                None,
                change,
            ));
        }
        self.subscriptions.insert(
            id.clone(),
            Subscription {
                connection_id: connection_id.to_owned(),
                options,
                seq: 0,
                held: Some(Vec::new()),
            },
        );
        Subscribed {
            subscription_id: id,
            snapshot,
            seq: 0,
        }
    }

    /// Flips the subscription live once its answer is on the connection's FIFO, flushing held deltas.
    pub fn activate(&mut self, delivery: &dyn SessionStatusDelivery, subscription_id: &str) {
        let Some(subscription) = self.subscriptions.get_mut(subscription_id) else {
            return;
        };
        let Some(held) = subscription.held.take() else {
            return;
        };
        for line in held {
            delivery.enqueue(&subscription.connection_id, &line);
        }
    }

    pub fn unsubscribe(&mut self, connection_id: &str, subscription_id: &str) -> bool {
        if self
            .subscriptions
            .get(subscription_id)
            .is_some_and(|subscription| subscription.connection_id == connection_id)
        {
            self.subscriptions.remove(subscription_id);
            return true;
        }
        false
    }

    pub fn drop_connection(&mut self, connection_id: &str) {
        self.subscriptions
            .retain(|_, subscription| subscription.connection_id != connection_id);
    }

    /// Publishes one change. `context_only` changes reach only subscribers that asked for context.
    pub fn publish(
        &mut self,
        source: &dyn SessionStatusSource,
        delivery: &dyn SessionStatusDelivery,
        session_id: &str,
        cause: SessionStatusCause,
        by: SessionStatusActor,
        context_only: bool,
    ) {
        let change = json!({"at":(self.now)(),"cause":cause,"by":by});
        self.last_changes
            .insert(session_id.to_owned(), change.clone());
        if self.subscriptions.is_empty() {
            return;
        }
        let Some(view) = self.current_view(source, session_id) else {
            return;
        };
        let questions = delivery.pending_questions(session_id);
        let mut dropped = Vec::new();
        let ids = self.subscriptions.keys().cloned().collect::<Vec<_>>();
        for id in ids {
            let subscription = &self.subscriptions[&id];
            if (context_only && !subscription.options.include_context)
                || !subscription_matches(&subscription.options, session_id, &view.entry)
            {
                continue;
            }
            let seq = subscription.seq + 1;
            let record = self.record(
                &id,
                seq,
                &subscription.options,
                session_id,
                &view,
                questions,
                Some(cause),
                change.clone(),
            );
            let line = match crate::jsonl::serialize_json_line(&record) {
                Ok(line) => line,
                Err(_) => continue,
            };
            let subscription = self.subscriptions.get_mut(&id).expect("listed above");
            subscription.seq = seq;
            match &mut subscription.held {
                Some(held) => held.push(line),
                None => {
                    if !delivery.enqueue(&subscription.connection_id, &line) {
                        dropped.push(id);
                    }
                }
            }
        }
        for id in dropped {
            self.subscriptions.remove(&id);
        }
    }

    /// Publishes one effect transition to every matched `effects: true` subscriber, sharing its `seq` (senpi
    /// `publishEffect`).
    pub fn publish_effect(
        &mut self,
        source: &dyn SessionStatusSource,
        delivery: &dyn SessionStatusDelivery,
        session_id: &str,
        status: &crate::effect_record::EffectStatus,
    ) {
        if self.subscriptions.is_empty() {
            return;
        }
        let Some(view) = self.current_view(source, session_id) else {
            return;
        };
        let at = status
            .updated_at
            .clone()
            .or_else(|| status.accepted_at.clone())
            .unwrap_or_else(|| (self.now)());
        let mut dropped = Vec::new();
        for (id, subscription) in &mut self.subscriptions {
            if !subscription.options.effects
                || !subscription_matches(&subscription.options, session_id, &view.entry)
            {
                continue;
            }
            let seq = subscription.seq + 1;
            let record = crate::session_effect_status::effect_status_record(
                id, seq, session_id, status, &at,
            );
            let Ok(line) = crate::jsonl::serialize_json_line(&record) else {
                continue;
            };
            subscription.seq = seq;
            match &mut subscription.held {
                Some(held) => held.push(line),
                None => {
                    if !delivery.enqueue(&subscription.connection_id, &line) {
                        dropped.push(id.clone());
                    }
                }
            }
        }
        for id in dropped {
            self.subscriptions.remove(&id);
        }
    }

    pub fn seal(
        &mut self,
        source: &dyn SessionStatusSource,
        delivery: &dyn SessionStatusDelivery,
        session_id: &str,
        terminal: SessionStatusTerminal,
        cause: SessionStatusCause,
        by: SessionStatusActor,
    ) {
        self.terminals.insert(session_id.to_owned(), terminal);
        self.publish(source, delivery, session_id, cause, by, false);
    }

    pub fn forget(&mut self, session_id: &str) {
        self.terminals.remove(session_id);
        self.last_views.remove(session_id);
        self.last_changes.remove(session_id);
    }

    /// Connections whose dialog-opted subscription (with any mirror) matches the session (senpi `dialogTargets`).
    pub fn dialog_targets(
        &self,
        source: &dyn SessionStatusSource,
        session_id: &str,
    ) -> Vec<String> {
        self.targets(source, session_id, "ui_request_pending", true)
    }

    /// Connections that mirror `record_type` of this session; `snapshot_marker` reaches any subscription whose mirror
    /// is not `none` (senpi `mirrorTargets`).
    pub fn mirror_targets(
        &self,
        source: &dyn SessionStatusSource,
        session_id: &str,
        record_type: &str,
    ) -> Vec<String> {
        self.targets(source, session_id, record_type, false)
    }

    fn targets(
        &self,
        source: &dyn SessionStatusSource,
        session_id: &str,
        record_type: &str,
        dialogs: bool,
    ) -> Vec<String> {
        let wanted = self
            .subscriptions
            .values()
            .filter(|subscription| {
                let options = &subscription.options;
                (!dialogs || options.dialogs)
                    && if record_type == "snapshot_marker" || dialogs {
                        options.mirror != crate::session_status_subscriptions::MirrorSelection::None
                    } else {
                        mirrors_record(&options.mirror, record_type)
                    }
            })
            .collect::<Vec<_>>();
        if wanted.is_empty() {
            return Vec::new();
        }
        let entry = source.view(session_id).map(|view| view.entry).or_else(|| {
            self.last_views
                .get(session_id)
                .map(|view| view.entry.clone())
        });
        let Some(entry) = entry else {
            return Vec::new();
        };
        let mut targets = BTreeSet::new();
        let mut ordered = Vec::new();
        for subscription in wanted {
            if subscription_matches(&subscription.options, session_id, &entry)
                && targets.insert(subscription.connection_id.clone())
            {
                ordered.push(subscription.connection_id.clone());
            }
        }
        ordered
    }

    /// Live subscribers of a session and whether any of their subscriptions asked for context.
    pub fn matching_connections(
        &mut self,
        source: &dyn SessionStatusSource,
        session_id: &str,
    ) -> BTreeMap<String, bool> {
        let mut matches = BTreeMap::new();
        let Some(view) = self.current_view(source, session_id) else {
            return matches;
        };
        for subscription in self
            .subscriptions
            .values()
            .filter(|subscription| subscription.held.is_none())
        {
            if subscription_matches(&subscription.options, session_id, &view.entry) {
                *matches
                    .entry(subscription.connection_id.clone())
                    .or_insert(false) |= subscription.options.include_context;
            }
        }
        matches
    }

    /// The live view, falling back to the last one seen once the registry dropped a sealed entry.
    fn current_view(
        &mut self,
        source: &dyn SessionStatusSource,
        session_id: &str,
    ) -> Option<SessionView> {
        let live = source.view(session_id);
        if let Some(live) = &live {
            self.last_views.insert(session_id.to_owned(), live.clone());
        }
        live.or_else(|| self.last_views.get(session_id).cloned())
    }

    fn last_change_of(&self, session_id: &str) -> Value {
        self.last_changes
            .get(session_id)
            .cloned()
            .unwrap_or_else(|| json!({"at":(self.now)(),"cause":"open","by":"host"}))
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "senpi recordFor's inputs, one per record field group"
    )]
    fn record(
        &self,
        id: &str,
        seq: u64,
        options: &SubscriptionOptions,
        session_id: &str,
        view: &SessionView,
        fanout_questions: usize,
        cause: Option<SessionStatusCause>,
        change: Value,
    ) -> Value {
        let mut derived = derive_session_status(
            &view.entry,
            view.runtime.as_ref(),
            fanout_questions,
            self.terminals.get(session_id),
            cause,
        );
        let context = derived.remove("context");
        let mut record = serde_json::Map::new();
        record.insert("type".into(), "session_status".into());
        record.insert("subscriptionId".into(), id.into());
        record.insert("seq".into(), seq.into());
        record.insert("sessionId".into(), session_id.into());
        record.extend(derived);
        if options.include_context
            && let Some(context) = context
        {
            record.insert("context".into(), context);
        }
        if let Some(last_effect) = self
            .last_effect
            .as_ref()
            .and_then(|provider| provider(session_id))
        {
            record.insert("lastEffect".into(), last_effect);
        }
        record.insert("lastChange".into(), change);
        Value::Object(record)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_registry::RpcSessionState;
    use crate::session_status::SessionStatusEntryView;
    use crate::session_status_subscriptions::parse_subscription;
    use std::sync::{Arc, Mutex};

    struct OneSession;
    impl SessionStatusSource for OneSession {
        fn session_ids(&self) -> Vec<String> {
            vec!["s1".into()]
        }
        fn view(&self, session_id: &str) -> Option<SessionView> {
            (session_id == "s1").then(|| SessionView {
                entry: SessionStatusEntryView {
                    state: RpcSessionState::Open,
                    kind: "interactive",
                    cwd: "/w".into(),
                    attachments: 1,
                    durable_session_id: None,
                    session_path: None,
                    name: None,
                    context: serde_json::Map::new(),
                    open_context: None,
                },
                runtime: Some(Default::default()),
            })
        }
    }

    #[derive(Default)]
    struct Sent(Mutex<Vec<(String, String)>>);
    impl SessionStatusDelivery for Sent {
        fn enqueue(&self, connection_id: &str, line: &str) -> bool {
            self.0
                .lock()
                .unwrap()
                .push((connection_id.to_owned(), line.to_owned()));
            true
        }
        fn pending_questions(&self, _session_id: &str) -> usize {
            0
        }
    }

    fn stream() -> (SessionStatusStream, Arc<Sent>) {
        let stream = SessionStatusStream::new(Box::new(|| "2026-10-27T00:00:00.000Z".into()));
        (stream, Arc::default())
    }

    #[test]
    fn deltas_published_before_activation_are_held_behind_the_snapshot() {
        let (mut stream, sent) = stream();
        let subscribed = stream.subscribe(
            &OneSession,
            &*sent,
            "c1",
            parse_subscription(&json!({})).unwrap(),
        );
        assert_eq!(subscribed.snapshot[0]["seq"], 0);
        stream.publish(&OneSession, &*sent, "s1", "prompt", "client", false);
        assert!(sent.0.lock().unwrap().is_empty());
        stream.activate(&*sent, &subscribed.subscription_id);
        stream.publish(&OneSession, &*sent, "s1", "agent_end", "agent", false);
        let seqs = sent
            .0
            .lock()
            .unwrap()
            .iter()
            .map(|(_, line)| {
                serde_json::from_str::<Value>(line).unwrap()["seq"]
                    .as_u64()
                    .unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(seqs, [1, 2]);
        assert!(stream.unsubscribe("c1", &subscribed.subscription_id));
        assert!(!stream.unsubscribe("c1", &subscribed.subscription_id));
    }

    /// rpc-session-status.test.ts effect subscriptions: an `effects: true` subscription gets `effect_status` on its
    /// own seq, held behind the snapshot until activation; a plain one gets none; `lastEffect` rides every record.
    #[test]
    fn effect_transitions_reach_only_effect_subscriptions_on_their_seq() {
        let (mut stream, sent) = stream();
        stream.set_last_effect_provider(Some(Box::new(|session_id: &str| {
            (session_id == "s1").then(|| json!({"effectId":"e1","kind":"input","state":"accepted"}))
        })));
        let effects = stream.subscribe(
            &OneSession,
            &*sent,
            "effects",
            parse_subscription(&json!({"effects":true})).unwrap(),
        );
        assert_eq!(effects.snapshot[0]["lastEffect"]["effectId"], "e1");
        let plain = stream.subscribe(
            &OneSession,
            &*sent,
            "plain",
            parse_subscription(&json!({})).unwrap(),
        );
        let status: crate::effect_record::EffectStatus = serde_json::from_value(json!({
            "effectId":"e1","kind":"input","state":"done","updatedAt":"2026-10-27T00:00:01.000Z",
            "durableSessionId":"d"
        }))
        .unwrap();
        stream.publish_effect(&OneSession, &*sent, "s1", &status);
        assert!(sent.0.lock().unwrap().is_empty());
        stream.activate(&*sent, &effects.subscription_id);
        stream.activate(&*sent, &plain.subscription_id);
        stream.publish(&OneSession, &*sent, "s1", "agent_end", "agent", false);
        let lines = sent.0.lock().unwrap().clone();
        let records = lines
            .iter()
            .map(|(connection, line)| {
                let record = serde_json::from_str::<Value>(line).unwrap();
                (
                    connection.clone(),
                    record["type"].as_str().unwrap().to_owned(),
                    record["seq"].as_u64().unwrap(),
                    record["at"].clone(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            records,
            [
                (
                    "effects".to_owned(),
                    "effect_status".to_owned(),
                    1,
                    json!("2026-10-27T00:00:01.000Z")
                ),
                (
                    "effects".to_owned(),
                    "session_status".to_owned(),
                    2,
                    Value::Null
                ),
                (
                    "plain".to_owned(),
                    "session_status".to_owned(),
                    1,
                    Value::Null
                ),
            ]
        );
    }

    /// dialog-observer.test.ts: dialog records reach only `dialogs: true` subscriptions with a mirror; a snapshot
    /// marker reaches any subscription that mirrors anything.
    #[test]
    fn dialog_and_marker_targets_follow_the_subscription_opt_ins() {
        let (mut stream, sent) = stream();
        let source = OneSession;
        let sent = &*sent;
        stream.subscribe(
            &source,
            sent,
            "observer",
            parse_subscription(&json!({"mirror":"all","dialogs":true})).unwrap(),
        );
        stream.subscribe(
            &source,
            sent,
            "mirror",
            parse_subscription(&json!({"mirror":["message_update"]})).unwrap(),
        );
        stream.subscribe(
            &source,
            sent,
            "plain",
            parse_subscription(&json!({"dialogs":true})).unwrap(),
        );
        assert_eq!(stream.dialog_targets(&source, "s1"), ["observer"]);
        assert_eq!(
            stream.mirror_targets(&source, "s1", "snapshot_marker"),
            ["observer", "mirror"]
        );
        assert_eq!(
            stream.mirror_targets(&source, "s1", "message_update"),
            ["observer", "mirror"]
        );
        assert_eq!(
            stream.mirror_targets(&source, "s1", "agent_end"),
            ["observer"]
        );
        assert!(stream.dialog_targets(&source, "other").is_empty());
        stream.drop_connection("observer");
        assert!(stream.dialog_targets(&source, "s1").is_empty());
    }
}
