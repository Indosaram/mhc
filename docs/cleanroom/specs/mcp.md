# MCP client and builtin MCP servers — MIT profile (Phase 3, step 3)

Status: spec for `crates/maho-mcp-builtins`. Written from MIT sources (the senpi MCP client
already ported as `crates/builtins/maho-ext-mcp`, oh-my-pi @ f59bbac
`docs/mcp-server-tool-authoring.md`) and the public documentation of the two remote services
(Context7: `github.com/upstash/context7` README and `docs/resources/all-clients.mdx`; grep.app's
public MCP endpoint). No SUL source was used; the OmO builtin-MCP component was not read.

## 1. MCP client (reused)

The MIT profile already mounts `maho-ext-mcp` (MIT, port of senpi's MCP client). It provides:

- transports: stdio (spawned command, cleaned environment, process-tree shutdown) and streamable
  HTTP (with SSE responses, bearer/OAuth auth, step-up);
- configuration: `<agent dir>/mcp.json` (global), project `.omo/mcp.json` (only when the project
  is trusted), optional Claude config import; `mcpServers.<name>` with `command`/`args`/`env`/`cwd`
  or `url`/`headers`, `enabled`, `lifecycle` (`lazy`, `eager`, `keep-alive`), timeouts,
  `includeTools`/`excludeTools`, `directTools`, `exposure` (`auto`, `direct`, `search`, `proxy`);
- tool namespacing: each MCP tool is exposed as `mcp_<server>_<tool>` (non `[A-Za-z0-9_-]`
  characters become `_`, names longer than 64 characters are shortened in the middle, colliding
  names get a 4-hex-digit suffix); `auto` exposure registers the tools directly up to the search
  threshold (default 10) and otherwise behind tool search;
- enable/disable: `enabled: false` in config, `/mcp enable|disable <name>` (global config), and
  extension-declared servers (`ExtensionApi::register_mcp_server`), over which any user/project
  config entry of the same name wins.

Nothing in this step re-implements the client. "The MCP manager" for the MIT profile is this
extension; this spec only adds builtin server declarations on top of it.

## 2. Builtin servers

Two remote documentation/code-search services, declared as extension MCP servers:

| name | transport | url | auth |
| --- | --- | --- | --- |
| `context7` | streamable HTTP | `https://mcp.context7.com/mcp` | none; when `CONTEXT7_API_KEY` is set, bearer token from that variable |
| `grep_app` | streamable HTTP | `https://mcp.grep.app` | none |

Both are declared with `lifecycle: lazy`, `exposure: auto` and no OAuth (`auth: disabled` when no
key is configured, so a 401 never opens a browser login).

No web-search builtin is added: the MIT profile already has `websearch`/`webfetch` builtin
extensions. Other OmO-era builtins are not part of this list.

## 3. Turning them on and off

Evaluated when the extension registers (once per session runtime):

1. `MAHO_BUILTIN_MCPS` set to `0`, `false`, `off` or `no` (case-insensitive) → none is declared.
2. Offline mode (`PI_OFFLINE` or `MAHO_OFFLINE` truthy, which `--offline` sets) → none is
   declared (they are remote services).
3. `omo.jsonc` key `mcp.builtins: false` (merged view for the cwd) → none is declared.
4. `omo.jsonc` key `mcp.disabled_builtins: ["context7", ...]` → those are not declared. Unknown
   names are ignored. Like `disabled_skills`, this list is a union over layers (a later layer
   cannot re-enable a builtin an earlier layer disabled).
5. A user/project `mcp.json` entry with the same name replaces the builtin (client rule, §1); with
   `enabled: false` it disables it.

New config keys (added to config-keys.md and the `maho-config` schema):

- `mcp` : object
- `mcp.builtins` : boolean [default=true]
- `mcp.disabled_builtins` : array<string>

They are also accepted inside the `[native]`/`[senpi]`/`[codex]` harness blocks and profiles,
like the other typed sections.

## 4. Mounting

`maho-cli` mounts the extension as `<builtin:mcp-builtins>` in the MIT profile only. The default
(`omo`) build keeps its own builtin servers and does not mount this one.

## 5. Tests (written from this spec)

1. Declarations: both servers by default, with the URLs above, HTTP transport, lazy lifecycle.
2. `CONTEXT7_API_KEY` → `context7` declared with bearer auth from that variable.
3. Env switch, offline mode, `mcp.builtins: false` and `mcp.disabled_builtins` each remove what
   §3 says; the union across user and project layers holds.
4. Merging the declarations into `maho-ext-mcp`'s resolved config: a user `mcp.json` entry of the
   same name wins with a diagnostic; the other builtin stays enabled.
5. End to end (smoke): an `mcp.json` stdio server is started by `mhc -p`, its tool is offered as
   `mcp_<server>_<tool>` and a model call to it returns the server's answer; builtins are absent
   in offline mode.
