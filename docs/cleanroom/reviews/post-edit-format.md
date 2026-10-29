# Leak-check record: post-edit-format

Spec-writer self-check and independent reviewer record for docs/cleanroom/specs/post-edit-format.md. Reviewer input only; not an implementer input.

## Leak self-check

- No fenced code from the SUL tree; the only command lines are formatter invocations
  (interface facts of external tools).
- Prose is the writer's own description of observed behavior; notice and warning sentences
  are left to the implementer (only the required facts and the `+a/-r` token are fixed).
- The marker table lists external tool conventions (file names, CLI flags); the tool-name set
  and patch header strings are interface facts.
- Proposed identifiers (`maho-format`, "format after edit") are neutral; no OmO names, no
  product prefix in messages.
- No OmO-original feature is specified.

## Review (leak check)

Reviewer session: separate from the spec writer; compared against
`crates/omo/components/maho-omo-formatter` (`formatter.rs`, tests) and
`crates/omo/components/maho-omo-post-mutation` (`post_mutation.rs`, tests).

- [x] no fenced code block is Rust, TypeScript or JS from the SUL tree (none at all);
- [x] no sentence is a translation or paraphrase of a SUL comment, doc comment or prompt;
- [x] no list or table mirrors a SUL constant table row-for-row except interface facts (mutation
      tool names, patch header strings, formatter marker files and command lines, config keys);
- [x] no proposed identifier is an OmO-original name;
- [x] OmO-original features are absent.

Fixes:
- §3 marker table: now explicitly marked as interface facts (external formatter conventions),
  since it otherwise follows the SUL detection order row for row.
- §3 Bun substitution: "could shadow Bun's built-in subcommand" paraphrased the SUL doc comment
  on the Bun resolver; replaced by a plain condition list.
- §6 formatted notice: the required tokens `auto-formatted` / `re-read` and "re-read it before
  exact-text edits" reproduced the SUL notice text; replaced by the facts the block carries and
  the `+a/-r` token only. The missing-formatter warning no longer prescribes an install
  suggestion (mirrored the SUL warning).
- §8 tests 4, 5, 6, 9, 10, 11, 15, 18, 20: inputs copied SUL test vectors (`a.ts`..`d.ts` patch,
  `src/../src/a.ts`, `./sub/./b.rs`, `[tool.black]`, 524288/2500, `a\nb\nc`, LSP 2/1,
  `format.ts`); replaced by new vectors covering the same rules. Leak self-check wording updated
  to match.
