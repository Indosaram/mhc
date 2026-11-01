//! senpi main.ts:1119-1148 `resolveSessionRuntime`: a stdio multi-session host runs every session in
//! its own isolated worker process, while `--session-runtime in-process` keeps them in the host.
//! Drives the real binary over its stdio protocol and observes the host's child processes. The
//! `supervisor_stdio_concurrent` cases run on every platform: the stdio host serves sessions
//! concurrently, and stdin EOF mid-turn ends the sessions and the worker processes it owns.

use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};

/// Safety bound on every single wait: a hung host fails the test instead of stalling it.
const STEP: Duration = Duration::from_secs(60);

struct Host {
    child: tokio::process::Child,
    stdin: Option<tokio::process::ChildStdin>,
    lines: tokio::io::Lines<tokio::io::BufReader<tokio::process::ChildStdout>>,
    _home: tempfile::TempDir,
    cwd: String,
    /// Every record read so far, in arrival order.
    seen: Vec<serde_json::Value>,
}

impl Host {
    #[cfg(target_os = "linux")]
    fn start(extra: &[&str]) -> Self {
        Self::launch(extra, tempfile::tempdir().expect("isolated home"), None)
    }

    /// A worker-runtime host whose agent dir names one model, `loopback/loopback`, served at `address`.
    fn start_with_provider(address: std::net::SocketAddr) -> Self {
        let home = tempfile::tempdir().expect("isolated home");
        let agent = home.path().join("agent");
        // Onboarding state lives under the agent home; mark it complete so no session waits on it.
        std::fs::create_dir_all(agent.join("omo-senpi/omo-native")).expect("agent state dir");
        std::fs::write(agent.join("omo-senpi/omo-native/onboarding-completed"), "{}\n").expect("onboarding marker");
        std::fs::create_dir_all(home.path().join(".omo")).expect("home state dir");
        std::fs::write(home.path().join(".omo/onboarding-completed"), "{}\n").expect("onboarding marker");
        std::fs::write(
            agent.join("models.json"),
            serde_json::json!({"providers":{"loopback":{
                "api":"openai-completions", "baseUrl":format!("http://{address}/v1"), "apiKey":"loopback-fixture",
                "models":[{"id":"loopback","reasoning":false,"input":["text"],"contextWindow":128000,"maxTokens":4096}]
            }}})
            .to_string(),
        )
        .expect("write models.json");
        Self::launch(&[], home, Some(agent))
    }

    fn launch(extra: &[&str], home: tempfile::TempDir, agent: Option<std::path::PathBuf>) -> Self {
        let cwd = home.path().join("project");
        std::fs::create_dir_all(&cwd).expect("project dir");
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_mhc"));
        command
            .current_dir(&cwd)
            .env("HOME", home.path())
            .env("USERPROFILE", home.path())
            .env("PI_OFFLINE", "1")
            .env_remove("__PI_INTERNAL_SPAWN");
        if let Some(agent) = &agent {
            command
                .env_remove("OMO_CODING_AGENT_DIR")
                .env_remove("SENPI_CODING_AGENT_DIR")
                .env_remove("PI_CODING_AGENT_DIR")
                .env("MAHO_CODING_AGENT_DIR", agent);
        }
        let mut child = command
            .args(["--mode", "rpc", "--multi-session", "--no-extensions"])
            .args(extra)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .expect("start the real host");
        let stdin = child.stdin.take().expect("piped stdin");
        let lines = tokio::io::BufReader::new(child.stdout.take().expect("piped stdout")).lines();
        let cwd = plain_path(&cwd.canonicalize().expect("cwd"));
        Self { child, stdin: Some(stdin), lines, _home: home, cwd, seen: Vec::new() }
    }

    async fn send(&mut self, command: serde_json::Value) {
        let stdin = self.stdin.as_mut().expect("the host's stdin is still open");
        tokio::time::timeout(STEP, stdin.write_all(format!("{command}\n").as_bytes()))
            .await
            .expect("the host read its stdin in time")
            .expect("write command");
    }

    /// The first matching record, already seen or still to come.
    async fn wait_for(&mut self, what: &str, matches: impl Fn(&serde_json::Value) -> bool) -> serde_json::Value {
        if let Some(record) = self.seen.iter().find(|&record| matches(record)) {
            return record.clone();
        }
        self.next_matching(what, matches).await
    }

