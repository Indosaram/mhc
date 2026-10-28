# Config key inventory (`omo.jsonc`)

Companion to [config.md](config.md). This is the complete list of keys a user may write, with
type, bounds and default. It is an **interface fact sheet**: key names, value types, enumerated
values and defaults are what existing user files contain, so a compatible loader must accept
exactly these. It was derived mechanically (names/types/defaults/bounds only, no descriptions)
and re-grouped; regenerate the JSON Schema (`assets/omo.schema.json`) from this list in Phase 4.

Notation: `path : type [default=…] [REQUIRED]`. `<name>` is a user-chosen map key; `[]` is a list
element. `string or object` means either form is accepted. A default is applied only when the
enclosing object is present (config.md §6.4), except `task`, which is always materialised.
Bounds: `minimum`/`maximum` inclusive, `exclusiveMinimum=0` means "> 0", `pattern` is a regex the
whole string must match.

## Composition rules

- Top level: every section below, plus `$schema`, the harness blocks `[native]`, `[senpi]`
  (legacy spelling of `[native]`), `[codex]`, `[opencode]`, and `profiles`, `_migrations`,
  `legacy_migrations`. Unknown keys are removed with a diagnostic (config.md §4.4).
- `[native]` / `[senpi]` / `[codex]`: may contain `side_panel`, `formatOnMutation`, `gateway`,
  `categories`, `agents`, `git_master`, `task`, `teams`, `models`, `model_profiles`,
  `model_profile`, `memory`, `telemetry`, `computer`, `disabled_skills`, `mcp` (same shapes as top level,
  all optional, no defaults inside the block).
- `[opencode]`: object with arbitrary content (never interpreted by mhc).
- `profiles.<name>`: may contain the same keys as a typed harness block **except `gateway`**, plus
  the four harness blocks.
- Inside layers (single files) every field is optional and no defaults apply; required markers
  below apply to the merged view (and to list elements, which must be complete in one file).
- Model-bearing objects also accept the legacy fields listed with them; they are folded into the
  canonical fields on read (config.md §4.5).

Reasoning value (`reasoning`, `reasoningEffort`): one of `off|minimal|low|medium|high|xhigh|max|auto`
or any other string.

# Part A — sections used by Phase 1

## `model_profile`

Owner: Phase 1 (model-selection.md §6)

- `model_profile` : string

## `model_profiles`

Owner: Phase 1 (model-selection.md §6)

- `model_profiles` : map<string,object>
- `model_profiles.<name>.display_name` : string
- `model_profiles.<name>.family` : one of daily|geeky
- `model_profiles.<name>.tier` : one of normal|heavy
- `model_profiles.<name>.models` : array<string or object>
- `model_profiles.<name>.models[].model` : string REQUIRED
- `model_profiles.<name>.models[].reasoning` : one of off|minimal|low|medium|high|xhigh|max|auto or string
- `model_profiles.<name>.models[].temperature` : number (minimum=0, maximum=2)
- `model_profiles.<name>.models[].top_p` : number (minimum=0, maximum=1)
- `model_profiles.<name>.models[].max_tokens` : integer (exclusiveMinimum=0)
- `model_profiles.<name>.models[].provider_options` : map<string,any>
- `model_profiles.<name>.models[].variant` : string
- `model_profiles.<name>.models[].reasoningEffort` : one of off|minimal|low|medium|high|xhigh|max|auto or string
- `model_profiles.<name>.models[].thinking` : object
- `model_profiles.<name>.models[].thinking.type` : one of enabled|disabled REQUIRED
- `model_profiles.<name>.models[].thinking.budgetTokens` : number
- `model_profiles.<name>.models[].textVerbosity` : one of low|medium|high
- `model_profiles.<name>.models[].maxTokens` : number
- `model_profiles.<name>.models[].providerOptions` : map<string,any>

## `models`

Owner: Phase 1 (catalog, config.md §8)

- `models` : map<string,object>
- `models.<name>.model` : string REQUIRED
- `models.<name>.reasoning` : one of off|minimal|low|medium|high|xhigh|max|auto or string
- `models.<name>.variant` : string
- `models.<name>.reasoningEffort` : one of off|minimal|low|medium|high|xhigh|max|auto or string

