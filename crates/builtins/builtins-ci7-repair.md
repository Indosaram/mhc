# Builtins CI7 repair

Worktree: `/home/projects/mhc-wt/nonwindows-20261020`.
Write scope: `crates/builtins/**` only.
Source logs: `/home/projects/mhc/.omo/nonwindows-20261020/ci-39428472935/`.

## Observed CI failures

Searched all 13 supplied job logs for missing-field diagnostics. The builtins failures were:

- `test__4_.log:1045`: `maho-ext-cache-keepalive/src/extension.rs:17` omitted `Context.active_tool_names`.
- `test__6_.log:1553,1559,1565`: `maho-ext-terminal/src/tools/bash.rs:299,309` and `src/extension.rs:57` omitted `ToolResult.is_error`.

The same logs also contain failures outside builtins, including maho-interactive source errors and other message/model constructor omissions. Those remain outside this repair's ownership; this report does not claim the CI run is green.

## Changes

- `maho-ext-cache-keepalive/src/extension.rs`: populate `active_tool_names` with the already retrieved active tool list, retaining session selection when warming the prompt cache.
- `maho-ext-btw/src/side_query.rs`: add `active_tool_names: None` to both tool-free side-query contexts discovered by the inventory.
- `maho-ext-terminal/src/tools/bash.rs`: add `is_error: None` to both interim progress results. These are progress updates, not completed command outcomes.
- `maho-ext-terminal/src/extension.rs`: forward `result.is_error` on the successful adapter path. The existing `Some(true)` branch still returns `ToolError::Message`; failures are not defaulted to success.
- `maho-ext-compaction/src/emergency_prune.rs`: carry the source tool-result message's `isError` boolean into the temporary result used during pruning. Final message reconstruction still replaces only content and preserves original metadata.
- `maho-ext-compaction/tests/tool_truncation.rs`: use an error-marked result in the existing metadata-preservation fixture and assert that pruning preserves its error status.
- `maho-ext-goal/src/renderers.rs`: preserve the incoming agent result's optional error status in the renderer adapter.
- `maho-ext-websearch/src/websearch/tool.rs`: explicitly mark local error results with `Some(true)` and derive completed search status from `details.error.is_some()`. Progress updates remain unmarked.

## Constructor inventory

Searched the whole builtins tree, including inline tests, integration tests, and examples, for `Context` and `ToolResult` literals. Unrelated `ExtensionContext`, render contexts, event variants, and local test context types were distinguished from the affected types.

AI contexts inspected:

- Anthropic subscription `tests/request_env.rs` and both `tests/stream.rs` contexts use `..Default::default()`.
- Cursor CLI OAuth's two `tests/stream.rs` contexts use `..Default::default()`.
- BTW's two `src/side_query.rs` contexts needed the field and were repaired.
- Cache keepalive's `src/extension.rs` context needed the field and was repaired.
- Compaction's `src/model_usability_budget.rs` production and test contexts, `src/openai_remote.rs`, `src/openai_remote_responses_v2.rs`, `src/speculative_summary.rs`, and `tests/message_safety.rs` already specify `active_tool_names`.
- Look-at's `src/runner.rs` context already specifies `active_tool_names`.

Tool-result literals inspected:

- Compaction: `src/emergency_prune.rs`, `tests/tool_truncation.rs`.
- Goal: `src/renderers.rs`, `src/tool_registration.rs`.
- GPT apply-patch: all three `src/tool.rs` literals.
- Loop: both `src/tools.rs` literals.
- MCP: `src/expose/proxy.rs`, both `src/expose/register.rs` literals, both `src/resources.rs` literals. The completed MCP adapter already converts mapped failure results into `ToolError` before constructing its successful result.
- Terminal: `src/extension.rs` and both `src/tools/bash.rs` progress literals.
- Todo tools: `src/tools_todo.rs`.
- Tool search: `src/tool.rs`.
- Video input: `src/lib.rs`.
- Webfetch: both `src/webfetch/tool.rs` literals.
- Websearch: all four `src/websearch/tool.rs` literals.

Outside the three terminal omissions, these result literals already had the required field. Error-status forwarding corrections found during this inventory are listed above. Agent-tool-result literals use their separate contract and were not treated as missing `ToolResult` fields.

## Verification limits

Only source/log inspection and `apply_patch` were used. No cargo command, test execution, CI dispatch, commit, formatter, or explicit diagnostic run was performed, as requested. After the test fixture patch, the patch tool automatically attempted fresh LSP diagnostics and reported a 3000 ms timeout; that supplied no compiler verdict. The adjusted test is authored but unrun. Compilation, runtime behavior, and the final combined CI result remain unverified.
