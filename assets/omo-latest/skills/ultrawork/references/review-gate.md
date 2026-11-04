# Independent Review Gate and Escalation

This reference defines the trigger conditions, execution protocol, remediation cycles, and human escalation procedures governing independent code review in ultrawork mode.

---

## 1. Dual-Trigger Activation Protocol

To conserve computational resources and eliminate unnecessary review latency, an independent third-party reviewer subagent is summoned in exactly two scenarios:
1. **Explicit User Demand**: The user explicitly requests a strict review, second opinion, or dedicated reviewer pass in their prompt.
2. **Heavyweight Work with Approved Plan**: The task is classified as heavyweight and is governed by an approved, persisted plan document in `.omo/plans/<slug>.md`.

In all other situations (including lightweight tasks, routine bug fixes, and unprompted implementations), formal reviewer invocation is bypassed in favor of a systematic self-review conducted by the primary agent.

---

## 2. Reviewer Subagent Configuration and Prompting

When the dual-trigger condition is met, the reviewer is dispatched as a read-only subagent:
- **Read-Only Mode**: The reviewer operates under strict read-only permissions and cannot modify workspace files or run destructive commands.
- **Criteria-Bound Evaluation**: The reviewer evaluates the change set strictly against the stated goal requirements, approved plan specifications, and test evidence. Speculative style opinions or personal preferences outside documented project standards are out of scope.
- **Structured Findings**: The reviewer returns a structured report categorizing findings into:
  - `BLOCKER`: Concrete architectural flaws, security risks, broken contract invariants, or missing test coverage that directly violates acceptance criteria.
  - `ADVISORY`: Non-blocking suggestions, stylistic notes, or future optimization ideas.

---

## 3. Delta Remediation and Re-Review Allowance

When the reviewer identifies blocking issues:
- **Focused Delta Fixes**: The primary agent implements minimal, targeted remediations addressing each specific blocker without introducing unrelated changes.
- **Delta Re-Review Allowance**: The agent submits the updated diff to the reviewer for re-evaluation. Up to two iterative re-review cycles are permitted to resolve identified blockers.
- **Evidence-Backed Verification**: Each re-review submission must include fresh execution receipts and test passes demonstrating that the identified blockers have been resolved.

---

## 4. Human Escalation Dialog

If blocking disagreements or unresolved technical obstacles persist after the two permitted re-review cycles:
- **Escalation Trigger**: The agent must not enter an unbounded review loop or attempt a third automated re-review pass.
- **Standard Tool Invocation**: Escalate the dispute directly to the human user using the appropriate registered user question tool (`ask_user_question` or `request_user_input`).
- **Objective Framing**: Present the user with a concise summary of the conflict, articulating the reviewer's specific concern, the executor's technical justification, and the concrete choices available to resolve the impasse.
- **User Precedence**: The human user's decision is authoritative and final.
