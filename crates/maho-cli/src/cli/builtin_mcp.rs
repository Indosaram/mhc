//! MIT profile (`--no-default-features`): the builtin MCP servers from `maho-mcp-builtins`
//! (`context7`, `grep_app`), mounted as `<builtin:mcp-builtins>` and declared on the MCP client
//! (`maho-ext-mcp`). Off with `MAHO_BUILTIN_MCPS=0`, in offline mode, with `mcp.builtins: false`
//! or per server with `mcp.disabled_builtins` in `omo.jsonc`; a user/project `mcp.json` entry of
//! the same name replaces a builtin. The `omo` build does not mount it.

/// The builtin MCP server extension.
pub fn extension() -> maho_mcp_builtins::McpBuiltinsExtension {
    maho_mcp_builtins::McpBuiltinsExtension
}
