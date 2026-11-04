// SPDX-License-Identifier: MIT

import { describe, it } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import os from "node:os";
import { createWaveTracker } from "./wave-tracker.mjs";
import { executeScenarioRun } from "./run-scenarios.mjs";
import { parseSessionBytesFromLines } from "./session-bytes.mjs";

const lifecycle = (state, children) => ({ type: "session_lifecycle", lifecycle: { state, waitingOn: { children, background: [] } } });
const wakeText = (id, report) => `task completion name:n id:${id} status:completed duration:1s\nresult:"${report}"`;
const wake = (...texts) => ({ type: "message_end", message: { role: "custom", customType: "omo-senpi:wake", content: [{ type: "text", text: texts.join("\n\n") }] } });
const feed = (tracker, events) => events.forEach((e) => tracker.observe(e, JSON.stringify(e)));

describe("wave-tracker", () => {
  it("is not settled while the parent idles waiting on children", () => {
    const t = createWaveTracker();
    feed(t, [{ type: "agent_start" }, lifecycle("running", 2), lifecycle("waiting_children", 2), { type: "agent_idle" }]);
    assert.equal(t.isSettled(), false);
  });

  it("records each wake batch and each child report separately, and settles after the last wake turn", () => {
    const t = createWaveTracker();
    feed(t, [
      { type: "agent_start" },
      lifecycle("running", 2),
      { type: "agent_idle" },
      wake(wakeText("a", "x".repeat(10))),
      { type: "agent_start" },
      { type: "tool_execution_start", toolName: "task" },
      wake(wakeText("b", "y".repeat(30))),
      lifecycle("running", 0),
      { type: "agent_idle" },
    ]);
    assert.equal(t.isSettled(), true);
    const s = t.summary();
    assert.deepEqual(s.childReportSizes, [10, 30]);
    assert.equal(s.wakeBatchBytes.length, 2);
    assert.deepEqual(s.wakeBatchChildren, [1, 1]);
    assert.equal(s.secondWaveObserved, true);
  });

  it("does not count a task call before the first wake as a second wave", () => {
    const t = createWaveTracker();
    feed(t, [{ type: "tool_execution_start", toolName: "task" }, lifecycle("running", 1)]);
    assert.equal(t.summary().secondWaveObserved, false);
  });
});

// Synthetic task results in the exact serde shape of TaskToolDetails / TaskToolItemDetail (types.rs).
const batchItem = (id, status, queuePosition) => ({
  task_id: id,
  task_summary: `child ${id}`,
  name: id,
  category: "quick",
  status,
  ...(queuePosition === undefined ? {} : { queue_position: queuePosition }),
});
// execute_batch.rs background_result: top-level task_id is the first live child; every child is in items[].
const batchEnd = (items) => ({
  type: "tool_execution_end",
  toolName: "task",
  result: {
    content: [{ type: "text", text: "Started tasks" }],
    details: { task_id: items[0].task_id, status: "running", mode: "spawn", run_in_background: true, items },
  },
  isError: false,
});
const singleEnd = (id, status, queuePosition) => ({
  type: "tool_execution_end",
  toolName: "task",
  result: {
    content: [{ type: "text", text: `Started task ${id}` }],
    details: { task_id: id, status, mode: "spawn", run_in_background: true, ...(queuePosition === undefined ? {} : { queue_position: queuePosition }) },
  },
  isError: false,
});

