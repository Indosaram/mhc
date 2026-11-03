//! Per-session memory write policy (`open_session.context.memory_write = "deny"`).
//!
//! A denied session keeps recall: the committed memory block is still projected into its prompt
//! and read-only surfaces keep working. Every memory mutation route refuses with the typed
//! [`MEMORY_WRITE_DENIED_CODE`]: the memory write tools (direct, `mcp_omo-memory_*` and MCP gateway
//! calls), every other MCP tool outside a remote read-only allowlist, file tools writing under the
//! identity root, the mutating slash commands (and, through the host's extension command gate, every
//! extension command outside a read-only allowlist), and the journal, facts, reflection and dream
//! work the session would otherwise feed.
//!
//! Denial is sticky. The marker is recorded in the session's memory-binding entry, and ANY earlier
//! denied binding keeps a resumed session denied, so neither a reopen without the context key, a
//! later binding entry without the marker, nor transcript text can lift it. Only host-supplied
//! session context and the binding entries this component writes are consulted.
//!
//! The same tool-call guard serves `effect_tools = "deny"` (memory writes allowed, no execution or
//! effect tool): it refuses the effect tool family and every MCP tool outside the read-only
//! allowlist plus the `omo-memory` server's memory tools, with the typed [`EFFECT_TOOLS_DENIED_CODE`].
use maho_ext_api::{ExtensionSessionProfile, SessionEntry};
use serde_json::Value;

use crate::context::MemoryIdentityContext;
use crate::tool_metadata::{MEMORY_APPLY_PATCH_TOOL_NAME, MEMORY_MCP_SERVER_NAME, MEMORY_TOOL_NAME};

/// Session context key carrying the write policy.
pub const MEMORY_WRITE_CONTEXT_KEY: &str = "memory_write";
/// The context value that denies memory writes.
pub const MEMORY_WRITE_DENY_VALUE: &str = "deny";
/// Typed code every refused write carries (message prefix and failure code).
pub const MEMORY_WRITE_DENIED_CODE: &str = "memory_write_denied";
/// Field of the memory-binding entry data that records the denial.
pub const BINDING_MEMORY_WRITE_FIELD: &str = "memoryWrite";
/// The memory write tool names a denied session must never run.
pub const MEMORY_WRITE_TOOL_NAMES: [&str; 2] = [MEMORY_TOOL_NAME, MEMORY_APPLY_PATCH_TOOL_NAME];
/// Tools a denied session never registers because they write without consulting the memory
/// filesystem policy or the tool-call write guard: shells and process steering (terminal family,
/// `powershell`), code execution (`eval`), the GPT `apply_patch` builtin (plain file writes), the
/// other direct file writers (`lsp_rename`/`lsp_format` workspace edits, `ast_grep_replace`,
/// `generate_image` to any output path), desktop control (`computer`), and every child-session
/// spawner (`task`, `dag`, `workflow`, `workpool`; a child rebuilds its own unguarded builtins).
/// Decision and conversation sessions need none of them.
pub const MEMORY_WRITE_DENY_EXCLUDED_TOOL_NAMES: [&str; 18] = [
    "bash", "bash_input", "bash_output", "bash_resize", "kill_bash", "monitor", "powershell",
    "eval", "apply_patch", "lsp_rename", "lsp_format", "ast_grep_replace", "generate_image",
    "computer", "task", "dag", "workflow", "workpool",
];
/// Name prefixes of the task and team tool families, excluded as `<prefix>*` patterns.
pub const MEMORY_WRITE_DENY_EXCLUDED_TOOL_PREFIXES: [&str; 2] = ["task_", "team_"];

/// Every exclusion entry a memory-write-denied session adds: exact names, then `prefix*` patterns
/// (the session's tool exclusion matches a trailing `*` as a name prefix).
pub fn memory_write_deny_tool_exclusions() -> Vec<String> {
    MEMORY_WRITE_TOOL_NAMES.iter().chain(MEMORY_WRITE_DENY_EXCLUDED_TOOL_NAMES.iter()).map(|name| (*name).to_owned())
        .chain(MEMORY_WRITE_DENY_EXCLUDED_TOOL_PREFIXES.iter().map(|prefix| format!("{prefix}*")))
        .collect()
}

