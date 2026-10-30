# Leak-check record: telemetry delta (omo v5.1.52)

Independent review of `docs/cleanroom/specs/telemetry.md`, section
`## Delta (omo v5.1.52, 2026-10-30)` (spec commit e5f32317, branch `sync5152/spec`). This record
is reviewer input only. Do not give it to the implementer.

## Working-tree state before review

`git status` in `/home/projects/mhc-wt/sync5152-spec` was clean. The branch head was
e5f32317, and `git diff` showed nothing. The uncommitted change the task mentioned was the delta
itself, and e5f32317 already committed it (reflog: 80074bfa, then e5f32317, with no other
telemetry.md writes). No foreign change was found, so none was left uncommitted or folded into
this commit. The only edits to `telemetry.md` in this commit are the fixes listed below.

## Sources compared

- SUL Rust: `crates/omo/telemetry-core/src` (`otel_resource.rs`, `system_os.rs`, `otlp_*`) and
  `crates/omo/components/maho-omo-telemetry/src` (`omo_native_turn_span.rs`
  `gen_ai_provider_name`, `model_vocabulary.rs`, `product_identity.rs`).
- Upstream target `~/.cache/sync-5152/omo-target/packages/telemetry-core` (`AGENTS.md`,
  `src/*.test.ts`) and `packages/omo-senpi/src/components/telemetry` (`otlp-projection.ts` and
  its test, `otlp-runtime.ts`, `semconv-map.ts`, `schema-registry/semconv-map.json`, `AGENTS.md`,
  `lazy-posthog.ts`).
- MIT side: `crates/maho-telemetry/src/{otlp,settings,record}.rs`, `crates/maho-ai/src/providers`
  (provider ids), `crates/maho-ai/src/legacy_provider_ids.rs`, `docs/cleanroom/specs/skills.md`
  Delta and `config.md` section 14.
- Census `/home/projects/mhc/.omo/ulw/sync-5152/census/omo-telemetry.md` (rows 01-27) and
  `omo-components.md` row 34.
- OpenTelemetry semantic-convention registry pages on opentelemetry.io (`registry/attributes/gen-ai/`,
  `os/`, `host/`, `service/`), fetched 2026-10-30.

## Checklist

- [x] **No SUL code block.** The delta has no fenced block. I checked with a count of fence
      markers over the delta, which returned 0.
- [x] **No translated or paraphrased SUL comment, doc comment or prompt.** I compared D1-D6
      with `omo_native_turn_span.rs` (doc comment on `gen_ai_provider_name`: "an id with no
      well-known value keeps our id"), `semconv-map.json` (`note` of the provider table, with the
      same wording) and both `AGENTS.md` files ("No identifier is derived from the hostname...").
      The original D4 sentence "otherwise the mhc provider id passes through unchanged" carries
      the same idea as that comment. The idea is an interface rule, not expression, but I
      reworded the sentence anyway: it now states the OpenTelemetry rule (listed value MUST be
      used when one applies, custom value otherwise) and the mhc choice of custom value (fix 3).
      D2's list of things that never appear is written from the MIT design ("What is recorded
      and where it goes"). It does not follow the AGENTS.md sentence order or wording. D6 does
      not resemble the AGENTS.md consent-gate prose, which is about a PostHog queue and fan-in
      that D6 does not have.
