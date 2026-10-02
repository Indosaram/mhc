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
    args.extend(["--permission-prompt-tool".into(), "stdio".into()]);
    if options["mcpServers"].is_object() { args.extend(["--mcp-config".into(), json!({"mcpServers":options["mcpServers"]}).to_string()]); }
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

async fn settle_child(child: &mut tokio::process::Child, commands: &mut mpsc::UnboundedReceiver<Command>, closed: &mut bool) -> std::io::Result<std::process::ExitStatus> {
    loop {
        tokio::select! {
            status = child.wait() => return status,
            command = commands.recv(), if !*closed => match command {
                Some(Command::Close) | None => { *closed = true; child.start_kill()?; },
                Some(Command::Send(_, reply)) => { let _ = reply.send(Err(anyhow::anyhow!("Query output closed"))); },
                Some(Command::Request(_, reply)) => { let _ = reply.send(Err(anyhow::anyhow!("Query output closed"))); },
            },
        }
    }
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
        let custom_server = options["customTools"].as_array().and_then(|tools| crate::custom_tools::build_custom_tool_server(tools));
        let server_name = custom_server.as_ref().map(|server| server.name);
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
                                    let request = &frame["request"];
                                    let result = match request["subtype"].as_str() {
                                        Some("can_use_tool") => {
                                            let mut denied = crate::tools::deny_tool_execution(); denied["toolUseID"] = request["tool_use_id"].clone(); Ok(denied)
                                        },
                                        Some("mcp_message") => match custom_server.as_ref().filter(|server| request["server_name"] == server.name) {
                                            Some(server) => Ok(json!({"mcp_response":server.rpc(&request["message"])})),
                                            None => Err(format!("SDK MCP server not found: {}",request["server_name"].as_str().unwrap_or(""))),
                                        },
                                        Some("elicitation") => Ok(json!({"action":"decline"})),
                                        _ => Err(format!("Unsupported control request subtype: {}",request["subtype"].as_str().unwrap_or(""))),
                                    };
                                    let response = match result {
                                        Ok(value) => json!({"subtype":"success","request_id":frame["request_id"],"response":value}),
                                        Err(error) => json!({"subtype":"error","request_id":frame["request_id"],"error":error}),
                                    };
                                    write_frame(&mut stdin, &json!({"type":"control_response","response":response})).await?;
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
            let status = settle_child(&mut child, &mut command_rx, &mut closed).await;
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
        if let Some(name) = server_name { initialize["sdkMcpServers"] = json!([name]); }
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

/// Select the newest main transcript lineage, before SDK message projection.
pub fn transcript_lineage(entries: &[Value]) -> Vec<Value> {
    let mut nodes = BTreeMap::new();
    let mut order = Vec::new();
    let mut positions = BTreeMap::new();
    for (index, entry) in entries.iter().enumerate() {
        if !matches!(entry["type"].as_str(), Some("user" | "assistant" | "progress" | "system" | "attachment")) { continue; }
        let Some(uuid) = entry["uuid"].as_str() else { continue; };
        if !nodes.contains_key(uuid) { order.push(uuid.to_owned()); }
        nodes.insert(uuid.to_owned(), entry.clone()); positions.insert(uuid.to_owned(), index);
    }
    for uuid in &order {
        let entry = nodes[uuid].clone();
        if entry["type"] != "system" || entry["subtype"] != "compact_boundary" { continue; }
        let metadata = &entry["compactMetadata"];
        let (anchor, head, tail) = if let Some(preserved) = metadata.get("preservedMessages") {
            let Some(uuids) = preserved["uuids"].as_array().filter(|uuids| !uuids.is_empty()) else { continue; };
            let Some(uuids) = uuids.iter().map(Value::as_str).collect::<Option<Vec<_>>>() else { continue; };
            if uuids.iter().any(|uuid| !nodes.contains_key(*uuid)) { continue; }
            let mut parent = preserved["anchorUuid"].clone();
            for uuid in &uuids { nodes.get_mut(*uuid).expect("preserved node")["parentUuid"] = parent; parent = json!(uuid); }
            (preserved["anchorUuid"].clone(), uuids[0].to_owned(), uuids.last().expect("preserved tail").to_string())
        } else if let Some(segment) = metadata.get("preservedSegment") {
            let (Some(head), Some(tail)) = (segment["headUuid"].as_str(), segment["tailUuid"].as_str()) else { continue; };
            if let Some(node) = nodes.get_mut(head) { node["parentUuid"] = segment["anchorUuid"].clone(); }
            (segment["anchorUuid"].clone(), head.to_owned(), tail.to_owned())
        } else { continue; };
        for (uuid, node) in &mut nodes { if *uuid != head && node["parentUuid"] == anchor { node["parentUuid"] = json!(tail); } }
    }
    let parents: std::collections::BTreeSet<_> = nodes.values().filter_map(|node| node["parentUuid"].as_str()).collect();
    let mut candidates = Vec::new();
    for uuid in &order {
        if parents.contains(uuid.as_str()) { continue; }
        let mut current = Some(uuid.as_str()); let mut seen = std::collections::BTreeSet::new();
        while let Some(uuid) = current {
            if !seen.insert(uuid) { break; }
            let Some(node) = nodes.get(uuid) else { break; };
            if matches!(node["type"].as_str(), Some("user" | "assistant")) { candidates.push(uuid); break; }
            current = node["parentUuid"].as_str();
        }
    }
    let main: Vec<_> = candidates.iter().copied().filter(|uuid| {
        let node = &nodes[*uuid]; node["isSidechain"] != true && node["isMeta"] != true && node["teamName"].as_str().is_none_or(str::is_empty)
    }).collect();
    let candidates = if main.is_empty() { &candidates } else { &main };
    let Some(mut current) = candidates.iter().copied().reduce(|selected, candidate| {
        if positions.get(candidate) > positions.get(selected) { candidate } else { selected }
    }).map(Some) else { return Vec::new(); };
    let mut seen = std::collections::BTreeSet::new(); let mut chain = Vec::new();
    while let Some(uuid) = current {
        if !seen.insert(uuid.to_owned()) { break; }
        let Some(node) = nodes.get(uuid) else { break; };
        chain.push(node.clone()); current = node["parentUuid"].as_str();
    }
    chain.reverse();
    let assistant_id = |node: &Value| {
        if node["type"] == "assistant" { node["message"]["id"].as_str().map(str::to_owned) } else { None }
    };
    let mut last_assistant = BTreeMap::new();
    for node in &chain { if let Some(id) = assistant_id(node) { last_assistant.insert(id, node["uuid"].as_str().expect("uuid").to_owned()); } }
    let mut expanded = BTreeMap::new(); let mut expanded_ids = std::collections::BTreeSet::new();
    for node in &chain {
        let Some(id) = assistant_id(node) else { continue; };
        if !expanded_ids.insert(id.clone()) { continue; }
        let siblings: Vec<_> = order.iter().filter_map(|uuid| { let node=&nodes[uuid]; (assistant_id(node).as_deref()==Some(id.as_str())).then_some(node) }).collect();
        let mut missing: Vec<_> = siblings.iter().copied().filter(|node| !seen.contains(node["uuid"].as_str().expect("uuid"))).cloned().collect();
        let mut tools = Vec::new();
        for sibling in &siblings {
            for uuid in &order {
                let node=&nodes[uuid];
                if node["type"]=="user" && node["parentUuid"]==sibling["uuid"]
                    && node["message"]["content"].as_array().is_some_and(|blocks| blocks.iter().any(|block| block["type"]=="tool_result"))
                    && !seen.contains(uuid.as_str()) { tools.push(node.clone()); }
            }
        }
        missing.sort_by(|a,b|a["timestamp"].as_str().unwrap_or("").cmp(b["timestamp"].as_str().unwrap_or("")));
        tools.sort_by(|a,b|a["timestamp"].as_str().unwrap_or("").cmp(b["timestamp"].as_str().unwrap_or("")));
        missing.extend(tools);
        for node in &missing { seen.insert(node["uuid"].as_str().expect("uuid").to_owned()); }
        expanded.insert(last_assistant[&id].clone(),missing);
    }
    let mut result=Vec::new();
    for node in chain { let uuid=node["uuid"].as_str().expect("uuid").to_owned();result.push(node);if let Some(siblings)=expanded.remove(&uuid) {result.extend(siblings);} }
    result
}

pub fn transcript_project_key(path: &str) -> String {
    let encoded: String = path.encode_utf16().map(|unit| {
        if unit <= 127 && (unit as u8).is_ascii_alphanumeric() { char::from(unit as u8) } else { '-' }
    }).collect();
    if encoded.len() <= 200 { return encoded; }
    let hash = path.encode_utf16().fold(0_i32, |hash, unit| hash.wrapping_mul(31).wrapping_add(i32::from(unit)));
    let mut value = hash.unsigned_abs(); let mut suffix = Vec::new();
    loop {
        suffix.push(char::from(b"0123456789abcdefghijklmnopqrstuvwxyz"[(value % 36) as usize]));
        value /= 36; if value == 0 { break; }
    }
    format!("{}-{}", &encoded[..200], suffix.into_iter().rev().collect::<String>())
}

pub fn transcript_messages(entries: &[Value]) -> Vec<Value> {
    let mut chain=transcript_lineage(entries);
    let source=|node:&Value| {
        if node.get("promptSource").is_some() {return None;}
        let content=&node["message"]["content"];
        let text=content.as_str().or_else(||content.as_array().and_then(|blocks|blocks.iter().rev().find(|block|block["type"]=="text")).and_then(|block|block["text"].as_str()));
        text.and_then(|text|[("<command-name>","record"),("<local-command-stdout>","output"),("<local-command-stderr>","output"),("<local-command-caveat>","caveat")].into_iter().find(|(prefix,_)|text.starts_with(prefix)).map(|(_,source)|source))
    };
    let mut completed=std::collections::BTreeSet::new();
    for (index,node) in chain.iter().enumerate() {
        if node["type"]!="user" || node["isMeta"]!=true || source(node)!=Some("caveat") {continue;}
        let mut record=false;
        for (index,node) in chain.iter().enumerate().skip(index+1) {
            if node["type"]=="assistant" {break;}
            if node["type"]!="user" || node["isMeta"]==true {continue;}
            if source(node)==Some("record") && !record {record=true;}
            else if source(node)!=Some("output") || !record {break;}
            completed.insert(index);
        }
    }
    let mut reply=None;let mut before_reply=vec![false;chain.len()];
    for (index,node) in chain.iter().enumerate().rev() {
        before_reply[index]=reply==Some(true);
        let tool_result=node["type"]=="user" && !node["parentUuid"].is_null() && node["message"]["content"].as_array().is_some_and(|blocks|blocks.iter().any(|block|block["type"]=="tool_result"));
        let interrupted=|text:&str| ["[Request interrupted by user]","[Request interrupted by user for tool use]","[Tool call did not complete: the turn was ended to deliver the message that follows. Nothing refused it; re-run it if still needed.]","The user doesn't want to take this action right now. STOP what you are doing and wait for the user to tell you how to proceed.","[Tool call skipped: the turn was stopped before this call ran, by the check whose denial is on another call in this batch. Nothing refused this call and it had no effects; re-run it if still needed.]","[Tool call skipped: the turn ended to deliver the message that follows before this call ran. Nothing refused it; re-run it if still needed.]"].iter().any(|prefix|text.starts_with(prefix));
        let content=&node["message"]["content"];
        let synthetic=node["type"]=="user" && (content.as_str().is_some_and(interrupted) || content.as_array().is_some_and(|blocks|!blocks.is_empty() && blocks.iter().all(|block| {
            let text=if block["type"]=="text" {block["text"].as_str()}else if block["type"]=="tool_result" && block["is_error"]==true {block["content"].as_str()}else {None};text.is_some_and(interrupted)
        })));
        if node["type"]=="assistant" || tool_result || synthetic {reply=Some(true);}
        else if node["type"]=="user" && node["isMeta"]!=true && node["isCompactSummary"]!=true && !completed.contains(&index) {reply=Some(false);}
    }
    let mut uuids:std::collections::BTreeSet<String>=chain.iter().filter_map(|node|node["uuid"].as_str().map(str::to_owned)).collect();
    for (index,node) in chain.iter_mut().enumerate() {
        if completed.contains(&index) {node["isCompletedLocalCommand"]=json!(true);continue;}
        if !before_reply[index] || node["type"]!="attachment" {continue;}
        let attachment=&node["attachment"];
        let forwarded=&attachment["forwardedIntent"];
        let forwarded=forwarded["lineage"].as_str().is_some_and(|lineage|!lineage.is_empty()) && forwarded.get("source").is_none_or(|source|source.is_string());
        if attachment["type"]!="queued_command" || attachment["isMeta"]==true || forwarded || !(attachment["prompt"].is_string() || attachment["prompt"].is_array()) {continue;}
        let uuid=attachment["source_uuid"].as_str().filter(|uuid|!uuid.is_empty()).unwrap_or_else(||node["uuid"].as_str().expect("uuid")).to_owned();
        if node["uuid"]!=uuid && uuids.contains(&uuid) {continue;}
        uuids.insert(uuid.clone());
        let origin=if attachment["origin"]["kind"].is_string() {attachment["origin"].clone()}else if attachment["commandMode"]=="task-notification" {json!({"kind":"task-notification"})}else {Value::Null};
        *node=json!({"type":"user","uuid":uuid,"parentUuid":node["parentUuid"],"sessionId":node["sessionId"],"timestamp":node["timestamp"],"message":{"role":"user","content":attachment["prompt"]},"isQueuedCommand":true,"isSidechain":node["isSidechain"],"teamName":node["teamName"]});
        if !origin.is_null() {node["origin"]=origin;}
    }
    chain.into_iter().filter(|node|matches!(node["type"].as_str(),Some("user"|"assistant")) && node["isMeta"]!=true && node["isSidechain"]!=true && node["teamName"].as_str().is_none_or(str::is_empty)).map(|node| {
        let mut message=json!({"type":node["type"],"uuid":node["uuid"],"session_id":node["sessionId"],"message":node["message"],"parent_tool_use_id":null,"parent_agent_id":null,"timestamp":node["timestamp"]});
        for flag in ["interruptedByShutdown","isCompactSummary","isQueuedCommand","isCompletedLocalCommand"] {if node[flag]==true {message[flag]=json!(true);}}
        if node["isCompactSummary"]==true || node["isVisibleInTranscriptOnly"]==true {message["is_meta"]=json!(true);}
        if !node["origin"].is_null() {
            let mut origin=node["origin"].clone();
            if origin["kind"]=="task-notification" {
                let mut filtered=json!({"kind":"task-notification"});
                for key in ["subkind","fireReason"] {if let Some(value)=origin.get(key) {filtered[key]=value.clone();}}
                origin=filtered;
            }
            message["origin"]=origin;
        }
        message
    }).collect()
}

pub async fn get_session_messages(session: &str, cwd: &Path, environment: &BTreeMap<String,String>) -> anyhow::Result<Vec<Value>> {
    if !regex::Regex::new(r"(?i)^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$").expect("session UUID").is_match(session) {return Ok(Vec::new());}
    let root=environment.get("CLAUDE_CONFIG_DIR").map(std::path::PathBuf::from).or_else(||environment.get("HOME").map(|home|std::path::PathBuf::from(home).join(".claude"))).ok_or_else(||anyhow::anyhow!("Claude transcript HOME is unavailable"))?.join("projects");
    let original_cwd=cwd.to_string_lossy().into_owned();
    let cwd=std::fs::canonicalize(cwd).unwrap_or_else(|_|cwd.to_owned());
    let mut worktrees=vec![cwd.clone()];
    let mut command=tokio::process::Command::new("git");command.args(["-c","core.hooksPath=/dev/null","-c","core.fsmonitor=","worktree","list","--porcelain"]).current_dir(&cwd).env_clear().envs(environment).stdin(Stdio::null()).stderr(Stdio::null()).kill_on_drop(true);
    if let Ok(Ok(output))=tokio::time::timeout(std::time::Duration::from_secs(5),command.output()).await
        && output.status.success() {
            for path in String::from_utf8_lossy(&output.stdout).lines().filter_map(|line|line.strip_prefix("worktree ")) {
                let path=std::path::PathBuf::from(path);if !worktrees.contains(&path) {worktrees.push(path);}
            }
    }
    let override_key=environment.get("CLAUDE_CODE_PROJECT_DIR_NAME").filter(|key|environment.contains_key("CLAUDE_CONFIG_DIR") && regex::Regex::new(r"^[A-Za-z0-9_-]{1,64}$").expect("project key").is_match(key) && !regex::Regex::new(r"(?i)^(con|prn|aux|nul|com[0-9]|lpt[0-9])$").expect("reserved project key").is_match(key));
    for worktree in worktrees {
        let worktree=std::fs::canonicalize(&worktree).unwrap_or(worktree);
        let path=worktree.to_string_lossy();let key=transcript_project_key(&path);
        let mut projects=Vec::new();if let Some(key)=override_key {projects.push(root.join(key));}projects.push(root.join(&key));
        let windows_alias=cfg!(windows).then(||original_cwd.clone()).filter(|original|original.len()>2 && original.as_bytes()[1]==b':' && original.as_bytes()[0].is_ascii_alphabetic());
        if let Some(original)=windows_alias {
            let parts:Vec<_>=original[2..].split(['\\','/']).filter(|part|!part.is_empty()).collect();
            let canonical_parts:Vec<_>=path.split(['\\','/']).filter(|part|!part.is_empty()).collect();
            if canonical_parts.len()>parts.len() && canonical_parts[canonical_parts.len()-parts.len()..].iter().zip(&parts).all(|(a,b)|a.eq_ignore_ascii_case(b)) {
                let alias=format!("{}:\\{}",&original[..1],parts.join("\\"));let alias=root.join(transcript_project_key(&alias));
                if !projects.contains(&alias) {projects.push(alias);}
            }
        }
        let encoded:String=path.encode_utf16().map(|unit|if unit<=127 && (unit as u8).is_ascii_alphanumeric() {char::from(unit as u8)}else {'-'}).collect();
        if override_key.is_none() && encoded.len()>200
            && let Ok(directories)=std::fs::read_dir(&root) {
                for directory in directories.flatten().filter(|entry|entry.file_type().is_ok_and(|kind|kind.is_dir())) {
                    if !directory.file_name().to_string_lossy().starts_with(&format!("{}-",&encoded[..200])) || projects.contains(&directory.path()) {continue;}
                    let matches=std::fs::read_dir(directory.path()).is_ok_and(|files|files.flatten().filter(|entry|entry.path().extension().is_some_and(|extension|extension=="jsonl")).any(|file| {
                        let Ok(file)=std::fs::File::open(file.path())else {return false;};
                        let mut relocated=None;let mut recorded=None;
                        for line in std::io::BufRead::lines(std::io::BufReader::new(file)) {
                            let Ok(line)=line else {return false;};
                            let Ok(entry)=serde_json::from_str::<Value>(&line)else {continue;};
                            if recorded.is_none() {recorded=entry["cwd"].as_str().map(str::to_owned);}
                            if entry["type"]=="relocated" && let Some(cwd)=entry["relocatedCwd"].as_str() {relocated=Some(cwd.to_owned());}
                        }
                        relocated.or(recorded).is_some_and(|recorded| {
                            let recorded=std::path::Path::new(&recorded).components().collect::<std::path::PathBuf>();
                            recorded.to_string_lossy().encode_utf16().map(|unit|if unit<=127 && (unit as u8).is_ascii_alphanumeric(){char::from(unit as u8)}else {'-'}).collect::<String>()==encoded
                        })
                    }));
                    if matches {projects.push(directory.path());}
                }
        }
        for project in projects {
            let file=project.join(format!("{session}.jsonl"));
            if !std::fs::metadata(&file).is_ok_and(|metadata|metadata.is_file() && metadata.len()>0) {continue;}
            let file=std::fs::File::open(file)?;
            let skip=file.metadata()?.len()>5*1024*1024 && environment.get("CLAUDE_CODE_DISABLE_PRECOMPACT_SKIP").is_none_or(|value|value.is_empty());
            let mut entries=Vec::new();
            for line in std::io::BufRead::lines(std::io::BufReader::new(file)) {
                let line=line?;let Ok(entry)=serde_json::from_str::<Value>(&line)else {continue;};
                if skip && entry["type"]=="system" && entry["subtype"]=="compact_boundary" && entry["compactMetadata"]["preservedMessages"].is_null() && entry["compactMetadata"]["preservedSegment"].is_null() {entries.clear();}
                entries.push(entry);
            }
            return Ok(transcript_messages(&entries));
        }
    }
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn transcript_reader_resolves_private_project_and_projects_main_chain() {
        let directory=tempfile::tempdir().expect("directory");let cwd=directory.path().join("work");std::fs::create_dir(&cwd).expect("cwd");
        let config=directory.path().join("config");let project=config.join("projects").join("fixture-project");std::fs::create_dir_all(&project).expect("project");
        let session="00000000-0000-0000-0000-000000000001";
        std::fs::write(project.join(format!("{session}.jsonl")),format!("{{\"type\":\"user\",\"uuid\":\"u\",\"sessionId\":\"{session}\"}}\nmalformed\n{{\"type\":\"assistant\",\"uuid\":\"a\",\"parentUuid\":\"u\",\"sessionId\":\"{session}\"}}\n{{\"type\":\"user\",\"uuid\":\"side\",\"isSidechain\":true,\"sessionId\":\"{session}\"}}\n")).expect("fixture");
        let environment=BTreeMap::from([("CLAUDE_CONFIG_DIR".into(),config.to_string_lossy().into_owned()),("CLAUDE_CODE_PROJECT_DIR_NAME".into(),"fixture-project".into())]);
        let messages=get_session_messages(session,&cwd,&environment).await.expect("read");
        assert_eq!(messages.iter().map(|message|message["uuid"].as_str().expect("uuid")).collect::<Vec<_>>(),["u","a"]);
        assert!(messages.iter().all(|message|message["session_id"]==session));
        assert!(get_session_messages("../invalid",&cwd,&environment).await.expect("invalid id").is_empty());
    }
    #[tokio::test]
    async fn transcript_reader_large_compaction_drops_unpreserved_ancestry() {
        let directory=tempfile::tempdir().expect("directory");let config=directory.path().join("config");let project=config.join("projects").join("fixture");std::fs::create_dir_all(&project).expect("project");
        let session="00000000-0000-0000-0000-000000000002";
        let entries=[json!({"type":"user","uuid":"old","sessionId":session,"message":{"content":"x".repeat(5*1024*1024)}}),json!({"type":"system","subtype":"compact_boundary","uuid":"compact","parentUuid":"old"}),json!({"type":"assistant","uuid":"a","parentUuid":"compact","sessionId":session})];
        let text=entries.iter().map(|entry|entry.to_string()+"\n").collect::<String>();std::fs::write(project.join(format!("{session}.jsonl")),text).expect("transcript");
        let mut environment=BTreeMap::from([("CLAUDE_CONFIG_DIR".into(),config.to_string_lossy().into_owned()),("CLAUDE_CODE_PROJECT_DIR_NAME".into(),"fixture".into())]);
        let messages=get_session_messages(session,directory.path(),&environment).await.expect("compact read");assert_eq!(messages.len(),1);assert_eq!(messages[0]["uuid"],"a");
        environment.insert("CLAUDE_CODE_DISABLE_PRECOMPACT_SKIP".into(),"1".into());
        let messages=get_session_messages(session,directory.path(),&environment).await.expect("full read");assert_eq!(messages.len(),2);assert_eq!(messages[0]["uuid"],"old");
    }
    #[tokio::test]
    async fn transcript_reader_finds_other_worktree_project() {
        let directory=tempfile::tempdir().expect("directory");let primary=directory.path().join("primary");let secondary=directory.path().join("secondary");std::fs::create_dir(&primary).expect("primary");
        for args in [vec!["init"],vec!["-c","user.name=Fixture","-c","user.email=fixture@example.invalid","commit","--allow-empty","-m","fixture"],vec!["worktree","add","-b","secondary",secondary.to_str().expect("path")]] {
            let output=tokio::process::Command::new("git").args(args).current_dir(&primary).env("GIT_CONFIG_NOSYSTEM","1").env("GIT_CONFIG_GLOBAL","/dev/null").stdin(Stdio::null()).output().await.expect("git");assert!(output.status.success(),"git fixture failed");
        }
        let config=directory.path().join("config");let canonical=std::fs::canonicalize(&primary).expect("canonical primary");let project=config.join("projects").join(transcript_project_key(canonical.to_str().expect("path")));std::fs::create_dir_all(&project).expect("project");
        let session="00000000-0000-0000-0000-000000000003";
        std::fs::write(project.join(format!("{session}.jsonl")),json!({"type":"assistant","uuid":"anchor","sessionId":session}).to_string()+"\n").expect("transcript");
        let environment=BTreeMap::from([("CLAUDE_CONFIG_DIR".into(),config.to_string_lossy().into_owned()),("PATH".into(),std::env::var("PATH").expect("PATH")),("GIT_CONFIG_NOSYSTEM".into(),"1".into()),("GIT_CONFIG_GLOBAL".into(),"/dev/null".into())]);
        let messages=get_session_messages(session,&secondary,&environment).await.expect("worktree read");assert_eq!(messages.len(),1);assert_eq!(messages[0]["uuid"],"anchor");
    }
    #[tokio::test]
    async fn transcript_reader_matches_long_project_by_recorded_cwd() {
        let directory=tempfile::tempdir().expect("directory");let cwd=directory.path().join("w".repeat(210));std::fs::create_dir(&cwd).expect("cwd");
        let canonical=std::fs::canonicalize(&cwd).expect("canonical");let path=canonical.to_string_lossy();
        let encoded:String=path.encode_utf16().map(|unit|if unit<=127 && (unit as u8).is_ascii_alphanumeric(){char::from(unit as u8)}else {'-'}).collect();
        let config=directory.path().join("config");let project=config.join("projects").join(format!("{}-previous-hash",&encoded[..200]));std::fs::create_dir_all(&project).expect("project");
        let session="00000000-0000-0000-0000-000000000004";
        std::fs::write(project.join(format!("{session}.jsonl")),json!({"type":"assistant","uuid":"anchor","sessionId":session,"cwd":path}).to_string()+"\n").expect("transcript");
        let environment=BTreeMap::from([("CLAUDE_CONFIG_DIR".into(),config.to_string_lossy().into_owned())]);
        let messages=get_session_messages(session,&cwd,&environment).await.expect("long project read");assert_eq!(messages.len(),1);assert_eq!(messages[0]["uuid"],"anchor");
        std::fs::write(project.join(format!("{session}.jsonl")),json!({"type":"assistant","uuid":"anchor","sessionId":session,"cwd":"/unrelated"}).to_string()+"\n").expect("unrelated transcript");
        assert!(get_session_messages(session,&cwd,&environment).await.expect("unrelated project").is_empty());
    }
    #[tokio::test]
    async fn transcript_reader_uses_latest_relocation_over_initial_cwd() {
        let directory=tempfile::tempdir().expect("directory");let cwd=directory.path().join("r".repeat(210));std::fs::create_dir(&cwd).expect("cwd");
        let canonical=std::fs::canonicalize(&cwd).expect("canonical");let path=canonical.to_string_lossy();
        let encoded:String=path.encode_utf16().map(|unit|if unit<=127 && (unit as u8).is_ascii_alphanumeric(){char::from(unit as u8)}else {'-'}).collect();
        let config=directory.path().join("config");let project=config.join("projects").join(format!("{}-old-hash",&encoded[..200]));std::fs::create_dir_all(&project).expect("project");
        let session="00000000-0000-0000-0000-000000000005";
        let entries=[json!({"type":"assistant","uuid":"anchor","sessionId":session,"cwd":"/initial"}),json!({"type":"relocated","relocatedCwd":"/previous"}),json!({"type":"relocated","relocatedCwd":path})];
        std::fs::write(project.join(format!("{session}.jsonl")),entries.iter().map(Value::to_string).collect::<Vec<_>>().join("\n")+"\n").expect("transcript");
        let environment=BTreeMap::from([("CLAUDE_CONFIG_DIR".into(),config.to_string_lossy().into_owned())]);
        let messages=get_session_messages(session,&cwd,&environment).await.expect("relocated project");
        assert_eq!(messages.len(),1);assert_eq!(messages[0]["uuid"],"anchor");
    }
    #[test]
    fn transcript_projection_exposes_queued_users_only_before_reply() {
        for (forwarded,meta,projected) in [(false,false,true),(true,false,false),(false,true,false)] {
            let entries=vec![json!({"type":"user","uuid":"u","sessionId":"sdk"}),json!({"type":"attachment","uuid":"queued","parentUuid":"u","sessionId":"sdk","attachment":{"type":"queued_command","prompt":"queued prompt","isMeta":meta,"forwardedIntent":if forwarded {json!({"lineage":"intent"})}else {Value::Null}}}),json!({"type":"assistant","uuid":"a","parentUuid":"queued","sessionId":"sdk","message":{"content":[]}})];
            let messages=transcript_messages(&entries);
            assert_eq!(messages.len(),if projected {3}else {2});
            assert!(messages.iter().all(|message|message["session_id"]=="sdk" && message["parent_tool_use_id"].is_null()));
            assert_eq!(messages.iter().any(|message|message["uuid"]=="queued"),projected);
        }
    }
    #[test]
    fn transcript_projection_preserves_completed_command_markers() {
        let entries=vec![json!({"type":"user","uuid":"caveat","isMeta":true,"message":{"content":"<local-command-caveat>"}}),json!({"type":"user","uuid":"record","parentUuid":"caveat","message":{"content":"<command-name>"}}),json!({"type":"user","uuid":"output","parentUuid":"record","message":{"content":"<local-command-stdout>"}}),json!({"type":"assistant","uuid":"a","parentUuid":"output"})];
        let messages=transcript_messages(&entries);
        assert_eq!(messages.len(),3);
        assert_eq!(messages[0]["isCompletedLocalCommand"],true);
        assert_eq!(messages[1]["isCompletedLocalCommand"],true);
        assert!(messages[2].get("isCompletedLocalCommand").is_none());
    }
    #[test]
    fn transcript_projection_preserves_origin_and_prevents_duplicate_queued_uuid() {
        let entries=vec![json!({"type":"user","uuid":"u","sessionId":"sdk"}),json!({"type":"attachment","uuid":"queued","parentUuid":"u","sessionId":"sdk","attachment":{"type":"queued_command","source_uuid":"u","prompt":"duplicate"}}),json!({"type":"assistant","uuid":"a","parentUuid":"queued","sessionId":"sdk","origin":{"kind":"task-notification","subkind":"done","fireReason":"event","private":"excluded"}})];
        let messages=transcript_messages(&entries);assert_eq!(messages.len(),2);
        assert_eq!(messages[1]["origin"],json!({"kind":"task-notification","subkind":"done","fireReason":"event"}));
        assert!(messages.iter().all(|message|message.get("isQueuedCommand").is_none()));
    }
    #[test]
    fn transcript_project_key_preserves_sdk_utf16_hash_and_prefix() {
        assert_eq!(transcript_project_key("/tmp/work.tree"),"-tmp-work-tree");
        assert_eq!(transcript_project_key("/tmp/\u{1f600}"),"-tmp---");
        assert_eq!(transcript_project_key(&"a".repeat(201)),format!("{}-{}","a".repeat(200),"rkvsv5"));
    }
    #[test]
    fn transcript_lineage_selects_main_leaf_and_rewrites_preserved_compaction() {
        let entries = vec![json!({"type":"user","uuid":"u","parentUuid":null}),json!({"type":"assistant","uuid":"a","parentUuid":"u"}),json!({"type":"user","uuid":"next","parentUuid":"u"}),json!({"type":"system","subtype":"compact_boundary","uuid":"compact","parentUuid":"next","compactMetadata":{"preservedMessages":{"anchorUuid":"u","uuids":["a"]}}}),json!({"type":"assistant","uuid":"side","parentUuid":null,"isSidechain":true})];
        let lineage=transcript_lineage(&entries);
        assert_eq!(lineage.iter().map(|node|node["uuid"].as_str().expect("uuid")).collect::<Vec<_>>(),["u","a","next"]);
        assert_eq!(lineage[2]["parentUuid"],"a");
    }
    #[test]
    fn transcript_lineage_recovers_split_assistant_and_tool_siblings() {
        let entries=vec![json!({"type":"user","uuid":"u"}),json!({"type":"assistant","uuid":"a","parentUuid":"u","message":{"id":"shared"},"timestamp":"1"}),json!({"type":"assistant","uuid":"b","parentUuid":"u","message":{"id":"shared"},"timestamp":"2"}),json!({"type":"user","uuid":"tool","parentUuid":"a","message":{"content":[{"type":"tool_result"}]},"timestamp":"3"}),json!({"type":"user","uuid":"next","parentUuid":"b"})];
        let lineage=transcript_lineage(&entries);
        assert_eq!(lineage.iter().map(|node|node["uuid"].as_str().expect("uuid")).collect::<Vec<_>>(),["u","b","a","tool","next"]);
    }
    #[test]
    fn transcript_lineage_preserved_segment_and_invalid_preservation() {
        for preserved in [json!({"preservedSegment":{"anchorUuid":"u","headUuid":"a","tailUuid":"a"}}),json!({"preservedMessages":{"anchorUuid":"u","uuids":["missing"]}})] {
            let valid=preserved.get("preservedSegment").is_some();
            let entries=vec![json!({"type":"user","uuid":"u"}),json!({"type":"assistant","uuid":"a","parentUuid":"old"}),json!({"type":"user","uuid":"next","parentUuid":"u"}),json!({"type":"system","subtype":"compact_boundary","uuid":"compact","parentUuid":"next","compactMetadata":preserved})];
            let lineage=transcript_lineage(&entries);
            let uuids:Vec<_>=lineage.iter().map(|node|node["uuid"].as_str().expect("uuid")).collect();
            assert_eq!(uuids,if valid {vec!["u","a","next"]}else {vec!["u","next"]});
        }
    }
    #[test]
    fn transcript_lineage_ignores_metadata_leaves_and_bounds_parent_cycles() {
        let entries=vec![json!({"type":"user","uuid":"u","parentUuid":"a"}),json!({"type":"assistant","uuid":"a","parentUuid":"u"}),json!({"type":"progress","uuid":"progress","parentUuid":"a"}),json!({"type":"user","uuid":"meta","isMeta":true})];
        let lineage=transcript_lineage(&entries);
        assert_eq!(lineage.iter().map(|node|node["uuid"].as_str().expect("uuid")).collect::<Vec<_>>(),["u","a"]);
    }
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
    #[cfg(unix)]
    #[tokio::test]
    async fn child_requests_host_denial_and_native_mcp_tool_list_and_call() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().expect("dir"); let script = directory.path().join("claude");
        std::fs::write(&script, "#!/usr/bin/python3\nimport sys,json\ndef read(): return json.loads(sys.stdin.readline())\ndef send(f): print(json.dumps(f),flush=True)\nf=read()\nassert f['request']['sdkMcpServers']==['custom-tools']\nsend({'type':'control_response','response':{'subtype':'success','request_id':f['request_id'],'response':{}}})\nfor request in [{'subtype':'can_use_tool','tool_name':'Bash','input':{'command':'false'},'tool_use_id':'t'}, {'subtype':'mcp_message','server_name':'custom-tools','message':{'jsonrpc':'2.0','id':1,'method':'tools/list'}}, {'subtype':'mcp_message','server_name':'custom-tools','message':{'jsonrpc':'2.0','id':2,'method':'tools/call','params':{'name':'search','arguments':{'query':'symbol'}}}}]:\n send({'type':'control_request','request_id':'child','request':request})\n response=read()['response']\n assert response['subtype']=='success'\n if request['subtype']=='can_use_tool': assert response['response']['behavior']=='deny' and response['response']['toolUseID']=='t'\n elif request['message']['method']=='tools/list': assert response['response']['mcp_response']['result']['tools'][0]['name']=='search'\n else: assert response['response']['mcp_response']['result']['isError']==True\nsend({'type':'result','subtype':'success','result':'callbacks checked'})\nfor line in sys.stdin: pass\n").expect("script");
        std::fs::set_permissions(&script,std::fs::Permissions::from_mode(0o700)).expect("permissions");
        let options = json!({"customTools":[{"name":"search","description":"Search","parameters":{"type":"object","properties":{"query":{"type":"string"}},"required":["query"]}}]});
        let mut query = SdkQueryHandle::spawn(&script,&options,&BTreeMap::new()).await.expect("query");
        let result = tokio::time::timeout(std::time::Duration::from_secs(5),query.next()).await.expect("bounded callbacks").expect("frame").expect("result");
        assert_eq!(result["result"],"callbacks checked"); query.close().await.expect("close");
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn settlement_accepts_close_while_child_remains_alive_after_stdout_eof() {
        let mut child = tokio::process::Command::new("/usr/bin/python3").args(["-c", "import sys; sys.stdin.read()"]).stdin(Stdio::piped()).stdout(Stdio::null()).kill_on_drop(true).spawn().expect("child");
        let (send, mut receive) = mpsc::unbounded_channel();
        let (reply, response) = oneshot::channel(); send.send(Command::Send(json!({}),reply)).expect("send");
        let cleanup = tokio::spawn(async move { let mut closed = false; let status = settle_child(&mut child,&mut receive,&mut closed).await.expect("settle"); assert!(closed); assert!(!status.success()); });
        response.await.expect("reply").expect_err("stdout closed");
        send.send(Command::Close).expect("close signal");
        tokio::time::timeout(std::time::Duration::from_secs(5),cleanup).await.expect("bounded cleanup").expect("reaped");
    }
}
