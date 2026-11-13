# Ultrawork Orchestration Engine — Behavior Specification

Status: Clean-room behavior specification for `crates/maho-ultrawork`.
Author: Spec Writer (SUL reader role; clean-room boundary applied).
Audience: Clean-room implementers (fresh sessions without access to proprietary sources).
Governing rules: `docs/cleanroom/specs/README.md` and `docs/cleanroom/specs/README-ulw.md`.

---

## Overview and Architectural Role

The ultrawork engine is an extension that provides disciplined, high-rigor orchestration for complex engineering tasks. When the user requests this execution mode using designated keywords, the engine arms the session, injects an operational contract into the conversation history, and provides sizing reminders for multi-step task lists.

The engine operates on four core principles:
1. **Zero Unsolicited Ceremony**: Invocations inside quotes, code fences, blockquotes, URLs, negated requests, or identifiers do not arm.
2. **Minimal Context Occupancy**: The core execution contract is injected exactly once per session. Subsequent mentions while armed receive a short reminder. Compaction only re-arms upon an explicit re-mention.
3. **Zero History Transformation Overhead**: The engine performs no O(N) message scanning or cloning on normal model evaluation turns.
4. **Bounded Subagent Overhead**: Delegated child completions and serialized parent wake messages are strictly size-capped with local file offloading.

The implementer defines appropriate internal types for session tracking, input classification, and extension event handling.

---

## 1. Keyword Arming and Text Masking

### 1.1 Target Keywords
The engine activates when user prompt text contains either of the following trigger keywords as standalone words (case-insensitive ASCII comparison):
- `ulw` (abbreviated alias)
- `ultrawork` (full alias)

### 1.2 Standalone Word Boundary Rules
A keyword triggers arming only when it appears as a distinct word token:
- **Valid Preceding Boundaries**: Start of input, whitespace, opening punctuation (parentheses, brackets, braces, angle brackets, quotation marks), or standard separators (commas, colons, semicolons).
- **Disallowed Preceding Boundaries**: Any letter or digit followed by a connector or path separator (hyphen, underscore, period, slash, or backslash). Examples: `my-ulw`, `prev_ulw`, `/ulw`.
- **Valid Following Boundaries**: End of input, whitespace, closing punctuation (parentheses, brackets, braces, angle brackets, quotation marks), or terminal punctuation (periods, exclamation marks, question marks, commas, colons, semicolons).
- **Disallowed Following Boundaries**: Any transition into a compound identifier, dot-separated file extension, double-dash parameter, or path component. Examples: `ulw-loop`, `ulw-plan`, `ulw_task`, `ultrawork_v2`, `ulw.exe`, `ulw/path`.

### 1.3 Pre-Masked Inactive Contexts
Before matching keyword patterns, the engine masks inactive regions of the input text by replacing masked characters with neutral padding (preserving exact byte lengths and newline offsets). A keyword inside any of the following regions MUST NOT trigger arming:
1. **Fenced Code Blocks**: Markdown code fences opened by three or more backticks or tildes and closed by a matching fence line.
2. **Inline Code Spans**: Text delimited by matching backtick sequences.
3. **Quoted Spans**: Text enclosed within straight double quotes or curly typographical double quotes.
4. **Markdown Blockquotes**: Lines beginning with the greater-than symbol (optionally indented by up to three spaces).
5. **Uniform Resource Identifiers (URLs)**: Standard web addresses and file paths containing protocol schemes.
6. **Injected Structural Blocks**: System or extension metadata blocks demarcated by XML-like tag pairs.
7. **Relayed Message Headers**: Chat transcript or forwarding headers identifying speaker and recipient pairs.
8. **Prohibitive Phrasing**: Statements where the user explicitly tells the assistant to avoid, bypass, or forgo the execution mode. These patterns combine negative qualifiers (such as never or negative modal contractions) with invocation verbs (such as use, enable, or run) prior to sentence boundaries or contrastive transitions.

### 1.4 Command and Expansion Routing
The classifier inspects input commands to assign appropriate routing metadata:
- **Direct Skill Invocation**: When input begins with `/skill:ultrawork`, the invocation is handled directly by the skill subsystem. Direct directive message injection by the engine is bypassed to prevent redundant contracts.
- **Skill Invocations with Arguments**: When another skill command is invoked (e.g. `/skill:<other> [args]`), the argument text following the skill name is inspected. If the argument string contains an active keyword, it is classified as an argument invocation and is eligible for arming. If no keyword appears in the arguments, arming is suppressed.
- **Pre-existing Contract Markup**: If the prompt already contains active contract boundary tags, the engine recognizes the session as armed while bypassing duplicate message emission.

