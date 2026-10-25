//! One correlated JSONL conversation under one monotonic deadline.
#[cfg(unix)]
pub struct RpcSocket {
    reader: tokio::io::BufReader<tokio::net::UnixStream>,
    deadline: tokio::time::Instant,
}
#[cfg(unix)]
impl RpcSocket {
    pub async fn connect(endpoint: &str, timeout: std::time::Duration) -> std::io::Result<Self> {
        let deadline = tokio::time::Instant::now() + timeout;
        let stream =
            tokio::time::timeout_at(deadline, tokio::net::UnixStream::connect(endpoint)).await??;
        Ok(Self {
            reader: tokio::io::BufReader::new(stream),
            deadline,
        })
    }
    /// Transfer an authenticated, pre-session connection to the persistent mux.
    pub fn into_stream(self) -> std::io::Result<tokio::net::UnixStream> {
        if !self.reader.buffer().is_empty() {
            return Err(std::io::Error::other(
                "unexpected buffered host data at authentication handoff",
            ));
        }
        Ok(self.reader.into_inner())
    }
    pub async fn request(
        &mut self,
        mut command: serde_json::Value,
    ) -> std::io::Result<serde_json::Value> {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
        let id = command
            .get("id")
            .cloned()
            .unwrap_or_else(|| serde_json::json!(uuid::Uuid::new_v4().to_string()));
        command["id"] = id.clone();
        let deadline = self.deadline;
        tokio::time::timeout_at(deadline, async {
            let mut text = serde_json::to_vec(&command)?;
            text.push(b'\n');
            self.reader.get_mut().write_all(&text).await?;
            loop {
                let mut bytes = Vec::new();
                if self.reader.read_until(b'\n', &mut bytes).await? == 0 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "host closed before response",
                    ));
                }
                let Ok(frame) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
                    continue;
                };
                if frame.get("id") == Some(&id) {
                    return Ok(frame);
                }
            }
        })
        .await?
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    #[tokio::test]
    async fn split_utf8_and_stray_output_do_not_corrupt_correlated_reply() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("socket");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let serve = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = tokio::io::BufReader::new(stream);
            let mut line = String::new();
            stream.read_line(&mut line).await.unwrap();
            let command: serde_json::Value = serde_json::from_str(&line).unwrap();
            let response =
                serde_json::json!({"id":command["id"],"success":true,"data":{"text":"日本語"}})
                    .to_string()
                    + "\n";
            stream
                .get_mut()
                .write_all(b"stray output\n{\"id\":\"other\"}\n")
                .await
                .unwrap();
            for byte in response.as_bytes() {
                stream.get_mut().write_all(&[*byte]).await.unwrap();
            }
        });
        let mut socket =
            RpcSocket::connect(path.to_str().unwrap(), std::time::Duration::from_secs(5))
                .await
                .unwrap();
        let response = socket
            .request(serde_json::json!({"type":"get_state"}))
            .await
            .unwrap();
        assert_eq!(response["data"]["text"], "日本語");
        serve.await.unwrap();
    }
}
