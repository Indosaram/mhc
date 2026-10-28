//! The memory MCP server: tool definitions, the tool dispatcher and a newline-delimited JSON-RPC
//! stdio loop.
//!
//! Port of mnemopi `mcp-tools.ts` (schemas, argument coercion, result shapes of the tools listed
//! in [`TOOL_NAMES`]) and `mcp-server.ts` (`handleJsonRpc`, `runStdio`). Not ported: the shared
//! surface DB tools, `sleep` (consolidation), `validate`, `diagnose` and the memory graph tools.
use crate::banks::{Banks, DEFAULT_BANK, validate_name};
use crate::recall::RecallOptions;
use crate::store::{RememberOptions, Store};
use serde_json::{Value, json};
use std::io::{BufRead, Write};
use std::path::PathBuf;

/// Protocol version announced by `initialize`.
pub const PROTOCOL_VERSION: &str = "2024-11-05";

/// Tools this server offers.
pub const TOOL_NAMES: &[&str] = &[
    "mnemopi_remember",
    "mnemopi_recall",
    "mnemopi_get",
    "mnemopi_update",
    "mnemopi_forget",
    "mnemopi_invalidate",
    "mnemopi_stats",
    "mnemopi_scratchpad_write",
    "mnemopi_scratchpad_read",
    "mnemopi_scratchpad_clear",
    "mnemopi_triple_add",
    "mnemopi_triple_query",
    "mnemopi_export",
    "mnemopi_import",
];

/// Where the server keeps its banks and which bank and session it uses by default.
#[derive(Clone, Debug)]
pub struct ServerConfig {
    pub data_dir: PathBuf,
    /// Bank used when a call names none (`MNEMOPI_MCP_BANK`, else `default`).
    pub bank: String,
    /// Session of every write (`MNEMOPI_SESSION_ID`, else `mcp_<bank>`).
    pub session_id: Option<String>,
}

impl ServerConfig {
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
            bank: DEFAULT_BANK.into(),
            session_id: None,
        }
    }
}

fn bank_prop() -> Value {
    json!({"type": "string", "description": "Memory bank.", "default": "default"})
}

fn schema(properties: Value, required: &[&str]) -> Value {
    json!({"type": "object", "properties": properties, "required": required})
}

