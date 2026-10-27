# Clean-room specifications — Phase 1 (foundation)

These documents are the hand-off between the **spec writer** and the **implementer** of the
Phase 1 MIT rewrite described in the MIT migration plan (sections 0, 2, Phase 1 and 5). They
describe *what* the new MIT crates must do — observable behaviour, inputs and outputs, on-disk
formats, precedence and error cases — never *how* the SUL-licensed code under `crates/omo/**`
does it.

| File | Covers | Replaces (SUL, to be deleted in Phase 5) |
| --- | --- | --- |
| [config.md](config.md) | `~/.omo/omo.jsonc` + project `.omo/omo.jsonc`: locations, keys, validation, merge, profiles, writer, migrations, startup notices, hot reload | `omo-config-core`, `config-migration`, `maho-omo-config-resolution`, `maho-omo-config-startup`, `maho-omo-config-watch` |
| [config-keys.md](config-keys.md) | Flat inventory of every key a user may write (name, type, default, bounds) | the generated `assets/omo.schema.json` |
| [utils.md](utils.md) | The helper functions MIT code still needs, which ones a standard crate replaces, and which ones are dropped | `utils` (`maho-utils`) |
| [model-selection.md](model-selection.md) | Model reference grammar, catalog aliases, fuzzy matching, delegated-task model choice, model profiles, fallback/retry policy | `model-core`, `maho-omo-model-profile`, `delegate-core` |
| [memory-format.md](memory-format.md) | Phase 2 step 3: on-disk format of the legacy memory store (`~/.omo/memory`) written from user docs and MIT call sites only, and how the MIT migrator maps it into the mnemopi SQLite store | `memory-core` data compatibility (the store itself is replaced by `crates/maho-memory`, ported from MIT mnemopi) |
| [browser-tools.md](browser-tools.md) | Phase 2 step 4: behaviour of the `browser` and `computer` agent tools, written from MIT sources only (omowright, pi-cua-integration, oh-my-pi), since no MIT original has this discrete-action shape | `maho-omo-browser-bridge`, `maho-omo-computer-use` (both unmounted; replaced by `crates/maho-browser`) |
| [telemetry.md](telemetry.md) | Phase 2 step 5: opt-out rules (`MAHO_TELEMETRY`, `DO_NOT_TRACK`, `OTEL_SDK_DISABLED`, config), the local usage ledger and `mhc usage`, and the reduced OTLP export, written from MIT oh-my-pi sources and public OpenTelemetry/Do-Not-Track conventions | `telemetry-core`, `maho-omo-telemetry` (OmO telemetry product dropped; replaced by `crates/maho-telemetry`) |
| [isolation.md](isolation.md) | Phase 3 step 1: task workspace isolation (git worktree / copy backends, baseline and delta capture, patch/branch merge-back, retention, stale cleanup) and the read-only git tools, written from MIT oh-my-pi sources (pi-iso rcopy, task worktree/isolation runner, agentic commit git tools) | `isolation-core`, `git-master` component, `git-bash-mcp` (replaced by `crates/maho-isolation`, `crates/maho-git`) |
| [team.md](team.md) | Phase 3 step 2: team runs on top of `maho-task` (create, members, message, wait/collect, stop; members are tasks, messages are follow-up runs, queued while a run is active), team notifications, and showing delegated tasks in tmux/cmux panes with a headless fallback, written from MIT oh-my-pi (terminal launch, multiplexer detection), pi-agent-teams, pi-interactive-subagents and pi-tmux | `team-core`, `tmux-core` (replaced by `crates/maho-team`) |

## Roles (plan §5)

| Role | May read | Must not read | Produces |
| --- | --- | --- | --- |
| Spec writer (done for these files) | OmO public docs, run results, real `~/.omo` files, and the SUL code *for understanding only* | — | these specs, written in the writer's own words |
| Spec reviewer | the specs and the SUL code | — | a leak-check record (see checklist below) |
| Implementer | these specs, MIT originals (senpi, oh-my-pi, pi-*), mhc MIT code | see "Forbidden inputs" | new MIT crates + tests written from these specs |

The spec writer and the implementer must be **different sessions/people**. The user of this
repository has read and written SUL code, so the user approves specs but does not implement.

### Forbidden inputs for the implementer