/// A tool name a memory-write-denied session must never register.
pub fn memory_write_deny_excludes_tool(name: &str) -> bool {
    MEMORY_WRITE_TOOL_NAMES.contains(&name) || MEMORY_WRITE_DENY_EXCLUDED_TOOL_NAMES.contains(&name)
        || MEMORY_WRITE_DENY_EXCLUDED_TOOL_PREFIXES.iter().any(|prefix| name.starts_with(prefix))
}

/// The MCP tools a memory-write-denied session may still call, as `(server, tool)`: the read-only
/// tools of the two remote HTTP builtin servers (`builtin-mcps`: context7 documentation lookup and
/// grep.app code search). Any other MCP tool may belong to a local (stdio) server process, whose
/// writes never pass the session's filesystem policy, so a denied session refuses it.
pub const MEMORY_WRITE_DENY_READ_ONLY_MCP_TOOLS: [(&str, &str); 3] =
    [("context7", "resolve-library-id"), ("context7", "query-docs"), ("grep_app", "searchGitHub")];
/// The MCP extension's resource tools; their `server` argument picks the server they reach.
const MCP_RESOURCE_TOOLS: [&str; 2] = ["mcp_list_resources", "mcp_read_resource"];
/// Session context key of the effect-tool policy.
pub const EFFECT_TOOLS_CONTEXT_KEY: &str = "effect_tools";
/// The context value that denies effect tools.
pub const EFFECT_TOOLS_DENY_VALUE: &str = "deny";
/// Typed code every refused call or command of an effect-tool-denied session carries.
pub const EFFECT_TOOLS_DENIED_CODE: &str = "effect_tools_denied";
/// The `omo-memory` MCP server's tools, as `(server, tool)`: an effect-tool-denied session keeps
/// memory writes, so it may also call these, directly and through the server gateway.
pub const EFFECT_TOOLS_DENY_MEMORY_MCP_TOOLS: [(&str, &str); 2] =
    [(MEMORY_MCP_SERVER_NAME, MEMORY_TOOL_NAME), (MEMORY_MCP_SERVER_NAME, MEMORY_APPLY_PATCH_TOOL_NAME)];

/// An MCP tool call a denied session may make, `listed` as `(server, tool)`. Only exact exposed names match
/// (`mcp_<server>_<tool>`, case and `-` kept), so a middle-ellipsized long name, a collision-suffixed
/// name or a look-alike never does. A server gateway (`mcp_<first server word>`, `maho-ext-mcp`
/// `create_mcp_proxy_tool`) may `search`/`describe` its local catalog, and `call` only a listed tool.
/// The resource tools reach only the remote read-only servers.
fn mcp_call_allowed(listed: &[(&str, &str)], tool_name: &str, input: &Value) -> bool {
    if MCP_RESOURCE_TOOLS.contains(&tool_name) {
        let server = input.get("server").and_then(Value::as_str);
        return MEMORY_WRITE_DENY_READ_ONLY_MCP_TOOLS.iter().any(|(read_only, _)| server == Some(*read_only));
    }
    listed.iter().any(|(server, tool)| {
        let gateway = format!("mcp_{}", server.split('_').next().unwrap_or_default());
        tool_name == format!("mcp_{server}_{tool}")
            || (tool_name == gateway && match input.get("op").and_then(Value::as_str) {
                Some("call") => input.get("tool").and_then(Value::as_str) == Some(*tool),
                _ => true,
            })
    })
}

/// The host-supplied session context asks for write denial.
pub fn profile_denies_memory_write(profile: &ExtensionSessionProfile) -> bool {
    profile.session_context.get(MEMORY_WRITE_CONTEXT_KEY).map(String::as_str) == Some(MEMORY_WRITE_DENY_VALUE)
}

/// The host-supplied session context denies effect tools. A host unions the recorded policy into
/// the context it hands every open, reload and replacement, so this covers a resumed session too.
pub fn profile_denies_effect_tools(profile: &ExtensionSessionProfile) -> bool {
    profile.session_context.get(EFFECT_TOOLS_CONTEXT_KEY).map(String::as_str) == Some(EFFECT_TOOLS_DENY_VALUE)
}

/// Any memory-binding entry of the session recorded the denial.
pub fn recorded_memory_write_denied(entries: &[SessionEntry]) -> bool {
    entries.iter().any(|entry| {
        entry.kind == "custom"
            && entry.data["customType"] == crate::binding::MEMORY_BINDING_CUSTOM_TYPE
            && entry.data["data"][BINDING_MEMORY_WRITE_FIELD] == MEMORY_WRITE_DENY_VALUE
    })
}