## `categories`

Owner: Phase 1 (model fields) / Phase 2 task

- `categories` : map<string,object>
- `categories.<name>.description` : string
- `categories.<name>.model` : string
- `categories.<name>.models` : array<string or object>
- `categories.<name>.models[].model` : string REQUIRED
- `categories.<name>.models[].reasoning` : one of off|minimal|low|medium|high|xhigh|max|auto or string
- `categories.<name>.models[].temperature` : number (minimum=0, maximum=2)
- `categories.<name>.models[].top_p` : number (minimum=0, maximum=1)
- `categories.<name>.models[].max_tokens` : integer (exclusiveMinimum=0)
- `categories.<name>.models[].provider_options` : map<string,any>
- `categories.<name>.models[].variant` : string
- `categories.<name>.models[].reasoningEffort` : one of off|minimal|low|medium|high|xhigh|max|auto or string
- `categories.<name>.models[].thinking` : object
- `categories.<name>.models[].thinking.type` : one of enabled|disabled REQUIRED
- `categories.<name>.models[].thinking.budgetTokens` : number
- `categories.<name>.models[].textVerbosity` : one of low|medium|high
- `categories.<name>.models[].maxTokens` : number
- `categories.<name>.models[].providerOptions` : map<string,any>
- `categories.<name>.reasoning` : one of off|minimal|low|medium|high|xhigh|max|auto or string
- `categories.<name>.temperature` : number (minimum=0, maximum=2)
- `categories.<name>.top_p` : number (minimum=0, maximum=1)
- `categories.<name>.max_tokens` : integer (exclusiveMinimum=0)
- `categories.<name>.provider_options` : map<string,any>
- `categories.<name>.fallback_models` : string or array<string> or array<object> or array<string or object>
- `categories.<name>.variant` : string
- `categories.<name>.maxTokens` : number
- `categories.<name>.thinking` : object
- `categories.<name>.thinking.type` : one of enabled|disabled REQUIRED
- `categories.<name>.thinking.budgetTokens` : number
- `categories.<name>.reasoningEffort` : one of off|minimal|low|medium|high|xhigh|max|auto or string
- `categories.<name>.textVerbosity` : one of low|medium|high
- `categories.<name>.tools` : map<string,boolean>
- `categories.<name>.prompt_append` : string
- `categories.<name>.max_prompt_tokens` : integer (exclusiveMinimum=0)
- `categories.<name>.is_unstable_agent` : boolean
- `categories.<name>.disable` : boolean
- `categories.<name>.warn_unavailable` : boolean

## `agents`

Owner: Phase 1 (model fields) / Phase 2 task (execution fields)

- `agents` : map<string,object>
- `agents.<name>.description` : string
- `agents.<name>.prompt` : string
- `agents.<name>.model` : string
- `agents.<name>.models` : array<string or object>
- `agents.<name>.models[].model` : string REQUIRED
- `agents.<name>.models[].reasoning` : one of off|minimal|low|medium|high|xhigh|max|auto or string
- `agents.<name>.models[].temperature` : number (minimum=0, maximum=2)
- `agents.<name>.models[].top_p` : number (minimum=0, maximum=1)
- `agents.<name>.models[].max_tokens` : integer (exclusiveMinimum=0)
- `agents.<name>.models[].provider_options` : map<string,any>
- `agents.<name>.models[].variant` : string
- `agents.<name>.models[].reasoningEffort` : one of off|minimal|low|medium|high|xhigh|max|auto or string
- `agents.<name>.models[].thinking` : object
- `agents.<name>.models[].thinking.type` : one of enabled|disabled REQUIRED
- `agents.<name>.models[].thinking.budgetTokens` : number
- `agents.<name>.models[].textVerbosity` : one of low|medium|high
- `agents.<name>.models[].maxTokens` : number
- `agents.<name>.models[].providerOptions` : map<string,any>
- `agents.<name>.reasoning` : one of off|minimal|low|medium|high|xhigh|max|auto or string
- `agents.<name>.variant` : string
- `agents.<name>.reasoningEffort` : one of off|minimal|low|medium|high|xhigh|max|auto or string
- `agents.<name>.tools` : map<string,boolean>
- `agents.<name>.execution_mode` : one of in-process|process
- `agents.<name>.background` : boolean
- `agents.<name>.max_depth` : integer (minimum=0)
- `agents.<name>.allowed_subagents` : array<string>
- `agents.<name>.disallowed_tools` : array<string>
- `agents.<name>.max_turns` : integer (minimum=0)
- `agents.<name>.temperature` : number (minimum=0, maximum=2)
- `agents.<name>.disable` : boolean
- `agents.<name>.permission` : map<string,one of ask|allow|deny or map<string,one of ask|allow|deny>>
- `agents.<name>.prompt_append` : string

