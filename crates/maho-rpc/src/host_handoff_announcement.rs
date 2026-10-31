//! The handoff announcement of one host generation (senpi `host-handoff-announcement.ts`, senpi#2285, senpi#3816
//! review). The sealed internal hop authenticates before the host can register a connection, so a connection that
//! reached the listener before the drain began can finish authenticating after the announcement was broadcast. It
//! still gets that one record, then a clean close; a connection that arrives after the drain began is refused.
use tokio::io::{AsyncWrite, AsyncWriteExt};

#[derive(Default)]
pub struct HandoffAnnouncement {
    /// Unset until `begin`; then `None` until the record is built, `Some(None)` when it could not be.
    record:
        std::sync::Mutex<Option<tokio::sync::watch::Receiver<Option<Option<serde_json::Value>>>>>,
}

impl HandoffAnnouncement {
    /// Builds the `host_superseded` record once the successor is known; `None` (logged) when the endpoint could
    /// not be checked. Later `deliver_late` calls wait for it.
    ///
    /// The announcement is registered before this returns, so a late connection admitted at any point after `begin`
    /// waits for the record instead of being refused.
    pub fn begin<'a>(
        &self,
        superseded: impl std::future::Future<Output = Result<bool, String>> + 'a,
        instance_id: &'a str,
        generation: f64,
        public_socket: &'a str,
        log: impl Fn(&str) + 'a,
    ) -> impl std::future::Future<Output = Option<serde_json::Value>> + 'a {
        let (built, record) = tokio::sync::watch::channel(None);
        *self
            .record
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(record);
        Self::build(
            built,
            superseded,
            instance_id,
            generation,
            public_socket,
            log,
        )
    }

    async fn build(
        built: tokio::sync::watch::Sender<Option<Option<serde_json::Value>>>,
        superseded: impl std::future::Future<Output = Result<bool, String>>,
        instance_id: &str,
        generation: f64,
        public_socket: &str,
        log: impl Fn(&str),
    ) -> Option<serde_json::Value> {
        let announced = match superseded.await {
            Ok(replaced) => {
                serde_json::to_value(crate::rpc_types::RpcHostLifecycleEvent::HostSuperseded {
                    instance_id: instance_id.to_owned(),
                    generation,
                    successor: replaced.then(|| crate::rpc_types::RpcHostSuccessor {
                        socket: public_socket.to_owned(),
                    }),
                })
                .ok()
            }
            Err(cause) => {
                log(&format!("handoff announcement failed: {cause}"));
                None
            }
        };
        built.send_replace(Some(announced.clone()));
        announced
    }

    /// Takes a connection that finished authenticating during the drain; `false` when it must be refused instead.
    pub fn deliver_late<S>(&self, mut stream: S, arrived_before_drain: bool) -> bool
    where
        S: AsyncWrite + Send + Unpin + 'static,
    {
        let record = self
            .record
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let Some(record) = record.filter(|_| arrived_before_drain) else {
            return false;
        };
        tokio::spawn(async move {
            let mut record = record;
            let built = match record.wait_for(Option::is_some).await {
                Ok(built) => built.clone(),
                Err(_) => return,
            };
            // An announcement that could not be built destroys the stream (drop); otherwise one record, then end.
            if let Some(Some(announced)) = built
                && let Ok(line) = crate::jsonl::serialize_json_line(&announced)
            {
                let _ = stream.write_all(line.as_bytes()).await;
                let _ = stream.shutdown().await;
            }
        });
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    #[tokio::test]
    async fn a_connection_that_predates_the_drain_gets_the_record_then_a_clean_close() {
        let announcement = HandoffAnnouncement::default();
        let (late, mut client) = tokio::io::duplex(4096);
        let (refused, _other) = tokio::io::duplex(64);
        assert!(!announcement.deliver_late(refused, true));
        let record = announcement
            .begin(async { Ok(true) }, "i", 2., "/tmp/pub.sock", |_| {})
            .await
            .expect("record");
        assert_eq!(record["successor"]["socket"], "/tmp/pub.sock");
        let (after, _peer) = tokio::io::duplex(64);
        assert!(!announcement.deliver_late(after, false));
        assert!(announcement.deliver_late(late, true));
        let mut received = String::new();
        client.read_to_string(&mut received).await.expect("read");
        let line: serde_json::Value = serde_json::from_str(received.trim_end()).expect("json");
        assert_eq!(line, record);
        assert!(received.ends_with('\n'));
    }

    #[tokio::test]
    async fn a_failed_check_logs_and_destroys_late_connections() {
        let announcement = HandoffAnnouncement::default();
        let logged = std::sync::Mutex::new(Vec::new());
        assert!(
            announcement
                .begin(async { Err("boom".into()) }, "i", 0., "/p", |line| logged
                    .lock()
                    .expect("lock")
                    .push(line.to_owned()))
                .await
                .is_none()
        );
        assert_eq!(
            *logged.lock().expect("lock"),
            ["handoff announcement failed: boom"]
        );
        let (late, mut client) = tokio::io::duplex(64);
        assert!(announcement.deliver_late(late, true));
        let mut received = Vec::new();
        client.read_to_end(&mut received).await.expect("closed");
        assert!(received.is_empty());
    }
}
