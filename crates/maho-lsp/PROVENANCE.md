# Provenance — maho-lsp

- License: MIT.
- Method: **P** (port of MIT originals) for the client, pool, tools, formatting, workspace edits,
  server table and post-edit diagnostics; **P (design)** for the daemon; **C** for the hook
  interfaces (new, neutral API).
- Origins (licenses verified as MIT before reading):
  - `code-yeongyu/pi-lsp-client` @ `fc8f6124d0022ba8e6ed4e0678007c42d0f5e74e` (LICENSE: MIT,
    Copyright 2026 Yeongyu Kim; NOTICE: MIT re-license of the ported LSP stack by its author).
    Ported: `src/lsp/{transport,connection,client,client-wrapper,manager,config-loader,
    server-resolution,server-installation,server-definitions,language-mappings,infer-extension,
    directory-diagnostics,formatters,workspace-edit,post-edit-diagnostics,errors,utils,constants}.ts`,
    `src/lsp/tools/*.ts`, `src/index.ts` (tools, `/lsp` command, status, post-edit hook).
    The data tables in `src/servers.rs` were generated mechanically from
    `server-definitions.ts` / `language-mappings.ts`.
  - `can1357/oh-my-pi` @ `f59bbac3d56e8b485fef466388b1cb5be4b93123` (LICENSE: MIT,
    Copyright 2025 Mario Zechner) — `packages/coding-agent/src/lsp/mux/{protocol,daemon}.ts`:
    the mux design (one-request handshake binding a link to a server keyed by a SHA-256 of the
    canonical command/args/cwd/env/initializationOptions; cached `initialize`; intercepted
    `shutdown`/`exit`; idle reuse; ping; fallback to a local spawn). Rewritten for a standalone
    `mhc-lsp` binary with neutral method names (`mhcLsp/*`).
- Not ported: the TUI inspector (`inspector.ts`) and themed renderers (`renderers.ts`); `/lsp`
  prints a text snapshot instead.
- Differences: config files are `<cwd>/.omo/lsp-client.json`, `<cwd>/.pi/lsp-client.json`,
  `<agent dir>/lsp-client.json`, `~/.pi/lsp-client.json`; column offsets in workspace edits are
  UTF-16 (LSP default), matching the JS string semantics of the original.
- No SUL source (`crates/omo/**`, including `lsp-core`, `lsp-daemon`, `maho-omo-lsp`,
  `maho-omo-formatter`, `maho-omo-post-mutation`) was opened. The `lsp-daemon` staging name in
  `tools/package-native.mjs` was read from that MIT script only.
- Tests (`src/**` unit tests, `tests/fake_server.rs`, `tests/fixtures/fake_lsp.py`) were written
  for this crate; no upstream or OmO fixtures were reused.
