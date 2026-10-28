# Phase 3 step 3 — skill loader additions and builtin MCP servers

Commits: f763b127 (spec), 0b7ce9e4 (crates), 3f30929a (maho-cli wiring + smoke), this note.

## Inputs
- `can1357/oh-my-pi` @ f59bbac3d56e8b485fef466388b1cb5be4b93123 (MIT, already pinned): skill path rules.
- senpi @ 26acd9353 (MIT, already ported in-tree): skill discovery in `maho-core`, MCP client `maho-ext-mcp`.
- Public service docs: Context7 (https://github.com/upstash/context7), grep.app public MCP endpoint.
- Specs: `docs/cleanroom/specs/skills.md`, `docs/cleanroom/specs/mcp.md`.

No SUL path, `crates/omo/**`, `assets/omo*`, `.omo/` or omo-gated maho-cli module was opened.

## Decisions
- The MCP client was reused (`maho-ext-mcp`, MIT senpi port) instead of writing `maho-mcp`; only
  builtin server declarations were added (`crates/maho-mcp-builtins`).
- No web-search MCP builtin: the existing `websearch`/`webfetch` tools cover it.
- `context7` and `grep_app` are declared for online sessions unless `MAHO_BUILTIN_MCPS=0`,
  `mcp.builtins: false` or `mcp.disabled_builtins` turns them off.
- Smoke uses fixture skills; staging the real MIT skill set beside the binary is Phase 4.

## Workspace notes
- Uncommitted rustfmt-only edits found at start were archived to
  `/workspace/work/mhc-clean-uncommitted-team-20261028.tgz`.
- A mid-step workspace rollback lost the first feat commit (c8c0c78c); it was redone as 0b7ce9e4/3f30929a.

## Verification (local only)
maho-skills 8, maho-mcp-builtins 6, maho-config tests, clippy/fmt clean; maho-cli MIT clippy clean,
lib 190; check-mit-profile ok; full mit-profile-smoke ok (browser + tmux).
