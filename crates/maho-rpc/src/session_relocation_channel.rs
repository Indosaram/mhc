//! Where a session's relocation requests go (senpi `modes/rpc/session-relocation-channel.ts`, docs/rpc.md
//! "Continuing a session on another computer"): to ONE connection, the one attached to the session that advertised
//! `session_relocation`, and only that connection's answer counts.
pub type SendCallback = Arc<dyn Fn(&str, Value, RelocationRecordKind) + Send + Sync>;
use crate::custom_capability::SESSION_RELOCATION_CAPABILITY;
use serde_json::{Map, Value};
use std::sync::Arc;

/// The connection a classic single-client handler reports for itself (senpi `SOLE_CONNECTION`).
pub const SOLE_CONNECTION: &str = "sole";

/// Whether a record keeps its order behind the session's buffered events (`event`) or answers the caller
/// (`request`): the two lanes a shared host delivers through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelocationRecordKind {
    Request,
    Event,
}

/// A source of connection ids: the connection a frame came from, or the one offering relocation.
pub type ConnectionSource = Arc<dyn Fn() -> Option<String> + Send + Sync>;

/// `deliverTo(connectionId, deliver)`: runs `deliver` on that connection's own lane.
pub type ConnectionDelivery = Arc<dyn Fn(&str, &(dyn Fn() + Send + Sync)) + Send + Sync>;

/// One output lane of a handler: the request lane answers the caller, the event lane keeps buffered order.
pub type OutputLane = Arc<dyn Fn(Value) + Send + Sync>;

/// The session's own capabilities, or `None` before it declared any.
pub type CapabilitySource = Arc<dyn Fn() -> Option<Vec<String>> + Send + Sync>;

/// What a shared host lends a session binding; absent on a classic single-connection handler.
#[derive(Clone)]
pub struct RelocationClientInfo {
    /// `connectionId()`: the connection the frame being handled came from.
    pub connection_id: ConnectionSource,
    /// `relocationConnection()`: the connection that offers relocation for this session right now.
    pub relocation_connection: Option<ConnectionSource>,
    pub deliver_to: Option<ConnectionDelivery>,
}

/// The one destination a session's relocation traffic has.
#[derive(Clone)]
pub struct RelocationChannel {
    offerer: ConnectionSource,
    current: ConnectionSource,
    send: SendCallback,
}

impl RelocationChannel {
    /// The connection that offers relocation for this session right now, or `None`.
    pub fn offerer(&self) -> Option<String> {
        (self.offerer)()
    }

    /// The connection the frame being handled came from.
    pub fn current(&self) -> Option<String> {
        (self.current)()
    }

    /// Delivers a record to `connection` only. `kind` keeps it ordered behind the session's buffered events.
    pub fn send(&self, connection: &str, record: Value, kind: RelocationRecordKind) {
        (self.send)(connection, record, kind)
    }
}

/// What a handler lends the channel: its own capabilities, its host's connection view, and its two output lanes.
pub struct RelocationChannelOptions {
    pub capabilities: CapabilitySource,
    pub client_info: Option<RelocationClientInfo>,
    pub output: OutputLane,
    pub output_event: OutputLane,
}

/// A classic handler has one client, so its own capabilities decide. A shared host names the offering connection from
/// its per-connection capabilities; a binding the host cannot ask (a worker isolate) offers nothing, which is also
/// right because `release_session` cannot release such a session.
pub fn relocation_channel(options: RelocationChannelOptions) -> RelocationChannel {
    let RelocationChannelOptions {
        capabilities,
        client_info,
        output,
        output_event,
    } = options;
    let Some(client_info) = client_info else {
        let offered = Arc::clone(&capabilities);
        let request = Arc::clone(&output);
        let event = Arc::clone(&output_event);
        return RelocationChannel {
            offerer: Arc::new(move || {
                offered()
                    .is_some_and(|capabilities| {
                        capabilities
                            .iter()
                            .any(|capability| capability == SESSION_RELOCATION_CAPABILITY)
                    })
                    .then(|| SOLE_CONNECTION.to_owned())
            }),
            current: Arc::new(|| Some(SOLE_CONNECTION.to_owned())),
            send: Arc::new(move |_connection, record, kind| match kind {
                RelocationRecordKind::Event => event(record),
                RelocationRecordKind::Request => request(record),
            }),
        };
    };
    // A shared host answers from its per-connection capabilities, so the handler's own list and event lane are
    // unused here: every record goes through `deliverTo`, which owns the ordering per connection.
    drop((capabilities, output_event));
    let deliver_to = client_info.deliver_to.clone();
    let relocation_connection = client_info.relocation_connection.clone();
    let connection_id = Arc::clone(&client_info.connection_id);
    let output = Arc::clone(&output);
    let can_deliver = deliver_to.is_some();
    RelocationChannel {
        offerer: Arc::new(move || match can_deliver {
            false => None,
            true => relocation_connection.as_ref().and_then(|offer| offer()),
        }),
        current: Arc::new(move || connection_id()),
        send: Arc::new(move |connection, record, _kind| {
            if let Some(deliver_to) = &deliver_to {
                let output = Arc::clone(&output);
                deliver_to(connection, &move || output(record.clone()));
            }
        }),
    }
}

