# crates/maho-ai

## OVERVIEW
Multi-provider LLM abstraction layer, model client bindings, and streaming protocol adapters.

## WHERE TO LOOK
| Task | Location | Notes |
|------|----------|-------|
| Provider implementations | `src/providers/` | Anthropic, OpenAI, Gemini, Bedrock, Groq, Ollama |
| Model registry & specs | `src/models/` | Token limits, capabilities, pricing definitions |
| Image & multimodal models | `src/images_models/` | Vision and multimodal capability metadata |
| Authentication & credentials | `src/auth/` | OAuth tokens, API keys, environment token resolution |
| Tool calling middleware | `src/tool_call_middleware/` | Function schema adaptation and call parsing |
| API transport & retries | `src/api/` | HTTP client configuration, exponential backoff |
| Cursor & node compatibility | `src/cursor/`, `src/node/` | Protocol compatibility layers |

## CONVENTIONS
- Streaming responses are mapped directly to standard token/chunk events.
- Never hardcode model IDs; use model registry definitions.
- Workspace lints enforced: zero unwrap in library code.
- Provider client calls use async-native transport with explicit retry budgets.

## ANTI-PATTERNS (THIS COMPONENT)
- Do not log or leak raw API keys, OAuth tokens, or authorization headers.
- Never swallow provider rate-limit (429) or transient 5xx errors silently.
- Do not bypass token truncation safeguards on large context payloads.
- Avoid provider-specific payload shapes outside `src/providers/`.

## NOTES
- Scored 15 due to high export count, extensive provider matrix, and heavy downstream dependents.
- Tests verify request serialization against upstream golden JSON fixtures.