    /// The next matching record still to come; every record read is kept in `seen`.
    async fn next_matching(&mut self, what: &str, matches: impl Fn(&serde_json::Value) -> bool) -> serde_json::Value {
        let found = tokio::time::timeout(STEP, async {
            loop {
                let line = self.lines.next_line().await.expect("read host").expect("host closed stdout");
                let record: serde_json::Value = serde_json::from_str(&line).expect("json record");
                self.seen.push(record.clone());
                if matches(&record) {
                    return record;
                }
            }
        })
        .await;
        found.unwrap_or_else(|_| panic!("timed out waiting for {what}; records seen:\n{}", summary(&self.seen)))
    }

    /// Sends one command and awaits exactly its response record; other records are skipped.
    #[cfg(target_os = "linux")]
    async fn call(&mut self, command: serde_json::Value) -> serde_json::Value {
        let kind = command["type"].as_str().expect("command type").to_owned();
        self.send(command).await;
        self.next_matching(&format!("the {kind} response"), |record| record["type"] == "response" && record["command"] == kind.as_str()).await
    }

    /// Opens one interactive session on the loopback model and answers its routing handle.
    async fn open_loopback_session(&mut self, id: &str) -> String {
        let cwd = self.cwd.clone();
        self.send(serde_json::json!({"id":id,"type":"open_session","cwd":cwd,"provider":"loopback","modelId":"loopback","auto_title":false})).await;
        let opened = self.wait_for("the open_session response", |record| is_response(record, id)).await;
        assert_eq!(opened["success"], true, "{opened}");
        opened["data"]["sessionId"].as_str().expect("routing handle").to_owned()
    }

    fn pid(&self) -> u32 {
        self.child.id().expect("host is running")
    }
}

fn summary(records: &[serde_json::Value]) -> String {
    records
        .iter()
        .map(|record| format!("{} {} {} {} {}", record["sessionId"], record["type"], record["command"], record["id"], record["error"]))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The canonical path without Windows' `\\?\` verbatim prefix, which a session cwd never carries.
fn plain_path(path: &std::path::Path) -> String {
    let path = path.to_string_lossy();
    path.strip_prefix(r"\\?\").unwrap_or(&path).to_owned()
}

fn is_response(record: &serde_json::Value, id: &str) -> bool {
    record["type"] == "response" && record["id"] == id
}

fn is_session_event(record: &serde_json::Value, session: &str, kind: &str) -> bool {
    record["type"] == kind && record["sessionId"] == session
}

/// Direct children of `pid`, across every thread of the process.
#[cfg(target_os = "linux")]
fn children(pid: u32) -> Vec<u32> {
    let mut children = Vec::new();
    let Ok(tasks) = std::fs::read_dir(format!("/proc/{pid}/task")) else { return children };
    for task in tasks.flatten() {
        let listed = std::fs::read_to_string(task.path().join("children")).unwrap_or_default();
        children.extend(listed.split_whitespace().filter_map(|value| value.parse::<u32>().ok()));
    }
    children
}

#[cfg(target_os = "linux")]
fn role_of(pid: u32) -> Option<String> {
    let environ = std::fs::read(format!("/proc/{pid}/environ")).ok()?;
    environ.split(|byte| *byte == 0).find_map(|entry| {
        std::str::from_utf8(entry).ok()?.strip_prefix("__PI_INTERNAL_SPAWN=").map(str::to_owned)
    })
}

#[cfg(target_os = "linux")]
fn session_workers(pid: u32) -> Vec<u32> {
    children(pid).into_iter().filter(|pid| role_of(*pid).as_deref() == Some("rpc-session-worker")).collect()
}

/// The session workers `host_pid` owns. Linux reads each direct child's role from `/proc`; elsewhere
/// a direct child running the mhc executable is a worker, since a worker-runtime stdio host spawns
/// no other mhc process.
#[cfg(target_os = "linux")]
async fn owned_workers(host_pid: u32) -> Vec<u32> {
    session_workers(host_pid)
}

#[cfg(all(unix, not(target_os = "linux")))]
async fn owned_workers(host_pid: u32) -> Vec<u32> {
    let name = executable_name();
    let listed = command_output("ps", &["-A", "-o", "pid=", "-o", "ppid=", "-o", "ucomm="]).await;
    listed
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse::<u32>().ok()?;
            let parent = fields.next()?.parse::<u32>().ok()?;
            (parent == host_pid && fields.next()? == name).then_some(pid)
        })
        .collect()
}

#[cfg(windows)]
async fn owned_workers(host_pid: u32) -> Vec<u32> {
    let script = format!("Get-CimInstance Win32_Process -Filter ParentProcessId={host_pid} | Where-Object Name -eq {} | ForEach-Object ProcessId", executable_name());
    let listed = command_output("powershell", &["-NoProfile", "-NonInteractive", "-Command", &script]).await;
    listed.lines().filter_map(|line| line.trim().parse::<u32>().ok()).collect()
}