/// `tools/list` entries.
pub fn tool_definitions() -> Vec<Value> {
    let id = json!({"type": "string", "description": "Memory ID."});
    let defs = [
        (
            "mnemopi_remember",
            "Store a durable memory in Mnemopi.",
            schema(
                json!({
                    "content": {"type": "string", "description": "The memory content to store."},
                    "importance": {"type": "number", "description": "Importance score from 0.0 to 1.0.", "default": 0.5},
                    "source": {"type": "string", "description": "Source tag for this memory.", "default": "mcp"},
                    "scope": {"type": "string", "description": "Memory scope: session, global, channel, or a custom scope.", "default": "session"},
                    "valid_until": {"type": "string", "description": "Optional expiry date or timestamp."},
                    "metadata": {"type": "object", "description": "Optional key-value metadata.", "default": {}},
                    "veracity": {"type": "string", "description": "Confidence label for the memory.", "default": "unknown"},
                    "bank": bank_prop(),
                }),
                &["content"],
            ),
        ),
        (
            "mnemopi_recall",
            "Search memories with hybrid scoring.",
            schema(
                json!({
                    "query": {"type": "string", "description": "Natural-language search query."},
                    "limit": {"type": "integer", "description": "Maximum results to return.", "default": 5},
                    "top_k": {"type": "integer", "description": "Maximum results to return.", "default": 5},
                    "temporal_weight": {"type": "number", "description": "Temporal boost weight. 0.0 disables recency boost.", "default": 0.0},
                    "query_time": {"type": "string", "description": "ISO timestamp to treat as now for temporal scoring."},
                    "temporal_halflife": {"type": "number", "description": "Temporal decay half-life in hours.", "default": 24},
                    "fts_weight": {"type": "number", "description": "Full-text search weight."},
                    "importance_weight": {"type": "number", "description": "Importance score weight."},
                    "bank": bank_prop(),
                }),
                &["query"],
            ),
        ),
        (
            "mnemopi_get",
            "Retrieve one memory by ID.",
            schema(
                json!({"memory_id": id, "bank": bank_prop()}),
                &["memory_id"],
            ),
        ),
        (
            "mnemopi_update",
            "Update the content or importance of an existing memory.",
            schema(
                json!({
                    "memory_id": id, "content": {"type": "string", "description": "New content for the memory."},
                    "importance": {"type": "number", "description": "New importance score."}, "bank": bank_prop(),
                }),
                &["memory_id"],
            ),
        ),
        (
            "mnemopi_forget",
            "Permanently delete a memory by ID.",
            schema(
                json!({"memory_id": id, "bank": bank_prop()}),
                &["memory_id"],
            ),
        ),
        (
            "mnemopi_invalidate",
            "Mark a memory as expired or superseded.",
            schema(
                json!({
                    "memory_id": id, "replacement_id": {"type": "string", "description": "Optional replacement memory ID."}, "bank": bank_prop(),
                }),
                &["memory_id"],
            ),
        ),
        (
            "mnemopi_stats",
            "Return Mnemopi memory statistics.",
            schema(json!({"bank": bank_prop()}), &[]),
        ),
        (
            "mnemopi_scratchpad_write",
            "Write a temporary scratchpad note.",
            schema(
                json!({
                    "content": {"type": "string", "description": "Content to write to scratchpad."}, "bank": bank_prop(),
                }),
                &["content"],
            ),
        ),
        (
            "mnemopi_scratchpad_read",
            "Read scratchpad entries.",
            schema(json!({"bank": bank_prop()}), &[]),
        ),
        (
            "mnemopi_scratchpad_clear",
            "Clear scratchpad entries.",
            schema(json!({"bank": bank_prop()}), &[]),
        ),
        (
            "mnemopi_triple_add",
            "Add a temporal fact triple.",
            schema(
                json!({
                    "subject": {"type": "string"}, "predicate": {"type": "string"}, "object": {"type": "string"},
                    "valid_from": {"type": "string", "description": "ISO date."},
                    "source": {"type": "string", "default": "conversation"},
                    "confidence": {"type": "number", "default": 1.0}, "bank": bank_prop(),
                }),
                &["subject", "predicate", "object"],
            ),
        ),
        (
            "mnemopi_triple_query",
            "Query temporal fact triples.",
            schema(
                json!({
                    "subject": {"type": "string"}, "predicate": {"type": "string"}, "object": {"type": "string"},
                    "as_of": {"type": "string"}, "bank": bank_prop(),
                }),
                &[],
            ),
        ),
        (
            "mnemopi_export",
            "Export Mnemopi memories to a JSON file.",
            schema(
                json!({
                    "output_path": {"type": "string", "description": "File path to write the export JSON."}, "bank": bank_prop(),
                }),
                &["output_path"],
            ),
        ),
        (
            "mnemopi_import",
            "Import Mnemopi memories from a JSON file.",
            schema(
                json!({
                    "input_path": {"type": "string", "description": "File path to read the export JSON from."},
                    "force": {"type": "boolean", "description": "Overwrite existing records instead of skipping.", "default": false},
                    "bank": bank_prop(),
                }),
                &["input_path"],
            ),
        ),
    ];
    defs.into_iter()
        .map(|(name, description, input_schema)| json!({"name": name, "description": description, "inputSchema": input_schema}))
        .collect()
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
}

fn num_arg(args: &Value, key: &str) -> Option<f64> {
    match args.get(key)? {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }
    .filter(|value: &f64| value.is_finite())
}

fn required<'a>(args: &'a Value, key: &str) -> Result<&'a str, Value> {
    str_arg(args, key).ok_or_else(|| json!({"error": format!("{key} is required")}))
}

impl ServerConfig {
    fn bank<'a>(&'a self, args: &'a Value) -> &'a str {
        str_arg(args, "bank").unwrap_or(&self.bank)
    }

    fn open(&self, bank: &str) -> Result<Store, String> {
        validate_name(bank).or_else(|error| {
            if bank == DEFAULT_BANK {
                Ok(())
            } else {
                Err(error)
            }
        })?;
        let session = self
            .session_id
            .clone()
            .unwrap_or_else(|| format!("mcp_{bank}"));
        Store::open(&Banks::new(&self.data_dir).db_path(bank)?, &session)
    }

