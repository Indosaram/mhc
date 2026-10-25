use std::time::Duration;
use telemetry_core::{
    PostHogTelemetryTransport, TelemetryCaptureMessage, TelemetryTransport,
    TelemetryTransportOptions,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
fn options(host: String) -> TelemetryTransportOptions {
    TelemetryTransportOptions {
        host,
        disable_geoip: true,
        enable_exception_autocapture: false,
        enable_local_evaluation: false,
        strict_local_evaluation: true,
        disable_remote_config: true,
        flush_at: 20,
        flush_interval: 10_000,
    }
}
#[tokio::test]
async fn immediate_send_waits_for_response_and_classifies_status_without_retry() {
    tokio::time::timeout(Duration::from_secs(5), async {
        for status in [200, 400, 413, 429, 500] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
            let address = listener.local_addr().expect("address");
            let (received_tx, received_rx) = tokio::sync::oneshot::channel();
            let (answer_tx, answer_rx) = tokio::sync::oneshot::channel();
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.expect("accept");
                let mut request = Vec::new(); let mut chunk = [0;1024];
                loop {
                    let n = stream.read(&mut chunk).await.expect("read"); assert!(n > 0); request.extend_from_slice(&chunk[..n]);
                    if let Some(header_end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                        let header = String::from_utf8_lossy(&request[..header_end]);
                        let length = header.lines().find_map(|line| line.to_ascii_lowercase().strip_prefix("content-length:").and_then(|s| s.trim().parse::<usize>().ok())).expect("content length");
                        if request.len() >= header_end + 4 + length { break; }
                    }
                }
                received_tx.send(()).expect("received signal"); answer_rx.await.expect("answer signal");
                stream.write_all(format!("HTTP/1.1 {status} Test\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}").as_bytes()).await.expect("response");
            });
            let transport = PostHogTelemetryTransport::new("key", &options(format!("http://{address}"))).expect("transport");
            let message = TelemetryCaptureMessage { uuid: None, timestamp: None, distinct_id: "machine".into(), event: "summary".into(), properties: Default::default() };
            let send = transport.capture_immediate(&message).expect("immediate"); tokio::pin!(send);
            tokio::select! { result = &mut send => panic!("resolved without response: {result:?}"), signal = received_rx => signal.expect("request received") }
            answer_tx.send(()).expect("answer"); let result = send.await;
            if status == 200 { assert!(result.is_ok()); } else { let error = result.expect_err("rejected"); assert_eq!(error.status, Some(status)); assert_eq!(error.permanent, matches!(status, 400 | 413)); }
            server.await.expect("server");
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("address"); drop(listener);
        let transport = PostHogTelemetryTransport::new("key", &options(format!("http://{address}"))).expect("transport");
        let message = TelemetryCaptureMessage { uuid: None, timestamp: None, distinct_id: "machine".into(), event: "summary".into(), properties: Default::default() };
        let error = transport.capture_immediate(&message).expect("immediate").await.expect_err("refused"); assert!(!error.permanent); assert_eq!(error.status, None);
    }).await.expect("bounded delivery test");
}
