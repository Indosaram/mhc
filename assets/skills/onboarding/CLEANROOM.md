# Clean-room record: onboarding

- **Implementer inputs:** `/workspace/work/cleanroom/onboarding/spec.md` only. I also read some mhc Rust code at commit `13699780` read-only, just to confirm the interface facts the spec names:
  - `crates/omo/components/maho-omo-onboarding/src/component.rs`: the `omo-onboarding:bootstrap`, `omo-onboarding:context` and `omo-onboarding:history` custom types, the `Follow {skills_root}/onboarding/SKILL.md…` openers, and the ` Migration inventory (from the same scan omo setup runs): ` sentence. I found these with grep only.
  - `…/maho-omo-onboarding/src/session_history/tool.rs`: the `onboarding_session_history` schema (`workspace_root` minLength 1, `recent_days` 1..365) and `search_group = "onboarding"`.
  - `…/maho-omo-onboarding/src/tui_memory_tools.rs` and `tui_memory_runtime.rs` (via grep): the `onboarding_understand` schema (`consent`, `model` minLength 3), the `onboarding_memory_apply` schema (`understand_id`, `selected` required), the `understanding <id>` / `candidate … (nothing is saved yet):` output, `fallback: "scan_only"`, and `Not written (...)`.
  - `…/session_history/digest_store.rs` (grep): the `onboarding/scans/<scanId>/digests.jsonl` path.
  - `crates/omo/components/maho-omo-memory/src/tool_metadata.rs`: the `memory` tool commands (`create`, `str_replace`, `insert`, …) and the `reason`/`file_path` arguments.
  - `crates/omo/components/maho-omo-memory/src/commands/register.rs` (grep): the `/memory` and `/remember` commands exist.
  - `crates/omo/components/maho-omo-init-deep-advisor/src/constants.rs` and `coverage.rs`: the gate constants, the walk rules (the root itself is never a candidate, depth 1..=3, line count = pieces after splitting on `\n`, symlinks not followed), ancestor coverage, and `should_propose_init` (≥ 0.5).
  - `crates/omo/components/maho-omo-telemetry/src/product_identity.rs` `BUILTIN_SKILL_NAMES`, and only the `.selection` key of `assets/omo-latest/manifest.json` (`jq '.selection'`), to see which skills ship.
  - `…/maho-omo-telemetry/src/omo_native_notice.rs` (grep): the telemetry opt-out (`telemetry.enabled=false`, `DO_NOT_TRACK=1`).
  - `crates/maho-cli/src/cli/args.rs` (grep): `--list-tips` exists.
  - `crates/maho-core/src/skills.rs` (grep): `MAX_DESCRIPTION_LENGTH = 1024` and the `disable-model-invocation` key.
  - Directory listings only (no file contents): `assets/omo-latest/skills/` and the `scripts/` folders of `coding-agent-sessions` (the `find-agent-sessions.py` file exists), `init-deep` and `give-me-tips`. I grepped the `coding-agent-sessions/scripts/` tree for the literal `senpi` (it appears in `agent_sessions/scanners.py` and `pi_family.py`) and checked the finder's `--help` usage line (`find-agent-sessions.py list|find|… [--platform NAME …]`). I used only the subcommand and flag names.
  - The style reference `assets/omo-latest/skills/adversarial-plan/CLEANROOM.md`, as the brief allowed.
- **Not read:** anything under `/workspace/work/mhcg/assets/omo-latest/skills/onboarding/`, `/workspace/work/mhc/assets/`, any other copy of an `onboarding` skill (`/workspace/work/lc`, oh-my-openagent checkouts), git history or blobs of those paths, the onboarding row of `assets/omo-latest/skills/AGENTS.md`, the bodies of any other SUL skill files, and the upstream `qa-savings-fixture.sh` / `qa-validator.sh`.
- **Authorship:** every sentence, heading, table, the step order, the hypothesis-line format and the fixture data (vector B) were written fresh. Vector A is the spec's own independent vector (§6.1). No upstream persona or product names appear.
- **License:** MIT, (c) 2026 Indo Yoon (`LICENSE`). The scripts also carry SPDX headers.
- **Nothing in `/workspace/work/mhcg` was modified.**

