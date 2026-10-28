# MIT-licensed skills (relicensed from lazycodex)

These skill directories were replaced with the MIT-licensed copies published in
`code-yeongyu/lazycodex` @ `75b017473c71d669dd9aed0a10e950e70724b283`
(`plugins/omo/skills/<name>`). License text: `LICENSES/lazycodex-MIT.txt`
(Copyright (c) 2026 Yeongyu Kim). `ast-grep` also carries its own `LICENSE`
(upstream `code-yeongyu/ast-grep-skill`, MIT).

| Skill | Changes made in mhc |
| --- | --- |
| coding-agent-sessions | Removed Codex-only `agents/` role files and the "Codex Harness Tool Compatibility" section |
| data-scientist | same |
| debugging | same |
| lsp-setup | same |
| remove-ai-slops | same; restored a top-level heading |
| git-master | same; frontmatter `description` rewritten by mhc |
| ast-grep | same |

No content from the SUL-1.0 OmO upstream (the earlier skill overlay bytes,
`oh-my-openagent`) remains in these directories.

## Clean-room skills (2026-10-26)

Written by implementers who were given only a functional spec and the MIT base
(lazycodex @ 75b01747) and never read the SUL-1.0 versions. Each directory has a
`CLEANROOM.md` with inputs and a requirement checklist.

| Skill | Base | Notes |
| --- | --- | --- |
| browser | lazycodex MIT | engine guard rewritten (connected/builtin/none policy, ask-before-irreversible) |
| ultimate-browsing | lazycodex MIT | broadened routing scope |
| frontend | lazycodex MIT / Apache-2.0 parts | capability additions only; base refinements kept |
| init-deep | lazycodex MIT (matrix/templates) | full rewrite; state in `.mhc/init-deep/` |
| refactor, review-work, visual-qa, programming | lazycodex MIT | Codex-only parts removed, base features kept |
| dag-library, visualize, adversarial-plan | none | written from spec only; MIT (c) 2026 Indo Yoon. `adversarial-plan` replaces `hyperplan` |
| give-me-tips (2026-10-27) | none | written from spec only; MIT (c) 2026 Indo Yoon; `tests/static-check.mjs` |
| onboarding (2026-10-27) | none | written from spec only; MIT (c) 2026 Indo Yoon; ships `scripts/savings-estimate.mjs`, `scripts/init-deep-gate.mjs` and `tests/` (replacing `qa-validator.sh` / `qa-savings-fixture.sh`) |

## Removed

- `ultrawork`, `ulw-execute`, `ulw-loop`, `ulw-plan`, `ulw-research`, `mass-ulw` (2026-10-28):
  dropped with the OmO workflow features they drove.
- `hyperplan` (2026-10-26): replaced by `adversarial-plan`.

## License

Every skill in this directory is MIT. `manifest.json` lists each file with its sha256.
