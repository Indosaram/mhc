# 별도 정적 확인 결과 (원문 기록)

기준 커밋은 README와 같다. 모두 정적 비교이며 심각도는 UNSET이다. kind: difference=확인된 정적 차이, positive=정적으로 같다고 읽음(런타임 동등성 아님), candidate/other=후보 또는 기타. 기록 원문(영문)을 그대로 옮겼다.

## TTSR-FAMILY (positive)

- **text**: All TTSR detectors (token-grammar 9 modes, control-leak, leak-context, collapse+lines/paragraphs/periods/scalars/near-duplicates, repetitive-turns) and manager, index, commands, remediation, stream-remediation, activation parser, rule-parser, discovery(scope parse side), coordinator, watch, message-update, builtin-rules, prompts, rule-condition, stream-utils, types, repetitive-turns-lane branch-paired
- **qualifiers**: static-reviewed equivalence, not runtime. discovery/index edge cases partial. family not complete. repetitive-turns-lane native incremental trigram cache vs source full recompute: native test is self-differential only; equivalence to source normalizer inferred
- **source**: senpi packages/coding-agent/src/core/extensions/builtin/ttsr/*; mhc crates/builtins/maho-ext-ttsr/src/*
- **execution**: synthesis_own_work_static_only_no_repo_code_executed_except_noted_node_string_compare;other_lanes_axis_separate

## TTSR-PROMPT-1 (difference)

- **text**: static_pair REPETITIVE_TURNS_RULE_CONTENT: source uses ': a cross-turn' and '(if the task is actually blocked)'; native uses em dashes. Other prompt strings string-equal (Node load of source prompts.ts vs literals parsed from prompts.rs)
- **qualifiers**: concrete, text only, no behavior impact claimed; native test pin unchecked
- **source**: ttsr/prompts.ts; maho-ext-ttsr/src/prompts.rs
- **execution**: synthesis_own_work_static_only_no_repo_code_executed_except_noted_node_string_compare;other_lanes_axis_separate

## TTSR-LOOP-1 (difference)

- **text**: Source ruleAlreadyCorrected + ttsr-loop-stopped entry/event + notice + engine-paused(reason repetition) in agent_settled, and settledRepetitiveTurnPause call at agent-session.ts:3328 are unwired natively (engine_paused.rs:45 has definition+test, no caller; index.rs AgentSettled always sends nudge). Generic stop_engine_turn counts engine-turn-start with 150 per user input bound (agent_session.rs:5644, engine_turn_limit.rs:10) so no unbounded-loop claim
- **qualifiers**: static; per-rule one-correction stop absent; impact inferred; consumers in other crates not exhaustively traced
- **source**: ttsr/index.ts:275-312, follow-up-limit.ts, engine-paused.ts; maho-ext-ttsr/src/index.rs:74; maho-core agent_session.rs, engine_paused.rs
- **execution**: synthesis_own_work_static_only_no_repo_code_executed_except_noted_node_string_compare;other_lanes_axis_separate

## TTSR-NUM-1 (candidate)

- **text**: unconfirmed numeric difference
- **qualifiers**: unconfirmed
- **execution**: synthesis_own_work_static_only_no_repo_code_executed_except_noted_node_string_compare;other_lanes_axis_separate

## TTSR-SIM-1 (candidate)

- **text**: toFixed(3) vs round()/1000 in collapse-near-duplicates and repetitive-turns detail
- **qualifiers**: candidate; likely nonfiring (no reader of match.detail outside detectors); 206 enumerated fractions are not observed cases
- **execution**: synthesis_own_work_static_only_no_repo_code_executed_except_noted_node_string_compare;other_lanes_axis_separate

## TTSR-SAMPLE-1 (candidate)

- **text**: narrow sampling difference
- **qualifiers**: nonfiring
- **execution**: synthesis_own_work_static_only_no_repo_code_executed_except_noted_node_string_compare;other_lanes_axis_separate

## TTSR-GLOB-1 (candidate)

- **text**: picomatch vs native globset/regex translator
- **qualifiers**: unproven; picomatch not runnable
- **execution**: synthesis_own_work_static_only_no_repo_code_executed_except_noted_node_string_compare;other_lanes_axis_separate

## TTSR-YAML-1 (candidate)

- **text**: npm yaml 1.2 vs serde_yaml 0.9.34 plus normalizer
- **qualifiers**: unproven
- **execution**: synthesis_own_work_static_only_no_repo_code_executed_except_noted_node_string_compare;other_lanes_axis_separate

## TTSR-SCOPE-CASE-1 (candidate)

- **text**: Rust (?i) Unicode folding vs source scope case handling
- **qualifiers**: narrow, inferred
- **execution**: synthesis_own_work_static_only_no_repo_code_executed_except_noted_node_string_compare;other_lanes_axis_separate

## ANTH-FOLD-1 (difference)

- **text**: refresh failure prefix: source keeps rate_limit/overloaded as plain detail, maps status=5xx/TimeoutError to server_error; native auth_lane.rs refresh_selected maps rate_limit/overloaded to authentication_failed, no status= pattern. Native classify_sdk_error checks authentication_failed first, failover.rs:211 blocked_until None for AuthError
- **qualifiers**: static pair concrete; auth-block impact static-traced; stream.rs:110 `?` -> stream.rs:255 handler link not read line by line
- **source**: senpi anthropic-subscription/*; maho-ext-anthropic-subscription auth_lane.rs, errors.rs, failover.rs, stream.rs
- **execution**: synthesis_own_work_static_only_no_repo_code_executed_except_noted_node_string_compare;other_lanes_axis_separate

## ANTH-FOLD-2 (difference)

- **text**: auth-block recovery: native AccountSlot lacks blockedAt, modelBlocks, authRecoveryGrant, importedFrom. Path-specific behaviors kept distinct: ext failover blocked_until=None cleared by clear_expired_blocks at next run_failover; core pool permanent AuthError nontimed block cleared on credential_revision change; do not merge
- **qualifiers**: functional absence provisional
- **source**: maho-ext-anthropic-subscription/src/failover.rs, accounts.rs; maho-core rotation_stream
- **execution**: synthesis_own_work_static_only_no_repo_code_executed_except_noted_node_string_compare;other_lanes_axis_separate

## ANTH-FOLD-3 (difference)

- **text**: account context guidance text: source stream-guidance.ts names account label and source for billing/out of extra usage; native stream_guidance.rs generic. Log-event absence grep-only
- **qualifiers**: text difference concrete; log absence unproven
- **execution**: synthesis_own_work_static_only_no_repo_code_executed_except_noted_node_string_compare;other_lanes_axis_separate

## ANTH-FOLD-4 (difference)

- **text**: session cwd: native has no absolute-cwd validation (source requireAnthropicSubscriptionCwd)
- **qualifiers**: static difference confirmed; wrong normal cwd unproven
- **execution**: synthesis_own_work_static_only_no_repo_code_executed_except_noted_node_string_compare;other_lanes_axis_separate

## ANTH-FOLD-5 (difference)

- **text**: sent images: native accepts any '[Image elided:' prefix text; image without data hashes ''
- **qualifiers**: static pair concrete
- **execution**: synthesis_own_work_static_only_no_repo_code_executed_except_noted_node_string_compare;other_lanes_axis_separate

## ANTH-FOLD-6 (difference)

- **text**: cold-seed budget: native live_owns_compaction (lane_policy.rs:61) owns every Overflow on SDK lanes; source ownsCompaction requires lastTurnIsColdSeedOverflow (lane-policy.ts:173). Native passes CompactionReason::Overflow (agent_session.rs 7303,7341,8521,8736). Native has no cold-seed-budget module: no estimateColdSeedTokens, calibration, claude_sdk_oauth_cold_seed_overflow diagnostic, 'too long to resend' refusal (grep over crates)
- **qualifiers**: static; functional impact inferred; other native oversize guard not traced
- **execution**: synthesis_own_work_static_only_no_repo_code_executed_except_noted_node_string_compare;other_lanes_axis_separate

## IM-WITHDRAWN (withdrawn)

- **text**: L4 zip symlink candidate; LSP is_alive self-lock candidate; EXTINPUTPH refuted; DSI1 identity wire no claim; circuit-breaker probe-lane equivalence unresolved
- **qualifiers**: preserve
- **execution**: synthesis_own_work_static_only_no_repo_code_executed_except_noted_node_string_compare;other_lanes_axis_separate