/// What a `host_response` did to the request it names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostResponseOutcome {
    /// The request was pending and its owner answered it.
    Answered,
    /// No request is pending under that id.
    Unknown,
    /// The frame came from a connection other than the one that was asked.
    NotOfferingConnection,
}

impl HostResponseOutcome {
    /// The wire `error` code of an answer that did not settle a request.
    pub fn error_code(self) -> &'static str {
        match self {
            Self::Answered => "answered",
            Self::Unknown => "unknown_host_request",
            Self::NotOfferingConnection => "not_offering_connection",
        }
    }
}

/// The `response` an inbound `host_response` is answered with (senpi `answerHostResponse`): `respond` settles the
/// pending request, and an id that never named one is refused here rather than by the session.
pub fn answer_host_response(
    frame: &Value,
    respond: Option<&mut dyn FnMut(&Value) -> HostResponseOutcome>,
) -> Value {
    let mut reply = Map::new();
    if let Some(id) = frame.get("id") {
        reply.insert("id".into(), id.clone());
    }
    reply.insert("type".into(), Value::from("response"));
    reply.insert("command".into(), Value::from("host_response"));
    if frame
        .get("id")
        .and_then(Value::as_str).is_none_or(|id| id.is_empty())
    {
        reply.insert("success".into(), Value::Bool(false));
        reply.insert("error".into(), Value::from("invalid_host_response"));
        return Value::Object(reply);
    }
    let outcome = match respond {
        Some(respond) => respond(frame),
        None => HostResponseOutcome::Unknown,
    };
    match outcome {
        HostResponseOutcome::Answered => {
            reply.insert("success".into(), Value::Bool(true));
        }
        outcome => {
            reply.insert("success".into(), Value::Bool(false));
            reply.insert("error".into(), Value::from(outcome.error_code()));
        }
    }
    Value::Object(reply)
}

