//! Real-subprocess QA for the MOUNTED recall path: `mhc -p` with the memory component registered.
//!
//! This drives the product route end to end (`runtime.rs` -> `assembled_factories` -> `OmoMount::for_parent`
//! -> `recall_wiring.register`), not a direct memory unit call. The loopback provider serves the parent turn
//! and the kibitzer child turns. The parent response is HELD until the delivery artifact has been READ and
//! cached, so the parent cannot end the run before the child's nudge was accepted and delivered - and the
//! cached payload survives that shutdown, which deletes the on-disk pending file.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use maho_omo_memory::kibitzer_nudge_tool::{KibitzerNudgeInput, KibitzerNudgeParams, execute_nudge};
use memory_core::git::{
    GitCommitAuthor, GitExec, GitExecOptions, GitExecResult, GitMemoryRepo, GitMemoryRepoOptions,
    GitSeedFile, InitializeGitRepoOptions, system_git_exec,
};
use memory_core::identity::{build_identity_paths, resolve_memory_identity};
use memory_core::identity::layout::MemoryIdentityPaths;
use notify::Watcher;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio::task::{JoinHandle, JoinSet};

/// Failure deadline for the delivery-artifact wait. It is NEVER the pass condition.
const DELIVERY_DEADLINE: Duration = Duration::from_secs(30);
/// Failure deadline for the whole `mhc` run.
const RUN_DEADLINE: Duration = Duration::from_secs(60);
/// Failure deadline for the server's own shutdown join.
const CLEANUP_DEADLINE: Duration = Duration::from_secs(10);
/// The corpus document the fixture commits; the envelope must offer this path to the child.
const CORPUS_PATH: &str = "notes/qa.md";
/// A fixed identity, so the memory paths are deterministic (`memory.agent` in the user omo config).
const IDENTITY: &str = "qa-fixture";
/// The one-line hint the child sends with its nudge call.
const HINT: &str = "The note records the canary rollback gate and the release order.";
/// The user prompt. It shares tokens with the corpus doc so recall finds a candidate, and it NEVER contains
/// the doc's path, which is how the server tells the parent request from the child's envelope.
const PROMPT: &str = "what does the note record about the canary rollback gate and the release order?";
/// The scripted tool-call id, which is what the continuation's tool result must name.
const TOOL_CALL_ID: &str = "call_qa_nudge";
/// `memory.recall.max_items`: 2, so ONE accepted nudge does NOT reach the cap and the batch continues.
const RECALL_MAX_ITEMS: usize = 2;
/// The fixture's git identity: used for the seeded commit and by the child through `<tmp>/.gitconfig`.
const GIT_NAME: &str = "QA Fixture";
const GIT_EMAIL: &str = "qa@example.invalid";

struct Recorded {
    parent: Mutex<Option<String>>,
    children: Mutex<Vec<String>>,
}

impl Recorded {
    fn new() -> Arc<Self> {
        Arc::new(Self { parent: Mutex::new(None), children: Mutex::new(Vec::new()) })
    }

    fn set_parent(&self, body: String) {
        *self.parent.lock().unwrap_or_else(PoisonError::into_inner) = Some(body);
    }

    fn push_child(&self, body: String) -> usize {
        let mut children = self.children.lock().unwrap_or_else(PoisonError::into_inner);
        children.push(body);
        children.len()
    }

    fn parent(&self) -> Option<String> {
        self.parent.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn children(&self) -> Vec<String> {
        self.children.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

async fn read_request(connection: &mut TcpStream) -> std::io::Result<Option<String>> {
    let mut request = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let count = connection.read(&mut chunk).await?;
        if count == 0 {
            return Ok(None);
        }
        request.extend_from_slice(&chunk[..count]);
        if let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&request[..end]).into_owned();
            let length = headers
                .lines()
                .find_map(|line| {
                    line.split_once(':')
                        .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                        .map(|(_, value)| value.trim().to_owned())
                })
                .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "missing content length"))?
                .parse::<usize>()
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
            if request.len() >= end + 4 + length {
                return Ok(Some(String::from_utf8_lossy(&request[end + 4..end + 4 + length]).into_owned()));
            }
        }
    }
}

async fn write_stream(connection: &mut TcpStream, chunks: &[String]) -> std::io::Result<()> {
    let body: String = chunks.concat();
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    connection.write_all(head.as_bytes()).await?;
    connection.write_all(body.as_bytes()).await?;
    connection.flush().await
}