    /// Runs one tool and returns its result object. `Err` is a failure reported to the client as
    /// an error result.
    pub fn call_tool(&self, name: &str, args: &Value) -> Result<Value, String> {
        let bank = self.bank(args).to_owned();
        macro_rules! req {
            ($key:expr) => {
                match required(args, $key) {
                    Ok(value) => value,
                    Err(error) => return Ok(error),
                }
            };
        }
        let store = || self.open(&bank);
        Ok(match name {
            "mnemopi_remember" => {
                let content = req!("content");
                let id = store()?.remember(
                    content,
                    &RememberOptions {
                        source: str_arg(args, "source").unwrap_or("mcp").to_owned(),
                        importance: num_arg(args, "importance").unwrap_or(0.5),
                        scope: str_arg(args, "scope").unwrap_or("session").to_owned(),
                        veracity: str_arg(args, "veracity").unwrap_or("unknown").to_owned(),
                        valid_until: str_arg(args, "valid_until").map(str::to_owned),
                        metadata: args
                            .get("metadata")
                            .filter(|value| value.is_object())
                            .cloned(),
                        ..Default::default()
                    },
                )?;
                let preview: String = content.chars().take(100).collect();
                json!({"status": "stored", "memory_id": id, "bank": bank, "content_preview": preview})
            }
            "mnemopi_recall" => {
                let query = req!("query");
                let top_k = num_arg(args, "top_k")
                    .or_else(|| num_arg(args, "limit"))
                    .unwrap_or(5.0)
                    .max(0.0) as usize;
                let defaults = RecallOptions::default();
                let options = RecallOptions {
                    temporal_weight: num_arg(args, "temporal_weight"),
                    query_time: str_arg(args, "query_time").map(str::to_owned),
                    temporal_halflife_hours: num_arg(args, "temporal_halflife").unwrap_or(24.0),
                    vec_weight: num_arg(args, "vec_weight").unwrap_or(defaults.vec_weight),
                    fts_weight: num_arg(args, "fts_weight").unwrap_or(defaults.fts_weight),
                    importance_weight: num_arg(args, "importance_weight")
                        .unwrap_or(defaults.importance_weight),
                    ..defaults
                };
                let results = store()?.recall(query, top_k, &options)?;
                json!({"status": "ok", "query": query, "count": results.len(), "results": results, "bank": bank})
            }
            "mnemopi_get" => {
                let id = req!("memory_id");
                match store()?.get(id)? {
                    Some(memory) => json!({"status": "ok", "memory": memory, "bank": bank}),
                    None => json!({"status": "not_found", "memory_id": id, "bank": bank}),
                }
            }
            "mnemopi_update" => {
                let id = req!("memory_id");
                let content = str_arg(args, "content");
                let importance = num_arg(args, "importance");
                if content.is_none() && importance.is_none() {
                    return Ok(json!({"error": "content or importance is required"}));
                }
                let ok = store()?.update(id, content, importance)?;
                json!({"status": if ok { "updated" } else { "not_found" }, "memory_id": id, "bank": bank})
            }
            "mnemopi_forget" => {
                let id = req!("memory_id");
                let ok = store()?.forget(id)?;
                json!({"status": if ok { "deleted" } else { "not_found" }, "memory_id": id, "bank": bank})
            }
            "mnemopi_invalidate" => {
                let id = req!("memory_id");
                let ok = store()?.invalidate(id, str_arg(args, "replacement_id"))?;
                json!({"status": if ok { "invalidated" } else { "not_found" }, "memory_id": id, "bank": bank})
            }
            "mnemopi_stats" => {
                json!({"status": "ok", "provider": "mnemopi", "bank": bank, "stats": store()?.stats()?})
            }
            "mnemopi_scratchpad_write" => {
                let content = req!("content");
                let id = store()?.scratchpad_write(content)?;
                json!({"status": "written", "id": id, "entry_id": id, "bank": bank})
            }
            "mnemopi_scratchpad_read" => {
                let entries = store()?.scratchpad_read()?;
                json!({"status": "ok", "count": entries.len(), "entries": entries, "bank": bank})
            }
            "mnemopi_scratchpad_clear" => {
                store()?.scratchpad_clear()?;
                json!({"status": "cleared", "bank": bank})
            }
            "mnemopi_triple_add" => {
                let (subject, predicate, object) =
                    (req!("subject"), req!("predicate"), req!("object"));
                let id = store()?.triple_add(
                    subject,
                    predicate,
                    object,
                    str_arg(args, "valid_from"),
                    str_arg(args, "source").unwrap_or("conversation"),
                    num_arg(args, "confidence").unwrap_or(1.0),
                )?;
                json!({"status": "stored", "triple_id": id, "store": "triples", "bank": bank})
            }
            "mnemopi_triple_query" => {
                let results = store()?.triple_query(
                    str_arg(args, "subject"),
                    str_arg(args, "predicate"),
                    str_arg(args, "object"),
                    str_arg(args, "as_of"),
                )?;
                json!({"count": results.len(), "results": results, "results_count": results.len(), "store": "triples", "bank": bank})
            }
            "mnemopi_export" => {
                let output = PathBuf::from(req!("output_path"));
                let store = store()?;
                let data = store.export_json()?;
                if let Some(parent) = output
                    .parent()
                    .filter(|parent| !parent.as_os_str().is_empty())
                {
                    std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
                }
                let text =
                    serde_json::to_string_pretty(&data).map_err(|error| error.to_string())?;
                std::fs::write(&output, text)
                    .map_err(|error| format!("cannot write {}: {error}", output.display()))?;
                json!({"status": "exported", "output_path": output, "bank": bank, "stats": store.stats()?})
            }
            "mnemopi_import" => {
                let input = PathBuf::from(req!("input_path"));
                if !input.exists() {
                    return Ok(
                        json!({"error": format!("input_path does not exist: {}", input.display())}),
                    );
                }
                let text = std::fs::read_to_string(&input).map_err(|error| error.to_string())?;
                let data: Value = serde_json::from_str(&text)
                    .map_err(|error| format!("invalid export JSON: {error}"))?;
                let force = args.get("force").and_then(Value::as_bool).unwrap_or(false);
                json!({"status": "imported", "stats": store()?.import_json(&data, force)?, "bank": bank})
            }
            other => return Err(format!("Unknown tool: {other}")),
        })
    }