/// Whether any process with this id exists. The host reaps each worker before it exits, so after the
/// host's own exit a worker id is gone at once; one check suffices and needs no polling.
#[cfg(target_os = "linux")]
async fn process_exists(pid: u32) -> bool {
    std::path::Path::new(&format!("/proc/{pid}")).exists()
}

#[cfg(all(unix, not(target_os = "linux")))]
async fn process_exists(pid: u32) -> bool {
    !command_output("ps", &["-p", &pid.to_string(), "-o", "pid="]).await.trim().is_empty()
}

#[cfg(windows)]
async fn process_exists(pid: u32) -> bool {
    let script = format!("Get-CimInstance Win32_Process -Filter ProcessId={pid} | ForEach-Object ProcessId");
    !command_output("powershell", &["-NoProfile", "-NonInteractive", "-Command", &script]).await.trim().is_empty()
}

/// The file name the host and its workers run under (`mhc`, or `mhc.exe` on Windows).
#[cfg(not(target_os = "linux"))]
fn executable_name() -> String {
    std::path::Path::new(env!("CARGO_BIN_EXE_mhc")).file_name().expect("executable file name").to_string_lossy().into_owned()
}

/// Runs one process-table query within the safety bound and answers its stdout. A query that finds
/// nothing may exit nonzero (`ps -p`), so only a failure to run or a hang fails the test.
#[cfg(not(target_os = "linux"))]
async fn command_output(program: &str, args: &[&str]) -> String {
    let output = tokio::time::timeout(
        STEP,
        tokio::process::Command::new(program).args(args).stdin(std::process::Stdio::null()).stderr(std::process::Stdio::inherit()).kill_on_drop(true).output(),
    )
    .await
    .unwrap_or_else(|_| panic!("{program} answered within the safety bound"))
    .unwrap_or_else(|error| panic!("run {program}: {error}"));
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_stdio_host_runs_each_session_in_its_own_worker_process() {
    let mut host = Host::start(&[]);
    let info = host.call(serde_json::json!({ "type": "get_protocol_info" })).await;
    assert_eq!(info["data"]["launch_profile"]["core"]["session_runtime"], "worker", "{info}");

    let cwd = host.cwd.clone();
    let opened = host.call(serde_json::json!({ "type": "open_session", "cwd": cwd })).await;
    assert_eq!(opened["success"], true, "{opened}");
    let handle = opened["data"]["sessionId"].as_str().expect("routing handle").to_owned();
    assert_eq!(opened["data"]["state"]["cwd"], cwd.as_str(), "{opened}");

    let workers = session_workers(host.pid());
    assert_eq!(workers.len(), 1, "one isolated worker per open session");

    let state = host.call(serde_json::json!({ "type": "get_state", "sessionId": handle })).await;
    assert_eq!(state["success"], true, "{state}");
    assert_eq!(state["sessionId"], handle.as_str());
    assert_eq!(state["data"]["cwd"], cwd.as_str(), "{state}");

    let listed = host.call(serde_json::json!({ "type": "list_sessions", "include_workers": true })).await;
    assert_eq!(listed["data"]["sessions"][0]["sessionId"], handle.as_str(), "{listed}");
    assert_eq!(listed["data"]["sessions"][0]["status"], "open", "{listed}");

    let closed = host.call(serde_json::json!({ "type": "close_session", "sessionId": handle })).await;
    assert_eq!(closed["success"], true, "{closed}");
    // The close answers only after the worker's actual exit, which the host has reaped.
    assert!(!children(host.pid()).contains(&workers[0]), "the worker exited with its session");
    let listed = host.call(serde_json::json!({ "type": "list_sessions", "include_workers": true })).await;
    assert_eq!(listed["data"]["sessions"], serde_json::json!([]), "{listed}");
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn an_in_process_host_keeps_sessions_in_the_host_process() {
    let mut host = Host::start(&["--session-runtime", "in-process"]);
    let info = host.call(serde_json::json!({ "type": "get_protocol_info" })).await;
    assert_eq!(info["data"]["launch_profile"]["core"]["session_runtime"], "in-process", "{info}");
    let cwd = host.cwd.clone();
    let opened = host.call(serde_json::json!({ "type": "open_session", "cwd": cwd })).await;
    assert_eq!(opened["success"], true, "{opened}");
    assert!(
        !children(host.pid()).into_iter().any(|pid| role_of(pid).as_deref() == Some("rpc-session-worker")),
        "an in-process session spawns no worker"
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn real_host_republishes_terminal_then_running_task_context_without_changing_identity() {
    let mut host = Host::start(&["--session-runtime", "in-process"]);
    let cwd = host.cwd.clone();
    let opened = host
        .call(serde_json::json!({"type":"open_session","cwd":cwd,"kind":"worker"}))
        .await;
    assert_eq!(opened["success"], true, "{opened}");
    let id = opened["data"]["sessionId"].as_str().unwrap().to_owned();
    for status in ["completed", "running", "error"] {
        let published = host.call(serde_json::json!({"type":"set_session_context","sessionId":id,"patch":{"task_status":status}})).await;
        assert_eq!(published["success"], true, "{published}");
        let listed = host
            .call(serde_json::json!({"type":"list_sessions","include_workers":true}))
            .await;
        let row = &listed["data"]["sessions"][0];
        assert_eq!(row["sessionId"], id);
        assert_eq!(row["context"]["task_status"], status, "{listed}");
    }
    assert_eq!(
        host.call(serde_json::json!({"type":"close_session","sessionId":id}))
            .await["success"],
        true
    );
}

/// A prompt carrying this marker is held mid-stream by the loopback provider.
const HOLD_MARKER: &str = "HOLD_STREAM_SUPERVISOR_4c1e";

/// What the loopback provider reports about the held stream, sent the moment it happens.
#[derive(Debug, PartialEq)]
enum ProviderEvent {
    /// The held request was answered with its first chunk and its stream left open.
    HeldStarted,
    /// The client hung up the held stream (an abort, or the owning process's exit).
    HeldClosed,
}

struct Provider {
    address: std::net::SocketAddr,
    events: tokio::sync::mpsc::UnboundedReceiver<ProviderEvent>,
    task: tokio::task::JoinHandle<()>,
}

impl Provider {
    async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind loopback provider");
        let address = listener.local_addr().expect("loopback address");
        let (sender, events) = tokio::sync::mpsc::unbounded_channel();
        Self { address, events, task: tokio::spawn(serve_loopback_provider(listener, sender)) }
    }

    async fn next_event(&mut self, what: &str) -> ProviderEvent {
        tokio::time::timeout(STEP, self.events.recv())
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
            .expect("the loopback provider is running")
    }
}

impl Drop for Provider {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Reads one HTTP request (headers plus content-length body) and answers its body text.
async fn read_request(connection: &mut tokio::net::TcpStream) -> std::io::Result<String> {
    let mut request = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let count = connection.read(&mut chunk).await?;
        if count == 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "request closed before its body"));
        }
        request.extend_from_slice(&chunk[..count]);
        if let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&request[..end]).into_owned();
            let length = headers
                .lines()
                .find_map(|line| line.split_once(':').filter(|(name, _)| name.eq_ignore_ascii_case("content-length")).map(|(_, value)| value.trim().to_owned()))
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(0);
            if request.len() >= end + 4 + length {
                return Ok(String::from_utf8_lossy(&request[end + 4..end + 4 + length]).into_owned());
            }
        }
    }
}

