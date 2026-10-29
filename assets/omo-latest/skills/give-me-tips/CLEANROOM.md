# Clean-room record: give-me-tips

- **Implementer inputs:** `/workspace/work/cleanroom/give-me-tips/spec.md` only, plus read-only confirmation of the interface facts it names in the mhc tree (`/workspace/work/mhcg/crates`, not modified):
  - `maho-cli/src/cli/args.rs` (`--list-tips` flag), `maho-cli/src/main.rs` (pretty-printed JSON output), `maho-cli/src/cli/list_tips.rs` (`collect_tips`: `id`, `text`, optional `requiresCommand`; default keybindings joined with `/`)
  - `maho-interactive/src/tips/` directory listing and `catalog/` file names; `scheduler.rs` (`select_tip` gate order and unseen-first / oldest-shown rule); `startup_tip.rs` (`TIP_POINTER` exists, `Tip: <body>\n<pointer>` format, whitespace collapse); `working_tip.rs` (session-shown ids as the exclude set)
  - `maho-interactive/src/interactive_mode.rs` (`tips` default `true`, `quietStartup` default `false`, `tipsHistory` written on show)
  - `maho-core/src/config.rs` (`resolve_agent_dir`: env dir, else `~/.omo/agent` standalone, flat layout for brands), `maho-core/src/keybindings.rs` (`keybindings.json` in agent dir), `maho-core/src/skills.rs` (name ≤ 64, description ≤ 1024, warnings on bad frontmatter)
  - `maho-interactive/src/tips/catalog/model_tips.rs` — only to confirm the id `efforts-command` exists.
- **Not read:** anything under `/workspace/work/mhcg/assets/omo-latest/skills/give-me-tips/`, `/workspace/work/mhc/assets/`, any other copy of the skill (`/workspace/work/lc`, upstream repo), git history/blobs of those paths, or the `skills/AGENTS.md` row describing the skill. The only file read under `assets/omo-latest/` was `skills/adversarial-plan/CLEANROOM.md`, as a format template (permitted by the assignment).
- **Authorship:** every sentence, heading, table and example written fresh. No upstream persona or product names; runtime identifiers kept exact (`give-me-tips`, `--list-tips`, setting keys, crate paths, custom message types).
- **License:** MIT, (c) 2026 Indo Yoon (`LICENSE`).

## Decisions / drops (spec §7)
- Command: `mhc`, else the command the session was launched with (brand build). No upstream launcher names or env vars.
- Source pointers: mhc crate paths and `https://github.com/Indosaram/mhc` only; no upstream package paths.
- Both upstream deep walkthroughs dropped. The fallback-model notice and memory-recall notice are handled by one generic "notices that look like tips" procedure (search by text or custom type, explain from code). The three custom-type strings and `memory.recall.enabled` are quoted as runtime identifiers only; the memory switch is phrased conditionally ("if that is what the code reads") so the skill never asserts mechanics it has not verified.
- Tone: neutral, precise, interested in the engineering (R7); no persona.
- Removed workflow features from §7 are not named anywhere in the skill. Workflow/ethos/dag tips still in the catalog are not mentioned; the skill explains whatever `--list-tips` returns.
- `TIP_POINTER` text is not copied into the skill; SKILL.md only says the startup tip is followed by a pointer line.
- `metadata.short-description` omitted (optional, not read by mhc).

## Ambiguities resolved
- **Banned "OmO" vs. real identifiers.** A case-insensitive substring ban on `omo` would hit live paths (`~/.omo/agent/`, `crates/omo/components/`, `maho-omo-memory`) and custom types. The static check removes an explicit allow-list of exact identifiers, then matches banned names on word boundaries (case-insensitive). `senpi` is banned on its own too (it is the upstream launcher name, §7 bullet 1).
- **Gate wiring vs. spec.** While confirming `interactive_mode.rs` I saw that the startup-tip call path currently passes no `has_command` callback (comment: it stays `None` until the session exposes its command registry), so the `requiresCommand` gate may not be applied at startup in this build. The skill does not hard-code either behaviour: it describes the `select_tip` rule and tells the agent to check which gates the call sites actually wire up and report what the code does (R5). Flag for the integrator: R4/§3.4's statement that command-gated tips "never appear" holds only once that callback is wired.
- **Grouping in listing mode.** `--list-tips` carries no group field; the skill groups by id prefix or source file, or by gate.
- **Settings edits.** Allowed only on explicit request; the skill adds "change just that key and show the diff".

## Files
- `SKILL.md`, `LICENSE`, `CLEANROOM.md`, `tests/static-check.mjs` (`node tests/static-check.mjs`)
- Integrator: add `LICENSE`, `CLEANROOM.md`, `tests/static-check.mjs` to `manifest.json` (`skills["give-me-tips"]`, `targets["mc:H3"]`, `targets["mc:H13"]`) and regenerate sha256 before `tools/package-native.mjs` (spec §5).

## Acceptance checklist (spec §6)
Static check: 30/30 PASS (`node tests/static-check.mjs`, exit 0). Negative test: appending two banned names to a temp copy makes it fail on both. Frontmatter also parsed with a YAML parser (name `give-me-tips`, description 621 chars).
| # | Check | Result |
|---|---|---|
| 1 | frontmatter parses; exact name; fresh description ≤ 1024 | PASS (static + YAML parse); mhc loader not executed |
| 2 | `/efforts` tip → `--list-tips`, id `efforts-command`, read impl, cite paths, user's language | encoded (Steps 1–2, ground rules); id confirmed in catalog; live not run |
| 3 | `--list-tips` unavailable → catalog sources, says so | encoded ("If the flag does not work"); live not run |
| 4 | "which tips can I see?" → settings.json; `tips:false`, `quietStartup:true`, `tipsHistory` order, gate exclusions | encoded (Step 3 table + selection rule + report list); live not run |
| 5 | component notice → locate emitter by custom type/text, explain from code | encoded ("Notices that look like tips"); live not run |
| 6 | tip text vs. code disagree → state it, follow code | encoded (ground rules, Step 2) |
| 7 | no per-tip essays; no upstream names; banned grep empty | PASS (static check; manual review: no feature descriptions embedded) |
| 8 | `task34` `TIP_POINTER` golden tests unchanged | not affected (skill name unchanged; mhc tree untouched); tests not run here |
| 9 | package staging after manifest update | integrator step; not run (manifest not modified by this work) |
