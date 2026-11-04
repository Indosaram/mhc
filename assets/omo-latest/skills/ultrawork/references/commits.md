# Git Commit Protocol and Message Conventions

This reference defines authorization boundaries, message formatting standards, and historical alignment rules governing git commits in ultrawork mode.

---

## 1. Commit Authorization Boundary

Automated or unrequested git commits are strictly prohibited:
- **Explicit User Authorization**: Git commits may only be authored when the user has explicitly requested a commit or granted unambiguous permission to create commits in the current task. In the absence of direct authorization, all modifications remain staged or unstaged in the working tree.
- **Atomic Verified Units**: When commits are authorized, each commit must represent a single, self-contained unit of work that leaves the repository in a fully compiling, green-test state. Intermediate half-broken commits are forbidden.

---

## 2. Conventional Commit Standards

Authorized commit messages follow the Conventional Commits specification, formatted to match the target repository's historical conventions:

```
<type>(<scope>): <short imperative summary>

[optional longer body explaining context, motivation, and trade-offs]

[optional footer(s) for issue references or breaking change notices]
```

### Type Tags
- `feat`: Introduces new user-visible functionality, public API capabilities, or CLI flags.
- `fix`: Resolves a bug, defect, or unexpected runtime failure.
- `refactor`: Restructures existing code without altering external behavior, APIs, or fixing bugs.
- `test`: Adds, updates, or reorganizes automated test suites without modifying product logic.
- `docs`: Updates user documentation, contract specifications, or internal architecture notes.
- `chore`: Modifies build scripts, package manifests, tool configurations, or auxiliary files.

### Scope Guidelines
- Scopes identify the specific crate, module, package, or subsystem affected by the change (e.g., `feat(ultrawork): ...`, `fix(task): ...`).
- If a change spans multiple subsystems equally, omit the scope or select the primary entry point according to project history.

### Subject Line Rules
- Use the imperative present tense: "add support for X", "fix memory leak in Y", not "added" or "adds".
- Keep the summary line concise (72 characters or fewer).
- Do not capitalize the first letter following the colon unless it is a proper noun or code identifier.
- Do not append a trailing period to the subject line.

---

## 3. Body and Footer Conventions

When a change warrants deeper explanation:
- **Context and Motivation**: Explain why the change was necessary and what operational failure mode it resolves.
- **Architectural Seams**: Note non-obvious design decisions, migration paths, or performance considerations.
- **Metadata Footers**: Reference related issue numbers or tracking tickets (e.g., `Closes: #123`, `Fixes: ISSUE-456`) and declare breaking changes with `BREAKING CHANGE:` headers where applicable.
