# Leak-check record: config-keys delta (omo v5.1.52)

Independent review of `docs/cleanroom/specs/config-keys.md`, section `## Delta (omo v5.1.52)`
(spec commit f5d6e2a1, branch `sync5152/spec`). This is reviewer input only. Do not give it to
the implementer.

This review ran in a session separate from the spec writer's. I compared the delta against:

- upstream target `~/.cache/sync-5152/omo-target/packages/omo-config-core/src/schema/memory.ts`
  (projection schema, lines 220-223, and the settings default, line 302);
- the upstream range diff `0a401eb8c8..v5.1.52 -- packages/omo-config-core`, which touches
  `schema/{config,index,memory,model-pad}.ts` and the tests `memory-overrides.test.ts`,
  `memory.test-support.ts` and `model-pad.test.ts`;
- the current SUL Rust tree `crates/omo/omo-config-core/src/schema/memory.rs:365-420` (still
  40/24576), `src/schema/parity_5138_tests.rs:75-95` and `tests/schema.rs:670`;
- the census `/home/projects/mhc/.omo/ulw/sync-5152/census/omo-*.md` (components,
  desktop-rust, memory, senpi-task-a, senpi-task-b, telemetry) and, for the coverage check,
  `census-discarded/omo-cores.md`;
- the companion specs `config.md` §4.4/§6.4 and `memory-format.md`.

## Checklist

- [x] **No SUL code block.** The delta has no fenced block. Its inline JSON values are
      user-config examples and the default object of `memory.projection`. Both are interface
      facts under rule 2. None of the zod chains (`z.number().int().min(0).default(...)`) or
      the Rust `defaulted(...)` / `nonnegative_integer()` builders appear.
