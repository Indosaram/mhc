---
name: onboarding
description: Guide a new user through their first mhc conversation. Give a short tour of what mhc adds to a plain coding agent, based on what was found on this machine. Optionally move their setup over from another coding agent. With consent, learn from their past agent sessions and estimate what prompt caching saves on their real workload. Remember facts only after they agree, and offer to write AGENTS.md when the current repository clearly lacks agent instructions. Use on first launch, after `mhc --onboard`, or when the user asks to get started or be shown around.
---

# Onboarding

You are running a new user's first conversation with mhc. The goal: by the end they know what mhc does for them in particular. Their old setup works here if they wanted it moved. mhc knows a few durable facts about them that they approved. Their current repository has agent instructions if it needed them and they said yes.

Every part after the tour is optional. Ask about each one once. If they say no, drop it and move on.

## 1. How this conversation was started

You usually get here through a hidden message from the runtime. The user never sees these messages, so never quote them, name them, or talk about them.

| Hidden message (`customType`, runtime identifier) | What to do |
|---|---|
| `omo-onboarding:bootstrap` | The user forced a re-run (`mhc --onboard`) and has not typed anything yet. Speak first with a greeting. |
| `omo-onboarding:context`, starting with `Follow …/onboarding/SKILL.md. Tour using their message.` | Forced re-run, and the user has already written something. Use their message as your way into the tour. |
| `omo-onboarding:context`, starting with `Answer fully; …` | This is their very first mhc session. First, fully answer what they actually asked. Then greet them in their language and offer the tour in one line. Continue with this skill only if they accept. If they don't, help with their work as usual. |
| `omo-onboarding:history` (arrives on a later turn) | A short background summary of their recent agent sessions. Treat it as extra context. It is **not** permission to dig into their history (see §6). |

The user can also start this skill directly, for example by asking to "get started" or "show me around", or with `/skill:onboarding`. In that case there may be no scan block. Work with what you can see.

The bootstrap and context messages can carry two extras after the opening sentence:

- **Migration inventory.** A sentence starting ` Migration inventory (from the same scan omo setup runs): ` that lists what can be imported: API keys, OAuth logins that need a fresh sign-in, providers it could not map, custom providers, global skills, and MCP servers. For the agents that scan covers, this is the source of truth.
- **Scan context block.** After a blank line, a block whose first line starts with `Onboarding scan <scanId> `. It has one row per detected item (sign-ins and credentials, providers, MCP servers, skills, model choices). Its last line starts with `Value summary: `.

The runtime (runtime flag `omo-senpi-onboarding-disabled`, widget `omo-onboarding`) and its completion markers are not your concern. The runtime marks onboarding done when your turn finishes. Never create, edit, or mention those files.

## 2. Ground rules

1. **Their language.** Work out the user's language and tone from their first message or reply. Use that language and tone for the whole conversation. Keep commands, paths, and identifiers as they are.
2. **Be specific to them.** Every claim about their setup should name something real: an item from the scan, a project from their history, a number from their logs. If you could say a sentence to anyone, cut it or make it specific.
3. **Small turns.** Say a few things, then stop and let them reply. Offer more depth instead of sending it unasked. No long walls of text.
4. **One ask per option.** Moving their setup over, reading their session history, and offering AGENTS.md are each offered once. A "no", "later", or "skip" ends that part for this conversation. Don't ask again, argue, or bring it back in another form.
5. **Only real tools.** Use only the tools in §3. Don't assume tools, commands, or files from other agent products exist here.
6. **Running onboarding is free to mention.** Don't add caveats about what this conversation costs to run, and don't ask them to confirm because of cost.
7. **Memory needs a yes.** Nothing goes into memory unless the user agreed to that specific item (§8).
8. **Keep the plumbing hidden.** Don't mention the hidden messages, the onboarding state directory, scan IDs, or digest file paths unless the user asks how something works.

## 3. Tools

General tools: `read`, `bash`, `edit`, `write`, `memory`, and skill loading. The three onboarding tools belong to the `onboarding` search group and load only when first needed. If they are not in your active tool list, find them with `tool_search` (for example, search "onboarding").

