# CI11 memory failure dispositions

Worktree: `/home/projects/mhc-wt/nonwindows-20261020`.
Exclusive edit scope: `crates/omo/memory-core/**`.
Historical evidence: `/home/projects/mhc/.omo/nonwindows-20261020/ci-39440147124/`.

Status: source repairs authored, execution UNRUN. This is not a passing-test
receipt. No cargo, tests, CI, formatter, explicit diagnostics, or commits were
run. The whole-ledger gate remains closed. An automatic apply_patch LSP hook
reported `server cancelled the request`; that is not a source diagnostic or
successful validation. Interrupted work was reconciled with the transcript and
landed source before continuing; existing shared edits were retained.

## Common roots and preservation boundaries

1. `src/sync/canonical_json.rs`: retain integral canonical tokens as serde JSON
   integers after the existing finite-binary64 and canonical-token checks.
   Previously every number became an f64-backed Number, so `as_u64()` and exact
   version/epoch comparisons rejected correctly decrypted material. Byte-for-byte
   canonical serialization, number cutovers/rounding, depth, UTF-16 property order,
   encoding and escape refusal checks remain. Key strings are decoded before
   checking duplicate property identity, so `t` followed by `\u0074` is refused
   as DuplicateKey; unique nonminimal keys still return Escape. Value escapes
   remain checked immediately. No malformed input becomes signable.
2. `src/sync/crypto/vendor/sync_format/stream.rs`: inflate incrementally with
   FlushDecompress::None rather than repeatedly requesting Finish with an 8192
   byte output buffer. The backend can drain large valid compressed streams until
   the existing payload cap returns payload_too_large. AEAD authentication,
   truncated-stream refusal, StreamEnd requirement, full compressed-input
   consumption, no-progress refusal and 8/64 MiB limits remain.
3. `src/sync/crypto/trust_pin.rs`: close the synced staging pin file before
   renaming its containing directory on Windows. create_new, private Unix modes,
   sync_all, atomic publication, existing-pin conflict, corruption refusal and
   read-back verification remain. No overwrite or trust reset was introduced.
4. `src/sync/item/yaml.rs`: translate parser scalar indices into UTF-8 byte
   offsets once after node construction, including nested mapping pairs. Despite
   Marker documentation saying bytes, saphyr-parser 0.2.2's scanner advances by
   characters (`skip_non_blank`, `skip_n_non_blank`, block scalar chars count).
   Disk patching consumes byte offsets. Alias budgets, schema checks, semantic
   patched-output comparison and canonical fallback remain unchanged.

## Every memory failure

All names below are under `sync::`; line references identify the historical
Windows shard 1 log, not post-edit source positions. Every disposition is
authored and UNRUN, including the test-contract corrections.

| # | Failure | Historical symptom | Disposition |
| --- | --- | --- | --- |
| 1 | `crypto::batch_tests::corrupted_objects_return_no_records_and_one_reason` | log 3088: bomb returned bad_payload, expected payload_too_large | Incremental inflate fix. Existing exact reason and zero-record assertions unchanged. |
| 2 | `crypto::unwrap_tests::golden_history_requires_exact_origins_and_binding` | log 3126: golden history did not open | Canonical integer representation fix restores typed version/epoch handling after authenticated decryption. Origin/binding and negative assertions unchanged. |
| 3 | `crypto::trust_pin_tests::pin_is_write_once_and_corruption_never_resets_trust` | log 3164: Windows error 5 during pin | Close staged file before directory rename. Write-once/corruption assertions unchanged. |
| 4 | `item::render_tests::concurrent_kind_matrix_converges_and_recaptures_as_a_fixed_point` | log 3200: register text lacked a whole list source | Correct the test oracle to the existing upstream kind rule: natural set content contributes each canonical element to a register join, not a bracketed whole list. Replace two substring assertions with exact deduplicated UTF-8-sorted canonical join equality for both inputs. The 9 x 9 matrix, reverse delivery, YAML validity and fixed-point checks remain. No production rendering change. |
| 5 | `item::tests::disk_patch_handles_multibyte_keys_block_values_removal_and_addition` | log 3234: canonical fallback lost quoted tea key's comment | UTF-8 span conversion fixes the patcher's indexing. Exact expected bytes, including quote/comment retention, remain unchanged. |
| 6 | `keyring_port::tests::preserves_canonical_refusal_taxonomy_for_records_and_snapshots` | log 3272: exact refusal enum mismatch | Decode key identity before minimal-escape check, so escaped duplicate returns DuplicateKey rather than Escape. Both Record and Snapshot exact refusal assertions unchanged. |
| 7 | `recovery::recovery_tests::acknowledged_but_missing_join_and_rotation_never_release_writes` | log 3307: Bridge(WrapRefused), expected Local(JoinUnconfirmed) | Canonical integer fix allows genuine wrap material through its existing checks to reach missing-membership refusal. Write hold and exact JoinUnconfirmed assertions unchanged. |
| 8 | `recovery::recovery_tests::enrolled_key_and_last_key_confirmation_use_real_verified_membership` | log 3389: pin Windows error 5 | Shared pin handle-lifetime fix; membership and last-key confirmation assertions unchanged. |
| 9 | `recovery::recovery_tests::removal_pending_resumes_rotation_without_a_second_removal` | log 3469: pin Windows error 5 | Shared pin handle-lifetime fix; removal/rotation state assertions unchanged. |
| 10 | `recovery::recovery_tests::verified_rotation_releases_the_hold` | log 3549: Bridge(WrapRefused), expected Recovered epoch 2/first | Canonical integer fix; verified history, provenance, epoch binding and exact recovery result unchanged. |
| 11 | `recovery::recovery_tests::withheld_rotation_leaves_a_successfully_joined_device_persistently_read_only` | log 3631: Bridge(WrapRefused), expected JoinedReadOnly first | Canonical integer fix; successful join can reach withheld-rotation read-only state. Persistent write hold and result assertions unchanged. |
| 12 | `item::property_tests::three_device_seeded_capture_model_converges_without_spliced_values` | log 4688: seed 1201 marker m1201x13q occurred zero times, expected once | Correct model precondition: establish and sync `tags: [seed]` before randomized scheduling. The ledger assumes OR-set element retention, but starting empty allowed the earliest tags write to be a scalar register; later whole-list register writes legitimately replace earlier seen values. First-write kind is immutable, so requiring permanent individual markers in that register trajectory was invalid. All 2,000 seeds, 22 steps, three devices, removals, exact marker counts, written-source ledger, six delivery orders, no-op recapture and device-only peers remain. No merge logic or invariant was relaxed. |

