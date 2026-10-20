# CI11 memory property investigation

## Verdict

Inconclusive: diagnostic instrumentation delivered, not a source fix or a claim
that the marker oracle is invalid. No cargo, tests, formatter, LSP diagnostics,
CI, upstream runtime or CRDT replay was run. The only computation was the
test's integer PRNG and branch selection in the resident JavaScript kernel.
The next integrated CI remains necessary. Every original assertion is retained.

## Provenance

Failure: `/home/projects/mhc/.omo/nonwindows-20261020/ci-39440147124/windows-test__1_.log`,
lines 4688-4691: original snapshot
`89424352dd294dfee7243430c394d842ce0cec54`, property line 282,
seed 1201, `m1201x13q`, actual 0 versus expected 1.

The original source receipt was located at
`/home/projects/mhc/.omo/nonwindows-20261020/memory-item-source.md`.
It records authoring in `/home/projects/mhc-wt/omo-5138` and upstream reads in
`/tmp/parity-up/omo-5138/packages/memory-core/src/sync/item`.
The latter checkout resolves to `ce910506c13f6f9dfb973e686769e22dbd91a5d1`,
matching the oh-my-openagent pin in the repair worktree's `PINS.md`.
The same commit exists in `/home/indo/code/oh-my-openagent`.
The senpi pin `903b83367545bea516f35fc7f7c7cb692847988d` also exists in
`/home/indo/code/senpi`; item sources belong to the pinned omo repository.
Upstream was available, not an access blocker.

Read at that pin: `key-writers.ts`, `history.ts`, `frontmatter.ts`,
`field-delta.ts`, `device-capture.ts`, `idempotence.r3.test.ts`, and
`kind-rule.test.ts`. The original receipt explicitly describes the native
property as an adapted generator, not an exact upstream trajectory.

## Deterministic schedule, not CRDT evidence

The native `next` calculation was reproduced using unsigned 32-bit addition,
`Math.imul`, unsigned shifts, and precisely the additional draws consumed by
edit branches. Steps are zero-based; peers are 1-based. `extra` is the index
into the native hostile-string array. These are scheduled inputs only:

| Step | Roll | Peer | Scheduled action |
| --- | --- | --- | --- |
| 0 | 589489076 | 3 | description edit, extra 10 |
| 1 | 1059463125 | 1 | sync peers 1 and 2 |
| 2 | 2484195616 | 2 | sync peers 2 and 3 |
| 3 | 984691817 | 3 | capture/materialize, crash after write |
| 4 | 3766828213 | 2 | tags edit, extra 8, operation 0 |
| 5 | 3392270711 | 3 | capture/materialize/save baseline |
| 6 | 2422357060 | 2 | tags edit, extra 8, operation 3 |
| 7 | 117270345 | 1 | description edit, extra 1 |
| 8 | 473457754 | 2 | sync peers 2 and 3 |
| 9 | 2498319439 | 2 | double capture |
| 10 | 3355909587 | 1 | sync peers 1 and 2 |
| 11 | 3162781066 | 2 | append body marker, extra draw 9 |
| 12 | 3005223543 | 1 | sync peers 1 and 2 |
| 13 | 3443521251 | 1 | tags edit, extra 6, operation 1 |
| 14 | 2060511929 | 3 | capture/materialize, crash after write |
| 15 | 2450909182 | 2 | tags edit, extra 0, operation 0 |
| 16 | 3284155953 | 1 | sync peers 1 and 2 |
| 17 | 3242519180 | 3 | double capture |
| 18 | 2783300089 | 2 | capture/materialize, crash after write |
| 19 | 2436645846 | 1 | capture/materialize/save baseline |
| 20 | 3546822710 | 3 | description edit, extra 1 |
| 21 | 353227665 | 1 | tags edit, extra 3, operation 2 |

Step 4 puts a list on peer 2's empty disk; step 6 replaces it with a scalar
before that peer's first scheduled capture at step 9. Thus the first capture
of that peer's tags input is scalar, not its earlier list. Step 13 on peer 1
constructs the list containing `m1201x13q`. Later tags edits may encounter
register renderings, not the original list model. This is a concrete reason
to inspect whole-value replacement and visibility, NOT proof of the marker's
actual removal or of a correct replacement expectation. No complete expected
state was derived and no oracle was changed.

## Competing explanations and missing evidence

1. Legitimate whole-value replacement on a register-kind key: pinned
   `key-writers.ts` fixes kind at first write, converts a later list to its
   entire flow source, and supersedes seen register entries. `field-delta.ts`
   does not perform element deltas for register-kind keys. The native generator
   turns any non-set disk value into an empty element list before a tags edit;
   its removed ledger records explicit removals but not arbitrary register
   replacement. To establish this explanation requires the actual kind stamp,
   register entry containing the marker, later supersedes edge, and receipt
   showing the writer saw that entry. CI11 records none of those.
2. Capture/receipt visibility defect: an entry absent from the writer's seen
   receipt could be superseded or deleted incorrectly. Distinguish this using
   the selected landed receipt, its seen tags, and operation dependencies at
   the exact capture where the marker disappears.
3. History/rendering defect: the marker could remain active in the maps while
   history classification or YAML rendering hides it. Distinguish this using
   raw map entries, history stamps, active supersedes edges, and render bytes.

The native implementations read here have the same high-level first-kind and
receipt branches as the pin. This does not establish their execution equivalence.
The original failure log alone cannot decide these explanations. Dropping a
marker check based on explanation 1 would repeat the rejected speculative repair.

## Delivered changes and next-CI evidence

Only `property_tests.rs` and this report were edited, within the granted item
subtree in `/home/projects/mhc-wt/nonwindows-20261020`. The rejected `[seed]`
initialization and its initial syncs were removed; disks, saved text and saved
frontiers again start empty exactly as the original source. No preseed or
baseline substitution remains. Production item behavior is unchanged.

For seed 1201 only, `trace_ci11` emits one JSON line per peer with prefix
`CI11 `. It records all peers before every scheduled step and each final
capture, between the two calls of a double capture, after capture but before
materialization, and after final synchronization. Each snapshot includes:

- Stage, exact roll/action/selected peer, disk bytes, saved text and frontier.
- Current frontier, render result, raw tags state and computed history.
- Full device receipts and their landed status.
- Full uncompressed JSON causal operations including IDs, dependencies,
  Lamport stamps, map values and supersedes arrays.

The final snapshot also prints the unmodified markers, removed and written
ledgers. Existing nextest failed-test stderr should therefore carry the
evidence without changing workflows or needing retained temporary directories.
The trace is observational test code, not a production-derived oracle, and
does not filter seeds or bypass assertions. It should be removed after the
failure mechanism is established.

At the next integrated CI, find the first snapshot containing the marker in
an active tags entry and the first subsequent snapshot where it disappears.
Use the causal operations to identify the removing/superseding entry and its
author. Compare that author's immediately preceding landed receipt and seen
tags. If the entry remains active, compare kind history and render instead.
Only then fix the production defect, or implement an independent complete
expected-state model of writes, visibility, supersession, set removals and
body edits. Do not derive expected presence from production maps or render,
and do not simply label every missing/replaced marker optional.

## Source inspection limits

The source patch was inspected against the original snapshot without executing
checks. A normal worktree `git diff` cannot describe these item files because
they are currently untracked relative to local HEAD; the original CI snapshot
and the preserved authoring copy are the meaningful comparison sources.
No unrelated landed fixes were edited. No commit or CI dispatch was performed.
Compilation of the diagnostic addition and the failing property remain unrun.
