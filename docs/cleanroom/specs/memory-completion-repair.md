# Completion repair — clean-room spec

Behaviour reference: `crates/omo/memory-core/src/reflection/completion_repair.rs`. Implemented in
`crates/maho-memory/src/completion_repair.rs`.

The completion gate can refuse a memory entry a reflection run wrote and still accept the run. The run's branch
is then committed without that entry, and the refusal has to be readable afterwards — including by a second
validation of the same tip, which would otherwise see an already-repaired branch and report nothing.

## Recorded as a trailer, not prose

Each dropped entry is one trailer line on the commit body:

```
Omo-Completion-Dropped: {"path":"...","reason":"..."}
```

JSON, because a path is chosen by the agent and may contain a newline, ` | `, a `:`, or anything else a
separator-based format would split on. A prose format would read back as a different set of entries than it was
given.

## Redaction is part of the format, not the caller's job

`path` can be secret-shaped (an agent may write a note whose name contains what looks like a credential), so:

- `trailer_line` redacts both values before serializing. What lands in git history is already safe.
- `describe_dropped_entries` redacts again before building the sentence.

Redacting on write matters more than redacting on read: a value that reaches the commit body is in history
whether or not it is displayed.

`reason` never carries the matched secret value in the first place — only the class of failure.

## Reading is tolerant

`dropped_entries_in_body` skips:

- lines that do not start with the trailer prefix;
- a trailer whose value is not the JSON object this format writes.

A hand-edited or truncated body must not become a wrong report of what was dropped, and a prose line mentioning
the key must not be mistaken for a record. The result is "the entries I could read", never an error.

## What is not in this module

Reading the trailer off a real commit needs to run git (`rev-parse` to confirm the tip's parent is the recorded
base, then `log -1 --format=%B`), and squashing the branch needs `reset --soft` plus path-by-path restore. Both
are caller work: this module never spawns a process, which keeps the format testable without a repository.

A dropped path must be applied to git as a **literal** pathspec (`GIT_LITERAL_PATHSPECS=1`). A name such as
`k*.md` or `:!x.md` is a glob or magic pathspec otherwise, and dropping it would revert or delete other notes.
That env belongs to the caller that builds the git invocation.
