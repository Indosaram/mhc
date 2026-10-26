# Clean-room record: visual-qa

- Base: lazycodex `75b017473c71d669dd9aed0a10e950e70724b283`, `plugins/omo/skills/visual-qa` (MIT), copied from `cleanroom/visual-qa/base/`.
- Inputs used by the implementer: the MIT base, `cleanroom/visual-qa/spec.md`, and the live output of the bundled scripts. No SUL-1.0 text was opened, read, grepped or diffed (`mhcg/assets/omo-latest/`, `mhc/assets/` untouched).
- Coordinator decision (spec §3): keep base behaviour, no downgrade. Kept: bounded round budget (2/3) and NEEDS HUMAN REVIEW, `[defect]`/`[taste]` split and "Recorded taste findings", Step 6 before/after mode, A12 layout contract and scroll-owner probe, B7 brief check, 44 CSS px note.

## Changes

| Spec item | Done | What |
|---|---|---|
| §2.1/2.2 | yes | Removed the whole "Codex Harness Tool Compatibility" section, including the self-review-by-default rule. Independent reviewer stays mandatory. |
| §2 (inline) | yes | Removed the Codex `spawn_agent` dispatch sentence in Step 3, the "**Codex:**" line and "**OpenCode:**" label in Step 5, and the Codex model recommendation. |
| neutral rewrite | yes | New Step 3 dispatch paragraph written for mhc tools: reviewers via `task` (`run_in_background=true`, full prompt, no parent history), one-shot, not `team_*` members or long-lived `workflow` steps, re-review = new `task`. |
| rename | yes | "oracle" persona -> "reviewer" (title "Dual-Reviewer", "reviewer passes", "visual/code reviewer"); `subagent_type="oracle"` -> `"reviewer"` with a note to use the configured read-only review agent name. AGENTS.md "dual oracle" -> "dual reviewer". |
| §1.1 | yes | New "Script output contract" section: stable field names and own example values for `image-diff` and `tui-check`, with field tables. Old one-line "Key fields" notes point to it. |
| §1.2 | yes | New section "Script numbers are evidence, not the verdict" (own wording), replacing the inline paragraph. |
| scripts | unchanged | `scripts/*` and `references/browser-setup.md` byte-identical to base. |

## Acceptance

- A1: ran `node scripts/visual-qa.mjs image-diff` and `tui-check` on synthetic fixtures; documented field set == emitted field set for both commands, including `hotspots[]` items, `reference`/`actual`, `overflowLines[]` items. (`--help` is not a command; the CLI only accepts the two subcommands. No `*.test.ts` files ship in the base, so `bun test` has nothing to run.)
- A2: `grep -nE 'lazycodex|multi_agent|fork_context|fork_turns|\.codex' SKILL.md` -> empty; also no `Codex`/`OpenCode`.
- A3: no omo/oracle/ultrawork/ulw/Sisyphus/momus/metis in new text.
- A4: decision recorded above.

## Left as-is (flag)

- `.omo/evidence/<slug>/...` paths in the TUI step: base text, and mhc itself uses this path today. Rename if mhc changes its evidence directory.
- `omowright` (browser library, MIT) and `script/qa/web-terminal-visual-qa.mjs` references kept from base.