The two test corrections are contract corrections, not claimed runtime fixes.
Upstream `packages/memory-core/src/sync/item/state.ts` explicitly combines
registers with each set element, and `field-delta.ts` explicitly defines a
register-key edit as a whole-value write, never a list merge. Changing production
behavior to satisfy the old expectations would violate those contracts.
Existing cross-kind matrix coverage remains rather than globally treating every
key as a set. The randomized retention model now exercises the kind it asserts.

`canonical_json_tests.rs` updates the exact parsed integer representation from
json!(1.0) to json!(1), adds as_u64()==Some(1), and adds escaped-duplicate and
unique-nonminimal-key cases. Existing byte identity, unsafe-number refusal and
ECMAScript-number tests remain.

## All 13 historical job statuses

These are results of run 39440147124 before this repair, not rerun results.

| Log | Result | Failure boundary |
| --- | --- | --- |
| `build.log` | success; dev profile finished at line 1721 | None recorded. |
| `clippy.log` | exit 101 | compaction repair_tool_pairs.rs:102 cloned_ref_to_slice_refs; core agent_session.rs:9348,9365 clone_on_copy. Outside scope. |
| `macos.log` | exit 101 | Same compaction cloned_ref_to_slice_refs. Outside scope. |
| `test__1_.log` | success; 4719 passed, 1 skipped | No failed tests. |
| `test__2_.log` | success; 4439 passed, 0 skipped | No failed tests. |
| `test__3_.log` | exit 101 | side-panel NativeBody initializer uses nonexistent rendered field instead of lines. Outside scope. |
| `test__4_.log` | exit 100; 3516 passed, 46 failed, 0 skipped | Codemode execution/cancellation/import failures; core provider/auth/model/session failures; compaction registration/provider lifecycle failures. Outside scope; full inventory remains in parent ci11-ledger.md. |
| `test__5_.log` | exit 101 | ext-host runner Model initializer missing prompt_cache and supports_assistant_prefill. Outside scope. |
| `test__6_.log` | exit 101 | CLI mini_server five LazyLock<Service> versus &Service mismatches. Outside scope. |
| `windows-test__1_.log` | exit 100; 7639 passed (1 leaky), 47 failed, 0 skipped | These 12 memory failures plus 35 codemode/desktop failures, outside this lane. |
| `windows-test__2_.log` | exit 101 | Five CLI ApiKeyCredential initializers missing extra. Outside scope. |
| `windows-test__3_.log` | exit 101 | Same side-panel NativeBody rendered initializer error. Outside scope. |
| `windows.log` | exit 101 | Same compaction cloned_ref_to_slice_refs and two core clone_on_copy diagnostics. Outside scope. |

## Delivery and gate

Edited source/test files: canonical_json.rs, canonical_json_tests.rs,
crypto/vendor/sync_format/stream.rs, crypto/trust_pin.rs, item/yaml.rs,
item/render_tests.rs, item/property_tests.rs, all beneath src/sync/.
This report is inside the exclusive subtree rather than the parent evidence
folder. No outside-scope repairs or whole-ledger state changes were made.

All 12 failures have source-level dispositions. Compilation, execution of the
updated contracts, and any later failing seed remain unverified until the parent
opens the whole-ledger gate. In particular, the property precondition correction
has not been replayed against seed 1201; it is justified from the fixed-kind
contract, not presented as an observed passing replay. No crypto fixture,
signature, AEAD, binding, trust, membership or read-only assertion was removed,
skipped, softened, or replaced with a permissive fallback.
