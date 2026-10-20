# Chord facets and public API port

Source: `@earendil-works/chord` 0.99.1 at
`/tmp/parity-up/senpi-1019/packages/chord`.

The requested `uninspected-audit-20261020.md` was read and contains no Chord
row. The exact census rows are `gaps.tsv` 1846-1848 and 1860-1861, with
the shared facet contracts in row 1851. Before this work, this crate had no
facets, API, index, or bundler modules. Its existing context, delta, JSON,
and service ports were retained.

## File mapping and implementation

- `src/facets/host.ts` -> `src/facets/host.rs`
- `src/facets/loader.ts` -> `src/facets/loader.rs`
- `src/api.ts` -> `src/api.rs`
- `src/index.ts` -> `src/index.rs`
- `test/facets.test.ts` -> `tests/facets.rs`
- `test/facet-loader.test.ts` -> `tests/facet_loader.rs`
- Shared test fixtures live in `tests/facets_support/mod.rs`.

Host code follows upstream declaration order. It discovers synchronous setup
requirements, validates providers/modes/cycles, orders activation, assembles
local and remote providers, opens grouped external sources, awaits readiness,
and owns reverse-order cleanup. Keyed services stage candidate instances until
cutover; lifecycle-scoped observations cancel on retirement. Singleton member
handles resolve current targets. Reload validates declaration and remote member
shape, preserves old targets during candidate activation, rolls back failed
candidates, and terminates on cleanup/publication failure after cutover.

Loader combination is sequential on load, concurrent on disposal, with reverse
generation order, idempotent combined disposal, and collected cleanup failures.
Public service definitions reject empty/reserved IDs and preserve the local
flag. Mutable and source-attached replicated-state constructors forward to the
existing state implementation. The public index re-exports the native API,
types, provider, JSON, state codec and wire implementations.

Rust adaptation: `ServiceDescriptor` preserves ID and local flag at the erased
facet boundary. Local implementations use `ServiceObject`; remote implementations
use `ProviderImplementation`. Consumers downcast acquired values to
`FacetServiceHandle`, whose `member` returns a lifecycle-guarded cached member.
`clone_environment` permits activation callbacks to own resources without a
borrow escaping synchronous setup. Setup's return type cannot be asynchronous.
The two replicated-state overloads are `replicated_state` and
`replicated_state_from_source`. No existing workspace caller implemented the
facet environment/spawner contracts before this work; a workspace-wide caller
search preceded their changes.

## Blocked and remaining parity

`src/bundler.ts` exists upstream but is only a barrel for `node/bundle.ts`,
`node/manifest.ts`, and `node/package.ts`. This crate has no native `node`
modules. Their implementations (actual JavaScript bundling, versioned manifest,
and package production) belong to the separate Node/bundle scope. A faithful
`bundler.rs` cannot re-export absent symbols. No stub or empty barrel was added.
The requested scope is therefore not wholly complete; do not close all rows.

The new tests are upstream-derived native fixtures, not a claim of complete
one-to-one porting of every assertion. In particular, all source-reopening,
local arbitrary-object reflection, proxy descriptor, and source-overload cases
are not reproduced here. JavaScript structural contracts are represented by
explicit Rust objects, not arbitrary reflected Rust structs. External singleton
sources currently must return `ServiceFacade` values. `FacetMember` is the stable
reload-safe access surface. Cancellation of a Rust host-transition
future is not equivalent to an uncancellable JavaScript Promise: dropping a
transition future is not supported as a rollback operation.

## Consolidation outside this scope

Add the root workspace member line (not edited here):

```toml
    "crates/maho-chord",
```

Register this local package in `Cargo.lock` during global consolidation. Its
existing Cargo manifest already opts into workspace lints and uses only locked
workspace dependencies: serde, serde_json, thiserror, tokio and futures. No
additional dependency was introduced by this worker.

The inspected `maho-agent/src/harness/context.rs` retains its existing context
primitive and maho-ai abort integration. Later it should consume/adapt
`maho_chord::context` and shared Context/AbortSignal contracts instead of its
description-plus-TypeId identity. Nothing was moved or edited there.

The inspected `maho-cli/src/experimental/mini/shared/protocol.rs` keeps its
`ServiceToken` and const `define_service` unchanged. Later it should use validated
`maho_chord::define_service` and typed `Service<T>` descriptors; runtime validation
will require lazy initialization rather than the current unchecked const helper.
No call site outside this crate was modified.

## Verification

Source inspection only. No cargo, builds, tests, diagnostics, commits, pushes,
or child agents were run, as explicitly requested. The new tests use channels,
oneshot signals, and barriers with bounded awaits, never fixed sleeps or polling.
Compilation, clippy, test outcomes, and runtime parity remain unverified until
the consolidated GitHub CI run.
