//! Native Claude stream-json boundary. Owns the child until close or EOF settlement.
use std::{collections::BTreeMap, path::Path, process::Stdio};
use serde_json::{Value, json};
use tokio::{io::{AsyncBufReadExt, AsyncWriteExt, BufReader}, sync::{mpsc, oneshot}};

enum Command {
    Send(Value, oneshot::Sender<anyhow::Result<()>>),
    Request(Value, oneshot::Sender<anyhow::Result<Value>>),
    Close,
}

pub struct SdkQueryHandle {
    commands: mpsc::UnboundedSender<Command>,
    messages: mpsc::UnboundedReceiver<anyhow::Result<Value>>,
    task: Option<tokio::task::JoinHandle<()>>,
    pub initialization: Value,
}

/// Arguments consumed by the native provider's composed query configuration.
pub fn arguments(options: &Value) -> Vec<String> {
    let mut args = ["--output-format", "stream-json", "--verbose", "--input-format", "stream-json"].map(str::to_owned).to_vec();
    for (key, flag) in [("model", "--model"), ("permissionMode", "--permission-mode"), ("effort", "--effort"), ("maxTurns", "--max-turns"), ("maxThinkingTokens", "--max-thinking-tokens")] {
        if let Some(value) = options.get(key).filter(|value| !value.is_null()) {
            args.extend([flag.into(), value.as_str().map(str::to_owned).unwrap_or_else(|| value.to_string())]);
        }
    }
    if let Some(thinking) = options.get("thinking") {
        if let Some(kind) = thinking["type"].as_str() { args.extend(["--thinking".into(), kind.into()]); }
        if let Some(display) = thinking["display"].as_str() { args.extend(["--thinking-display".into(), display.into()]); }
    }
    for (key, flag) in [("resume", "resume"), ("resumeSessionAt", "resume-session-at"), ("sessionId", "session-id")] {
        if let Some(value) = options[key].as_str() { args.push(format!("--{flag}={value}")); }
    }
    for (key, flag) in [("forkSession", "--fork-session"), ("includePartialMessages", "--include-partial-messages")] {
        if options[key] == true { args.push(flag.into()); }
    }
    if let Some(tools) = options["tools"].as_array() { args.extend(["--tools".into(), tools.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(",")]); }
    if let Some(sources) = options["settingSources"].as_array() { args.push(format!("--setting-sources={}", sources.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(","))); }
    let mut extra = options["extraArgs"].as_object().cloned().unwrap_or_default();
    if let Some(settings) = options.get("settings") { extra.insert("settings".into(), settings.clone()); }
    for (key, value) in extra {
        args.push(format!("--{key}"));
        if !value.is_null() { args.push(value.as_str().map(str::to_owned).unwrap_or_else(|| value.to_string())); }
    }
    args
}

async fn write_frame(stdin: &mut tokio::process::ChildStdin, frame: &Value) -> anyhow::Result<()> {
    let mut bytes = serde_json::to_vec(frame)?; bytes.push(b'\n');
    stdin.write_all(&bytes).await?; Ok(())
}