## Files
| File | Purpose |
|---|---|
| `SKILL.md` | The skill. Frontmatter `name: onboarding` with a fresh description (559 chars). The body covers R1–R31. |
| `LICENSE` | MIT. |
| `CLEANROOM.md` | This record. |
| `scripts/savings-estimate.mjs` | Caching-value calculator (R22). Node ≥ 18, no dependencies. |
| `scripts/init-deep-gate.mjs` | AGENTS.md eligibility gate (R28/R31). Node, no dependencies. Uses the advisor's constants. |
| `tests/savings-fixture.sh` | Replaces the upstream `qa-savings-fixture.sh`. Runs spec vector A online and offline, own vector B, and the CLI exit-code contract. |
| `tests/static-check.sh` | Replaces the upstream `qa-validator.sh`. Uses paths relative to the skill directory. Checks frontmatter, cost-warning phrases, banned names, stray `senpi`/`omo`, required identifiers, the LICENSE, and the required files. |
| `tests/init-deep-gate.sh` | An extra test (not required by the spec): the four reasons, boundaries (8 files, 500 lines, ratio 0.5), exclusions, depth, ancestor coverage, usage errors, and constants checked against the spec. With `ADVISOR_CONSTANTS=<constants.rs>` it also cross-checks against the Rust source. |

Run all tests from the skill directory: `bash tests/static-check.sh && bash tests/savings-fixture.sh && bash tests/init-deep-gate.sh`.

## Decisions / spec ambiguities resolved
1. **Directory expansion is recursive.** mhc stores sessions in per-cwd subdirectories under `<agentDir>/sessions/`, so a non-recursive expansion would find nothing when given the sessions root.
2. **"Ascending filename order"** means sort by basename, with the full path as tiebreak, which makes the order total. Sessions are independent, so file order only affects float summation and never the rounded outputs.
3. **Malformed timestamp.** An entry *with* a `timestamp` key that is not a finite number and not a string `Date.parse` accepts is skipped and counted in `malformedSkipped`. An entry *without* a `timestamp` key is kept. Lines that are not valid JSON objects are counted separately in `unparseableLines` and never in `malformedSkipped`, which keeps the spec counter exact.
4. **Identity** needs both `message.provider` and `message.model` to be non-empty strings. A `model_change` needs both `provider` and `modelId`, otherwise it is ignored. A per-message identity does not change the "most recent model_change".
5. **Usage fields** that are missing or non-finite count as 0. The role is not filtered, as the spec says ("usage comes only from `message.usage`").
6. **Usage that is never attributed** (no `model_change` later in the file) is dropped and reported in `unattributedDropped`. A session whose attributed input is 0 counts as zero-input even if it had unattributed usage, as in vector A's file c.
7. **Price lookup.** "Miss" means no entry for that model id under the provider. If the exact entry (or the first alphabetical fallback match) exists but lacks a finite cost field, the session is insufficient: the search does not continue to the next provider. That is how vector A's `p-two/m-nocw` behaves.
8. **Unreachable catalog.** A missing file, HTTP error or bad JSON is treated like `--offline`. The output still exits 0, every non-empty session is insufficient, and `catalog.error` says why.
9. **Rounding** is `Math.round(v*100)/100` for v ≥ 0, mirrored for negatives (true half-away-from-zero). Only final aggregates are rounded: totals, plus extra per-model `actualCost`/`savings`/`baselineCost`/`cacheRatePct` added to the `byModel` rows. `cacheRatePct` is `null` when there is no priced input.
10. **Extra output fields** beyond the spec minimum: `sessions`, `pricedSessions`, `unparseableLines`, `unattributedDropped`, `catalog{source,available,error?}`, and per-row rounded costs. The spec says "at least", so these are allowed.
11. **Gate.** It mirrors `coverage.rs` exactly. The repo root is never a candidate (depth ≥ 1 only). Depth runs up to 3. Excluded names are checked only when descending, never against the root's own name. Symlinked files and directories are not followed (as with Rust `DirEntry::file_type`). A file with no extension, or a dotfile like `.ts`, is not source. An unreadable file counts as a file with 0 lines; Rust would abort with an I/O error, and the gate prefers to keep evaluating. Reason `covered` means candidates exist and the missing ratio is < 0.5.
12. **Hypothesis format (R27).** I chose a dedicated block `system/human/working-style.md`. Each entry is a single line: `- [low confidence] <tentative observation> (basis: <harness>; models <ids>; sessions <ids>; noted <date>)`. These lines hold no content, code, paths, repo names or secrets. When the user confirms an entry, it is replaced with their words without the tag.
13. **Migration apply path (§3.4, flagged for the integrator).** The skill runs `omo setup --dry-run` and then `omo setup --yes` when `omo` is on PATH. Otherwise it edits files by hand or uses the `omo.onboarding.apply` RPC if the host offers it. Every `omo` token is marked as a runtime identifier in the text.
14. **Tour (R8).** The skill tells the agent to name a bundled skill only if it appears in the installed skills list it sees at run time. It does not rely on a frozen catalog. The six named examples all appear in `.selection` and `BUILTIN_SKILL_NAMES`, and none is on the §8 drop list.
15. **Telemetry wording** follows the mhc notice: no prompts, paths or location; opt out with `telemetry.enabled=false` in `~/.omo/omo.jsonc` or `DO_NOT_TRACK=1`.
16. **R5 wording.** The skill words its rule as "don't add caveats about what this conversation costs", so the text itself never trips the validator regex `warning.*token`.
17. **Finder invocation.** The finder needs a subcommand, so the skill calls `scripts/find-agent-sessions.py list --platform senpi`, based on the usage line.
18. **Order of the flow.** Greet → machine summary → tour → migration → history → estimate → AGENTS.md gate → close. Memory consent applies throughout (§8 of the skill). The spec fixes only "E after D" and the gate at the close.

