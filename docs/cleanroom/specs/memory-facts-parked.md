# Facts parked batches — clean-room spec

Behaviour reference: `crates/omo/memory-core/src/facts/parked.rs`. Implemented in
`crates/maho-memory/src/facts_parked.rs`.

A range that fails extraction repeatedly is **parked**: it leaves the queue so it stops being retried, but
it is never deleted. Parking is a move between two directories under the same queue root, so it is a rename
on one filesystem.

## Matching by name, not by content

`failures.json` records a parked range by `conversationId` and `end_message_id`. The corresponding queue
file is found by its **name suffix** `-<convHash12>-<endHash8>.json` — the same suffix
`queue_entry_suffix` produces.

The payload is deliberately never read. A parked batch is usually parked *because* its payload could not be
parsed or was oversized; opening it to decide where it belongs would fail the same way and the batch could
never be moved. The file name is the whole contract.

Only names matching the exact `YYYYMMDDTHHMMSSmmmZ-<12 hex>-<8 hex>.json` shape are candidates, so
`claims.json`, `.tmp` leftovers, and hand-edited names are left untouched.

## Direction

One function moves both ways:

- **park** — `queue/` → `queue/parked/`
- **restore** — `queue/parked/` → `queue/`

`restore` exists for an explicit retry. It uses the same matcher so a batch can always be found again after
being parked.

A missing source directory is `0` moved, not an error: a fresh store has nothing parked, and the sweep runs
on every extraction cycle.

The target directory is created by the **first** moved file only. Per-file `create_dir_all` costs a syscall
per batch; never creating it fails the first rename.

## Records that participate

Only records whose state is `Parked` produce suffixes. A `Retrying` record is still in the queue and must
not be moved — moving it would remove material that is about to be retried.

## Migration of a pre-parking store

A store written before parking existed has parked records whose files are still in the queue. The migration
reads `failures.json` and moves those files once per queue directory.

The once-per-directory mark is set **after** the move succeeds, not before. The ledger write and the file
move are not one transaction: a crash between them leaves a parked record whose file is still in the queue.
Retrying the move is the repair, so a failed move must be attempted again rather than skipped for the rest
of the process's life.
