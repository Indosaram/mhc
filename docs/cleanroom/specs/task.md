# Delegated tasks — behavior spec (Phase 2, step 2)

Scope: the parts of `crates/maho-task` that have **no MIT original** to port. The task model
itself (agent definitions, ids, concurrency, result envelope, background delivery, follow-up runs)
is ported from MIT oh-my-pi (`parity.d/maho-task.md`). This spec was written only from public
documentation, the MIT oh-my-pi sources, the mhc CLI help (`crates/maho-cli/src/cli/help.txt`),
`docs/cleanroom/specs/config-keys.md` (`task`, `agents`, `categories`) and
`docs/cleanroom/specs/model-selection.md` §5. No OmO/SUL source was read.

## 1. Why a process runner

oh-my-pi runs subagents as in-process sessions. mhc's MIT extension API exposes no host call that
creates a nested agent session, so a child runs as a separate non-interactive mhc process. This
keeps the child's tools, extensions (LSP, rules, ...), provider logins and configuration identical
to a normal mhc run, and lets a background child outlive the parent process.

## 2. State directory

- Root: `task.state_dir` when set (relative to the session cwd), else
  `<agent dir>/tasks/<parent session id>` (session id characters outside `[A-Za-z0-9_-]` become `_`).
- Per task `<id>`: `<id>.json` (record), `<id>.md` (final answer of the last run),
  `<id>.log` (stderr of the last run), `<id>.prompt.md` (agent prompt + category `prompt_append`).
- `sessions/`: the children's session files (`--session-dir`).
- Record fields: id, agent, description (first line of the task, ≤120 chars), status
  (`queued|running|completed|failed|cancelled|interrupted`), model, thinking, model_source,
  session_id (UUID v4, fixed for the life of the task), cwd, background, pid, exit_code, error,
  runs, started/finished timestamps (ms), changed_files, hook_notes, tools, exclude_tools, isolated.
- Writes are atomic (temp file + rename).
- An active record (`queued`/`running`) not owned by a run in this process, whose pid is absent or
  dead, is reported as `interrupted` (its owner process ended). It can be resumed.

## 3. Child command

`<current exe> -p --mode text --session-dir <root>/sessions --session-id <session id> --name <id>
[--model <provider/id>] [--thinking <level>] [--append-system-prompt <root>/<id>.prompt.md]
[--tools a,b] [--exclude-tools x,y] -- <prompt>`

- cwd = the task cwd (or the isolated workspace's cwd); stdin = null; stdout → `<id>.md`;
  stderr → `<id>.log`; environment inherited plus `MHC_TASK_DEPTH=<parent depth + 1>` and
  `MHC_TASK_ID=<id>`. The child is not killed when the parent process exits.
- First run prompt: `# Context\n<context>\n\n# Task\n<task>` when a shared context is given, else
  the task text. A resume reuses the same session id with the follow-up text (default: "Continue
  the task where you stopped and finish it. Reply with the final result.").
- Exit 0 → `completed`; other exit or spawn failure → `failed` (with the error); cancellation →
  `cancelled` (the child is killed).

## 4. Depth

The extension registers no task tools when `MHC_TASK_DEPTH` (default 0) is not below
`task.max_depth` (default 1). So by default only the main session delegates.

## 5. Model choice

Order: explicit `model` → `category` (must be configured in `categories` and not disabled,
otherwise an error listing the configured ones) → the agent's models (frontmatter `model`/`models`
or the `agents.<name>` config entry; oh-my-pi `@role` selectors are skipped) → the parent
session's model. Inputs go to `resolve_delegate_model` (model-selection §5.4) with the available
models from `models.json` + `auth.json` and `system_default` = the parent model; `Deferred`/none
falls back to the parent model. No built-in category data is shipped (the defaults and chains are
left empty; categories are user-configured).

## 6. Tools

- `task`: `{task, agent?, category?, model?, name?, background?, isolated?, context?, tasks?[]}`.
  `tasks` runs a batch with the shared `context`. Foreground items return their `<task-result>`
  envelopes in order; background items return their id immediately and their envelope is later
  delivered to the session as a custom message (`task-result`, follow-up, triggers a turn).
- `task_output {id, wait?, timeout_ms?}`: envelope of a settled task or its status line; `wait`
  is clamped to `task.wait.{min,max}_ms` (default `task.wait.default_ms`).
- `task_list {}`; `task_resume {id, prompt?, background?}` (not while active); `task_cancel {id}`
  (a run owned elsewhere is signalled by pid).
- `/tasks` command: the list as a notification.

## 7. Interfaces for later phases

- `WorkspaceIsolation { prepare(id, cwd) -> {cwd, handle}, finish(workspace, succeeded) -> note }`.
  `isolated: true` (or `task.isolation.enabled`) without a backend is an error; no backend is
  shipped in this step.
- `TaskObserver { started(record), finished(record) }` for team views / terminal panes.
- Post-run hooks: a `maho_lsp::hooks::HookChain` runs over the files a child changed (git porcelain
  difference before/after the run) and its texts are appended to the envelope (`<post-run>`).
