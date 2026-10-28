---
name: adversarial-plan
description: High-rigor planning for risky or architecture-heavy changes. Five hostile critic agents argue over the request for three rounds (findings, cross-attack, defend/concede); only points that survive are passed to a dedicated planner, which writes the actual plan. Use when the user asks for an adversarial, red-team, or cross-critiqued plan, or for maximum-rigor planning. Not for small, well-understood tasks.
argument-hint: "<what you want planned>"
---

# adversarial-plan

**Why this exists:** ideas that cannot stand up to a hostile reviewer get thrown out *before* anyone plans around them, and the plan itself is written by a planner that had no stake in the argument. You (the session running this skill) are the **lead and broker**: you run the rounds, forward messages, and filter. You never write the plan.

**Cost:** about five parallel agents × three rounds, plus one planner run. Do not use it for a small or obvious change — suggest the plain plan skill instead.

Throughout, "the plan skill" means the harness's planning skill (in mhc today: `ulw-plan`; substitute whatever planning skill your build ships).

## 0. Preconditions — check before anything else

1. **Tools present.** `team_create`, `task_send` and `team_delete` must all be callable in this session. If any is missing, stop and tell the user: *"Adversarial planning needs the team and task tools, which are turned off in this session. Enable the team/task component and run this again."* Call nothing else.
2. **You are the top-level session.** Team tools only work for a lead; a spawned child cannot create a team. If you are running as a child/subagent, stop and tell the user: *"This has to be started from the main session, not from inside a delegated task."* Do not call `team_create`.

## 1. Start

- Print one short line, once: `Adversarial planning started.`
- Restate the request in one sentence. This sentence is the shared scope every critic receives.
- Create a todo list with these items: create team · round 1 findings · round 2 cross-attack · round 3 defend/revise/concede · distill survivors · hand off to planner · show plan · tear down team.

## 2. The critics

Five members, one line of attack each. Category names are mhc config keys; a project may remap them.

| id | Role | Goes after | category |
|---|---|---|---|
| `minimalist` | simplicity critic | scope that is not needed, abstractions built for imagined futures, polish nobody asked for; wants things cut or shrunk | `unspecified-low` |
| `breakage` | failure-mode critic | edge cases, effects across modules, state changes, error paths, tests that would break, how far a mistake spreads | `unspecified-high` |
| `evidence` | evidence auditor | any claim without proof: wants `file:line` for code, a doc URL for libraries, otherwise writes "no evidence found" | `deep-low` |
| `structure` | architecture critic | coupling, leaky interfaces, mixed responsibilities, debt — and equally, needless patterns; wants the simplest design that meets real requirements | `ultrabrain` |
| `alternatives` | reframing critic | anchoring on the first idea; must put forward concrete other approaches or reframe toward what the user actually needs; the obvious answer only stands if it beats them | `artistry` |

`minimalist`, `breakage`, `structure`, `alternatives` are required. `evidence` is dropped only under the retry rule in §3.

### Member prompts

Write each member's `prompt` in English from this template. **Do not soften it** — no "be polite", "be constructive", "be collegial". The hostility is what makes the filter work.

```
You are the <ROLE> on an adversarial review panel. Your only lens: <ATTACK FOCUS from the table>.

Stance:
- Treat every point from the other panelists as wrong until proven otherwise. No partial credit, no "good point, but".
- Change your position only when concrete evidence leaves you no choice. Say so plainly when it happens.
- Stay inside your lens. Do not drift into other panelists' angles.

Output:
- Numbered items only. Each item at most three sentences.
- No preamble, no hedging words, no summary paragraph.
- Address every reply to the lead with task_send (to: "lead").

You will receive tasks from the lead in rounds. Do only what the current round asks.
```

## 3. Create the team

Exactly one call:

```json
team_create({
  "inline_spec": {
    "name": "adversarial-plan",
    "description": "Hostile multi-angle review feeding a separate planner",
    "members": [
      { "name": "minimalist",   "category": "unspecified-low",  "prompt": "<from template>", "summary": "simplicity critic" },
      { "name": "breakage",     "category": "unspecified-high", "prompt": "<from template>", "summary": "failure-mode critic" },
      { "name": "evidence",     "category": "deep-low",         "prompt": "<from template>", "summary": "evidence auditor" },
      { "name": "structure",    "category": "ultrabrain",       "prompt": "<from template>", "summary": "architecture critic" },
      { "name": "alternatives", "category": "artistry",         "prompt": "<from template>", "summary": "reframing critic" }
    ]
  }
})
```

- Do not add a lead member; you are the lead automatically.
- Save the returned `team_run_id`. Every later `task_send` and `team_delete` needs it.
- **Retry rule:** if the call fails because `deep-low` does not resolve, retry **once** with the same spec minus `evidence`, and tell the user the panel is down to four. Any other failure: report it and stop.

## 4. How waiting works

- `task_send` is fire-and-forget. Members answer with `task_send` to `"lead"`; their replies show up as notifications at your next tool boundary or when you go idle. You may end your turn while waiting.
- Members never see each other's messages. They only know what you forward.
- **Gate:** a round starts only when every expected reply from the previous round is in. If one member is clearly lagging, send it one nudge (`task_send` to that member; `team_status` with the `team_run_id` can show whether it is still working). If it still does not answer, declare it stalled, tell the user, and continue without it. Never drop a lane silently.

