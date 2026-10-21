# Chord delta port status

Upstream: `/tmp/parity-up/senpi-1019/packages/chord`, package
`@earendil-works/chord` 0.99.1. Census group: `c098-senpi-chord-api`.
The aggregate `uninspected-audit-20261020.md` contains no Chord row; the exact
scoped rows are in `gaps.tsv` at 1849, 1851, and 1854-1859.

## Module mapping

| Upstream | Native |
| --- | --- |
| `src/delta/index.ts` | `src/delta/index.rs` |
| `src/delta/diff.ts` | `src/delta/diff.rs` |
| `src/delta/tracker.ts` | `src/delta/tracker.rs` |
| `src/delta/revision-validator.ts` | `src/delta/revision_validator.rs` |
| `src/delta/apply-immutable-trusted.ts` | `src/delta/apply_immutable_trusted.rs` |
| `src/json.ts` | `src/json.rs` |
| `src/types.ts` | `src/types.rs` |

The Rust API uses typed operation variants serialized as upstream tuples.
`JsonValue` owns primitives and shares container allocations. Checked and trusted
replay use `Arc::make_mut` so previous revisions and payloads remain untouched.
Draft operations are explicit Rust methods, not JavaScript property proxies.
Prepared base/value/operations are read-only getters; adoption accepts a borrowed
preparation so retained publication reads and repeated-adoption errors are possible.

## Consolidation changes outside this worker's scope

Add this member to the root `Cargo.toml` workspace members:

```toml
    "crates/maho-chord",
```

Only existing workspace dependencies are used: `serde`, `serde_json`, `thiserror`,
`tokio`, and `futures`. Their versions are already locked. Consolidation must regenerate
the workspace lockfile to register this local package. The crate enables
`serde`'s `derive`/`rc` and `serde_json`'s `preserve_order`/`raw_value` features.

`crates/maho-agent/src/harness/context.rs` currently implements its own immutable
context chain and integrates `maho_ai` abort signals. A later context worker should
implement the shared Context/AbortSignal contracts or provide an explicit adapter,
then make the harness consume that implementation. Its existing key identity is
description plus TypeId, whereas upstream symbol identity requires independent
allocations even for identical descriptions. Nothing was moved or changed there.

`crates/maho-cli/src/experimental/mini/shared/protocol.rs` retains its local
`ServiceToken` and `define_service`. After the service worker implements the native
service definition module, these constants should consume typed `Service<T>`
identities rather than introducing a second provider runtime. Nothing was moved or
changed there.

`crates/maho-server/src/client/service_wire.rs` still contains the older private
wire decoder and rejects the `m` verb. Its callers in the server and tests should
later consume the shared delta decoder (or adapt its JSON output) to gain
permutation, root-array and short-form support. A workspace-wide caller search
found no consumers of `maho_chord` outside this crate. This pass changed
`JsonValue::String` and `Op::Append` payloads to `JsString`, `WireOp`/`to_tuple`
to lossless `JsonValue`, and conversion to `serde_json::Value` to `TryFrom`.
Existing UTF-8 constructors use `.into()`; consumers must serialize wire JSON
directly to text/bytes rather than pass surrogate-bearing values through
`serde_json::Value`. No outside call sites were edited.

## Chord delta3 implementation

- Confirmed upstream `current.slice(operation[2])` preserves isolated UTF-16
  units, while native `from_utf16_lossy` replaced them. `JsString` now uses a
  UTF-8 fast representation for scalar strings and UTF-16 storage otherwise.
  Equality, sorting, append, overlap, suffix slicing, diff/tracker operation
  generation, JSON parsing, operation tuples, codec interning, service calls,
  snapshots and updates preserve code units. Serde raw JSON escapes isolated
  surrogates; conversion to `serde_json::Value` fails rather than replacing data.
- Replaced eager array entry loading with a seeded piece treap. Base and inserted
  source ranges support split/merge, normalized joins, reverse, indexed lookup,
  sparse overrides, sort restoration and detached duplicate placements. Merely
  reading length allocates no per-element nodes. Materialization reads untouched
  source values directly. Held descendants retain source identity across moves.
