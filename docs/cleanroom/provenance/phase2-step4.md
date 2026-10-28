# Phase 2 step 4 — computer use / browser (implementer notes)

- Worktree: `/workspace/work/mhc-clean` (branch `cleanroom/phase1`). Before starting, the branch
  was brought to `mit/skills-relicense` @ 440b1f4f (the step 3 commits 225310d7 and 440b1f4f were
  on the remote but not in the local branch; the worktree already held exactly their changes, so
  the branch was moved there without content changes). The SUL paths listed in `phase1-env.txt`
  (skip-worktree in this checkout) were not opened; neither were `maho-omo-browser-bridge`,
  `maho-omo-computer-use`, the omo-gated `maho-cli` modules, `assets/omo*`, `.omo/` or any
  OmO/COMP source.
- Sources read (MIT; each LICENSE checked first; checkouts in `/workspace/work/upstream`):
  - `code-yeongyu/omowright` @ a019fdfbc337bd917d10b8f3d64026b3549e0b2c (LICENSE MIT, Copyright
    2026 Yeongyu Kim): `README.md`, `package.json`, `src/{pipe,connection,chrome,cua,
    dialog-policy}.js`, `src/core.js` (parts: CDP client, session manager, keyboard layout, locator
    and actions), `src/page-bundle.js` (vendored).
  - `code-yeongyu/pi-cua-integration` @ 9ba0c9e23f13a7fc834c28c65e8733b40a162163 (LICENSE MIT,
    NOTICE): `README.md`, `src/cua/{protocol,client,daemon}.ts`, `src/tools/{click,key,screenshot,
    result}.ts`, `python/daemon.py` (vendored).
  - `can1357/oh-my-pi` @ f59bbac3d56e8b485fef466388b1cb5be4b93123 (root LICENSE MIT; already
    pinned): `packages/coding-agent/src/tools/browser.ts` (schema, kind resolution),
    `tools/computer.ts` (schema, approval), `tools/browser/launch.ts` (candidates, launch args).
- Spec step (separate commit dfea3509, before the code): `docs/cleanroom/specs/browser-tools.md`
  fixes the discrete-action tool surface, which no MIT original has (oh-my-pi's tools run
  model-written JavaScript over puppeteer; omowright is a library; pi-cua-integration covers only the
  desktop). It was written from the MIT sources above only.
- MIT call sites read in the tree: `crates/maho-ext-api/src/types.rs` (events, tools),
  `crates/maho-tools/src/definition.rs` (tool definition, content, errors),
  `crates/maho-memory/{Cargo.toml,PROVENANCE.md,src/extension.rs}` and `crates/maho-lsp` (extension
  patterns), `crates/maho-cli/src/cli/{default_extensions,long_term_memory,mod}.rs`,
  `crates/maho-cli/Cargo.toml`, `scripts/{check-mit-profile,mit-profile-smoke}.py`,
  `docs/MIT-PROFILE.md`, `PINS.md`, `Cargo.toml` (workspace lints: `unsafe_code = "forbid"`).
- Naming: neutral names only. The crate is `maho-browser`, the tools `browser` and `computer`, the
  page global `__mahoPage`, the `maho-cli` module `cli/browser_automation.rs`, the mount
  `<builtin:browser>`. The upstream product names appear only in provenance and notices.
- Local test browser: Playwright's `chromium-headless-shell` r1200 for linux-arm64 (Chromium 143),
  with four Debian trixie libraries extracted beside it (libatk, libatspi, libXcomposite,
  libXdamage), wrapped by `/workspace/work/browsers/chrome`. Used through `MAHO_BROWSER_TEST_PATH`
  (crate tests) and `MHC_SMOKE_BROWSER` (smoke). Not committed.
- Results: `crates/maho-browser` (P + C, see its PROVENANCE.md, LICENSE-THIRD-PARTY.md and
  `parity.d/maho-browser.md`); `crates/maho-cli/src/cli/browser_automation.rs` (MIT glue:
  `<builtin:browser>` in the MIT profile only). The default (`omo`) build links the crate but does
  not mount it, so its behaviour is unchanged.