describe("wave-tracker on task tool results", () => {
  it("counts every child of a background batch and settles only after the last staggered wake", () => {
    const t = createWaveTracker();
    feed(t, [
      { type: "agent_start" },
      { type: "tool_execution_start", toolName: "task" },
      batchEnd([batchItem("st_a", "running"), batchItem("st_b", "running"), batchItem("st_c", "pending", 1)]),
      { type: "agent_idle" },
    ]);
    assert.equal(t.summary().peakChildren, 3);
    assert.equal(t.isSettled(), false);

    feed(t, [wake(wakeText("st_a", "a")), { type: "agent_start" }, { type: "agent_idle" }]);
    assert.equal(t.isSettled(), false, "settled after 1st wake");

    feed(t, [wake(wakeText("st_b", "bb")), { type: "agent_start" }, { type: "agent_idle" }]);
    assert.equal(t.isSettled(), false, "settled after 2nd wake");

    feed(t, [wake(wakeText("st_c", "ccc")), { type: "agent_start" }, { type: "agent_idle" }]);
    assert.equal(t.isSettled(), true, "not settled after 3rd wake");
    const s = t.summary();
    assert.equal(s.peakChildren, 3);
    assert.deepEqual(s.childReportSizes, [1, 2, 3]);
  });

  it("keeps a single child queued behind the concurrency limit (pending) live until its wake", () => {
    const t = createWaveTracker();
    feed(t, [
      { type: "agent_start" },
      singleEnd("st_q", "pending", 2),
      { type: "agent_idle" },
    ]);
    assert.equal(t.summary().peakChildren, 1);
    assert.equal(t.isSettled(), false);
    feed(t, [wake(wakeText("st_q", "q")), { type: "agent_start" }, { type: "agent_idle" }]);
    assert.equal(t.isSettled(), true);
  });

  it("drops a batch child whose item reports a terminal status", () => {
    const t = createWaveTracker();
    feed(t, [
      { type: "agent_start" },
      batchEnd([batchItem("st_a", "running"), batchItem("st_b", "pending", 1)]),
      batchEnd([batchItem("st_a", "completed"), batchItem("st_b", "running")]),
    ]);
    assert.equal(t.summary().peakChildren, 2);
    feed(t, [wake(wakeText("st_a", "a")), wake(wakeText("st_b", "b")), { type: "agent_idle" }]);
    assert.equal(t.isSettled(), true);
  });
});

// TaskOutputDetails::Status (output/types.rs): serde tag "kind", the child under snapshot.
const outputStatusEnd = (id, status) => ({
  type: "tool_execution_end",
  toolName: "task_output",
  result: {
    content: [{ type: "text", text: `${id} ${status}` }],
    details: {
      kind: "status",
      snapshot: { task_id: id, name: id, status, residency_state: "resident", execution_mode: "background", model: "m" },
    },
  },
  isError: false,
});
// CancelResultDetails::Cancelled (control/types.rs): serde tag "kind", task_id/status at the top level.
const cancelEnd = (id, previousStatus) => ({
  type: "tool_execution_end",
  toolName: "task_cancel",
  result: {
    content: [{ type: "text", text: `Cancelled ${id} (was ${previousStatus}, now cancelled).` }],
    details: { kind: "cancelled", task_id: id, previous_status: previousStatus, status: "cancelled" },
  },
  isError: false,
});

describe("wave-tracker on task_output and task_cancel results", () => {
  it("reads the child from a task_output status snapshot", () => {
    const t = createWaveTracker();
    feed(t, [{ type: "agent_start" }, outputStatusEnd("st_o", "running"), { type: "agent_idle" }]);
    assert.equal(t.summary().peakChildren, 1);
    assert.equal(t.isSettled(), false);
    feed(t, [outputStatusEnd("st_o", "completed"), { type: "agent_idle" }]);
    assert.equal(t.isSettled(), false, "a completed child is still owed its wake");
    feed(t, [wake(wakeText("st_o", "o")), { type: "agent_start" }, { type: "agent_idle" }]);
    assert.equal(t.isSettled(), true);
  });

  it("settles a cancelled child, which never gets a wake, at the next agent_idle", () => {
    const t = createWaveTracker();
    feed(t, [
      { type: "agent_start" },
      batchEnd([batchItem("st_a", "running"), batchItem("st_b", "running")]),
      { type: "agent_idle" },
      wake(wakeText("st_a", "a")),
      { type: "agent_start" },
      cancelEnd("st_b", "running"),
    ]);
    assert.equal(t.isSettled(), false, "settled while the parent is still streaming");
    feed(t, [{ type: "agent_idle" }]);
    assert.equal(t.isSettled(), true);
    assert.equal(t.summary().peakChildren, 2);
  });

  it("settles a child whose task_output snapshot reports cancelled", () => {
    const t = createWaveTracker();
    feed(t, [{ type: "agent_start" }, singleEnd("st_c", "running"), outputStatusEnd("st_c", "cancelled"), { type: "agent_idle" }]);
    assert.equal(t.isSettled(), true);
  });
});