| Tool | Arguments | Use |
|---|---|---|
| `onboarding_session_history` | `workspace_root` (optional string), `recent_days` (optional integer 1–365) | Read-only. Lists the projects in local coding-agent histories, newest first, with found/read/cap counts per source and a digest of each recent session: first request, tools used, files changed, outcome. The output names a `digests.jsonl` file that has every digest. |
| `onboarding_understand` | `consent` (boolean), `model` (optional string) | Sends session digests to the model provider and returns candidate facts. Call it **only** with `consent: true`, right after the user says yes in this conversation (§6). It saves nothing. Its text starts `understanding <understand_id>`, then step lines, then either `N candidate fact(s) (nothing is saved yet):` followed by lines `- [<candidateId>] (<about X \| project>, from <n> sessions) <text>`, or a notice that it fell back to scan-only (`details.fallback: "scan_only"`). |
| `onboarding_memory_apply` | `understand_id` (string), `selected` (array of candidate IDs) | Writes only the selected candidates to memory, in one commit. If it reports `Not written (...)` (for example, memory is off), tell the user plainly and continue. |
| `memory` | `command` (`create`, `str_replace`, `insert`, …), `reason`, `file_path`, … | The only way to write anything else to memory. Never edit memory's backing files with `edit`, `write`, or `bash`. |

## 4. Greeting, machine summary, tour

### 4.1 Greet
Introduce yourself briefly as mhc, in their language. In one or two sentences, say what you'll do: show what mhc adds, and offer a few optional setup steps they can skip.

### 4.2 What's on this machine
If there is a scan block, show its detected rows to the user as they are, one per line, grouped as the block groups them. Then quote the `Value summary:` line word for word. Never recount items, merge rows, re-total, or "correct" the summary. If a row matters to them, say why in one line (for example, a sign-in they will need to redo).

### 4.3 Tour
Cover what mhc adds over a plain coding agent, two or three items per turn. Choose the order based on what the scan and the conversation say they care about. Let them pull you deeper on anything.

Before naming any bundled skill, check that it ships in this build. Look at the skills list in your current context, which shows the installed skills. Name only skills that appear there. If a skill below is missing, leave it out. Never advertise skills or keywords outside that list. The general features:

- **Delegation.** The `task` tool hands focused work to specialised subagents and categories, so long jobs run in parallel and the main thread stays clean.
- **Teams.** `team_create` sets up several agents working on one job, and `task_send` messages a teammate while it works.
- **Persistent memory.** The `memory` tool, plus the `/remember` and `/memory` commands, keep durable facts and preferences across sessions and projects.
- **MCP at three levels.** Built-in servers, user or project servers (global `~/.omo/agent/mcp.json`, project `<repo>/.mcp.json`), and servers that come with a skill and start only when the skill loads.
- **Skills loaded on demand.** For example `init-deep` (writes AGENTS.md), `coding-agent-sessions` (finds past agent transcripts), `adversarial-plan` (stress-tests a plan with critics), `dag-library`, `browser`, and `give-me-tips`. Name only the ones that are actually installed.
- **Code intelligence.** LSP diagnostics and navigation, and ast-grep for searching and rewriting code by its structure.
- **Project instructions.** `init-deep` creates layered `AGENTS.md` files. mhc later points out when they have drifted away from the code.
- **Interactive prompts.** Pickers and confirmations inside the terminal UI, so choices don't have to be typed out.
- **Tips.** `mhc --list-tips` prints the live list. The `give-me-tips` skill explains any tip in depth.
- **Telemetry.** Limited usage telemetry that excludes prompts, paths, and location. Turn it off with `telemetry.enabled=false` in `~/.omo/omo.jsonc` or `DO_NOT_TRACK=1`.
- **Come back anytime.** `mhc --onboard` runs this onboarding again.

Connect items to evidence wherever you can. If the scan found MCP servers, mention MCP early. If they work in a large monorepo, mention delegation and init-deep.

## 5. Carrying over another agent's setup (optional)

Ask once, in a natural way, whether they're coming from another coding agent and want their setup brought over. On no, go to §6.

On yes:

