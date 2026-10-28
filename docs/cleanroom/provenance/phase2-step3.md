# Phase 2 step 3 — long-term memory (implementer notes)

- Worktree: `/workspace/work/mhc-clean` (branch `cleanroom/phase1`, fast-forwarded to
  `mit/skills-relicense` @ 9f140029 before starting; already up to date). The SUL paths listed in
  `phase1-env.txt` were not opened; neither were the omo-gated `maho-cli` modules (`omo_mount`,
  `omo_shipped`, `memory_runtime`, `recall_wiring`, `kibitzer_*`, `task_runners`, `task_session`,
  `kernel_tools_host`, `host_transport`, `facts_child`, `doctor.rs`), the omo fixtures in
  `crates/maho-cli/tests/`, `.omo/`, `assets/omo*` or any OmO/COMP source.
- Sources read (MIT, LICENSE checked first; checkout in `/workspace/work/upstream/oh-my-pi`):
  `can1357/oh-my-pi` @ f59bbac3d56e8b485fef466388b1cb5be4b93123 (root LICENSE MIT;
  `packages/mnemopi/package.json` `"license": "MIT"`) —
  `packages/mnemopi/README.md`, `src/{mcp-tools,mcp-server,config,db,types}.ts` (parts),
  `src/core/beam/{schema,store,recall}.ts` (parts), `src/core/{synonyms,banks,triples,memory}.ts`
  (parts), `src/util/{ids,regex,datetime}.ts` (parts);
  `packages/coding-agent/src/mnemopi/{config,state,backend}.ts` (bank scoping, recall block,
  first-turn recall), `src/memory-backend/{types,tool-names,recall-entry}.ts`,
  `src/tools/memory-{recall,retain}.ts`, `src/prompts/tools/{recall,retain}.md`,
  `src/prompts/system/mnemopi-instructions.md`.
  `code-yeongyu/senpi` @ 26acd935 (MIT) was checked out to look for a memory implementation; it has
  none (only process memory reports), so nothing from it was used.
- Spec step (separate commit, before any code): `docs/cleanroom/specs/memory-format.md` describes
  the legacy Markdown memory store from user docs and MIT call sites only (`docs/configuration.md`,
  `AGENTS.md`, `docs/divergences.md` SD-7, `README*.md`, `docs/cleanroom/specs/config-keys.md`
  § memory, the MIT `memory_tips.rs` catalog). No legacy memory data existed in the sandbox
  (`~/.omo`, `MAHO_MEMORY_HOME` absent), so nothing was observed from real files; the spec lists
  its open points.
- MIT call sites read in the tree: `crates/maho-cli/src/{main.rs (entry dispatch),
  cli/{default_extensions,delegation,doctor_mit,mod}.rs}`, `crates/maho-ext-api/src/types.rs`
  (events, tools, commands), `crates/maho-tools/src/definition.rs`, `crates/maho-task/src/
  {extension,lib}.rs`, `crates/maho-config/src/{load,env}.rs`, `crates/maho-core/src/
  {config,output_guard}.rs`, `crates/builtins/maho-ext-bash-timeout/src/lib.rs`
  (`BeforeAgentStart` pattern), `scripts/mit-profile-smoke.py`, `docs/MIT-PROFILE.md`, `PINS.md`.
- Naming: the OmO recall sidecar name is not used; the MIT feature is "auto-recall" (first-turn
  recall, as in oh-my-pi). The new `maho-cli` module is `cli/long_term_memory.rs`.
- Results: `crates/maho-memory` (P + C, see its PROVENANCE.md and `parity.d/maho-memory.md`),
  `crates/maho-cli/src/cli/long_term_memory.rs` (MIT glue: `<builtin:memory>`, `mhc memory`,
  `--memory-mcp` / `--omo-memory-mcp`). `--memory-supervisor` / `--child-bootstrap` (omo reflection
  workers) stay unavailable in the MIT profile: they have no MIT original and nothing in the MIT
  profile needs them.
