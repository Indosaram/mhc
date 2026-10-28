# Provenance — maho-memory

- License: MIT.
- Method: **P** (port of MIT originals) for the store, recall, MCP server and agent tools; **C**
  (clean-room, from `docs/cleanroom/specs/memory-format.md`) for the legacy importer
  (`src/legacy.rs`), which has no MIT original.
- Origin (license verified as MIT before reading): `can1357/oh-my-pi` @
  `f59bbac3d56e8b485fef466388b1cb5be4b93123` (root LICENSE: MIT, Copyright 2025 Mario Zechner,
  2025-2026 Can Bölük, 2026 Stencil Labs, Inc.; `packages/mnemopi/package.json` `"license": "MIT"`,
  `packages/mnemopi/LICENSE` MIT).
  - `packages/mnemopi/src/core/beam/schema.ts` (`initBeam`), `core/triples.ts` (`initTriples`)
    → `src/schema.rs` (same tables, columns, defaults, FTS5 mirrors and triggers, for the subset
    used here);
  - `core/beam/store.ts` (`remember` with same-session duplicate merge, `get`, `updateWorking`,
    `forgetWorking`, `invalidate`, `getWorkingStats`, scratchpad, `exportToDict`,
    `importFromDict`), `core/triples.ts` (`TripleStore.add`/`query`), `db.ts` (pragmas),
    `util/ids.ts` (`sha256Hex16`, `generateId`, `stableMemoryId`), `util/datetime.ts`
    (`toUtcIso`) → `src/store.rs`;
  - `core/beam/recall.ts` (`recall`, `ftsRows`, `normalizeRanks`, `fetchCandidates`,
    `fallbackCandidates`, `scoreCandidate`, `dedupeResults`, `updateRecallCounts`,
    `clipRecallContent`, `recencyDecay`, `temporalBoost`, trust weights), `config.ts`
    (`normalizedRecallWeights`, veracity/tier weight defaults and `MNEMOPI_*_WEIGHT` overrides)
    → `src/recall.rs`;
  - `core/synonyms.ts` (`SYNONYM_GROUPS`, `getSynonyms`), `util/regex.ts` (`hasCjk`,
    `matchesWordForm`), tokenizer and relevance helpers of `core/beam/recall.ts` → `src/text.rs`;
  - `core/banks.ts` (`BankManager`) and `packages/coding-agent/src/mnemopi/config.ts`
    (`computeMnemopiBankScope`, `projectBank`, `projectBankSegment`, `sanitizeBankName`,
    `limitBankName`) → `src/banks.rs`;
  - `src/mcp-tools.ts` (schemas, argument coercion, result shapes) and `src/mcp-server.ts`
    (`handleJsonRpc`, `runStdio`) → `src/mcp.rs`;
  - `packages/coding-agent/src/tools/memory-retain.ts`, `tools/memory-recall.ts`,
    `prompts/tools/{retain,recall}.md`, `prompts/system/mnemopi-instructions.md`,
    `mnemopi/state.ts` (`beforeAgentStartPrompt`, `formatRecallBlock`, `mergeRecallResult`,
    `compareRecallResults`) → `src/extension.rs` (prompt texts adapted to mhc's tool names).
- Not ported: embeddings (fastembed / remote), vector and binary-vector indexes, polyphonic and
  enhanced recall, query cache, temporal parser, MMR and coverage diversification, LLM and
  heuristic extraction, consolidation (`sleep`), episodic degradation, annotations, entities,
  episodic graph, veracity consolidation, memoria tables, shared-surface DB, plugins, streaming,
  diagnostics, CLI; the coding-agent's automatic retain, `reflect`, recall persistence/resume
  change reports, Hindsight and other backends.
- No SUL source (`crates/omo/**`, including `memory-core` and `maho-omo-memory`; the omo-gated
  `maho-cli` memory modules `memory_runtime`, `recall_wiring`, `kibitzer_*`, `facts_child`) was
  opened. The legacy format spec was written from user docs and MIT call sites only (see the spec's
  source table).
- Tests were written for this crate; no upstream or OmO fixtures were reused.
