# Chord service and context port

Source: `@earendil-works/chord` 0.99.1 at
`/tmp/parity-up/senpi-1019/packages/chord`.

The requested `uninspected-audit-20261020.md` contains no Chord row. The exact
service/context census entries are in `gaps.tsv` lines 1852-1853 and 1864-1874.
The native harness context and CLI service token were inspected and left intact.
The existing crate's delta, JSON, and type port was preserved.

## File mapping

Each upstream services file maps to the same Rust module, using underscores
for Rust module identifiers: `state`, `state_codec`, `state_internals`, `provider`,
`consumer`, `handle`, `instances`, `loopback`, `wire`, and `errors`.
`src/context/index.ts` maps to `src/context/index.rs`.

Implemented source paths cover identity-keyed immutable contexts, cancellation
inheritance/masking, externally owned operation waiting, typed cancellation
reasons, mutable state transactions, source attachment cursors, replica sequences,
bounded per-listener delivery, exact provider publications, weak state internals,
service wire parsing, subscription-scoped codecs, provider singleton/keyed
lifecycles, overflow resets, endpoints, loopback transport, consumer facades,
guarded cached handles, and keyed observer cancellation.

Rust represents JS reflection contracts explicitly. `ProviderImplementation`
classifies method/state variants; `ServiceObject` models receiver-owned members;
`ServiceFacade::member` and `MemberSlot::invoke/value/subscribe` replace proxies.
The Rust method signature requires a Context argument, so omission is rejected
at compilation rather than by a trailing-argument runtime check. Mutation
callbacks are synchronous Rust functions; a future cannot satisfy their return
type. Cancellation reasons are typed errors rather than arbitrary JS values.

The shared `AbortSignal` trait gained default `reason` and `cancellation_order` methods; a workspace
caller search found no other implementations of this trait outside this crate.
Source cursor fields use i64 to preserve upstream negative safe integers. A whole
workspace caller search found these structs only within maho-chord. Other
existing APIs retain their signatures.

## Consolidation changes outside scope

Add the following workspace member to the root Cargo.toml (not edited here):

```toml
    "crates/maho-chord",
```

Regenerate Cargo.lock during consolidation. The crate already opts into workspace
lints. Only existing locked dependencies are used: serde, serde_json, thiserror,
tokio, and futures. Tokio needs rt, macros, sync, and time features. Pending public
listeners and remote binding startup need an entered Tokio runtime.

The facet/API worker should call `MutableReplicatedStateImpl::new`,
`attach_replicated_state_source`, and `RemoteServiceBindingImpl::new` from the
corresponding api.ts ports, and implement service-definition validation there.
Facade consumers downcast the `ServiceImplementation` to `ServiceFacade`; providers
construct explicit `ProviderImplementation` member maps.

The harness should later adapt `maho_ai` cancellation into the shared AbortSignal
trait and use this crate's ContextKey identity and contexts. Its current owned-
future cancellation helper must not simply alias this crate's helper: the latter
takes a borrowed JoinHandle so the caller retains operation ownership after
cancellation. No harness implementation was moved or changed.

The CLI's `ServiceToken`/`define_service` should later consume the shared
`Service<T>` definitions from the API port, including empty-ID/reserved-namespace
validation and local flags. No CLI token or call site was changed.

The server's private service_wire decoder/parsers remain unchanged; route them
through the shared wire/codec modules to acquire reset/permutation support.
These existing callers mean the census cannot be closed solely by adding a
crate that has not yet been registered and adopted.

## Remaining work and verification boundary

No cargo, build, test, diagnostics command, commit, push, or child agent was run,
as requested. The edit hook attempted automatic LSP refreshes on several files;
those attempts timed out and yielded no diagnostics. Compilation, lint cleanliness, and test results are unknown until
consolidated GitHub CI. Source review is not a substitute for that validation.

Tests port representative upstream cases for context identities/cancellation,
transaction rollback/sharing, source cursor failure, replica gaps, listener
overflow, singleton rebind/replacement, keyed generation cancellation, provider
activation FIFO, endpoints, strict wire parsing, independent state codecs, and
cached receiver-preserving handles. They use exact channels/cancellation signals
with bounded timeouts, never sleeps or polling.

Status: native implementation added; do not close the service/context census
rows before consolidated CI and integration. This worker does not claim
CHORD_SERVICES_DONE: the remaining full-test translation and the inherited
delta parity gaps prevent a faithful-complete handoff.
Consumer rebind/dispose transitions are serialized, readiness observes transition
completion, and startup checks its revision again after activation. These source
changes still need the full adversarial upstream transition tests in CI.
The complete upstream test corpus is not translated. Additional fixtures cover
reporting after unsubscribe, disconnect/rehydration serialization, cold-replica
reentrancy, no-op identity, isolated source failures, shape enforcement, buffered
disposal, subscriber failure aggregation, pending-start rebind cleanup,
cancelled readiness, reset rebaselining, permutation wire operations, negative
source cursors, and directory readiness/replacement. Further parity validation
is needed for overlapping concurrent rebind/readiness transitions, reentrant
startup/disposal races, exceptions thrown by error reporters, and JS proxy
reflection. Native cancellation signals record first-abort ordering. External
AbortSignal adapters must supply cancellation_order to preserve ordering when
both inputs abort with distinct reasons. Numeric wire sequences
use u64, whereas JS Number integers can extend beyond its lossless range. These
are explicit residual parity gaps, not claims of complete row closure. The
delta worker's remaining algorithm/string gaps in PORTING.md also remain.

Specific untranslated cases include JS-only conditional contract typing,
PromiseLike callback detection (not expressible with the synchronous Rust
callback signature), prototype/descriptor/proxy reflection, the complete
attached-state delivery matrix, every upstream guarded-host activation case,
and exhaustive simultaneous lifecycle transitions. These are remaining work,
not passing or verified tests. API/facet integration tests belong to their
workers and were not changed here.