fn chunk(content: &str, finish: Option<&str>) -> String {
    let finish = finish.map(|value| format!("\"{value}\"")).unwrap_or_else(|| "null".to_owned());
    format!(
        "data: {{\"id\":\"offline\",\"object\":\"chat.completion.chunk\",\"created\":0,\"model\":\"offline\",\"choices\":[{{\"index\":0,\"delta\":{{\"content\":{}}},\"finish_reason\":{finish}}}]}}\n\n",
        serde_json::to_string(content).expect("chunk content serializes")
    )
}

/// The child's FIRST turn: a REAL `nudge` tool call for the offered corpus path.
fn nudge_tool_call_chunk() -> String {
    let arguments = serde_json::json!({ "path": CORPUS_PATH, "hint": HINT }).to_string();
    let delta = serde_json::json!({
        "tool_calls": [{
            "index": 0,
            "id": TOOL_CALL_ID,
            "type": "function",
            "function": { "name": "nudge", "arguments": arguments },
        }],
    });
    format!(
        "data: {{\"id\":\"offline\",\"object\":\"chat.completion.chunk\",\"created\":0,\"model\":\"offline\",\"choices\":[{{\"index\":0,\"delta\":{delta},\"finish_reason\":\"tool_calls\"}}]}}\n\n"
    )
}

fn done_chunk() -> String {
    "data: [DONE]\n\n".to_owned()
}

/// Serves ONE connection. A child request is answered immediately (tool call, then completions); the parent
/// response is held until the delivery artifact has been read and cached, so the parent cannot end the run
/// before the child's nudge was accepted and delivered.
async fn serve_connection(mut connection: TcpStream, recorded: Arc<Recorded>, delivery: &mut watch::Receiver<bool>) {
    let Ok(Some(body)) = read_request(&mut connection).await else { return };
    if body.contains(CORPUS_PATH) {
        let turn = recorded.push_child(body);
        let chunks = if turn == 1 {
            vec![nudge_tool_call_chunk(), done_chunk()]
        } else {
            vec![chunk("offline kibitzer done", Some("stop")), done_chunk()]
        };
        let _ = write_stream(&mut connection, &chunks).await;
    } else {
        recorded.set_parent(body);
        while !*delivery.borrow() {
            if delivery.changed().await.is_err() {
                break;
            }
        }
        let _ = write_stream(&mut connection, &[chunk("offline parent done", Some("stop")), done_chunk()]).await;
    }
}

/// The accept loop. Every connection runs in its own task inside a `JoinSet` owned by this task, so a held
/// parent NEVER blocks the accept loop. Shutdown is explicit and COMPLETE: the signal ends the accept loop,
/// `abort_all` aborts every connection task, and the loop then AWAITS each one through `join_next` - the
/// `JoinSet` is drained, not merely dropped.
fn spawn_server(listener: TcpListener, recorded: Arc<Recorded>, delivery: watch::Receiver<bool>, mut shutdown: watch::Receiver<bool>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut connections = JoinSet::new();
        loop {
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        break;
                    }
                }
                accepted = listener.accept() => {
                    let Ok((connection, _)) = accepted else { break };
                    let recorded = Arc::clone(&recorded);
                    let mut delivery = delivery.clone();
                    connections.spawn(async move { serve_connection(connection, recorded, &mut delivery).await });
                }
            }
        }
        connections.abort_all();
        while connections.join_next().await.is_some() {}
    })
}

/// Injects the fixture's git identity and neutralizes the developer's global/system config for EVERY command
/// the seeding repo runs, without mutating the process environment.
struct FixtureGitExec {
    inner: Arc<dyn GitExec>,
}

impl GitExec for FixtureGitExec {
    fn run(&self, argv: &[String], options: &GitExecOptions) -> std::io::Result<GitExecResult> {
        let mut options = options.clone();
        for (key, value) in [
            ("GIT_AUTHOR_NAME", GIT_NAME),
            ("GIT_AUTHOR_EMAIL", GIT_EMAIL),
            ("GIT_COMMITTER_NAME", GIT_NAME),
            ("GIT_COMMITTER_EMAIL", GIT_EMAIL),
            ("GIT_CONFIG_GLOBAL", "/dev/null"),
            ("GIT_CONFIG_SYSTEM", "/dev/null"),
        ] {
            options.env.insert(key.to_owned(), value.to_owned());
        }
        self.inner.run(argv, &options)
    }
}

