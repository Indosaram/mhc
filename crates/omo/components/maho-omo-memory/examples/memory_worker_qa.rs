pub use maho_omo_memory::worker::{
    memory_run_supervisor, run_artifacts, run_sentinel, supervisor_process_identity,
    supervisor_test_signals,
};
use std::{collections::BTreeMap, path::Path};
#[path = "../src/worker/fixtures/mod.rs"]
pub mod fixtures;
#[path = "../src/worker/memory_run_supervisor_ic8_exit_resources.rs"]
pub mod memory_run_supervisor_ic8_exit_resources;
#[path = "../src/worker/memory_run_supervisor_ic8_harness.rs"]
pub mod memory_run_supervisor_ic8_harness;
#[path = "../src/worker/memory_run_supervisor_ic8_process_groups.rs"]
pub mod memory_run_supervisor_ic8_process_groups;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    match run().await {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}

async fn run() -> Result<i32, String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let env: BTreeMap<String, String> = std::env::vars().collect();
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    match args.first().map(String::as_str) {
        Some("--qa") => qa(&executable).await,
        Some("--qa-facts") => {
            qa_facts(&executable).await?;
            Ok(0)
        }
        Some("--qa-facts-broker-child") => {
            qa_facts_broker_child(&args).await?;
            Ok(0)
        }
        Some("--qa-facts-model") => {
            if args.iter().any(|arg| arg == "--list-models") {
                println!("fixture/facts");
                Ok(0)
            } else {
                fixtures::facts_child::run("fact", &env)
            }
        }
        Some("--qa-reflection-model") => {
            if args.iter().any(|arg| arg == "--list-models") {
                println!("fixture/reflection");
                Ok(0)
            } else {
                fixtures::reflection_child::run("commit", &env).await
            }
        }
        Some("--qa-dream-model") => {
            if args.iter().any(|arg| arg == "--list-models") {
                println!("fixture/reflection");
                Ok(0)
            } else {
                fixtures::dream_child::run(&env).await?;
                Ok(0)
            }
        }
        Some("--fixture-supervisor-child") => {
            fixtures::supervisor_child::run(
                required(&args, 1)?,
                Path::new(required(&args, 2)?),
                &env,
            )
            .await
        }
        Some("--fixture-taskkill") => {
            #[cfg(unix)]
            {
                fixtures::supervisor_taskkill::run(
                    &args[1..],
                    Path::new(
                        env.get("OMO_MEMORY_SUPERVISOR_TASKKILL_RUN_DIR")
                            .ok_or("taskkill run directory missing")?,
                    ),
                )?;
                Ok(0)
            }
            #[cfg(not(unix))]
            {
                Err("injected taskkill fixture requires Unix".into())
            }
        }
        Some("--fixture-facts") => fixtures::facts_child::run(required(&args, 1)?, &env),
        Some("--fixture-reflection") => {
            fixtures::reflection_child::run(required(&args, 1)?, &env).await
        }
        Some("--fixture-hold-lock") => {
            fixtures::hold_lock::run(Path::new(required(&args, 1)?)).await?;
            Ok(0)
        }
        Some("--fixture-dream") => {
            fixtures::dream_child::run(&env).await?;
            Ok(0)
        }
        Some("--fixture-supervisor-parent") => {
            fixtures::supervisor_parent::run(&executable, &[], Path::new(required(&args, 1)?))
                .await?;
            Ok(0)
        }
        Some("--fixture-finished-child") => {
            fixtures::supervisor_finished_child::run(Path::new(required(&args, 1)?))
                .map_err(|error| error.to_string())?;
            Ok(0)
        }
        Some("--fixture-incomplete-outcome") => {
            fixtures::supervisor_incomplete_outcome_then_fail::run(Path::new(required(&args, 1)?))
                .map_err(|error| error.to_string())
        }
        Some("--fixture-outcome-fail") => {
            fixtures::supervisor_outcome_then_fail::run(Path::new(required(&args, 1)?))
                .map_err(|error| error.to_string())
        }
        Some("--fixture-outcome-linger") => {
            fixtures::supervisor_outcome_then_linger::run(Path::new(required(&args, 1)?))
                .await
                .map_err(|error| error.to_string())?;
            Ok(0)
        }
        Some("--fixture-never-publishes") => {
            fixtures::supervisor_never_publishes::run(Path::new(required(&args, 1)?))
                .await
                .map_err(|error| error.to_string())?;
            Ok(0)
        }
        Some("--fixture-keepalive") => {
            let directory = Path::new(required(&args, 1)?);
            fixtures::supervisor_keepalive::run(directory, &executable, &[], |operation| {
                qa_terminal_gate(directory, operation)
            })
            .await?;
            Ok(0)
        }
        _ => {
            maho_omo_memory::worker::memory_run_supervisor::run_entry(
                &args,
                &executable,
                &[],
                |directory, operation| qa_terminal_gate(directory, operation),
            )
            .await?;
            Ok(0)
        }
    }
}

