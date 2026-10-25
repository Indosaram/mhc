use crate::telemetry_delivery_error::TelemetryDeliveryError;
use crate::types::{TelemetryCaptureMessage, TelemetryError, TelemetryTransportOptions};
use serde_json::json;

/// Dedicated, serialized sender with no in-process retry or queue loss on rejection.
pub struct ImmediateSender {
    api_key: String,
    options: TelemetryTransportOptions,
    http: reqwest::Client,
    queue: tokio::sync::Mutex<()>,
}

pub fn create_immediate_sender(
    api_key: &str,
    options: &TelemetryTransportOptions,
) -> Result<ImmediateSender, TelemetryError> {
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|error| TelemetryError::new(error.to_string()))?;
    Ok(ImmediateSender {
        api_key: api_key.into(),
        options: options.clone(),
        http,
        queue: tokio::sync::Mutex::new(()),
    })
}

impl ImmediateSender {
    pub async fn send(
        &self,
        message: &TelemetryCaptureMessage,
    ) -> Result<(), TelemetryDeliveryError> {
        let _guard = self.queue.lock().await;
        let mut properties = message.properties.clone();
        properties.insert("$lib".into(), "omo-telemetry-core-rs".into());
        properties.insert("$lib_version".into(), env!("CARGO_PKG_VERSION").into());
        if self.options.disable_geoip {
            properties.insert("$geoip_disable".into(), true.into());
        }
        let mut entry = json!({
            "type": "capture", "event": message.event, "distinct_id": message.distinct_id,
            "properties": properties,
            "timestamp": message.timestamp.clone().unwrap_or_else(|| crate::diagnostics::to_iso_string(chrono::Utc::now())),
        });
        if let Some(uuid) = &message.uuid {
            entry["uuid"] = uuid.clone().into();
        }
        let response = self
            .http
            .post(format!(
                "{}/batch/",
                self.options.host.trim_end_matches('/')
            ))
            .json(&json!({"api_key": self.api_key, "batch": [entry]}))
            .send()
            .await
            .map_err(|error| {
                TelemetryDeliveryError::new(None, Some(TelemetryError::new(error.to_string())))
            })?;
        let status = response.status().as_u16();
        if (200..300).contains(&status) {
            Ok(())
        } else {
            Err(TelemetryDeliveryError::new(Some(status), None))
        }
    }

    pub async fn shutdown(&self) {
        let _guard = self.queue.lock().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn server(
        statuses: Vec<u16>,
    ) -> (String, tokio::task::JoinHandle<Vec<serde_json::Value>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let host = format!("http://{}", listener.local_addr().expect("address"));
        let task = tokio::spawn(async move {
            let mut bodies = Vec::new();
            for status in statuses {
                let (mut stream, _) = listener.accept().await.expect("accept");
                let mut bytes = Vec::new();
                let (offset, length) = loop {
                    let mut buffer = [0; 4096];
                    let size = stream.read(&mut buffer).await.expect("request");
                    assert_ne!(size, 0);
                    bytes.extend_from_slice(&buffer[..size]);
                    if let Some(index) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                        let header = String::from_utf8_lossy(&bytes[..index]).to_lowercase();
                        let length: usize = header
                            .lines()
                            .find_map(|line| line.strip_prefix("content-length:"))
                            .expect("content length")
                            .trim()
                            .parse()
                            .expect("length");
                        break (index + 4, length);
                    }
                };
                while bytes.len() < offset + length {
                    let mut buffer = [0; 4096];
                    let size = stream.read(&mut buffer).await.expect("body");
                    assert_ne!(size, 0);
                    bytes.extend_from_slice(&buffer[..size]);
                }
                bodies.push(serde_json::from_slice(&bytes[offset..offset + length]).expect("json"));
                stream.write_all(format!("HTTP/1.1 {status} Answer\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await.expect("response");
            }
            bodies
        });
        (host, task)
    }

    fn options(host: String) -> TelemetryTransportOptions {
        TelemetryTransportOptions {
            host,
            disable_geoip: true,
            enable_exception_autocapture: false,
            enable_local_evaluation: false,
            strict_local_evaluation: true,
            disable_remote_config: true,
            flush_at: 1,
            flush_interval: 0,
        }
    }

    fn message(event: &str) -> TelemetryCaptureMessage {
        TelemetryCaptureMessage {
            uuid: Some("test-uuid".into()),
            timestamp: Some("2026-05-25T00:00:00.000Z".into()),
            distinct_id: "machine".into(),
            event: event.into(),
            properties: Default::default(),
        }
    }

    #[tokio::test]
    async fn success_requires_2xx_and_preserves_capture_identity() {
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            let (host, server) = server(vec![200, 204]).await;
            let sender = create_immediate_sender("key", &options(host)).expect("sender");
            sender.send(&message("first")).await.expect("200");
            sender.send(&message("second")).await.expect("204");
            sender.shutdown().await;
            let bodies = server.await.expect("server");
            assert_eq!(bodies[0]["batch"][0]["uuid"], "test-uuid");
            assert_eq!(
                bodies[0]["batch"][0]["timestamp"],
                "2026-05-25T00:00:00.000Z"
            );
        })
        .await
        .expect("bounded delivery");
    }

    #[tokio::test]
    async fn rejected_delivery_is_not_retried_and_does_not_poison_next_send() {
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            let statuses = vec![400, 413, 408, 429, 500, 200];
            let (host, server) = server(statuses.clone()).await;
            let sender = create_immediate_sender("key", &options(host)).expect("sender");
            for status in &statuses[..5] {
                let error = sender
                    .send(&message("rejected"))
                    .await
                    .expect_err("rejection");
                assert_eq!(error.status, Some(*status));
                assert_eq!(error.permanent, matches!(*status, 400 | 413));
            }
            sender
                .send(&message("accepted"))
                .await
                .expect("queue recovers");
            assert_eq!(server.await.expect("server").len(), statuses.len());
        })
        .await
        .expect("bounded rejection");
    }

    #[tokio::test]
    async fn network_failure_is_retryable_without_a_status() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let options = options(format!(
            "http://{}",
            listener.local_addr().expect("address")
        ));
        // Accept and explicitly close the request rather than racing a free port allocation.
        let close = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            drop(stream);
        });
        let sender = create_immediate_sender("key", &options).expect("sender");
        let error = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            sender.send(&message("network")),
        )
        .await
        .expect("bounded")
        .expect_err("network error");
        close.await.expect("closed");
        assert_eq!(error.status, None);
        assert!(!error.permanent);
        assert!(error.cause.is_some());
    }
    #[tokio::test]
    async fn silent_server_times_out_as_retryable() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let mut sender = create_immediate_sender(
            "key",
            &options(format!(
                "http://{}",
                listener.local_addr().expect("address")
            )),
        )
        .expect("sender");
        sender.http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(100))
            .build()
            .expect("client");
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (_stream, _) = listener.accept().await.expect("accept");
            stopped.await.expect("stop");
        });
        let error = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            sender.send(&message("timeout")),
        )
        .await
        .expect("bounded")
        .expect_err("timeout");
        stop.send(()).expect("stop server");
        server.await.expect("server");
        assert_eq!(error.status, None);
        assert!(!error.permanent);
    }
}
