# Ultrawork Engine Clean-Room Addendum

Scope: Scoped addendum to `docs/cleanroom/specs/README.md` governing the clean-room specification, leak review, and implementation of the ultrawork engine (`crates/maho-ultrawork`), its contract, and companion integration.

This addendum applies exclusively to the ultrawork unit (plan todos 3, 4, 5, 13, 14, 15, 16, 17) and its implementers. It overrides or clarifies exactly four clauses of `docs/cleanroom/specs/README.md` and keeps every other rule, prohibition, and checklist item from `README.md` strictly binding.

---

## Overridden and Clarified Clauses

### 1. In-Scope Features (Overrides README:87-88)
- **Reference in README.md**: Lines 87-88 stated:
  > "3. **General features only** (plan §0 and the 독창성 table). OmO-original features are not specified and must not be built: the magic-keyword maximum-orchestration mode, the mass fan-out mode, the persona prompt set, the plan-continuation state-file format."
- **Overriding Decision**: On 2026-11-01, the user determined that severe per-turn issues in the orchestration layer should be rebuilt clean-room under MIT ("심각한 문제 만 다시 mit 클린룸 재구현 하는게 나을거같은데").
- **Addendum Rule**: The magic-keyword invocation mode (`ulw`, `ultrawork`) and the mass fan-out mode (`mass ulw`, `mulw`, `ulw mass`) are explicitly in scope for clean-room specification and implementation in the MIT crate `crates/maho-ultrawork`. They are specified as clean-room behavioral contracts with zero proprietary code or prompt text. Persona prompt sets and plan-continuation state-file formats remain out of scope.

### 2. User-Facing Keyword and Crate Names (Clarifies README:89)
- **Reference in README.md**: Line 89 stated:
  > "4. **Neutral names.** New identifiers must not use OmO-specific names (`ultrawork`, `ulw`, `boulder`, `Sisyphus`, `Prometheus`, `Oracle`, `Librarian`, `Kibitzer`, `senpi`-prefixed type names, "OmO Native" in messages). Each spec proposes neutral names; the implementer may choose others as long as they stay neutral. Existing *on-disk* names (`~/.omo`, `omo.jsonc`, `OMO_PROFILE`) stay because users' files depend on them."
- **Overriding Decision**: User-facing keywords `ulw`, `ultrawork`, `mass ulw`, and the crate name `maho-ultrawork` are permitted. These names are already established in the MIT lazycodex skill `assets/omo-latest/skills/ultrawork/SKILL.md` (licensed under MIT, documented in `assets/omo-latest/MIT-SKILLS.md`).
- **Addendum Rule**: User-facing invocation strings (`ulw`, `ultrawork`, `mass ulw`, `mulw`), custom message type (`omo-ultrawork:directive`), reminder tags (`<omo-ultrawork-reminder>`), and the target crate name `crates/maho-ultrawork` are permitted as interface compatibility facts. However, all internal types, functions, structs, enums, fields, and error codes must remain strictly neutral and must not match any SUL internal identifier.

### 3. Allowed Skill References for Implementers (Clarifies README:63)
- **Reference in README.md**: Line 63 stated:
  > "- `assets/omo.schema.json` and `assets/omo-latest/**` (regenerate the schema from `config-keys.md` instead);"
- **Clarification**: Clean-room implementers may read skill directories under `assets/omo-latest/skills/` whose `ATTRIBUTION.md` does not name `oh-my-openagent`.
- **Addendum Rule**:
  - **Permitted skill directories**:
    - `adversarial-plan`
    - `ast-grep`
    - `browser`
    - `coding-agent-sessions`
    - `dag-library`
    - `data-scientist`
    - `debugging`
    - `frontend` (excluding `scripts/detect/**` and `references/perfection/detect.md`, which remain SUL-1.0)
    - `git-master`
    - `give-me-tips`
    - `init-deep`
    - `lsp-setup`
    - `onboarding`
    - `programming`
    - `refactor`
    - `remove-ai-slops`
    - `review-work`
    - `ultrawork`
    - `ulw-execute`
    - `ulw-loop`
    - `ulw-plan`
    - `visual-qa`
    - `visualize`
  - **Strictly forbidden inputs**:
    - `ultimate-browsing` (names `oh-my-openagent`)
    - `ulw-research` (names `oh-my-openagent`)
    - `assets/omo-latest/skills/AGENTS.md` (pool-root document; SUL-1.0)
    - `assets/omo-latest/manifest.json`
    - `assets/omo.schema.json`
    - `crates/omo/**` (all files and tests)
    - `.omo/**`

### 4. Paraphrase Boundary and Contract Requirement Rows (Clarifies README:105)
- **Reference in README.md**: Line 105 stated:
  > "- [ ] no sentence is a translation or paraphrase of a SUL comment, doc comment or prompt;"
- **Clarification**: The contract specification must communicate which behavioral rules the new contract requires, without copying or paraphrasing SUL prompt text.
- **Addendum Rule**: A requirement row in the contract specification is permitted if and only if it satisfies all of the following:
  1. It states an observable behavior and the concrete failure mode it prevents using the spec writer's own structure and vocabulary.
  2. It is organized by the spec's own topic categories, not following the sequence or outline of any SUL directive.
  3. It contains no sentence structure, list format, illustrative example, ordering, or rhetorical framing from any SUL document.
  4. It shares no sequence of 8 consecutive words with any SUL file (`ngram-overlap` count of 0).
  5. Where the behavior already exists in the MIT lazycodex reference (`assets/omo-latest/skills/ultrawork/SKILL.md`), the requirement row cites that MIT section rather than restating it.
  6. Any requirement row that reads as a line-by-line rewording or translation of a SUL sentence is prohibited as paraphrase. The leak reviewer audits compliance row-by-row.

---

## All Other Rules Remain Binding

Implementers and reviewers must continue to enforce all other principles of `docs/cleanroom/specs/README.md`:
- Behavior over expression (Rule 1).
- Exact reproduction of interface facts and config keys (Rule 2).
- Clean-room test authoring from Given/When/Then without reusing proprietary fixtures (Rule 5).
- Fresh message composition (Rule 6).
- Complete isolation of the implementer workspace, verified by environment manifests (`phase4-env.sha256` / `ulw-env.sha256`).
- Independent leak review prior to code implementation.