#[cfg(unix)]
async fn qa_facts_broker_child(args: &[String]) -> Result<(), String> {
    if args.len() != 3 {
        return Err("expected key and content arguments".into());
    }
    let key = &args[1];
    if key != "auth" && key != "settings" {
        return Err("unsupported QA broker key".into());
    }
    let config = maho_core::lockfile_policy::facts_lock_broker_config_from_environment()?
        .ok_or("facts broker config missing")?;
    maho_core::lockfile_policy::install_facts_lock_broker(config.clone());
    let path = if key == "auth" {
        &config.auth_path
    } else {
        &config.settings_path
    };
    let mut guard = tokio::task::spawn_blocking({
        let path = path.to_string_lossy().into_owned();
        move || maho_core::lockfile_policy::acquire_lock_sync(&path, 5000)
    })
    .await
    .map_err(|error| error.to_string())?
    .map_err(|error| error.to_string())?;
    guard
        .broker_write(&args[2])?
        .then_some(())
        .ok_or("broker write did not use lease")?;
    println!("BROKER_CHILD_WRITE key={key}");
    Ok(())
}
#[cfg(not(unix))]
async fn qa_facts_broker_child(_: &[String]) -> Result<(), String> {
    Err("facts broker QA requires Unix".into())
}

fn qa_terminal_gate(
    directory: &Path,
    operation: &mut dyn FnMut() -> Result<(), String>,
) -> Result<(), String> {
    let record = memory_core::locks::create_lock_record("reflection-finalize", Default::default())
        .map_err(|error| error.to_string())?;
    memory_core::locks::with_lock(
        &directory.join("terminalization.lock"),
        &record,
        &memory_core::locks::AcquireLockOptions {
            wait_timeout_ms: Some(2000),
            ..Default::default()
        },
        operation,
    )
    .map_err(|error| error.to_string())
}

fn required(args: &[String], index: usize) -> Result<&str, String> {
    args.get(index)
        .map(String::as_str)
        .ok_or_else(|| format!("missing argument {index}"))
}

async fn qa(executable: &Path) -> Result<i32, String> {
    qa_facts(executable).await?;
    qa_reflection(executable, false).await?;
    qa_reflection(executable, true).await?;
    qa_outcome_authority(executable).await?;
    for platform in memory_run_supervisor_ic8_harness::IC8_PLATFORMS {
        for mode in ["graceful", "stubborn"] {
            let mut harness =
                memory_run_supervisor_ic8_harness::Ic8Harness::new(executable.into(), vec![]);
            let result = async {
                let mut run = harness.make_run(mode).await?;
                let mut supervisor = harness.launch_supervisor(&run, platform)?;
                let pid = supervisor.id().ok_or("supervisor pid missing")?;
                harness
                    .wait_for_path(&run.root.join("child-started.json"))
                    .await?;
                (&mut run.exit.exit_socket_accepted)
                    .await
                    .map_err(|error| error.to_string())?;
                harness.advance_clock(&run, 2000.0)?;
                harness
                    .wait_for_path(&run.root.join("child-terminated.json"))
                    .await?;
                if mode == "stubborn" {
                    harness.advance_clock(&run, 3000.0)?;
                }
                let status = harness.wait_for_exit(&mut supervisor).await?;
                if !status.success() {
                    return Err(format!("{platform}/{mode}: supervisor exited {status}"));
                }
                run.exit
                    .child_exited
                    .await
                    .map_err(|error| error.to_string())??;
                let outcome: run_artifacts::RunOutcome =
                    run_artifacts::read_run_json(&run.root.join("outcome.json"))
                        .map_err(|error| error.to_string())?;
                if !outcome.timed_out || run.root.join("launch.json").exists() {
                    return Err(format!("{platform}/{mode}: incomplete terminal outcome"));
                }
                harness.untrack_process_group(pid);
                Ok::<_, String>(())
            }
            .await;
            let cleanup = harness.cleanup().await;
            result?;
            cleanup?;
            println!("PASS supervisor {platform}/{mode}; resources cleaned");
        }
    }
    Ok(0)
}