## `disabled_skills`

Owner: Phase 1 (config.md §9) / Phase 3 skill loader

- `disabled_skills` : array<string>

## `mcp`

Owner: Phase 3 skill/MCP step (mcp.md §3). Added by the MIT profile; not present in older files.

- `mcp` : object
- `mcp.builtins` : boolean [default=true]
- `mcp.disabled_builtins` : array<string>

## `profiles`

Owner: control key (config.md §3, §6)

- `profiles` : map<string,object>

## `[native]`

Owner: control key (config.md §3, §6)

- `[native]` : object

## `[senpi]`

Owner: control key (config.md §3, §6)

- `[senpi]` : object

## `[codex]`

Owner: control key (config.md §3, §6)

- `[codex]` : object

## `[opencode]`

Owner: control key (config.md §3, §6)

- `[opencode]` : map<string,any>

## `$schema`

Owner: Phase 1 (ignored)

- `$schema` : string

## `_migrations`

Owner: Phase 1 (config.md §12)

- `_migrations` : array<string>

## `legacy_migrations`

Owner: Phase 1 (preserved, not interpreted)

- `legacy_migrations` : map<string,any>


# Part B — sections owned by later phases (accept and validate in Phase 1)

## `task`

Owner: Phase 2 task (Phase 1 materialises defaults, config.md §6.4)

- `task` : object
- `task.isolation` : object default={"enabled": false, "backend": "auto", "apply": true, "merge": "patch", "commits": "generic"}
- `task.isolation.enabled` : boolean default=false
- `task.isolation.backend` : one of auto|apfs|btrfs|zfs|reflink|overlayfs|block-clone|rcopy default="auto"
- `task.isolation.apply` : boolean default=true
- `task.isolation.merge` : one of patch|branch default="patch"
- `task.isolation.commits` : one of generic|ai default="generic"
- `task.default_execution_mode` : one of auto|in-process|process default="auto"
- `task.process_runner` : one of host|child-process default="host"
- `task.host_engine_policy` : one of upgrade|fallback default="upgrade"
- `task.host_idle_exit_ms` : integer (exclusiveMinimum=0)
- `task.host_shard_prewarm` : one of off|first-turn|session-start default="first-turn"
- `task.default_concurrency` : integer (minimum=0) default=5
- `task.global_concurrency` : integer (minimum=0) default=8
- `task.provider_concurrency` : map<string,integer (minimum=0)>
- `task.model_concurrency` : map<string,integer (minimum=0)>
- `task.max_depth` : integer (minimum=0) default=1
- `task.residency_max_children` : integer (minimum=0) or const "unlimited" default="unlimited"
- `task.ttl_ms` : integer (exclusiveMinimum=0) default=86400000
- `task.resident_idle_timeout_ms` : integer (minimum=1, maximum=9007199254740991.0) default=900000
- `task.state_dir` : string
- `task.reattach_on_reconcile` : boolean
- `task.resume_children` : boolean default=true
- `task.warnings` : object default={"unavailable_categories": true}
- `task.warnings.unavailable_categories` : boolean default=true
- `task.wait` : object default={"min_ms": 5000, "default_ms": 60000, "max_ms": 600000}
- `task.wait.min_ms` : integer (exclusiveMinimum=0) default=5000
- `task.wait.default_ms` : integer (exclusiveMinimum=0) default=60000
- `task.wait.max_ms` : integer (exclusiveMinimum=0) default=600000
- `task.team` : object default={"max_members": 8, "max_parallel_members": 4, "max_wall_clock_minutes": 120}
- `task.team.max_members` : integer (minimum=1, maximum=8) default=8
- `task.team.max_parallel_members` : integer (minimum=1, maximum=8) default=4
- `task.team.max_wall_clock_minutes` : integer (exclusiveMinimum=0) default=120
- `task.dag` : object
- `task.dag.max_nodes_per_run` : integer (exclusiveMinimum=0) default=64
- `task.dag.max_runs_per_session` : integer (exclusiveMinimum=0) default=16
- `task.dag.subscriber_ring` : integer (exclusiveMinimum=0) default=1000
- `task.dag.heartbeat_ms` : integer (exclusiveMinimum=0) default=15000
- `task.dag.history_default_limit` : integer (exclusiveMinimum=0) default=256
- `task.dag.history_max_limit` : integer (exclusiveMinimum=0) default=1000
- `task.dag.retention_days` : integer (exclusiveMinimum=0) default=7
- `task.dag.max_prompt_bytes` : integer (exclusiveMinimum=0) default=262144