### 1.5 Input Source Filtering
- **Interactive and RPC Inputs**: Evaluated normally for arming and classification.
- **Extension-Generated Inputs**: Inputs arriving from automated extensions are ignored for arming purposes to avoid feedback loops.

---

## 2. Directive Injection Lifecycle

### 2.1 Initial Session Arming
When an un-armed session receives an effective keyword invocation:
1. The session tracking state records that the session is now armed.
2. The full core execution contract is dispatched to the session as a hidden custom message with the directive custom message type (`display: false`).
3. For streaming or transform inputs where the input framework requires prompt text transformation, the contract is appended directly to the prompt text.

### 2.2 Re-mention Reminder
When an already-armed session receives another user prompt containing an active keyword:
1. The engine notes that the session was already armed.
2. The full contract is NOT re-sent.
3. Instead, a short notice is emitted in place of the contract. Its wording and wrapper markup are the implementer's own; the notice only has to tell the model that no new contract text follows because the copy already in the conversation still applies.

### 2.3 Compaction Handling and Re-arm Policy
Session compaction compresses historical messages to reclaim context:
1. Upon receiving an accepted compaction event, the engine updates session state:
   - The armed status is cleared.
   - A compaction-rearm-pending indicator is set.
2. **Re-arm on Demand Rule**: The engine does NOT inject the contract unprompted following compaction.
3. If and only if the user explicitly submits a prompt containing an active keyword after compaction:
   - The engine recognizes a post-compaction re-arm.
   - The full core contract is re-injected.
   - The pending indicator is cleared, and armed status is restored.
4. If the user continues the session without mentioning the keyword, no directive is injected.

### 2.4 Contract Revision Tracking
If the runtime configuration or contract revision changes during a session, the engine re-evaluates the active contract hash. If the active revision differs from the revision previously injected into the session, the engine re-arms and injects the updated contract text.

---

## 3. Disable Control

The engine supports dynamic deactivation via a runtime boolean flag:
- Configuration key / runtime flag: `ultrawork-disabled` (boolean).
- When this flag evaluates to `true`:
  - Input hooks immediately return `continue` without scanning text or mutating session state.
  - Tool result hooks bypass all reminder evaluations.
  - The classifier reports no effective action due to the active disable flag.

---

## 4. Context Handler Policy (Zero O(N) Rewriting)

To prevent latency and memory regressions:
1. **No Unconditional Context Hooks**: The engine must NOT register a per-turn context transformation hook if all models in the environment share the same baseline contract.
2. **Fast Equality Guard**: If a specialized model variant requires an alternate contract text:
   - The handler inspects only the directive message content.
   - If the existing directive in the message array already matches the target model's variant, the handler returns immediately indicating no change.
   - Message cloning and full history traversals are forbidden when the active text is already equivalent.

---

## 5. Mass Orchestration Mode

### 5.1 Mass Mode Keywords
The engine recognizes designated mass-mode keywords (case-insensitive, with optional whitespace or hyphens):
- `mass ulw`
- `mass-ulw`
- `mass_ulw`
- `mulw`
- `ulw mass`
- `ulw-mass`

### 5.2 Mass Mode Behavior
When any mass-mode keyword is matched:
1. Ultrawork mode is armed.
2. In addition to the core contract, a compact mass-mode orchestration block (~1.5 KB) is attached.
3. The mass-mode orchestration block is fully inline and self-contained; it never instructs the agent to read external skill files.

### 5.3 Phased Execution Structure
The mass-mode block instructs the model to structure complex undertakings into distinct waves:
- **Wave 1 — Disjoint Parallel Execution**: Independent subagent lanes are dispatched in parallel. Each lane operates on an isolated, non-overlapping write scope. Each lane must produce a compact claim summary (maximum 3,500 characters) containing concrete file and line references.
- **Wave 2 — Adversarial Falsification**: Dedicated verification lanes run to audit Wave 1 findings. Reviewers actively attempt to disprove reported claims and check for unhandled edge cases.
- **Integration Phase**: The parent reconciles verified findings and resolves cross-cutting concerns.
- **Fix Wave**: A focused, minimal edit pass addressing verified defects.
- **Verification Wave**: A final end-to-end check confirming solution integrity.