async fn qa_outcome_authority(executable: &Path) -> Result<(), String> {
    use maho_omo_memory::worker::spawn_supervisor::{SupervisedChildInput, run_supervised_child};
    for fixture in [
        "--fixture-outcome-fail",
        "--fixture-incomplete-outcome",
        "--fixture-outcome-linger",
        "--fixture-never-publishes",
    ] {
        let root = tempfile::tempdir().map_err(|error| error.to_string())?;
        let args = vec![fixture.into()];
        let env = BTreeMap::new();
        let lingering = matches!(
            fixture,
            "--fixture-outcome-linger" | "--fixture-never-publishes"
        );
        let now = chrono::Utc::now().timestamp_millis() as f64;
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            run_supervised_child(SupervisedChildInput {
                queued_at: now,
                memory_root: root.path().to_string_lossy().into_owned(),
                max_concurrent_runs: None,
                run_dir: root.path(),
                run_id: "authority",
                attempt: 1,
                model: "fixture/model",
                thinking: None,
                next_attempt: None,
                kind: run_artifacts::RunKind::Reflection,
                command: "unused",
                args: &[],
                cwd: root.path(),
                env: &env,
                hard_deadline_at: if fixture == "--fixture-never-publishes" {
                    now - 5001.0
                } else {
                    now + 10000.0
                },
                termination_grace_ms: 0.0,
                max_output_bytes: 1024,
                supervisor_command: executable,
                supervisor_args: &args,
                ledger: serde_json::Map::new(),
            }),
        )
        .await;
        let cleanup = if lingering {
            run_artifacts::write_run_json_atomic(&root.path().join("release"), &true, 0o600)
                .map_err(|error| error.to_string())?;
            maho_omo_memory::worker::supervisor_test_signals::wait_for_filesystem_state(
                root.path(),
                || async { Ok(root.path().join("released.json").exists().then_some(())) },
                5000,
                "fixture release receipt",
            )
            .await
        } else {
            Ok(())
        };
        let result = result.map_err(|error| error.to_string())?;
        cleanup?;
        match fixture {
            "--fixture-outcome-fail" => {
                let child = result?;
                if child.code.is_some()
                    || child.signal.as_deref() != Some("SIGTERM")
                    || !child.timed_out
                {
                    return Err("durable outcome did not override failed supervisor".into());
                }
            }
            "--fixture-incomplete-outcome" => {
                if result.err().as_deref() != Some("memory run supervisor exited with 1") {
                    return Err("incomplete outcome authorized supervisor exit".into());
                }
            }
            "--fixture-outcome-linger" => {
                let child = result?;
                if child.code != Some(0) || child.signal.is_some() || child.timed_out {
                    return Err("durable outcome did not complete before supervisor close".into());
                }
            }
            "--fixture-never-publishes" => {
                if result.err().as_deref()
                    != Some("memory run supervisor did not publish an outcome before its deadline")
                {
                    return Err("missing publication deadline was not reported".into());
                }
            }
            _ => unreachable!(),
        }
        println!("PASS supervisor authority {fixture}; release receipt cleaned");
    }
    Ok(())
}