impl SdkQueryHandle {
    pub async fn spawn(executable: &Path, options: &Value, environment: &BTreeMap<String, String>) -> anyhow::Result<Self> {
        let mut env = environment.clone();
        env.entry("CLAUDE_CODE_ENTRYPOINT".into()).or_insert_with(|| "sdk-ts".into());
        env.remove("NODE_OPTIONS");
        if env.get("DEBUG_CLAUDE_AGENT_SDK").is_none_or(|value| value.is_empty() || value == "0") { env.remove("DEBUG"); } else { env.insert("DEBUG".into(), "1".into()); }
        let mut process = tokio::process::Command::new(executable);
        process.args(arguments(options)).env_clear().envs(env).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true);
        if let Some(cwd) = options["cwd"].as_str() { process.current_dir(cwd); }
        let mut child = process.spawn()?;
        let mut stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let (commands, mut command_rx) = mpsc::unbounded_channel();
        let (events, messages) = mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            let mut pending = BTreeMap::<String, oneshot::Sender<anyhow::Result<Value>>>::new();
            let mut sequence = 0_u64;
            let mut closed = false;
            let outcome: anyhow::Result<()> = async {
                loop {
                    tokio::select! {
                        command = command_rx.recv() => match command {
                            Some(Command::Send(frame, reply)) => {
                                let result = write_frame(&mut stdin, &frame).await;
                                let failed = result.is_err(); let _ = reply.send(result);
                                if failed { anyhow::bail!("Claude Code input write failed"); }
                            },
                            Some(Command::Request(request, reply)) => {
                                sequence += 1; let id = format!("native-{sequence}");
                                pending.insert(id.clone(), reply);
                                write_frame(&mut stdin, &json!({"type":"control_request","request_id":id,"request":request})).await?;
                            },
                            Some(Command::Close) | None => { closed = true; child.start_kill()?; return Ok(()); },
                        },
                        line = lines.next_line() => {
                            let Some(line) = line? else { return Ok(()); };
                            let frame: Value = serde_json::from_str(&line)?;
                            match frame["type"].as_str() {
                                Some("control_response") => {
                                    let response = &frame["response"];
                                    if let Some(reply) = response["request_id"].as_str().and_then(|id| pending.remove(id)) {
                                        let result = if response["subtype"] == "success" { Ok(response["response"].clone()) } else { Err(anyhow::anyhow!("{}", response["error"].as_str().unwrap_or("Claude Code control request failed"))) };
                                        let _ = reply.send(result);
                                    }
                                },
                                Some("control_request") => {
                                    write_frame(&mut stdin, &json!({"type":"control_response","response":{"subtype":"error","request_id":frame["request_id"],"error":format!("Unsupported control request subtype: {}",frame["request"]["subtype"].as_str().unwrap_or(""))}})).await?;
                                },
                                Some("keep_alive" | "control_cancel_request") => {},
                                _ => { let _ = events.send(Ok(frame)); },
                            }
                        }
                    }
                }
            }.await;
            // Reap before surfacing terminal failures or closing the event channel.
            if outcome.is_err() { let _ = child.start_kill(); }
            let status = child.wait().await;
            let result = outcome.and_then(|()| {
                let status = status?;
                if !closed && !status.success() { anyhow::bail!("Claude Code process exited with {status}"); }
                Ok(())
            });
            for (_, reply) in pending { let _ = reply.send(Err(anyhow::anyhow!("Query closed before response received"))); }
            if let Err(error) = result { let _ = events.send(Err(error)); }
        });
        let mut handle = Self {commands, messages, task: Some(task), initialization: Value::Null};
        let mut initialize = json!({"subtype":"initialize","hooks":{}});
        if let Some(prompt) = options.get("systemPrompt") {
            if prompt.is_string() { initialize["systemPrompt"] = json!([prompt]); }
            else if prompt["type"] == "preset" { if let Some(append) = prompt.get("append") { initialize["appendSystemPrompt"] = json!([append]); } }
            else { initialize["systemPrompt"] = prompt.clone(); }
        }
        match handle.request(initialize).await {
            Ok(response) => handle.initialization = response,
            Err(error) => { handle.close().await?; return Err(error); },
        }
        Ok(handle)
    }
    pub async fn send(&self, message: Value) -> anyhow::Result<()> {
        let (reply, result) = oneshot::channel(); self.commands.send(Command::Send(message, reply)).map_err(|_| anyhow::anyhow!("Query closed"))?;
        result.await.map_err(|_| anyhow::anyhow!("Query closed"))?
    }
    pub async fn request(&self, request: Value) -> anyhow::Result<Value> {
        let (reply, result) = oneshot::channel(); self.commands.send(Command::Request(request, reply)).map_err(|_| anyhow::anyhow!("Query closed"))?;
        result.await.map_err(|_| anyhow::anyhow!("Query closed"))?
    }
    pub async fn interrupt(&self) -> anyhow::Result<Value> { self.request(json!({"subtype":"interrupt"})).await }
    pub async fn set_model(&self, model: &str) -> anyhow::Result<()> { self.request(json!({"subtype":"set_model","model":model})).await?; Ok(()) }
    pub async fn next(&mut self) -> Option<anyhow::Result<Value>> { self.messages.recv().await }
    pub async fn close(mut self) -> anyhow::Result<()> {
        let _ = self.commands.send(Command::Close);
        if let Some(task) = self.task.take() { task.await?; } Ok(())
    }
}
impl Drop for SdkQueryHandle {
    fn drop(&mut self) { let _ = self.commands.send(Command::Close); }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    #[tokio::test]
    async fn real_child_initializes_streams_correlates_controls_and_reaps() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().expect("dir"); let script = directory.path().join("claude");
        std::fs::write(&script, "#!/usr/bin/python3\nimport sys,json\nfor line in sys.stdin:\n f=json.loads(line)\n if f['type']=='control_request':\n  r=f['request']; result={'ready':True} if r['subtype']=='initialize' else {'still_queued':[]}\n  print(json.dumps({'type':'control_response','response':{'subtype':'success','request_id':f['request_id'],'response':result}}),flush=True)\n else:\n  print(json.dumps({'type':'result','subtype':'success','result':f['message']['content']}),flush=True)\n").expect("script");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).expect("permissions");
        let options = json!({"systemPrompt":"exact prompt", "tools":[], "settingSources":[]});
        let mut query = tokio::time::timeout(std::time::Duration::from_secs(5), SdkQueryHandle::spawn(&script, &options, &BTreeMap::new())).await.expect("bounded init").expect("spawn");
        assert_eq!(query.initialization["ready"], true);
        query.send(json!({"type":"user","message":{"role":"user","content":"hello"}})).await.expect("send");
        assert_eq!(query.next().await.expect("frame").expect("message")["result"], "hello");
        assert_eq!(query.interrupt().await.expect("interrupt")["still_queued"], json!([]));
        query.set_model("claude-test").await.expect("model"); query.close().await.expect("reaped");
    }
    #[test]
    fn native_spawn_arguments_preserve_empty_tools_and_lineage() {
        let args = arguments(&json!({"tools":[],"settingSources":[],"resume":"parent","forkSession":true,"sessionId":"child","extraArgs":{"strict-mcp-config":null},"settings":{"autoCompactEnabled":true}}));
        assert!(args.windows(2).any(|pair| pair == ["--tools", ""])); assert!(args.contains(&"--setting-sources=".into())); assert!(args.contains(&"--resume=parent".into())); assert!(args.contains(&"--fork-session".into())); assert!(args.contains(&"--strict-mcp-config".into()));
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn abnormal_exit_is_reported_only_after_child_settlement() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().expect("dir"); let script = directory.path().join("claude");
        std::fs::write(&script, "#!/usr/bin/python3\nimport sys,json,os\nf=json.loads(sys.stdin.readline())\nprint(json.dumps({'type':'control_response','response':{'subtype':'success','request_id':f['request_id'],'response':{}}}),flush=True)\nf=json.loads(sys.stdin.readline())\nos.close(1)\nopen('settled','w').write('done')\nos._exit(7)\n").expect("script");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).expect("permissions");
        let mut query = SdkQueryHandle::spawn(&script, &json!({"cwd":directory.path()}), &BTreeMap::new()).await.expect("spawn");
        query.send(json!({"type":"user"})).await.expect("send");
        let failure = tokio::time::timeout(std::time::Duration::from_secs(5), query.next()).await.expect("bounded exit").expect("failure").expect_err("abnormal exit");
        assert!(failure.to_string().contains('7')); assert!(directory.path().join("settled").is_file());
        query.close().await.expect("join");
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn dropped_query_signals_owned_worker_to_reap_child() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().expect("dir"); let script = directory.path().join("claude");
        std::fs::write(&script, "#!/usr/bin/python3\nimport sys,json\nf=json.loads(sys.stdin.readline())\nprint(json.dumps({'type':'control_response','response':{'subtype':'success','request_id':f['request_id'],'response':{}}}),flush=True)\nfor line in sys.stdin: pass\n").expect("script");
        std::fs::set_permissions(&script,std::fs::Permissions::from_mode(0o700)).expect("permissions");
        let mut query = SdkQueryHandle::spawn(&script,&json!({}),&BTreeMap::new()).await.expect("query");
        let worker = query.task.take().expect("worker"); drop(query);
        tokio::time::timeout(std::time::Duration::from_secs(5),worker).await.expect("bounded cleanup").expect("reaped worker");
    }
}
