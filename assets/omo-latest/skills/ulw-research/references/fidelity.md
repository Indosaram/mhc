# ulw-research - source fidelity

Read this when writing or re-entering the `## Fidelity` block of `brief.md`, asking the fidelity questions, or when a `fact` command refuses with `fidelity_interview_incomplete`, `brief_changed`, `derived_not_allowed`, or `source_outside_floor`. The inference step that decides each field is step 4 of [deliverable-phase.md](deliverable-phase.md) section 4.

## The fidelity fields and the lock

The values below are read by `scripts/fidelity-policy.mjs`, the single registry: `SOURCE_FLOOR_RULES` and `parseSourceFloor()` for the `source floor` forms (`pointedFlag` in source registration reads the same grammar), and `FIDELITY_VALUE_RULES` for `derived numbers`, `languages`, `fallback`, and `meaning check`. `derived numbers` and `fallback` accept only the values in their row below (`FIDELITY_VALUE_RULES.tables`, any case); any other value, a free-text answer included, is a usage error (exit `2`) naming the field and the value, as an unknown `source floor` form is. Map a free-text answer onto a table value before writing it: a partial allowance ("growth rates yes, estimates no") records `derived numbers: none`.

Beside `## Deliverable`, the brief records what the report must stay faithful to, under `## Fidelity`. The fields are inferred first; only `as of`, `meaning check`, `source floor`, and `derived numbers` may be asked, each on its own trigger, in the same question call as Q1-Q3 (at most four questions in that call). The inference rules and the triggers are step 4 of [deliverable-phase.md](deliverable-phase.md) section 4; the question texts are below.

