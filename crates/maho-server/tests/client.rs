use maho_server::{
    client::{
        Client,
        errors::ClientError,
        service_wire::ServiceStateDecoder,
        transport::{ByteTransport, ByteTransportFactory, ByteTransportHandlers, TransportFuture},
        types::ConnectionState,
    },
    protocol::{
        codec::{MessageDecoder, encode_server_message},
        framing::DEFAULT_MAX_FRAME_LENGTH as MAX,
    },
};
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use tokio::sync::mpsc;

const ID: &str = "00000000-0000-4000-8000-000000000001";
#[tokio::test]
async fn synchronous_listeners_preserve_every_change_and_unsubscribe() {
    let (memory, _requests) =
        Memory::create(json!({"type":"hello","version":8,"serverId":ID}), false);
    let client = Client::new(ID.into(), MAX, Arc::new(MemoryFactory(memory.clone()))).unwrap();
    let changes = Arc::new(Mutex::new(Vec::new()));
    let captured = changes.clone();
    let subscription = client
        .on_connection_state_change(Arc::new(move |change| {
            captured.lock().unwrap().push(change.state);
        }))
        .unwrap();
    client.connect().await.unwrap();
    assert_eq!(
        *changes.lock().unwrap(),
        vec![ConnectionState::Connecting, ConnectionState::Connected]
    );
    drop(subscription);
    client.disconnect("done");
    assert_eq!(changes.lock().unwrap().len(), 2);
    let attachments = Arc::new(Mutex::new(Vec::new()));
    let captured = attachments.clone();
    let _subscription = client
        .on_attachment_change(Arc::new(move |attachment| {
            captured.lock().unwrap().push(attachment);
        }))
        .unwrap();
    client.connect().await.unwrap();
    memory.send(json!({"type":"attachment","attachment":{"serverId":ID,"sessionId":"s","attachmentId":"a"}}));
    memory.send(json!({"type":"attachment","attachment":null}));
    assert_eq!(attachments.lock().unwrap().len(), 2);
    client.dispose();
    assert!(client.on_attachment_change(Arc::new(|_| {})).is_err());
}
struct Memory {
    handlers: Mutex<Option<ByteTransportHandlers>>,
    outbound: mpsc::UnboundedSender<Value>,
    closed: AtomicUsize,
    hello: Value,
    early: bool,
}
impl Memory {
    fn create(hello: Value, early: bool) -> (Arc<Self>, mpsc::UnboundedReceiver<Value>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (
            Arc::new(Self {
                handlers: Mutex::new(None),
                outbound: tx,
                closed: AtomicUsize::new(0),
                hello,
                early,
            }),
            rx,
        )
    }
    fn send(&self, message: Value) {
        let frame = encode_server_message(&message, MAX).expect("valid server fixture");
        let handlers = self.handlers.lock().expect("memory lock");
        (handlers.as_ref().expect("connected handlers").on_data)(&frame);
    }
    fn transport_close(&self) {
        let handlers = self.handlers.lock().expect("memory lock");
        (handlers.as_ref().expect("connected handlers").on_close)();
    }
}
struct MemoryFactory(Arc<Memory>);
struct MemoryTransport(Arc<Memory>);
impl ByteTransportFactory for MemoryFactory {
    fn connect(
        &self,
        handlers: ByteTransportHandlers,
    ) -> TransportFuture<'_, Arc<dyn ByteTransport>> {
        Box::pin(async move {
            *self.0.handlers.lock().expect("memory lock") = Some(handlers);
            if self.0.early {
                self.0.send(self.0.hello.clone());
            }
            Ok(Arc::new(MemoryTransport(self.0.clone())) as Arc<dyn ByteTransport>)
        })
    }
}
impl ByteTransport for MemoryTransport {
    fn send<'a>(&'a self, bytes: &'a [u8]) -> TransportFuture<'a, ()> {
        Box::pin(async move {
            let messages = MessageDecoder::client().push(bytes)?;
            for message in messages {
                self.0
                    .outbound
                    .send(message.clone())
                    .map_err(|e| ClientError::Disconnected(e.to_string()))?;
                if message["type"] == "hello" {
                    self.0.send(self.0.hello.clone());
                }
            }
            Ok(())
        })
    }
    fn close(&self) {
        self.0.closed.fetch_add(1, Ordering::SeqCst);
    }
}
fn setup() -> (Arc<Client>, Arc<Memory>, mpsc::UnboundedReceiver<Value>) {
    let (memory, rx) = Memory::create(json!({"type":"hello","version":8,"serverId":ID}), false);
    let client =
        Client::new(ID.into(), MAX, Arc::new(MemoryFactory(memory.clone()))).expect("valid client");
    (client, memory, rx)
}

