# Observation provenance is carried verbatim

**Status:** implemented
**Crates:** `maho-memory` (`crates/maho-memory/src/records.rs`, `people_store.rs`)
**Upstream reference:** `crates/omo/memory-core/src/people/format.rs`

## The defect

`Observation` modelled the provenance comment as three optional fields — `src`, `pattern`,
`confidence` — and `render` regenerated the comment from those three. A comment on disk may carry
any keys; the ones actually present in this machine's memory store include `n`, `status`,
`expires`, `agreement-event`, `since`, `last`, `id`, `by` and `premise`.

Because `Observations::render` rebuilds every line from the parsed struct, a parse/render cycle
**deleted every key it did not model**. The module header claimed the opposite ("provenance is
carried, never regenerated"), and `people_store::render_files` advertised that "rendering an
unmodified book reproduces the input files byte for byte".

Measured on the live store (`~/.omo/memory/agents/mhc-e1e16759/repo`), before the fix, all 6 of the
9 observation-shaped files changed and lost provenance:

```
system/human.md       lost "src: direct user instruction after codemode-asset-cleanup merge;
                            reaffirmed after retry-reqwest-decode merge; pattern: ..."
system/boundaries.md  lost "src: release v2026.11.09 correction; also recorded 2026-10-25"
system/self-aware.md  lost every "status: observed; expires: 2027-01-14" expiry
```

`n=2` entries (74 of them exist) lost their count outright, and an expiry silently disappearing is
the worst case: the entry then never expires.

The existing round-trip test passed because its fixture only used the three modelled keys. That is
the general shape of this defect class — the test was written against the implementation, so it
could not fail.

## The rule

**A field this crate does not understand is not a field it may delete.**

`Observation` now stores `provenance: Option<String>` — the comment exactly as it appeared between
`<!--` and `-->` — and `render` writes it back untouched. Known keys stay readable through
`provenance_value`, `src()`, `pattern()`, `confidence()`.

`provenance_value` detects the separator instead of assuming one, because the format on disk uses
both: `n=` is written with `=`, every other key with `:`. A reader that fixed a separator would
miss the other half of the comments.

Constructors are `Observation::new(date, text)` and `.with_provenance(Option<String>)`, so a caller
supplies the comment text rather than a set of fields the crate models.

Not changed: `Observation::parse` still only accepts `- [YYYY-MM-DD] text`, and a line without a
recognisable date is still kept verbatim. Blank-line preservation inside observation sections is a
separate, older divergence and is not addressed here.

## Verification

- `cargo nextest run -p maho-memory` → 1468 passed, 0 skipped, exit 0.
- `cargo clippy -p maho-memory --all-targets -- -D warnings` → exit 0.
- `provenance_keys_this_crate_does_not_model_survive_a_rewrite` covers `n=`, `status:`,
  `expires:` and a four-key comment.
- `provenance is never upgraded by a later read`, `appending_keeps_the_keys_this_crate_does_not_model`
  cover the append path in `people_store`.
- A store-wide probe over the live memory repository compared every `<!-- ... -->` comment before
  and after a parse/render cycle: 9 files, 0 provenance losses after the fix; the same probe on the
  pre-fix tree reported 6 files losing comments. The probe was removed after the measurement —
  it reads a machine-local path and must not become a test.