## `teams`

Owner: Phase 3 team mode

- `teams` : map<string,object>
- `teams.<name>.version` : const 1 default=1
- `teams.<name>.name` : string (pattern=^[a-z0-9-]+$)
- `teams.<name>.description` : string
- `teams.<name>.createdAt` : integer (minimum=1)
- `teams.<name>.leadAgentId` : string
- `teams.<name>.teamAllowedPaths` : array<string>
- `teams.<name>.sessionPermission` : string
- `teams.<name>.members` : array<object or object> REQUIRED
- members element, form 1 (`kind` = `"category"`):
  - `teams.<name>.members[].name` : string (pattern=^[a-z0-9-]+$) REQUIRED
  - `teams.<name>.members[].cwd` : string
  - `teams.<name>.members[].worktreePath` : string
  - `teams.<name>.members[].subscriptions` : array<string>
  - `teams.<name>.members[].backendType` : one of in-process|tmux default="in-process"
  - `teams.<name>.members[].color` : string
  - `teams.<name>.members[].isActive` : boolean default=true
  - `teams.<name>.members[].kind` : const "category" REQUIRED
  - `teams.<name>.members[].category` : string (minLength=1) REQUIRED
  - `teams.<name>.members[].prompt` : string (minLength=1) REQUIRED
- members element, form 2 (`kind` = `"subagent_type"`):
  - `teams.<name>.members[].name` : string (pattern=^[a-z0-9-]+$) REQUIRED
  - `teams.<name>.members[].cwd` : string
  - `teams.<name>.members[].worktreePath` : string
  - `teams.<name>.members[].subscriptions` : array<string>
  - `teams.<name>.members[].backendType` : one of in-process|tmux default="in-process"
  - `teams.<name>.members[].color` : string
  - `teams.<name>.members[].isActive` : boolean default=true
  - `teams.<name>.members[].kind` : const "subagent_type" REQUIRED
  - `teams.<name>.members[].subagent_type` : string (minLength=1) REQUIRED
  - `teams.<name>.members[].prompt` : string

## `memory`

Owner: Phase 2 memory

