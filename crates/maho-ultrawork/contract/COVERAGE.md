# Ultrawork Contract Coverage Checklist

This document maps every rule from `docs/cleanroom/specs/ultrawork-contract.md` with disposition `core` or `condensed-core` to the corresponding section heading in `crates/maho-ultrawork/contract/core.md`.

## Core and Condensed-Core Rule Mapping

| Rule ID | Disposition | Behavioral Summary | Heading in `core.md` |
| :--- | :--- | :--- | :--- |
| **R1** | condensed-core | Quiet, outcome-oriented posture reserving communication for handoffs | `## 1. Operational Posture and Verification Standard` |
| **R2** | core | Validate user-visible features through actual runtime surfaces | `## 1. Operational Posture and Verification Standard` |
| **R65** | core | Base task completion on empirical runtime behavior, not static inspection | `## 1. Operational Posture and Verification Standard` |
| **R66** | core | Remedy all regressions and broken tests within touched blast radius | `## 1. Operational Posture and Verification Standard` |
| **R67** | core | Prohibit silencing compiler errors, lint checks, or test assertions | `## 1. Operational Posture and Verification Standard` |
| **R68** | core | Require tangible execution evidence before closing tasks | `## 1. Operational Posture and Verification Standard` |
| **R3** | core | Classify work into lightweight (localized) or heavyweight (structural) | `## 2. Task Sizing and Ceremony Tiers` |
| **R4** | core | Default to lightweight, escalate monotonically, never downgrade | `## 2. Task Sizing and Ceremony Tiers` |
| **R5** | condensed-core | Control-plane coordination tasks sized as lightweight | `## 2. Task Sizing and Ceremony Tiers` |
| **R12** | core | Outer runner (loop or workflow) state supersedes contract setup | `## 3. Lifecycle Initialization and Outer Precedence` |
| **R13** | core | Allocate isolated per-task temporary scratch and private build caches | `## 3. Lifecycle Initialization and Outer Precedence` |
| **R14** | core | Deterministic cleanup of task-owned temporary dirs and caches | `## 3. Lifecycle Initialization and Outer Precedence` |
| **R15** | core | Survey skill catalog; load instructions dynamically on demand | `## 3. Lifecycle Initialization and Outer Precedence` |
| **R16** | core | Survey architecture, baseline state, and conventions before editing | `## 3. Lifecycle Initialization and Outer Precedence` |
| **R18** | core | Register concrete goal with unambiguous success and stop criteria | `## 3. Lifecycle Initialization and Outer Precedence` |
| **R19** | core | Goal blocked status reserved for true external obstacles, not async waits | `## 3. Lifecycle Initialization and Outer Precedence` |
| **R28** | core | Multi-step plans persisted under `.omo/plans/` and mirrored in checklist | `## 3. Lifecycle Initialization and Outer Precedence` |
| **R43** | core | Read and verify existing tests and behavior prior to editing source | `## 4. Active Execution and Verification Loop` |
| **R44** | core | Apply minimal targeted modifications without unrequested abstractions | `## 4. Active Execution and Verification Loop` |
| **R45** | core | Author regression tests only when needed; avoid pinning constants | `## 4. Active Execution and Verification Loop` |
| **R46** | core | Execute direct surface scenarios driving the changed interface | `## 4. Active Execution and Verification Loop` |
| **R47** | core | Terminate spawned test processes, listeners, containers, and drivers | `## 4. Active Execution and Verification Loop` |
| **R48** | core | Validate clean semantic language server diagnostics across touched files | `## 4. Active Execution and Verification Loop` |
| **R49** | core | Two-tiered verification: fast local checks per edit, full suite at merge | `## 4. Active Execution and Verification Loop` |
| **R29** | core | Synchronize task progression with checklist tool in real time | `## 5. State Tracking and Checklist Management` |
| **R30** | core | Register cleanup tasks immediately upon spawning temporary resources | `## 5. State Tracking and Checklist Management` |
| **R31** | condensed-core | Actionable checklist items with paths, rationale, and checks | `## 5. State Tracking and Checklist Management` |
| **R50** | core | Asynchronous waiting driven by pushed monitor events, never polling | `## 6. Asynchronous Waiting and Event Monitoring` |
| **R52** | condensed-core | Inspect partial background output only when decisions strictly require it | `## 6. Asynchronous Waiting and Event Monitoring` |
| **R36** | core | Run independent read operations concurrently in evaluation cells | `## 7. Code Evaluation and Runtime Execution` |
| **R37** | condensed-core | Leverage native runtime primitives rather than shell subprocesses | `## 7. Code Evaluation and Runtime Execution` |
| **R38** | core | Parallelize independent queries while sequencing dependent operations | `## 7. Code Evaluation and Runtime Execution` |
| **R39** | core | Execute mutating operations singly and inspect each outcome | `## 7. Code Evaluation and Runtime Execution` |
| **R32** | core | Semantic LSP navigation for definitions, references, and diagnostics | `## 8. Semantic Code Discovery and Inspection` |
| **R33** | core | Structural AST search and transformation for syntax inspection | `## 8. Semantic Code Discovery and Inspection` |
| **R34** | condensed-core | Fast textual content search, globbing, and version history | `## 8. Semantic Code Discovery and Inspection` |
| **R60** | core | Systematic self-review verifying diffs, diagnostics, and test evidence | `## 9. Interaction Protocol and Completion Discipline` |
| **R70** | condensed-core | Brief approach orientation summary following initial setup | `## 9. Interaction Protocol and Completion Discipline` |
| **R71** | core | Standardized milestone handoff summaries (`Ask`, `wanted`, `For you`, `Now`, `Next`) | `## 9. Interaction Protocol and Completion Discipline` |
| **R72** | core | Empirical completion reports with verified criteria and evidence paths | `## 9. Interaction Protocol and Completion Discipline` |
| **R73** | core | Immediate completion upon satisfaction of user request | `## 9. Interaction Protocol and Completion Discipline` |
| **R74** | core | Complete verification and confirmed teardown receipts before closing | `## 9. Interaction Protocol and Completion Discipline` |
| **R75** | core | Prohibit completion while background processes, ports, or scratch dirs live | `## 9. Interaction Protocol and Completion Discipline` |
| **R76** | core | Escalate to user via `ask_user_question` / `request_user_input` after 2 failures | `## 9. Interaction Protocol and Completion Discipline` |
| **R77** | core | Discontinue exploration when two search waves yield no novel insights | `## 9. Interaction Protocol and Completion Discipline` |

## Summary of Reference-Offloaded Rules

The following rules are offloaded to specialized on-demand reference modules:
- `references/qa-channels.md`: R6, R8, R9, R10, R11, R40, R51
- `references/delegation-and-teams.md`: R17, R35, R42, R53, R54, R55, R56, R57, R58
- `references/commits.md`: R64
- `references/review-gate.md`: R59, R61, R62
- `references/tui-visual-qa.md`: R7

## Summary of Deprecated/Deleted Mechanisms

The following legacy mechanisms are eliminated in the clean-room specification:
- R20, R21, R22, R23, R24, R25, R26, R27: Temporary notepad scratchpad in `/tmp` eliminated.
- R41: Non-existent Python kernel hopping eliminated.
- R63: Frequent atomic git commits eliminated (commits strictly user-authorized).
- R69: Hardcoded greeting banner line eliminated (intent gate preserved).
