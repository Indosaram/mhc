# Leak-check record: workpool

Spec-writer self-check and independent reviewer record for docs/cleanroom/specs/workpool.md. Reviewer input only; not an implementer input.

## Leak self-check

- [x] no fenced code from the SUL tree; the only literal shapes are tool parameters and JSON
      payloads (interface facts);
- [x] no sentence translates a SUL comment, doc comment, tool description or worker prompt; the
      assignment instruction is described by its required facts only;
- [x] the error-code list is marked as an interface fact (machine-consumed values); no other SUL
      table is mirrored;
- [x] proposed identifiers are neutral (`work_pool`, `PoolRecord`, ...); no OmO persona names;
- [x] OmO-original features (maximum-orchestration mode, mass fan-out mode, persona prompts,
      plan-continuation state) are absent.

## Review (leak check)

Reviewer session: separate from the spec writer. Compared against the SUL workpool runtime,
its tool description and schema, the component wiring, and the error-code and worker-message
tables.

- [x] no fenced code block is Rust, TypeScript or JS from the SUL tree (the file has no fenced blocks);
- [x] no sentence is a translation or paraphrase of a SUL comment, doc comment or prompt (after the fixes below);
- [x] no list or table mirrors a SUL constant table except interface facts: the error-code list is a
      user-visible subset (tool error text prefix and item `error.code`), not the full SUL table; op
      names, statuses, modes, id prefixes and the `workpool-aggregate` message type are caller-visible;
- [x] no proposed identifier is an OmO-original name (`work_pool`, `PoolRecord`, `PoolItem`, `PoolWorker`);
- [x] OmO-original features are absent.

Fixes:
- §3 push: the sentence about returning before a worker slot is free paraphrased the SUL tool
  description. Rewrote it as a persistence/dispatch ordering fact, and removed a garbled duplicated
  fragment in the same bullet.
- §5 and §7: the wording on uncertain deliveries and failed aggregates followed the same tool
  description sentence. Restated both as dispatcher/flush-point behaviour.