describe("wave-tracker on a captured real rpc stream", () => {
  const lines = fs.readFileSync(new URL("./fixtures/rpc-background-task-stream.jsonl", import.meta.url), "utf8").split("\n").filter(Boolean);

  it("counts the background child from task events and settles only after its wake turn", () => {
    const t = createWaveTracker();
    let settledBeforeWake = false;
    let woke = false;
    for (const line of lines) {
      const e = JSON.parse(line);
      t.observe(e, line);
      if (e.type === "message_end" && e.message?.customType === "omo-senpi:wake") woke = true;
      if (!woke && t.isSettled()) settledBeforeWake = true;
    }
    assert.equal(settledBeforeWake, false);
    assert.equal(t.isSettled(), true);
    const s = t.summary();
    assert.equal(s.peakChildren, 1);
    assert.equal(s.wakeBatchChildren.length, 1);
    assert.equal(s.childReportSizes.length, 1);
  });
});

describe("session-bytes wake batches", () => {
  it("parses a wake whose content is a plain string", () => {
    const line = JSON.stringify({ type: "custom_message", customType: "omo-senpi:wake", content: `${wakeText("a", "abcd")}\n\n${wakeText("b", "ef")}` });
    const r = parseSessionBytesFromLines([line]);
    assert.deepEqual(r.childReportChars, [4, 2]);
  });

  it("splits a multi-child wake into per-child report lengths", () => {
    const line = JSON.stringify({
      type: "custom_message",
      customType: "omo-senpi:wake",
      content: [{ type: "text", text: `${wakeText("a", "abc")}\n\n${wakeText("b", "defgh")}` }],
    });
    const r = parseSessionBytesFromLines([line]);
    assert.equal(r.wakeBatches.length, 1);
    assert.equal(r.wakeBatches[0].bytes, line.length);
    assert.deepEqual(r.childReportChars, [3, 5]);
  });
});

describe("executeScenarioRun headless wait", () => {
  it("kills a run whose child never settles at the wall-clock cap and records timed-out", async () => {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), "ulw-wall-"));
    try {
      const bin = path.join(dir, "hang-mhc.mjs");
      fs.writeFileSync(bin, `#!/usr/bin/env node
process.stdin.on("data", () => process.stdout.write(${JSON.stringify(JSON.stringify(lifecycle("running", 1)) + "\n")}));
setInterval(() => {}, 1000);
`, { mode: 0o755 });
      const scenario = { id: "S-H", name: "h", prompt: "go", waitForChildren: true, expectedOutcome: {} };
      const r = await executeScenarioRun({ bin, model: "m", scenario, runIndex: 1, fixtureDir: dir, maxInputTokens: 1e9, outDir: dir, maxWallMs: 300 });
      assert.equal(r.status, "timed-out");
    } finally {
      fs.rmSync(dir, { recursive: true, force: true });
    }
  });

  it("keeps an rpc-mode run alive past the first wait until children settle, recording waves", async () => {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), "ulw-wave-"));
    try {
      const bin = path.join(dir, "fake-mhc.mjs");
      const out = (e) => `process.stdout.write(${JSON.stringify(JSON.stringify(e) + "\n")});`;
      fs.writeFileSync(bin, `#!/usr/bin/env node
process.stdin.on("data", () => {
  ${out(lifecycle("running", 1))}
  ${out({ type: "agent_idle" })}
  ${out({ type: "agent_start" })}
  ${out(wake(wakeText("a", "z".repeat(7))))}
  ${out({ type: "tool_execution_start", toolName: "task", args: {} })}
  ${out(lifecycle("running", 0))}
  ${out({ type: "agent_idle" })}
});
process.stdin.on("end", () => process.exit(0));
`, { mode: 0o755 });
      const scenario = { id: "S-W", name: "w", prompt: "go", waitForChildren: true, expectedOutcome: {} };
      const r = await executeScenarioRun({ bin, model: "m", scenario, runIndex: 1, fixtureDir: dir, maxInputTokens: 1e9, outDir: dir });
      assert.equal(r.status, "completed");
      assert.deepEqual(r.childReportSizes, [7]);
      assert.equal(r.wakeBatchBytes.length, 1);
      assert.equal(r.secondWaveObserved, true);
    } finally {
      fs.rmSync(dir, { recursive: true, force: true });
    }
  });
});