fn completion_chunk(delta: &serde_json::Value, finish: Option<&str>) -> String {
    let chunk = serde_json::json!({"id":"loopback","object":"chat.completion.chunk","created":0,"model":"loopback",
        "choices":[{"index":0,"delta":delta,"finish_reason":finish}]});
    format!("data: {chunk}\n\n")
}

/// A loopback openai-completions provider. A request carrying `HOLD_MARKER` gets one text chunk and
/// then a stream that never finishes on its own; every other request completes at once.
async fn serve_loopback_provider(listener: tokio::net::TcpListener, events: tokio::sync::mpsc::UnboundedSender<ProviderEvent>) {
    loop {
        let Ok((mut connection, _)) = listener.accept().await else { return };
        let events = events.clone();
        tokio::spawn(async move {
            let Ok(body) = read_request(&mut connection).await else { return };
            if body.contains(HOLD_MARKER) {
                let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n";
                let first = completion_chunk(&serde_json::json!({"role":"assistant","content":"still streaming"}), None);
                if connection.write_all(format!("{head}{first}").as_bytes()).await.is_err() || connection.flush().await.is_err() {
                    return;
                }
                let _ = events.send(ProviderEvent::HeldStarted);
                // Held until the client hangs up: end of stream or a reset both mean it is gone.
                let mut rest = [0u8; 1024];
                while matches!(connection.read(&mut rest).await, Ok(count) if count > 0) {}
                let _ = events.send(ProviderEvent::HeldClosed);
            } else {
                let body = format!(
                    "{}{}data: [DONE]\n\n",
                    completion_chunk(&serde_json::json!({"role":"assistant","content":"settled"}), None),
                    completion_chunk(&serde_json::json!({}), Some("stop")),
                );
                let reply = format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                let _ = connection.write_all(reply.as_bytes()).await;
            }
        });
    }
}

