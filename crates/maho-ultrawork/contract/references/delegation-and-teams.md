# Delegation and Multi-Agent Team Orchestration

This reference defines rules for dispatching subagents, managing parallel worker lanes, coordinating team communication, and validating delegated findings.

---

## 1. Subagent Dispatch Sizing and Planning Roles

Subagent delegation introduces coordination overhead and must be justified by task structure:
- **Planning Delegates**: Spawn a dedicated planning subagent only when architecture boundaries, subsystem interfaces, or multi-step execution paths are genuinely uncertain. For direct procedural implementations, author the plan directly in the coordinator session without spawning planning delegates.
- **Exploration Delegates**: Dispatch parallel exploration subagents when conducting wide architectural surveys, dependency audits, or cross-repository searches that would otherwise overwhelm the coordinator's context window.
- **Dispatch Parameters**: Specify either a task category or a discrete subagent type for each child task, matching the delegate's specialized configuration and capabilities.

---

## 2. Disjoint File Scopes and Partitioning

Parallel execution requires strict workspace boundaries to prevent race conditions:
- **Disjoint Write Boundaries**: Assign completely disjoint, non-overlapping file scopes to concurrently executing worker subagents. Two active workers must never be given write permissions or modification assignments covering the same source files.
- **Read-Only Mode for Analysis**: Restrict research, exploration, audit, and advisory subagents to read-only operational modes. Subagents analyzing architecture or auditing code must not possess write access.

---

## 3. Team Coordination and Task Boards

When coordinating complex multi-agent efforts whose workflows depend on shared state:
- **Shared Task Board**: Interdependent agents synchronize their state through a single shared task board rather than maintaining independent divergent task queues.
- **Structured Message Exchange**: Agents communicate progress, state handoffs, and blockers through structured message delivery rather than ad-hoc polling loops.
- **Explicit Task Framing**: Initialize each subagent with an unambiguous assignment, defined file boundaries, concrete deliverable formats, and observable stop criteria.

---

## 4. Lifecycle Monitoring and Terminal Barriers

Parent coordinators oversee active children with bounded monitoring:
- **Progress Tracking**: Treat subagent heartbeat messages, milestone completions, and diagnostic events as active progress.
- **Silent Worker Handling**: If a subagent stops emitting progress events, initiate targeted diagnostic follow-up queries to determine worker health before issuing a termination or cancellation request.
- **Terminal Barrier Invariant**: All active child tasks and background subagents must reach confirmed terminal states (completed, failed, or cancelled) before the coordinator completes dependent milestones or concludes parent session execution. Orphaned background processes are strictly prohibited.

---

## 5. Compact Deliverables and Adversarial Falsification

To prevent context window bloat and eliminate fabricated claims:
- **Compact Claim Summaries**: Child tasks delivering findings, audit results, or implementation reports must return structured claim summaries of at most 3,500 characters. Summaries must cite concrete file paths, line references, and observed symptoms. Detailed run logs remain in local files.
- **Adversarial Falsification Pass**: Before adopting findings or candidates delivered by exploration workers, the coordinator coordinates an adversarial falsification check. A verification lane actively attempts to disprove reported claims, verify whether supposed unused symbols are called dynamically, or confirm that reported defects reproduce against current code. Only findings surviving falsification are incorporated into final deliverables.