/// Whether an inbound record is a `host_response` (the router's inbound dispatch asks this first).
pub fn is_host_response(value: &Value) -> bool {
    value
        .as_object()
        .is_some_and(|fields| fields.get("type").and_then(Value::as_str) == Some("host_response"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Sent {
        requests: Vec<Value>,
        events: Vec<Value>,
        delivered: Vec<(String, Value)>,
    }

    fn capturing() -> (Arc<Mutex<Sent>>, OutputLane, OutputLane) {
        let sent = Arc::new(Mutex::new(Sent::default()));
        let requests = Arc::clone(&sent);
        let output: OutputLane = Arc::new(move |record| {
            requests.lock().unwrap().requests.push(record);
        });
        let events = Arc::clone(&sent);
        let output_event: OutputLane = Arc::new(move |record| {
            events.lock().unwrap().events.push(record);
        });
        (sent, output, output_event)
    }

    #[test]
    fn a_classic_handler_offers_relocation_only_when_its_own_client_does() {
        let (_sent, output, output_event) = capturing();
        let offered = Arc::new(Mutex::new(Some(vec![SESSION_RELOCATION_CAPABILITY.to_owned()])));
        let capabilities = Arc::clone(&offered);
        let channel = relocation_channel(RelocationChannelOptions {
            capabilities: Arc::new(move || capabilities.lock().unwrap().clone()),
            client_info: None,
            output,
            output_event,
        });
        assert_eq!(channel.offerer().as_deref(), Some(SOLE_CONNECTION));
        assert_eq!(channel.current().as_deref(), Some(SOLE_CONNECTION));
        *offered.lock().unwrap() = Some(vec!["question".into()]);
        assert_eq!(channel.offerer(), None);
        *offered.lock().unwrap() = None;
        assert_eq!(channel.offerer(), None);
    }

    #[test]
    fn a_classic_handler_sends_requests_and_events_on_their_own_lanes() {
        let (sent, output, output_event) = capturing();
        let channel = relocation_channel(RelocationChannelOptions {
            capabilities: Arc::new(|| Some(Vec::<String>::new())),
            client_info: None,
            output,
            output_event,
        });
        channel.send(
            SOLE_CONNECTION,
            json!({"type":"host_request","id":"1"}),
            RelocationRecordKind::Request,
        );
        channel.send(
            SOLE_CONNECTION,
            json!({"type":"relocation_due","moveId":"m"}),
            RelocationRecordKind::Event,
        );
        let sent = sent.lock().unwrap();
        assert_eq!(sent.requests, vec![json!({"type":"host_request","id":"1"})]);
        assert_eq!(sent.events, vec![json!({"type":"relocation_due","moveId":"m"})]);
    }

    #[test]
    fn a_shared_host_names_the_offering_connection_and_delivers_only_to_it() {
        let (sent, output, output_event) = capturing();
        let offerer = Arc::new(Mutex::new(Some("peer-b".to_owned())));
        let current = Arc::new(Mutex::new(Some("peer-a".to_owned())));
        let deliver_to = {
            let sent = Arc::clone(&sent);
            Arc::new(move |connection: &str, deliver: &(dyn Fn() + Send + Sync)| {
                sent.lock().unwrap().delivered.push((connection.to_owned(), json!({})));
                deliver();
            }) as ConnectionDelivery
        };
        let offerer_source = Arc::clone(&offerer);
        let current_source = Arc::clone(&current);
        let channel = relocation_channel(RelocationChannelOptions {
            capabilities: Arc::new(|| -> Option<Vec<String>> {
                Some(vec![SESSION_RELOCATION_CAPABILITY.to_owned()])
            }),
            client_info: Some(RelocationClientInfo {
                connection_id: Arc::new(move || current_source.lock().unwrap().clone()),
                relocation_connection: Some(Arc::new(move || offerer_source.lock().unwrap().clone())),
                deliver_to: Some(deliver_to),
            }),
            output,
            output_event,
        });
        // The shared host's per-connection capabilities decide, never the handler's own list.
        assert_eq!(channel.offerer().as_deref(), Some("peer-b"));
        assert_eq!(channel.current().as_deref(), Some("peer-a"));
        channel.send(
            "peer-b",
            json!({"type":"host_request","id":"2"}),
            RelocationRecordKind::Event,
        );
        let sent = sent.lock().unwrap();
        assert_eq!(sent.delivered.len(), 1);
        assert_eq!(sent.delivered[0].0, "peer-b");
        // A shared host delivers events through `deliverTo`, never the untargeted event lane.
        assert_eq!(sent.requests, vec![json!({"type":"host_request","id":"2"})]);
        assert!(sent.events.is_empty());
    }

    #[test]
    fn a_binding_the_host_cannot_ask_offers_nothing() {
        let (_sent, output, output_event) = capturing();
        let channel = relocation_channel(RelocationChannelOptions {
            capabilities: Arc::new(|| -> Option<Vec<String>> {
                Some(vec![SESSION_RELOCATION_CAPABILITY.to_owned()])
            }),
            client_info: Some(RelocationClientInfo {
                connection_id: Arc::new(|| None),
                relocation_connection: None,
                deliver_to: None,
            }),
            output,
            output_event,
        });
        assert_eq!(channel.offerer(), None);
        assert_eq!(channel.current(), None);
        // Nothing can be delivered either: the host has no lane for this binding.
        channel.send("peer", json!({"type":"host_request","id":"3"}), RelocationRecordKind::Request);
    }

    #[test]
    fn a_host_response_without_a_usable_id_is_refused_locally() {
        for frame in [
            json!({"type":"host_response"}),
            json!({"type":"host_response","id":""}),
            json!({"type":"host_response","id":7}),
        ] {
            let mut respond = |_: &Value| HostResponseOutcome::Answered;
            let reply = answer_host_response(&frame, Some(&mut respond));
            assert_eq!(reply["command"], json!("host_response"));
            assert_eq!(reply["success"], json!(false));
            assert_eq!(reply["error"], json!("invalid_host_response"));
        }
    }

    #[test]
    fn a_host_response_answers_with_what_its_request_did() {
        let frame = json!({"type":"host_response","id":"hr_1","data":{"accepted":true}});
        let mut answered = |_: &Value| HostResponseOutcome::Answered;
        let reply = answer_host_response(&frame, Some(&mut answered));
        assert_eq!(
            reply,
            json!({"id":"hr_1","type":"response","command":"host_response","success":true})
        );
        let mut unknown = |_: &Value| HostResponseOutcome::Unknown;
        assert_eq!(
            answer_host_response(&frame, Some(&mut unknown))["error"],
            json!("unknown_host_request")
        );
        let mut wrong = |_: &Value| HostResponseOutcome::NotOfferingConnection;
        assert_eq!(
            answer_host_response(&frame, Some(&mut wrong))["error"],
            json!("not_offering_connection")
        );
        // No session-side handler: nothing is pending anywhere.
        assert_eq!(
            answer_host_response(&frame, None)["error"],
            json!("unknown_host_request")
        );
    }

    #[test]
    fn only_a_host_response_record_is_one() {
        assert!(is_host_response(&json!({"type":"host_response","id":"1"})));
        assert!(!is_host_response(&json!({"type":"response"})));
        assert!(!is_host_response(&json!("host_response")));
    }
}
