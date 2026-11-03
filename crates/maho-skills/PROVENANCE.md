# Provenance — maho-skills

- License: MIT.
- Method: **P + C**. The `skill` tool's path rules are ports of MIT oh-my-pi; the rest (bundled
  root, disabled filtering, `/skill` text, wiring) is written from the clean-room spec
  `docs/cleanroom/specs/skills.md`, which itself was written from MIT sources only.
- Origins (license verified as MIT before reading):
  - `can1357/oh-my-pi` @ `f59bbac3d56e8b485fef466388b1cb5be4b93123` (LICENSE: MIT):
    `docs/skills.md` (discovery/precedence model, `hide`/`disableModelInvocation`, `skill://`
    behaviour); `packages/coding-agent/src/internal-urls/skill-protocol.ts` (`findUrlSkill`,
    `skillTargetPath`: name before the first `/`, relative path inside the skill directory,
    traversal and containment checks) and `internal-urls/filesystem-resource.ts`
    (`validateRelativePath`) → `src/tool.rs` `run_skill_tool`/`read_relative`;
    `prompts/skills/user-invocation.md` (skill directory + `User:` arguments) → the `User:`
    suffix in `src/tool.rs`.
  - `code-yeongyu/senpi` @ `26acd9353` as already ported in this tree (MIT):
    `maho-core::skills` (`format_skills_for_prompt` listing format filtered by
    `src/prompt.rs`), `maho-agent::harness::skills::format_skill_invocation` (the
    `<skill name location>` wrapper reused by the tool), `maho-ext-api` `resources_discover`.
- Not ported: Skillshare registry, namespaced collision aliases, `skill://` URLs in `read`/`bash`,
  managed (auto-learn) skills, provider toggles (`enableClaudeUser`, ...).
- No SUL source (`crates/omo/**`, in particular `skill-pointers`, `skill-commands`,
  `bundled-skills`; the omo-gated `maho-cli` modules; `assets/omo-latest/**`; OmO/COMP sources)
  was opened.
- Tests were written from the spec; no upstream or OmO fixtures were reused.

## Unit skills-5152 (omo v5.1.52 delta)

- Method: **C** (clean room). Built from the `## Delta (omo v5.1.52, 2026-10-30)` section of
  `docs/cleanroom/specs/skills.md` at spec commit `da2b172b`; unit `skills-5152`.
- Allowed inputs (and the only ones used): that spec, this crate's existing MIT code, and the MIT
  `maho-ext-api` `ExtensionContext` (`logger`, `ui`, `has_ui`) API. No SUL, OmO or upstream
  source, checkout, cache or web page was opened.
- New: the validated override resolver (`RootResolver`, `validate_root`, once-per-process
  `NoticeLedger`) in `src/root.rs`, used by `/skill`, the `skill` tool and `resources_discover`
  in `src/extension.rs`. Environment variable names are interface facts from the spec.
- Tests (`tests/root_override.rs`) were written from the delta's D3 cases 1-5.
