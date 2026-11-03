# Leak-check record: skills, delta omo v5.1.52

Scope: `## Delta (omo v5.1.52, 2026-10-30)` of docs/cleanroom/specs/skills.md (spec commit
dbf5abaa). Reviewer: a separate session, not the spec writer. Reviewer input only.

Compared against: crates/omo/components/maho-omo-bundled-skills, maho-omo-skill-commands;
upstream omo v5.1.52 `packages/omo-senpi/src/components/bundled-skills/{override,index}.ts`,
`skill-commands/index.ts`, `onboarding/component.ts`; MIT `crates/maho-skills/src/root.rs`,
`crates/maho-cli/src/cli/skill_library.rs`; census omo-components-01, -02 (and -03 for context).

## Checklist

- [x] No fenced code block from the SUL tree (no fenced blocks).
- [x] No sentence translates or paraphrases a SUL comment or message, **after the fixes below**.
      D1 "says an absolute readable directory is needed" tracked the SUL notice tail
      ("needs a readable absolute dir"); "accessible for reading and traversal" mirrored the
      `accessSync(value, 5) // R_OK | X_OK` line; both reworded to the requirement.
- [x] No table mirrors a SUL constant table. The OS error names (`ENOENT`, `EACCES`, `ENOTDIR`)
      are OS interface values.
- [x] No OmO-original identifier proposed. `MAHO_SKILLS_ROOT` is the existing MIT name;
      `OMO_BUNDLED_SKILLS_DIR` is kept only as the launcher's legacy env interface. The SUL names
      `resolveBundledSkillsOverride`, `getBuiltinSkillsRoot` do not appear.
- [x] OmO-original features absent (the onboarding tour is explicitly out of scope).

## Behaviour check against SUL

- D1 matches `override.ts`: valid only if absolute, readable/listable directory; invalid ->
  ignored with fallback, one log and one UI notice per process per distinct message; reason is
  `relative` or the OS error name; explicit option wins with no notice; unset/empty absent.
- The original D1 made `OMO_BUNDLED_SKILLS_DIR` a "lower-precedence alias" without saying what
  happens when `MAHO_SKILLS_ROOT` is invalid and the legacy one valid. Fixed: the legacy name is
  read only when `MAHO_SKILLS_ROOT` is unset/empty, only the one value read is validated; test 5
  added.
- D2 matches the skill-commands half of row 02 (the `/skill` command and the `skill` tool pass
  their notice context to the one resolver). MIT `maho-skills` today reads `MAHO_SKILLS_ROOT`
  with an `is_dir` check only (`root.rs`), so the delta is real work.

## N-A check

Rows 01 and 02 are APPLIES. Row 02's onboarding half is declared N-A here and in onboarding.md;
justified: MIT onboarding (onboarding.md §1, two steps `provider`, `model`) has no tour step and
references no bundled skill. Row 03 (task skill loader) is routed to `crates/maho-task`, not this
spec, so its absence here is correct. Coverage complete.

## Fixes applied to the delta

1. D1: legacy-variable precedence defined; requirement wording no longer tracks the SUL notice
   or access-mode comment.
2. D3: test 5 for the legacy variable.

verdict: approved
