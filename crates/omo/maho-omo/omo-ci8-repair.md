# CI8 repair

Worktree: `/home/projects/mhc-wt/nonwindows-20261020`.
Source ownership: `crates/omo/maho-omo/**` only.
Status: source repairs applied; compilation and runtime behavior remain unverified.

## Changes

- `src/component_list.rs`: use the actual public path
  `maho_omo_lsp::index::LspComponentOptions::default()`.
  `maho-omo-lsp/src/lib.rs` re-exports only `LspComponent`, while its public
  `index` module defines `LspComponentOptions` with formatter and diagnostic
  runner options. The call retains those default options and
  `Some(runtime.defer_macrotask())`; it does not bypass option-aware registration.
- `src/claude_compat.rs`: bridge the command context's
  `maho_tools::definition::AbortSignal` to the AI signal required by
  `maho_ext_api::ExecuteToolOptions`. Create an AI `AbortController` for the
  invocation, forward an already-aborted context before tool execution, and
  select the tools signal's `cancelled()` notification against tool completion.
  On cancellation, abort the AI controller and await the same pinned execution
  future so the downstream tool receives cancellation and can finish cleanup.
  The biased cancellation branch handles an abort already observable when the
  operation becomes ready. No context signal remains `None` in tool options.
  There is no detached forwarding task or listener to leak when execution ends
  or the command future is dropped. The tools signal has only a boolean abort
  state, so the destination uses the AI controller's default abort reason.
- `Cargo.toml`: add the existing workspace path dependency on `maho-ai` to name
  its real `AbortController`; no dependency package or version is introduced.

## Inspected contracts and upstream

- `crates/omo/components/maho-omo-lsp/src/{lib,index}.rs`:
  public module path and `register_with_startup_deferral` options contract.
- `crates/maho-ext-api/src/types.rs`: context signal aliases the tools signal;
  `ExecuteToolOptions.signal` explicitly uses the AI signal.
- `crates/maho-tools/src/definition.rs`: persistent watch-backed cancellation
  supports subscribers installed after abort without timing-based polling.
- `crates/maho-ai/src/utils/abort.rs`: controller signal clones share state;
  `abort(None)` publishes the default reason and wakes cancellation subscribers.
- `/home/indo/code/oh-my-openagent/packages/omo-senpi/src/extension/component-list.ts`
  and `src/components/lsp/index.ts`: local upstream composition invokes the
  LSP factory with default options, and the factory retains optional diagnostic
  runner injection. This checkout was read as supporting context, not claimed
  to match the worktree's pinned upstream revision.
- Existing scoped tests in `tests/all/component_list.rs` and the inline
  `claude_compat.rs` tests were read, not executed or modified.

## External path needed by the parent

`/home/projects/mhc-wt/nonwindows-20261020/Cargo.lock` is outside this lane's
grant and was not edited. Its `[[package]]` entry for `name = "maho-omo"`
needs `"maho-ai",` added to `dependencies` (before `"maho-ext-api",` in
the existing alphabetical ordering). The package already exists in the lockfile;
this records the newly direct dependency edge. The parent must grant that exact
path or incorporate this lockfile change during consolidation before a locked
verification run.

## Log and verification limitations

The supplied log
`/home/projects/mhc/.omo/nonwindows-20261020/ci-39430684876/clippy.log`
contains no `component_list`, `claude_compat`, `LspComponentOptions`, or
`AbortSignal` diagnostic. Its failure is an unrelated E0063 in
`crates/builtins/maho-ext-cache-keepalive/tests/policy.rs:3`, missing
`prompt_cache` and `supports_assistant_prefill` in a `Model` initializer;
the recorded command exits 101. That external file was not changed.
The two assigned CI8 defects were established from the current source/API
contracts rather than attributed to nonexistent lines in that log.

No cargo, tests, CI, diagnostics, formatter, or commits were run. All edits used
`apply_patch`. The parent owns final merged verification, including pre-aborted
and in-flight command cancellation and completion without a context signal.
