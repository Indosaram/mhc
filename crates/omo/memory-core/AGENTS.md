# crates/omo/memory-core

## OVERVIEW
Persistent agent memory filesystem, Git-backed memory commit storage, and identity tracking.

## WHERE TO LOOK
| Task | Location | Notes |
|------|----------|-------|
| Git commit storage | `src/git/` | Auto-committing memory revisions and log history |
| Virtual memory filesystem | `src/memfs/` | Reading and writing markdown blocks with YAML frontmatter |
| Memory locks | `src/locks/` | Concurrency synchronization across parallel operations |
| Identity & human profiles | `src/identity/` | Persona projection and agent self-awareness |
| People records | `src/people/` | Person profiles and interaction records |
| Fact compilation & reflection | `src/compile/` | Memory synthesis and observation indexing |
| Journal & reminders | `src/journal/`, `src/reminders/` | Time-based event logs and delayed triggers |

## CONVENTIONS
- All mutations must commit atomic changes with meaningful commit reasons.
- YAML frontmatter schema must be strictly validated before disk writes.
- Projections into agent system prompts must honor bounded size limits.
- Memory paths must remain strictly inside the bound memory repository.

## ANTI-PATTERNS (THIS COMPONENT)
- Never write memory files outside the designated memory repository root.
- Do not bypass lock primitives during concurrent agent reads/writes.
- Never omit the git commit reason when invoking mutation operations.

## NOTES
- Scored 15 due to file operations, git repo management, and state integrity requirements.
- Memory tests execute in temporary isolated git repos.