/// The memory-binding entry data, carrying the denial marker when the session is denied.
pub fn binding_entry_value(binding: &crate::binding::MemorySessionBinding, denied: bool) -> Value {
    let mut value = serde_json::json!({"identity":binding.identity,"repoPathHash":binding.repo_path_hash,"boundAt":binding.bound_at});
    if denied {
        value[BINDING_MEMORY_WRITE_FIELD] = MEMORY_WRITE_DENY_VALUE.into();
    }
    value
}

/// The typed refusal text for one write attempt.
pub fn memory_write_denied_message(what: &str) -> String {
    format!("{MEMORY_WRITE_DENIED_CODE}: {what} refused; this session is memory-write-denied (memory_write=deny), memory is read-only here")
}

/// The typed refusal as an extension failure carrying the machine code.
pub fn memory_write_denied_failure(what: &str) -> maho_ext_api::ExtensionFailure {
    maho_ext_api::ExtensionFailure::classified(memory_write_denied_message(what), None, Some(MEMORY_WRITE_DENIED_CODE.to_owned()))
}

/// The typed refusal text for one effect attempt of an effect-tool-denied session.
pub fn effect_tools_denied_message(what: &str) -> String {
    format!("{EFFECT_TOOLS_DENIED_CODE}: {what} refused; this session holds no execution or effect tool (effect_tools=deny), only memory writes")
}

fn normalized(name: &str) -> String {
    name.trim().to_lowercase().replace('-', "_")
}

fn is_write_tool_name(name: &str) -> bool {
    let name = normalized(name);
    MEMORY_WRITE_TOOL_NAMES.iter().any(|tool| name == *tool)
}

/// A tool call that would mutate memory: a direct write tool, any tool of the `omo-memory` MCP
/// server (`mcp_omo-memory_*`, whose server runs without a session), its gateway, or an MCP
/// gateway `call` naming a memory write tool.
pub fn is_memory_write_tool_call(tool_name: &str, input: &Value) -> bool {
    if is_write_tool_name(tool_name) {
        return true;
    }
    let name = normalized(tool_name);
    let server = format!("mcp_{}", normalized(MEMORY_MCP_SERVER_NAME));
    if name == server || name.starts_with(&format!("{server}_")) {
        return true;
    }
    name.starts_with("mcp_")
        && input.get("op").and_then(Value::as_str) == Some("call")
        && input.get("tool").and_then(Value::as_str).is_some_and(is_write_tool_name)
}

/// The block reason for a tool call of a denied session, `None` when the call may proceed: a
/// memory write tool call, any tool of the policy-bypassing family (normally never registered for a
/// denied session; refused here too when a host did not exclude it), any MCP tool outside
/// [`MEMORY_WRITE_DENY_READ_ONLY_MCP_TOOLS`] (fail closed: MCP servers register after the open and
/// a local one writes outside every session policy), or a shell command naming the bound identity's
/// memory root. File tools are covered by the filesystem policy.
pub fn memory_write_block(denied: bool, context: Option<&MemoryIdentityContext>, tool_name: &str, input: &Value) -> Option<String> {
    if !denied { return None; }
    if is_memory_write_tool_call(tool_name, input) { return Some(memory_write_denied_message(tool_name)); }
    if memory_write_deny_excludes_tool(&normalized(tool_name)) {
        return Some(memory_write_denied_message(&format!("{tool_name} (writes outside the memory policy)")));
    }
    if normalized(tool_name).starts_with("mcp_") && !mcp_call_allowed(&MEMORY_WRITE_DENY_READ_ONLY_MCP_TOOLS, tool_name.trim(), input) {
        return Some(memory_write_denied_message(&format!("{tool_name} (MCP tool outside the read-only allowlist; a local MCP server writes outside the memory policy)")));
    }
    let shell = normalized(tool_name);
    let command = input.get("command").and_then(Value::as_str)?;
    let root = context?.identity_paths.root.to_string_lossy().into_owned();
    ((shell == "bash" || shell.ends_with("_bash")) && !root.is_empty() && command.contains(&root))
        .then(|| memory_write_denied_message(&format!("{tool_name} command on the memory root")))
}