/// Writes the ONE user-layer config file the loader reads: `<HOME>/.maho/omo.jsonc`. Recall settings live
/// under `memory.recall` (the `memory` block is what the consumer resolves, and the recall resolver reads
/// `recall` from it); a top-level `recall` is not read.
fn write_user_config(home: &Path) -> PathBuf {
    let directory = home.join(".maho");
    std::fs::create_dir_all(&directory).expect("user config directory");
    let path = directory.join("omo.jsonc");
    std::fs::write(
        &path,
        serde_json::json!({
            "memory": {
                "enabled": true,
                "agent": IDENTITY,
                "recall": { "enabled": true, "max_items": RECALL_MAX_ITEMS },
            },
            "categories": { "quick": { "model": "offline/offline" } },
        })
        .to_string(),
    )
    .expect("user omo config");
    path
}

fn write_models(agent: &Path, address: std::net::SocketAddr) {
    std::fs::write(
        agent.join("models.json"),
        serde_json::json!({
            "providers": {
                "offline": {
                    "api": "openai-completions",
                    "baseUrl": format!("http://{address}/v1"),
                    "apiKey": "offline-fixture",
                    "models": [{ "id": "offline", "reasoning": false, "input": ["text"], "contextWindow": 128000, "maxTokens": 4096 }],
                }
            }
        })
        .to_string(),
    )
    .expect("models.json");
}

fn seed_corpus(paths: &MemoryIdentityPaths) {
    let repo = GitMemoryRepo::new(GitMemoryRepoOptions {
        dir: paths.repo.clone(),
        agent_id: IDENTITY.to_owned(),
        exec: Some(Arc::new(FixtureGitExec { inner: system_git_exec() })),
        install_hooks: None,
    })
    .expect("memory repo");
    let seeds: Vec<GitSeedFile> = memory_core::seeds::seeds::build_default_seed_files()
        .into_iter()
        .map(|seed| GitSeedFile { relative_path: seed.relative_path, content: seed.content })
        .collect();
    repo.init(InitializeGitRepoOptions { author_name: Some(GIT_NAME.to_owned()), seed_files: seeds, install_hooks: None })
        .expect("repo init");
    let notes = paths.repo.join("notes");
    std::fs::create_dir_all(&notes).expect("notes directory");
    std::fs::write(notes.join("qa.md"), "# Deploy gate\nThe note records the canary rollback gate and the release order.\n")
        .expect("corpus document");
    let author = GitCommitAuthor { agent_id: IDENTITY.to_owned(), author_name: GIT_NAME.to_owned(), author_email: Some(GIT_EMAIL.to_owned()) };
    repo.commit_write(&[CORPUS_PATH], "fixture corpus", &author).expect("corpus commit");
}

/// The child's environment: cleared, then set explicitly, so no inherited memory sentinel (which would
/// disable memory) and no developer memory root can leak into the fixture.
fn child_env(home: &Path, agent: &Path, memory_root: &Path) -> Vec<(String, String)> {
    let mut env: BTreeMap<String, String> = BTreeMap::new();
    env.insert("HOME".to_owned(), home.to_string_lossy().into_owned());
    env.insert("MAHO_CODING_AGENT_DIR".to_owned(), agent.to_string_lossy().into_owned());
    env.insert("MAHO_MEMORY_HOME".to_owned(), memory_root.to_string_lossy().into_owned());
    env.insert("PATH".to_owned(), std::env::var("PATH").unwrap_or_default());
    env.into_iter().collect()
}

/// Reads the pending payload that OFFERS the corpus path, returning an OWNED parse. The watcher caches this
/// before signalling, because a normal shutdown deletes the pending file.
fn read_offered_payload(directory: &Path) -> Option<serde_json::Value> {
    let names = std::fs::read_dir(directory).ok()?;
    for name in names.flatten() {
        let path = name.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
            continue;
        }
        let Ok(raw) = std::fs::read_to_string(&path) else { continue };
        let Ok(payload) = serde_json::from_str::<serde_json::Value>(&raw) else { continue };
        let offers = payload["nudges"]
            .as_array()
            .is_some_and(|nudges| nudges.iter().any(|nudge| nudge["path"].as_str() == Some(CORPUS_PATH)));
        if offers {
            return Some(payload);
        }
    }
    None
}



