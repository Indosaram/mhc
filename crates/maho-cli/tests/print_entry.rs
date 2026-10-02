#[test]
fn real_print_entry_streams_loopback_model_and_exits() {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let agent = dir.path().join("agent");
    std::fs::create_dir(&agent).unwrap();
    std::fs::write(agent.join("models.json"), serde_json::json!({"providers":{"offline":{
        "api":"openai-completions", "baseUrl":format!("http://{address}/v1"), "apiKey":"offline-fixture",
        "models":[{"id":"offline","reasoning":false,"input":["text"],"contextWindow":128000,"maxTokens":4096}]
    }}}).to_string()).unwrap();
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_mhc"))
        .current_dir(dir.path()).env("HOME", dir.path()).env("MAHO_CODING_AGENT_DIR", &agent)
        .env_remove("__PI_INTERNAL_SPAWN")
        .args(["-p", "--offline", "--no-session", "--no-tools", "--model", "offline/offline", "Respond offline"])
        .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped())
        .spawn().unwrap();
    let (accepted, received) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        let connection = listener.accept();
        let _ = accepted.send(connection);
    });
    let (mut connection, _) = match received.recv_timeout(std::time::Duration::from_secs(10)) {
        Ok(connection) => connection.unwrap(),
        Err(error) => {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("model request not received: {error}");
        }
    };
    connection.set_read_timeout(Some(std::time::Duration::from_secs(10))).unwrap();
    let mut request = Vec::new();
    let mut chunk = [0; 4096];
    loop {
        let count = connection.read(&mut chunk).unwrap();
        assert_ne!(count, 0, "request closed before body");
        request.extend_from_slice(&chunk[..count]);
        if let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&request[..end]);
            let length = headers.lines().find_map(|line| line.split_once(':').filter(|(name, _)| name.eq_ignore_ascii_case("content-length")).map(|(_, value)| value.trim().parse::<usize>().expect("request content length"))).unwrap();
            if request.len() >= end + 4 + length { break; }
        }
    }
    let body = concat!(
        "data: {\"id\":\"offline\",\"object\":\"chat.completion.chunk\",\"created\":0,\"model\":\"offline\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"offline acceptance\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"offline\",\"object\":\"chat.completion.chunk\",\"created\":0,\"model\":\"offline\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n"
    );
    write!(connection, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
    drop(connection);
    server.join().unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "offline acceptance\n");
}