| Field | Values | Default | Read by |
| --- | --- | --- | --- |
| `as of` | `latest at run start, <YYYY-MM-DD>`; one anchor (version, release, tag, commit, date, period); `multiple: <anchor>, <anchor>; rows tagged` | `latest at run start, <run date>` | `fact verify` (`quote_stale_version` when a quoted source's revision is not the anchor) |
| `meaning check` | `on`, `off`, `on; force strong: <scope>` | `on` in the full tier, `off` in the light tier | `fact pairs` (`off` writes `{"skipped": true}`) and the orchestrator's reader lanes |
| `source floor` | `standard`, `primary only`, `pointed only: <list>`, `classes: <list>` | `standard` | `fact fill` and `fact verify` (`source_outside_floor`) |
| `derived numbers` | `allowed`, `none` | `allowed` | `fact fill` and `fact verify` (`derived_not_allowed`) |
| `languages` | `report <lang>; sources <langs>; quotes original`, or `...; quotes original with translation beside` | report in the requester's language; quotes with translation when a source language differs | `fact fill` (whether a translation binds to its quote) |
| `identifier classes` | `none`, or a list of token classes | from the domain of the Phase 0 axes | recorded only: no gate enforces it yet |
| `fallback` | `block-quote`, `omit` | `block-quote` | `fact fill --replace-failed` |

### The fidelity questions

Ask them in the user's language, translating these English texts faithfully and keeping the option order; `<run date>` is the `$SESSION_DIR` date.

- **`as of`**, header `As of`: "Which version or date must the facts hold for? Sources are snapshotted and quoted against that anchor."
  1. `Latest as of today, <run date>` - The state published at the run's start; anything older is flagged as stale.
  2. `Other: a named version, release, or date` - Type it (v3.2, 2026-Q3, a commit or tag). Every quote and number is checked against sources valid at that anchor.
  3. `Don't care, you decide` - Resolves to option 1.
- **`meaning check`**, header `Drift check`: "Numbers and quotes are always script-checked against source snapshots. Should every explanatory sentence also be read against its source quote by a separate reader? Slower and more tokens; it catches widened scope and dropped hedges."
  1. `On` - Every sentence is paired with its source and read; confirmed drift is rewritten from the quote or listed in the briefing.
  2. `Off (faster)` - Script checks only: numbers, units, quotes, qualifiers, bindings. A paraphrase that shifts meaning is not read.
  3. `Other: where to focus` - Type it ("the financial sections", "anything about the competitor"); those pairs go to the strongest reader.
  4. `Don't care, you decide` - Resolves to option 1.
- **`source floor`**, header `Sources`: "Which sources may back a statement of fact?"
  1. `Standard: primary source required for high-risk claims` - Numeric, legal, financial, dated, and causal claims need a primary source plus two independent domains; other claims may cite attributed secondary sources.
  2. `Primary only` - Every fact traces to a first-party source (standard, filing, dataset, official doc); commentary appears only as attributed opinion.
  3. `Only the sources I pointed at` - Ledger rows come from the named files and URLs only; anything else is context, not evidence.
  4. `Other: name the source classes` - Type it ("peer-reviewed only", "no vendor blogs").
  5. `Don't care, you decide` - Resolves to option 1.
- **`derived numbers`**, header `Derived`: "May the report state numbers no source states (sums, growth rates, shares, unit conversions, estimates)?"
  1. `Yes, computed by script and labeled DERIVED` - Each one names its operation and input rows; inputs are checked for unit, period, and population compatibility; an estimate carries its assumptions.
  2. `No, sourced numbers only` - Every number is a MEASURED ledger value; comparisons are stated in words or shown side by side.
  3. `Other: describe` - Type it ("growth rates yes, estimates no"). A partial allowance is recorded as `none`, and the reply that opens collection says so.
  4. `Don't care, you decide` - Resolves to option 1.

### The `## Fidelity` block

Write it in the same step as `## Deliverable`, provisional defaults included, one line per field with its `answered_by`, plus an `asked:` line naming the fidelity questions sent:

```text
## Fidelity
as of: latest at run start, 2026-10-23 (answered_by: default)
meaning check: on (answered_by: default)
source floor: standard (answered_by: default)
derived numbers: pending (answered_by: pending)
languages: report en; sources en; quotes original (answered_by: default)
identifier classes: version strings, package names, commit ids (answered_by: default)
fallback: block-quote (answered_by: default)
asked: derived numbers
```

Each line is `<field>: <value> (answered_by: user|default|request|pending)`; values carry no parentheses, `pending` goes only with `answered_by: pending`, and `asked:` is an optional history line. A malformed line or a second block is a usage error (exit `2`) naming the line. When an answer lands, rewrite that one line with `answered_by: user` and leave `asked:` as history; a late answer after `fact fill` is a re-render: fill again, then verify.

**The lock.** The brief is locked when all seven fields are present and none is `pending`. Six stages refuse an unlocked brief with one `fidelity_interview_incomplete` defect (blocker, integrity), exit `1`, and write nothing: `fact fill`, `fact verify`, `fact pairs`, `fact meaning-check`, `fact polish-apply`, `fact polish-diff`. Each reads `--brief`, default `$SESSION_DIR/brief.md`. Collection never waits: `source snapshot`, `fact add`, `fact quote`, `fact derive`, `fact retract`, `fact export`, and `claims index` never read the lock. Provisional defaults are written at brief time, so the lock fires only on a `pending` field still open when the assembly lane is about to start; that one question is then asked again, blocking.

**What fill and verify enforce.** At every `fact fill`, each placeholder's row is checked against the current lock, and `fact verify` checks every `data-fact` element of the edition against it again, so tightening the brief after fill fails the next verify:

- `derived numbers: none` refuses a `derived`, `converted`, or `assumed` row (`derived_not_allowed`): the requester was promised measured values only.
- The source floor is evaluated live against the `meta.json` of every source behind the row (a derived row's sources are its operands' sources): `pointed only: <list>` admits a source whose id (`S4`), URL, or path is in the list; `primary only` admits a source whose declared provenance is `primary` (`source snapshot --provenance primary|secondary`, default `unknown`; `secondary` and `unknown` are refused); `classes: <list>` admits a source whose URL host equals a listed host suffix or ends with `.<suffix>` (`sec.gov`, `go.kr`); `standard` refuses nothing. Under any floor but `standard`, a row resting on no source (an `assumed` row, or a derived row with such an operand) is refused as well: no source of it can be inside the floor. A refused row is `source_outside_floor`. Rows registered while the floor was pending are reclassified the moment it locks or changes. The row's `outside_floor` is the cached result of the last fill, never an input; fill writes it to the ledger even when it refuses.
- `languages` with `quotes original with translation beside` lets a `{{Q7:label}}` be followed by its translation, `<span data-translates="Q7">...</span>` in HTML or `<!-- translates: Q7 -->` and the sentence in Markdown; fill binds it to Q7, and `fact pairs` emits it as a `translation` pair. Under `quotes original` fill does not bind it.
- `fallback: omit` makes `--replace-failed` remove a twice-failed sentence instead of replacing it with an attributed block quote.
- Fill records `brief_sha256` in `<out>.fill-map.json`: the sha256 of the locked block normalized to its seven `field: value` lines in field order (`answered_by` and `asked:` excluded). `fact verify` requires that map for every edition, HTML or Markdown (`--fill-map`, else whichever of `<report>.fill-map.json` and the `<report>.polished.fill-map.json` that `fact polish-apply` writes records the edition's bytes), and refuses with `brief_changed` when there is no map, when its hash is not the current block's, or when it records none; re-fill, then verify.

**Re-entry.** An `edit-existing` session, or any session created before this block existed, has no `## Fidelity`. Before any `fact` command, run the inference step ([deliverable-phase.md](deliverable-phase.md) section 4, step 4) and write the block with `answered_by: default` on every field. `claims index` on a free-form `claim-graph.md` (no claim table) returns `{claims: [], errors: [...]}`, exits 2, and writes no `claims.json`; run `fact verify` without it, and the gate reports each unbound sentence as `unbound_sentence` (major) instead of blocking.

**Calibration.** `fact meaning-check` appends one line per flagged pair to `--calibration` (default `$SESSION_DIR/fidelity-calibration.jsonl`). After each run, also append that run's new lines to the workspace-level `.omo/ulw-research/fidelity-calibration.jsonl`; promotion of `meaning_drift_confirmed` is decided from that log.