- Dirty registration records mutation order and propagates subtree dirtiness.
  Emission is depth-ordered and stable within depth, skips clean nodes, folds
  placements/reserved names and dense regions, and caps operation batches.
  Array plans emit descending removals, optional permutation, then insert runs.
  Settled and invalidated overlays release old container references even when a
  draft or Change is retained; Prepared still owns readable base/value/ops.
- Removed the equal-length semantic positional fallback absent upstream. The
  greater-than-LCS-budget semantic regression now expects a splice, not leaf sets.

## Upstream delta test mapping

The original deterministic fixtures remain. Additional Rust tests translate the
upstream behavior exposed by explicit draft methods:

| Upstream suite | Native coverage |
| --- | --- |
| `delta.test.ts` | `delta.rs`, `delta_tracker.rs`, `delta_upstream_codec.rs`, `delta_upstream_tracker.rs` |
| `delta-diff.test.ts` | `delta_diff.rs`, `delta_upstream_diff.rs` |
| `delta-apply-immutable.test.ts` | `delta.rs`, `delta_upstream_immutable.rs` |
| `delta-clone.test.ts` | `json.rs`, `delta_upstream_tracker.rs` ownership/placement cases |
| `delta-tracker/tracker.test.ts` | `delta_tracker.rs`, `delta_piece_tracker.rs`, `delta_upstream_tracker.rs`, `delta_upstream_transactions.rs` |
| `delta-tracker/retention.test.ts`, `retention.worker.ts` | `delta_retention.rs`; deterministic Arc/Weak ownership assertions replace V8 GC/heap/time thresholds |
| isolated-surrogate regressions | `delta_utf16.rs`, including every isolated surrogate, split-pair truncation, pair rejoining, service snapshots and call arguments |

The upstream eleven-choice PRNG transaction stream is ported separately from the
pre-existing seven-choice stream. Large cases cover 40,000 sparse diff rows,
100,000-item insertion, 300,000-item replay, queue normalization, fragmentation,
dense subregions, reserved-key coverage and operation overflow. A tracker unit
test inspects lazy entry allocation without wall-clock timing.

## Remaining language-boundary differences and consolidation gate

- JSON's Rust type rules out undefined, cyclic graphs, accessors, symbol properties,
  sparse arrays and non-finite numbers before these APIs. The omission option is
  represented at application serialization boundaries, not by an undefined-bearing
  input type. JavaScript null prototypes and descriptor reflection have no native
  representation. These are deliberate type-boundary differences, not tested JS
  reflection equivalents.
- JavaScript coercion callbacks, borrowed Array.prototype methods, Promise
  assimilation, proxy descriptors and null prototypes have no executable
  equivalent in the explicit typed Rust API. Their JSON-visible results are
  covered where representable; these are not claims of JavaScript reflection
  compatibility. Invalid JSON is tested at the parsing/number boundary.
- Object overlays still load their own entries on first access. The upstream
  one-write/bounded-object fast layout and cached source-location generation
  optimizations are not exact structural translations. Array lookup is treap
  indexed, but resolving accessed nodes uses ordered sparse source ranges.
- JSON object keys, service identifiers and path keys remain Rust `String`;
  isolated-surrogate **values** and append payloads are lossless. Extending
  isolated-surrogate support to every identifier/key contract is separate work.
- Do not close the aggregate audit group solely from this source pass. Complete
  native test execution and compile/lint review remain a global CI gate.

## Verification

No cargo build, check, test, clippy, explicit diagnostics, commits, pushes or child
agents were run. The edit hook applied rustfmt automatically and reported fresh
LSP diagnostic timeouts on edited files; these are not passing diagnostics.
Consolidated
GitHub CI must compile and run the new tests; their passing status is unknown.

### Completion verdict

`CHORD_DELTA3_BLOCKED`: the lossless value/wire path, canonical diff correction,
lazy piece-array implementation and expanded deterministic test port are present,
but an exact full upstream-suite port cannot be claimed: JavaScript-only cases
above lack a native API equivalent, and the tracker still differs structurally
in bounded object overlays/source-location caching. The test mapping is a
coverage guide, not evidence that every upstream assertion was executed or that
all large-array complexity contracts have been proven. Preserve these changes
and keep this row open until those remaining equivalence decisions and global
CI results are available.