- [x] **No list or table mirrors a SUL constant table row for row** (after fixes 1, 2 and 4).
      - D4 table compared with the SUL `gen_ai_provider_name` (4 rows: anthropic, openai,
        google -> gcp.gemini, xai -> x_ai) and the upstream `semconv-map.json` provider table
        (28 rows, mostly identity rows for OmO's vocabulary, plus `custom`). The delta's table is
        keyed by the mhc `maho-ai` provider ids (`amazon-bedrock`, `azure-openai-responses`,
        `google-vertex`, `mistral`, `groq`, `deepseek`). Most of those are absent from the SUL
        table. The right-hand column is the OpenTelemetry well-known list, which is an interface
        fact. No mirroring.
      - D5 `os.type`/`host.arch` compared with SUL `otel_os_type` (3 rows) and `otel_host_arch`
        (4 rows plus `other`) and with the upstream `semconv-map.json` tables (same rows). The
        delta's lists are keyed by Rust target names, cover the full OpenTelemetry well-known
        lists, and leave the attribute out for unmapped platforms. SUL sends `other` in that
        case. The delta does not mirror those tables.
      - D1 model ids are interface facts (public model names). The delta lists only the ids
        that this release added, as test pins. There is no allowlist, so the SUL
        `model_vocabulary.rs` per-provider table is not reproduced.
      - Test values: the D3 vector (100/900/50) differs from upstream `otlp-projection.test.ts`
        (10/20/30 -> 60). The D2 host-name vector `test-host-7` was a variant of the upstream
        fixture `test-host` (`posthog-client.test.ts`, `shell-name.test.ts`,
        `terminal-program.test.ts`). Rule 5 forbids reused test vectors, so I replaced it (fix 1).
        The D2 test that asserted the absence of `machine_hash`, `omo.machine_hash` and
        `*.install.hash` named SUL resource keys. It would have led the implementer to
        OmO-internal names, and it was weaker than a positive check. I replaced it with an
        exact-key-set assertion over the MIT resource (fix 2). That keeps the invariant without
        naming SUL keys.
- [x] **No OmO-original identifier.** The new names are `mhc.provider.id` (neutral, `mhc.*`
      prefix as in the base spec) and OpenTelemetry names. I searched the delta for ultrawork,
      ulw, boulder, Sisyphus, Prometheus, Oracle, Librarian, Kibitzer, senpi, "OmO Native" and
      PostHog: 0 hits. `machine_hash` remains only in the mapping-table reason for
      omo-components-34, as census bookkeeping that names what is absent. It is not proposed.
- [x] **No OmO-original feature.** The delta specifies no analytics vendor, install id,
      salted session hash, crash capture, notice, on-disk spool, product event, model masking or
      ingest key.

## N-A decisions (second check)

All 28 rows were checked against the census. A row is a finding when it is user-observable and
wrongly marked N-A.

| row | census behaviour | verdict |
| --- | --- | --- |
| omo-components-34 | schema drops machine_hash, adds model ids and computer-use result kinds | APPLIES is correct (D1, D2). The result kinds belong to the desktop-protocol shard, not telemetry. |
| 01 | random install-id file | N-A justified. The MIT design has no persistent id (base spec, line "no anonymous installation id"). |
| 02, 04 | hostname-derived id and machine_hash removed | APPLIES via D2 invariants. Correct. |
| 03 | legacy daily-active prefix | N-A justified (no such product). |
| 05, 06 | shell/terminal property sanitised | N-A justified. The MIT payload has neither property (`grep SHELL\|TERM_PROGRAM crates/maho-telemetry/src` returns 0 hits). D2 pins the absence. |
| 07, 08 | install disclosure and first-run notice | N-A justified. Export is opt-in by configuring an endpoint, so there is nothing to disclose (SD-6). |
| 09 | crash reports retired | N-A justified. The MIT crate has no crash path. D2 pins it. |
| 10, 11 | ephemeral-home and profile-scoped consent | N-A justified. Consent is read from env and the merged `maho-config` view. There is no per-record consent scope. |
| 12 | KNOWN_MODELS gains fable-5-5 and K3.1 ids | APPLIES via D1 (verbatim pass-through). Correct. |
| 13 | `OMO_BUNDLED_SKILLS_DIR` validated override | **User-observable.** The original reason ("not a telemetry concern") did not say where the behaviour goes. It is covered by skills.md Delta D1/D2 (dbf5abaa). I reworded the reason to point there (fix 6). Not lost, so no rejection. |
| 14 | pinned model ids masked to `custom` | N-A justified, and D1 explicitly forbids masking. |
| 15 | OTLP projection | The parts a user can observe in a collector (cache-inclusive input tokens, provider name) APPLY via D3/D4. The product-event projection is N-A. Correct. |
| 16, 18, 20 | ingest key, dual-send, producer wiring | N-A justified (one user-configured destination, one producer). |
| 17 | resource attributes | APPLIES via D5 for the standard attributes. `omo.*` keys and `os.version` are excluded. Correct. |
| 19 | consent re-read before every forward | APPLIES via D6. Correct. |
| 21-23 | on-disk spool purge, owner markers, queue purge | N-A justified. The MIT crate keeps pending data in memory only (`record.rs` writes only the ledger). |
| 24-27 | type-only, packaging, generated artifact, docs | N-A justified. |

## OpenTelemetry values (author could not fetch; now verified)

- `gen_ai.provider.name` well-known list (opentelemetry.io registry, gen-ai): `anthropic`,
  `aws.bedrock`, `azure.ai.inference`, `azure.ai.openai`, `cohere`, `deepseek`, `gcp.gemini`
  (generativelanguage.googleapis.com, AI Studio), `gcp.gen_ai`, `gcp.vertex_ai`
  (aiplatform.googleapis.com), `groq`, `ibm.watsonx.ai`, `mistral_ai`, `openai`, `perplexity`,
  `x_ai`. Every right-hand value the author used is on the list and spelled correctly.
  Corrections to the left-hand side (fix 4):
  - `google-gemini-cli` is not an mhc provider id (`maho-ai` registers none; the id appears
    nowhere outside `crates/omo`). Gemini CLI also does not call the AI Studio endpoint that
    `gcp.gemini` is defined for. The row was removed.
  - `perplexity` and `cohere` are not mhc provider ids (no `maho-ai` provider). The rows were
    removed. A user-defined provider with those ids would reach a custom value, which the
    conventions allow.
  - `openai-codex` is a legacy id that `maho-ai` normalizes to `chatgpt-subscription`
    (`legacy_provider_ids.rs`). `chatgpt-subscription` was added to the row, and the legacy id
    was kept because old ledger lines and messages may still carry it.
- `os.type` well-known list: `aix`, `darwin`, `dragonflybsd`, `freebsd`, `hpux`, `linux`,
  `netbsd`, `openbsd`, `solaris`, `windows`, `zos`. The author's values are correct. The rule
  said "from the build target OS" but did not say that Rust names macOS `macos` and DragonFly
  `dragonfly`, so the two renames are now explicit (fix 5).
- `host.arch` well-known list: `amd64`, `arm32`, `arm64`, `ia64`, `ppc32`, `ppc64`, `s390x`,
  `x86`. The author's mapping is correct.
- `gen_ai.usage.input_tokens`: the registry note says the value "SHOULD include all types of
  input tokens, including cached tokens". D3 is confirmed.
- `service.instance.id`: string ID of the service instance. The registry example is a UUID, so
  D5's UUID v4 per process fits.

## Author-chosen extras

- **D4 `mhc.provider.id`: kept.** Mapping `chatgpt-subscription` and `openai-codex` to
  `openai`, or `google` to `gcp.gemini`, loses which mhc provider (subscription or API key)
  served the call. That distinction exists in the ledger and `mhc usage`. One extra neutral
  attribute on the user's own collector carries it. It is not a SUL concept: SUL uses
  `omo.provider`, with masking, which the delta does not copy.
- **D5 precedence: kept.** It follows the base spec's resource rule (defaults ->
  `OTEL_RESOURCE_ATTRIBUTES` -> `OTEL_SERVICE_NAME` -> `service.version`/`process.pid`, as in
  `crates/maho-telemetry/src/settings.rs` `resource`). The user can override the new defaults,
  which matches the OpenTelemetry convention that user resource attributes win over detected
  ones. SUL has no override path, so nothing leaked.