---

## 6. Task List Reminder (Todo Fanout)

### 6.1 Trigger Conditions
The engine monitors tool execution to provide sizing and delegation reminders:
1. The tool result event is for the `todo` tool.
2. The session is armed, or awaiting explicit re-arm after compaction.
3. The tool operation is `init` or `append`. (Long checklists are usually grown by appending, so the reminder fires for either operation; the latch below keeps it to one reminder until the next accepted compaction).
4. No task-count or non-empty-list check is performed: the engine never reads the item list.
5. The reminder latch has not been set since session-state creation or the latest accepted compaction.
6. The tool result did not report an error.

### 6.2 Injected Content
When triggered, the engine appends one advisory text block, written by the implementer, to the tool result output. Its required outcome: before starting the first item, the model first weighs how much work the whole new list represents, judged by its number of items and the effort each one implies; it then makes an explicit choice between handing items to parallel child agents and working them itself, bases that choice on whether the items can proceed independently and whether parallelism actually saves time once its setup cost is counted, and states the choice and its reason to the user. The block also asks that the checklist keep mirroring reality for the rest of the run.

### 6.3 Compaction Reset
When session compaction occurs, the reminder latch is reset and the session moves to compaction-pending rather than armed. The next eligible `todo init` or `todo append` result after that reset receives the reminder — at any item count, and without the user repeating the keyword.

---

## 7. Public Capabilities and API Surface

The crate exports capabilities required by telemetry, command handlers, and test harnesses:

### 7.1 Input Classification
- **Capability**: Analyze prompt text and determine arming eligibility, keyword counts, and routing classification without mutating state.
- **Return Data**:
  - `short_form_seen`: Boolean indicating whether the `ulw` keyword matched.
  - `long_form_seen`: Boolean indicating whether the `ultrawork` keyword matched.
  - `visible_mentions`: Total count of visible keyword occurrences.
  - `effective`: Boolean indicating whether the input triggers an active injection or arming change.
  - Lifecycle stage: Whether this is an initial arming, a subsequent reminder, a post-compaction re-arm, or none.
  - Routing classification: Whether invocation is direct, skill expansion, skill arguments, embedded, or none.
  - Suppression reason: Explanation if no action is taken (e.g. extension source, absence of keyword, compound identifier reference, explicit negative instruction, embedded markup, or skill command).
- **Telemetry Invariant**: Keyword flags and occurrence counts must be computed and exposed prior to applying any arming mutations.

### 7.2 Arming State Management
- **Snapshot Reading**: Returns the current arming status (armed flag and compaction-pending flag) for a given session identifier.
- **Arming Mutation**: Marks a session as armed, clearing any pending compaction flags.
- **Compaction Mutation**: Records that compaction occurred, setting the compaction-rearm-pending flag and clearing active armed status.
- **Session Tracking**: Tracks the active session identifier.

### 7.3 Fast Presence Check
- **Fast Filter**: A function returning boolean `true` if visible unmasked keywords exist in the text, allowing callers to skip complex parsing on non-matching inputs.

---

## 8. Platform and Profile Parity (MIT Build)

1. The crate `crates/maho-ultrawork` is licensed under the MIT License.
2. It must compile and function identically under both the default workspace build and the standalone MIT profile (`--no-default-features`).
3. It relies solely on safe standard library primitives and public APIs from `maho-ext-api`.

---

## 9. Delegated Task Completion Transport and Wake Batch Limiting

This section defines the transport boundaries for child task completion delivery to parent agents (used by `crates/maho-task`):

### 9.1 Per-Child Completion Response Transport Limit
- **Configuration Key**: `task.completion_transport_limit` in `~/.omo/omo.jsonc`.
- **Default Value**: 8,000 UTF-16 code units (minimum: 1,000; maximum: 64,000).
- **Behavior**:
  - If a child task's final response length in UTF-16 code units is less than or equal to the limit, the response is delivered inline in the completion notification details.
  - If the response exceeds the limit:
    1. The inline text is cleanly sliced at the limit boundary. Crucially, the slice must respect Unicode scalar value boundaries (surrogate pairs in UTF-16 must never be split).
    2. The full, unmodified response text is written to a local file: `<state_dir>/completion-results/<task_id>.txt`.
    3. The completion details object records the sliced text and attaches the file reference URI: `local://<state_dir>/completion-results/<task_id>.txt`.

