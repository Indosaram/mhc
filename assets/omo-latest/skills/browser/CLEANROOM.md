# Clean-room record — `browser` skill

## Inputs used
- Spec: `/workspace/work/cleanroom/browser/spec.md` (behaviour-only delta spec).
- MIT base: `/workspace/work/cleanroom/browser/base/` — lazycodex commit `75b017473c71d669dd9aed0a10e950e70724b283` (MIT, see `ATTRIBUTION.md`).
- mhc Rust source under `/workspace/work/mhcg/crates` was consulted only to confirm names: `omo_browser_bridge` (permission-system tests), `ask_user_question` / `request_user_input`, their `waitForAnswer` / `wait_for_answer` flags, and the answer shape (`details.status`, `details.answers` map) in `maho-ext-ask-user`.

## Statement
No file under `/workspace/work/mhcg/assets/omo-latest/` or `/workspace/work/mhc/assets/` (SUL-1.0 text) was opened, read, grepped or diffed. All new code and prose was written from the spec alone.

## Packaging decisions
- `runtime/omowright/` kept (spec §0).
- Identifiers kept: `OMO_BROWSER_ENGINE`, `omowright`, `omo_browser_bridge`, `omo.browser.state` (exported as constants `BROWSER_ENGINE_ENV`, `BROWSER_BRIDGE_TOOL`, `BROWSER_STATE_EVENT`).
- The base contained no Codex-only harness sections, `lazycodex-*` role names or `multi_agent_v1` references (grep clean), so nothing needed replacing with `task` / `team_*` / `workflow`.
- Unchanged files: `ATTRIBUTION.md`, `references/remote.md`, `references/owned-engine/*`, `references/recipes/1password.md`, `scripts/browser-install.mjs`, `scripts/browser-doctor.mjs`, `runtime/**`.

## Requirement → change checklist
| Req | Change |
|---|---|
| R1 | The built-in-browser connect export, toolkit acquire, token-file read, bearer refresh and its unavailable-error class deleted; `node:fs` import gone; removed from `omowright.mjs` re-exports, SKILL.md Step 0 and commands.md. |
| E1 | `ENGINES = connected/builtin/none`; anything else → unsupported. |
| E2–E4 | `engineRefusal()` → `browser_engine_none` / `browser_engine_builtin` / `browser_engine_unsupported` (names bad value + valid ones). |
| E5 | Guarded `connectBrowserSkill` checks bridge `status` first (throws `BrowserUserStoppedError` without connecting), then wraps the raw session. |
| E6 | `no_browser_connected` / `no_daemon` / `unsupported` → `BrowserNotConnectedError` with `cause`; others rethrown; `connect_failed` emitted with the original code either way. |
| T1 | `BrowserGuardError` base (name, code, cause) + `BrowserEngineRefusal` (optional `engine`), `BrowserNotConnectedError`, `BrowserUserStoppedError`, `BrowserActionDeclinedError` (`user_declined` / `confirmation_unavailable`); exported from guard and `omowright.mjs`. |
| T2 | `BROWSER_CONFIRMATION_POLICY`, `BROWSER_STATE_EVENT` exported with `BROWSER_ENGINE_ENV`, `guardOmowright` (plus `isIrreversibleLabel`). |
| G1 | New object: non-functions copied; 33-name allowlist copied by identity; all other functions → named `blockedStub` (reject on call, sync throw on `new`; `browser_engine_owned_blocked` with per-engine reason for connect/connectPipe/connectCloakProfile, else `browser_engine_export_blocked`; `.engine` set); guarded `connectBrowserSkill` and `bskSnapshot` (WeakMap guarded→raw) overlaid. Verified against the staged runtime: all 33 allowlisted names exist. |
| G2 | `guardSession` Proxy; pipeline: stop check → preflight/confirm → `acting` → raw call (`user_aborted` → bridge `stopped` + `user_stopped` + raw stop + user-stopped error; other → `failed` + rethrow) → `navigated`(tab) + `idle` for navigating actions. Unhandled methods pass through bound to raw. |
| G3 | `NAVIGATING`, `PLAIN`, `PREFLIGHT` tables as listed. |
| G4 | `tool()` allows observe/snapshot/get_html/screenshot/tab_list/console/network; others → `browser_tool_blocked` naming the session method. |
| G5 | `stop()` returns null if already stopped; otherwise stops and always emits `stopped` (finally). |
| G6 | `currentTab()` lists agent-scope tabs, active-or-first, id/url/title + favicon under several spellings; failures → undefined. |
| C1 | `isIrreversibleLabel()` — English whole-word groups (incl. sign…/order-except/cancel…/close account) + Korean substrings. |
| C2 | `classifyTarget` (ref / selector / point); ref → `getHtml` ≤4 KB → `describeElementHtml` (aria-label/title/value/alt + stripped text, editable rules); selector → in-page describer (tag, editable, text≤200, aria-label, title, button-only value, form `{action, method, submitLabels}`); failures → unknown. |
| C3 | `clickPreflight`: unknown → ask ("label could not be read"); matching label → ask quoting it. |
| C4 | `pressPreflight`: Enter/Return only; Ctrl/Meta/Cmd always ask; Shift never; ref/point and selector/no-target branches in the spec's order, incl. POST action-path segment check. |
| C5 | `scriptNeedsConfirmation`: `.click(`, `.submit(`, `.requestSubmit(`, `dispatchEvent`, `sendBeacon(`, or fetch/XHR + quoted POST/PUT/PATCH/DELETE; non-strings never ask. |
| C6 | `confirm()`: prefers `ask_user_question` then `request_user_input`; none → `confirmation_unavailable`; emits `awaiting_confirmation` with tab; header "Browser", "on <host>", options `Allow` / `Don't allow`, waits; per-tool payload and answer parsing; anything but `Allow` → `failed`/`user_declined` + declined error. |
| B1 | `makeBridge`: best-effort `state` / `status` (`details.stopped === true`) / `stopped` ops; errors, missing host and `isError` results = no answer. |
| B2 | Emits started, connect_failed, acting, navigated, idle, failed, awaiting_confirmation, user_stopped, stopped. |
| B3 | `kernelHost()` (`listTools`, `callTool`) kept unchanged in `omowright.mjs`; used for question-tool discovery. |
| D1 | SKILL.md Step 0 rewritten: engine-choice override, four-case table, guarded-library behaviour, confirmation, declined/stopped handling, not-a-security-boundary note; built-in browser option removed. |
| D2 | Engine table "neither" row broadened (text extraction, 403, fetcher-blocking platforms). |
| D3 | Bot-scored/WAF rule gains "don't switch engines while the env var is set" exception. |
| D4 | commands.md: `user_aborted` row extended; built-in paragraph replaced by typed-error table. |
| D5 | install.md: builtin/none sessions never use the attached engine. Section heading reworded to "desktop harness". |

## Acceptance checks
- `node --check` passes for all four scripts.
- `/workspace/work/cleanroom/browser/acceptance.test.mjs` (kept outside `out/`, `node --test acceptance.test.mjs`) covers all 14 spec checks plus extras (Allow path with `request_user_input`, mid-session Stop, label table): **19/19 pass**.
- Smoke test with the real staged runtime: unset env returns the raw library; `connected` keeps exactly the 33 allowlisted functions by identity.