/// The block reason for a tool call of an effect-tool-denied session, `None` when the call may
/// proceed: any tool of the effect family (normally never registered for the session; refused here
/// too when a host did not exclude it), and, failing closed, any MCP tool (direct, gateway or
/// resource) outside [`MEMORY_WRITE_DENY_READ_ONLY_MCP_TOOLS`] and the `omo-memory` memory tools
/// [`EFFECT_TOOLS_DENY_MEMORY_MCP_TOOLS`]: a local MCP server process acts outside every session policy.
pub fn effect_tools_block(denied: bool, tool_name: &str, input: &Value) -> Option<String> {
    if !denied { return None; }
    let name = normalized(tool_name);
    if MEMORY_WRITE_DENY_EXCLUDED_TOOL_NAMES.contains(&name.as_str()) || MEMORY_WRITE_DENY_EXCLUDED_TOOL_PREFIXES.iter().any(|prefix| name.starts_with(prefix)) {
        return Some(effect_tools_denied_message(&format!("{tool_name} (an effect tool)")));
    }
    let listed: Vec<(&str, &str)> = MEMORY_WRITE_DENY_READ_ONLY_MCP_TOOLS.into_iter().chain(EFFECT_TOOLS_DENY_MEMORY_MCP_TOOLS).collect();
    (name.starts_with("mcp_") && !mcp_call_allowed(&listed, tool_name.trim(), input)).then(|| effect_tools_denied_message(&format!(
        "{tool_name} (MCP tool outside the read-only and memory allowlist; a local MCP server acts outside every session policy)")))
}

/// Blocks every memory write tool call of a denied session, and every effect or unlisted MCP tool
/// call of an effect-tool-denied session, before it executes.
pub fn register_memory_write_guard(api: &mut maho_ext_api::ExtensionApi, resolve: crate::prompt::PromptContextResolver) {
    let profile_denied = profile_denies_memory_write(&api.profile);
    let effect_denied = profile_denies_effect_tools(&api.profile);
    api.on(
        maho_ext_api::EventKind::ToolCall,
        std::sync::Arc::new(move |event, context| {
            let reason = match event {
                maho_ext_api::ExtensionEvent::ToolCall(call) => {
                    let bound = resolve(context.session_manager.session_id());
                    let denied = profile_denied || bound.as_ref().is_some_and(|bound| bound.memory_write_denied);
                    memory_write_block(denied, bound.as_ref(), &call.tool_name, &call.input)
                        .or_else(|| effect_tools_block(effect_denied, &call.tool_name, &call.input))
                }
                _ => None,
            };
            Box::pin(async move {
                Ok(match reason {
                    Some(reason) => maho_ext_api::EventResult::ToolCall(maho_ext_api::ToolCallEventResult {
                        block: Some(true),
                        reason: Some(reason),
                        terminate: None,
                    }),
                    None => maho_ext_api::EventResult::None,
                })
            })
        }),
    );
}

/// Whether one memory slash command invocation only reads: these stay available in a denied
/// session; everything else (remember, init, recompile, reflect, dream, `/facts retry`, `/memory
/// unlock`, mutating `/memfs` and `/memory-repository` subcommands, unknown commands) is refused.
pub fn memory_command_is_read_only(name: &str, args: &str) -> bool {
    let first = args.split_whitespace().next();
    match name {
        "search" | "doctor" | "people" | "sleeptime" => true,
        "memory" | "facts" => first.is_none(),
        "memfs" => matches!(first, Some("status" | "diff" | "tokens")),
        "memory-repository" => first == Some("status"),
        _ => false,
    }
}

/// Whether a memory-write-denied session may run this extension command invocation. A prompt that
/// starts with `/name` dispatches ANY registered extension command, so a denied session keeps only an
/// explicit read-only allowlist: the read-only memory command forms, plus commands whose source only
/// reads: `/tasks` (lists tasks), `/history` (searches prompt history, writes nothing), `/rules`
/// (every subcommand inspects loaded rules) and `/hooks` with no argument, `list` or `diagnostics`
/// (`trust`/`enable`/`disable` write hook trust storage, `reload` reloads extensions). Everything
/// else is refused, among them `/todo` (`export` writes any path), `/palace` (writes a viewer under
/// the identity root), `/files` (spawns an editor process) and `/loop` (schedules prompts).
pub fn memory_write_denied_command_allowed(name: &str, args: &str) -> bool {
    let mut tokens = args.split_whitespace();
    match name {
        "tasks" | "history" | "rules" => true,
        "hooks" => matches!((tokens.next(), tokens.next()), (None | Some("list" | "diagnostics"), None)),
        _ => memory_command_is_read_only(name, args),
    }
}