### 9.2 Parent Wake Batch Limit
- **Configuration Key**: `task.completion_batch_limit` in `~/.omo/omo.jsonc`.
- **Default Value**: 24,000 UTF-16 code units (minimum: 2,000; maximum: 128,000).
- **Behavior**:
  - Multiple child tasks may finish concurrently, triggering a batched wake notification to the parent session.
  - The serialized wake message (including all headers, task identifiers, status lines, and inline details) must not exceed `task.completion_batch_limit`.
  - When constructing the wake message:
    1. The notifier appends completed child details while the cumulative serialized UTF-16 length remains within the budget (reserving space for a manifest trailer line).
    2. If appending another child result would exceed the batch limit, the remaining results are offloaded to a batch manifest file: `<state_dir>/completion-results/wake-overflow-<batch_id>.json`.
    3. The wake message appends a single summary trailer line pointing to the offloaded results:
       `N more task results: local://<path_to_manifest>`
    4. This ensures that parent context is bounded regardless of how many subagents settle simultaneously.

---

## 10. Test Vectors

The following 39 test vectors specify expected behavior across all functional areas. All examples are newly invented:

### Group A: Standalone Positive Keywords
1. **Input**: `"ulw please audit the authentication module"`
   - **Expected**: `effective: true`, `short_form_seen: true`, `long_form_seen: false`, `visible_mentions: 1`, action: first-time arming, route: direct prompt.
2. **Input**: `"ULTRAWORK refactor the database connector"`
   - **Expected**: `effective: true`, `short_form_seen: false`, `long_form_seen: true`, `visible_mentions: 1`, action: first-time arming, route: direct prompt.
3. **Input**: `"Use ulw for planning and ultrawork for implementation"`
   - **Expected**: `effective: true`, `short_form_seen: true`, `long_form_seen: true`, `visible_mentions: 2`, action: first-time arming, route: direct prompt.
4. **Input**: `"프로젝트 전체를 ulw 모드로 점검해줘"`
   - **Expected**: `effective: true`, `short_form_seen: true`, `long_form_seen: false`, `visible_mentions: 1`, action: first-time arming.
5. **Input**: `"버그 원인을 ultrawork 방식으로 분석해"`
   - **Expected**: `effective: true`, `short_form_seen: false`, `long_form_seen: true`, `visible_mentions: 1`, action: first-time arming.

### Group B: Quoted and Fenced Inactive Contexts
6. **Input**: `"Command to run: \"ulw execute\""`
   - **Expected**: `effective: false`, `visible_mentions: 0`, suppression: no visible keyword.
7. **Input**: `"He recommended “ultrawork” during the standup"`
   - **Expected**: `effective: false`, `visible_mentions: 0`, suppression: no visible keyword.
8. **Input**: `"Run the script using `ulw start`"`
   - **Expected**: `effective: false`, `visible_mentions: 0`, suppression: no visible keyword.
9. **Input**: `"```bash\nulw test --all\n```"`
   - **Expected**: `effective: false`, `visible_mentions: 0`, suppression: no visible keyword.
10. **Input**: `"~~~markdown\nultrawork active\n~~~"`
    - **Expected**: `effective: false`, `visible_mentions: 0`, suppression: no visible keyword.
11. **Input**: `"> Note: always run ulw before deploy"`
    - **Expected**: `effective: false`, `visible_mentions: 0`, suppression: no visible keyword.
12. **Input**: `"Documentation: https://example.org/docs/ulw/getting-started"`
    - **Expected**: `effective: false`, `visible_mentions: 0`, suppression: no visible keyword.
13. **Input**: `"<custom-reminder-tag>ulw is active</custom-reminder-tag>"`
    - **Expected**: `effective: false`, `visible_mentions: 0`, suppression: no visible keyword.
14. **Input**: `"Prompt already carrying directive wrapper tags"`
    - **Expected**: `effective: false`, route: embedded directive, suppression: embedded directive present.

### Group C: Negated Mentions
15. **Input**: `"Please do not use ulw for this query; keep it simple"`
    - **Expected**: `effective: false`, `visible_mentions: 0`, suppression: prohibitive phrasing detected.
16. **Input**: `"Don't enable ultrawork mode on quick fixes"`
    - **Expected**: `effective: false`, `visible_mentions: 0`, suppression: prohibitive phrasing detected.
