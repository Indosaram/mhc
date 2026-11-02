# Ultrawork Contract Requirements and Corrections Specification

This document specifies the behavioral requirements, corrections, reference topic divisions, size constraints, and verification scenarios for the clean-room reimplementation of the ultrawork operating contract.

## 1. Rule Inventory and Dispositions

Every operational requirement from the baseline contract is inventoried below. Requirements are grouped by functional domain rather than legacy prompt sequence. Each entry defines the required behavior in clean-room terms, identifies the failure mode it prevents, cross-references corresponding sections in the open skill documentation (`assets/omo-latest/skills/ultrawork/SKILL.md`), and specifies its architectural disposition.

Dispositions:
- `core`: Essential rule included in the always-injected base contract.
- `condensed-core`: Retained in base contract but condensed into a single concise directive.
- `reference:<topic>`: Offloaded to an on-demand reference module loaded only when triggered.
- `delete-wrong`: Removed due to policy contradiction, architectural defect, or obsolescence.

### 1.1 Operational Posture and Verification Standard

| Rule | Behavioral Requirement | Failure Prevented | MIT Reference | Disposition |
| :--- | :--- | :--- | :--- | :--- |
| R1 | Maintain a quiet, outcome-oriented posture that delivers proven modifications while reserving communication for designated milestone transitions. | Context pollution and distracting stream-of-consciousness narration during intermediate tool execution. | `assets/omo-latest/skills/ultrawork/SKILL.md#Role` | condensed-core |
| R2 | Validate user-visible features through their actual runtime interfaces rather than treating automated unit tests as sole proof of completion. | Shipping broken interfaces that satisfy isolated mock tests while failing in real-world execution. | `assets/omo-latest/skills/ultrawork/SKILL.md#Goal` | core |
| R65 | Base task completion on empirical runtime behavior and observed execution artifacts rather than static code inspection. | Declaring deliverables complete based on wishful reading without verifying runtime execution. | `assets/omo-latest/skills/ultrawork/SKILL.md#Constraints` | core |
| R66 | Remedy all regressions, broken tests, and stale documentation introduced within the blast radius of modified components during the current run. | Leaving secondary breakages or inconsistencies behind in touched files. | `assets/omo-latest/skills/ultrawork/SKILL.md#Constraints` | core |
| R67 | Prohibit silencing compiler errors, lint warnings, or test suites through test skipping, commenting out assertions, or deleting checks. | Faking successful test suites by eliminating failing validation logic. | `assets/omo-latest/skills/ultrawork/SKILL.md#Constraints` | core |
| R68 | Require tangible run evidence for all completion assertions before closing tasks. | Fabricating success claims that lack verifiable execution output. | `assets/omo-latest/skills/ultrawork/SKILL.md#Constraints` | core |

### 1.2 Task Sizing and Ceremony Tiers

| Rule | Behavioral Requirement | Failure Prevented | MIT Reference | Disposition |
| :--- | :--- | :--- | :--- | :--- |
| R3 | Classify incoming work into lightweight execution for localized routines or heavyweight execution for structural and security boundaries. | Imposing excessive planning overhead on minor fixes or insufficient rigor on architectural modifications. | `assets/omo-latest/skills/ultrawork/SKILL.md:25 (Tier triage)` | core |
| R4 | Start by defaulting to lightweight sizing; escalate to heavyweight upon discovering complex boundary conditions, but never downgrade mid-execution. | Dropping required verification standards halfway through complex multi-step tasks. | `assets/omo-latest/skills/ultrawork/SKILL.md:25 (Tier triage)` | core |
| R5 | Classify control-plane tasks that launch or steer external sessions as lightweight regardless of the size of the delegated workload. | Treating coordinator and orchestration prompts as massive implementation projects. | `assets/omo-latest/skills/ultrawork/SKILL.md:25 (Tier triage)` | condensed-core |

### 1.3 Lifecycle Initialization and Contract Precedence