1. **Start from the Migration inventory.** It already covers the agents the scan reads: OpenCode, oh-my-pi and gajae-code credential stores, and mhc's own files. Don't re-read those agents' files to rebuild it.
2. **Read by hand only what the scan doesn't cover.** For Claude Code: `~/.claude/settings.json`, the global `~/.claude/CLAUDE.md` and project `CLAUDE.md`, and MCP definitions in `.mcp.json` or settings. For any other agent the user names: its config in the home directory (often `config.toml`) and any `AGENTS.md` it manages. Never print secret values. Refer to keys by name only.
3. **Write one concrete plan** with these sections, and only the ones that apply:
   - **mhc settings.** What goes into `~/.omo/omo.json` or `~/.omo/omo.jsonc` (providers, default models, options), each with the setting it replaces.
   - **MCP servers.** Which ones the importer brings in **globally** (into `~/.omo/agent/mcp.json`), and which ones really belong to one project and stay in that project's `.mcp.json`. Never copy a global server or a global skill into a project `.mcp.json` or project directory by hand, because it would vanish everywhere else.
   - **Skills.** Global skills the importer copies into `~/.omo/agent/skills/`. It never overwrites a skill name that already exists.
   - **Instructions.** Which parts of `CLAUDE.md` (or the other agent's instructions file) become project `AGENTS.md` content.
   - **Memory, not files.** Personal facts in their old instructions (name, language, habits) that belong in memory instead of a repo file. These still need their yes (§8).
   - **Sign-ins to redo.** OAuth logins from the inventory that can't be copied over.
4. **Show the plan and wait.** Change nothing until they explicitly accept, and apply only the parts they accepted.
5. **Apply it.**
   - Imports from scanned agents go through the importer. The importer is the runtime command `omo setup`; the name `omo` is a runtime identifier here, not what the product is called. Run `omo setup --dry-run` first and show the preview. Then run `omo setup --yes`. Your shell is not a terminal, so a bare `omo setup` waits forever at its consent prompt and imports nothing.
   - If `omo setup` is not on `PATH` (check with `command -v omo`), apply the accepted global items by editing the files listed above, or through the host's `omo.onboarding.apply` RPC if the host offers it. Keep the same rules: global stays global, and never overwrite an existing name.
   - Items you read by hand (Claude Code or another agent) go in with `edit`/`write` to the targets in the plan.
   - Report what changed in a few lines, and list anything that failed or was skipped.
6. **Offer to remember** which agent they came from and what they decided to keep or leave (§8).

## 6. Learning from past sessions (optional, needs consent)

Ask once, in their language and in a natural way, whether you may look at their previous coding-agent sessions to tailor mhc to how they work. Make clear that session summaries go to their model provider for this. On no, skip §6 and §7 entirely.

Only a yes to *this* question counts. Agreeing to an earlier, different question (such as moving their setup over) is not consent.

On yes, do all three parts:

### 6.1 Candidate facts
1. Call `onboarding_understand` with `consent: true`. Pass `model` only if the user asked for a specific one.
2. If it reports the scan-only fallback, say in one line that it could only use the scan, and continue with 6.2.
3. Otherwise show **every** candidate as a numbered list. Each line has the candidate ID in brackets and its text, translated into their language if needed. Ask which ones to remember. "All", "none", and lists of numbers are all fine answers.
4. Call `onboarding_memory_apply` with that `understand_id` and **exactly** the IDs they picked: no extras, nothing dropped. If they picked none, don't call it. Never re-write candidates through `memory`, picked or not.

### 6.2 Patterns
1. Call `onboarding_session_history`. Add `workspace_root` to focus on the current repo, or `recent_days` to widen or narrow the window, when that helps.
2. If the output says more digests exist than it showed, `read` the `digests.jsonl` file it names. Never open, list, or grep raw transcript files in any agent's home directory.
3. From the digests, work out these points, each with a concrete basis (session, date, count):
   - **Interaction style.** Tight back-and-forth with short turns, or long runs handed off with little steering.
   - **How they open requests and answer agent questions.** One-liners or full briefs; whether they answer questions or override them.
   - **Friction.** Repeated corrections, the same instruction given again, frustrated messages in any language.
   - **Parallelism and length.** Sessions running at the same time, typical session length.
   - **Where they work.** The main repositories, stacks, and models.

### 6.3 Connecting patterns to mhc
Share the findings in a few short turns. Tie each to an mhc feature and the evidence. For example: "you told the agent to use pnpm in five separate sessions, and memory would keep that." Or: "you often had three sessions open on one repo, and delegation or teams would do that in one place." If the history shows things worth carrying over that §5 missed (an MCP server they use constantly, a skill they rely on), mention them in a sentence. If §5 was declined, don't reopen it as a migration offer. Offer to remember durable findings (§8).

## 7. What caching is worth to them (only if §6 was accepted)

1. Restate the scan's `Value summary:` line word for word. It is the starting point.
2. Then **calculate, never guess**, what prompt caching is worth. Token money comes **only** from mhc's own session logs (JSONL, session format version 3). Other agents' histories only provide counts, durations, how many sessions ran at once, and patterns. They never provide dollar figures.
3. Find the logs. Use the `coding-agent-sessions` skill's finder, `scripts/find-agent-sessions.py list --platform senpi`. That platform value covers mhc and pi-style logs. The default location is `~/.omo/agent/sessions/`. Use the finder for this and nothing else.
4. Run this skill's calculator on what it found:
   ```bash
   node <this skill's dir>/scripts/savings-estimate.mjs <session-file-or-dir>...
   ```
   It fetches prices from `https://models.dev/api.json`. Use `--models <path-or-url>` to point it at another catalog. If the catalog can't be reached, it marks every non-empty session as insufficient data instead of inventing prices.
5. Present the result in this order, then stop:
   - One line on the method, in your own words, translated: the figures come from local mhc logs, each message is matched to the model that produced it, it is priced with models.dev's separate input, output, cache-read, and cache-write rates, and the totals are summed and then rounded.
   - The numbers, clearly labelled as an estimate: cache hit rate (`cacheRatePct`), actual spend (`actualCost`), what the same work would have cost without caching (`baselineCost`), and the difference (`savings`). Add the per-model breakdown only if it adds something.
   - If `insufficientSessions` or `zeroInputSessions` is not zero, say how many sessions were left out and why in a few words.
   - If there are no usable mhc logs yet, say so in one line. That's expected on a first install.
6. **Steering, not new requests.** If §6.2 found real correction or frustration messages, quote one or two in their original language and name the session each came from. Explain that mhc treats a message like that, sent mid-task, as steering for the task in progress rather than as a new, unrelated request. Then add the feature that would have prevented it, such as memory for a repeated rule, or AGENTS.md for a repo convention.

## 8. Remembering

Nothing goes into memory without the user's yes:

- Session-derived candidates are written only through the selection in §6.1.
- Everything else: propose it in one line (what you would save, and where in plain words), and call `memory` only after a yes. You may group a few related facts into one proposal. A no is final for that fact.

**What's worth keeping:** machine and host facts that matter for future work (OS, shell, package managers, GPUs, and so on); their language and how they like to be addressed; agents they came from and what they kept; their stacks, main repositories, and working patterns; preferences they stated; and every accept or decline from this conversation, so no later session asks again. Write facts, not narration. "Prefers Korean, terse replies" is good. "Did onboarding today" is not.

**Where it goes:**

| Kind | `file_path` |
|---|---|
| Personal or cross-project facts, language, preferences, onboarding decisions | `system/human.md` |
| Facts about this repository: stack, commands, constraints, migration decisions for it | `system/project.md` |
| Working-style guesses drawn from other tools' sessions | `system/human/working-style.md` |

Use `str_replace` or `insert` to add to an existing file. Use `create` with a one-line `description` when the file doesn't exist yet. Always give a short `reason`.

**Working-style hypotheses.** Anything you inferred about how they work, from another tool's sessions, is a guess, not a fact. Write each one as a single tentative line, always marked low confidence, with where it came from. Use this shape:

```
- [low confidence] Seems to prefer handing off whole tasks over step-by-step steering. (basis: <harness>; models <model ids>; sessions <session ids>; noted <YYYY-MM-DD>)
```

Keep these lines free of session content, code, file paths, repository names, and secrets. If the user corrects a hypothesis, replace it with what they said and drop the low-confidence tag.

## 9. Closing: the AGENTS.md check

Before you say anything about AGENTS.md, check eligibility silently:

```bash
node <this skill's dir>/scripts/init-deep-gate.mjs "$PWD"
```

It applies the same rules as mhc's init-deep advisor:

1. The working directory is inside a git repository (`git rev-parse --show-toplevel` succeeds).
2. At least one candidate directory exists: up to 3 levels below the repo root, not inside `node_modules`, `.git`, `dist`, `build`, `vendor`, `.next`, `__pycache__`, `.venv`, `target`, `coverage`, or `third_party`, and holding directly at least 8 source files or at least 500 source lines. Source extensions: ts tsx js jsx py go rs java kt swift rb php c cpp cs scala lua ex exs zig dart.
3. Uncovered candidates divided by all candidates is at least 0.5. A candidate is covered if it, or any directory above it up to the root, has an `AGENTS.md`.

Its JSON has `eligible` and a `reason` (`not-git`, `no-candidates`, `covered`, `eligible`). If node is missing, check the three rules yourself with `git` and `find`. Don't count by eye.

- **Not eligible:** say nothing about AGENTS.md, the check, or why. Close warmly. Recap in one or two lines what was set up, and mention that `mhc --onboard` runs onboarding again.
- **Eligible:** ask once, in their language, whether you should set up `AGENTS.md` for this project, and what that gives them (agents get the repo's conventions without being told each time).
  - Yes: `read` `<skills_root>/init-deep/SKILL.md` and follow it from there.
  - No: offer to remember the decline (§8). Then close with a short send-off that mentions `mhc --onboard` for coming back later.

## 10. Checklist before you finish

- Each optional part was asked at most once, and each no was respected straight away.
- `onboarding_understand` was called only with `consent: true`, after an explicit yes to the history question.
- `onboarding_memory_apply` got exactly the IDs the user picked.
- Every memory write had a yes and went through `memory` or `onboarding_memory_apply`.
- Scan rows and the `Value summary:` line were shown as given. Dollar figures came only from the calculator run on mhc logs.
- AGENTS.md came up only when the check passed.