    /// `tools/call` result: the tool result as pretty JSON text; failures set `isError`.
    pub fn call_tool_json(&self, name: &str, args: &Value) -> Value {
        match self.call_tool(name, args) {
            Ok(result) => json!({"content": [{"type": "text", "text": pretty(&result)}]}),
            Err(message) => json!({
                "content": [{"type": "text", "text": pretty(&json!({"status": "error", "message": message}))}],
                "isError": true,
            }),
        }
    }

    /// One JSON-RPC request → its response; `None` for notifications and id-less requests.
    pub fn handle_request(&self, request: &Value) -> Option<Value> {
        let method = request.get("method").and_then(Value::as_str).unwrap_or("");
        if method.starts_with("notifications/") || request.get("id").is_none() {
            return None;
        }
        let id = match request.get("id") {
            Some(value @ (Value::String(_) | Value::Number(_) | Value::Null)) => value.clone(),
            _ => Value::Null,
        };
        let ok = |result: Value| json!({"jsonrpc": "2.0", "id": id, "result": result});
        let err = |code: i64, message: String| json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}});
        Some(match method {
            "initialize" => ok(json!({
                "protocolVersion": PROTOCOL_VERSION,
                "serverInfo": {"name": "mnemopi", "version": env!("CARGO_PKG_VERSION")},
                "capabilities": {"tools": {}},
            })),
            "ping" => ok(json!({})),
            "tools/list" => ok(json!({"tools": tool_definitions()})),
            "tools/call" => {
                let params = request.get("params").cloned().unwrap_or(Value::Null);
                let name = params.get("name").and_then(Value::as_str).unwrap_or("");
                if name.is_empty() {
                    return Some(err(-32602, "tools/call requires params.name".into()));
                }
                let args = params
                    .get("arguments")
                    .filter(|value| value.is_object())
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                ok(self.call_tool_json(name, &args))
            }
            other => err(-32601, format!("Unknown method: {other}")),
        })
    }

    /// Serves newline-delimited JSON-RPC from `input` to `output` until EOF.
    pub fn serve(&self, input: impl BufRead, mut output: impl Write) -> std::io::Result<()> {
        for line in input.lines() {
            let line = line?;
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let response = match serde_json::from_str::<Value>(line) {
                Ok(request) => self.handle_request(&request),
                Err(_) => Some(
                    json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": "Parse error"}}),
                ),
            };
            if let Some(response) = response {
                writeln!(output, "{response}")?;
                output.flush()?;
            }
        }
        Ok(())
    }
}

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

