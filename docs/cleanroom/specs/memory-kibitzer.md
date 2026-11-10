# Kibitzer delivery — clean-room spec

Behaviour reference: `crates/omo/components/maho-omo-memory/src/kibitzer_delivery.rs`. Implemented
from scratch in `crates/maho-memory/src/kibitzer.rs`.

The kibitzer is a passive advisor: it holds nudges (short reminders pointing at stored memory) for
a session and hands them over at the moment the session can actually read them. It never
interrupts a busy turn.

## Holding nudges

`accept(session, nudges)` merges nudges into the session's held map, keyed by memory path, so a
later nudge for the same path replaces the earlier one rather than queueing twice. Accepting a
nudge records it as surfaced in the recall ledger. Accepting an empty batch is a no-op.

A nudge whose content restates something the person retracted is dropped before it is held: if
every nudge in a batch is dropped, nothing is held and nothing is surfaced.

## When nudges are handed over

There are three routes, tried in this order:

1. **steer** — a hidden steer message sent while the session is running. This is used at a tool
   result boundary when all of these hold: nudges are held, no steer is already in flight, the
   session has no pending messages, and the session is not idle.
2. **wake** — a new turn started for the session when it is idle and a nudge arrives.
3. **prompt** — nudges are appended to the next prompt block.

`drain_for_prompt(session)` takes every held nudge and clears the held map, so a nudge is never
delivered twice through two routes.

## Steer lifecycle

A steer sets the session's `steering` flag before the message is sent, so a second tool-result
boundary cannot send the same nudges again while the first is in flight. The flag clears when the
steer is marked delivered, and on shutdown. A steer that fails to send leaves the nudges held and
clears the flag so a later boundary can retry.

## Tool-result gate

A `tool_result` boundary delivers only when:

- at least one nudge is held,
- no steer is in flight,
- `has_pending_messages` is false,
- `is_idle` is false.

The last two together are the upstream gate: the session must be between turns but still running.

## Session state

Per-session state is independent: one session's held nudges, steering flag and in-flight tool
calls never affect another's. When a session shuts down, its held nudges and state are dropped, and
nothing is delivered after that.