17. **Input**: `"Never run ulw on read-only repositories"`
    - **Expected**: `effective: false`, `visible_mentions: 0`, suppression: prohibitive phrasing detected.
18. **Input**: `"dont trigger ultrawork until tests pass"`
    - **Expected**: `effective: false`, `visible_mentions: 0`, suppression: prohibitive phrasing detected.

### Group D: Identifier and Path Boundaries
19. **Input**: `"Check the ulw-loop status in the terminal"`
    - **Expected**: `effective: false`, suppression: compound identifier reference.
20. **Input**: `"Review the plan in ulw-plan file"`
    - **Expected**: `effective: false`, suppression: compound identifier reference.
21. **Input**: `"Inspect parse_ulw_flags function"`
    - **Expected**: `effective: false`, suppression: compound identifier reference.
22. **Input**: `"Binary is at target/release/ulw.exe"`
    - **Expected**: `effective: false`, suppression: compound identifier reference.
23. **Input**: `"Pass --ulw flag to compiler"`
    - **Expected**: `effective: false`, suppression: compound identifier reference.

### Group E: Command and Extension Handling
24. **Input**: `"/skill:ultrawork"`
    - **Expected**: `effective: false`, route: skill expansion, suppression: handled by skill system.
25. **Input**: `"/skill:reviewer ulw inspect the changes"`
    - **Expected**: `effective: true`, route: skill arguments, action: first-time arming.
26. **Input**: `"/skill:reviewer inspect the changes"`
    - **Expected**: `effective: false`, route: none, suppression: skill name without keyword arguments.
27. **Input**: `"Automated summary: ulw completed"` with source marked as automated extension
    - **Expected**: `effective: false`, suppression: automated extension source.

### Group F: Mass Mode Activation
28. **Input**: `"mass ulw audit security vulnerabilities"`
    - **Expected**: Arms ultrawork mode and attaches the 1.5 KB mass orchestration block.
29. **Input**: `"Run mass-ulw on performance benchmarks"`
    - **Expected**: Arms ultrawork mode and attaches the mass orchestration block.
30. **Input**: `"Use mulw to migrate all service endpoints"`
    - **Expected**: Arms ultrawork mode and attaches the mass orchestration block.
31. **Input**: `"ulw mass optimize query latency"`
    - **Expected**: Arms ultrawork mode and attaches the mass orchestration block.
32. **Input**: `"ulw-mass refactor type definitions"`
    - **Expected**: Arms ultrawork mode and attaches the mass orchestration block.

### Group G: Task List (Todo Fanout) Reminder
33. **Condition**: Session armed; `todo` tool `init` with 2 items.
    - **Expected**: Sizing advisory notice appended (no task-count threshold).
34. **Condition**: Session armed; `todo` tool `init` with 3 items.
    - **Expected**: Sizing advisory notice appended to tool result.
35. **Condition**: Session armed; reminder already fired; subsequent `todo` tool `append` with 4 items.
    - **Expected**: No reminder injected (the latch is already set, not because the operation is `append`).
36. **Condition**: Session armed; compaction accepted; subsequent `todo` tool `init` with 4 items.
    - **Expected**: Sizing advisory notice appended (compaction reset the latch).

### Group H: Delegated Task Transport Limiting
37. **Condition**: Child response length 10,000 UTF-16 code units; transport limit configured to 8,000.
    - **Expected**: Response sliced to 8,000 UTF-16 units; full response written to disk; details contains local URI reference.
38. **Condition**: Child response with 4-byte emoji ending at unit index 8,000.
    - **Expected**: Slice stops cleanly before the emoji surrogate pair at 7,999 units; surrogate pair is not broken.
39. **Condition**: Wake batch with 5 children each producing 6,000 UTF-16 units; batch limit configured to 24,000.
    - **Expected**: First 3 children fit inline (~18,000 units); remaining 2 children spilled to manifest file; trailer line added pointing to manifest URI.

---

## Leak Self-Check

As required by `docs/cleanroom/specs/README.md` and `docs/cleanroom/specs/README-ulw.md`, the spec writer performed the leak self-audit:
- [x] No fenced code block contains Rust, TypeScript, or JavaScript implementation from proprietary sources.
- [x] No sentence paraphrases proprietary comments, docstrings, or prompts.
- [x] All identifier proposals are neutral.
- [x] Tested against the forbidden pattern: zero occurrences of forbidden identifiers, internal helper names, proprietary crate paths, or vendor terms.
