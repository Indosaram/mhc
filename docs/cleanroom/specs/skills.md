# Skill loader — MIT profile (Phase 3, step 3)

Status: spec for `crates/maho-skills`. Written from MIT sources only: the senpi skill loader that
is already ported into `maho-core` (`skills.rs`, `skill_discovery.rs`, `resource_loader.rs`,
`package_manager.rs`, `skill_invocation.rs`), oh-my-pi @ f59bbac `docs/skills.md`,
`packages/coding-agent/src/internal-urls/skill-protocol.ts`,
`internal-urls/filesystem-resource.ts` (`validateRelativePath`),
`prompts/skills/user-invocation.md`, and the public Agent Skills format (`SKILL.md` with YAML
front matter `name`, `description`). No SUL source was used; the OmO skill-pointer/skill-command
components are not described here and were not read.

## 1. What already exists (reused, not re-implemented)

The MIT profile already links the senpi port in `maho-core`, which provides, for every build:

- Skill discovery: `SKILL.md` directories and loose `.md` files under the user skill root
  (`<agent dir>/skills`), `~/.agents/skills`, and for a trusted project the project skill root
  (`.omo/skills`) and every ancestor `.agents/skills` up to the repository root (plus the legacy
  `.pi/skills`), settings `skills` paths, installed packages,
  `--skill <path>` (repeatable) and `--no-skills`.
- Front matter parsing and validation (`name` ≤ 64 chars of `a-z0-9-`, no leading/trailing or
  doubled hyphen; `description` required, ≤ 1024 chars; `disable-model-invocation`), diagnostics
  for invalid skills and name collisions (first loaded wins).
- The prompt listing (`<available_skills>` with name, description and location; skills with
  `disable-model-invocation` are omitted) and the `/skill:<name> [args]` expansion in user input.
- Extension-contributed skill paths through the `resources_discover` event.

`maho-skills` adds only what the MIT profile lacks after the omo glue was compiled out.

## 2. Bundled skill root

- The bundled root is, in order: the `MAHO_SKILLS_ROOT` environment variable when it is set,
  non-empty and an existing directory; else the `skills` directory beside the running executable
  when it exists; else none.
- When a bundled root exists, `maho-skills` contributes it from `resources_discover` as one skill
  path. It therefore has the lowest precedence: a user, project, settings or `--skill` skill with
  the same name wins (the loader's collision rule), and the bundled copy is reported as a
  collision diagnostic.
- `--no-skills` keeps its meaning (the loader drops discovered paths); `maho-skills` does not
  override it.
- The app server's `skills/list` includes the bundled root as an extra path in the MIT profile.
- Staging the MIT skill set beside the binary is packaging work (Phase 4); the loader works with
  any directory laid out as `<root>/<name>/SKILL.md`.

## 3. Disabled skills

- The denylist is `maho_config::collect_disabled_skills(layers, native, profile)` over the
  layered `omo.jsonc` files for the session cwd (config.md §9: union across layers, trimmed,
  first-seen order). It is read when the extension registers and again at each
  `before_agent_start` (so a hot-reloaded config applies on the next turn).
- A disabled skill:
  - is removed from the system prompt's `<available_skills>` listing (the whole `<skill>` entry
    whose `<name>` matches; the rest of the prompt is unchanged byte for byte; when no entry is
    left, the listing block stays as the loader wrote it minus the entries);
  - cannot be loaded with the `skill` tool (error result naming it as disabled);
  - shows as `disabled` in `/skill`.
- `/skill:<name>` typed by the user is not blocked (an explicit user request).

## 4. `skill` tool

Name `skill`, label `Skill`. Parameters:

| field | type | meaning |
| --- | --- | --- |
| `name` | string, required | skill name, optionally followed by `/<relative path>` inside the skill directory |
| `arguments` | string, optional | extra instructions appended after the skill body |

Behaviour:

1. The session's loaded skills are read live from the extension context (the same list the
   prompt uses). Lookup is by exact name on the part before the first `/`.
2. Unknown name → error result `Unknown skill: <name>` plus `Available: a, b, c` (or `none`),
   listing only skills that are neither disabled nor `disable-model-invocation`.
3. Disabled (§3) → error result `Skill "<name>" is disabled by configuration (disabled_skills).`
   `disable-model-invocation` → error result saying the skill can only be invoked by the user.
4. With only a name: the result is the skill file with its front matter removed, wrapped as

   ```
   <skill name="NAME" location="/abs/path/SKILL.md">
   References are relative to /abs/skill/dir.

   BODY
   </skill>
   ```

   followed, when `arguments` is non-empty, by a blank line and `User: ARGUMENTS`. (The wrapper is
   the format the senpi port already uses for skill invocations, so the model sees one format.)
5. With `name/<path>`: the path must be relative and contain no `..` segment (error
   `Path traversal (..) is not allowed in skill paths` / `Absolute paths are not allowed in skill
   paths`); after canonicalisation it must stay inside the skill directory (error
   `Path resolves outside the skill directory`). A file is returned as plain text with a first
   line `[<skill>/<path>]`; a directory returns a sorted listing (directories with a trailing
   `/`); a missing path is an error. Files over 256 KiB are refused with an error naming the size.
6. Details carry `{name, path, baseDir}`.
7. The tool definition's prompt guideline says: load a skill with the `skill` tool whenever its
   description matches the task; its relative paths resolve against the skill directory.

## 5. `/skill` command

- `/skill` (no argument): lists every loaded skill, one line each: name, scope (`user`,
  `project`, `path`, `bundled` when its file lies under the bundled root, or the source string),
  status flags (`disabled`, `user-only` for `disable-model-invocation`), then the location. When
  there are none: `No skills loaded.`
- `/skill <name>`: shows that skill's description, location, directory, status, and how to run it
  (`/skill:<name> [instructions]`). Unknown names list the available ones.
- Output goes through the UI notification channel (info level).

## 6. Mounting

- `maho-cli` mounts the extension as `<builtin:skills>` in the MIT profile only, after the other
  MIT extensions. The default (`omo`) build links the crate but does not mount it.

## 7. Tests (written from this spec)

1. Bundled root: env var wins over the executable sibling; a missing/empty value falls through;
   no directory → none.
2. `resources_discover` returns the bundled root once, nothing without one.
3. Disabled filtering removes exactly the named `<skill>` entries from a prompt produced by
   `maho_core::skills::format_skills_for_prompt` and keeps every other byte.
4. `skill` tool: loads a body without front matter in the wrapper; appends `User:` arguments;
   unknown/disabled/user-only errors; relative file read; directory listing; `..`, absolute and
   symlink-escape paths refused.
5. `/skill` listing and detail text.
6. `collect_disabled_skills` integration: user + project `omo.jsonc` union.