async fn qa_facts(executable: &Path) -> Result<(), String> {
    use maho_omo_memory::{
        facts_runner::{FactsExtractorRunner, NativeFactsAttemptOptions},
        facts_wiring::FactsExtractorPort,
    };
    use memory_core::journal::entries::{TextTranscriptEntry, TranscriptEntry};
    let evidence_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../target/linux-facts-sandbox-qa");
    std::fs::create_dir_all(&evidence_root).map_err(|error| {
        format!(
            "prepare isolated QA root {}: {error}",
            evidence_root.display()
        )
    })?;
    let root = tempfile::Builder::new()
        .prefix("native-facts-")
        .tempdir_in(&evidence_root)
        .map_err(|error| {
            format!(
                "create isolated native facts run root under {}: {error}",
                evidence_root.display()
            )
        })?;
    let home = root.path().join("home");
    let agent = home.join(".omo/agent");
    std::fs::create_dir_all(&agent).map_err(|error| error.to_string())?;
    let endpoint = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|error| format!("bind faux provider loopback: {error}"))?;
    let address = endpoint
        .local_addr()
        .map_err(|error| format!("read faux provider address: {error}"))?;
    std::fs::write(agent.join("models.json"),serde_json::json!({"providers":{"offline":{"api":"openai-completions","baseUrl":format!("http://{address}/v1"),"apiKey":"offline-fixture","models":[{"id":"offline","reasoning":false,"input":["text"],"contextWindow":128000,"maxTokens":4096}]}}}).to_string()).map_err(|error|error.to_string())?;
    std::fs::write(
        agent.join("settings.jsonc"),
        "{ // global source fixture\n}\n",
    )
    .map_err(|error| error.to_string())?;
    let identity = memory_core::identity::resolve::MemoryIdentity {
        id: "agent".into(),
        safe_slug: "agent".into(),
        paths: memory_core::identity::layout::build_identity_paths(root.path(), "agent"),
    };
    let runner = std::sync::Arc::new(FactsExtractorRunner::new(
        identity,
        std::sync::Arc::new(|| 0),
    ));
    runner
        .queue
        .enqueue(memory_core::facts::queue::FactsEnqueueRequest {
            identity: "agent".into(),
            session_id: "session".into(),
            conversation_id: "conversation".into(),
            signal: None,
            entries: vec![
                TranscriptEntry::Text(TextTranscriptEntry::new(
                    "user",
                    "Question",
                    "1970-01-01T00:00:00.000Z",
                    "m:user",
                    "m",
                )),
                TranscriptEntry::Text(TextTranscriptEntry::new(
                    "assistant",
                    "Answer",
                    "1970-01-01T00:00:00.000Z",
                    "m:assistant",
                    "m",
                )),
            ],
        })
        .map_err(|error| error.to_string())?;
    let warnings = std::sync::Arc::new(std::sync::Mutex::new(vec![]));
    let seen = warnings.clone();
    let sandbox_receipts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let receipts = sandbox_receipts.clone();
    let agent_dir = agent.clone();
    let broker_dir = std::sync::Arc::new(std::sync::Mutex::new(None::<std::path::PathBuf>));
    let broker_dir_capture = broker_dir.clone();
    let mounts = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let mounts_capture = mounts.clone();
    let sandbox = std::sync::Arc::new(
        move |args: maho_omo_memory::worker::spawn_types::FactsSpawnArgs| {
            if !args.paths.payload.exists() {
                return Err("facts sandbox ran before payload publication".into());
            }
            let endpoint = args
                .env
                .get("MAHO_FACTS_LOCK_BROKER")
                .ok_or("facts broker endpoint missing")?;
            let broker = Path::new(endpoint)
                .parent()
                .ok_or("facts broker directory missing")?
                .to_owned();
            *broker_dir_capture
                .lock()
                .map_err(|error| error.to_string())? = Some(broker.clone());
            let transformed = maho_omo_memory::sandbox::apply_facts_sandbox(
                args,
                maho_omo_memory::sandbox::FactsSpawnSandboxInput {
                    policy: maho_omo_memory::sandbox::SandboxPolicy::Required,
                    agent_dir: &agent_dir,
                    broker_dir: &broker,
                    foreign_roots: &[],
                    platform: "linux",
                },
                &|_| Some("/usr/bin/bwrap".into()),
                |_, _| {},
            )
            .map_err(|error| error.to_string())?;
            if transformed.command != "/usr/bin/bwrap" {
                return Err("facts QA command was not wrapped by bwrap".into());
            }
            let separator = transformed
                .args
                .iter()
                .position(|argument| argument == "--")
                .ok_or("bwrap separator missing")?;
            let mut probe = std::process::Command::new(&transformed.command);
            probe
                .args(&transformed.args[..=separator])
                .args([
                    "/bin/sh",
                    "-c",
                    &format!("printf denied > '{}/qa-kernel-denial'", agent_dir.display()),
                ])
                .env("LC_ALL", "C");
            let probe = probe
                .output()
                .map_err(|error| format!("launch bwrap sibling denial probe: {error}"))?;
            if probe.status.success() {
                return Err("bwrap permitted an unrelated agent sibling write".into());
            }
            let denial = String::from_utf8_lossy(&probe.stderr);
            if !denial.contains("Read-only file system") && !denial.contains("Permission denied") {
                return Err(format!(
                    "bwrap sibling write failed for unexpected reason: {denial}"
                ));
            }
            if agent_dir.join("qa-kernel-denial").exists() {
                return Err("bwrap sibling denial probe created the host file".into());
            }
            eprintln!(
                "BWRAP_SIBLING_DENIAL_EXIT={}; STDERR={}",
                probe.status,
                denial.trim()
            );
            *mounts_capture.lock().map_err(|error| error.to_string())? = transformed.args.clone();
            receipts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(transformed)
        },
    );
    let mut port = runner.clone().native_extractor(NativeFactsAttemptOptions {
        resolve_model: std::sync::Arc::new(|| {
            Ok(
                maho_omo_memory::worker::resolve_model::ReflectionModelResolution::Resolved {
                    category: "quick".into(),
                    model: "offline/offline".into(),
                    thinking: None,
                    source: None,
                    fallbacks: vec![],
                },
            )
        }),
        env: BTreeMap::from([
            ("HOME".into(), home.to_string_lossy().into_owned()),
            (
                "MAHO_CODING_AGENT_DIR".into(),
                agent.to_string_lossy().into_owned(),
            ),
            ("PATH".into(), "/usr/bin:/bin".into()),
        ]),
        config_sources: vec![],
        launch: maho_omo_memory::worker::model_preflight::Launcher {
            command: std::env::var_os("MHC_BINARY")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| {
                    executable
                        .parent()
                        .and_then(Path::parent)
                        .unwrap_or(Path::new("."))
                        .join("mhc")
                })
                .to_string_lossy()
                .into_owned(),
            prefix_args: vec![],
        },
        supervisor_command: executable.into(),
        supervisor_args: vec![],
        deadline_ms: 60_000,
        termination_grace_ms: 100,
        max_output_bytes: 4096,
        people: memory_core::facts::person_routing::FactsPeopleRouting {
            enabled: true,
            max_entries: 40,
            max_entry_chars: 200,
        },
        sandbox: Some(sandbox),
        warn: std::sync::Arc::new(move |error| {
            seen.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(error.to_owned());
        }),
        spawner: None,
    });
    let qa_agent = agent.clone();
    let server = tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        async fn read_request(
            stream: &mut tokio::net::TcpStream,
        ) -> Result<serde_json::Value, String> {
            let mut request = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let n = stream
                    .read(&mut chunk)
                    .await
                    .map_err(|error| error.to_string())?;
                if n == 0 {
                    return Err("native CLI closed provider request early".into());
                }
                request.extend_from_slice(&chunk[..n]);
                if let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&request[..end]);
                    let length = headers
                        .lines()
                        .find_map(|line| {
                            line.split_once(':')
                                .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                                .and_then(|(_, value)| value.trim().parse::<usize>().ok())
                        })
                        .ok_or("provider request missing content length")?;
                    if request.len() >= end + 4 + length {
                        return serde_json::from_slice(&request[end + 4..end + 4 + length])
                            .map_err(|error| error.to_string());
                    }
                }
            }
        }
        async fn reply(stream: &mut tokio::net::TcpStream, body: &str) -> Result<(), String> {
            stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.map_err(|error|error.to_string())
        }
        let (mut stream, _) =
            tokio::time::timeout(std::time::Duration::from_secs(10), endpoint.accept())
                .await
                .map_err(|error| format!("provider stage 1 accept timed out: {error}"))?
                .map_err(|error| error.to_string())?;
        let request = read_request(&mut stream).await?;
        let user = request["messages"]
            .as_array()
            .and_then(|messages| messages.iter().find(|message| message["role"] == "user"))
            .ok_or("facts user prompt missing")?;
        let content = &user["content"];
        let text = content
            .as_str()
            .or_else(|| {
                content
                    .as_array()
                    .and_then(|parts| parts.iter().find_map(|part| part["text"].as_str()))
            })
            .ok_or("facts user prompt text missing")?;
        let extraction = text
            .split("write only ")
            .nth(1)
            .and_then(|tail| tail.split_whitespace().next())
            .ok_or("extraction target missing from prompt")?;
        if !text.contains("facts-payload.json") {
            return Err("facts payload path was not sent in real CLI prompt".into());
        }
        let write_arguments = serde_json::json!({"path":extraction,"content":"{\"scope\":\"project\",\"text\":\"isolated facts extraction\",\"date\":\"2026-10-11\"}\n"});
        let sibling = qa_agent.join("qa-sibling-must-not-exist.txt");
        let denied_arguments = serde_json::json!({"path":sibling,"content":"denied"});
        let calls = serde_json::json!([{ "index":0,"id":"call_write","type":"function","function":{"name":"write","arguments":write_arguments.to_string()}},{"index":1,"id":"call_denied","type":"function","function":{"name":"write","arguments":denied_arguments.to_string()}}]);
        let calls = serde_json::to_string(&calls).map_err(|error| error.to_string())?;
        let tool_body = format!(
            "data: {{\"id\":\"qa\",\"object\":\"chat.completion.chunk\",\"created\":0,\"model\":\"offline\",\"choices\":[{{\"index\":0,\"delta\":{{\"role\":\"assistant\",\"tool_calls\":{calls}}},\"finish_reason\":null}}]}}\n\ndata: {{\"id\":\"qa\",\"object\":\"chat.completion.chunk\",\"created\":0,\"model\":\"offline\",\"choices\":[{{\"index\":0,\"delta\":{{}},\"finish_reason\":\"tool_calls\"}}]}}\n\ndata: [DONE]\n\n"
        );
        reply(&mut stream, &tool_body).await?;
        let (mut stream, _) =
            tokio::time::timeout(std::time::Duration::from_secs(10), endpoint.accept())
                .await
                .map_err(|error| format!("provider stage 2 follow-up accept timed out: {error}"))?
                .map_err(|error| error.to_string())?;
        let followup = read_request(&mut stream).await?;
        let tool_results = followup["messages"]
            .as_array()
            .ok_or("follow-up messages missing")?
            .iter()
            .filter(|message| message["role"] == "tool")
            .collect::<Vec<_>>();
        let successful = tool_results
            .iter()
            .find(|message| message["tool_call_id"] == "call_write")
            .ok_or("facts write tool result missing")?;
        let denied = tool_results
            .iter()
            .find(|message| message["tool_call_id"] == "call_denied")
            .ok_or("sibling write tool result missing")?;
        if successful["content"]
            .as_str()
            .is_none_or(|content| !content.contains("Successfully wrote"))
        {
            return Err("facts extraction write tool failed".into());
        }
        if denied["content"].as_str().is_none_or(|content| {
            !content.contains("Failed")
                && !content.contains("Permission denied")
                && !content.contains("Read-only")
        }) {
            return Err("sandboxed CLI write to unrelated sibling was not denied".into());
        }
        if sibling.exists() {
            return Err("denied sibling write changed agent directory".into());
        }
        let final_body = "data: {\"id\":\"qa\",\"object\":\"chat.completion.chunk\",\"created\":0,\"model\":\"offline\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"Extraction complete.\"},\"finish_reason\":null}]}\n\ndata: {\"id\":\"qa\",\"object\":\"chat.completion.chunk\",\"created\":0,\"model\":\"offline\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
        reply(&mut stream, final_body).await?;
        Ok::<(), String>(())
    });
    let mut server = server;
    let launch = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        port.launch_pending(None),
    )
    .await;
    let launch_error = match launch {
        Ok(Ok(())) => None,
        Ok(Err(error)) => Some(error),
        Err(error) => Some(format!("production facts runner timed out: {error}")),
    };
    if let Some(error) = launch_error {
        server.abort();
        let _ = server.await;
        let failures = runner
            .failures
            .read_failures()
            .map_err(|read| read.to_string())?;
        let details = failures
            .entries
            .iter()
            .filter_map(|entry| entry.last_detail.as_deref())
            .map(|detail| detail.replace("offline-fixture", "[REDACTED]"))
            .collect::<Vec<_>>();
        return Err(format!(
            "native facts run error: {error}; durable child details: {details:?}; isolated root {}",
            root.path().display()
        ));
    }
    let runs_path = runner.identity.paths.facts.join("runs");
    let runs = match std::fs::read_dir(&runs_path)
        .map_err(|error| format!("read QA runs {}: {error}", runs_path.display()))
        .and_then(|entries| {
            entries
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| error.to_string())
        }) {
        Ok(runs) => runs,
        Err(error) => {
            server.abort();
            let _ = server.await;
            return Err(format!("inspect native run: {error}"));
        }
    };
    if runs.len() != 1 {
        server.abort();
        let _ = server.await;
        return Err(format!(
            "production facts run count {} before provider completion; isolated root {}",
            runs.len(),
            root.path().display()
        ));
    }
    let precheck = runs[0].path();
    if !precheck.join("final.json").exists() {
        let stderr = std::fs::read_to_string(precheck.join("child-stderr.log")).unwrap_or_default();
        server.abort();
        let _ = server.await;
        return Err(format!(
            "facts child has no final sentinel; stderr={} isolated root={}",
            stderr.replace("offline-fixture", "[REDACTED]"),
            root.path().display()
        ));
    }
    let early: maho_omo_memory::facts_runner_types::FactsFinalRecord =
        match run_artifacts::read_run_json(&precheck.join("final.json")) {
            Ok(record) => record,
            Err(error) => {
                server.abort();
                let _ = server.await;
                return Err(format!("read native final record: {error}"));
            }
        };
    if early.outcome != maho_omo_memory::facts_runner_types::FactsTerminalOutcome::Committed {
        server.abort();
        let _ = server.await;
        let failures = runner
            .failures
            .read_failures()
            .map_err(|error| error.to_string())?;
        let details = failures
            .entries
            .iter()
            .filter_map(|entry| entry.last_detail.as_deref())
            .map(|detail| detail.replace("offline-fixture", "[REDACTED]"))
            .collect::<Vec<_>>();
        let stderr = std::fs::read_to_string(precheck.join("child-stderr.log"))
            .map_err(|error| format!("read child stderr: {error}"))?
            .replace("offline-fixture", "[REDACTED]");
        return Err(format!(
            "facts child finalized {:?}; failure details={details:?}; child stderr={stderr}; isolated root {}",
            early.outcome,
            root.path().display()
        ));
    }
    match tokio::time::timeout(std::time::Duration::from_secs(3), &mut server).await {
        Ok(Ok(Ok(()))) => {}
        Ok(Ok(Err(error))) => return Err(error),
        Ok(Err(error)) => return Err(error.to_string()),
        Err(error) => {
            server.abort();
            let _ = server.await;
            return Err(format!("provider fixture server timed out: {error}"));
        }
    }
    if !runner
        .queue
        .list_pending()
        .map_err(|error| error.to_string())?
        .is_empty()
    {
        return Err("facts QA queue was not consumed".into());
    }
    if sandbox_receipts.load(std::sync::atomic::Ordering::SeqCst) != 1 {
        return Err("facts QA did not apply exactly one bwrap facts sandbox".into());
    }
    let runs = std::fs::read_dir(runner.identity.paths.facts.join("runs"))
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    if runs.len() != 1 {
        return Err(format!("facts QA expected one run, got {}", runs.len()));
    }
    let dir = runs[0].path();
    let record: maho_omo_memory::facts_runner_types::FactsFinalRecord =
        run_artifacts::read_run_json(&dir.join("final.json")).map_err(|error| error.to_string())?;
    if record.outcome != maho_omo_memory::facts_runner_types::FactsTerminalOutcome::Committed
        || record.sha.is_none()
        || dir.join("launch.json").exists()
        || dir.join("facts-payload.json").exists()
    {
        let stderr = std::fs::read_to_string(dir.join("child-stderr.log")).unwrap_or_default();
        let safe = stderr.replace("offline-fixture", "[REDACTED]");
        return Err(format!(
            "facts QA incomplete terminal commit or cleanup; stderr={}",
            safe.chars().take(4000).collect::<String>()
        ));
    }
    let extraction =
        std::fs::read_to_string(dir.join("extraction.jsonl")).map_err(|error| error.to_string())?;
    let records = memory_core::facts::extraction::parse_facts_extraction_jsonl(&extraction)
        .map_err(|error| error.to_string())?;
    if records.len() != 1
        || !matches!(&records[0],memory_core::facts::extraction::FactsExtractionRecord::Project{text,date,..} if text=="isolated facts extraction"&&date=="2026-10-11")
    {
        return Err(
            "native CLI write tool did not persist the expected project extraction record".into(),
        );
    }
    #[cfg(unix)]
    qa_bwrap_broker_writes(executable, &agent, &dir).await?;
    println!(
        "FACTS_BWRAP_MOUNTS={:?}",
        mounts.lock().map_err(|error| error.to_string())?
    );
    let warnings = warnings.lock().map_err(|error| error.to_string())?;
    if !warnings.is_empty() {
        return Err(format!("facts QA warnings: {warnings:?}"));
    }
    if broker_dir
        .lock()
        .map_err(|error| error.to_string())?
        .as_ref()
        .is_some_and(|path| path.exists())
    {
        return Err("facts QA broker socket directory was not cleaned".into());
    }
    println!("PASS native facts bwrap queue/model/supervisor; broker socket resources cleaned");
    Ok(())
}