- `memory` : object
- `memory.enabled` : boolean default=true
- `memory.agent` : string (minLength=1) default="auto"
- `memory.projection` : object default={"max_entries_per_directory": 40, "max_bytes": 24576}
- `memory.projection.max_entries_per_directory` : integer (minimum=0) default=40
- `memory.projection.max_bytes` : integer (minimum=0) default=24576
- `memory.reflection` : object default={"enabled": true, "trigger": {"step_count": 25, "on_compaction": true}, "merge": "auto", "category": "quick", "timeout_minutes": 15, "sandbox": "auto"}
- `memory.reflection.enabled` : boolean default=true
- `memory.reflection.trigger` : object default={"step_count": 25, "on_compaction": true}
- `memory.reflection.trigger.step_count` : integer (minimum=0) default=25
- `memory.reflection.trigger.on_compaction` : boolean default=true
- `memory.reflection.merge` : one of auto|integration default="auto"
- `memory.reflection.category` : string (minLength=1) default="quick"
- `memory.reflection.timeout_minutes` : integer (exclusiveMinimum=0) default=15
- `memory.reflection.sandbox` : one of auto|required|off default="auto"
- `memory.nudge` : object default={"enabled": true, "every_user_turns": 10}
- `memory.nudge.enabled` : boolean default=true
- `memory.nudge.every_user_turns` : integer (minimum=1) default=10
- `memory.facts` : object default={"enabled": true, "debounce_settles": 4}
- `memory.facts.enabled` : boolean default=true
- `memory.facts.debounce_settles` : integer (minimum=1) default=4
- `memory.facts.model` : string (minLength=1)
- `memory.dream` : object default={"enabled": true, "idle_minutes": 30, "min_hours_between": 24, "shutdown_launch": true, "auto_select_max": 5, "auto_select_max_chars": 150000}
- `memory.dream.enabled` : boolean default=true
- `memory.dream.idle_minutes` : integer (minimum=0) default=30
- `memory.dream.min_hours_between` : integer (minimum=1) default=24
- `memory.dream.shutdown_launch` : boolean default=true
- `memory.dream.auto_select_max` : integer (minimum=1, maximum=10) default=5
- `memory.dream.auto_select_max_chars` : integer (minimum=10000) default=150000
- `memory.people` : object default={"enabled": false, "max_entries": 40, "max_entry_chars": 200}
- `memory.people.enabled` : boolean default=false
- `memory.people.max_entries` : integer (minimum=1, maximum=100) default=40
- `memory.people.max_entry_chars` : integer (minimum=50, maximum=500) default=200
- `memory.agreements` : object default={"enabled": true, "needs_human_threshold": 3, "max_items": 64}
- `memory.agreements.enabled` : boolean default=true
- `memory.agreements.needs_human_threshold` : integer (minimum=1, maximum=9007199254740991.0) default=3
- `memory.agreements.max_items` : integer (minimum=8, maximum=256) default=64
- `memory.soul` : object default={"edit_notice": true}
- `memory.soul.edit_notice` : boolean default=true
- `memory.write_notice` : object default={"enabled": true}
- `memory.write_notice.enabled` : boolean default=true
- `memory.sync` : object default={"enabled": true}
- `memory.sync.remote` : string (minLength=1)
- `memory.sync.enabled` : boolean default=true
- `memory.search` : object default={"enabled": true}
- `memory.search.enabled` : boolean default=true
- `memory.recall` : object default={"enabled": true, "max_items": 2, "category": "quick", "event_caps": {"tool_args": 400, "result_head": 600, "assistant": 1500, "prompt": 4000}, "sidecar_max_tokens": 48000, "max_concurrent_wakes": 2, "tool_budget": 8, "query_expansion": false}
- `memory.recall.enabled` : boolean default=true
- `memory.recall.max_items` : integer (minimum=1, maximum=5) default=2
- `memory.recall.category` : string (minLength=1) default="quick"
- `memory.recall.event_caps` : object default={"tool_args": 400, "result_head": 600, "assistant": 1500, "prompt": 4000}
- `memory.recall.event_caps.tool_args` : integer (minimum=0) default=400
- `memory.recall.event_caps.result_head` : integer (minimum=0) default=600
- `memory.recall.event_caps.assistant` : integer (minimum=0) default=1500
- `memory.recall.event_caps.prompt` : integer (minimum=0) default=4000
- `memory.recall.sidecar_max_tokens` : integer (exclusiveMinimum=0) default=48000
- `memory.recall.max_concurrent_wakes` : integer (exclusiveMinimum=0) default=2
- `memory.recall.tool_budget` : integer (exclusiveMinimum=0) default=8
- `memory.recall.query_expansion` : boolean default=false
- `memory.compile_warn_tokens` : integer (exclusiveMinimum=0) default=30000
- `memory.agents` : map<string,object> default={}
- `memory.agents.<name>.projection` : object
- `memory.agents.<name>.projection.max_entries_per_directory` : integer (minimum=0)
- `memory.agents.<name>.projection.max_bytes` : integer (minimum=0)
- `memory.agents.<name>.enabled` : boolean
- `memory.agents.<name>.agent` : string (minLength=1)
- `memory.agents.<name>.reflection` : object
- `memory.agents.<name>.reflection.enabled` : boolean
- `memory.agents.<name>.reflection.trigger` : object
- `memory.agents.<name>.reflection.trigger.step_count` : integer (minimum=0)
- `memory.agents.<name>.reflection.trigger.on_compaction` : boolean
- `memory.agents.<name>.reflection.merge` : one of auto|integration
- `memory.agents.<name>.reflection.category` : string (minLength=1)
- `memory.agents.<name>.reflection.timeout_minutes` : integer (exclusiveMinimum=0)
- `memory.agents.<name>.reflection.sandbox` : one of auto|required|off
- `memory.agents.<name>.nudge` : object
- `memory.agents.<name>.nudge.enabled` : boolean
- `memory.agents.<name>.nudge.every_user_turns` : integer (minimum=1)
- `memory.agents.<name>.facts` : object
- `memory.agents.<name>.facts.enabled` : boolean
- `memory.agents.<name>.facts.debounce_settles` : integer (minimum=1)
- `memory.agents.<name>.facts.model` : string (minLength=1)
- `memory.agents.<name>.dream` : object
- `memory.agents.<name>.dream.enabled` : boolean
- `memory.agents.<name>.dream.idle_minutes` : integer (minimum=0)
- `memory.agents.<name>.dream.min_hours_between` : integer (minimum=1)
- `memory.agents.<name>.dream.shutdown_launch` : boolean
- `memory.agents.<name>.dream.auto_select_max` : integer (minimum=1, maximum=10)
- `memory.agents.<name>.dream.auto_select_max_chars` : integer (minimum=10000)
- `memory.agents.<name>.people` : object
- `memory.agents.<name>.people.enabled` : boolean
- `memory.agents.<name>.people.max_entries` : integer (minimum=1, maximum=100)
- `memory.agents.<name>.people.max_entry_chars` : integer (minimum=50, maximum=500)
- `memory.agents.<name>.agreements` : object
- `memory.agents.<name>.agreements.enabled` : boolean
- `memory.agents.<name>.agreements.needs_human_threshold` : integer (minimum=1, maximum=9007199254740991.0)
- `memory.agents.<name>.agreements.max_items` : integer (minimum=8, maximum=256)
- `memory.agents.<name>.soul` : object
- `memory.agents.<name>.soul.edit_notice` : boolean
- `memory.agents.<name>.write_notice` : object
- `memory.agents.<name>.write_notice.enabled` : boolean
- `memory.agents.<name>.sync` : object
- `memory.agents.<name>.sync.remote` : string (minLength=1)
- `memory.agents.<name>.sync.enabled` : boolean
- `memory.agents.<name>.search` : object
- `memory.agents.<name>.search.enabled` : boolean
- `memory.agents.<name>.recall` : object
- `memory.agents.<name>.recall.enabled` : boolean
- `memory.agents.<name>.recall.max_items` : integer (minimum=1, maximum=5)
- `memory.agents.<name>.recall.category` : string (minLength=1)
- `memory.agents.<name>.recall.event_caps` : object
- `memory.agents.<name>.recall.event_caps.tool_args` : integer (minimum=0)
- `memory.agents.<name>.recall.event_caps.result_head` : integer (minimum=0)
- `memory.agents.<name>.recall.event_caps.assistant` : integer (minimum=0)
- `memory.agents.<name>.recall.event_caps.prompt` : integer (minimum=0)
- `memory.agents.<name>.recall.sidecar_max_tokens` : integer (exclusiveMinimum=0)
- `memory.agents.<name>.recall.max_concurrent_wakes` : integer (exclusiveMinimum=0)
- `memory.agents.<name>.recall.tool_budget` : integer (exclusiveMinimum=0)
- `memory.agents.<name>.recall.query_expansion` : boolean
- `memory.agents.<name>.compile_warn_tokens` : integer (exclusiveMinimum=0)

