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
  setTimeout(() => {
    ${out({ type: "agent_start" })}
    ${out(wake(wakeText("a", "z".repeat(7))))}
    ${out({ type: "tool_execution_start", toolName: "task", args: {} })}
    ${out(lifecycle("running", 0))}
    ${out({ type: "agent_idle" })}
  }, 50);
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