| Rule | Behavioral Requirement | Failure Prevented | MIT Reference | Disposition |
| :--- | :--- | :--- | :--- | :--- |
| R12 | When an outer runner (a loop runner or a workflow engine) already owns goal and plan state for the run, the contract's own goal, plan and checklist setup steps are skipped in favour of that runner's state. | Two competing sources of truth for the same run's goal and progress. | none in MIT text; see correction 14 in section 2 | core |
| R13 | Allocate isolated per-task temporary directories and assign private compiler cache directories for process scratch work. | Worktree pollution and cache race conditions across concurrent processes. | `assets/omo-latest/skills/ultrawork/SKILL.md:114 (Task resources)` | core |
| R14 | Clean up task-owned temporary directories and private caches at conclusion while preserving shared global environments. | Accumulating disk clutter and dangling files across multiple runs. | `assets/omo-latest/skills/ultrawork/SKILL.md:114 (Task resources)` | core |
| R15 | Review installed skills and select applicable modules, loading skill content only when executing that specific workflow. | Loading unneeded instructions into memory or failing to utilize relevant specialized capabilities. | `assets/omo-latest/skills/ultrawork/SKILL.md:118 (Skill survey & context)` | core |
| R16 | Survey repository architecture and prior state before editing to establish the target operational outcome. | Jumping into code modifications before understanding existing system boundaries. | `assets/omo-latest/skills/ultrawork/SKILL.md:118 (Skill survey & context)` | core |
| R18 | Register concrete goal objectives with unambiguous success conditions and observable stop criteria prior to structural changes. | Drifting off-target due to unstated requirements and vague completion criteria. | `assets/omo-latest/skills/ultrawork/SKILL.md:140 (Goal creation)` | core |
| R19 | The goal tool's `blocked` status is reserved for an obstacle the run cannot get past on its own; a run that is merely waiting for an outcome it has arranged to be told about keeps the goal active and ends the turn. | Goals marked blocked by a run that is only waiting, which stops goal-driven continuation for no reason. | none in MIT text (`blocked` semantics are an mhc goal-tool fact) | core |
| R28 | Work with more than a handful of steps has its ordered plan saved under `.omo/plans/` (or the run's notes) before the first edit, and the checklist tool then mirrors that saved plan. | Plan intent existing only in the context window and disappearing at compaction. | `assets/omo-latest/skills/ultrawork/SKILL.md#Commits` (names the `.omo/plans/<slug>.md` location) | core |

### 1.4 Active Execution and Verification Loop

| Rule | Behavioral Requirement | Failure Prevented | MIT Reference | Disposition |
| :--- | :--- | :--- | :--- | :--- |
| R43 | Read existing tests and verify current behavior prior to modifying any source code. | Overwriting existing contracts without understanding baseline invariants. | `assets/omo-latest/skills/ultrawork/SKILL.md#Execution loop (READ → CHANGE → RUN → CLEAN)` | core |
| R44 | Apply minimal targeted changes that satisfy criteria without introducing unrequested abstractions. | Scope explosion and unnecessary code complexity. | `assets/omo-latest/skills/ultrawork/SKILL.md#Execution loop (READ → CHANGE → RUN → CLEAN)` | core |
| R45 | Author automated tests only when existing suites do not catch regressions, avoiding tests that merely pin implementation constants. | Fragile, useless tests that duplicate code rather than guarding behavior. | `assets/omo-latest/skills/ultrawork/SKILL.md#Execution loop (READ → CHANGE → RUN → CLEAN)` | core |
| R46 | Execute end-to-end operational scenarios exercising changed surfaces directly with captured evidence. | Passing internal unit assertions while the actual exposed command or endpoint fails. | `assets/omo-latest/skills/ultrawork/SKILL.md#Execution loop (READ → CHANGE → RUN → CLEAN)` | core |
| R47 | Terminate all spawned test processes, listener ports, containers, and temporary drivers upon completing scenario verification. | Leaving leaked background processes, occupied ports, and orphaned test daemons behind. | `assets/omo-latest/skills/ultrawork/SKILL.md#Execution loop (READ → CHANGE → RUN → CLEAN)` | core |
| R48 | Validate that semantic language diagnostics are clean across all modified files before concluding verification. | Leaving subtle type errors, unimported symbols, or broken syntax in touched files. | `assets/omo-latest/skills/ultrawork/SKILL.md#Execution loop (READ → CHANGE → RUN → CLEAN)` | core |
| R49 | Re-evaluate tests and scenarios affecting touched files after each unit of change, executing full validation prior to final delivery. | Undetected regression cascades across related workspace modules. | `assets/omo-latest/skills/ultrawork/SKILL.md#Execution loop (READ → CHANGE → RUN → CLEAN)` | core |

### 1.5 State Tracking and Checklist Management

| Rule | Behavioral Requirement | Failure Prevented | MIT Reference | Disposition |
| :--- | :--- | :--- | :--- | :--- |
| R29 | Synchronize task progression with the checklist tool in real time, updating item states immediately as work transitions. | State lag where checklist status is batched or disconnected from reality. | `assets/omo-latest/skills/ultrawork/SKILL.md#3. Register obsessive todos via todo` | core |
| R30 | Register cleanup and teardown tasks at the moment temporary resources or background processes are created. | Forgetting to release resources at the conclusion of multi-step workflows. | `assets/omo-latest/skills/ultrawork/SKILL.md#3. Register obsessive todos via todo` | core |
| R31 | Structure checklist item descriptions with target paths, clear rationale, and specific verification checks. | Vague checklist tasks that lack actionable success criteria. | `assets/omo-latest/skills/ultrawork/SKILL.md#3. Register obsessive todos via todo` | condensed-core |

### 1.6 Asynchronous Waiting and Event Monitoring

| Rule | Behavioral Requirement | Failure Prevented | MIT Reference | Disposition |
| :--- | :--- | :--- | :--- | :--- |
| R50 | The agent learns that a long operation has finished from a pushed event, never by repeating status calls or sleeping. | Every repeated status call re-sends the whole context and burns a model round for no new information. | `assets/omo-latest/skills/ultrawork/SKILL.md#Waiting discipline (a poll costs a full model round)` | core |
| R51 | Any operation whose result a later decision depends on has a completion notification arranged for it before the agent moves on to other work. | A long operation finishes unnoticed because nothing was listening for it. | `assets/omo-latest/skills/ultrawork/SKILL.md#Waiting discipline (a poll costs a full model round)` | reference:qa-channels |
| R52 | Reading a background job's partial output is allowed only when a decision in the middle of the job depends on it. | Using output reads as a disguised wait loop. | `assets/omo-latest/skills/ultrawork/SKILL.md#Delegation on mhc` (one midpoint peek) | condensed-core |

### 1.7 Code Evaluation and Concurrent Execution

| Rule | Behavioral Requirement | Failure Prevented | MIT Reference | Disposition |
| :--- | :--- | :--- | :--- | :--- |
| R36 | Run independent read operations and queries concurrently within code evaluation environments. | High latency from sequential round-trip tool invocations for independent lookups. | `assets/omo-latest/skills/ultrawork/SKILL.md:233 (Discovery tools)` | core |
| R37 | Utilize built-in runtime capabilities for filesystem scanning, process execution, and network fetches. | Spawning shell subprocesses for operations natively supported in the evaluation engine. | `assets/omo-latest/skills/ultrawork/SKILL.md:233 (Discovery tools)` | condensed-core |
| R38 | Parallelize independent queries while chaining dependent operations sequentially within evaluation code. | Blocked lookups or race conditions between interdependent queries. | `assets/omo-latest/skills/ultrawork/SKILL.md:233 (Discovery tools)` | core |
| R39 | Calls that change files, the environment or remote state are issued singly, and each outcome is read before the next such call. | One failed write silently corrupting the edits queued behind it. | none in MIT text | core |

### 1.8 Semantic and Structural Code Discovery

| Rule | Behavioral Requirement | Failure Prevented | MIT Reference | Disposition |
| :--- | :--- | :--- | :--- | :--- |
| R32 | Query symbol definitions, references, and diagnostics via semantic language server capabilities rather than plain text searches. | Broken refactors and missed symbol occurrences caused by naive regex matching. | `assets/omo-latest/skills/ultrawork/SKILL.md:233 (Discovery tools)` | core |
| R33 | Perform structural code search and AST transformations using syntax tree inspection tools. | Fragile text replacements that misinterpret nested syntax structures. | `assets/omo-latest/skills/ultrawork/SKILL.md:233 (Discovery tools)` | core |
| R34 | Use fast content search and directory globbing tools for textual patterns, paths, and version history. | Sluggish codebase traversal using manual line-by-line reading. | `assets/omo-latest/skills/ultrawork/SKILL.md:233 (Discovery tools)` | condensed-core |

### 1.9 Interaction Protocol and Completion Criteria

| Rule | Behavioral Requirement | Failure Prevented | MIT Reference | Disposition |
| :--- | :--- | :--- | :--- | :--- |
| R60 | Conduct systematic self-review verifying diffs, diagnostics, and test evidence when external review is not triggered. | Shipping uninspected modifications when running without independent reviewer gates. | `assets/omo-latest/skills/ultrawork/SKILL.md#Verification gate (TRIGGERED ONLY ON EXPLICIT DEMAND)` | core |
| R70 | Provide a brief summary of the intended approach after completing initial setup. | Leaving users uninformed about the planned execution sequence. | `assets/omo-latest/skills/ultrawork/SKILL.md#Output discipline` | condensed-core |
| R71 | Structure progress reports at phase transitions using standardized handoff summaries without intermediate noise. | Unstructured updates and fragmented status reports scattered throughout execution. | `assets/omo-latest/skills/ultrawork/SKILL.md#Output discipline` | core |
| R72 | Present completion summaries that document verified criteria, captured evidence paths, and clean repository state. | Vague declarations of success lacking evidence references or verification details. | `assets/omo-latest/skills/ultrawork/SKILL.md#Output discipline` | core |
| R73 | Complete execution immediately when the user's inquiry or requested deliverable is satisfied by available evidence. | Unnecessary execution loops, extraneous verification passes, and post-completion scope creep. | `assets/omo-latest/skills/ultrawork/SKILL.md#Stop rules` | core |
| R74 | Verify that all success conditions are satisfied and all teardown receipts are confirmed before declaring completion. | Prematurely stopping work while criteria remain incomplete or resources uncleaned. | `assets/omo-latest/skills/ultrawork/SKILL.md#Stop rules` | core |
| R75 | Prohibit declaring completion while background servers, ports, or temporary directories remain uncleaned. | Leaving active listeners, leaked processes, and dirty workspace state. | `assets/omo-latest/skills/ultrawork/SKILL.md#Stop rules` | core |
| R76 | Consult the user for clarification after two consecutive failed attempts at resolving a specific step. | Entering endless retry loops without pausing to seek human guidance. | `assets/omo-latest/skills/ultrawork/SKILL.md#Stop rules` | core |
| R77 | Discontinue background exploration when two successive search waves produce no novel insights. | Consuming excessive tokens in unbounded exploration loops that yield no new information. | `assets/omo-latest/skills/ultrawork/SKILL.md#Stop rules` | core |

### 1.10 Specialized Reference Topics (On-Demand)

| Rule | Behavioral Requirement | Failure Prevented | MIT Reference | Disposition |
| :--- | :--- | :--- | :--- | :--- |
| R6 | Execute protocol-level verification for HTTP endpoints, validating status codes, headers, and response bodies. | Mock-only validation that overlooks networking headers, encoding issues, or endpoint routing flaws. | `assets/omo-latest/skills/ultrawork/SKILL.md#Manual-QA channels` | reference:qa-channels |
| R7 | Capture truecolor terminal output and inspect glyph rendering via browser-backed pseudoterminal capture utilities. | Truncated terminal output, corrupted ANSI colors, or broken multibyte character layouts. | `assets/omo-latest/skills/ultrawork/SKILL.md#Manual-QA channels` | reference:tui-visual-qa |
| R8 | Drive real browser sessions without clearing existing user authentication cookies or profile storage. | Invalidation of user logins or testing against incomplete browser environments. | `assets/omo-latest/skills/ultrawork/SKILL.md#Manual-QA channels` | reference:qa-channels |
| R9 | Exercise desktop graphical interfaces through native operating system automation tools. | Testing desktop applications with simulated text dumps that fail to exercise user controls. | `assets/omo-latest/skills/ultrawork/SKILL.md#Manual-QA channels` | reference:qa-channels |
| R10 | Define concrete test inputs, execution commands, and binary pass/fail observables before running manual proofs. | Subjective manual testing that lacks definitive pass criteria. | `assets/omo-latest/skills/ultrawork/SKILL.md#Manual-QA channels` | reference:qa-channels |
| R11 | Accept structured command-line output, schema diffs, or database records as primary evidence for non-GUI components. | Forcing visual screenshot requirements onto headless background libraries and CLI tools. | `assets/omo-latest/skills/ultrawork/SKILL.md#Manual-QA channels` | reference:qa-channels |
| R17 | Spawn dedicated planning subagents only when architecture boundaries or multi-path decompositions remain uncertain. | Over-engineering straightforward procedural tasks with unnecessary planning delegates. | `assets/omo-latest/skills/ultrawork/SKILL.md:118 (Skill survey & context)` | reference:delegation-and-teams |
| R35 | Dispatch parallel exploration subagents for wide architecture surveys that exceed single-turn context limits. | Overwhelming parent session context with extensive file reads during discovery. | `assets/omo-latest/skills/ultrawork/SKILL.md:233 (Discovery tools)` | reference:delegation-and-teams |
| R40 | Visual deliverables are captured as images after each change and checked at both a narrow (phone) and a wide (desktop) width before the next change. | Layout breakage at one width shipping unnoticed. | none in MIT text | reference:qa-channels |
| R42 | Assign disjoint non-overlapping file scopes to parallel worker subagents. | Merge conflicts and clobbered changes caused by concurrent edits to identical files. | `assets/omo-latest/skills/ultrawork/SKILL.md#Delegation on mhc` | reference:delegation-and-teams |
| R53 | Dispatch background workers by specifying task categories or discrete agent types. | Uncoordinated worker delegation without defined capability scopes. | `assets/omo-latest/skills/ultrawork/SKILL.md#Delegation on mhc` | reference:delegation-and-teams |
| R54 | Restrict analytical, exploration, and advisory agents to read-only execution modes. | Unintended workspace modifications performed by background research agents. | `assets/omo-latest/skills/ultrawork/SKILL.md#Delegation on mhc` | reference:delegation-and-teams |
| R55 | Multi-agent efforts whose lanes depend on each other share one task board and exchange status through messages rather than ad-hoc polling. | Chaos in complex multi-agent tasks lacking central task tracking and communication. | none in MIT text (team tools are an mhc interface fact) | reference:delegation-and-teams |
| R56 | Initialize subagent tasks with explicit assignments, boundaries, deliverables, and stop criteria. | Open-ended worker execution that wastes resources or wanders off-scope. | `assets/omo-latest/skills/ultrawork/SKILL.md#Subagent-dependent transition barrier` | reference:delegation-and-teams |
| R57 | Treat subagent status updates as progress; handle silent workers with targeted follow-up before cancellation. | Prematurely killing active workers that are making progress on intensive tasks. | `assets/omo-latest/skills/ultrawork/SKILL.md#Subagent-dependent transition barrier` | reference:delegation-and-teams |
| R58 | Ensure all active child tasks reach terminal states before completing dependent milestones or concluding parent execution. | Leaving orphaned workers running in background after parent task completion. | `assets/omo-latest/skills/ultrawork/SKILL.md#Subagent-dependent transition barrier` | reference:delegation-and-teams |
| R59 | A separate reviewer agent runs in exactly two situations: the user asked for a strict review, or the work is heavyweight and covered by an approved plan file; every other run does a self-review. | Paying for a full reviewer run on work that does not warrant it. | `assets/omo-latest/skills/ultrawork/SKILL.md#Verification gate (TRIGGERED ONLY ON EXPLICIT DEMAND)` | reference:review-gate |
| R61 | Remediate concrete reviewer concerns tied to success criteria, requesting up to two re-reviews on delta diffs. | Review cycles spinning indefinitely on stylistic disagreements or vague suggestions. | `assets/omo-latest/skills/ultrawork/SKILL.md#Verification gate (TRIGGERED ONLY ON EXPLICIT DEMAND)` | reference:review-gate |
| R62 | Reviewer blockers still open after the re-review allowance go to the user as a choice through the ask-user tool (`ask_user_question` or `request_user_input`). | Worker and reviewer cycling forever with no one empowered to decide. | `assets/omo-latest/skills/ultrawork/SKILL.md#Verification gate (TRIGGERED ONLY ON EXPLICIT DEMAND)` | reference:review-gate |
| R64 | Format commit messages according to conventional commit standards matching project history when commits are authorized. | Inconsistent, uninformative commit histories that break repository changelog conventions. | `assets/omo-latest/skills/ultrawork/SKILL.md#Commits` | reference:commits |

### 1.11 Deprecated and Defective Mechanisms

| Rule | Behavioral Requirement | Reason for Removal | MIT Reference | Disposition |
| :--- | :--- | :--- | :--- | :--- |
| R20 | Create a scratchpad markdown file in temporary storage for tracking progress. | Superseded by authoritative goal registration and checklist tool state. Generating an auxiliary notepad creates duplicate state synchronization problems. | `assets/omo-latest/skills/ultrawork/SKILL.md#2. Open the durable notepad` | delete-wrong |
| R21 | Maintain temporary notepad headers with timestamps and goals. | Obsolete tracking format tied to the removed scratchpad file mechanism. | `assets/omo-latest/skills/ultrawork/SKILL.md#Ultrawork Notepad — <one-line goal>` | delete-wrong |
| R22 | Maintain an exhaustively detailed step plan inside the scratchpad file. | Redundant with persisted plan files in project storage (`.omo/plans/`) and checklist items. | `assets/omo-latest/skills/ultrawork/SKILL.md#Plan (exhaustively detailed)` | delete-wrong |
| R23 | Duplicate goal success criteria inside the scratchpad file. | Redundant duplication of authoritative goal state maintained by the goal management tool. | `assets/omo-latest/skills/ultrawork/SKILL.md#Success criteria + QA scenarios` | delete-wrong |
| R24 | Record active step pointer within the scratchpad file. | Duplicates the active cursor inherently maintained by the task checklist tool. | `assets/omo-latest/skills/ultrawork/SKILL.md#Now` | delete-wrong |
| R25 | List remaining steps inside the scratchpad file. | Duplicates the live task queue maintained by the task checklist tool. | `assets/omo-latest/skills/ultrawork/SKILL.md#Todo` | delete-wrong |
| R26 | Record ad-hoc findings into the scratchpad file. | Context recovery relies on conversation history and persisted project plan artifacts. | `assets/omo-latest/skills/ultrawork/SKILL.md#Findings` | delete-wrong |
| R27 | Log retrospective lessons into the scratchpad file. | Unused ceremony that clutters context without machine-consumed utility. | `assets/omo-latest/skills/ultrawork/SKILL.md#Learnings` | delete-wrong |
| R41 | A fallback that moved code evaluation to a second-language kernel while the primary kernel was occupied. | Only a JavaScript/Bun kernel exists in this runtime, so the fallback names a surface that is never present. | none in MIT text; see correction 10 in section 2 | delete-wrong |
| R63 | Author frequent atomic git commits for each incremental unit of change. | Directly contradicts core policy prohibiting automated commits unless explicitly requested or authorized by the user. | `assets/omo-latest/skills/ultrawork/SKILL.md#Commits` | delete-wrong |
| R69 | Emit a hardcoded mode banner line at the beginning of every turn. | Directly contradicts the preset intent gate routing line ("I read this as [intent] - [plan]. I'll stop when [condition]."). | `assets/omo-latest/skills/ultrawork/SKILL.md#Output discipline` | delete-wrong |

---

## 2. Mandatory Architectural Corrections

The clean-room contract incorporates the following fourteen structural corrections over legacy patterns:

1. **Elimination of the Temporary Notepad Scratchpad**: Authoritative state resides exclusively in the registered goal and the live checklist tool. Writing, formatting, and re-reading markdown scratchpad files in `/tmp` is eliminated.
2. **Removal of the Mandatory Greeting Line**: The contract must not mandate emitting an artificial greeting or banner string on the first turn.
3. **Preservation of Intent Gate Routing**: The preset intent gate routing line (`I read this as [intent] - [plan]. I'll stop when [observable condition].`) remains the mandatory opening sentence for every turn.
4. **Direct Answers for Information and Judgment Requests**: Queries asking for explanations, code locations, assessments, or architectural reviews are answered immediately with zero goal creation, no checklist tool initialization, and no editing actions.
5. **Lightweight Work Governed by Checklist Only**: Localized bug fixes and routine single-spot modifications utilize the checklist tool for execution tracking without requiring goal registration or standalone plan documents.
6. **Commits Governed Strictly by User Permission**: Git commits are created only when the user has explicitly authorized or requested them. In the absence of explicit permission, modifications remain uncommitted.
7. **Two-Tiered Verification Economy**: Fast local checks (file diagnostics and targeted tests) run after each editing step. Heavy validation (workspace compilation and integration suites) executes once across the merged changeset prior to delivery. External continuous integration failures are batched and addressed together rather than iterating run-by-run.
8. **Subscription-Based Event Monitoring**: Asynchronous waiting is conducted exclusively via event monitor subscriptions (`tool.monitor`), strictly banning blocking sleeps, poll loops, or watching subagents.
9. **Dual-Trigger Independent Review Gate**: Third-party review agents are triggered only when either (a) the user explicitly requests strict/proper review, or (b) an approved plan file exists for a heavyweight task. In all other scenarios, systematic self-review is performed.
10. **Exclusion of Non-Existent Python Kernel**: Code evaluation guidance refers exclusively to the JavaScript/Bun execution environment. Guidance mentioning Python kernel hopping is eliminated.
11. **Bundled Tool for Terminal Visual QA**: Terminal capture and text interface validation utilize the bundled capture script provided by the visual QA skill (`scripts/tui-capture.mjs`) rather than nonexistent repository scripts.
12. **Standardized User Question Tool Names**: Guidance for consulting the human user specifies the actual registered tool interfaces (`ask_user_question` or `request_user_input`).
13. **Compact Delegation Reports and Falsification**: Subagent deliverables are restricted to structured claim tables of at most 3,500 characters. Delegated findings undergo an adversarial falsification check before being integrated by the coordinator.
14. **Outer Orchestration Precedence**: When operating under an active execution loop or workflow runner, the outer orchestrator's state automatically supersedes bootstrap requirements.

---

## 3. Reference Topics and Trigger Conditions

The contract partitions specialized situational instructions into modular reference documents loaded on demand:

| Topic Identifier | Covered Capabilities | Trigger Condition |
| :--- | :--- | :--- |
| `qa-channels` | Concrete execution protocols for HTTP endpoint testing (curl/fetch), browser automation via browser drivers, desktop GUI interaction, and auxiliary evidence formats (CLI dumps, diffs). | The task deliverable involves testing external network endpoints, web applications, desktop software, or non-CLI interfaces. |
| `delegation-and-teams` | Subagent dispatch parameters, team workspaces, disjoint file partitioning, claim-table reporting formats (<=3,500 chars), and falsification waves. | The task requires coordinating multiple concurrent background subagents or collaborating through team structures. |
| `commits` | History matching conventions, Conventional Commit message formatting, and metadata footers. | The user has granted explicit permission to create git commits, and a verified unit of work is ready for committing. |
| `review-gate` | Independent reviewer prompt specification, blocker categorization against criteria, two-attempt delta re-review cycles, and user escalation dialogs. | An approved plan file exists for heavyweight work, or the user has explicitly demanded formal review. |
| `tui-visual-qa` | Pseudoterminal execution, truecolor rendering capture via `scripts/tui-capture.mjs`, wide-glyph validation, and terminal screenshot verification. | The deliverable adds or modifies command-line user interfaces, terminal formatting, or interactive console menus. |

---

## 4. Size Budget and Contract Constraints

- **Core Contract Size Target**: 14,000 to 16,000 UTF-8 bytes (14–16 KB), roughly 50% smaller than the 31,542-byte baseline.
- **Build-Time Size Assertion**: The build harness must assert that the injected core contract does not exceed the measured size of the authored text plus a 10% ceiling.
- **Reference Document Budget**: Individual on-demand reference modules must not exceed 6,000 UTF-8 bytes each, keeping overall context overhead strictly bounded when loaded.

---

## 5. Behavioral Scenarios

The contract must produce the expected observable behaviors across the five standard evaluation scenarios:

### Scenario S1: Read-Only Information Inquiry
- **Input Prompt**: `ulw where is fn add defined?`
- **Expected Behavior**: The agent inspects code using symbol lookups or text search and replies immediately with the exact file path and line location. It does not invoke goal registration, does not initialize the checklist tool, executes no file modifications, and launches no subagents.

### Scenario S2: Lightweight Single-Spot Fix
- **Input Prompt**: `ulw fix the failing test in math.rs`
- **Expected Behavior**: The agent sizes the task as lightweight and initializes tracking via the checklist tool without creating goal artifacts or standalone plan files. It inspects existing tests, reproduces the failure, applies the minimal targeted edit, verifies the test passes, performs self-review, and concludes with a clean workspace. No external reviewer is summoned.

### Scenario S3: Heavyweight Multi-File Feature
- **Input Prompt**: `ulw add a --verbose flag across cli.rs and lib.rs with tests`
- **Expected Behavior**: The agent classifies the work as heavyweight, defines explicit success criteria covering normal and edge cases, tracks progress via the checklist tool, updates affected files, ensures language server diagnostics remain clean, and executes tests. An external reviewer is invoked only if an approved plan file exists or the user explicitly demanded review; otherwise thorough self-review is recorded.

### Scenario S4: Delegated Multi-Agent Audit
- **Input Prompt**: `ulw audit this repo for unused functions using parallel subagents`
- **Expected Behavior**: The agent launches concurrent background subagents with strictly partitioned non-overlapping scopes. Each subagent returns a concise claim table under 3,500 characters. The parent coordinates an adversarial falsification pass to verify candidate findings before synthesizing final results, avoiding parent context explosion.

### Scenario S5: Asynchronous Long-Running Command
- **Input Prompt**: `ulw run ./slow.sh (sleeps 90s) and report its output`
- **Expected Behavior**: The agent starts the script and registers an event monitor (`tool.monitor`) instead of blocking the turn with sleep commands or polling loops. The agent concludes its turn, wakes automatically upon the process completion event, and reports the final execution output and cleanup status.