## Drops (spec §8)
I left out all of these: the keyword-driven workflow tour items, DAG mass-orchestration, plan/loop/research/execute workflow skills, hyperplanning, goal-state marketing, the named persona roster, the vendor-model fallback pitch, the upstream component list, the post-init-deep orchestration message, dual product command naming, the "native" product wording and the upstream seed-comment layout. User-facing text says "mhc" throughout.

## Acceptance checklist (spec §9)
| # | Check | Result |
|---|---|---|
| 1 | Frontmatter `name: onboarding`, fresh description ≤ 1024 chars, no `disable-model-invocation` | PASS (static check; description is 559 chars). I did not run the mhc loader: no built `mhc` binary was available, and I was not allowed to build in `mhcg`. |
| 2 | `tests/static-check.sh` passes | PASS. Mutation-tested: appending "This may cost you.", "ulw" or "the senpi product" each makes it fail. |
| 3 | `tests/savings-fixture.sh` passes, §6.1 online and offline | PASS: input 30000, cacheRead 60000, cacheWrite 10000, output 3500, cacheRatePct 60, actualCost 0.15, baselineCost 0.35, savings 0.2, insufficient 1, zero-input 1, malformed 1, `m-shared` pricedAs `a-prov`. Offline: insufficient 2, zero-input 1. Vector B and the CLI exits also pass. Mutation-tested: reversing the fallback sort makes it fail (actualCost 0.19). |
| 4 | Gate returns not-git / no-candidates / covered / eligible | PASS (`tests/init-deep-gate.sh`). Constants match `constants.rs` (cross-checked with `ADVISOR_CONSTANTS`). Read-only sanity run on the mhcg repo: `covered`, 28/28. |
| 5 | Declines produce no re-ask; history "no" skips the estimate | Encoded (skill §2 rule 4, §5, §6 "skip §6 and §7 entirely", §9). Not tested in a live run. |
| 6 | `onboarding_understand` only with `consent: true` after an explicit yes; apply gets exactly the picked IDs | Encoded (§6, §6.1, §10). Not tested in a live run. |
| 7 | No memory write without a yes; no direct edits of backing files | Encoded (§2 rule 7, §3 table, §8). Not tested in a live run. |
| 8 | Gate failure → no mention of AGENTS.md | Encoded (§9 "Not eligible"). Not tested in a live run. |
| 9 | Tour mentions only shipped, non-dropped features | PASS (static check of dropped names, plus the six named skills checked against the shipped list). The run-time cross-check is encoded. |
| 10 | Package staging, `argument_hints`, `BUILTIN_SKILL_NAMES` | Integrator. The file set changed (`tests/static-check.sh`, `tests/savings-fixture.sh` and `tests/init-deep-gate.sh` replace the upstream `qa-*.sh`, and `scripts/init-deep-gate.mjs` is new), so the manifest entries `skills.onboarding` and `targets["mc:H3"]`/`["mc:H13"]` must be regenerated. The skill name and directory are unchanged. |

## Flagged for the integrator
- Regenerate the `assets/omo-latest/manifest.json` onboarding file list and sha256 values for the new file set.
- `component.rs::show_welcome` still says "Use ulw for full workflows."
- Confirm whether `omo setup` stays the importer, or whether migration should go only through `omo.onboarding.apply` or manual edits.
- Update the onboarding row in `skills/AGENTS.md` and the "still SUL" list in `MIT-SKILLS.md`.