- **D6 unreadable config blocks the push: kept, narrowed.** Failing closed is the right default
  for an opt-out check, because a missing view must not be read as "still allowed". As written,
  though, it could be read as "any broken config file stops export". config.md section 14
  already says a rejected reload keeps the previous view. The error case now applies only when
  the host returns no view at all (fix 7).

## Fixes applied (in this commit, telemetry.md delta only)

1. D2 test: host-name vector `test-host-7` -> `build-box-gamma` (upstream fixture variant, rule 5).
2. D2 test: the absence check naming `machine_hash`/`omo.machine_hash`/`*.install.hash` was
   replaced by an exact resource key-set assertion.
3. D4 behaviour sentence reworded to the OpenTelemetry MUST/MAY rule. This removes the
   closeness to the SUL doc comment.
4. D4 table: `google-gemini-cli`, `perplexity` and `cohere` rows removed (not mhc provider
   ids). `chatgpt-subscription` added to the `openai` row, and `openai-codex` marked legacy.
5. D5 `os.type`: the Rust target renames `macos` -> `darwin` and `dragonfly` -> `dragonflybsd`
   are now explicit.
6. Mapping row omo-telemetry-13: the reason now points to skills.md Delta D1/D2.
7. D6 error case: fail-closed is limited to "no view returned". A rejected reload keeps the
   previous view (config.md section 14).

No cargo command was run (docs only).

verdict: approved
