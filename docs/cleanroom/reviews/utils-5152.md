# Leak-check record: utils delta (omo v5.1.52)

Scope: `## Delta (omo v5.1.52, 2026-11-01)` of docs/cleanroom/specs/utils.md (D1, D2), added in
spec commit cf9b447a0 on sync5152/spec; census row omo-cores-35. Reviewer: a separate session,
not the spec writer. This record is for reviewers only; implementers must not use it as input.

Compared against: upstream omo v5.1.52 `packages/utils/src/path-containment.ts` (diff base
0a401eb8c8); mhc MIT `crates/maho-support/src/paths.rs` (`canonicalize_lenient`,
`contains_path`, `is_within`) and `crates/maho-support/tests/support.rs`.

## Checklist (README rules 1-6)

- [x] No fenced code blocks.
- [x] No translated SUL comment, after fixes. Two passages paraphrased the upstream doc comment:
      - the "Windows compares ... long names case-insensitively; POSIX compares the canonical
        spelling ... a case alias ... counts as inside" bullet. This was also a behaviour error:
        neither §2.5 nor `paths.rs` has Windows case folding, so "as before" was false;
      - the "callers that open a lexically normalised path must ... raw-string openers must pass
        the raw string" paragraph.
      Both were rewritten as mhc's own contract.
- [x] No constant table.
- [x] No new identifier. The existing mhc names are kept.
- [x] No OmO-original feature.

## Behaviour check

- Core change: an existing path is canonicalised without lexical normalisation first, so a
  symlink is followed before a later `..`. This matches the upstream change and applies cleanly
  to `canonicalize_lenient`, which today runs `normalize_lexically` first.
- **Gap fixed:** the delta did not say how the non-existent-path fallback handles `..` once
  normalisation is gone. With today's ancestor walk, `T/root/new/../../x` (with `new` absent)
  has no file name at a `..` step, so the walk returns the raw path. That path starts with
  `T/root` component-wise, which would wrongly return "inside". The delta now says the walk runs
  on the un-normalised path, the deepest canonicalisable prefix is kept, and the non-existent
  remainder is then folded (`.` dropped, `..` pops). Tests 6 and 7 were added for this.
  Upstream has no fallback (it requires both paths to exist), so this is mhc's own behaviour.
- Tests 1-5 agree with the text, and tests 1 and 2 are the observable symlink case.

## Fixes made

1. Replaced the case-sensitivity bullet: no case folding is added, and the false "as before"
   claim about Windows is gone. This removes a SUL paraphrase.
2. Rewrote the caller-contract paragraph in own words.
3. Specified `..` handling in the non-existent remainder and added tests 6 and 7.

verdict: approved