/// The typed refusal of an extension command invocation in a memory-write-denied session, `None`
/// when [`memory_write_denied_command_allowed`] keeps it. Hosts install it as the session's
/// extension command gate.
pub fn memory_write_deny_command_refusal(name: &str, args: &str) -> Option<String> {
    (!memory_write_denied_command_allowed(name, args)).then(|| memory_write_denied_message(&format!("/{name}")))
}

/// Whether an effect-tool-denied session may run this extension command invocation: the read-only
/// allowlist of [`memory_write_denied_command_allowed`] plus every memory command (memory writes are
/// allowed). Everything else is refused, among them `/todo` (`export` writes any path), `/files`,
/// `/loop`, `/palace` and `/hooks trust|reload`.
pub fn effect_tools_denied_command_allowed(name: &str, args: &str) -> bool {
    memory_write_denied_command_allowed(name, args) || crate::commands::register::MEMORY_COMMAND_NAMES.contains(&name)
}

/// The typed refusal of an extension command invocation in an effect-tool-denied session, `None`
/// when [`effect_tools_denied_command_allowed`] keeps it.
pub fn effect_tools_deny_command_refusal(name: &str, args: &str) -> Option<String> {
    (!effect_tools_denied_command_allowed(name, args)).then(|| effect_tools_denied_message(&format!("/{name}")))
}

/// The refused command name (`/name`) when this invocation must be refused in this session.
type CommandRefusal = std::sync::Arc<dyn Fn(&str, &maho_ext_api::ExtensionContext) -> Option<String> + Send + Sync>;

fn refuse_command<'a>(context: &maho_ext_api::ExtensionContext, what: String) -> maho_ext_api::ExtensionFuture<'a, ()> {
    context.ui.notify(&memory_write_denied_message(&what), maho_ext_api::NotificationType::Error);
    Box::pin(async move { Err(memory_write_denied_failure(&what)) })
}

