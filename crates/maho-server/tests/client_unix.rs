use maho_server::{
    client::{
        Client,
        errors::ClientError,
        unix::{UnixTransportFactory, discover_unix_servers},
    },
    protocol::{
        codec::{MessageDecoder, encode_server_message},
        framing::DEFAULT_MAX_FRAME_LENGTH as MAX,
    },
};
use serde_json::json;
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixListener,
};

const ID: &str = "00000000-0000-4000-8000-000000000001";

#[test]
fn rejects_invalid_transport_options() {
    assert!(UnixTransportFactory::new("".into(), None).is_err());
    assert!(UnixTransportFactory::new("/tmp/pi.sock".into(), Some(0)).is_err());
}
#[tokio::test]
async fn handshake_and_request_use_real_socket() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pi.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let client = Client::new(
        ID.into(),
        MAX,
        Arc::new(UnixTransportFactory::new(path.clone(), None).unwrap()),
    )
    .unwrap();
    let server = async {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut decoder = MessageDecoder::client();
        let mut buffer = [0; 4096];
        loop {
            let n = stream.read(&mut buffer).await.unwrap();
            if n == 0 {
                break;
            }
            for message in decoder.push(&buffer[..n]).unwrap() {
                let response = if message["type"] == "hello" {
                    json!({"type":"hello","version":8,"serverId":ID})
                } else {
                    assert_eq!(message["call"]["member"], "list");
                    json!({"type":"response","id":message["id"],"ok":true,"result":[]})
                };
                let frame = encode_server_message(&response, MAX).unwrap();
                for byte in frame {
                    stream.write_all(&[byte]).await.unwrap();
                }
            }
        }
    };
    let run = async {
        client.connect().await.unwrap();
        assert_eq!(
            client
                .request(
                    json!({"serverId":ID}),
                    json!({"serviceId":"test.server","member":"list","args":[]}),
                    None
                )
                .await
                .unwrap(),
            Some(json!([]))
        );
        client.dispose();
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(run, server);
    })
    .await
    .unwrap();
    drop(client);
    drop(listener);
    tokio::fs::remove_file(&path).await.unwrap();
    assert!(!path.exists());
}
#[tokio::test]
async fn reports_truncated_final_frame() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pi.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let client = Client::new(
        ID.into(),
        MAX,
        Arc::new(UnixTransportFactory::new(path.clone(), None).unwrap()),
    )
    .unwrap();
    let server = async {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut decoder = MessageDecoder::client();
        let mut buffer = [0; 4096];
        loop {
            let n = stream.read(&mut buffer).await.unwrap();
            if n == 0 {
                break;
            }
            for message in decoder.push(&buffer[..n]).unwrap() {
                if message["type"] == "hello" {
                    stream
                        .write_all(
                            &encode_server_message(
                                &json!({"type":"hello","version":8,"serverId":ID}),
                                MAX,
                            )
                            .unwrap(),
                        )
                        .await
                        .unwrap();
                } else {
                    stream.write_all(&[0, 0, 0, 2, 1]).await.unwrap();
                    stream.shutdown().await.unwrap();
                    return;
                }
            }
        }
    };
    let run = async {
        client.connect().await.unwrap();
        assert!(
            matches!(client.request(json!({"serverId":ID}),json!({"serviceId":"test","member":"list","args":[]}),None).await,Err(ClientError::Protocol(message)) if message.contains("Truncated"))
        );
        client.dispose();
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(run, server);
    })
    .await
    .unwrap();
    drop(client);
    drop(listener);
    tokio::fs::remove_file(path).await.unwrap();
}
#[tokio::test]
async fn rejects_missing_socket() {
    let dir = tempfile::tempdir().unwrap();
    let client = Client::new(
        ID.into(),
        MAX,
        Arc::new(UnixTransportFactory::new(dir.path().join("absent.sock"), None).unwrap()),
    )
    .unwrap();
    assert!(matches!(
        client.connect().await,
        Err(ClientError::Io { kind:std::io::ErrorKind::NotFound, .. })
    ));
    client.dispose();
}
#[tokio::test]
async fn discovery_returns_empty_for_missing_directory() {
    let dir = tempfile::tempdir().unwrap();
    assert!(
        discover_unix_servers(&dir.path().join("missing"), Duration::from_secs(1))
            .await
            .unwrap()
            .is_empty()
    );
}
#[tokio::test]
async fn discovery_ignores_files_and_malformed_names() {
    let dir = tempfile::tempdir().unwrap();
    tokio::fs::write(dir.path().join(format!("{ID}.sock")), "not a socket")
        .await
        .unwrap();
    tokio::fs::write(dir.path().join("not-a-server.sock"), "ignored")
        .await
        .unwrap();
    assert!(
        discover_unix_servers(dir.path(), Duration::from_secs(1))
            .await
            .unwrap()
            .is_empty()
    );
}
#[tokio::test]
async fn discovery_preserves_stale_socket() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(format!("{ID}.sock"));
    let listener = UnixListener::bind(&path).unwrap();
    drop(listener);
    assert!(
        discover_unix_servers(dir.path(), Duration::from_secs(1))
            .await
            .unwrap()
            .is_empty()
    );
    assert!(path.exists());
    tokio::fs::remove_file(path).await.unwrap();
}
#[tokio::test]
async fn discovery_propagates_filesystem_errors() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("file");
    tokio::fs::write(&path, "content").await.unwrap();
    assert!(matches!(
        discover_unix_servers(&path, Duration::from_secs(1))
            .await,
        Err(ClientError::Io { kind:std::io::ErrorKind::NotADirectory, .. })
    ));
}

