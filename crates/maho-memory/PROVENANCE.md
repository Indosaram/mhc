# Provenance — maho-memory

- License: MIT.
- Method: **P** (port of MIT originals) for the store, recall, MCP server and agent tools; **C**
  (clean-room, from the specs under `docs/cleanroom/specs/`) for the legacy importer
  (`src/legacy.rs`), which has no MIT original, and for the Phase 1-7 clean-room modules listed
  below, which reimplement behaviour of the SUL crate `crates/omo/memory-core` from its specs
  without copying its source.
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
  opened **while writing the legacy importer or the MIT-original port above**. The legacy format
  spec was written from user docs and MIT call sites only (see the spec's source table).
- Tests were written for this crate; no upstream or OmO fixtures were reused.

## Phase 1-7 clean-room modules (method C)

- Scope: the modules this crate gained from the Phase 1-7 memory clean-room track, each described by
  a spec under `docs/cleanroom/specs/memory-*.md`.
- Method: **C** (clean-room). Each module was written from its spec plus the MIT tree. Behaviour was
  cross-checked by reading `crates/omo/memory-core` (SUL) as a **behaviour reference only** — what
  the observable contract is, not how it is expressed. No SUL source text was copied; these modules
  are a different shape from their references (flat module names rather than nested directories,
  different types, independently written fixtures).
- Where a reference's own architecture does not exist in this crate, the port stops and the spec
  records why, rather than recreating a module with no caller.
- Tests were written for this crate; no SUL or upstream fixtures were reused.

## memory-format-5152 (method C)

- Unit: `memory-format-5152`. Method: **C** (clean-room) from the `## Delta (omo v5.1.52,
  2026-10-30)` section of `docs/cleanroom/specs/memory-format.md` at spec commit `80074bfa`.
- Implemented in `src/legacy.rs` (`parse_expires`, `expiry_of`, `format_instant`, `import_at`,
  `ImportReport::expired_on_import`, `valid_until` and `metadata_json.expires` on imported rows)
  and `src/recall.rs` (`RecallOptions::valid_at`, the instant `valid_until` is checked against;
  defaults to the current clock). Only the spec and this MIT tree were used; no SUL or upstream
  source was opened. Each given/when/then case of the delta has a test in `src/legacy.rs`.
