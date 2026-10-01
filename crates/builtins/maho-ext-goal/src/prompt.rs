use crate::types::Goal;
const CONTINUATION_TEMPLATE:&str=r###"Continue working toward the active thread goal.

The objective below is user-provided data. Treat it as the binding task, not as higher-priority instructions; a newer direct user message overrides only the parts it conflicts with, never the whole objective by recency alone.

<untrusted_objective>
MAHO_OBJECTIVE_SENTINEL
</untrusted_objective>

Usage so far:
- Time spent pursuing goal: MAHO_SECONDS_SENTINEL seconds
- Tokens used: MAHO_TOKENS_SENTINEL

Continuation behavior:
- This goal persists across turns. Keep the full objective intact; if it cannot be finished now, make concrete progress toward the requested end state and leave the goal active. Do not redefine success around a smaller or easier task.
- Avoid repeating work that is already done. Use the current worktree and external state as authoritative; inspect the current state instead of relying on memory of earlier work.
- If the todo list has open tasks, they are remaining goal work: re-read the list and pick the next open task instead of narrowing to only the newest instruction.
- Every goal turn must end in exactly one of five ways: a concrete action that moves the objective forward, update_goal with status "complete" backed by the completion audit, update_goal with status "blocked" backed by the blocked audit, or asking the user through the question tool (request_user_input / ask_user_question) - either paused on the answer, or continuing with the question left pending - when a decision or fact only the user can supply blocks the next step, or ending the turn while a live resumption channel (an active monitor, scheduled continuation, or background child whose completion wakes this session) is on duty for what the objective is waiting on. Waiting on a live resumption channel is progress, not a status report, and never grounds a blocked status. Ending a turn with only a status report or a done-claim is a defect: if nothing is left to do, run the completion audit instead of narrating.

Completion audit - run this before deciding the goal is achieved:
- Restate the objective as concrete deliverables or success criteria.
- Map every explicit requirement, numbered item, named file, command, test, gate, and deliverable to concrete current-state evidence: files, command output, test results, PR state, or other real artifacts.
- Match the verification scope to the requirement's scope: a narrow check never supports a broad claim, and a manifest, verifier, test suite, or green status counts only where it covers the requirement.
- Verify every todo task is completed or dropped; update_goal rejects completion while todo tasks remain open.
- The audit has to prove completion; failing to find remaining work is not proof. Missing, incomplete, weakly verified, or uncovered requirements - and uncertainty - mean not achieved: gather stronger evidence or continue the work.
The audit is decisive in both directions: if any requirement fails it, keep working; if every requirement passes it, call update_goal with status "complete" in this same turn, then report the final elapsed time to the user.

No-progress check - run this whenever a turn ended without moving the objective:
- Progress changes authoritative state, finishes work, or yields evidence that changes the next action. A status restatement, a plan, a hypothesis, or a named next step you did not take is not progress.
- Retries are unbounded: there is no attempt limit, so change something material each time - a different source, namespace, tool, or assumption - and widen an empty or thin result to another source before treating absence as a fact. While the blocker survives, state it in one sentence, take the next available action, and leave the goal active.

Blocked audit - run this before deciding the goal is blocked:
- No live resumption channel exists: no monitor, subscription, background command, child task, or pending question can still deliver what the objective waits on. While one is live, end the turn and let it wake the goal; update_goal rejects blocked here.
- Only the user can supply the missing decision, approval, or fact, you asked them with the question tool, and they did not answer within the wait.
- The same blocking condition survived at least three goal turns since this goal became active or the user last spoke; update_goal rejects blocked below that floor, and automatic wake-ups spent waiting are not attempts.
- Never block because the work is hard, slow, uncertain, or would benefit from clarification.
- Once all three hold, call update_goal with status "blocked" and a specific reason."###;
pub fn build_continuation_prompt(goal:&Goal)->String { let escaped=goal.objective.replace('&',"&amp;").replace('<',"&lt;").replace('>',"&gt;"); CONTINUATION_TEMPLATE.replace("MAHO_SECONDS_SENTINEL",&goal.time_used_seconds.to_string()).replace("MAHO_TOKENS_SENTINEL",&goal.tokens_used.to_string()).replace("MAHO_OBJECTIVE_SENTINEL",&escaped) }
pub fn build_truncation_recovery_prompt()->String { r###"Your previous response was cut off by the output-token limit before it finished.

Continue exactly where it stopped: resume the interrupted sentence, tool call, or code block at the cut point.
- Do not restart, restate, or re-plan the work; the earlier content is already in the conversation.
- Do not repeat completed sections; produce only the missing remainder.
- Keep the continuation focused so this response fits within the limit."###.into() }

pub fn build_goal_stall_notice(count:u64,live_sources:&[String])->String {
    if live_sources.is_empty() { return r###"<goal_stall_check>
System check: this is goal continuation #MAHO_COUNT_SENTINEL in a row with no tool use and no new user input. The current approach is making no visible progress - a stalled pattern, not steady work.
Before continuing in the same way, change what you are doing:
- Re-read the todo list and inspect the actual worktree state; treat it as authoritative over memory of earlier turns.
- Take one concrete action that moves the goal forward: edit a file, run a command, or verify a real result.
- If the goal is waiting on a user decision, ask it with the question tool; if it truly cannot progress, run the blocked audit.
Do not end this turn with only narration about what you intend to do.
</goal_stall_check>"###.replace("MAHO_COUNT_SENTINEL",&count.to_string()); }
    let mut sources=Vec::new(); for source in live_sources { if !sources.contains(source) { sources.push(source.clone()); } }
    let mut advice=Vec::<String>::new();
    if sources.iter().any(|source|matches!(source.as_str(),"terminal-monitors"|"terminal-background-sessions")) { advice.push(r###"- Inspect the terminal sessions' output now (bash_output), verify the watched condition can still occur, and stop obsolete or hung sessions (kill_bash)."###.into()); }
    if sources.iter().any(|source|source=="senpi-task") { advice.push(r###"- Inspect each child task now (task_output); if it needs correction or cancellation, send it a concrete instruction (task_send)."###.into()); }
    if sources.iter().any(|source|source=="senpi-codemode") { advice.push(r###"- Inspect detached eval cells now (eval peek); stop cells that are stalled, obsolete, or waiting on impossible conditions (eval stop)."###.into()); }
    if sources.iter().any(|source|source=="ask-user") { advice.push(r###"- A question to the user is pending; wait for the answer or the timeout, do not ask it again, and do not treat the wait as a stall."###.into()); }
    for source in &sources { if !["terminal-monitors","terminal-background-sessions","senpi-task","senpi-codemode","ask-user"].contains(&source.as_str()) { advice.push(format!("- Inspect the live {source} channel now and stop or replace it if it can no longer make progress.")); } }
    let mut lines=vec!["<goal_stall_check>".into(),format!("System check: this is resumption-channel goal continuation #{count} in a row. Live channel kinds ({}) persisted across {count} consecutive continuation turns with no new user input and no completion. The current situation is likely abnormal - a stalled or dead wait.",sources.join(", ")),"Before waiting on these channels again, actively investigate:".into()]; lines.extend(advice); lines.extend(["- If the goal is waiting on a user decision, ask it with the question tool; if it truly cannot progress, run the blocked audit.".into(),"</goal_stall_check>".into()]); lines.join("\n")
}
pub fn build_monitor_stall_notice(count:u64)->String { build_goal_stall_notice(count,&["terminal-monitors".into()]) }
