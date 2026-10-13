# crates/omo/senpi-task

## OVERVIEW
Multi-agent workflow orchestration, DAG graph execution, and background task supervisor.

## WHERE TO LOOK
| Task | Location | Notes |
|------|----------|-------|
| Task supervisor & lifecycle | `src/lifecycle/` | Process spawning, monitoring, and cleanup |
| Task manager core | `src/manager/` | Multi-agent registry, state sync, and tracking |
| DAG planning & execution | `src/dag/` | Dependency resolution, node scheduling, wave execution |
| Category & worker dispatch | `src/category/` | Category routing, model constraints, and defaults |
| Agent runners | `src/agents/`, `src/runners/` | Specific runner implementations and execution engines |
| Task steering & messaging | `src/steering/` | Mid-run message injection and graceful cancellation |
| Execution state machine | `src/state/` | Status tracking, progress recording, completion hooks |

## CONVENTIONS
- Maintain strict parent-child task isolation and state boundaries.
- DAG dependency resolution must detect and reject circular references before run start.
- Preserve task output tails and status records in durable storage.
- Subagent runners must honor assigned category models and skills.

## ANTI-PATTERNS (THIS COMPONENT)
- Do not lose child process handles on abort or early termination.
- Avoid polling loops without exponential backoff or event-driven wakeups.
- Never cross-contaminate task working directories or unisolated checkouts.

## NOTES
- Scored 15 due to high complexity, DAG graph scheduling logic, and background process management.
- Task outputs persist in local storage for inspectability.
