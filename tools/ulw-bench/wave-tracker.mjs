// SPDX-License-Identifier: MIT

import fs from "node:fs";
import { isWakeType, parseWakeChildren, parseWakeSpill } from "./session-bytes.mjs";

const WAITING_STATES = new Set(["waiting_children", "waiting_background", "waiting_human"]);
// A child queued behind the concurrency limit reports "pending"; it is still owed a wake.
const LIVE_CHILD_STATES = new Set(["running", "pending"]);
// Only a background child is owed a wake: a task result marked run_in_background true (spawned in
// the background, or a foreground child promoted at its wait budget) whose item is running/pending.
// A foreground task result already carries the finished child, so no wake comes. task_output and
// task_cancel never add an owed child; they only update ids already owed. For an owed child, a
// reported externally-caused terminal (completed/error/lost) does not consume the wake;
// senpi-task completion/routing.rs still delivers it. Parent-initiated terminals never wake the
// parent; the tool result stands in for the wake. A task_send that revives a finished background
// child starts a new run whose completion wakes the parent again, so it re-owes that child; a
// revived foreground or unknown child never wakes and is ignored.
const SILENT_TERMINAL_STATES = new Set(["cancelled", "interrupted"]);
const CHILD_TOOLS = new Set(["task", "task_output", "task_cancel", "task_send"]);

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
  // Live children, only for the peakChildren metric.
  const running = new Set();
  // Children still owed a wake, in the order first seen. Settling requires this to be empty.
  const owed = new Set();
  // Children already woken, so a late or repeated tool result cannot re-owe them.
  const woken = new Set();
  // Children spawned by (or promoted to) a background task result. Only these wake on terminal, so
  // only these are re-owed when task_send revives them; a revived foreground child never wakes
  // (notify_on_terminal stays false).
  const backgroundIds = new Set();
  const wakeChild = (id) => {
    woken.add(id);
    owed.delete(id);
    running.delete(id);
  };
  // A wake child without a parseable id, or one of N children spilled to an unreadable manifest,
  // is still one delivered wake: it clears the oldest owed id. Wakes are delivered roughly in
  // completion order, and only the count matters for settling.
  const anonymousWake = () => {
    const oldest = owed.values().next();
    if (!oldest.done) wakeChild(oldest.value);
  };
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
          // Real rpc runs never emit lifecycle child counts; the task, task_output, task_cancel, and
          // task_send results carry the child id and status.
          if (CHILD_TOOLS.has(event.toolName)) {
            const details = event.result?.details;
            // SendResultDetails::Revived (control/types.rs): the finished child runs again and wakes again.
            if (event.toolName === "task_send") {
              if (details?.kind === "revived" && backgroundIds.has(details.task_id)) {
                woken.delete(details.task_id);
                owed.add(details.task_id);
                running.add(details.task_id);
              }
            } else {
              const isTask = event.toolName === "task";
              for (const item of childEntries(details)) {
                if (typeof item?.task_id !== "string" || item.task_id === "") continue;
                // cancel_pending has no status: the child is still live until its host stops it.
                if (typeof item.status !== "string") continue;
                const id = item.task_id;
                const live = LIVE_CHILD_STATES.has(item.status);
                // A promoted foreground item carries run_in_background true on the item itself.
                const background = isTask && (details?.run_in_background === true || item.run_in_background === true);
                if (background) backgroundIds.add(id);
                if (live) running.add(id);
                else running.delete(id);
                if (live) {
                  if (background && !woken.has(id)) owed.add(id);
                } else if ((isTask && !background) || SILENT_TERMINAL_STATES.has(item.status)) {
                  owed.delete(id);
                }
              }
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
            // An oversized batch spills its tail children to a manifest file (maho-omo
            // coordinator.rs); the manifest holds their completions in the same format. Read it
            // when it is on this machine, else count the spilled children as anonymous wakes.
            const spill = parseWakeSpill(text);
            let spilled = [];
            if (spill) {
              try {
                spilled = parseWakeChildren(fs.readFileSync(spill.manifestPath, "utf8"));
              } catch {
                spilled = [];
              }
              if (spilled.length !== spill.count) spilled = Array.from({ length: spill.count }, () => ({ id: null }));
            }
            wakeBatches.push({ bytes: rawLine.length, children: children.length + spilled.length });
            for (const c of [...children, ...spilled]) {
              if (c.id) wakeChild(c.id);
              else anonymousWake();
            }
            childReports.push(...children.map((c) => c.reportChars));
          }
          break;
        default:
          break;
      }
    },
    isSettled: () => idle && !waiting && owed.size === 0,
    summary: () => ({
      wakeBatchBytes: wakeBatches.map((b) => b.bytes),
      wakeBatchChildren: wakeBatches.map((b) => b.children),
      childReportSizes: childReports,
      peakChildren,
      secondWaveObserved: toolCallsAfterFirstWake > 0,
    }),
  };
}