## `computer`

Owner: Phase 2 CUA (if kept)

- `computer` : object
- `computer.enabled` : boolean
- `computer.display` : string (minLength=1)
- `computer.max_width` : integer (exclusiveMinimum=0)
- `computer.max_height` : integer (exclusiveMinimum=0)
- `computer.screenshot_max_bytes` : integer (exclusiveMinimum=0)
- `computer.stop_hotkey` : string (minLength=1)
- `computer.allow_host_relay_only_stop` : boolean
- `computer.macos_canary` : one of session|off
- `computer.audit_log` : object
- `computer.audit_log.enabled` : boolean
- `computer.screenshot_gc` : object
- `computer.screenshot_gc.enabled` : boolean
- `computer.screenshot_gc.stale_ms` : integer (minimum=0)
- `computer.screenshot_gc.scan_interval_ms` : integer (minimum=0)
- `computer.engine_path` : string (minLength=1)
- `computer.cua_adapter` : boolean

## `telemetry`

Owner: Phase 2 telemetry (if kept)

- `telemetry` : object
- `telemetry.enabled` : boolean default=false

## `formatOnMutation`

Owner: Phase 2/3 formatter

- `formatOnMutation` : object
- `formatOnMutation.mode` : one of off|best-effort|required default="best-effort"
- `formatOnMutation.languages` : map<string,boolean>
- `formatOnMutation.maxFileBytes` : integer (exclusiveMinimum=0) default=1048576
- `formatOnMutation.timeoutMs` : integer (exclusiveMinimum=0) default=3000