#[test]
fn requires_canonical_server_identity() {
    let (memory, _) = Memory::create(json!({}), false);
    assert!(
        Client::new(
            "invalid-server".into(),
            MAX,
            Arc::new(MemoryFactory(memory))
        )
        .is_err()
    );
}
#[tokio::test]
async fn connects_only_to_expected_server() {
    let (client, _memory, _requests) = setup();
    assert_eq!(client.connect().await.unwrap()["serverId"], ID);
    client.dispose();
    let (memory, _requests) = Memory::create(
        json!({"type":"hello","version":8,"serverId":"00000000-0000-4000-8000-000000000002"}),
        false,
    );
    let client = Client::new(ID.into(), MAX, Arc::new(MemoryFactory(memory.clone()))).unwrap();
    assert!(matches!(
        client.connect().await,
        Err(ClientError::Protocol(_))
    ));
    assert_eq!(memory.closed.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn receives_out_of_band_attachment_updates() {
    let (client, memory, _requests) = setup();
    client.connect().await.unwrap();
    let mut events = client.attachment_changes();
    memory.send(json!({"type":"attachment","attachment":{"serverId":ID,"sessionId":"s","attachmentId":"a"}}));
    events.changed().await.unwrap();
    assert_eq!(client.attachment().unwrap()["sessionId"], "s");
    memory.send(json!({"type":"attachment","attachment":null}));
    events.changed().await.unwrap();
    assert!(client.attachment().is_none());
    client.dispose();
}
#[tokio::test]
async fn correlates_out_of_order_responses() {
    let (client, memory, mut requests) = setup();
    client.connect().await.unwrap();
    requests.recv().await.unwrap();
    let first = client.request(
        json!({"serverId":ID}),
        json!({"serviceId":"test","member":"first","args":[]}),
        None,
    );
    let second = client.request(
        json!({"serverId":ID}),
        json!({"serviceId":"test","member":"second","args":[]}),
        None,
    );
    let server = async {
        let first = requests.recv().await.unwrap();
        let second = requests.recv().await.unwrap();
        memory.send(json!({"type":"response","id":second["id"],"ok":true,"result":"second"}));
        memory.send(json!({"type":"response","id":first["id"],"ok":true,"result":"first"}));
    };
    let (first, second, _) = tokio::join!(first, second, server);
    assert_eq!(first.unwrap(), Some(json!("first")));
    assert_eq!(second.unwrap(), Some(json!("second")));
    client.dispose();
}
#[tokio::test]
async fn exposes_server_error_codes() {
    let (client, memory, mut requests) = setup();
    client.connect().await.unwrap();
    requests.recv().await.unwrap();
    let pending = client.request(
        json!({"serverId":ID}),
        json!({"serviceId":"test","member":"missing","args":[]}),
        None,
    );
    let server = async {
        let request = requests.recv().await.unwrap();
        memory.send(json!({"type":"response","id":request["id"],"ok":false,"error":{"code":"session_not_found","message":"Unknown session"}}));
    };
    let (result, _) = tokio::join!(pending, server);
    assert!(matches!(result,Err(ClientError::Server {code,..}) if code=="session_not_found"));
    client.dispose();
}
#[tokio::test]
async fn does_not_send_pre_aborted_request() {
    let (client, _, mut requests) = setup();
    client.connect().await.unwrap();
    requests.recv().await.unwrap();
    let (_, cancel) =
        tokio::sync::watch::channel(Some(ClientError::Disconnected("already cancelled".into())));
    assert!(
        client
            .request(
                json!({"serverId":ID}),
                json!({"serviceId":"test","member":"noop","args":[]}),
                Some(cancel)
            )
            .await
            .is_err()
    );
    assert!(requests.try_recv().is_err());
    client.dispose();
}
#[tokio::test]
async fn cancels_request_without_disconnect() {
    let (client, memory, mut requests) = setup();
    client.connect().await.unwrap();
    requests.recv().await.unwrap();
    let (cancel, signal) = tokio::sync::watch::channel(None);
    let request = client.request(
        json!({"serverId":ID}),
        json!({"serviceId":"test","member":"mutate","args":[]}),
        Some(signal),
    );
    let server = async {
        let request = requests.recv().await.unwrap();
        cancel.send_replace(Some(ClientError::Disconnected("stop this request".into())));
        let cancellation = requests.recv().await.unwrap();
        assert_eq!(cancellation["type"], "cancel");
        memory.send(json!({"type":"response","id":request["id"],"ok":false,"error":{"code":"cancelled","message":"cancelled"}}));
    };
    let (result, _) = tokio::join!(request, server);
    assert!(result.is_err());
    assert!(client.connected());
    client.dispose();
}
#[tokio::test]
async fn rejects_pending_on_disconnect_and_disposal() {
    let (client, memory, mut requests) = setup();
    client.connect().await.unwrap();
    requests.recv().await.unwrap();
    let request = client.request(
        json!({"serverId":ID}),
        json!({"serviceId":"test","member":"pending","args":[]}),
        None,
    );
    let server = async {
        requests.recv().await.unwrap();
        memory.transport_close();
    };
    let (result, _) = tokio::join!(request, server);
    assert!(matches!(result, Err(ClientError::Disconnected(_))));
    client.dispose();
    assert_eq!(client.connect().await.unwrap_err(), ClientError::Disposed);
}
#[tokio::test]
async fn rejects_data_before_client_hello() {
    let (memory, mut requests) =
        Memory::create(json!({"type":"hello","version":8,"serverId":ID}), true);
    let client = Client::new(ID.into(), MAX, Arc::new(MemoryFactory(memory.clone()))).unwrap();
    assert_eq!(
        client.connect().await.unwrap_err().to_string(),
        "Received server data before the client hello was sent"
    );
    assert!(requests.try_recv().is_err());
    assert_eq!(memory.closed.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn rejects_typed_handshake_error() {
    let (memory, _requests) = Memory::create(
        json!({"type":"hello_error","error":{"code":"version","message":"Unsupported protocol version"}}),
        false,
    );
    let client = Client::new(ID.into(), MAX, Arc::new(MemoryFactory(memory.clone()))).unwrap();
    assert!(
        matches!(client.connect().await,Err(ClientError::Server { code,.. }) if code=="version")
    );
    assert_eq!(memory.closed.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn reconnects_through_fresh_transport() {
    let (client, memory, mut requests) = setup();
    client.connect().await.unwrap();
    requests.recv().await.unwrap();
    client.disconnect("Client disconnected");
    assert_eq!(client.connection_state(), ConnectionState::Disconnected);
    client.connect().await.unwrap();
    assert_eq!(requests.recv().await.unwrap()["type"], "hello");
    assert!(client.connected());
    assert_eq!(memory.closed.load(Ordering::SeqCst), 1);
    client.dispose();
}
#[tokio::test]
async fn reports_transport_failure() {
    let (client, memory, _requests) = setup();
    client.connect().await.unwrap();
    let handlers = memory.handlers.lock().unwrap();
    (handlers.as_ref().unwrap().on_error)(ClientError::Disconnected("read failed".into()));
    assert_eq!(client.connection_state(), ConnectionState::Disconnected);
}
#[tokio::test]
async fn disconnects_on_truncated_framing() {
    let (client, memory, _requests) = setup();
    client.connect().await.unwrap();
    let mut events = client.connection_changes();
    {
        let handlers = memory.handlers.lock().unwrap();
        (handlers.as_ref().unwrap().on_data)(&[0, 0, 0, 2, 1]);
    }
    memory.transport_close();
    events.changed().await.unwrap();
    assert!(
        matches!(&events.borrow().error,Some(ClientError::Protocol(message)) if message.contains("Truncated"))
    );
}
#[tokio::test]
async fn disconnects_on_unmatched_response() {
    let (client, memory, _requests) = setup();
    client.connect().await.unwrap();
    memory.send(json!({"type":"response","id":"unknown","ok":true,"result":[]}));
    assert_eq!(client.connection_state(), ConnectionState::Disconnected);
    assert_eq!(memory.closed.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn buffers_updates_until_snapshot_and_activation() {
    let (client, memory, mut requests) = setup();
    client.connect().await.unwrap();
    requests.recv().await.unwrap();
    let opening = client.subscribe(json!({"serverId":ID}), "pi.models", "singleton");
    let server = async {
        let request = requests.recv().await.unwrap();
        memory.send(json!({"type":"service_update","subscriptionId":"service-1","update":{"type":"state","member":"state","sequence":1,"ops":[["s",["revision"],1]]}}));
        memory.send(json!({"type":"response","id":request["id"],"ok":true,"result":{"serviceId":"pi.models","mode":"singleton","instances":[{"members":[{"name":"state","kind":"state","sequence":0,"ops":[["r",{"revision":0}]]}]}]}}));
    };
    let (subscription, _) = tokio::join!(opening, server);
    let mut subscription = subscription.unwrap();
    assert!(subscription.updates.try_recv().is_err());
    subscription.start();
    assert_eq!(
        subscription.updates.recv().await.unwrap()["ops"],
        json!([["s", ["revision"], 1]])
    );
    memory.send(json!({"type":"service_update","subscriptionId":"service-1","update":{"type":"state","member":"state","sequence":2,"ops":[["#",0,["revision"]],["s",0,2]]}}));
    assert_eq!(
        subscription.updates.recv().await.unwrap()["ops"],
        json!([["s", ["revision"], 2]])
    );
    let closing = subscription.dispose();
    let server = async {
        let request = requests.recv().await.unwrap();
        assert_eq!(request["call"]["member"], "unsubscribe");
        memory.send(json!({"type":"response","id":request["id"],"ok":true}));
    };
    let (result, _) = tokio::join!(closing, server);
    result.unwrap();
    client.dispose();
}
#[test]
fn service_decoder_rejects_unknown_path_ids() {
    let mut decoder = ServiceStateDecoder::default();
    decoder.snapshot(&json!({"serviceId":"s","mode":"singleton","instances":[{"members":[{"name":"state","kind":"state","sequence":0,"ops":[["r",{}]]}]}]})).unwrap();
    assert!(
        decoder
            .update(&json!({"type":"state","member":"state","sequence":1,"ops":[["s",99,1]]}))
            .is_err()
    );
}