/// Wraps the registered memory slash commands so a denied session (host context, or a recorded
/// denied binding) refuses every mutating invocation with the typed denial. A command's
/// command-context handler and argument completions are kept.
pub fn deny_mutating_memory_commands(api: &mut maho_ext_api::ExtensionApi, resolve: crate::prompt::PromptContextResolver) {
    let profile_denied = profile_denies_memory_write(&api.profile);
    let commands = api
        .registered
        .commands
        .iter()
        .filter(|command| crate::commands::register::MEMORY_COMMAND_NAMES.contains(&command.name.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    for command in commands {
        let refusal: CommandRefusal = {
            let resolve = resolve.clone();
            let name = command.name.clone();
            std::sync::Arc::new(move |args, context| {
                let denied = profile_denied
                    || resolve(context.session_manager.session_id()).is_some_and(|identity| identity.memory_write_denied);
                (denied && !memory_command_is_read_only(&name, args)).then(|| format!("/{name}"))
            })
        };
        let completions = api.registered.command_argument_completions.get(&command.name).cloned();
        let context_handler = api.registered.command_context_handlers.get(&command.name).cloned();
        let (name, description, hint) = (command.name.clone(), command.description.clone(), command.argument_hint.clone());
        if let Some(inner) = context_handler {
            let handler: maho_ext_api::CommandContextHandler = std::sync::Arc::new(move |args, context| match refusal(args, &context.context) {
                Some(what) => refuse_command(&context.context, what),
                None => inner(args, context),
            });
            match completions {
                Some(completions) => api.register_command_with_context_and_completions(&name, description, hint, handler, completions),
                None => api.register_command_with_context(&name, description, hint, handler),
            }
            continue;
        }
        let inner = command.handler.clone();
        let handler: maho_ext_api::CommandHandler = std::sync::Arc::new(move |args, context| match refusal(args, context) {
            Some(what) => refuse_command(context, what),
            None => inner(args, context),
        });
        match completions {
            Some(completions) => api.register_command_with_completions(&name, description, hint, handler, completions),
            None => api.register_command(&name, description, hint, handler),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn denied_sessions_keep_only_read_only_command_forms() {
        for (name, args) in [("search", "query"), ("doctor", "--json"), ("people", ""), ("sleeptime", ""), ("memory", ""), ("facts", ""), ("memfs", "status"), ("memfs", "diff"), ("memory-repository", "status")] {
            assert!(memory_command_is_read_only(name, args), "/{name} {args}");
        }
        for (name, args) in [("remember", "x"), ("init", ""), ("recompile", ""), ("reflect", ""), ("dream", ""), ("facts", "retry"), ("memory", "unlock /x"), ("memfs", "init"), ("memfs", "reset"), ("memfs", "restore"), ("memfs", "sync"), ("memfs", ""), ("memory-repository", "push"), ("memory-repository", "set u"), ("unknown", "")] {
            assert!(!memory_command_is_read_only(name, args), "/{name} {args}");
        }
    }

    #[test]
    fn denied_sessions_refuse_every_extension_command_outside_the_read_only_allowlist() {
        for (name, args) in [("tasks", ""), ("tasks", "--all"), ("history", ""), ("rules", "show x"), ("hooks", ""), ("hooks", "list"), ("hooks", "diagnostics"), ("search", "deploy"), ("memfs", "status")] {
            assert_eq!(memory_write_deny_command_refusal(name, args), None, "/{name} {args}");
        }
        for (name, args) in [("todo", "export /tmp/x.md"), ("todo", ""), ("palace", ""), ("files", ""), ("loop", "5m /remember x"), ("hooks", "trust hk_1"), ("hooks", "reload"), ("hooks", "list extra"),
            ("remember", "x"), ("doctor-fix", ""), ("task-kill", ""), ("reload-rules", ""), ("unknown", "")] {
            let refusal = memory_write_deny_command_refusal(name, args).unwrap_or_else(|| panic!("/{name} {args} must be refused"));
            assert!(refusal.starts_with(MEMORY_WRITE_DENIED_CODE) && refusal.contains(&format!("/{name}")), "{refusal}");
        }
    }

    #[test]
    fn denied_sessions_refuse_mcp_tools_outside_the_remote_read_only_allowlist() {
        let none = Value::Null;
        let refused = |name: &str, input: &Value| memory_write_block(true, None, name, input).is_some_and(|reason| reason.starts_with(MEMORY_WRITE_DENIED_CODE));
        // Remote HTTP builtins stay: exact direct names, gateway catalog ops, gateway calls of a listed tool, resources.
        for name in ["mcp_context7_resolve-library-id", "mcp_context7_query-docs", "mcp_grep_app_searchGitHub"] { assert!(!refused(name, &none), "{name}"); }
        assert!(!refused("mcp_context7", &serde_json::json!({"op":"call","tool":"query-docs","args":"{}"})));
        assert!(!refused("mcp_grep", &serde_json::json!({"op":"search","query":"code"})));
        assert!(!refused("mcp_read_resource", &serde_json::json!({"server":"context7","uri":"docs://x"})));
        // A local stdio server (e.g. ast-grep `rewrite` with apply:true), its gateway, look-alikes, ellipsized and suffixed names fail closed.
        for name in ["mcp_ast_grep_rewrite", "mcp_ast_grep_scan", "mcp_fs_write_file", "mcp_git-bash_run", "mcp_context7_query_docs", "mcp_context7_query-docs_1a2b",
            "mcp_context7_evil_tool", "mcp_a_very_long_local_server_name_th...ool_that_writes_files_x", "MCP_FS_write"] {
            assert!(refused(name, &none), "{name}");
        }
        assert!(refused("mcp_ast", &serde_json::json!({"op":"call","tool":"rewrite","args":"{\"apply\":true}"})));
        assert!(refused("mcp_tools", &serde_json::json!({"op":"call","tool":"run_shell"})));
        assert!(refused("mcp_docs", &serde_json::json!({"op":"search","query":"x"})), "an unlisted server's gateway is refused outright");
        assert!(refused("mcp_context7", &serde_json::json!({"op":"call","tool":"write_file"})));
        assert!(refused("mcp_grep", &serde_json::json!({"op":"call","tool":"query-docs"})), "a listed tool only through its own server's gateway");
        assert!(refused("mcp_read_resource", &serde_json::json!({"server":"fs","uri":"file:///x"})));
        assert!(refused("mcp_list_resources", &serde_json::json!({})));
        assert_eq!(memory_write_block(false, None, "mcp_fs_write_file", &none), None, "an owner session keeps every MCP tool");
    }

    #[test]
    fn effect_denied_sessions_refuse_effect_and_unlisted_mcp_tools_but_keep_memory_tools() {
        let none = Value::Null;
        let refused = |name: &str, input: &Value| effect_tools_block(true, name, input).is_some_and(|reason| reason.starts_with(EFFECT_TOOLS_DENIED_CODE));
        // Memory writes stay: the omo-memory MCP tools directly and through their gateway, and the native tools.
        for name in ["mcp_omo-memory_memory", "mcp_omo-memory_memory_apply_patch", "memory", "memory_apply_patch", "read", "mcp_context7_query-docs", "mcp_grep_app_searchGitHub"] {
            assert!(!refused(name, &none), "{name}");
        }
        assert!(!refused("mcp_omo-memory", &serde_json::json!({"op":"call","tool":"memory_apply_patch","args":"{}"})));
        assert!(!refused("mcp_omo-memory", &serde_json::json!({"op":"search","query":"memory"})));
        assert!(!refused("mcp_read_resource", &serde_json::json!({"server":"grep_app","uri":"x"})));
        // Effect tools, local stdio servers (file writers, shells), their gateways and look-alikes fail closed.
        for name in ["bash", "powershell", "eval", "apply_patch", "task", "task_send", "team_create", "mcp_ast_grep_rewrite", "mcp_fs_write_file",
            "mcp_desktop_execute_command", "mcp_slack_post_message", "mcp_omo-memory_evil", "mcp_omo-memory-x_memory", "mcp_omo-memory_memory_1a2b"] {
            assert!(refused(name, &none), "{name}");
        }
        assert!(refused("mcp_ast", &serde_json::json!({"op":"call","tool":"rewrite","args":"{\"apply\":true}"})));
        assert!(refused("mcp_desktop", &serde_json::json!({"op":"call","tool":"execute_command"})));
        assert!(refused("mcp_omo-memory", &serde_json::json!({"op":"call","tool":"write_file"})), "the memory gateway calls only memory tools");
        assert!(refused("mcp_read_resource", &serde_json::json!({"server":"omo-memory","uri":"x"})), "resources reach only the remote read-only servers");
        assert_eq!(effect_tools_block(false, "mcp_fs_write_file", &none), None, "an unrestricted session keeps every MCP tool");
        assert_eq!(effect_tools_block(false, "bash", &none), None);
    }

    #[test]
    fn effect_denied_sessions_keep_memory_commands_and_the_read_only_allowlist() {
        for (name, args) in [("tasks", ""), ("history", ""), ("rules", "show x"), ("hooks", "list"), ("remember", "x"), ("reflect", ""), ("memfs", "sync"), ("search", "deploy")] {
            assert_eq!(effect_tools_deny_command_refusal(name, args), None, "/{name} {args}");
        }
        for (name, args) in [("todo", "export /tmp/x.md"), ("palace", ""), ("files", ""), ("loop", "5m /x"), ("hooks", "trust hk_1"), ("hooks", "reload"), ("unknown", "")] {
            let refusal = effect_tools_deny_command_refusal(name, args).unwrap_or_else(|| panic!("/{name} {args} must be refused"));
            assert!(refusal.starts_with(EFFECT_TOOLS_DENIED_CODE) && refusal.contains(&format!("/{name}")), "{refusal}");
        }
    }

    fn binding_entry(data: Value) -> SessionEntry {
        SessionEntry {
            id: "entry".into(),
            parent_id: None,
            timestamp: "fixed".into(),
            kind: "custom".into(),
            data: serde_json::json!({"type":"custom","customType":crate::binding::MEMORY_BINDING_CUSTOM_TYPE,"data":data}),
        }
    }

    #[test]
    fn write_tool_calls_cover_direct_mcp_and_gateway_surfaces_only() {
        let none = Value::Null;
        for name in ["memory", "Memory", "memory_apply_patch", "memory-apply-patch", "mcp_omo-memory_memory", "mcp_omo-memory_memory_apply_patch", "mcp_omo-memory"] {
            assert!(is_memory_write_tool_call(name, &none), "{name}");
        }
        assert!(is_memory_write_tool_call("mcp_other", &serde_json::json!({"op":"call","tool":"memory_apply_patch"})));
        assert!(!is_memory_write_tool_call("mcp_other", &serde_json::json!({"op":"search","query":"memory"})));
        for name in ["read", "bash", "memoryx", "mcp_other_search_memory", "memory_search"] {
            assert!(!is_memory_write_tool_call(name, &none), "{name}");
        }
    }

    #[test]
    fn any_denied_binding_keeps_the_session_denied() {
        let binding = crate::binding::create_memory_binding("agent", "repo", 1.0);
        let denied = binding_entry(binding_entry_value(&binding, true));
        let allowed = binding_entry(binding_entry_value(&binding, false));
        assert!(recorded_memory_write_denied(std::slice::from_ref(&denied)));
        assert!(recorded_memory_write_denied(&[denied.clone(), allowed.clone()]), "a later unmarked binding cannot lift the denial");
        assert!(recorded_memory_write_denied(&[allowed.clone(), denied]));
        assert!(!recorded_memory_write_denied(&[allowed]));
        assert!(!recorded_memory_write_denied(&[]));
    }

    #[test]
    fn denied_sessions_block_writes_shells_and_spawners_but_not_reads() {
        let paths = memory_core::identity::layout::build_identity_paths(std::path::Path::new("/memory-home"), "agent");
        let root = paths.root.to_string_lossy().into_owned();
        let context = MemoryIdentityContext::new("agent".into(), paths, crate::binding::create_memory_binding("agent", "repo", 1.0)).with_memory_write_denied(true);
        let shell = serde_json::json!({"command":format!("cd {root} && git commit -am x")});
        assert!(memory_write_block(true, Some(&context), "bash", &shell).unwrap().starts_with(MEMORY_WRITE_DENIED_CODE));
        assert!(memory_write_block(true, Some(&context), "custom_bash", &shell).unwrap().starts_with(MEMORY_WRITE_DENIED_CODE));
        assert_eq!(memory_write_block(true, Some(&context), "custom_bash", &serde_json::json!({"command":"cargo test"})), None);
        assert!(memory_write_block(true, Some(&context), "memory", &Value::Null).is_some());
        // A relative or `$HOME` path defeats any path match, so the whole policy-bypassing family is refused.
        for name in ["bash", "powershell", "bash_input", "monitor", "eval", "apply_patch", "task", "task_send", "team_create", "workflow", "workpool", "computer"] {
            assert!(memory_write_block(true, Some(&context), name, &serde_json::json!({"command":"cd $HOME/.omo && echo x >> a"})).is_some(), "{name}");
        }
        assert!(memory_write_block(true, Some(&context), "mcp_git-bash_run", &Value::Null).is_some());
        assert!(memory_write_block(true, Some(&context), "mcp_tools", &serde_json::json!({"op":"call","tool":"run_shell"})).is_some());
        assert!(memory_write_block(true, Some(&context), "mcp_docs", &serde_json::json!({"op":"call","tool":"search"})).is_some(), "an unlisted MCP server fails closed");
        assert_eq!(memory_write_block(true, Some(&context), "mcp_grep_app_searchGitHub", &serde_json::json!({"query":"x"})), None);
        assert_eq!(memory_write_block(true, Some(&context), "read", &serde_json::json!({"path":root})), None);
        assert_eq!(memory_write_block(true, Some(&context), "write", &serde_json::json!({"path":"notes.md"})), None, "file tools are left to the filesystem policy");
        assert_eq!(memory_write_block(false, Some(&context), "bash", &shell), None);
        assert_eq!(memory_write_block(false, None, "memory", &Value::Null), None);
    }

    #[test]
    fn deny_exclusions_cover_exact_names_and_family_prefixes() {
        let exclusions = memory_write_deny_tool_exclusions();
        for entry in ["memory", "memory_apply_patch", "bash", "apply_patch", "eval", "workpool", "task_*", "team_*"] {
            assert!(exclusions.iter().any(|excluded| excluded == entry), "{entry}: {exclusions:?}");
        }
        for name in ["task_send", "team_wait", "dag", "monitor", "lsp_rename", "ast_grep_replace", "generate_image"] { assert!(memory_write_deny_excludes_tool(name), "{name}"); }
        for name in ["read", "write", "edit", "grep", "find", "ls", "supervisor_submit_work", "tasks", "lsp_diagnostics", "ast_grep_search"] { assert!(!memory_write_deny_excludes_tool(name), "{name}"); }
    }

    #[test]
    fn only_the_exact_deny_value_denies() {
        let mut profile = ExtensionSessionProfile::default();
        assert!(!profile_denies_memory_write(&profile));
        profile.session_context.insert(MEMORY_WRITE_CONTEXT_KEY.into(), "allow".into());
        assert!(!profile_denies_memory_write(&profile));
        profile.session_context.insert(MEMORY_WRITE_CONTEXT_KEY.into(), MEMORY_WRITE_DENY_VALUE.into());
        assert!(profile_denies_memory_write(&profile));
    }
}
