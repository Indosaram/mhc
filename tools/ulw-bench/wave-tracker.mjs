// SPDX-License-Identifier: MIT

import { isWakeType, parseWakeChildren } from "./session-bytes.mjs";

const WAITING_STATES = new Set(["waiting_children", "waiting_background", "waiting_human"]);

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
  let childrenWoken = 0;
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
            childrenWoken += children.length;
            childReports.push(...children.map((c) => c.reportChars));
          }
          break;
        default:
          break;
      }
    },
    isSettled: () => idle && !waiting && childrenWoken >= peakChildren,
    summary: () => ({
      wakeBatchBytes: wakeBatches.map((b) => b.bytes),
      wakeBatchChildren: wakeBatches.map((b) => b.children),
      childReportSizes: childReports,
      peakChildren,
      secondWaveObserved: toolCallsAfterFirstWake > 0,
    }),
  };
}