#[tokio::test]
async fn discovers_reachable_servers_in_identity_order() {
    let dir = tempfile::tempdir().unwrap();
    let second = "00000000-0000-4000-8000-000000000002";
    let first = UnixListener::bind(dir.path().join(format!("{ID}.sock"))).unwrap();
    let next = UnixListener::bind(dir.path().join(format!("{second}.sock"))).unwrap();
    let server = async {
        let serve = |listener: UnixListener, id: &str| {
            let id = id.to_owned();
            async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut bytes = [0; 4096];
                assert!(stream.read(&mut bytes).await.unwrap() > 0);
                stream
                    .write_all(
                        &encode_server_message(
                            &json!({"type":"hello","version":8,"serverId":id}),
                            MAX,
                        )
                        .unwrap(),
                    )
                    .await
                    .unwrap();
            }
        };
        tokio::join!(serve(first, ID), serve(next, second));
    };
    let (routes, _) = tokio::join!(
        discover_unix_servers(dir.path(), Duration::from_secs(1)),
        server
    );
    assert_eq!(
        routes
            .unwrap()
            .iter()
            .map(|r| r.server_id.as_str())
            .collect::<Vec<_>>(),
        vec![ID, second]
    );
}
#[tokio::test]
async fn discovery_times_out_silent_socket_without_deleting() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(format!("{ID}.sock"));
    let listener = UnixListener::bind(&path).unwrap();
    let server = async {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut bytes = [0; 4096];
        while stream.read(&mut bytes).await.unwrap() != 0 {}
    };
    let (routes, _) = tokio::join!(
        discover_unix_servers(dir.path(), Duration::from_millis(20)),
        server
    );
    assert!(routes.unwrap().is_empty());
    assert!(path.exists());
}
#[tokio::test]
async fn discovery_ignores_pre_handshake_close() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(format!("{ID}.sock"));
    let listener = UnixListener::bind(&path).unwrap();
    let server = async {
        let (stream, _) = listener.accept().await.unwrap();
        drop(stream);
    };
    let (routes, _) = tokio::join!(
        discover_unix_servers(dir.path(), Duration::from_secs(1)),
        server
    );
    assert!(routes.unwrap().is_empty());
}
#[tokio::test]
async fn discovery_limits_concurrent_probes_to_sixteen() {
    let dir = tempfile::tempdir().unwrap();
    let mut listeners = Vec::new();
    for i in 1..=20 {
        let id = format!("00000000-0000-4000-8000-{i:012x}");
        listeners.push(UnixListener::bind(dir.path().join(format!("{id}.sock"))).unwrap());
    }
    let (accepted, mut events) = tokio::sync::mpsc::unbounded_channel();
    let (release, signal) = tokio::sync::watch::channel(false);
    let mut tasks = tokio::task::JoinSet::new();
    for listener in listeners {
        let accepted = accepted.clone();
        let mut signal = signal.clone();
        tasks.spawn(async move {
        let (mut stream,_)=listener.accept().await.unwrap(); accepted.send(()).unwrap();
        signal.wait_for(|released| *released).await.unwrap(); let mut bytes=[0;4096]; assert!(stream.read(&mut bytes).await.unwrap()>0);
        stream.write_all(&encode_server_message(&json!({"type":"hello_error","error":{"code":"version","message":"Unsupported protocol version"}}),MAX).unwrap()).await.unwrap();
    });
    }
    drop(accepted);
    let control = async {
        for _ in 0..16 {
            events.recv().await.unwrap();
        }
        assert!(events.try_recv().is_err());
        release.send_replace(true);
        for _ in 16..20 {
            events.recv().await.unwrap();
        }
    };
    let (routes, _) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(
            discover_unix_servers(dir.path(), Duration::from_secs(2)),
            control
        )
    })
    .await
    .unwrap();
    assert!(routes.unwrap().is_empty());
    while let Some(result) = tasks.join_next().await {
        result.unwrap();
    }
}