/// The session ids the run actually used: each session file's HEADER entry must be `type: "session"` and carry
/// a non-empty `id`. The file name stem is NOT the session id.
fn session_ids(directory: &Path) -> Vec<String> {
    let Ok(names) = std::fs::read_dir(directory) else { return Vec::new() };
    names
        .flatten()
        .filter_map(|name| {
            let path = name.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("jsonl") {
                return None;
            }
            let raw = std::fs::read_to_string(&path).ok()?;
            let header: serde_json::Value = serde_json::from_str(raw.lines().next()?).ok()?;
            (header["type"].as_str() == Some("session"))
                .then(|| header["id"].as_str().filter(|id| !id.is_empty()).map(str::to_owned))
                .flatten()
        })
        .collect()
}

fn session_entries(directory: &Path) -> Vec<serde_json::Value> {
    let Ok(names) = std::fs::read_dir(directory) else { return Vec::new() };
    names
        .flatten()
        .filter(|name| name.path().extension().and_then(|extension| extension.to_str()) == Some("jsonl"))
        .filter_map(|name| std::fs::read_to_string(name.path()).ok())
        .flat_map(|raw| raw.lines().filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok()).collect::<Vec<_>>())
        .collect()
}

fn wake_slot_locks(directory: &Path) -> Vec<String> {
    let Ok(names) = std::fs::read_dir(directory) else { return Vec::new() };
    names
        .flatten()
        .filter_map(|name| name.file_name().to_str().map(str::to_owned))
        .filter(|name| name.starts_with("recall-wake.slot-") && name.ends_with(".lock"))
        .collect()
}

/// The text the SHIPPED nudge tool produces for this call. Acceptance is asserted against THIS, never
/// against prose written by the test.
fn shipped_nudge_text() -> String {
    let mut accepted = Vec::new();
    let result = execute_nudge(
        KibitzerNudgeInput {
            candidates: &BTreeSet::from([CORPUS_PATH.to_owned()]),
            surfaced: &BTreeSet::new(),
            max_items: RECALL_MAX_ITEMS,
            accepted: &mut accepted,
        },
        &KibitzerNudgeParams { path: CORPUS_PATH.to_owned(), hint: HINT.to_owned() },
    );
    assert!(!result.is_error, "the shipped nudge contract accepts this call: {}", result.text);
    assert_eq!(accepted.len(), 1, "the shipped nudge records exactly one accepted nudge");
    result.text
}

/// The `role: "tool"` content for the scripted call, from the continuation body.
fn tool_result_content(body: &str, tool_call_id: &str) -> Option<String> {
    let parsed: serde_json::Value = serde_json::from_str(body).ok()?;
    let message = parsed["messages"].as_array()?.iter().find(|message| {
        message["role"] == "tool" && message["tool_call_id"].as_str() == Some(tool_call_id)
    })?;
    message["content"]
        .as_str()
        .map(str::to_owned)
        .or_else(|| message["content"].as_array()?.iter().find_map(|part| part["text"].as_str().map(str::to_owned)))
}

