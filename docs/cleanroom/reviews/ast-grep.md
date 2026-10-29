# Leak-check record: ast-grep

Spec-writer self-check and independent reviewer record for docs/cleanroom/specs/ast-grep.md. Reviewer input only; not an implementer input.

## Leak self-check

- [x] No fenced code blocks. Command lines are public ast-grep CLI usage.
- [x] Prose is in the writer's own words. Tool descriptions, preflight messages and error message
      texts from the SUL code are not reproduced. The preflight checks are described by category,
      not row by row.
- [x] Lists marked as interface facts: tool, server and parameter names, enum values, the language
      list, error codes, result field names, environment names. The `OMO_` environment names are
      only read for compatibility. The new names are neutral.
- [x] Proposed identifiers (`maho-ast-grep`, `MHC_AST_GREP_*`) are neutral.
- [x] No OmO-original features: this is a general structural-search tool.

## Review (leak check)

Reviewer: a separate session from the spec writer. Compared against `crates/omo/ast-grep-mcp`,
`crates/omo/components/maho-omo-ast-grep` and the resolver in `crates/omo/utils/src/ast_grep`
(comments, tool/parameter descriptions, hint messages, constant tables, tests).

- [x] no fenced code block is Rust, TypeScript or JS from the SUL tree
- [x] no sentence is a translation or paraphrase of a SUL comment, doc comment or prompt
- [x] no list or table mirrors a SUL constant table row-for-row except marked interface facts
- [x] no proposed identifier is an OmO-original name
- [x] OmO-original features are absent

Fixes:
- §2: replaced the Homebrew prefix list copied from the SUL constant table with a pointer to the
  Homebrew defaults; marked the managed-install path as an on-disk interface fact.
- §3: rewrote the `paths`, `rewrite` and `scan` descriptions, which echoed the SUL schema and
  tool-description strings ("no implicit '.'", "empty deletes the match", "config discovery is never used").
- §4: replaced the preflight list, which followed the SUL hint table in order with its example
  tokens, with grouped minimum requirements the implementer may extend.
- §5/§6: rewrote three sentences that paraphrased SUL error and warning messages (error-node parse
  failure, deadline before the second pass, narrowing warning).
- Tests 9, 10, 12, 13, 27: replaced SUL test fixtures (`console.log($MSG)`, `$$$ARGS`, `\w+`,
  `$name`, `def $F($$$):`, `typo`, `0`) with new values.
