# Session provider — clean-room spec

Behaviour reference: `crates/omo/memory-core/src/search/senpi_session_provider.rs`. Implemented in
`crates/maho-memory/src/session_provider.rs`.

This is the IO half of transcript search. It turns a directory of senpi session JSONL files into
`TranscriptConversation`s, which `search_transcripts` then ranks. Keeping the split means ranking has no disk
dependency and the provider has no ranking rules.

## Layout

Two shapes are supported, both installed layouts rather than one legacy alias:

- `<sessions dir>/<project>/<id>.jsonl` — the normal nested form.
- `<sessions dir>/<id>.jsonl` — flat, also accepted.

Entries are sorted before reading so a run over the same directory produces the same conversation order. A
missing directory and an unreadable one both yield no conversations rather than an error: search is a read path,
and a cold start with no history is normal.

## Conversation identity

The id comes from the `type: "session"` header line. If a file has no readable header it still has messages worth
searching, so the file stem becomes the id — dropping the conversation would silently hide real history.

Documents are read with an **empty** `conversation_id` and the id is filled in once the file has been read,
because a header line may appear after message lines. Filling per line would mislabel every early message with an
empty id.

## What becomes a message

Only `type: "message"` entries with an id, a `message` object, and a `role`. Bodies flatten as:

| Body shape | Result |
|---|---|
| string | content |
| array | `text` parts join into content, `thinking` parts into reasoning, `toolCall` parts into tool calls |
| anything else | all three empty |

`toolCall` arguments are kept as text: a JSON string as-is, any other JSON serialized, `null` as none. A tool call
with no arguments is still recorded, because the tool *name* is what a query usually matches.

A `toolName` field on the message adds one more call with that name and no arguments.

## Tool results are not assistant content

A `toolResult` message puts its body in `tool_return`, never in `content`. Content is what the assistant said;
mixing output into it would make tool output read as the agent's own statement.

## Exclusion

Three custom message types are dropped: `omo-kibitzer:recall`, `omo-memorian:recall`, `omo-memory:notice`. These
are the memory system's own injections, and indexing them would let a recall match the recall that produced it.

A message id appearing twice in one file is kept once — a duplicated line must not become a duplicated hit.

## Hidden

A session is hidden when any of these holds:

1. A sibling file named `<session filename>.archived` exists. Archiving works by creating that file rather than
   renaming the JSONL, so this check replaces a suffix test.
2. Its directory name is in `excluded_dirs`.
3. A file named `hidden_marker_file` exists in the session's parent directory.
4. A caller-supplied `is_hidden` predicate returns true.

Hidden sessions are still read and returned with `hidden: Some(true)`; the ranking decides whether to include them
(`include_hidden`). The provider never makes that decision.

A malformed line is skipped, not fatal. One corrupt line in a long session must not cost the whole conversation.
