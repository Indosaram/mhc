# Leak-check record: x-search

Spec-writer self-check and independent reviewer record for docs/cleanroom/specs/x-search.md. Reviewer input only; not an implementer input.

## Leak self-check

- [x] No fenced code from the SUL tree (none at all).
- [x] No sentence translates a SUL comment, doc comment, tool description, skill text or the
      prompt sent to xAI; those texts are to be newly written.
- [x] Tables list only interface facts: parameter keys/bounds, error codes, HTTP status mapping,
      xAI wire fields, server tool names, output line prefixes.
- [x] Proposed identifiers are neutral (`maho-x-search`, `MAHO_X_SEARCH_MODEL`); `OMO_X_SEARCH_MODEL`
      and `auth.json` are kept only as existing user-facing names.
- [x] No OmO-original feature is described.

## Review (leak check)

Reviewer session: separate from the spec writer; compared against
`crates/omo/components/maho-omo-x-search` (`auth.rs`, `client.rs`, `format.rs`, `index.rs`,
`params.rs`, `tool.rs`, `skill/SKILL.md`).

- [x] no fenced code block is Rust, TypeScript or JS from the SUL tree (none at all);
- [x] no sentence is a translation or paraphrase of a SUL comment, doc comment or prompt;
- [x] no list or table mirrors a SUL constant table row-for-row except interface facts
      (parameter keys/bounds, error codes, HTTP status mapping, xAI wire fields and server tool
      names, output line prefixes);
- [x] no proposed identifier is an OmO-original name;
- [x] OmO-original features are absent.

Fixes:
- §2 tool surface: the label quoted the SUL label string and the keyword list copied four of the
  SUL search keywords verbatim; replaced by "own label" and a findability requirement.
- §2 description: the fact list mirrored the SUL tool description sentence by sentence (date and
  handle hints, max 20, URLs plus server queries, credential); reduced to "searches X via xAI,
  needs a credential".
- §4.4 `input` message: the clause list paraphrased the SUL prompt template (keyword search only,
  one per line with URL/summary/handle/date, no commentary); now only the facts tests rely on
  (query, `since:`/`until:` operators, mode, `max_results`).
- §4.6 cancel row: wording echoed the SUL error message; neutralized.
- §5 empty-result line: "no matching posts" echoed the SUL empty-result string; now
  implementer's wording; the line prefixes are marked as interface facts.
- §6 skill: the topic list mirrored the SUL SKILL.md section structure (tool search, web-search
  fallback, date bounds, handle scoping, operators, split searches); replaced by a requirement to
  write new guidance without reusing its layout, rules or examples.
