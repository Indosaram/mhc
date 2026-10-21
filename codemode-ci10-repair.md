# CI10 codemode shared-constructor repair

## Scope and status

Worktree: `/home/projects/mhc-wt/nonwindows-20261020`.

Changed `crates/maho-codemode/tests/completion_handler.rs`: added `prompt_cache_diagnostics: None` and `error_cause: None` to the fresh `AssistantMessage` fixture returned by `message(text)`.

No cargo commands, tests, CI, or commits were run. All edits used `apply_patch`. Compilation and runtime behavior remain unverified. This report is the only authored file outside the codemode crate for this lane.

## Diagnostic evidence

Evidence directory: `/home/projects/mhc/.omo/nonwindows-20261020/ci-39437518025/`.

Read the complete diagnostic and failure termination in `test__4_.log:1829-1839`:

```text
error[E0063]: missing fields `error_cause` and `prompt_cache_diagnostics`
in initializer of `maho_ai::types::AssistantMessage`
--> crates/maho-codemode/tests/completion_handler.rs:68:5
error: could not compile `maho-codemode` (test "completion_handler")
due to 1 previous error
```

The build-tests step then exits with code 101. The cross-log search also found other AssistantMessage missing-field diagnostics in Linux shard 3 and Windows Clippy; those were not codemode source locations and were not changed by this lane.

## Inventory method and contracts

Searched all 203 codemode Rust source and integration-test files, covering inline test modules as well as `tests/**`. Searches covered shared message names, message variants, model types and deserialization, auth and credential names, shared tool results, usage, aliases/imports, and serde constructor sites. Searches spanning more than the tool's 20-file page were continued through the final page.

Read the current owning definitions for:

- `maho_ai::types::AssistantMessage`, `ToolResultMessage`, `Usage`, and `UsageCost`.
- `maho_ai::model::Model` and its optional model capability fields.
- `maho_ai::auth::types::{ModelAuth, ApiKeyCredential, OAuthCredential, Credential}`.
- `maho_agent::types::AgentToolResult`, re-exported by `maho_ext_api`.
- `maho_tools::definition::ToolResult`.

## Constructor inventory

| Shared family | Codemode construction sites and disposition |
| --- | --- |
| Assistant messages | `tests/completion_handler.rs:68` is the sole literal constructor. It supplied the other current fields but omitted the two fields in the CI diagnostic. Both are now explicitly absent for this fresh synthetic message. No inline-test AssistantMessage constructor was found. |
| Other transcript messages | No `UserMessage` or `ToolResultMessage` literal constructor, or reconstruction through `Message` variants, was found in codemode source or tests. `send_user_message` methods in the extension fixtures accept shared extension inputs rather than constructing AI transcript messages. |
| Model fixtures | `tests/completion_handler.rs:37`, `tests/eval_model_context.rs:43`, and `tests/extension_factory.rs:179` deserialize fresh model JSON. All supply required identity, API/provider, endpoint, reasoning, input, cost, and token-limit values. The remaining current model fields have serde defaults. No model literal requiring additional fields was found. |
| Model copying | Completion selection uses the selected model's clone; the extension factory fixture clones its actual model for context and previous-model state. Those paths retain every current model field without reconstructing them as empty values. |
| Auth and credentials | No `ModelAuth`, `ApiKeyCredential`, `OAuthCredential`, or `Credential` constructor was found in codemode inline or integration tests. HTTP Authorization fixture strings and abort controllers are not model-auth constructors. |
| Usage | The AssistantMessage helper uses `Usage::default()`, which includes the current usage fields. No explicit usage literal needing migration was found in codemode tests. |
| Shared agent results | Fixtures use `AgentToolResult::text(...)`, then assign scenario content/details/error values, or clone existing results. The constructor already initializes structured content, usage, nested calls, added tool names, terminate, and error metadata. No literal AgentToolResult constructor requiring new fields was found. |
| Deserialized result fixtures | `tests/render_golden.rs:13` deserializes the actual `case["result"].clone()` value. Current optional result metadata has serde defaults; the fixture data is retained rather than replaced by a fresh empty result. |
| Native tool-result adapter | `src/tool/eval_tool.rs:16` supplies all four current `maho_tools::definition::ToolResult` fields. Its existing mapping was not changed. This inventory does not claim that the adapter forwards every field of the distinct AgentToolResult type. |

Agent-result fixture users found across integration tests include `active_kernel_prelude`, `agent_bridge`, `call_capture`, `detached_cell_manager`, `detached_cell_notification`, `detached_cell_snapshot`, `detached_eval_result`, `detached_notification_queue`, `eval_execution_event`, `image_collector`, `kernel_registry`, `kernel_tools_scope`, `output_bridge`, `render`, `render_golden`, `reserved_dispatch`, `session_cwd`, `terminal_snapshot_store`, and `tool_result_marshal`.

Inline result fixtures in `src/tool/cell_runtime.rs` and `src/tool/render.rs` use the same shared constructor. The source-wide search also covered result creation and copying in `detached_cell_snapshot`, `detached_eval_result`, `run_eval_cell`, and the result adapter. Existing clone-based preservation is left intact.

## Data preservation and verification boundary

The repaired helper creates a synthetic provider response from text, API/provider/model fixture identifiers, default usage, and explicit absent optionals. It does not copy an existing provider response. `None` therefore records absent prompt-cache diagnostics and absent structured error cause; it does not erase real values from a source message. Existing content, metadata, usage, stop state, error-message mutation, and timestamp values remain unchanged.

No clone was replaced by a manual partial reconstruction, and no deserialized message, model, auth, or result value was replaced with defaults. No new test was added: the existing integration target's compilation directly detects the missing-field regression, and its existing assertions retain completion-format behavior coverage.

This report records a source repair and constructor inventory, not a passing compile/test verdict. Final integrated verification remains with the parent gate under the explicit no-cargo/no-tests/no-CI restriction.
