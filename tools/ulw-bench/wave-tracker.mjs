// SPDX-License-Identifier: MIT

import { isWakeType, parseWakeChildren } from "./session-bytes.mjs";

const WAITING_STATES = new Set(["waiting_children", "waiting_background", "waiting_human"]);
// A child queued behind the concurrency limit reports "pending"; it is still owed a wake.
const LIVE_CHILD_STATES = new Set(["running", "pending"]);
// Parent-initiated terminals never wake the parent (senpi-task completion/routing.rs notifies only
// completed/error/lost), so observing one in a tool result stands in for the child's wake.
const SILENT_TERMINAL_STATES = new Set(["cancelled", "interrupted"]);
const CHILD_TOOLS = new Set(["task", "task_output", "task_cancel"]);

/**
 * Child entries of a child-tool result: the task tool lists every batch child in details.items[]
 * (its top-level task_id is only the first child); task_output nests the child under
 * details.snapshot (TaskOutputDetails); task_cancel carries task_id/status at the top level
 * (CancelResultDetails).
 */
function childEntries(details) {
  if (Array.isArray(details?.items)) return details.items;
  if (details?.snapshot) return [details.snapshot];
  return [details];
}

/**
 * Tracks the waves of a headless rpc-mode run from its event stream. `mhc -p` ends the turn at the
 * first wait for background children; `--mode rpc` keeps the session alive, so the runner can wait
 * until the children settled and the parent finished its wake turns.
 * @returns {{ observe: (event: object, rawLine: string) => void, isSettled: () => boolean, summary: () => object }}
 */
export function createWaveTracker() {
  let idle = false;
  let waiting = false;
  let peakChildren = 0;
  const running = new Set();
  // Children whose wake obligation is met, by id: woken, or ended by a silent terminal.
  const satisfied = new Set();
  let anonymousWakes = 0;
  let toolCallsAfterFirstWake = 0;
  const wakeBatches = [];
  const childReports = [];

  return {
    observe(event, rawLine) {
      switch (event.type) {
        case "agent_start":
          idle = false;
          break;
        case "agent_idle":
          idle = true;
          break;
        case "session_lifecycle": {
          const l = event.lifecycle ?? {};
          const wait = l.waitingOn ?? {};
          const children = Number(wait.children) || 0;
          peakChildren = Math.max(peakChildren, children);
          waiting = WAITING_STATES.has(l.state) || children > 0 || (wait.background?.length ?? 0) > 0;
          break;
        }
        case "tool_execution_end": {
          // Real rpc runs never emit lifecycle child counts; the task, task_output, and task_cancel
          // results carry the child id and status.
          if (CHILD_TOOLS.has(event.toolName)) {
            for (const item of childEntries(event.result?.details)) {
              if (typeof item?.task_id !== "string" || item.task_id === "") continue;
              // cancel_pending has no status: the child is still live until its host stops it.
              if (typeof item.status !== "string") continue;
              if (LIVE_CHILD_STATES.has(item.status)) running.add(item.task_id);
              else running.delete(item.task_id);
              if (SILENT_TERMINAL_STATES.has(item.status)) satisfied.add(item.task_id);
            }
            peakChildren = Math.max(peakChildren, running.size);
          }
          break;
        }
        case "tool_execution_start":
          if (wakeBatches.length > 0 && (event.toolName === "task" || event.toolName === "workflow")) {
            toolCallsAfterFirstWake++;
          }
          break;
        case "message_end":
          if (event.message?.role === "custom" && isWakeType(event.message.customType)) {
            const content = event.message.content;
            const text = Array.isArray(content) ? content.map((c) => c?.text ?? "").join("") : String(content ?? "");
            const children = parseWakeChildren(text);
            wakeBatches.push({ bytes: rawLine.length, children: children.length });
            for (const c of children) {
              if (!c.id) {
                anonymousWakes++;
                continue;
              }
              running.delete(c.id);
              satisfied.add(c.id);
            }
            childReports.push(...children.map((c) => c.reportChars));
          }
          break;
        default:
          break;
      }
    },
    isSettled: () => idle && !waiting && running.size === 0 && satisfied.size + anonymousWakes >= peakChildren,
    summary: () => ({
      wakeBatchBytes: wakeBatches.map((b) => b.bytes),
      wakeBatchChildren: wakeBatches.map((b) => b.children),
      childReportSizes: childReports,
      peakChildren,
      secondWaveObserved: toolCallsAfterFirstWake > 0,
    }),
  };
}
