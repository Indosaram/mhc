//! A slow `event_resync` peer is resynchronised instead of cut (senpi `socket-peer-recovery.ts`).
use serde_json::{Value, json};

pub const EVENT_RESYNC_CAPABILITY: &str = "event_resync";
const RPC_PING_LINE: &str = "{\"type\":\"rpc_ping\"}\n";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResyncReason {
    Overflow,
    Stall,
}
impl ResyncReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Overflow => "overflow",
            Self::Stall => "stall",
        }
    }
}

/// `RpcSessionResyncRequired`: the per-session gap marker a client receives after a shed.
#[derive(Clone, Debug, PartialEq)]
pub struct RpcSessionResyncRequired {
    pub session_id: String,
    pub reason: ResyncReason,
    pub message: Value,
    pub closed: bool,
}
impl RpcSessionResyncRequired {
    pub fn to_value(&self) -> Value {
        let mut value = json!({"type":"resync_required","sessionId":self.session_id,"reason":self.reason.as_str(),"message":self.message});
        if self.closed {
            value["closed"] = true.into();
        }
        value
    }
}

/// A `response` record carrying a string id: the only record a shed must not drop.
pub fn is_correlated_reply(value: &Value) -> bool {
    value.get("type").and_then(Value::as_str) == Some("response")
        && value.get("id").is_some_and(Value::is_string)
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("socket peer missed its liveness ping")]
pub struct SocketPeerLivenessError;
impl SocketPeerLivenessError {
    pub const NAME: &'static str = "SocketPeerLivenessError";
}

pub type SnapshotReader = Box<dyn Fn() -> Vec<Value> + Send>;
pub type SnapshotFactory = Box<dyn Fn(ResyncReason) -> SnapshotReader + Send>;

/// One gap and one outstanding ping, regardless of how many events the producer emits.
pub struct SocketPeerRecovery {
    reason: Option<ResyncReason>,
    awaiting_ping: bool,
    ping_buffered: bool,
    reply_lost: bool,
    write: Box<dyn FnMut(String) + Send>,
    snapshot: SnapshotFactory,
    read_snapshot: Option<SnapshotReader>,
}
impl SocketPeerRecovery {
    /// Negotiates the heartbeat before session traffic can fill the output side.
    pub fn new(mut write: Box<dyn FnMut(String) + Send>, snapshot: SnapshotFactory) -> Self {
        write(RPC_PING_LINE.to_owned());
        Self {
            reason: None,
            awaiting_ping: false,
            ping_buffered: true,
            reply_lost: false,
            write,
            snapshot,
            read_snapshot: None,
        }
    }
    pub fn pending(&self) -> bool {
        self.reason.is_some()
    }
    pub fn shed(&mut self, reason: ResyncReason) {
        if self.reason.is_some() {
            return;
        }
        self.reason = Some(reason);
        self.read_snapshot = Some((self.snapshot)(reason));
    }
    /// A fresh `set_client_info` is the connection-scoped acknowledgement.
    pub fn acknowledge(&mut self) {
        self.awaiting_ping = false;
    }
    pub fn lose_reply(&mut self) {
        self.reply_lost = true;
    }
    pub fn ping(&mut self) -> Result<(), SocketPeerLivenessError> {
        if self.awaiting_ping {
            return Err(SocketPeerLivenessError);
        }
        self.awaiting_ping = true;
        if !self.ping_buffered {
            self.ping_buffered = true;
            (self.write)(RPC_PING_LINE.to_owned());
        }
        Ok(())
    }
    pub fn take_snapshot(&mut self) -> Vec<Value> {
        let reason = self.reason.take();
        self.awaiting_ping = false;
        self.ping_buffered = false;
        let snapshot = self.read_snapshot.take();
        let mut records = Vec::new();
        if let Some(reason) = reason {
            records.push(json!({"type":"resync_required","reason":reason.as_str(),"replyLost":self.reply_lost}));
        }
        self.reply_lost = false;
        if let Some(read) = snapshot {
            records.extend(read());
        }
        records
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    fn recovery() -> (SocketPeerRecovery, Arc<Mutex<Vec<String>>>) {
        let written = Arc::new(Mutex::new(Vec::new()));
        let sink = written.clone();
        let recovery = SocketPeerRecovery::new(
            Box::new(move |line| {
                sink.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(line)
            }),
            Box::new(|_| Box::new(|| vec![json!({"type":"snapshot"})])),
        );
        (recovery, written)
    }
    #[test]
    fn construction_pings_and_second_unacknowledged_ping_is_a_liveness_failure() {
        let (mut recovery, written) = recovery();
        assert_eq!(
            written
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_slice(),
            [RPC_PING_LINE]
        );
        assert_eq!(recovery.ping(), Ok(()));
        assert_eq!(recovery.ping(), Err(SocketPeerLivenessError));
        recovery.acknowledge();
        assert_eq!(recovery.ping(), Ok(()));
        assert_eq!(
            SocketPeerLivenessError.to_string(),
            "socket peer missed its liveness ping"
        );
    }
    #[test]
    fn shed_records_one_gap_and_snapshot_resets_state() {
        let (mut recovery, written) = recovery();
        recovery.shed(ResyncReason::Overflow);
        recovery.shed(ResyncReason::Stall);
        recovery.lose_reply();
        assert!(recovery.pending());
        let records = recovery.take_snapshot();
        assert_eq!(
            records,
            vec![
                json!({"type":"resync_required","reason":"overflow","replyLost":true}),
                json!({"type":"snapshot"})
            ]
        );
        assert!(!recovery.pending());
        assert_eq!(recovery.take_snapshot(), Vec::<Value>::new());
        assert_eq!(recovery.ping(), Ok(()));
        assert_eq!(
            written
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .len(),
            2
        );
    }
    #[test]
    fn correlated_replies_need_a_string_id() {
        assert!(is_correlated_reply(&json!({"type":"response","id":"1"})));
        assert!(!is_correlated_reply(&json!({"type":"response","id":1})));
        assert!(!is_correlated_reply(&json!({"type":"event","id":"1"})));
    }
}
