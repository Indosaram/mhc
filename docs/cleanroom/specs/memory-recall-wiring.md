# Recall wiring — clean-room spec

Implemented from scratch in `crates/maho-memory/src/recall_wire.rs`.

The ledger ([memory-ledger](memory-ledger.md)) knows what a session was already shown; the selector
([memory-select](memory-select.md)) knows what is worth showing now. Neither is useful alone. This
module is the single place that joins them, and its contract is the whole behaviour of recall:

1. **Suppress only unchanged memories.** The ledger stores a content hash per path, so suppression is
   decided by `is_current(path, hash)`, not by path membership. A memory the user edited must come
   back: it carries information the session has not seen. Passing the ledger's raw path set to the
   selector is a bug, because it treats an edited memory as already delivered.

2. **Suppress before scoring.** Exclusion happens inside the selector, before any scoring, so a
   suppressed memory never consumes part of the `max_items` budget.

3. **Record exactly what was returned.** Only hits are marked surfaced, in the same call. Recording
   the candidates that were dropped by the budget would permanently hide memories the user never saw,
   and there is no way to recover from that: the ledger has no expiry.

The timestamp passed in is used for every hit in a pass, so the ledger's `at` values are comparable
within a pass.
