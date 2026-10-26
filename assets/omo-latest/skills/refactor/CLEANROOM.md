# Clean-room record: refactor

## Inputs used
- Delta spec: `/workspace/work/cleanroom/refactor/spec.md`.
- MIT base: `/workspace/work/cleanroom/refactor/base/SKILL.md`, from lazycodex commit `75b017473c71d669dd9aed0a10e950e70724b283` (MIT).

## Not read
- No SUL-1.0 source was opened, read, grepped, or diffed (nothing under `/workspace/work/mhcg/assets/omo-latest/` or `/workspace/work/mhc/assets/`).

## Changes vs base
1. Removed the whole "Codex Harness Tool Compatibility" section (translation table, `multi_agent_v1`/`v2`, `fork_context`/`fork_turns`, `~/.codex/agents/`, `lazycodex-*` roles, empty-key rule, mailbox backoff, Codex self-review default). (spec 1.1)
2. Team-mode MUST NOT rule rewritten in my own words: read-only curated agents (consultant, documentation researcher) are never team members; no `deep` category lane because in-team routing downcasts it; use them via a normal `task()` outside the team. (spec 2.2)
3. Verifier category kept `deep-high` everywhere (dispatcher decision, spec 2.1 recommendation). One base line said "If `deep` is unavailable, fall back to `unspecified-high`" right after `category="deep-high"`; changed to "If `deep-high` is unavailable" so the category is consistent.
4. No other hunks edited (spec 2.3).

## Requirement checklist
| Req | Status |
|---|---|
| 1.1 Codex section removed | done |
| 1.2 `grep -nE 'lazycodex|multi_agent|fork_context|fork_turns|\.codex' SKILL.md` empty | PASS |
| 2.1 one verifier category (`deep-high`, fallback `unspecified-high`) in team-path summary, design notes, Lead dispatch | PASS |
| 2.2 generic read-only-agent rule, `deep` listed by id | PASS |
| 2.3 no wording-only edits | done |

## Note
- Base text outside the edited lines still names `oracle`, `librarian`, `call_omo_agent` and `~/.omo/teams/refactor-squad/config.json`. The spec's renames apply only to text the implementer writes and 2.3 forbids other edits, so they were left as is.