The implementer's sandbox/checkout and context must not contain, and the implementer must not
open, any of:

- `crates/omo/**` (all of it, including tests, `parity.md`, golden fixtures);
- the OmO-derived `maho-cli` modules listed in plan §2 ("MIT 크레이트 안의 SUL 유래분"):
  `cli/omo_mount.rs`, `cli/omo_shipped.rs`, `cli/memory_runtime.rs`, `cli/kibitzer_*.rs`,
  `cli/recall_wiring.rs`, `cli/host_transport.rs`, `cli/task_runners.rs`, `cli/task_session.rs`,
  `cli/kernel_tools_host.rs`, `cli/doctor.rs`, and the omo fixtures in `maho-cli/tests/`;
- COMP (`omo-components`, `omo-native-rs`), any OmO / oh-my-openagent source checkout,
  lazycodex `src/` and `dist/`;
- OmO-only `tools/` scripts (`omo-schema.mjs`, `check-omo-sites.mjs`, `latest-omo-*`, `hg-v-*`,
  the omo goldens) and the local `.omo/` evidence directory;
- `assets/omo.schema.json` and `assets/omo-latest/**` (regenerate the schema from
  `config-keys.md` instead);
- any output of `faux-harness --omo` or other reference produced by running the SUL code.

Run the implementation in a checkout from which those paths were removed, and attach the
environment manifest (list of files + hashes) to the implementing commit, as plan §5 requires.

### Allowed inputs for the implementer

- these spec files;
- MIT/Apache sources: senpi, oh-my-pi, pi-* ports already in the tree (for example
  `maho-core::retry_fallback`, `maho-ext-model-fallback`, `maho-ext-recommended-models`,
  `maho-ext-config-reload`, `maho-ext-api` config-watch types);
- the rest of the mhc MIT code (everything outside the forbidden list).

## Rules the specs follow (and the implementer must keep)

1. **Behaviour, not expression.** Specs state contracts and test cases. They contain no code,
   comments, prompt text or struct layouts from the SUL source. Code-like snippets in the specs
   are *user-written config examples* (interface facts), not implementation.
2. **Interface facts are reproduced verbatim** because compatibility requires it: file paths and
   names, JSON keys a user writes, enumerated values, environment variable names, migration
   marker ids stored inside user files, the lock-file record format shared with other tools.
3. **General features only** (plan §0 and the 독창성 table). OmO-original features are not
   specified and must not be built: the magic-keyword maximum-orchestration mode, the
   mass fan-out mode, the persona prompt set, the plan-continuation state-file format.
4. **Neutral names.** New identifiers must not use OmO-specific names (`ultrawork`, `ulw`,
   `boulder`, `Sisyphus`, `Prometheus`, `Oracle`, `Librarian`, `Kibitzer`, `senpi`-prefixed type
   names, "OmO Native" in messages). Each spec proposes neutral names; the implementer may choose
   others as long as they stay neutral. Existing *on-disk* names (`~/.omo`, `omo.jsonc`,
   `OMO_PROFILE`) stay because users' files depend on them.
5. **Tests are new.** Write tests from the given/when/then cases in the specs. Do not reuse OmO
   golden fixtures, parity vectors or test files.
6. **Messages are new.** Where a spec says "a notice that says X", write your own sentence that
   carries the required facts. Only the *facts* (paths, keys, counts) are mandated.

## Spec review checklist (leak check)

For each spec file the reviewer confirms, and records the result in the PR:

- [ ] no fenced code block is Rust, TypeScript or JS from the SUL tree (only user JSONC
      examples and shell commands are allowed);
- [ ] no sentence is a translation or paraphrase of a SUL comment, doc comment or prompt;
- [ ] no list or table mirrors a SUL constant table row-for-row except where the spec marks it
      as an interface fact (keys, enum values, env names, marker ids, file names);
- [ ] no proposed identifier is an OmO-original name;
- [ ] OmO-original features are absent.

The spec writer's self-review result for this set is at the end of each file ("Leak self-check").

## Provenance

Keep, next to the implementing PR: these specs (git history is the record), the implementer's
session prompt(s) and logs, the environment manifest, and the reviewer's checklist. Add a
`PROVENANCE.md` to each new crate naming method **C** (clean room) and the spec commit it was
built from.
