//! Port of `packages/omo-senpi/src/mcp/memory-server.ts`.
//!
//! Standalone stdio MCP server exposing the omo memory tools. The memory extension registers this
//! server with exposure "search" so the tools surface through the tool_search catalog instead of
//! occupying the always-on tool set. It runs with no session runtime: the extension injects the
//! bound identity and accepted-turn provenance, with cwd-based auto identity retained for
//! standalone calls. The native binary serves it behind [`MEMORY_MCP_ENTRY_FLAG`].

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use mcp_stdio_core::{
    JsonRpcId, JsonRpcResponse, JsonRpcStdioServerConfig, ParentWatchdogConfig, ServerError,
    ServerOutcome, error_response, json_rpc_id, run_json_rpc_stdio_server, success_response,
};
use memory_core::git::GitLogOptions;
use memory_core::identity::layout::{MemoryIdentityPaths, build_identity_paths};
use memory_core::identity::resolve::resolve_memory_identity;
use memory_core::tools::memory::{
    MemoryToolCommit, MemoryToolParams, MemoryToolProvenanceInput, run_memory_tool,
};
use memory_core::tools::memory_apply_patch::{MemoryApplyPatchParams, run_memory_apply_patch};
use serde_json::{Map, Value, json};

use crate::engine_session::{MemoryEngineSession, prepare_memory_engine_session};
use crate::tool_metadata::{
    MEMORY_APPLY_PATCH_DESCRIPTION, MEMORY_APPLY_PATCH_TOOL_NAME, MEMORY_MCP_SERVER_NAME,
    MEMORY_TOOL_DESCRIPTION, MEMORY_TOOL_NAME,
};
use crate::tool_receipts::{MemoryToolReceipt, write_tool_receipt};

/// Hidden argv role the native binary answers by serving this MCP server on stdio (the native
/// counterpart of the bundled `omo-memory-mcp.js` entry).
pub const MEMORY_MCP_ENTRY_FLAG: &str = "--omo-memory-mcp";
const SERVER_VERSION: &str = "0.1.0";
const DEFAULT_PROTOCOL_VERSION: &str = "2024-11-05";
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

fn memory_input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "command": {
                "type": "string",
                "enum": ["create", "str_replace", "insert", "delete", "rename", "update_description"],
                "description": "The memory operation to perform.",
            },
            "reason": { "type": "string", "description": "Git commit message recorded for this memory change." },
            "file_path": { "type": "string", "description": "Target memory file: relative to the memory repo, or absolute inside it. Required by create, str_replace, insert, delete, and update_description." },
            "old_path": { "type": "string", "description": "Current path of the memory file. Required by rename." },
            "new_path": { "type": "string", "description": "Destination path of the memory file. Required by rename." },
            "old_string": { "type": "string", "description": "Exact text to replace. Required by str_replace." },
            "new_string": { "type": "string", "description": "Replacement text. Required by str_replace." },
            "insert_line": { "type": "number", "description": "1-based line number at which to insert text. Required by insert." },
            "insert_text": { "type": "string", "description": "Text to insert. Required by insert." },
            "description": { "type": "string", "description": "Frontmatter description of the memory block. Required by create and update_description." },
            "file_text": { "type": "string", "description": "Initial body text for create." },
        },
        "required": ["command", "reason"],
        "additionalProperties": false,
    })
}

fn apply_patch_input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "reason": { "type": "string", "description": "Git commit message recorded for this memory change." },
            "input": { "type": "string", "description": "Patch text in the standard apply_patch format (*** Begin Patch ... *** End Patch)." },
        },
        "required": ["reason", "input"],
        "additionalProperties": false,
    })
}

fn tools() -> Value {
    json!([
        { "name": MEMORY_TOOL_NAME, "description": MEMORY_TOOL_DESCRIPTION, "inputSchema": memory_input_schema() },
        { "name": MEMORY_APPLY_PATCH_TOOL_NAME, "description": MEMORY_APPLY_PATCH_DESCRIPTION, "inputSchema": apply_patch_input_schema() },
    ])
}

