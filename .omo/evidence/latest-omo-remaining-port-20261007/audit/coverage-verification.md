# Coverage verification

Result: **PASS**

This is an independent, read-only comparison of the four assigned reports with the contract files under `/home/projects/mhc/.omo/evidence/latest-omo-implementation-20261006/contracts`. JSON was parsed from the exact absolute paths with `Bun.file(...).json()`. The report populations were recomputed rather than trusted from their self-reported counters.

## Pass criteria

The audit passes only when every value below is zero:

| Check | Result |
| --- | ---: |
| Uncovered rows | 0 |
| Duplicate rows | 0 |
| Invalid dispositions | 0 |
| Evidence-shape failures | 0 |
| Spot-check failures | 0 |

## Domain results

| Domain | Source population | Report population | Uncovered | Duplicates | Invalid dispositions | Evidence-shape failures | Spot checks |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| interfaces | 1,222 owner-registry rows | 879 groups, 1,222 expanded rows | 0 | 0 | 0 | 0 | 10/10 |
| memory-config | 504 canonical report targets | 504 records | 0 | 0 | 0 | 0 | 10/10 |
| runtime | 19 unique rows, 23 expanded bindings | 7 units, 19 unique rows | 0 | 0 | 0 | 0 | 7/7 |
| shared-core | 111 canonical report rows | 111 rows | 0 | 0 | 0 | 0 | 10/10 |

## Normalization rules

* Interfaces uses the contract owner registry, 745 A rows plus 477 U rows. Each report group expands through `row_ids`. The expansion is exactly 1,222 unique rows.
* Memory-config assigns the 504 report records as its canonical target population. Some records share a shortened source identifier, but they are separate contract records and are checked by their full record and `contract_row` fields. No record is uncovered or duplicated under this target definition.
* Runtime has 23 expanded unit bindings and 19 unique contract row IDs. `md:runtime/owned-file-inventory.md:P507` and `P593` are intentionally shared by RU-03a, RU-03b, and RU-03c. Shared ownership is normalized once for coverage and isn't treated as a duplicate.
* Shared-core audits the report's 111 canonical rows. The seven contract units expand to 33 unit references, while the report provides the row-level population and canonical IDs for those rows.

## Allowed dispositions

Every report uses only this allowed vocabulary:

`complete`, `partial`, `missing`, `explicit_exclusion`, `deferred_by_owner`, `verification_only`

The recomputed disposition counts are:

| Domain | Counts |
| --- | --- |
| interfaces | verification_only 22, explicit_exclusion 188, missing 153, deferred_by_owner 516 |
| memory-config | explicit_exclusion 352, partial 132, missing 18, deferred_by_owner 2 |
| runtime | complete 1, partial 3, missing 1, verification_only 2 |
| shared-core | deferred_by_owner 5, verification_only 100, explicit_exclusion 4, partial 2 |

## Evidence shape checks

The first ten records in file order for each domain were checked against the fields required by that report's schema. Interfaces records had row IDs, matching contract rows, a valid disposition, and rationale. Memory-config records had an ID, contract row, valid disposition, rationale, current path proof, and citations. Runtime units had contract rows and evidence with disposition, current, upstream, and rationale. Shared-core rows had matching provenance IDs, valid dispositions, rationale, verification proof state, current evidence, and upstream evidence. All checks passed.

Spot-check IDs:

* Interfaces: `interfaces-0001` through `interfaces-0010`, 10/10 passed.
* Memory-config: `inv:track:B`, `mc-opt:config:L21` through `mc-opt:config:L29`, 10/10 passed.
* Runtime: `RU-01`, `RU-02`, `RU-03a`, `RU-03b`, `RU-03c`, `RU-04`, `RU-05`, 7/7 passed.
* Shared-core: `sc-gap:claude-code-compat-core:0`, `sc-gap:comment-checker-core:0`, `sc-gap:comment-checker-core:1`, `sc-gap:comment-checker-core:2`, `sc-gap:git-bash-mcp:0`, `sc-gap:hashline-core:0`, `sc-gap:mcp-client-core:0`, `sc-gap:mcp-client-core:1`, `sc-gap:mcp-client-core:2`, `sc-gap:rules-engine:0`, 10/10 passed.

The machine-readable companion, `coverage-verification.json`, contains the same counts, IDs, normalization rules, and final result.
