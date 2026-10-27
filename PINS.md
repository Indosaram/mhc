# Source pins

These revisions are the behavior of record. Ports, parity ledgers and golden fixtures refer to these exact commits; a pin moves only by an explicit sync change.

| Source | Short | Full commit | Location |
| --- | --- | --- | --- |
| senpi | 26acd9353 | 26acd935303869fd624f203dd8beb41f66ca4e5d | code-yeongyu/senpi |
| oh-my-openagent | 7d818abaf9 | 7d818abaf97831537271b4af9629a5264649de37 | code-yeongyu/oh-my-openagent |
| oh-my-pi | f59bbac3d | f59bbac3d56e8b485fef466388b1cb5be4b93123 | can1357/oh-my-pi (MIT; LSP mux design in crates/maho-lsp, task model in crates/maho-task, mnemopi memory engine in crates/maho-memory, Chromium discovery and browser/computer tool surface in crates/maho-browser, telemetry export and usage stats in crates/maho-telemetry, pi-iso rcopy backend and task worktree isolation in crates/maho-isolation, agentic-commit git tools in crates/maho-git) |
| omowright | a019fdf | a019fdfbc337bd917d10b8f3d64026b3549e0b2c | code-yeongyu/omowright (MIT; CDP transport, page model and snapshot world in crates/maho-browser) |
| pi-cua-integration | 9ba0c9e | 9ba0c9e23f13a7fc834c28c65e8733b40a162163 | code-yeongyu/pi-cua-integration (MIT; desktop computer-use daemon in crates/maho-browser) |
| omo-components (COMP) | 93b78ac | 93b78ac1cc8d488d124da7ed1d19f678e93fb0fd | (private checkout) |

## Pin roles

- `PINS.md` senpi row: latest upstream commit whose behavior has been ported (parity baseline).
- `tools/golden/pin.mjs` `SENPI_PIN`: commit the golden fixtures were generated from.
- `.github/workflows/ci.yml` `SENPI_PIN`: must equal pin.mjs (fixtures checkout).

## User extensions

Git extensions live under `~/.omo/agent/git/github.com/<owner>/<name>`.

| Extension | Short | Full commit | License |
| --- | --- | --- | --- |
| code-yeongyu/pi-webfetch | 8a4743d | 8a4743da6a1f32a6a53f4ddae9484c32c78adcd5 | MIT |
| code-yeongyu/pi-nested-agents-md | 998d31f | 998d31fcc0385e526132f05c2fa22a47ca3d8a21 | MIT |
| code-yeongyu/pi-websearch | ddf5f5d | ddf5f5d21de57ee3e80f1a6f96aff21a9dd34662 | MIT |
| code-yeongyu/pi-goal | 6937ce1 | 6937ce1d9ae2f9c3da11041cdf953ebe01303668 | MIT |
| code-yeongyu/pi-ast-grep | 4a7d1be | 4a7d1beee684d96a6890e5fc55710bb63fecca85 | MIT |
| code-yeongyu/pi-comment-checker | 0a38dd8 | 0a38dd8ff362be1b6020f2baba7b5723cbc5ea76 | MIT |
| code-yeongyu/pi-lsp-client | fc8f612 | fc8f6124d0022ba8e6ed4e0678007c42d0f5e74e | MIT (ported into crates/maho-lsp) |
| code-yeongyu/go-claude-code-comment-checker | 311565f | 311565fc9380579449ae34a8ab4c2cd386da840c | MIT (binary lookup in crates/maho-comment-checker) |
| code-yeongyu/pi-rules | 12ad906 | 12ad906f0b29e949ebbd1f89d8f85789578aa6e6 | MIT |
| DevNewbie1826/omo-zcode-oauth | a6f4c27 | a6f4c27a830abb6da65382898b805fa7a8ae306f | MIT (keep its notice) |

The local extensions in `~/.omo/agent/extensions` (orca-agent-status, orca-prefill, orca-titlebar-spinner, herdr-agent-state, ferryx-agent-state, quiet-model-profile) are unversioned files; their ports record a content hash in their parity fragment.