#[cfg(unix)]
async fn qa_bwrap_broker_writes(
    executable: &Path,
    agent: &Path,
    run_dir: &Path,
) -> Result<(), String> {
    use maho_omo_memory::worker::facts_lock_broker::{FactsLockBroker, FactsLockBrokerTask};
    use maho_omo_memory::{
        sandbox::{FactsSpawnSandboxInput, SandboxPolicy, apply_facts_sandbox},
        worker::spawn_types::{FactsSpawnArgs, FactsSpawnPaths},
    };
    let auth = agent.join("auth.json");
    let settings = agent.join("settings.jsonc");
    std::fs::write(&auth, "{\"before\":true}\n").map_err(|error| error.to_string())?;
    std::fs::write(&settings, "{\"before\":true}\n").map_err(|error| error.to_string())?;
    let broker = std::sync::Arc::new(FactsLockBroker::bind(&auth, &settings)?);
    let endpoint = broker.endpoint().to_owned();
    let socket_dir = endpoint
        .parent()
        .ok_or("broker socket directory missing")?
        .to_owned();
    let mut owner = FactsLockBrokerTask::start(broker);
    let mut env: BTreeMap<String, String> = std::env::vars().collect();
    env.insert(
        "MAHO_FACTS_LOCK_BROKER".into(),
        endpoint.to_string_lossy().into_owned(),
    );
    env.insert(
        "MAHO_FACTS_AUTH_LOCK_TARGET".into(),
        auth.to_string_lossy().into_owned(),
    );
    env.insert(
        "MAHO_FACTS_SETTINGS_LOCK_TARGET".into(),
        settings.to_string_lossy().into_owned(),
    );
    let result =
        async {
            for (key, path, value) in [
                ("auth", &auth, "{\"synthetic_auth\":\"written\"}\n"),
                (
                    "settings",
                    &settings,
                    "{\"synthetic_settings\":\"written\"}\n",
                ),
            ] {
                let child = FactsSpawnArgs {
                    run_id: "broker-qa".into(),
                    attempt: 1,
                    hard_deadline_at: 0.0,
                    model: "qa".into(),
                    thinking: None,
                    load_extensions: None,
                    next_attempt: None,
                    command: executable.to_string_lossy().into_owned(),
                    args: vec!["--qa-facts-broker-child".into(), key.into(), value.into()],
                    cwd: run_dir.into(),
                    env: env.clone(),
                    paths: FactsSpawnPaths {
                        run_dir: run_dir.into(),
                        payload: run_dir.join("facts-payload.json"),
                        extraction: run_dir.join("extraction.jsonl"),
                    },
                };
                let transformed = apply_facts_sandbox(
                    child,
                    FactsSpawnSandboxInput {
                        policy: SandboxPolicy::Required,
                        agent_dir: agent,
                        broker_dir: &socket_dir,
                        foreign_roots: &[],
                        platform: "linux",
                    },
                    &|name| matches!(name, "bwrap").then(|| "/usr/bin/bwrap".into()),
                    |_, _| {},
                )
                .map_err(|error| error.to_string())?;
                if transformed.command != "/usr/bin/bwrap" {
                    return Err("broker helper was not wrapped in bwrap".into());
                }
                let mut child = tokio::process::Command::new(&transformed.command)
                    .args(&transformed.args)
                    .envs(&transformed.env)
                    .kill_on_drop(true)
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn()
                    .map_err(|error| error.to_string())?;
                let mut stdout = child.stdout.take().ok_or("broker helper stdout missing")?;
                let mut stderr = child.stderr.take().ok_or("broker helper stderr missing")?;
                let status =
                    match tokio::time::timeout(std::time::Duration::from_secs(15), child.wait())
                        .await
                    {
                        Ok(result) => result.map_err(|error| error.to_string())?,
                        Err(error) => {
                            child.kill().await.map_err(|kill| {
                                format!("{key} helper timeout {error}; kill failed: {kill}")
                            })?;
                            let _ = child.wait().await;
                            return Err(format!("{key} broker child timed out: {error}"));
                        }
                    };
                let mut output = Vec::new();
                let mut errors = Vec::new();
                tokio::io::AsyncReadExt::read_to_end(&mut stdout, &mut output)
                    .await
                    .map_err(|error| error.to_string())?;
                tokio::io::AsyncReadExt::read_to_end(&mut stderr, &mut errors)
                    .await
                    .map_err(|error| error.to_string())?;
                if !status.success() {
                    return Err(format!(
                        "{key} broker child failed: {}",
                        String::from_utf8_lossy(&errors)
                    ));
                }
                let receipt = String::from_utf8_lossy(&output);
                if !receipt.contains(&format!("BROKER_CHILD_WRITE key={key}")) {
                    return Err(format!("{key} broker child receipt missing: {receipt}"));
                }
                let written = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
                let json: serde_json::Value =
                    serde_json::from_str(&written).map_err(|error| error.to_string())?;
                if json[format!("synthetic_{key}")] != "written" {
                    return Err(format!("{key} host file missing brokered mutation"));
                }
                println!("PASS bwrap broker mutation key={key}");
            }
            Ok::<(), String>(())
        }
        .await;
    owner.close().await?;
    result?;
    if endpoint.exists() || socket_dir.exists() {
        return Err("broker socket resources remained after shutdown".into());
    }
    for path in [&auth, &settings] {
        let lock = path.with_file_name(format!(
            "{}.lock",
            path.file_name()
                .ok_or("lock target missing filename")?
                .to_string_lossy()
        ));
        if lock.exists() {
            return Err(format!(
                "canonical host lock {} remained after broker shutdown",
                lock.display()
            ));
        }
    }
    println!("PASS bwrap broker canonical locks and socket directory cleaned");
    Ok(())
}

