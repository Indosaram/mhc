const SAFETY_BOUND: std::time::Duration = std::time::Duration::from_secs(60);

#[tokio::test]
async fn real_print_entry_streams_loopback_model_and_exits() {
    run_print_entry(false, None).await;
}

#[tokio::test]
async fn real_print_entry_suppresses_project_context_when_requested() {
    run_print_entry(true, None).await;
}

/// senpi main.ts `readPipedStdin`: piped stdin is trimmed before it prefixes the first message.
#[tokio::test]
async fn real_print_entry_trims_piped_stdin() {
    run_print_entry(false, Some("piped context\n")).await;
}

/// senpi `applyExtensionFlagValues` + `resolveModelScope`: an unregistered `--flag` is a runtime
/// error (exit 1) and an unmatched `--models` pattern is a `Warning:` line.
#[tokio::test]
async fn real_print_entry_rejects_unknown_extension_flags_and_warns_on_unmatched_scope() {
    let dir = tempfile::tempdir().expect("print-entry fixture operation");
    std::fs::create_dir_all(dir.path().join(".omo")).expect("print-entry fixture operation");
    std::fs::write(dir.path().join(".omo/onboarding-completed"), "{}\n").expect("print-entry fixture operation");
    let agent = dir.path().join("agent");
    std::fs::create_dir(&agent).expect("print-entry fixture operation");
    std::fs::write(agent.join("models.json"), serde_json::json!({"providers":{"offline":{
        "api":"openai-completions", "baseUrl":"http://127.0.0.1:9/v1", "apiKey":"offline-fixture",
        "models":[{"id":"offline","reasoning":false,"input":["text"],"contextWindow":128000,"maxTokens":4096}]
    }}}).to_string()).expect("print-entry fixture operation");
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_mhc"));
    command
        .current_dir(dir.path()).env("HOME", dir.path()).env("MAHO_CODING_AGENT_DIR", &agent)
        .env_remove("__PI_INTERNAL_SPAWN")
        .args(["-p", "--offline", "--no-session", "--no-tools", "--no-extensions", "--model", "offline/offline", "--models", "nomatch-fixture*", "--definitely-unknown-flag=1", "hello"])
        .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    if cfg!(windows) {
        command.env("USERPROFILE", dir.path());
    }
    let child = command.spawn().expect("print-entry fixture operation");
    let output = tokio::time::timeout(SAFETY_BOUND, child.wait_with_output()).await
        .expect("print entry finished within the safety bound").expect("print-entry fixture operation");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("Error: Unknown option: --definitely-unknown-flag"), "{stderr}");
    assert!(stderr.contains("Warning: No models match pattern \"nomatch-fixture*\""), "{stderr}");
}

async fn run_print_entry(no_context_files: bool, piped_stdin: Option<&str>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("print-entry fixture operation");
    let address = listener.local_addr().expect("print-entry fixture operation");
    let dir = tempfile::tempdir().expect("print-entry fixture operation");
    std::fs::create_dir_all(dir.path().join(".omo")).expect("print-entry fixture operation");
    std::fs::write(dir.path().join(".omo/onboarding-completed"), "{}\n").expect("print-entry fixture operation");
    let agent = dir.path().join("agent");
    std::fs::create_dir(&agent).expect("print-entry fixture operation");
    // Onboarding state lives under the agent home (`getOmoNativeStateDir` -> `<agentDir>/omo-senpi/omo-native`);
    // headless startup claims and injects it like upstream onboarding/component.ts, so mark it complete there.
    std::fs::create_dir_all(agent.join("omo-senpi/omo-native")).expect("print-entry fixture operation");
    std::fs::write(agent.join("omo-senpi/omo-native/onboarding-completed"), "{}\n").expect("print-entry fixture operation");
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
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_mhc"));
    command
        .current_dir(dir.path()).env("HOME", dir.path()).env("MAHO_CODING_AGENT_DIR", &agent)
        .env_remove("__PI_INTERNAL_SPAWN")
        .args(["-p", "--offline", "--no-session", "--no-tools", "--no-skills", "--no-prompt-templates", "--prompt-template", "acceptance.md", "--model", "offline/offline", "/acceptance fixture"])
        .args(["--system-prompt", "system.md", "--append-system-prompt", "append.md", "--append-system-prompt", "fixture literal append"])
        .args(if no_context_files { vec!["--no-context-files"] } else { Vec::new() })
        .stdin(if piped_stdin.is_some() { std::process::Stdio::piped() } else { std::process::Stdio::null() })
        .stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    if cfg!(windows) {
        command.env("USERPROFILE", dir.path());
    }
    let mut child = command.spawn().expect("print-entry fixture operation");
    if let Some(text) = piped_stdin {
        let mut stdin = child.stdin.take().expect("print stdin");
        stdin.write_all(text.as_bytes()).await.expect("print-entry fixture operation");
        drop(stdin);
    }
    let mut stdout = child.stdout.take().expect("print stdout");
    let mut stderr = child.stderr.take().expect("print stderr");
    let result = tokio::time::timeout(SAFETY_BOUND, async {
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
            let mut err = Vec::new();
            stderr.read_to_end(&mut err).await.expect("read print diagnostics after exit");
            panic!("print operation failed: {other:?}\nstderr: {}", String::from_utf8_lossy(&err));
        }
    };
    drop(listener);
    let messages = payload["messages"].as_array().expect("print messages");
    assert!(messages.iter().any(|message| message["role"] == "user" && message["content"].as_array().is_some_and(|parts| parts.iter().any(|part| part["type"] == "text" && part["text"] == piped_stdin.map_or_else(|| "expanded acceptance fixture".to_owned(), |text| format!("{}/acceptance fixture", text.trim())).as_str()))), "{payload}");
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(String::from_utf8(output.stdout).expect("print-entry fixture operation"), "offline acceptance\n");
    let system = messages.iter().find(|message| message["role"] == "system").expect("print-entry fixture operation");
    let system = system["content"].as_str().expect("print-entry fixture operation");
    let custom = system.find("fixture custom system").expect("custom system source forwarded");
    let file_append = system.find("fixture file append").expect("file append source forwarded");
    let literal_append = system.find("fixture literal append").expect("literal append source forwarded");
    assert!(custom < file_append && file_append < literal_append, "source order preserved");
    // senpi `_rebuildSystemPrompt` uses a `--system-prompt` verbatim (agent-session.ts:3580): no project context either way.
    let _ = no_context_files;
    assert!(!system.contains("CONTEXT_FIXTURE_7f29"), "a custom system prompt carries no project context");
}
