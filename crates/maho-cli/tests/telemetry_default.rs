use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const SAFETY_BOUND: Duration = Duration::from_secs(60);

struct MockLlmServer {
    address: std::net::SocketAddr,
    shutdown: tokio::sync::broadcast::Sender<()>,
}

impl MockLlmServer {
    async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind llm listener");
        let address = listener.local_addr().expect("llm address");
        let (shutdown, _) = tokio::sync::broadcast::channel(1);
        let mut shutdown_rx = shutdown.subscribe();

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    accept_res = listener.accept() => {
                        let Ok((mut socket, _)) = accept_res else { break; };
                        tokio::spawn(async move {
                            let mut buf = [0u8; 4096];
                            let mut req_bytes = Vec::new();
                            loop {
                                let Ok(n) = socket.read(&mut buf).await else { return; };
                                if n == 0 { return; }
                                req_bytes.extend_from_slice(&buf[..n]);
                                if let Some(end) = req_bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                                    let headers = String::from_utf8_lossy(&req_bytes[..end]);
                                    let len = headers
                                        .lines()
                                        .find_map(|l| {
                                            l.split_once(':').and_then(|(k, v)| {
                                                if k.trim().eq_ignore_ascii_case("content-length") {
                                                    v.trim().parse::<usize>().ok()
                                                } else {
                                                    None
                                                }
                                            })
                                        })
                                        .unwrap_or(0);
                                    if req_bytes.len() >= end + 4 + len {
                                        break;
                                    }
                                }
                            }
                            let sse_body = concat!(
                                "data: {\"id\":\"offline\",\"object\":\"chat.completion.chunk\",\"created\":0,\"model\":\"offline\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"hi back\"},\"finish_reason\":null}]}\n\n",
                                "data: {\"id\":\"offline\",\"object\":\"chat.completion.chunk\",\"created\":0,\"model\":\"offline\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
                                "data: [DONE]\n\n"
                            );
                            let resp = format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{sse_body}",
                                sse_body.len()
                            );
                            let _ = socket.write_all(resp.as_bytes()).await;
                        });
                    }
                    _ = shutdown_rx.recv() => {
                        break;
                    }
                }
            }
        });

        Self { address, shutdown }
    }
}

struct PosthogRecorder {
    address: std::net::SocketAddr,
    requests: Arc<Mutex<Vec<serde_json::Value>>>,
    shutdown: tokio::sync::broadcast::Sender<()>,
}

impl PosthogRecorder {
    async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind posthog listener");
        let address = listener.local_addr().expect("posthog address");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let (shutdown, _) = tokio::sync::broadcast::channel(1);
        let mut shutdown_rx = shutdown.subscribe();
        let captured = Arc::clone(&requests);

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    accept_res = listener.accept() => {
                        let Ok((mut socket, _)) = accept_res else { break; };
                        let requests = Arc::clone(&captured);
                        tokio::spawn(async move {
                            let mut buf = [0u8; 4096];
                            let mut req_bytes = Vec::new();
                            loop {
                                let Ok(n) = socket.read(&mut buf).await else { return; };
                                if n == 0 { return; }
                                req_bytes.extend_from_slice(&buf[..n]);
                                if let Some(end) = req_bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                                    let headers = String::from_utf8_lossy(&req_bytes[..end]);
                                    let len = headers
                                        .lines()
                                        .find_map(|l| {
                                            l.split_once(':').and_then(|(k, v)| {
                                                if k.trim().eq_ignore_ascii_case("content-length") {
                                                    v.trim().parse::<usize>().ok()
                                                } else {
                                                    None
                                                }
                                            })
                                        })
                                        .unwrap_or(0);
                                    if req_bytes.len() >= end + 4 + len {
                                        break;
                                    }
                                }
                            }
                            if let Some(end) = req_bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                                let body = &req_bytes[end + 4..];
                                if let Ok(val) = serde_json::from_slice::<serde_json::Value>(body) {
                                    requests.lock().expect("test").push(val);
                                }
                            }
                            let resp = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 15\r\nConnection: close\r\n\r\n{\"status\":\"ok\"}";
                            let _ = socket.write_all(resp.as_bytes()).await;
                        });
                    }
                    _ = shutdown_rx.recv() => {
                        break;
                    }
                }
            }
        });

        Self {
            address,
            requests,
            shutdown,
        }
    }
}

