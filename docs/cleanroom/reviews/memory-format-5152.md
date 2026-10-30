# Leak-check record: memory-format delta (omo v5.1.52)

Independent review of `docs/cleanroom/specs/memory-format.md`, section
`## Delta (omo v5.1.52, 2026-10-30)` (spec commit 3083fa87, branch `sync5152/spec`). This is
reviewer input only and must not be given to the implementer.

Reviewer session: separate from the spec writer. I compared the delta against:

- upstream target `~/.cache/sync-5152/omo-target/packages/memory-core/src/memfs/expiry.ts`,
  `memfs/expiry.test.ts`, `memfs/hooks-scripts.ts`, `memfs/hooks.test.ts`,
  `compile/cache-expiry.test.ts`, `reflection/assets/{dream,reflection}-persona.md`;
- upstream `packages/omo-senpi/src/components/memory/{tool-metadata.ts,projection-expiry.ts}`;
- `packages/omo-config-core/src/schema/memory.ts` (projection defaults);
- `crates/omo/memory-core/src/memfs` and `crates/omo/components/maho-omo-memory` (the
  current SUL Rust tree). It has no expiry code yet, so the only text the delta could have
  leaked from is upstream;
- the MIT side, `crates/maho-memory/src/{legacy,store,recall,mcp,schema}.rs` and `PROVENANCE.md`;
- the census `/home/projects/mhc/.omo/ulw/sync-5152/census/omo-memory.md`.

## Checklist

- [x] **No SUL code block.** The delta has no fenced block. The regex in `expiry.ts`
      (`EXPIRY_RE`) does not appear, even in prose form. The accepted grammar is stated as
      field rules, and those rules are the interface fact users write against.
- [x] **No translated or paraphrased SUL comment, doc comment or prompt** (after fixes 1-4).
      Before the fixes, three sentences followed SUL text too closely:
      - §1.1 "a datetime without `Z`/offset (it would mean a different instant on each machine)"
        and "A malformed value never hides content" paraphrased the `expiry.ts` header doc
        comment ("an offset-less datetime, which means a different instant on each device",
        "so a malformed value never hides content").
      - §1.2 "Re-validating hand-edited files must never be blocked by it" and the mapping-row
        wording "unreadable value means no expiry and never rejects the file" paraphrased the
        `hooks-scripts.ts` comment ("an unreadable value means no expiry, never a rejection").
      - §1.3 "consistent with the file rule 'at its instant it is expired'" quoted the
        `isMemoryExpired` doc comment ("A file is expired from its instant on").

      None of the delta's text matches the persona prompts (`reflection-persona.md` line 87,
      `dream-persona.md` line 67) or the tool-metadata line (`tool-metadata.ts` line 10).
- [x] **No list or table that mirrors a SUL table row for row** (after fix 5). Before the fix,
      the 1.1 given/when/then cases took their values from upstream `expiry.test.ts`: the
      values `2026-10-28`, `2026-10-27T15:33+09:00`, `2026-10-27T06:33:15.250Z`,
      `2026-10-27T06:33`, `2026-02-30`, `2026-10-27T25:00Z`, `after the outage` and the empty
      value, plus the "Anything else" list in the same order as the test's negative cases.
      §1.3 reused `2026-10-28` from `cache-expiry.test.ts` and `hooks.test.ts`. Rule 5 forbids
      reusing SUL test vectors. The mapping table is census bookkeeping, not a SUL constant
      table. The key `expires`, the date/datetime forms and `valid_until` are interface facts.
- [x] **No OmO-original identifier.** The only new name is `expired_on_import`, which is
      neutral. `expires` is an existing on-disk key, and `valid_until` is the MIT mnemopi
      column (`crates/maho-memory/src/schema.rs:22`).
- [x] **No OmO-original feature.** The delta does not specify the persona/dream time-boxed rule
      (census -20), the pinned-prompt expiry notice (-34/-35/-36) or the compiled-block expiry
      slot (-11/-14). Only the importer behaviour is specified.

## N-A decisions (second check)

The mapping table has 43 rows: 3 APPLIES (-12, -13, -19) and 40 N-A. I sampled 14 N-A rows
against the census and against `crates/maho-memory`:

