# maho-skills and maho-mcp-builtins (MIT migration Phase 3, step 3: skill loader and builtin MCPs)

| Source (MIT) | Commit | Rust |
|---|---|---|
| can1357/oh-my-pi `packages/coding-agent/src/internal-urls/skill-protocol.ts` (`findUrlSkill`, `skillTargetPath`) | f59bbac | crates/maho-skills/src/tool.rs (`run_skill_tool`, `read_relative`) |
| oh-my-pi `internal-urls/filesystem-resource.ts` (`validateRelativePath`) | f59bbac | crates/maho-skills/src/tool.rs (`check_relative`) |
| oh-my-pi `prompts/skills/user-invocation.md` | f59bbac | crates/maho-skills/src/tool.rs (`User:` suffix) |
| oh-my-pi `docs/skills.md` | f59bbac | docs/cleanroom/specs/skills.md (behaviour reference) |
| senpi skill loader (already ported: `maho-core::skills`, `skill_discovery`, `resource_loader`, `skill_invocation`) | 26acd9353 | reused unchanged; `crates/maho-skills/src/prompt.rs` filters its listing |
| senpi MCP client (already ported: `crates/builtins/maho-ext-mcp`) | 26acd9353 | reused unchanged; `crates/maho-mcp-builtins` declares servers on it |
| clean-room spec docs/cleanroom/specs/skills.md, mcp.md | f763b127 | bundled root, disabled filtering, `/skill`, builtin servers and their on/off rules |

Differences from the originals:

- The skill tool is a separate `skill` tool rather than `skill://` URLs in `read`/`bash`; the path
  rules are the same (name before the first `/`, no absolute paths, no `..`, canonical target
  inside the skill directory).
- Collisions keep the senpi loader's rule (first loaded wins, diagnostic) instead of oh-my-pi's
  namespaced aliases.
- Builtin MCP servers are plain extension declarations: lazy, auto exposure, no OAuth.