/// Request options (`{ cwd?, env? }` upstream); the stdio entry passes the process cwd and env.
#[derive(Clone, Debug, Default)]
pub struct MemoryMcpRequestOptions {
    pub cwd: PathBuf,
    pub env: BTreeMap<String, String>,
}

impl MemoryMcpRequestOptions {
    pub fn from_process() -> std::io::Result<Self> {
        Ok(Self {
            cwd: std::env::current_dir()?,
            env: std::env::vars().collect(),
        })
    }
}

fn record(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

pub fn handle_memory_mcp_request(
    input: &Value,
    options: &MemoryMcpRequestOptions,
) -> Option<JsonRpcResponse> {
    let Value::Object(request) = input else {
        return Some(error_response(
            JsonRpcId::Null,
            -32600,
            "Invalid Request",
            None,
        ));
    };
    let id = json_rpc_id(request.get("id").unwrap_or(&Value::Null));
    match request.get("method").and_then(Value::as_str) {
        Some("initialize") => {
            let protocol_version = request
                .get("params")
                .and_then(|params| params.get("protocolVersion"))
                .and_then(Value::as_str)
                .unwrap_or(DEFAULT_PROTOCOL_VERSION);
            Some(success_response(
                id,
                record(json!({
                    "capabilities": { "tools": { "listChanged": false } },
                    "serverInfo": { "name": MEMORY_MCP_SERVER_NAME, "version": SERVER_VERSION },
                    "protocolVersion": protocol_version,
                })),
            ))
        }
        Some("tools/list") => Some(success_response(id, record(json!({ "tools": tools() })))),
        Some("notifications/initialized") => None,
        Some("tools/call") => {
            let empty = Map::new();
            let params = request
                .get("params")
                .and_then(Value::as_object)
                .unwrap_or(&empty);
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params
                .get("arguments")
                .and_then(Value::as_object)
                .unwrap_or(&empty);
            Some(call_memory_tool(id, name, args, options))
        }
        _ => Some(error_response(id, -32601, "Method not found", None)),
    }
}

fn call_memory_tool(
    id: JsonRpcId,
    name: &str,
    args: &Map<String, Value>,
    options: &MemoryMcpRequestOptions,
) -> JsonRpcResponse {
    if name != MEMORY_TOOL_NAME && name != MEMORY_APPLY_PATCH_TOOL_NAME {
        return tool_text(
            id,
            format!("Unknown {MEMORY_MCP_SERVER_NAME} tool: {name}"),
            true,
        );
    }
    let provenance = read_mcp_provenance(args, &options.cwd);
    let (identity, paths) = match &provenance {
        Some(provenance) => {
            let memory_root = provenance
                .repo_path
                .ancestors()
                .nth(3)
                .unwrap_or(provenance.repo_path.as_path());
            (
                provenance.identity_id.clone(),
                build_identity_paths(memory_root, &provenance.identity_id),
            )
        }
        None => match resolve_memory_identity(None, &options.cwd, &options.env) {
            Ok(identity) => (identity.id, identity.paths),
            Err(error) => return tool_text(id, error.message, true),
        },
    };
    let session = match prepare_memory_engine_session(&identity, &paths, Default::default()) {
        Ok(session) => session,
        Err(error) => return tool_text(id, error.message, true),
    };
    let tool_provenance = provenance
        .as_ref()
        .map(|provenance| MemoryToolProvenanceInput {
            session_id: provenance.session_id.clone(),
            user_turns: provenance.user_turns,
            claim: provenance.claim.clone(),
        });
    let tool_call_id = provenance
        .as_ref()
        .and_then(|provenance| provenance.tool_call_id.as_deref());
    let outcome = if name == MEMORY_TOOL_NAME {
        // Field-level validation lives in run_memory_tool's required guards; the injected
        // provenance object is replaced by the validated input shape.
        let mut fields = args.clone();
        fields.remove("provenance");
        match serde_json::from_value::<MemoryToolParams>(Value::Object(fields)) {
            Err(error) => Err(format!("memory: invalid arguments: {error}")),
            Ok(mut params) => {
                params.provenance = tool_provenance;
                session
                    .lock
                    .run("memory-write", || {
                        run_memory_tool(&session.repo, &session.author, &params, None)
                    })
                    .map(|result| (result.result, result.commit))
                    .map_err(|error| error.message)
            }
        }
    } else {
        let params = MemoryApplyPatchParams {
            reason: args
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("")
                .into(),
            input: args
                .get("input")
                .and_then(Value::as_str)
                .unwrap_or("")
                .into(),
            author: session.author.clone(),
            provenance: tool_provenance,
        };
        session
            .lock
            .run("memory-write", || {
                run_memory_apply_patch(&session.repo, &params, None)
            })
            .map(|result| (result.message, result.commit))
            .map_err(|error| error.message)
    };
    match outcome {
        Ok((message, commit)) => {
            write_commit_receipt(&session, &paths, tool_call_id, commit.as_ref());
            tool_text(id, message, false)
        }
        Err(message) => tool_text(id, message, true),
    }
}

struct McpMemoryProvenance {
    session_id: String,
    user_turns: u64,
    /// The parsed claim receipt the trusted object carried; a malformed one is dropped, never trusted.
    claim: Option<memory_core::tools::claim_frontmatter::ClaimReceipt>,
    identity_id: String,
    repo_path: PathBuf,
    /// Injected by the extension's tool_call bridge; keys the IC-17 out-of-band receipt.
    tool_call_id: Option<String>,
}

fn safe_turn(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| {
            value
                .as_f64()
                .filter(|turn| turn.fract() == 0.0 && *turn >= 0.0)
                .map(|turn| turn as u64)
        })
        .filter(|turn| *turn <= MAX_SAFE_INTEGER)
}

