#[tokio::test]
async fn real_print_entry_streams_loopback_model_and_exits() {
    run_print_entry(false).await;
}

#[tokio::test]
async fn real_print_entry_suppresses_project_context_when_requested() {
    run_print_entry(true).await;
}

async fn run_print_entry(no_context_files: bool) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("print-entry fixture operation");
    let address = listener.local_addr().expect("print-entry fixture operation");
    let dir = tempfile::tempdir().expect("print-entry fixture operation");
    let agent = dir.path().join("agent");
    std::fs::create_dir(&agent).expect("print-entry fixture operation");
    std::fs::create_dir(agent.join("prompts")).expect("print-entry fixture operation");
    std::fs::write(agent.join("prompts/default.md"), "default prompt must not load").expect("print-entry fixture operation");
    std::fs::write(dir.path().join("acceptance.md"), "expanded acceptance $1").expect("print-entry fixture operation");
    std::fs::write(dir.path().join("system.md"), "fixture custom system").expect("print-entry fixture operation");
    std::fs::write(dir.path().join("append.md"), "fixture file append").expect("print-entry fixture operation");
    std::fs::write(dir.path().join("AGENTS.md"), "CONTEXT_FIXTURE_7f29").expect("print-entry fixture operation");
    std::fs::write(agent.join("models.json"), serde_json::json!({"providers":{"offline":{
        "api":"openai-completions", "baseUrl":format!("http://{address}/v1"), "apiKey":"offline-fixture",
        "models":[{"id":"offline","reasoning":false,"input":["text"],"contextWindow":128000,"maxTokens":4096}]
    }}}).to_string()).expect("print-entry fixture operation");
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_mhc"))
        .current_dir(dir.path()).env("HOME", dir.path()).env("MAHO_CODING_AGENT_DIR", &agent)
        .env_remove("__PI_INTERNAL_SPAWN")
        .args(["-p", "--offline", "--no-session", "--no-tools", "--no-skills", "--no-prompt-templates", "--prompt-template", "acceptance.md", "--model", "offline/offline", "/acceptance fixture"])
        .args(["--system-prompt", "system.md", "--append-system-prompt", "append.md", "--append-system-prompt", "fixture literal append"])
        .args(if no_context_files { vec!["--no-context-files"] } else { Vec::new() })
        .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn().expect("print-entry fixture operation");
    let mut stdout = child.stdout.take().expect("print stdout");
    let mut stderr = child.stderr.take().expect("print stderr");
    let result = tokio::time::timeout(std::time::Duration::from_secs(10), async {
    let (mut connection, _) = listener.accept().await?;
    let mut request = Vec::new();
    let mut chunk = [0; 4096];
    loop {
        let count = connection.read(&mut chunk).await?;
        if count == 0 { return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "request closed before body")); }
        request.extend_from_slice(&chunk[..count]);
        if let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&request[..end]);
            let length = headers.lines().find_map(|line| line.split_once(':')
                .filter(|(name, _)| name.eq_ignore_ascii_case("content-length")).map(|(_, value)| value.trim()))
                .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "missing content length"))?
                .parse::<usize>().map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
            if request.len() >= end + 4 + length { break; }
        }
    }
    let end = request.windows(4).position(|bytes| bytes == b"\r\n\r\n")
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "missing request headers"))? + 4;
    let payload: serde_json::Value = serde_json::from_slice(&request[end..])
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    let body = concat!(
        "data: {\"id\":\"offline\",\"object\":\"chat.completion.chunk\",\"created\":0,\"model\":\"offline\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"offline acceptance\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"offline\",\"object\":\"chat.completion.chunk\",\"created\":0,\"model\":\"offline\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n"
    );
    connection.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await?;
    drop(connection);
    let mut output = Vec::new();
    let mut errors = Vec::new();
    let (status, _, _) = tokio::try_join!(child.wait(), stdout.read_to_end(&mut output), stderr.read_to_end(&mut errors))?;
    Ok::<_, std::io::Error>((std::process::Output { status, stdout: output, stderr: errors }, payload))
    }).await;
    let (output, payload) = match result {
        Ok(Ok(result)) => result,
        other => {
            child.kill().await.expect("kill print after failed bounded operation");
            child.wait().await.expect("reap print after failed bounded operation");
            panic!("print operation failed: {other:?}");
        }
    };
    drop(listener);
    let messages = payload["messages"].as_array().expect("print messages");
    assert!(messages.iter().any(|message| message["role"] == "user" && message["content"].as_array().is_some_and(|parts| parts.iter().any(|part| part["type"] == "text" && part["text"] == "expanded acceptance fixture"))), "{payload}");
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(String::from_utf8(output.stdout).expect("print-entry fixture operation"), "offline acceptance\n");
    let system = messages.iter().find(|message| message["role"] == "system").expect("print-entry fixture operation");
    let system = system["content"].as_str().expect("print-entry fixture operation");
    let custom = system.find("fixture custom system").expect("custom system source forwarded");
    let file_append = system.find("fixture file append").expect("file append source forwarded");
    let literal_append = system.find("fixture literal append").expect("literal append source forwarded");
    assert!(custom < file_append && file_append < literal_append, "source order preserved");
    assert_eq!(system.contains("CONTEXT_FIXTURE_7f29"), !no_context_files, "project context suppression");
}
