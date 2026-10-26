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
| ultrawork | adapted to mhc's tool surface (`task`, `todo`, `eval`, `workflow`); Codex subagent/TOML routing sections replaced by an mhc delegation section; Codex-only `agents/` dropped |
| ulw-execute | same; Codex harness compatibility table replaced by mhc delegation notes |
| ulw-loop | same; driven through `mhc ulw-loop` (the `--codex-goal-json` flag keeps its historical name) |
| ulw-plan | same; planning delegated to `task` children |
| ulw-research | same; research lanes on `task`/teams |

The `ultrawork` and `ulw-*` rows were added on 2026-10-27 from the same
lazycodex commit; their manifest entries carry the source
`lazycodex@75b01747:plugins/omo/skills/<name>/<path> (MIT)`.

No content from the SUL-1.0 OmO upstream (`assets/omo-latest` previous bytes,
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

- `mass-ulw` (2026-10-27): dropped from the bundle and listed under `retired` in
  `manifest.json`; graph work goes through the `workflow` tool directly.
- `hyperplan` (2026-10-26): replaced by `adversarial-plan`.

## Still SUL-1.0

No skill under `assets/omo-latest/skills` is SUL-1.0 any more. Out of scope
for this file: the runtime crates under `crates/omo/**` (for example
`maho-omo-mass-ulw`, `maho-omo-ultrawork`, `maho-omo-skill-pointers`,
`maho-omo-onboarding`, `maho-omo-init-deep-advisor`) remain
`LicenseRef-SUL-1.0`, as does the pool-root developer doc
`assets/omo-latest/skills/AGENTS.md` (`poolDocs`), which is not a skill.