## 5. Round 1 — independent findings

Send to all members (`task_send` with `to: "*"`, or one per member):

```
ROUND 1. Request (verbatim): <user request>
Shared scope: <your one-sentence restatement>

From your lens only, give 3 to 7 numbered, specific findings about what this work must get right or avoid.
Do not critique anyone else. Do not propose a plan or steps. Reply to lead.
```

Wait for all replies (gate).

## 6. Round 2 — cross-attack

Build one digest of every Round 1 finding, labelled `<member>#<n>` (e.g. `breakage#3`). Send the full digest to every member:

```
ROUND 2. Below are all Round 1 findings, labelled member#number.
Attack the findings of the OTHER panelists from your lens; skip your own.
For each finding you address, give either:
  ATTACK <label>: <concrete objection, max 3 sentences>
  STANDS <label>: <why it survives your lens, one sentence>
Reply to lead.

<digest>
```

Wait for all replies (gate). If the digest is very large you may condense wording, but every finding and its substance must remain.

## 7. Round 3 — defend, revise, or concede

Group all attacks by the finding they target. Send each member **only the attacks aimed at its own findings** — nothing else:

```
ROUND 3. These are the attacks on your findings.
For each attacked finding answer with exactly one verdict:
  DEFEND <label>: rebuttal backed by evidence
  REVISE <label>: a stronger restatement that absorbs the attack
  CONCEDE <label>: you drop it; state what, if anything, still holds
Conceding a point that lost is the right call, not a failure. Reply to lead.

<attacks grouped by your finding label>
```

A member whose findings drew no attacks gets a short note saying none of its findings were attacked and no reply is needed (or skip it and count it as complete).

Wait for all replies (gate).

## 8. Distill — filter only

Keep a finding if it was **never attacked**, **defended with evidence**, or **revised** (keep the revised wording). Drop every **conceded** finding; if the concession named a remainder, that remainder may survive as a revised point.

Write the **insight bundle**:

```
## Survivors of adversarial review

### Constraints (must hold)
- <finding> — [origin member#n]

### Agreed decisions
- <decision> — raised by <member>; attacked by <members or "none">; held because <defended/revised/unattacked + one-line reason>

### Risks and mitigations
- Risk: <...> → Mitigation: <...> — [origin]

### Open questions for the user
- <question where the panel did not converge, with the competing positions in one line each>

### Provenance
- Survivors per member: minimalist N, breakage N, evidence N, structure N, alternatives N
- Dropped (conceded): N  · Stalled members: <none | list>
```

Tell the user in one line that filtering is done and the planner is being engaged.

The bundle is **not** the deliverable. Do not show it as the plan, and do not add tasks, steps or ordering to it — that is the planner's job.

## 9. Planner handoff — mandatory

One foreground `task` call:

```json
task({
  "category": "ultrabrain",
  "load_skills": ["<the plan skill, e.g. ulw-plan>"],
  "run_in_background": false,
  "description": "Plan from adversarially reviewed insights",
  "prompt": "<handoff prompt below>"
})
```

Use `subagent_type` instead of `category` if a planner agent is configured (never both).

Handoff prompt (must stand on its own — the planner has seen none of the team's messages):

```
Write an executable implementation plan for the request below.

The material in INSIGHTS has already survived a three-round adversarial review by five hostile critics. Treat it as established input; do not re-derive the facts.

REQUEST (verbatim):
<user request>

INSIGHTS:
<full insight bundle>

Your obligations:
1. Every constraint holds in the plan.
2. Each risk's mitigation is attached to the task or tasks it affects.
3. Each open question becomes an explicit user decision gate, placed before the tasks that depend on its answer.
4. Every task has explicit success criteria.
5. Spend your effort on structure, ordering, dependencies, parallelism and verification.
6. Return the finished plan as your final answer (or, if you cannot plan without more input, return your questions).
```

- If the planner needs more context, gather it yourself and dispatch `task` again. Planner traffic never goes through `task_send`, and the bundle is never sent to a team member.
- Show the planner's output **unchanged**, preceded by one line such as: *"This plan came out of a <N>-critic, three-round adversarial review and was written up by the plan skill."*
- If the planner returns questions instead of a plan, pass them to the user unchanged.

## 10. Teardown

Only after the planner has returned and its output has been shown:

```json
team_delete({ "team_run_id": "<id>", "force": true })
```

Confirm in one line that the panel was shut down. If it fails, show the error and give the user the exact call above to retry. Teardown is final; the team cannot be resumed.

## What the user sees

Start line → one-sentence scope → optional one-line note per round → "handing to the planner" note → provenance line + planner's plan verbatim (or its questions) → teardown confirmation. A reduced panel or any stalled member is always stated.

## Rules — no shortcuts

- Never skip a round, and never start one with replies missing.
- Nothing gets synthesized before Round 3 is complete.
- No conceded material in the bundle.
- You never write the plan, never sketch tasks before the handoff, and never skip the planner.
- The bundle goes to a `task` call, never to a team member.
- The team is always torn down, and never before the planner has returned.