fn setup_fixture(home: &std::path::Path, llm_addr: std::net::SocketAddr, telemetry_enabled: bool) {
    let omo_dir = home.join(".omo");
    std::fs::create_dir_all(&omo_dir).expect("create .omo");
    std::fs::write(omo_dir.join("onboarding-completed"), "{}\n").expect("write onboarding");

    if telemetry_enabled {
        std::fs::write(
            omo_dir.join("omo.jsonc"),
            "{\n  \"telemetry\": {\n    \"enabled\": true\n  }\n}\n",
        )
        .expect("write omo.jsonc");
    }

    let agent_dir = home.join("agent");
    std::fs::create_dir_all(&agent_dir).expect("create agent dir");
    std::fs::create_dir_all(agent_dir.join("omo-senpi/omo-native")).expect("create native state");
    std::fs::write(
        agent_dir.join("omo-senpi/omo-native/onboarding-completed"),
        "{}\n",
    )
    .expect("write native onboarding");

    let models = serde_json::json!({
        "providers": {
            "offline": {
                "api": "openai-completions",
                "baseUrl": format!("http://{llm_addr}/v1"),
                "apiKey": "offline-fixture",
                "models": [
                    {
                        "id": "offline",
                        "reasoning": false,
                        "input": ["text"],
                        "contextWindow": 128000,
                        "maxTokens": 4096
                    }
                ]
            }
        }
    });
    std::fs::write(agent_dir.join("models.json"), models.to_string()).expect("write models.json");

    let settings = serde_json::json!({
        "defaultProvider": "offline",
        "defaultModel": "offline"
    });
    std::fs::write(agent_dir.join("settings.json"), settings.to_string()).expect("write settings.json");
}

async fn run_mhc_print(
    home: &std::path::Path,
    posthog_addr: std::net::SocketAddr,
) -> std::process::Output {
    let agent_dir = home.join("agent");
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_mhc"));
    command
        .current_dir(home)
        .env("HOME", home)
        .env("MAHO_CODING_AGENT_DIR", &agent_dir)
        .env("POSTHOG_HOST", format!("http://{posthog_addr}"))
        .env("POSTHOG_API_KEY", "phc_test_key_1234567890")
        // This test deliberately exercises capture from an ephemeral test HOME.
        .env("OMO_TELEMETRY_ALLOW_AUTOMATION", "1")
        .env_remove("DO_NOT_TRACK")
        .env_remove("OMO_DISABLE_POSTHOG")
        .env_remove("OMO_SENPI_DISABLE_POSTHOG")
        .env_remove("OMO_SEND_ANONYMOUS_TELEMETRY")
        .env_remove("__PI_INTERNAL_SPAWN")
        .args(["-p", "hi", "--model", "offline/offline"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);

    if cfg!(windows) {
        command.env("USERPROFILE", home);
    }

    let child = command.spawn().expect("spawn mhc");
    tokio::time::timeout(SAFETY_BOUND, child.wait_with_output())
        .await
        .expect("mhc finished within safety bound")
        .expect("mhc wait output")
}

#[tokio::test]
async fn telemetry_defaults_to_disabled_recording_zero_requests() {
    let temp = tempfile::tempdir().expect("tempdir");
    let llm = MockLlmServer::start().await;
    let recorder = PosthogRecorder::start().await;

    setup_fixture(temp.path(), llm.address, false);

    let output = run_mhc_print(temp.path(), recorder.address).await;
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "stderr: {stderr}\nstdout: {stdout}");

    // The process has exited, so every request it would have sent is already recorded.
    let recorded = recorder.requests.lock().expect("test").clone();
    assert_eq!(
        recorded.len(),
        0,
        "expected zero telemetry requests by default, got: {recorded:?}"
    );

    let _ = llm.shutdown.send(());
    let _ = recorder.shutdown.send(());
}

// Telemetry is recorded by the omo telemetry component; the MIT profile sends none (the
// disabled-by-default test above still holds there).