## `git_master`

Owner: Phase 3 git attribution

- `git_master` : object
- `git_master.commit_footer` : boolean or string default=false
- `git_master.include_co_authored_by` : boolean default=false
- `git_master.git_env_prefix` : string default="GIT_MASTER=1"

## `gateway`

Owner: Phase 3 gateway (`crates/maho-gateway`, spec `gateway.md` §8)

- `gateway` : map<string,any>
- `gateway.enabled` : boolean default=true (read by `maho-gateway`)
- `gateway.poll_ms` : integer default=500, minimum 50 (read by `maho-gateway`)
- `gateway.max_hops` : integer default=3 (read by `maho-gateway`)
- other keys are accepted and ignored

## `setup`

Owner: Phase 3 first-run setup (`crates/maho-onboarding`, spec `onboarding.md` §5)

- `setup` : map<string,any> (top level only, like `gateway`)
- `setup.enabled` : boolean default=true (read by `maho-onboarding`)
- other keys are accepted and ignored

## `init_deep`

Owner: Phase 3 project context files (`crates/maho-context-files`, spec `init-deep.md` §6)

- `init_deep` : map<string,any> (top level only, like `gateway`)
- `init_deep.advise` : boolean default=true
- `init_deep.max_depth` : integer default=3, minimum 0
- `init_deep.max_dirs` : integer default=12, minimum 1
- `init_deep.min_files` : integer default=20, minimum 1
- other keys are accepted and ignored

## `side_panel`

Owner: Phase 3 (desktop/TUI side panel)

- `side_panel` : object
- `side_panel.enabled` : boolean default=false
- `side_panel.width` : integer (minimum=24, maximum=160) or a percentage string "10%" … "50%" (whole numbers) default="26%"
- `side_panel.min_columns` : integer (minimum=60, maximum=400) default=120
- `side_panel.clickable` : boolean default=true
- `side_panel.usage_poll_seconds` : integer (minimum=60, maximum=3600) default=150
- `side_panel.sections` : object default={"session": true, "goal": true, "context": true, "usage": false, "agents": true, "tools": true, "files": true, "memory": true}
- `side_panel.sections.session` : boolean default=true
- `side_panel.sections.goal` : boolean default=true
- `side_panel.sections.context` : boolean default=true
- `side_panel.sections.usage` : boolean default=false
- `side_panel.sections.agents` : boolean default=true
- `side_panel.sections.tools` : boolean default=true
- `side_panel.sections.files` : boolean default=true
- `side_panel.sections.memory` : boolean default=true

## Leak self-check

Only key names, types, enumerations, bounds and defaults (interface facts) are listed. No
description text, comments or code from the SUL sources or the generated schema is included.