- [x] **No translated or paraphrased SUL comment, doc comment or prompt.** The only comment
      near the changed code is the section banner above `OmoMemoryProjectionSchema` in
      `memory.ts` ("External projection limits ...; 0 disables a limit"). The delta does not
      repeat it. It says nothing about what `0` means, only "it does not change what a given
      value means". The `model-pad.ts` doc comment is not echoed either (see fix 2: the note I
      added gives key names and shapes only). The upstream test titles ("default to no
      per-directory cap and 40960 bytes", "only one limit set ... the other keeps its
      default") do not appear in the delta's wording.
- [x] **No list or table that mirrors a SUL table row for row, except interface facts.** The
      three D1 bullets are key name, type, bound and default. Those are interface facts, and
      they match upstream `memory.ts:220-223,302` exactly (0 / 40960). I checked the test
      vectors against rule 5:
      - upstream `memory-overrides.test.ts` uses `{}` -> `{0, 40960}` and
        `{max_entries_per_directory: 5}` -> `{5, 40960}`;
      - SUL `parity_5138_tests.rs` uses `max_bytes: 0`, `max_bytes: 17`, an agent `reviewer`
        and the values 0/7/-1/1.5;
      - `tests/schema.rs:724` uses 12/8192.

      The delta uses `{"memory": {"enabled": true}}`, `max_bytes: 24576` and
      `max_entries_per_directory: 40`. None of those vectors is reused. The "no projection
      section -> defaults" case is the only possible defaults test, so it is not a copied
      vector. The mapping table is census bookkeeping, not a SUL constant table.
- [x] **No OmO-original identifier.** The delta introduces no identifier. `memory.projection.*`
      and `model_pad.*` are existing user-writable key names (rule 4 keeps on-disk names).
- [x] **No OmO-original feature.** The delta does not specify the external-projection
      name-folding, legend or grouping behaviour (census omo-memory-16/-17/-18). It also leaves
      out the fallback-chain staging (-b-39/-b-40). It changes defaults only.

## Correctness of the delta

- Values: 0 and 40960 match upstream `memory.ts:221-222` and `:302`. 40 and 24576 match the
  pre-delta inventory (`config-keys.md:312-314`) and SUL `memory.rs:367-370,419`.
- Per-agent inheritance: upstream `OmoMemoryAgentOverridesSchema.projection` is the layer
  schema (no defaults). The statement "no default of its own" is therefore correct, and so is
  the agent-inherits-top-level claim.
- Error-case wording was loose (fix 1).

## "No other MIT-routed default changed" claim

I checked the claim against every row in `census/omo-*.md`. 170 rows are routed
`MIT-cleanroom+SUL` and 27 `MIT-direct`. I grepped for default, schema, key, option, setting,
env and section names (`memory.`, `task.`, `teams.`, `computer.`, `telemetry.`, `side_panel`,
`gateway`, `setup`, `init_deep`, `mcp`, `categories`, `model_profile`, `omo.json`):

| row | change | user key in config-keys.md? | result |
| --- | --- | --- | --- |
| omo-memory-18 | projection defaults 40/24576 -> 0/40960 | yes, `memory.projection.*` | covered by D1 |
| omo-senpi-task-b-39/-40 | built-in fallback-chain models (Fable 5.5, Kimi K3.1) | no; `categories.*.model(s)` has no default in the inventory | claim holds |
| omo-components-01/-02, omo-telemetry-13 | `OMO_BUNDLED_SKILLS_DIR` env override | no; env var, not an omo.jsonc key | claim holds (an env var belongs to skills.md if anywhere) |
| omo-components-08 | comment-checker cache dir parameter default | no; internal function default | claim holds |
| omo-components-34, omo-telemetry-04 | telemetry schema drops `machine_hash`, adds model ids | no; collector event schema, not config | claim holds |
| omo-telemetry-07/-08/-20 | telemetry disclosure, notice text, transport defaults | no; `telemetry.enabled` default is unchanged in the census | claim holds |
| omo-memory-29 | `deferredRetryMs` | no; internal option | claim holds |
| omo-desktop-rust-* | engine protocol, capabilities | no | claim holds |

The claim holds for the census as given.

**Coverage gap outside the census (not the author's error).** The upstream range adds a new
top-level key `model_pad` (`schema/model-pad.ts`, commit 678c59d416). It is wired into the
config, layer, typed-harness and profile shapes in `config.ts`, and its only sub-keys are
`labels.{daily,geeky,normal,heavy}` (optional strings, no defaults). The only census row naming
`omo-config-core` is `omo-cores-06` in `census-discarded/omo-cores.md` (marked DIVERGENT,
covering both `memory.ts` and `model-pad.ts`). The shards the author was told to use therefore
never showed it. The SUL tree has no `model_pad` either (grep empty). A new key does not falsify
"no other default changed", since `model_pad` has no default. It is still a user-writable key
missing from the inventory. Without a spec, mhc drops it as an unknown key with a diagnostic
(config.md §4.4 step 4), so a user who copies an upstream `omo.json` with `model_pad` sees a
warning. I recorded this in the delta (fix 2). Whether to add a D2 key entry is a scope decision
for the sync owner. I did not specify behaviour for it, because the pad is a desktop presentation
feature and the census route for it was discarded.

## Fixes applied

1. `config-keys.md` delta, "Error cases": "a validation error for that key (config.md §4) and
   the default applies" was imprecise. config.md §4.4 prunes the bad value as an
   **invalid-value** (step 5), and the merged view then applies the default (§6.4). The sentence
   now cites both steps. No leak was involved.
2. `config-keys.md` delta, Mapping: added an "Outside the census" note naming `model_pad` and
   its sub-keys and shapes (interface facts only, no upstream comment wording). The note says the
   key is unspecified and is treated as unknown until a spec covers it.
3. `memory-format.md:24` (sources table): the stale `=40` / `=24576` defaults now read `=0` /
   `=40960`, with the earlier values noted and a pointer to config-keys.md Delta D1. The spec set
   is now consistent. memory-format.md row omo-memory-18 (line 235) already pointed the change at
   config-keys.md.

Not changed (outside spec-text scope): SUL `crates/omo/omo-config-core` still defaults to
40/24576 (`memory.rs:367-370,419`, `parity_5138_tests.rs:78,81`, `tests/schema.rs:670`). That
is the SUL port's job under census row omo-memory-18.

verdict: approved
