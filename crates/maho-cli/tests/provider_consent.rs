//! Provider risk consent gate: without recorded consent an impersonating provider's request fails
//! before any network work; after `mhc auth consent <provider> --yes` the request reaches the server.
use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn fixture(address: std::net::SocketAddr) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("home");
    let agent = dir.path().join("agent");
    std::fs::create_dir_all(agent.join("omo-senpi/omo-native")).expect("agent");
    std::fs::write(agent.join("omo-senpi/omo-native/onboarding-completed"), "{}\n").expect("onboarding");
    std::fs::write(agent.join("models.json"), serde_json::json!({"providers":{"github-copilot":{
        "api":"openai-completions","baseUrl":format!("http://{address}/v1"),
        "models":[{"id":"fixture","reasoning":false,"input":["text"],"contextWindow":128000,"maxTokens":4096}]
    }}}).to_string()).expect("models");
    // An already-expired OAuth credential: any refresh attempt would be a network request.
    std::fs::write(agent.join("auth.json"), serde_json::json!({"github-copilot":{
        "type":"oauth","access":"tid=fake","refresh":"ghu_fake","expires":1
    }}).to_string()).expect("auth");
    dir
}

fn mhc(dir: &tempfile::TempDir, args: &[&str]) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_mhc"))
        .current_dir(dir.path()).env("HOME", dir.path()).env("MAHO_CODING_AGENT_DIR", dir.path().join("agent"))
        .env_remove("__PI_INTERNAL_SPAWN")
        .args(args).stdin(std::process::Stdio::null()).output().expect("mhc")
}

async fn serve() -> (std::net::SocketAddr, Arc<AtomicUsize>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("addr");
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            counter.fetch_add(1, Ordering::SeqCst);
            let mut buffer = [0; 4096];
            let _ = socket.read(&mut buffer).await;
            let _ = socket.write_all(b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
        }
    });
    (address, hits)
}

#[tokio::test]
async fn consent_gate_blocks_then_allows_github_copilot_requests() {
    let (address, hits) = serve().await;
    let dir = fixture(address);
    let print = ["-p", "--offline", "--no-session", "--no-tools", "--no-extensions", "--model", "github-copilot/fixture", "hello"];

    let blocked = mhc(&dir, &print);
    let text = format!("{}{}", String::from_utf8_lossy(&blocked.stdout), String::from_utf8_lossy(&blocked.stderr));
    assert!(!blocked.status.success(), "-p without consent must exit non-zero: {text}");
    assert!(text.contains("`mhc auth consent github-copilot`"), "{text}");
    assert!(!text.contains("not found") && !text.contains("Unknown model"), "model must resolve to reach the gate: {text}");
    assert_eq!(hits.load(Ordering::SeqCst), 0, "no request, refresh included, before consent");

    let consent = mhc(&dir, &["auth", "consent", "github-copilot", "--yes"]);
    assert!(consent.status.success(), "{}", String::from_utf8_lossy(&consent.stderr));
    let settings = std::fs::read_to_string(dir.path().join("agent/settings.json")).expect("settings");
    assert!(settings.contains("providerRiskConsent") && settings.contains("github-copilot"), "{settings}");

    let allowed = mhc(&dir, &print);
    let text = format!("{}{}", String::from_utf8_lossy(&allowed.stdout), String::from_utf8_lossy(&allowed.stderr));
    assert!(!text.contains("`mhc auth consent github-copilot`"), "{text}");
    assert!(hits.load(Ordering::SeqCst) > 0, "server receives a request after consent");
}
