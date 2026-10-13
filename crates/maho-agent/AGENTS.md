# crates/maho-agent

## OVERVIEW
Core agent loop runtime and tool execution harness for senpi/maho agents.

## WHERE TO LOOK
| Task | Location | Notes |
|------|----------|-------|
| Agent loop & step execution | `src/agent_loop.rs` | Main event cycle, stream parsing, and state updates |
| Tool execution harness | `src/harness/` | Tool invocation, sandboxing, and response mapping |
| Assistant state transitions | `src/assistant_terminal_state.rs` | Terminal status handling and finish conditions |
| Recovery logic | `src/empty_assistant_recovery.rs` | Empty response detection and retries |
| Tool routing & aliases | `src/tool_arguments.rs` | Parameter normalization and aliasing |
| Tool aliases resolution | `src/tool_name_alias.rs` | Resolving legacy tool names to active definitions |
| Search integrations | `src/search/` | Internal codebase search primitives for agent context |

## CONVENTIONS
- Async runtime operates over asynchronous event streams without raw blocking calls.
- Strict typing on tool input payloads and agent state representations.
- Error handling propagates structured errors without unwraps across boundaries.
- Workspace lints enforced: zero unwrap allowed in library code.

## ANTI-PATTERNS (THIS COMPONENT)
- Never execute external commands directly outside the provided harness context.
- Do not bypass tool result schema validation before passing responses back to model.
- Avoid persistent in-memory mutable state across independent agent invocations.
- Never use unvalidated fallback aliases when a tool schema mismatch is detected.

## NOTES
- Crate scored 15 in complexity analysis due to high module centrality and critical event loop role.
- All agent loop tests run faux LLM streams to guarantee deterministic behavior.