async fn qa_reflection(executable: &Path, dream: bool) -> Result<(), String> {
    use maho_omo_memory::worker::{
        model_preflight::{Launcher, ModelPreflight},
        resolve_model::ReflectionModelResolution,
        runner::{
            NativeReflectionCallbacks, NativeReflectionExecutionOptions, ReflectionRunnerInput,
            SenpiSubprocessRunner,
        },
        spawn_supervisor::ReflectionChildOptions,
    };
    let qa_base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../target/linux-facts-bwrap");
    std::fs::create_dir_all(&qa_base)
        .map_err(|error| format!("create visible QA base {}: {error}", qa_base.display()))?;
    let root = tempfile::Builder::new()
        .prefix("facts-")
        .tempdir_in(&qa_base)
        .map_err(|error| error.to_string())?;
    let context = maho_omo_memory::context::MemoryIdentityContext::new(
        "agent".into(),
        memory_core::identity::layout::build_identity_paths(root.path(), "agent"),
        maho_omo_memory::binding::create_memory_binding("agent", "repo", 0.0),
    );
    let settings = maho_omo_memory::reflection_settings::resolve_memory_settings(None)?;
    let store =
        maho_omo_memory::identity_runtime::create_identity_reservation_store(&context, &settings)?;
    let run = store
        .try_reserve(
            memory_core::reflection::ReflectionRequest {
                trigger: if dream {
                    memory_core::reflection::ReflectionTrigger::Dream
                } else {
                    memory_core::reflection::ReflectionTrigger::Manual
                },
                origin: dream.then_some(memory_core::reflection::DreamOrigin::Manual),
                conversation_ids: vec![],
                snapshots: vec![],
                focus: None,
                recent_n: None,
                target_doc: dream.then(|| "system/persona.md".into()),
            },
            None,
        )
        .map_err(|error| error.to_string())?
        .run;
    let identity = maho_omo_memory::identity_runtime::as_memory_identity(&context);
    let config = serde_json::json!({"memory":settings});
    let mut env: std::collections::BTreeMap<String, String> = std::env::vars().collect();
    if dream {
        env.insert("DREAM_FIXTURE_EMPTY_LEDGERS".into(), "1".into());
    }
    let launcher = Launcher {
        command: executable.to_string_lossy().into_owned(),
        prefix_args: vec![
            if dream {
                "--qa-dream-model"
            } else {
                "--qa-reflection-model"
            }
            .into(),
        ],
    };
    let now = || chrono::Utc::now().timestamp_millis();
    let started = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let resolution = ReflectionModelResolution::Resolved {
        category: "quick".into(),
        model: "fixture/reflection".into(),
        thinking: None,
        source: None,
        fallbacks: vec![],
    };
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        SenpiSubprocessRunner::default().launch_native(
            ReflectionRunnerInput {
                run: &run,
                identity: &identity,
                config: &config,
                resolution: &resolution,
                reservation: store.as_ref(),
                started_at: &started,
                now_ms: &now,
            },
            &mut ModelPreflight::default(),
            NativeReflectionExecutionOptions {
                env: &env,
                sources: &[],
                launcher: &launcher,
                parent_session_file: None,
                parent_cwd: None,
                route: None,
                sandbox: None,
                deadline_ms: Some(60_000),
                memory_root: None,
                child: ReflectionChildOptions {
                    termination_grace_ms: 100.0,
                    max_output_bytes: 4096,
                    supervisor_command: executable,
                    supervisor_args: &[],
                    launched_at: now(),
                },
                callbacks: NativeReflectionCallbacks {
                    ensure_renderer: &|| {},
                    health_alert: &|_| {},
                    append_launched: &|| Ok(()),
                    warn: &|error| eprintln!("{error}"),
                },
            },
            None,
            |operation| {
                let record = memory_core::locks::create_lock_record(
                    "reflection-finalize",
                    memory_core::locks::CreateLockRecordOptions {
                        run_id: Some(run.run_id.clone()),
                    },
                )
                .map_err(|error| error.to_string())?;
                memory_core::locks::with_lock(
                    &identity
                        .paths
                        .reflection
                        .join("runs")
                        .join(&run.run_id)
                        .join("terminalization.lock"),
                    &record,
                    &memory_core::locks::AcquireLockOptions {
                        wait_timeout_ms: Some(2000),
                        ..Default::default()
                    },
                    operation,
                )
                .map_err(|error| error.to_string())
            },
        ),
    )
    .await
    .map_err(|error| error.to_string())??;
    if result.outcome != "merged" {
        return Err(format!(
            "reflection QA failed: {} {:?}",
            result.outcome, result.detail
        ));
    }
    if dream {
        if !std::fs::read_to_string(identity.paths.repo.join("system/persona.md"))
            .map_err(|error| error.to_string())?
            .contains("Maintained by dream.")
        {
            return Err("dream QA target was not integrated".into());
        }
    } else if !identity.paths.repo.join("system/reflected.md").exists() {
        return Err("reflection QA document missing".into());
    }
    if store
        .read_state()
        .map_err(|error| error.to_string())?
        .active
        .is_some()
    {
        return Err("reflection QA active reservation retained".into());
    }
    let dir = identity.paths.reflection.join("runs").join(&run.run_id);
    if !dir.join("final.json").exists() || dir.join("launch.json").exists() {
        return Err("reflection QA incomplete terminal cleanup".into());
    }
    println!(
        "PASS native {} reservation/model/supervisor/merge; resources cleaned",
        if dream { "dream" } else { "reflection" }
    );
    Ok(())
}