/// Starts session A's held turn and returns once BOTH sides report the stream running: the provider
/// answered A's first chunk, and the host relayed A's streaming update.
async fn start_held_turn(host: &mut Host, provider: &mut Provider, session: &str) {
    host.send(serde_json::json!({"id":"prompt-held","type":"prompt","sessionId":session,"message":format!("{HOLD_MARKER} keep streaming")})).await;
    assert_eq!(provider.next_event("the held stream to start").await, ProviderEvent::HeldStarted);
    host.wait_for("the held turn to stream", |record| is_session_event(record, session, "message_update")).await;
}

/// Plan todo 4: while session A's turn is held mid-stream, session B's prompt runs to completion and
/// `get_protocol_info` is answered; an abort for A is then answered and ends A's stream.
#[tokio::test]
async fn supervisor_stdio_concurrent_sessions_progress_while_one_streams() {
    let mut provider = Provider::start().await;
    let mut host = Host::start_with_provider(provider.address);
    let streaming = host.open_loopback_session("open-a").await;
    let other = host.open_loopback_session("open-b").await;

    start_held_turn(&mut host, &mut provider, &streaming).await;

    host.send(serde_json::json!({"id":"prompt-other","type":"prompt","sessionId":other,"message":"answer at once"})).await;
    host.send(serde_json::json!({"id":"info","type":"get_protocol_info"})).await;
    let prompted = host.wait_for("the other session's prompt response", |record| is_response(record, "prompt-other")).await;
    assert_eq!(prompted["success"], true, "{prompted}");
    host.wait_for("the other session's turn to end", |record| is_session_event(record, &other, "agent_end")).await;
    let info = host.wait_for("the get_protocol_info response", |record| is_response(record, "info")).await;
    assert_eq!(info["success"], true, "{info}");
    assert!(info["data"]["capabilities"].as_array().is_some_and(|capabilities| capabilities.iter().any(|capability| capability == "multi_session")), "{info}");

    // Session A is still mid-turn: the host relayed no end for it and the provider still holds it.
    assert!(
        !host.seen.iter().any(|record| is_session_event(record, &streaming, "agent_end")),
        "session A's turn ended before the abort; records seen:\n{}",
        summary(&host.seen)
    );
    assert!(
        matches!(provider.events.try_recv(), Err(tokio::sync::mpsc::error::TryRecvError::Empty)),
        "the held stream ended before the abort"
    );

    host.send(serde_json::json!({"id":"abort-held","type":"abort","sessionId":streaming})).await;
    let aborted = host.wait_for("the abort response", |record| is_response(record, "abort-held")).await;
    assert_eq!(aborted["success"], true, "{aborted}");
    assert_eq!(provider.next_event("the aborted stream to hang up").await, ProviderEvent::HeldClosed);
    host.wait_for("the aborted turn to end", |record| is_session_event(record, &streaming, "agent_end")).await;
}

/// The failure path: stdin closes while session A's turn is held mid-stream. The host ends the
/// session, awaits its worker's exit and leaves; the provider sees the held stream hang up, and on
/// every OS the worker process the host owned is gone once the host has exited.
#[tokio::test]
async fn supervisor_stdio_concurrent_stdin_eof_mid_turn_cleans_up_owned_workers() {
    let mut provider = Provider::start().await;
    let mut host = Host::start_with_provider(provider.address);
    let streaming = host.open_loopback_session("open-a").await;
    start_held_turn(&mut host, &mut provider, &streaming).await;
    let workers = owned_workers(host.pid()).await;
    assert_eq!(workers.len(), 1, "the open session owns exactly one worker: {workers:?}");
    // The existence check must see a live worker, or its later "gone" would prove nothing.
    for worker in &workers {
        assert!(process_exists(*worker).await, "worker {worker} is visible while its turn streams");
    }

    drop(host.stdin.take());
    let closed = host.wait_for("the session to end with its peer", |record| is_session_event(record, &streaming, "session_closed")).await;
    assert_eq!(closed["reason"], "host_shutdown", "{closed}");
    let status = tokio::time::timeout(STEP, host.child.wait()).await.expect("the host exited in time").expect("reap the host");
    assert!(status.success(), "the host left cleanly after stdin EOF: {status}");
    assert_eq!(provider.next_event("the held stream to hang up").await, ProviderEvent::HeldClosed);
    // The host reaped its worker before it exited, so no process with that id remains.
    for worker in workers {
        assert!(!process_exists(worker).await, "worker {worker} outlived its host");
    }
}
