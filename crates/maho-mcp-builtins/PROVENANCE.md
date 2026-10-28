# Provenance — maho-mcp-builtins

- License: MIT.
- Method: **C** (clean-room from `docs/cleanroom/specs/mcp.md`) over the existing MIT MCP client
  `crates/builtins/maho-ext-mcp` (port of senpi's MCP client, pinned in PINS.md).
- Inputs: the spec; `maho-ext-api` `McpServerDeclaration`/`register_mcp_server`; `maho-ext-mcp`
  `config.rs` (`load_mcp_config`, `merge_extension_mcp_servers`), `config_schema.rs`,
  `expose/naming.rs`, `expose/policy.rs`; oh-my-pi @ f59bbac `docs/mcp-server-tool-authoring.md`
  (MIT, naming notes only); the public setup docs of the services: Context7
  (`https://github.com/upstash/context7` README and `docs/resources/all-clients.mdx`: endpoint
  `https://mcp.context7.com/mcp`, bearer key `CONTEXT7_API_KEY`) and grep.app's public MCP endpoint
  `https://mcp.grep.app`. The servers are third-party network services; no code of theirs is
  included.
- No SUL source (`crates/omo/**`, in particular `maho-omo-builtin-mcps` and `mcp-stdio-core`; the
  omo-gated `maho-cli` modules; OmO/COMP sources) was opened.
- Tests were written from the spec.
