# Clean-room record: programming

- Base: lazycodex `75b017473c71d669dd9aed0a10e950e70724b283`, `plugins/omo/skills/programming` (MIT), copied from `cleanroom/programming/base/`.
- Inputs: MIT base + `cleanroom/programming/spec.md` only. No SUL-1.0 text opened, read, grepped or diffed.
- Coordinator decision (spec §0): do NOT downgrade. P-1..P-24 and R-1/R-2 (Biome rollback, dropping JS triggers, dropping stricter tsconfig settings) were intentionally not applied. D-1 not replicated (base argument handling kept). C-1 no change.

## Changes

| Item | What |
|---|---|
| Codex parts | Spec notes none in this skill. One stray harness reference fixed: comment in `scripts/typescript/check-no-excuse-rules.ts` said `~/.codex/...`; now "a user-level skills folder". Only file changed. |

## Acceptance (adapted to the no-downgrade decision)

- `grep -rEi 'lazycodex|multi_agent|fork_context|\.codex'` -> empty.
- `bun run scripts/typescript/new-project.ts --help` OK; scaffolding `tsdemo` produced package.json, tsconfig.json, .oxlintrc.json, .oxfmtrc.json, lint/oxlint-plugin/*, src/index.ts, .gitignore. In it, oxlint (type-aware), oxfmt --check and tsc --noEmit all pass.
- `check-no-excuse-rules.ts` on `new-project.ts` + itself: 0 violations (D-1 guard). Python audit on `scripts/python`: 0 violations. Python/Rust/Go scaffolders `--help` OK (need `typer`); `*.sh` audits pass `bash -n`.
- `diff -rq base out`: only the one comment line differs.

## Left as-is (flag)

- Pre-existing base issue: the scaffold's `bun run check` ended with `bun test`, which exits 1 when there are no tests ("No tests found"). *(Integrator, 2026-10-27: fixed; the scaffold's `check` and `test` scripts now run `bun test --pass-with-no-tests`, verified with Bun 1.4.4.)*
- `scripts/typescript/oxc/lint/oxlint-plugin/require-suppression-reason.js` line 2 provenance comment names an intermediate repo (`sisyphuslabs/omo-relay`). Kept, because it is attribution for MIT-licensed third-party code, not product text; remove only if counsel agrees.
- `references/go/backend-stack.md` mentions "Codex APIs" describing a third-party proxy; incidental, kept. `references/rust-ub` "Miri is the oracle" is generic English.
