# Provenance - maho-x-search

- License: MIT.
- Method: **C** (clean room). Built from `docs/cleanroom/specs/x-search.md` at spec commit
  `8f3d2e29` (the spec set's README `docs/cleanroom/specs/README.md` and, for the skill
  precedence and `disabled_skills` rules it references, `docs/cleanroom/specs/skills.md`).
- Allowed inputs read in this session:
  - the spec files named above;
  - MIT code already in this checkout, used only for the host contracts this crate plugs into:
    `crates/maho-ext-api` (extension, tool, event and model-registry types),
    `crates/maho-tools/src/definition.rs` (tool definition fields), `crates/maho-ai`
    (`utils/abort.rs`, `models.rs` auth types, `auth/types.rs`), `crates/maho-config`
    (`collect_disabled_skills`, `load_native`), `crates/maho-core` (`skills.rs`,
    `resource_loader.rs` skill loading and collision rule), `crates/builtins/maho-ext-imagegen`
    and `crates/builtins/maho-ext-tool-search` (deferred-tool registration and tool-search
    ranking), `crates/maho-skills`, `crates/maho-gateway`, `crates/maho-mcp-builtins`
    (registration and offline conventions), `crates/maho-cli/src/cli/default_extensions.rs`
    (how MIT-profile extensions are mounted), `AGENTS.md`;
  - xAI's public Responses API wire format as stated in the spec.
- No forbidden input was read: nothing under `crates/omo/**` (absent from this checkout),
  `assets/omo-latest/**`, `assets/omo.schema.json`, `.omo/**`, the OmO-derived `maho-cli`
  modules listed in the spec README, any oh-my-openagent / OmO / lazycodex source or URL, the git
  history of removed paths, or another machine's copy of this repository. The SUL component this
  replaces (`maho-omo-x-search`) and its skill were not opened.
- The tool description, prompt wording, error messages, skill text, schema descriptions and
  search keywords are new. Interface facts (tool name `x_search`, group `x-search`, error codes,
  output line prefixes, env names, endpoint, request fields, default model) come from the spec.
- Tests are new, written from the spec's numbered acceptance tests (§8); no upstream or OmO
  fixtures were reused.