fn read_mcp_provenance(args: &Map<String, Value>, cwd: &Path) -> Option<McpMemoryProvenance> {
    let value = args.get("provenance")?.as_object()?;
    let text = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    };
    let session_id = text("sessionId")?;
    let identity_id = text("identityId")?;
    let repo_path = text("repoPath")?;
    let user_turns = safe_turn(value.get("userTurns")?)?;
    let claim = value
        .get("claim")
        .and_then(memory_core::tools::claim_frontmatter::parse_claim_receipt);
    Some(McpMemoryProvenance {
        session_id,
        user_turns,
        claim,
        identity_id,
        repo_path: resolve_path(cwd, &repo_path),
        tool_call_id: text("toolCallId"),
    })
}

/// `path.resolve(p)`: absolute paths stand, relative ones join the cwd, `.`/`..` collapse.
fn resolve_path(cwd: &Path, path: &str) -> PathBuf {
    let mut resolved = PathBuf::new();
    for component in cwd.join(path).components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                resolved.pop();
            }
            other => resolved.push(other),
        }
    }
    resolved
}

// IC-17 outbound half: commit metadata reaches the extension through an identity-scoped receipt
// file keyed by the injected toolCallId, never through the tool text (the MCP output guard
// truncates large results, which would silently drop the notice on exactly the largest edits).
fn write_commit_receipt(
    session: &MemoryEngineSession,
    paths: &MemoryIdentityPaths,
    tool_call_id: Option<&str>,
    commit: Option<&MemoryToolCommit>,
) {
    let (Some(tool_call_id), Some(commit)) = (tool_call_id, commit) else {
        return;
    };
    let written = session
        .repo
        .log(Some(&GitLogOptions {
            range: Some(commit.sha.clone()),
            paths: None,
            limit: Some(1),
            include_paths: true,
            grep: None,
        }))
        .map_err(|error| error.to_string())
        .and_then(|log| {
            let entry = log.into_iter().next();
            let receipt = MemoryToolReceipt {
                version: 1,
                tool_call_id: tool_call_id.into(),
                sha: commit.sha.clone(),
                subject: entry
                    .as_ref()
                    .map(|entry| entry.subject.clone())
                    .unwrap_or_default(),
                affected_paths: entry.and_then(|entry| entry.paths).unwrap_or_default(),
            };
            write_tool_receipt(&paths.tool_receipts, &receipt).map_err(|error| error.to_string())
        });
    // The commit is already durable; a lost receipt only drops the visible notice.
    if let Err(error) = written {
        eprintln!("{MEMORY_MCP_SERVER_NAME}: failed to write tool receipt: {error}");
    }
}

