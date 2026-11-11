# Memory reminders — clean-room spec

Behaviour reference: `crates/omo/memory-core/src/reminders/reminders.rs`. Implemented in
`crates/maho-memory/src/reminders.rs`.

The memory store is a git repository. When it needs a commit, has a conflict, or failed to push, the agent gets
a nudge. The interesting part is not the text — it is **when it is allowed to appear**.

## One reminder per condition, not per turn

A store left dirty for twenty turns would print the same warning twenty times. By the third the reader has
learned to skip it, which is worse than not warning at all: the one time it matters it scrolls past.

So `RemindersState` holds the last emitted **condition key**:

| Condition | Key |
|---|---|
| uncommitted paths | `dirty` |
| conflict | `conflict:<merge\|rebase\|unmerged>` |
| failed push | `push_failed:<reason>` |

Consequences worth stating:

- Any number of uncommitted edits stays one condition.
- A rebase becoming a merge is a **new** condition — different state, different advice.
- A push failing for a new reason is a new condition.
- Returning to a clean store clears the key, so a condition that clears and returns is reported **again**.

## Priority

`Conflict` > `PushFailed` > `Dirty`.

A conflict is the state where further memory writes stack onto an unresolved merge, so it has to be named
first. Once a conflict is active the other two are consequences of it, and reporting all three would bury the
one that needs action.

## Text

Three fixed messages, each naming the concrete state:

- **Dirty**: the paths (first 20, then `...and N more`), plus an explicit instruction *not* to `git push` —
  the harness pushes clean committed memory after a turn, and a manual push from the agent races it.
- **Conflict**: the operation in progress and the conflicted files, and which operation must be completed
  (`rebase` for a rebase, `merge` otherwise).
- **Push failed**: the reason, and the number of local commits ahead when there are any.

## Scope

Pure. `RepoStatusSnapshot` is the boundary: git status is read elsewhere and handed in as a plain value, so the
dedupe and priority rules are testable without a repository.
