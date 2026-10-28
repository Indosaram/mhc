# Leak-check record: git-bash-mcp

Spec-writer self-check and independent reviewer record for docs/cleanroom/specs/git-bash-mcp.md. Reviewer input only; not an implementer input.

## Leak self-check

- No fenced code blocks; config/env names, enum strings, paths, numbers and the winget command
  are interface facts that users or clients depend on.
- Tool descriptions, schema descriptions and error/hint sentences of the SUL crate are not
  reproduced; §4/§5 list only the facts a message must carry.
- No private type or function names; proposed names (`maho-git-bash`, `mhc-git-bash`,
  `MHC_GIT_BASH_PATH`) are neutral. The `OMO_*`/`CODEX_*` variable names are kept as existing
  user-facing interface facts (README rule 2/4).
- No OmO-original feature is specified.

## Review (leak check)

Reviewer session: separate from the spec writer; compared against `crates/omo/git-bash-mcp`
(`cli.rs`, `mcp.rs`, `runner.rs`) and the resolver it re-exports from `crates/omo/utils`.

- [x] no fenced code block is Rust, TypeScript or JS from the SUL tree (none at all);
- [x] no sentence is a translation or paraphrase of a SUL comment, doc comment or prompt;
- [x] no list or table mirrors a SUL constant table row-for-row except interface facts (tool
      names, JSON result keys, source enum values, env names, Windows install paths, error codes);
- [x] no proposed identifier is an OmO-original name;
- [x] OmO-original features are absent.

Fixes:
- §4 `installHint`: the three listed statements followed the SUL hint's three lines one by one;
  rewritten as a neutral list of facts the hint carries (no bash found, winget command, env name
  and value meaning).
- §5 `diagnose` status: the non-Windows status was described by paraphrasing the SUL status
  sentence; now only "first word `disabled` plus a reason in the implementer's words".
- §5.3 output capture: the parenthetical rationale paraphrased the SUL runner module comment
  (pipe-buffer deadlock); replaced by the observable requirement (large output returned whole,
  child never stalls).