fn tool_text(id: JsonRpcId, text: String, is_error: bool) -> JsonRpcResponse {
    success_response(
        id,
        record(json!({ "content": [{ "type": "text", "text": text }], "isError": is_error })),
    )
}

pub fn run_memory_mcp_stdio_server<R, W>(
    input: R,
    output: &mut W,
    options: MemoryMcpRequestOptions,
    parent_watchdog: ParentWatchdogConfig,
) -> Result<ServerOutcome, ServerError<Infallible>>
where
    R: Read + Send + 'static,
    W: Write,
{
    let handler = move |request: &Value| -> Result<Option<JsonRpcResponse>, Infallible> {
        Ok(handle_memory_mcp_request(request, &options))
    };
    let mut config = JsonRpcStdioServerConfig::new(handler);
    config.parent_watchdog = Some(parent_watchdog);
    config.parse_error_response = Some(std::sync::Arc::new(|_message: &str| {
        Some(error_response(
            JsonRpcId::Null,
            -32601,
            "Method not found",
            None,
        ))
    }));
    run_json_rpc_stdio_server(input, output, config)
}

/// The `MEMORY_MCP_ENTRY_FLAG` process entry: serves stdin/stdout with the parent watchdog on.
pub fn run_memory_mcp_stdio_entry() -> Result<(), String> {
    let options = MemoryMcpRequestOptions::from_process().map_err(|error| error.to_string())?;
    let mut output = std::io::stdout().lock();
    run_memory_mcp_stdio_server(
        std::io::stdin(),
        &mut output,
        options,
        ParentWatchdogConfig::default(),
    )
    .map(|_| ())
    .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use memory_core::git::GitMemoryRepo;
    use memory_core::memfs::parse_memory_file;
    use memory_core::support::sha256::sha256_hex;

    struct Fixture {
        _root: tempfile::TempDir,
        home: PathBuf,
        options: MemoryMcpRequestOptions,
    }

    fn fixture() -> Fixture {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("memory-home");
        let cwd = root.path().join("project");
        std::fs::create_dir_all(&cwd).unwrap();
        let env = BTreeMap::from([(
            memory_core::identity::layout::MEMORY_ROOT_ENV_VAR.to_owned(),
            home.to_string_lossy().into_owned(),
        )]);
        Fixture {
            home,
            options: MemoryMcpRequestOptions { cwd, env },
            _root: root,
        }
    }

    fn call(fixture: &Fixture, id: i64, name: &str, arguments: Value) -> Map<String, Value> {
        let response = handle_memory_mcp_request(&json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call", "params": { "name": name, "arguments": arguments } }), &fixture.options).unwrap();
        response.result.unwrap()
    }

    fn bound_repo(fixture: &Fixture, identity: &str) -> (MemoryIdentityPaths, GitMemoryRepo) {
        let paths = build_identity_paths(&fixture.home, identity);
        let repo = GitMemoryRepo::open(&paths.repo, identity).unwrap();
        repo.init(None).unwrap();
        (paths, repo)
    }

    fn receipt_path(paths: &MemoryIdentityPaths, call: &str) -> PathBuf {
        paths
            .tool_receipts
            .join(format!("{}.json", &sha256_hex(call.as_bytes())[..32]))
    }

    #[test]
    fn initialize_returns_server_info_and_tool_capabilities() {
        let result = handle_memory_mcp_request(
            &json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} }),
            &Default::default(),
        )
        .unwrap()
        .result
        .unwrap();
        assert_eq!(result["serverInfo"]["name"], "omo-memory");
        assert!(result["capabilities"]["tools"].is_object());
        assert_eq!(result["protocolVersion"], DEFAULT_PROTOCOL_VERSION);
    }

    #[test]
    fn tools_list_exposes_exactly_the_two_memory_tools() {
        let result = handle_memory_mcp_request(
            &json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
            &Default::default(),
        )
        .unwrap()
        .result
        .unwrap();
        let names: Vec<_> = result["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(names, ["memory", "memory_apply_patch"]);
    }

    #[test]
    fn fresh_project_create_then_str_replace_records_both_commits() {
        let fixture = fixture();
        let created = call(
            &fixture,
            3,
            "memory",
            json!({ "command": "create", "reason": "Record preference", "file_path": "system/human/preferences.md", "description": "User preferences", "file_text": "theme: dark" }),
        );
        assert_eq!(created["isError"], false, "{created:?}");
        let replaced = call(
            &fixture,
            4,
            "memory",
            json!({ "command": "str_replace", "reason": "Switch theme", "file_path": "system/human/preferences.md", "old_string": "theme: dark", "new_string": "theme: light" }),
        );
        assert_eq!(replaced["isError"], false, "{replaced:?}");
        let identities: Vec<_> = std::fs::read_dir(fixture.home.join("agents"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(identities.len(), 1);
        let repo_dir = fixture
            .home
            .join("agents")
            .join(&identities[0])
            .join("repo");
        let profile = parse_memory_file(
            &std::fs::read_to_string(repo_dir.join("system/human/preferences.md")).unwrap(),
        )
        .unwrap();
        assert_eq!(profile.frontmatter.description, "User preferences");
        assert_eq!(profile.body.trim(), "theme: light");
        let repo = GitMemoryRepo::open(&repo_dir, &identities[0]).unwrap();
        let subjects: Vec<_> = repo
            .log(Some(&GitLogOptions {
                limit: Some(2),
                ..Default::default()
            }))
            .unwrap()
            .into_iter()
            .map(|commit| commit.subject)
            .collect();
        assert_eq!(subjects, ["Switch theme", "Record preference"]);
    }

    #[test]
    fn injected_provenance_writes_the_bound_repo_with_turn_trailers() {
        let fixture = fixture();
        let identity = "explicit-agent-deadbeef";
        let (paths, repo) = bound_repo(&fixture, identity);
        let result = call(
            &fixture,
            6,
            "memory",
            json!({ "command": "create", "reason": "Record bound identity", "file_path": "bound.md", "description": "Bound",
            "provenance": { "sessionId": "session-mcp", "userTurns": 3, "identityId": identity, "repoPath": paths.repo } }),
        );
        assert_eq!(result["isError"], false, "{result:?}");
        let committed = parse_memory_file(&repo.show("HEAD", "bound.md").unwrap()).unwrap();
        assert_eq!(committed.frontmatter.description, "Bound");
        assert_eq!(committed.body.trim(), "");
        let head = repo
            .log(Some(&GitLogOptions {
                limit: Some(1),
                ..Default::default()
            }))
            .unwrap()
            .remove(0);
        assert_eq!(
            head.trailers,
            BTreeMap::from([
                ("Omo-Writer".into(), "memory-tool".into()),
                ("Omo-Session".into(), "session-mcp".into()),
                ("Omo-Turn".into(), "3".into())
            ])
        );
        let identities: Vec<_> = std::fs::read_dir(fixture.home.join("agents"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(identities, [identity]);
    }

    #[test]
    fn injected_tool_call_id_writes_an_out_of_band_receipt() {
        let fixture = fixture();
        let identity = "receipt-agent-deadbeef";
        let (paths, repo) = bound_repo(&fixture, identity);
        let result = call(
            &fixture,
            7,
            "memory",
            json!({ "command": "create", "reason": "rewrite persona", "file_path": "system/persona.md", "description": "Persona", "file_text": "v2 soul",
            "provenance": { "sessionId": "session-mcp", "userTurns": 3, "identityId": identity, "repoPath": paths.repo, "toolCallId": "call-receipt-1" } }),
        );
        assert_eq!(result["isError"], false, "{result:?}");
        let receipt: Value = serde_json::from_str(
            &std::fs::read_to_string(receipt_path(&paths, "call-receipt-1")).unwrap(),
        )
        .unwrap();
        assert_eq!(receipt["version"], 1);
        assert_eq!(receipt["toolCallId"], "call-receipt-1");
        assert_eq!(receipt["sha"], repo.head().unwrap().unwrap());
        assert_eq!(receipt["subject"], "rewrite persona");
        assert_eq!(receipt["affectedPaths"], json!(["system/persona.md"]));
    }

    #[test]
    fn standalone_commit_without_tool_call_id_writes_no_receipt() {
        let fixture = fixture();
        let created = call(
            &fixture,
            8,
            "memory",
            json!({ "command": "create", "reason": "standalone write", "file_path": "notes.md", "description": "Note", "file_text": "standalone" }),
        );
        assert_eq!(created["isError"], false, "{created:?}");
        let identity = std::fs::read_dir(fixture.home.join("agents"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .file_name()
            .to_string_lossy()
            .into_owned();
        let repo = GitMemoryRepo::open(
            fixture.home.join("agents").join(&identity).join("repo"),
            &identity,
        )
        .unwrap();
        assert_eq!(
            repo.log(Some(&GitLogOptions {
                limit: Some(1),
                ..Default::default()
            }))
            .unwrap()[0]
                .subject,
            "standalone write"
        );
        assert_eq!(
            parse_memory_file(&repo.show("HEAD", "notes.md").unwrap())
                .unwrap()
                .body
                .trim(),
            "standalone"
        );
        let receipts = build_identity_paths(&fixture.home, &identity).tool_receipts;
        assert_eq!(
            std::fs::read_dir(&receipts)
                .map(|entries| entries.count())
                .unwrap_or(0),
            0
        );
    }

    #[test]
    fn failing_call_with_tool_call_id_writes_no_receipt() {
        let fixture = fixture();
        let identity = "receipt-failure-agent";
        let (paths, _repo) = bound_repo(&fixture, identity);
        let result = call(
            &fixture,
            9,
            "memory",
            json!({ "command": "str_replace", "reason": "edit a missing file", "file_path": "system/persona.md", "old_string": "nothing", "new_string": "something",
            "provenance": { "sessionId": "session-mcp", "userTurns": 3, "identityId": identity, "repoPath": paths.repo, "toolCallId": "call-failed-1" } }),
        );
        assert_eq!(result["isError"], true);
        assert!(!receipt_path(&paths, "call-failed-1").exists());
    }

    #[test]
    fn unknown_tool_name_returns_an_error_result() {
        let fixture = fixture();
        assert_eq!(call(&fixture, 5, "nope", json!({}))["isError"], true);
    }

    #[test]
    fn stdio_server_answers_initialize_and_tools_list_until_input_closes() {
        let input = std::io::Cursor::new(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\n{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\"}\n".to_vec());
        let mut output = Vec::new();
        let outcome = run_memory_mcp_stdio_server(
            input,
            &mut output,
            Default::default(),
            ParentWatchdogConfig {
                poll_interval_ms: Some(0),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(outcome, ServerOutcome::InputClosed);
        let lines: Vec<Value> = String::from_utf8(output)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["result"]["serverInfo"]["name"], "omo-memory");
        assert_eq!(lines[1]["result"]["tools"].as_array().unwrap().len(), 2);
    }
}