| row | census behaviour | maho-memory evidence | N-A justified? |
| --- | --- | --- | --- |
| -01..-04 | genesis key bundle, sealing, signing, vector | no sync, keyring or crypto module in `src/` (files: banks, extension, legacy, lib, mcp, recall, schema, store, text) | yes |
| -05 | auth-expired sign-in text (user-visible) | no account sign-in or sync failures exist, so there is no surface to show it | yes |
| -06/-07 | agreements ledger lsTree cache, notice watermark | no agreements, no git calls (`git` appears only as a synonym word in `text.rs:84`) | yes |
| -11/-14 | compiled-block expiry slot and cache version | recall queries SQLite directly (`recall.rs:188,221`); no compiled block | yes |
| -15 | HEAD read from files | `legacy.rs` is read-only and runs no git (spec "Migrator behaviour") | yes |
| -18 | projection defaults 40/24576 -> 0/40960 (user-visible config default) | importer does not read `memory.projection.*` | yes for this spec; the default change belongs to `config-keys.md` (still 40/24576 at lines 312-314) and is noted in the row as out of scope here |
| -20 | persona time-boxed rule | maho-memory ships no reflection/dream personas; OmO-original prompt content (rule 3) | yes |
| -25 | patch tool affectedPaths | MIT tools are `mnemopi_*` (`mcp.rs:19-32`); no patch tool | yes |
| -28/-29 | keychain-pending deferral and retry | no credential store or sync runner | yes |
| -32/-33 | project memory grants and write guard | no file-path tools or guard in the MIT store | yes |
| -34 | "expired since pinned" prompt notice (user-visible) | MIT injects recall results, not a pinned file projection (`extension.rs`); expired rows drop out of recall through `valid_until` (1.3) | yes |
| -37 | tool metadata `expires` hint (user-visible) | `mnemopi_remember` already has a `valid_until` argument ("Optional expiry date or timestamp.", `mcp.rs:76`) | yes |
| -39/-40 | identity reconcile single-flight, user-activity dedup | no identity runtime | yes |
| -46/-47 | worker run reconciliation, Windows tree kill | no background workers or supervisor | yes |

I found no user-observable behaviour of the MIT store that is wrongly marked N-A. The census
rows missing from the table are -24, -30, -31, -38, -48, -49 and -50. The delta's footnote
covered only -24/-30/-31/-38, so I extended it (fix 6). All seven are N-A upstream or routed to
`maho-core`, so no APPLIES row is missing.

## Fixes applied to the spec

1. §1.1: replaced the "different instant on each machine" and "never hides content" phrasing
   with a new rule list. The list is ordered differently from the test's negative cases and
   adds two cases (non-`T` separator, list value). The facts are unchanged.
2. §1.1: made the optional-component nesting explicit (the fraction only after seconds; offset
   hour and minute ranges). This is a precision fix, not a leak fix.
3. §1.2: dropped the hook-comment paraphrase ("never blocked", "unknown key"). The MIT importer
   has no unknown-key reporting anyway, so the old sentence described nothing. The fixed
   warning reason string `unreadable expires value` is now the implementer's own wording
   (rule 6). It is a message, not an interface fact.
4. §1.3: replaced the `isMemoryExpired` doc-comment quote with a statement of the store's
   exclusive bound (`valid_until > now`, `store.rs:436,471`).
5. §1.1 and §1.3: replaced every upstream test vector with new values (2031 dates, a `-05:30`
   offset, a 7-digit fraction that checks truncation, `2031-04-31`, minute 61, `/` separator,
   a list value, new prose). The scenario in §1.3 is new: three items, boundary checked at
   23:59:59.999 and at 00:00:00.000.
6. Mapping table: the -18 reason now points the default change to `config-keys.md`, the -19
   reason was reworded (fix 3), and the out-of-table footnote now also lists -48, -49 and -50.

Remaining note, outside this review's scope: `config-keys.md` still lists the
`memory.projection` defaults as 40/24576, while v5.1.52 uses 0/40960
(`omo-config-core/src/schema/memory.ts:302`). This needs its own config-keys delta.

verdict: approved