/// Parses `--bank <name>` / `--data-dir <dir>` (and ignores `--transport stdio`) over `config`.
pub fn apply_args(mut config: ServerConfig, args: &[String]) -> Result<ServerConfig, String> {
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--bank" => config.bank = iter.next().cloned().ok_or("--bank needs a value")?,
            "--data-dir" => {
                config.data_dir = PathBuf::from(iter.next().ok_or("--data-dir needs a value")?)
            }
            "--transport" => {
                let transport = iter.next().map(String::as_str).unwrap_or("stdio");
                if transport != "stdio" {
                    return Err("only the stdio transport is implemented".into());
                }
            }
            other => return Err(format!("unknown memory MCP argument: {other}")),
        }
    }
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> (tempfile::TempDir, ServerConfig) {
        let dir = tempfile::tempdir().expect("dir");
        let config = ServerConfig::new(dir.path());
        (dir, config)
    }

    fn call(config: &ServerConfig, name: &str, args: Value) -> Value {
        config.call_tool(name, &args).expect("call")
    }

    #[test]
    fn remember_recall_get_forget() {
        let (_dir, config) = config();
        let stored = call(
            &config,
            "mnemopi_remember",
            json!({"content": "CI runs on self-hosted runners"}),
        );
        let id = stored["memory_id"].as_str().expect("id").to_owned();
        let recalled = call(
            &config,
            "mnemopi_recall",
            json!({"query": "self-hosted runners"}),
        );
        assert_eq!(recalled["count"], 1);
        assert_eq!(recalled["results"][0]["id"], id.as_str());
        assert_eq!(
            call(&config, "mnemopi_get", json!({"memory_id": id}))["status"],
            "ok"
        );
        assert_eq!(
            call(&config, "mnemopi_forget", json!({"memory_id": id}))["status"],
            "deleted"
        );
        assert_eq!(
            call(&config, "mnemopi_get", json!({"memory_id": id}))["status"],
            "not_found"
        );
        assert!(call(&config, "mnemopi_remember", json!({}))["error"].is_string());
    }

    #[test]
    fn banks_are_separate_files() {
        let (dir, config) = config();
        call(
            &config,
            "mnemopi_remember",
            json!({"content": "only in p1", "bank": "p1"}),
        );
        assert!(dir.path().join("banks/p1/mnemopi.db").exists());
        assert_eq!(
            call(&config, "mnemopi_recall", json!({"query": "only p1"}))["count"],
            0
        );
        assert!(
            config
                .call_tool("mnemopi_stats", &json!({"bank": "../x"}))
                .is_err()
        );
    }

    #[test]
    fn json_rpc_round_trip() {
        let (_dir, config) = config();
        let input = [
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}).to_string(),
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}).to_string(),
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}).to_string(),
            json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "mnemopi_remember", "arguments": {"content": "hello world"}}}).to_string(),
            json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "nope", "arguments": {}}}).to_string(),
            "{not json".to_owned(),
        ]
        .join("\n");
        let mut output = Vec::new();
        config.serve(input.as_bytes(), &mut output).expect("serve");
        let lines: Vec<Value> = String::from_utf8(output)
            .expect("utf8")
            .lines()
            .map(|line| serde_json::from_str(line).expect("json"))
            .collect();
        assert_eq!(lines.len(), 5);
        assert_eq!(lines[0]["result"]["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(
            lines[1]["result"]["tools"].as_array().expect("tools").len(),
            TOOL_NAMES.len()
        );
        assert!(
            lines[2]["result"]["content"][0]["text"]
                .as_str()
                .expect("text")
                .contains("stored")
        );
        assert_eq!(lines[3]["result"]["isError"], true);
        assert_eq!(lines[4]["error"]["code"], -32700);
    }

    #[test]
    fn definitions_match_names() {
        let names: Vec<String> = tool_definitions()
            .iter()
            .map(|tool| tool["name"].as_str().expect("name").to_owned())
            .collect();
        assert_eq!(names, TOOL_NAMES);
    }

    #[test]
    fn export_import_and_triples() {
        let (dir, config) = config();
        call(
            &config,
            "mnemopi_remember",
            json!({"content": "exported row"}),
        );
        let path = dir.path().join("out/export.json");
        assert_eq!(
            call(&config, "mnemopi_export", json!({"output_path": path}))["status"],
            "exported"
        );
        let imported = call(
            &config,
            "mnemopi_import",
            json!({"input_path": path, "bank": "copy"}),
        );
        assert_eq!(imported["stats"]["inserted"], 1);
        call(
            &config,
            "mnemopi_triple_add",
            json!({"subject": "app", "predicate": "uses", "object": "postgres", "valid_from": "2026-01-01"}),
        );
        let found = call(
            &config,
            "mnemopi_triple_query",
            json!({"subject": "app", "as_of": "2026-06-01"}),
        );
        assert_eq!(found["count"], 1);
    }

    #[test]
    fn args_parse() {
        let config = apply_args(
            ServerConfig::new("/d"),
            &[
                "--bank".into(),
                "x".into(),
                "--transport".into(),
                "stdio".into(),
            ],
        )
        .expect("args");
        assert_eq!(config.bank, "x");
        assert!(
            apply_args(
                ServerConfig::new("/d"),
                &["--transport".into(), "http".into()]
            )
            .is_err()
        );
    }
}