#[tokio::test]
async fn real_mount_recall_delivers_a_nudge_and_releases_its_wake_lease() {
    let dir = tempfile::tempdir().expect("fixture root");
    let home = dir.path();
    // The project is nested INSIDE the temp home, so the config walk terminates at the home boundary and no
    // ancestor `.omo` layer can be read.
    let project = home.join("project");
    let agent = home.join("agent");
    let memory_root = home.join("memory");
    let sessions = home.join("sessions");
    for path in [&project, &agent, &memory_root, &sessions] {
        std::fs::create_dir_all(path).expect("fixture directory");
    }
    // The child's own git reads this fixture HOME, never the developer's.
    std::fs::write(home.join(".gitconfig"), format!("[user]\n\tname = {GIT_NAME}\n\temail = {GIT_EMAIL}\n"))
        .expect("gitconfig");
    let config_path = write_user_config(home);

    let env = child_env(home, &agent, &memory_root);
    let env_map: BTreeMap<String, String> = env.iter().cloned().collect();
    let identity = resolve_memory_identity(Some(IDENTITY), &project, &env_map).expect("memory identity");
    let paths = build_identity_paths(&memory_root, &identity.id);
    seed_corpus(&paths);
    std::fs::create_dir_all(&paths.recall_pending).expect("recall pending directory");

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("loopback listener");
    let address = listener.local_addr().expect("loopback address");
    write_models(&agent, address);

    let recorded = Recorded::new();
    let (delivery_tx, delivery_rx) = watch::channel(false);
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    // The watcher READS the exact offered payload and caches it BEFORE it signals, so the signal means "the
    // delivery artifact exists and was consumed", not "a file appeared".
    let evidence: Arc<Mutex<Option<serde_json::Value>>> = Arc::new(Mutex::new(None));
    let evidence_for_watcher = Arc::clone(&evidence);
    let pending_dir = paths.recall_pending.clone();
    let watcher_tx = delivery_tx.clone();
    let mut watcher = notify::RecommendedWatcher::new(
        move |event: notify::Result<notify::Event>| {
            let Ok(event) = event else { return };
            if !event.paths.iter().any(|path| path.extension().and_then(|extension| extension.to_str()) == Some("json")) {
                return;
            }
            if evidence_for_watcher.lock().unwrap_or_else(PoisonError::into_inner).is_some() {
                return;
            }
            let Some(payload) = read_offered_payload(&pending_dir) else { return };
            *evidence_for_watcher.lock().unwrap_or_else(PoisonError::into_inner) = Some(payload);
            let _ = watcher_tx.send(true);
        },
        notify::Config::default(),
    )
    .expect("pending-nudge watcher");
    watcher
        .watch(&paths.recall_pending, notify::RecursiveMode::NonRecursive)
        .expect("watch the pending directory");

    let mut server = spawn_server(listener, Arc::clone(&recorded), delivery_rx.clone(), shutdown_rx);

    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_mhc"));
    command
        .current_dir(&project)
        .env_clear()
        .envs(env)
        .args(["-p", "--offline", "--no-skills", "--no-prompt-templates", "--session-dir"])
        .arg(&sessions)
        .args(["--model", "offline/offline", PROMPT])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn().expect("mhc spawn");
    // The pipes are drained CONCURRENTLY with the delivery wait, so a full pipe can never deadlock the run,
    // and each drain returns its read RESULT so a pipe error is reported rather than ignored.
    let stdout = child.stdout.take().expect("stdout");
    let stderr = child.stderr.take().expect("stderr");
    let stdout_drain = tokio::spawn(async move {
        let mut buffer = Vec::new();
        let mut stdout = stdout;
        stdout.read_to_end(&mut buffer).await.map(|_| buffer)
    });
    let stderr_drain = tokio::spawn(async move {
        let mut buffer = Vec::new();
        let mut stderr = stderr;
        stderr.read_to_end(&mut buffer).await.map(|_| buffer)
    });

    // The exact delivery event: the parent response stays held until the offered payload was read and cached.
    let mut delivery_wait = delivery_rx.clone();
    let delivery_observed = tokio::time::timeout(DELIVERY_DEADLINE, async move {
        while !*delivery_wait.borrow() {
            if delivery_wait.changed().await.is_err() {
                break;
            }
        }
        *delivery_wait.borrow()
    })
    .await;
    if !matches!(delivery_observed, Ok(true)) {
        // Never observed: release the held parent anyway so the run can end and the failure is reportable.
        let _ = delivery_tx.send(true);
    }

    // The run outcome is captured as an explicit flag; BOTH a timeout and an await failure are reported after
    // the cleanup below, never before it. The kill/reap results are preserved for the same reason.
    let mut run_failure: Option<String> = None;
    let mut teardown_failure: Option<String> = None;
    let status = match tokio::time::timeout(RUN_DEADLINE, child.wait()).await {
        Ok(Ok(status)) => Some(status),
        Ok(Err(error)) => {
            run_failure = Some(format!("the mhc process could not be awaited: {error}"));
            None
        }
        Err(elapsed) => {
            run_failure = Some(format!("the mhc run exceeded its deadline: {elapsed}"));
            None
        }
    };
    if run_failure.is_some() {
        if let Err(error) = child.kill().await {
            teardown_failure = Some(format!("the mhc process could not be killed: {error}"));
        }
        if let Err(error) = child.wait().await {
            teardown_failure.get_or_insert_with(|| format!("the mhc process could not be reaped: {error}"));
        }
    }

    // The drain results are COLLECTED here without asserting, so a read failure cannot panic before the
    // server has been shut down and joined.
    let stdout_result = match stdout_drain.await {
        Ok(result) => result,
        Err(error) => Err(std::io::Error::other(format!("the stdout drain task failed: {error}"))),
    };
    let stderr_result = match stderr_drain.await {
        Ok(result) => result,
        Err(error) => Err(std::io::Error::other(format!("the stderr drain task failed: {error}"))),
    };

    // The server shutdown is complete and ASSERTED: signal, then join the accept task. On timeout the handle
    // is retained, aborted and awaited - a dropped `JoinHandle` would detach the task instead.
    let _ = shutdown_tx.send(true);
    let cleanup_ok = match tokio::time::timeout(CLEANUP_DEADLINE, &mut server).await {
        Ok(Ok(())) => true,
        Ok(Err(join_error)) => {
            eprintln!("memory-recall-entry cleanup receipt: server join failed: {join_error}");
            false
        }
        Err(_elapsed) => {
            server.abort();
            let _ = server.await;
            false
        }
    };

    // Only now are the collected results asserted: the server is already shut down and joined on every path.
    let stdout_bytes = stdout_result.expect("the child's stdout must be readable");
    let stderr_bytes = stderr_result.expect("the child's stderr must be readable");
    let stderr_text = String::from_utf8_lossy(&stderr_bytes).into_owned();
    eprintln!(
        "memory-recall-entry cleanup receipt: server joined = {cleanup_ok}, status = {status:?}, stdout bytes = {}, stderr bytes = {}",
        stdout_bytes.len(),
        stderr_bytes.len()
    );
    assert!(cleanup_ok, "the loopback server must shut down and join cleanly; stderr={stderr_text}");
    if let Some(failure) = teardown_failure {
        panic!("{failure}; stderr={stderr_text}");
    }
    if let Some(failure) = run_failure {
        panic!("{failure}; stderr={stderr_text}");
    }

    let children = recorded.children();
    let parent = recorded.parent();
    assert!(parent.is_some(), "the parent request must be recorded: stderr={stderr_text} config={}", config_path.display());
    assert!(parent.as_deref().is_some_and(|body| !body.contains(CORPUS_PATH)), "the parent request is not the envelope");
    assert!(
        children.first().is_some_and(|body| body.contains(CORPUS_PATH)),
        "the child's first request is the rendered envelope carrying the offered corpus path; children={}",
        children.len()
    );
    let continuation = children.get(1).unwrap_or_else(|| {
        panic!("the child must continue after an accepted nudge under max_items={RECALL_MAX_ITEMS}; children={}", children.len())
    });
    let shipped = shipped_nudge_text();
    let tool_result = tool_result_content(continuation, TOOL_CALL_ID)
        .unwrap_or_else(|| panic!("the continuation must carry the tool result for {TOOL_CALL_ID}: {continuation}"));
    assert_eq!(tool_result, shipped, "the child's tool result is exactly the shipped nudge text");

    assert!(
        matches!(delivery_observed, Ok(true)),
        "the registered delivery must consume the accepted nudge into the pending artifact before the run ends; \
         observed={delivery_observed:?} pending_dir={}",
        paths.recall_pending.display()
    );
    let ids = session_ids(&sessions);
    assert!(!ids.is_empty(), "the run must persist a session artifact whose HEADER entry is type=session with a non-empty id");
    let cached = evidence.lock().unwrap_or_else(PoisonError::into_inner).clone().expect("the cached pending payload");
    let payload_session = cached["sessionId"].as_str().unwrap_or_default();
    assert!(
        ids.iter().any(|id| id == payload_session),
        "the cached pending payload must name a real session id from the session header: {ids:?} vs {payload_session:?}"
    );
    let nudged_entries = session_entries(&sessions)
        .into_iter()
        .filter(|entry| entry["customType"].as_str() == Some("omo-kibitzer:nudged"))
        .filter(|entry| entry.to_string().contains(CORPUS_PATH))
        .count();
    assert!(
        cached["nudges"].as_array().is_some_and(|nudges| nudges.iter().any(|nudge| nudge["path"].as_str() == Some(CORPUS_PATH)))
            || nudged_entries > 0,
        "the cached payload must name the offered path (or the nudged entry must exist); cached={cached}"
    );

    let status = status.expect("a completed run has a status");
    assert!(status.success(), "mhc must exit successfully: {stderr_text}");
    let locks = wake_slot_locks(&paths.locks);
    assert!(locks.is_empty(), "every wake lease is released after a normal shutdown, found {locks:?}");
}

