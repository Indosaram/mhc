# maho-memory (MIT migration Phase 2, step 3)

| Source (MIT) | Commit | Rust |
|---|---|---|
| can1357/oh-my-pi `packages/mnemopi/src/core/beam/schema.ts`, `core/triples.ts` (schema) | f59bbac | crates/maho-memory/src/schema.rs |
| oh-my-pi `packages/mnemopi/src/core/beam/store.ts`, `core/triples.ts`, `db.ts`, `util/ids.ts` | f59bbac | crates/maho-memory/src/store.rs |
| oh-my-pi `packages/mnemopi/src/core/beam/recall.ts`, `config.ts` (weights) | f59bbac | crates/maho-memory/src/recall.rs |
| oh-my-pi `packages/mnemopi/src/core/synonyms.ts`, `util/regex.ts` | f59bbac | crates/maho-memory/src/text.rs |
| oh-my-pi `packages/mnemopi/src/core/banks.ts`, `packages/coding-agent/src/mnemopi/config.ts` | f59bbac | crates/maho-memory/src/banks.rs |
| oh-my-pi `packages/mnemopi/src/mcp-tools.ts`, `mcp-server.ts` | f59bbac | crates/maho-memory/src/mcp.rs |
| oh-my-pi `packages/coding-agent/src/tools/memory-{retain,recall}.ts`, `prompts/tools/{retain,recall}.md`, `prompts/system/mnemopi-instructions.md`, `mnemopi/state.ts` (recall block) | f59bbac | crates/maho-memory/src/extension.rs |
| (no MIT original) legacy Markdown store importer, spec docs/cleanroom/specs/memory-format.md | — | crates/maho-memory/src/legacy.rs |

Differences from mnemopi / oh-my-pi:

- Lexical recall only: no embeddings, vector voice, MMR, coverage diversification, temporal query
  parsing, query cache or enhanced/polyphonic recall. Scoring otherwise follows `scoreCandidate`
  with the vector weight unused.
- No consolidation (`sleep`): every row is written with `consolidated_at` set so a mnemopi TTL trim
  would not drop it, and working memory is never trimmed here.
- `update`/`forget` act on rows of this session or rows with `scope = 'global'` (upstream: this
  session only), because the agent writes bank-wide rows from many sessions.
- Agent tools are `retain`, `recall`, `memory_get`, `memory_forget` (oh-my-pi: `retain`, `recall`,
  `reflect`, `memory_edit`, `memory://` URLs). The first-turn recall block is a hidden custom message
  (`memory-recall`), not a system-prompt section, and is not persisted for resume.
- Project bank names hash the path with SHA-256 (upstream: Bun's hash), so they differ from
  oh-my-pi's for the same directory.
- MCP server: subset of tools (no shared surface, sleep, validate, diagnose, graph); `ping` added.
